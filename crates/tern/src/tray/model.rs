//! What the tray shows for a given state of tern (personal-agent-tooling-spec
//! §11.4): the icon, the tooltip and the menu. Pure data, no AppKit, so it's
//! tested on every platform.

use crate::daemon::api_lifecycle::Status;

/// The part of `/v1/status` the tray shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub running_runs: usize,
    pub pending_approvals: usize,
    pub active_schedules: usize,
    /// Gateways by name, and whether each is still connected.
    pub gateways: Vec<(String, bool)>,
}

impl From<&Status> for Summary {
    fn from(s: &Status) -> Self {
        Self {
            running_runs: s.running_runs,
            pending_approvals: s.pending_approvals,
            active_schedules: s.active_schedules,
            gateways: s.gateways.iter().map(|g| (g.name.clone(), g.ok)).collect(),
        }
    }
}

/// tern's state as the tray sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Answering `/v1/status`.
    Running(Summary),
    /// Holds its PID lock but doesn't answer: starting up, wedged, or
    /// being restarted by the service manager.
    Unreachable,
    /// The tray asked tern to start and is waiting for it.
    Starting,
    /// The tray asked tern to quit and is waiting for it.
    Stopping,
    Stopped,
}

/// The menu-bar icon: ● running, ○ stopped, ◐ in between (§11.5a), with a
/// ring while a task runs and a badge when something needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Running,
    Busy,
    NeedsYou,
    Between,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Start,
    Quit,
    StartAtLogin,
    CloseIcon,
}

#[cfg_attr(not(all(feature = "tray", target_os = "macos")), allow(dead_code))]
impl Action {
    pub fn id(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Start => "start",
            Self::Quit => "quit",
            Self::StartAtLogin => "login",
            Self::CloseIcon => "close",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        [
            Self::Open,
            Self::Start,
            Self::Quit,
            Self::StartAtLogin,
            Self::CloseIcon,
        ]
        .into_iter()
        .find(|a| a.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A line of text that does nothing when clicked.
    Label(String),
    Separator,
    Button {
        action: Action,
        label: &'static str,
        enabled: bool,
        /// A check mark (`Some`) for toggles.
        checked: Option<bool>,
        /// ⌘ plus this key while the menu is open.
        key: Option<char>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub icon: Icon,
    pub tooltip: String,
    pub items: Vec<Item>,
}

/// What the tray knows besides tern's health.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    /// Whether tern (and the tray) start at login; `None` if unknown.
    pub at_login: Option<bool>,
    /// Gateway names seen while tern ran, so the stopped state can say
    /// what's paused.
    pub gateways: Vec<String>,
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "Telegram" for the gateway `telegram`.
fn gateway_name(name: &str) -> String {
    let mut c = name.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// "A", "A and B", "A, B and C".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn status_text(health: &Health, cx: &Context) -> String {
    match health {
        Health::Running(s) => {
            let mut parts = vec!["tern — running".to_owned()];
            if s.pending_approvals > 0 {
                parts.push(format!(
                    "{} you",
                    plural(s.pending_approvals, "needs", "need")
                ));
            }
            if s.running_runs > 0 {
                parts.push(plural(s.running_runs, "task", "tasks"));
            }
            if s.active_schedules > 0 {
                parts.push(format!(
                    "{} active",
                    plural(s.active_schedules, "routine", "routines")
                ));
            }
            for (name, ok) in &s.gateways {
                if !ok {
                    parts.push(format!("{} disconnected", gateway_name(name)));
                }
            }
            parts.join(" · ")
        }
        Health::Unreachable => "tern isn't answering…".to_owned(),
        Health::Starting => "Starting tern…".to_owned(),
        Health::Stopping => "Quitting tern…".to_owned(),
        Health::Stopped => {
            let mut paused = vec!["Routines".to_owned()];
            paused.extend(cx.gateways.iter().map(|g| gateway_name(g)));
            format!("tern is stopped. {} are paused.", and_list(&paused))
        }
    }
}

/// The "Quit tern?" question when tasks are in progress (§11.5a).
pub fn quit_prompt(runs: &[String], gateways: &[String]) -> String {
    let names: Vec<String> = runs
        .iter()
        .map(|t| format!("\u{201c}{t}\u{201d}"))
        .collect();
    let mut paused = vec!["Routines".to_owned()];
    paused.extend(gateways.iter().map(|g| gateway_name(g)));
    format!(
        "{} in progress ({}). {} will pick up where {} left off next time tern starts.\n\
         {} will be paused until then.",
        if runs.len() == 1 {
            "1 task is".to_owned()
        } else {
            format!("{} tasks are", runs.len())
        },
        names.join(", "),
        if runs.len() == 1 { "It" } else { "They" },
        if runs.len() == 1 { "it" } else { "they" },
        and_list(&paused),
    )
}

pub fn icon(health: &Health) -> Icon {
    match health {
        Health::Running(s) if s.pending_approvals > 0 => Icon::NeedsYou,
        Health::Running(s) if s.running_runs > 0 => Icon::Busy,
        Health::Running(_) => Icon::Running,
        Health::Unreachable | Health::Starting | Health::Stopping => Icon::Between,
        Health::Stopped => Icon::Stopped,
    }
}

fn glyph(icon: Icon) -> char {
    match icon {
        Icon::Running | Icon::Busy | Icon::NeedsYou => '●',
        Icon::Between => '◐',
        Icon::Stopped => '○',
    }
}

/// The menu for `health`. The items whose features don't exist yet (Needs
/// you, Recent posts, New message…, Initiative) are left out; see
/// DECISIONS.md, T8b.
pub fn menu(health: &Health, cx: &Context) -> Menu {
    let icon = icon(health);
    let text = status_text(health, cx);
    let button = |action, label, enabled, key| Item::Button {
        action,
        label,
        enabled,
        checked: None,
        key,
    };
    let login = Item::Button {
        action: Action::StartAtLogin,
        label: "Start at login",
        enabled: !matches!(health, Health::Starting | Health::Stopping),
        checked: Some(cx.at_login == Some(true)),
        key: None,
    };
    let mut items = vec![
        Item::Label(format!("{} {text}", glyph(icon))),
        Item::Separator,
    ];
    match health {
        Health::Running(_) | Health::Unreachable | Health::Starting => {
            items.push(button(Action::Open, "Open tern", true, Some('o')));
            items.push(Item::Separator);
            items.push(login);
            items.push(Item::Separator);
            // Unreachable still quits: `tern stop` escalates (§11.5a).
            let can_quit = !matches!(health, Health::Starting);
            items.push(button(Action::Quit, "Quit tern", can_quit, Some('q')));
        }
        Health::Stopping => {
            items.push(button(Action::Quit, "Quit tern", false, Some('q')));
        }
        Health::Stopped => {
            items.push(button(Action::Start, "Start tern", true, None));
            items.push(login);
            items.push(Item::Separator);
            items.push(button(Action::CloseIcon, "Close this icon", true, None));
        }
    }
    Menu {
        icon,
        tooltip: text,
        items,
    }
}

/// The icon's pixels: a black shape with alpha, `size` × `size` RGBA, for
/// a template image (macOS tints it to suit the menu bar).
pub fn icon_rgba(icon: Icon, size: u32) -> Vec<u8> {
    let s = size as f32;
    let (cx, cy) = (s / 2.0, s / 2.0);
    // The disc, its outline when hollow, and the busy ring around it.
    let r = s * 0.28;
    let stroke = (s * 0.07).max(1.0);
    let ring = s * 0.44;
    let badge = (s * 0.80, s * 0.20, s * 0.14);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            // Coverage of a filled circle of radius `rad` at this pixel,
            // feathered over one pixel.
            let fill = |dist: f32, rad: f32| (rad - dist + 0.5).clamp(0.0, 1.0);
            let line =
                |dist: f32, rad: f32| (stroke / 2.0 - (dist - rad).abs() + 0.5).clamp(0.0, 1.0);
            let disc = fill(d, r);
            let outline = line(d, r - stroke / 2.0);
            let a: f32 = match icon {
                Icon::Running => disc,
                Icon::Busy => disc.max(line(d, ring - stroke / 2.0)),
                Icon::NeedsYou => {
                    let bd = ((px - badge.0).powi(2) + (py - badge.1).powi(2)).sqrt();
                    disc.max(fill(bd, badge.2))
                }
                Icon::Between => {
                    if px < cx {
                        disc
                    } else {
                        outline
                    }
                }
                Icon::Stopped => outline,
            };
            out.extend_from_slice(&[0, 0, 0, (a * 255.0).round() as u8]);
        }
    }
    out
}

/// The icon as a StatusNotifierItem pixmap: ARGB32 in network byte order
/// (bytes A, R, G, B), `size` × `size`, drawn in `rgb`. The black template
/// icon would vanish on a dark panel, and Linux has no template images, so
/// the shape is tinted a mid grey-blue that reads on light and dark.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn icon_argb(icon: Icon, size: u32, rgb: [u8; 3]) -> Vec<u8> {
    icon_rgba(icon, size)
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[3], rgb[0], rgb[1], rgb[2]])
        .collect()
}

/// Menu labels with `_` marking an access key (the dbusmenu convention):
/// a literal underscore is written `__`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn escape_mnemonic(label: &str) -> String {
    label.replace('_', "__")
}

/// The dialog program on Linux (neither is guaranteed to be installed).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogTool {
    Zenity,
    Kdialog,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl DialogTool {
    pub fn program(self) -> &'static str {
        match self {
            Self::Zenity => "zenity",
            Self::Kdialog => "kdialog",
        }
    }

    /// Arguments for a yes/no question; the program exits 0 for yes.
    pub fn confirm_args(self, title: &str, body: &str, yes: &str, no: &str) -> Vec<String> {
        let s = |s: &str| s.to_owned();
        match self {
            Self::Zenity => vec![
                s("--question"),
                s("--title"),
                s(title),
                s("--text"),
                // zenity reads Pango markup in the text.
                markup_escape(body),
                s("--ok-label"),
                s(yes),
                s("--cancel-label"),
                s(no),
            ],
            Self::Kdialog => vec![
                s("--title"),
                s(title),
                s("--yes-label"),
                s(yes),
                s("--no-label"),
                s(no),
                s("--yesno"),
                s(body),
            ],
        }
    }

    /// Arguments for a message with an OK button.
    pub fn message_args(self, title: &str, body: &str) -> Vec<String> {
        let s = |s: &str| s.to_owned();
        match self {
            Self::Zenity => vec![
                s("--info"),
                s("--title"),
                s(title),
                s("--text"),
                markup_escape(body),
            ],
            Self::Kdialog => vec![s("--title"), s(title), s("--msgbox"), s(body)],
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn markup_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_pixmap_is_argb_in_network_order() {
        let size = 36;
        let argb = icon_argb(Icon::Running, size, [10, 20, 30]);
        assert_eq!(argb.len(), (size * size * 4) as usize);
        let centre = ((size / 2 * size + size / 2) * 4) as usize;
        assert_eq!(&argb[centre..centre + 4], [255, 10, 20, 30]);
        // Transparent corners keep the colour but have no alpha.
        assert_eq!(&argb[..4], [0, 10, 20, 30]);
    }

    #[test]
    fn underscores_in_labels_are_literal() {
        assert_eq!(escape_mnemonic("my_gateway down"), "my__gateway down");
        assert_eq!(escape_mnemonic("Open tern"), "Open tern");
    }

    #[test]
    fn dialog_arguments_suit_each_tool() {
        let z = DialogTool::Zenity.confirm_args("Quit tern?", "a < b & c", "Quit tern", "Cancel");
        assert_eq!(z[0], "--question");
        assert!(z.contains(&"a &lt; b &amp; c".to_owned()));
        assert!(z.windows(2).any(|w| w == ["--ok-label", "Quit tern"]));
        let k = DialogTool::Kdialog.confirm_args("Quit tern?", "body", "Quit tern", "Cancel");
        assert_eq!(&k[k.len() - 2..], ["--yesno", "body"]);
        assert_eq!(
            DialogTool::Kdialog.message_args("T", "B"),
            ["--title", "T", "--msgbox", "B"]
        );
        assert_eq!(
            DialogTool::Zenity.message_args("T", "B"),
            ["--info", "--title", "T", "--text", "B"]
        );
    }

    fn running(runs: usize, approvals: usize, routines: usize) -> Health {
        Health::Running(Summary {
            running_runs: runs,
            pending_approvals: approvals,
            active_schedules: routines,
            gateways: vec![("telegram".into(), true)],
        })
    }

    fn actions(m: &Menu) -> Vec<(Action, bool)> {
        m.items
            .iter()
            .filter_map(|i| match i {
                Item::Button {
                    action, enabled, ..
                } => Some((*action, *enabled)),
                _ => None,
            })
            .collect()
    }

    fn first_line(m: &Menu) -> &str {
        match &m.items[0] {
            Item::Label(t) => t,
            other => panic!("first item is {other:?}"),
        }
    }

    #[test]
    fn running_menu_opens_and_quits() {
        let cx = Context {
            at_login: Some(true),
            gateways: vec![],
        };
        let m = menu(&running(0, 0, 3), &cx);
        assert_eq!(m.icon, Icon::Running);
        assert_eq!(first_line(&m), "● tern — running · 3 routines active");
        assert_eq!(m.tooltip, "tern — running · 3 routines active");
        assert_eq!(
            actions(&m),
            [
                (Action::Open, true),
                (Action::StartAtLogin, true),
                (Action::Quit, true)
            ]
        );
        assert!(m.items.contains(&Item::Button {
            action: Action::StartAtLogin,
            label: "Start at login",
            enabled: true,
            checked: Some(true),
            key: None,
        }));
    }

    #[test]
    fn icon_follows_work_and_approvals() {
        let cx = Context::default();
        let busy = menu(&running(2, 0, 1), &cx);
        assert_eq!(busy.icon, Icon::Busy);
        assert_eq!(busy.tooltip, "tern — running · 2 tasks · 1 routine active");
        let needs = menu(&running(1, 1, 0), &cx);
        assert_eq!(needs.icon, Icon::NeedsYou);
        assert_eq!(needs.tooltip, "tern — running · 1 needs you · 1 task");
        assert_eq!(
            menu(&running(0, 2, 0), &cx).tooltip,
            "tern — running · 2 need you"
        );
    }

    #[test]
    fn a_disconnected_gateway_is_named() {
        let h = Health::Running(Summary {
            gateways: vec![("telegram".into(), false), ("slack".into(), true)],
            ..Summary::default()
        });
        assert_eq!(
            menu(&h, &Context::default()).tooltip,
            "tern — running · Telegram disconnected"
        );
    }

    #[test]
    fn stopped_menu_starts_or_closes_and_says_what_is_paused() {
        let cx = Context {
            at_login: Some(false),
            gateways: vec!["telegram".into()],
        };
        let m = menu(&Health::Stopped, &cx);
        assert_eq!(m.icon, Icon::Stopped);
        assert_eq!(
            first_line(&m),
            "○ tern is stopped. Routines and Telegram are paused."
        );
        assert_eq!(
            actions(&m),
            [
                (Action::Start, true),
                (Action::StartAtLogin, true),
                (Action::CloseIcon, true)
            ]
        );
        assert_eq!(
            menu(&Health::Stopped, &Context::default()).tooltip,
            "tern is stopped. Routines are paused."
        );
        let three = Context {
            at_login: None,
            gateways: vec!["telegram".into(), "slack".into()],
        };
        assert_eq!(
            menu(&Health::Stopped, &three).tooltip,
            "tern is stopped. Routines, Telegram and Slack are paused."
        );
    }

    #[test]
    fn in_between_states_show_half_icon_and_block_double_actions() {
        let cx = Context::default();
        let m = menu(&Health::Unreachable, &cx);
        assert_eq!(m.icon, Icon::Between);
        assert_eq!(first_line(&m), "◐ tern isn't answering…");
        // A wedged tern can still be quit (by force if need be).
        assert!(actions(&m).contains(&(Action::Quit, true)));

        let m = menu(&Health::Starting, &cx);
        assert_eq!(m.icon, Icon::Between);
        assert!(actions(&m).contains(&(Action::Quit, false)));
        assert!(actions(&m).contains(&(Action::StartAtLogin, false)));

        let m = menu(&Health::Stopping, &cx);
        assert_eq!(first_line(&m), "◐ Quitting tern…");
        assert_eq!(actions(&m), [(Action::Quit, false)]);
    }

    #[test]
    fn quit_prompt_names_the_tasks() {
        assert_eq!(
            quit_prompt(&["Summarize the Q3 report".into()], &["telegram".into()]),
            "1 task is in progress (\u{201c}Summarize the Q3 report\u{201d}). It will pick up \
             where it left off next time tern starts.\nRoutines and Telegram will be paused \
             until then."
        );
        assert!(quit_prompt(&["a".into(), "b".into()], &[]).starts_with(
            "2 tasks are in progress (\u{201c}a\u{201d}, \u{201c}b\u{201d}). They will"
        ));
    }

    #[test]
    fn action_ids_round_trip() {
        for a in [
            Action::Open,
            Action::Start,
            Action::Quit,
            Action::StartAtLogin,
            Action::CloseIcon,
        ] {
            assert_eq!(Action::from_id(a.id()), Some(a));
        }
        assert_eq!(Action::from_id("nope"), None);
    }

    #[test]
    fn icons_are_distinct_shapes() {
        let size = 36;
        let alpha = |i: Icon, x: u32, y: u32| icon_rgba(i, size)[((y * size + x) * 4 + 3) as usize];
        let c = size / 2;
        // Filled centre when running, hollow when stopped.
        assert_eq!(alpha(Icon::Running, c, c), 255);
        assert_eq!(alpha(Icon::Stopped, c, c), 0);
        // Half and half in between.
        assert_eq!(alpha(Icon::Between, c - 4, c), 255);
        assert_eq!(alpha(Icon::Between, c + 4, c), 0);
        // The busy ring and the badge sit outside the disc.
        assert_eq!(alpha(Icon::Running, c, 2), 0);
        assert!(alpha(Icon::Busy, c, 2) > 0);
        assert_eq!(alpha(Icon::Running, 29, 7), 0);
        assert_eq!(alpha(Icon::NeedsYou, 29, 7), 255);
        for i in [
            Icon::Running,
            Icon::Busy,
            Icon::NeedsYou,
            Icon::Between,
            Icon::Stopped,
        ] {
            let px = icon_rgba(i, size);
            assert_eq!(px.len(), (size * size * 4) as usize);
            assert!(px.chunks(4).all(|p| p[..3] == [0, 0, 0]));
        }
    }
}
