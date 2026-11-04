//! The tray's AppKit side. The icon, menu and dialogs live on the main
//! thread, which runs `NSApplication`; the worker thread posts `Update`s to
//! it through the main dispatch queue, and menu clicks go back as
//! `Command`s.

use std::cell::RefCell;
use std::process::ExitCode;

use anyhow::{Context as _, bail};
use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSRunningApplication,
};
use objc2_foundation::NSString;
use tern_core::TernHome;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tray_icon::menu::accelerator::{Accelerator, Code, Modifiers};
use tray_icon::menu::{
    CheckMenuItem, IsMenuItem, Menu as NativeMenu, MenuEvent, MenuItem, PredefinedMenuItem,
};
use tray_icon::{Icon as NativeIcon, TrayIcon, TrayIconBuilder};

use super::model::{self, Action, Item, Menu};
use super::worker::{self, Command, Update};
use crate::service::bundle::DESKTOP_ID;

/// Pixels per side: 18 pt at 2×.
const ICON_PX: u32 = 36;

thread_local! {
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    static COMMANDS: RefCell<Option<UnboundedSender<Command>>> = const { RefCell::new(None) };
}

pub fn run(home: &TernHome) -> anyhow::Result<ExitCode> {
    let Some(mtm) = MainThreadMarker::new() else {
        bail!("tern tray must run on the main thread");
    };
    gui_session()?;
    home.ensure_layout()?;
    // One icon per user: a second tray (the login item and a hand-started
    // one, say) just leaves.
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

    let app = NSApplication::sharedApplication(mtm);
    // No Dock icon and no menu bar of its own (like `LSUIElement`).
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let (tx, rx) = unbounded_channel();
    let clicks = tx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Some(a) = Action::from_id(e.id.as_ref()) {
            let _ = clicks.send(Command::Menu(a));
        }
    }));
    COMMANDS.with(|c| *c.borrow_mut() = Some(tx));

    let home = home.clone();
    std::thread::Builder::new()
        .name("tray-worker".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "tray worker runtime");
                    std::process::exit(1);
                }
            };
            rt.block_on(worker::run(home, post, rx));
        })
        .context("starting the tray worker")?;

    app.run();
    drop(lock);
    Ok(ExitCode::SUCCESS)
}

/// The tray needs a logged-in desktop: not SSH, not a background session.
fn gui_session() -> anyhow::Result<()> {
    let out = std::process::Command::new("launchctl")
        .arg("managername")
        .output()
        .context("running launchctl managername")?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if name != "Aqua" {
        bail!(
            "tern tray needs a desktop session (this is a {} session, e.g. over SSH); \
             log in to the Mac to see the menu-bar icon",
            if name.is_empty() { "non-GUI" } else { &name }
        );
    }
    Ok(())
}

/// From the worker: run `u` on the main thread.
fn post(u: Update) {
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        apply(mtm, u);
    });
}

fn apply(mtm: MainThreadMarker, u: Update) {
    match u {
        Update::Menu(m) => show(&m),
        Update::ConfirmQuit(body) => {
            if ask(mtm, "Quit tern?", &body, &["Quit tern", "Cancel"]) {
                COMMANDS.with(|c| {
                    if let Some(tx) = c.borrow().as_ref() {
                        let _ = tx.send(Command::QuitConfirmed);
                    }
                });
            }
        }
        Update::Alert { title, body } => {
            ask(mtm, &title, &body, &["OK"]);
        }
        Update::QuitApp => {
            let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(
                &NSString::from_str(DESKTOP_ID),
            );
            for app in apps.iter() {
                app.terminate();
            }
        }
        Update::Exit => std::process::exit(0),
    }
}

/// A dialog in front of everything; true for the first button.
fn ask(mtm: MainThreadMarker, title: &str, body: &str, buttons: &[&str]) -> bool {
    let app = NSApplication::sharedApplication(mtm);
    // `activate()` is macOS 14+; this still works on 13.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    for b in buttons {
        alert.addButtonWithTitle(&NSString::from_str(b));
    }
    alert.runModal() == NSAlertFirstButtonReturn
}

fn native_icon(icon: model::Icon) -> Option<NativeIcon> {
    NativeIcon::from_rgba(model::icon_rgba(icon, ICON_PX), ICON_PX, ICON_PX).ok()
}

fn native_menu(m: &Menu) -> NativeMenu {
    let menu = NativeMenu::new();
    for item in &m.items {
        let built: Box<dyn IsMenuItem> = match item {
            Item::Label(text) => Box::new(MenuItem::new(text, false, None)),
            Item::Separator => Box::new(PredefinedMenuItem::separator()),
            Item::Button {
                action,
                label,
                enabled,
                checked,
                key,
            } => {
                let accel = key.and_then(|k| {
                    let code = match k {
                        'o' => Code::KeyO,
                        'q' => Code::KeyQ,
                        _ => return None,
                    };
                    Some(Accelerator::new(Modifiers::META, code))
                });
                match checked {
                    Some(on) => Box::new(CheckMenuItem::with_id(
                        action.id(),
                        label,
                        *enabled,
                        *on,
                        accel,
                    )),
                    None => Box::new(MenuItem::with_id(action.id(), label, *enabled, accel)),
                }
            }
        };
        if let Err(e) = menu.append(built.as_ref()) {
            tracing::warn!(error = %e, "tray menu item");
        }
    }
    menu
}

fn show(m: &Menu) {
    TRAY.with(|t| {
        let mut t = t.borrow_mut();
        match t.as_ref() {
            Some(tray) => {
                tray.set_menu(Some(Box::new(native_menu(m))));
                let _ = tray.set_icon_templated(native_icon(m.icon));
                let _ = tray.set_tooltip(Some(&m.tooltip));
            }
            None => {
                let mut b = TrayIconBuilder::new()
                    .with_menu(Box::new(native_menu(m)))
                    .with_tooltip(&m.tooltip);
                if let Some(icon) = native_icon(m.icon) {
                    b = b.with_icon_templated(icon);
                }
                match b.build() {
                    Ok(tray) => *t = Some(tray),
                    Err(e) => tracing::error!(error = %e, "creating the tray icon"),
                }
            }
        }
    });
}
