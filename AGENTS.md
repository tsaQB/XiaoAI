# XiaoAI 0.2.0 — Rust Telegram AI Assistant

XiaoAI is a standalone asynchronous Rust application using the **Telegram Bot API 10.3 subset required by XiaoAI**, Rich Message AST formatting, cancellable streaming drafts, SQLite sessions, and multi-provider OpenAI-compatible AI routing. Do not describe the client as a full Bot API implementation.

## Required Quality Gates

Run before declaring a change ready:

- `cargo fmt --all -- --check`
- `cargo check --locked`
- `cargo test --locked`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo audit --deny warnings`
- `cargo build --release --locked`

## Essential CLI Commands

- `xiao start`
- `xiao setup`
- `xiao provider [add] [del] [status]`
- `xiao telegram [check] [bind] [change]`
- `xiao telegram owner <telegram_user_id>`
- `xiao model [query]`
- `xiao model probe`
- `xiao model pick`
- `xiao status`

## Security & Session Invariants

1. `OWNER_USER_ID` is required. Only that Telegram user may operate XiaoAI; `ALLOWED_CHAT_IDS` extends where the owner may use the bot, not who may use it.
2. Never log a URL that contains `BOT_TOKEN`.
3. Session identity is the stable SQLite `session_id`, never a vector/list index.
4. New session IDs must come from the persistent high-water sequence and must not reuse deleted IDs.
5. Every AI request captures its originating `session_id`; late output must never be written into whichever session is active later.
6. Every generation captures the originating session revision. Clear/reset increments it durably; append must compare the captured revision in SQLite before writing.
7. If the originating session is deleted while generation is running, discard the late persistence write. Switching active session must never redirect the late output.
8. Public persistence mutations are durable-first: commit SQLite before publishing RAM; delete attachments/secrets only after durable commit.
9. User generations are serialized so concurrent prompts cannot reorder the same owner's history.
10. Telegram `stopped_message_generation` must cancel the matching `(chat_id, draft_id)` provider stream.
11. Binary documents must use explicit bounded extractors. Never reinterpret PDF/DOCX/XLSX bytes as UTF-8; scanned PDFs must use the render-to-vision path.
12. Multimodal capability is fail-closed: only explicit `Some(true)` authorizes media routing. Unknown/missing records must be probed safely or rejected.
13. Provider API keys and `BOT_TOKEN` must persist through the SecretStore reference abstraction, never as ordinary plaintext config values. Do not claim the local file SecretStore is encrypted or equivalent to an OS keyring.
14. External image fallback is opt-in only (`IMAGE_FALLBACK_PROVIDER=pollinations`).

## Architecture

```text
src/
├── main.rs         # access policy, durable inbox routing, control/generation lanes
├── cli.rs          # terminal setup/provider/model/Telegram commands
├── document.rs     # bounded PDF/DOCX/XLSX extraction + scanned-PDF rendering
├── attachments.rs  # per-session multimodal persistence
├── util.rs         # Unicode-safe truncation and HTML escaping
├── bot/
│   ├── client.rs   # Telegram HTTP client + 10.3 methods used by XiaoAI
│   └── models.rs   # Telegram/Rich Message serde models
├── ai/
│   ├── provider.rs # single-owner provider state + capability probes
│   ├── storage.rs  # SQLite persistence / blocking boundary
│   ├── stream.rs   # SSE state machine
│   ├── http.rs     # retry/backoff policy
│   └── service.rs  # chat/session orchestration, STT/image
├── parser.rs       # Markdown -> Telegram Rich Message AST
└── timeline.rs     # Cancellable/streaming draft presentation state
```

## Telegram 10.3 Notes

The currently modeled 10.3 surface includes native generation stop (`can_stop`, `keep_on_stop`, `stopped_message_generation`), disabled buttons, Rich Message buttons, expandable quotations, document blocks, compact tables, `force_reply`, and `EphemeralMessageParameters` where used by the client.

Execution/status UI must describe observable application state. Do not infer fake tool execution such as “Searching”, “Testing”, or “Coding” solely from prompt/reasoning keywords.

## Persistence Notes

SQLite is the runtime source of truth for sessions, active session identity, non-secret settings, provider/model selections, and migration markers. Secret settings persist as `secret://...` references whose material is stored separately by the local SecretStore. Legacy JSON/.env/plaintext rows may be imported for compatibility only after the replacement secret is durably written; do not delete the legacy value first.

## Runtime Boundaries

Bot-time SQLite work must go through the async wrappers in `ai/storage.rs` so blocking `rusqlite` calls do not run on Tokio workers. Provider configuration is single-owner global state; do not reintroduce pseudo-multi-user provider maps. Capability claims must remain tri-state and identify metadata/probe evidence.

Telegram ingestion is a durable inbox. After intake, updates are classified into a responsive **control lane** and serialized **generation lane**; native Stop bypasses both for immediate cancellation. Inbox delivery semantics are explicitly **at-least-once**: retain a claimed payload until the completed checkpoint and recover abandoned `processing` rows after restart. Do not document this as exactly-once.

Streaming drafts must render stable Markdown through native Rich blocks while sanitizing incomplete provisional syntax. The permanent final is emitted once from the canonical AST. Final fallback is Rich → safe HTML → AST-derived semantic plain text, never raw model Markdown.
