//! Power only narrows: what an agent that creates, edits or deletes other
//! agents may give them or touch. The rules are code, not prompt: an agent
//! never hands out a tool, connector or autonomy ceiling it doesn't have
//! itself, and never edits an agent that can do more than it can.
//!
//! "Its own" is the calling run's frozen tool scope (`runs.tool_scope`,
//! `None` = everything) and autonomy level, and the agent's ceiling.

use crate::agents::{Pick, ToolScope};
use crate::approval::Autonomy;
use crate::tool::in_scope;

/// The tools that create, change and delete agents and channels. A new
/// agent's default scope leaves them out, and sub-agents never get them.
pub const ADMIN_TOOLS: &[&str] = &[
    "agent_create",
    "agent_edit",
    "agent_delete",
    "channel_create",
    "channel_edit",
    "channel_delete",
];

/// A tool name as the user or a model might spell it, as tern spells it:
/// `Read File`, `read-file` and `READ_FILE` are all `read_file`.
pub fn normalize_tool(name: &str) -> String {
    name.trim()
        .trim_matches(|c| c == '`' || c == '"' || c == '\'')
        .to_ascii_lowercase()
        .replace(['-', ' ', '.'], "_")
}

/// `requested` as tool names tern knows (`known`), each normalised and
/// checked. All the unknown names are named in the refusal, with the ones
/// that exist, so one retry can fix them (or the caller can stop).
pub fn resolve_tools(requested: &[String], known: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut unknown = Vec::new();
    for r in requested {
        let n = normalize_tool(r);
        if known.contains(&n) {
            if !out.contains(&n) {
                out.push(n);
            }
        } else {
            unknown.push(r.trim().to_owned());
        }
    }
    if unknown.is_empty() {
        return Ok(out);
    }
    let mut names: Vec<&str> = known.iter().map(String::as_str).collect();
    names.sort_unstable();
    Err(format!(
        "unknown tool{} {}. The tools are: {}",
        if unknown.len() == 1 { "" } else { "s" },
        unknown
            .iter()
            .map(|u| format!("{u:?}"))
            .collect::<Vec<_>>()
            .join(", "),
        names.join(", ")
    ))
}

/// Whether the frozen scope `creator` (`None` = everything) lets `tool`
/// through.
fn has(creator: Option<&[String]>, tool: &str) -> bool {
    in_scope(creator, tool)
}

/// The scope a new or edited agent gets: `builtins` (already resolved;
/// `None` = the narrow default) and `connectors` (names; `None` = none),
/// never more than `creator` has. `known` are the registry's built-ins.
pub fn narrow_scope(
    creator: Option<&[String]>,
    builtins: Option<&[String]>,
    connectors: Option<&[String]>,
    known: &[String],
) -> Result<ToolScope, String> {
    let wanted: Vec<String> = match builtins {
        Some(b) => b.to_vec(),
        None => match ToolScope::narrow(known).builtins {
            Pick::Names(n) => n,
            _ => Vec::new(),
        },
    };
    let (allowed, refused): (Vec<String>, Vec<String>) =
        wanted.into_iter().partition(|t| has(creator, t));
    if !refused.is_empty() && builtins.is_some() {
        return Err(format!(
            "you can't give {} to another agent: you don't have {}. An agent never gets more than the agent that made it",
            refused.join(", "),
            if refused.len() == 1 { "it" } else { "them" }
        ));
    }
    // The default quietly leaves out what its creator lacks.
    let mut conn = Vec::new();
    for c in connectors.unwrap_or(&[]) {
        let name = c.trim();
        if name.is_empty() {
            continue;
        }
        if !has(creator, &format!("lua__{name}__probe")) {
            return Err(format!(
                "you can't give connector {name:?} to another agent: you don't have it"
            ));
        }
        if !conn.iter().any(|x: &String| x == name) {
            conn.push(name.to_owned());
        }
    }
    // Skills: all, unless the creator's are limited to some.
    let skills = match creator {
        None => Pick::All,
        Some(c) if c.iter().any(|n| n == "skill:*") => Pick::All,
        Some(c) => Pick::Names(
            c.iter()
                .filter_map(|n| n.strip_prefix("skill:"))
                .map(str::to_owned)
                .collect(),
        ),
    };
    Ok(ToolScope {
        builtins: Pick::Names(allowed),
        connectors: Pick::Names(conn),
        mcp: Pick::Names(Vec::new()),
        skills,
    })
}

/// The autonomy ceiling for an agent `creator` makes or edits: `requested`
/// (default `supervised`), never above the creator's own level (`level`,
/// the run's) or its ceiling. The creator can always go lower.
pub fn narrow_autonomy(
    level: Autonomy,
    ceiling: Option<Autonomy>,
    requested: Option<Autonomy>,
) -> Result<Autonomy, String> {
    let own = ceiling.map_or(level, |c| c.min(level));
    let want = requested.unwrap_or(Autonomy::Supervised.min(own));
    if want > own {
        return Err(format!(
            "you can't give another agent the autonomy {want}: yours is {own}. An agent never gets more than the agent that made it"
        ));
    }
    Ok(want)
}

/// A ceiling's rank, `None` (no ceiling) on top.
fn rank(c: Option<Autonomy>) -> (u8, Autonomy) {
    match c {
        Some(a) => (0, a),
        None => (1, Autonomy::Yolo),
    }
}

/// Whether the agent whose ceiling is `target` and whose frozen scope is
/// `target_scope` can do no more than `creator` (its ceiling and frozen
/// scope): the condition for editing or deleting it. Otherwise a weak
/// agent could rewrite a strong one's instructions and borrow its power.
pub fn within_reach(
    creator_ceiling: Option<Autonomy>,
    creator: Option<&[String]>,
    target_ceiling: Option<Autonomy>,
    target_scope: &[String],
) -> Result<(), String> {
    if rank(target_ceiling) > rank(creator_ceiling) {
        return Err(
            "that agent can do more than you can (a higher autonomy ceiling), so only the user can change it"
                .into(),
        );
    }
    if let Some(c) = creator
        && let Some(extra) = target_scope.iter().find(|t| !has(Some(c), t))
    {
        return Err(format!(
            "that agent can use {extra}, which you can't, so only the user can change it"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn tool_names_are_normalised() {
        assert_eq!(normalize_tool(" Read File "), "read_file");
        assert_eq!(normalize_tool("`read-file`"), "read_file");
        assert_eq!(normalize_tool("HTTP.Fetch"), "http_fetch");
        let known = names(&["read_file", "http_fetch"]);
        assert_eq!(
            resolve_tools(&names(&["Read File", "read_file", "http-fetch"]), &known).unwrap(),
            ["read_file", "http_fetch"]
        );
        let e = resolve_tools(&names(&["read_file", "teleport", "Mind Read"]), &known).unwrap_err();
        assert!(
            e.contains("unknown tools \"teleport\", \"Mind Read\""),
            "{e}"
        );
        assert!(e.contains("http_fetch, read_file"), "{e}");
    }

    #[test]
    fn a_new_agent_never_gets_a_tool_its_creator_lacks() {
        let known = names(&["read_file", "write_file", "shell", "http_fetch"]);
        let creator = names(&["read_file", "http_fetch", "lua__crm__*"]);
        // Asked for, and the creator has it: fine.
        let s = narrow_scope(
            Some(&creator),
            Some(&names(&["read_file"])),
            Some(&names(&["crm"])),
            &known,
        )
        .unwrap();
        assert_eq!(s.builtins, Pick::Names(names(&["read_file"])));
        assert_eq!(s.connectors, Pick::Names(names(&["crm"])));
        // Asked for, and the creator doesn't have it: refused, naming it.
        let e = narrow_scope(
            Some(&creator),
            Some(&names(&["read_file", "write_file", "shell"])),
            None,
            &known,
        )
        .unwrap_err();
        assert!(e.contains("write_file, shell"), "{e}");
        let e = narrow_scope(Some(&creator), None, Some(&names(&["mail"])), &known).unwrap_err();
        assert!(e.contains("connector \"mail\""), "{e}");
        // The default leaves out what the creator lacks (and `shell`).
        let s = narrow_scope(Some(&creator), None, None, &known).unwrap();
        assert_eq!(s.builtins, Pick::Names(names(&["read_file", "http_fetch"])));
        assert_eq!(s.connectors, Pick::Names(vec![]));
        // An unrestricted creator gives the narrow default, never `shell`.
        let s = narrow_scope(None, None, None, &known).unwrap();
        assert_eq!(
            s.builtins,
            Pick::Names(names(&["read_file", "write_file", "http_fetch"]))
        );
        // Skills follow the creator's.
        let s = narrow_scope(
            Some(&names(&["read_file", "skill:notes"])),
            None,
            None,
            &known,
        )
        .unwrap();
        assert_eq!(s.skills, Pick::Names(names(&["notes"])));
    }

    #[test]
    fn a_ceiling_never_goes_above_the_creators() {
        use Autonomy::*;
        // Default: supervised, or lower when the creator is.
        assert_eq!(narrow_autonomy(Autonomous, None, None).unwrap(), Supervised);
        assert_eq!(narrow_autonomy(ReadOnly, None, None).unwrap(), ReadOnly);
        // Lower is always fine; equal is fine.
        assert_eq!(
            narrow_autonomy(Autonomous, None, Some(ReadOnly)).unwrap(),
            ReadOnly
        );
        assert_eq!(
            narrow_autonomy(Autonomous, None, Some(Autonomous)).unwrap(),
            Autonomous
        );
        // Higher is refused, by the run's level or by the agent's ceiling.
        let e = narrow_autonomy(Supervised, None, Some(Autonomous)).unwrap_err();
        assert!(e.contains("yours is supervised"), "{e}");
        let e = narrow_autonomy(Yolo, Some(ReadOnly), Some(Supervised)).unwrap_err();
        assert!(e.contains("yours is readonly"), "{e}");
    }

    #[test]
    fn only_agents_within_reach_can_be_edited() {
        use Autonomy::*;
        let me = names(&["read_file", "http_fetch"]);
        // Lower ceiling and a subset of tools: in reach.
        within_reach(
            Some(Supervised),
            Some(&me),
            Some(ReadOnly),
            &names(&["read_file"]),
        )
        .unwrap();
        // No ceiling means the most; an agent with a ceiling can't touch it.
        assert!(within_reach(Some(Supervised), Some(&me), None, &[]).is_err());
        within_reach(None, None, None, &names(&["shell"])).unwrap();
        // A higher ceiling, or a tool the editor lacks.
        assert!(within_reach(Some(ReadOnly), None, Some(Autonomous), &[]).is_err());
        let e =
            within_reach(Some(Yolo), Some(&me), Some(ReadOnly), &names(&["shell"])).unwrap_err();
        assert!(e.contains("shell"), "{e}");
    }
}
