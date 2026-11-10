# Recipe: set up Clerk accounts

Accounts are optional. Skip this and tern works as before. Design: [deployment spec §10.4a](../deployment-spec.md).

## In the Clerk dashboard

1. **Create an application** (dashboard.clerk.com). Name it "tern". Keep a separate one for development and production.
2. **Enable sign-in methods** (Configure → User & authentication → Social connections / Email):
   - Email (code or link)
   - Google
   - **Apple** (required if you offer any other social login; Clerk walks you through the Apple Developer setup)
3. **Create an OAuth application** (Configure → OAuth applications → Add):
   - Name: `tern desktop`
   - **Public client** (no client secret), **PKCE required**
   - **Redirect URI:** `http://127.0.0.1/callback`. If Clerk rejects a missing port, allow loopback redirects with any
     port (RFC 8252 §7.3), or add the ports you want to allow.
   - **Scopes:** `openid`, `profile`, `email`, `offline_access` (the last gives the refresh token)
   - If there is a setting for access token format, JWT is fine, but tern uses the **ID token**.
4. Copy three values:
   - **Publishable key** (`pk_test_…` / `pk_live_…`): API keys page.
   - **Frontend API URL** (`https://clerk.<your domain>` or `https://<name>.clerk.accounts.dev`): Domains page. It is
     also the issuer.
   - **Client ID** of the OAuth application from step 3.
5. Find your own **user id** (`user_…`): Users → your user → copy the id.

You do not need a secret key anywhere. Never put one in tern.

## Where the keys go

**Desktop app** (build time, or an environment variable when running it):

```sh
flutter build macos \
  --dart-define=TERN_CLERK_PUBLISHABLE_KEY=pk_test_… \
  --dart-define=TERN_CLERK_CLIENT_ID=<client id>
```

With those two set, first launch and Settings show **Sign in**. Without them nothing about accounts is shown.

**Server core** (`~/.tern/config.toml`, or the container's):

```toml
[account]
issuer = "https://<Frontend API URL>"
audience = "<client id>"
publishable_key = "pk_test_…"
owner = "<your user id>"
```

Restart the core, then check it:

```sh
tern account status
```

It should print `Accounts: on`, your owner id, and `keys: N fetched`.

## Try it

1. Start a core with a TCP listener and a real certificate (`tls = "files"` or a TLS proxy).
2. In the app, choose **Sign in**, enter the server address, continue, and sign in in the browser.
3. Settings shows "Signed in as …" with **Sign out**.

## Things to check the first time

- The ID token's `aud` must equal the client id. If the core answers 401 "wrong audience", compare the two.
- If renewal fails after an hour, check `offline_access` is a scope of the OAuth application.
- Another account that signs in gets 403, not 401: it is a valid account that doesn't own the core.
