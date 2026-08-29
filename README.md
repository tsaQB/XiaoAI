# 🤖 XiaoAI - Telegram Bot API 10.2 (Rust Single Binary)

**XiaoAI** adalah asisten AI Telegram berkinerja tinggi yang dibangun dengan **Rust** (single binary mandiri) dan mendukung penuh spesifikasi serta fitur terbaru **Telegram Bot API 10.2**.

---

## 🌟 Fitur Unggulan Telegram Bot API 10.2

### 1. **Rich Messages (`InputRichMessage` & Block Formatting)**
Mendukung penyusunan pesan berbasis blok terstruktur native Telegram:
*   `InputRichBlockThinking` (Blok proses berpikir/penalaran AI yang expandable)
*   `InputRichBlockSectionHeading` (Judul & Subjudul level 1-6)
*   `InputRichBlockParagraph` (Paragraf teks)
*   `InputRichBlockList` & `InputRichBlockListItem` (Daftar item ordered / unordered)
*   `InputRichBlockBlockQuotation` & `InputRichBlockPullQuotation` (Kutipan blok & pull quote)
*   `InputRichBlockPreformatted` (Blok kode dengan syntax highlighting)
*   `InputRichBlockMathematicalExpression` (Formula matematika LaTeX / ekspresi)
*   `InputRichBlockTable` & `RichBlockTableCell` (Tabel terstruktur native Telegram)
*   `InputRichBlockDivider` (Garis pemisah horizontal)
*   `InputRichBlockFooter` (Catatan kaki / footer pesan)
*   `InputRichBlockAnchor` (Anchor link dalam pesan)

### 2. **Real-time Live Execution Timeline Engine**
*   Streaming tahapan proses komputasi AI secara langsung via `sendRichMessageDraft`.
*   Animasi status aktif (💭 Thinking, 🔎 Searching, 🌐 Fetching, 💻 Coding, ⚙️ Tool, 🧪 Testing, 📑 Table, 🪶 Writing, 🫟 Drawing, 🎥 Watching) dengan elapsed seconds timer real-time.
*   Finalisasi mulus digantikan oleh pesan permanen `sendRichMessage`.

### 3. **OpenAI-Compatible Custom Provider Manager**
*   Manajemen provider melalui CLI `xiao provider` untuk menghubungkan berbagai endpoint AI (OpenAI, OpenRouter, Groq, Cliproxy, Ollama, vLLM).
*   Deteksi model otomatis langsung dari endpoint `/models` dengan 1-click model switcher.

### 4. **Multimodal AI Support & Image Generator**
*   Mendukung analisis gambar (Vision OCR), dokumen teks, video vision, dan pesan suara (Whisper STT / Native Audio).
*   Pembuatan gambar resolusi tinggi (FLUX.1 via Pollinations & endpoint provider).

---

## 📁 Struktur Direktori

```text
XiaoAI/
├── Cargo.toml            # Manifest & dependensi Rust
├── src/
│   ├── main.rs           # Entry point CLI (xiao start, setup, provider, model, status)
│   ├── bot/
│   │   ├── mod.rs        # Modul bot Telegram
│   │   ├── client.rs     # Asynchronous Telegram Bot API 10.2 HTTP Client
│   │   └── models.rs     # Data models & serialisasi AST Bot API 10.2
│   ├── ai/
│   │   ├── mod.rs        # Modul integrasi AI
│   │   └── service.rs    # OpenAI-compatible engine, streaming SSE & multi-session
│   ├── parser.rs         # Markdown to Bot API 10.2 Rich Message AST Parser
│   └── timeline.rs       # State machine Live Execution Timeline
├── .env                  # Konfigurasi Token Bot & AI Endpoint
├── .env.example          # Template environment
└── README.md             # Dokumentasi lengkap
```

---

## 🚀 Perintah CLI `xiao`

```bash
xiao start                               # Run Telegram bot
xiao setup                               # Quickstart setup wizard
xiao provider [add] [del] [status]       # Manage AI providers
xiao telegram [check] [bind] [change]    # Manage Telegram bot token
xiao model [query]                       # Select or search model
xiao status                              # System health check
xiao help                                # Show this help
```

---

## 🛠️ Build & Instalasi Global `xiao`
```bash
# 1. Kompilasi binary release
cargo build --release

# 2. Salin binary ke PATH agar bisa dipanggil langsung dari mana saja
cp target/release/xiao $PREFIX/bin/xiao
chmod +x $PREFIX/bin/xiao
```

Setelah disalin ke `$PREFIX/bin/` (atau `/usr/local/bin/`), Anda dapat langsung menjalankan perintah `xiao` dari folder mana pun tanpa path lokal.

---

## 💬 Daftar Perintah Bot Telegram

| Perintah / Aksi | Deskripsi |
| :--- | :--- |
| *Kirim Pesan / File* | Mengobrol dengan AI (Vision, Dokumen, Voice Note, Video) |
| `/start` | Membuka menu sambutan & status model aktif |
| `/menu` | Menampilkan menu navigasi utama interaktif |
| `/model [keyword]` | Mengganti / mencari model AI aktif langsung dari chat |
| `/image` | Menghasilkan gambar AI dari prompt teks |
| `/context` | Monitor kapasitas token memori & kapabilitas model |
| `/session` | Membuka Session Manager & tabel sesi aktif |
| `/new` | Membuat sesi percakapan baru |
| `/clear` | Mereset memori riwayat sesi saat ini |
| `/help` | Menampilkan panduan dan daftar seluruh perintah |
