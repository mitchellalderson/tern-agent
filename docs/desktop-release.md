# Releasing the desktop app (G6)

How the macOS app is signed, notarized and packed, how the Linux app is
packed, and what has to be done once with Apple credentials. Design context:
[gui-spec.md](gui-spec.md) §9–§10. The CLI tarballs and `install.sh` are
`release.yml`'s and aren't covered here.

## What a release produces

| File | For | Made by |
|---|---|---|
| `tern-desktop-macos.dmg` (+ `.sha256`) | people: drag tern to Applications | `scripts/release-macos.sh` |
| `tern-desktop-macos.zip` (+ `.sha256`) | auto-updaters (Sparkle and the like) | same |
| `tern-desktop-linux-x86_64.tar.gz`, `.deb` (+ `.sha256`) | Linux, x86_64 | `scripts/package-linux-app.sh` |
| `tern-desktop-linux-aarch64.tar.gz`, `.deb` (+ `.sha256`) | Linux, arm64 | same |

Names carry no version, so `releases/latest/download/<name>` always works.
Every build has its own `tern` inside, matched to the app.

## macOS

### Run it locally without credentials (dry run)

```sh
cargo build --profile dist -p tern                 # or any tern binary (a lipo'd universal one makes a universal app)
scripts/release-macos.sh --dry-run target/dist/tern out/
```

This builds the Flutter app (`flutter build macos --release`), puts `tern` into
`tern Tray.app`, signs ad-hoc, runs `scripts/verify-macos-app.sh` (bundle
layout, `tern-tray --version`, architectures, `codesign --verify --deep
--strict`), packs the zip and dmg, mounts the dmg and verifies the app inside,
unpacks the zip and verifies that, and writes the checksums. The
`notarytool` and `stapler` commands it would have run are printed, not run.
`--skip-flutter-build` reuses the last build. The same happens without
`--dry-run` when `MACOS_SIGN_IDENTITY` is unset.

`scripts/package-macos-app.sh <tern> <out>` is the old entry point; it now just
calls `release-macos.sh`, so `release.yml` keeps working (and gains the zip and
the stricter signing).

### Modes

| Mode | When | What happens |
|---|---|---|
| unsigned | no `MACOS_SIGN_IDENTITY`, or `--dry-run` | ad-hoc signature; users allow the app once in Privacy & Security |
| signed | `MACOS_SIGN_IDENTITY` set | Developer ID, hardened runtime, secure timestamp |
| notarized | signed, and notary credentials set | app and dmg submitted to Apple, tickets stapled, `spctl`/`stapler` checked |

`REQUIRE_NOTARIZE=1` turns "signed but no notary credentials" into an error,
so a release can't go out half done. The workflow sets it for tags.

### How it signs

Inside out, one `codesign` per piece, never `--deep` (it applies one set of
options to everything, and the notary service rejects code signed without the
hardened runtime and a timestamp): frameworks and dylibs, then
`tern Tray.app` (which carries the `tern-tray` binary), then `tern.app` with
`macos/Runner/Release.entitlements`; the dmg is signed too. Adding the tray
bundle breaks the Flutter build's seal, which is why the app is always signed
again after bundling.

### Entitlements

`apps/desktop/macos/Runner/Release.entitlements` has one key,
`com.apple.security.app-sandbox = false`, and that is enough under the hardened
runtime. Checked against what the app does:

| Needs | Entitlement? |
|---|---|
| Network client/server (daemon API over TCP, model calls by the daemon) | No: those keys exist for sandboxed apps; unsandboxed apps aren't restricted |
| Keychain (device tokens, through `/usr/bin/security`) | No: the CLI is run as a child process; no keychain-access-groups, since nothing uses the Keychain API directly |
| Spawning `tern` (the sidecar), `sandbox-exec`, shell commands | No: the hardened runtime doesn't restrict `exec` |
| Login items (`SMAppService`, registered by `tern service install --tray`) | No entitlement; macOS shows the "Added a login item" notification |
| JIT or unsigned executable memory | No: Flutter release builds are AOT and the `python` tool is Monty, a pure Rust interpreter |
| Loading unsigned libraries | No: everything inside is signed with one identity, so library validation passes. Don't add `disable-library-validation` |
| `get-task-allow` | Must be absent (the dry run's verify step fails if it appears); Xcode adds it to debug builds only |

If a future feature needs one (microphone, Apple Events for automation, a
camera), add it to `Release.entitlements` and to this table. Notarized apps
also need an `NSAppleEventsUsageDescription`-style Info.plist string for the
matching TCC prompt.

### One-time setup (needs your Apple credentials)

You need a paid Apple Developer Program membership.

1. **Developer ID Application certificate.** developer.apple.com > Certificates
   > "+" > *Developer ID Application*, with a CSR made in Keychain Access. Install
   the downloaded `.cer`. Then:
   ```sh
   security find-identity -v -p codesigning
   #   1) ABCD… "Developer ID Application: Your Name (TEAMID)"
   ```
   The quoted string is `MACOS_SIGN_IDENTITY`. (A "Developer ID Installer" certificate isn't needed: tern ships as a dmg.)
2. **Notary credentials**, one of:
   - *Keychain profile* (best on your own Mac): create an app-specific password at
     appleid.apple.com, then
     `xcrun notarytool store-credentials tern-notary --apple-id you@example.com --team-id TEAMID`
     and set `NOTARY_KEYCHAIN_PROFILE=tern-notary`.
   - *App Store Connect API key* (best for CI): App Store Connect > Users and
     Access > Integrations > Team Keys, a key with the *Developer* role; download
     the `.p8` once. Set `APPLE_API_KEY_PATH` (the file), `APPLE_API_KEY_ID`,
     `APPLE_API_ISSUER`.
   - *Apple ID*: `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` (the app-specific password).
     Works, but the password is on the `notarytool` command line while it runs.
3. **Try it locally** before CI:
   ```sh
   export MACOS_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)"
   export NOTARY_KEYCHAIN_PROFILE=tern-notary
   REQUIRE_NOTARIZE=1 scripts/release-macos.sh target/dist/tern out/
   ```
   Expect `==> done (notarized)`. A rejected submission prints Apple's log.
4. **Check the result like a user would**: download the dmg through a browser
   (so it's quarantined), open it, drag tern to Applications, open it: no
   "unidentified developer" prompt. `spctl --assess -vv /Applications/tern.app`
   says `source=Notarized Developer ID`.
5. **CI secrets** (repository settings > Secrets and variables > Actions):

   | Secret | Value |
   |---|---|
   | `MACOS_CERTIFICATE` | `base64 -i cert.p12` of the exported certificate and private key (Keychain Access > right-click the identity > Export, `.p12`) |
   | `MACOS_CERTIFICATE_PASSWORD` | the `.p12` password |
   | `MACOS_SIGN_IDENTITY` | the identity string from step 1 |
   | `APPLE_API_KEY_P8` | `base64 -i AuthKey_XXXX.p8` |
   | `APPLE_API_KEY_ID` | the key's id |
   | `APPLE_API_ISSUER` | the issuer id shown above the keys list |
   | or `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` | instead of the API key |

   Without them the workflow still builds, unsigned. Nothing is ever written to
   the repository; the `.p8` is written to the runner's temp dir and deleted at
   the end, and the signing keychain is deleted too.

### Checklist

- [ ] Apple Developer Program membership active
- [ ] Developer ID Application certificate installed, `MACOS_SIGN_IDENTITY` noted
- [ ] Notary profile or API key created
- [ ] Local run: `REQUIRE_NOTARIZE=1 scripts/release-macos.sh …` ends `done (notarized)`
- [ ] Downloaded dmg opens on a second Mac with no Gatekeeper prompt
- [ ] CI secrets added
- [ ] Version bumped in `Cargo.toml` and `apps/desktop/pubspec.yaml`, pushed, tagged `desktop-vX.Y.Z`
- [ ] The workflow's draft release has the 4 mac + 8 Linux files; review, publish
- [ ] `gh release download` the published dmg and run `spctl` once more

## Linux

### Formats, and why these

- **`.tar.gz`**: Flutter's `bundle/` folder plus `tern`, a desktop entry and icons. The executable's RPATH is `$ORIGIN/lib`, so it runs from wherever it's unpacked. No dependencies on any packaging tool; works on every distro that has GTK 3. `install-local.sh` inside adds the menu entry for one user.
- **`.deb`**: installs to `/usr/lib/tern-desktop`, links `tern_desktop` and `tern` into `/usr/bin`, adds the menu entry and icons, and declares its GTK dependencies, so `apt install ./file.deb` is one step on Debian, Ubuntu and Mint. It builds with just `dpkg-deb`.
- **Not AppImage**: a Flutter GTK app is only really portable if GTK and its theme/IM modules are bundled (a large, fiddly set of excludes), it needs FUSE 2 (gone from recent Ubuntu defaults) or `--appimage-extract-and-run`, and `appimagetool` is a download at build time. **Not Flatpak** yet: it needs its own sandbox permission work (the daemon, the tern sandbox backends, talking to the host's login service). Both fit later; the folder layout is what they'd wrap.
- **No `.rpm`**: `rpmbuild` isn't worth it until someone asks; the tarball covers Fedora.

### System dependencies

To run (the `.deb` declares these; for the tarball install them yourself):

- GTK 3 (`libgtk-3-0`) and GLib; a working OpenGL/EGL driver (Flutter draws with GL; Mesa is fine)
- For the **tray icon**: a StatusNotifier host. KDE, XFCE, Cinnamon and others have one; **GNOME needs the "AppIndicator and KStatusNotifierItem Support" extension**. The tray is pure Rust over D-Bus (no GTK, no libappindicator). Quit confirmations use `zenity` or `kdialog` when present; without either, Quit just quits.
- **No `libsecret`**: the app has no secure-storage plugin. On Linux device tokens go in a `0600` file beside `desktop.json` (`apps/desktop/lib/state/connection.dart`); only macOS uses the Keychain. If a libsecret-backed store is added later (`flutter_secure_storage`), it adds `libsecret-1-0` to Depends and `libsecret-1-dev` to the build.
- Start at login: `tern service install --tray` writes an XDG autostart entry and a systemd user unit; both are the CLI's (T8c), not the package's.

To build (Ubuntu 22.04+/Debian 12+; the workflow's list):

```sh
sudo apt-get install clang cmake ninja-build pkg-config libgtk-3-dev liblzma-dev dpkg-dev
```

### Build it

On a Linux machine with Flutter 3.47+ (and a `tern` for the same architecture, e.g. the musl build from `release.yml`):

```sh
scripts/package-linux-app.sh target/x86_64-unknown-linux-musl/dist/tern out/
```

It runs `flutter build linux --release`, assembles the folder, writes the tarball and the `.deb`, and checks both (expected files present, `.deb` contents and control, `desktop-file-validate`, `ldd` for unresolved libraries). `--arch aarch64` names an arm64 build (it has to run on an arm64 host: Flutter doesn't cross-compile Linux desktop apps). `--bundle <dir>` packs an already built bundle, which is how the packaging was tested on a Mac.

The tray's "Open tern" runs `tern_desktop` next to the `tern` binary, and the app finds its `tern` the same way, so the layout (both files in one folder, or both linked from `/usr/bin` into one) matters. The window title is "tern", the icon name `tern`, and the desktop entry is `dev.tern.tern_desktop.desktop` (the GTK application id, which is how Wayland matches window to entry).

## The workflow

`.github/workflows/desktop-release.yml`:

- **Triggers**: pushing a tag `desktop-v*`, or *Run workflow*. With a tag (pushed, or typed into the dispatch form) it ends in a **draft** release with the files attached; with the form's tag empty it only leaves workflow artifacts. It never creates a tag and never publishes.
- **Why `desktop-v*` and not `v*`**: `release.yml` already owns `v*` (CLI tarballs, plus an ad-hoc or signed dmg) and creates the release. Two workflows creating the same release would race. To fold them together later: drop `release.yml`'s `desktop-macos` job and have this workflow's `release` job run on `v*`, or call this workflow from `release.yml`.
- **Jobs**: lint the scripts and the workflow; build `tern` for four targets; macOS (universal) signed and notarized when the secrets exist, else unsigned; Linux on `ubuntu-24.04` and `ubuntu-24.04-arm` (install the `.deb` and run `tern --version`, then an informational Xvfb smoke test that the window stays up); attach to the draft.
- CI's own `shellcheck` step (in `ci.yml`) globs `scripts/*.sh` and `apps/desktop/tool/*.sh`; it doesn't reach `apps/desktop/linux/packaging/*.sh`. This workflow's lint job covers them; add that glob to `ci.yml` when convenient.

## What has and hasn't been verified

Verified on a Mac (Apple Silicon, Xcode 27, Flutter 3.47.9):

- `scripts/release-macos.sh --dry-run`: release build, bundling, ad-hoc signing, all structure checks, zip, dmg, mounting and verifying both, checksums. The hardened-runtime signing options and the entitlements file were also applied once with an ad-hoc identity (`flags=…(adhoc,runtime)`).
- `scripts/package-linux-app.sh --bundle <stub>`: tarball and `.deb` contents, ownership (`root/root`), symlinks, control fields, `desktop-file-validate`, `install-local.sh` install and uninstall into a scratch `XDG_DATA_HOME`.
- `shellcheck` on every new script; `actionlint` on the workflow.
- `flutter analyze lib test` and `flutter test` in `apps/desktop`.

Not verified (no credentials or Linux box here):

- **Developer ID signing, `notarytool`, `stapler`, and `spctl` on a notarized build**: the code paths exist and print their commands in a dry run, but Apple has never seen this app. Expect to iterate on the first real run (a rejected submission's log says which binary and why).
- **The CI certificate import and API-key steps** are copied from `release.yml` (certificate) or new (API key), unexercised here.
- **`flutter build linux`** on a real Linux host, the `.deb`'s dependency names on each distro, `ldd` on the real bundle, the Xvfb smoke test, the arm64 runner and Flutter's arm64 Linux SDK, the tray and window on a real desktop, the icon and `StartupWMClass` matching under X11 and Wayland.
- The release job against a real tag.
