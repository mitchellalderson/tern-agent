# tern

A personal AI agent shipped as one static native binary. See `DECISIONS.md` for implementation notes.

Status: **M0 (skeleton)**: config, data directory, SQLite schema, `tern doctor`, `tern config`.

```sh
cargo build --release
./target/release/tern config init
./target/release/tern doctor
```

Data lives in `~/.tern` (override with `TERN_HOME`).
