#![allow(dead_code)]

use std::time::Duration;
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, info, warn};

use super::models::{
    ApiResponse, BotCommand, FileInfo, InputRichMessage,
    RichBlock, RichBlockTableCell, Update, User,
};

#[derive(Clone)]
pub struct TelegramBotClient {
    token: String,
    base_url: String,
    client: Client,
}

impl TelegramBotClient {
    pub fn new(token: impl Into<String>) -> Self {
        let token_str = token.into().trim().to_string();
        let base_url = format!("https://api.telegram.org/bot{}", token_str);
        let client = Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            token: token_str,
            base_url,
            client,
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    async fn post_json(&self, method: &str, payload: Value) -> Result<Value, String> {
        let url = format!("{}/{}", self.base_url, method);
        match self.client.post(&url).json(&payload).send().await {
            Ok(resp) => {
                let status = resp.status();
                match resp.json::<Value>().await {
                    Ok(json_res) => {
                        if !json_res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                            warn!("Telegram API error [{method}] status {status}: {json_res}");
                        }
                        Ok(json_res)
                    }
                    Err(e) => {
                        let err_msg = format!("Failed to parse response JSON for {method}: {e}");
                        error!("{err_msg}");
                        Err(err_msg)
                    }
                }
            }
            Err(e) => {
                let err_msg = format!("HTTP error for {method}: {e}");
                error!("{err_msg}");
                Err(err_msg)
            }
        }
    }

    // ==========================================
    // Basic Telegram API Methods
    // ==========================================

    pub async fn get_me(&self) -> Result<ApiResponse<User>, String> {
        let val = self.post_json("getMe", json!({})).await?;
        serde_json::from_value(val).map_err(|e| e.to_string())
    }

    pub async fn get_file(&self, file_id: &str) -> Result<ApiResponse<FileInfo>, String> {
        let val = self.post_json("getFile", json!({ "file_id": file_id })).await?;
        serde_json::from_value(val).map_err(|e| e.to_string())
    }

    pub async fn get_file_bytes(&self, file_id: &str) -> Option<(Vec<u8>, String)> {
        let file_res = self.get_file(file_id).await.ok()?;
        if !file_res.ok {
            return None;
        }
        let info = file_res.result?;
        let file_path = info.file_path?;
        let file_url = format!("https://api.telegram.org/file/bot{}/{}", self.token, file_path);

        let dl_client = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()
            .unwrap_or_else(|_| self.client.clone());

        match dl_client.get(&file_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                use futures_util::StreamExt;
                let mut stream = resp.bytes_stream();
                let mut bytes_buf = Vec::new();
                while let Some(chunk_res) = stream.next().await {
                    match chunk_res {
                        Ok(chunk) => bytes_buf.extend_from_slice(&chunk),
                        Err(e) => {
                            error!("Streaming error while downloading from {file_url}: {e}");
                            return None;
                        }
                    }
                }
                Some((bytes_buf, file_path))
            }
            Ok(resp) => {
                error!("Download failed with status {}: {}", resp.status(), file_url);
                None
            }
            Err(e) => {
                error!("HTTP download error for {file_url}: {e}");
                None
            }
        }
    }

    pub async fn get_updates(
        &self,
        offset: Option<i64>,
        limit: i32,
        timeout: i32,
        allowed_updates: Option<Vec<String>>,
    ) -> Result<ApiResponse<Vec<Update>>, String> {
        let mut payload = json!({
            "limit": limit,
            "timeout": timeout,
        });
        if let Some(off) = offset {
            payload["offset"] = json!(off);
        }
        if let Some(allowed) = allowed_updates {
            payload["allowed_updates"] = json!(allowed);
        }

        let val = self.post_json("getUpdates", payload).await?;
        serde_json::from_value(val).map_err(|e| e.to_string())
    }

    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
        receiver_user_id: Option<i64>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        if text.len() > 4000 {
            let chunks = self.split_text_chunks(text, 3800);
            let mut last_res = json!({ "ok": true });
            let total = chunks.len();
            for (idx, chunk) in chunks.into_iter().enumerate() {
                let is_last = idx == total - 1;
                let is_first = idx == 0;

                let mut payload = json!({
                    "chat_id": chat_id,
                    "text": chunk,
                });
                if let Some(pm) = parse_mode {
                    payload["parse_mode"] = json!(pm);
                }
                if is_last {
                    if let Some(ref rm) = reply_markup {
                        payload["reply_markup"] = rm.clone();
                    }
                }
                if let Some(recv) = receiver_user_id {
                    payload["receiver_user_id"] = json!(recv);
                }
                if is_first {
                    if let Some(rep) = reply_to_message_id {
                        payload["reply_to_message_id"] = json!(rep);
                    }
                }
                last_res = self.post_json("sendMessage", payload).await?;
            }
            return Ok(last_res);
        }

        let mut payload = json!({
            "chat_id": chat_id,
            "text": text,
        });
        if let Some(pm) = parse_mode {
            payload["parse_mode"] = json!(pm);
        }
        if let Some(rm) = reply_markup {
            payload["reply_markup"] = rm;
        }
        if let Some(recv) = receiver_user_id {
            payload["receiver_user_id"] = json!(recv);
        }
        if let Some(rep) = reply_to_message_id {
            payload["reply_to_message_id"] = json!(rep);
        }

        self.post_json("sendMessage", payload).await
    }

    pub async fn send_photo_bytes(
        &self,
        chat_id: i64,
        photo_bytes: Vec<u8>,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        let url = format!("{}/sendPhoto", self.base_url);
        let part = Part::bytes(photo_bytes)
            .file_name("image.png")
            .mime_str("image/png")
            .map_err(|e| e.to_string())?;

        let mut form = Form::new()
            .text("chat_id", chat_id.to_string())
            .part("photo", part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }
        if let Some(pm) = parse_mode {
            form = form.text("parse_mode", pm.to_string());
        }
        if let Some(rep) = reply_to_message_id {
            form = form.text("reply_to_message_id", rep.to_string());
        }
        if let Some(rm) = reply_markup {
            form = form.text("reply_markup", rm.to_string());
        }

        match self.client.post(&url).multipart(form).send().await {
            Ok(resp) => resp.json::<Value>().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("sendPhoto multipart error: {e}")),
        }
    }

    pub async fn edit_message_text(
        &self,
        chat_id: Option<i64>,
        message_id: Option<i64>,
        text: &str,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
    ) -> Result<Value, String> {
        let mut payload = json!({ "text": text });
        if let Some(cid) = chat_id {
            payload["chat_id"] = json!(cid);
        }
        if let Some(mid) = message_id {
            payload["message_id"] = json!(mid);
        }
        if let Some(pm) = parse_mode {
            payload["parse_mode"] = json!(pm);
        }
        if let Some(rm) = reply_markup {
            payload["reply_markup"] = rm;
        }

        self.post_json("editMessageText", payload).await
    }

    pub async fn edit_rich_message(
        &self,
        chat_id: i64,
        message_id: i64,
        rich_message: &InputRichMessage,
        reply_markup: Option<Value>,
    ) -> Result<Value, String> {
        let rich_json = serde_json::to_value(rich_message).unwrap_or(json!({}));
        let mut payload = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "rich_message": rich_json,
        });
        if let Some(ref rm) = reply_markup {
            payload["reply_markup"] = rm.clone();
        }

        let res = self.post_json("editMessageText", payload).await?;
        if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Ok(res);
        }
        if res
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase().contains("message is not modified"))
            .unwrap_or(false)
        {
            return Ok(res);
        }

        // Fallback to editMessageText with HTML rendering
        let html_content = self.render_blocks_to_html(&rich_message.blocks);
        self.edit_message_text(
            Some(chat_id),
            Some(message_id),
            &html_content,
            Some("HTML"),
            reply_markup,
        )
        .await
    }

    pub async fn delete_message(&self, chat_id: i64, message_id: i64) -> Result<Value, String> {
        let payload = json!({
            "chat_id": chat_id,
            "message_id": message_id,
        });
        self.post_json("deleteMessage", payload).await
    }

    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<Value, String> {
        let mut payload = json!({
            "callback_query_id": callback_query_id,
            "show_alert": show_alert,
        });
        if let Some(t) = text {
            payload["text"] = json!(t);
        }
        self.post_json("answerCallbackQuery", payload).await
    }

    pub async fn send_chat_action(&self, chat_id: i64, action: &str) -> Result<Value, String> {
        let payload = json!({
            "chat_id": chat_id,
            "action": action,
        });
        self.post_json("sendChatAction", payload).await
    }

    // ==========================================
    // Telegram Bot API 10.2: Rich Message & Draft Methods
    // ==========================================

    pub async fn send_rich_message_draft(
        &self,
        chat_id: i64,
        draft_id: i64,
        rich_message: &InputRichMessage,
        can_stop: bool,
        keep_on_stop: bool,
    ) -> Result<Value, String> {
        let rich_json = serde_json::to_value(rich_message).unwrap_or(json!({}));
        let payload = json!({
            "chat_id": chat_id,
            "draft_id": draft_id,
            "rich_message": rich_json,
            "can_stop": can_stop,
            "keep_on_stop": keep_on_stop,
        });

        let res = self.post_json("sendRichMessageDraft", payload).await?;
        let is_ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);

        if !is_ok {
            if res.get("error_code").and_then(|v| v.as_i64()) == Some(429) {
                return Ok(res);
            }
            // Fallback to sendMessageDraft
            let mut fallback_text = "Thinking...".to_string();
            if let Some(first_b) = rich_message.blocks.first() {
                if let RichBlock::Thinking { ref text, .. } = first_b {
                    fallback_text = text.clone();
                }
            }
            return self
                .send_message_draft(chat_id, draft_id, &fallback_text, Some("HTML"), can_stop, keep_on_stop)
                .await;
        }

        Ok(res)
    }

    pub async fn send_message_draft(
        &self,
        chat_id: i64,
        draft_id: i64,
        text: &str,
        parse_mode: Option<&str>,
        can_stop: bool,
        keep_on_stop: bool,
    ) -> Result<Value, String> {
        let mut payload = json!({
            "chat_id": chat_id,
            "draft_id": draft_id,
            "text": text,
            "can_stop": can_stop,
            "keep_on_stop": keep_on_stop,
        });
        if let Some(pm) = parse_mode {
            payload["parse_mode"] = json!(pm);
        }
        self.post_json("sendMessageDraft", payload).await
    }

    pub async fn send_rich_message(
        &self,
        chat_id: i64,
        rich_message: &InputRichMessage,
        reply_markup: Option<Value>,
        receiver_user_id: Option<i64>,
    ) -> Result<Value, String> {
        let rich_json = serde_json::to_value(rich_message).unwrap_or(json!({}));
        let mut payload = json!({
            "chat_id": chat_id,
            "rich_message": rich_json,
        });
        if let Some(ref rm) = reply_markup {
            payload["reply_markup"] = rm.clone();
        }
        if let Some(recv) = receiver_user_id {
            payload["receiver_user_id"] = json!(recv);
        }

        let res = self.post_json("sendRichMessage", payload).await?;
        if res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            return Ok(res);
        }

        // Fallback rendering to HTML chunks
        info!("Falling back to HTML rendering for rich message.");
        let chunks = self.render_blocks_to_html_chunks(&rich_message.blocks, 3800);
        let mut last_res = json!({ "ok": true });
        let total = chunks.len();

        for (idx, chunk) in chunks.into_iter().enumerate() {
            let is_last = idx == total - 1;
            last_res = self
                .send_message(
                    chat_id,
                    &chunk,
                    Some("HTML"),
                    if is_last { reply_markup.clone() } else { None },
                    receiver_user_id,
                    None,
                )
                .await?;
        }

        Ok(last_res)
    }

    pub async fn set_my_commands(&self, commands: &[BotCommand]) -> Result<Value, String> {
        let cmds_json = serde_json::to_value(commands).unwrap_or(json!([]));
        let payload = json!({ "commands": cmds_json });
        self.post_json("setMyCommands", payload).await
    }

    // ==========================================
    // HTML Rendering Helpers & Chunking
    // ==========================================

    pub fn split_text_chunks(&self, text: &str, max_chunk_len: usize) -> Vec<String> {
        if text.len() <= max_chunk_len {
            return if text.is_empty() { vec![] } else { vec![text.to_string()] };
        }

        let mut chunks = Vec::new();
        let mut current_chunk = Vec::new();
        let mut current_len = 0;

        let sections: Vec<&str> = text.split("\n\n").collect();
        for sec in sections {
            let sec_len = sec.len() + 2;
            if current_len + sec_len > max_chunk_len {
                if !current_chunk.is_empty() {
                    chunks.push(current_chunk.join("\n\n"));
                    current_chunk.clear();
                    current_len = 0;
                }
                if sec.len() > max_chunk_len {
                    let lines: Vec<&str> = sec.split('\n').collect();
                    for line in lines {
                        if current_len + line.len() + 1 > max_chunk_len {
                            if !current_chunk.is_empty() {
                                chunks.push(current_chunk.join("\n"));
                                current_chunk.clear();
                                current_len = 0;
                            }
                        }
                        current_chunk.push(line.to_string());
                        current_len += line.len() + 1;
                    }
                } else {
                    current_chunk.push(sec.to_string());
                    current_len += sec_len;
                }
            } else {
                current_chunk.push(sec.to_string());
                current_len += sec_len;
            }
        }

        if !current_chunk.is_empty() {
            chunks.push(current_chunk.join("\n\n"));
        }

        chunks
    }

    pub fn render_blocks_to_html_chunks(&self, blocks: &[RichBlock], max_chars: usize) -> Vec<String> {
        let mut chunks = Vec::new();
        let mut current_text = String::new();

        for block in blocks {
            let b_html = self.render_single_block_html(block);
            if b_html.is_empty() {
                continue;
            }

            if b_html.len() > max_chars {
                if !current_text.is_empty() {
                    chunks.push(current_text.trim().to_string());
                    current_text.clear();
                }
                let sub_chunks = self.split_text_chunks(&b_html, max_chars);
                for sub in sub_chunks {
                    chunks.push(sub);
                }
                continue;
            }

            if current_text.len() + b_html.len() + 2 > max_chars && !current_text.is_empty() {
                chunks.push(current_text.trim().to_string());
                current_text.clear();
            }

            if !current_text.is_empty() {
                current_text.push('\n');
            }
            current_text.push_str(&b_html);
        }

        if !current_text.trim().is_empty() {
            chunks.push(current_text.trim().to_string());
        }

        if chunks.is_empty() {
            vec!["".to_string()]
        } else {
            chunks
        }
    }

    pub fn render_blocks_to_html(&self, blocks: &[RichBlock]) -> String {
        let mut lines = Vec::new();
        for block in blocks {
            let h = self.render_single_block_html(block);
            if !h.is_empty() {
                lines.push(h);
            }
        }
        lines.join("\n").trim().to_string()
    }

    fn render_single_block_html(&self, block: &RichBlock) -> String {
        match block {
            RichBlock::Paragraph { text } => {
                let inner = self.rich_value_to_html(text);
                format!("{inner}\n")
            }
            RichBlock::SectionHeading { text, .. } => {
                let inner = self.rich_value_to_html(text);
                format!("\n<b>{inner}</b>\n")
            }
            RichBlock::Preformatted { text, language } => {
                let lang_attr = language
                    .as_ref()
                    .map(|l| format!(" class=\"language-{l}\""))
                    .unwrap_or_default();
                let esc = html_escape::encode_text(text);
                format!("<pre{lang_attr}>{esc}</pre>\n")
            }
            RichBlock::List { items } => {
                let mut list_lines = Vec::new();
                for (_idx, item) in items.iter().enumerate() {
                    let item_str = if let Some(obj) = item.as_object() {
                        if let Some(b) = obj.get("blocks") {
                            self.rich_value_to_html(b)
                        } else if let Some(t) = obj.get("text") {
                            self.rich_value_to_html(t)
                        } else {
                            self.rich_value_to_html(item)
                        }
                    } else {
                        self.rich_value_to_html(item)
                    };
                    list_lines.push(format!("• {item_str}"));
                }
                list_lines.join("\n")
            }
            RichBlock::BlockQuotation { blocks } => {
                let mut q_text = String::new();
                for b in blocks {
                    q_text.push_str(&self.rich_value_to_html(b));
                }
                format!("<blockquote>{q_text}</blockquote>\n")
            }
            RichBlock::Divider {} => "────────────────────────\n".to_string(),
            RichBlock::MathematicalExpression { expression } => {
                let esc = html_escape::encode_text(expression);
                format!("<code>{esc}</code>\n")
            }
            RichBlock::Table { cells, has_header, .. } => {
                let ascii_tbl = self.render_table_to_ascii(cells, *has_header);
                if !ascii_tbl.is_empty() {
                    format!("\n{ascii_tbl}\n")
                } else {
                    String::new()
                }
            }
            RichBlock::Details { title, content, .. } => {
                let t_esc = html_escape::encode_text(title);
                let c_esc = html_escape::encode_text(content);
                format!("<blockquote expandable><b>{t_esc}</b>\n{c_esc}</blockquote>\n")
            }
            RichBlock::Thinking { text, .. } => {
                let t_esc = html_escape::encode_text(text);
                format!("<blockquote expandable>💭 <b>Thinking:</b>\n{t_esc}</blockquote>\n")
            }
            RichBlock::Anchor { .. } => String::new(),
        }
    }

    fn rich_value_to_html(&self, v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr.iter().map(|item| self.rich_value_to_html(item)).collect(),
            Value::Object(obj) => {
                let t = obj.get("type").and_then(|s| s.as_str()).unwrap_or("");
                let inner = obj
                    .get("text")
                    .map(|sub| self.rich_value_to_html(sub))
                    .unwrap_or_default();
                match t {
                    "bold" => format!("<b>{inner}</b>"),
                    "italic" => format!("<i>{inner}</i>"),
                    "code" => format!("<code>{inner}</code>"),
                    "url" => {
                        let url = obj.get("url").and_then(|s| s.as_str()).unwrap_or("#");
                        format!("<a href=\"{url}\">{inner}</a>")
                    }
                    "strike" => format!("<s>{inner}</s>"),
                    "underline" => format!("<u>{inner}</u>"),
                    "paragraph" => inner,
                    _ => inner,
                }
            }
            _ => v.to_string(),
        }
    }

    fn rich_value_to_plain(&self, v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr.iter().map(|item| self.rich_value_to_plain(item)).collect(),
            Value::Object(obj) => {
                if let Some(text) = obj.get("text") {
                    self.rich_value_to_plain(text)
                } else if let Some(expr) = obj.get("expression") {
                    self.rich_value_to_plain(expr)
                } else {
                    String::new()
                }
            }
            _ => v.to_string(),
        }
    }

    fn char_display_width(c: char) -> usize {
        match c {
            '\u{1100}'..='\u{115F}'
            | '\u{2329}'
            | '\u{232A}'
            | '\u{2E80}'..='\u{303E}'
            | '\u{3040}'..='\u{A4CF}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE10}'..='\u{FE19}'
            | '\u{FE30}'..='\u{FE6F}'
            | '\u{FF00}'..='\u{FF60}'
            | '\u{FFE0}'..='\u{FFE6}'
            | '\u{1F300}'..='\u{1FAFF}'
            | '\u{2600}'..='\u{27BF}' => 2,
            _ => 1,
        }
    }

    fn str_display_width(s: &str) -> usize {
        s.chars().map(Self::char_display_width).sum()
    }

    fn truncate_display_width(s: &str, max_width: usize) -> String {
        if Self::str_display_width(s) <= max_width {
            return s.to_string();
        }
        if max_width <= 1 {
            return "…".to_string();
        }
        let mut out = String::new();
        let mut width = 0;
        for c in s.chars() {
            let c_width = Self::char_display_width(c);
            if width + c_width > max_width - 1 {
                break;
            }
            out.push(c);
            width += c_width;
        }
        out.push('…');
        out
    }

    fn render_table_to_ascii(&self, rows: &[Vec<RichBlockTableCell>], has_header: bool) -> String {
        if rows.is_empty() {
            return String::new();
        }

        let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return String::new();
        }

        let tag_clean_re = regex::Regex::new(r"</?[^>]+>").unwrap();
        let mut norm_rows: Vec<Vec<(String, String)>> = Vec::new();
        for r in rows {
            let mut row_cells: Vec<(String, String)> = r
                .iter()
                .map(|c| {
                    let plain = self.rich_value_to_plain(&c.text);
                    let cleaned = tag_clean_re.replace_all(&plain, "").to_string();
                    let decoded = html_escape::decode_html_entities(&cleaned)
                        .replace(['\n', '\r', '\t'], " ")
                        .to_string();
                    let align = c.align.clone().unwrap_or_else(|| "left".to_string());
                    (decoded, align)
                })
                .collect();
            while row_cells.len() < num_cols {
                row_cells.push((String::new(), "left".to_string()));
            }
            norm_rows.push(row_cells);
        }

        let mut col_widths = vec![2usize; num_cols];
        for r in &norm_rows {
            for (i, (c, _)) in r.iter().enumerate() {
                col_widths[i] = col_widths[i].max(Self::str_display_width(c));
            }
        }

        const MAX_TABLE_WIDTH: usize = 64;
        let border_overhead = num_cols + 1 + (num_cols * 2);
        let available = MAX_TABLE_WIDTH.saturating_sub(border_overhead);
        let current: usize = col_widths.iter().sum();
        if current > available && available >= num_cols {
            let min_width = 3usize;
            let mut excess = current.saturating_sub(available);
            while excess > 0 {
                let mut changed = false;
                for width in &mut col_widths {
                    if excess == 0 {
                        break;
                    }
                    if *width > min_width {
                        *width -= 1;
                        excess -= 1;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
        }

        let separator = format!(
            "|-{}-|",
            col_widths
                .iter()
                .map(|w| "-".repeat(*w + 2))
                .collect::<Vec<_>>()
                .join("-|-")
        );

        let mut table_lines = Vec::new();
        for (idx, r) in norm_rows.iter().enumerate() {
            let mut line_cells = Vec::new();
            for i in 0..num_cols {
                let (cell_txt, align) = &r[i];
                let display_txt = Self::truncate_display_width(cell_txt, col_widths[i]);
                let width = Self::str_display_width(&display_txt);
                let total_pad = col_widths[i].saturating_sub(width);
                let esc_txt = html_escape::encode_text(&display_txt);

                let padded = match align.as_str() {
                    "center" => {
                        let l_pad = total_pad / 2;
                        let r_pad = total_pad - l_pad;
                        format!(" {}{}{} ", " ".repeat(l_pad), esc_txt, " ".repeat(r_pad))
                    }
                    "right" => {
                        format!(" {}{} ", " ".repeat(total_pad), esc_txt)
                    }
                    _ => {
                        format!(" {}{} ", esc_txt, " ".repeat(total_pad))
                    }
                };
                line_cells.push(padded);
            }
            table_lines.push(format!("|{}|", line_cells.join("|")));
            if idx == 0 && has_header && norm_rows.len() > 1 {
                table_lines.push(separator.clone());
            }
        }
        if !has_header {
            table_lines.insert(1.min(table_lines.len()), separator);
        }

        format!("<pre><code>{}</code></pre>", table_lines.join("\n"))
    }
}
