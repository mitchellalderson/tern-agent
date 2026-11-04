//! Security fix 4: a connector and an MCP server with the same name keep
//! separate secrets; a new grant replaces the whole token set; the first
//! layout's names move to the one connection that can own them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use tern_auth::{ClientSecret, ConnAuth, Connections, Guard, Kind, OAuthConfig, TokenSet, Tokens};

fn tokens(access: &str, refresh: Option<&str>, expires: Option<i64>) -> TokenSet {
    TokenSet {
        access_token: access.into(),
        refresh_token: refresh.map(Into::into),
        expires_at: expires,
        scope: None,
        id_token: None,
    }
}

fn oauth(token_url: &str) -> ConnAuth {
    ConnAuth::OAuth(Box::new(OAuthConfig {
        provider: "p".into(),
        authorize_url: format!("{token_url}/authorize"),
        token_url: token_url.into(),
        client_id: "c".into(),
        ..OAuthConfig::default()
    }))
}

fn open(dir: &std::path::Path, store: tern_store::Store) -> Connections {
    let secrets = tern_core::Secrets::load(&dir.join("secrets.toml")).unwrap();
    Connections::new(store, Arc::new(Tokens::new(secrets, Guard::offline())), 5)
}

#[tokio::test]
async fn a_connector_and_an_mcp_server_of_one_name_dont_share_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let conns = open(dir.path(), tern_store::Store::open_in_memory().unwrap());
    let auth = oauth("https://login.example.com/token");
    let lua = conns
        .add(Kind::Connector, "acme", "default", None, None, &auth)
        .unwrap();
    let mcp = conns
        .add(Kind::Mcp, "acme", "default", None, None, &auth)
        .unwrap();
    let t = &conns.tokens;
    t.save(&lua.key(), &tokens("AT-lua", Some("RT-lua"), None))
        .unwrap();
    // Signing the MCP server in doesn't overwrite the connector's grant.
    t.save(&mcp.key(), &tokens("AT-mcp", None, None)).unwrap();
    let l = t.load(&lua.key()).unwrap();
    assert_eq!(
        (l.access_token.as_str(), l.refresh_token.as_deref()),
        ("AT-lua", Some("RT-lua"))
    );
    assert_eq!(t.load(&mcp.key()).unwrap().refresh_token, None);
    // Removing the MCP server's account leaves the connector's.
    conns.remove(&mcp).await.unwrap();
    assert!(t.load(&mcp.key()).is_none());
    assert_eq!(t.load(&lua.key()).unwrap().access_token, "AT-lua");
}

#[test]
fn a_new_grant_replaces_the_whole_token_set() {
    let dir = tempfile::tempdir().unwrap();
    let conns = open(dir.path(), tern_store::Store::open_in_memory().unwrap());
    let c = conns
        .add(
            Kind::Mcp,
            "acme",
            "work",
            None,
            None,
            &oauth("https://a.example/token"),
        )
        .unwrap();
    let t = &conns.tokens;
    t.save_grant(
        &c.key(),
        &tokens("AT-1", Some("RT-1"), Some(1_000)),
        ClientSecret::New("cs-registered"),
    )
    .unwrap();
    // A grant from another client and endpoint, with no refresh token or
    // expiry: nothing of the first one is left to send anywhere.
    t.save_grant(&c.key(), &tokens("AT-2", None, None), ClientSecret::Drop)
        .unwrap();
    let now = t.load(&c.key()).unwrap();
    assert_eq!(
        (now.access_token.as_str(), now.refresh_token, now.expires_at),
        ("AT-2", None, None)
    );
    let secrets = t.secrets();
    assert_eq!(secrets.get(&c.key().client_secret_name()), None);
    // Still redacted, though gone.
    assert_eq!(
        secrets.redact("RT-1 cs-registered"),
        "[REDACTED] [REDACTED]"
    );
}

#[test]
fn first_layout_names_move_to_their_one_owner() {
    let dir = tempfile::tempdir().unwrap();
    let store = tern_store::Store::open_in_memory().unwrap();
    {
        let conns = open(dir.path(), store.clone());
        let auth = oauth("https://a.example/token");
        conns
            .add(Kind::Connector, "x", "work", None, None, &auth)
            .unwrap();
        conns
            .add(
                Kind::Mcp,
                "y",
                "work",
                None,
                None,
                &ConnAuth::Key {
                    names: vec!["TOKEN".into()],
                },
            )
            .unwrap();
        conns
            .add(Kind::Connector, "z", "default", None, None, &auth)
            .unwrap();
        conns
            .add(Kind::Mcp, "z", "default", None, None, &auth)
            .unwrap();
    }
    // What an earlier tern wrote.
    let path = dir.path().join("secrets.toml");
    std::fs::write(
        &path,
        "\"conn/x/work/oauth/access_token\" = \"AT-x\"\n\
         \"conn/x/work/oauth/refresh_token\" = \"RT-x\"\n\
         \"conn/y/work/TOKEN\" = \"tok-y\"\n\
         \"conn/z/default/oauth/refresh_token\" = \"RT-z\"\n\
         PLAIN = \"plain\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let conns = open(dir.path(), store);
    let s = conns.tokens.secrets();
    let x = conns.get(Kind::Connector, "x", "work").unwrap();
    assert_eq!(
        conns
            .tokens
            .load(&x.key())
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("RT-x")
    );
    let y = conns.get(Kind::Mcp, "y", "work").unwrap();
    assert_eq!(
        s.get(&y.key().secret_name("TOKEN")).as_deref(),
        Some("tok-y")
    );
    assert!(y.key().secret_name("TOKEN").starts_with("conn/mcp:y/work/"));
    // z's grant could be either's: gone, and both must sign in again.
    for k in [Kind::Connector, Kind::Mcp] {
        let z = conns.get(k, "z", "default").unwrap();
        assert_eq!(z.status, "needs_reauth", "{k:?}");
        assert!(conns.tokens.load(&z.key()).is_none());
    }
    let names = s.file_names();
    assert!(
        names
            .iter()
            .all(|n| !n.starts_with("conn/") || n.contains(':')),
        "{names:?}"
    );
    assert!(names.contains(&"PLAIN".to_owned()));
    assert_eq!(s.redact("RT-z"), "[REDACTED]");
}
