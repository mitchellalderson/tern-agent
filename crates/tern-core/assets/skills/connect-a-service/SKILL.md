---
name: connect-a-service
description: Connect tern to a new service or another account of one (Gmail, Notion, GitHub, a remote MCP server): signing in with OAuth first, a key as the fallback, the user choosing.
---

# Connecting a service

Use this whenever the user wants tern to reach a service it can't yet (write a connector, add an MCP server) or to add another account of one it can ("add my personal Gmail too").

## The steps

1. **Identify** the service and, if you can, its API base URL or MCP server URL.
2. **Discover**: call `auth_discover` with the service's name and the URL (`mcp: true` for an MCP server). It checks tern's built-in profiles and the service's own metadata; no guessing.
3. **Research, only if discovery says "unknown"**: search the service's developer docs for "OAuth". If it has OAuth, pass `auth_propose` a plan with `authorize_url`, `token_url`, `api_hosts` (the hosts your connector will call) and `evidence` (the doc URLs). tern refuses addresses that aren't on the API's own domain or a known identity provider, so a page can't send the user to a lookalike. Don't use addresses from anywhere but the service's own docs. A researched plan also needs `target`, the connector (a draft is fine) or MCP server it is for: its declared hosts (`capabilities.http`, the server's URL) must cover `api_hosts`, and a plan the service's metadata can't confirm is refused without one. So when research is needed, write the draft connector first.
4. **Propose**: call `auth_propose` with the `plan_id`. The user picks signing in or a key on a card; you never pick for them when both are offered. Respect "cancel".
5. **Build**:
   - Connector with OAuth: `capabilities.oauth = { <provider> = { scopes = { … } } }` using the plan's `provider`, and only the scopes the tools need. No `auth` line is needed: tern sends the token.
   - Connector with a key: `capabilities.auth = "NAME"` (or another form from `connector_write`'s guide).
   - Remote MCP server with OAuth: `mcp_add` with `oauth: true` and no secret fields.
   - MCP server with a key: `mcp_add` with `bearer_token_secret`, `header_secrets` or `env_secrets`.
6. **Connect**: `connection_add` with the target and the `plan_id`. The user signs in on a card (their browser, or a code) or enters the key privately. You get `connected`, `reconnected`, `skipped` or `timed_out`, never a token.
7. **Test** with `connector_test` or a read-only call. With several accounts, pass `connection`.

## Several accounts

Each account is a connection: `connection_add` again with a `connection` name ("work", "personal") adds another. Once a service has two or more, its tools take a `connection` argument: reads without it use the default account; writes and deletes must name one, so say which account you mean and let the approval card show it. Signing in again to an account that is already connected refreshes it instead of adding a duplicate.

## When OAuth needs the user's own app

Some providers (Google, Microsoft, GitHub, Linear, Dropbox) need the user to register an OAuth app first. The card says so and links the setup page. Walk them through it in plain words, then they add it to `~/.tern/config.toml`:

```toml
[auth.clients]
google = { client_id = "1234-abc.apps.googleusercontent.com", client_secret_secret = "GOOGLE_CLIENT_SECRET" }
```

For a provider without a built-in profile (the plan's `provider` is its domain, like `acme_com`), the entry also names its issuer, `issuer = "https://login.acme.com"`: tern only sends the app to endpoints there.

For Google: in Google Cloud Console, create a project, enable the API (Gmail, Calendar, Drive…), set up the OAuth consent screen (External; add themselves as a test user, or publish the app to avoid sign-ins expiring after 7 days), then create credentials of type **Desktop app**. The client secret goes in tern's secrets under the name they chose (ask with `secret_request`), never in the chat or the config file. A key or personal access token, where the service has one, is the quicker alternative: offer it.

## Never

- Ask for a token, key or password in the chat.
- Put a client secret or key in config, a file, or a connector's code.
- Choose OAuth or a key for the user when the card offers both.
