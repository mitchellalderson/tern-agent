//! The no-LLM reader (design §2.4): a small, tolerant HTML tree builder over
//! hostile bytes, with main-content text, title, meta, headings, tables,
//! anchors and simple CSS selectors. Everything here is deterministic Rust
//! and memory-safe; it is also the known same-process residual of design
//! §4 (parser isolation is deferred to F3).

use std::collections::BTreeMap;

use super::sanitize::{clean_line, clean_text};

/// More elements than this and the rest of the page is ignored.
const MAX_NODES: usize = 200_000;
/// Deeper nesting is flattened into the node at this depth.
const MAX_DEPTH: usize = 256;

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
/// Text inside these is never page content.
const HIDDEN: &[&str] = &[
    "script", "style", "noscript", "template", "head", "svg", "iframe", "object",
];
/// Page chrome: left out of the main text when a `<main>`/`<article>` isn't found.
const CHROME: &[&str] = &["nav", "header", "footer", "aside", "form"];
const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "br",
    "blockquote",
    "pre",
    "dd",
    "dt",
    "dl",
    "figure",
    "figcaption",
    "details",
    "summary",
    "hr",
];

#[derive(Debug, Default)]
struct Node {
    tag: String,
    attrs: Vec<(String, String)>,
    parent: usize,
    children: Vec<usize>,
    /// Set for text nodes (`tag` is empty).
    text: Option<String>,
}

impl Node {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A parsed page.
#[derive(Debug, Default)]
pub struct Parsed {
    pub title: String,
    /// Main text, sanitized.
    pub text: String,
    pub meta: BTreeMap<String, String>,
    pub headings: Vec<String>,
    /// Absolute http(s) anchors, in page order, deduplicated.
    pub anchors: Vec<(String, String)>,
    pub tables: Vec<Vec<Vec<String>>>,
    nodes: Vec<Node>,
}

/// Parse `html`; relative anchors resolve against `base`.
pub fn parse_html(html: &str, base: &str) -> Parsed {
    let nodes = build(html);
    let mut p = Parsed {
        nodes,
        ..Parsed::default()
    };
    let base = reqwest::Url::parse(base).ok();
    p.title = p
        .find_all("title")
        .first()
        .map(|&i| clean_line(&p.raw_text(i), 200))
        .unwrap_or_default();
    // Meta tags.
    for i in 0..p.nodes.len() {
        let n = &p.nodes[i];
        match n.tag.as_str() {
            "meta" => {
                let key = n.attr("name").or_else(|| n.attr("property"));
                if let (Some(k), Some(v)) = (key, n.attr("content")) {
                    let k = k.to_ascii_lowercase();
                    if k == "description" || k.starts_with("og:") {
                        p.meta.entry(k).or_insert_with(|| clean_line(v, 300));
                    }
                }
            }
            "link"
                if n.attr("rel")
                    .is_some_and(|r| r.eq_ignore_ascii_case("canonical")) =>
            {
                if let Some(h) = n.attr("href") {
                    let abs = resolve(base.as_ref(), h).unwrap_or_default();
                    if !abs.is_empty() {
                        p.meta.entry("canonical".into()).or_insert(abs);
                    }
                }
            }
            _ => {}
        }
    }
    // Headings and tables.
    for tag in ["h1", "h2", "h3"] {
        for i in p.find_all(tag) {
            let t = clean_line(&p.body_text(i, true), 120);
            if !t.is_empty() {
                p.headings.push(t);
            }
        }
    }
    p.headings.truncate(30);
    for t in p.find_all("table").into_iter().take(3) {
        let mut rows = Vec::new();
        for r in p.descendants(t, "tr").into_iter().take(20) {
            let cells: Vec<String> = p
                .children_tagged(r, &["td", "th"])
                .into_iter()
                .take(8)
                .map(|c| clean_line(&p.body_text(c, true), 80))
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
        if !rows.is_empty() {
            p.tables.push(rows);
        }
    }
    // Anchors.
    let mut seen = std::collections::BTreeSet::new();
    for a in p.find_all("a") {
        let Some(h) = p.nodes[a].attr("href") else {
            continue;
        };
        let Some(abs) = resolve(base.as_ref(), h) else {
            continue;
        };
        if seen.insert(abs.clone()) {
            let text = clean_line(&p.body_text(a, true), 80);
            p.anchors.push((abs, text));
        }
        if p.anchors.len() >= 300 {
            break;
        }
    }
    // Main text: <main>, else <article>, else the body without chrome.
    let root = ["main", "article", "body"]
        .iter()
        .find_map(|t| p.find_all(t).first().copied())
        .unwrap_or(0);
    let chrome_out = !matches!(p.nodes[root].tag.as_str(), "main" | "article");
    p.text = clean_text(&p.body_text(root, chrome_out));
    p
}

/// Absolute, credential-free http(s) URL, or `None`.
fn resolve(base: Option<&reqwest::Url>, href: &str) -> Option<String> {
    let href = href.trim();
    let url = match base {
        Some(b) => b.join(href).ok()?,
        None => reqwest::Url::parse(href).ok()?,
    };
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let mut url = url;
    url.set_fragment(None);
    Some(url.to_string())
}

impl Parsed {
    fn find_all(&self, tag: &str) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| self.nodes[i].tag == tag)
            .collect()
    }

    fn descendants(&self, from: usize, tag: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![from];
        while let Some(i) = stack.pop() {
            for &c in self.nodes[i].children.iter().rev() {
                if self.nodes[c].tag == tag {
                    out.push(c);
                }
                stack.push(c);
            }
        }
        out.sort_unstable();
        out
    }

    fn children_tagged(&self, from: usize, tags: &[&str]) -> Vec<usize> {
        self.nodes[from]
            .children
            .iter()
            .copied()
            .filter(|&c| tags.contains(&self.nodes[c].tag.as_str()))
            .collect()
    }

    /// Text of `i` including hidden elements (the `<title>`).
    fn raw_text(&self, i: usize) -> String {
        let mut out = String::new();
        self.walk(i, false, false, 0, &mut out);
        out
    }

    /// Visible text under `i`.
    fn body_text(&self, i: usize, skip_chrome: bool) -> String {
        let mut out = String::new();
        self.walk(i, true, skip_chrome, 0, &mut out);
        out
    }

    fn walk(
        &self,
        i: usize,
        visible_only: bool,
        skip_chrome: bool,
        depth: usize,
        out: &mut String,
    ) {
        let n = &self.nodes[i];
        if let Some(t) = &n.text {
            out.push_str(t);
            return;
        }
        if depth > MAX_DEPTH + 2 {
            return;
        }
        if visible_only
            && depth > 0
            && (HIDDEN.contains(&n.tag.as_str())
                || n.attr("hidden").is_some()
                || n.attr("aria-hidden") == Some("true")
                || (skip_chrome && CHROME.contains(&n.tag.as_str())))
        {
            return;
        }
        let block = BLOCK.contains(&n.tag.as_str());
        if block && !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
        for &c in &n.children {
            self.walk(c, visible_only, skip_chrome, depth + 1, out);
        }
        if block && !out.ends_with('\n') {
            out.push('\n');
        }
    }

    /// Text of every element matching `selector` (a descendant chain of
    /// `tag`, `.class` and `#id` parts), at most `limit` hits of 300 chars.
    pub fn select(&self, selector: &str, limit: usize) -> Vec<String> {
        let chain: Vec<Simple> = selector
            .split_whitespace()
            .filter_map(Simple::parse)
            .collect();
        if chain.is_empty() || chain.len() != selector.split_whitespace().count() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 1..self.nodes.len() {
            if self.nodes[i].text.is_some() || !self.matches(i, &chain) {
                continue;
            }
            let t = clean_line(&self.body_text(i, false), 300);
            if !t.is_empty() {
                out.push(t);
            }
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    fn matches(&self, i: usize, chain: &[Simple]) -> bool {
        let Some((last, rest)) = chain.split_last() else {
            return true;
        };
        if !last.matches(&self.nodes[i]) {
            return false;
        }
        if rest.is_empty() {
            return true;
        }
        let mut anc = self.nodes[i].parent;
        let mut want = rest.len();
        let mut steps = 0;
        while anc != 0 && want > 0 && steps < MAX_DEPTH + 2 {
            if rest[want - 1].matches(&self.nodes[anc]) {
                want -= 1;
            }
            anc = self.nodes[anc].parent;
            steps += 1;
        }
        want == 0
    }
}

struct Simple {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

impl Simple {
    fn parse(s: &str) -> Option<Self> {
        if s.len() > 100 || s.is_empty() {
            return None;
        }
        let mut out = Simple {
            tag: None,
            id: None,
            classes: Vec::new(),
        };
        let mut cur = String::new();
        let mut kind = 't';
        let flush = |out: &mut Simple, kind: char, cur: &mut String| -> bool {
            if cur.is_empty() {
                return kind == 't';
            }
            let v = std::mem::take(cur);
            match kind {
                't' => out.tag = Some(v.to_ascii_lowercase()),
                '#' => out.id = Some(v),
                _ => out.classes.push(v),
            }
            true
        };
        for ch in s.chars() {
            match ch {
                '.' | '#' => {
                    if !flush(&mut out, kind, &mut cur) {
                        return None;
                    }
                    kind = ch;
                }
                c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => cur.push(c),
                '*' if cur.is_empty() && kind == 't' => {}
                _ => return None,
            }
        }
        if !flush(&mut out, kind, &mut cur) {
            return None;
        }
        Some(out)
    }

    fn matches(&self, n: &Node) -> bool {
        n.text.is_none()
            && self.tag.as_ref().is_none_or(|t| &n.tag == t)
            && self.id.as_ref().is_none_or(|i| n.attr("id") == Some(i))
            && self.classes.iter().all(|c| {
                n.attr("class")
                    .is_some_and(|v| v.split_whitespace().any(|x| x == c))
            })
    }
}

/// Tokenize and build the tree. Node 0 is a synthetic root.
fn build(html: &str) -> Vec<Node> {
    let mut nodes = vec![Node::default()];
    let mut stack: Vec<usize> = vec![0];
    let b = html.as_bytes();
    let mut i = 0;
    let push_text = |nodes: &mut Vec<Node>, stack: &[usize], t: &str| {
        if t.is_empty() || nodes.len() >= MAX_NODES {
            return;
        }
        let parent = *stack.last().unwrap_or(&0);
        let id = nodes.len();
        nodes.push(Node {
            parent,
            text: Some(decode_entities(t)),
            ..Node::default()
        });
        nodes[parent].children.push(id);
    };
    while i < b.len() && nodes.len() < MAX_NODES {
        let Some(lt) = html[i..].find('<').map(|p| p + i) else {
            push_text(&mut nodes, &stack, &html[i..]);
            break;
        };
        push_text(&mut nodes, &stack, &html[i..lt]);
        i = lt;
        let rest = &html[i..];
        if let Some(r) = rest.strip_prefix("<!--") {
            i += 4 + r.find("-->").map_or(r.len(), |p| p + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i += rest.find('>').map_or(rest.len(), |p| p + 1);
            continue;
        }
        let closing = rest.starts_with("</");
        let name_start = if closing { 2 } else { 1 };
        let name: String = rest[name_start.min(rest.len())..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
            .collect::<String>()
            .to_ascii_lowercase();
        if name.is_empty() {
            // A stray "<": text.
            push_text(&mut nodes, &stack, "<");
            i += 1;
            continue;
        }
        let (end, attrs, self_closing) = parse_tag(&rest[name_start + name.len()..]);
        i += name_start + name.len() + end;
        if closing {
            if let Some(pos) = stack.iter().rposition(|&n| nodes[n].tag == name)
                && pos > 0
            {
                stack.truncate(pos);
            }
            continue;
        }
        let parent = *stack.last().unwrap_or(&0);
        let id = nodes.len();
        nodes.push(Node {
            tag: name.clone(),
            attrs,
            parent,
            ..Node::default()
        });
        nodes[parent].children.push(id);
        // Raw-text elements: swallow up to the closing tag.
        if matches!(name.as_str(), "script" | "style" | "textarea" | "title") && !self_closing {
            let lower_rest = html[i..].to_ascii_lowercase();
            let close = format!("</{name}");
            let end = lower_rest.find(&close).unwrap_or(lower_rest.len());
            let body = &html[i..i + end];
            if name == "title" || name == "textarea" {
                push_text(&mut nodes, &[id], body);
            }
            i += end;
            if let Some(gt) = html[i..].find('>') {
                i += gt + 1;
            } else {
                i = html.len();
            }
            continue;
        }
        if !VOID.contains(&name.as_str()) && !self_closing && stack.len() < MAX_DEPTH {
            // <p>, <li> and friends close an open sibling.
            if matches!(name.as_str(), "p" | "li" | "tr" | "td" | "th" | "dt" | "dd")
                && let Some(&top) = stack.last()
                && nodes[top].tag == name
            {
                stack.pop();
                let np = *stack.last().unwrap_or(&0);
                nodes[parent].children.retain(|&c| c != id);
                nodes[id].parent = np;
                nodes[np].children.push(id);
            }
            stack.push(id);
        }
    }
    nodes
}

/// Attributes up to the closing `>`: (bytes consumed, attrs, `/>`).
fn parse_tag(s: &str) -> (usize, Vec<(String, String)>, bool) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut attrs = Vec::new();
    let mut self_closing = false;
    while i < b.len() {
        match b[i] {
            b'>' => return (i + 1, attrs, self_closing),
            b'/' => {
                self_closing = b.get(i + 1) == Some(&b'>');
                i += 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b'=' | b'>' | b'/')
                {
                    i += 1;
                }
                if i == start {
                    i += 1;
                    continue;
                }
                let key = s[start..i].to_ascii_lowercase();
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                let mut val = String::new();
                if b.get(i) == Some(&b'=') {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    match b.get(i) {
                        Some(&q @ (b'"' | b'\'')) => {
                            let vs = i + 1;
                            let ve = s[vs..].find(q as char).map_or(s.len(), |p| p + vs);
                            val = s[vs..ve].to_owned();
                            i = (ve + 1).min(s.len());
                        }
                        _ => {
                            let vs = i;
                            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                                i += 1;
                            }
                            val = s[vs..i].to_owned();
                        }
                    }
                }
                if attrs.len() < 32 {
                    attrs.push((key, decode_entities(&val)));
                }
            }
        }
    }
    (s.len(), attrs, self_closing)
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(semi) = crate::text::prefix_bytes(rest, 12).find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" => Some('\u{2013}'),
            "mdash" => Some('\u{2014}'),
            "hellip" => Some('\u{2026}'),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Plain text of a non-HTML body.
pub fn plain_text(body: &[u8]) -> String {
    clean_text(&String::from_utf8_lossy(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<!doctype html><html><head><title>Pricing &amp; plans</title>
<meta name="description" content="All the plans">
<meta property="og:title" content="OG">
<link rel="canonical" href="/pricing">
<script>var x = "<p>not content</p>";</script></head>
<body><nav><a href="/home">Home</a></nav>
<main><h1>Plans</h1><p>The Pro plan costs <b>$29/mo</b>.</p>
<table class="pricing"><tr><th>Plan</th><th>Price</th></tr><tr><td>Pro</td><td>$29</td></tr></table>
<a href="https://other.test/x#frag">Other</a> <a href="javascript:alert(1)">bad</a>
<a href="https://u:p@evil.test/">creds</a><a href="mailto:a@b.c">mail</a>
<div class="a b" id="x"><span class="price">$29</span></div></main>
<footer>Footer text</footer></body></html>"#;

    #[test]
    fn extracts_structure() {
        let p = parse_html(PAGE, "https://ex.test/docs/page");
        assert_eq!(p.title, "Pricing & plans");
        assert_eq!(p.meta["description"], "All the plans");
        assert_eq!(p.meta["canonical"], "https://ex.test/pricing");
        assert_eq!(p.headings, ["Plans"]);
        assert!(p.text.contains("The Pro plan costs $29/mo."), "{}", p.text);
        assert!(!p.text.contains("not content") && !p.text.contains("Footer"));
        assert_eq!(p.tables[0], vec![vec!["Plan", "Price"], vec!["Pro", "$29"]]);
        let urls: Vec<&str> = p.anchors.iter().map(|a| a.0.as_str()).collect();
        assert_eq!(urls, ["https://ex.test/home", "https://other.test/x"]);
    }

    #[test]
    fn entity_lookahead_never_splits_a_multibyte_char() {
        // 10 ASCII bytes then a 4-byte emoji: byte 12 is inside it.
        let html = "<p>&aaaaaaaaa💫 and 💫&x💫💫💫 &amp; &#128171;</p>";
        let p = parse_html(html, "https://ex.test/");
        assert!(p.text.contains("💫"), "{}", p.text);
        assert!(p.text.contains("& 💫"), "{}", p.text);
        for prefix in 0..20 {
            let s: String = "a".repeat(prefix) + "&" + "💫".repeat(4).as_str();
            assert_eq!(decode_entities(&s), s);
        }
        assert_eq!(decode_entities("&amp;💫&gt;"), "&💫>");
    }

    #[test]
    fn selectors() {
        let p = parse_html(PAGE, "https://ex.test/");
        assert_eq!(p.select("h1", 5), ["Plans"]);
        assert_eq!(p.select("div.a#x .price", 5), ["$29"]);
        assert_eq!(p.select("table.pricing td", 5), ["Pro", "$29"]);
        assert!(
            p.select("a[href]", 5).is_empty(),
            "unsupported syntax finds nothing"
        );
    }

    #[test]
    fn survives_hostile_markup() {
        let deep = "<div>".repeat(5000) + "deep" + &"</div>".repeat(5000);
        let p = parse_html(&deep, "https://ex.test/");
        assert!(p.text.contains("deep") || p.text.is_empty());
        let junk = "<a href='x".repeat(1000) + "<<<>>><!-- unterminated";
        let _ = parse_html(&junk, "https://ex.test/");
        let _ = parse_html("<title>x", "https://ex.test/");
        let _ = parse_html("&#xFFFFFFFF; &#0; &;", "https://ex.test/");
    }
}
