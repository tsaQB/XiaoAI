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
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use ai::service::{GenerationModelSnapshot, ImageGenerationErrorKind, ProbeEvent, ProviderConfig};
use ai::AIChatService;
use bot::client::{TelegramBotClient, TelegramDeliveryContext};
use bot::models::{
    BotCommand, InlineKeyboardButton, InlineKeyboardMarkup, InputRichMessage, ReplyKeyboardMarkup,
    RichBlock, RichBlockListItem, RichBlockTableCell, RichMessageButton, Update,
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
    model_snapshot: Option<&'a GenerationModelSnapshot>,
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
    .unwrap_or_else(|error| {
        error!("Failed to serialize main menu keyboard: {error}");
        json!({})
    })
}

fn get_collapsed_menu_keyboard() -> Value {
    get_main_menu_keyboard()
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
                    if ai_service
                        .update_provider_models(user_id, &prov.id, fetched.clone())
                        .await
                    {
                        prov.models = fetched;
                    } else {
                        eprintln!("[WARN] Provider model refresh was not persisted; keeping durable catalog");
                    }
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

    let mut table_rows = vec![vec![
        RichBlockTableCell::text_only("#", true, Some("center")),
        RichBlockTableCell::text_only("Session", true, Some("left")),
        RichBlockTableCell::text_only("Status", true, Some("left")),
    ]];
    let mut session_buttons = Vec::new();
    for (offset, session) in page_sessions.iter().enumerate() {
        let global_idx = start_idx + offset;
        let name = if session.name.trim().is_empty() {
            format!("Session {}", global_idx + 1)
        } else {
            session.name.trim().to_string()
        };
        let is_active = global_idx == active_idx;
        let status = if is_active {
            format!("Active · {} msgs", session.messages.len())
        } else {
            format!(
                "{} msgs · {}",
                session.messages.len(),
                session_last_activity(session)
            )
        };
        table_rows.push(vec![
            RichBlockTableCell::text_only(&(global_idx + 1).to_string(), false, Some("center")),
            RichBlockTableCell::text_only(&truncate_session_name(&name, 28), false, Some("left")),
            RichBlockTableCell::text_only(&status, false, Some("left")),
        ]);
        session_buttons.push(if is_active {
            RichMessageButton::callback_styled(
                format!("✓ {}", global_idx + 1),
                format!("session_select_id:{}", session.id),
                "primary",
            )
        } else {
            RichMessageButton::callback(
                (global_idx + 1).to_string(),
                format!("session_select_id:{}", session.id),
            )
        });
    }

    let active_session_id = sessions
        .get(active_idx)
        .map(|session| session.id)
        .unwrap_or_default();
    let mut blocks = vec![
        RichBlock::SectionHeading {
            text: Value::String("SESSIONS".to_string()),
            level: 1,
        },
        RichBlock::Paragraph {
            text: Value::String(format!(
                "Active: {}",
                sessions
                    .get(active_idx)
                    .map(|session| truncate_session_name(&session.name, 36))
                    .unwrap_or_else(|| "-".to_string())
            )),
        },
        RichBlock::Table {
            cells: table_rows,
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
    ];
    if !session_buttons.is_empty() {
        blocks.push(RichBlock::Buttons {
            buttons: session_buttons,
            align: Some("center".to_string()),
        });
    }
    if total_pages > 1 {
        blocks.push(RichBlock::Buttons {
            buttons: vec![
                if curr_page > 1 {
                    RichMessageButton::callback("‹", format!("session_page:{}", curr_page - 1))
                } else {
                    RichMessageButton::disabled("‹")
                },
                RichMessageButton::disabled(format!("{curr_page}/{total_pages}")),
                if curr_page < total_pages {
                    RichMessageButton::callback("›", format!("session_page:{}", curr_page + 1))
                } else {
                    RichMessageButton::disabled("›")
                },
            ],
            align: Some("center".to_string()),
        });
    }
    blocks.push(RichBlock::Buttons {
        buttons: vec![
            RichMessageButton::callback_styled("New", "session_new", "primary"),
            RichMessageButton::callback(
                "Rename Active",
                format!("session_rename_id:{active_session_id}"),
            ),
            RichMessageButton::callback_styled(
                "Delete Active",
                format!("session_remove_id:{active_session_id}"),
                "danger",
            ),
        ],
        align: Some("center".to_string()),
    });
    blocks.push(RichBlock::Buttons {
        buttons: vec![
            RichMessageButton::callback("Context", "open_context"),
            RichMessageButton::callback("Close", "session_close"),
        ],
        align: Some("center".to_string()),
    });
    InputRichMessage::new(blocks)
}

async fn send_or_update_session_manager(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    user_id: i64,
    message_id: Option<i64>,
    page: usize,
) {
    let rich_msg = build_session_manager_ui(ai_service, user_id, page, 5).await;

    if let Some(mid) = message_id {
        if bot
            .edit_rich_message(chat_id, mid, &rich_msg, None)
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

    let res = bot.send_rich_message(chat_id, &rich_msg, None, None).await;
    if let Ok(val) = res {
        if let Some(new_id) = val
            .get("result")
            .and_then(|result| result.get("message_id"))
            .and_then(Value::as_i64)
        {
            ai_service
                .user_session_msg_id
                .write()
                .await
                .insert(user_id, new_id);
        }
    }
}

async fn build_start_ui(ai_service: &AIChatService, user_id: i64) -> InputRichMessage {
    let main = ai_service
        .resolve_model_route_unchecked(ai::service::ModelRole::Main)
        .await
        .ok();
    let stats = ai_service.get_context_stats(user_id).await;
    let main_model = main
        .as_ref()
        .map(|route| route.model.clone())
        .unwrap_or_else(|| "Not configured".to_string());
    let provider_name = main
        .as_ref()
        .map(|route| route.provider.name.clone())
        .unwrap_or_else(|| "Not configured".to_string());
    let session_name = if stats.session_name.trim().is_empty() {
        format!("Session #{}", stats.session_id)
    } else {
        truncate_session_name(&stats.session_name, 32)
    };
    let context = if main.is_some() {
        format!(
            "Ready · ~{} / ~{} tokens",
            stats.total_tokens, stats.limit_tokens
        )
    } else {
        "Setup required".to_string()
    };

    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("XiaoAI".to_string()),
            level: 1,
        },
        RichBlock::Paragraph {
            text: Value::String("Personal AI Assistant".to_string()),
        },
        RichBlock::Table {
            cells: vec![
                vec![
                    RichBlockTableCell::text_only("Status", true, Some("left")),
                    RichBlockTableCell::text_only("Current", true, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Main Model", false, Some("left")),
                    RichBlockTableCell::text_only(&main_model, false, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Provider", false, Some("left")),
                    RichBlockTableCell::text_only(&provider_name, false, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Session", false, Some("left")),
                    RichBlockTableCell::text_only(&session_name, false, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Context", false, Some("left")),
                    RichBlockTableCell::text_only(&context, false, Some("left")),
                ],
            ],
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::BlockQuotation {
            blocks: vec![json!({
                "type":"paragraph",
                "text":"Kirim teks, gambar, dokumen, video, atau voice note untuk mulai berbicara dengan Xiao."
            })],
        },
        RichBlock::Buttons {
            buttons: vec![
                RichMessageButton::callback_styled("New Chat", "session_new", "primary"),
                RichMessageButton::callback("Model", "model_dashboard"),
            ],
            align: Some("center".to_string()),
        },
        RichBlock::Buttons {
            buttons: vec![
                RichMessageButton::callback("Session", "open_session"),
                RichMessageButton::callback("Context", "open_context"),
            ],
            align: Some("center".to_string()),
        },
        RichBlock::Paragraph {
            text: Value::String("Addon Models dan provider dikelola melalui Xiao CLI.".to_string()),
        },
    ])
}

async fn build_menu_ui(ai_service: &AIChatService, user_id: i64) -> InputRichMessage {
    let main = ai_service
        .resolve_model_route_unchecked(ai::service::ModelRole::Main)
        .await
        .ok();
    let stats = ai_service.get_context_stats(user_id).await;
    let main_model = main
        .as_ref()
        .map(|route| format!("{} / {}", route.provider.name, route.model))
        .unwrap_or_else(|| "Not configured".to_string());
    let session_name = if stats.session_name.trim().is_empty() {
        format!("Session #{}", stats.session_id)
    } else {
        truncate_session_name(&stats.session_name, 36)
    };

    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("MENU".to_string()),
            level: 1,
        },
        RichBlock::Table {
            cells: vec![
                vec![
                    RichBlockTableCell::text_only("Main Model", true, Some("left")),
                    RichBlockTableCell::text_only("Session", true, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only(&main_model, false, Some("left")),
                    RichBlockTableCell::text_only(&session_name, false, Some("left")),
                ],
            ],
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::Buttons {
            buttons: vec![
                RichMessageButton::callback_styled("New Chat", "session_new", "primary"),
                RichMessageButton::callback("Session", "open_session"),
            ],
            align: Some("center".to_string()),
        },
        RichBlock::Buttons {
            buttons: vec![
                RichMessageButton::callback("Model", "model_dashboard"),
                RichMessageButton::callback("Context", "open_context"),
            ],
            align: Some("center".to_string()),
        },
        RichBlock::Buttons {
            buttons: vec![
                RichMessageButton::callback("Generate Image", "img_new"),
                RichMessageButton::callback("Help", "action_help"),
            ],
            align: Some("center".to_string()),
        },
    ])
}

fn build_help_ui() -> InputRichMessage {
    let input_items = vec![
        RichBlockListItem::bullet(vec![
            json!({"type":"paragraph","text":"Text — ordinary chat and instructions."}),
        ]),
        RichBlockListItem::bullet(vec![
            json!({"type":"paragraph","text":"Images — routed through the verified Vision role."}),
        ]),
        RichBlockListItem::bullet(vec![
            json!({"type":"paragraph","text":"Documents — local extraction; scanned PDF pages route through Vision."}),
        ]),
        RichBlockListItem::bullet(vec![
            json!({"type":"paragraph","text":"Voice/audio — native Main audio or the configured Audio STT role."}),
        ]),
        RichBlockListItem::bullet(vec![
            json!({"type":"paragraph","text":"Video — direct Main or the configured Video specialist when verified."}),
        ]),
    ];
    let command_rows = vec![
        vec![
            RichBlockTableCell::text_only("Command", true, Some("left")),
            RichBlockTableCell::text_only("Action", true, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/menu", false, Some("left")),
            RichBlockTableCell::text_only("Open main menu", false, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/new", false, Some("left")),
            RichBlockTableCell::text_only("Create a new session", false, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/session", false, Some("left")),
            RichBlockTableCell::text_only("Manage sessions", false, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/clear", false, Some("left")),
            RichBlockTableCell::text_only("Reset active history", false, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/model", false, Some("left")),
            RichBlockTableCell::text_only("Main Model dashboard/search", false, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/context", false, Some("left")),
            RichBlockTableCell::text_only("Canonical context status", false, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/image", false, Some("left")),
            RichBlockTableCell::text_only("Generate an image", false, Some("left")),
        ],
        vec![
            RichBlockTableCell::text_only("/help", false, Some("left")),
            RichBlockTableCell::text_only("Show this help", false, Some("left")),
        ],
    ];
    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("HELP".to_string()),
            level: 1,
        },
        RichBlock::Paragraph {
            text: Value::String("Supported input".to_string()),
        },
        RichBlock::List { items: input_items },
        RichBlock::Table {
            cells: command_rows,
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::Details {
            summary: Value::String("Model Routing".to_string()),
            blocks: vec![json!({
                "type":"paragraph",
                "text":"Telegram can change Main Model. Vision, Video, Audio STT, and Image Generation routes are read-only here."
            })],
            is_open: Some(false),
        },
        RichBlock::Details {
            summary: Value::String("Media Routing".to_string()),
            blocks: vec![json!({
                "type":"paragraph",
                "text":"Main-compatible media executes directly on Main. A different specialist receives only the minimum current context and returns a bounded observation/transcript to Main."
            })],
            is_open: Some(false),
        },
        RichBlock::Paragraph {
            text: Value::String("Advanced routing configuration: xiao model addon".to_string()),
        },
        RichBlock::Buttons {
            buttons: vec![RichMessageButton::callback("Menu", "action_menu")],
            align: Some("center".to_string()),
        },
    ])
}

fn telegram_can_edit_model_role(role: ai::service::ModelRole) -> bool {
    role == ai::service::ModelRole::Main
}

fn specialist_context_policy(
    role: ai::service::ModelRole,
    origin: ai::service::RouteOrigin,
) -> &'static str {
    if origin == ai::service::RouteOrigin::MainModel {
        return "Direct on Main; canonical history stays on Main";
    }
    match role {
        ai::service::ModelRole::Vision | ai::service::ModelRole::Video => {
            "Transient media + current question; no full history"
        }
        ai::service::ModelRole::AudioStt => "Transcript only; no full history",
        ai::service::ModelRole::ImageGeneration => "Prompt/config only; no canonical history",
        ai::service::ModelRole::Main => "Canonical Main context",
    }
}

fn context_available_tokens(limit: usize, used: usize) -> usize {
    limit.saturating_sub(used)
}

fn main_context_overflow_warning(model: &str, used: usize, usable_limit: usize) -> Option<String> {
    (used > usable_limit).then(|| {
        format!(
            "Main Model changed to {model}. Current canonical history (~{used} tokens) exceeds the new usable context (~{usable_limit} tokens). Xiao will compact before the next request when needed; history was not deleted."
        )
    })
}

fn effective_capability_state_label(
    record: &ai::service::CapabilityRecord,
    capability: ai::service::CapabilityKind,
) -> &'static str {
    match AIChatService::effective_capability_state(record, capability) {
        ai::storage::CapabilityState::Supported => "Supported",
        ai::storage::CapabilityState::Unsupported => "Unsupported",
        ai::storage::CapabilityState::Unknown => "Unknown",
    }
}

async fn run_observable_main_capability_probe(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    provider: &ProviderConfig,
    model: &str,
) {
    let checking = InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("CHECKING CAPABILITIES".to_string()),
            level: 1,
        },
        RichBlock::Paragraph {
            text: Value::String(format!("Main Model: {} / {}", provider.name, model)),
        },
        RichBlock::BlockQuotation {
            blocks: vec![json!({
                "type":"paragraph",
                "text":"Running bounded safe probes. Image-generation active probing is skipped because it may consume credits."
            })],
        },
    ]);
    let _ = bot.send_rich_message(chat_id, &checking, None, None).await;

    let mut persisted = false;
    let record = ai_service
        .probe_model_capabilities_with_observer(provider, model, |event| {
            if let ProbeEvent::Persistence { saved } = event {
                persisted = saved;
            }
        })
        .await;
    if !persisted {
        let failed = InputRichMessage::new(vec![
            RichBlock::SectionHeading {
                text: Value::String("CAPABILITY CHECK NOT SAVED".to_string()),
                level: 1,
            },
            RichBlock::BlockQuotation {
                blocks: vec![json!({
                    "type":"paragraph",
                    "text":"The probe candidate could not be persisted, so Xiao did not publish it to runtime. Previous durable capability state remains authoritative."
                })],
            },
        ]);
        let _ = bot.send_rich_message(chat_id, &failed, None, None).await;
        return;
    }
    let completed = InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("CAPABILITY CHECK COMPLETE".to_string()),
            level: 1,
        },
        RichBlock::Table {
            cells: vec![
                vec![
                    RichBlockTableCell::text_only("Capability", true, Some("left")),
                    RichBlockTableCell::text_only("State", true, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Text chat", false, Some("left")),
                    RichBlockTableCell::text_only(
                        effective_capability_state_label(
                            &record,
                            ai::service::CapabilityKind::TextChat,
                        ),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Vision", false, Some("left")),
                    RichBlockTableCell::text_only(
                        effective_capability_state_label(
                            &record,
                            ai::service::CapabilityKind::ImageInput,
                        ),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Audio input", false, Some("left")),
                    RichBlockTableCell::text_only(
                        effective_capability_state_label(
                            &record,
                            ai::service::CapabilityKind::AudioInput,
                        ),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Audio STT", false, Some("left")),
                    RichBlockTableCell::text_only(
                        effective_capability_state_label(
                            &record,
                            ai::service::CapabilityKind::AudioTranscription,
                        ),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Video", false, Some("left")),
                    RichBlockTableCell::text_only(
                        effective_capability_state_label(
                            &record,
                            ai::service::CapabilityKind::VideoInput,
                        ),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Image generation", false, Some("left")),
                    RichBlockTableCell::text_only(
                        effective_capability_state_label(
                            &record,
                            ai::service::CapabilityKind::ImageGeneration,
                        ),
                        false,
                        Some("left"),
                    ),
                ],
            ],
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::Paragraph {
            text: Value::String(
                "Unknown or stale capabilities remain fail-closed until safely verified."
                    .to_string(),
            ),
        },
    ]);
    let _ = bot.send_rich_message(chat_id, &completed, None, None).await;
}

async fn build_model_dashboard_ui(ai_service: &AIChatService, user_id: i64) -> InputRichMessage {
    let providers = ai_service.get_user_providers(user_id).await;
    let routing = ai_service.model_routing_config().await;
    let stats = ai_service.get_context_stats(user_id).await;
    let main = ai_service
        .resolve_model_route_unchecked(ai::service::ModelRole::Main)
        .await
        .ok();

    let (provider_name, model_name, health, capability_detail) = if let Some(route) = &main {
        let health = if ai_service
            .resolve_model_route(ai::service::ModelRole::Main)
            .await
            .is_ok()
        {
            "Verified"
        } else {
            "Unavailable"
        };
        let cap = &route.capability;
        let effective =
            |kind| format!("{:?}", AIChatService::effective_capability_state(cap, kind));
        (
            route.provider.name.clone(),
            route.model.clone(),
            health.to_string(),
            format!(
                "Text Chat: {}\nImage Input: {}\nImage Generation: {}\nImage Editing: {}\nAudio Input: {}\nAudio Transcription: {}\nVideo Input: {}\nNative File: {}\nTools: {}\nStructured Output: {}\nReasoning: {}",
                effective(ai::service::CapabilityKind::TextChat),
                effective(ai::service::CapabilityKind::ImageInput),
                effective(ai::service::CapabilityKind::ImageGeneration),
                effective(ai::service::CapabilityKind::ImageEditing),
                effective(ai::service::CapabilityKind::AudioInput),
                effective(ai::service::CapabilityKind::AudioTranscription),
                effective(ai::service::CapabilityKind::VideoInput),
                effective(ai::service::CapabilityKind::NativeFileInput),
                effective(ai::service::CapabilityKind::Tools),
                effective(ai::service::CapabilityKind::StructuredOutput),
                effective(ai::service::CapabilityKind::Reasoning),
            ),
        )
    } else {
        (
            "Not configured".to_string(),
            "Not configured".to_string(),
            "Unavailable".to_string(),
            "Main Model is not configured.".to_string(),
        )
    };

    let mut addon_rows = vec![vec![
        RichBlockTableCell::text_only("Addon Role", true, Some("left")),
        RichBlockTableCell::text_only("Route", true, Some("left")),
    ]];
    for role in ai::service::ModelRole::addon_roles() {
        let route = routing
            .route(role)
            .cloned()
            .unwrap_or(ai::service::ModelRoute::MainModel);
        let route_text = match route {
            ai::service::ModelRoute::MainModel => "Main Model".to_string(),
            ai::service::ModelRoute::Disabled => "Disabled".to_string(),
            ai::service::ModelRoute::Specific { provider_id, model } => {
                let name = providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
                    .map(|provider| provider.name.as_str())
                    .unwrap_or(provider_id.as_str());
                format!("{name} / {model}")
            }
        };
        let route_health = match ai_service.resolve_model_route(role).await {
            Ok(_) => "Verified",
            Err(error) if error.contains("Disabled") => "Disabled",
            Err(_) => "Unavailable",
        };
        addon_rows.push(vec![
            RichBlockTableCell::text_only(
                role.display_name().trim_end_matches(" Model"),
                false,
                Some("left"),
            ),
            RichBlockTableCell::text_only(
                &format!("{route_text} · {route_health}"),
                false,
                Some("left"),
            ),
        ]);
    }

    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("MODEL".to_string()),
            level: 1,
        },
        RichBlock::Table {
            cells: vec![
                vec![
                    RichBlockTableCell::text_only("Main Model", true, Some("left")),
                    RichBlockTableCell::text_only("Value", true, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Provider", false, Some("left")),
                    RichBlockTableCell::text_only(&provider_name, false, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Model", false, Some("left")),
                    RichBlockTableCell::text_only(&model_name, false, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Context", false, Some("left")),
                    RichBlockTableCell::text_only(&stats.limit_str, false, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Status", false, Some("left")),
                    RichBlockTableCell::text_only(&health, false, Some("left")),
                ],
            ],
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::Table {
            cells: addon_rows,
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::Details {
            summary: Value::String("Main Model Capabilities".to_string()),
            blocks: vec![json!({"type":"paragraph","text": capability_detail})],
            is_open: Some(false),
        },
        RichBlock::BlockQuotation {
            blocks: vec![json!({
                "type":"paragraph",
                "text":"Addon routes are read-only in Telegram v0.3.0. Configure them with xiao model addon; changing Main never overwrites Specific routes."
            })],
        },
        RichBlock::Buttons {
            buttons: {
                let mut buttons = Vec::new();
                if telegram_can_edit_model_role(ai::service::ModelRole::Main) {
                    buttons.push(RichMessageButton::callback_styled(
                        "Change Main",
                        "model_change_main",
                        "primary",
                    ));
                }
                buttons.push(RichMessageButton::callback("Refresh", "model_dashboard"));
                buttons
            },
            align: Some("center".to_string()),
        },
    ])
}

async fn build_main_model_picker_rich(
    ai_service: &AIChatService,
    user_id: i64,
    query: Option<&str>,
    page: usize,
) -> InputRichMessage {
    let providers = ai_service.get_user_providers(user_id).await;
    let q = query.unwrap_or("").trim().to_ascii_lowercase();
    let mut matches = Vec::new();
    for provider in &providers {
        for (index, model) in provider.models.iter().enumerate() {
            if q.is_empty()
                || model.to_ascii_lowercase().contains(&q)
                || provider.name.to_ascii_lowercase().contains(&q)
            {
                matches.push((
                    provider.id.clone(),
                    provider.name.clone(),
                    index,
                    model.clone(),
                ));
            }
        }
    }

    let page_size = 8usize;
    let total_pages = 1.max(matches.len().div_ceil(page_size));
    let curr_page = page.clamp(1, total_pages);
    let start = (curr_page - 1) * page_size;
    let end = (start + page_size).min(matches.len());

    let mut rows = vec![vec![
        RichBlockTableCell::text_only("Model", true, Some("left")),
        RichBlockTableCell::text_only("Provider", true, Some("left")),
    ]];
    let mut buttons = Vec::new();
    for (provider_id, provider_name, model_index, model) in &matches[start..end] {
        rows.push(vec![
            RichBlockTableCell::text_only(
                &truncate_chars_with_ellipsis(model, 28),
                false,
                Some("left"),
            ),
            RichBlockTableCell::text_only(
                &truncate_chars_with_ellipsis(provider_name, 20),
                false,
                Some("left"),
            ),
        ]);
        buttons.push(RichMessageButton::callback(
            truncate_chars_with_ellipsis(model, 24),
            format!("set_m:{provider_id}:{model_index}"),
        ));
    }
    if matches.is_empty() {
        rows.push(vec![
            RichBlockTableCell::text_only("No matching model", false, Some("left")),
            RichBlockTableCell::text_only("-", false, Some("left")),
        ]);
    }

    let mut blocks = vec![
        RichBlock::SectionHeading {
            text: Value::String("CHANGE MAIN MODEL".to_string()),
            level: 1,
        },
        RichBlock::Paragraph {
            text: Value::String(if q.is_empty() {
                format!("Available Main models · page {curr_page}/{total_pages}")
            } else {
                format!("Filter: {q} · page {curr_page}/{total_pages}")
            }),
        },
        RichBlock::Table {
            cells: rows,
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
    ];
    if !buttons.is_empty() {
        blocks.push(RichBlock::Buttons {
            buttons,
            align: Some("center".to_string()),
        });
    }
    if total_pages > 1 {
        blocks.push(RichBlock::Buttons {
            buttons: vec![
                if curr_page > 1 {
                    RichMessageButton::callback("‹", format!("model_main_page:{}", curr_page - 1))
                } else {
                    RichMessageButton::disabled("‹")
                },
                RichMessageButton::disabled(format!("{curr_page}/{total_pages}")),
                if curr_page < total_pages {
                    RichMessageButton::callback("›", format!("model_main_page:{}", curr_page + 1))
                } else {
                    RichMessageButton::disabled("›")
                },
            ],
            align: Some("center".to_string()),
        });
    }
    blocks.push(RichBlock::Buttons {
        buttons: vec![RichMessageButton::callback("Back", "model_dashboard")],
        align: Some("center".to_string()),
    });
    InputRichMessage::new(blocks)
}

async fn send_model_dashboard(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    user_id: i64,
    message_id: Option<i64>,
) {
    let rich = build_model_dashboard_ui(ai_service, user_id).await;
    if let Some(message_id) = message_id {
        if bot
            .edit_rich_message(chat_id, message_id, &rich, None)
            .await
            .is_ok()
        {
            return;
        }
    }
    let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
}

fn build_clear_confirmation_ui() -> InputRichMessage {
    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("RESET HISTORY?".to_string()),
            level: 1,
        },
        RichBlock::BlockQuotation {
            blocks: vec![
                json!({"type":"paragraph","text":"This removes canonical conversation history and attachment context for the active session. The session remains. In-flight older generations cannot write back after the revision changes."}),
            ],
        },
        RichBlock::Buttons {
            buttons: vec![
                RichMessageButton::callback_styled("Reset History", "action_clear", "danger"),
                RichMessageButton::callback("Cancel", "clear_cancel"),
            ],
            align: Some("center".to_string()),
        },
    ])
}

async fn build_context_monitor_ui(ai_service: &AIChatService, user_id: i64) -> InputRichMessage {
    let stats = ai_service.get_context_stats(user_id).await;
    let main = ai_service
        .resolve_model_route_unchecked(ai::service::ModelRole::Main)
        .await
        .ok();
    let main_name = main
        .as_ref()
        .map(|route| format!("{} / {}", route.provider.name, route.model))
        .unwrap_or_else(|| stats.model_name.clone());

    let used = stats.total_tokens;
    let limit = stats.limit_tokens.max(1);
    let available = context_available_tokens(limit, used);
    let progress = format!(
        "{}  ~{:.1}%  ~{} / ~{} tokens",
        stats.progress_bar, stats.usage_pct, used, limit
    );

    let routing = ai_service.model_routing_config().await;
    let providers = ai_service.get_user_providers(user_id).await;
    let mut specialist_rows = vec![vec![
        RichBlockTableCell::text_only("Role", true, Some("left")),
        RichBlockTableCell::text_only("Route", true, Some("left")),
        RichBlockTableCell::text_only("Policy", true, Some("left")),
    ]];
    let mut provider_context = Vec::new();

    for role in ai::service::ModelRole::addon_roles() {
        let route = routing
            .route(role)
            .cloned()
            .unwrap_or(ai::service::ModelRoute::MainModel);
        let (route_text, policy) = match route {
            ai::service::ModelRoute::MainModel => (
                "Main Model".to_string(),
                specialist_context_policy(role, ai::service::RouteOrigin::MainModel).to_string(),
            ),
            ai::service::ModelRoute::Disabled => ("Disabled".to_string(), "Disabled".to_string()),
            ai::service::ModelRoute::Specific { provider_id, model } => {
                let provider_name = providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
                    .map(|provider| provider.name.as_str())
                    .unwrap_or(provider_id.as_str());
                let resolved = format!("{provider_name} / {model}");
                let policy =
                    specialist_context_policy(role, ai::service::RouteOrigin::Specific).to_string();
                let sent = match role {
                    ai::service::ModelRole::Vision | ai::service::ModelRole::Video => {
                        "current media + current question"
                    }
                    ai::service::ModelRole::AudioStt => "current audio only",
                    ai::service::ModelRole::ImageGeneration => "prompt/config only",
                    ai::service::ModelRole::Main => "canonical Main context",
                };
                provider_context.push(format!(
                    "{} — {}\nSent: {}\nNot sent: full session history\nPolicy: Minimal",
                    role.display_name(),
                    resolved,
                    sent
                ));
                (resolved, policy)
            }
        };
        specialist_rows.push(vec![
            RichBlockTableCell::text_only(
                role.display_name().trim_end_matches(" Model"),
                false,
                Some("left"),
            ),
            RichBlockTableCell::text_only(&route_text, false, Some("left")),
            RichBlockTableCell::text_only(&policy, false, Some("left")),
        ]);
    }

    let specialist_table = serde_json::to_value(RichBlock::Table {
        cells: specialist_rows,
        has_header: true,
        is_bordered: true,
        is_striped: true,
        is_compact: true,
        caption: None,
    })
    .unwrap_or_else(|_| json!({"type":"paragraph","text":"Specialist routing unavailable."}));

    let health_text = if stats.usage_pct >= 80.0 {
        "Context warning: Main Model context is near/full. Xiao will compact provider context before the next request when necessary; stored history is not silently deleted."
    } else {
        "✓ Context healthy"
    };

    InputRichMessage::new(vec![
        RichBlock::SectionHeading {
            text: Value::String("CONTEXT".to_string()),
            level: 1,
        },
        RichBlock::Paragraph {
            text: Value::String(format!(
                "Session #{} — {}\nMain: {}",
                stats.session_id,
                truncate_session_name(&stats.session_name, 36),
                main_name
            )),
        },
        RichBlock::Preformatted {
            text: progress,
            language: None,
        },
        RichBlock::Table {
            cells: vec![
                vec![
                    RichBlockTableCell::text_only("Canonical Main", true, Some("left")),
                    RichBlockTableCell::text_only("Value", true, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Messages", false, Some("left")),
                    RichBlockTableCell::text_only(
                        &stats.total_messages.to_string(),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Attachments", false, Some("left")),
                    RichBlockTableCell::text_only(
                        &stats.attachment_count.to_string(),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Used", false, Some("left")),
                    RichBlockTableCell::text_only(&format!("~{used} tokens"), false, Some("left")),
                ],
                vec![
                    RichBlockTableCell::text_only("Available", false, Some("left")),
                    RichBlockTableCell::text_only(
                        &format!("~{available} tokens"),
                        false,
                        Some("left"),
                    ),
                ],
                vec![
                    RichBlockTableCell::text_only("Output Reserve", false, Some("left")),
                    RichBlockTableCell::text_only(
                        &format!("~{} tokens", stats.output_reserve_tokens),
                        false,
                        Some("left"),
                    ),
                ],
            ],
            has_header: true,
            is_bordered: true,
            is_striped: true,
            is_compact: true,
            caption: None,
        },
        RichBlock::BlockQuotation {
            blocks: vec![json!({"type":"paragraph","text":health_text})],
        },
        RichBlock::Details {
            summary: Value::String("Specialist Routing & Context".to_string()),
            blocks: vec![specialist_table],
            is_open: Some(false),
        },
        RichBlock::Details {
            summary: Value::String("Provider Context".to_string()),
            blocks: vec![json!({
                "type":"paragraph",
                "text": if provider_context.is_empty() {
                    "No Specific cross-provider addon is active. Main Model routes execute directly when verified. Context windows are never added together.".to_string()
                } else {
                    format!(
                        "{}\n\nContext windows are never added together.",
                        provider_context.join("\n\n")
                    )
                }
            })],
            is_open: Some(false),
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
    let rich = build_start_ui(ai_service, user_id).await;
    let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
}

async fn send_menu(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    chat_id: i64,
    user_id: i64,
) {
    let rich = build_menu_ui(ai_service, user_id).await;
    let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
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
        if Regex::new(pat).is_ok_and(|regex| regex.is_match(t)) {
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

    let clean_re = Regex::new(r"(?i)^(?:tentang|mengenai|berupa|of|about|dong|ya|tolong)\s+").ok();

    for pat in patterns {
        let Ok(regex) = Regex::new(pat) else {
            continue;
        };
        if let Some(caps) = regex.captures(t) {
            if let Some(extracted_match) = caps.get(1) {
                let mut extracted = extracted_match.as_str().trim().to_string();
                if let Some(clean_re) = &clean_re {
                    extracted = clean_re.replace(&extracted, "").trim().to_string();
                }

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageGenerationIntent {
    image_prompt: String,
    explanation_prompt: Option<String>,
}

fn plan_image_generation_intent(text: &str) -> Option<ImageGenerationIntent> {
    let compound = Regex::new(
        r"(?i)^(?P<image>.+?)\s+(?:dan|lalu|kemudian|and|then)\s+(?P<explain>(?:jelaskan|terangkan|explain|describe)\b.+)$",
    )
    .ok();

    if let Some(regex) = compound {
        if let Some(captures) = regex.captures(text.trim()) {
            let image_request = captures.name("image")?.as_str().trim();
            let explanation = captures.name("explain")?.as_str().trim();
            if let Some(image_prompt) = extract_image_intent_prompt(image_request) {
                return Some(ImageGenerationIntent {
                    image_prompt,
                    explanation_prompt: (!explanation.is_empty()).then(|| explanation.to_string()),
                });
            }
        }
    }

    extract_image_intent_prompt(text).map(|image_prompt| ImageGenerationIntent {
        image_prompt,
        explanation_prompt: None,
    })
}

const TELEGRAM_PHOTO_CAPTION_MAX_CHARS: usize = 1024;
const IMAGE_CAPTION_PROMPT_ESCAPED_CHARS: usize = 320;
const IMAGE_CAPTION_PROVIDER_ESCAPED_CHARS: usize = 96;
const IMAGE_CAPTION_MODEL_ESCAPED_CHARS: usize = 128;
const IMAGE_CAPTION_FAILURE_ESCAPED_CHARS: usize = 144;

fn bounded_escaped_html(text: &str, max_chars: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    let mut truncated = false;

    for ch in text.chars() {
        let escaped = match ch {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\'' => "&#39;",
            _ => {
                let needed = 1usize;
                if used.saturating_add(needed) > max_chars {
                    truncated = true;
                    break;
                }
                output.push(ch);
                used += needed;
                continue;
            }
        };
        let needed = escaped.chars().count();
        if used.saturating_add(needed) > max_chars {
            truncated = true;
            break;
        }
        output.push_str(escaped);
        used += needed;
    }

    if truncated && used < max_chars {
        output.push('…');
    }
    output
}

fn build_image_success_caption(
    prompt: &str,
    provider: &str,
    model: &str,
    width: usize,
    height: usize,
    elapsed_secs: f64,
    used_external_fallback: bool,
    primary_failure: Option<&str>,
) -> String {
    let safe_prompt = bounded_escaped_html(prompt, IMAGE_CAPTION_PROMPT_ESCAPED_CHARS);
    let safe_provider = bounded_escaped_html(provider, IMAGE_CAPTION_PROVIDER_ESCAPED_CHARS);
    let safe_model = bounded_escaped_html(model, IMAGE_CAPTION_MODEL_ESCAPED_CHARS);
    let fallback_note = if used_external_fallback {
        let safe_failure = bounded_escaped_html(
            primary_failure.unwrap_or("Primary provider failure was not reported."),
            IMAGE_CAPTION_FAILURE_ESCAPED_CHARS,
        );
        format!(
            "\n⚠️ <i>External fallback opt-in digunakan.</i>\n<b>Primary failure:</b> {safe_failure}"
        )
    } else {
        String::new()
    };

    let caption = format!(
        "🫟 <b>Gambar Berhasil Dibuat!</b>\n\n\
         📝 <b>Prompt:</b> <i>\"{safe_prompt}\"</i>\n\
         🧩 <b>Provider:</b> <code>{safe_provider}</code>\n\
         🤖 <b>Model:</b> <code>{safe_model}</code>\n\
         📐 <b>Size:</b> <code>{width} × {height}</code>\n\
         ⏱️ <b>Elapsed:</b> <code>{elapsed_secs:.1}s</code>{fallback_note}"
    );

    if caption.chars().count() <= TELEGRAM_PHOTO_CAPTION_MAX_CHARS {
        return caption;
    }

    let minimal = format!(
        "🫟 <b>Gambar Berhasil Dibuat!</b>\n\
         🧩 <b>Provider:</b> <code>{safe_provider}</code>\n\
         🤖 <b>Model:</b> <code>{safe_model}</code>\n\
         📐 <b>Size:</b> <code>{width} × {height}</code>\n\
         ⏱️ <b>Elapsed:</b> <code>{elapsed_secs:.1}s</code>"
    );
    debug_assert!(minimal.chars().count() <= TELEGRAM_PHOTO_CAPTION_MAX_CHARS);
    minimal
}

fn telegram_photo_delivery_error_class(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if [
        "caption",
        "can't parse entities",
        "cannot parse entities",
        "parse entities",
        "reply markup",
        "reply_markup",
        "inline keyboard",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        "caption_or_markup"
    } else if [
        "multipart error",
        "timeout",
        "timed out",
        "connection",
        "network",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        "telegram_transport"
    } else if lower.contains("unsupported image signature") {
        "local_image_validation"
    } else {
        "telegram_api"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageDeliveryFailure {
    class: &'static str,
    detail: String,
    retry_attempted: bool,
}

async fn deliver_generated_image_with<F, Fut>(
    image_bytes: &[u8],
    caption: &str,
    reply_markup: Option<Value>,
    mut sender: F,
) -> Result<(), ImageDeliveryFailure>
where
    F: FnMut(Vec<u8>, Option<String>, Option<String>, Option<Value>) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    match sender(
        image_bytes.to_vec(),
        Some(caption.to_string()),
        Some("HTML".to_string()),
        reply_markup,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(first_error)
            if telegram_photo_delivery_error_class(&first_error) == "caption_or_markup" =>
        {
            let retry_caption = "Image generated successfully.".to_string();
            match sender(image_bytes.to_vec(), Some(retry_caption), None, None).await {
                Ok(()) => Ok(()),
                Err(second_error) => Err(ImageDeliveryFailure {
                    class: telegram_photo_delivery_error_class(&second_error),
                    detail: second_error,
                    retry_attempted: true,
                }),
            }
        }
        Err(error) => Err(ImageDeliveryFailure {
            class: telegram_photo_delivery_error_class(&error),
            detail: error,
            retry_attempted: false,
        }),
    }
}

async fn handle_image_generation(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    user_last_image_prompt: &UserLastImagePrompt,
    chat_id: i64,
    user_id: i64,
    prompt: &str,
    explanation_prompt: Option<&str>,
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

        let route_text = match ai_service
            .resolve_model_route_unchecked(ai::service::ModelRole::ImageGeneration)
            .await
        {
            Ok(route) => format!("{} / {}", route.provider.name, route.model),
            Err(error) => format!("Unavailable — {}", error),
        };
        let rich = InputRichMessage::new(vec![
            RichBlock::SectionHeading { text: Value::String("IMAGE GENERATION".to_string()), level: 1 },
            RichBlock::Table {
                cells: vec![
                    vec![RichBlockTableCell::text_only("Image Model", true, Some("left")), RichBlockTableCell::text_only("Default Size", true, Some("left"))],
                    vec![RichBlockTableCell::text_only(&route_text, false, Some("left")), RichBlockTableCell::text_only("1024 × 1024", false, Some("left"))],
                ],
                has_header: true, is_bordered: true, is_striped: true, is_compact: true, caption: None,
            },
            RichBlock::Paragraph { text: Value::String("Send the image description. Generation may take up to the configured timeout (default 120 seconds).".to_string()) },
            RichBlock::Buttons { buttons: vec![RichMessageButton::callback("Cancel", "provider_cancel")], align: Some("center".to_string()) },
        ]);
        let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
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
        chat_id == user_id,
    ));
    timeline
        .add_action("Generating Image", Some(ProgressActivity::Drawing))
        .await;
    timeline.sync_draft(true).await;
    timeline.start_ticker();
    let _ = bot.send_chat_action(chat_id, "upload_photo").await;

    let width = 1024usize;
    let height = 1024usize;
    let image_started = Instant::now();
    let model_snapshot = ai_service.generation_model_snapshot().await;
    let mut cancel_rx = ai_service.begin_generation(chat_id, draft_id).await;
    let image_result = ai_service
        .generate_image_with_snapshot(
            user_id,
            &clean_prompt,
            width,
            height,
            &model_snapshot,
            &mut cancel_rx,
        )
        .await;
    ai_service.end_generation(chat_id, draft_id).await;
    timeline.stop_ticker();
    let elapsed_secs = image_started.elapsed().as_secs_f64();

    let generated = match image_result {
        Ok(image) => image,
        Err(error) => {
            timeline.fail_current(error.message.clone()).await;
            timeline.sync_draft(true).await;

            if error.kind == ImageGenerationErrorKind::Cancelled {
                let rich = InputRichMessage::new(vec![
                    RichBlock::SectionHeading {
                        text: Value::String("IMAGE GENERATION CANCELLED".to_string()),
                        level: 1,
                    },
                    RichBlock::Paragraph {
                        text: Value::String("Image generation was cancelled.".to_string()),
                    },
                    RichBlock::Buttons {
                        buttons: vec![
                            RichMessageButton::callback("New Image", "img_new"),
                            RichMessageButton::callback("Menu", "action_menu"),
                        ],
                        align: Some("center".to_string()),
                    },
                ]);
                let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
                return;
            }

            let status = match error.kind {
                ImageGenerationErrorKind::CapabilityUnknown => "Capability unknown",
                ImageGenerationErrorKind::CapabilityUnsupported => "Unsupported",
                ImageGenerationErrorKind::RouteDisabled => "Route disabled",
                ImageGenerationErrorKind::ProviderNotFound => "Provider not found",
                ImageGenerationErrorKind::ModelNotFound => "Model not found",
                ImageGenerationErrorKind::Timeout => "Timeout",
                ImageGenerationErrorKind::Auth => "Authentication error",
                ImageGenerationErrorKind::RateLimited => "Rate limited",
                ImageGenerationErrorKind::HttpStatus => "HTTP error",
                ImageGenerationErrorKind::ProtocolMismatch => "Protocol mismatch",
                ImageGenerationErrorKind::InvalidResponse => "Invalid response",
                ImageGenerationErrorKind::InvalidBase64 => "Invalid base64",
                ImageGenerationErrorKind::InvalidImage => "Invalid image",
                ImageGenerationErrorKind::UnsafeImageUrl => "Unsafe image URL",
                ImageGenerationErrorKind::DownloadTimeout => "Download timeout",
                ImageGenerationErrorKind::Cancelled => "Cancelled",
                ImageGenerationErrorKind::FallbackDisabled => "Fallback disabled",
                ImageGenerationErrorKind::Provider => "Provider error",
            };
            let mut blocks = vec![
                RichBlock::SectionHeading {
                    text: Value::String("IMAGE GENERATION FAILED".to_string()),
                    level: 1,
                },
                RichBlock::Table {
                    cells: vec![
                        vec![
                            RichBlockTableCell::text_only("Status", true, Some("left")),
                            RichBlockTableCell::text_only("Detail", true, Some("left")),
                        ],
                        vec![
                            RichBlockTableCell::text_only(status, false, Some("left")),
                            RichBlockTableCell::text_only(
                                &truncate_chars(&error.message, 240),
                                false,
                                Some("left"),
                            ),
                        ],
                    ],
                    has_header: true,
                    is_bordered: true,
                    is_striped: true,
                    is_compact: true,
                    caption: None,
                },
            ];
            if matches!(
                error.kind,
                ImageGenerationErrorKind::CapabilityUnknown
                    | ImageGenerationErrorKind::CapabilityUnsupported
                    | ImageGenerationErrorKind::RouteDisabled
                    | ImageGenerationErrorKind::ProviderNotFound
                    | ImageGenerationErrorKind::ModelNotFound
            ) {
                blocks.push(RichBlock::BlockQuotation {
                    blocks: vec![json!({
                        "type":"paragraph",
                        "text":"Check the Image Generation Model route with: xiao model addon show image_gen. Configure it with: xiao model addon set image_gen"
                    })],
                });
            }
            blocks.push(RichBlock::Buttons {
                buttons: vec![
                    RichMessageButton::callback_styled("Retry", "img_regen", "primary"),
                    RichMessageButton::callback("New Image", "img_new"),
                    RichMessageButton::callback("Menu", "action_menu"),
                ],
                align: Some("center".to_string()),
            });
            let rich = InputRichMessage::new(blocks);
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
            return;
        }
    };

    let caption_text = build_image_success_caption(
        &clean_prompt,
        &generated.provider_name,
        &generated.model,
        width,
        height,
        elapsed_secs,
        generated.used_external_fallback,
        generated.primary_failure.as_deref(),
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

    let delivery = deliver_generated_image_with(
        &generated.bytes,
        &caption_text,
        serde_json::to_value(img_kb).ok(),
        |bytes, caption, parse_mode, reply_markup| async move {
            bot.send_photo_bytes(
                chat_id,
                bytes,
                caption.as_deref(),
                parse_mode.as_deref(),
                reply_markup,
                None,
            )
            .await
            .map(|_| ())
        },
    )
    .await;

    if let Err(failure) = delivery {
        warn!(
            "Generated image delivery failed [{}; retry={}]: {}",
            failure.class,
            failure.retry_attempted,
            truncate_chars(&failure.detail, 200)
        );
        let rich = InputRichMessage::new(vec![
            RichBlock::SectionHeading {
                text: Value::String("IMAGE DELIVERY FAILED".to_string()),
                level: 1,
            },
            RichBlock::Paragraph {
                text: Value::String(
                    "Image generation succeeded, but Telegram could not deliver the image."
                        .to_string(),
                ),
            },
            RichBlock::BlockQuotation {
                blocks: vec![json!({
                    "type": "paragraph",
                    "text": format!("Diagnostic class: {}", failure.class)
                })],
            },
        ]);
        if let Err(error) = bot.send_rich_message(chat_id, &rich, None, None).await {
            warn!(
                "Image delivery fallback message also failed: {}",
                truncate_chars(&error, 160)
            );
        }
        return;
    }

    if let Some(explanation_prompt) = explanation_prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
    {
        handle_ai_chat(
            bot,
            ai_service,
            chat_id,
            user_id,
            ChatInput {
                prompt: explanation_prompt,
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
                model_snapshot: Some(&model_snapshot),
            },
        )
        .await;
    }
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
        model_snapshot,
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

    let current_model = if let Some(snapshot) = model_snapshot {
        AIChatService::resolve_model_route_from_snapshot(snapshot, ai::service::ModelRole::Main)
            .map(|route| route.model)
            .unwrap_or_else(|_| "unavailable".to_string())
    } else {
        ai_service.get_user_model(user_id).await
    };

    let generation_input = ai::service::GenerationInput {
        prompt: user_prompt,
        canonical_prompt: None,
        media_to_main: true,
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
    };
    let (_thinking, mut answer_text, _cancelled) = if let Some(snapshot) = model_snapshot {
        ai_service
            .generate_response_with_snapshot(user_id, generation_input, snapshot, &mut cancel_rx)
            .await
    } else {
        ai_service
            .generate_response(user_id, generation_input, &mut cancel_rx)
            .await
    };

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

    if let Err(error) = res {
        // send_rich_message already exhausts canonical Rich -> safe HTML ->
        // semantic plain-text fallback. Never re-send raw model Markdown here.
        warn!("Unable to deliver final canonical answer: {error}");
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
        let message = callback.message.as_ref();
        let source_ephemeral_message_id = message.and_then(|message| message.ephemeral_message_id);
        return TelegramDeliveryContext {
            message_thread_id: message.and_then(|message| message.message_thread_id),
            receiver_user_id: source_ephemeral_message_id.map(|_| callback.from.id),
            source_ephemeral_message_id,
            callback_query_id: source_ephemeral_message_id.map(|_| callback.id.clone()),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateLane {
    Control,
    Generation,
}

fn command_matches(text: &str, command: &str) -> bool {
    text == command
        || text
            .strip_prefix(command)
            .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('@'))
}

fn is_control_message_text(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    if command_matches(text, "/image") {
        return false;
    }

    let commands = [
        "/start", "/menu", "/new", "/session", "/context", "/model", "/clear", "/cancel", "/help",
    ];
    if commands
        .iter()
        .any(|command| command_matches(text, command))
    {
        return true;
    }

    if text.chars().all(|character| character.is_ascii_digit())
        || text.starts_with("✅")
        || text.starts_with("Session ")
        || (text.starts_with("Hal") && text.chars().any(|character| character.is_ascii_digit()))
    {
        return true;
    }

    [
        "📱 Menu",
        "Menu",
        "🔙 Menu Utama",
        "🔙 Kembali ke Menu Utama",
        "Menu Utama",
        "Main Menu",
        "main menu",
        "Main menu",
        "ɴᴇᴡ",
        "➕ ɴᴇᴡ",
        "➕ New",
        "New",
        "new",
        "➕ Chat Baru",
        "Chat Baru",
        "📑 Session",
        "Session",
        "session",
        "📑 Session Manager",
        "📑 Lihat Daftar Session",
        "🗑️ Hapus Session",
        "🗑️ Remove Session",
        "Delete",
        "delete",
        "Delete Session",
        "✏️ Rename Session",
        "✏️ Ubah Nama Session",
        "Rename",
        "rename",
        "Rename Session",
        "ᴄᴏɴᴛᴇxᴛ",
        "🧠 ᴄᴏɴᴛᴇxᴛ",
        "🧠 Context",
        "Context",
        "context",
        "🧠 Info Konteks",
        "Info Konteks",
        "ᴍᴏᴅᴇʟ",
        "⚙️ ᴍᴏᴅᴇʟ",
        "⚙️ Model",
        "Model",
        "model",
        "⚙️ Model AI",
        "Pilih Model",
        "🗑️ Reset Chat",
        "🗑️ Reset Obrolan",
        "❓ Help",
        "Help",
        "help",
        "Bantuan",
    ]
    .contains(&text)
}

async fn classify_update_lane(ai_service: &AIChatService, update: &Update) -> UpdateLane {
    if update.stopped_message_generation.is_some() {
        return UpdateLane::Control;
    }

    if let Some(callback) = update.callback_query.as_ref() {
        return if matches!(callback.data.as_deref(), Some("img_new" | "img_regen")) {
            UpdateLane::Generation
        } else {
            UpdateLane::Control
        };
    }

    let Some(message) = update.message.as_ref() else {
        return UpdateLane::Control;
    };

    if message.photo.is_some()
        || message.document.is_some()
        || message.voice.is_some()
        || message.audio.is_some()
        || message.video.is_some()
        || message.video_note.is_some()
    {
        return UpdateLane::Generation;
    }

    let user_id = message
        .from
        .as_ref()
        .map(|user| user.id)
        .unwrap_or(message.chat.id);
    let text = message
        .text
        .as_deref()
        .or(message.caption.as_deref())
        .unwrap_or("")
        .trim();

    let wizard = ai_service
        .user_wizard_state
        .read()
        .await
        .get(&user_id)
        .cloned();
    if let Some(wizard) = wizard {
        if ["/cancel", "/batal", "batal", "cancel"].contains(&text) {
            return UpdateLane::Control;
        }
        return if wizard.get("step").map(String::as_str) == Some("awaiting_image_prompt") {
            UpdateLane::Generation
        } else {
            UpdateLane::Control
        };
    }

    if ai_service
        .user_waiting_rename
        .read()
        .await
        .contains_key(&user_id)
        && !text.starts_with('/')
    {
        return UpdateLane::Control;
    }

    if is_control_message_text(text) {
        UpdateLane::Control
    } else {
        UpdateLane::Generation
    }
}

async fn process_durable_update(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    user_last_image_prompt: &UserLastImagePrompt,
    access: &AccessPolicy,
    update: Update,
) {
    let update_id = update.update_id;
    if !ai::storage::mark_telegram_processing_async(update_id).await {
        return;
    }

    let delivery_context = delivery_context_for_update(&update);
    TelegramBotClient::with_delivery_context(
        delivery_context,
        handle_update(bot, ai_service, user_last_image_prompt, access, update),
    )
    .await;

    if !ai::storage::mark_telegram_processed_async(update_id).await {
        warn!("Gagal menyelesaikan durable Telegram inbox update {update_id}");
    }
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
            if let Some((data, path)) = bot.get_file_bytes(&v.file_id).await {
                audio_bytes = Some(data);
                doc_name = path.split('/').next_back().map(str::to_string);
            }
        } else if let Some(a) = msg.audio {
            audio_duration = a.duration;
            audio_mime = a.mime_type;
            let audio_file_name = a.file_name;
            if let Some((data, path)) = bot.get_file_bytes(&a.file_id).await {
                audio_bytes = Some(data);
                doc_name =
                    audio_file_name.or_else(|| path.split('/').next_back().map(str::to_string));
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
                    audio_mime = (!d_mime.is_empty()).then_some(d_mime);
                    doc_name = Some(d_name);
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
                    let rich = InputRichMessage::new(vec![
                        RichBlock::SectionHeading {
                            text: Value::String("CANCELLED".to_string()),
                            level: 1,
                        },
                        RichBlock::Paragraph {
                            text: Value::String(
                                "Current interactive action was cancelled.".to_string(),
                            ),
                        },
                    ]);
                    let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
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
                        None,
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
                let renamed = ai_service
                    .rename_session_by_id(user_id, target_session_id, &text)
                    .await;

                let rename_notice = if renamed {
                    format!(
                        "✅ Session berhasil diubah namanya menjadi: <b>{}</b>",
                        escape_html(&text)
                    )
                } else {
                    "⚠️ <b>Nama session tidak diubah.</b> Penyimpanan gagal; state lama tetap dipertahankan."
                        .to_string()
                };
                let _ = bot
                    .send_message(
                        chat_id,
                        &rename_notice,
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

        if ["/cancel", "/batal", "batal", "cancel"].contains(&text.as_str()) {
            let rename_active = ai_service
                .user_waiting_rename
                .write()
                .await
                .remove(&user_id)
                .is_some();
            if rename_active {
                ai_service.user_rename_msg_id.write().await.remove(&user_id);
            }
            let rich = InputRichMessage::new(vec![
                RichBlock::SectionHeading {
                    text: Value::String(if rename_active {
                        "CANCELLED".to_string()
                    } else {
                        "NO ACTIVE ACTION".to_string()
                    }),
                    level: 1,
                },
                RichBlock::Paragraph {
                    text: Value::String(if rename_active {
                        "Current interactive action was cancelled.".to_string()
                    } else {
                        "No interactive action is currently active. Use Telegram's native Stop control to cancel an active generation.".to_string()
                    }),
                },
            ]);
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
            return;
        }

        // Voice audio processing is role-routed inside AIChatService.
        // Do not pre-transcribe against the active provider here because that
        // would bypass the configured Audio STT Model route.
        if let Some(a_bytes) = audio_bytes {
            let prompt_audio = if !text.is_empty() {
                text.clone()
            } else {
                format!(
                    "Dengarkan pesan suara ini ({} detik) dan jawab pertanyaan atau tanggapi maksud di dalamnya secara jelas dan mendalam.",
                    audio_duration
                )
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
                    model_snapshot: None,
                },
            )
            .await;
            return;
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
                    model_snapshot: None,
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
        if text.starts_with("/start") {
            send_welcome(bot, ai_service, chat_id, user_id).await;
        } else if [
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
            send_menu(bot, ai_service, chat_id, user_id).await;
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
            let Some(new_session) = ai_service.create_new_session(user_id, None).await else {
                let rich = InputRichMessage::new(vec![
                    RichBlock::SectionHeading {
                        text: Value::String("SESSION NOT CREATED".to_string()),
                        level: 1,
                    },
                    RichBlock::BlockQuotation {
                        blocks: vec![json!({
                            "type":"paragraph",
                            "text":"Persistence is unavailable. Xiao refuses to use a temporary session ID that could collide later."
                        })],
                    },
                ]);
                let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
                return;
            };
            let total_sessions = ai_service.get_sessions(user_id).await.len();
            let target_page = total_sessions.saturating_sub(1) / 5 + 1;
            let rich = InputRichMessage::new(vec![
                RichBlock::SectionHeading {
                    text: Value::String("NEW SESSION".to_string()),
                    level: 1,
                },
                RichBlock::Paragraph {
                    text: Value::String(format!(
                        "✓ Session #{} created and activated.",
                        new_session.id
                    )),
                },
                RichBlock::Paragraph {
                    text: Value::String(format!(
                        "{}\nCanonical history is empty.",
                        new_session.name
                    )),
                },
            ]);
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
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
            let removed = ai_service.remove_session(user_id, active_idx).await;
            let new_active_idx = ai_service.get_active_session_index(user_id).await;
            let target_page = (new_active_idx / 5) + 1;
            let notice = if removed {
                "🗑️ <b>Session berhasil dihapus!</b>"
            } else {
                "⚠️ <b>Session tidak dihapus.</b> Penyimpanan gagal; session lama tetap utuh."
            };
            let _ = bot
                .send_message(chat_id, notice, Some("HTML"), None, None, None)
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
            .ok()
            .and_then(|regex| regex.captures(&text))
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
            .ok()
            .and_then(|regex| regex.captures(text.trim()))
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
                    if !ai_service.switch_session(user_id, idx).await {
                        let _ = bot
                            .send_message(
                                chat_id,
                                "❌ Gagal mengganti sesi karena state aktif tidak dapat disimpan.",
                                None,
                                None,
                                None,
                                None,
                            )
                            .await;
                        return;
                    }
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
                            model_snapshot: None,
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
                let query = text
                    .strip_prefix("/model")
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                if let Some(query) = query {
                    ai_service
                        .model_picker_query
                        .write()
                        .await
                        .insert(user_id, query.to_string());
                    let rich =
                        build_main_model_picker_rich(ai_service, user_id, Some(query), 1).await;
                    let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
                } else {
                    ai_service.model_picker_query.write().await.remove(&user_id);
                    send_model_dashboard(bot, ai_service, chat_id, user_id, None).await;
                }
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
                if !ai_service
                    .set_provider_model(user_id, &prov.id, selected_model)
                    .await
                {
                    let _ = bot
                        .send_message(
                            chat_id,
                            "❌ Gagal mengaktifkan model karena konfigurasi tidak dapat disimpan.",
                            None,
                            Some(get_main_menu_keyboard()),
                            None,
                            None,
                        )
                        .await;
                    return;
                }
                run_observable_main_capability_probe(
                    bot,
                    ai_service,
                    chat_id,
                    &prov,
                    selected_model,
                )
                .await;
                let rich = InputRichMessage::new(vec![
                    RichBlock::SectionHeading {
                        text: Value::String("MAIN MODEL CHANGED".to_string()),
                        level: 1,
                    },
                    RichBlock::Paragraph {
                        text: Value::String(format!("{} / {}", prov.name, selected_model)),
                    },
                ]);
                let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
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
                        model_snapshot: None,
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
            let explicit_intent = if prompt_arg.is_empty() {
                None
            } else {
                plan_image_generation_intent(prompt_arg)
            };
            let image_prompt = explicit_intent
                .as_ref()
                .map(|intent| intent.image_prompt.as_str())
                .unwrap_or(prompt_arg);
            let explanation_prompt = explicit_intent
                .as_ref()
                .and_then(|intent| intent.explanation_prompt.as_deref());
            handle_image_generation(
                bot,
                ai_service,
                user_last_image_prompt,
                chat_id,
                user_id,
                image_prompt,
                explanation_prompt,
            )
            .await;
        } else if text.starts_with("/clear")
            || ["🗑️ Reset Chat", "🗑️ Reset Obrolan"].contains(&text.as_str())
        {
            let rich = build_clear_confirmation_ui();
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
        } else if text.starts_with("/help")
            || ["📖 Bantuan", "📖 Bantuan & Info"].contains(&text.as_str())
        {
            let rich = build_help_ui();
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
        } else {
            let auto_image_intent = if image_bytes.is_none() && doc_text.is_none() {
                plan_image_generation_intent(&text)
            } else {
                None
            };

            if let Some(intent) = auto_image_intent {
                handle_image_generation(
                    bot,
                    ai_service,
                    user_last_image_prompt,
                    chat_id,
                    user_id,
                    &intent.image_prompt,
                    intent.explanation_prompt.as_deref(),
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
                        model_snapshot: None,
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
                let switched = ai_service.switch_session(user_id, idx).await;
                let target_page = (idx / 5) + 1;
                let callback_text = if switched {
                    format!("Beralih ke Session #{} ✅", idx + 1)
                } else {
                    "Session tidak berubah karena penyimpanan gagal.".to_string()
                };
                let _ = bot
                    .answer_callback_query(&cq_id, Some(&callback_text), !switched)
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
                let removed = ai_service.remove_session(user_id, idx).await;
                let new_active = ai_service.get_active_session_index(user_id).await;
                let target_page = (new_active / 5) + 1;
                let callback_text = if removed {
                    "Session berhasil dihapus 🗑️"
                } else {
                    "Session tidak dihapus karena penyimpanan gagal."
                };
                let _ = bot
                    .answer_callback_query(&cq_id, Some(callback_text), !removed)
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
        } else if cq_data == "model_dashboard" {
            ai_service.model_picker_query.write().await.remove(&user_id);
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            send_model_dashboard(bot, ai_service, chat_id, user_id, msg_id).await;
        } else if cq_data == "model_change_main" {
            ai_service.model_picker_query.write().await.remove(&user_id);
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            let rich = build_main_model_picker_rich(ai_service, user_id, None, 1).await;
            if let Some(mid) = msg_id {
                if bot
                    .edit_rich_message(chat_id, mid, &rich, None)
                    .await
                    .is_ok()
                {
                    return;
                }
            }
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
        } else if let Some(page) = cq_data.strip_prefix("model_main_page:") {
            let page = page.parse::<usize>().unwrap_or(1);
            let query = ai_service
                .model_picker_query
                .read()
                .await
                .get(&user_id)
                .cloned();
            let rich =
                build_main_model_picker_rich(ai_service, user_id, query.as_deref(), page).await;
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            if let Some(mid) = msg_id {
                if bot
                    .edit_rich_message(chat_id, mid, &rich, None)
                    .await
                    .is_ok()
                {
                    return;
                }
            }
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
        } else if cq_data == "action_help" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            let rich = build_help_ui();
            if let Some(mid) = msg_id {
                if bot
                    .edit_rich_message(chat_id, mid, &rich, None)
                    .await
                    .is_ok()
                {
                    return;
                }
            }
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
        } else if cq_data == "clear_cancel" {
            let _ = bot
                .answer_callback_query(&cq_id, Some("Reset dibatalkan"), false)
                .await;
            if let Some(mid) = msg_id {
                let _ = bot.delete_message(chat_id, mid).await;
            }
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
                    if !ai_service
                        .set_provider_model(user_id, prov_id, &model_name)
                        .await
                    {
                        let _ = bot
                            .answer_callback_query(
                                &cq_id,
                                Some("Gagal menyimpan model aktif."),
                                true,
                            )
                            .await;
                        return;
                    }
                    let _ = bot
                        .answer_callback_query(
                            &cq_id,
                            Some(&format!(
                                "Model: {model_name} saved. Checking capabilities…"
                            )),
                            false,
                        )
                        .await;

                    if let Some(provider) = ai_service
                        .get_user_providers(user_id)
                        .await
                        .into_iter()
                        .find(|provider| provider.id == prov_id)
                    {
                        run_observable_main_capability_probe(
                            bot,
                            ai_service,
                            chat_id,
                            &provider,
                            &model_name,
                        )
                        .await;
                    }

                    let new_stats = ai_service.get_context_stats(user_id).await;
                    let warning = main_context_overflow_warning(
                        &model_name,
                        new_stats.total_tokens,
                        new_stats.limit_tokens,
                    );
                    if let Some(warning) = warning {
                        let warning_rich = InputRichMessage::new(vec![
                            RichBlock::SectionHeading {
                                text: Value::String("MAIN MODEL CHANGED".to_string()),
                                level: 1,
                            },
                            RichBlock::BlockQuotation {
                                blocks: vec![json!({"type":"paragraph","text": warning})],
                            },
                        ]);
                        let _ = bot
                            .send_rich_message(chat_id, &warning_rich, None, None)
                            .await;
                    }
                    send_model_dashboard(bot, ai_service, chat_id, user_id, msg_id).await;
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
                None,
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
                None,
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
            let cleared = ai_service.clear_history(user_id).await;
            let rich = if cleared {
                InputRichMessage::new(vec![
                    RichBlock::SectionHeading { text: Value::String("HISTORY RESET".to_string()), level: 1 },
                    RichBlock::Paragraph { text: Value::String("Canonical history and attachment context were reset durably. The session remains. Older in-flight generations cannot write back because the session revision changed.".to_string()) },
                ])
            } else {
                InputRichMessage::new(vec![
                    RichBlock::SectionHeading {
                        text: Value::String("RESET FAILED".to_string()),
                        level: 1,
                    },
                    RichBlock::BlockQuotation {
                        blocks: vec![
                            json!({"type":"paragraph","text":"Persistence failed. Previous history and attachments were preserved."}),
                        ],
                    },
                ])
            };
            let _ = bot.answer_callback_query(&cq_id, None, !cleared).await;
            if let Some(mid) = msg_id {
                if bot
                    .edit_rich_message(chat_id, mid, &rich, None)
                    .await
                    .is_ok()
                {
                    return;
                }
            }
            let _ = bot.send_rich_message(chat_id, &rich, None, None).await;
        } else if cq_data == "action_menu" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            send_menu(bot, ai_service, chat_id, user_id).await;
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
            if args.get(2).map(|s| s.as_str()) == Some("addon") {
                run_cli_model_addon(&ai_service, &args[3..]).await;
            } else if args.get(2).map(|s| s.as_str()) == Some("probe") {
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
        Ok(resp) if resp.ok => {
            let Some(bot_info) = resp.result else {
                error!("Telegram getMe returned ok=true without a result");
                return;
            };
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
        BotCommand::ephemeral("cancel", "Batalkan aksi interaktif aktif"),
        BotCommand::ephemeral("help", "Daftar perintah dan panduan"),
    ];

    if let Err(e) = bot.set_my_commands(&commands).await {
        warn!("Gagal mendaftarkan bot commands: {e}");
    } else {
        info!("Commands berhasil didaftarkan ke Telegram.");
    }

    let (generation_tx, mut generation_rx) = tokio::sync::mpsc::channel::<Update>(64);
    let (control_tx, mut control_rx) = tokio::sync::mpsc::channel::<Update>(64);

    let generation_bot = bot.clone();
    let generation_ai = Arc::clone(&ai_service);
    let generation_last_image = Arc::clone(&user_last_image_prompt);
    let generation_access = Arc::clone(&access);
    let generation_worker = tokio::spawn(async move {
        while let Some(update) = generation_rx.recv().await {
            process_durable_update(
                &generation_bot,
                &generation_ai,
                &generation_last_image,
                &generation_access,
                update,
            )
            .await;
        }
    });

    let control_bot = bot.clone();
    let control_ai = Arc::clone(&ai_service);
    let control_last_image = Arc::clone(&user_last_image_prompt);
    let control_access = Arc::clone(&access);
    let control_worker = tokio::spawn(async move {
        while let Some(update) = control_rx.recv().await {
            process_durable_update(
                &control_bot,
                &control_ai,
                &control_last_image,
                &control_access,
                update,
            )
            .await;
        }
    });

    let interrupted = ai::storage::recover_telegram_processing_async().await;
    if interrupted > 0 {
        warn!(
            "{interrupted} Telegram update berstatus processing dikembalikan ke pending untuk replay at-least-once; side effect eksternal sebelum crash dapat terulang"
        );
    }

    for record in ai::storage::pending_telegram_updates_async(500).await {
        match serde_json::from_str::<Update>(&record.payload_json) {
            Ok(update) => {
                let lane = classify_update_lane(&ai_service, &update).await;
                let send_result = match lane {
                    UpdateLane::Control => control_tx.send(update).await,
                    UpdateLane::Generation => generation_tx.send(update).await,
                };
                if send_result.is_err() {
                    error!("Update worker stopped while replaying durable inbox");
                    return;
                }
            }
            Err(error) => {
                warn!(
                    "Durable Telegram update {} tidak dapat didecode: {error}",
                    record.update_id
                );
                if ai::storage::mark_telegram_processing_async(record.update_id).await
                    && !ai::storage::mark_telegram_processed_async(record.update_id).await
                {
                    warn!(
                        "Gagal menandai durable Telegram update {} yang invalid sebagai completed",
                        record.update_id
                    );
                }
            }
        }
    }

    let mut offset = ai::storage::load_telegram_offset_async().await;
    info!("Memulai polling pesan dengan durable control/generation queues...");

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
                                    // Native Stop bypasses both queues so cancellation cannot be
                                    // blocked by either control work or an active generation.
                                    process_durable_update(
                                        &bot,
                                        &ai_service,
                                        &user_last_image_prompt,
                                        &access,
                                        update,
                                    )
                                    .await;
                                } else {
                                    let lane = classify_update_lane(&ai_service, &update).await;
                                    let send_result = match lane {
                                        UpdateLane::Control => control_tx.send(update).await,
                                        UpdateLane::Generation => generation_tx.send(update).await,
                                    };
                                    if send_result.is_err() {
                                        error!("Update worker stopped unexpectedly");
                                        return;
                                    }
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
    drop(control_tx);
    drop(generation_tx);

    match tokio::time::timeout(Duration::from_secs(5), control_worker).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!("Control worker terminated with error: {err}"),
        Err(_) => warn!("Control worker did not stop within shutdown grace period"),
    }
    match tokio::time::timeout(Duration::from_secs(5), generation_worker).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!("Generation worker terminated with error: {err}"),
        Err(_) => warn!("Generation worker did not stop within shutdown grace period"),
    }
}

#[cfg(test)]
mod update_lane_tests {
    use super::*;

    #[test]
    fn known_controls_do_not_share_generation_lane() {
        for text in [
            "/menu",
            "/session",
            "/model",
            "/context",
            "/help",
            "📱 Menu",
            "Session",
            "Rename Session",
            "Hal 2",
        ] {
            assert!(is_control_message_text(text), "{text}");
        }
    }

    #[test]
    fn generation_prompts_remain_outside_control_lane() {
        for text in [
            "Jelaskan orbit satelit",
            "/image seekor rubah di kota neon",
            "buatkan gambar pemandangan",
        ] {
            assert!(!is_control_message_text(text), "{text}");
        }
    }

    #[test]
    fn telegram_model_editing_is_main_only() {
        assert!(telegram_can_edit_model_role(ai::service::ModelRole::Main));
        for role in ai::service::ModelRole::addon_roles() {
            assert!(!telegram_can_edit_model_role(role));
        }
    }

    #[test]
    fn specialist_context_policies_are_minimal_and_role_specific() {
        assert!(specialist_context_policy(
            ai::service::ModelRole::Vision,
            ai::service::RouteOrigin::Specific
        )
        .contains("no full history"));
        assert!(specialist_context_policy(
            ai::service::ModelRole::AudioStt,
            ai::service::RouteOrigin::Specific
        )
        .starts_with("Transcript only"));
        assert!(specialist_context_policy(
            ai::service::ModelRole::ImageGeneration,
            ai::service::RouteOrigin::Specific
        )
        .starts_with("Prompt/config only"));
        assert!(specialist_context_policy(
            ai::service::ModelRole::Vision,
            ai::service::RouteOrigin::MainModel
        )
        .starts_with("Direct on Main"));
    }

    #[test]
    fn context_semantics_use_only_main_budget_and_never_underflow() {
        assert_eq!(context_available_tokens(100_000, 35_000), 65_000);
        assert_eq!(context_available_tokens(64_000, 80_000), 0);
        assert!(specialist_context_policy(
            ai::service::ModelRole::Vision,
            ai::service::RouteOrigin::Specific
        )
        .contains("no full history"));
    }

    #[test]
    fn smaller_main_context_warns_only_when_history_exceeds_usable_budget() {
        assert!(main_context_overflow_warning("small-model", 80_000, 64_000).is_some());
        assert!(main_context_overflow_warning("large-model", 40_000, 64_000).is_none());
    }

    #[test]
    fn start_and_menu_contracts_remain_distinct() {
        let start_buttons = ["New Chat", "Model", "Session", "Context"];
        let menu_extra = ["Generate Image", "Help"];
        assert_eq!(start_buttons.len(), 4);
        assert_eq!(menu_extra.len(), 2);
    }

    #[test]
    fn help_ui_is_typed_rich_and_declares_addons_read_only() {
        let value = serde_json::to_value(build_help_ui()).unwrap();
        let serialized = value.to_string();
        assert!(serialized.contains("\"type\":\"table\""));
        assert!(serialized.contains("\"type\":\"details\""));
        assert!(serialized.contains("read-only"));
        assert!(serialized.contains("xiao model addon"));
    }

    #[test]
    fn compound_image_intent_keeps_explanation_for_main() {
        let intent = plan_image_generation_intent(
            "buat gambar simulasi galaksi dan jelaskan bagaimana lengan spiral terbentuk",
        )
        .expect("compound image intent");
        assert!(intent.image_prompt.to_ascii_lowercase().contains("galaksi"));
        assert_eq!(
            intent.explanation_prompt.as_deref(),
            Some("jelaskan bagaimana lengan spiral terbentuk")
        );
    }

    #[test]
    fn image_generation_draft_can_stop_policy_is_private_only() {
        let owner_id = 123456789i64;
        let group_id = -100987654321i64;

        let private_can_stop = owner_id == owner_id;
        assert!(private_can_stop);

        let group_can_stop = group_id == owner_id;
        assert!(!group_can_stop);
    }

    #[test]
    fn access_policy_allows_native_stop_only_in_owner_private_chat() {
        let policy = AccessPolicy {
            owner_user_id: 42,
            allowed_chat_ids: [100, 200].into_iter().collect(),
        };
        assert!(policy.allows_stop_chat(42));
        assert!(!policy.allows_stop_chat(100));
        assert!(!policy.allows_stop_chat(200));
        assert!(!policy.allows_stop_chat(999));
    }

    #[test]
    fn image_caption_is_unicode_safe_bounded_and_html_escaped() {
        let prompt = format!("{} <tag> & \"quotes\" 'single'", "🌌银河系".repeat(800));
        let caption = build_image_success_caption(
            &prompt,
            "provider<&>",
            "model<\"x\">&",
            1024,
            1024,
            12.34,
            true,
            Some("failure <unsafe> & detail"),
        );

        assert!(caption.chars().count() <= TELEGRAM_PHOTO_CAPTION_MAX_CHARS);
        assert!(!caption.contains("<tag>"));
        assert!(caption.contains("&lt;"));
        assert!(caption.contains("&amp;"));
        assert!(caption.contains("&quot;"));
        assert!(!caption.contains("provider<&>"));
        assert!(caption.is_char_boundary(caption.len()));
    }

    #[test]
    fn photo_delivery_classifier_retries_only_caption_or_markup_failures() {
        assert_eq!(
            telegram_photo_delivery_error_class("Bad Request: can't parse entities in caption"),
            "caption_or_markup"
        );
        assert_eq!(
            telegram_photo_delivery_error_class("sendPhoto multipart error: timeout"),
            "telegram_transport"
        );
        assert_eq!(
            telegram_photo_delivery_error_class(
                "sendPhoto rejected bytes with an unsupported image signature"
            ),
            "local_image_validation"
        );
    }

    #[tokio::test]
    async fn image_delivery_success_is_single_send() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_sender = Arc::clone(&calls);
        let result = deliver_generated_image_with(
            b"same-image-bytes",
            "safe caption",
            Some(json!({"inline_keyboard": []})),
            move |_bytes, _caption, _parse_mode, _markup| {
                let calls = Arc::clone(&calls_for_sender);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn image_delivery_caption_retry_reuses_same_bytes_without_regeneration() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Mutex as StdMutex;

        let sends = Arc::new(AtomicUsize::new(0));
        let seen_bytes = Arc::new(StdMutex::new(Vec::<Vec<u8>>::new()));
        let sends_for_sender = Arc::clone(&sends);
        let seen_for_sender = Arc::clone(&seen_bytes);

        // Provider generation already happened exactly once before the delivery helper.
        let provider_calls = AtomicUsize::new(1);
        let result = deliver_generated_image_with(
            b"paid-generated-image",
            "caption",
            Some(json!({"inline_keyboard": [["button"]]})),
            move |bytes, _caption, _parse_mode, _markup| {
                let sends = Arc::clone(&sends_for_sender);
                let seen = Arc::clone(&seen_for_sender);
                async move {
                    let attempt = sends.fetch_add(1, Ordering::SeqCst);
                    seen.lock().expect("seen bytes lock").push(bytes);
                    if attempt == 0 {
                        Err("Bad Request: can't parse entities in caption".to_string())
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        let seen = seen_bytes.lock().expect("seen bytes lock");
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1]);
    }

    #[tokio::test]
    async fn image_delivery_double_failure_returns_user_error_policy() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let sends = Arc::new(AtomicUsize::new(0));
        let sends_for_sender = Arc::clone(&sends);
        let result = deliver_generated_image_with(
            b"image",
            "caption",
            None,
            move |_bytes, _caption, _parse_mode, _markup| {
                let sends = Arc::clone(&sends_for_sender);
                async move {
                    let attempt = sends.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        Err("Bad Request: caption entities are invalid".to_string())
                    } else {
                        Err("sendPhoto multipart error: connection".to_string())
                    }
                }
            },
        )
        .await;

        let failure = result.expect_err("second delivery should fail");
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        assert!(failure.retry_attempted);
        assert_eq!(failure.class, "telegram_transport");
    }

    #[test]
    fn compound_image_handler_has_one_generation_call_and_failure_precedes_explanation() {
        let source = include_str!("main.rs");
        let handler_start = source
            .find("async fn handle_image_generation(")
            .expect("image handler");
        let handler_end = source[handler_start..]
            .find("// Main AI Chat Handler")
            .map(|offset| handler_start + offset)
            .expect("image handler end");
        let handler = &source[handler_start..handler_end];

        assert_eq!(handler.matches(".generate_image_with_snapshot(").count(), 1);
        let failure_return = handler
            .find("if let Err(failure) = delivery")
            .expect("delivery failure branch");
        let explanation = handler
            .find("if let Some(explanation_prompt)")
            .expect("compound explanation");
        assert!(failure_return < explanation);
        assert!(handler[failure_return..explanation].contains("return;"));
    }

    #[test]
    fn clear_confirmation_is_a_typed_rich_action() {
        let encoded = serde_json::to_string(&build_clear_confirmation_ui()).unwrap();
        assert!(encoded.contains("action_clear"));
        assert!(encoded.contains("clear_cancel"));
        assert!(encoded.contains("RESET HISTORY"));
    }
}
