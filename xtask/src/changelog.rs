//! `cargo xtask changelog`: gather `changelog.d/` fragments into `CHANGELOG.md`.
//!
//! A fragment is `changelog.d/<slug>.<kind>.md` holding one or more markdown
//! bullets. Releasing folds them (plus anything already under `[Unreleased]`)
//! into a new version section and deletes them.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

pub const USAGE: &str = "usage: cargo xtask changelog <version> [--date YYYY-MM-DD] [--dry-run]
       cargo xtask changelog --check

Gathers changelog.d/<slug>.<kind>.md into a new `## [<version>] - <date>`
section of CHANGELOG.md, then deletes the fragments. --check only validates.";

/// Keep a Changelog section order.
const KINDS: [&str; 6] = [
    "added",
    "changed",
    "deprecated",
    "removed",
    "fixed",
    "security",
];

#[derive(Debug, PartialEq, Eq)]
pub struct Fragment {
    pub name: String,
    pub kind: &'static str,
    pub body: String,
}

/// Parse `<slug>.<kind>.md` and check the body. `Ok(None)` for files that
/// aren't fragments (the README, dotfiles).
pub fn parse_fragment(name: &str, body: &str) -> Result<Option<Fragment>, String> {
    if name.starts_with('.') || name == "README.md" {
        return Ok(None);
    }
    let bad = |why: &str| format!("changelog.d/{name}: {why}");
    let stem = name
        .strip_suffix(".md")
        .ok_or_else(|| bad("name must be <slug>.<kind>.md"))?;
    let (slug, kind) = stem
        .rsplit_once('.')
        .ok_or_else(|| bad("name must be <slug>.<kind>.md"))?;
    let slug_ok = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if !slug_ok {
        return Err(bad("slug must be lowercase letters, digits, - or _"));
    }
    let kind = KINDS
        .iter()
        .find(|k| **k == kind)
        .ok_or_else(|| bad(&format!("unknown kind `{kind}` (use {})", KINDS.join(", "))))?;
    let body = body.trim();
    if body.is_empty() {
        return Err(bad("is empty"));
    }
    if !body.starts_with("- ") {
        return Err(bad("must start with a `- ` bullet"));
    }
    Ok(Some(Fragment {
        name: name.to_owned(),
        kind,
        body: body.to_owned(),
    }))
}

/// Read and validate every fragment in `dir`, sorted by file name. All
/// problems are reported together.
pub fn read_fragments(dir: &Path) -> Result<Vec<Fragment>, Vec<String>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let (mut frags, mut errors) = (Vec::new(), Vec::new());
    for name in names {
        match fs::read_to_string(dir.join(&name)) {
            Ok(body) => match parse_fragment(&name, &body) {
                Ok(Some(f)) => frags.push(f),
                Ok(None) => {}
                Err(e) => errors.push(e),
            },
            Err(e) => errors.push(format!("changelog.d/{name}: {e}")),
        }
    }
    if errors.is_empty() {
        Ok(frags)
    } else {
        Err(errors)
    }
}

/// Split bullets out of an `[Unreleased]` body, by section. Continuation
/// lines stay with their bullet.
fn parse_unreleased(block: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut kind: Option<&'static str> = None;
    for line in block.lines() {
        if let Some(title) = line.strip_prefix("### ") {
            let title = title.trim().to_ascii_lowercase();
            kind = KINDS.iter().find(|k| **k == title).copied();
            continue;
        }
        let Some(k) = kind else { continue };
        if line.starts_with("- ") {
            out.push((k, line.to_owned()));
        } else if let Some((_, last)) = out.last_mut()
            && !line.trim().is_empty()
        {
            last.push('\n');
            last.push_str(line);
        }
    }
    out
}

/// The new `CHANGELOG.md` text: the version section goes under `[Unreleased]`,
/// which ends up empty. Existing Unreleased bullets come first in each section,
/// then fragments by file name.
pub fn render(
    changelog: &str,
    frags: &[Fragment],
    version: &str,
    date: &str,
) -> Result<String, String> {
    let marker = "## [Unreleased]";
    let start = changelog
        .find(&format!("\n{marker}"))
        .map(|i| i + 1)
        .or_else(|| changelog.starts_with(marker).then_some(0))
        .ok_or("CHANGELOG.md has no `## [Unreleased]` heading")?;
    let after = start + marker.len();
    let end = changelog[after..]
        .find("\n## [")
        .map_or(changelog.len(), |i| after + i + 1);
    if changelog.contains(&format!("\n## [{version}]")) {
        return Err(format!("CHANGELOG.md already has a `[{version}]` section"));
    }

    let mut items = parse_unreleased(&changelog[after..end]);
    let mut sorted: Vec<&Fragment> = frags.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    items.extend(sorted.into_iter().map(|f| (f.kind, f.body.clone())));
    if items.is_empty() {
        return Err("nothing to release: no fragments and nothing under [Unreleased]".into());
    }

    let mut section = format!("## [{version}] - {date}\n");
    for kind in KINDS {
        let bullets: Vec<&str> = items
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, b)| b.as_str())
            .collect();
        if bullets.is_empty() {
            continue;
        }
        let mut title = kind.to_owned();
        title[..1].make_ascii_uppercase();
        let _ = write!(section, "\n### {title}\n\n{}\n", bullets.join("\n"));
    }

    let mut out = changelog[..after].to_owned();
    out.push_str("\n\n");
    out.push_str(&section);
    if end < changelog.len() {
        out.push('\n');
        out.push_str(&changelog[end..]);
    }
    Ok(out)
}

/// Today (UTC) as YYYY-MM-DD, std only.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let (mut version, mut date, mut check, mut dry) = (None, None, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" => check = true,
            "--dry-run" => dry = true,
            "--date" => date = Some(it.next().ok_or("--date needs a value")?.clone()),
            v if !v.starts_with('-') && version.is_none() => version = Some(v.to_owned()),
            _ => return Err(USAGE.into()),
        }
    }
    let dir = root.join("changelog.d");
    let frags = read_fragments(&dir).map_err(|errs| errs.join("\n"))?;
    if check {
        eprintln!("changelog.d: {} fragment(s) ok", frags.len());
        return Ok(());
    }
    let version = version.ok_or(USAGE)?;
    let version = version.strip_prefix('v').unwrap_or(&version);
    let date = date.unwrap_or_else(today);
    if !valid_date(&date) {
        return Err(format!("bad --date `{date}` (use YYYY-MM-DD)"));
    }
    let path = root.join("CHANGELOG.md");
    let old = fs::read_to_string(&path).map_err(|e| format!("CHANGELOG.md: {e}"))?;
    let new = render(&old, &frags, version, &date)?;
    if dry {
        print!("{new}");
        return Ok(());
    }
    fs::write(&path, new).map_err(|e| format!("CHANGELOG.md: {e}"))?;
    for f in &frags {
        fs::remove_file(dir.join(&f.name)).map_err(|e| format!("{}: {e}", f.name))?;
    }
    eprintln!(
        "CHANGELOG.md: added [{version}] from {} fragment(s); review and commit",
        frags.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(name: &str, body: &str) -> Fragment {
        parse_fragment(name, body).unwrap().unwrap()
    }

    const BASE: &str = "# Changelog\n\nintro\n\n## [Unreleased]\n\n### Fixed\n\n- old fix\n\n## [0.1.0] - 2026-01-01\n\n### Added\n\n- first\n";

    #[test]
    fn orders_sections_and_sorts_by_file_name() {
        let frags = [
            frag("a-thing.fixed.md", "- fix a\n"),
            frag("b-thing.added.md", "- add b\n"),
            frag("a-thing.added.md", "- add a\n- add a2\n"),
            frag("c-thing.security.md", "- sec\n"),
        ];
        let out = render("## [Unreleased]\n", &frags, "0.2.0", "2026-02-02").unwrap();
        assert_eq!(
            out,
            "## [Unreleased]\n\n## [0.2.0] - 2026-02-02\n\n### Added\n\n- add a\n- add a2\n- add b\n\n### Fixed\n\n- fix a\n\n### Security\n\n- sec\n"
        );
    }

    #[test]
    fn folds_unreleased_and_keeps_older_versions() {
        let frags = [frag("x.fixed.md", "- new fix")];
        let out = render(BASE, &frags, "0.2.0", "2026-02-02").unwrap();
        assert!(out.contains(
            "## [Unreleased]\n\n## [0.2.0] - 2026-02-02\n\n### Fixed\n\n- old fix\n- new fix\n\n## [0.1.0] - 2026-01-01\n"
        ));
        assert!(out.ends_with("- first\n"));
    }

    #[test]
    fn folds_multiline_unreleased_bullets() {
        let cl = "## [Unreleased]\n\n### Added\n\n- one\n  more\n\n## [0.1.0] - x\n";
        let out = render(cl, &[], "0.2.0", "2026-02-02").unwrap();
        assert!(out.contains("### Added\n\n- one\n  more\n\n## [0.1.0]"));
    }

    #[test]
    fn rejects_bad_kind() {
        let e = parse_fragment("x.oops.md", "- hi").unwrap_err();
        assert!(e.contains("unknown kind `oops`"), "{e}");
        assert!(parse_fragment("nokind.md", "- hi").is_err());
    }

    #[test]
    fn rejects_empty_and_non_bullet() {
        assert!(
            parse_fragment("x.added.md", " \n")
                .unwrap_err()
                .contains("empty")
        );
        assert!(parse_fragment("x.added.md", "text").is_err());
    }

    #[test]
    fn skips_readme_and_refuses_empty_release() {
        assert_eq!(parse_fragment("README.md", "").unwrap(), None);
        assert!(render("## [Unreleased]\n", &[], "1.0.0", "2026-01-01").is_err());
    }

    #[test]
    fn refuses_duplicate_version() {
        let frags = [frag("x.added.md", "- a")];
        assert!(render(BASE, &frags, "0.1.0", "2026-02-02").is_err());
    }

    #[test]
    fn dates() {
        assert!(valid_date("2026-11-06") && !valid_date("2026-1-06"));
        assert_eq!(today().len(), 10);
    }
}
