# Connect apps with Composio

One API key, hundreds of apps (Gmail, Slack, GitHub, Notion, Linear …). You sign in in your browser; tern never sees the sign-in link or your tokens.

1. Ask for something that needs an app: "Summarise my unread Gmail."
2. tern asks for a Composio key in a private prompt. Create one at [dashboard.composio.dev](https://dashboard.composio.dev). It's saved as `COMPOSIO_API_KEY`.
3. A sign-in card opens for the app. Sign in; the run carries on.

If tern doesn't have a way to reach an app, it shows a card instead of "I can't": sign in with Composio, add an MCP server, or build a connector. It offers each app once per conversation.

## What asks and what doesn't

| Tool | Does | Asks |
|---|---|---|
| `composio_search_tools` | find tools by task | never |
| `composio_list_connections` | list signed-in apps | never |
| `composio_connect_app` | sign-in card | you open it |
| `composio_run_read_tool` | tools Composio tags read-only | never |
| `composio_run_tool` | changes something | every time unless you "always allow" |
| `composio_run_destructive_tool` | deletes or can't be undone | every time |

- tern checks Composio's `readOnlyHint` / `destructiveHint` before anything runs. An untagged tool counts as a write.
- Payment apps (Stripe, PayPal …) are read-only.
- Results are outside text: they taint the run, HTML becomes plain text, and long strings are cut at 4,000 characters.

## Errors

- No key: tern asks for `COMPOSIO_API_KEY`.
- Key rejected: tern asks you to replace it.
- App not signed in: tern opens the sign-in card.

Prefer Composio, then an official MCP server with OAuth, a connector with OAuth, app-only OAuth, then an API key. tern never asks you to paste a short-lived access token.
