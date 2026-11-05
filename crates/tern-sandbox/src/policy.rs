//! What the host backend allows a command to touch, and the per-platform
//! plans built from it. See `docs/sandbox-design.md`.

use std::path::{Path, PathBuf};

use crate::toolchain::{self, ToolchainGrant};

/// What to do when the OS sandbox isn't available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Confine {
    /// Confine when possible; otherwise run unconfined and warn once.
    #[default]
    Auto,
    /// Refuse to run commands unless they can be confined.
    Required,
    /// Never confine.
    Off,
}

#[derive(Debug, Clone, Default)]
pub struct HostPolicy {
    pub confine: Confine,
    /// Allow IP networking.
    pub network: bool,
    /// Extra read-only paths (already `~`-expanded). Missing ones are skipped.
    pub read_paths: Vec<PathBuf>,
    /// Extra writable paths (already `~`-expanded).
    pub write_paths: Vec<PathBuf>,
    /// Never readable or writable, whatever else allows it (tern's data dir).
    pub deny_paths: Vec<PathBuf>,
    /// Linux: the binary that applies the sandbox (`<helper> __sandbox-exec`).
    /// `None` means the current executable.
    pub helper: Option<PathBuf>,
}

impl HostPolicy {
    /// The policy commands get unless config says otherwise: confined, with
    /// network, nothing extra.
    pub fn confined() -> Self {
        Self {
            network: true,
            ..Self::default()
        }
    }

    pub fn unconfined() -> Self {
        Self {
            confine: Confine::Off,
            network: true,
            ..Self::default()
        }
    }
}

/// Resolve symlinks (`/tmp` → `/private/tmp` on macOS) so the sandbox matches
/// the paths the kernel sees. A missing path is kept as given.
pub(crate) fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| canonical(Path::new(&h)))
}

/// Whether `p` is in a private area: `$HOME`, or on macOS `/Users` and
/// `/Volumes`, where reads are denied unless something grants them.
fn in_private_area(p: &Path, home: Option<&Path>) -> bool {
    home.is_some_and(|h| p.starts_with(h))
        || (cfg!(target_os = "macos")
            && toolchain::MAC_PRIVATE_ROOTS
                .iter()
                .any(|r| p.starts_with(canonical(Path::new(r)))))
}

/// `PATH` entries worth granting read on: absolute, existing, not `/` or
/// `$HOME` itself, and not in a private area (a `PATH` entry under `$HOME`
/// is readable only through a toolchain grant or a read-only folder).
fn path_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|p| p.is_absolute() && p.exists())
        .map(|p| canonical(&p))
        .filter(|p| p.parent().is_some() && Some(p.as_path()) != home)
        .filter(|p| !in_private_area(p, home))
        .collect()
}

/// Where a sandboxed command can write: the workspace, the temp dirs and
/// `write_paths` (canonical, so they compare with canonical paths).
pub(crate) fn writable_roots(policy: &HostPolicy, workspace: &Path) -> Vec<PathBuf> {
    let mut w = vec![
        canonical(workspace),
        canonical(Path::new("/tmp")),
        canonical(Path::new("/var/tmp")),
        canonical(Path::new("/dev/shm")),
    ];
    if let Some(t) = std::env::var_os("TMPDIR").filter(|t| !t.is_empty()) {
        w.push(canonical(Path::new(&t)));
    }
    w.extend(policy.write_paths.iter().map(|p| canonical(p)));
    w
}

// Each platform uses half of what follows; both halves are unit-tested everywhere.
/// Paths a command may read and write on Linux, where Landlock can only
/// grant. A grant that would expose a denied path is dropped.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinuxGrants {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    /// Grants dropped because they contain a denied path.
    pub dropped: Vec<PathBuf>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const LINUX_READ_ROOTS: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/opt", "/proc", "/sys",
    "/dev", "/run", "/nix", "/snap",
];
/// The parts of `/etc` programs need: name service and TLS setup, users and
/// groups (`ls -l`, version control), the loader cache, time zone,
/// alternatives symlinks, system config for the VCS. The rest of `/etc`
/// (service configs, `ssh`, `sudoers.d`, `cron.*`) is not readable. Missing
/// entries are skipped by the helper.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const LINUX_ETC_READ: &[&str] = &[
    "/etc/passwd",
    "/etc/group",
    "/etc/nsswitch.conf",
    "/etc/hosts",
    "/etc/host.conf",
    "/etc/hostname",
    "/etc/resolv.conf",
    "/etc/gai.conf",
    "/etc/services",
    "/etc/protocols",
    "/etc/networks",
    "/etc/localtime",
    "/etc/timezone",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/alternatives",
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates",
    "/etc/ca-certificates.conf",
    "/etc/crypto-policies",
    "/etc/os-release",
    "/etc/lsb-release",
    "/etc/debian_version",
    "/etc/redhat-release",
    "/etc/issue",
    "/etc/mime.types",
    "/etc/magic",
    "/etc/terminfo",
    "/etc/inputrc",
    "/etc/profile",
    "/etc/profile.d",
    "/etc/bash.bashrc",
    "/etc/environment",
    "/etc/shells",
    "/etc/gitconfig",
    "/etc/fonts",
    "/etc/locale.alias",
    "/etc/locale.gen",
    "/etc/default/locale",
    "/etc/java",
    "/etc/python3",
    "/etc/perl",
    "/etc/xdg",
];
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const LINUX_WRITE_ROOTS: &[&str] = &[
    "/tmp",
    "/var/tmp",
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
    "/dev/shm",
];

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn linux_grants(policy: &HostPolicy, workspace: &Path) -> LinuxGrants {
    let deny: Vec<PathBuf> = policy.deny_paths.iter().map(|p| canonical(p)).collect();
    let home = home();
    let mut read: Vec<PathBuf> = LINUX_READ_ROOTS.iter().map(PathBuf::from).collect();
    read.extend(policy.read_paths.iter().map(|p| canonical(p)));
    read.extend(LINUX_ETC_READ.iter().map(PathBuf::from));
    read.extend(path_dirs(home.as_deref()));
    read.extend(toolchain::detect_here().into_iter().map(|g| g.path));
    let mut write: Vec<PathBuf> = vec![canonical(workspace)];
    write.extend(LINUX_WRITE_ROOTS.iter().map(PathBuf::from));
    write.extend(policy.write_paths.iter().map(|p| canonical(p)));

    let mut dropped = Vec::new();
    // Landlock can only grant, so a read grant that would include a sensitive
    // home location (a read-only folder that is `$HOME`) is dropped whole.
    let mut read_deny = deny.clone();
    if let Some(h) = &home {
        read_deny.extend(toolchain::sensitive_paths(h));
    }
    let mut keep = |list: Vec<PathBuf>, deny: &[PathBuf]| -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for p in list {
            if deny.iter().any(|d| d.starts_with(&p)) {
                dropped.push(p);
            } else if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    };
    let read = keep(read, &read_deny);
    let write = keep(write, &deny);
    LinuxGrants {
        read,
        write,
        dropped,
    }
}

/// Seatbelt string literal.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn sb_str(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The Seatbelt profile for one command on macOS. Later rules win.
///
/// Reads: everything outside the private areas (`/Users`, `/Volumes`,
/// `$HOME`) stays readable (system dirs, Homebrew, Xcode tools); inside them
/// a read is denied except for the workspace (with its state dir), temp and
/// writable dirs, explicit read-only folders (`read_paths`, which carries the
/// workspace's read roots) and the detected toolchain installs. Sensitive
/// home locations ([`toolchain::SENSITIVE_HOME`]) are denied again after
/// those grants, unless a folder the user explicitly granted lies inside one
/// (the default `~/.config/git`), and the deny-list comes last.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn seatbelt_profile(policy: &HostPolicy, workspace: &Path) -> String {
    seatbelt_profile_with(
        policy,
        workspace,
        &toolchain::detect_here(),
        home().as_deref(),
    )
}

/// [`seatbelt_profile`] with the toolchains and `$HOME` given (for tests).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn seatbelt_profile_with(
    policy: &HostPolicy,
    workspace: &Path,
    toolchains: &[ToolchainGrant],
    home: Option<&Path>,
) -> String {
    let workspace = canonical(workspace);
    let mut writable = vec![workspace.clone(), canonical(Path::new("/tmp"))];
    writable.push(canonical(Path::new("/var/tmp")));
    if let Some(t) = std::env::var_os("TMPDIR").filter(|t| !t.is_empty()) {
        writable.push(canonical(Path::new(&t)));
    }
    writable.extend(policy.write_paths.iter().map(|p| canonical(p)));
    // What the user or the workspace granted on purpose.
    let mut explicit: Vec<PathBuf> = policy.read_paths.iter().map(|p| canonical(p)).collect();
    explicit.extend(writable.iter().cloned());
    let mut readable = explicit.clone();
    readable.extend(toolchains.iter().map(|t| t.path.clone()));
    readable.extend(path_dirs(home));
    let deny: Vec<PathBuf> = policy.deny_paths.iter().map(|p| canonical(p)).collect();

    // Where reads are denied unless granted; nested roots collapse.
    let mut roots: Vec<PathBuf> = toolchain::MAC_PRIVATE_ROOTS
        .iter()
        .map(|r| canonical(Path::new(r)))
        .collect();
    if let Some(h) = home {
        roots.push(h.to_path_buf());
    }
    let roots: Vec<PathBuf> = roots
        .iter()
        .enumerate()
        .filter(|(i, r)| {
            !roots
                .iter()
                .enumerate()
                .any(|(j, o)| (o == *r && j < *i) || (o != *r && r.starts_with(o)))
        })
        .map(|(_, r)| r.clone())
        .collect();
    let private = |p: &Path| roots.iter().any(|r| p.starts_with(r));

    let subpaths = |ps: &[PathBuf]| -> String {
        ps.iter()
            .map(|p| format!(" (subpath {})", sb_str(p)))
            .collect()
    };
    let mut sb = String::from("(version 1)\n(allow default)\n");
    sb.push_str("(deny file-write*)\n");
    sb.push_str(&format!(
        "(allow file-write*{} (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (regex #\"^/dev/fd/\") (regex #\"^/dev/tty\"))\n",
        subpaths(&writable)
    ));
    sb.push_str(&format!("(deny file-read*{})\n", subpaths(&roots)));
    // Path lookups stat each ancestor of the paths we allow.
    let mut meta: Vec<PathBuf> = Vec::new();
    for p in &readable {
        for a in p.ancestors().skip(1) {
            if private(a) && !meta.iter().any(|m| m == a) {
                meta.push(a.to_path_buf());
            }
        }
    }
    let literals: String = meta
        .iter()
        .map(|p| format!(" (literal {})", sb_str(p)))
        .collect();
    if !literals.is_empty() {
        sb.push_str(&format!("(allow file-read-metadata{literals})\n"));
    }
    let inside: Vec<PathBuf> = readable.iter().filter(|p| private(p)).cloned().collect();
    if !inside.is_empty() {
        sb.push_str(&format!("(allow file-read*{})\n", subpaths(&inside)));
    }
    if let Some(h) = home {
        let sensitive = toolchain::sensitive_paths(h);
        sb.push_str(&format!("(deny file-read*{})\n", subpaths(&sensitive)));
        // A folder the user granted on purpose inside a sensitive location
        // stays readable.
        let again: Vec<PathBuf> = explicit
            .iter()
            .filter(|p| sensitive.iter().any(|s| p.starts_with(s)))
            .cloned()
            .collect();
        if !again.is_empty() {
            sb.push_str(&format!("(allow file-read*{})\n", subpaths(&again)));
        }
    }
    if !policy.network {
        sb.push_str("(deny network-outbound (remote ip))\n");
        sb.push_str("(deny network-inbound (local ip))\n");
        sb.push_str("(deny network-bind (local ip))\n");
    }
    // No signals to tern or anything else outside the sandbox.
    sb.push_str("(deny signal)\n(allow signal (target same-sandbox))\n");
    // No Keychain: items are read through securityd, not the file system.
    sb.push_str("(deny mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.securityd.xpc\") (global-name \"com.apple.security.agent\"))\n");
    if !deny.is_empty() {
        sb.push_str(&format!(
            "(deny file-read* file-write*{})\n",
            subpaths(&deny)
        ));
    }
    sb
}

/// Arguments for `__sandbox-exec` (before the `--` and the command).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn helper_args(policy: &HostPolicy, grants: &LinuxGrants) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    if policy.confine == Confine::Required {
        args.push("--require".into());
    }
    if !policy.network {
        args.push("--no-network".into());
    }
    for p in &grants.read {
        args.push("--read".into());
        args.push(p.into());
    }
    for p in &grants.write {
        args.push("--write".into());
        args.push(p.into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> HostPolicy {
        HostPolicy {
            read_paths: vec!["/x/r".into()],
            write_paths: vec!["/x/w".into()],
            deny_paths: vec!["/x/w/secret".into(), "/data/tern".into()],
            ..HostPolicy::confined()
        }
    }

    #[test]
    fn linux_grants_drop_anything_containing_a_denied_path() {
        let g = linux_grants(&policy(), Path::new("/ws"));
        assert!(g.write.contains(&PathBuf::from("/ws")));
        assert!(g.write.contains(&PathBuf::from("/tmp")));
        assert!(g.read.contains(&PathBuf::from("/x/r")));
        assert!(g.read.contains(&PathBuf::from("/usr")));
        // Contains /x/w/secret, so it can't be granted.
        assert!(!g.write.contains(&PathBuf::from("/x/w")));
        assert_eq!(g.dropped, [PathBuf::from("/x/w")]);
        // Home is never a read root.
        assert!(!g.read.iter().any(|p| p == Path::new("/home")));
    }

    #[test]
    fn helper_args_carry_the_policy() {
        let mut p = policy();
        p.network = false;
        p.confine = Confine::Required;
        let g = linux_grants(&p, Path::new("/ws"));
        let args: Vec<String> = helper_args(&p, &g)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[..2], ["--require", "--no-network"]);
        assert!(args.windows(2).any(|w| w == ["--write", "/ws"]));
        assert!(args.windows(2).any(|w| w == ["--read", "/x/r"]));
    }

    #[test]
    fn seatbelt_profile_shape() {
        let mut p = policy();
        p.network = false;
        let sb = seatbelt_profile(&p, Path::new("/ws/a\"b"));
        assert!(sb.contains(r#"(subpath "/ws/a\"b")"#), "{sb}");
        assert!(sb.contains("(deny file-write*)"));
        assert!(sb.contains("(deny network-outbound (remote ip))"));
        assert!(sb.contains("(allow signal (target same-sandbox))"));
        // The deny-list comes last so nothing re-allows it.
        let last = sb.lines().last().unwrap();
        assert!(last.starts_with("(deny file-read* file-write*"), "{last}");
        assert!(last.contains("/data/tern"));
        let with_net = seatbelt_profile(&HostPolicy::confined(), Path::new("/ws"));
        assert!(!with_net.contains("network-outbound"));
    }

    fn grant(name: &'static str, path: &str) -> ToolchainGrant {
        ToolchainGrant {
            name,
            path: path.into(),
        }
    }

    fn profile(p: &HostPolicy, ws: &str, tc: &[ToolchainGrant]) -> String {
        seatbelt_profile_with(p, Path::new(ws), tc, Some(Path::new("/Users/u")))
    }

    fn pos(sb: &str, needle: &str) -> usize {
        sb.find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?} in\n{sb}"))
    }

    #[test]
    fn seatbelt_denies_users_and_volumes_then_allows_the_grants() {
        let tc = [grant("rustup", "/Users/u/.rustup")];
        let sb = profile(&HostPolicy::confined(), "/Users/u/proj", &tc);
        let deny = pos(
            &sb,
            r#"(deny file-read* (subpath "/Users") (subpath "/Volumes"))"#,
        );
        let ws = pos(&sb, r#"(allow file-read* (subpath "/Users/u/proj")"#);
        assert!(deny < ws, "{sb}");
        assert!(sb.contains(r#"(subpath "/Users/u/.rustup")"#), "{sb}");
        // Ancestors are stat-able so path lookups work.
        assert!(sb.contains(r#"(literal "/Users/u")"#), "{sb}");
        assert!(sb.contains(r#"(literal "/Users")"#), "{sb}");
        // The system dirs outside /Users are never mentioned: still open.
        assert!(!sb.contains(r#"(subpath "/etc")"#));
    }

    #[test]
    fn seatbelt_sensitive_home_dirs_are_denied_after_every_allow() {
        // Even when the whole home folder is granted as a read-only folder.
        let p = HostPolicy {
            read_paths: vec!["/Users/u".into()],
            ..HostPolicy::confined()
        };
        let tc = [grant("cargo bin", "/Users/u/.cargo/bin")];
        let sb = profile(&p, "/Users/u/proj", &tc);
        let allow_all = sb.rfind("(allow file-read* (subpath").expect(&sb);
        let sens = pos(&sb, "(deny file-read* (subpath \"/Users/u/.ssh\")");
        assert!(
            allow_all < sens || sens > pos(&sb, "(allow file-read*"),
            "{sb}"
        );
        let deny_line = &sb[sens..sb[sens..].find('\n').map_or(sb.len(), |n| sens + n)];
        for s in [
            ".aws",
            ".gnupg",
            ".config",
            ".tern",
            "Library/Keychains",
            ".cargo/credentials",
        ] {
            let needle = format!(r#"(subpath "/Users/u/{s}")"#);
            assert!(deny_line.contains(&needle), "{s}\n{sb}");
        }
        // The credentials file is not inside the cargo bin grant.
        assert!(!sb.contains(r#"(subpath "/Users/u/.cargo")"#), "{sb}");
    }

    #[test]
    fn seatbelt_explicit_folders_inside_a_sensitive_dir_are_reallowed() {
        let p = HostPolicy {
            read_paths: vec!["/Users/u/.config/git".into()],
            ..HostPolicy::confined()
        };
        let sb = profile(&p, "/ws", &[]);
        let deny = pos(&sb, r#"(deny file-read* (subpath "/Users/u/.ssh")"#);
        let again = sb
            .rfind(r#"(allow file-read* (subpath "/Users/u/.config/git"))"#)
            .expect(&sb);
        assert!(deny < again, "{sb}");
        // A toolchain is not re-allowed over a sensitive path.
        let tc = [grant("x", "/Users/u/.config/mise")];
        let sb = profile(&HostPolicy::confined(), "/ws", &tc);
        let denied = pos(&sb, r#"(deny file-read* (subpath "/Users/u/.ssh")"#);
        assert!(!sb[denied..].contains("(allow file-read*"), "{sb}");
    }

    #[test]
    fn seatbelt_deny_paths_stay_last_and_other_volumes_work_as_workspaces() {
        let p = HostPolicy {
            deny_paths: vec!["/Users/u/.tern-data".into()],
            ..HostPolicy::confined()
        };
        let sb = profile(&p, "/Volumes/ext/ws", &[]);
        assert!(
            sb.contains(r#"(allow file-read* (subpath "/Volumes/ext/ws")"#),
            "{sb}"
        );
        let last = sb.lines().last().unwrap();
        assert!(
            last.starts_with("(deny file-read* file-write*") && last.contains(".tern-data"),
            "{last}"
        );
    }

    #[test]
    fn seatbelt_without_a_home_still_denies_users_and_volumes() {
        let sb = seatbelt_profile_with(&HostPolicy::confined(), Path::new("/ws"), &[], None);
        assert!(
            sb.contains(r#"(deny file-read* (subpath "/Users") (subpath "/Volumes"))"#),
            "{sb}"
        );
        assert!(!sb.contains(".ssh"));
    }

    #[test]
    fn a_home_outside_users_is_denied_too() {
        let sb = seatbelt_profile_with(
            &HostPolicy::confined(),
            Path::new("/ws"),
            &[],
            Some(Path::new("/var/root")),
        );
        assert!(sb.contains(r#"(subpath "/var/root")"#), "{sb}");
    }

    #[test]
    fn linux_etc_is_a_list_not_the_whole_directory() {
        let g = linux_grants(&HostPolicy::confined(), Path::new("/ws"));
        assert!(!g.read.contains(&PathBuf::from("/etc")));
        for p in ["/etc/passwd", "/etc/ssl", "/etc/resolv.conf", "/usr"] {
            assert!(g.read.contains(&PathBuf::from(p)), "{p}");
        }
        for p in ["/etc/shadow", "/etc/ssh", "/etc/sudoers"] {
            assert!(!g.read.contains(&PathBuf::from(p)), "{p}");
        }
    }

    #[test]
    fn linux_never_grants_a_read_root_that_contains_a_sensitive_home_dir() {
        let Some(home) = home() else { return };
        let p = HostPolicy {
            read_paths: vec![home.clone()],
            ..HostPolicy::confined()
        };
        let g = linux_grants(&p, Path::new("/ws"));
        assert!(!g.read.contains(&home));
        assert!(g.dropped.contains(&home));
    }
}
