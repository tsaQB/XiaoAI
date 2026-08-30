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
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_app: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Value>,
}

impl InlineKeyboardButton {
    pub fn callback(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            callback_data: Some(callback_data.into()),
            url: None,
            web_app: None,
            disabled: None,
        }
    }

    pub fn callback_styled(
        text: impl Into<String>,
        callback_data: impl Into<String>,
        style: impl Into<String>,
    ) -> Self {
        let mut button = Self::callback(text, callback_data);
        button.style = Some(style.into());
        button
    }

    pub fn url_btn(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            callback_data: None,
            url: Some(url.into()),
            web_app: None,
            disabled: None,
        }
    }

    pub fn disabled(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: None,
            callback_data: None,
            url: None,
            web_app: None,
            disabled: Some(serde_json::json!({})),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_reply: Option<bool>,
}

impl InlineKeyboardMarkup {
    pub fn new(rows: Vec<Vec<InlineKeyboardButton>>) -> Self {
        Self {
            inline_keyboard: rows,
            force_reply: None,
        }
    }

    pub fn with_force_reply(mut self, force_reply: bool) -> Self {
        self.force_reply = Some(force_reply);
        self
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_reply: Option<bool>,
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
            .map(|row| row.into_iter().map(KeyboardButton::new).collect())
            .collect();

        Self {
            keyboard,
            is_persistent: Some(is_persistent),
            resize_keyboard: Some(resize_keyboard),
            one_time_keyboard: Some(false),
            input_field_placeholder: placeholder.map(|s| s.to_string()),
            selective: None,
            force_reply: None,
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
// Telegram Bot API 10.3: Rich Message Blocks
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RichMessageButton {
    pub text: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_app: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<Value>,
}

impl RichMessageButton {
    pub fn callback(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: Value::String(text.into()),
            style: None,
            url: None,
            callback_data: Some(callback_data.into()),
            web_app: None,
            disabled: None,
        }
    }

    pub fn callback_styled(
        text: impl Into<String>,
        callback_data: impl Into<String>,
        style: impl Into<String>,
    ) -> Self {
        let mut button = Self::callback(text, callback_data);
        button.style = Some(style.into());
        button
    }

    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            text: Value::String(text.into()),
            style: None,
            url: Some(url.into()),
            callback_data: None,
            web_app: None,
            disabled: None,
        }
    }

    pub fn disabled(text: impl Into<String>) -> Self {
        Self {
            text: Value::String(text.into()),
            style: None,
            url: None,
            callback_data: None,
            web_app: None,
            disabled: Some(serde_json::json!({})),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let action_count = usize::from(self.url.is_some())
            + usize::from(self.callback_data.is_some())
            + usize::from(self.web_app.is_some())
            + usize::from(self.disabled.is_some());
        if action_count != 1 {
            return Err(format!(
                "RichMessageButton must contain exactly one action, found {action_count}"
            ));
        }
        Ok(())
    }
}

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
pub struct RichBlockListItem {
    pub blocks: Vec<Value>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_checkbox: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_checked: Option<bool>,
}

impl RichBlockListItem {
    pub fn bullet(blocks: Vec<Value>) -> Self {
        Self {
            blocks,
            kind: None,
            value: None,
            has_checkbox: None,
            is_checked: None,
        }
    }

    pub fn ordered(blocks: Vec<Value>, value: Option<i64>) -> Self {
        Self {
            blocks,
            kind: Some("1".to_string()),
            value,
            has_checkbox: None,
            is_checked: None,
        }
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
    List { items: Vec<RichBlockListItem> },

    #[serde(rename = "blockquote")]
    BlockQuotation {
        blocks: Vec<Value>, // [{"type": "paragraph", "text": ...}]
    },

    #[serde(rename = "expandable_blockquote")]
    ExpandableBlockQuotation {
        text: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        credit: Option<Value>,
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

    #[serde(rename = "buttons")]
    Buttons {
        buttons: Vec<RichMessageButton>,
        #[serde(skip_serializing_if = "Option::is_none")]
        align: Option<String>,
    },

    #[serde(rename = "document")]
    Document {
        document: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<Value>,
    },

    #[serde(rename = "details")]
    Details {
        summary: Value,
        blocks: Vec<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_open: Option<bool>,
    },

    #[serde(rename = "anchor")]
    Anchor { name: String },

    #[serde(rename = "thinking")]
    Thinking { text: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct InputRichMessage {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<RichBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_rtl: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_entity_detection: Option<bool>,
}

impl InputRichMessage {
    pub fn new(blocks: Vec<RichBlock>) -> Self {
        Self {
            blocks,
            html: None,
            markdown: None,
            media: None,
            is_rtl: None,
            skip_entity_detection: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let representation_count = usize::from(!self.blocks.is_empty())
            + usize::from(self.html.is_some())
            + usize::from(self.markdown.is_some());
        if representation_count != 1 {
            return Err(format!(
                "InputRichMessage must contain exactly one of blocks, html, or markdown; found {representation_count}"
            ));
        }

        for block in &self.blocks {
            if let RichBlock::Buttons { buttons, .. } = block {
                for button in buttons {
                    button.validate()?;
                }
            }
        }
        Ok(())
    }
}

// ==========================================
// Telegram Updates & Message Payloads
// ==========================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
    pub error_code: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
    pub stopped_message_generation: Option<MessageGenerationStopped>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageGenerationStopped {
    pub chat: Chat,
    pub message_thread_id: Option<i64>,
    pub draft_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub from: Option<User>,
    pub chat: Chat,
    pub date: i64,
    pub message_thread_id: Option<i64>,
    pub receiver_user: Option<User>,
    pub ephemeral_message_id: Option<i64>,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub photo: Option<Vec<PhotoSize>>,
    pub document: Option<Document>,
    pub voice: Option<Voice>,
    pub audio: Option<Audio>,
    pub video: Option<Video>,
    pub video_note: Option<VideoNote>,
    pub reply_to_message: Option<Box<Message>>,
    pub community_chat_joined: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyParameters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ephemeral_message_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_sending_without_reply: Option<bool>,
}

impl ReplyParameters {
    pub fn new(message_id: i64) -> Self {
        Self {
            message_id: Some(message_id),
            ephemeral_message_id: None,
            allow_sending_without_reply: None,
        }
    }

    pub fn ephemeral(ephemeral_message_id: i64) -> Self {
        Self {
            message_id: None,
            ephemeral_message_id: Some(ephemeral_message_id),
            allow_sending_without_reply: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EphemeralMessageParameters {
    pub receiver_user_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_query_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_callback_query_message: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: i64,
    pub is_bot: bool,
    pub first_name: String,
    pub last_name: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    pub title: Option<String>,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: i32,
    pub height: i32,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Voice {
    pub file_id: String,
    pub duration: i32,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audio {
    pub file_id: String,
    pub duration: i32,
    pub performer: Option<String>,
    pub title: Option<String>,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub file_id: String,
    pub width: i32,
    pub height: i32,
    pub duration: i32,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoNote {
    pub file_id: String,
    pub length: i32,
    pub duration: i32,
    pub file_size: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub inline_message_id: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub file_id: String,
    pub file_size: Option<i64>,
    pub file_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_message_requires_exactly_one_representation() {
        let empty = InputRichMessage::default();
        assert!(empty.validate().is_err());

        let mut conflicting = InputRichMessage::new(vec![RichBlock::Paragraph {
            text: Value::String("hello".to_string()),
        }]);
        conflicting.markdown = Some("hello".to_string());
        assert!(conflicting.validate().is_err());

        let valid = InputRichMessage::new(vec![RichBlock::Paragraph {
            text: Value::String("hello".to_string()),
        }]);
        assert!(valid.validate().is_ok());
    }

    #[test]
    fn rich_message_button_requires_exactly_one_action() {
        let mut button = RichMessageButton::callback("Open", "open");
        assert!(button.validate().is_ok());

        button.url = Some("https://example.com".to_string());
        assert!(button.validate().is_err());

        button.callback_data = None;
        assert!(button.validate().is_ok());

        button.url = None;
        assert!(button.validate().is_err());
    }

    #[test]
    fn disabled_inline_button_serializes_as_empty_object() {
        let value = serde_json::to_value(InlineKeyboardButton::disabled("Unavailable")).unwrap();
        assert_eq!(value["disabled"], serde_json::json!({}));
        assert!(value.get("callback_data").is_none());
    }

    #[test]
    fn bot_api_10_3_stop_update_deserializes() {
        let update: Update = serde_json::from_value(serde_json::json!({
            "update_id": 42,
            "stopped_message_generation": {
                "chat": {"id": 7, "type": "private"},
                "draft_id": 99
            }
        }))
        .unwrap();
        let stopped = update.stopped_message_generation.unwrap();
        assert_eq!(stopped.chat.id, 7);
        assert_eq!(stopped.draft_id, 99);
    }

    #[test]
    fn rich_message_buttons_follow_10_3_shape() {
        let block = RichBlock::Buttons {
            buttons: vec![RichMessageButton::callback_styled(
                "Retry", "retry", "primary",
            )],
            align: Some("center".to_string()),
        };
        let value = serde_json::to_value(block).unwrap();
        assert_eq!(value["type"], "buttons");
        assert_eq!(value["buttons"][0]["callback_data"], "retry");
        assert_eq!(value["buttons"][0]["style"], "primary");
    }

    #[test]
    fn expandable_quote_follows_10_3_shape() {
        let block = RichBlock::ExpandableBlockQuotation {
            text: Value::String("detail".to_string()),
            credit: Some(Value::String("source".to_string())),
        };
        let value = serde_json::to_value(block).unwrap();
        assert_eq!(value["type"], "expandable_blockquote");
        assert_eq!(value["text"], "detail");
        assert_eq!(value["credit"], "source");
        assert!(value.get("blocks").is_none());
        assert!(value.get("is_open").is_none());
    }

    #[test]
    fn details_block_uses_summary_and_blocks() {
        let block = RichBlock::Details {
            summary: Value::String("More".to_string()),
            blocks: vec![serde_json::json!({"type": "paragraph", "text": "Body"})],
            is_open: Some(true),
        };
        let value = serde_json::to_value(block).unwrap();
        assert_eq!(value["summary"], "More");
        assert!(value["blocks"].is_array());
        assert_eq!(value["is_open"], true);
    }
}
