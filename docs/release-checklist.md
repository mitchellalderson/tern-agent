# Release checklist (before the first public release)

Nothing is published until a person pushes a tag. The first release is the pre-release `v0.1.0-alpha.1`; the exact
steps are in [Cut the alpha](#cut-the-alpha) below, and the rest of this page is what has to be true first.

**Pre-releases and "latest".** A tag with a hyphen (`v0.1.0-alpha.1`, `-beta.N`, `-rc.N`) is published by `release.yml`
as a GitHub *pre-release*, and GitHub's `releases/latest` (and `releases/latest/download/...`) ignores pre-releases, so
it returns 404 while every release is one. That is why `install.sh` looks up the newest release through the API
(`--version <tag>` pins one, `--stable` asks for `releases/latest`) and the README links the DMG by tag:
`releases/download/<tag>/tern-desktop-macos.dmg`. When the first stable release exists, `releases/latest` starts
working and a README link without the tag becomes possible.

## Cut the alpha

From a clean checkout of the commit to release (on `main`, after the branch is merged):

1. `scripts/release-dry-run.sh --tag v0.1.0-alpha.1` passes. It checks that `Cargo.toml`, `Cargo.lock`,
   `apps/desktop/pubspec.yaml` (`0.1.0-alpha.1+1`) and the `CHANGELOG.md` entry agree, runs `actionlint` and
   `shellcheck`, and prints what the tag publishes. Add `--build` to also build the CLI with `--profile dist` (slow).
2. CI is green on that commit (`fmt`, `clippy -D warnings`, tests, `flutter analyze` and `flutter test`, `shellcheck`).
   `release.yml` runs the same checks again through `ci.yml` before it builds anything.
3. Replace `TBD` in the CHANGELOG heading with today's date and commit that, if not already done.
4. Decide notarization (section 4). Without the Apple secrets the release still works: the DMG is ad-hoc signed.
5. Tag and push (this is the step that publishes; nothing else does):

   ```sh
   git tag -a v0.1.0-alpha.1 -m "tern v0.1.0-alpha.1"
   git push origin v0.1.0-alpha.1
   ```

6. Watch `release` in the Actions tab: `tag matches the versions`, then `ci`, then the four builds, the desktop app,
   and `publish`.
7. Check the release page, `https://github.com/mitchellalderson/tern-agent/releases/tag/v0.1.0-alpha.1`: it is marked
   **Pre-release** (not Latest) and has 4 tarballs, the DMG and zip, a `.sha256` for each, and `SHA256SUMS`. Then:

   ```sh
   gh release view v0.1.0-alpha.1 --repo mitchellalderson/tern-agent --json isPrerelease,assets --jq '.isPrerelease, (.assets|length)'
   curl -fsSL https://raw.githubusercontent.com/mitchellalderson/tern-agent/main/install.sh | sh   # installs the alpha
   tern --version                                                                                 # tern 0.1.0-alpha.1
   gh attestation verify tern-aarch64-apple-darwin.tar.gz --repo mitchellalderson/tern-agent
   ```

8. Run the smoke tests in section 5 on clean machines.

If the workflow fails before `publish`, nothing is published: fix it, delete the tag
(`git push origin :refs/tags/v0.1.0-alpha.1 && git tag -d v0.1.0-alpha.1`), and tag again.

### Yank a release

```sh
gh release delete v0.1.0-alpha.1 --repo mitchellalderson/tern-agent --yes --cleanup-tag
```

Deleting the release and its tag makes the installer fall back to the next newest release. To keep the tag but hide the
release, `gh release edit v0.1.0-alpha.1 --draft`. Never reuse a version number for different bits: ship
`v0.1.0-alpha.2` (bump `Cargo.toml`, `Cargo.lock`, the pubspec and the CHANGELOG together).

## 1. The repository is reachable

- [ ] `https://github.com/mitchellalderson/tern-agent` is public (or `TERN_REPO` and every README URL point at the
      repo that is). `curl -fsSI https://raw.githubusercontent.com/mitchellalderson/tern-agent/main/install.sh` returns 200.
- [ ] `LICENSE-*`, `SECURITY.md` and the README's `TODO(launch)` demo comment are dealt with.

## 2. Versions and tag

- [ ] `Cargo.toml` `[workspace.package] version` and `apps/desktop/pubspec.yaml` `version:` agree (the `version` job in
      `release.yml` and `desktop-release.yml` fails the tag otherwise). The desktop build is passed that version as
      `TERN_APP_VERSION`, which Settings › About and the version banner compare with the running core.
- [ ] CI is green on the commit: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace --no-fail-fast`, `flutter analyze` and `flutter test` in `apps/desktop`, `shellcheck`.
- [ ] macOS coverage is current: pull-request CI skips macOS unless labelled `ci-macos`, so look for a green
      `macOS-specific tests` job on the commit (main pushes and the tag's own `ci` run have it). See
      [ci-cost.md](ci-cost.md). Optionally run `ci` by hand on the commit for the universal DMG first.
- [ ] Tag the commit `vX.Y.Z` (or `vX.Y.Z-alpha.N`; the version job accepts semver with a pre-release part). `release.yml` owns `v*`: CLI tarballs for four targets, each with `.sha256`, an ad-hoc or
      signed DMG, and build provenance attestations, published as a release. `desktop-release.yml` owns `desktop-v*`
      and only leaves a draft; do not push both tags for the same version unless you mean to.

## 3. Assets (what `install.sh` and the README need)

- [ ] `tern-x86_64-unknown-linux-musl.tar.gz`, `tern-aarch64-unknown-linux-musl.tar.gz`,
      `tern-x86_64-apple-darwin.tar.gz`, `tern-aarch64-apple-darwin.tar.gz`, each with a `.sha256` beside it.
- [ ] `tern-desktop-macos.dmg`, `tern-desktop-macos.zip`, their `.sha256` files and `SHA256SUMS` (the README links the
      DMG by tag).
- [ ] Checksums verify: `shasum -a 256 -c tern-aarch64-apple-darwin.tar.gz.sha256` and
      `gh attestation verify <file> --repo mitchellalderson/tern-agent`.
- [ ] `install.sh` is served from `main` (raw URL in the README); it is not a release asset.

## 4. Notarization status (decide, then write it down)

- [ ] Either notarize: Apple Developer membership, Developer ID certificate, the `MACOS_*` / `APPLE_*` secrets
      ([desktop-release.md](desktop-release.md#checklist)), and `REQUIRE_NOTARIZE=1 scripts/release-macos.sh …` ends
      `done (notarized)`; the DMG opens on a second Mac with no prompt. Then delete the Gatekeeper "Open Anyway"
      paragraph in the README and `docs/reference.md`.
- [ ] Or ship ad-hoc signed: keep those paragraphs, and say in the release notes that the first open needs
      System Settings › Privacy & Security › **Open Anyway**.

## 5. Smoke test on clean machines (nothing here has been run against a real release)

- [ ] Clean macOS, no Rust or Flutter: run the README `curl … | sh`, then `tern --version`, `tern doctor`, `tern setup`.
- [ ] Clean Linux (a Docker `debian:stable-slim` with curl is fine): the same.
- [ ] Clean Mac: download the DMG from the releases page, drag to Applications, open via Open Anyway (or normally if
      notarized), choose "On this Mac", add a model, send a message.
- [ ] Upgrade: install the old version, `tern start`, install the new one, run `tern start`: it must report the
      version difference; open the app: it must show the version banner and **Restart tern** must clear it.
- [ ] Uninstall: `tern uninstall` leaves your data and lists what remains; `tern uninstall --purge` removes the data.
- [ ] Every URL in `README.md` and `docs/reference.md` returns 200.

## 6. After publishing

- [ ] At the first stable release, remove the "Status: alpha" paragraph from the README Quick start and the matching lines in
      `docs/reference.md`; put the `curl` line first.
- [ ] Note anything the smoke test found in the release notes, and file it.
