//! Which parts of `$HOME` a command may read so that language toolchains
//! work, and which parts of it are never readable. Both are data: the tables
//! below are the whole policy, and [`detect`] is a pure function over an
//! environment and a file-system probe so it can be unit-tested.
//!
//! Grants are the toolchain *installs* only (compilers, interpreters, shims),
//! read-only. Caches and package registries are not here: they live in the
//! workspace's state directory (`CARGO_HOME`, `npm_config_cache`, ...), so
//! no workspace reads or poisons another's downloads.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One toolchain installer or version manager.
pub(crate) struct Toolchain {
    pub name: &'static str,
    /// Environment variable that relocates the install (probed first).
    pub env: Option<&'static str>,
    /// Default location, relative to `$HOME`, when `env` is unset.
    pub default: &'static str,
    /// What to allow, relative to the install root; empty means the root.
    pub allow: &'static [&'static str],
}

/// The detection table. A row grants nothing unless its directory exists.
pub(crate) const TOOLCHAINS: &[Toolchain] = &[
    // rustup: toolchains, settings, update hashes. Cargo's registry is not
    // here (CARGO_HOME is per workspace).
    Toolchain {
        name: "rustup",
        env: Some("RUSTUP_HOME"),
        default: ".rustup",
        allow: &[],
    },
    // Cargo's own bin dir holds the rustup proxies (`cargo`, `rustc`) and
    // `cargo install`ed tools; its credentials and registry are not granted.
    Toolchain {
        name: "cargo bin",
        env: Some("CARGO_HOME"),
        default: ".cargo",
        allow: &["bin"],
    },
    Toolchain {
        name: "Volta",
        env: Some("VOLTA_HOME"),
        default: ".volta",
        allow: &[],
    },
    Toolchain {
        name: "nvm",
        env: Some("NVM_DIR"),
        default: ".nvm",
        allow: &[],
    },
    Toolchain {
        name: "fnm",
        env: Some("FNM_DIR"),
        default: ".local/share/fnm",
        allow: &[],
    },
    Toolchain {
        name: "pyenv",
        env: Some("PYENV_ROOT"),
        default: ".pyenv",
        allow: &[],
    },
    Toolchain {
        name: "rbenv",
        env: Some("RBENV_ROOT"),
        default: ".rbenv",
        allow: &[],
    },
    Toolchain {
        name: "asdf",
        env: Some("ASDF_DATA_DIR"),
        default: ".asdf",
        allow: &[],
    },
    Toolchain {
        name: "mise",
        env: Some("MISE_DATA_DIR"),
        default: ".local/share/mise",
        allow: &["installs", "shims"],
    },
    Toolchain {
        name: "mise binary",
        env: None,
        default: ".local/bin/mise",
        allow: &[],
    },
    Toolchain {
        name: "uv pythons",
        env: Some("UV_PYTHON_INSTALL_DIR"),
        default: ".local/share/uv/python",
        allow: &[],
    },
    Toolchain {
        name: "uv binary",
        env: None,
        default: ".local/bin/uv",
        allow: &[],
    },
    Toolchain {
        name: "SDKMAN",
        env: Some("SDKMAN_DIR"),
        default: ".sdkman",
        allow: &["candidates"],
    },
    Toolchain {
        name: "Bun",
        env: Some("BUN_INSTALL"),
        default: ".bun",
        allow: &["bin"],
    },
    Toolchain {
        name: "Deno",
        env: Some("DENO_INSTALL"),
        default: ".deno",
        allow: &["bin"],
    },
    // A Homebrew or Linuxbrew prefix outside the system dirs (the default
    // /opt/homebrew, /usr/local and /home/linuxbrew need no grant on macOS;
    // /home/linuxbrew does on Linux).
    Toolchain {
        name: "Homebrew prefix",
        env: Some("HOMEBREW_PREFIX"),
        default: "homebrew",
        allow: &[],
    },
    Toolchain {
        name: "Xcode developer dir",
        env: Some("DEVELOPER_DIR"),
        default: "",
        allow: &[],
    },
];

/// Absolute locations granted whatever `$HOME` is (no environment probe).
pub(crate) const FIXED_TOOLCHAIN_DIRS: &[(&str, &str)] = &[
    ("Linuxbrew", "/home/linuxbrew/.linuxbrew"),
    ("Go toolchain", "/usr/local/go"),
];

/// Locations under `$HOME` that are never readable, even when a grant (a
/// read-only folder, a toolchain, `$HOME` itself) would include them: keys,
/// cloud and registry credentials, shell history, browser profiles, the
/// Keychain, tern's own data. Relative to `$HOME`.
pub(crate) const SENSITIVE_HOME: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config",
    ".kube",
    ".docker",
    ".azure",
    ".oci",
    ".gcloud",
    ".terraform.d",
    ".password-store",
    ".netrc",
    ".npmrc",
    ".yarnrc",
    ".pypirc",
    ".git-credentials",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".bash_history",
    ".zsh_history",
    ".python_history",
    ".psql_history",
    ".mysql_history",
    ".tern",
    ".local/share/keyrings",
    ".mozilla",
    ".thunderbird",
    "Library/Keychains",
    "Library/Application Support",
    "Library/Cookies",
    "Library/Safari",
    "Library/Mail",
    "Library/Messages",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Accounts",
];

/// Roots below which macOS reads are denied unless granted (besides `$HOME`).
pub(crate) const MAC_PRIVATE_ROOTS: &[&str] = &["/Users", "/Volumes"];

/// A detected, existing toolchain location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainGrant {
    pub name: &'static str,
    pub path: PathBuf,
}

/// What detection needs from the machine; a trait object so tests can fake it.
pub(crate) struct Probe<'a> {
    pub home: Option<&'a Path>,
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    pub exists: &'a dyn Fn(&Path) -> bool,
}

/// Sensitive home paths, joined to `home`.
pub(crate) fn sensitive_paths(home: &Path) -> Vec<PathBuf> {
    SENSITIVE_HOME.iter().map(|s| home.join(s)).collect()
}

/// The toolchain locations present under `probe`, read-only grants. A grant
/// that is, contains or lies inside a sensitive path is dropped (an
/// environment variable pointing a toolchain at `~/.ssh` or `$HOME` must not
/// widen access).
pub(crate) fn detect(
    table: &[Toolchain],
    fixed: &[(&'static str, &str)],
    probe: &Probe<'_>,
) -> Vec<ToolchainGrant> {
    let sensitive: Vec<PathBuf> = probe.home.map(sensitive_paths).unwrap_or_default();
    let mut out: Vec<ToolchainGrant> = Vec::new();
    let mut push = |name: &'static str, path: PathBuf| {
        if !path.is_absolute() || !(probe.exists)(&path) {
            return;
        }
        if Some(path.as_path()) == probe.home || path.parent().is_none() {
            return;
        }
        if sensitive
            .iter()
            .any(|s| path.starts_with(s) || s.starts_with(&path))
        {
            return;
        }
        if !out.iter().any(|g| g.path == path) {
            out.push(ToolchainGrant { name, path });
        }
    };
    for t in table {
        let root = t
            .env
            .and_then(|k| (probe.env)(k))
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                (!t.default.is_empty())
                    .then(|| probe.home.map(|h| h.join(t.default)))
                    .flatten()
            });
        let Some(root) = root else { continue };
        if t.allow.is_empty() {
            push(t.name, root);
        } else {
            for sub in t.allow {
                push(t.name, root.join(sub));
            }
        }
    }
    for (name, path) in fixed {
        push(name, PathBuf::from(path));
    }
    out
}

/// [`detect`] against the real machine (`$HOME`, the process environment,
/// the file system). Paths are canonical so they match what the kernel sees.
pub fn detect_here() -> Vec<ToolchainGrant> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| crate::policy::canonical(Path::new(&h)));
    let env = |k: &str| std::env::var_os(k);
    let exists = |p: &Path| p.exists();
    detect(
        TOOLCHAINS,
        FIXED_TOOLCHAIN_DIRS,
        &Probe {
            home: home.as_deref(),
            env: &env,
            exists: &exists,
        },
    )
    .into_iter()
    .map(|g| ToolchainGrant {
        path: crate::policy::canonical(&g.path),
        ..g
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn run(env: &[(&str, &str)], present: &[&str], home: &str) -> Vec<(&'static str, String)> {
        let env: HashMap<String, OsString> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        let present: HashSet<PathBuf> = present.iter().map(PathBuf::from).collect();
        let getenv = |k: &str| env.get(k).cloned();
        let exists = |p: &Path| present.contains(p);
        detect(
            TOOLCHAINS,
            FIXED_TOOLCHAIN_DIRS,
            &Probe {
                home: Some(Path::new(home)),
                env: &getenv,
                exists: &exists,
            },
        )
        .into_iter()
        .map(|g| (g.name, g.path.to_string_lossy().into_owned()))
        .collect()
    }

    #[test]
    fn defaults_are_probed_under_home_and_only_when_present() {
        let got = run(
            &[],
            &["/Users/u/.rustup", "/Users/u/.cargo/bin", "/Users/u/.volta"],
            "/Users/u",
        );
        let paths: Vec<&str> = got.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(
            paths,
            ["/Users/u/.rustup", "/Users/u/.cargo/bin", "/Users/u/.volta"]
        );
        assert!(run(&[], &[], "/Users/u").is_empty());
    }

    #[test]
    fn cargo_grants_the_bin_dir_never_the_registry_or_credentials() {
        let got = run(
            &[],
            &[
                "/Users/u/.cargo",
                "/Users/u/.cargo/bin",
                "/Users/u/.cargo/registry",
            ],
            "/Users/u",
        );
        assert_eq!(got, [("cargo bin", "/Users/u/.cargo/bin".to_owned())]);
    }

    #[test]
    fn an_environment_variable_relocates_an_install() {
        let got = run(
            &[
                ("RUSTUP_HOME", "/Volumes/dev/rustup"),
                ("NVM_DIR", "/opt/nvm"),
            ],
            &["/Volumes/dev/rustup", "/opt/nvm", "/Users/u/.rustup"],
            "/Users/u",
        );
        let paths: Vec<&str> = got.iter().map(|(_, p)| p.as_str()).collect();
        assert_eq!(paths, ["/Volumes/dev/rustup", "/opt/nvm"]);
    }

    #[test]
    fn mise_and_sdkman_grant_only_their_install_subdirs() {
        let got = run(
            &[],
            &[
                "/Users/u/.local/share/mise/installs",
                "/Users/u/.local/share/mise/shims",
                "/Users/u/.local/share/mise/state",
                "/Users/u/.sdkman/candidates",
                "/Users/u/.local/bin/mise",
            ],
            "/Users/u",
        );
        let paths: Vec<&str> = got.iter().map(|(_, p)| p.as_str()).collect();
        assert!(paths.contains(&"/Users/u/.local/share/mise/installs"));
        assert!(paths.contains(&"/Users/u/.local/share/mise/shims"));
        assert!(paths.contains(&"/Users/u/.sdkman/candidates"));
        assert!(paths.contains(&"/Users/u/.local/bin/mise"));
        assert!(!paths.iter().any(|p| p.ends_with("/state")));
    }

    #[test]
    fn a_variable_can_not_point_a_toolchain_at_secrets_or_home() {
        let got = run(
            &[
                ("RUSTUP_HOME", "/Users/u/.ssh"),
                ("NVM_DIR", "/Users/u"),
                ("PYENV_ROOT", "/Users/u/.config/pyenv"),
                ("ASDF_DATA_DIR", "/Users/u/.aws/asdf"),
            ],
            &[
                "/Users/u/.ssh",
                "/Users/u",
                "/Users/u/.config/pyenv",
                "/Users/u/.aws/asdf",
            ],
            "/Users/u",
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn relative_variables_are_ignored() {
        assert!(run(&[("VOLTA_HOME", "volta")], &["volta"], "/Users/u").is_empty());
    }

    #[test]
    fn every_row_is_sane() {
        for t in TOOLCHAINS {
            assert!(!t.name.is_empty());
            assert!(!Path::new(t.default).is_absolute(), "{}", t.name);
            for a in t.allow {
                assert!(!a.starts_with('/') && !a.contains(".."), "{}: {a}", t.name);
            }
            // No default grant overlaps a sensitive location.
            let home = Path::new("/h");
            let root = home.join(t.default);
            let granted: Vec<PathBuf> = if t.allow.is_empty() {
                vec![root]
            } else {
                t.allow.iter().map(|a| root.join(a)).collect()
            };
            for g in granted {
                for s in sensitive_paths(home) {
                    assert!(
                        t.default.is_empty() || !(g.starts_with(&s) || s.starts_with(&g)),
                        "{} overlaps {}",
                        t.name,
                        s.display()
                    );
                }
            }
        }
        for s in SENSITIVE_HOME {
            assert!(!s.starts_with('/') && !s.contains(".."), "{s}");
        }
    }

    #[test]
    fn sensitive_list_covers_the_owner_named_locations() {
        for must in [
            ".ssh",
            ".aws",
            ".gnupg",
            ".config",
            ".tern",
            "Library/Keychains",
        ] {
            assert!(SENSITIVE_HOME.contains(&must), "{must}");
        }
    }
}
