//! `tern __sandbox-exec`: the Linux side of the host sandbox. The `tern`
//! binary re-executes itself with this argument; the helper applies Landlock
//! (and seccomp when networking is off) to itself and then `exec`s the
//! command, so no `pre_exec` hook (and no `unsafe`) is needed.
//!
//! Usage: `<tern> __sandbox-exec [--require] [--no-network] [--read P]...
//! [--write P]... -- <program> <args>...`, or `--probe` to print what the
//! kernel supports.

use std::ffi::OsString;
use std::process::ExitCode;

/// The argument that turns the `tern` binary into the helper.
pub const HELPER_ARG: &str = "__sandbox-exec";

/// Exit code when the helper itself fails (like `env`'s 125).
#[cfg(target_os = "linux")]
const HELPER_FAILED: u8 = 125;

/// Call first thing in `main`: if this process was started as the sandbox
/// helper, run it and return the exit code (on success it never returns,
/// since the command replaces it).
pub fn run_if_requested() -> Option<ExitCode> {
    let mut args = std::env::args_os();
    let _argv0 = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) {
        return None;
    }
    Some(run(args.collect()))
}

#[cfg(not(target_os = "linux"))]
fn run(_args: Vec<OsString>) -> ExitCode {
    eprintln!("tern sandbox: {HELPER_ARG} is only used on Linux");
    ExitCode::from(2)
}

#[cfg(target_os = "linux")]
fn run(args: Vec<OsString>) -> ExitCode {
    match linux::run(args) {
        Ok(never) => match never {},
        Err(e) => {
            eprintln!("tern sandbox: {e}");
            ExitCode::from(HELPER_FAILED)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::convert::Infallible;
    use std::ffi::OsString;
    use std::os::unix::process::CommandExt;
    use std::path::PathBuf;

    use landlock::{
        ABI, Access, AccessFs, AccessNet, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus, Scope, path_beneath_rules,
    };
    use seccompiler::{
        SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule, TargetArch,
    };

    /// The newest Landlock ABI these rules were written and tested against.
    const ABI_TARGET: ABI = ABI::V6;

    #[derive(Default)]
    struct Args {
        require: bool,
        network: bool,
        probe: bool,
        read: Vec<PathBuf>,
        write: Vec<PathBuf>,
        command: Vec<OsString>,
    }

    fn parse(args: Vec<OsString>) -> Result<Args, String> {
        let mut a = Args {
            network: true,
            ..Args::default()
        };
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.to_str() {
                Some("--require") => a.require = true,
                Some("--no-network") => a.network = false,
                Some("--probe") => a.probe = true,
                Some("--read") => a.read.push(it.next().ok_or("--read needs a path")?.into()),
                Some("--write") => a
                    .write
                    .push(it.next().ok_or("--write needs a path")?.into()),
                Some("--") => {
                    a.command = it.collect();
                    break;
                }
                _ => return Err(format!("unexpected argument {arg:?}")),
            }
        }
        if !a.probe && a.command.is_empty() {
            return Err("no command given".into());
        }
        Ok(a)
    }

    fn landlock(a: &Args) -> Result<RulesetStatus, String> {
        let err = |e: landlock::RulesetError| format!("landlock: {e}");
        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(ABI_TARGET))
            .map_err(err)?
            .scope(Scope::from_all(ABI_TARGET))
            .map_err(err)?;
        if !a.network {
            // Handled with no rules: every TCP bind/connect is denied.
            ruleset = ruleset
                .handle_access(AccessNet::from_all(ABI_TARGET))
                .map_err(err)?;
        }
        let device_read = AccessFs::from_read(ABI_TARGET) | AccessFs::IoctlDev;
        let status = ruleset
            .create()
            .map_err(err)?
            .add_rules(path_beneath_rules(&a.read, AccessFs::from_read(ABI_TARGET)))
            .map_err(err)?
            .add_rules(path_beneath_rules(["/dev"], device_read))
            .map_err(err)?
            .add_rules(path_beneath_rules(&a.write, AccessFs::from_all(ABI_TARGET)))
            .map_err(err)?
            .restrict_self()
            .map_err(err)?;
        Ok(status.ruleset)
    }

    /// The seccomp filter for the command. It always denies `ptrace` and
    /// `process_vm_*`, so the command can't read another same-user process's
    /// memory or environment. With `network` off it also denies new IP
    /// sockets (and io_uring, which could open them without the `socket`
    /// syscall); Unix sockets still work.
    ///
    /// The rules list the calls to deny, so they go with `Errno` as the match
    /// action and `Allow` for everything else.
    fn command_filter(network: bool) -> Result<seccompiler::BpfProgram, String> {
        let err = |e: seccompiler::BackendError| format!("seccomp: {e}");
        let arch =
            TargetArch::try_from(std::env::consts::ARCH).map_err(|e| format!("seccomp: {e}"))?;
        let domain = |af: libc::c_int| {
            SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, af as u64)
                .and_then(|c| SeccompRule::new(vec![c]))
        };
        // An empty rule list matches every call with that number.
        let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for nr in [
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
        ] {
            rules.insert(nr, Vec::new());
        }
        if !network {
            let socket_rules = vec![
                domain(libc::AF_INET).map_err(err)?,
                domain(libc::AF_INET6).map_err(err)?,
            ];
            rules.insert(libc::SYS_socket, socket_rules);
            rules.insert(libc::SYS_io_uring_setup, Vec::new());
        }
        #[cfg(target_arch = "x86_64")]
        {
            // The x32 ABI numbers the same calls with bit 30 set.
            const X32: i64 = 0x4000_0000;
            let mut nrs = vec![
                libc::SYS_ptrace,
                libc::SYS_process_vm_readv,
                libc::SYS_process_vm_writev,
            ];
            if !network {
                nrs.extend([libc::SYS_socket, libc::SYS_io_uring_setup]);
            }
            for nr in nrs {
                rules.insert(X32 | nr, Vec::new());
            }
        }
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Errno(libc::EPERM as u32),
            SeccompAction::Allow,
            arch,
        )
        .map_err(err)?;
        filter.try_into().map_err(err)
    }

    pub(super) fn run(args: Vec<OsString>) -> Result<Infallible, String> {
        let a = parse(args)?;
        let status = if std::env::var_os("DIAG_NO_LANDLOCK").is_some() { RulesetStatus::FullyEnforced } else { landlock(&a)? };
        if a.probe {
            let abi = (1..=6).rev().find(|&v| probe_abi(v)).unwrap_or_default();
            println!("landlock {abi} {status:?}");
            std::process::exit(0);
        }
        if a.require && status == RulesetStatus::NotEnforced {
            return Err(
                "Landlock is not available on this kernel and sandbox.confine = \"required\""
                    .into(),
            );
        }
        if std::env::var_os("DIAG_NO_SECCOMP").is_none() { match command_filter(a.network)
            .and_then(|p| seccompiler::apply_filter(&p).map_err(|e| format!("seccomp: {e}")))
        {
            Ok(()) => {}
            Err(e) if a.require => return Err(e),
            Err(e) => eprintln!("tern sandbox: seccomp filter not applied: {e}"),
        } }
        let (program, rest) = a.command.split_first().ok_or("no command given")?;
        let e = std::process::Command::new(program).args(rest).exec();
        Err(format!("exec {}: {e}", program.to_string_lossy()))
    }

    /// Whether the kernel enforces a ruleset that handles exactly the rights
    /// added in Landlock ABI `v` (checked in a throwaway ruleset, never applied).
    fn probe_abi(v: u8) -> bool {
        let abi = match v {
            1 => ABI::V1,
            2 => ABI::V2,
            3 => ABI::V3,
            4 => ABI::V4,
            5 => ABI::V5,
            _ => ABI::V6,
        };
        let r = Ruleset::default().set_compatibility(landlock::CompatLevel::HardRequirement);
        let r = match abi {
            ABI::V4 => r
                .handle_access(AccessNet::from_all(abi))
                .map(|r| r.create()),
            ABI::V6 => r
                .handle_access(AccessFs::from_all(ABI::V1))
                .and_then(|r| r.scope(Scope::from_all(abi)))
                .map(|r| r.create()),
            _ => r.handle_access(AccessFs::from_all(abi)).map(|r| r.create()),
        };
        matches!(r, Ok(Ok(_)))
    }
}
