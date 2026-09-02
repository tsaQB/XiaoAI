#![allow(dead_code)]

#[path = "models/base.rs"]
mod base;

pub use base::{
    deserialize_flexible_i32, deserialize_flexible_i64, deserialize_flexible_opt_i32,
    deserialize_flexible_opt_i64, ApiResponse, Audio, BotCommand, CallbackQuery, Chat,
    CopyTextButton, Document, EphemeralMessageParameters, FileInfo, InlineKeyboardButton,
    InlineKeyboardMarkup, InputRichMessage, KeyboardButton, Location, LoginUrl, Message,
    MessageGenerationStopped, PhotoSize, ReplyKeyboardMarkup, ReplyKeyboardRemove, ReplyParameters,
    RichBlock, RichBlockCaption, RichBlockListItem, RichBlockTableCell, RichMessageButton,
    SwitchInlineQueryChosenChat, Update, User, Video, VideoNote, Voice, RICH_MESSAGE_MAX_BLOCKS,
    RICH_MESSAGE_MAX_BUTTONS_PER_ROW, RICH_MESSAGE_MAX_MEDIA, RICH_MESSAGE_MAX_NESTING,
    RICH_MESSAGE_MAX_TABLE_COLUMNS, RICH_MESSAGE_MAX_TEXT_CHARS,
};

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputMedia {
    #[serde(rename = "photo")]
    Photo {
        media: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parse_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        show_caption_above_media: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        has_spoiler: Option<bool>,
    },
    #[serde(rename = "video")]
    Video {
        media: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parse_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        show_caption_above_media: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        has_spoiler: Option<bool>,
    },
    #[serde(rename = "animation")]
    Animation {
        media: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parse_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        show_caption_above_media: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        width: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        has_spoiler: Option<bool>,
    },
    #[serde(rename = "audio")]
    Audio {
        media: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parse_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        performer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(rename = "document")]
    Document {
        media: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parse_mode: Option<String>,
    },
    #[serde(rename = "voice_note")]
    VoiceNote {
        media: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parse_mode: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration: Option<i32>,
    },
}

impl InputMedia {
    pub fn photo(
        media: impl Into<String>,
        caption: Option<String>,
        parse_mode: Option<String>,
    ) -> Self {
        Self::Photo {
            media: media.into(),
            caption,
            parse_mode,
            show_caption_above_media: None,
            has_spoiler: None,
        }
    }

    pub fn video(
        media: impl Into<String>,
        caption: Option<String>,
        parse_mode: Option<String>,
    ) -> Self {
        Self::Video {
            media: media.into(),
            caption,
            parse_mode,
            show_caption_above_media: None,
            width: None,
            height: None,
            duration: None,
            has_spoiler: None,
        }
    }

    pub fn audio(
        media: impl Into<String>,
        caption: Option<String>,
        parse_mode: Option<String>,
        title: Option<String>,
        performer: Option<String>,
    ) -> Self {
        Self::Audio {
            media: media.into(),
            caption,
            parse_mode,
            duration: None,
            performer,
            title,
        }
    }

    pub fn document(
        media: impl Into<String>,
        caption: Option<String>,
        parse_mode: Option<String>,
    ) -> Self {
        Self::Document {
            media: media.into(),
            caption,
            parse_mode,
        }
    }

    pub fn validate_media_group(media: &[Self]) -> Result<(), String> {
        if !(2..=10).contains(&media.len()) {
            return Err(format!(
                "sendMediaGroup requires 2-10 media items; found {}",
                media.len()
            ));
        }
        if media
            .iter()
            .any(|item| matches!(item, Self::Animation { .. } | Self::VoiceNote { .. }))
        {
            return Err(
                "sendMediaGroup only supports photo, video, audio, or document media".to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputRichMessageMedia {
    pub id: String,
    pub media: InputMedia,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RichTextButton {
    pub button: RichMessageButton,
}

impl Serialize for RichTextButton {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("RichTextButton", 2)?;
        state.serialize_field("type", "button")?;
        state.serialize_field("button", &self.button)?;
        state.end()
    }
}
