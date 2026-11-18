//! `offer_connection`: instead of "I can't", the agent posts a card that
//! routes to a way of connecting an app: Composio sign-in, adding an MCP
//! server, or building a connector. The host allows one offer per app per
//! conversation, so a model can't nag.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::approval::{ApprovalHandler, AuthCard, AuthOption, AuthPrompt};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, current_call_id};
use tern_core::types::ToolSpec;

use crate::{args, spec};

pub const NAME: &str = "offer_connection";

pub struct OfferConnection {
    asker: Arc<dyn ApprovalHandler>,
    /// `(conversation, app)` pairs already offered.
    offered: Mutex<HashSet<(String, String)>>,
}

impl OfferConnection {
    pub fn new(asker: Arc<dyn ApprovalHandler>) -> Self {
        Self {
            asker,
            offered: Mutex::default(),
        }
    }

    /// `true` the first time `app` is offered in `session`.
    fn first(&self, session: &str, app: &str) -> bool {
        let mut o = self.offered.lock().unwrap_or_else(|e| e.into_inner());
        if o.len() > 5000 {
            o.clear();
        }
        o.insert((session.to_owned(), app.to_ascii_lowercase()))
    }
}

#[derive(Deserialize)]
struct Args {
    app: String,
    #[serde(default)]
    task: Option<String>,
}

fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[async_trait]
impl Tool for OfferConnection {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            NAME,
            "When the user needs an app tern isn't connected to, don't say you can't: call this to show a card with the ways to connect it (sign in through Composio, add an MCP server, or build a connector). The user picks; you're told which. Once per app per conversation; after the answer, follow it (composio_connect_app, mcp_add, or write the connector).",
            json!({
                "type": "object",
                "properties": {
                    "app": { "type": "string", "description": "The app's name, e.g. Gmail" },
                    "task": { "type": "string", "description": "What the user wants to do with it (one short line, shown on the card)" }
                },
                "required": ["app"]
            }),
        )
    }

    /// The card is the user's decision; nothing runs until they pick.
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::granted_write(true)
    }

    fn summary(&self, raw: &Value) -> Option<String> {
        Some(format!("Offer to connect {}", raw.get("app")?.as_str()?))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: Args = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let app = clean(&a.app, 40);
        if app.is_empty() {
            return ToolResult::error("app is empty");
        }
        if !self.first(&ctx.session_id, &app) {
            return ToolResult::ok(
                json!({"offered": false, "why": format!("already offered {app} in this conversation; don't offer again, tell the user in a sentence what's needed")})
                    .to_string(),
            );
        }
        let mut notes = Vec::new();
        if let Some(t) = a
            .task
            .as_deref()
            .map(|t| clean(t, 120))
            .filter(|t| !t.is_empty())
        {
            notes.push(format!("For: {t}"));
        }
        let req = AuthPrompt {
            id: tern_core::agent::new_id(),
            run_id: ctx.run_id.clone(),
            session_id: ctx.session_id.clone(),
            call_id: current_call_id(),
            parent_run_id: None,
            agent_label: None,
            card: AuthCard::Choose {
                title: format!("Connect {app}"),
                options: vec![
                    AuthOption {
                        id: "composio".into(),
                        label: format!("Sign in to {app} with Composio"),
                        detail: "One key, hundreds of apps. You sign in in your browser.".into(),
                        recommended: true,
                    },
                    AuthOption {
                        id: "mcp".into(),
                        label: "Add an MCP server".into(),
                        detail: "If the app has an official one.".into(),
                        recommended: false,
                    },
                    AuthOption {
                        id: "build".into(),
                        label: "Build a connector".into(),
                        detail: "tern writes one; you check it.".into(),
                        recommended: false,
                    },
                ],
                notes,
            },
        };
        let answer = tokio::select! {
            a = self.asker.ask_auth(&req) => a,
            () = ctx.cancel.cancelled() => return ToolResult::error("cancelled"),
        };
        let Some(choice) = answer.filter(|a| req.accepts(a)) else {
            return ToolResult::ok(
                json!({"choice": "none", "why": "nobody chose; tell the user in a sentence how they can connect it"}).to_string(),
            );
        };
        let next = match choice.as_str() {
            "composio" => format!(
                "Call composio_list_connections, then composio_connect_app with app \"{app}\"."
            ),
            "mcp" => format!(
                "Find {app}'s official MCP server and add it (mcp_add); sign in with OAuth if it offers it."
            ),
            "build" => format!(
                "Build a {app} connector. Prefer OAuth sign-in (auth_discover, auth_propose) over a key."
            ),
            _ => "The user cancelled. Don't offer again.".into(),
        };
        ToolResult::ok(json!({"choice": choice, "next": next}).to_string())
    }
}
