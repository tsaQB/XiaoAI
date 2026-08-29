#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

use crate::bot::client::TelegramBotClient;
use crate::bot::models::{InputRichMessage, RichBlock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    Active,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressActivity {
    Thinking,
    Looking,
    Reading,
    Searching,
    Fetching,
    Coding,
    Tool,
    Writing,
    Testing,
    Table,
    Listening,
    Drawing,
    Watching,
}

impl ProgressActivity {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Thinking => "Thinking",
            Self::Looking => "Looking",
            Self::Reading => "Reading",
            Self::Searching => "Searching",
            Self::Fetching => "Fetching",
            Self::Coding => "Coding",
            Self::Tool => "Tool",
            Self::Writing => "Writing",
            Self::Testing => "Testing",
            Self::Table => "Table",
            Self::Listening => "Listening",
            Self::Drawing => "Generating Image",
            Self::Watching => "Watching",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Thinking => "🧩",
            Self::Looking => "👀",
            Self::Reading => "📖",
            Self::Searching => "🔎",
            Self::Fetching => "🌐",
            Self::Coding => "</>",
            Self::Tool => "⚙️",
            Self::Writing => "🪶",
            Self::Testing => "🧪",
            Self::Table => "📑",
            Self::Listening => "🎧",
            Self::Drawing => "🫟",
            Self::Watching => "🎥",
        }
    }

    pub fn short_status(&self) -> String {
        format!("{} {}", self.icon(), self.display_name())
    }
}

#[derive(Debug, Clone)]
pub struct ProgressItem {
    pub label: String,
    pub activity: ProgressActivity,
    pub state: ProgressState,
    pub detail: Option<String>,
}

impl ProgressItem {
    pub fn format_line(&self) -> String {
        match self.state {
            ProgressState::Active => format!("{} {}", self.activity.icon(), self.label),
            ProgressState::Done => format!("✓ {}", self.label),
            ProgressState::Failed => format!("✗ {}", self.label),
        }
    }
}

pub fn classify_text_activity(text: &str) -> ProgressActivity {
    let t = text.to_lowercase();
    if ["look", "lihat", "gambar", "image", "photo", "foto", "vision", "visual", "logo"]
        .iter()
        .any(|k| t.contains(k))
    {
        return ProgressActivity::Looking;
    }
    if ["read", "baca", "dokumen", "document", "pdf", "file", "teks", "text"]
        .iter()
        .any(|k| t.contains(k))
    {
        return ProgressActivity::Reading;
    }
    if ["tabel", "table", "grid", "kolom", "baris", "matrix", "rekap", "jadwal", "klasemen"]
        .iter()
        .any(|k| t.contains(k))
    {
        return ProgressActivity::Table;
    }
    if ["test", "uji", "validat", "evaluat", "verif", "benchmark", "check"]
        .iter()
        .any(|k| t.contains(k))
    {
        return ProgressActivity::Testing;
    }
    if ["search", "cari", "find", "scan", "telusuri", "inspect", "query", "explore", "lookup"]
        .iter()
        .any(|k| t.contains(k))
    {
        return ProgressActivity::Searching;
    }
    if ["fetch", "request", "api", "endpoint", "download", "ambil", "connect", "hubungi", "retrieve", "http", "network", "sync"]
        .iter()
        .any(|k| t.contains(k))
    {
        return ProgressActivity::Fetching;
    }
    if [
        "code", "coding", "syntax", "hitung", "calculat", "math", "rumus", "run", "eksekusi", "compile",
        "algoritma", "derivat", "logic", "logika", "program", "fungsi", "function",
    ]
    .iter()
    .any(|k| t.contains(k))
    {
        return ProgressActivity::Coding;
    }
    if [
        "tool", "patch", "modifikasi", "update", "simpan", "apply", "terapkan", "perbaiki", "simulate",
        "branch", "exec", "database", "db",
    ]
    .iter()
    .any(|k| t.contains(k))
    {
        return ProgressActivity::Tool;
    }
    if [
        "write", "tulis", "format", "susun", "rangkum", "synthesize", "jelaskan", "draft", "compose",
        "render", "jawab",
    ]
    .iter()
    .any(|k| t.contains(k))
    {
        return ProgressActivity::Writing;
    }
    ProgressActivity::Thinking
}

pub fn generate_contextual_stages(prompt: &str, _model: &str) -> Vec<(&'static str, ProgressActivity)> {
    let p = prompt.to_lowercase();
    let mut stages = Vec::new();

    if ["tabel", "table", "jadwal", "rekap", "klasemen", "grid", "kolom", "baris"]
        .iter()
        .any(|k| p.contains(k))
    {
        stages.push(("Searching", ProgressActivity::Searching));
        stages.push(("Table", ProgressActivity::Table));
        stages.push(("Writing", ProgressActivity::Writing));
    } else if [
        "code", "coding", "python", "rust", "javascript", "html", "css", "sql", "query", "script",
        "function", "fungsi", "class", "api", "jwt", "auth", "endpoint", "bug", "error", "fix", "patch",
    ]
    .iter()
    .any(|k| p.contains(k))
    {
        stages.push(("Searching", ProgressActivity::Searching));
        stages.push(("Fetching", ProgressActivity::Fetching));
        stages.push(("Coding", ProgressActivity::Coding));
        if ["error", "bug", "fix", "debug", "masalah", "fail"].iter().any(|k| p.contains(k)) {
            stages.push(("Tool", ProgressActivity::Tool));
        } else {
            stages.push(("Testing", ProgressActivity::Testing));
        }
        stages.push(("Writing", ProgressActivity::Writing));
    } else if [
        "hitung", "berapa", "matematika", "fisika", "rumus", "integral", "turunan", "aljabar",
        "persamaan", "deret", "probabilitas", "kecepatan", "jarak", "energi", "kalkulus",
    ]
    .iter()
    .any(|k| p.contains(k))
    {
        stages.push(("Thinking", ProgressActivity::Thinking));
        stages.push(("Coding", ProgressActivity::Coding));
        stages.push(("Tool", ProgressActivity::Tool));
        stages.push(("Writing", ProgressActivity::Writing));
    } else if [
        "teka-teki", "logika", "puzzle", "riddle", "petani", "serigala", "kambing", "kotak", "apel",
        "jeruk", "strategi", "analisa lebih dalam", "analisis", "bandingkan",
    ]
    .iter()
    .any(|k| p.contains(k))
    {
        stages.push(("Thinking", ProgressActivity::Thinking));
        stages.push(("Tool", ProgressActivity::Tool));
        stages.push(("Testing", ProgressActivity::Testing));
        stages.push(("Writing", ProgressActivity::Writing));
    } else if [
        "sejarah", "kenapa", "mengapa", "apa itu", "jelaskan", "bagaimana", "definisi", "teori",
        "faktor", "penyebab", "prinsip",
    ]
    .iter()
    .any(|k| p.contains(k))
    {
        stages.push(("Searching", ProgressActivity::Searching));
        stages.push(("Fetching", ProgressActivity::Fetching));
        stages.push(("Thinking", ProgressActivity::Thinking));
        stages.push(("Writing", ProgressActivity::Writing));
    } else if [
        "terjemah", "translate", "inggris", "indonesia", "puisi", "cerita", "esai", "surat", "kalimat",
    ]
    .iter()
    .any(|k| p.contains(k))
    {
        stages.push(("Searching", ProgressActivity::Searching));
        stages.push(("Writing", ProgressActivity::Writing));
    } else {
        stages.push(("Thinking", ProgressActivity::Thinking));
        stages.push(("Searching", ProgressActivity::Searching));
        stages.push(("Writing", ProgressActivity::Writing));
    }

    stages
}

pub struct ExecutionTimeline {
    bot: TelegramBotClient,
    chat_id: i64,
    draft_id: i64,
    max_items: usize,
    items: Arc<RwLock<Vec<ProgressItem>>>,
    start_time: Instant,
    last_sync_time: Arc<RwLock<Instant>>,
    sync_lock: Arc<Mutex<()>>,
    stopped: Arc<AtomicBool>,
}

impl ExecutionTimeline {
    pub fn new(bot: TelegramBotClient, chat_id: i64, draft_id: i64, max_items: usize) -> Self {
        Self {
            bot,
            chat_id,
            draft_id,
            max_items,
            items: Arc::new(RwLock::new(Vec::new())),
            start_time: Instant::now(),
            last_sync_time: Arc::new(RwLock::new(Instant::now())),
            sync_lock: Arc::new(Mutex::new(())),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn add_action(&self, label: impl Into<String>, activity: Option<ProgressActivity>) {
        let lbl = label.into();
        let act = activity.unwrap_or_else(|| classify_text_activity(&lbl));

        let mut items = self.items.write().await;
        for it in items.iter_mut() {
            if it.state == ProgressState::Active {
                it.state = ProgressState::Done;
            }
        }

        items.push(ProgressItem {
            label: lbl,
            activity: act,
            state: ProgressState::Active,
            detail: None,
        });

        if items.len() > self.max_items {
            let drain_count = items.len() - self.max_items;
            items.drain(0..drain_count);
        }
    }

    pub async fn complete_current(&self, detail: Option<String>) {
        let mut items = self.items.write().await;
        for it in items.iter_mut().rev() {
            if it.state == ProgressState::Active {
                it.state = ProgressState::Done;
                if let Some(d) = detail {
                    it.detail = Some(d);
                }
                break;
            }
        }
    }

    pub async fn fail_current(&self, error: String) {
        self.stop_ticker();
        let mut items = self.items.write().await;
        for it in items.iter_mut().rev() {
            if it.state == ProgressState::Active {
                it.state = ProgressState::Failed;
                it.detail = Some(error);
                break;
            }
        }
    }

    pub async fn finish_all(&self, final_state: ProgressState) {
        self.stop_ticker();
        let mut items = self.items.write().await;
        for it in items.iter_mut() {
            if it.state == ProgressState::Active {
                it.state = final_state;
            }
        }
    }

    pub fn stop_ticker(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    pub fn start_ticker(self: &Arc<Self>) {
        self.stopped.store(false, Ordering::SeqCst);
        let timeline = Arc::clone(self);
        tokio::spawn(async move {
            while !timeline.stopped.load(Ordering::SeqCst) {
                tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                if timeline.stopped.load(Ordering::SeqCst) {
                    break;
                }
                timeline.sync_draft(false).await;
            }
        });
    }

    pub async fn render_current_status(&self) -> String {
        let elapsed = self.start_time.elapsed().as_secs();
        let items = self.items.read().await;
        let mut status_line = "🧩 Thinking".to_string();

        for it in items.iter().rev() {
            if it.state == ProgressState::Active {
                let lbl = if it.label.is_empty() {
                    it.activity.display_name()
                } else {
                    &it.label
                };
                status_line = if lbl.starts_with(it.activity.icon()) {
                    lbl.to_string()
                } else {
                    format!("{} {}", it.activity.icon(), lbl)
                };
                break;
            } else if it.state == ProgressState::Failed {
                status_line = format!("✗ {} Failed", it.activity.display_name());
                break;
            }
        }

        format!("{status_line}\n{elapsed}s")
    }

    pub async fn render_timeline_text(&self) -> String {
        let items = self.items.read().await;
        if items.is_empty() {
            return "🧩 Thinking...".to_string();
        }
        items.iter().map(|it| it.format_line()).collect::<Vec<_>>().join("\n")
    }

    pub async fn sync_draft(&self, force: bool) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }

        let now = Instant::now();
        {
            let last_sync = *self.last_sync_time.read().await;
            if !force && (now.duration_since(last_sync).as_millis() < 1200) {
                return;
            }
        }

        let Ok(_guard) = self.sync_lock.try_lock() else {
            return;
        };

        *self.last_sync_time.write().await = Instant::now();
        let status = self.render_current_status().await;

        let rich_message = InputRichMessage::new(vec![RichBlock::Thinking {
            text: status,
            collapsed: true,
            expandable: true,
        }]);

        if let Err(e) = self
            .bot
            .send_rich_message_draft(self.chat_id, self.draft_id, &rich_message, true, false)
            .await
        {
            debug!("Failed to sync draft update: {e}");
        }
    }
}
