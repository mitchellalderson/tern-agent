//! TCP listeners next to the Unix socket (deployment spec §6.1). Off unless
//! `[api] listen` names one. Same router as the socket, with
//! [`auth::require_auth`] in front and TLS unless a proxy terminates it.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use axum::extract::DefaultBodyLimit;
use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tern_core::TernHome;
use tern_core::config::{ApiConfig, TlsMode};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use super::auth::{self, AuthState, Peer};
use super::manager::Manager;

/// Requests over TCP are capped at this, except uploads.
pub const BODY_LIMIT: usize = 1024 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The `tcp://host:port` entries of `[api] listen`.
pub fn tcp_addrs(api: &ApiConfig) -> anyhow::Result<Vec<SocketAddr>> {
    parse_tcp(&api.listen, "api.listen")
}

/// `tcp://host:port` entries (and `unix`, skipped) of setting `key`.
pub fn parse_tcp(entries: &[String], key: &str) -> anyhow::Result<Vec<SocketAddr>> {
    let mut out = Vec::new();
    for entry in entries {
        if entry == "unix" {
            continue;
        }
        let Some(hostport) = entry.strip_prefix("tcp://") else {
            bail!("{key} entry {entry:?} must be \"unix\" or \"tcp://host:port\"");
        };
        let addr = hostport
            .to_socket_addrs()
            .with_context(|| format!("{key} entry {entry:?}"))?
            .next()
            .with_context(|| format!("{key} entry {entry:?} resolves to nothing"))?;
        out.push(addr);
    }
    Ok(out)
}

/// Refuse configurations that would put tokens on the network in clear.
pub fn check(api: &ApiConfig) -> anyhow::Result<Vec<SocketAddr>> {
    let addrs = tcp_addrs(api)?;
    if api.tls == TlsMode::Off
        && !addrs.is_empty()
        && !(api.trust_proxy && addrs.iter().all(|a| a.ip().is_loopback()))
    {
        bail!(
            "api.tls = \"off\" is only allowed when every TCP listener is on loopback and api.trust_proxy = true (a TLS proxy in front)"
        );
    }
    Ok(addrs)
}

/// Bind every TCP listener and serve until `shutdown`. Returns the bound
/// addresses (useful with port 0) and the server tasks.
pub async fn serve(
    manager: Arc<Manager>,
    home: &TernHome,
    api: &ApiConfig,
    shutdown: CancellationToken,
) -> anyhow::Result<Vec<(SocketAddr, tokio::task::JoinHandle<()>)>> {
    let addrs = check(api)?;
    if addrs.is_empty() {
        return Ok(Vec::new());
    }
    let tls = super::tls::server_config(home, api)?.map(TlsAcceptor::from);
    if tls.is_none() {
        tracing::warn!("api.tls = \"off\": expecting a TLS-terminating proxy in front");
    }
    let state = AuthState::new(manager.clone(), api.trust_proxy);
    let app = super::api::router(manager)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(axum::middleware::from_fn_with_state(
            state,
            auth::require_auth,
        ));
    let mut out = Vec::new();
    for addr in addrs {
        let tcp = TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        let local = tcp.local_addr()?;
        let listener = Accepted::spawn(tcp, tls.clone(), shutdown.clone());
        let app = app.clone().into_make_service_with_connect_info::<Peer>();
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move { stop.cancelled().await })
                .await;
            if let Err(e) = result {
                tracing::error!(error = %e, "TCP API server stopped");
            }
        });
        tracing::info!(addr = %local, tls = tls.is_some(), "API listening on TCP");
        out.push((local, task));
    }
    Ok(out)
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Connections that finished their TLS handshake. Handshakes run in their
/// own tasks, so a slow client can't hold up the others.
pub struct Accepted {
    rx: mpsc::Receiver<(Box<dyn Io>, SocketAddr)>,
    local: SocketAddr,
}

impl Accepted {
    pub(super) fn spawn(
        tcp: TcpListener,
        tls: Option<TlsAcceptor>,
        shutdown: CancellationToken,
    ) -> Self {
        let local = tcp
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                let (stream, peer) = tokio::select! {
                    r = tcp.accept() => match r {
                        Ok(x) => x,
                        Err(e) => {
                            tracing::debug!(error = %e, "TCP accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            continue;
                        }
                    },
                    () = shutdown.cancelled() => return,
                };
                let _ = stream.set_nodelay(true);
                let tx = tx.clone();
                match tls.clone() {
                    None => {
                        let _ = tx.send((Box::new(stream) as Box<dyn Io>, peer)).await;
                    }
                    Some(acceptor) => {
                        tokio::spawn(async move {
                            match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
                                .await
                            {
                                Ok(Ok(s)) => {
                                    let _ = tx.send((Box::new(s) as Box<dyn Io>, peer)).await;
                                }
                                Ok(Err(e)) => {
                                    tracing::debug!(%peer, error = %e, "TLS handshake failed");
                                }
                                Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
                            }
                        });
                    }
                }
            }
        });
        Self { rx, local }
    }
}

impl Listener for Accepted {
    type Io = Box<dyn Io>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(conn) => conn,
            // The accept loop stopped (shutdown); graceful shutdown ends us.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

impl Connected<IncomingStream<'_, Accepted>> for Peer {
    fn connect_info(stream: IncomingStream<'_, Accepted>) -> Self {
        Peer(*stream.remote_addr())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(listen: &[&str], tls: TlsMode, trust_proxy: bool) -> ApiConfig {
        ApiConfig {
            listen: listen.iter().map(|s| (*s).to_owned()).collect(),
            tls,
            trust_proxy,
            ..ApiConfig::default()
        }
    }

    #[test]
    fn listen_entries_parse_and_plaintext_is_refused_on_networks() {
        assert!(check(&ApiConfig::default()).unwrap().is_empty());
        let a = check(&api(
            &["unix", "tcp://127.0.0.1:7420"],
            TlsMode::Auto,
            false,
        ))
        .unwrap();
        assert_eq!(a, vec!["127.0.0.1:7420".parse().unwrap()]);
        assert!(check(&api(&["tcp://nonsense"], TlsMode::Auto, false)).is_err());
        assert!(check(&api(&["http://127.0.0.1:1"], TlsMode::Auto, false)).is_err());
        // tls = off: only loopback behind a trusted proxy.
        assert!(check(&api(&["tcp://127.0.0.1:7420"], TlsMode::Off, false)).is_err());
        assert!(check(&api(&["tcp://0.0.0.0:7420"], TlsMode::Off, true)).is_err());
        assert!(check(&api(&["tcp://127.0.0.1:7420"], TlsMode::Off, true)).is_ok());
    }
}
