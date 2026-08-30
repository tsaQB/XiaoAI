# XiaoAI 0.2.0

XiaoAI adalah bot Telegram asynchronous berbasis Rust untuk endpoint AI yang kompatibel dengan OpenAI. Versi 0.2.0 berfokus pada **session/data-integrity hardening, owner-only security, streaming yang dapat dibatalkan, serta integrasi fitur Telegram Bot API 10.3 yang dipakai XiaoAI**.

> XiaoAI tidak mengklaim mengimplementasikan seluruh Telegram Bot API. Client hanya memodelkan method dan update yang dibutuhkan aplikasi.

## Fitur Utama

- Rich Message Telegram dengan AST heading, paragraph, list, quote, code, math, divider, table, buttons, document block, dan expandable quotation.
- Streaming draft jawaban AI melalui `sendRichMessageDraft`, termasuk partial answer selama provider masih menghasilkan respons.
- Native generation stop Bot API 10.3 di private chat owner: draft memakai `can_stop`/`keep_on_stop`, lalu update `stopped_message_generation` membatalkan stream provider aktif. Di group, Stop tidak diaktifkan karena event stop tidak membawa identitas user penekan tombol.
- Native disabled inline button dan Rich Message buttons Bot API 10.3.
- Session persisten di SQLite dengan stable `session_id`, active-session migration, monotonic ID allocation, delete nyata, dan late-result protection.
- Update normal diproses oleh bounded ordered queue dan generation owner diserialisasi; native Stop diproses langsung agar cancellation tetap responsif.
- Provider OpenAI-compatible, discovery model `/models`, STT `/audio/transcriptions`, SSE chat completion, dan image generation.
- Owner-only authorization melalui `OWNER_USER_ID`; `ALLOWED_CHAT_IDS` hanya mengatur chat tambahan tempat owner boleh menggunakan XiaoAI.
- Batas download media dan fallback image eksternal yang default-nya nonaktif.
- Unicode-safe truncation/prefix handling serta HTML escaping untuk data dinamis yang masuk ke `parse_mode=HTML`.

## Telegram Bot API 10.3 yang Digunakan

XiaoAI 0.2.0 memakai subset 10.3 yang relevan untuk UI/AI flow:

- `can_stop` dan `keep_on_stop` pada streaming draft.
- update `stopped_message_generation` / `MessageGenerationStopped`.
- `DisabledButton` pada inline keyboard.
- `force_reply` pada keyboard markup yang dimodelkan XiaoAI.
- `RichMessageButton`, `RichBlockButtons`, dan style button.
- `RichBlockExpandableBlockQuotation`.
- `RichBlockDocument`.
- `is_compact` pada Rich Table.
- `EphemeralMessageParameters` pada method yang dipakai client.

## Security & Data Integrity 0.2.0

### Owner wajib

`OWNER_USER_ID` adalah hard invariant. XiaoAI menolak start jika owner belum dikonfigurasi.

```bash
xiao telegram owner 123456789
# atau
xiao setup
```

Secara default owner hanya dapat menggunakan bot di private chat miliknya. Tambahkan group/chat ID secara eksplisit melalui `ALLOWED_CHAT_IDS` bila diperlukan.

### Session identity

Session tidak lagi menggunakan posisi vector sebagai identitas. `session_id` stabil disimpan di SQLite, ID baru berasal dari persistent high-water counter, session yang dihapus benar-benar dihapus dari DB, dan hasil generation yang selesai terlambat tidak boleh berpindah ke session lain.

### Credential/logging

URL Telegram yang mengandung bot token tidak dicetak pada error log. Hindari menjalankan binary dengan logging HTTP library yang sangat verbose bila log akan dibagikan ke pihak lain.

### External image fallback

Fallback ke layanan image eksternal nonaktif secara default:

```dotenv
IMAGE_FALLBACK_PROVIDER=none
```

Untuk opt-in Pollinations:

```dotenv
IMAGE_FALLBACK_PROVIDER=pollinations
```

Mengaktifkannya berarti prompt image dapat dikirim ke provider eksternal tersebut bila provider aktif gagal/tidak mendukung image generation.

## Multimedia & Document Boundaries

- Image dikirim sebagai data URL ke input vision bila endpoint mendukungnya.
- Audio/voice dapat ditranskripsi melalui `/audio/transcriptions` bila provider mendukung STT.
- Video dapat diteruskan sebagai data URL `video/*` melalui jalur kompatibilitas saat endpoint memang menerima format tersebut.
- Dokumen text-like (`text/*`, Markdown, JSON, CSV, XML, source code) dibaca sebagai UTF-8 dengan batas ukuran.
- PDF text-native diekstrak lokal; DOCX dan XLSX diekstrak dari container XML dengan batas entry/worksheet untuk mencegah resource exhaustion.
- PDF scan/image-only dirender maksimal 6 halaman melalui `pdftoppm` dan dianalisis oleh vision model. Pada Linux, instal `poppler-utils` untuk jalur ini.
- Attachment image/audio/video dan halaman PDF scan dipersist per-session dengan permission ketat, lalu dapat direhidrasi pada turn berikutnya sesuai capability model dan budget context.
- Capability memakai state `Supported / Unsupported / Unknown`: `/models` metadata digabung dengan probe aman untuk text, vision, tools, dan structured output. Unknown tidak dipromosikan menjadi dukungan terverifikasi.

## Struktur Direktori

```text
XiaoAI/
├── Cargo.toml
├── src/
│   ├── main.rs           # access policy, ordered update router, Telegram UI handlers
│   ├── cli.rs            # terminal setup/provider/model/Telegram commands
│   ├── document.rs       # text/PDF/DOCX/XLSX + scanned-PDF render pipeline
│   ├── attachments.rs    # bounded per-session multimodal persistence
│   ├── util.rs           # Unicode/HTML safety helpers
│   ├── bot/
│   │   ├── mod.rs
│   │   ├── client.rs     # Telegram HTTP client + Bot API 10.3 surface used by XiaoAI
│   │   └── models.rs     # Telegram/Rich Message serde models
│   ├── ai/
│   │   ├── capability.rs # capability heuristics/metadata projection
│   │   ├── provider.rs   # single-owner provider registry + active probes
│   │   ├── storage.rs    # SQLite persistence + spawn_blocking boundary
│   │   ├── stream.rs     # tested SSE state machine
│   │   ├── http.rs       # retry/backoff policy
│   │   └── service.rs    # chat/session orchestration, STT, image generation
│   ├── parser.rs         # Markdown -> Rich Message AST
│   └── timeline.rs       # Draft streaming state/timeline
├── .github/workflows/build.yml
├── .env.example
└── CHANGELOG.md
```

## Perintah CLI

```bash
xiao start
xiao setup
xiao provider [add] [del] [status]
xiao telegram [check] [bind] [change]
xiao telegram owner <telegram_user_id>
xiao model [name]
xiao model probe
xiao model pick
xiao status
xiao help
```

`xiao model pick` mengelola whitelist model Telegram di SQLite. File JSON legacy hanya digunakan sebagai sumber migrasi kompatibilitas bila masih ditemukan. Environment/.env dapat menjadi bootstrap input, tetapi SQLite adalah runtime source of truth.

## Konfigurasi Environment

Salin `.env.example` menjadi `.env`:

```dotenv
BOT_TOKEN=123456:telegram-bot-token
OWNER_USER_ID=123456789
ALLOWED_CHAT_IDS=
AI_ENDPOINT=https://provider.example/v1
AI_API_KEY=provider-api-key
AI_MODEL=model-name
IMAGE_FALLBACK_PROVIDER=none
```

`AI_ENDPOINT` dan `AI_API_KEY` bersifat generik untuk endpoint OpenAI-compatible.

## Build & Validation

```bash
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets --all-features
cargo build --release --locked
```

GitHub Actions menjalankan quality gates tersebut sebelum build Linux ARM64 dan Android ARM64. Action pihak ketiga dipin ke commit SHA dan checkout tidak menyimpan credential repository.

Untuk instalasi lokal:

```bash
cargo build --release --locked
cp target/release/xiao "$PREFIX/bin/xiao"
chmod +x "$PREFIX/bin/xiao"
```

## Perintah Bot Telegram

| Perintah / aksi | Deskripsi |
| --- | --- |
| Kirim pesan | Chat dengan model aktif |
| Kirim image/audio/video/text document | Multimodal sesuai dukungan provider dan batas XiaoAI |
| `/start` | Menu sambutan dan status model |
| `/menu` | Menu navigasi |
| `/model` | Pilih model dari whitelist |
| `/image` | Generate image |
| `/context` | Estimasi context dan capability |
| `/session` | Session manager |
| `/new` | Session baru |
| `/clear` | Hapus history session aktif |
| `/help` | Bantuan |

## Reliability Notes

- Runtime SQLite operations invoked by the bot are routed through `spawn_blocking`; synchronous helpers remain for startup/CLI compatibility only.
- Provider retry policy covers transient connect/timeout, HTTP 408/429/502/503/504, and honors `Retry-After`. Mid-stream interruption is preserved as a partial result rather than retried blindly.
- Media is still encoded as base64 when an OpenAI-compatible JSON payload requires it, but downloads/persistence are bounded and normal updates are serialized, preventing unbounded concurrent memory growth.
- Context sizing remains an estimate because tokenizer behavior differs by provider, but history selection is now context-budget-aware and reserves output/system headroom.
