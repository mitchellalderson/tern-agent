//! `skill_read`: load a skill's full instructions.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::skills;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use crate::{args, spec};

pub struct SkillRead {
    root: PathBuf,
}

impl SkillRead {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    name: String,
}

#[async_trait]
impl Tool for SkillRead {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "skill_read",
            "Load the full instructions of a skill listed in the system prompt, plus the other files in its folder.",
            json!({ "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, _: &ToolCtx, raw: Value) -> ToolResult {
        let a: ReadArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let all = skills::discover(&self.root);
        let Some(skill) = all.iter().find(|s| s.name == a.name) else {
            let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
            return ToolResult::error(format!(
                "no skill named {:?}; available: {}",
                a.name,
                names.join(", ")
            ));
        };
        let text = match std::fs::read_to_string(skill.dir.join("SKILL.md")) {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("reading skill: {e}")),
        };
        let body = skills::parse(&text).map(|(_, _, b)| b).unwrap_or(text);
        let mut files: Vec<String> = walk(&skill.dir)
            .into_iter()
            .filter_map(|p| {
                p.strip_prefix(&skill.dir)
                    .ok()
                    .map(|r| r.display().to_string())
            })
            .filter(|p| p != "SKILL.md")
            .collect();
        files.sort();
        let mut out = format!(
            "# Skill: {}\n\nFolder: {}\n\n{body}",
            skill.name,
            skill.dir.display()
        );
        if !files.is_empty() {
            out.push_str(
                "\n\nOther files in this skill (read them with read_file using the folder path):\n",
            );
            for f in files {
                out.push_str(&format!("- {f}\n"));
            }
        }
        ToolResult::ok(out)
    }
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    ignore::WalkBuilder::new(dir)
        .max_depth(Some(4))
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .take(200)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn reads_skill_body_and_lists_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("pdf");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: pdf\ndescription: PDFs\n---\nUse merge.py.\n",
        )
        .unwrap();
        std::fs::write(dir.join("scripts/merge.py"), "print(1)").unwrap();
        let ctx = ToolCtx {
            workspace: tmp.path().into(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let tool = SkillRead::new(tmp.path().into());
        let r = tool.call(&ctx, json!({"name": "pdf"})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(
            r.content.contains("Use merge.py.") && r.content.contains("scripts/merge.py"),
            "{}",
            r.content
        );
        assert!(!r.content.contains("description: PDFs"));
        let miss = tool.call(&ctx, json!({"name": "nope"})).await;
        assert!(miss.is_error && miss.content.contains("available: pdf"));
    }
}
