//! Skills (spec §9.4): `~/.tern/skills/<dir>/SKILL.md` with `name` and
//! `description` frontmatter, in the agentskills format. Only the index goes
//! in the system prompt; `skill_read` loads a skill's body on demand.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
    /// Captured from a run and not yet published (`draft: true`).
    pub draft: bool,
}

/// Every published skill under `root`, sorted by name: what the system
/// prompt lists and `skill_read` loads. Drafts (T5b) and invalid skills
/// are left out; invalid ones are logged.
pub fn discover(root: &Path) -> Vec<Skill> {
    let mut skills = discover_all(root);
    skills.retain(|s| !s.draft);
    skills
}

/// Published skills and drafts, sorted by name.
pub fn discover_all(root: &Path) -> Vec<Skill> {
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
                Some((name, description, _)) => Some(Skill {
                    name,
                    description,
                    dir,
                    draft: is_draft(&text),
                }),
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

/// Whether `SKILL.md` text is an unpublished draft: `draft: true` in its
/// frontmatter.
pub fn is_draft(text: &str) -> bool {
    frontmatter_lines(text).is_some_and(|front| front.lines().any(is_draft_line))
}

fn is_draft_line(line: &str) -> bool {
    line.split_once(':')
        .is_some_and(|(k, v)| k.trim() == "draft" && v.trim() == "true")
}

fn frontmatter_lines(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?.trim_start_matches(['\r', '\n']);
    Some(&rest[..rest.find("\n---")?])
}

#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("no draft skill named {0:?}")]
    NotFound(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Save a captured draft as `skills/<slug>/SKILL.md`, in a directory no
/// existing skill uses (`slug`, then `slug-2`, ...). Returns the directory
/// name, which is also the draft's `name`.
pub fn write_draft(root: &Path, slug: &str, content: &str) -> std::io::Result<String> {
    let taken: Vec<String> = discover_all(root).into_iter().map(|s| s.name).collect();
    let mut name = slug.to_owned();
    let mut n = 2;
    while taken.contains(&name) || root.join(&name).exists() {
        name = format!("{slug}-{n}");
        n += 1;
    }
    let dir = root.join(&name);
    std::fs::create_dir_all(&dir)?;
    let content = if name == slug {
        content.to_owned()
    } else {
        content.replacen(&format!("name: {slug}\n"), &format!("name: {name}\n"), 1)
    };
    std::fs::write(dir.join("SKILL.md"), content)?;
    Ok(name)
}

fn draft_dir(root: &Path, name: &str) -> Result<PathBuf, DraftError> {
    discover_all(root)
        .into_iter()
        .find(|s| s.draft && s.name == name)
        .map(|s| s.dir)
        .ok_or_else(|| DraftError::NotFound(name.to_owned()))
}

/// Publish a draft: drop its `draft` marker, so the next run lists it.
pub fn publish_draft(root: &Path, name: &str) -> Result<(), DraftError> {
    let dir = draft_dir(root, name)?;
    let path = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&path)?;
    let mut out = String::new();
    let mut in_front = false;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let t = line.trim_end();
        if i == 0 && t == "---" {
            in_front = true;
        } else if in_front && t == "---" {
            in_front = false;
        } else if in_front && is_draft_line(line) {
            continue;
        }
        out.push_str(line);
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// Delete a draft's directory. Only drafts: a published skill is never
/// removed this way.
pub fn discard_draft(root: &Path, name: &str) -> Result<(), DraftError> {
    std::fs::remove_dir_all(draft_dir(root, name)?)?;
    Ok(())
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

    #[test]
    fn drafts_are_listed_only_by_discover_all_until_published() {
        let tmp = tempfile::tempdir().unwrap();
        let draft =
            "---\nname: weekly\ndescription: \"d\"\ndraft: true\ncaptured_from: r1\n---\n# Steps\n";
        assert_eq!(write_draft(tmp.path(), "weekly", draft).unwrap(), "weekly");
        assert!(discover(tmp.path()).is_empty());
        let all = discover_all(tmp.path());
        assert_eq!((all.len(), all[0].draft), (1, true));
        // A second capture with the same name gets its own directory and name.
        assert_eq!(
            write_draft(tmp.path(), "weekly", draft).unwrap(),
            "weekly-2"
        );
        let names: Vec<_> = discover_all(tmp.path())
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["weekly", "weekly-2"]);

        publish_draft(tmp.path(), "weekly").unwrap();
        let found = discover(tmp.path());
        assert_eq!(found.len(), 1);
        let text = std::fs::read_to_string(found[0].dir.join("SKILL.md")).unwrap();
        assert!(!text.contains("draft:") && text.contains("captured_from: r1"));
        assert!(text.ends_with("# Steps\n"));
        // Published skills can't be published again or discarded this way.
        assert!(matches!(
            publish_draft(tmp.path(), "weekly"),
            Err(DraftError::NotFound(_))
        ));
        assert!(matches!(
            discard_draft(tmp.path(), "weekly"),
            Err(DraftError::NotFound(_))
        ));
        assert!(found[0].dir.exists());
        discard_draft(tmp.path(), "weekly-2").unwrap();
        assert!(!tmp.path().join("weekly-2").exists());
    }
}
