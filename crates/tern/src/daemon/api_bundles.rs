//! Bundles over the API (tooling spec §6.5), for the desktop's install
//! flow and Library → Bundles. Owner-only (`auth::OWNER_ONLY`): installing
//! adds code and routines, and fetching makes this machine reach a URL.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/bundles` | `{bundles: [...]}`: what is installed, from where, pinned to which hash and key |
//! | POST | `/v1/bundles/preview` | `{source, sha256?}` → `{plan, conflicts, grows, can_install}`: everything it can do; installs nothing |
//! | POST | `/v1/bundles/install` | `{source, sha256, agent?, accept_new_key?}` → `{name, lines}`. `sha256` is the preview's, so what installs is exactly what was shown |
//! | DELETE | `/v1/bundles/{name}` | remove it and what it installed → `{name, lines}` |
//!
//! An update is a preview and install of the installed bundle's `source`.

use axum::extract::{Path as UrlPath, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tern_bundle::{InstallOptions, InstalledBundle, Installer, Outcome, Plan};

use super::api::{ApiResult, Shared, bad, internal, not_found};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/bundles", get(list))
        .route("/v1/bundles/preview", post(preview))
        .route("/v1/bundles/install", post(install))
        .route("/v1/bundles/{name}", delete(remove))
}

#[derive(Serialize)]
struct ApiBundle {
    #[serde(flatten)]
    bundle: InstalledBundle,
    /// `signed by D4CA… (trusted)`, `unsigned`.
    signature: String,
    key_trusted: bool,
}

#[derive(Serialize)]
struct Listing {
    bundles: Vec<ApiBundle>,
}

async fn list(State(m): State<Shared>) -> ApiResult<Listing> {
    let bundles = tern_bundle::installed(&m.rt.home)
        .into_iter()
        .map(|b| ApiBundle {
            signature: b.signature_words(),
            key_trusted: b.key_trusted,
            bundle: b,
        })
        .collect();
    Ok(Json(Listing { bundles }))
}

#[derive(Deserialize)]
struct PreviewBody {
    source: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Serialize)]
struct Preview {
    plan: Plan,
    /// Why it can't be installed as it is (names taken by your own things).
    conflicts: Vec<String>,
    /// An update that can do more than the installed version, or a first
    /// install: it needs approving.
    grows: bool,
    can_install: bool,
}

async fn preview(State(m): State<Shared>, Json(b): Json<PreviewBody>) -> ApiResult<Preview> {
    let installer = Installer::new(&m.rt.home);
    let bundle = installer
        .read(b.source.trim(), b.sha256.as_deref())
        .await
        .map_err(bad)?;
    let p = installer.prepare(bundle, Some(&m.rt.store)).map_err(bad)?;
    let conflicts = p.plan.conflicts();
    let refused_unsigned = installer.require_signed && p.plan.signature.state == "unsigned";
    Ok(Json(Preview {
        grows: p.plan.changes.as_ref().is_none_or(|c| c.grows())
            || p.plan.signature.state == "new_key",
        can_install: conflicts.is_empty() && !refused_unsigned,
        conflicts,
        plan: p.plan,
    }))
}

#[derive(Deserialize)]
struct InstallBody {
    source: String,
    /// The hash the preview showed.
    sha256: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    accept_new_key: bool,
}

async fn install(State(m): State<Shared>, Json(b): Json<InstallBody>) -> ApiResult<Outcome> {
    if b.sha256.trim().is_empty() {
        return Err(bad("sha256 is required: install what the preview showed"));
    }
    let installer = Installer::new(&m.rt.home);
    // The pin makes sure the bytes are the ones the user approved. A
    // directory's hash is its manifest's, which pins every file.
    let bundle = installer
        .read(b.source.trim(), Some(&b.sha256))
        .await
        .map_err(bad)?;
    let p = installer.prepare(bundle, Some(&m.rt.store)).map_err(bad)?;
    let out = installer
        .install(
            &p,
            &m.rt.store,
            &InstallOptions {
                agent: b.agent,
                accept_new_key: b.accept_new_key,
            },
        )
        .map_err(bad)?;
    // A bundle can bring agents (and their DMs).
    let _ = m.rt.changes.send(tern_core::models::Change::AgentsChanged {});
    let _ = m.rt.changes.send(tern_core::models::Change::ChannelsChanged {});
    Ok(Json(out))
}

async fn remove(State(m): State<Shared>, UrlPath(name): UrlPath<String>) -> ApiResult<Outcome> {
    let installer = Installer::new(&m.rt.home);
    if installer.installed(&name).map_err(internal)?.is_none() {
        return Err(not_found(&format!("bundle {name}")));
    }
    let out = installer
        .remove(&name, &m.rt.store)
        .map_err(internal)?;
    let _ = m.rt.changes.send(tern_core::models::Change::AgentsChanged {});
    let _ = m.rt.changes.send(tern_core::models::Change::ChannelsChanged {});
    Ok(Json(out))
}
