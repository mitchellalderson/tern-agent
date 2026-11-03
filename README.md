# tern

A personal AI agent shipped as one static native binary. See `DECISIONS.md` for implementation notes.

Status: **M5 (daemon)**: `tern daemon` runs the scheduler, a Telegram gateway (pair with `tern gateway pair`), and a local API on `~/.tern/tern.sock` that `tern` attaches to; runs survive daemon restarts and still deliver. Earlier, **M4 (extensibility)**: MCP servers (stdio and HTTP), sandboxed Luau connectors (including ones the agent writes, tests, and activates with your approval), and agentskills-format skills. Earlier milestones: a chat UI (`tern`; `/help` lists commands);: crash-safe step journal with resume, cancellation, context compaction, long-term memory and session search, and `tern runs` / `tern sessions` for inspection. Providers: Anthropic and OpenAI-compatible.

```sh
cargo build --release
./target/release/tern config init
./target/release/tern config set default_model anthropic:claude-sonnet-4-5
./target/release/tern config set providers.anthropic.kind anthropic
./target/release/tern config set providers.anthropic.api_key_secret ANTHROPIC_API_KEY
./target/release/tern doctor
./target/release/tern run --yes "create hello.txt containing hi, then read it back"
./target/release/tern            # chat UI
```

Data lives in `~/.tern` (override with `TERN_HOME`).
