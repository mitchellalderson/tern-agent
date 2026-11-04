//! The tray on Linux (T8c): a StatusNotifierItem over D-Bus, through `ksni`
//! (pure Rust on zbus, so no GTK and it builds into static binaries). KDE
//! and most desktops show these icons; GNOME needs the AppIndicator
//! extension.
//!
//! One tokio runtime on the main thread runs everything: ksni's D-Bus
//! tasks, the shared worker (`worker::run`, the same one the macOS tray
//! uses) and the small tasks that carry out its `Update`s. Menu clicks
//! arrive on ksni's tasks and go to the worker as `Command`s. The menu
//! itself comes from `model::menu`, so what it says is tested without a
//! desktop.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{Handle, MenuItem, Tray, TrayMethods};
use tern_core::TernHome;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::model::{self, DialogTool, Health, Item, Menu};
use super::worker::{self, Command, DESKTOP_EXE, Update};

/// Pixels per side of the two icon sizes offered to the panel.
const ICON_SIZES: [u32; 2] = [22, 44];
/// A mid grey-blue that reads on light and dark panels.
const ICON_RGB: [u8; 3] = [0x8a, 0x9b, 0xb5];

struct TernTray {
    menu: Menu,
    commands: UnboundedSender<Command>,
}

impl Tray for TernTray {
    // A click opens the menu, as the macOS icon does.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "tern".into()
    }

    fn title(&self) -> String {
        "tern".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        ICON_SIZES
            .iter()
            .map(|&px| ksni::Icon {
                width: px as i32,
                height: px as i32,
                data: model::icon_argb(self.menu.icon, px, ICON_RGB),
            })
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.menu.tooltip.clone(),
            ..ksni::ToolTip::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        self.menu
            .items
            .iter()
            .map(|item| match item {
                Item::Label(text) => StandardItem {
                    label: model::escape_mnemonic(text),
                    enabled: false,
                    ..StandardItem::default()
                }
                .into(),
                Item::Separator => MenuItem::Separator,
                Item::Button {
                    action,
                    label,
                    enabled,
                    checked,
                    ..
                } => {
                    let tx = self.commands.clone();
                    let action = *action;
                    let activate = Box::new(move |_: &mut Self| {
                        let _ = tx.send(Command::Menu(action));
                    });
                    match checked {
                        Some(on) => CheckmarkItem {
                            label: (*label).to_owned(),
                            enabled: *enabled,
                            checked: *on,
                            activate,
                            ..CheckmarkItem::default()
                        }
                        .into(),
                        None => StandardItem {
                            label: (*label).to_owned(),
                            enabled: *enabled,
                            activate,
                            ..StandardItem::default()
                        }
                        .into(),
                    }
                }
            })
            .collect()
    }
}

pub fn run(home: &TernHome) -> anyhow::Result<ExitCode> {
    if !super::desktop_session() {
        bail!(
            "tern tray needs a desktop session (no display or D-Bus session bus here, e.g. over \
             SSH); log in to the desktop to see the tray icon"
        );
    }
    home.ensure_layout()?;
    // One icon per user: a second tray (the autostart entry and a
    // hand-started one, say) just leaves.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(super::lock_path(home))
        .context("opening tray.lock")?;
    if lock.try_lock().is_err() {
        println!("tern tray is already running.");
        return Ok(ExitCode::SUCCESS);
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the tray runtime")?;
    let home = home.clone();
    let result = rt.block_on(async move {
        let (tx, rx) = unbounded_channel();
        let tray = TernTray {
            menu: model::menu(&Health::Starting, &model::Context::default()),
            commands: tx.clone(),
        };
        let handle = tray.spawn().await.map_err(|e| {
            anyhow::anyhow!(
                "couldn't show the tray icon: {e}\n\
                 (KDE and most desktops show these icons; GNOME needs the AppIndicator \
                 extension, \"AppIndicator and KStatusNotifierItem Support\")"
            )
        })?;
        worker::run(home, ui(handle, tx), rx).await;
        anyhow::Ok(())
    });
    drop(lock);
    result?;
    Ok(ExitCode::SUCCESS)
}

/// The worker's way to the screen: each `Update` becomes a small task.
fn ui(handle: Handle<TernTray>, commands: UnboundedSender<Command>) -> impl Fn(Update) {
    move |u| match u {
        Update::Menu(m) => {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle.update(|t| t.menu = m).await;
            });
        }
        Update::ConfirmQuit(body) => {
            let commands = commands.clone();
            tokio::spawn(async move {
                // Without a dialog program there's nothing to ask with.
                // Quitting is safe (runs pick up where they left off), and
                // not quitting would look like a dead button.
                let yes = match dialog_tool() {
                    Some(tool) => {
                        let args = tool.confirm_args("Quit tern?", &body, "Quit tern", "Cancel");
                        run_dialog(tool, &args).await
                    }
                    None => true,
                };
                if yes {
                    let _ = commands.send(Command::QuitConfirmed);
                }
            });
        }
        Update::Alert { title, body } => {
            tokio::spawn(async move {
                match dialog_tool() {
                    Some(tool) => {
                        run_dialog(tool, &tool.message_args(&title, &body)).await;
                    }
                    None => tracing::warn!(%title, %body, "tray alert (no zenity or kdialog)"),
                }
            });
        }
        Update::QuitApp => {
            tokio::spawn(async {
                let _ = tokio::process::Command::new("pkill")
                    .args(["-x", DESKTOP_EXE])
                    .status()
                    .await;
            });
        }
        Update::Exit => {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle.shutdown().await;
                std::process::exit(0);
            });
        }
    }
}

/// The first of `zenity` and `kdialog` on `PATH`.
fn dialog_tool() -> Option<DialogTool> {
    let path = std::env::var_os("PATH")?;
    [DialogTool::Zenity, DialogTool::Kdialog]
        .into_iter()
        .find(|t| {
            std::env::split_paths(&path)
                .map(|d| d.join(t.program()))
                .any(|p: PathBuf| p.is_file())
        })
}

/// Run a dialog; true when it exits 0 (OK or Yes).
async fn run_dialog(tool: DialogTool, args: &[String]) -> bool {
    tokio::process::Command::new(tool.program())
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}
