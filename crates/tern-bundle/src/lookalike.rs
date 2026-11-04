//! Hosts that imitate other sites (wargame W2: a shared "Notion" connector
//! that also reaches `notion-api.co`). The allowlist already bounds where a
//! connector can send anything; this makes a suspicious entry in it
//! impossible to miss at install. It is a heuristic and errs on the side of
//! saying something: a warning on an honest host costs a second look.

use tern_auth::domain::registrable;

/// Well-known services: a name people recognise and the sites that are
/// really theirs.
const KNOWN: &[(&str, &[&str])] = &[
    ("airtable", &["airtable.com"]),
    ("amazon", &["amazon.com", "amazonaws.com"]),
    ("anthropic", &["anthropic.com", "claude.ai"]),
    ("apple", &["apple.com", "icloud.com"]),
    ("asana", &["asana.com"]),
    ("atlassian", &["atlassian.com", "atlassian.net"]),
    ("discord", &["discord.com", "discordapp.com"]),
    ("dropbox", &["dropbox.com", "dropboxapi.com"]),
    ("figma", &["figma.com"]),
    (
        "github",
        &["github.com", "githubusercontent.com", "github.io"],
    ),
    ("gitlab", &["gitlab.com"]),
    ("gmail", &["google.com", "googleapis.com", "gmail.com"]),
    (
        "google",
        &[
            "google.com",
            "googleapis.com",
            "googleusercontent.com",
            "gmail.com",
        ],
    ),
    ("hubspot", &["hubspot.com", "hubapi.com"]),
    ("linear", &["linear.app"]),
    (
        "microsoft",
        &[
            "microsoft.com",
            "microsoftonline.com",
            "live.com",
            "office.com",
            "office365.com",
        ],
    ),
    ("notion", &["notion.so", "notion.com"]),
    ("ntfy", &["ntfy.sh"]),
    ("openai", &["openai.com"]),
    (
        "outlook",
        &["outlook.com", "office.com", "microsoft.com", "live.com"],
    ),
    ("paypal", &["paypal.com"]),
    ("salesforce", &["salesforce.com", "force.com"]),
    ("shopify", &["shopify.com", "myshopify.com"]),
    ("slack", &["slack.com"]),
    ("stripe", &["stripe.com"]),
    ("telegram", &["telegram.org"]),
    ("todoist", &["todoist.com"]),
    ("trello", &["trello.com"]),
    ("twilio", &["twilio.com"]),
    ("zoom", &["zoom.us"]),
];

/// The host of an allowlist entry: `*.notion.so/v1/*` → `notion.so`.
pub fn entry_host(entry: &str) -> String {
    let host = entry.split('/').next().unwrap_or_default();
    host.trim_start_matches("*.").to_ascii_lowercase()
}

/// Warnings about the hosts a connector named `connector` may reach.
pub fn check(connector: &str, entries: &[String]) -> Vec<String> {
    let hosts: Vec<String> = {
        let mut h: Vec<String> = entries.iter().map(|e| entry_host(e)).collect();
        h.sort();
        h.dedup();
        h
    };
    let mut out = Vec::new();
    for host in &hosts {
        if host.split('.').any(|l| l.starts_with("xn--")) || !host.is_ascii() {
            out.push(format!(
                "{host} uses an internationalized (punycode) name, which can be made to look like another site"
            ));
            continue;
        }
        let Some(site) = registrable(host) else {
            if host.parse::<std::net::IpAddr>().is_ok() {
                out.push(format!("{host} is a bare IP address, not a named site"));
            }
            continue;
        };
        if let Some(w) = imitates(host, &site) {
            out.push(w);
        }
    }
    // A connector named after a service that also reaches somewhere that
    // isn't that service's.
    let name = connector.to_ascii_lowercase();
    if let Some((brand, sites)) = KNOWN.iter().find(|(b, _)| name.contains(b)) {
        for host in &hosts {
            if registrable(host).is_none() {
                continue;
            }
            let already = out.iter().any(|w| w.starts_with(host.as_str()));
            if !already && !on(host, sites) {
                out.push(format!(
                    "{host} isn't {brand}'s site ({}), though the connector is named {connector}: check why it needs it",
                    sites.join(", ")
                ));
            }
        }
    }
    // Two hosts of one connector a letter or two apart (notion.so and
    // notlon.so), for services this list doesn't know.
    let sites: Vec<String> = {
        let mut s: Vec<String> = hosts.iter().filter_map(|h| registrable(h)).collect();
        s.sort();
        s.dedup();
        s
    };
    for (i, a) in sites.iter().enumerate() {
        for b in &sites[i + 1..] {
            let (la, lb) = (label(a), label(b));
            if la != lb && la.len() >= 4 && lb.len() >= 4 && distance(&la, &lb) <= 2 {
                let w = format!("{a} and {b} look alike but are different sites");
                if !out
                    .iter()
                    .any(|o| o.contains(a.as_str()) && o.contains(b.as_str()))
                {
                    out.push(w);
                }
            }
        }
    }
    out
}

/// Whether `host` is on one of `sites` (or a subdomain). By suffix, not
/// registrable domain: the public suffix list's private section makes
/// `googleapis.com` and `github.io` suffixes, so `gmail.googleapis.com` is
/// its own "site" there.
fn on(host: &str, sites: &[&str]) -> bool {
    sites
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// Whether `host` (on site `site`) looks like a known service it isn't.
fn imitates(host: &str, site: &str) -> Option<String> {
    if KNOWN.iter().any(|(_, sites)| on(host, sites)) {
        return None;
    }
    let site_label = label(site);
    let plain = deconfuse(&site_label);
    for (brand, sites) in KNOWN {
        let near = site_label.len() >= 4
            && distance(&site_label, brand) <= if brand.len() >= 6 { 2 } else { 1 };
        let named =
            host.split(['.', '-']).any(|part| deconfuse(part) == *brand) || plain.contains(brand);
        if near || named {
            return Some(format!(
                "{host} looks like {brand} ({}) but is {site}, a different site",
                sites.join(", ")
            ));
        }
    }
    None
}

/// `notion-api.co` → `notion-api`; `foo.co.uk` → `foo`.
fn label(site: &str) -> String {
    site.split('.').next().unwrap_or_default().to_owned()
}

/// Undo the usual digit-for-letter swaps: `n0t1on` → `notlon`, `paypa1` → `paypal`.
fn deconfuse(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '0' => 'o',
            '1' => 'l',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '7' => 't',
            '$' => 's',
            c => c,
        })
        .collect()
}

/// Levenshtein distance.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn honest_hosts_pass() {
        assert!(check("notion", &v(&["api.notion.com", "*.notion.so/v1/*"])).is_empty());
        assert!(check("github", &v(&["api.github.com/repos/*"])).is_empty());
        assert!(check("gmail", &v(&["gmail.googleapis.com"])).is_empty());
        assert!(check("google_calendar", &v(&["www.googleapis.com/calendar/v3/*"])).is_empty());
        assert!(check("weather", &v(&["api.open-meteo.com"])).is_empty());
    }

    #[test]
    fn lookalikes_are_flagged() {
        let w = check("notion", &v(&["api.notion.com", "notion-api.co"]));
        assert!(
            w.iter()
                .any(|w| w.contains("notion-api.co") && w.contains("different site")),
            "{w:?}"
        );
        assert!(!check("x", &v(&["api.n0tion.so"])).is_empty());
        assert!(!check("x", &v(&["api.notlon.so"])).is_empty());
        assert!(!check("x", &v(&["github.com.evil.io"])).is_empty());
        assert!(!check("x", &v(&["paypa1.com"])).is_empty());
        assert!(!check("x", &v(&["xn--ntion-6ra.so"])).is_empty());
        // A Notion connector reaching an unrelated site.
        let w = check("notion", &v(&["api.notion.com", "collect.example.net"]));
        assert!(
            w.iter()
                .any(|w| w.contains("collect.example.net") && w.contains("notion")),
            "{w:?}"
        );
        // Unknown services a letter apart.
        let w = check("acme", &v(&["api.acmecorp.io", "api.acmec0rp.io"]));
        assert!(w.iter().any(|w| w.contains("look alike")), "{w:?}");
    }
}
