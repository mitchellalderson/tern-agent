//! The code-auto-mode shell classifier (docs/alpha-coding-agent.md §3.2).
//!
//! In a verified sandbox the OS is the boundary; this is the speed bump on
//! top of it. It answers one question per command: may it run without
//! asking in code auto mode, or does it belong to a short list of things the
//! user should see first (remote effects, installs, deletions that reach
//! outside the folder or take much of it, credentials, anything it can't
//! parse with confidence)?
//!
//! It is a filter, not a wall. `sh -c`, a script the agent just wrote, or a
//! Makefile can all get around it, and the copy never calls it protection.
//! A miss is expected and cheap; a false "ask" only costs one click. Anything
//! unparseable asks.

/// What the classifier decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing on the ask list: may run unasked in code auto mode.
    Auto,
    /// Asks, with the reason for the card ("Asked because: ...").
    Ask(String),
}

impl Verdict {
    pub fn is_auto(&self) -> bool {
        matches!(self, Verdict::Auto)
    }
}

/// Programs that always ask: privilege, remote shells and transfers, the
/// network, containers, desktop and OS control, and system package managers.
const ASK_PROGRAMS: &[&str] = &[
    "sudo",
    "su",
    "doas",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "curl",
    "wget",
    "nc",
    "ncat",
    "socat",
    "telnet",
    "ftp",
    "docker",
    "podman",
    "kubectl",
    "osascript",
    "open",
    "launchctl",
    "security",
    "defaults",
    "crontab",
    "at",
    "systemctl",
    "mount",
    "brew",
    "apt",
    "apt-get",
    "yum",
    "dnf",
    "pacman",
    "snap",
    "gem",
    "composer",
    "pipx",
    "conda",
    "mamba",
    "xargs",
    "nohup",
    "setsid",
    "disown",
    "eval",
    "exec",
    "source",
    ".",
    "npx",
    "bunx",
    "pnpx",
    "gh",
    "aws",
    "gcloud",
    "az",
    "terraform",
    "pbcopy",
    "pbpaste",
    "xdg-open",
    "shutdown",
    "reboot",
    "kill",
    "killall",
    "pkill",
    "diskutil",
    "hdiutil",
];

/// Programs that run another command given on their command line.
const WRAPPERS: &[&str] = &[
    "env",
    "time",
    "nice",
    "command",
    "timeout",
    "stdbuf",
    "caffeinate",
    "ionice",
    "builtin",
];

/// Programs that delete, move or rewrite what their operands name: an operand
/// that is absolute, `~`, a variable or climbs out with `..` asks.
const PATH_PROGRAMS: &[&str] = &[
    "rm", "rmdir", "mv", "cp", "ln", "chmod", "chown", "chgrp", "truncate", "shred", "dd",
    "install", "tee", "touch", "mkdir", "unlink",
];

/// Build-output directories a recursive `rm` may remove without asking.
const BUILD_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "build",
    "dist",
    "out",
    "coverage",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".venv",
    "venv",
    ".next",
    ".turbo",
    ".gradle",
    ".dart_tool",
    ".cache",
];

/// Variables a command may not set for itself: they redirect what the
/// toolchain, the loader or git run.
const BAD_VARS: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "SHELL",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_ASKPASS",
    "GIT_EXEC_PATH",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_COUNT",
    "NODE_OPTIONS",
    "PYTHONSTARTUP",
    "RUSTC_WRAPPER",
    "BASH_ENV",
    "ENV",
];

/// Prefixes of well-known credentials.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "sk-",
    "xoxb-",
    "xoxp-",
    "akia",
    "npm_",
    "glpat-",
    "pypi-",
    "ya29.",
];

/// Flags that carry a credential.
const SECRET_FLAGS: &[&str] = &[
    "--token",
    "--password",
    "--passwd",
    "--api-key",
    "--apikey",
    "--secret",
    "--auth-token",
    "--access-token",
    "--client-secret",
];

const MAX_DEPTH: usize = 3;

/// Classify a whole command line (`sh -c` text).
pub fn classify(command: &str) -> Verdict {
    classify_at(command, 0)
}

fn ask(why: impl Into<String>) -> Verdict {
    Verdict::Ask(why.into())
}

fn classify_at(command: &str, depth: usize) -> Verdict {
    if depth > MAX_DEPTH {
        return ask("it nests shells too deeply to read");
    }
    let parts = match split(command) {
        Ok(p) => p,
        Err(why) => return ask(why),
    };
    if parts.is_empty() {
        return ask("it is empty");
    }
    for words in &parts {
        if let Verdict::Ask(why) = classify_simple(words, depth) {
            return Verdict::Ask(why);
        }
    }
    Verdict::Auto
}

/// Split into simple commands (on `; && || | &` and newlines outside quotes)
/// of words with quotes removed. Command substitution, backticks and
/// process substitution anywhere (except inside single quotes) fail it, as
/// do unbalanced quotes and heredocs.
fn split(command: &str) -> Result<Vec<Vec<String>>, &'static str> {
    let mut out: Vec<Vec<String>> = vec![];
    let mut words: Vec<String> = vec![];
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    let flush = |cur: &mut String, in_word: &mut bool, words: &mut Vec<String>| {
        if *in_word {
            words.push(std::mem::take(cur));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return Err("its quotes don't balance"),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(ch) => cur.push(ch),
                            None => return Err("its quotes don't balance"),
                        },
                        Some('`') => return Err("it uses command substitution"),
                        Some('$') if chars.peek() == Some(&'(') => {
                            return Err("it uses command substitution");
                        }
                        Some(ch) => cur.push(ch),
                        None => return Err("its quotes don't balance"),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some('\n') => {}
                    Some(ch) => cur.push(ch),
                    None => return Err("it ends in a backslash"),
                }
            }
            '`' => return Err("it uses command substitution"),
            '$' if chars.peek() == Some(&'(') => return Err("it uses command substitution"),
            '<' if matches!(chars.peek(), Some('(' | '<')) => {
                return Err("it uses process substitution or a heredoc");
            }
            '>' if chars.peek() == Some(&'(') => return Err("it uses process substitution"),
            ';' | '\n' | '|' | '&' => {
                // `2>&1` and `>&2`: part of a redirect word.
                if c == '&' && in_word && (cur.ends_with('>') || cur.ends_with('<')) {
                    cur.push('&');
                    continue;
                }
                // `&> file` is a redirect, keep it as a word.
                if c == '&' && chars.peek() == Some(&'>') {
                    cur.push_str("&>");
                    chars.next();
                    in_word = true;
                    continue;
                }
                // `&&`, `||` fold into one separator; a lone `&` backgrounds.
                if matches!(c, '|' | '&') && chars.peek() == Some(&c) {
                    chars.next();
                } else if c == '&' {
                    return Err("it starts a background process");
                }
                flush(&mut cur, &mut in_word, &mut words);
                if !words.is_empty() {
                    out.push(std::mem::take(&mut words));
                }
            }
            c if c.is_whitespace() => flush(&mut cur, &mut in_word, &mut words),
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    flush(&mut cur, &mut in_word, &mut words);
    if !words.is_empty() {
        out.push(words);
    }
    Ok(out)
}

fn is_assignment(w: &str) -> Option<&str> {
    let (name, _) = w.split_once('=')?;
    (!name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit()))
    .then_some(name)
}

/// A path operand that reaches outside the folder (or can't be resolved).
fn escapes(arg: &str) -> bool {
    if arg == "/dev/null" {
        return false;
    }
    arg.starts_with('/')
        || arg.starts_with('~')
        || arg.contains('$')
        || arg.split('/').any(|seg| seg == "..")
}

fn looks_secret(w: &str) -> bool {
    let lower = w.to_ascii_lowercase();
    if TOKEN_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    if let Some((flag, _)) = lower.split_once('=')
        && SECRET_FLAGS.contains(&flag)
    {
        return true;
    }
    SECRET_FLAGS.contains(&lower.as_str())
        || lower.contains("token=")
        || lower.contains("password=")
        || lower.contains("secret=")
        || lower.contains("api_key=")
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn classify_simple(words: &[String], depth: usize) -> Verdict {
    // Leading `VAR=value` assignments.
    let mut i = 0;
    while let Some(name) = words.get(i).and_then(|w| is_assignment(w)) {
        if BAD_VARS.contains(&name) || name.starts_with("DYLD_") {
            return ask(format!("it sets {name}, which changes what runs"));
        }
        i += 1;
    }
    let words = &words[i..];
    let Some(first) = words.first() else {
        return Verdict::Auto; // only assignments
    };
    if first.starts_with('/') && !first.starts_with("/usr/bin/") && !first.starts_with("/bin/") {
        return ask("it runs a program by absolute path");
    }
    if first.contains("..") {
        return ask("it runs a program from outside this folder");
    }
    let prog = basename(first);

    if words.iter().skip(1).any(|w| looks_secret(w)) || looks_secret(first) {
        return ask("it carries something that looks like a credential");
    }
    // Redirects that write outside the folder.
    for (n, w) in words.iter().enumerate() {
        let target = w
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .strip_prefix(">>")
            .or_else(|| {
                w.trim_start_matches(|c: char| c.is_ascii_digit())
                    .strip_prefix('>')
            })
            .or_else(|| w.strip_prefix("&>"));
        if let Some(t) = target {
            let t = if t.is_empty() {
                words.get(n + 1).map_or("", String::as_str)
            } else {
                t
            };
            if t.starts_with('&') {
                continue; // `2>&1`
            }
            if escapes(t) {
                return ask("it redirects output outside this folder");
            }
        }
    }
    if ASK_PROGRAMS.contains(&prog) {
        return ask(format!("`{prog}` leaves the folder or changes the machine"));
    }
    // Shells with -c: judge the text they run.
    if matches!(prog, "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish") {
        if let Some(pos) = words
            .iter()
            .position(|w| w == "-c" || w == "-lc" || w == "-ec")
            && let Some(text) = words.get(pos + 1)
        {
            return classify_at(text, depth + 1);
        }
        return ask("it starts a shell");
    }
    if WRAPPERS.contains(&prog) {
        // Skip flags, numbers (`timeout 30`) and assignments to the real command.
        let rest: Vec<String> = words[1..]
            .iter()
            .skip_while(|w| {
                w.starts_with('-') || w.chars().all(|c| c.is_ascii_digit() || c == '.') || {
                    is_assignment(w).is_some()
                }
            })
            .cloned()
            .collect();
        if rest.is_empty() {
            return Verdict::Auto;
        }
        for w in &words[1..words.len() - rest.len()] {
            if let Some(name) = is_assignment(w)
                && (BAD_VARS.contains(&name) || name.starts_with("DYLD_"))
            {
                return ask(format!("it sets {name}, which changes what runs"));
            }
        }
        return classify_simple(&rest, depth + 1);
    }
    let args = &words[1..];
    let operands: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let has_flag = |f: &[&str]| args.iter().any(|a| f.contains(&a.as_str()));

    if prog == "cd" || prog == "pushd" {
        if operands.iter().any(|a| escapes(a)) {
            return ask("it changes directory outside this folder");
        }
        return Verdict::Auto;
    }
    if prog == "rm" {
        let recursive = args.iter().any(|a| {
            a == "--recursive"
                || (a.starts_with('-')
                    && !a.starts_with("--")
                    && a.chars().any(|c| c == 'r' || c == 'R'))
        });
        if operands.iter().any(|a| escapes(a)) {
            return ask("it deletes outside this folder");
        }
        let broad = |a: &str| {
            let t = a.trim_end_matches('/');
            matches!(t, "" | "." | "*" | ".*" | "**" | "./*" | "./.*")
                || t.ends_with("/*")
                || t == ".git"
                || t.starts_with(".git/")
        };
        if operands.iter().any(|a| broad(a)) {
            return ask("it deletes much of this folder");
        }
        if recursive {
            let build_only = !operands.is_empty()
                && operands.iter().all(|a| {
                    let seg = a.trim_end_matches('/').trim_start_matches("./");
                    !seg.contains('/') && BUILD_DIRS.contains(&seg)
                });
            if !build_only {
                return ask("it deletes a directory tree (only build output is automatic)");
            }
        }
        return Verdict::Auto;
    }
    if PATH_PROGRAMS.contains(&prog) && operands.iter().any(|a| escapes(a)) {
        return ask(format!("`{prog}` reaches outside this folder"));
    }
    // Protected files inside the folder: hooks, git config, direnv.
    let protected = |a: &str| {
        let t = a.trim_start_matches("./");
        t.starts_with(".git/hooks")
            || t.starts_with(".git/config")
            || t.starts_with(".git/info")
            || t == ".envrc"
            || t.ends_with("/.envrc")
            || t.contains(".git/hooks")
    };
    if words.iter().skip(1).any(|a| protected(a)) {
        return ask("it touches git hooks, git config or .envrc, which later run as you");
    }
    match prog {
        "find" => {
            if has_flag(&["-delete", "-exec", "-execdir", "-ok", "-okdir"]) {
                return ask("`find` would delete or run commands on what it finds");
            }
        }
        "git" => return classify_git(args),
        "npm" | "pnpm" | "yarn" | "bun" => {
            let sub = operands.first().copied().unwrap_or("");
            if matches!(
                sub,
                "install"
                    | "i"
                    | "add"
                    | "ci"
                    | "update"
                    | "upgrade"
                    | "up"
                    | "remove"
                    | "rm"
                    | "uninstall"
                    | "publish"
                    | "login"
                    | "logout"
                    | "link"
                    | "dlx"
                    | "exec"
                    | "create"
                    | "init"
                    | "adduser"
                    | "token"
                    | "whoami"
                    | "audit"
                    | "outdated"
                    | "view"
                    | "info"
                    | "search"
            ) {
                return ask(format!(
                    "`{prog} {sub}` installs, publishes or talks to a registry"
                ));
            }
        }
        "pip" | "pip3" | "uv" | "poetry" | "pdm" => {
            let sub = operands.first().copied().unwrap_or("");
            if matches!(
                sub,
                "install"
                    | "uninstall"
                    | "download"
                    | "add"
                    | "remove"
                    | "sync"
                    | "lock"
                    | "publish"
                    | "pip"
                    | "tool"
                    | "update"
                    | "self"
            ) {
                return ask(format!("`{prog} {sub}` installs or talks to a registry"));
            }
        }
        "cargo" => {
            let sub = operands.first().copied().unwrap_or("");
            if matches!(
                sub,
                "add"
                    | "install"
                    | "uninstall"
                    | "publish"
                    | "login"
                    | "logout"
                    | "yank"
                    | "owner"
                    | "fetch"
                    | "update"
                    | "search"
                    | "vendor"
                    | "generate-lockfile"
            ) {
                return ask(format!("`cargo {sub}` installs or talks to a registry"));
            }
        }
        "go" => {
            let sub = operands.first().copied().unwrap_or("");
            if matches!(sub, "get" | "install") {
                return ask(format!("`go {sub}` downloads packages"));
            }
            if sub == "mod" && matches!(operands.get(1).copied(), Some("download" | "vendor")) {
                return ask("`go mod` downloads packages");
            }
        }
        p if p.starts_with("python") => {
            // `python -m pip install ...`
            if let Some(pos) = args.iter().position(|a| a == "-m")
                && args.get(pos + 1).map(String::as_str) == Some("pip")
            {
                return ask("`pip` installs packages");
            }
        }
        "bundle" | "dotnet" | "mvn" | "gradle" | "gradlew" | "./gradlew" => {
            if has_flag(&["install", "add", "restore", "publish", "deploy"]) || {
                matches!(
                    operands.first().copied(),
                    Some("install" | "add" | "restore")
                )
            } {
                return ask(format!("`{prog}` fetches dependencies"));
            }
        }
        "flutter" | "dart"
            if matches!(operands.first().copied(), Some("pub"))
                && matches!(
                    operands.get(1).copied(),
                    Some("get" | "add" | "upgrade" | "publish")
                ) =>
        {
            return ask("`pub` downloads packages");
        }
        _ => {}
    }
    Verdict::Auto
}

fn classify_git(args: &[String]) -> Verdict {
    // Global options before the subcommand.
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "-c" | "--config-env" => {
                return ask("`git -c` can run a program through a config value");
            }
            "-C" => {
                if args.get(i + 1).is_some_and(|d| escapes(d)) {
                    return ask("`git -C` points outside this folder");
                }
                i += 1;
            }
            "--git-dir" | "--work-tree" | "--exec-path" | "--namespace" => {
                return ask("it points git somewhere else");
            }
            a if a.starts_with("--git-dir=") || a.starts_with("--work-tree=") => {
                return ask("it points git somewhere else");
            }
            _ => {}
        }
        i += 1;
    }
    let Some(sub) = args.get(i).map(String::as_str) else {
        return Verdict::Auto;
    };
    let rest = &args[i + 1..];
    let any = |flags: &[&str]| rest.iter().any(|a| flags.contains(&a.as_str()));
    let dot = || {
        rest.iter()
            .any(|a| matches!(a.as_str(), "." | ":/" | "*" | "./"))
    };
    match sub {
        "push" | "remote" | "config" | "credential" | "submodule" | "clean" | "fetch" | "pull"
        | "clone" | "ls-remote" | "send-email" | "request-pull" | "svn" | "filter-branch"
        | "filter-repo" | "gc" | "prune" | "worktree" | "bundle" | "daemon" | "update-ref"
        | "replace" | "hash-object" | "fast-import" | "archive" | "instaweb" | "difftool"
        | "mergetool" | "bisect" | "lfs" | "maintenance" | "sparse-checkout" | "update-index"
        | "symbolic-ref" => ask(format!("`git {sub}` has remote or hard-to-undo effects")),
        "reset" if any(&["--hard", "--merge", "--keep"]) => {
            ask("`git reset --hard` discards uncommitted work")
        }
        "checkout" | "restore" | "switch" if dot() || any(&["-f", "--force"]) => {
            ask("it would overwrite uncommitted work across the folder")
        }
        "branch" if any(&["-D", "--delete", "-d", "-M", "-m"]) => {
            ask("it deletes or renames a branch")
        }
        "reflog" => ask("`git reflog` can expire history"),
        "stash" if matches!(rest.first().map(String::as_str), Some("drop" | "clear")) => {
            ask("it drops stashed work")
        }
        "rebase" | "cherry-pick" | "revert" | "commit" | "merge" | "tag" | "am" | "apply"
            if rest.iter().any(|a| a.starts_with("--exec")) =>
        {
            ask("it runs a command for each commit")
        }
        _ => Verdict::Auto,
    }
}

#[cfg(test)]
mod tests;
