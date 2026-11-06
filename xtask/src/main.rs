//! Repo tasks, run through the aliases in `.cargo/config.toml`:
//!
//! ```sh
//! cargo test-all            # Rust, then Dart: everything CI runs for tests
//! cargo test-all --rust     # only the Rust suites
//! cargo test-all --dart     # only the Dart suites (still builds tern for the live tests)
//! cargo xtask changelog 0.2.0   # fold changelog.d/ into CHANGELOG.md (see changelog.rs)
//! ```
//!
//! Every step runs even if an earlier one fails, and a summary at the end
//! lists what failed. Std only, so it builds in a second.

mod changelog;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

const USAGE: &str = "usage: cargo test-all [--rust | --dart]

Runs the Rust tests (workspace tests, TUI smoke test) and the Dart tests
(tern_client and the desktop app, including the live tests against a
freshly built target/debug/tern). Needs python3 for the TUI smoke test
and Flutter 3.47+ (which provides dart) for the Dart suites.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("changelog") {
        return match changelog::run(&repo_root(), &args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let (rust, dart) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["test"] => (true, true),
        ["test", "--rust"] => (true, false),
        ["test", "--dart"] => (false, true),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    let root = repo_root();
    let tern = root.join("target/debug/tern");
    let client = root.join("apps/desktop/packages/tern_client");
    let desktop = root.join("apps/desktop");

    let mut steps: Vec<Step> = Vec::new();
    if rust {
        steps.push(Step::new(
            "rust: workspace tests",
            &root,
            "cargo",
            &["test", "--workspace"],
        ));
    }
    // The TUI smoke test and the Dart live tests both drive the real binary.
    steps.push(Step::new(
        "build target/debug/tern",
        &root,
        "cargo",
        &["build", "-p", "tern"],
    ));
    if rust {
        let tern = tern.to_string_lossy();
        steps.push(Step::new(
            "rust: TUI smoke test",
            &root,
            "python3",
            &["scripts/tui-smoke.py", &tern],
        ));
    }
    if dart {
        for tool in ["flutter", "dart"] {
            if !on_path(tool) {
                eprintln!(
                    "error: `{tool}` isn't on PATH. Install Flutter 3.47+, or run `cargo test-all --rust`."
                );
                return ExitCode::FAILURE;
            }
        }
        steps.push(Step::new(
            "dart: tern_client pub get",
            &client,
            "dart",
            &["pub", "get"],
        ));
        steps.push(Step::new("dart: tern_client tests", &client, "dart", &["test"]).bin(&tern));
        steps.push(Step::new("dart: desktop tests", &desktop, "flutter", &["test"]).bin(&tern));
    }

    let mut results = Vec::new();
    for step in &steps {
        // Nothing after the build can work without the binary.
        let ok = step.run();
        let fatal = !ok && step.name.starts_with("build");
        results.push((step.name, ok));
        if fatal {
            break;
        }
    }

    eprintln!("\n── summary ──");
    for (name, ok) in &results {
        eprintln!("  {} {name}", if *ok { "✓" } else { "✗" });
    }
    for step in &steps[results.len()..] {
        eprintln!("  - {} (skipped: the build failed)", step.name);
    }
    let failed = results.iter().filter(|(_, ok)| !ok).count();
    if failed == 0 && results.len() == steps.len() {
        eprintln!("all passed");
        ExitCode::SUCCESS
    } else {
        eprintln!("{failed} step(s) failed");
        ExitCode::FAILURE
    }
}

struct Step {
    name: &'static str,
    dir: PathBuf,
    program: &'static str,
    args: Vec<String>,
    tern_bin: Option<PathBuf>,
}

impl Step {
    fn new(name: &'static str, dir: &Path, program: &'static str, args: &[&str]) -> Self {
        Self {
            name,
            dir: dir.to_path_buf(),
            program,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            tern_bin: None,
        }
    }

    /// Set `TERN_BIN`, which turns on the Dart live tests.
    fn bin(mut self, tern: &Path) -> Self {
        self.tern_bin = Some(tern.to_path_buf());
        self
    }

    fn run(&self) -> bool {
        eprintln!(
            "\n── {} ──\n$ {} {}",
            self.name,
            self.program,
            self.args.join(" ")
        );
        let start = Instant::now();
        let mut cmd = Command::new(self.program);
        cmd.args(&self.args).current_dir(&self.dir);
        if let Some(bin) = &self.tern_bin {
            cmd.env("TERN_BIN", bin);
        }
        let ok = match cmd.status() {
            Ok(status) => status.success(),
            Err(e) => {
                eprintln!("error: couldn't run {}: {e}", self.program);
                false
            }
        };
        eprintln!(
            "── {} {} in {:.1}s",
            self.name,
            if ok { "passed" } else { "FAILED" },
            start.elapsed().as_secs_f64()
        );
        ok
    }
}

/// The workspace root: xtask lives one level below it.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map_or(manifest.clone(), Path::to_path_buf)
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}
