#[path = "../src/bot/models.rs"]
mod models;
#[path = "../src/ai/stream.rs"]
mod stream;

use models::{InputMedia, RichMessageButton, RichTextButton};
use stream::SseDecoder;

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
