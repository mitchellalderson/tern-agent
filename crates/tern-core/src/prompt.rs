//! System prompt assembly (spec §6.1). Built once per run and frozen on the
//! run row, so replay after a crash sends the same prompt.

use std::path::Path;

use chrono::{DateTime, Local};

use crate::defaults;
use crate::home::TernHome;

/// Who the prompt is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole<'a> {
    /// The agent the user talks to.
    Main,
    /// A sub-agent another run started with `agent_spawn`, by its label.
    Subagent { label: &'a str },
}

/// What the "Sub-agents" section needs, when this run may start them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentPrompt {
    pub max_concurrent: u32,
    /// What `agent_spawn` accepts as `model`: aliases, described.
    pub models: Vec<String>,
    /// Whether its sub-agents may start their own.
    pub can_nest: bool,
}

pub struct PromptInputs<'a> {
    pub home: &'a TernHome,
    pub role: AgentRole<'a>,
    /// Set when this run has the `agent_*` tools.
    pub subagents: Option<&'a SubagentPrompt>,
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

    if let AgentRole::Subagent { label } = inp.role {
        out.push_str("\n\n");
        out.push_str(&SUBAGENT_ROLE.replace("{label}", label));
    }

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
    out.push_str(
        "\n\n## Secrets\n\nAPI keys and tokens are set by the user as environment variables or in \
         ~/.tern/secrets.toml (`NAME = \"value\"`, file mode 0600), and take effect after restarting tern \
         (or /reload in the chat UI). tern does not read .env files. Never ask the user to paste a secret \
         into the conversation, and never write one to a file yourself.",
    );

    if let Some(sub) = inp.subagents {
        out.push_str("\n\n");
        out.push_str(&subagents_section(sub));
    }

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

/// For a run with the `agent_*` tools: delegate proactively, and how.
fn subagents_section(sub: &SubagentPrompt) -> String {
    let models = if sub.models.is_empty() {
        "the default is configured for you; pass `provider:model` only if the job clearly needs a different one"
            .to_owned()
    } else {
        sub.models.join(", ")
    };
    let nest = if sub.can_nest {
        "They can start sub-agents of their own, one level deep."
    } else {
        "They can't start sub-agents of their own."
    };
    SUBAGENTS
        .replace("{max_concurrent}", &sub.max_concurrent.to_string())
        .replace("{models}", &models)
        .replace("{nest}", nest)
}

const SUBAGENTS: &str = "## Sub-agents

You can delegate work to sub-agents: separate instances of you that run in parallel, each with a fresh context, the \
same workspace and, unless you limit them, the same tools. Delegating well is one of the most useful things you do. \
It finishes independent work in parallel, keeps your own context free of detail you only need the conclusion of, and \
gives you independent checks. Be proactive about it: you coordinate, they do the legwork.

Delegate by default when:
- the work splits into parts that don't depend on each other (several companies, files, repos, sources or \
approaches): spawn one sub-agent per part, all in the same turn;
- a part needs a lot of reading, searching or trial and error, and you only need what it found;
- a step would flood your context (long logs, large files, many web pages);
- you want a second opinion on your own work: a fresh reviewer who hasn't seen your reasoning.
Do it yourself when the task takes only a few tool calls, when each step depends on the one before, or when it needs \
the conversation with the user that only you have. Never delegate talking to the user.

Fanning out:
- Call `agent_spawn` once per part, then `agent_wait`, all in one turn. Up to {max_concurrent} run at once; more are \
queued.
- Split by outcome, not by tool. Give each sub-agent a distinct slice, and say what is out of its scope and who covers \
it, so no two do the same work.
- Give `tools: \"read_only\"` for research and review. Give write access only to a sub-agent that must change files, \
and never let two of them write the same files.
- Pick `model` for the job: {models}. A light model is fine for lookups and summaries; use a strong one for hard \
reasoning and code.

Briefing: a sub-agent knows only what you put in `task`. Write it like a handoff to a capable colleague who has just \
walked in:
- the goal, why it matters, and what done looks like;
- what you already know or have ruled out, with exact paths, URLs, names and numbers, so it doesn't rediscover them;
- constraints: what not to touch, what to avoid that would need the user's approval, how far to go;
- the exact shape of the report you want back, for example \"a table of name, URL, stage and one line of evidence, \
under 300 words; list what you couldn't verify\".
Give your hypotheses as things to confirm or refute, not as facts.

While they work:
- Do other useful work if there is any, but never the same work they are doing. Their reports arrive through \
`agent_wait` or as messages before your next step.
- A sub-agent that asks you a question is blocked until you answer. Answer at once with `agent_send`.
- Redirect one that is off track with `agent_send`; stop one you no longer need with `agent_cancel`. Check on them \
without waiting with `agent_status`.

When reports come back:
- Treat each report as a claim, not a fact. Before you act on it or tell the user, check what matters: open the file it \
changed, rerun the test, look at the source it cites. A confident tone is not evidence.
- If reports conflict, find out why instead of picking one. Spawn a focused follow-up if you need to.
- If a sub-agent failed or ran out of budget, retry it with a better brief, take that part over yourself, or tell the \
user what is missing.
- Write one answer in your own words. The user has not seen the reports, so don't refer to them as if they had, and \
say what you checked and what you didn't.

Sub-agents can't talk to the user except through approval prompts, which the user sees with the sub-agent's name. \
{nest}";

const SUBAGENT_ROLE: &str = "## You are a sub-agent

The main tern agent started you, as \"{label}\", to do one task, described in the first message. The user doesn't see \
this conversation. Your final message is returned to the main agent as your report.

- Stay within the task. If you notice something important outside it, mention it in your report rather than acting on \
it.
- Work on your own. Ask the main agent with `agent_ask_parent` only when you are blocked on a decision that is really \
its call: one clear question, with the answer you'd choose. Otherwise make a sensible assumption and state it in your \
report.
- Messages that start with \"[message from the main agent]\" or \"[answer from the main agent]\" come from it; follow \
them.
- Don't ask the user anything. Don't save memories, create routines or change connectors unless the task says to.
- Check your work before you finish: rerun the test, re-read the file, confirm the source.
- Your final message is the report. Lead with the answer in the shape the brief asked for, then what you checked, what \
you assumed, and anything left undone. Use exact paths, links and figures. No preamble, and no \"let me know if…\".";

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
            role: AgentRole::Main,
            subagents: None,
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
            role: AgentRole::Main,
            subagents: None,
        });
        assert!(p.starts_with("You are tern"));
        assert!(!p.contains("## Skills"));
        assert!(!p.contains("## Sub-agents") && !p.contains("agent_spawn"));
    }

    #[test]
    fn sub_agent_sections_follow_role_and_config() {
        let tmp = tempfile::tempdir().unwrap();
        let home = TernHome::new(tmp.path());
        std::fs::write(home.persona_path(), "I am tern.").unwrap();
        let sub = SubagentPrompt {
            max_concurrent: 3,
            models: vec!["fast (anthropic:claude-haiku-5-5)".to_owned()],
            can_nest: false,
        };
        let inp = |role, subagents| PromptInputs {
            home: &home,
            now: Local::now(),
            workspace: Path::new("/ws"),
            skills: &[],
            integrations: &[],
            role,
            subagents,
        };

        let main = system_prompt(&inp(AgentRole::Main, Some(&sub)));
        assert!(main.contains("## Sub-agents"));
        assert!(main.contains("Be proactive about it"));
        assert!(main.contains("Up to 3 run at once"));
        assert!(main.contains("Pick `model` for the job: fast (anthropic:claude-haiku-5-5)."));
        assert!(main.contains("They can't start sub-agents of their own."));
        assert!(!main.contains("## You are a sub-agent"));
        assert!(!main.contains('{'), "unfilled placeholder:\n{main}");
        // Before Skills, after the workspace.
        assert!(main.find("## Workspace") < main.find("## Sub-agents"));

        let child = system_prompt(&inp(AgentRole::Subagent { label: "pricing" }, None));
        assert!(child.starts_with("I am tern.\n\n## You are a sub-agent"));
        assert!(child.contains("as \"pricing\""));
        assert!(!child.contains("## Sub-agents"));

        let nested = system_prompt(&inp(AgentRole::Subagent { label: "a" }, Some(&sub)));
        assert!(nested.contains("## You are a sub-agent") && nested.contains("## Sub-agents"));
    }
}
