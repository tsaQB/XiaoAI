#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

use crate::bot::client::TelegramBotClient;
use crate::bot::models::{InputRichMessage, RichBlock};
use crate::parser::parse_markdown_to_rich_blocks;
use serde_json::Value;

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

pub struct ExecutionTimeline {
    bot: TelegramBotClient,
    chat_id: i64,
    draft_id: i64,
    max_items: usize,
    draft_enabled: bool,
    can_stop: bool,
    items: Arc<RwLock<Vec<ProgressItem>>>,
    start_time: Instant,
    last_sync_time: Arc<RwLock<Instant>>,
    sync_lock: Arc<Mutex<()>>,
    stopped: Arc<AtomicBool>,
    partial_answer: Arc<RwLock<String>>,
}

impl ExecutionTimeline {
    pub fn new(
        bot: TelegramBotClient,
        chat_id: i64,
        draft_id: i64,
        max_items: usize,
        draft_enabled: bool,
        can_stop: bool,
    ) -> Self {
        Self {
            bot,
            chat_id,
            draft_id,
            max_items,
            draft_enabled,
            can_stop,
            items: Arc::new(RwLock::new(Vec::new())),
            start_time: Instant::now(),
            last_sync_time: Arc::new(RwLock::new(Instant::now())),
            sync_lock: Arc::new(Mutex::new(())),
            stopped: Arc::new(AtomicBool::new(false)),
            partial_answer: Arc::new(RwLock::new(String::new())),
        }
    }

    pub async fn add_action(&self, label: impl Into<String>, activity: Option<ProgressActivity>) {
        let lbl = label.into();
        let act = activity.unwrap_or(ProgressActivity::Thinking);

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

    pub async fn set_partial_answer(&self, text: &str) {
        *self.partial_answer.write().await = text.to_string();
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
        items
            .iter()
            .map(|it| it.format_line())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub async fn sync_draft(&self, force: bool) {
        // Telegram streaming drafts are a private-chat feature. In allowed group
        // chats Xiao still completes the request and sends the final response,
        // but it doesn't repeatedly call a draft method that Telegram will reject.
        if !self.draft_enabled {
            return;
        }
        if self.stopped.load(Ordering::SeqCst) && !force {
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

        let partial = self.partial_answer.read().await.clone();
        let mut blocks = vec![RichBlock::Thinking {
            text: Value::String(status),
        }];
        if !partial.trim().is_empty() {
            // The model streams Markdown. Feed the accumulated answer through
            // the same semantic parser used by the permanent final so users do
            // not see serialization markers such as **, ###, or --- while
            // Xiao is in the Writing state.
            blocks.extend(parse_markdown_to_rich_blocks(&partial));
        }
        let rich_message = InputRichMessage::new(blocks);

        if let Err(e) = self
            .bot
            .send_rich_message_draft(
                self.chat_id,
                self.draft_id,
                &rich_message,
                self.can_stop,
                false,
            )
            .await
        {
            debug!("Failed to sync draft update: {e}");
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_markdown_uses_native_rich_blocks() {
        let partial = "## Dua Gaya yang Bertarung\n\nOrbit itu **jatuh terus-menerus**.\n\n---\n\n1. **Gravitasi Bumi** — tarik ke bawah\n2. **Kecepatan tangensial** — dorong ke samping";
        let blocks = parse_markdown_to_rich_blocks(partial);
        let wire = serde_json::to_string(&blocks).unwrap();

        assert!(blocks
            .iter()
            .any(|block| matches!(block, RichBlock::SectionHeading { .. })));
        assert!(blocks
            .iter()
            .any(|block| matches!(block, RichBlock::Divider { .. })));
        assert!(blocks
            .iter()
            .any(|block| matches!(block, RichBlock::List { .. })));
        assert!(wire.contains("\"type\":\"bold\""));
        assert!(!wire.contains("## Dua Gaya"));
        assert!(!wire.contains("**jatuh terus-menerus**"));
    }
}
