//! A strict allowlist for read-only shell pipelines (`tail -c 200 f | od -c`).
//!
//! Unlike [`crate::shell_class`], which is a speed bump over everything else,
//! this answers a narrower question with a short yes: is the command
//! *nothing but* a pipeline of well-known reading programs, with no
//! redirects, substitutions, variables, globs, chaining or backgrounding, and
//! with every operand a path (or pattern) that stays inside the run's
//! workspace? Only then does the shell tool call it a `read`. Anything else,
//! including anything it can't parse, answers `false` and the usual rules
//! apply. It is a filter in front of the sandbox, not a replacement for it.

use tern_core::workspace::WorkspaceScope;

/// Programs that only read their operands and stdin and write stdout.
/// Left out on purpose: `sed`, `awk`, `find`, `xargs`, `tee`, `sort` and
/// `uniq` (can write files), `xxd` (writes a second operand), `file`
/// (`-C`), `env`, and anything that runs another command.
const READERS: &[&str] = &[
    "cat", "head", "tail", "od", "hexdump", "wc", "grep", "egrep", "fgrep", "ls", "stat", "cut",
    "tr", "nl", "tac", "rev",
];

/// Characters that never appear outside quotes: redirects, chaining,
/// substitution, globs, history, comments, tilde, escapes, newlines.
const FORBIDDEN: &[char] = &[
    ';', '&', '<', '>', '(', ')', '{', '}', '`', '$', '\\', '!', '*', '?', '[', ']', '#', '~',
    '\n', '\r', '\0',
];

/// Whether `command` is a read-only pipeline confined to `scope`'s folder.
pub fn is_read_only(command: &str, scope: &WorkspaceScope) -> bool {
    let Some(stages) = split(command) else {
        return false;
    };
    !stages.is_empty() && stages.iter().all(|words| stage_ok(words, scope))
}

/// Split into pipeline stages of words, or `None` on anything unusual.
fn split(command: &str) -> Option<Vec<Vec<String>>> {
    let mut stages: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            // Inside quotes nothing expands except in double quotes.
            Some('"') if matches!(c, '$' | '`' | '\\' | '!' | '\n' | '\r' | '\0') => return None,
            Some(_) if matches!(c, '\n' | '\r' | '\0') => return None,
            Some(_) => word.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '|' => {
                    if in_word {
                        stages.last_mut()?.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                    if stages.last()?.is_empty() {
                        return None;
                    }
                    stages.push(Vec::new());
                }
                c if c.is_whitespace() && c != '\n' && c != '\r' => {
                    if in_word {
                        stages.last_mut()?.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                c if FORBIDDEN.contains(&c) || c.is_control() => return None,
                c => {
                    word.push(c);
                    in_word = true;
                }
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        stages.last_mut()?.push(word);
    }
    // A trailing `|` leaves an empty last stage.
    if stages.last()?.is_empty() {
        return None;
    }
    Some(stages)
}

fn stage_ok(words: &[String], scope: &WorkspaceScope) -> bool {
    let Some((prog, rest)) = words.split_first() else {
        return false;
    };
    // A bare program name only: no path to a different binary.
    if !READERS.contains(&prog.as_str()) {
        return false;
    }
    let recursive_grep = matches!(prog.as_str(), "grep" | "egrep" | "fgrep");
    for w in rest {
        if let Some(flag) = w.strip_prefix('-').filter(|f| !f.is_empty()) {
            // A flag is never a path. A value glued on with `=` must not be
            // one either (`--file=/etc/passwd`).
            if flag.contains('/') || flag.contains("..") {
                return false;
            }
            let long = flag.starts_with('-');
            // Following a file never ends; recursion follows symlinks out.
            if matches!(prog.as_str(), "tail") && (flag.contains('f') || flag.contains('F')) {
                return false;
            }
            if recursive_grep
                && (flag == "-recursive"
                    || flag == "-dereference-recursive"
                    || (!long && flag.chars().any(|c| c == 'r' || c == 'R')))
            {
                return false;
            }
            // `ls -R` and friends only list; nothing else to forbid.
            continue;
        }
        if !operand_inside(w, scope) {
            return false;
        }
    }
    true
}

/// A non-flag word: a path, a pattern or a number. It must not be able to
/// point outside the folder, whichever it is.
fn operand_inside(w: &str, scope: &WorkspaceScope) -> bool {
    if w.is_empty() {
        return true;
    }
    if w.split('/').any(|p| p == "..") {
        return false;
    }
    // Plain words (numbers, patterns) with no slash resolve under the cwd and
    // are checked the same way as paths, so a symlink can't smuggle one out.
    scope.read_target_inside(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn scope(ws: &Path) -> std::sync::Arc<WorkspaceScope> {
        WorkspaceScope::for_dir(ws.to_path_buf())
    }

    #[test]
    fn plain_reading_pipelines_are_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().canonicalize().unwrap();
        std::fs::write(ws.join("vs-trigger-outline.md"), "x").unwrap();
        let s = scope(&ws);
        let abs = ws.join("vs-trigger-outline.md");
        let abs = abs.display();
        for ok in [
            format!("tail -c 200 {abs} | od -c | tail -10"),
            "cat vs-trigger-outline.md".to_owned(),
            "head -n 5 vs-trigger-outline.md | wc -l".to_owned(),
            "grep -n 'a|b' vs-trigger-outline.md | head".to_owned(),
            "grep -i \"two words\" vs-trigger-outline.md".to_owned(),
            "ls -la".to_owned(),
            "ls -la .".to_owned(),
            "wc -c vs-trigger-outline.md".to_owned(),
            "cat vs-trigger-outline.md | tr a-z A-Z | cut -c 1-3".to_owned(),
        ] {
            assert!(is_read_only(&ok, &s), "{ok}");
        }
    }

    #[test]
    fn redirects_substitution_network_writes_and_outside_paths_are_not() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (ws, other) = (root.join("ws"), root.join("other"));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("o.txt"), "x").unwrap();
        std::fs::write(ws.join("a.md"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, ws.join("link")).unwrap();
        let s = scope(&ws);
        let outside = other.join("o.txt");
        let mut bad: Vec<String> = [
            "",
            "|",
            "cat a.md |",
            "| cat a.md",
            "cat a.md > b.md",
            "cat a.md >> b.md",
            "cat a.md 2>&1",
            "cat < a.md",
            "cat a.md; rm a.md",
            "cat a.md && echo hi",
            "cat a.md || true",
            "cat a.md &",
            "cat $(echo a.md)",
            "cat `echo a.md`",
            "cat \"$HOME/x\"",
            "cat ~/x",
            "cat *.md",
            "cat a.md\nrm a.md",
            "cat 'unterminated",
            "cat ../other/o.txt",
            "cat /etc/passwd",
            "cat /",
            "ls /",
            "ls ..",
            "curl http://example.com",
            "wget http://example.com | cat",
            "find . -exec rm {} +",
            "find . -name a",
            "sed -i s/a/b/ a.md",
            "awk 'BEGIN{system(\"id\")}'",
            "tee out.md",
            "sort -o a.md a.md",
            "xxd a.md b.md",
            "env cat a.md",
            "sh -c 'cat a.md'",
            "xargs cat",
            "grep -r foo .",
            "grep -R foo .",
            "grep -rn foo .",
            "grep --recursive foo .",
            "grep --file=/etc/passwd a.md",
            "tail -f a.md",
            "tail -F a.md",
            "head --lines=/etc/passwd a.md",
            "./cat a.md",
            "/bin/cat a.md",
            "FOO=1 cat a.md",
            "cat a.md # comment",
            "git log",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        bad.push(format!("cat {}", outside.display()));
        bad.push(format!("tail -c 5 {} | od -c", outside.display()));
        #[cfg(unix)]
        bad.extend([
            "cat link/o.txt".to_owned(),
            "ls link".to_owned(),
            format!("cat {}/link/o.txt", ws.display()),
        ]);
        for b in &bad {
            assert!(!is_read_only(b, &s), "{b:?}");
        }
    }

    #[test]
    fn protected_files_are_not_read_only_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().canonicalize().unwrap();
        let data = ws.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("secrets.toml"), "k").unwrap();
        let s = WorkspaceScope::for_dir_guarded(
            ws.clone(),
            std::sync::Arc::new(tern_core::workspace::DenySet::for_data_dir(&data)),
        );
        assert!(!is_read_only("cat data/secrets.toml", &s));
        assert!(!is_read_only("ls data", &s));
    }
}
