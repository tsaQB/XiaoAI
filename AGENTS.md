# XiaoAI - Telegram Bot API 10.2 Showcase & Live Execution Engine (Rust Edition)

This repository is a high-performance, standalone asynchronous Rust application (**XiaoAI**) implementing the full **Telegram Bot API 10.2** specification, rich message AST formatting, streaming execution timelines, and multi-provider AI routing.

## Essential CLI Commands

- `cargo check`: Check code
- `cargo build --release`: Build release binary
- `cp target/release/xiao $PREFIX/bin/xiao`: Install binary to PATH
- `xiao start`: Run Telegram bot
- `xiao setup`: Quickstart setup wizard
- `xiao provider [add] [del] [status]`: Manage AI providers
- `xiao telegram [check] [bind] [change]`: Manage Telegram bot token
- `xiao model [query]`: Select or search model
- `xiao status`: System health check
- `xiao help`: Show this help

## Architecture & Code Organization

```text
src/
├── main.rs         # Application entry point, update router, interactive wizards, keyboard builders
├── bot/
│   ├── mod.rs      # Bot module exports
│   ├── client.rs   # TelegramBotClient wrapping reqwest (supports Bot API 10.2 methods + HTML fallback)
│   └── models.rs   # Strongly-typed serde structures for Bot API 10.2 (InputRichMessage, Keyboards, Updates)
├── ai/
│   ├── mod.rs      # AI module exports
│   └── service.rs  # AIChatService: OpenAI-compatible SSE streaming, Whisper STT, FLUX.1 image generator, session management
├── parser.rs       # Markdown to Telegram Bot API 10.2 Rich Message AST Parser (supports tables, code, math, quotes)
└── timeline.rs     # Live Execution Timeline engine streaming rich drafts with elapsed timer & state machine
```

## Non-Obvious Patterns & Gotchas

1. **Auto-Chunking & HTML Fallbacks**:
   - `TelegramBotClient::send_message` automatically splits texts exceeding 4000 characters cleanly across paragraph breaks (`\n\n`) and lines.
   - `TelegramBotClient::send_rich_message` tries the native Telegram `sendRichMessage` method first; if unsupported by the client endpoint, it gracefully falls back to chunked rich HTML and rendered Unicode box tables (`<pre>`).

2. **Live Execution Timeline**:
   - Updates during AI processing are pushed as lightweight drafts via `send_rich_message_draft`.
   - Rate limiting is enforced natively with a mutex and min-sync interval (1200ms) to prevent Telegram API 429 errors.
   - The final response replaces the draft using `send_rich_message`.

3. **Markdown AST Parser (`parser.rs`)**:
   - Bypasses raw markdown transmission. Markdown headings, tables (Markdown, ASCII, Unicode Box), code blocks, math formulas (`$$` or `\[`), blockquotes, and lists are converted into structured `RichBlock` AST objects.
   - Leaked HTML tags are sanitized before parsing.

4. **Multi-Session & Provider Engine (`ai/service.rs`)**:
   - All state is managed thread-safely in-memory using `Arc<RwLock<...>>` structures.
   - Custom provider discovery calls `/models` on user-specified endpoints to dynamically populate available AI models.
