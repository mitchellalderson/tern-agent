# tern

A personal AI agent shipped as one static native binary. See `DECISIONS.md` for implementation notes.

Status: **M1 (agent loop)**: Anthropic and OpenAI-compatible providers, the crash-safe step journal, built-in file/shell/http tools, `tern run`, `tern resume`.

```sh
cargo build --release
./target/release/tern config init
./target/release/tern config set default_model anthropic:claude-sonnet-4-5
./target/release/tern config set providers.anthropic.kind anthropic
./target/release/tern config set providers.anthropic.api_key_secret ANTHROPIC_API_KEY
./target/release/tern doctor
./target/release/tern run --yes "create hello.txt containing hi, then read it back"
```

Data lives in `~/.tern` (override with `TERN_HOME`).
