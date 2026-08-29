mod ai;
mod bot;
mod parser;
mod timeline;

use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use rand::Rng;
use regex::Regex;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use ai::AIChatService;
use bot::client::TelegramBotClient;
use bot::models::{
    BotCommand, InlineKeyboardButton, InlineKeyboardMarkup, InputRichMessage,
    ReplyKeyboardMarkup, RichBlock, RichBlockTableCell, Update,
};
use parser::build_full_rich_message;
use timeline::{ExecutionTimeline, ProgressActivity};

type UserLastImagePrompt = Arc<RwLock<HashMap<i64, String>>>;

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

fn load_environment() {
    let cfg_path = get_config_path();
    if cfg_path.exists() {
        let _ = dotenvy::from_path(&cfg_path);
    } else {
        let _ = dotenvy::dotenv();
    }
}

fn save_env_kv(key: &str, value: &str) -> io::Result<()> {
    ai::service::save_app_setting(key, value)
}

fn save_token_to_env(token: &str) -> io::Result<()> {
    ai::service::save_app_setting("BOT_TOKEN", token)
}

fn get_configured_token() -> Option<String> {
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

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    style::Print,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use ai::service::{load_provider_store, save_provider_store, ProviderConfig};

struct CleanRawMode;
impl CleanRawMode {
    fn new() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        let _ = execute!(stdout, EnterAlternateScreen, cursor::Hide);
        Ok(Self)
    }
}
impl Drop for CleanRawMode {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

pub fn terminal_interactive_select(
    title: &str,
    items: &[String],
    initial_idx: usize,
    allow_search: bool,
    initial_query: Option<&str>,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }

    let _raw_guard = CleanRawMode::new().ok()?;
    let mut stdout = io::stdout();

    let mut query = initial_query.unwrap_or("").to_string();
    let mut selected_pos = initial_idx.min(items.len() - 1);
    let page_size = 25usize;
    let mut top_idx = 0usize;

    loop {
        // Filter items based on query
        let filtered: Vec<(usize, &String)> = if query.is_empty() {
            items.iter().enumerate().collect()
        } else {
            let q_low = query.to_lowercase();
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.to_lowercase().contains(&q_low))
                .collect()
        };

        if selected_pos >= filtered.len() {
            selected_pos = filtered.len().saturating_sub(1);
        }

        if selected_pos < top_idx {
            top_idx = selected_pos;
        } else if selected_pos >= top_idx + page_size {
            top_idx = selected_pos + 1 - page_size;
        }

        // Render screen
        let mut buffer = Vec::new();
        buffer.push(format!("\x1b[1;38;5;39m{}\x1b[0m", title));
        if allow_search {
            buffer.push(format!(
                "\x1b[38;5;214mFilter:\x1b[0m \x1b[1;37m{}\x1b[38;5;244m_  \x1b[38;5;240m[▲/▼ Navigate • Enter Select • Esc Back]\x1b[0m",
                query
            ));
        } else {
            buffer.push("\x1b[38;5;243m[▲/▼ Navigate • Enter Select • Esc Back]\x1b[0m".to_string());
        }
        buffer.push("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m".to_string());

        if filtered.is_empty() {
            buffer.push("  \x1b[38;5;203mNo items matching filter.\x1b[0m".to_string());
        } else {
            let end_idx = (top_idx + page_size).min(filtered.len());
            if top_idx > 0 {
                buffer.push("  \x1b[38;5;240m▲ (more items above)\x1b[0m".to_string());
            }
            for (curr_idx, (orig_idx, item_text)) in filtered[top_idx..end_idx].iter().enumerate() {
                let actual_idx = top_idx + curr_idx;
                let is_sel = actual_idx == selected_pos;
                if is_sel {
                    buffer.push(format!(
                        "\x1b[1;38;5;42m ❯ \x1b[1;37m{:>2}. {}\x1b[0m",
                        orig_idx + 1,
                        item_text
                    ));
                } else {
                    buffer.push(format!(
                        "   \x1b[38;5;244m{:>2}. \x1b[38;5;250m{}\x1b[0m",
                        orig_idx + 1,
                        item_text
                    ));
                }
            }
            if end_idx < filtered.len() {
                buffer.push(format!("  \x1b[38;5;240m▼ ({} more items below)\x1b[0m", filtered.len() - end_idx));
            }
        }
        buffer.push("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m".to_string());

        // Clear and print at top of alternate screen
        let _ = execute!(
            stdout,
            cursor::MoveTo(0, 0),
            Clear(ClearType::All),
            Print(buffer.join("\r\n"))
        );
        let _ = stdout.flush();

        // Read event
        if let Ok(Event::Key(KeyEvent { code, modifiers, .. })) = event::read() {
            if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
                return None;
            }
            match code {
                KeyCode::Esc => {
                    return None;
                }
                KeyCode::Enter => {
                    if let Some(&(orig_idx, _)) = filtered.get(selected_pos) {
                        return Some(orig_idx);
                    }
                }
                KeyCode::Up => {
                    if selected_pos > 0 {
                        selected_pos -= 1;
                    }
                }
                KeyCode::Down => {
                    if !filtered.is_empty() && selected_pos + 1 < filtered.len() {
                        selected_pos += 1;
                    }
                }
                KeyCode::PageUp => {
                    selected_pos = selected_pos.saturating_sub(page_size);
                }
                KeyCode::PageDown => {
                    if !filtered.is_empty() {
                        selected_pos = (selected_pos + page_size).min(filtered.len() - 1);
                    }
                }
                KeyCode::Backspace if allow_search => {
                    query.pop();
                    selected_pos = 0;
                }
                KeyCode::Char(c) if allow_search => {
                    query.push(c);
                    selected_pos = 0;
                }
                _ => {}
            }
        }
    }
}

fn terminal_interactive_multi_select(title: &str, items: &[String], selected: &[bool], max_selected: usize) -> Option<Vec<usize>> {
    if items.is_empty() {
        return None;
    }
    let _raw_guard = CleanRawMode::new().ok()?;
    let mut stdout = io::stdout();
    let mut cursor_idx = 0usize;
    let mut top_idx = 0usize;
    let page_size = 25usize;
    let mut query = String::new();
    let mut picked = selected.to_vec();
    picked.resize(items.len(), false);

    loop {
        let filtered: Vec<usize> = if query.is_empty() {
            (0..items.len()).collect()
        } else {
            let q = query.to_lowercase();
            items.iter().enumerate().filter_map(|(idx, item)| item.to_lowercase().contains(&q).then_some(idx)).collect()
        };
        if filtered.is_empty() {
            cursor_idx = 0;
            top_idx = 0;
        } else {
            cursor_idx = cursor_idx.min(filtered.len() - 1);
            if cursor_idx < top_idx { top_idx = cursor_idx; }
            if cursor_idx >= top_idx + page_size { top_idx = cursor_idx + 1 - page_size; }
        }
        let end_idx = (top_idx + page_size).min(filtered.len());
        let mut buffer = vec![format!("\x1b[1;38;5;39m{}\x1b[0m", title)];
        buffer.push(format!("\x1b[38;5;214mFilter:\x1b[0m \x1b[1;37m{}_\x1b[0m  \x1b[38;5;240m[▲/▼ Navigate • Space Toggle • Enter Save • Esc Save]\x1b[0m", query));
        buffer.push(format!("\x1b[38;5;244mSelected: {}/{} • Showing {}-{} of {}\x1b[0m", picked.iter().filter(|v| **v).count(), max_selected, if filtered.is_empty() { 0 } else { top_idx + 1 }, end_idx, filtered.len()));
        buffer.push("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m".to_string());
        for (display_idx, idx) in filtered[top_idx..end_idx].iter().enumerate() {
            let marker = if picked[*idx] { "[x]" } else { "[ ]" };
            let is_cursor = top_idx + display_idx == cursor_idx;
            if is_cursor {
                buffer.push(format!("\x1b[1;38;5;42m > \x1b[1;37m{} {:>3}. {}\x1b[0m", marker, idx + 1, items[*idx]));
            } else {
                buffer.push(format!("   \x1b[38;5;244m{} {:>3}. \x1b[38;5;250m{}\x1b[0m", marker, idx + 1, items[*idx]));
            }
        }
        let chosen: Vec<&str> = picked.iter().enumerate().filter_map(|(idx, value)| value.then_some(items[idx].as_str())).collect();
        if !chosen.is_empty() {
            buffer.push("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m".to_string());
            buffer.push("\x1b[38;5;214mPicked:\x1b[0m".to_string());
            for model in chosen {
                buffer.push(format!("  \x1b[38;5;250m- {}\x1b[0m", model));
            }
        }
        buffer.push("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m".to_string());
        buffer.push("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m".to_string());
        let _ = execute!(stdout, cursor::MoveTo(0, 0), Clear(ClearType::All), Print(buffer.join("\r\n")));
        let _ = stdout.flush();

        if let Ok(Event::Key(KeyEvent { code, .. })) = event::read() {
            match code {
                KeyCode::Esc => return Some(picked.iter().enumerate().filter_map(|(idx, chosen)| chosen.then_some(idx)).collect()),
                KeyCode::Up => cursor_idx = cursor_idx.saturating_sub(1),
                KeyCode::Down => cursor_idx = (cursor_idx + 1).min(filtered.len().saturating_sub(1)),
                KeyCode::PageUp => cursor_idx = cursor_idx.saturating_sub(page_size),
                KeyCode::PageDown => cursor_idx = (cursor_idx + page_size).min(filtered.len().saturating_sub(1)),
                KeyCode::Char(' ') => {
                    if let Some(&idx) = filtered.get(cursor_idx) {
                        if picked[idx] || picked.iter().filter(|v| **v).count() < max_selected {
                            picked[idx] = !picked[idx];
                        }
                    }
                }
                KeyCode::Enter => return Some(picked.iter().enumerate().filter_map(|(idx, chosen)| chosen.then_some(idx)).collect()),
                KeyCode::Backspace => { query.pop(); cursor_idx = 0; top_idx = 0; }
                KeyCode::Char(c) => { query.push(c); cursor_idx = 0; top_idx = 0; }
                _ => {}
            }
        }
    }
}

async fn run_cli_quickstart_wizard(ai_service: &AIChatService) -> Option<String> {
    println!("\n\x1b[1;36m== XiaoAI Quickstart Setup Wizard ==\x1b[0m");
    println!("\x1b[38;5;244mThis wizard will configure your AI Provider and Telegram Bot.\x1b[0m");
    println!("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m");

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    // Step 1: AI Provider Endpoint
    println!("\n\x1b[1;37m[1/3] Configure AI Provider\x1b[0m");
    println!("\x1b[38;5;244mCommon Endpoints:\x1b[0m");
    println!("  \x1b[38;5;246m• https://api.openai.com/v1\x1b[0m");
    println!("  \x1b[38;5;246m• https://openrouter.ai/api/v1\x1b[0m");
    println!("  \x1b[38;5;246m• https://api.groq.com/openai/v1\x1b[0m");
    println!("  \x1b[38;5;246m• http://127.0.0.1:8317/v1 (Local Cliproxy / Ollama)\x1b[0m");

    let endpoint = loop {
        print!("\n\x1b[1;37mEndpoint URL:\x1b[0m ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if reader.read_line(&mut input).is_err() {
            println!("\n\x1b[38;5;244mSetup cancelled.\x1b[0m");
            return None;
        }
        let clean = input.trim().trim_end_matches('/').to_string();
        if clean.starts_with("http://") || clean.starts_with("https://") {
            break clean;
        }
        println!("\x1b[1;31m[ERROR] Endpoint must start with http:// or https://\x1b[0m");
    };

    // Step 2: API Key (Optional)
    print!("\x1b[1;37mAPI Key\x1b[0m \x1b[38;5;244m(Optional - press Enter if none):\x1b[0m ");
    let _ = io::stdout().flush();
    let mut key_input = String::new();
    let _ = reader.read_line(&mut key_input);
    let mut api_key = key_input.trim().to_string();
    if api_key.is_empty() {
        api_key = "none".to_string();
    }

    // Step 3: Provider Alias (Optional)
    print!("\x1b[1;37mProvider Alias/Name\x1b[0m \x1b[38;5;244m(Optional - press Enter for default):\x1b[0m ");
    let _ = io::stdout().flush();
    let mut alias_input = String::new();
    let _ = reader.read_line(&mut alias_input);
    let raw_alias = alias_input.trim();
    let clean_alias = if raw_alias.is_empty() {
        if let Ok(u) = url::Url::parse(&endpoint) {
            u.host_str().unwrap_or("Custom Provider").to_string()
        } else {
            "Custom Provider".to_string()
        }
    } else {
        raw_alias.to_string()
    };

    // Step 4: Fetch & Select Model via Arrow Keys
    println!("\n\x1b[38;5;244mConnecting to endpoint {endpoint} & fetching models...\x1b[0m");
    let (ok, res) = ai_service.fetch_models_from_endpoint(&endpoint, &api_key).await;

    if !ok {
        let err = res.err().unwrap_or_else(|| "Unknown error".to_string());
        println!("\x1b[1;31m[FAIL] Could not connect to provider: {err}\x1b[0m");
        return None;
    }

    let models = res.unwrap_or_else(|_| vec!["gpt-4o".to_string()]);
    println!("\x1b[1;32m[OK] Connected! Found {} models.\x1b[0m", models.len());

    let selected_idx = terminal_interactive_select(
        "⚡ Select Active AI Model for This Provider:",
        &models,
        0,
        true,
        None,
    );

    let active_model = if let Some(idx) = selected_idx {
        models[idx].clone()
    } else {
        models.first().cloned().unwrap_or_else(|| "gpt-4o".to_string())
    };

    use rand::Rng;
    let random_suffix: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();
    let provider_id = format!("prov_{}", random_suffix.to_lowercase());

    let provider = ProviderConfig {
        id: provider_id.clone(),
        name: clean_alias.clone(),
        endpoint: endpoint.clone(),
        api_key: api_key.clone(),
        models: models.clone(),
        active_model: active_model.clone(),
    };

    let mut store = load_provider_store();
    store.providers.push(provider);
    store.active_id = Some(provider_id);
    let _ = save_provider_store(&store);

    let _ = save_env_kv("AI_ENDPOINT", &endpoint);
    let _ = save_env_kv("AI_API_KEY", &api_key);
    let _ = save_env_kv("AI_MODEL", &active_model);

    println!("\x1b[1;32m[OK] AI Provider '{}' configured with model '{}'!\x1b[0m", clean_alias, active_model);

    // Step 5: Telegram Bot Token
    println!("\n\x1b[1;37m[2/3] Configure Telegram Bot Token\x1b[0m");
    println!("\x1b[38;5;244mHow to get your Bot Token:\x1b[0m");
    println!("  \x1b[38;5;246m1. Open Telegram and search for @BotFather\x1b[0m");
    println!("  \x1b[38;5;246m2. Send /newbot and follow instructions\x1b[0m");
    println!("  \x1b[38;5;246m3. Copy the API token provided\x1b[0m");
    println!("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m");

    let (final_token, bot_username) = loop {
        print!("\n\x1b[1;37mEnter Telegram Bot Token:\x1b[0m ");
        let _ = io::stdout().flush();

        let mut input = String::new();
        if reader.read_line(&mut input).is_err() {
            println!("\n\x1b[38;5;244mSetup cancelled.\x1b[0m");
            return None;
        }

        let user_token = input.trim().to_string();
        if user_token.is_empty() {
            println!("\x1b[1;31m[ERROR] Token cannot be empty.\x1b[0m");
            continue;
        }

        println!("\x1b[38;5;244mVerifying token with Telegram API...\x1b[0m");
        let temp_bot = TelegramBotClient::new(&user_token);
        match temp_bot.get_me().await {
            Ok(resp) if resp.ok && resp.result.is_some() => {
                let bot_info = resp.result.unwrap();
                let username = bot_info.username.unwrap_or_else(|| "Unknown".to_string());
                println!("\x1b[1;32m[OK] Token valid! Connected to @{} ({})\x1b[0m", username, bot_info.first_name);

                let _ = save_token_to_env(&user_token);
                break (user_token, username);
            }
            Ok(resp) => {
                let desc = resp.description.unwrap_or_else(|| "Invalid token".to_string());
                println!("\x1b[1;31m[FAIL] Invalid token: {desc}\x1b[0m");
            }
            Err(e) => {
                println!("\x1b[1;31m[FAIL] Verification error: {e}\x1b[0m");
            }
        }
    };

    println!("\n\x1b[1;36m== [3/3] Quickstart Setup Complete! ==\x1b[0m");
    println!("  \x1b[1;37mBot:\x1b[0m          \x1b[1;32m@{}\x1b[0m", bot_username);
    println!("  \x1b[1;37mProvider:\x1b[0m     {}", clean_alias);
    println!("  \x1b[1;37mActive Model:\x1b[0m \x1b[1;36m{}\x1b[0m", active_model);
    println!("  \x1b[1;37mEndpoint:\x1b[0m     {}", endpoint);
    println!("\n\x1b[1;32m🎉 Everything is set up! Run 'xiao start' to launch the bot.\x1b[0m\n");

    Some(final_token)
}

async fn get_or_prompt_token(ai_service: &AIChatService) -> Option<String> {
    if let Some(token) = get_configured_token() {
        if ai_service.has_configured_provider(0).await {
            return Some(token);
        }
    }
    println!("\x1b[38;5;214m[WARN] Configuration missing. Starting Quickstart Setup Wizard...\x1b[0m");
    run_cli_quickstart_wizard(ai_service).await
}

async fn run_cli_provider_add(ai_service: &AIChatService) {
    println!("\n\x1b[1;36m== Add New AI Provider ==\x1b[0m");
    println!("\x1b[38;5;244mCommon Endpoints:\x1b[0m");
    println!("  \x1b[38;5;246m• https://api.openai.com/v1\x1b[0m");
    println!("  \x1b[38;5;246m• https://openrouter.ai/api/v1\x1b[0m");
    println!("  \x1b[38;5;246m• https://api.groq.com/openai/v1\x1b[0m");
    println!("  \x1b[38;5;246m• http://127.0.0.1:8317/v1 (Local Cliproxy / Ollama)\x1b[0m");
    println!("\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m");

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    print!("\n\x1b[1;37mEndpoint URL:\x1b[0m ");
    let _ = io::stdout().flush();
    let mut endpoint_input = String::new();
    if reader.read_line(&mut endpoint_input).is_err() {
        return;
    }
    let endpoint = endpoint_input.trim().trim_end_matches('/').to_string();
    if endpoint.is_empty() || (!endpoint.starts_with("http://") && !endpoint.starts_with("https://")) {
        println!("\x1b[1;31m[ERROR] Invalid endpoint URL format!\x1b[0m");
        return;
    }

    print!("\x1b[1;37mAPI Key\x1b[0m \x1b[38;5;244m('none' if local/no key)\x1b[0m: ");
    let _ = io::stdout().flush();
    let mut key_input = String::new();
    if reader.read_line(&mut key_input).is_err() {
        return;
    }
    let api_key = key_input.trim().to_string();

    print!("\x1b[1;37mProvider Alias/Name\x1b[0m \x1b[38;5;244m(e.g. OpenRouter, Groq, Local)\x1b[0m: ");
    let _ = io::stdout().flush();
    let mut alias_input = String::new();
    if reader.read_line(&mut alias_input).is_err() {
        return;
    }
    let alias = alias_input.trim().to_string();

    println!("\n\x1b[38;5;244mConnecting to endpoint {endpoint} & fetching models...\x1b[0m");
    let (ok, res) = ai_service.fetch_models_from_endpoint(&endpoint, &api_key).await;

    if !ok {
        let err = res.err().unwrap_or_else(|| "Unknown error".to_string());
        println!("\x1b[1;31m[FAIL] Could not connect to provider: {err}\x1b[0m");
        return;
    }

    let models = res.unwrap_or_else(|_| vec!["gpt-4o".to_string()]);
    println!("\x1b[1;32m[OK] Connected! Found {} models.\x1b[0m", models.len());

    let selected_idx = terminal_interactive_select(
        "Select Active AI Model for This Provider:",
        &models,
        0,
        true,
        None,
    );

    let active_model = if let Some(idx) = selected_idx {
        models[idx].clone()
    } else {
        models.first().cloned().unwrap_or_else(|| "gpt-4o".to_string())
    };

    let clean_alias = if alias.is_empty() {
        if let Ok(u) = url::Url::parse(&endpoint) {
            u.host_str().unwrap_or("Custom Provider").to_string()
        } else {
            "Custom Provider".to_string()
        }
    } else {
        alias
    };

    use rand::Rng;
    let random_suffix: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();
    let provider_id = format!("prov_{}", random_suffix.to_lowercase());

    let provider = ProviderConfig {
        id: provider_id.clone(),
        name: clean_alias.clone(),
        endpoint: endpoint.clone(),
        api_key: api_key.clone(),
        models: models.clone(),
        active_model: active_model.clone(),
    };

    let mut store = load_provider_store();
    store.providers.push(provider);
    store.active_id = Some(provider_id);
    let _ = save_provider_store(&store);

    let _ = save_env_kv("AI_ENDPOINT", &endpoint);
    let _ = save_env_kv("AI_API_KEY", &api_key);
    let _ = save_env_kv("AI_MODEL", &active_model);

    println!("\n\x1b[1;32m[SUCCESS] Provider '{}' added and activated!\x1b[0m", clean_alias);
    println!("  \x1b[1;36mActive Model:\x1b[0m \x1b[1;37m{active_model}\x1b[0m");
    println!("  \x1b[38;5;244mConfiguration saved to ~/.xiao_providers.json and .env\x1b[0m\n");
}

async fn run_cli_provider_remove(_ai_service: &AIChatService) {
    let mut store = load_provider_store();
    if store.providers.is_empty() {
        println!("\n\x1b[38;5;214m[WARN] No configured providers found.\x1b[0m");
        return;
    }

    let items: Vec<String> = store
        .providers
        .iter()
        .map(|p| {
            let is_act = store.active_id.as_deref() == Some(p.id.as_str());
            if is_act {
                format!("{} \x1b[1;32m[ACTIVE]\x1b[0m", p.name)
            } else {
                p.name.clone()
            }
        })
        .collect();

    let selected = terminal_interactive_select(
        "Select Provider to Remove:",
        &items,
        0,
        false,
        None,
    );

    if let Some(idx) = selected {
        let removed = store.providers.remove(idx);
        if store.active_id.as_deref() == Some(removed.id.as_str()) {
            if let Some(first_p) = store.providers.first() {
                store.active_id = Some(first_p.id.clone());
                let _ = save_env_kv("AI_ENDPOINT", &first_p.endpoint);
                let _ = save_env_kv("AI_API_KEY", &first_p.api_key);
                let _ = save_env_kv("AI_MODEL", &first_p.active_model);
            } else {
                store.active_id = None;
                let _ = save_env_kv("AI_ENDPOINT", "");
                let _ = save_env_kv("AI_API_KEY", "");
                let _ = save_env_kv("AI_MODEL", "");
            }
        }
        let _ = save_provider_store(&store);
        println!("\n\x1b[1;32m[OK] Provider '{}' successfully removed.\x1b[0m\n", removed.name);
    } else {
        println!("\n\x1b[38;5;244mCancelled.\x1b[0m\n");
    }
}

async fn run_cli_provider_status(_ai_service: &AIChatService) {
    load_environment();
    let store = load_provider_store();
    let active_p = if let Some(ref aid) = store.active_id {
        store.providers.iter().find(|p| &p.id == aid).cloned()
    } else {
        store.providers.first().cloned()
    };

    println!("\n\x1b[1;36m== Active Provider Status ==\x1b[0m");

    if let Some(p) = active_p {
        println!("  \x1b[1;37mProvider:\x1b[0m     \x1b[1;32m{}\x1b[0m", p.name);
        println!("  \x1b[1;37mEndpoint:\x1b[0m     {}", p.endpoint);
        println!("  \x1b[1;37mActive Model:\x1b[0m \x1b[1;36m{}\x1b[0m", p.active_model);
        println!("  \x1b[1;37mTotal Models:\x1b[0m {} available", p.models.len());

        let cap = crate::ai::service::get_model_capabilities(&p.active_model);
        println!("\n\x1b[1;36mCapabilities ({}):\x1b[0m", cap.family);
        println!("  • Context:    {}", cap.context_str);
        println!("  • Vision:     {}", cap.vision_desc);
        println!("  • Video:      {}", cap.video_desc);
        println!("  • Audio:      {}", cap.audio_desc);
        println!("  • Thinking:   {}", cap.thinking_desc);
    } else {
        println!("  \x1b[38;5;214m[WARN] No AI provider configured yet.\x1b[0m");
        println!("  \x1b[38;5;244mRun 'xiao provider add' to configure a provider.\x1b[0m");
    }
    println!();
}

async fn run_cli_provider_menu(ai_service: &AIChatService, action: Option<&str>) {
    load_environment();
    match action {
        Some("add") | Some("new") => {
            run_cli_provider_add(ai_service).await;
            return;
        }
        Some("remove") | Some("del") | Some("delete") | Some("rm") => {
            run_cli_provider_remove(ai_service).await;
            return;
        }
        Some("status") => {
            run_cli_provider_status(ai_service).await;
            return;
        }
        _ => {}
    }

    loop {
        let store = load_provider_store();
        if store.providers.is_empty() {
            println!("\n\x1b[38;5;214m[WARN] No providers configured yet.\x1b[0m");
            print!("\x1b[1;37mAdd a provider now? [Y/n]:\x1b[0m ");
            let _ = io::stdout().flush();
            let mut ans = String::new();
            let _ = io::stdin().read_line(&mut ans);
            if ans.trim().eq_ignore_ascii_case("n") {
                return;
            }
            run_cli_provider_add(ai_service).await;
            return;
        }

        let mut menu_items: Vec<String> = store
            .providers
            .iter()
            .map(|p| {
                let is_act = store.active_id.as_deref() == Some(p.id.as_str());
                if is_act {
                    format!("{} \x1b[1;32m[ACTIVE]\x1b[0m", p.name)
                } else {
                    p.name.clone()
                }
            })
            .collect();

        menu_items.push("\x1b[1;32m+ Add Provider\x1b[0m".to_string());
        menu_items.push("\x1b[1;31m- Remove Provider\x1b[0m".to_string());
        menu_items.push("\x1b[38;5;244mx Exit\x1b[0m".to_string());

        let sel = terminal_interactive_select(
            "Configured AI Providers:",
            &menu_items,
            0,
            false,
            None,
        );

        let Some(idx) = sel else {
            break;
        };

        if idx < store.providers.len() {
            let target_prov = &store.providers[idx];
            let is_act = store.active_id.as_deref() == Some(target_prov.id.as_str());

            let meta_guard = ai_service.model_metadata.read().await;
            let metadata = meta_guard.get(&ai::service::model_metadata_key(&target_prov.endpoint, &target_prov.active_model)).cloned();
            drop(meta_guard);
            let cap = crate::ai::service::get_model_capabilities_with_meta(&target_prov.active_model, metadata.as_ref());

            let status_tag = if is_act {
                "\x1b[1;32mACTIVE (Default)\x1b[0m"
            } else {
                "\x1b[38;5;244mINACTIVE\x1b[0m"
            };

            let title_summary = format!(
                "== Provider: {} ==\r\n\
                 \x1b[38;5;244m• Endpoint:\x1b[0m     {}\r\n\
                 \x1b[38;5;244m• Active Model:\x1b[0m \x1b[1;36m{}\x1b[0m ({})\r\n\
                 \x1b[38;5;244m• Total Models:\x1b[0m {} models available\r\n\
                 \x1b[38;5;244m• Context:\x1b[0m      {}\r\n\
                 \x1b[38;5;244m• Capabilities:\x1b[0m Vision: {}, Audio: {}, Video: {}\r\n\
                 \x1b[38;5;244m• Status:\x1b[0m       {}",
                target_prov.name,
                target_prov.endpoint,
                target_prov.active_model,
                cap.family,
                target_prov.models.len(),
                cap.context_str,
                if cap.vision { "YES" } else { "NO" },
                if cap.audio { "YES" } else { "NO" },
                if cap.video { "YES" } else { "NO" },
                status_tag
            );

            let mut sub_actions = Vec::new();
            if !is_act {
                sub_actions.push(format!("\x1b[1;32mSet as Active Provider ({})\x1b[0m", target_prov.name));
            }
            sub_actions.push("Select / Switch Model for this Provider".to_string());
            sub_actions.push(format!("\x1b[1;31mDelete Provider ({})\x1b[0m", target_prov.name));
            sub_actions.push("\x1b[38;5;244mBack to Providers Menu\x1b[0m".to_string());

            let sub_sel = terminal_interactive_select(
                &title_summary,
                &sub_actions,
                0,
                false,
                None,
            );

            let Some(action_idx) = sub_sel else {
                continue;
            };

            let chosen_action = if is_act { action_idx + 1 } else { action_idx };

            match chosen_action {
                0 => {
                    let mut updated_store = load_provider_store();
                    updated_store.active_id = Some(target_prov.id.clone());
                    let _ = save_provider_store(&updated_store);
                    let _ = save_env_kv("AI_ENDPOINT", &target_prov.endpoint);
                    let _ = save_env_kv("AI_API_KEY", &target_prov.api_key);
                    let _ = save_env_kv("AI_MODEL", &target_prov.active_model);
                    println!("\n\x1b[1;32m[OK] Provider '{}' is now active!\x1b[0m\n", target_prov.name);
                }
                1 => {
                    let (ok, res) = ai_service.fetch_models_from_endpoint(&target_prov.endpoint, &target_prov.api_key).await;
                    let models = if ok { res.unwrap_or_default() } else { target_prov.models.clone() };
                    if models.is_empty() {
                        println!("\x1b[38;5;214m[WARN] No models found on endpoint.\x1b[0m");
                    } else {
                        let curr_idx = models.iter().position(|m| m == &target_prov.active_model).unwrap_or(0);
                        if let Some(m_idx) = terminal_interactive_select(
                            &format!("Select Model for '{}':", target_prov.name),
                            &models,
                            curr_idx,
                            true,
                            None,
                        ) {
                            let chosen_model = models[m_idx].clone();
                            let mut updated_store = load_provider_store();
                            if let Some(p) = updated_store.providers.iter_mut().find(|p| p.id == target_prov.id) {
                                p.active_model = chosen_model.clone();
                                p.models = models;
                            }
                            let _ = save_provider_store(&updated_store);
                            if store.active_id.as_deref() == Some(target_prov.id.as_str()) {
                                let _ = save_env_kv("AI_MODEL", &chosen_model);
                            }
                            println!("\n\x1b[1;32m[OK] Model for '{}' set to: {}\x1b[0m\n", target_prov.name, chosen_model);
                        }
                    }
                }
                2 => {
                    let mut updated_store = load_provider_store();
                    if let Some(pos) = updated_store.providers.iter().position(|p| p.id == target_prov.id) {
                        let removed = updated_store.providers.remove(pos);
                        if updated_store.active_id.as_deref() == Some(removed.id.as_str()) {
                            if let Some(first_p) = updated_store.providers.first() {
                                updated_store.active_id = Some(first_p.id.clone());
                                let _ = save_env_kv("AI_ENDPOINT", &first_p.endpoint);
                                let _ = save_env_kv("AI_API_KEY", &first_p.api_key);
                                let _ = save_env_kv("AI_MODEL", &first_p.active_model);
                            } else {
                                updated_store.active_id = None;
                                let _ = save_env_kv("AI_ENDPOINT", "");
                                let _ = save_env_kv("AI_API_KEY", "");
                                let _ = save_env_kv("AI_MODEL", "");
                            }
                        }
                        let _ = save_provider_store(&updated_store);
                        println!("\n\x1b[1;32m[OK] Provider '{}' deleted.\x1b[0m\n", removed.name);
                    }
                }
                _ => {}
            }
        } else if idx == store.providers.len() {
            run_cli_provider_add(ai_service).await;
        } else if idx == store.providers.len() + 1 {
            run_cli_provider_remove(ai_service).await;
        } else {
            break;
        }
    }
}

async fn run_cli_model_probe(ai_service: &AIChatService) {
    let providers = ai_service.get_user_providers(0).await;
    if providers.is_empty() {
        println!("No AI providers configured.");
        return;
    }
    for provider in providers {
        let (ok, result) = ai_service.fetch_models_from_endpoint(&provider.endpoint, &provider.api_key).await;
        match result {
            Ok(models) if ok => println!("{}: recorded {} model capabilities", provider.name, models.len()),
            Ok(_) | Err(_) => println!("{}: capability probe failed", provider.name),
        }
    }
    let registry = ai::service::load_capability_registry();
    println!("Capability registry: {} model(s)", registry.models.len());
    for record in registry.models {
        println!("- {} / {}: image={:?}, audio={:?}, video={:?}, context={:?} [{}]", record.provider_name, record.model, record.supports_image, record.supports_audio, record.supports_video, record.context_window, record.source);
    }
}

async fn run_cli_model_picker(ai_service: &AIChatService, initial_filter: Option<&str>) {
    load_environment();
    let mut store = load_provider_store();

    if store.providers.is_empty() {
        println!("\n\x1b[38;5;214m[WARN] No AI provider configured yet.\x1b[0m");
        println!("  \x1b[38;5;244mRun 'xiao provider add' to configure a provider.\x1b[0m\n");
        return;
    }

    println!("\n\x1b[38;5;244mLoading model catalogs from all saved providers...\x1b[0m");

    for prov in store.providers.iter_mut() {
        if prov.models.len() <= 1 && !prov.endpoint.is_empty() {
            if let (true, Ok(fetched)) = ai_service.fetch_models_from_endpoint(&prov.endpoint, &prov.api_key).await {
                if !fetched.is_empty() {
                    prov.models = fetched;
                }
            }
        }
    }
    let _ = save_provider_store(&store);

    let active_prov_id = store.active_id.clone().unwrap_or_default();
    let current_model = env::var("AI_MODEL").unwrap_or_default();

    let mut catalog: Vec<(String, String, String, bool)> = Vec::new();
    for prov in &store.providers {
        let is_prov_active = prov.id == active_prov_id;
        for m in &prov.models {
            let is_model_active = is_prov_active && (m == &prov.active_model || m == &current_model);
            catalog.push((prov.id.clone(), prov.name.clone(), m.clone(), is_model_active));
        }
    }

    if catalog.is_empty() {
        println!("\x1b[38;5;214m[WARN] No models found across any configured provider.\x1b[0m\n");
        return;
    }

    let is_multi_provider = store.providers.len() > 1;

    let items: Vec<String> = catalog
        .iter()
        .map(|(_, prov_name, model_name, is_act)| {
            let act_tag = if *is_act { " \x1b[1;32m[ACTIVE]\x1b[0m" } else { "" };
            if is_multi_provider {
                format!("{} \x1b[38;5;244m({})\x1b[0m{}", model_name, prov_name, act_tag)
            } else {
                format!("{}{}", model_name, act_tag)
            }
        })
        .collect();

    let curr_idx = catalog.iter().position(|(_, _, _, is_act)| *is_act).unwrap_or(0);
    let title = format!(
        "Select Active Model (Total {} Models from {} Providers):",
        catalog.len(),
        store.providers.len()
    );

    let selected_idx = terminal_interactive_select(
        &title,
        &items,
        curr_idx,
        true,
        initial_filter,
    );

    if let Some(idx) = selected_idx {
        if let Some((prov_id, prov_name, chosen_model, _)) = catalog.get(idx) {
            let mut updated_store = load_provider_store();
            updated_store.active_id = Some(prov_id.clone());
            if let Some(p) = updated_store.providers.iter_mut().find(|p| &p.id == prov_id) {
                p.active_model = chosen_model.clone();
                let _ = save_env_kv("AI_ENDPOINT", &p.endpoint);
                let _ = save_env_kv("AI_API_KEY", &p.api_key);
                let _ = save_env_kv("AI_MODEL", chosen_model);
            }
            let _ = save_provider_store(&updated_store);
            println!("\n\x1b[1;32m[SUCCESS] Active model set to: {}\x1b[0m", chosen_model);
            println!("  \x1b[1;36mProvider:\x1b[0m \x1b[1;37m{}\x1b[0m", prov_name);
            println!("  \x1b[38;5;244mActivated provider & saved to configuration.\x1b[0m\n");
        }
    } else {
        println!("\n\x1b[38;5;244mModel selection cancelled.\x1b[0m\n");
    }
}

async fn run_cli_status(ai_service: &AIChatService) {
    load_environment();
    println!("\n\x1b[1;36m== XiaoAI System Status ==\x1b[0m");

    let token = env::var("BOT_TOKEN").ok().or_else(|| ai::service::load_app_setting("BOT_TOKEN")).unwrap_or_default();
    let endpoint = env::var("AI_ENDPOINT").ok().or_else(|| ai::service::load_app_setting("AI_ENDPOINT")).unwrap_or_default();
    let api_key = env::var("AI_API_KEY").ok().or_else(|| ai::service::load_app_setting("AI_API_KEY")).unwrap_or_else(|| "none".to_string());
    let model = env::var("AI_MODEL").ok().or_else(|| ai::service::load_app_setting("AI_MODEL")).unwrap_or_default();

    println!("\x1b[1;37m1. TELEGRAM BOT API\x1b[0m");
    if token.is_empty() || token == "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
        println!("   \x1b[31m[FAIL]\x1b[0m BOT_TOKEN: Unconfigured (Run 'xiao setup')");
    } else {
        let bot = TelegramBotClient::new(&token);
        match bot.get_me().await {
            Ok(resp) if resp.ok && resp.result.is_some() => {
                let info = resp.result.unwrap();
                let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
                println!("   \x1b[1;32m[OK]\x1b[0m   BOT_TOKEN: Connected to @{} (ID: {})", uname, info.id);
            }
            Ok(resp) => {
                println!("   \x1b[1;31m[FAIL]\x1b[0m BOT_TOKEN: Invalid ({:?})", resp.description);
            }
            Err(e) => {
                println!("   \x1b[1;31m[FAIL]\x1b[0m BOT_TOKEN: Connection error ({e})");
            }
        }
    }

    println!("\n\x1b[1;37m2. ACTIVE AI PROVIDER\x1b[0m");
    if endpoint.is_empty() {
        println!("   \x1b[38;5;214m[WARN]\x1b[0m No AI provider configured (Run 'xiao provider add')");
    } else {
        println!("   Endpoint: {}", endpoint);
        println!("   Model:    \x1b[1;36m{}\x1b[0m", model);

        let (ok, res) = ai_service.fetch_models_from_endpoint(&endpoint, &api_key).await;
        if ok {
            let models = res.unwrap_or_default();
            println!("   \x1b[1;32m[OK]\x1b[0m   Endpoint Status: ONLINE ({} models available)", models.len());
            if models.iter().any(|m| m == &model) {
                println!("   \x1b[1;32m[OK]\x1b[0m   Model Verification: Confirmed available on server");
            } else {
                println!("   \x1b[38;5;214m[WARN]\x1b[0m Model Verification: Model '{model}' not listed in /models endpoint");
            }
        } else {
            let err = res.err().unwrap_or_else(|| "Offline".to_string());
            println!("   \x1b[1;31m[FAIL]\x1b[0m Endpoint Status: OFFLINE / Error ({err})");
        }

        let meta_guard = ai_service.model_metadata.read().await;
        let metadata = meta_guard.get(&ai::service::model_metadata_key(&endpoint, &model)).cloned();
        drop(meta_guard);
        let cap = crate::ai::service::get_model_capabilities_with_meta(&model, metadata.as_ref());
        println!("\n\x1b[1;37m3. MODEL CAPABILITIES ({})\x1b[0m", cap.family);
        println!("   • Context:  {}", cap.context_str);
        println!("   • Vision:   {}", cap.vision_desc);
        println!("   • Video:    {}", cap.video_desc);
        println!("   • Audio:    {}", cap.audio_desc);
        println!("   • Docs:     {}", cap.docs_desc);
        println!("   • CoT:      {}", cap.thinking_desc);
    }
    println!();
}

async fn run_cli_telegram_check() {
    load_environment();
    println!("\n\x1b[1;36m== Telegram Bot Status Check ==\x1b[0m");

    let token = env::var("BOT_TOKEN").unwrap_or_default();
    if token.is_empty() || token == "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
        println!("  \x1b[1;31m[FAIL]\x1b[0m BOT_TOKEN is not configured.");
        println!("  \x1b[38;5;244mRun 'xiao telegram bind' or 'xiao setup' to bind a token.\x1b[0m\n");
        return;
    }

    println!("  \x1b[38;5;244mConnecting to Telegram API...\x1b[0m");
    let bot = TelegramBotClient::new(&token);
    match bot.get_me().await {
        Ok(resp) if resp.ok && resp.result.is_some() => {
            let info = resp.result.unwrap();
            let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
            println!("  \x1b[1;32m[OK]\x1b[0m   Status: Connected & Verified");
            println!("  \x1b[1;37mBot Name:\x1b[0m {}", info.first_name);
            println!("  \x1b[1;37mUsername:\x1b[0m \x1b[1;36m@{}\x1b[0m", uname);
            println!("  \x1b[1;37mBot ID:\x1b[0m   {}", info.id);
            println!("  \x1b[1;37mBot Link:\x1b[0m https://t.me/{}", uname);
        }
        Ok(resp) => {
            println!("  \x1b[1;31m[FAIL]\x1b[0m Token is invalid ({:?})", resp.description);
        }
        Err(e) => {
            println!("  \x1b[1;31m[FAIL]\x1b[0m Verification error: {e}");
        }
    }
    println!();
}

async fn run_cli_telegram_bind(manual_token: Option<&str>) {
    load_environment();
    println!("\n\x1b[1;36m== Bind Telegram Bot Token ==\x1b[0m");

    let token = if let Some(t) = manual_token {
        t.trim().to_string()
    } else {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        print!("\x1b[1;37mEnter Telegram Bot Token:\x1b[0m ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if reader.read_line(&mut input).is_err() {
            println!("\n\x1b[38;5;244mCancelled.\x1b[0m\n");
            return;
        }
        input.trim().to_string()
    };

    if token.is_empty() {
        println!("\x1b[1;31m[ERROR] Token cannot be empty.\x1b[0m\n");
        return;
    }

    println!("  \x1b[38;5;244mVerifying token with Telegram API...\x1b[0m");
    let bot = TelegramBotClient::new(&token);
    match bot.get_me().await {
        Ok(resp) if resp.ok && resp.result.is_some() => {
            let info = resp.result.unwrap();
            let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
            println!("  \x1b[1;32m[OK] Token verified! Connected to @{} ({})\x1b[0m", uname, info.first_name);

            if let Err(e) = save_token_to_env(&token) {
                println!("  \x1b[1;31m[ERROR] Failed to save token: {e}\x1b[0m\n");
            } else {
                println!("  \x1b[1;32m[SUCCESS] Telegram bot token bound successfully!\x1b[0m\n");
            }
        }
        Ok(resp) => {
            println!("  \x1b[1;31m[FAIL] Invalid token: {:?}\x1b[0m\n", resp.description);
        }
        Err(e) => {
            println!("  \x1b[1;31m[FAIL] Verification error: {e}\x1b[0m\n");
        }
    }
}

async fn run_cli_telegram_change() {
    load_environment();
    println!("\n\x1b[1;36m== Change Telegram Bot Token ==\x1b[0m");

    let current_token = env::var("BOT_TOKEN").unwrap_or_default();
    if !current_token.is_empty() && current_token != "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
        let temp_bot = TelegramBotClient::new(&current_token);
        if let Ok(resp) = temp_bot.get_me().await {
            if let Some(info) = resp.result {
                let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
                println!("  \x1b[38;5;244mCurrent Bot:\x1b[0m \x1b[1;36m@{}\x1b[0m (ID: {})", uname, info.id);
            }
        }
    }

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    print!("\x1b[1;37mEnter New Telegram Bot Token:\x1b[0m ");
    let _ = io::stdout().flush();
    let mut input = String::new();
    if reader.read_line(&mut input).is_err() {
        println!("\n\x1b[38;5;244mCancelled.\x1b[0m\n");
        return;
    }
    let new_token = input.trim().to_string();
    if new_token.is_empty() {
        println!("\x1b[1;31m[ERROR] Token cannot be empty.\x1b[0m\n");
        return;
    }

    println!("  \x1b[38;5;244mVerifying new token with Telegram API...\x1b[0m");
    let bot = TelegramBotClient::new(&new_token);
    match bot.get_me().await {
        Ok(resp) if resp.ok && resp.result.is_some() => {
            let info = resp.result.unwrap();
            let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
            println!("  \x1b[1;32m[OK] Token valid! Connected to @{} ({})\x1b[0m", uname, info.first_name);

            if let Err(e) = save_token_to_env(&new_token) {
                println!("  \x1b[1;31m[ERROR] Failed to save token: {e}\x1b[0m\n");
            } else {
                println!("  \x1b[1;32m[SUCCESS] Telegram bot token updated successfully!\x1b[0m\n");
            }
        }
        Ok(resp) => {
            println!("  \x1b[1;31m[FAIL] Invalid token: {:?}\x1b[0m\n", resp.description);
        }
        Err(e) => {
            println!("  \x1b[1;31m[FAIL] Verification error: {e}\x1b[0m\n");
        }
    }
}

async fn run_cli_telegram_pick(ai_service: &AIChatService) {
    let mut store = load_provider_store();
    store.telegram_models.truncate(10);
    if store.providers.is_empty() {
        println!("\x1b[38;5;214m[WARN] No AI provider configured yet.\x1b[0m");
        return;
    }
    for provider in &mut store.providers {
        if provider.models.len() <= 1 && !provider.endpoint.is_empty() {
            if let (true, Ok(models)) = ai_service.fetch_models_from_endpoint(&provider.endpoint, &provider.api_key).await {
                if !models.is_empty() {
                    provider.models = models;
                }
            }
        }
    }
    let mut catalog: Vec<(String, String, String)> = Vec::new();
    for provider in &store.providers {
        for model in &provider.models {
            catalog.push((provider.id.clone(), model.clone(), format!("{} ({})", model, provider.name)));
        }
    }
    if catalog.is_empty() {
        println!("\x1b[38;5;214m[WARN] No models found.\x1b[0m");
        return;
    }
    let items: Vec<String> = catalog.iter().map(|(_, _, display)| display.clone()).collect();
    let selected_flags: Vec<bool> = catalog.iter().map(|(provider_id, model, _)| {
        let key = format!("{}::{}", provider_id, model);
        store.telegram_models.iter().any(|selected| selected == &key || selected == model)
    }).collect();
    let selected = terminal_interactive_multi_select("Pilih model yang ditampilkan di Telegram", &items, &selected_flags, 10);
    let Some(indices) = selected else { return; };
    store.telegram_models = indices.into_iter().filter_map(|idx| {
        let (provider_id, model, _) = catalog.get(idx)?;
        Some(format!("{}::{}", provider_id, model))
    }).collect();
    let _ = save_provider_store(&store);
    if store.telegram_models.is_empty() {
        println!("\n\x1b[1;32m[OK] Telegram model whitelist cleared; all models are visible.\x1b[0m");
    } else {
        println!("\n\x1b[1;32m[OK] {} model(s) enabled for Telegram (max 10).\x1b[0m", store.telegram_models.len());
    }
}

async fn run_cli_telegram_menu(action: Option<&str>, arg: Option<&str>) {
    load_environment();
    match action {
        Some("check") => {
            run_cli_telegram_check().await;
        }
        Some("bind") | Some("set") => {
            run_cli_telegram_bind(arg).await;
        }
        Some("change") | Some("update") => {
            run_cli_telegram_change().await;
        }
        _ => {
            run_cli_telegram_check().await;
        }
    }
}

fn print_cli_help() {
    println!("\n\x1b[1;36mUsage:\x1b[0m xiao [command] [args]\n");
    println!("\x1b[1;37mCommands:\x1b[0m");
    println!("  \x1b[36mstart\x1b[0m                               Run Telegram bot");
    println!("  \x1b[36msetup\x1b[0m                               Quickstart setup wizard");
    println!("  \x1b[36mprovider [add] [del] [status]\x1b[0m       Manage AI providers");
    println!("  \x1b[36mtelegram [check] [bind] [change]\x1b[0m    Manage Telegram bot token");
    println!("  \x1b[36mmodel [name] [pick]\x1b[0m                 Select, search, or whitelist models");
    println!("  \x1b[36mstatus\x1b[0m                              System health check");
    println!("  \x1b[36mhelp\x1b[0m                                Show this help\n");
}

// ==========================================
// Keyboard Interfaces
// ==========================================

fn get_main_menu_keyboard() -> Value {
    serde_json::to_value(ReplyKeyboardMarkup::from_strings(
        vec![
            vec!["ɴᴇᴡ", "ᴍᴏᴅᴇʟ", "ᴄᴏɴᴛᴇxᴛ"],
        ],
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
    sessions_len: usize,
    active_idx: usize,
    page: usize,
    page_size: usize,
) -> InlineKeyboardMarkup {
    let total_pages = 1.max((sessions_len + page_size - 1) / page_size);
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
        row_numbers.push(InlineKeyboardButton::callback(label, format!("session_select:{}", i)));
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
            InlineKeyboardButton::callback("‹", "noop")
        };
        let next_button = if curr_page < total_pages {
            InlineKeyboardButton::callback("›", format!("session_page:{next_page}"))
        } else {
            InlineKeyboardButton::callback("›", "noop")
        };
        rows.push(vec![
            previous_button,
            InlineKeyboardButton::callback(format!("{curr_page}/{total_pages}"), "noop"),
            next_button,
        ]);
    }

    rows.push(vec![
        InlineKeyboardButton::callback("ᴅᴇʟᴇᴛᴇ", format!("session_remove:{}", active_idx)),
        InlineKeyboardButton::callback("ʀᴇɴᴀᴍᴇ", format!("session_rename:{}", active_idx)),
        InlineKeyboardButton::callback("ɴᴇᴡ", "session_new"),
    ]);
    rows.push(vec![InlineKeyboardButton::callback("ᴄʟᴏꜱᴇ", "session_close")]);

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
            if let (true, Ok(fetched)) = ai_service.fetch_models_from_endpoint(&prov.endpoint, &prov.api_key).await {
                if !fetched.is_empty() {
                    ai_service.update_provider_models(user_id, &prov.id, fetched.clone()).await;
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

    // If setup mode and a specific provider is specified, only show that provider's models
    let target_providers: Vec<&ProviderConfig> = if is_setup && provider_id != "all" && !provider_id.is_empty() {
        providers.iter().filter(|p| p.id == provider_id).collect()
    } else {
        providers.iter().collect()
    };

    // Aggregate catalog across all target providers: (prov_id, prov_name, orig_model_idx, model_name, is_active)
    let mut display_models: Vec<(String, String, usize, String, bool)> = Vec::new();
    let telegram_whitelist = load_provider_store().telegram_models;
    for prov in target_providers {
        let is_this_prov_active = prov.id == active_prov_id;
        for (orig_idx, m) in prov.models.iter().enumerate() {
            let whitelist_key = format!("{}::{}", prov.id, m);
            if !is_setup && !telegram_whitelist.is_empty() && !telegram_whitelist.iter().any(|selected| selected == &whitelist_key || selected == m) {
                continue;
            }
            if is_search && !m.to_lowercase().contains(&q_low) && !prov.name.to_lowercase().contains(&q_low) {
                continue;
            }
            let is_model_active = is_this_prov_active && (m == &prov.active_model || m == &current_active_model);
            display_models.push((prov.id.clone(), prov.name.clone(), orig_idx, m.clone(), is_model_active));
        }
    }

    let total_models = display_models.len();
    let total_pages = 1.max((total_models + page_size - 1) / page_size);
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
        let prov_name = providers.iter().find(|p| p.id == provider_id).map(|p| p.name.as_str()).unwrap_or("Provider");
        format!(
            "✨ <b>Endpoint Terhubung! ({})</b>\n\n\
             📋 Ditemukan <b>{} model AI</b> pada endpoint ini.\n\
             Silakan <b>klik 1x pada model pilihan Anda</b> di bawah untuk langsung mengaktifkannya dan menyelesaikan setup:",
            prov_name, total_models
        )
    } else if is_search {
        if total_models == 0 {
            format!(
                "🔍 <b>Pencarian Model AI:</b> \"<code>{clean_query}</code>\"\n\n\
                 ⚠️ <i>Tidak ada model yang cocok dengan kata kunci tersebut di semua provider.</i>\n\
                 Silakan cari kata kunci lain atau sentuh tombol di bawah:"
            )
        } else {
            format!(
                "🔍 <b>Hasil Pencarian Model untuk:</b> \"<code>{clean_query}</code>\"\n\
                 Model aktif saat ini: <code>{}</code>\n\
                 Ditemukan: <b>{} model</b> (Halaman {}/{})\n\n\
                 Sentuh salah satu model di bawah untuk mengaktifkannya:",
                current_active_model, total_models, curr_page, total_pages
            )
        }
    } else {
        if is_multi_provider {
            format!(
                "<b>Model</b> (Total <b>{} model</b> dari <b>{} provider</b>)\n\
                 Model aktif saat ini: <code>{}</code> (Halaman {}/{})\n\n\
                 Sentuh salah satu model di bawah untuk langsung mengaktifkannya dalam 1x klik:",
                total_models, providers.len(), current_active_model, curr_page, total_pages
            )
        } else {
            let prov_name = providers.first().map(|p| p.name.as_str()).unwrap_or("Provider");
            format!(
                "<b>Model untuk '{}'</b>\n\
                 Model aktif saat ini: <code>{}</code>\n\
                 Total Model Tersedia: <b>{}</b> (Halaman {}/{})\n\n\
                 Sentuh salah satu model di bawah untuk langsung mengaktifkannya dalam 1x klik:",
                prov_name, current_active_model, total_models, curr_page, total_pages
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
            btn_txt = format!("{}...", &btn_txt[..27]);
        }
        current_row.push(InlineKeyboardButton::callback(btn_txt, format!("set_m:{p_id}:{orig_global_idx}")));
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
            nav_row.push(InlineKeyboardButton::callback("Prev", format!("provider_models:{target_nav_id}:{}", curr_page - 1)));
        }
        nav_row.push(InlineKeyboardButton::callback(format!("Hal {curr_page}/{total_pages}"), "noop"));
        if curr_page < total_pages {
            nav_row.push(InlineKeyboardButton::callback("Next", format!("provider_models:{target_nav_id}:{}", curr_page + 1)));
        }
        rows.push(nav_row);
    }

    if !is_setup {
        rows.push(vec![
            InlineKeyboardButton::callback("Close", "provider_close"),
        ]);
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
        Value::String(content) if !content.trim().is_empty() => truncate_session_name(content.trim(), 16),
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
    let total_pages = 1.max((total_sessions + page_size - 1) / page_size);
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
    let inline_kb = build_session_manager_inline_keyboard(sessions.len(), active_idx, page, 5);
    let kb_val = serde_json::to_value(inline_kb).ok();

    if let Some(mid) = message_id {
        if bot.edit_rich_message(chat_id, mid, &rich_msg, kb_val.clone()).await.is_ok() {
            ai_service.user_session_msg_id.write().await.insert(user_id, mid);
            return;
        }
    }

    let res = bot.send_rich_message(chat_id, &rich_msg, kb_val, None).await;
    if let Ok(val) = res {
        if let Some(new_id) = val.get("result").and_then(|r| r.get("message_id")).and_then(|m| m.as_i64()) {
            ai_service.user_session_msg_id.write().await.insert(user_id, new_id);
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

async fn build_context_monitor_ui(
    ai_service: &AIChatService,
    user_id: i64,
) -> InputRichMessage {
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
        cap_cells.push(RichBlockTableCell::text_only(&to_small_caps("VISION"), true, Some("center")));
    }
    if cap.documents {
        cap_cells.push(RichBlockTableCell::text_only(&to_small_caps("DOCUMENT"), true, Some("center")));
    }
    if cap.video {
        cap_cells.push(RichBlockTableCell::text_only(&to_small_caps("VIDEO"), true, Some("center")));
    }
    if cap.audio {
        cap_cells.push(RichBlockTableCell::text_only(&to_small_caps("AUDIO"), true, Some("center")));
    }
    if cap.thinking {
        cap_cells.push(RichBlockTableCell::text_only(&to_small_caps("THINKING"), true, Some("center")));
    }
    if cap_cells.is_empty() {
        cap_cells.push(RichBlockTableCell::text_only(&to_small_caps("TEXT"), true, Some("center")));
    }

    let progress_text = if stats.total_messages > 0 {
        format!(
            "[{}] {:.2}% | {}/{} Tokens | {} Pesan",
            stats.progress_bar,
            stats.usage_pct,
            used_str,
            limit_str,
            stats.total_messages
        )
    } else {
        format!(
            "[{}] {:.2}% | {}/{} Tokens",
            stats.progress_bar,
            stats.usage_pct,
            used_str,
            limit_str
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
        if bot.edit_rich_message(chat_id, mid, &rich_msg, None).await.is_ok() {
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
        let _ = bot.send_message(chat_id, text, Some("HTML"), Some(get_main_menu_keyboard()), None, None).await;
    } else {
        let text = "👋 <b>Hi, how can I help you?</b>\n\n\
                    <i>Silakan ketik pertanyaan Anda langsung atau gunakan tombol menu di bawah:</i>";
        let _ = bot.send_message(chat_id, &text, Some("HTML"), Some(get_main_menu_keyboard()), None, None).await;
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
        "apa itu", "apa arti", "jelaskan", "mengapa", "kenapa", "bagaimana cara", "cara ",
        "tutorial", "definisi", "what is", "why", "how to", "explain",
    ];
    if inquiry_prefixes.iter().any(|pref| t_lower.starts_with(pref)) {
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

    for pat in patterns {
        if let Some(caps) = Regex::new(pat).unwrap().captures(t) {
            if let Some(extracted_match) = caps.get(1) {
                let mut extracted = extracted_match.as_str().trim().to_string();
                let clean_re = Regex::new(r"(?i)^(?:tentang|mengenai|berupa|of|about|dong|ya|tolong)\s+").unwrap();
                extracted = clean_re.replace(&extracted, "").trim().to_string();

                let ext_low = extracted.to_lowercase();
                if ["dong", "ya", "ini", "itu", "nya", "tadi", "tersebut", "gambarnya", "fotonya"].contains(&ext_low.as_str()) {
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
        || ["gambarnya", "gambarnya dong", "itu", "yang tadi", "ini", "dong", "ya", "fotonya"].contains(&clean_prompt.as_str())
    {
        let sess = ai_service.get_active_session(user_id).await;
        let mut last_context = String::new();
        for msg in sess.messages.iter().rev() {
            if let Value::String(s) = &msg.content {
                if s.trim().len() > 8 {
                    last_context = s.trim().to_string();
                    break;
                }
            }
        }

        if !last_context.is_empty() {
            clean_prompt = format!("illustration of {}", &last_context[..last_context.len().min(250)]);
        } else {
            let last_guard = user_last_image_prompt.read().await;
            clean_prompt = last_guard
                .get(&user_id)
                .cloned()
                .unwrap_or_else(|| "majestic mountain scenery with clouds and ancient kingdom".to_string());
        }
    }

    if clean_prompt.is_empty() {
        let mut map = HashMap::new();
        map.insert("step".to_string(), "awaiting_image_prompt".to_string());
        ai_service.user_wizard_state.write().await.insert(user_id, map);

        let kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback("❌ Batalkan", "provider_cancel")]]);
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
    user_last_image_prompt.write().await.insert(user_id, clean_prompt.clone());

    let draft_id: i64 = rand::thread_rng().gen_range(100000..999999);
    let timeline = Arc::new(ExecutionTimeline::new(bot.clone(), chat_id, draft_id, 10));
    timeline.add_action("Generating Image", Some(ProgressActivity::Drawing)).await;
    timeline.sync_draft(true).await;
    timeline.start_ticker();
    let _ = bot.send_chat_action(chat_id, "upload_photo").await;

    let (success, img_bytes, engine_info) = ai_service.generate_image(user_id, &clean_prompt, 1024, 1024).await;
    timeline.stop_ticker();

    if !success || img_bytes.is_none() {
        timeline.fail_current(engine_info.clone()).await;
        timeline.sync_draft(true).await;

        let err_text = format!(
            "❌ <b>Gagal Membuat Gambar</b>\n\n\
             <b>Penyebab:</b> {engine_info}\n\n\
             Silakan coba lagi atau gunakan kata kunci/prompt yang berbeda."
        );
        let retry_kb = InlineKeyboardMarkup::new(vec![
            vec![InlineKeyboardButton::callback("🔄 Coba Lagi", "img_regen")],
            vec![InlineKeyboardButton::callback("📱 Menu Utama", "action_menu")],
        ]);
        let _ = bot.send_message(chat_id, &err_text, Some("HTML"), serde_json::to_value(retry_kb).ok(), None, None).await;
        return;
    }

    let caption_text = format!(
        "🫟 <b>Gambar Berhasil Dibuat!</b>\n\n\
         📝 <b>Prompt:</b> <i>\"{clean_prompt}\"</i>\n\
         ⚡ <b>Engine:</b> <code>{engine_info}</code>"
    );

    let img_kb = InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("🔄 Buat Ulang (Regenerate)", "img_regen"),
            InlineKeyboardButton::callback("🫟 Gambar Baru", "img_new"),
        ],
        vec![InlineKeyboardButton::callback("📱 Buka Menu", "action_menu")],
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
    user_prompt: &str,
    image_bytes: Option<Vec<u8>>,
    mime_type: Option<&str>,
    doc_text: Option<&str>,
    doc_name: Option<&str>,
    audio_bytes: Option<Vec<u8>>,
    audio_mime: Option<&str>,
    video_bytes: Option<Vec<u8>>,
    video_mime: Option<&str>,
    video_duration: Option<i32>,
) {
    let draft_id: i64 = rand::thread_rng().gen_range(100000..999999);
    let timeline = Arc::new(ExecutionTimeline::new(bot.clone(), chat_id, draft_id, 30));

    let (initial_lbl, initial_act) = if video_bytes.is_some() {
        ("Watching", ProgressActivity::Watching)
    } else if image_bytes.is_some() {
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

    let (_thinking, mut answer_text) = ai_service
        .generate_response(
            user_id,
            user_prompt,
            Some(&timeline),
            image_bytes,
            mime_type,
            doc_text,
            doc_name,
            audio_bytes,
            audio_mime,
            video_bytes,
            video_mime,
            video_duration,
        )
        .await;

    timeline.stop_ticker();

    if answer_text.trim().is_empty() {
        answer_text = "Maaf, respon AI kosong untuk permintaan ini.".to_string();
    }

    let full_rich_msg = build_full_rich_message(&answer_text, Some(&current_model));
    let res = bot
        .send_rich_message(chat_id, &full_rich_msg, Some(get_collapsed_menu_keyboard()), None)
        .await;

    if res.is_err() || !res.unwrap().get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let _ = bot
            .send_message(chat_id, &answer_text, Some("HTML"), Some(get_collapsed_menu_keyboard()), None, None)
            .await;
    }
}

// ==========================================
// Update Router
// ==========================================

async fn handle_update(
    bot: &TelegramBotClient,
    ai_service: &AIChatService,
    user_last_image_prompt: &UserLastImagePrompt,
    update: Update,
) {
    if let Some(msg) = update.message {
        let chat_id = msg.chat.id;
        let user_id = msg.from.as_ref().map(|u| u.id).unwrap_or(chat_id);
        let _user_name = msg.from.as_ref().map(|u| u.first_name.as_str()).unwrap_or("Pengguna");
        let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("").trim().to_string();

        let mut image_bytes = None;
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
                let ext = path.split('.').last().unwrap_or("mp4");
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
                    let ext = path.split('.').last().unwrap_or("jpeg");
                    mime_type = Some(if ext == "jpg" { "image/jpeg".to_string() } else { format!("image/{ext}") });
                }
            }
        } else if let Some(doc) = msg.document {
            let d_mime = doc.mime_type.clone().unwrap_or_default();
            let d_name = doc.file_name.clone().unwrap_or_else(|| "dokumen".to_string());
            if let Some((data, path)) = bot.get_file_bytes(&doc.file_id).await {
                if d_mime.starts_with("image/") || [".png", ".jpg", ".jpeg", ".webp"].iter().any(|ext| path.to_lowercase().ends_with(ext)) {
                    image_bytes = Some(data);
                    mime_type = Some(if d_mime.is_empty() { "image/jpeg".to_string() } else { d_mime });
                } else if d_mime.starts_with("video/") || [".mp4", ".mov", ".avi", ".webm", ".mkv"].iter().any(|ext| path.to_lowercase().ends_with(ext)) {
                    video_bytes = Some(data);
                    video_mime = Some(if d_mime.is_empty() { "video/mp4".to_string() } else { d_mime });
                } else if d_mime.starts_with("audio/") || [".ogg", ".mp3", ".wav", ".m4a"].iter().any(|ext| path.to_lowercase().ends_with(ext)) {
                    audio_bytes = Some(data);
                    audio_mime = Some(if d_mime.is_empty() { "audio/ogg".to_string() } else { d_mime });
                } else {
                    doc_text = Some(String::from_utf8_lossy(&data).to_string());
                    doc_name = Some(d_name);
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
                    let _ = bot.send_message(chat_id, "❌ <b>Aksi dibatalkan.</b>", Some("HTML"), Some(get_collapsed_menu_keyboard()), None, None).await;
                    return;
                }

                let step = wizard.get("step").map(|s| s.as_str()).unwrap_or("");
                if step == "awaiting_image_prompt" {
                    handle_image_generation(bot, ai_service, user_last_image_prompt, chat_id, user_id, &text).await;
                    return;
                }
            }
        }

        // Rename session handler
        let rename_opt = {
            let guard = ai_service.user_waiting_rename.read().await;
            guard.get(&user_id).copied()
        };
        if let Some(target_idx) = rename_opt {
            if !text.is_empty() && !text.starts_with('/') {
                ai_service.user_waiting_rename.write().await.remove(&user_id);
                let orig_msg_id = ai_service.user_rename_msg_id.write().await.remove(&user_id);
                ai_service.rename_session(user_id, target_idx, &text).await;

                let _ = bot
                    .send_message(
                        chat_id,
                        &format!("✅ Session #{} berhasil diubah namanya menjadi: <b>{}</b>", target_idx + 1, text),
                        Some("HTML"),
                        Some(get_collapsed_menu_keyboard()),
                        None,
                        None,
                    )
                    .await;
                send_or_update_session_manager(bot, ai_service, chat_id, user_id, orig_msg_id, 1).await;
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
                .transcribe_audio(user_id, a_bytes.clone(), &format!("voice_{}.ogg", msg.message_id))
                .await;

            if stt_ok {
                let user_prompt = transcript_res.unwrap_or_default();
                if let Some(img_p) = extract_image_intent_prompt(&user_prompt) {
                    handle_image_generation(bot, ai_service, user_last_image_prompt, chat_id, user_id, &img_p).await;
                    return;
                }
                let prompt_fmt = format!(
                    "[Pesan Suara ({} detik)]: \"{}\"\n\nJawab pertanyaan atau tanggapi pesan suara di atas secara mendalam dan jelas.",
                    audio_duration, user_prompt
                );
                handle_ai_chat(
                    bot, ai_service, chat_id, user_id, &prompt_fmt, None, None, None, None, None, None, None, None, None,
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
                    &prompt_audio,
                    None,
                    None,
                    None,
                    None,
                    Some(a_bytes),
                    audio_mime.as_deref(),
                    None,
                    None,
                    None,
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
                &prompt_video,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(v_bytes),
                video_mime.as_deref(),
                Some(video_duration),
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
        } else if ["📱 Menu", "Menu", "/menu", "🔙 Menu Utama", "🔙 Kembali ke Menu Utama", "Menu Utama", "Main Menu", "main menu", "Main menu"].contains(&text.as_str()) {
            send_welcome(bot, ai_service, chat_id, user_id).await;
        } else if text.starts_with("/new") || ["ɴᴇᴡ", "➕ ɴᴇᴡ", "➕ New", "New", "new", "➕ Chat Baru", "Chat Baru"].contains(&text.as_str()) {
            ai_service.create_new_session(user_id, None).await;
            let total_sessions = ai_service.get_sessions(user_id).await.len();
            let target_page = (total_sessions - 1) / 5 + 1;
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
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page).await;
        } else if (text == "/session" || text.starts_with("/session ")) || ["📑 Session", "Session", "session", "📑 Session Manager", "📑 Lihat Daftar Session"].contains(&text.as_str()) {
            let active_idx = ai_service.get_active_session_index(user_id).await;
            let target_page = (active_idx / 5) + 1;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page).await;
        } else if ["🗑️ Hapus Session", "🗑️ Remove Session", "Delete", "delete", "Delete Session"].contains(&text.as_str()) {
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
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page).await;
        } else if ["✏️ Rename Session", "✏️ Ubah Nama Session", "Rename", "rename", "Rename Session"].contains(&text.as_str()) {
            let active_idx = ai_service.get_active_session_index(user_id).await;
            ai_service.user_waiting_rename.write().await.insert(user_id, active_idx);
            let _ = bot
                .send_message(
                    chat_id,
                    &format!("✏️ <b>Ketikkan nama baru untuk Session #{}:</b>", active_idx + 1),
                    Some("HTML"),
                    None,
                    None,
                    None,
                )
                .await;
        } else if let Some(caps) = Regex::new(r"(?i)Hal(?:aman)?\s*([0-9]+)").unwrap().captures(&text) {
            if ["Hal", "▶", "◀"].iter().any(|k| text.contains(k)) {
                let target_page: usize = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(1);
                send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page).await;
            }
        } else if let Some(caps) = Regex::new(r"^(?:✅\s*|Session\s*)?([0-9]+)$").unwrap().captures(text.trim()) {
            if text.trim().chars().all(|c| c.is_ascii_digit()) || text.trim().starts_with("✅") || text.trim().starts_with("Session ") {
                let num: usize = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(1);
                let idx = num.saturating_sub(1);
                let sessions = ai_service.get_sessions(user_id).await;
                if idx < sessions.len() {
                    ai_service.switch_session(user_id, idx).await;
                    let target_page = (idx / 5) + 1;
                    send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, target_page).await;
                } else {
                    handle_ai_chat(
                        bot,
                        ai_service,
                        chat_id,
                        user_id,
                        &text,
                        image_bytes,
                        mime_type.as_deref(),
                        doc_text.as_deref(),
                        doc_name.as_deref(),
                        None,
                        None,
                        None,
                        None,
                        None,
                    )
                    .await;
                }
            }
        } else if text.starts_with("/context") || ["ᴄᴏɴᴛᴇxᴛ", "🧠 ᴄᴏɴᴛᴇxᴛ", "🧠 Context", "Context", "context", "🧠 Info Konteks", "Info Konteks"].contains(&text.as_str()) {
            send_or_update_context_monitor(bot, ai_service, chat_id, user_id, None).await;
        } else if text.starts_with("/model") || ["ᴍᴏᴅᴇʟ", "⚙️ ᴍᴏᴅᴇʟ", "⚙️ Model", "Model", "model", "⚙️ Model AI", "Pilih Model"].contains(&text.as_str()) {
            if ai_service.has_configured_provider(user_id).await {
                let (msg_txt, kb_m) = build_provider_model_picker(ai_service, user_id, "all", 1, 8, false, None).await;
                let _ = bot.send_message(chat_id, &msg_txt, Some("HTML"), serde_json::to_value(kb_m).ok(), None, None).await;
            }
        } else if text.starts_with("⚡ ") {
            let selected_model = text[2..].trim();
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
                ai_service.set_provider_model(user_id, &prov.id, selected_model).await;
                ai_service.set_user_model(user_id, selected_model).await;
                let _ = bot
                    .send_message(
                        chat_id,
                        &format!("✅ <b>Model AI diubah ke:</b> <code>{selected_model}</code> (<i>{}</i>)", prov.name),
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
                    &text,
                    image_bytes,
                    mime_type.as_deref(),
                    doc_text.as_deref(),
                    doc_name.as_deref(),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .await;
            }
        } else if text.starts_with("/image") || ["🫟 Buat Gambar", "🫟 Generate Gambar", "📸 Buat Gambar", "🎨 Buat Gambar", "Buat Gambar"].contains(&text.as_str()) {
            let prompt_arg = if text.starts_with("/image") {
                text.strip_prefix("/image").unwrap_or("").trim()
            } else {
                ""
            };
            handle_image_generation(bot, ai_service, user_last_image_prompt, chat_id, user_id, prompt_arg).await;
        } else if text.starts_with("/clear") || ["🗑️ Reset Chat", "🗑️ Reset Obrolan"].contains(&text.as_str()) {
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
        } else if text.starts_with("/help") || ["📖 Bantuan", "📖 Bantuan & Info"].contains(&text.as_str()) {
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
            let _ = bot.send_message(chat_id, help_text, Some("HTML"), Some(get_main_menu_keyboard()), None, None).await;
        } else {
            let auto_img_prompt = if image_bytes.is_none() && doc_text.is_none() {
                extract_image_intent_prompt(&text)
            } else {
                None
            };

            if let Some(ref img_p) = auto_img_prompt {
                handle_image_generation(bot, ai_service, user_last_image_prompt, chat_id, user_id, img_p).await;
            } else {
                handle_ai_chat(
                    bot,
                    ai_service,
                    chat_id,
                    user_id,
                    &text,
                    image_bytes,
                    mime_type.as_deref(),
                    doc_text.as_deref(),
                    doc_name.as_deref(),
                    None,
                    None,
                    video_bytes,
                    video_mime.as_deref(),
                    Some(video_duration),
                )
                .await;
            }
        }
    } else if let Some(cq) = update.callback_query {
        let cq_id = cq.id;
        let cq_data = cq.data.unwrap_or_default();
        let user_id = cq.from.id;
        let chat_id = cq.message.as_ref().map(|m| m.chat.id).unwrap_or(user_id);
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

        if let Some(idx_str) = cq_data.strip_prefix("session_select:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                ai_service.switch_session(user_id, idx).await;
                let target_page = (idx / 5) + 1;
                let _ = bot.answer_callback_query(&cq_id, Some(&format!("Beralih ke Session #{} ✅", idx + 1)), false).await;
                send_or_update_session_manager(bot, ai_service, chat_id, user_id, msg_id, target_page).await;
            }
        } else if cq_data == "session_new" {
            ai_service.create_new_session(user_id, None).await;
            let total_sessions = ai_service.get_sessions(user_id).await.len();
            let target_page = (total_sessions - 1) / 5 + 1;
            let _ = bot.answer_callback_query(&cq_id, Some("Session baru berhasil dibuat! ➕"), false).await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, msg_id, target_page).await;
        } else if let Some(idx_str) = cq_data.strip_prefix("session_remove:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                ai_service.remove_session(user_id, idx).await;
                let new_active = ai_service.get_active_session_index(user_id).await;
                let target_page = (new_active / 5) + 1;
                let _ = bot.answer_callback_query(&cq_id, Some("Session berhasil dihapus 🗑️"), false).await;
                send_or_update_session_manager(bot, ai_service, chat_id, user_id, msg_id, target_page).await;
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
                        "<b>{name}</b>\n\nMessages: <b>{}</b>\nCreated: <code>{}</code>\nLast: <code>{}</code>",
                        session.messages.len() / 2,
                        session.created_at,
                        session_last_activity(session)
                    );
                    let _ = bot.answer_callback_query(&cq_id, None, false).await;
                    let _ = bot.send_message(chat_id, &detail, Some("HTML"), None, None, None).await;
                }
            }
        } else if let Some(idx_str) = cq_data.strip_prefix("session_rename:") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                ai_service.user_waiting_rename.write().await.insert(user_id, idx);
                if let Some(mid) = msg_id {
                    ai_service.user_rename_msg_id.write().await.insert(user_id, mid);
                }
                let _ = bot.answer_callback_query(&cq_id, Some("Silakan ketik nama baru"), false).await;
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
            let _ = bot.answer_callback_query(&cq_id, Some("Menu ditutup"), false).await;
        } else if cq_data == "open_session" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, 1).await;
        } else if let Some(rest) = cq_data.strip_prefix("provider_models:") {
            let parts: Vec<&str> = rest.split(':').collect();
            let prov_id = parts[0];
            let target_page: usize = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(1);
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            let (text_m, kb_m) = build_provider_model_picker(ai_service, user_id, prov_id, target_page, 8, false, None).await;
            let kb_val = serde_json::to_value(kb_m).ok();

            if let Some(mid) = msg_id {
                if bot.edit_message_text(Some(chat_id), Some(mid), &text_m, Some("HTML"), kb_val.clone()).await.is_err() {
                    let _ = bot.send_message(chat_id, &text_m, Some("HTML"), kb_val, None, None).await;
                }
            }
        } else if let Some(rest) = cq_data.strip_prefix("set_m:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() == 2 {
                let prov_id = parts[0];
                let model_idx: usize = parts[1].parse().unwrap_or(0);

                let model_name_opt = ai_service.get_provider_model_by_index(user_id, prov_id, model_idx).await;
                if let Some(model_name) = model_name_opt {
                    ai_service.set_active_provider(user_id, prov_id).await;
                    ai_service.set_provider_model(user_id, prov_id, &model_name).await;
                    ai_service.set_user_model(user_id, &model_name).await;
                    let _ = bot.answer_callback_query(&cq_id, Some(&format!("Model: {model_name} Aktif! ✅")), false).await;

                    let providers = ai_service.get_user_providers(user_id).await;
                    let target_prov = providers.iter().find(|p| p.id == prov_id);
                    let prov_name = target_prov.map(|p| p.name.as_str()).unwrap_or("Custom Provider");
                    let endpoint_url = target_prov.map(|p| p.endpoint.as_str()).unwrap_or("");

                    let done_text = format!(
                        "🎉 <b>Model AI Berhasil Diubah!</b>\n\n\
                         🌐 <b>Provider:</b> <b>{prov_name}</b>\n\
                         🔗 <b>Endpoint:</b> <code>{endpoint_url}</code>\n\
                         ⚡ <b>Model Aktif:</b> <code>{model_name}</code>\n\n\
                         💬 <i>Silakan langsung ketik pesan Anda untuk mulai mengobrol.</i>"
                    );

                    let done_kb = InlineKeyboardMarkup::new(vec![vec![
                        InlineKeyboardButton::callback("⚙️ Ganti Model", format!("provider_models:{prov_id}:1")),
                        InlineKeyboardButton::callback("✖️ Tutup", "provider_close"),
                    ]]);
                    let done_val = serde_json::to_value(done_kb).ok();

                    if let Some(mid) = msg_id {
                        if bot.edit_message_text(Some(chat_id), Some(mid), &done_text, Some("HTML"), done_val.clone()).await.is_err() {
                            let _ = bot.send_message(chat_id, &done_text, Some("HTML"), done_val, None, None).await;
                        }
                    } else {
                        let _ = bot.send_message(chat_id, &done_text, Some("HTML"), done_val, None, None).await;
                    }
                } else {
                    let _ = bot.answer_callback_query(&cq_id, Some("Model tidak ditemukan."), false).await;
                }
            }
        } else if cq_data == "provider_cancel" {
            ai_service.user_wizard_state.write().await.remove(&user_id);
            let _ = bot.answer_callback_query(&cq_id, Some("Aksi dibatalkan"), false).await;
            if let Some(mid) = msg_id {
                let _ = bot.delete_message(chat_id, mid).await;
            }
        } else if cq_data == "img_new" {
            let _ = bot.answer_callback_query(&cq_id, None, false).await;
            handle_image_generation(bot, ai_service, user_last_image_prompt, chat_id, user_id, "").await;
        } else if cq_data == "img_regen" {
            let last_guard = user_last_image_prompt.read().await;
            let last_p = last_guard.get(&user_id).cloned().unwrap_or_else(|| "cyberpunk aesthetic landscape".to_string());
            drop(last_guard);
            let _ = bot.answer_callback_query(&cq_id, Some("Membuat ulang gambar... 🫟"), false).await;
            handle_image_generation(bot, ai_service, user_last_image_prompt, chat_id, user_id, &last_p).await;
        } else if cq_data == "context_refresh" || cq_data == "open_context" || cq_data == "show_context" {
            let _ = bot.answer_callback_query(&cq_id, Some("Konteks diperbarui! 🧠"), false).await;
            send_or_update_context_monitor(bot, ai_service, chat_id, user_id, msg_id).await;
        } else if cq_data == "open_new_session" {
            let new_sess = ai_service.create_new_session(user_id, None).await;
            let _ = bot.answer_callback_query(&cq_id, Some(&format!("Sesi #{} dibuat! ✨", new_sess.id)), false).await;
            let _ = bot
                .send_message(
                    chat_id,
                    &format!("✨ <b>Sesi Baru Berhasil Dibuat!</b>\nSesi aktif saat ini: <b>{}</b>", new_sess.name),
                    Some("HTML"),
                    Some(get_collapsed_menu_keyboard()),
                    None,
                    None,
                )
                .await;
            send_or_update_session_manager(bot, ai_service, chat_id, user_id, None, 1).await;
        } else if cq_data == "action_clear" {
            ai_service.clear_history(user_id).await;
            let _ = bot.answer_callback_query(&cq_id, Some("Konteks direset! 🧹"), false).await;
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

    let bot = TelegramBotClient::new(token);
    let user_last_image_prompt: UserLastImagePrompt = Arc::new(RwLock::new(HashMap::new()));

    // Test connection
    match bot.get_me().await {
        Ok(resp) if resp.ok && resp.result.is_some() => {
            let bot_info = resp.result.unwrap();
            println!("\n🚀 XiaoAI @{} online menggunakan Telegram Bot API 10.2!", bot_info.username.unwrap_or_default());
            println!("⚡ Full Live Execution Timeline Active!");
            println!("🌐 Custom OpenAI-Compatible Provider Setup Active (via CLI)\n");
        }
        Ok(resp) => {
            error!("Gagal terhubung ke Telegram Bot API: {:?}", resp.description);
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

    let mut offset: Option<i64> = None;
    info!("Memulai polling pesan...");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n🛑 Menerima sinyal berhenti. Bot dimatikan secara aman.");
                break;
            }
            updates_res = bot.get_updates(offset, 100, 20, None) => {
                match updates_res {
                    Ok(resp) if resp.ok => {
                        if let Some(updates) = resp.result {
                            for update in updates {
                                offset = Some(update.update_id + 1);
                                let bot_clone = bot.clone();
                                let ai_clone = Arc::clone(&ai_service);
                                let last_img_clone = Arc::clone(&user_last_image_prompt);

                                tokio::spawn(async move {
                                    handle_update(&bot_clone, &ai_clone, &last_img_clone, update).await;
                                });
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
}
