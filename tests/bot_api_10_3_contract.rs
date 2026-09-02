#[path = "../src/bot/models.rs"]
pub mod models;
pub mod bot {
    pub use crate::models;
}
#[path = "../src/parser.rs"]
mod parser;
#[path = "../src/ai/stream.rs"]
mod stream;

use models::{InputMedia, RichBlock, RichMessageButton, RichTextButton};
use stream::SseDecoder;

fn photo_media(id: &str) -> InputMedia {
    InputMedia::Photo {
        media: id.to_string(),
        caption: None,
        parse_mode: None,
        show_caption_above_media: None,
        has_spoiler: None,
    }
}

#[test]
fn voice_note_input_media_uses_bot_api_10_3_discriminator() {
    let media = InputMedia::VoiceNote {
        media: "file-id".to_string(),
        caption: None,
        parse_mode: None,
        duration: None,
    };
    let value = serde_json::to_value(media).expect("voice note should serialize");
    assert_eq!(value["type"], "voice_note");
}

#[test]
fn parsed_voice_note_uses_bot_api_10_3_nested_media_discriminator() {
    let blocks =
        parser::parse_markdown_to_rich_blocks("[voice: Rekaman](https://example.com/sample.ogg)");
    let Some(RichBlock::VoiceNote { voice_note, .. }) = blocks.first() else {
        panic!("expected parsed voice-note block");
    };
    assert_eq!(voice_note["type"], "voice_note");
}

#[test]
fn rich_text_button_includes_required_type_discriminator() {
    let rich_text = RichTextButton {
        button: RichMessageButton::callback("Retry", "retry"),
    };
    let value = serde_json::to_value(rich_text).expect("rich text button should serialize");
    assert_eq!(value["type"], "button");
    assert_eq!(value["button"]["callback_data"], "retry");
}

#[test]
fn malformed_sse_data_is_rejected_instead_of_silently_dropped() {
    let mut decoder = SseDecoder::default();
    let error = decoder
        .push(b"data: {not-json}\n\n")
        .expect_err("malformed SSE JSON must be surfaced as an error");
    assert!(error.contains("invalid JSON"), "unexpected error: {error}");
}

#[test]
fn media_group_requires_two_to_ten_album_compatible_items() {
    assert!(InputMedia::validate_media_group(&[photo_media("a")]).is_err());
    assert!(InputMedia::validate_media_group(&[photo_media("a"), photo_media("b")]).is_ok());
    assert!(InputMedia::validate_media_group(
        &(0..10)
            .map(|index| photo_media(&format!("p{index}")))
            .collect::<Vec<_>>()
    )
    .is_ok());
    assert!(InputMedia::validate_media_group(
        &(0..11)
            .map(|index| photo_media(&format!("p{index}")))
            .collect::<Vec<_>>()
    )
    .is_err());

    let animation = InputMedia::Animation {
        media: "anim".to_string(),
        caption: None,
        parse_mode: None,
        show_caption_above_media: None,
        width: None,
        height: None,
        duration: None,
        has_spoiler: None,
    };
    assert!(InputMedia::validate_media_group(&[photo_media("a"), animation]).is_err());

    let voice_note = InputMedia::VoiceNote {
        media: "voice".to_string(),
        caption: None,
        parse_mode: None,
        duration: None,
    };
    assert!(InputMedia::validate_media_group(&[photo_media("a"), voice_note]).is_err());
}

#[test]
fn permanent_send_paths_do_not_emit_draft_id_zero() {
    let source = include_str!("../src/bot/client.rs");
    assert!(
        !source.contains("\"draft_id\": 0"),
        "permanent sendMessage/sendRichMessage payloads must not include draft_id"
    );
}
