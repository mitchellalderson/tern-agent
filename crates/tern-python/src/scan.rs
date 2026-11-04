//! Does a script use `async`? (docs/python-tool-spec.md §3.2.)
//!
//! Monty makes the host decide, for each call to a host function, whether to
//! answer with the value or with a future the script awaits later, and it
//! gives no hint which the script wants: `r = f(x)` and the `f(x)` in
//! `asyncio.gather(f(x), f(y))` look the same at the call. So the choice is
//! made once per script from its source. A script that mentions `async`,
//! `await` or `asyncio` gets awaitables from tool calls (and `gather` runs
//! them together); any other script gets values.

/// Whether `code` uses `async`, `await` or `asyncio` outside comments and
/// string literals.
pub fn uses_async(code: &str) -> bool {
    let chars: Vec<char> = code.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '#' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '\'' | '"' => i = skip_string(&chars, i),
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if matches!(word.as_str(), "async" | "await" | "asyncio") {
                    return true;
                }
            }
            _ => i += 1,
        }
    }
    false
}

/// The index just past the string literal that starts at `i` (a quote).
fn skip_string(chars: &[char], i: usize) -> usize {
    let quote = chars[i];
    let triple = chars.get(i + 1) == Some(&quote) && chars.get(i + 2) == Some(&quote);
    let mut j = i + if triple { 3 } else { 1 };
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 2,
            c if c == quote => {
                if !triple {
                    return j + 1;
                }
                if chars.get(j + 1) == Some(&quote) && chars.get(j + 2) == Some(&quote) {
                    return j + 3;
                }
                j += 1;
            }
            '\n' if !triple => return j + 1,
            _ => j += 1,
        }
    }
    chars.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_async_in_code_only() {
        assert!(uses_async("import asyncio\nx = 1"));
        assert!(uses_async("async def f():\n    return await g()"));
        assert!(uses_async("r = await lookup('a')"));
        assert!(!uses_async(
            "x = 'await this'\ny = \"asyncio\"  # async await"
        ));
        assert!(!uses_async("s = '''\nawait\n'''\nprint(s)"));
        assert!(!uses_async("awaited = 1\nasynchronous = 2"));
        assert!(!uses_async(r"s = 'it\'s await'"));
        assert!(!uses_async(""));
    }
}
