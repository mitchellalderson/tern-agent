# tern

A personal AI agent shipped as one static native binary. See `DECISIONS.md` for implementation notes.

Status: **M3 (TUI)**: run `tern` for the chat UI (`/help` lists commands). Earlier milestones:: crash-safe step journal with resume, cancellation, context compaction, long-term memory and session search, and `tern runs` / `tern sessions` for inspection. Providers: Anthropic and OpenAI-compatible.

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
