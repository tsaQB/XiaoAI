# Changelog

## Unreleased — Post-merge P1/P2/P3 hardening

### Data integrity
- Make session/provider mutations durable-first and add SQLite transaction boundaries for clear/remove/active-session transitions.
- Add durable per-session revisions and conditional generation append so `/clear`, delete, and session switching cannot be undone or redirected by late completions.
- Keep attachment cleanup after the corresponding durable session transaction commits.

### Security & reliability
- Make new and rehydrated multimodal inputs fail closed unless the active capability record explicitly reports support.
- Move provider API keys and Telegram bot token out of ordinary plaintext configuration rows into a separately permissioned local SecretStore referenced by `secret://...`; migrate legacy plaintext only after the new secret commits.
- Retain Telegram inbox payloads through claim and recover abandoned `processing` rows, documenting the resulting at-least-once semantics instead of exactly-once.
- Add absolute visible/reasoning/wire SSE ceilings and prevent bounded/truncated streams from becoming normal canonical history.
- Propagate CLI/provider persistence failures instead of reporting false success.

### Rendering & Telegram
- Split streaming Markdown into stable native-Rich content plus a sanitized provisional tail so incomplete delimiters do not flash raw syntax.
- Enforce local Rich Message structural budgets and degrade deterministically when a payload exceeds them.
- Make permanent fallback canonical: Rich AST → safe HTML → AST-derived semantic plain text; never raw model Markdown.

### Governance & validation
- Add CODEOWNERS and document the owner-side branch-protection/ruleset settings required for `master`.
- Keep Actions least-privilege, SHA-pinned, and checkout credentials disabled; add an explicit host `cargo build --release --locked` gate.

## 0.2.0 — Hardening & Telegram Bot API 10.3

### Security
- Require a single `OWNER_USER_ID` and optionally restrict owner usage to `ALLOWED_CHAT_IDS`.
- Stop logging Telegram file URLs that contain the bot token.
- Add bounded Telegram media downloads and bounded text-document ingestion.
- Make external Pollinations image fallback opt-in.
- Escape dynamic values before HTML fallback/UI rendering.
- Reject unsupported binary documents instead of lossy UTF-8 interpretation.

### Session & reliability
- Replace vector-index session identity with stable persistent session IDs.
- Add persistent high-water session ID allocation so deleted IDs are not reused.
- Migrate legacy active-session indexes to stable IDs.
- Delete sessions/messages from SQLite rather than hiding them only in memory.
- Bind generation writes to the request's original session and discard late writes for deleted sessions.
- Serialize owner generation requests to prevent history reorder races.
- Convert normal message persistence to append-style writes.
- Remove unsafe UTF-8 byte truncation/prefix slicing in user/provider-facing paths.
- Mark interrupted provider streams as partial instead of silently treating them as complete.

### Telegram Bot API 10.3
- Add native generation-stop flow using `can_stop`, `keep_on_stop`, and `stopped_message_generation`.
- Add cancellation registry keyed by chat/draft ID.
- Add disabled inline buttons and Rich Message buttons.
- Add expandable quotation, document block, compact table, `force_reply`, and ephemeral parameter models used by XiaoAI.
- Stream partial answer text into Telegram drafts.
- Update runtime/version wording to Bot API 10.3 without claiming full API coverage.

### Quality
- Add regression tests for Unicode helpers/parser behavior, Telegram 10.3 serialization, legacy session migration, and session-ID high-water allocation.
- Add CI gates for `cargo fmt`, `cargo check`, `cargo test`, and `cargo clippy` before ARM64/Android builds.
- Pin GitHub Actions to full commit SHAs and disable persisted checkout credentials.

### Final P1 closure
- Split CLI, provider, persistence, capability, HTTP retry, SSE decoding, document extraction, and attachment persistence into dedicated modules.
- Route bot-time SQLite work through `spawn_blocking` wrappers and make provider state explicitly single-owner/global.
- Add tri-state provider capability records and active probes for text, vision, tools, and structured output.
- Add bounded PDF/DOCX/XLSX extractors plus scanned-PDF render-to-vision fallback.
- Persist and rehydrate multimodal session attachments with bounded sizes and restrictive Unix permissions.
- Replace unbounded per-update spawning with a bounded ordered queue while keeping native Stop immediate.
- Add context-aware history budgeting and transient provider retry/backoff with `Retry-After` support.
- Add tested SSE decoding for CRLF, split chunks, comments, multi-line data, and `[DONE]`.
