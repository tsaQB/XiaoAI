mod ai;
mod attachments;
mod bot;
mod cli;
mod document;
mod parser;
mod timeline;
mod util;

use rand::Rng;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::env;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use ai::service::ProviderConfig;
use ai::AIChatService;
use bot::client::{TelegramBotClient, TelegramDeliveryContext};
use bot::models::{
    BotCommand, InlineKeyboardButton, InlineKeyboardMarkup, InputRichMessage, ReplyKeyboardMarkup,
    RichBlock, RichBlockTableCell, RichMessageButton, Update,
};
use parser::build_full_rich_message;
use timeline::{ExecutionTimeline, ProgressActivity};
use util::{escape_html, truncate_chars, truncate_chars_with_ellipsis};

type UserLastImagePrompt = Arc<RwLock<HashMap<i64, String>>>;

struct ChatInput<'a> {
    prompt: &'a str,
    image_bytes: Option<Vec<u8>>,
    document_images: Option<Vec<Vec<u8>>>,
    mime_type: Option<&'a str>,
    doc_text: Option<&'a str>,
    doc_name: Option<&'a str>,
    audio_bytes: Option<Vec<u8>>,
    audio_mime: Option<&'a str>,
    video_bytes: Option<Vec<u8>>,
    video_mime: Option<&'a str>,
    video_duration: Option<i32>,
}

#[derive(Clone, Debug)]
struct AccessPolicy {
    owner_user_id: i64,
    allowed_chat_ids: HashSet<i64>,
}

impl AccessPolicy {
    fn allows(&self, user_id: i64, chat_id: i64) -> bool {
        user_id == self.owner_user_id
            && (chat_id == self.owner_user_id || self.allowed_chat_ids.contains(&chat_id))
    }

    fn allows_stop_chat(&self, chat_id: i64) -> bool {
        // MessageGenerationStopped does not identify who pressed Stop. To prevent
        // another group participant from cancelling the owner's request, native
        // Stop is accepted only in the owner's private chat.
        chat_id == self.owner_user_id
    }
}

pub(crate) fn get_configured_owner_id() -> Option<i64> {
    load_environment();
    env::var("OWNER_USER_ID")
        .ok()
        .or_else(|| ai::service::load_app_setting("OWNER_USER_ID"))
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|value| *value > 0)
}

fn get_allowed_chat_ids() -> HashSet<i64> {
    load_environment();
    env::var("ALLOWED_CHAT_IDS")
        .ok()
        .or_else(|| ai::service::load_app_setting("ALLOWED_CHAT_IDS"))
        .unwrap_or_default()
        .split(',')
        .filter_map(|value| value.trim().parse::<i64>().ok())
        .collect()
}

fn get_config_path() -> std::path::PathBuf {
    // 1. Current working directory .env
    if Path::new(".env").exists() {
        return Path::new(".env").to_path_buf();
    }
    // 2. ~/.xiao.env or ~/.xiao/.env
    if let Ok(home) = env::var("HOME") {
        let home_env = Path::new(&home).join(".xiao.env");
        if home_env.exists() {
            return home_env;
        }
        let app_dir_env = Path::new(&home).join("XiaoAI").join(".env");
        if app_dir_env.exists() {
            return app_dir_env;
        }
    }
    Path::new(".env").to_path_buf()
}

pub(crate) fn load_environment() {
    let cfg_path = get_config_path();
    if cfg_path.exists() {
        let _ = dotenvy::from_path(&cfg_path);
    } else {
        let _ = dotenvy::dotenv();
    }
}

pub(crate) fn save_env_kv(key: &str, value: &str) -> io::Result<()> {
    ai::service::save_app_setting(key, value)
}

pub(crate) fn save_token_to_env(token: &str) -> io::Result<()> {
    ai::service::save_app_setting("BOT_TOKEN", token)
}

pub(crate) fn get_configured_token() -> Option<String> {
    load_environment();
    if let Ok(token) = env::var("BOT_TOKEN") {
        let trimmed = token.trim().to_string();
        if !trimmed.is_empty() && trimmed != "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
            return Some(trimmed);
        }
    }
    if let Some(token) = ai::service::load_app_setting("BOT_TOKEN") {
        let trimmed = token.trim().to_string();
        if !trimmed.is_empty() && trimmed != "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
            return Some(trimmed);
        }
    }
    None
}

use cli::*;

fn get_main_menu_keyboard() -> Value {
    serde_json::to_value(ReplyKeyboardMarkup::from_strings(
        vec![vec!["ɴᴇᴡ", "ᴍᴏᴅᴇʟ", "ᴄᴏɴᴛᴇxᴛ"]],
        false,
        true,
        Some("Tanya AI atau pilih menu..."),
    ))
    .unwrap()
}

fn get_collapsed_menu_keyboard() -> Value {
    get_main_menu_keyboard()
}

fn build_session_manager_inline_keyboard(
    sessions: &[ai::service::ChatSession],
    active_idx: usize,
    page: usize,
    page_size: usize,
) -> InlineKeyboardMarkup {
    let sessions_len = sessions.len();
    let total_pages = 1.max(sessions_len.div_ceil(page_size));
    let curr_page = 1.max(page.min(total_pages));

    let start_idx = (curr_page - 1) * page_size;
    let end_idx = (start_idx + page_size).min(sessions_len);

    let mut row_numbers = Vec::new();
    for i in start_idx..end_idx {
        let label = if i == active_idx {
            format!("✅ {}", i + 1)
        } else {
            format!("{}", i + 1)
        };
        if let Some(session) = sessions.get(i) {
            row_numbers.push(InlineKeyboardButton::callback(
                label,
                format!("session_select_id:{}", session.id),
            ));
        }
    }

    let mut rows = Vec::new();
    if !row_numbers.is_empty() {
        rows.push(row_numbers);
    }

    if total_pages > 1 {
        let previous_page = curr_page.saturating_sub(1);
        let next_page = (curr_page + 1).min(total_pages);
        let previous_button = if curr_page > 1 {
            InlineKeyboardButton::callback("‹", format!("session_page:{previous_page}"))
        } else {
            InlineKeyboardButton::disabled("‹")
        };
        let next_button = if curr_page < total_pages {
            InlineKeyboardButton::callback("›", format!("session_page:{next_page}"))
        } else {
            InlineKeyboardButton::disabled("›")
        };
        rows.push(vec![
            previous_button,
            InlineKeyboardButton::disabled(format!("{curr_page}/{total_pages}")),
            next_button,
        ]);
    }

    let active_session_id = sessions
        .get(active_idx)
        .map(|session| session.id)
        .unwrap_or_default();
    rows.push(vec![
        InlineKeyboardButton::callback("ᴅᴇʟᴇᴛᴇ", format!("session_remove_id:{active_session_id}")),
        InlineKeyboardButton::callback("ʀᴇɴᴀᴍᴇ", format!("session_rename_id:{active_session_id}")),
        InlineKeyboardButton::callback("ɴᴇᴡ", "session_new"),
    ]);
    rows.push(vec![InlineKeyboardButton::callback(
        "ᴄʟᴏꜱᴇ",
        "session_close",
    )]);

    InlineKeyboardMarkup::new(rows)
}

// ==========================================
// UI Builders
// ==========================================

async fn build_provider_model_picker(
    ai_service: &AIChatService,
    user_id: i64,
    provider_id: &str,
    page: usize,
    page_size: usize,
    is_setup: bool,
    search_query: Option<&str>,
) -> (String, InlineKeyboardMarkup) {
    let mut providers = ai_service.get_user_providers(user_id).await;

    if providers.is_empty() {
        return (
            "<b>Belum ada AI Provider yang dikonfigurasi.</b>\n\nSilakan jalankan <code>xiao provider add</code> di terminal.".to_string(),
            InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback("Close", "provider_close")]]),
        );
    }

    // Auto fetch if needed
    for prov in providers.iter_mut() {
        if prov.models.len() <= 1 && !prov.endpoint.is_empty() {
            if let (true, Ok(fetched)) = ai_service
                .fetch_models_from_endpoint(&prov.endpoint, &prov.api_key)
                .await
            {
                if !fetched.is_empty() {
                    ai_service
                        .update_provider_models(user_id, &prov.id, fetched.clone())
                        .await;
                    prov.models = fetched;
                }
            }
        }
    }

    let active_prov = ai_service.get_active_provider(user_id).await;
    let active_prov_id = active_prov.as_ref().map(|p| p.id.as_str()).unwrap_or("");
    let current_active_model = ai_service.get_user_model(user_id).await;

    let is_search = search_query.map(|s| !s.trim().is_empty()).unwrap_or(false);
    let clean_query = search_query.unwrap_or("").trim();
    let q_low = clean_query.to_lowercase();
    let safe_query = escape_html(clean_query);
    let safe_active_model = escape_html(&current_active_model);

    // If setup mode and a specific provider is specified, only show that provider's models
    let target_providers: Vec<&ProviderConfig> =
        if is_setup && provider_id != "all" && !provider_id.is_empty() {
            providers.iter().filter(|p| p.id == provider_id).collect()
        } else {
            providers.iter().collect()
        };

    // Aggregate catalog across all target providers: (prov_id, prov_name, orig_model_idx, model_name, is_active)
    let mut display_models: Vec<(String, String, usize, String, bool)> = Vec::new();
    let telegram_whitelist = ai_service.telegram_model_whitelist().await;
    for prov in target_providers {
        let is_this_prov_active = prov.id == active_prov_id;
        for (orig_idx, m) in prov.models.iter().enumerate() {
            let whitelist_key = format!("{}::{}", prov.id, m);
            if !is_setup
                && !telegram_whitelist.is_empty()
                && !telegram_whitelist
                    .iter()
                    .any(|selected| selected == &whitelist_key || selected == m)
            {
                continue;
            }
            if is_search
                && !m.to_lowercase().contains(&q_low)
                && !prov.name.to_lowercase().contains(&q_low)
            {
                continue;
            }
            let is_model_active =
                is_this_prov_active && (m == &prov.active_model || m == &current_active_model);
            display_models.push((
                prov.id.clone(),
                prov.name.clone(),
                orig_idx,
                m.clone(),
                is_model_active,
            ));
        }
    }

    let total_models = display_models.len();
    let total_pages = 1.max(total_models.div_ceil(page_size));
    let curr_page = 1.max(page.min(total_pages));

    let start_idx = (curr_page - 1) * page_size;
    let end_idx = (start_idx + page_size).min(total_models);
    let page_models = if total_models > 0 {
        &display_models[start_idx..end_idx]
    } else {
        &[]
    };

    let is_multi_provider = providers.len() > 1;

    let text = if is_setup {
        let prov_name = providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.name.as_str())
            .unwrap_or("Provider");
        let safe_prov_name = escape_html(prov_name);
        format!(
            "✨ <b>Endpoint Terhubung! ({})</b>\n\n\
             📋 Ditemukan <b>{} model AI</b> pada endpoint ini.\n\
             Silakan <b>klik 1x pada model pilihan Anda</b> di bawah untuk langsung mengaktifkannya dan menyelesaikan setup:",
            safe_prov_name, total_models
        )
    } else if is_search {
        if total_models == 0 {
            format!(
                "🔍 <b>Pencarian Model AI:</b> \"<code>{safe_query}</code>\"\n\n\
                 ⚠️ <i>Tidak ada model yang cocok dengan kata kunci tersebut di semua provider.</i>\n\
                 Silakan cari kata kunci lain atau sentuh tombol di bawah:"
            )
        } else {
            format!(
                "🔍 <b>Hasil Pencarian Model untuk:</b> \"<code>{safe_query}</code>\"\n\
                 Model aktif saat ini: <code>{}</code>\n\
                 Ditemukan: <b>{} model</b> (Halaman {}/{})\n\n\
                 Sentuh salah satu model di bawah untuk mengaktifkannya:",
                safe_active_model, total_models, curr_page, total_pages
            )
        }
    } else {
        if is_multi_provider {
            format!(
                "<b>Model</b> (Total <b>{} model</b> dari <b>{} provider</b>)\n\
                 Model aktif saat ini: <code>{}</code> (Halaman {}/{})\n\n\
                 Sentuh salah satu model di bawah untuk langsung mengaktifkannya dalam 1x klik:",
                total_models,
                providers.len(),
                safe_active_model,
                curr_page,
                total_pages
            )
        } else {
            let prov_name = providers
                .first()
                .map(|p| p.name.as_str())
                .unwrap_or("Provider");
            let safe_prov_name = escape_html(prov_name);
            format!(
                "<b>Model untuk '{}'</b>\n\
                 Model aktif saat ini: <code>{}</code>\n\
                 Total Model Tersedia: <b>{}</b> (Halaman {}/{})\n\n\
                 Sentuh salah satu model di bawah untuk langsung mengaktifkannya dalam 1x klik:",
                safe_prov_name, safe_active_model, total_models, curr_page, total_pages
            )
        }
    };

    let mut rows = Vec::new();
    let mut current_row = Vec::new();

    for (p_id, p_name, orig_global_idx, m, is_sel) in page_models {
        let mut btn_txt = if is_setup || !is_sel {
            if is_multi_provider && !is_setup {
                format!("{m} ({p_name})")
            } else {
                m.to_string()
            }
        } else {
            if is_multi_provider && !is_setup {
                format!("[active] {m} ({p_name})")
            } else {
                format!("[active] {m}")
            }
        };

        if btn_txt.len() > 30 {
            btn_txt = truncate_chars_with_ellipsis(&btn_txt, 27);
        }
        current_row.push(InlineKeyboardButton::callback(
            btn_txt,
            format!("set_m:{p_id}:{orig_global_idx}"),
        ));
        if current_row.len() == 2 {
            rows.push(current_row);
            current_row = Vec::new();
        }
    }
    if !current_row.is_empty() {
        rows.push(current_row);
    }

    let target_nav_id = if is_setup { provider_id } else { "all" };

    if total_pages > 1 {
        let mut nav_row = Vec::new();
        if curr_page > 1 {
            nav_row.push(InlineKeyboardButton::callback(
                "Prev",
                format!("provider_models:{target_nav_id}:{}", curr_page - 1),
            ));
        }
        nav_row.push(InlineKeyboardButton::disabled(format!(
            "Hal {curr_page}/{total_pages}"
        )));
        if curr_page < total_pages {
            nav_row.push(InlineKeyboardButton::callback(
                "Next",
                format!("provider_models:{target_nav_id}:{}", curr_page + 1),
            ));
        }
        rows.push(nav_row);
    }

    if !is_setup {
        rows.push(vec![InlineKeyboardButton::callback(
            "Close",
            "provider_close",
        )]);
    }

    (text, InlineKeyboardMarkup::new(rows))
}

fn truncate_session_name(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_string();
    }

    let truncated: String = name.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{truncated}...")
}

fn session_last_activity(session: &ai::service::ChatSession) -> String {
    let last_message = session.messages.last();
    let Some(message) = last_message else {
        return session.created_at.clone();
    };

    match &message.content {
        Value::String(content) if !content.trim().is_empty() => {
            truncate_session_name(content.trim(), 16)
        }
        _ => session.created_at.clone(),
    }
}

async fn build_session_manager_ui(
    ai_service: &AIChatService,
    user_id: i64,
    page: usize,
    page_size: usize,
) -> InputRichMessage {
    let sessions = ai_service.get_sessions(user_id).await;
    let active_idx = ai_service.get_active_session_index(user_id).await;

    let total_sessions = sessions.len();
    let total_pages = 1.max(total_sessions.div_ceil(page_size));
    let curr_page = 1.max(page.min(total_pages));

    let start_idx = (curr_page - 1) * page_size;
    let end_idx = (start_idx + page_size).min(total_sessions);
    let page_sessions = &sessions[start_idx..end_idx];

    let header_row = vec![
        RichBlockTableCell::text_only("No", true, Some("center")),
        RichBlockTableCell::text_only("Session name", true, Some("left")),
        RichBlockTableCell::text_only("Messages", true, Some("center")),
        RichBlockTableCell::text_only("Last", true, Some("left")),
    ];

    let mut table_rows = vec![header_row];
    for (i, session) in page_sessions.iter().enumerate() {
        let global_idx = start_idx + i;
        let name = if session.name.trim().is_empty() {
            format!("Session {}", global_idx + 1)
        } else {
            session.name.trim().to_string()
        };
        let number = if global_idx == active_idx {
            format!("✅ {}", global_idx + 1)
        } else {
            (global_idx + 1).to_string()
        };
        let message_count = session.messages.len() / 2;
        let last_activity = session_last_activity(session);

        table_rows.push(vec![
            RichBlockTableCell::text_only(&number, false, Some("center")),
            RichBlockTableCell::text_only(&truncate_session_name(&name, 20), false, Some("left")),
            RichBlockTableCell::text_only(&message_count.to_string(), false, Some("center")),
            RichBlockTableCell::text_only(&last_activity, false, Some("left")),
        ]);
    }

    let active_name = sessions
        .get(active_idx)
        .map(|session| {
            if session.name.trim().is_empty() {
                format!("Session {}", active_idx + 1)
            } else {
                session.name.trim().to_string()
            }
        })
        .unwrap_or_else(|| "-".to_string());
    let subtitle = format!("Active: {}", truncate_session_name(&active_name, 36));
    let pagination = format!("{curr_page}/{total_pages}");

    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("SESSIONS".to_string()),
            level: 1,
        },
        RichBlock::Paragraph {
            text: Value::String(subtitle),
        },
        RichBlock::Table {
            cells: table_rows,
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::Paragraph {
            text: Value::String(pagination),
        },
    ])
}

async fn send_or_update_session_manager(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    user_id: i64,
    message_id: Option<i64>,
    page: usize,
) {
    let sessions = ai_service.get_sessions(user_id).await;
    let active_idx = ai_service.get_active_session_index(user_id).await;

    let rich_msg = build_session_manager_ui(ai_service, user_id, page, 5).await;
    let inline_kb = build_session_manager_inline_keyboard(&sessions, active_idx, page, 5);
    let kb_val = serde_json::to_value(inline_kb).ok();

    if let Some(mid) = message_id {
        if bot
            .edit_rich_message(chat_id, mid, &rich_msg, kb_val.clone())
            .await
            .is_ok()
        {
            ai_service
                .user_session_msg_id
                .write()
                .await
                .insert(user_id, mid);
            return;
        }
    }

    let res = bot
        .send_rich_message(chat_id, &rich_msg, kb_val, None)
        .await;
    if let Ok(val) = res {
        if let Some(new_id) = val
            .get("result")
            .and_then(|r| r.get("message_id"))
            .and_then(|m| m.as_i64())
        {
            ai_service
                .user_session_msg_id
                .write()
                .await
                .insert(user_id, new_id);
        }
    }
}

fn to_small_caps(s: &str) -> String {
    s.chars()
        .map(|c| match c.to_ascii_lowercase() {
            'a' => 'ᴀ',
            'b' => 'ʙ',
            'c' => 'ᴄ',
            'd' => 'ᴅ',
            'e' => 'ᴇ',
            'f' => 'ғ',
            'g' => 'ɢ',
            'h' => 'ʜ',
            'i' => 'ɪ',
            'j' => 'ᴊ',
            'k' => 'ᴋ',
            'l' => 'ʟ',
            'm' => 'ᴍ',
            'n' => 'ɴ',
            'o' => 'ᴏ',
            'p' => 'ᴘ',
            'q' => 'ǫ',
            'r' => 'ʀ',
            's' => 'ꜱ',
            't' => 'ᴛ',
            'u' => 'ᴜ',
            'v' => 'ᴠ',
            'w' => 'ᴡ',
            'x' => 'x',
            'y' => 'ʏ',
            'z' => 'ᴢ',
            other => other,
        })
        .collect()
}

async fn build_context_monitor_ui(ai_service: &AIChatService, user_id: i64) -> InputRichMessage {
    let stats = ai_service.get_context_stats(user_id).await;
    let cap = &stats.capabilities;

    let limit_str = if stats.limit_tokens >= 1_000_000 {
        format!("{:.1}M", stats.limit_tokens as f64 / 1_000_000.0)
    } else if stats.limit_tokens >= 1_000 {
        format!("{}K", stats.limit_tokens / 1_000)
    } else {
        format!("{}", stats.limit_tokens)
    };

    let used_str = if stats.total_tokens >= 1_000_000 {
        format!("{:.1}M", stats.total_tokens as f64 / 1_000_000.0)
    } else if stats.total_tokens >= 1_000 {
        format!("{:.1}k", stats.total_tokens as f64 / 1_000.0)
    } else {
        format!("{}", stats.total_tokens)
    };

    let raw_header = format!("{} ({} TOKENS)", stats.model_name, limit_str);
    let header_text = to_small_caps(&raw_header);

    let mut cap_cells = Vec::new();
    if cap.vision {
        cap_cells.push(RichBlockTableCell::text_only(
            &to_small_caps("VISION"),
            true,
            Some("center"),
        ));
    }
    if cap.documents {
        cap_cells.push(RichBlockTableCell::text_only(
            &to_small_caps("DOCUMENT"),
            true,
            Some("center"),
        ));
    }
    if cap.video {
        cap_cells.push(RichBlockTableCell::text_only(
            &to_small_caps("VIDEO"),
            true,
            Some("center"),
        ));
    }
    if cap.audio {
        cap_cells.push(RichBlockTableCell::text_only(
            &to_small_caps("AUDIO"),
            true,
            Some("center"),
        ));
    }
    if cap.thinking {
        cap_cells.push(RichBlockTableCell::text_only(
            &to_small_caps("THINKING"),
            true,
            Some("center"),
        ));
    }
    if cap_cells.is_empty() {
        cap_cells.push(RichBlockTableCell::text_only(
            &to_small_caps("TEXT"),
            true,
            Some("center"),
        ));
    }

    let progress_text = if stats.total_messages > 0 {
        format!(
            "[{}] ~{:.2}% | ~{}/~{} Tokens | {} Pesan",
            stats.progress_bar, stats.usage_pct, used_str, limit_str, stats.total_messages
        )
    } else {
        format!(
            "[{}] ~{:.2}% | ~{}/~{} Tokens",
            stats.progress_bar, stats.usage_pct, used_str, limit_str
        )
    };

    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String(header_text),
            level: 1,
        },
        RichBlock::Table {
            cells: vec![cap_cells],
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::Preformatted {
            text: progress_text,
            language: None,
        },
        RichBlock::Buttons {
            buttons: vec![
                RichMessageButton::callback_styled("Refresh", "context_refresh", "primary"),
                RichMessageButton::callback("Close", "context_close"),
            ],
            align: Some("center".to_string()),
        },
    ])
}

async fn send_or_update_context_monitor(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    user_id: i64,
    message_id: Option<i64>,
) {
    let rich_msg = build_context_monitor_ui(ai_service, user_id).await;
    if let Some(mid) = message_id {
        if bot
            .edit_rich_message(chat_id, mid, &rich_msg, None)
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = bot.send_rich_message(chat_id, &rich_msg, None, None).await;
}

async fn send_welcome(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    user_id: i64,
) {
    let has_prov = ai_service.has_configured_provider(user_id).await;

    if !has_prov {
        let text = "👋 <b>Hi, selamat datang di XiaoAI!</b>\n\n\
                    ⚠️ <i>AI Provider belum dikonfigurasi.</i>\n\
                    Silakan jalankan perintah <code>xiao provider add</code> di terminal untuk menghubungkan provider AI.";
        let _ = bot
            .send_message(
                chat_id,
                text,
                Some("HTML"),
                Some(get_main_menu_keyboard()),
                None,
                None,
            )
            .await;
    } else {
        let text = "👋 <b>Hi, how can I help you?</b>\n\n\
                    <i>Silakan ketik pertanyaan Anda langsung atau gunakan tombol menu di bawah:</i>";
        let _ = bot
            .send_message(
                chat_id,
                text,
                Some("HTML"),
                Some(get_main_menu_keyboard()),
                None,
                None,
            )
            .await;
    }
}

// ==========================================
// Intent Detection & Image Generation
// ==========================================

fn extract_image_intent_prompt(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }

    let t_lower = t.to_lowercase();
    let inquiry_prefixes = [
        "apa itu",
        "apa arti",
        "jelaskan",
        "mengapa",
        "kenapa",
        "bagaimana cara",
        "cara ",
        "tutorial",
        "definisi",
        "what is",
        "why",
        "how to",
        "explain",
    ];
    if inquiry_prefixes
        .iter()
        .any(|pref| t_lower.starts_with(pref))
    {
        return None;
    }

    let follow_up_patterns = [
        r"(?i)^(?:tolong\s+|pls\s+|please\s+)?(?:buatkan|bikinin|bikin|buat|generate|draw|render|lukiskan|lukis|gambarin|gambarkan)\s+(?:dong\s+|kan\s+)?(?:gambar(?:nya| ini| itu| tersebut| tadi)?|foto(?:nya| ini| itu| tersebut)?|lukisan(?:nya| ini)?|image(?:nya)?|it|this)$",
        r"(?i)^(?:gambar(?:nya| ini| itu| tersebut)?|foto(?:nya)?|lukisan(?:nya)?)\s*(?:dong|ya|tolong|pls)?$",
    ];
    for pat in follow_up_patterns {
        if Regex::new(pat).unwrap().is_match(t) {
            return Some("__CONTEXT_FOLLOWUP__".to_string());
        }
    }

    let patterns = [
        r"(?i)^(?:tolong\s+|pls\s+|please\s+)?(?:buatkan|buatlah|buat|bikinin|bikin|generate|create|render|hasilkan|lukiskan|lukis|gambarin|gambarkan|draw)\s+(?:saya\s+|aku\s+|in\s+)?(?:sebuah\s+|seekor\s+|seorang\s+|suatu\s+|an?\s+|the\s+)?(?:gambar|foto|photo|lukisan|image|picture|wallpaper|ilustrasi|illustration|artwork|poster|visual)\s+(?:tentang\s+|dari\s+|of\s+|about\s+)?(.+)$",
        r"(?i)^(?:tolong\s+|pls\s+|please\s+)?(?:gambarin|gambarkan|lukiskan|lukis)\s+(?:saya\s+|aku\s+|in\s+)?(?:dong\s+|kan\s+)?(.+)$",
        r"(?i)^(?:ilustrasi|lukisan|artwork|wallpaper|fanart|sketsa|foto)\s+(?:tentang\s+|dari\s+|of\s+|about\s+)?(.+)$",
        r"(?i)^(?:tolong\s+|pls\s+|please\s+)?(?:buatkan|bikinin|bikin)\s+(?:saya\s+|aku\s+)?(?:dong\s+)?(.+?\b(?:gaya|style|anime|wallpaper|realistis|realistic|3d|cyberpunk|lukisan|sketsa|art|hd|8k)\b.*)$",
        r"(?i)^(?:please\s+|can you\s+)?(?:generate|create|make|draw|render)\s+(?:me\s+)?(?:an?\s+|the\s+)?(?:image|picture|photo|illustration|drawing|wallpaper|artwork)\s+(?:of\s+|about\s+)?(.+)$",
    ];

    let clean_re = Regex::new(r"(?i)^(?:tentang|mengenai|berupa|of|about|dong|ya|tolong)\s+")
        .expect("static image intent cleanup regex must compile");

    for pat in patterns {
        if let Some(caps) = Regex::new(pat).unwrap().captures(t) {
            if let Some(extracted_match) = caps.get(1) {
                let mut extracted = extracted_match.as_str().trim().to_string();
                extracted = clean_re.replace(&extracted, "").trim().to_string();

                let ext_low = extracted.to_lowercase();
                if [
                    "dong",
                    "ya",
                    "ini",
                    "itu",
                    "nya",
                    "tadi",
                    "tersebut",
                    "gambarnya",
                    "fotonya",
                ]
                .contains(&ext_low.as_str())
                {
                    return Some("__CONTEXT_FOLLOWUP__".to_string());
                }
                if extracted.len() >= 3 {
                    return Some(extracted);
                }
            }
        }
    }

    None
}

async fn handle_image_generation(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    user_last_image_prompt: &UserLastImagePrompt,
    chat_id: i64,
    user_id: i64,
    prompt: &str,
) {
    let mut clean_prompt = prompt.trim().to_string();

    if clean_prompt == "__CONTEXT_FOLLOWUP__"
        || [
            "gambarnya",
            "gambarnya dong",
            "itu",
            "yang tadi",
            "ini",
            "dong",
            "ya",
            "fotonya",
        ]
        .contains(&clean_prompt.as_str())
    {
        let mut last_context = String::new();
        if let Some(sess) = ai_service.get_active_session(user_id).await {
            for msg in sess.messages.iter().rev() {
                let candidate = match &msg.content {
                    Value::String(value) => Some(value.clone()),
                    value => attachments::decode_user_content(value).map(|content| content.text),
                };
                if let Some(candidate) = candidate {
                    if candidate.trim().chars().count() > 8 {
                        last_context = candidate.trim().to_string();
                        break;
                    }
                }
            }
        }

        if !last_context.is_empty() {
            clean_prompt = format!("illustration of {}", truncate_chars(&last_context, 250));
        } else {
            let last_guard = user_last_image_prompt.read().await;
            clean_prompt = last_guard.get(&user_id).cloned().unwrap_or_else(|| {
                "majestic mountain scenery with clouds and ancient kingdom".to_string()
            });
        }
    }

    if clean_prompt.is_empty() {
        let mut map = HashMap::new();
        map.insert("step".to_string(), "awaiting_image_prompt".to_string());
        ai_service
            .user_wizard_state
            .write()
            .await
            .insert(user_id, map);

        let kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
            "❌ Batalkan",
            "provider_cancel",
        )]]);
        let _ = bot
            .send_message(
                chat_id,
                "🫟 <b>AI Image Generator</b>\n\n\
                 Silakan ketikkan <b>deskripsi/prompt gambar</b> yang ingin Anda buat.\n\n\
                 <i>Contoh:</i>\n\
                 • <code>seekor rubah cyberpunk bercahaya neon di tengah hujan</code>\n\
                 • <code>futuristic coffee shop in anime style 8k</code>\n\
                 • <code>pemandangan gunung fuji dengan bunga sakura saat senja</code>",
                Some("HTML"),
                serde_json::to_value(kb).ok(),
                None,
                None,
            )
            .await;
        return;
    }

    ai_service.user_wizard_state.write().await.remove(&user_id);
    user_last_image_prompt
        .write()
        .await
        .insert(user_id, clean_prompt.clone());

    let draft_id: i64 = rand::thread_rng().gen_range(100000..999999);
    let timeline = Arc::new(ExecutionTimeline::new(
        bot.clone(),
        chat_id,
        draft_id,
        10,
        chat_id == user_id,
        false,
    ));
    timeline
        .add_action("Generating Image", Some(ProgressActivity::Drawing))
        .await;
    timeline.sync_draft(true).await;
    timeline.start_ticker();
    let _ = bot.send_chat_action(chat_id, "upload_photo").await;

    let (success, img_bytes, engine_info) = ai_service
        .generate_image(user_id, &clean_prompt, 1024, 1024)
        .await;
    timeline.stop_ticker();

    if !success || img_bytes.is_none() {
        timeline.fail_current(engine_info.clone()).await;
        timeline.sync_draft(true).await;

        let safe_engine_info = escape_html(&engine_info);
        let err_text = format!(
            "❌ <b>Gagal Membuat Gambar</b>\n\n\
             <b>Penyebab:</b> {safe_engine_info}\n\n\
             Silakan coba lagi atau gunakan kata kunci/prompt yang berbeda."
        );
        let retry_kb = InlineKeyboardMarkup::new(vec![
            vec![InlineKeyboardButton::callback("🔄 Coba Lagi", "img_regen")],
            vec![InlineKeyboardButton::callback(
                "📱 Menu Utama",
                "action_menu",
            )],
        ]);
        let _ = bot
            .send_message(
                chat_id,
                &err_text,
                Some("HTML"),
                serde_json::to_value(retry_kb).ok(),
                None,
                None,
            )
            .await;
        return;
    }

    let safe_prompt = escape_html(&clean_prompt);
    let safe_engine_info = escape_html(&engine_info);
    let caption_text = format!(
        "🫟 <b>Gambar Berhasil Dibuat!</b>\n\n\
         📝 <b>Prompt:</b> <i>\"{safe_prompt}\"</i>\n\
         ⚡ <b>Engine:</b> <code>{safe_engine_info}</code>"
    );

    let img_kb = InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("🔄 Buat Ulang (Regenerate)", "img_regen"),
            InlineKeyboardButton::callback("🫟 Gambar Baru", "img_new"),
        ],
        vec![InlineKeyboardButton::callback(
            "📱 Buka Menu",
            "action_menu",
        )],
    ]);

    let _ = bot
        .send_photo_bytes(
            chat_id,
            img_bytes.unwrap(),
            Some(&caption_text),
            Some("HTML"),
            serde_json::to_value(img_kb).ok(),
            None,
        )
        .await;
}

// ==========================================
// Main AI Chat Handler
// ==========================================

async fn handle_ai_chat(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    user_id: i64,
    input: ChatInput<'_>,
) {
    let ChatInput {
        prompt: user_prompt,
        image_bytes,
        document_images,
        mime_type,
        doc_text,
        doc_name,
        audio_bytes,
        audio_mime,
        video_bytes,
        video_mime,
        video_duration,
    } = input;
    let generation_lock = ai_service.generation_lock(user_id).await;
    let _generation_guard = generation_lock.lock().await;

    let draft_id: i64 = rand::thread_rng().gen_range(100000..999999);
    let mut cancel_rx = ai_service.begin_generation(chat_id, draft_id).await;
    let timeline = Arc::new(ExecutionTimeline::new(
        bot.clone(),
        chat_id,
        draft_id,
        30,
        chat_id == user_id,
        chat_id == user_id,
    ));

    let (initial_lbl, initial_act) = if video_bytes.is_some() {
        ("Watching", ProgressActivity::Watching)
    } else if image_bytes.is_some()
        || document_images
            .as_ref()
            .is_some_and(|pages| !pages.is_empty())
    {
        ("Looking", ProgressActivity::Looking)
    } else if doc_text.is_some() {
        ("Reading", ProgressActivity::Reading)
    } else if audio_bytes.is_some() {
        ("Listening", ProgressActivity::Listening)
    } else {
        ("Thinking", ProgressActivity::Thinking)
    };

    timeline.add_action(initial_lbl, Some(initial_act)).await;
    timeline.sync_draft(true).await;
    timeline.start_ticker();
    let _ = bot.send_chat_action(chat_id, "typing").await;

    let current_model = ai_service.get_user_model(user_id).await;

    let (_thinking, mut answer_text, _cancelled) = ai_service
        .generate_response(
            user_id,
            ai::service::GenerationInput {
                prompt: user_prompt,
                timeline: Some(&timeline),
                image_bytes,
                document_images,
                mime_type,
                doc_text,
                doc_name,
                audio_bytes,
                audio_mime,
                video_bytes,
                video_mime,
                video_duration,
            },
            &mut cancel_rx,
        )
        .await;

    ai_service.end_generation(chat_id, draft_id).await;
    timeline.stop_ticker();

    if answer_text.trim().is_empty() {
        answer_text = "Maaf, respon AI kosong untuk permintaan ini.".to_string();
    }

    let full_rich_msg = build_full_rich_message(&answer_text, Some(&current_model));
    let res = bot
        .send_rich_message(
            chat_id,
            &full_rich_msg,
            Some(get_collapsed_menu_keyboard()),
            None,
        )
        .await;

    if res.is_err()
        || !res
            .unwrap()
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        let _ = bot
            .send_message(
                chat_id,
                &answer_text,
                None,
                Some(get_collapsed_menu_keyboard()),
                None,
                None,
            )
            .await;
    }
}

// ==========================================
// Update Router
// ==========================================

fn delivery_context_for_update(update: &Update) -> TelegramDeliveryContext {
    if let Some(message) = update.message.as_ref() {
        return TelegramDeliveryContext {
            message_thread_id: message.message_thread_id,
            receiver_user_id: message
                .ephemeral_message_id
                .and_then(|_| message.from.as_ref().map(|user| user.id)),
            source_ephemeral_message_id: message.ephemeral_message_id,
            callback_query_id: None,
        };
    }
    if let Some(callback) = update.callback_query.as_ref() {
        let message = callback.message.as_deref();
        let source_ephemeral_message_id =
            message.and_then(|message| message.ephemeral_message_id);
        return TelegramDeliveryContext {
            message_thread_id: message.and_then(|message| message.message_thread_id),
            receiver_user_id: source_ephemeral_message_id.map(|_| callback.from.id),
            source_ephemeral_message_id,
            callback_query_id: source_ephemeral_message_id
                .map(|_| callback.id.clone()),
        };
    }
    if let Some(stopped) = update.stopped_message_generation.as_ref() {
        return TelegramDeliveryContext {
            message_thread_id: stopped.message_thread_id,
            ..TelegramDeliveryContext::default()
        };
    }
    TelegramDeliveryContext::default()
}

async fn handle_update(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    user_last_image_prompt: &UserLastImagePrompt,
    access: &AccessPolicy,
    update: Update,
) {
    if let Some(stopped) = update.stopped_message_generation.as_ref() {
        if access.allows_stop_chat(stopped.chat.id) {
            let _ = ai_service
                .cancel_generation(stopped.chat.id, stopped.draft_id)
                .await;
        }
        return;
    }
    if let Some(msg) = update.message {
        let chat_id = msg.chat.id;
        let user_id = msg.from.as_ref().map(|u| u.id).unwrap_or(chat_id);
        if !access.allows(user_id, chat_id) {
            return;
        }
        let _user_name = msg
            .from
            .as_ref()
            .map(|u| u.first_name.as_str())
            .unwrap_or("Pengguna");
        let text = msg
            .text
            .as_deref()
            .or(msg.caption.as_deref())
            .unwrap_or("")
            .trim()
            .to_string();

        let mut image_bytes = None;
        let mut document_images = None;
        let mut mime_type = None;
        let mut doc_text = None;
        let mut doc_name = None;
        let mut audio_bytes = None;
        let mut audio_mime = None;
        let mut audio_duration = 0;
        let mut video_bytes = None;
        let mut video_mime = None;
        let mut video_duration = 0;

        let has_video = msg.video.is_some() || msg.video_note.is_some();
        let has_photo = msg.photo.is_some();

        if let Some(v) = msg.voice {
            audio_duration = v.duration;
            audio_mime = v.mime_type;
            if let Some((data, _)) = bot.get_file_bytes(&v.file_id).await {
                audio_bytes = Some(data);
            }
        } else if let Some(a) = msg.audio {
            audio_duration = a.duration;
            audio_mime = a.mime_type;
            if let Some((data, _)) = bot.get_file_bytes(&a.file_id).await {
                audio_bytes = Some(data);
            }
        } else if let Some(vid) = msg.video {
            video_duration = vid.duration;
            if let Some((data, path)) = bot.get_file_bytes(&vid.file_id).await {
                video_bytes = Some(data);
                let ext = path.split('.').next_back().unwrap_or("mp4");
                video_mime = vid.mime_type.or_else(|| Some(format!("video/{ext}")));
            }
        } else if let Some(vn) = msg.video_note {
            video_duration = vn.duration;
            if let Some((data, _)) = bot.get_file_bytes(&vn.file_id).await {
                video_bytes = Some(data);
                video_mime = Some("video/mp4".to_string());
            }
        } else if let Some(photos) = msg.photo {
            if let Some(largest) = photos.last() {
                if let Some((data, path)) = bot.get_file_bytes(&largest.file_id).await {
                    image_bytes = Some(data);
                    let ext = path.split('.').next_back().unwrap_or("jpeg");
                    mime_type = Some(if ext == "jpg" {
                        "image/jpeg".to_string()
                    } else {
                        format!("image/{ext}")
                    });
                }
            }
        } else if let Some(doc) = msg.document {
            let d_mime = doc.mime_type.clone().unwrap_or_default();
            let d_name = doc
                .file_name
                .clone()
                .unwrap_or_else(|| "dokumen".to_string());
            if let Some((data, path)) = bot.get_file_bytes(&doc.file_id).await {
                if d_mime.starts_with("image/")
                    || [".png", ".jpg", ".jpeg", ".webp"]
                        .iter()
                        .any(|ext| path.to_lowercase().ends_with(ext))
                {
                    image_bytes = Some(data);
                    mime_type = Some(if d_mime.is_empty() {
                        "image/jpeg".to_string()
                    } else {
                        d_mime
                    });
                } else if d_mime.starts_with("video/")
                    || [".mp4", ".mov", ".avi", ".webm", ".mkv"]
                        .iter()
                        .any(|ext| path.to_lowercase().ends_with(ext))
                {
                    video_bytes = Some(data);
                    video_mime = Some(if d_mime.is_empty() {
                        "video/mp4".to_string()
                    } else {
                        d_mime
                    });
                } else if d_mime.starts_with("audio/")
                    || [".ogg", ".mp3", ".wav", ".m4a"]
                        .iter()
                        .any(|ext| path.to_lowercase().ends_with(ext))
                {
                    audio_bytes = Some(data);
                    audio_mime = Some(if d_mime.is_empty() {
                        "audio/ogg".to_string()
                    } else {
                        d_mime
                    });
                } else if document::is_extractable_document(&d_mime, &d_name) {
                    match document::extract_document(data, &d_mime, &d_name).await {
                        Ok(extracted) => {
                            doc_text = extracted.text;
                            if !extracted.rendered_pages.is_empty() {
                                document_images = Some(extracted.rendered_pages);
                            }
                            doc_name = Some(d_name);
                            if let Some(warning) = extracted.warning {
                                info!("{warning}");
                            }
                        }
                        Err(err) => {
                            let safe_name = escape_html(&d_name);
                            let safe_error = escape_html(&err);
                            let _ = bot
                                .send_message(
                                    chat_id,
                                    &format!(
                                        "⚠️ <b>Dokumen tidak dapat diproses.</b>\n\n<code>{safe_name}</code>\n{safe_error}"
                                    ),
                                    Some("HTML"),
                                    None,
                                    None,
                                    None,
                                )
                                .await;
                            return;
                        }
                    }
                } else {
                    let safe_name = escape_html(&d_name);
                    let _ = bot.send_message(
                        chat_id,
                        &format!(
                            "⚠️ <b>Format dokumen belum didukung.</b>\n\n<code>{safe_name}</code> tidak akan dipaksa dibaca sebagai teks biner. Xiao mendukung dokumen teks/kode, PDF, DOCX, dan XLSX."
                        ),
                        Some("HTML"), None, None, None
                    ).await;
                    return;
                }
            }
        }

        // Wizard state handler
        let wizard_opt = {
            let guard = ai_service.user_wizard_state.read().await;
            guard.get(&user_id).cloned()
        };

        if let Some(wizard) = wizard_opt {
            if !text.is_empty() {
                if ["/cancel", "/batal", "batal", "cancel"].contains(&text.as_str()) {
                    ai_service.user_wizard_state.write().await.remove(&user_id);
                    let _ = bot
                        .send_message(
                            chat_id,
                            "❌ <b>Aksi dibatalkan.</b>",
                            Some("HTML"),
                            Some(get_collapsed_menu_keyboard()),
                            None,
                            None,
                        )
                        .await;
                    return;
                }

                let step = wizard.get("step").map(|s| s.as_str()).unwrap_or("");
                if step == "awaiting_image_prompt" {
                    handle_image_generation(
                        bot,
                        ai_service,
                        user_last_image_prompt,
                        chat_id,
                        user_id,
                        &text,
                    )
                    .await;
                    return;
                }
            }
        }

        // Rename session handler
        let rename_opt = {
            let guard = ai_service.user_waiting_rename.read().await;
            guard.get(&user_id).copied()
        };
        if let Some(target_session_id) = rename_opt {
            if !text.is_empty() && !text.starts_with('/') {
                ai_service
                    .user_waiting_rename
                    .write()
                    .await
                    .remove(&user_id);
                let orig_msg_id = ai_service.user_rename_msg_id.write().await.remove(&user_id);
                ai_service
                    .rename_session_by_id(user_id, target_session_id, &text)
                    .await;

                let _ = bot
                    .send_message(
                        chat_id,
                        &format!(
                            "✅ Session berhasil diubah namanya menjadi: <b>{}</b>",
                            escape_html(&text)
                        ),
                        Some("HTML"),
                        Some(get_collapsed_menu_keyboard()),
                        None,
                        None,
                    )
                    .await;
                send_or_update_session_manager(bot, ai_service, chat_id, user_id, orig_msg_id, 1)
                    .await;
                return;
            }
        }

        // Strict provider lock
        if !ai_service.has_configured_provider(user_id).await {
            send_welcome(bot, ai_service, chat_id, user_id).await;
            return;
        }

        // Voice audio processing
        if let Some(a_bytes) = audio_bytes {
            let (stt_ok, transcript_res) = ai_service
                .transcribe_audio(
                    user_id,
                    a_bytes.clone(),
                    &format!("voice_{}.ogg", msg.message_id),
                )
                .await;

            if stt_ok {
                let user_prompt = transcript_res.unwrap_or_default();
                if let Some(img_p) = extract_image_intent_prompt(&user_prompt) {
                    handle_image_generation(
                        bot,
                        ai_service,
                        user_last_image_prompt,
                        chat_id,
                        user_id,
                        &img_p,
                    )
                    .await;
                    return;
                }
                let prompt_fmt = format!(
                    "[Pesan Suara ({} detik)]: \"{}\"\n\nJawab pertanyaan atau tanggapi pesan suara di atas secara mendalam dan jelas.",
                    audio_duration, user_prompt
                );
                handle_ai_chat(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    ChatInput {
                        prompt: &prompt_fmt,
                        image_bytes: None,
                        document_images: None,
                        mime_type: None,
                        doc_text: None,
                        doc_name: None,
                        audio_bytes: None,
                        audio_mime: None,
                        video_bytes: None,
                        video_mime: None,
                        video_duration: None,
                    },
                )
                .await;
                return;
            } else {
                let prompt_audio = if !text.is_empty() {
                    text.clone()
                } else {
                    "Dengarkan pesan suara ini dan jawab pertanyaan atau tanggapi maksud di dalamnya secara jelas dan mendalam.".to_string()
                };

                handle_ai_chat(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    ChatInput {
                        prompt: &prompt_audio,
                        image_bytes: None,
                        document_images: None,
                        mime_type: None,
                        doc_text: None,
                        doc_name: None,
                        audio_bytes: Some(a_bytes),
                        audio_mime: audio_mime.as_deref(),
                        video_bytes: None,
                        video_mime: None,
                        video_duration: None,
                    },
                )
                .await;
                return;
            }
        }

        // Video processing
        if let Some(v_bytes) = video_bytes {
            let prompt_video = if !text.is_empty() {
                text.clone()
            } else {
                "Tonton dan analisis rekaman video ini secara mendalam. Jelaskan isi visual, alur peristiwa, teks di layar, dan suara di dalamnya.".to_string()
            };

            handle_ai_chat(
                bot,
                ai_service,
                chat_id,
                user_id,
                ChatInput {
                    prompt: &prompt_video,
                    image_bytes: None,
                    document_images: None,
                    mime_type: None,
                    doc_text: None,
                    doc_name: None,
                    audio_bytes: None,
                    audio_mime: None,
                    video_bytes: Some(v_bytes),
                    video_mime: video_mime.as_deref(),
                    video_duration: Some(video_duration),
                },
            )
            .await;
            return;
        } else if has_video {
            let _ = bot
                .send_message(
                    chat_id,
                    "⚠️ <b>Gagal mengunduh video dari Telegram.</b>\n\n\
                     Telegram membatasi ukuran unduhan file bot maksimal <b>20MB</b>. Pastikan durasi atau ukuran video di bawah 20MB.",
                    Some("HTML"),
                    Some(get_main_menu_keyboard()),
                    None,
                    None,
                )
                .await;
            return;
        } else if has_photo && image_bytes.is_none() {
            let _ = bot
                .send_message(
                    chat_id,
                    "⚠️ <b>Gagal mengunduh gambar dari server Telegram.</b> Silakan coba kirim ulang.",
                    Some("HTML"),
                    Some(get_main_menu_keyboard()),
                    None,
                    None,
                )
                .await;
            return;
        }

        // Navigation Commands
        if text.starts_with("/start")
            || [
                "📱 Menu",
                "Menu",
                "/menu",
                "🔙 Menu Utama",
                "🔙 Kembali ke Menu Utama",
                "Menu Utama",
                "Main Menu",
                "main menu",
                "Main menu",
            ]
            .contains(&text.as_str())
        {
            send_welcome(bot, ai_service, chat_id, user_id).await;
        } else if text.starts_with("/new")
            || [
                "ɴᴇᴡ",
                "➕ ɴᴇᴡ",
                "➕ New",
                "New",
                "new",
                "➕ Chat Baru",
                "Chat Baru",
            ]
            .contains(&text.as_str())
        {
            if ai_service.create_new_session(user_id, None).await.is_none() {
                let _ = bot
                    .send_message(
                        chat_id,
                        "⚠️ <b>Session baru tidak dibuat.</b> Penyimpanan sedang tidak tersedia; XiaoAI menolak memakai ID sementara yang dapat bentrok.",
                        Some("HTML"),
                        None,
                        None,
                        None,
                    )
                    .await;
                return;
            }
            let total_sessions = ai_service.get_sessions(user_id).await.len();
            let target_page = total_sessions.saturating_sub(1) / 5 + 1;
            let _ = bot
                .send_message(
                    chat_id,
                    "✨ <b>Session baru berhasil dibuat & diaktifkan!</b>",
                    Some("HTML"),
                    Some(get_main_menu_keyboard()),
                    None,
                    None,
                )
                .await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page)
                .await;
        } else if (text == "/session" || text.starts_with("/session "))
            || [
                "📑 Session",
                "Session",
                "session",
                "📑 Session Manager",
                "📑 Lihat Daftar Session",
            ]
            .contains(&text.as_str())
        {
            let active_idx = ai_service.get_active_session_index(user_id).await;
            let target_page = (active_idx / 5) + 1;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page)
                .await;
        } else if [
            "🗑️ Hapus Session",
            "🗑️ Remove Session",
            "Delete",
            "delete",
            "Delete Session",
        ]
        .contains(&text.as_str())
        {
            let active_idx = ai_service.get_active_session_index(user_id).await;
            ai_service.remove_session(user_id, active_idx).await;
            let new_active_idx = ai_service.get_active_session_index(user_id).await;
            let target_page = (new_active_idx / 5) + 1;
            let _ = bot
                .send_message(
                    chat_id,
                    "🗑️ <b>Session berhasil dihapus!</b>",
                    Some("HTML"),
                    None,
                    None,
                    None,
                )
                .await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page)
                .await;
        } else if [
            "✏️ Rename Session",
            "✏️ Ubah Nama Session",
            "Rename",
            "rename",
            "Rename Session",
        ]
        .contains(&text.as_str())
        {
            let active_idx = ai_service.get_active_session_index(user_id).await;
            let Some(active_session_id) = ai_service.get_active_session_id(user_id).await else {
                let _ = bot
                    .send_message(
                        chat_id,
                        "⚠️ Session aktif tidak tersedia karena storage gagal diakses.",
                        None,
                        None,
                        None,
                        None,
                    )
                    .await;
                return;
            };
            ai_service
                .user_waiting_rename
                .write()
                .await
                .insert(user_id, active_session_id);
            let _ = bot
                .send_message(
                    chat_id,
                    &format!(
                        "✏️ <b>Ketikkan nama baru untuk Session #{}:</b>",
                        active_idx + 1
                    ),
                    Some("HTML"),
                    None,
                    None,
                    None,
                )
                .await;
        } else if let Some(caps) = Regex::new(r"(?i)Hal(?:aman)?\s*([0-9]+)")
            .unwrap()
            .captures(&text)
        {
            if ["Hal", "▶", "◀"].iter().any(|k| text.contains(k)) {
                let target_page: usize = caps
                    .get(1)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(1);
                send_or_update_session_manager(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    None,
                    target_page,
                )
                .await;
            }
        } else if let Some(caps) = Regex::new(r"^(?:✅\s*|Session\s*)?([0-9]+)$")
            .unwrap()
            .captures(text.trim())
        {
            if text.trim().chars().all(|c| c.is_ascii_digit())
                || text.trim().starts_with("✅")
                || text.trim().starts_with("Session ")
            {
                let num: usize = caps
                    .get(1)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(1);
                let idx = num.saturating_sub(1);
                let sessions = ai_service.get_sessions(user_id).await;
                if idx < sessions.len() {
                    ai_service.switch_session(user_id, idx).await;
                    let target_page = (idx / 5) + 1;
                    send_or_update_session_manager(
                        bot,
                        ai_service,
                        chat_id,
                        user_id,
                        None,
                        target_page,
                    )
                    .await;
                } else {
                    handle_ai_chat(
                        bot,
                        ai_service,
                        chat_id,
                        user_id,
                        ChatInput {
                            prompt: &text,
                            image_bytes,
                            document_images,
                            mime_type: mime_type.as_deref(),
                            doc_text: doc_text.as_deref(),
                            doc_name: doc_name.as_deref(),
                            audio_bytes: None,
                            audio_mime: None,
                            video_bytes: None,
                            video_mime: None,
                            video_duration: None,
                        },
                    )
                    .await;
                }
            }
        } else if text.starts_with("/context")
            || [
                "ᴄᴏɴᴛᴇxᴛ",
                "🧠 ᴄᴏɴᴛᴇxᴛ",
                "🧠 Context",
                "Context",
                "context",
                "🧠 Info Konteks",
                "Info Konteks",
            ]
            .contains(&text.as_str())
        {
            send_or_update_context_monitor(bot, ai_service, chat_id, user_id, None).await;
        } else if text.starts_with("/model")
            || [
                "ᴍᴏᴅᴇʟ",
                "⚙️ ᴍᴏᴅᴇʟ",
                "⚙️ Model",
                "Model",
                "model",
                "⚙️ Model AI",
                "Pilih Model",
            ]
            .contains(&text.as_str())
        {
            if ai_service.has_configured_provider(user_id).await {
                let (msg_txt, kb_m) =
                    build_provider_model_picker(ai_service, user_id, "all", 1, 8, false, None)
                        .await;
                let _ = bot
                    .send_message(
                        chat_id,
                        &msg_txt,
                        Some("HTML"),
                        serde_json::to_value(kb_m).ok(),
                        None,
                        None,
                    )
                    .await;
            }
        } else if text.starts_with("⚡ ") {
            let selected_model = text.strip_prefix("⚡ ").unwrap_or(&text).trim();
            let all_providers = ai_service.get_user_providers(user_id).await;
            let mut found_prov = None;
            for p in &all_providers {
                if p.models.iter().any(|m| m == selected_model) {
                    found_prov = Some(p.clone());
                    break;
                }
            }

            if let Some(prov) = found_prov {
                ai_service.set_active_provider(user_id, &prov.id).await;
                ai_service
                    .set_provider_model(user_id, &prov.id, selected_model)
                    .await;
                ai_service.set_user_model(user_id, selected_model).await;
                let _ = bot
                    .send_message(
                        chat_id,
                        &format!(
                            "✅ <b>Model AI diubah ke:</b> <code>{}</code> (<i>{}</i>)",
                            escape_html(selected_model),
                            escape_html(&prov.name)
                        ),
                        Some("HTML"),
                        Some(get_main_menu_keyboard()),
                        None,
                        None,
                    )
                    .await;
            } else {
                handle_ai_chat(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    ChatInput {
                        prompt: &text,
                        image_bytes,
                        document_images,
                        mime_type: mime_type.as_deref(),
                        doc_text: doc_text.as_deref(),
                        doc_name: doc_name.as_deref(),
                        audio_bytes: None,
                        audio_mime: None,
                        video_bytes: None,
                        video_mime: None,
                        video_duration: None,
                    },
                )
                .await;
            }
        } else if text.starts_with("/image")
            || [
                "🫟 Buat Gambar",
                "🫟 Generate Gambar",
                "📸 Buat Gambar",
                "🎨 Buat Gambar",
                "Buat Gambar",
            ]
            .contains(&text.as_str())
        {
            let prompt_arg = if text.starts_with("/image") {
                text.strip_prefix("/image").unwrap_or("").trim()
            } else {
                ""
            };
            handle_image_generation(
                bot,
                ai_service,
                user_last_image_prompt,
                chat_id,
                user_id,
                prompt_arg,
            )
            .await;
        } else if text.starts_with("/clear")
            || ["🗑️ Reset Chat", "🗑️ Reset Obrolan"].contains(&text.as_str())
        {
            ai_service.clear_history(user_id).await;
            let _ = bot
                .send_message(
                    chat_id,
                    "🧹 <b>Riwayat percakapan session ini berhasil direset!</b> Anda dapat memulai topik obrolan baru.",
                    Some("HTML"),
                    Some(get_main_menu_keyboard()),
                    None,
                    None,
                )
                .await;
        } else if text.starts_with("/help")
            || ["📖 Bantuan", "📖 Bantuan & Info"].contains(&text.as_str())
        {
            let help_text = "📖 <b>Perintah XiaoAI</b>\n\n\
                             /start — Mulai bot & tampilkan menu\n\
                             /menu — Buka menu utama\n\
                             /help — Tampilkan bantuan ini\n\
                             /session — Kelola session percakapan\n\
                             /new — Buat session baru\n\
                             /clear — Hapus riwayat session aktif\n\
                             /context — Lihat penggunaan konteks/memori\n\
                             /model [kata kunci] — Pilih atau cari model AI\n\
                             /image [prompt] — Buat gambar AI\n\n\
                             💬 Kirim teks, gambar, dokumen, video, atau voice note untuk mengobrol dengan AI.";
            let _ = bot
                .send_message(
                    chat_id,
                    help_text,
                    Some("HTML"),
                    Some(get_main_menu_keyboard()),
                    None,
                    None,
                )
                .await;
        } else {
            let auto_img_prompt = if image_bytes.is_none() && doc_text.is_none() {
                extract_image_intent_prompt(&text)
            } else {
                None
            };

            if let Some(ref img_p) = auto_img_prompt {
                handle_image_generation(
                    bot,
                    ai_service,
                    user_last_image_prompt,
                    chat_id,
                    user_id,
                    img_p,
                )
                .await;
            } else {
                handle_ai_chat(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    ChatInput {
                        prompt: &text,
                        image_bytes,
                        document_images,
                        mime_type: mime_type.as_deref(),
                        doc_text: doc_text.as_deref(),
                        doc_name: doc_name.as_deref(),
                        audio_bytes: None,
                        audio_mime: None,
                        video_bytes,
                        video_mime: video_mime.as_deref(),
                        video_duration: Some(video_duration),
                    },
                )
                .await;
            }
        }
    } else if let Some(cq) = update.callback_query {
        let cq_id = cq.id;
        let cq_data = cq.data.unwrap_or_default();
        let user_id = cq.from.id;
        let chat_id = cq.message.as_ref().map(|m| m.chat.id).unwrap_or(user_id);
        if !access.allows(user_id, chat_id) {
            return;
        }
        let msg_id = cq.message.as_ref().map(|m| m.message_id);

        if cq_data == "noop" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            return;
        }

        // Provider lock on callbacks
        if !ai_service.has_configured_provider(user_id).await {
            let allowed_cqs = [
                "provider_new",
                "start_provider_wizard",
                "provider_retry_endpoint",
                "provider_retry_apikey",
                "provider_skip_alias",
                "provider_cancel",
                "provider_set_model",
                "set_m",
                "provider_models",
                "noop",
            ];
            if !allowed_cqs.iter().any(|pref| cq_data.starts_with(pref)) {
                let _ = bot
                    .answer_callback_query(&cq_id, Some("🔒 Menu terkunci! Jalankan `xiao provider add` di terminal terlebih dahulu."), true)
                    .await;
                return;
            }
        }

        if let Some(id_str) = cq_data.strip_prefix("session_select_id:") {
            if let Ok(session_id) = id_str.parse::<usize>() {
                if ai_service.switch_session_by_id(user_id, session_id).await {
                    let active_idx = ai_service.get_active_session_index(user_id).await;
                    let target_page = (active_idx / 5) + 1;
                    let _ = bot
                        .answer_callback_query(&cq_id, Some("Session aktif diperbarui ✅"), false)
                        .await;
                    send_or_update_session_manager(
                        bot,
                        ai_service,
                        chat_id,
                        user_id,
                        msg_id,
                        target_page,
                    )
                    .await;
                }
            }
        } else if let Some(id_str) = cq_data.strip_prefix("session_remove_id:") {
            if let Ok(session_id) = id_str.parse::<usize>() {
                if ai_service.remove_session_by_id(user_id, session_id).await {
                    let new_active = ai_service.get_active_session_index(user_id).await;
                    let target_page = (new_active / 5) + 1;
                    let _ = bot
                        .answer_callback_query(&cq_id, Some("Session berhasil dihapus 🗑️"), false)
                        .await;
                    send_or_update_session_manager(
                        bot,
                        ai_service,
                        chat_id,
                        user_id,
                        msg_id,
                        target_page,
                    )
                    .await;
                }
            }
        } else if let Some(id_str) = cq_data.strip_prefix("session_rename_id:") {
            if let Ok(session_id) = id_str.parse::<usize>() {
                let sessions = ai_service.get_sessions(user_id).await;
                if sessions.iter().any(|session| session.id == session_id) {
                    ai_service
                        .user_waiting_rename
                        .write()
                        .await
                        .insert(user_id, session_id);
                    if let Some(mid) = msg_id {
                        ai_service
                            .user_rename_msg_id
                            .write()
                            .await
                            .insert(user_id, mid);
                    }
                    let _ = bot
                        .answer_callback_query(&cq_id, Some("Silakan ketik nama baru"), false)
                        .await;
                    let _ = bot
                        .send_message(
                            chat_id,
                            "✏️ <b>Ketikkan nama baru untuk session aktif:</b>",
                            Some("HTML"),
                            None,
                            None,
                            None,
                        )
                        .await;
                }
            }
        } else if let Some(idx_str) = cq_data.strip_prefix("session_select:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                ai_service.switch_session(user_id, idx).await;
                let target_page = (idx / 5) + 1;
                let _ = bot
                    .answer_callback_query(
                        &cq_id,
                        Some(&format!("Beralih ke Session #{} ✅", idx + 1)),
                        false,
                    )
                    .await;
                send_or_update_session_manager(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    msg_id,
                    target_page,
                )
                .await;
            }
        } else if cq_data == "session_new" {
            if ai_service.create_new_session(user_id, None).await.is_none() {
                let _ = bot
                    .answer_callback_query(
                        &cq_id,
                        Some("Storage tidak tersedia; session tidak dibuat."),
                        true,
                    )
                    .await;
                return;
            }
            let total_sessions = ai_service.get_sessions(user_id).await.len();
            let target_page = total_sessions.saturating_sub(1) / 5 + 1;
            let _ = bot
                .answer_callback_query(&cq_id, Some("Session baru berhasil dibuat! ➕"), false)
                .await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, msg_id, target_page)
                .await;
        } else if let Some(idx_str) = cq_data.strip_prefix("session_remove:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                ai_service.remove_session(user_id, idx).await;
                let new_active = ai_service.get_active_session_index(user_id).await;
                let target_page = (new_active / 5) + 1;
                let _ = bot
                    .answer_callback_query(&cq_id, Some("Session berhasil dihapus 🗑️"), false)
                    .await;
                send_or_update_session_manager(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    msg_id,
                    target_page,
                )
                .await;
            }
        } else if let Some(idx_str) = cq_data.strip_prefix("session_detail:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                let sessions = ai_service.get_sessions(user_id).await;
                if let Some(session) = sessions.get(idx) {
                    let name = if session.name.trim().is_empty() {
                        format!("Session {}", idx + 1)
                    } else {
                        session.name.trim().to_string()
                    };
                    let detail = format!(
                        "<b>{}</b>\n\nMessages: <b>{}</b>\nCreated: <code>{}</code>\nLast: <code>{}</code>",
                        escape_html(&name),
                        session.messages.len() / 2,
                        escape_html(&session.created_at),
                        escape_html(&session_last_activity(session))
                    );
                    let _ = bot.answer_callback_query(&cq_id, None, false).await;
                    let _ = bot
                        .send_message(chat_id, &detail, Some("HTML"), None, None, None)
                        .await;
                }
            }
        } else if let Some(idx_str) = cq_data.strip_prefix("session_rename:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                let sessions = ai_service.get_sessions(user_id).await;
                let Some(session_id) = sessions.get(idx).map(|session| session.id) else {
                    return;
                };
                ai_service
                    .user_waiting_rename
                    .write()
                    .await
                    .insert(user_id, session_id);
                if let Some(mid) = msg_id {
                    ai_service
                        .user_rename_msg_id
                        .write()
                        .await
                        .insert(user_id, mid);
                }
                let _ = bot
                    .answer_callback_query(&cq_id, Some("Silakan ketik nama baru"), false)
                    .await;
                let _ = bot
                    .send_message(
                        chat_id,
                        &format!("✏️ <b>Ketikkan nama baru untuk Session #{}:</b>", idx + 1),
                        Some("HTML"),
                        None,
                        None,
                        None,
                    )
                    .await;
            }
        } else if let Some(page_str) = cq_data.strip_prefix("session_page:") {
            if let Ok(p) = page_str.parse::<usize>() {
                let _ = bot.answer_callback_query(&cq_id, None, false).await;
                send_or_update_session_manager(bot, ai_service, chat_id, user_id, msg_id, p).await;
            }
        } else if cq_data == "session_close" || cq_data == "provider_close" {
            if let Some(mid) = msg_id {
                let _ = bot.delete_message(chat_id, mid).await;
            }
            let _ = bot
                .answer_callback_query(&cq_id, Some("Menu ditutup"), false)
                .await;
        } else if cq_data == "open_session" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, 1).await;
        } else if let Some(rest) = cq_data.strip_prefix("provider_models:") {
            let parts: Vec<&str> = rest.split(':').collect();
            let prov_id = parts[0];
            let target_page: usize = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(1);
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            let (text_m, kb_m) = build_provider_model_picker(
                ai_service,
                user_id,
                prov_id,
                target_page,
                8,
                false,
                None,
            )
            .await;
            let kb_val = serde_json::to_value(kb_m).ok();

            if let Some(mid) = msg_id {
                if bot
                    .edit_message_text(
                        Some(chat_id),
                        Some(mid),
                        &text_m,
                        Some("HTML"),
                        kb_val.clone(),
                    )
                    .await
                    .is_err()
                {
                    let _ = bot
                        .send_message(chat_id, &text_m, Some("HTML"), kb_val, None, None)
                        .await;
                }
            }
        } else if let Some(rest) = cq_data.strip_prefix("set_m:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() == 2 {
                let prov_id = parts[0];
                let model_idx: usize = parts[1].parse().unwrap_or(0);

                let model_name_opt = ai_service
                    .get_provider_model_by_index(user_id, prov_id, model_idx)
                    .await;
                if let Some(model_name) = model_name_opt {
                    ai_service.set_active_provider(user_id, prov_id).await;
                    ai_service
                        .set_provider_model(user_id, prov_id, &model_name)
                        .await;
                    ai_service.set_user_model(user_id, &model_name).await;
                    let _ = bot
                        .answer_callback_query(
                            &cq_id,
                            Some(&format!("Model: {model_name} Aktif! ✅")),
                            false,
                        )
                        .await;

                    let providers = ai_service.get_user_providers(user_id).await;
                    let target_prov = providers.iter().find(|p| p.id == prov_id);
                    let prov_name = target_prov
                        .map(|p| p.name.as_str())
                        .unwrap_or("Custom Provider");
                    let endpoint_url = target_prov.map(|p| p.endpoint.as_str()).unwrap_or("");
                    let safe_prov_name = escape_html(prov_name);
                    let safe_endpoint_url = escape_html(endpoint_url);
                    let safe_model_name = escape_html(&model_name);

                    let done_text = format!(
                        "🎉 <b>Model AI Berhasil Diubah!</b>\n\n\
                         🌐 <b>Provider:</b> <b>{safe_prov_name}</b>\n\
                         🔗 <b>Endpoint:</b> <code>{safe_endpoint_url}</code>\n\
                         ⚡ <b>Model Aktif:</b> <code>{safe_model_name}</code>\n\n\
                         💬 <i>Silakan langsung ketik pesan Anda untuk mulai mengobrol.</i>"
                    );

                    let done_kb = InlineKeyboardMarkup::new(vec![vec![
                        InlineKeyboardButton::callback(
                            "⚙️ Ganti Model",
                            format!("provider_models:{prov_id}:1"),
                        ),
                        InlineKeyboardButton::callback("✖️ Tutup", "provider_close"),
                    ]]);
                    let done_val = serde_json::to_value(done_kb).ok();

                    if let Some(mid) = msg_id {
                        if bot
                            .edit_message_text(
                                Some(chat_id),
                                Some(mid),
                                &done_text,
                                Some("HTML"),
                                done_val.clone(),
                            )
                            .await
                            .is_err()
                        {
                            let _ = bot
                                .send_message(
                                    chat_id,
                                    &done_text,
                                    Some("HTML"),
                                    done_val,
                                    None,
                                    None,
                                )
                                .await;
                        }
                    } else {
                        let _ = bot
                            .send_message(chat_id, &done_text, Some("HTML"), done_val, None, None)
                            .await;
                    }
                } else {
                    let _ = bot
                        .answer_callback_query(&cq_id, Some("Model tidak ditemukan."), false)
                        .await;
                }
            }
        } else if cq_data == "provider_cancel" {
            ai_service.user_wizard_state.write().await.remove(&user_id);
            let _ = bot
                .answer_callback_query(&cq_id, Some("Aksi dibatalkan"), false)
                .await;
            if let Some(mid) = msg_id {
                let _ = bot.delete_message(chat_id, mid).await;
            }
        } else if cq_data == "img_new" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            handle_image_generation(
                bot,
                ai_service,
                user_last_image_prompt,
                chat_id,
                user_id,
                "",
            )
            .await;
        } else if cq_data == "img_regen" {
            let last_guard = user_last_image_prompt.read().await;
            let last_p = last_guard
                .get(&user_id)
                .cloned()
                .unwrap_or_else(|| "cyberpunk aesthetic landscape".to_string());
            drop(last_guard);
            let _ = bot
                .answer_callback_query(&cq_id, Some("Membuat ulang gambar... 🫟"), false)
                .await;
            handle_image_generation(
                bot,
                ai_service,
                user_last_image_prompt,
                chat_id,
                user_id,
                &last_p,
            )
            .await;
        } else if cq_data == "context_refresh"
            || cq_data == "open_context"
            || cq_data == "show_context"
        {
            let _ = bot
                .answer_callback_query(&cq_id, Some("Konteks diperbarui! 🧠"), false)
                .await;
            send_or_update_context_monitor(bot, ai_service, chat_id, user_id, msg_id).await;
        } else if cq_data == "context_close" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            if let Some(mid) = msg_id {
                let _ = bot.delete_message(chat_id, mid).await;
            }
        } else if cq_data == "open_new_session" {
            let Some(new_sess) = ai_service.create_new_session(user_id, None).await else {
                let _ = bot
                    .answer_callback_query(
                        &cq_id,
                        Some("Storage tidak tersedia; sesi tidak dibuat."),
                        true,
                    )
                    .await;
                return;
            };
            let _ = bot
                .answer_callback_query(
                    &cq_id,
                    Some(&format!("Sesi #{} dibuat! ✨", new_sess.id)),
                    false,
                )
                .await;
            let _ = bot
                .send_message(
                    chat_id,
                    &format!(
                        "✨ <b>Sesi Baru Berhasil Dibuat!</b>\nSesi aktif saat ini: <b>{}</b>",
                        escape_html(&new_sess.name)
                    ),
                    Some("HTML"),
                    Some(get_collapsed_menu_keyboard()),
                    None,
                    None,
                )
                .await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, 1).await;
        } else if cq_data == "action_clear" {
            ai_service.clear_history(user_id).await;
            let _ = bot
                .answer_callback_query(&cq_id, Some("Konteks direset! 🧹"), false)
                .await;
            let _ = bot
                .send_message(
                    chat_id,
                    "🧹 <b>Riwayat memori konteks pada sesi ini berhasil dibersihkan!</b>",
                    Some("HTML"),
                    None,
                    None,
                    None,
                )
                .await;
        } else if cq_data == "action_menu" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            send_welcome(bot, ai_service, chat_id, user_id).await;
        }
    }
}

// ==========================================
// Main Function & Polling Loop
// ==========================================

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let args: Vec<String> = env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("start");

    let ai_service = Arc::new(AIChatService::new());

    match subcommand {
        "setup" | "quickstart" => {
            let _ = run_cli_quickstart_wizard(&ai_service).await;
            return;
        }
        "provider" => {
            let action_arg = args.get(2).map(|s| s.as_str());
            run_cli_provider_menu(&ai_service, action_arg).await;
            return;
        }
        "telegram" | "tg" => {
            let action_arg = args.get(2).map(|s| s.as_str());
            let extra_arg = args.get(3).map(|s| s.as_str());
            run_cli_telegram_menu(action_arg, extra_arg).await;
            return;
        }
        "model" => {
            let is_pick = args.get(2).map(|s| s.as_str()) == Some("pick")
                || args.get(3).map(|s| s.as_str()) == Some("pick");
            if args.get(2).map(|s| s.as_str()) == Some("probe") {
                run_cli_model_probe(&ai_service).await;
            } else if is_pick {
                run_cli_telegram_pick(&ai_service).await;
            } else {
                let filter_arg = args.get(2).map(|s| s.as_str());
                run_cli_model_picker(&ai_service, filter_arg).await;
            }
            return;
        }
        "status" => {
            run_cli_status(&ai_service).await;
            return;
        }
        "help" | "--help" | "-h" => {
            print_cli_help();
            return;
        }
        "start" => {
            // Proceed to run bot server
        }
        unknown => {
            println!("⚠️ Perintah tidak dikenal: '{unknown}'");
            print_cli_help();
            return;
        }
    }

    tracing_subscriber::fmt::init();

    let Some(token) = get_or_prompt_token(&ai_service).await else {
        return;
    };

    let Some(owner_user_id) = get_configured_owner_id() else {
        error!("OWNER_USER_ID belum dikonfigurasi. Jalankan `xiao telegram owner <telegram_user_id>` atau `xiao setup`.");
        return;
    };
    let access = Arc::new(AccessPolicy {
        owner_user_id,
        allowed_chat_ids: get_allowed_chat_ids(),
    });

    let bot = TelegramBotClient::new(token);
    let user_last_image_prompt: UserLastImagePrompt = Arc::new(RwLock::new(HashMap::new()));

    // Test connection
    match bot.get_me().await {
        Ok(resp) if resp.ok && resp.result.is_some() => {
            let bot_info = resp.result.unwrap();
            println!(
                "\n🚀 XiaoAI @{} online menggunakan Telegram Bot API 10.3!",
                bot_info.username.unwrap_or_default()
            );
            println!("⚡ Streaming Timeline + Native Stop Active!");
            println!("🌐 Custom OpenAI-Compatible Provider Setup Active (via CLI)\n");
        }
        Ok(resp) => {
            error!(
                "Gagal terhubung ke Telegram Bot API: {:?}",
                resp.description
            );
            return;
        }
        Err(e) => {
            error!("HTTP connection error: {e}");
            return;
        }
    }

    // Register Bot Commands
    let commands = vec![
        BotCommand::ephemeral("menu", "Buka menu navigasi bot"),
        BotCommand::ephemeral("context", "Monitor penggunaan memori konteks model AI"),
        BotCommand::ephemeral("image", "Buat gambar AI dari deskripsi teks"),
        BotCommand::ephemeral("new", "Mulai chat baru & buka Session Manager"),
        BotCommand::ephemeral("session", "Kelola daftar session obrolan"),
        BotCommand::ephemeral("start", "Mulai bot & info provider"),
        BotCommand::ephemeral("model", "Ganti model AI"),
        BotCommand::ephemeral("clear", "Reset riwayat percakapan chat"),
        BotCommand::ephemeral("help", "Daftar perintah dan panduan"),
    ];

    if let Err(e) = bot.set_my_commands(&commands).await {
        warn!("Gagal mendaftarkan bot commands: {e}");
    } else {
        info!("Commands berhasil didaftarkan ke Telegram.");
    }

    let (update_tx, mut update_rx) = tokio::sync::mpsc::channel::<Update>(64);
    let worker_bot = bot.clone();
    let worker_ai = Arc::clone(&ai_service);
    let worker_last_image = Arc::clone(&user_last_image_prompt);
    let worker_access = Arc::clone(&access);
    let worker_handle = tokio::spawn(async move {
        while let Some(update) = update_rx.recv().await {
            let update_id = update.update_id;
            if !ai::storage::mark_telegram_processing_async(update_id).await {
                continue;
            }
            let delivery_context = delivery_context_for_update(&update);
            TelegramBotClient::with_delivery_context(
                delivery_context,
                handle_update(
                    &worker_bot,
                    &worker_ai,
                    &worker_last_image,
                    &worker_access,
                    update,
                ),
            )
            .await;
            if !ai::storage::mark_telegram_processed_async(update_id).await {
                warn!("Gagal menyelesaikan durable Telegram inbox update {update_id}");
            }
        }
    });

    let interrupted = ai::storage::quarantine_telegram_processing_async().await;
    if interrupted > 0 {
        warn!(
            "{interrupted} Telegram update dikarantina karena daemon berhenti saat processing; replay otomatis dinonaktifkan untuk mencegah side effect ganda"
        );
    }

    for record in ai::storage::pending_telegram_updates_async(500).await {
        match serde_json::from_str::<Update>(&record.payload_json) {
            Ok(update) => {
                if update_tx.send(update).await.is_err() {
                    error!("Update worker stopped while replaying durable inbox");
                    return;
                }
            }
            Err(error) => {
                warn!(
                    "Durable Telegram update {} tidak dapat didecode: {error}",
                    record.update_id
                );
                if ai::storage::mark_telegram_processing_async(record.update_id).await {
                    let _ = ai::storage::mark_telegram_processed_async(record.update_id).await;
                }
            }
        }
    }

    let mut offset = ai::storage::load_telegram_offset_async().await;
    info!("Memulai polling pesan dengan durable bounded ordered update queue...");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n🛑 Menerima sinyal berhenti. Bot dimatikan secara aman.");
                break;
            }
            updates_res = bot.get_updates(offset, 100, 20, Some(vec!["message".to_string(), "callback_query".to_string(), "stopped_message_generation".to_string()])) => {
                match updates_res {
                    Ok(resp) if resp.ok => {
                        if let Some(updates) = resp.result {
                            for update in updates {
                                let update_id = update.update_id;
                                let payload_json = match serde_json::to_string(&update) {
                                    Ok(payload) => payload,
                                    Err(error) => {
                                        error!("Gagal serialize Telegram update {update_id}: {error}");
                                        break;
                                    }
                                };
                                let Some(accepted) = ai::storage::enqueue_telegram_update_async(
                                    update_id,
                                    payload_json,
                                )
                                .await
                                else {
                                    // Never acknowledge a later Telegram update if the durable
                                    // acceptance transaction for this update failed.
                                    error!(
                                        "Durable Telegram intake gagal untuk update {update_id}; offset tidak dimajukan"
                                    );
                                    break;
                                };
                                offset = Some(update_id.saturating_add(1));
                                if !accepted {
                                    continue;
                                }

                                if update.stopped_message_generation.is_some() {
                                    // Native Stop bypasses the ordered worker so it remains
                                    // responsive, but it still crosses the same durable claim
                                    // boundary before any cancellation side effect.
                                    if ai::storage::mark_telegram_processing_async(update_id).await {
                                        let delivery_context =
                                            delivery_context_for_update(&update);
                                        TelegramBotClient::with_delivery_context(
                                            delivery_context,
                                            handle_update(
                                                &bot,
                                                &ai_service,
                                                &user_last_image_prompt,
                                                &access,
                                                update,
                                            ),
                                        )
                                        .await;
                                        let _ =
                                            ai::storage::mark_telegram_processed_async(update_id)
                                                .await;
                                    }
                                } else if update_tx.send(update).await.is_err() {
                                    error!("Update worker stopped unexpectedly");
                                    return;
                                }
                            }
                        }
                    }
                    Ok(resp) => {
                        warn!("Telegram polling update not ok: {:?}", resp.description);
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    Err(e) => {
                        error!("Polling network error: {e}");
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
        }
    }

    ai_service.cancel_all_generations().await;
    drop(update_tx);
    match tokio::time::timeout(Duration::from_secs(5), worker_handle).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!("Update worker terminated with error: {err}"),
        Err(_) => warn!("Update worker did not stop within shutdown grace period"),
    }
}
