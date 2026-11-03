//! System prompt assembly (spec §6.1). Built once per run and frozen on the
//! run row, so replay after a crash sends the same prompt.

use std::path::Path;

use chrono::{DateTime, Local};

use crate::defaults;
use crate::home::TernHome;

pub struct PromptInputs<'a> {
    pub home: &'a TernHome,
    pub now: DateTime<Local>,
    pub workspace: &'a Path,
    /// (name, description) of each skill.
    pub skills: &'a [(String, String)],
    /// One line per connected MCP server or Luau connector.
    pub integrations: &'a [String],
}

pub fn system_prompt(inp: &PromptInputs<'_>) -> String {
    let persona = read_nonempty(&inp.home.persona_path())
        .unwrap_or_else(|| defaults::PERSONA_MD.trim().to_owned());
    let mut out = persona;

    if let Some(user) = read_nonempty(&inp.home.user_path()) {
        out.push_str("\n\n## About the user\n\n");
        out.push_str(&user);
    }

    out.push_str("\n\n## Current time\n\n");
    out.push_str(&inp.now.format("%A, %Y-%m-%d %H:%M (UTC%:z)").to_string());
    if let Some(tz) = std::env::var("TZ").ok().filter(|t| !t.is_empty()) {
        out.push_str(&format!(", timezone {tz}"));
    }

    out.push_str("\n\n## Workspace\n\n");
    out.push_str(&format!(
        "Relative file paths and shell commands run in {}.",
        inp.workspace.display()
    ));

    if !inp.skills.is_empty() {
        out.push_str(
            "\n\n## Skills\n\nLoad a skill's instructions with `skill_read` before using it.\n",
        );
        for (name, description) in inp.skills {
            out.push_str(&format!("\n- {name}: {description}"));
        }
    }

    if !inp.integrations.is_empty() {
        out.push_str("\n\n## Integrations\n");
        for line in inp.integrations {
            out.push_str(&format!("\n- {line}"));
        }
    }
    out
}

/// File contents with HTML comments stripped, or `None` if nothing is left.
fn read_nonempty(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let stripped = strip_html_comments(&text);
    let trimmed = stripped.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn strip_html_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_sections_and_skips_placeholder_user_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path());
        std::fs::write(home.persona_path(), "I am tern.").unwrap();
        std::fs::write(home.user_path(), defaults::USER_MD).unwrap();
        let skills = vec![("pdf".to_owned(), "Work with PDFs".to_owned())];
        let integrations = vec!["MCP server github: 12 tools (mcp__github__*)".to_owned()];
        let inp = PromptInputs {
            home: &home,
            now: Local::now(),
            workspace: Path::new("/ws"),
            skills: &skills,
            integrations: &integrations,
        };
        let p = system_prompt(&inp);
        assert!(p.starts_with("I am tern."));
        assert!(!p.contains("About the user"));
        assert!(p.contains("## Current time"));
        assert!(p.contains("/ws"));
        assert!(p.contains("## Skills") && p.contains("- pdf: Work with PDFs"));
        assert!(p.contains("## Integrations\n\n- MCP server github"));

        std::fs::write(home.user_path(), "<!-- note -->\nName: Sam").unwrap();
        assert!(system_prompt(&inp).contains("## About the user\n\nName: Sam"));
    }

    #[test]
    fn falls_back_to_default_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path());
        let p = system_prompt(&PromptInputs {
            home: &home,
            now: Local::now(),
            workspace: Path::new("/ws"),
            skills: &[],
            integrations: &[],
        });
        assert!(p.starts_with("You are tern"));
        assert!(!p.contains("## Skills"));
    }
}
