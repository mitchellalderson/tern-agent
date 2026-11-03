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

## What's in the MVP (gui-spec G0–G1)

- Window: transparent title bar with content under the traffic lights, drag and double-click zoom through a `tern/window` method channel, native menu bar (⌘N new conversation, ⌘⌥I activity panel). Light theme, dark following the system.
- Connection: connect, reconnect with backoff (0.5 s doubling to 10 s), resync after every reconnect, an offline screen with **Start tern**.
- Sidebar: one agent ("tern") and its conversations grouped Today / This week / Older, with a source icon (cron, Telegram), search by title, running and waiting-for-approval markers, and daemon status.
- Conversation: streaming Markdown replies, work cards (one line per tool call, click for output), notes (routine created, memory saved, compaction, retries), approval cards (Allow once / Always / Deny, ⏎ and ⎋), stop, resume after a failure or interruption, and a per-conversation model picker validated by the daemon.
- Activity panel: the current run's steps, token usage, available tools.

Not yet: agent profiles, channels, connectors/MCP/skills/memory views, routines, run inspector, notifications, menu bar extra, packaging. Those need the engine APIs in gui-spec §7.1, and the phases are in §10.

## Layout

- `packages/tern_client`: pure Dart client (models, SSE, `Endpoint.unix` / `Endpoint.tcp` with a bearer token). The TCP side is the seed of `TernTransport` in [deployment-spec.md](../../docs/deployment-spec.md) §7.2. The daemon doesn't listen on TCP yet.
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
