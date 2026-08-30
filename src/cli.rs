use rand::Rng;
use std::env;
use std::io::{self, BufRead, Write};

use crate::ai::AIChatService;
use crate::bot::client::TelegramBotClient;
use crate::{
    get_configured_owner_id, get_configured_token, load_environment, save_env_kv, save_token_to_env,
};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    style::Print,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::ai::service::{load_provider_store, save_provider_store, ProviderConfig};

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
            buffer
                .push("\x1b[38;5;243m[▲/▼ Navigate • Enter Select • Esc Back]\x1b[0m".to_string());
        }
        buffer.push(
            "\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m"
                .to_string(),
        );

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
                buffer.push(format!(
                    "  \x1b[38;5;240m▼ ({} more items below)\x1b[0m",
                    filtered.len() - end_idx
                ));
            }
        }
        buffer.push(
            "\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m"
                .to_string(),
        );

        // Clear and print at top of alternate screen
        let _ = execute!(
            stdout,
            cursor::MoveTo(0, 0),
            Clear(ClearType::All),
            Print(buffer.join("\r\n"))
        );
        let _ = stdout.flush();

        // Read event
        if let Ok(Event::Key(KeyEvent {
            code, modifiers, ..
        })) = event::read()
        {
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
                    selected_pos = selected_pos.saturating_sub(1);
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

fn terminal_interactive_multi_select(
    title: &str,
    items: &[String],
    selected: &[bool],
    max_selected: usize,
) -> Option<Vec<usize>> {
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
            items
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| item.to_lowercase().contains(&q).then_some(idx))
                .collect()
        };
        if filtered.is_empty() {
            cursor_idx = 0;
            top_idx = 0;
        } else {
            cursor_idx = cursor_idx.min(filtered.len() - 1);
            if cursor_idx < top_idx {
                top_idx = cursor_idx;
            }
            if cursor_idx >= top_idx + page_size {
                top_idx = cursor_idx + 1 - page_size;
            }
        }
        let end_idx = (top_idx + page_size).min(filtered.len());
        let mut buffer = vec![format!("\x1b[1;38;5;39m{}\x1b[0m", title)];
        buffer.push(format!("\x1b[38;5;214mFilter:\x1b[0m \x1b[1;37m{}_\x1b[0m  \x1b[38;5;240m[▲/▼ Navigate • Space Toggle • Enter Save • Esc Save]\x1b[0m", query));
        buffer.push(format!(
            "\x1b[38;5;244mSelected: {}/{} • Showing {}-{} of {}\x1b[0m",
            picked.iter().filter(|v| **v).count(),
            max_selected,
            if filtered.is_empty() { 0 } else { top_idx + 1 },
            end_idx,
            filtered.len()
        ));
        buffer.push(
            "\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m"
                .to_string(),
        );
        for (display_idx, idx) in filtered[top_idx..end_idx].iter().enumerate() {
            let marker = if picked[*idx] { "[x]" } else { "[ ]" };
            let is_cursor = top_idx + display_idx == cursor_idx;
            if is_cursor {
                buffer.push(format!(
                    "\x1b[1;38;5;42m > \x1b[1;37m{} {:>3}. {}\x1b[0m",
                    marker,
                    idx + 1,
                    items[*idx]
                ));
            } else {
                buffer.push(format!(
                    "   \x1b[38;5;244m{} {:>3}. \x1b[38;5;250m{}\x1b[0m",
                    marker,
                    idx + 1,
                    items[*idx]
                ));
            }
        }
        let chosen: Vec<&str> = picked
            .iter()
            .enumerate()
            .filter_map(|(idx, value)| value.then_some(items[idx].as_str()))
            .collect();
        if !chosen.is_empty() {
            buffer.push(
                "\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m"
                    .to_string(),
            );
            buffer.push("\x1b[38;5;214mPicked:\x1b[0m".to_string());
            for model in chosen {
                buffer.push(format!("  \x1b[38;5;250m- {}\x1b[0m", model));
            }
        }
        buffer.push(
            "\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m"
                .to_string(),
        );
        buffer.push(
            "\x1b[38;5;238m────────────────────────────────────────────────────────────\x1b[0m"
                .to_string(),
        );
        let _ = execute!(
            stdout,
            cursor::MoveTo(0, 0),
            Clear(ClearType::All),
            Print(buffer.join("\r\n"))
        );
        let _ = stdout.flush();

        if let Ok(Event::Key(KeyEvent { code, .. })) = event::read() {
            match code {
                KeyCode::Esc => {
                    return Some(
                        picked
                            .iter()
                            .enumerate()
                            .filter_map(|(idx, chosen)| chosen.then_some(idx))
                            .collect(),
                    )
                }
                KeyCode::Up => cursor_idx = cursor_idx.saturating_sub(1),
                KeyCode::Down => {
                    cursor_idx = (cursor_idx + 1).min(filtered.len().saturating_sub(1))
                }
                KeyCode::PageUp => cursor_idx = cursor_idx.saturating_sub(page_size),
                KeyCode::PageDown => {
                    cursor_idx = (cursor_idx + page_size).min(filtered.len().saturating_sub(1))
                }
                KeyCode::Char(' ') => {
                    if let Some(&idx) = filtered.get(cursor_idx) {
                        if picked[idx] || picked.iter().filter(|v| **v).count() < max_selected {
                            picked[idx] = !picked[idx];
                        }
                    }
                }
                KeyCode::Enter => {
                    return Some(
                        picked
                            .iter()
                            .enumerate()
                            .filter_map(|(idx, chosen)| chosen.then_some(idx))
                            .collect(),
                    )
                }
                KeyCode::Backspace => {
                    query.pop();
                    cursor_idx = 0;
                    top_idx = 0;
                }
                KeyCode::Char(c) => {
                    query.push(c);
                    cursor_idx = 0;
                    top_idx = 0;
                }
                _ => {}
            }
        }
    }
}

pub(crate) async fn run_cli_quickstart_wizard(ai_service: &AIChatService) -> Option<String> {
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
    let (ok, res) = ai_service
        .fetch_models_from_endpoint(&endpoint, &api_key)
        .await;

    if !ok {
        let err = res.err().unwrap_or_else(|| "Unknown error".to_string());
        println!("\x1b[1;31m[FAIL] Could not connect to provider: {err}\x1b[0m");
        return None;
    }

    let models = res.unwrap_or_else(|_| vec!["gpt-4o".to_string()]);
    println!(
        "\x1b[1;32m[OK] Connected! Found {} models.\x1b[0m",
        models.len()
    );

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
        models
            .first()
            .cloned()
            .unwrap_or_else(|| "gpt-4o".to_string())
    };

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
        api_key_ref: None,
        models: models.clone(),
        active_model: active_model.clone(),
    };

    let mut store = load_provider_store();
    store.providers.push(provider.clone());
    store.active_id = Some(provider_id);
    if let Err(error) = save_provider_store(&store) {
        println!("\x1b[1;31m[ERROR] Provider configuration was not saved: {error}\x1b[0m");
        return None;
    }
    if !ai_service.reload_provider_store().await {
        println!("\x1b[1;31m[ERROR] Provider was committed but runtime reload failed. Restart XiaoAI before continuing.\x1b[0m");
        return None;
    }
    let probe = ai_service
        .probe_model_capabilities(&provider, &active_model)
        .await;
    println!(
        "\x1b[38;5;244mCapability probe: vision={:?}, tools={:?}, structured={:?}\x1b[0m",
        probe.supports_image, probe.supports_tools, probe.supports_structured_output
    );

    println!(
        "\x1b[1;32m[OK] AI Provider '{}' configured with model '{}'!\x1b[0m",
        clean_alias, active_model
    );

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
            Ok(resp) if resp.ok => {
                let Some(bot_info) = resp.result else {
                    println!("\x1b[1;31m[FAIL] Telegram returned ok=true without bot information.\x1b[0m");
                    continue;
                };
                let username = bot_info.username.unwrap_or_else(|| "Unknown".to_string());
                println!(
                    "\x1b[1;32m[OK] Token valid! Connected to @{} ({})\x1b[0m",
                    username, bot_info.first_name
                );

                if let Err(error) = save_token_to_env(&user_token) {
                    println!(
                        "\x1b[1;31m[ERROR] Token valid but could not be saved: {error}\x1b[0m"
                    );
                    return None;
                }
                break (user_token, username);
            }
            Ok(resp) => {
                let desc = resp
                    .description
                    .unwrap_or_else(|| "Invalid token".to_string());
                println!("\x1b[1;31m[FAIL] Invalid token: {desc}\x1b[0m");
            }
            Err(e) => {
                println!("\x1b[1;31m[FAIL] Verification error: {e}\x1b[0m");
            }
        }
    };

    println!("\n\x1b[1;37m[3/3] Configure Telegram Owner\x1b[0m");
    println!("\x1b[38;5;244mMasukkan numeric Telegram user ID Anda. Hanya owner ini yang boleh menggunakan Xiao.\x1b[0m");
    let owner_user_id = loop {
        print!("\x1b[1;37mOwner User ID:\x1b[0m ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if reader.read_line(&mut input).is_err() {
            println!("\n\x1b[38;5;244mSetup cancelled.\x1b[0m");
            return None;
        }
        match input.trim().parse::<i64>() {
            Ok(value) if value > 0 => break value,
            _ => println!("\x1b[1;31m[ERROR] Owner User ID harus berupa angka positif.\x1b[0m"),
        }
    };
    if let Err(error) = save_env_kv("OWNER_USER_ID", &owner_user_id.to_string()) {
        println!("\x1b[1;31m[ERROR] Owner ID could not be saved: {error}\x1b[0m");
        return None;
    }

    println!("\n\x1b[1;36m== Quickstart Setup Complete! ==\x1b[0m");
    println!(
        "  \x1b[1;37mBot:\x1b[0m          \x1b[1;32m@{}\x1b[0m",
        bot_username
    );
    println!("  \x1b[1;37mProvider:\x1b[0m     {}", clean_alias);
    println!(
        "  \x1b[1;37mActive Model:\x1b[0m \x1b[1;36m{}\x1b[0m",
        active_model
    );
    println!("  \x1b[1;37mEndpoint:\x1b[0m     {}", endpoint);
    println!("  \x1b[1;37mOwner ID:\x1b[0m     {}", owner_user_id);
    println!("\n\x1b[1;32m🎉 Everything is set up! Run 'xiao start' to launch the bot.\x1b[0m\n");

    Some(final_token)
}

pub(crate) async fn get_or_prompt_token(ai_service: &AIChatService) -> Option<String> {
    if let Some(token) = get_configured_token() {
        if ai_service.has_configured_provider(0).await {
            return Some(token);
        }
    }
    println!(
        "\x1b[38;5;214m[WARN] Configuration missing. Starting Quickstart Setup Wizard...\x1b[0m"
    );
    run_cli_quickstart_wizard(ai_service).await
}

pub(crate) async fn run_cli_provider_add(ai_service: &AIChatService) {
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
    if endpoint.is_empty()
        || (!endpoint.starts_with("http://") && !endpoint.starts_with("https://"))
    {
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
    let (ok, res) = ai_service
        .fetch_models_from_endpoint(&endpoint, &api_key)
        .await;

    if !ok {
        let err = res.err().unwrap_or_else(|| "Unknown error".to_string());
        println!("\x1b[1;31m[FAIL] Could not connect to provider: {err}\x1b[0m");
        return;
    }

    let models = res.unwrap_or_else(|_| vec!["gpt-4o".to_string()]);
    println!(
        "\x1b[1;32m[OK] Connected! Found {} models.\x1b[0m",
        models.len()
    );

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
        models
            .first()
            .cloned()
            .unwrap_or_else(|| "gpt-4o".to_string())
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
        api_key_ref: None,
        models: models.clone(),
        active_model: active_model.clone(),
    };

    let mut store = load_provider_store();
    store.providers.push(provider.clone());
    store.active_id = Some(provider_id);
    if let Err(error) = save_provider_store(&store) {
        println!("\x1b[1;31m[ERROR] Provider configuration was not saved: {error}\x1b[0m");
        return;
    }
    if !ai_service.reload_provider_store().await {
        println!("\x1b[1;31m[ERROR] Provider was committed but runtime reload failed. Restart XiaoAI before continuing.\x1b[0m");
        return;
    }
    let probe = ai_service
        .probe_model_capabilities(&provider, &active_model)
        .await;
    println!(
        "  \x1b[38;5;244mCapability probe: vision={:?}, tools={:?}, structured={:?}\x1b[0m",
        probe.supports_image, probe.supports_tools, probe.supports_structured_output
    );

    println!(
        "\n\x1b[1;32m[SUCCESS] Provider '{}' added and activated!\x1b[0m",
        clean_alias
    );
    println!("  \x1b[1;36mActive Model:\x1b[0m \x1b[1;37m{active_model}\x1b[0m");
    println!("  \x1b[38;5;244mConfiguration saved to SQLite (runtime source of truth)\x1b[0m\n");
}

pub(crate) async fn run_cli_provider_remove(_ai_service: &AIChatService) {
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

    let selected =
        terminal_interactive_select("Select Provider to Remove:", &items, 0, false, None);

    if let Some(idx) = selected {
        let removed = store.providers.remove(idx);
        if store.active_id.as_deref() == Some(removed.id.as_str()) {
            store.active_id = store.providers.first().map(|provider| provider.id.clone());
        }
        if let Err(error) = save_provider_store(&store) {
            println!("\n\x1b[1;31m[ERROR] Provider was not removed: {error}\x1b[0m\n");
            return;
        }
        println!(
            "\n\x1b[1;32m[OK] Provider '{}' successfully removed.\x1b[0m\n",
            removed.name
        );
    } else {
        println!("\n\x1b[38;5;244mCancelled.\x1b[0m\n");
    }
}

pub(crate) async fn run_cli_provider_status(ai_service: &AIChatService) {
    load_environment();
    let store = load_provider_store();
    let active_p = if let Some(ref aid) = store.active_id {
        store.providers.iter().find(|p| &p.id == aid).cloned()
    } else {
        store.providers.first().cloned()
    };

    println!("\n\x1b[1;36m== Active Provider Status ==\x1b[0m");

    if let Some(p) = active_p {
        println!(
            "  \x1b[1;37mProvider:\x1b[0m     \x1b[1;32m{}\x1b[0m",
            p.name
        );
        println!("  \x1b[1;37mEndpoint:\x1b[0m     {}", p.endpoint);
        println!(
            "  \x1b[1;37mActive Model:\x1b[0m \x1b[1;36m{}\x1b[0m",
            p.active_model
        );
        println!(
            "  \x1b[1;37mTotal Models:\x1b[0m {} available",
            p.models.len()
        );

        let cap = ai_service
            .resolved_model_capability(&p.endpoint, &p.active_model)
            .await;
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

pub(crate) async fn run_cli_provider_menu(ai_service: &AIChatService, action: Option<&str>) {
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

        let sel =
            terminal_interactive_select("Configured AI Providers:", &menu_items, 0, false, None);

        let Some(idx) = sel else {
            break;
        };

        if idx < store.providers.len() {
            let target_prov = &store.providers[idx];
            let is_act = store.active_id.as_deref() == Some(target_prov.id.as_str());

            let cap = ai_service
                .resolved_model_capability(&target_prov.endpoint, &target_prov.active_model)
                .await;

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
                sub_actions.push(format!(
                    "\x1b[1;32mSet as Active Provider ({})\x1b[0m",
                    target_prov.name
                ));
            }
            sub_actions.push("Select / Switch Model for this Provider".to_string());
            sub_actions.push(format!(
                "\x1b[1;31mDelete Provider ({})\x1b[0m",
                target_prov.name
            ));
            sub_actions.push("\x1b[38;5;244mBack to Providers Menu\x1b[0m".to_string());

            let sub_sel = terminal_interactive_select(&title_summary, &sub_actions, 0, false, None);

            let Some(action_idx) = sub_sel else {
                continue;
            };

            let chosen_action = if is_act { action_idx + 1 } else { action_idx };

            match chosen_action {
                0 => {
                    let mut updated_store = load_provider_store();
                    updated_store.active_id = Some(target_prov.id.clone());
                    if let Err(error) = save_provider_store(&updated_store) {
                        println!(
                            "\n\x1b[1;31m[ERROR] Active provider was not changed: {error}\x1b[0m\n"
                        );
                        continue;
                    }
                    println!(
                        "\n\x1b[1;32m[OK] Provider '{}' is now active!\x1b[0m\n",
                        target_prov.name
                    );
                }
                1 => {
                    let (ok, res) = ai_service
                        .fetch_models_from_endpoint(&target_prov.endpoint, &target_prov.api_key)
                        .await;
                    let models = if ok {
                        res.unwrap_or_default()
                    } else {
                        target_prov.models.clone()
                    };
                    if models.is_empty() {
                        println!("\x1b[38;5;214m[WARN] No models found on endpoint.\x1b[0m");
                    } else {
                        let curr_idx = models
                            .iter()
                            .position(|m| m == &target_prov.active_model)
                            .unwrap_or(0);
                        if let Some(m_idx) = terminal_interactive_select(
                            &format!("Select Model for '{}':", target_prov.name),
                            &models,
                            curr_idx,
                            true,
                            None,
                        ) {
                            let chosen_model = models[m_idx].clone();
                            let mut updated_store = load_provider_store();
                            if let Some(p) = updated_store
                                .providers
                                .iter_mut()
                                .find(|p| p.id == target_prov.id)
                            {
                                p.active_model = chosen_model.clone();
                                p.models = models;
                            }
                            if let Err(error) = save_provider_store(&updated_store) {
                                println!("\n\x1b[1;31m[ERROR] Model selection was not saved: {error}\x1b[0m\n");
                                continue;
                            }
                            println!(
                                "\n\x1b[1;32m[OK] Model for '{}' set to: {}\x1b[0m\n",
                                target_prov.name, chosen_model
                            );
                        }
                    }
                }
                2 => {
                    let mut updated_store = load_provider_store();
                    if let Some(pos) = updated_store
                        .providers
                        .iter()
                        .position(|p| p.id == target_prov.id)
                    {
                        let removed = updated_store.providers.remove(pos);
                        if updated_store.active_id.as_deref() == Some(removed.id.as_str()) {
                            updated_store.active_id = updated_store
                                .providers
                                .first()
                                .map(|provider| provider.id.clone());
                        }
                        if let Err(error) = save_provider_store(&updated_store) {
                            println!(
                                "\n\x1b[1;31m[ERROR] Provider was not deleted: {error}\x1b[0m\n"
                            );
                            continue;
                        }
                        println!(
                            "\n\x1b[1;32m[OK] Provider '{}' deleted.\x1b[0m\n",
                            removed.name
                        );
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

pub(crate) async fn run_cli_model_probe(ai_service: &AIChatService) {
    let providers = ai_service.get_user_providers(0).await;
    if providers.is_empty() {
        println!("No AI providers configured.");
        return;
    }
    for provider in providers {
        let (ok, result) = ai_service
            .fetch_models_from_endpoint(&provider.endpoint, &provider.api_key)
            .await;
        match result {
            Ok(models) if ok => {
                println!(
                    "{}: discovered metadata for {} model(s)",
                    provider.name,
                    models.len()
                );
                let model = if !provider.active_model.is_empty() {
                    provider.active_model.clone()
                } else {
                    models.first().cloned().unwrap_or_default()
                };
                if !model.is_empty() {
                    let record = ai_service.probe_model_capabilities(&provider, &model).await;
                    println!(
                        "  active {}: text={:?} image={:?} tools={:?} structured={:?} files={:?}",
                        record.model,
                        record.supports_text,
                        record.supports_image,
                        record.supports_tools,
                        record.supports_structured_output,
                        record.supports_file_input,
                    );
                }
            }
            Ok(_) | Err(_) => println!("{}: capability discovery failed", provider.name),
        }
    }
    let registry = crate::ai::service::load_capability_registry();
    println!("Capability registry: {} model(s)", registry.models.len());
    for record in registry.models {
        println!(
            "- {} / {}: image={:?}, audio={:?}, video={:?}, tools={:?}, structured={:?}, files={:?}, context={:?} [{}]",
            record.provider_name,
            record.model,
            record.supports_image,
            record.supports_audio,
            record.supports_video,
            record.supports_tools,
            record.supports_structured_output,
            record.supports_file_input,
            record.context_window,
            record.source
        );
    }
}

pub(crate) async fn run_cli_model_picker(ai_service: &AIChatService, initial_filter: Option<&str>) {
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
            if let (true, Ok(fetched)) = ai_service
                .fetch_models_from_endpoint(&prov.endpoint, &prov.api_key)
                .await
            {
                if !fetched.is_empty() {
                    prov.models = fetched;
                }
            }
        }
    }
    if let Err(error) = save_provider_store(&store) {
        println!("\x1b[38;5;214m[WARN] Refreshed model catalog was not persisted: {error}\x1b[0m");
        store = load_provider_store();
    }

    let active_prov_id = store.active_id.clone().unwrap_or_default();
    let current_model = env::var("AI_MODEL").unwrap_or_default();

    let mut catalog: Vec<(String, String, String, bool)> = Vec::new();
    for prov in &store.providers {
        let is_prov_active = prov.id == active_prov_id;
        for m in &prov.models {
            let is_model_active =
                is_prov_active && (m == &prov.active_model || m == &current_model);
            catalog.push((
                prov.id.clone(),
                prov.name.clone(),
                m.clone(),
                is_model_active,
            ));
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
            let act_tag = if *is_act {
                " \x1b[1;32m[ACTIVE]\x1b[0m"
            } else {
                ""
            };
            if is_multi_provider {
                format!(
                    "{} \x1b[38;5;244m({})\x1b[0m{}",
                    model_name, prov_name, act_tag
                )
            } else {
                format!("{}{}", model_name, act_tag)
            }
        })
        .collect();

    let curr_idx = catalog
        .iter()
        .position(|(_, _, _, is_act)| *is_act)
        .unwrap_or(0);
    let title = format!(
        "Select Active Model (Total {} Models from {} Providers):",
        catalog.len(),
        store.providers.len()
    );

    let selected_idx = terminal_interactive_select(&title, &items, curr_idx, true, initial_filter);

    if let Some(idx) = selected_idx {
        if let Some((prov_id, prov_name, chosen_model, _)) = catalog.get(idx) {
            let mut updated_store = load_provider_store();
            updated_store.active_id = Some(prov_id.clone());
            if let Some(p) = updated_store
                .providers
                .iter_mut()
                .find(|p| &p.id == prov_id)
            {
                p.active_model = chosen_model.clone();
            }
            if let Err(error) = save_provider_store(&updated_store) {
                println!("\n\x1b[1;31m[ERROR] Active model was not changed: {error}\x1b[0m\n");
                return;
            }
            println!(
                "\n\x1b[1;32m[SUCCESS] Active model set to: {}\x1b[0m",
                chosen_model
            );
            println!(
                "  \x1b[1;36mProvider:\x1b[0m \x1b[1;37m{}\x1b[0m",
                prov_name
            );
            println!("  \x1b[38;5;244mActivated provider & saved to configuration.\x1b[0m\n");
        }
    } else {
        println!("\n\x1b[38;5;244mModel selection cancelled.\x1b[0m\n");
    }
}

pub(crate) async fn run_cli_status(ai_service: &AIChatService) {
    load_environment();
    println!("\n\x1b[1;36m== XiaoAI System Status ==\x1b[0m");

    let token = env::var("BOT_TOKEN")
        .ok()
        .or_else(|| crate::ai::service::load_app_setting("BOT_TOKEN"))
        .unwrap_or_default();
    let endpoint = env::var("AI_ENDPOINT")
        .ok()
        .or_else(|| crate::ai::service::load_app_setting("AI_ENDPOINT"))
        .unwrap_or_default();
    let api_key = env::var("AI_API_KEY")
        .ok()
        .or_else(|| crate::ai::service::load_app_setting("AI_API_KEY"))
        .unwrap_or_else(|| "none".to_string());
    let model = env::var("AI_MODEL")
        .ok()
        .or_else(|| crate::ai::service::load_app_setting("AI_MODEL"))
        .unwrap_or_default();

    println!("\x1b[1;37m1. TELEGRAM BOT API\x1b[0m");
    if token.is_empty() || token == "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
        println!("   \x1b[31m[FAIL]\x1b[0m BOT_TOKEN: Unconfigured (Run 'xiao setup')");
    } else {
        let bot = TelegramBotClient::new(&token);
        match bot.get_me().await {
            Ok(resp) if resp.ok => {
                if let Some(info) = resp.result {
                    let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
                    println!(
                        "   \x1b[1;32m[OK]\x1b[0m   BOT_TOKEN: Connected to @{} (ID: {})",
                        uname, info.id
                    );
                } else {
                    println!("   \x1b[1;31m[FAIL]\x1b[0m BOT_TOKEN: Telegram returned no bot information");
                }
            }
            Ok(resp) => {
                println!(
                    "   \x1b[1;31m[FAIL]\x1b[0m BOT_TOKEN: Invalid ({:?})",
                    resp.description
                );
            }
            Err(e) => {
                println!("   \x1b[1;31m[FAIL]\x1b[0m BOT_TOKEN: Connection error ({e})");
            }
        }
    }

    println!("\n\x1b[1;37m2. ACTIVE AI PROVIDER\x1b[0m");
    if endpoint.is_empty() {
        println!(
            "   \x1b[38;5;214m[WARN]\x1b[0m No AI provider configured (Run 'xiao provider add')"
        );
    } else {
        println!("   Endpoint: {}", endpoint);
        println!("   Model:    \x1b[1;36m{}\x1b[0m", model);

        let (ok, res) = ai_service
            .fetch_models_from_endpoint(&endpoint, &api_key)
            .await;
        if ok {
            let models = res.unwrap_or_default();
            println!(
                "   \x1b[1;32m[OK]\x1b[0m   Endpoint Status: ONLINE ({} models available)",
                models.len()
            );
            if models.iter().any(|m| m == &model) {
                println!(
                    "   \x1b[1;32m[OK]\x1b[0m   Model Verification: Confirmed available on server"
                );
            } else {
                println!("   \x1b[38;5;214m[WARN]\x1b[0m Model Verification: Model '{model}' not listed in /models endpoint");
            }
        } else {
            let err = res.err().unwrap_or_else(|| "Offline".to_string());
            println!("   \x1b[1;31m[FAIL]\x1b[0m Endpoint Status: OFFLINE / Error ({err})");
        }

        let cap = ai_service
            .resolved_model_capability(&endpoint, &model)
            .await;
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

pub(crate) async fn run_cli_telegram_check() {
    load_environment();
    println!("\n\x1b[1;36m== Telegram Bot Status Check ==\x1b[0m");

    let token = get_configured_token().unwrap_or_default();
    if token.is_empty() || token == "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
        println!("  \x1b[1;31m[FAIL]\x1b[0m BOT_TOKEN is not configured.");
        println!(
            "  \x1b[38;5;244mRun 'xiao telegram bind' or 'xiao setup' to bind a token.\x1b[0m\n"
        );
        return;
    }

    println!("  \x1b[38;5;244mConnecting to Telegram API...\x1b[0m");
    let bot = TelegramBotClient::new(&token);
    match bot.get_me().await {
        Ok(resp) if resp.ok => {
            if let Some(info) = resp.result {
                let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
                println!("  \x1b[1;32m[OK]\x1b[0m   Status: Connected & Verified");
                println!("  \x1b[1;37mBot Name:\x1b[0m {}", info.first_name);
                println!("  \x1b[1;37mUsername:\x1b[0m \x1b[1;36m@{}\x1b[0m", uname);
                println!("  \x1b[1;37mBot ID:\x1b[0m   {}", info.id);
                println!("  \x1b[1;37mBot Link:\x1b[0m https://t.me/{}", uname);
                if let Some(owner) = get_configured_owner_id() {
                    println!("  \x1b[1;37mOwner ID:\x1b[0m  {}", owner);
                } else {
                    println!("  \x1b[1;31m[FAIL] OWNER_USER_ID belum dikonfigurasi.\x1b[0m");
                }
            } else {
                println!("  \x1b[1;31m[FAIL]\x1b[0m Telegram returned no bot information");
            }
        }
        Ok(resp) => {
            println!(
                "  \x1b[1;31m[FAIL]\x1b[0m Token is invalid ({:?})",
                resp.description
            );
        }
        Err(e) => {
            println!("  \x1b[1;31m[FAIL]\x1b[0m Verification error: {e}");
        }
    }
    println!();
}

pub(crate) async fn run_cli_telegram_bind(manual_token: Option<&str>) {
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
        Ok(resp) if resp.ok => {
            let Some(info) = resp.result else {
                println!("  \x1b[1;31m[FAIL] Telegram returned ok=true without bot information.\x1b[0m\n");
                return;
            };
            let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
            println!(
                "  \x1b[1;32m[OK] Token verified! Connected to @{} ({})\x1b[0m",
                uname, info.first_name
            );

            if let Err(e) = save_token_to_env(&token) {
                println!("  \x1b[1;31m[ERROR] Failed to save token: {e}\x1b[0m\n");
            } else {
                println!("  \x1b[1;32m[SUCCESS] Telegram bot token bound successfully!\x1b[0m\n");
            }
        }
        Ok(resp) => {
            println!(
                "  \x1b[1;31m[FAIL] Invalid token: {:?}\x1b[0m\n",
                resp.description
            );
        }
        Err(e) => {
            println!("  \x1b[1;31m[FAIL] Verification error: {e}\x1b[0m\n");
        }
    }
}

pub(crate) async fn run_cli_telegram_change() {
    load_environment();
    println!("\n\x1b[1;36m== Change Telegram Bot Token ==\x1b[0m");

    let current_token = env::var("BOT_TOKEN").unwrap_or_default();
    if !current_token.is_empty() && current_token != "YOUR_TELEGRAM_BOT_TOKEN_HERE" {
        let temp_bot = TelegramBotClient::new(&current_token);
        if let Ok(resp) = temp_bot.get_me().await {
            if let Some(info) = resp.result {
                let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
                println!(
                    "  \x1b[38;5;244mCurrent Bot:\x1b[0m \x1b[1;36m@{}\x1b[0m (ID: {})",
                    uname, info.id
                );
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
        Ok(resp) if resp.ok => {
            let Some(info) = resp.result else {
                println!("  \x1b[1;31m[FAIL] Telegram returned ok=true without bot information.\x1b[0m\n");
                return;
            };
            let uname = info.username.unwrap_or_else(|| "Unknown".to_string());
            println!(
                "  \x1b[1;32m[OK] Token valid! Connected to @{} ({})\x1b[0m",
                uname, info.first_name
            );

            if let Err(e) = save_token_to_env(&new_token) {
                println!("  \x1b[1;31m[ERROR] Failed to save token: {e}\x1b[0m\n");
            } else {
                println!("  \x1b[1;32m[SUCCESS] Telegram bot token updated successfully!\x1b[0m\n");
            }
        }
        Ok(resp) => {
            println!(
                "  \x1b[1;31m[FAIL] Invalid token: {:?}\x1b[0m\n",
                resp.description
            );
        }
        Err(e) => {
            println!("  \x1b[1;31m[FAIL] Verification error: {e}\x1b[0m\n");
        }
    }
}

pub(crate) async fn run_cli_telegram_pick(ai_service: &AIChatService) {
    let mut store = load_provider_store();
    store.telegram_models.truncate(10);
    if store.providers.is_empty() {
        println!("\x1b[38;5;214m[WARN] No AI provider configured yet.\x1b[0m");
        return;
    }
    for provider in &mut store.providers {
        if provider.models.len() <= 1 && !provider.endpoint.is_empty() {
            if let (true, Ok(models)) = ai_service
                .fetch_models_from_endpoint(&provider.endpoint, &provider.api_key)
                .await
            {
                if !models.is_empty() {
                    provider.models = models;
                }
            }
        }
    }
    let mut catalog: Vec<(String, String, String)> = Vec::new();
    for provider in &store.providers {
        for model in &provider.models {
            catalog.push((
                provider.id.clone(),
                model.clone(),
                format!("{} ({})", model, provider.name),
            ));
        }
    }
    if catalog.is_empty() {
        println!("\x1b[38;5;214m[WARN] No models found.\x1b[0m");
        return;
    }
    let items: Vec<String> = catalog
        .iter()
        .map(|(_, _, display)| display.clone())
        .collect();
    let selected_flags: Vec<bool> = catalog
        .iter()
        .map(|(provider_id, model, _)| {
            let key = format!("{}::{}", provider_id, model);
            store
                .telegram_models
                .iter()
                .any(|selected| selected == &key || selected == model)
        })
        .collect();
    let selected = terminal_interactive_multi_select(
        "Pilih model yang ditampilkan di Telegram",
        &items,
        &selected_flags,
        10,
    );
    let Some(indices) = selected else {
        return;
    };
    store.telegram_models = indices
        .into_iter()
        .filter_map(|idx| {
            let (provider_id, model, _) = catalog.get(idx)?;
            Some(format!("{}::{}", provider_id, model))
        })
        .collect();
    if let Err(error) = save_provider_store(&store) {
        println!("\n\x1b[1;31m[ERROR] Telegram model whitelist was not saved: {error}\x1b[0m\n");
        return;
    }
    if store.telegram_models.is_empty() {
        println!(
            "\n\x1b[1;32m[OK] Telegram model whitelist cleared; all models are visible.\x1b[0m"
        );
    } else {
        println!(
            "\n\x1b[1;32m[OK] {} model(s) enabled for Telegram (max 10).\x1b[0m",
            store.telegram_models.len()
        );
    }
}

pub(crate) async fn run_cli_telegram_owner(owner_arg: Option<&str>) {
    let owner = if let Some(value) = owner_arg {
        value.trim().parse::<i64>().ok()
    } else {
        print!("\x1b[1;37mTelegram Owner User ID:\x1b[0m ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            None
        } else {
            input.trim().parse::<i64>().ok()
        }
    };

    match owner.filter(|value| *value > 0) {
        Some(owner_id) => match save_env_kv("OWNER_USER_ID", &owner_id.to_string()) {
            Ok(()) => {
                println!("\n\x1b[1;32m[OK] Telegram owner diset ke user ID {owner_id}.\x1b[0m\n")
            }
            Err(error) => println!("\n\x1b[1;31m[ERROR] Gagal menyimpan owner: {error}\x1b[0m\n"),
        },
        None => println!("\n\x1b[1;31m[ERROR] Owner User ID harus berupa angka positif.\x1b[0m\n"),
    }
}

pub(crate) async fn run_cli_telegram_menu(action: Option<&str>, arg: Option<&str>) {
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
        Some("owner") => {
            run_cli_telegram_owner(arg).await;
        }
        _ => {
            run_cli_telegram_check().await;
        }
    }
}

pub(crate) fn print_cli_help() {
    println!("\n\x1b[1;36mUsage:\x1b[0m xiao [command] [args]\n");
    println!("\x1b[1;37mCommands:\x1b[0m");
    println!("  \x1b[36mstart\x1b[0m                               Run Telegram bot");
    println!("  \x1b[36msetup\x1b[0m                               Quickstart setup wizard");
    println!("  \x1b[36mprovider [add] [del] [status]\x1b[0m       Manage AI providers");
    println!("  \x1b[36mtelegram [check|bind|change|owner]\x1b[0m  Manage Telegram bot and owner");
    println!(
        "  \x1b[36mmodel [name] [pick]\x1b[0m                 Select, search, or whitelist models"
    );
    println!("  \x1b[36mstatus\x1b[0m                              System health check");
    println!("  \x1b[36mhelp\x1b[0m                                Show this help\n");
}

// ==========================================
// Keyboard Interfaces
// ==========================================
