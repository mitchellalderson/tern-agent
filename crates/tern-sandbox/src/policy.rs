//! What the host backend allows a command to touch, and the per-platform
//! plans built from it. See `docs/sandbox-design.md`.

use std::path::{Path, PathBuf};

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

/// `PATH` entries worth granting read on: absolute, existing, not `/` or
/// `$HOME` itself.
fn path_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .filter(|p| p.is_absolute() && p.exists())
        .map(|p| canonical(&p))
        .filter(|p| p.parent().is_some() && Some(p.as_path()) != home)
        .collect()
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
    "/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/proc",
    "/sys", "/dev", "/run", "/nix", "/snap",
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
    read.extend(path_dirs(home.as_deref()));
    let mut write: Vec<PathBuf> = vec![canonical(workspace)];
    write.extend(LINUX_WRITE_ROOTS.iter().map(PathBuf::from));
    write.extend(policy.write_paths.iter().map(|p| canonical(p)));

    let mut dropped = Vec::new();
    let mut keep = |list: Vec<PathBuf>| -> Vec<PathBuf> {
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
    let read = keep(read);
    let write = keep(write);
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn seatbelt_profile(policy: &HostPolicy, workspace: &Path) -> String {
    let workspace = canonical(workspace);
    let home = home();
    let mut writable = vec![workspace.clone(), canonical(Path::new("/tmp"))];
    writable.push(canonical(Path::new("/var/tmp")));
    if let Some(t) = std::env::var_os("TMPDIR").filter(|t| !t.is_empty()) {
        writable.push(canonical(Path::new(&t)));
    }
    writable.extend(policy.write_paths.iter().map(|p| canonical(p)));
    let mut readable: Vec<PathBuf> = policy.read_paths.iter().map(|p| canonical(p)).collect();
    readable.extend(path_dirs(home.as_deref()));
    readable.extend(writable.iter().cloned());
    let deny: Vec<PathBuf> = policy.deny_paths.iter().map(|p| canonical(p)).collect();

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
    if let Some(home) = &home {
        sb.push_str(&format!("(deny file-read* (subpath {}))\n", sb_str(home)));
        // Path lookups stat each ancestor of the paths we allow.
        let mut meta: Vec<PathBuf> = Vec::new();
        for p in &readable {
            for a in p.ancestors().skip(1) {
                if a.starts_with(home) && !meta.iter().any(|m| m == a) {
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
        let inside: Vec<PathBuf> = readable
            .iter()
            .filter(|p| p.starts_with(home))
            .cloned()
            .collect();
        if !inside.is_empty() {
            sb.push_str(&format!("(allow file-read*{})\n", subpaths(&inside)));
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
}
