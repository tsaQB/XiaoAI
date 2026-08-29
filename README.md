# XiaoAI

XiaoAI adalah bot Telegram asynchronous berbasis Rust untuk endpoint AI yang kompatibel dengan OpenAI. Aplikasi ini menangani provider/model, session percakapan, streaming timeline, rich message Telegram, gambar, dokumen, audio, dan video dari satu binary.

## Fitur Saat Ini

- Rich message Telegram menggunakan `sendRichMessage` dan `editMessageText` dengan `rich_message`; jika endpoint rich message menolak request, XiaoAI memakai fallback HTML/monospace.
- Parser Markdown membentuk blok heading, paragraf, list, quote, code block, math, divider, dan tabel native Telegram.
- Session manager menampilkan maksimal lima session per halaman, dengan tombol pilih session, pagination saat diperlukan, `Delete`, `Rename`, `New`, dan `Close`.
- Menu `context` menunjukkan capability model serta penggunaan konteks dengan progress bar monospace.
- Provider OpenAI-compatible dikelola dari CLI dan katalog model dapat diambil dari endpoint `/models`.
- Whitelist model Telegram dikelola dari `xiao model pick`; maksimal 10 model, dibedakan berdasarkan provider/alias, dan hanya daftar tersebut yang muncul pada menu model Telegram.
- Pesan suara ditranskripsi melalui endpoint `/audio/transcriptions` bila provider mendukungnya. Gambar dikirim sebagai data URL ke input vision.
- Video diteruskan sebagai data URL `video/*` ke input `image_url`. Ini hanya bekerja pada endpoint yang secara eksplisit menerima video pada format tersebut; banyak endpoint OpenAI-compatible menolaknya.
- Pembuatan gambar memakai endpoint/provider yang tersedia pada konfigurasi.

## Batasan Multimedia

- Capability berdasarkan nama model bersifat indikatif; capability aktual ditentukan oleh endpoint provider.
- Endpoint yang tidak mendukung `input_audio` akan menolak audio. Gunakan provider dengan STT atau transkripsikan audio terlebih dahulu.
- Endpoint yang tidak menerima `video/*` dalam `image_url` akan menolak video. Ekstraksi frame/audio dengan FFmpeg sebagai fallback hybrid belum diimplementasikan.

---

## 📁 Struktur Direktori

```text
XiaoAI/
├── Cargo.toml            # Manifest & dependensi Rust
├── src/
│   ├── main.rs           # Entry point CLI, bot handlers, keyboard dan UI builders
│   ├── bot/
│   │   ├── mod.rs        # Modul bot Telegram
│   │   ├── client.rs     # Asynchronous Telegram Bot API 10.2 HTTP Client
│   │   └── models.rs     # Data models & serialisasi AST Bot API 10.2
│   ├── ai/
│   │   ├── mod.rs        # Modul integrasi AI
│   │   └── service.rs    # Provider, session, SSE chat, STT dan image generation
│   ├── parser.rs         # Markdown ke Rich Message AST
│   └── timeline.rs       # Streaming execution timeline
├── .env                  # Token bot dan konfigurasi endpoint lokal (jangan commit)
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
xiao model [name]                        # Select/search model dari CLI
xiao model pick                          # Pilih maksimal 10 model untuk Telegram
xiao model [name] pick                   # Alias untuk membuka model whitelist picker
xiao status                              # System health check
xiao help                                # Show this help
```

`xiao model pick` menyimpan whitelist ke `~/.xiao_providers.json` sebagai pasangan provider dan model. Tekan `Space` untuk toggle pilihan, ketik untuk filter, dan `Enter` atau `Esc` untuk menyimpan.

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

## Build Otomatis

Workflow GitHub Actions `.github/workflows/build.yml` membuat artifact release untuk:

- `xiao-linux-arm64-armbian`: Linux ARM64 GNU untuk Armbian/STB ARM64.
- `xiao-android-arm64`: Android ARM64 untuk Termux.

Workflow berjalan saat push, pull request, atau dapat dijalankan manual dari tab **Actions**. Artifact tersedia pada halaman workflow run yang selesai.

---

## 💬 Daftar Perintah Bot Telegram

| Perintah / Aksi | Deskripsi |
| :--- | :--- |
| *Kirim Pesan / File* | Mengobrol dengan AI, gambar, dokumen, audio, atau video sesuai dukungan endpoint |
| `/start` | Membuka menu sambutan & status model aktif |
| `/menu` | Menampilkan menu navigasi utama interaktif |
| `/model` | Mengganti model dari whitelist yang dikonfigurasi dengan `xiao model pick` |
| `/image` | Menghasilkan gambar AI dari prompt teks |
| `/context` | Monitor kapasitas token memori & kapabilitas model |
| `/session` | Membuka Session Manager & tabel sesi aktif |
| `/new` | Membuat sesi percakapan baru |
| `/clear` | Mereset memori riwayat sesi saat ini |
| `/help` | Menampilkan panduan dan daftar seluruh perintah |
