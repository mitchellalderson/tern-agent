//! Seeded fuzz loops: random Unicode, HTML-ish and byte garbage through the
//! extract, sanitize and validation paths must never panic (a hostile page
//! is untrusted input, so a panic here is a remote crash).

use serde_json::json;

use super::extract::{parse_html, plain_text};
use super::sanitize::{clean_line, clean_text, cut};
use super::schema::*;

const FRAGMENTS: &[&str] = &[
    "<",
    ">",
    "</",
    "/>",
    "<!--",
    "-->",
    "<!",
    "<?",
    "&",
    ";",
    "&amp;",
    "&#x",
    "&#",
    "&#128171;",
    "&#xFFFFFFFF;",
    "&💫;",
    "&#💫",
    "=",
    "\"",
    "'",
    "<a href=\"",
    "<a href=",
    "</a>",
    "<p>",
    "<li>",
    "<script>",
    "</script",
    "<style>",
    "<title>",
    "</title",
    "<textarea>",
    "<table><tr><td>",
    "<h1>",
    "</h1>",
    "<meta name=\"description\" content=\"",
    "<untrusted>",
    "</ data>",
    "💫",
    "日本語",
    "e\u{301}",
    "\u{200b}",
    "\u{202e}",
    "\u{0}",
    "\u{fffd}",
    "👨\u{200d}👩",
    "\n",
    " ",
    "a",
    "Z",
    "0",
];

fn random_html(rng: &mut fastrand::Rng) -> String {
    let n = rng.usize(0..60);
    let mut s = String::new();
    for _ in 0..n {
        match rng.u8(0..4) {
            0 => s.push(rng.char(..)),
            _ => s.push_str(FRAGMENTS[rng.usize(0..FRAGMENTS.len())]),
        }
    }
    s
}

#[test]
fn extract_survives_garbage() {
    let mut rng = fastrand::Rng::with_seed(0x7e12);
    for _ in 0..3000 {
        let h = random_html(&mut rng);
        let p = parse_html(&h, "https://ex.test/a/b");
        let _ = p.select("div.a#x span", 3);
        let _ = p.select(&random_html(&mut rng), 3);
        let _ = clean_text(&h);
        let _ = clean_line(&h, rng.usize(0..40));
        let c = cut(&h, rng.usize(0..h.len() + 3));
        assert!(h.starts_with(c));
    }
}

#[test]
fn plain_text_survives_arbitrary_bytes() {
    let mut rng = fastrand::Rng::with_seed(0x5eed);
    for _ in 0..3000 {
        let n = rng.usize(0..80);
        let bytes: Vec<u8> = (0..n).map(|_| rng.u8(..)).collect();
        let _ = plain_text(&bytes);
        let lossy = String::from_utf8_lossy(&bytes).into_owned();
        let _ = parse_html(&lossy, "not a url");
    }
}

#[test]
fn validation_survives_garbage_quotes_and_offsets() {
    let mut rng = fastrand::Rng::with_seed(0xfa57);
    let anchors: Vec<(String, String)> = vec![("https://ex.test/a".into(), "💫".into())];
    for _ in 0..3000 {
        let page = random_html(&mut rng);
        let text = clean_text(&parse_html(&page, "https://ex.test/").text);
        let view = [PageView {
            text: &text,
            anchors: &anchors,
        }];
        // A quote lifted from the page at arbitrary byte positions, or pure
        // garbage; offsets anywhere, including inside multibyte characters.
        let quote = if text.is_empty() || rng.bool() {
            random_html(&mut rng)
        } else {
            let a = rng.usize(0..text.len());
            let b = rng.usize(a..=text.len());
            String::from_utf8_lossy(&text.as_bytes()[a..b]).into_owned()
        };
        let value: String = quote.chars().take(rng.usize(0..8)).collect();
        let offset = match rng.u8(0..3) {
            0 => json!(null),
            1 => json!([rng.usize(0..text.len() + 4), rng.usize(0..text.len() + 4)]),
            _ => json!([rng.usize(..), rng.usize(..)]),
        };
        let out: ModelOutput = serde_json::from_value(json!({
            "facts": [{"claim_key": "k", "value": value, "quote": quote, "offset": offset,
                       "page": rng.usize(0..3)}],
            "answer": random_html(&mut rng),
            "notes": random_html(&mut rng),
            "links": [{"url": "https://ex.test/a"}],
        }))
        .unwrap();
        let v = validate(out, &view, rng.bool());
        for f in &v.facts {
            // Whatever survives is a verbatim quote at valid, on-boundary offsets.
            assert_eq!(text.get(f.offset[0]..f.offset[1]), Some(f.quote.as_str()));
        }
        let _ = parse_model_output(&random_html(&mut rng));
    }
}

fn sample_result(text: String) -> FetchResult {
    FetchResult {
        source: Source {
            url: "https://ex.test/".into(),
            host: "ex.test".into(),
            status: 200,
            fetched_at: "t".into(),
            content_type: None,
            bytes: 1,
        },
        truncated: false,
        mode: "extract".into(),
        title: "t".into(),
        structured: None,
        facts: Vec::new(),
        answer: None,
        text: Some(text),
        links: Vec::new(),
        confidence: None,
        notes: None,
    }
}

#[test]
fn fit_truncates_on_char_boundaries() {
    for text in ["💫".repeat(500), "日本".repeat(300), "e\u{301}".repeat(400)] {
        let mut r = sample_result(text);
        r.fit(300);
        assert!(r.size() <= 300, "{}", r.size());
        assert!(r.truncated);
    }
}
