//! Skills (spec §9.4): `~/.tern/skills/<dir>/SKILL.md` with `name` and
//! `description` frontmatter, in the agentskills format. Only the index goes
//! in the system prompt; `skill_read` loads a skill's body on demand.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

/// Every valid skill under `root`, sorted by name. Invalid ones are skipped
/// with a warning.
pub fn discover(root: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut skills: Vec<Skill> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let dir = e.path();
            let text = std::fs::read_to_string(dir.join("SKILL.md")).ok()?;
            match parse(&text) {
                Some((name, description, _)) => Some(Skill { name, description, dir }),
                None => {
                    tracing::warn!(dir = %dir.display(), "SKILL.md needs `name` and `description` frontmatter");
                    None
                }
            }
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Split `SKILL.md` into (name, description, body). Supports the simple
/// `key: value` frontmatter skills use, with optional quotes.
pub fn parse(text: &str) -> Option<(String, String, String)> {
    let rest = text.strip_prefix("---")?.trim_start_matches(['\r', '\n']);
    let end = rest.find("\n---")?;
    let (front, body) = (&rest[..end], &rest[end + 4..]);
    let body = body
        .trim_start_matches(['-'])
        .trim_start_matches(['\r', '\n']);
    let mut name = None;
    let mut description = None;
    for line in front.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim().trim_matches(|c| c == '"' || c == '\'').to_owned();
        match k.trim() {
            "name" => name = Some(v),
            "description" => description = Some(v),
            _ => {}
        }
    }
    let (name, description) = (
        name.filter(|n| !n.is_empty())?,
        description.filter(|d| !d.is_empty())?,
    );
    Some((name, description, body.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let (n, d, b) = parse("---\nname: pdf\ndescription: \"Work with PDFs: merge, split\"\nlicense: MIT\n---\n# PDF\nUse qpdf.\n").unwrap();
        assert_eq!(
            (n.as_str(), d.as_str()),
            ("pdf", "Work with PDFs: merge, split")
        );
        assert_eq!(b, "# PDF\nUse qpdf.\n");
        assert!(parse("# no frontmatter").is_none());
        assert!(
            parse("---\nname: x\n---\nbody").is_none(),
            "description is required"
        );
    }

    #[test]
    fn discovers_skill_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        for (dir, body) in [
            ("b-skill", "---\nname: beta\ndescription: second\n---\nB"),
            ("a-skill", "---\nname: alpha\ndescription: first\n---\nA"),
            ("broken", "no frontmatter"),
        ] {
            std::fs::create_dir(tmp.path().join(dir)).unwrap();
            std::fs::write(tmp.path().join(dir).join("SKILL.md"), body).unwrap();
        }
        std::fs::write(tmp.path().join("stray.md"), "x").unwrap();
        let names: Vec<_> = discover(tmp.path()).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["alpha", "beta"]);
        assert!(discover(&tmp.path().join("missing")).is_empty());
    }
}
