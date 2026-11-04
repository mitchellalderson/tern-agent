//! The tray's daemon side: watches tern over the local socket and carries
//! out menu actions, on a background thread. It tells the UI what to show
//! through `Update`s and hears back through `Command`s.

use std::path::PathBuf;
use std::time::Duration;

use tern_core::TernHome;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, sleep, sleep_until};

use super::model::{self, Action, Context, Health, Menu, Summary};
use crate::daemon::api_lifecycle::{ShutdownReply, Status};
use crate::{lifecycle, service};

/// Worker → UI.
pub enum Update {
    Menu(Menu),
    /// Tasks are in progress: ask before quitting (§11.5a), with this text.
    ConfirmQuit(String),
    Alert {
        title: String,
        body: String,
    },
    /// Close the desktop app if it's open.
    QuitApp,
    Exit,
}

/// UI → worker.
pub enum Command {
    Menu(Action),
    QuitConfirmed,
}

/// How often to look for a tern that isn't running yet.
const STOPPED_POLL: Duration = Duration::from_secs(2);
/// Ask for the status this long after the first of a burst of events.
const SETTLE: Duration = Duration::from_millis(500);
/// And at least this often, in case an event was missed.
const IDLE_REFRESH: Duration = Duration::from_secs(60);

struct Worker<U> {
    home: TernHome,
    exe: PathBuf,
    ui: U,
    health: Health,
    cx: Context,
    shown: Option<Menu>,
}

pub async fn run<U: Fn(Update)>(home: TernHome, ui: U, mut rx: UnboundedReceiver<Command>) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "tern".into());
    let mut w = Worker {
        home,
        exe,
        ui,
        health: Health::Starting,
        cx: Context::default(),
        shown: None,
    };
    w.refresh_login();
    loop {
        w.probe().await;
        if matches!(w.health, Health::Running(_)) {
            w.follow(&mut rx).await;
        } else {
            tokio::select! {
                () = sleep(STOPPED_POLL) => {}
                c = rx.recv() => match c {
                    Some(c) => w.handle(c).await,
                    None => return,
                },
            }
        }
    }
}

fn clients(home: &TernHome) -> Option<(reqwest::Client, reqwest::Client)> {
    let socket = home.socket_connect_path().ok()?;
    if !socket.exists() {
        return None;
    }
    let quick = reqwest::Client::builder()
        .unix_socket(socket.clone())
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let stream = reqwest::Client::builder()
        .unix_socket(socket)
        .connect_timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    Some((quick, stream))
}

async fn status(http: &reqwest::Client) -> Option<Status> {
    let r = http.get("http://tern/v1/status").send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.json().await.ok()
}

impl<U: Fn(Update)> Worker<U> {
    fn set(&mut self, health: Health) {
        if let Health::Running(s) = &health {
            self.cx.gateways = s.gateways.iter().map(|(n, _)| n.clone()).collect();
        }
        self.health = health;
        self.show();
    }

    fn show(&mut self) {
        let menu = model::menu(&self.health, &self.cx);
        if self.shown.as_ref() != Some(&menu) {
            self.shown = Some(menu.clone());
            (self.ui)(Update::Menu(menu));
        }
    }

    fn refresh_login(&mut self) {
        self.cx.at_login = service::state().map(|s| s.at_login);
        self.show();
    }

    /// Where tern stands now.
    async fn probe(&mut self) {
        if let Some((http, _)) = clients(&self.home)
            && let Some(s) = status(&http).await
        {
            self.set(Health::Running(Summary::from(&s)));
            return;
        }
        let health = if lifecycle::running_pid(&self.home).is_some() {
            Health::Unreachable
        } else {
            Health::Stopped
        };
        self.set(health);
    }

    /// While tern runs: wait on its event stream, and re-read the status
    /// shortly after something happens. Returns when the stream ends.
    async fn follow(&mut self, rx: &mut UnboundedReceiver<Command>) {
        let Some((http, stream)) = clients(&self.home) else {
            return;
        };
        let Ok(mut events) = stream
            .get("http://tern/v1/events")
            .header("X-Tern-Client", concat!("tray/", env!("CARGO_PKG_VERSION")))
            .send()
            .await
        else {
            return;
        };
        let mut next = Instant::now() + IDLE_REFRESH;
        let mut dirty = false;
        loop {
            tokio::select! {
                chunk = events.chunk() => match chunk {
                    Ok(Some(_)) => {
                        if !dirty {
                            dirty = true;
                            next = Instant::now() + SETTLE;
                        }
                    }
                    _ => return,
                },
                () = sleep_until(next) => {
                    let Some(s) = status(&http).await else { return };
                    self.set(Health::Running(Summary::from(&s)));
                    dirty = false;
                    next = Instant::now() + IDLE_REFRESH;
                }
                c = rx.recv() => match c {
                    Some(c) => {
                        self.handle(c).await;
                        if !matches!(self.health, Health::Running(_)) {
                            return;
                        }
                    }
                    None => return,
                },
            }
        }
    }

    async fn handle(&mut self, c: Command) {
        match c {
            Command::Menu(Action::Open) => {
                if let Err(e) = open_app() {
                    self.alert("Couldn't open tern", &format!("{e:#}"));
                }
            }
            Command::Menu(Action::Start) => {
                self.set(Health::Starting);
                if let Err(e) = self.tern(&["start"]).await {
                    self.alert("tern didn't start", &e);
                }
                self.probe().await;
            }
            Command::Menu(Action::Quit) => match self.dry_run().await {
                Some(runs) if !runs.is_empty() => (self.ui)(Update::ConfirmQuit(
                    model::quit_prompt(&runs, &self.cx.gateways),
                )),
                _ => self.quit().await,
            },
            Command::QuitConfirmed => self.quit().await,
            Command::Menu(Action::StartAtLogin) => {
                let on = self.cx.at_login != Some(true);
                let args: &[&str] = if on {
                    &["service", "install", "--tray"]
                } else {
                    &["service", "uninstall"]
                };
                if let Err(e) = self.tern(args).await {
                    self.alert("Couldn't change Start at login", &e);
                }
                self.refresh_login();
            }
            Command::Menu(Action::CloseIcon) => (self.ui)(Update::Exit),
        }
    }

    async fn dry_run(&self) -> Option<Vec<String>> {
        let (http, _) = clients(&self.home)?;
        let r = http
            .post("http://tern/v1/shutdown?dry_run=1")
            .send()
            .await
            .ok()?;
        let reply: ShutdownReply = r.json().await.ok()?;
        Some(reply.runs.into_iter().map(|r| r.title).collect())
    }

    /// Quit tern completely: stop it the way `tern stop` does, close the
    /// app, and remove the icon.
    async fn quit(&mut self) {
        self.set(Health::Stopping);
        match self.tern(&["stop"]).await {
            Ok(()) => {
                (self.ui)(Update::QuitApp);
                (self.ui)(Update::Exit);
            }
            Err(e) => {
                self.alert("tern didn't quit", &e);
                self.probe().await;
            }
        }
    }

    fn alert(&self, title: &str, body: &str) {
        (self.ui)(Update::Alert {
            title: title.to_owned(),
            body: body.to_owned(),
        });
    }

    /// Run this binary with `args`; on failure, what it said.
    async fn tern(&self, args: &[&str]) -> Result<(), String> {
        let out = tokio::process::Command::new(&self.exe)
            .args(args)
            .env("TERN_HOME", self.home.root())
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().trim_start_matches("error: ");
        Err(if err.is_empty() {
            format!("`tern {}` failed", args.join(" "))
        } else {
            err.to_owned()
        })
    }
}

/// Open the desktop app: the `tern.app` this tray is bundled in, else
/// whichever one Launch Services knows.
fn open_app() -> anyhow::Result<()> {
    let host = std::env::current_exe()
        .ok()
        .and_then(|e| e.canonicalize().ok())
        .and_then(|e| service::bundle::host_app(&e));
    let mut cmd = std::process::Command::new("open");
    match host {
        Some(app) => cmd.arg(app),
        None => cmd.args(["-b", service::bundle::DESKTOP_ID]),
    };
    let out = cmd.stdin(std::process::Stdio::null()).output()?;
    if !out.status.success() {
        anyhow::bail!("the tern app isn't installed (put tern.app in Applications)");
    }
    Ok(())
}
