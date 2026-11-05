# tern desktop

Flutter app for tern. It attaches to the daemon (`tern daemon`) over `~/.tern/tern.sock`; the daemon owns all state, so closing the app loses nothing. Design: [docs/gui-spec.md](../../docs/gui-spec.md).

## Run it

Needs Flutter 3.47+ (Dart 3.13) and, for macOS, full Xcode (the Command Line Tools alone can't build apps).

```sh
cargo build -p tern                 # from the repo root
apps/desktop/tool/demo_daemon.sh    # mock provider, throwaway home; prints TERN_HOME
cd apps/desktop
TERN_HOME=<printed path> flutter run -d macos
```

In the demo, any message runs a scripted write-and-read with a Markdown reply. A message containing "approve" asks to run a shell command, so you get an approval card. Against your real setup, start `tern daemon` (or press **Start tern** in the app) and run `flutter run -d macos` with no `TERN_HOME`.

The macOS app runs outside the App Sandbox (see `macos/Runner/*.entitlements`). It has to reach the socket under `~/.tern` and start `tern daemon`.

## What's in it (gui-spec G0–G2)

- Window: transparent title bar with content under the traffic lights, drag and double-click zoom through a `tern/window` method channel, native menu bar (⌘N new conversation, ⌘⌥I activity panel). Light theme, dark following the system.
- Connection: connect, reconnect with backoff (0.5 s doubling to 10 s), resync after every reconnect, an offline screen with **Start tern**.
- Sidebar: one agent ("tern") and its conversations grouped Today / This week / Older, with a source icon (cron, Telegram), search by title, running and waiting-for-approval markers, and daemon status.
- Conversation: streaming Markdown replies, work cards (one line per tool call, click for output), notes (routine created, memory saved, compaction, retries), approval cards (Allow once / Always / Deny, ⏎ and ⎋), stop, resume after a failure or interruption, and a per-conversation model picker (see Models below).
- Activity panel: the current run's steps, **routines** (each schedule in words, paused or failed state, and a menu for Run now, Edit, Pause/Resume and Delete; "New routine" opens a sheet with presets, a cron field with a live preview of the next three runs from the daemon, and Telegram delivery), token usage, available tools.
- Run inspector: a conversation's runs and each run's journal steps (model calls, tools, approvals, compaction) with input, output, tokens and timing. It opens from "Inspect run" on a work card, "Details" on a stopped or failed run, or "All runs…" in the panel.
- Models (⌘,, or the key button in the sidebar): every provider with its key (masked) and status, the default model, and per provider **Change API key**, **Choose default model**, **Refresh models** and **Remove**. **Add provider** starts from a preset (Anthropic, OpenAI, OpenRouter, Gemini, Groq, Together, Ollama, LM Studio, or another OpenAI-compatible address), then a key field with **Paste** and **Show** that is checked with the provider as soon as a key is pasted, then a searchable list of that provider's models. Secrets moved under **Secrets (advanced)…** and the tern menu.
- First run: with no usable model, the conversation area shows the same setup inline ("Connect a model to get started") and the sidebar says **Set up a model…**.
- Model chip: a searchable picker over every provider's models (fetched by the daemon and cached), with the current model first, a refresh button, **Also make it the default**, and a `Use "<typed>"` row for models the list doesn't have. It works before the first message too: the choice is sent with it.
- Notifications: when the window doesn't have focus, or you're in another conversation, a finished run and an approval each post a system notification. Approvals have **Allow once** and **Deny** buttons that answer straight from the notification. The dock badge counts pending approvals. On macOS this uses `UserNotifications` through the `tern/window` channel; on Linux, `notify-send` (no buttons).

Beyond G0–G2 the app also has: agents (a searchable list of 25 starters, edit, archive and delete, a per-agent autonomy ceiling and Telegram bot), channels (Home, topic channels, "Let agents decide"), the outbox and Tasks views, scoped approval choices (this run, this conversation, always), Connections (OAuth sign-in), MCP servers, bundles, the activity ledger and security audit, a config editor, and a first-launch choice of where tern runs (this Mac, or a paired tern server). The menu-bar icon is `tern tray`, shipped inside the app bundle.

Not yet: a skills view (reviewing skill-capture drafts is `tern skills`), a Telegram pairing screen with a QR code, a quick switcher, several saved servers, a Linux build, and notarized releases without the project's Apple credentials. The phases are in gui-spec §10; what's left is in the [roadmap](../../ROADMAP.md).

## Release build

`scripts/package-macos-app.sh <tern-binary> <out-dir>` builds the release app, puts the binary in `tern.app/Contents/Library/LoginItems/tern Tray.app/Contents/MacOS/tern-tray` (the menu-bar icon when run with no arguments, the CLI otherwise; **Start tern** prefers it over `PATH`), signs it and writes `tern-desktop-macos.dmg`. The release workflow passes a universal `tern`; with `MACOS_SIGN_IDENTITY` and the `APPLE_*` variables set it signs with a Developer ID and notarizes, otherwise it ad-hoc signs.

```sh
cargo build --release -p tern
scripts/package-macos-app.sh target/release/tern dist    # from the repo root
```

## Layout

- `packages/tern_client`: pure Dart client (models, SSE, `Endpoint.unix` / `Endpoint.tcp` with a bearer token). The TCP side is the seed of `TernTransport` in [deployment-spec.md](../../docs/deployment-spec.md) §7.2; the daemon can listen on TCP for paired devices (D1).
- `lib/state/transcript.dart`: session history plus live events reduced to display items. Pure and unit-tested.
- `lib/state/store.dart`: the `ChangeNotifier` the widgets listen to. Connection, resync and event routing.
- `lib/ui/`: widgets.

## Tests

```sh
cd apps/desktop
flutter analyze
flutter test                                              # unit, widgets, screenshots
TERN_BIN=../../target/debug/tern flutter test test/store_live_test.dart
cd packages/tern_client && dart test                      # add TERN_BIN=../../../../target/debug/tern for the live test
```

`test/screenshot_test.dart` renders the main window (light and dark) with the system fonts into `build/screenshots/`, so the design can be checked without launching the app.
