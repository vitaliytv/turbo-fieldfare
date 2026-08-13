# Rust phase 1

This workspace contains the first migration phase: an OpenAI-compatible Rust
HTTP server backed by deterministic mock inference. It does not load a model or
call the Swift/Metal runtime yet.

```bash
cd rust
cargo run -p turbofieldfare -- --port 8080
```

The server binds to loopback only and implements `GET /health`,
`GET /v1/models`, and `POST /v1/chat/completions` with JSON and SSE responses.
Use prompts containing `[tool]`, `[fail]`, or `[slow]` to exercise deterministic
mock tool-call, failure, or delayed-generation paths.
