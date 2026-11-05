//! The one shell command shape a scoped grant may cover: `python3 <file>.py
//! args`, where the file is a regular file inside the workspace. Everything
//! else about a command (pipes, redirects, inline code, wrappers) keeps
//! asking exactly as before. The check is strict on purpose: it tokenises on
//! plain spaces over a small safe alphabet and rejects whatever it doesn't
//! recognise, so what the user approves is what `sh` would run, token for
//! token. See `docs/sandbox-design.md` ("Script grants").

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A script bigger than this isn't covered (it is read into memory to hash).
const MAX_SCRIPT_BYTES: u64 = 8 * 1024 * 1024;

/// A command that is lexically `python3 <script>.py args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// `python` or `python3`, as written.
    pub interpreter: String,
    pub script: String,
    pub args: Vec<String>,
}

/// Why a command isn't the covered shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// Not a python command at all: nothing to say.
    NotPython,
    /// `-c`, `-` (stdin) or a heredoc: the code isn't in a file.
    Inline,
    /// A pipe, redirect, quote, glob, variable or other shell syntax.
    ShellSyntax,
    /// Anything else, in plain words.
    Other(String),
}

impl Reject {
    /// The card's sentence for a python command that doesn't qualify.
    pub fn notice(&self) -> Option<String> {
        match self {
            Reject::NotPython => None,
            Reject::Inline => Some(
                "Not auto-approved: inline script. Saved as a .py file in the workspace and run as `python3 file.py`, it could be approved once for the whole run."
                    .into(),
            ),
            Reject::ShellSyntax => Some(
                "Not auto-approved: the command uses a pipe, redirect, quote or other shell syntax. Output is already captured and capped, so run it as plain `python3 file.py args`."
                    .into(),
            ),
            Reject::Other(why) => Some(format!("Not auto-approved: {why}.")),
        }
    }
}

/// Characters besides ASCII letters and digits an argument may contain. No
/// quotes, `$`, `~`, globs, `#`, `!`, braces or backslashes: nothing the
/// shell would expand, split or interpret differently from the literal text.
const SAFE_PUNCT: &[char] = &['_', '.', '/', '-', '+', ',', ':', '=', '@', '%'];

/// The lexical half of the check; touches no files.
pub fn parse(command: &str) -> Result<Parsed, Reject> {
    let command = command.trim();
    let first = command.split(' ').next().unwrap_or_default();
    let base = first.rsplit('/').next().unwrap_or(first);
    if !base.starts_with("python") {
        return Err(Reject::NotPython);
    }
    if first != "python3" && first != "python" {
        return Err(Reject::Other(
            "run it as `python3 file.py`, without a path or a version suffix".into(),
        ));
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    if command.contains("<<") || matches!(words.get(1), Some(&"-c" | &"-")) {
        return Err(Reject::Inline);
    }
    let plain = |c: char| c == ' ' || c.is_ascii_alphanumeric() || SAFE_PUNCT.contains(&c);
    if !command.chars().all(plain) {
        return Err(Reject::ShellSyntax);
    }
    let Some(&script) = words.get(1) else {
        return Err(Reject::Other("no script file named".into()));
    };
    if script.starts_with('-') {
        return Err(Reject::Other(
            "interpreter options (like -m or -i) aren't allowed".into(),
        ));
    }
    if !script.ends_with(".py") || script.starts_with('/') || script.ends_with('/') {
        return Err(Reject::Other(
            "the script must be a .py file given relative to the workspace".into(),
        ));
    }
    if script.split('/').any(|c| c == ".." || c.is_empty()) {
        return Err(Reject::Other(
            "the script path may not contain `..` or empty parts".into(),
        ));
    }
    Ok(Parsed {
        interpreter: first.to_owned(),
        script: script.to_owned(),
        args: words[2..].iter().map(|w| (*w).to_owned()).collect(),
    })
}

/// A parsed command resolved against the file system, as it was when the
/// plan was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub workspace: PathBuf,
    /// The first `python3` on `PATH`, as found (what will be run).
    pub interpreter: PathBuf,
    pub interpreter_canonical: PathBuf,
    pub script_canonical: PathBuf,
    /// The script relative to the workspace, for display.
    pub script_rel: String,
    pub args: Vec<String>,
    /// sha256 of the script's bytes, hex.
    pub hash: String,
}

/// Resolve `parsed` for a call in `workspace`. `writable` are the places the
/// sandboxed command can write: the interpreter must not live in one (the
/// command could replace it). `path` is the `PATH` the command would see.
pub fn resolve(
    parsed: &Parsed,
    workspace: &Path,
    writable: &[PathBuf],
    path: Option<OsString>,
) -> Result<Resolved, String> {
    let workspace = std::fs::canonicalize(workspace)
        .map_err(|_| "the workspace folder can't be resolved".to_owned())?;
    let (interpreter, interpreter_canonical) = find_interpreter(&parsed.interpreter, path, writable)?;
    let (script_canonical, hash) = read_script(&workspace, &workspace.join(&parsed.script))?;
    let script_rel = script_canonical
        .strip_prefix(&workspace)
        .map_or_else(|_| parsed.script.clone(), |p| p.display().to_string());
    Ok(Resolved {
        workspace,
        interpreter,
        interpreter_canonical,
        script_canonical,
        script_rel,
        args: parsed.args.clone(),
        hash,
    })
}

/// Canonicalise `script`, require that it is inside `workspace` and a regular
/// file (checked on the open handle, not the path), and hash it.
pub fn read_script(workspace: &Path, script: &Path) -> Result<(PathBuf, String), String> {
    let canonical = std::fs::canonicalize(script)
        .map_err(|_| "the script file doesn't exist (yet) in the workspace".to_owned())?;
    if !canonical.starts_with(workspace) {
        return Err("the script resolves to a file outside the workspace".into());
    }
    // Not a fifo or device: opening one could block forever. Check the path,
    // and open without blocking in case it is swapped for one meanwhile.
    if !std::fs::metadata(&canonical).is_ok_and(|m| m.is_file()) {
        return Err("the script isn't a regular file".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let mut file = options
        .open(&canonical)
        .map_err(|e| format!("can't read the script: {e}"))?;
    let meta = file.metadata().map_err(|e| format!("can't read the script: {e}"))?;
    if !meta.is_file() {
        return Err("the script isn't a regular file".into());
    }
    if meta.len() > MAX_SCRIPT_BYTES {
        return Err("the script is larger than 8 MB".into());
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.by_ref()
        .take(MAX_SCRIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("can't read the script: {e}"))?;
    if bytes.len() as u64 > MAX_SCRIPT_BYTES {
        return Err("the script is larger than 8 MB".into());
    }
    Ok((canonical, hex(&Sha256::digest(&bytes))))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The first executable regular file `name` on `path` (absolute entries
/// only), and its canonical path.
///
/// Python decides `sys.prefix` and site-packages (`pyvenv.cfg`, `.pth`
/// files) from the path it was *started* from, so a launcher in a place the
/// command can write is code the command controls even when it points at
/// `/usr/bin/python3`. So: the as-found path, every directory on the way
/// along its symlinks and the canonical target must all be outside
/// `writable`; and the canonical file must be a native binary, not a
/// version-manager shim (asdf, pyenv, mise pick the real interpreter at run
/// time from `.tool-versions`-style files in the workspace). The caller runs
/// the canonical path, never the one that was found.
fn find_interpreter(
    name: &str,
    path: Option<OsString>,
    writable: &[PathBuf],
) -> Result<(PathBuf, PathBuf), String> {
    let path = path.ok_or("there is no PATH to find python on")?;
    for dir in std::env::split_paths(&path) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        let Ok(canonical) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if !is_executable_file(&canonical) {
            continue;
        }
        if let Some(at) = writable_hop(&candidate, writable)
            .or_else(|| writable.iter().find(|w| canonical.starts_with(w)).cloned())
        {
            return Err(format!(
                "{name} is found at {} (via {}), a place commands can write",
                candidate.display(),
                at.display()
            ));
        }
        if !is_native_binary(&canonical) {
            return Err(format!(
                "{name} is {}, a script (a version-manager shim?) rather than a program",
                canonical.display()
            ));
        }
        // mise (and others) are one native launcher behind a `python3`
        // symlink that picks the interpreter from files in the workspace.
        if !canonical
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("python"))
        {
            return Err(format!(
                "{name} is {}, a launcher rather than a python interpreter",
                canonical.display()
            ));
        }
        interpreter_is_sealed(&candidate, &canonical, writable)?;
        return Ok((candidate, canonical));
    }
    Err(format!("{name} wasn't found on PATH"))
}

/// Python reads `pyvenv.cfg` next to its binary or one level up, and the
/// standard library, site-packages and `.pth` files from its prefix: none of
/// that may be somewhere the command can write. Checked on the binary's
/// directories (as found and canonical), and on what the interpreter itself
/// reports ([`interpreter_paths`]).
fn interpreter_is_sealed(
    candidate: &Path,
    canonical: &Path,
    writable: &[PathBuf],
) -> Result<(), String> {
    let under = |p: &Path| {
        writable.iter().any(|w| p.starts_with(w))
            || std::fs::canonicalize(p).is_ok_and(|c| writable.iter().any(|w| c.starts_with(w)))
    };
    for bin in [candidate, canonical] {
        for dir in bin.ancestors().skip(1).take(2) {
            if under(dir) || under(&dir.join("pyvenv.cfg")) {
                return Err(format!(
                    "the interpreter at {} has {} where commands can write (a venv's pyvenv.cfg or its prefix could be replaced)",
                    bin.display(),
                    dir.display()
                ));
            }
        }
    }
    for p in interpreter_paths(canonical)? {
        if under(&p) {
            return Err(format!(
                "the interpreter at {} loads code from {}, a place commands can write",
                canonical.display(),
                p.display()
            ));
        }
    }
    Ok(())
}

type PathsCache = std::collections::HashMap<
    (PathBuf, Option<std::time::SystemTime>),
    Result<Vec<PathBuf>, String>,
>;

/// What the interpreter says it loads code from: prefixes, `sys.path`
/// (standard library) and site-packages. Asked once per interpreter file
/// (cached by path and mtime) by running it outside the sandbox with a fixed
/// program, `-I -S` (so no `site`, no `.pth`, no user site: nothing the
/// workspace can influence runs), from `/` with an empty environment.
pub fn interpreter_paths(canonical: &Path) -> Result<Vec<PathBuf>, String> {
    static CACHE: std::sync::Mutex<Option<PathsCache>> = std::sync::Mutex::new(None);
    let mtime = std::fs::metadata(canonical).and_then(|m| m.modified()).ok();
    let key = (canonical.to_path_buf(), mtime);
    if let Some(hit) = CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.get(&key))
    {
        return hit.clone();
    }
    let result = probe_interpreter(canonical);
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(key, result.clone());
    result
}

fn probe_interpreter(canonical: &Path) -> Result<Vec<PathBuf>, String> {
    const CODE: &str = "import sys, site\nfor p in (sys.prefix, sys.base_prefix, sys.exec_prefix, *sys.path, *site.getsitepackages()):\n    print(p)\n";
    let mut child = std::process::Command::new(canonical)
        .args(["-I", "-S", "-c", CODE])
        .current_dir("/")
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't inspect {}: {e}", canonical.display()))?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() > std::time::Duration::from_secs(10) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("inspecting {} timed out", canonical.display()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(e) => return Err(format!("inspecting {}: {e}", canonical.display())),
        }
    };
    let mut out = String::new();
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if !status.success() {
        return Err(format!("couldn't inspect {} (it didn't run as python)", canonical.display()));
    }
    Ok(out
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect())
}

/// The first place on the way from `candidate` to its target (the path
/// itself, each directory it sits in, each symlink hop) that is under a
/// writable root.
fn writable_hop(candidate: &Path, writable: &[PathBuf]) -> Option<PathBuf> {
    let under = |p: &Path| writable.iter().any(|w| p.starts_with(w));
    let mut p = candidate.to_path_buf();
    for _ in 0..40 {
        if under(&p) {
            return Some(p);
        }
        let parent = p.parent().map(Path::to_path_buf);
        if let Some(parent) = &parent
            && (under(parent) || std::fs::canonicalize(parent).is_ok_and(|c| under(&c)))
        {
            return Some(parent.clone());
        }
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                let target = std::fs::read_link(&p).ok()?;
                p = match (&parent, target.is_absolute()) {
                    (_, true) => target,
                    (Some(parent), false) => parent.join(target),
                    (None, false) => target,
                };
            }
            _ => break,
        }
    }
    None
}

/// Mach-O (thin or universal) or ELF, by magic number.
fn is_native_binary(p: &Path) -> bool {
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut magic));
    read.is_ok()
        && matches!(
            magic,
            [0x7f, b'E', b'L', b'F']
                | [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
}

fn is_executable_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(p).is_ok_and(|m| m.is_file())
    }
}

/// The command `sh` runs for a resolved call: the canonical interpreter (never
/// the path it was found at), isolated mode (`-I`: no script directory, no
/// user site, no `PYTHON*` variables, so a module planted next to the script
/// can't shadow the standard library), the exact (canonical) script that was
/// hashed, and the plain arguments.
pub fn command_line(r: &Resolved) -> String {
    let mut cmd = format!(
        "{} -I {}",
        sh_quote(&r.interpreter_canonical.to_string_lossy()),
        sh_quote(&r.script_canonical.to_string_lossy())
    );
    for a in &r.args {
        cmd.push(' ');
        cmd.push_str(a);
    }
    cmd
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Whether the call's other arguments are plain: no `network: true` (or
/// anything that is not an explicit `false`).
pub fn offline(args: &serde_json::Value) -> bool {
    match args.get("network") {
        None | Some(serde_json::Value::Null) => true,
        Some(v) => v == &serde_json::Value::Bool(false),
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn ok(cmd: &str) -> Parsed {
        parse(cmd).unwrap_or_else(|e| panic!("{cmd:?} rejected: {e:?}"))
    }

    #[test]
    fn accepts_plain_script_runs() {
        let p = ok("python3 a.py x");
        assert_eq!((p.script.as_str(), p.args.as_slice()), ("a.py", &["x".to_owned()][..]));
        assert_eq!(ok("  python tools/q.py  --year=2024   data/in.csv ").args.len(), 2);
        assert_eq!(ok("python3 sub/dir/s.py").args.len(), 0);
        assert_eq!(ok("python3 s.py -n 5 --out=out.txt").args, ["-n", "5", "--out=out.txt"]);
    }

    #[test]
    fn rejects_everything_else() {
        for (cmd, want) in [
            ("python3 -c 'print(1)'", Reject::Inline),
            ("python3 -c print(1)", Reject::Inline),
            ("python3 -", Reject::Inline),
            ("python3 - <<'EOF'\nprint(1)\nEOF", Reject::Inline),
            ("python3 <<EOF\nx\nEOF", Reject::Inline),
            ("python3 a.py; id", Reject::ShellSyntax),
            ("python3 a.py | head", Reject::ShellSyntax),
            ("python3 a.py 2>&1", Reject::ShellSyntax),
            ("python3 a.py > out", Reject::ShellSyntax),
            ("python3 a.py < in", Reject::ShellSyntax),
            ("python3 a.py `id`", Reject::ShellSyntax),
            ("python3 a.py $(id)", Reject::ShellSyntax),
            ("python3 a.py $HOME", Reject::ShellSyntax),
            ("python3 a.py &", Reject::ShellSyntax),
            ("python3 a.py && id", Reject::ShellSyntax),
            ("python3 a.py (x)", Reject::ShellSyntax),
            ("python3 *.py", Reject::ShellSyntax),
            ("python3 a.py *", Reject::ShellSyntax),
            ("python3 a?.py", Reject::ShellSyntax),
            ("python3 a.py 'x y'", Reject::ShellSyntax),
            ("python3 a.py \"x\"", Reject::ShellSyntax),
            ("python3 a.py \\x", Reject::ShellSyntax),
            ("python3 ~/a.py", Reject::ShellSyntax),
            ("python3 a.py {1,2}", Reject::ShellSyntax),
            ("python3 a.py #x", Reject::ShellSyntax),
            ("python3 a.py\tb", Reject::ShellSyntax),
            ("python3 a.py\nid", Reject::ShellSyntax),
            ("FOO=1 python3 a.py", Reject::NotPython),
            ("env python3 a.py", Reject::NotPython),
            ("sudo python3 a.py", Reject::NotPython),
            ("bash -c 'python3 a.py'", Reject::NotPython),
            ("time python3 a.py", Reject::NotPython),
            ("ls", Reject::NotPython),
            ("", Reject::NotPython),
        ] {
            assert_eq!(parse(cmd), Err(want), "{cmd:?}");
        }
        for cmd in [
            "python3 -m http.server",
            "python3 -i a.py",
            "python3 -x a.py",
            "python3 -W error a.py",
            "python3",
            "python3 /etc/a.py",
            "python3 ../a.py",
            "python3 a/../b.py",
            "python3 a.txt",
            "python3 a.py/",
            "/usr/bin/python3 a.py",
            "python3.11 a.py",
            "python3 a//b.py",
        ] {
            assert!(matches!(parse(cmd), Err(Reject::Other(_))), "{cmd:?}: {:?}", parse(cmd));
        }
        assert!(Reject::Inline.notice().unwrap().contains("inline script"));
        assert!(Reject::NotPython.notice().is_none());
    }

    fn ws() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let w = std::fs::canonicalize(d.path()).unwrap();
        (d, w)
    }

    fn path() -> Option<OsString> {
        std::env::var_os("PATH")
    }

    fn py(w: &Path, cmd: &str) -> Result<Resolved, String> {
        resolve(&ok(cmd), w, &[w.to_path_buf()], path())
    }

    #[test]
    fn resolves_a_regular_file_inside_the_workspace() {
        let (_d, w) = ws();
        std::fs::create_dir(w.join("sub")).unwrap();
        std::fs::write(w.join("sub/a.py"), "print(1)").unwrap();
        let r = py(&w, "python3 sub/a.py x y").unwrap();
        assert_eq!(r.script_rel, "sub/a.py");
        assert_eq!(r.script_canonical, w.join("sub/a.py"));
        assert_eq!(r.hash.len(), 64);
        assert!(r.interpreter.is_absolute() && r.interpreter_canonical.is_absolute());
        let cmd = command_line(&r);
        assert!(cmd.ends_with(" x y") && cmd.contains(&r.script_canonical.to_string_lossy().to_string()));
    }

    #[test]
    fn refuses_missing_outside_symlinked_out_and_non_regular_scripts() {
        let (_d, w) = ws();
        let (_o, outside) = ws();
        std::fs::write(outside.join("evil.py"), "print(1)").unwrap();
        // Missing.
        assert!(py(&w, "python3 nope.py").is_err());
        // A symlink to a file outside the workspace.
        std::os::unix::fs::symlink(outside.join("evil.py"), w.join("link.py")).unwrap();
        let e = py(&w, "python3 link.py").unwrap_err();
        assert!(e.contains("outside the workspace"), "{e}");
        // A symlinked directory pointing out.
        std::os::unix::fs::symlink(&outside, w.join("dir")).unwrap();
        assert!(py(&w, "python3 dir/evil.py").is_err());
        // A symlink to a file inside is fine, and resolves to the target.
        std::fs::write(w.join("real.py"), "x").unwrap();
        std::os::unix::fs::symlink(w.join("real.py"), w.join("ok.py")).unwrap();
        assert_eq!(py(&w, "python3 ok.py").unwrap().script_canonical, w.join("real.py"));
        // A directory named like a script, and a fifo.
        std::fs::create_dir(w.join("d.py")).unwrap();
        assert!(py(&w, "python3 d.py").unwrap_err().contains("regular file"));
        let fifo = w.join("f.py");
        let ok = std::process::Command::new("mkfifo").arg(&fifo).status().is_ok_and(|s| s.success());
        if ok {
            assert!(py(&w, "python3 f.py").is_err());
        }
    }

    #[test]
    fn refuses_an_interpreter_the_command_could_replace() {
        let (_d, w) = ws();
        std::fs::write(w.join("a.py"), "x").unwrap();
        let bin = w.join("bin");
        std::fs::create_dir(&bin).unwrap();
        let fake = bin.join("python3");
        std::fs::write(&fake, "#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = resolve(
            &ok("python3 a.py"),
            &w,
            std::slice::from_ref(&w),
            Some(bin.clone().into()),
        )
        .unwrap_err();
        assert!(e.contains("a place commands can write"), "{e}");
        // Relative PATH entries are ignored.
        let e = resolve(&ok("python3 a.py"), &w, &[], Some(OsString::from("bin"))).unwrap_err();
        assert!(e.contains("wasn't found"), "{e}");
    }

    fn real_python() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join("python3"))
            .find_map(|c| std::fs::canonicalize(c).ok().filter(|c| is_native_binary(c)))
    }

    #[test]
    fn a_venv_style_launcher_in_a_writable_place_is_refused() {
        let Some(real) = real_python() else {
            return;
        };
        let (_d, w) = ws();
        std::fs::write(w.join("a.py"), "x").unwrap();
        // <ws>/.venv/bin/python3 -> the real interpreter, with a pyvenv.cfg:
        // python finds site-packages (and any .pth file) from where it was
        // started, so this would run whatever the command drops there.
        let bin = w.join(".venv/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&real, bin.join("python3")).unwrap();
        std::fs::write(w.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").unwrap();
        let e = resolve(&ok("python3 a.py"), &w, std::slice::from_ref(&w), Some(bin.into()))
            .unwrap_err();
        assert!(e.contains("a place commands can write"), "{e}");
        // A launcher dir outside the workspace but writable (write_paths, tmp).
        let (_o, other) = ws();
        let bin2 = other.join("bin");
        std::fs::create_dir_all(&bin2).unwrap();
        std::os::unix::fs::symlink(&real, bin2.join("python3")).unwrap();
        let roots = [w.clone(), other.clone()];
        let e = resolve(&ok("python3 a.py"), &w, &roots, Some(bin2.clone().into())).unwrap_err();
        assert!(e.contains("a place commands can write"), "{e}");
        // The same launcher in a place that is not writable is fine, and
        // what runs is the canonical interpreter, never the launcher path.
        let r = resolve(&ok("python3 a.py"), &w, std::slice::from_ref(&w), Some(bin2.into())).unwrap();
        assert_eq!(r.interpreter_canonical, real);
        let cmd = command_line(&r);
        assert!(cmd.starts_with(&format!("'{}' -I '", real.display())), "{cmd}");
        assert!(!cmd.contains("bin/python3 "), "{cmd}");
    }

    #[test]
    fn a_symlink_hop_through_a_writable_directory_is_refused() {
        let Some(real) = real_python() else {
            return;
        };
        let (_d, w) = ws();
        std::fs::write(w.join("a.py"), "x").unwrap();
        // /safe/bin/python3 -> <ws>/hop -> real: the middle hop is writable.
        let (_s, safe) = ws();
        std::fs::create_dir(safe.join("bin")).unwrap();
        std::os::unix::fs::symlink(&real, w.join("hop")).unwrap();
        std::os::unix::fs::symlink(w.join("hop"), safe.join("bin/python3")).unwrap();
        let e = resolve(
            &ok("python3 a.py"),
            &w,
            std::slice::from_ref(&w),
            Some(safe.join("bin").into()),
        )
        .unwrap_err();
        assert!(e.contains("a place commands can write"), "{e}");
    }

    #[test]
    fn a_version_manager_shim_is_not_a_program() {
        let (_d, w) = ws();
        std::fs::write(w.join("a.py"), "x").unwrap();
        let (_s, safe) = ws();
        let shim = safe.join("python3");
        std::fs::write(&shim, "#!/bin/sh\nexec \"$(asdf which python3)\" \"$@\"\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = resolve(&ok("python3 a.py"), &w, std::slice::from_ref(&w), Some(safe.into()))
            .unwrap_err();
        assert!(e.contains("version-manager shim"), "{e}");
    }

    #[test]
    fn a_writable_site_packages_stdlib_or_prefix_is_refused() {
        let Some(real) = real_python() else {
            return;
        };
        let (_d, w) = ws();
        std::fs::write(w.join("a.py"), "x").unwrap();
        let (_s, safe) = ws();
        std::os::unix::fs::symlink(&real, safe.join("python3")).unwrap();
        let path = || Some(OsString::from(safe.clone()));
        // Sealed as it is.
        let ok = resolve(&ok("python3 a.py"), &w, std::slice::from_ref(&w), path());
        assert!(ok.is_ok(), "{ok:?}");
        // What the interpreter reports it loads from, each made writable in turn
        // (as `write_paths = [site-packages]` would).
        let reported = interpreter_paths(&real).unwrap();
        assert!(reported.len() >= 3, "{reported:?}");
        for dir in reported.iter().filter(|p| p.is_dir()) {
            let roots = [w.clone(), dir.clone()];
            let e = resolve(&ok_cmd(), &w, &roots, path()).unwrap_err();
            assert!(e.contains("a place commands can write"), "{dir:?}: {e}");
        }
    }

    fn ok_cmd() -> Parsed {
        ok("python3 a.py")
    }

    #[test]
    fn a_pyvenv_cfg_a_command_could_write_beside_or_above_the_binary_is_refused() {
        let (_d, w) = ws();
        std::fs::write(w.join("a.py"), "x").unwrap();
        // <other>/bin/python3 is a program; only the cfg path is writable
        // (a write_paths entry naming it): python would read it.
        let (_o, other) = ws();
        std::fs::create_dir(other.join("bin")).unwrap();
        std::fs::copy("/bin/sh", other.join("bin/python3")).unwrap();
        let cfg = other.join("pyvenv.cfg");
        let roots = [w.clone(), cfg];
        let e = resolve(&ok_cmd(), &w, &roots, Some(other.join("bin").into())).unwrap_err();
        assert!(e.contains("pyvenv.cfg"), "{e}");
        // A writable prefix (the grandparent) holding it, reached by a link.
        let (_l, links) = ws();
        std::os::unix::fs::symlink(other.join("bin/python3"), links.join("python3")).unwrap();
        let roots = [w.clone(), other.clone()];
        let e = resolve(&ok_cmd(), &w, &roots, Some(links.into())).unwrap_err();
        assert!(e.contains("a place commands can write") || e.contains("pyvenv.cfg"), "{e}");
    }

    #[test]
    fn a_launcher_behind_a_python_name_is_refused() {
        let (_d, w) = ws();
        std::fs::write(w.join("a.py"), "x").unwrap();
        let (_s, safe) = ws();
        // mise: one native binary, a `python3` symlink to it.
        std::fs::copy("/bin/sh", safe.join("mise")).unwrap();
        std::os::unix::fs::symlink(safe.join("mise"), safe.join("python3")).unwrap();
        let e = resolve(&ok_cmd(), &w, std::slice::from_ref(&w), Some(safe.into())).unwrap_err();
        assert!(e.contains("a launcher rather than a python interpreter"), "{e}");
    }

    #[test]
    fn network_must_be_explicitly_off() {
        use serde_json::json;
        assert!(offline(&json!({"command": "x"})));
        assert!(offline(&json!({"network": false})));
        assert!(!offline(&json!({"network": true})));
        assert!(!offline(&json!({"network": "yes"})));
        assert!(!offline(&json!({"network": 1})));
    }
}
