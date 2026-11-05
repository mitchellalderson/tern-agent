# Release checklist (before the first public release)

Today nothing is published: `releases/latest` returns 404, so the README's one-line installer and DMG link do not work
and the README says so. This is what has to be true before that sentence is removed. Nothing here is done by a script
that publishes on its own; every step that publishes is a person pushing a tag or clicking Publish.

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
- [ ] Tag the commit `vX.Y.Z`. `release.yml` owns `v*`: CLI tarballs for four targets, each with `.sha256`, an ad-hoc or
      signed DMG, and build provenance attestations, published as a release. `desktop-release.yml` owns `desktop-v*`
      and only leaves a draft; do not push both tags for the same version unless you mean to.

## 3. Assets (what `install.sh` and the README need)

- [ ] `tern-x86_64-unknown-linux-musl.tar.gz`, `tern-aarch64-unknown-linux-musl.tar.gz`,
      `tern-x86_64-apple-darwin.tar.gz`, `tern-aarch64-apple-darwin.tar.gz`, each with a `.sha256` beside it.
- [ ] `tern-desktop-macos.dmg` and its `.sha256` (the README asks people to download it from the releases page).
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

- [ ] Remove the "Status: pre-release" paragraph from the README Quick start and the matching lines in
      `docs/reference.md`; put the `curl` line first.
- [ ] Note anything the smoke test found in the release notes, and file it.
