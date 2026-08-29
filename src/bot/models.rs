#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ==========================================
// Inline & Reply Keyboards
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_app: Option<Value>,
}

impl InlineKeyboardButton {
    pub fn callback(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: Some(callback_data.into()),
            url: None,
            web_app: None,
        }
    }

    pub fn url_btn(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: None,
            url: Some(url.into()),
            web_app: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

impl InlineKeyboardMarkup {
    pub fn new(rows: Vec<Vec<InlineKeyboardButton>>) -> Self {
        Self {
            inline_keyboard: rows,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_contact: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_location: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_app: Option<Value>,
}

impl KeyboardButton {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            request_contact: None,
            request_location: None,
            web_app: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyKeyboardMarkup {
    pub keyboard: Vec<Vec<KeyboardButton>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_persistent: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resize_keyboard: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_time_keyboard: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_field_placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selective: Option<bool>,
}

impl ReplyKeyboardMarkup {
    pub fn from_strings(
        rows: Vec<Vec<&str>>,
        is_persistent: bool,
        resize_keyboard: bool,
        placeholder: Option<&str>,
    ) -> Self {
        let keyboard = rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|btn| KeyboardButton::new(btn))
                    .collect()
            })
            .collect();

        Self {
            keyboard,
            is_persistent: Some(is_persistent),
            resize_keyboard: Some(resize_keyboard),
            one_time_keyboard: Some(false),
            input_field_placeholder: placeholder.map(|s| s.to_string()),
            selective: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyKeyboardRemove {
    pub remove_keyboard: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selective: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotCommand {
    pub command: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_ephemeral: Option<bool>,
}

impl BotCommand {
    pub fn new(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            is_ephemeral: None,
        }
    }

    pub fn ephemeral(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            is_ephemeral: Some(true),
        }
    }
}

// ==========================================
// Telegram Bot API 10.2: Rich Message Blocks
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichBlockTableCell {
    pub text: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub align: Option<String>,
    pub valign: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colspan: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rowspan: Option<usize>,
}

impl RichBlockTableCell {
    pub fn new(text: Value, is_header: bool, align: Option<&str>) -> Self {
        Self {
            text,
            is_header: if is_header { Some(true) } else { None },
            align: align.map(|a| a.to_string()),
            valign: "middle".to_string(),
            colspan: None,
            rowspan: None,
        }
    }

    pub fn text_only(text: &str, is_header: bool, align: Option<&str>) -> Self {
        Self::new(Value::String(text.to_string()), is_header, align)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RichBlock {
    #[serde(rename = "paragraph")]
    Paragraph { text: Value },

    #[serde(rename = "heading")]
    SectionHeading {
        text: Value,
        #[serde(rename = "size")]
        level: usize,
    },

    #[serde(rename = "pre")]
    Preformatted {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },

    #[serde(rename = "list")]
    List {
        items: Vec<Value>, // each item is {"blocks": [{"type": "paragraph", "text": ...}]}
    },

    #[serde(rename = "blockquote")]
    BlockQuotation {
        blocks: Vec<Value>, // [{"type": "paragraph", "text": ...}]
    },

    #[serde(rename = "divider")]
    Divider {},

    #[serde(rename = "mathematical_expression")]
    MathematicalExpression { expression: String },

    #[serde(rename = "table")]
    Table {
        cells: Vec<Vec<RichBlockTableCell>>,
        #[serde(skip)]
        has_header: bool,
        is_bordered: bool,
        is_striped: bool,
        is_compact: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },

    #[serde(rename = "details")]
    Details {
        title: String,
        content: String,
        is_open: bool,
    },

    #[serde(rename = "anchor")]
    Anchor { name: String },

    #[serde(rename = "thinking")]
    Thinking {
        text: String,
        collapsed: bool,
        expandable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputRichMessage {
    pub blocks: Vec<RichBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<Vec<Value>>,
}

impl InputRichMessage {
    pub fn new(blocks: Vec<RichBlock>) -> Self {
        Self {
            blocks,
            media: None,
        }
    }
}

// ==========================================
// Telegram Updates & Message Payloads
// ==========================================

#[derive(Debug, Clone, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
    pub error_code: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub from: Option<User>,
    pub chat: Chat,
    pub date: i64,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub photo: Option<Vec<PhotoSize>>,
    pub document: Option<Document>,
    pub voice: Option<Voice>,
    pub audio: Option<Audio>,
    pub video: Option<Video>,
    pub video_note: Option<VideoNote>,
    pub reply_to_message: Option<Box<Message>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    pub title: Option<String>,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: i32,
    pub height: i32,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Document {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Voice {
    pub file_id: String,
    pub duration: i32,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Audio {
    pub file_id: String,
    pub duration: i32,
    pub performer: Option<String>,
    pub title: Option<String>,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Video {
    pub file_id: String,
    pub width: i32,
    pub height: i32,
    pub duration: i32,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VideoNote {
    pub file_id: String,
    pub length: i32,
    pub duration: i32,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub inline_message_id: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub file_id: String,
    pub file_size: Option<i64>,
    pub file_path: Option<String>,
}
