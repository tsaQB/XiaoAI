#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def read(rel: str) -> str:
    return (ROOT / rel).read_text(encoding="utf-8")


def write(rel: str, text: str) -> None:
    (ROOT / rel).write_text(text, encoding="utf-8")


def replace_once(rel: str, old: str, new: str) -> None:
    text = read(rel)
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{rel}: expected exactly one literal match, found {count}: {old[:80]!r}")
    write(rel, text.replace(old, new, 1))


def regex_once(rel: str, pattern: str, replacement: str, flags: int = re.S) -> None:
    text = read(rel)
    new, count = re.subn(pattern, replacement, text, count=1, flags=flags)
    if count != 1:
        raise SystemExit(f"{rel}: expected exactly one regex match, found {count}: {pattern[:100]!r}")
    write(rel, new)


def append_once(rel: str, marker: str, block: str) -> None:
    text = read(rel)
    if marker in text:
        return
    if not text.endswith("\n"):
        text += "\n"
    write(rel, text + "\n" + block.strip() + "\n")


MODEL_TESTS = r'''
#[cfg(test)]
mod bot_api_audit_contract_tests {
    use super::*;

    #[test]
    fn input_media_voice_note_uses_voice_note_wire_discriminator() {
        let media = InputMedia::VoiceNote {
            media: "voice-file".to_string(),
            caption: None,
            parse_mode: None,
            duration: Some(3),
        };
        let value = serde_json::to_value(media).unwrap();
        assert_eq!(value["type"], "voice_note");
    }

    #[test]
    fn rich_text_button_includes_required_button_discriminator() {
        let value = serde_json::to_value(RichTextButton {
            button: RichMessageButton::callback("Open", "open"),
        })
        .unwrap();
        assert_eq!(value["type"], "button");
        assert_eq!(value["button"]["callback_data"], "open");
    }

    #[test]
    fn input_rich_message_media_is_typed_and_serializes_exact_media_shape() {
        let mut message = InputRichMessage::new(vec![RichBlock::Paragraph {
            text: Value::String("media".to_string()),
        }]);
        message.media = Some(vec![InputRichMessageMedia {
            id: "photo-1".to_string(),
            media: InputMedia::photo("file-id", None, None),
        }]);
        assert!(message.validate().is_ok());
        let value = serde_json::to_value(message).unwrap();
        assert_eq!(value["media"][0]["id"], "photo-1");
        assert_eq!(value["media"][0]["media"]["type"], "photo");
    }
}
'''

STREAM_TESTS = r'''
#[cfg(test)]
mod audit_malformed_sse_tests {
    use super::*;

    #[test]
    fn malformed_data_event_is_an_explicit_decoder_error() {
        let mut decoder = SseDecoder::default();
        let result = decoder.push(b"data: {not-json}\n\n");
        assert!(result.is_err());
    }
}
'''

CLIENT_TESTS = r'''
#[cfg(test)]
mod audit_transport_contract_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn media_group_contract_rejects_invalid_count_and_types() {
        let one = vec![InputMedia::photo("a", None, None)];
        assert!(validate_media_group_items(&one).is_err());

        let valid = vec![
            InputMedia::photo("a", None, None),
            InputMedia::video("b", None, None),
        ];
        assert!(validate_media_group_items(&valid).is_ok());

        let invalid_voice = vec![
            InputMedia::photo("a", None, None),
            InputMedia::VoiceNote {
                media: "v".to_string(),
                caption: None,
                parse_mode: None,
                duration: None,
            },
        ];
        assert!(validate_media_group_items(&invalid_voice).is_err());

        let mixed_document_audio = vec![
            InputMedia::document("d", None, None),
            InputMedia::audio("a", None, None, None, None),
        ];
        assert!(validate_media_group_items(&mixed_document_audio).is_err());
    }

    #[test]
    fn telegram_retry_policy_honors_retry_after_and_blocks_degradation() {
        let rate_limited = serde_json::json!({
            "ok": false,
            "error_code": 429,
            "parameters": {"retry_after": 7}
        });
        assert!(telegram_response_is_retryable(&rate_limited));
        assert_eq!(telegram_retry_delay(&rate_limited, 0), Duration::from_secs(7));
        assert!(!should_degrade_rich_response(&rate_limited));

        let server_error = serde_json::json!({"ok": false, "error_code": 503});
        assert!(telegram_response_is_retryable(&server_error));
        assert!(!should_degrade_rich_response(&server_error));

        let malformed_rich = serde_json::json!({"ok": false, "error_code": 400});
        assert!(should_degrade_rich_response(&malformed_rich));
    }

    #[test]
    fn multipart_fallback_preserves_thread_ephemeral_and_reply_context() {
        let context = TelegramDeliveryContext {
            message_thread_id: Some(77),
            receiver_user_id: Some(42),
            source_ephemeral_message_id: Some(99),
            callback_query_id: Some("callback-1".to_string()),
        };
        let fields = multipart_delivery_fields(&context, true, None, Some(123)).unwrap();
        let fields: HashMap<String, String> = fields.into_iter().collect();
        assert_eq!(fields.get("message_thread_id").map(String::as_str), Some("77"));

        let ephemeral: Value = serde_json::from_str(fields.get("ephemeral_message_parameters").unwrap()).unwrap();
        assert_eq!(ephemeral["receiver_user_id"], 42);
        assert_eq!(ephemeral["callback_query_id"], "callback-1");

        let reply: Value = serde_json::from_str(fields.get("reply_parameters").unwrap()).unwrap();
        assert_eq!(reply["ephemeral_message_id"], 99);
        assert!(reply.get("message_id").is_none());
    }

    #[tokio::test]
    async fn delivery_and_callback_trackers_are_explicit_scopes() {
        let (_, failed) = TelegramBotClient::with_delivery_tracking(async {
            TelegramBotClient::mark_terminal_delivery_failure();
        })
        .await;
        assert!(failed);

        let (_, answered) = TelegramBotClient::with_callback_answer_tracking(async {
            TelegramBotClient::mark_callback_answered();
        })
        .await;
        assert!(answered);
    }
}
'''

STORAGE_TESTS = r'''
#[cfg(test)]
mod audit_delivery_checkpoint_tests {
    use super::*;

    #[test]
    fn terminal_delivery_failure_never_becomes_completed() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE telegram_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE telegram_inbox (
                update_id INTEGER PRIMARY KEY,
                payload_json TEXT NOT NULL,
                status TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                received_at TEXT NOT NULL,
                last_error TEXT
             );",
        )
        .unwrap();
        assert!(enqueue_telegram_update_on_conn(&mut conn, 10, "{\"update_id\":10}").unwrap());
        assert!(mark_telegram_processing_on_conn(&conn, 10).unwrap());
        assert!(mark_telegram_delivery_failed_on_conn(&conn, 10, "sendMessage failed").unwrap());
        let (status, payload, error): (String, String, Option<String>) = conn
            .query_row(
                "SELECT status,payload_json,last_error FROM telegram_inbox WHERE update_id=10",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "delivery_failed");
        assert!(payload.contains("update_id"));
        assert_eq!(error.as_deref(), Some("sendMessage failed"));
    }
}
'''


def add_tests() -> None:
    append_once("src/bot/models.rs", "mod bot_api_audit_contract_tests", MODEL_TESTS)
    append_once("src/ai/stream.rs", "mod audit_malformed_sse_tests", STREAM_TESTS)
    append_once("src/bot/client.rs", "mod audit_transport_contract_tests", CLIENT_TESTS)
    append_once("src/ai/storage.rs", "mod audit_delivery_checkpoint_tests", STORAGE_TESTS)


def fix_models() -> None:
    replace_once(
        "src/bot/models.rs",
        '    #[serde(rename = "voice")]\n    VoiceNote {',
        '    #[serde(rename = "voice_note")]\n    VoiceNote {',
    )
    replace_once(
        "src/bot/models.rs",
        '#[derive(Debug, Clone, Serialize, Deserialize)]\npub struct RichTextButton {\n    pub button: RichMessageButton,\n}',
        '''#[derive(Debug, Clone, Deserialize)]
pub struct RichTextButton {
    pub button: RichMessageButton,
}

impl Serialize for RichTextButton {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("RichTextButton", 2)?;
        state.serialize_field("type", "button")?;
        state.serialize_field("button", &self.button)?;
        state.end()
    }
}''',
    )
    replace_once(
        "src/bot/models.rs",
        '    pub media: Option<Vec<Value>>,',
        '    pub media: Option<Vec<InputRichMessageMedia>>,',
    )
    replace_once(
        "src/bot/models.rs",
        '''        message.media = Some(
            (0..RICH_MESSAGE_MAX_MEDIA)
                .map(|_| serde_json::json!({}))
                .collect(),
        );''',
        '''        message.media = Some(
            (0..RICH_MESSAGE_MAX_MEDIA)
                .map(|index| InputRichMessageMedia {
                    id: format!("media-{index}"),
                    media: InputMedia::photo(format!("file-{index}"), None, None),
                })
                .collect(),
        );''',
    )
    replace_once(
        "src/bot/models.rs",
        '        message.media.as_mut().unwrap().push(serde_json::json!({}));',
        '''        message.media.as_mut().unwrap().push(InputRichMessageMedia {
            id: "overflow".to_string(),
            media: InputMedia::photo("overflow-file", None, None),
        });''',
    )

    parser = read("src/parser.rs")
    parser = parser.replace('("[voice:", "voice"),', '("[voice:", "voice_note"),')
    parser = parser.replace('("[voicenote:", "voice"),', '("[voicenote:", "voice_note"),')
    parser = parser.replace('                    "voice" => Some(RichBlock::VoiceNote {', '                    "voice_note" => Some(RichBlock::VoiceNote {')
    parser = parser.replace('json!({"type": "voice", "media": link})', 'json!({"type": "voice_note", "media": link})')
    write("src/parser.rs", parser)

    client = read("src/bot/client.rs")
    client = client.replace('serde_json::json!({"type": "voice", "media": "voice_1"})', 'serde_json::json!({"type": "voice_note", "media": "voice_1"})')
    write("src/bot/client.rs", client)


def fix_stream() -> None:
    path = "src/ai/stream.rs"
    text = read(path)
    text = text.replace('            self.process_line(&line, &mut events);', '            self.process_line(&line, &mut events)?;')
    text = text.replace('            self.process_line(&line, &mut events);\n        }\n        self.flush_event(&mut events);', '            self.process_line(&line, &mut events)?;\n        }\n        self.flush_event(&mut events)?;')
    old = '''    fn process_line(&mut self, line: &str, events: &mut Vec<StreamEvent>) {
        if line.is_empty() {
            self.flush_event(events);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data_lines.push(data.trim_start().to_string());
        }
    }

    fn flush_event(&mut self, events: &mut Vec<StreamEvent>) {
        if self.data_lines.is_empty() {
            return;
        }
        let payload = self.data_lines.join("\\n");
        self.data_lines.clear();
        let trimmed = payload.trim();
        if trimmed == "[DONE]" {
            events.push(StreamEvent::Done);
            return;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            events.push(StreamEvent::Json(value));
        }
    }'''
    new = '''    fn process_line(
        &mut self,
        line: &str,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), String> {
        if line.is_empty() {
            return self.flush_event(events);
        }
        if line.starts_with(':') {
            return Ok(());
        }
        if let Some(data) = line.strip_prefix("data:") {
            self.data_lines.push(data.trim_start().to_string());
        }
        Ok(())
    }

    fn flush_event(&mut self, events: &mut Vec<StreamEvent>) -> Result<(), String> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let payload = self.data_lines.join("\\n");
        self.data_lines.clear();
        let trimmed = payload.trim();
        if trimmed == "[DONE]" {
            events.push(StreamEvent::Done);
            return Ok(());
        }
        let value = serde_json::from_str::<Value>(trimmed)
            .map_err(|_| "provider SSE data event contained malformed JSON".to_string())?;
        events.push(StreamEvent::Json(value));
        Ok(())
    }'''
    if old not in text:
        raise SystemExit("src/ai/stream.rs: decoder body pattern not found")
    text = text.replace(old, new, 1)
    write(path, text)


def fix_storage() -> None:
    insert_after = '''fn mark_telegram_processed_on_conn(conn: &Connection, update_id: i64) -> rusqlite::Result<bool> {
    let scrubbed = serde_json::json!({
        "update_id": update_id,
        "payload": "redacted_after_completion"
    })
    .to_string();
    Ok(conn.execute(
        "UPDATE telegram_inbox
         SET status='completed',payload_json=?2,last_error=NULL
         WHERE update_id=?1 AND status='processing'",
        params![update_id, scrubbed],
    )? == 1)
}
'''
    addition = insert_after + '''
fn mark_telegram_delivery_failed_db(update_id: i64, error: &str) -> rusqlite::Result<bool> {
    let conn = open_session_db()?;
    mark_telegram_delivery_failed_on_conn(&conn, update_id, error)
}

fn mark_telegram_delivery_failed_on_conn(
    conn: &Connection,
    update_id: i64,
    error: &str,
) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE telegram_inbox
         SET status='delivery_failed',last_error=?2
         WHERE update_id=?1 AND status='processing'",
        params![update_id, error],
    )? == 1)
}
'''
    replace_once("src/ai/storage.rs", insert_after, addition)
    async_anchor = '''pub(crate) async fn mark_telegram_processed_async(update_id: i64) -> bool {
    run_db("mark_telegram_processed", move || {
        mark_telegram_processed_db(update_id)
    })
    .await
    .unwrap_or(false)
}
'''
    async_addition = async_anchor + '''
pub(crate) async fn mark_telegram_delivery_failed_async(update_id: i64, error: String) -> bool {
    run_db("mark_telegram_delivery_failed", move || {
        mark_telegram_delivery_failed_db(update_id, &error)
    })
    .await
    .unwrap_or(false)
}
'''
    replace_once("src/ai/storage.rs", async_anchor, async_addition)


def fix_client_helpers() -> None:
    replace_once(
        "src/bot/client.rs",
        'use std::time::Duration;\n',
        'use std::sync::{Arc, atomic::{AtomicBool, Ordering}};\nuse std::time::Duration;\n',
    )
    anchor = '''const MAX_TELEGRAM_DOWNLOAD_BYTES: usize = 20 * 1024 * 1024;
const MAX_TELEGRAM_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
'''
    helpers = anchor + r'''
const MAX_TELEGRAM_ATTEMPTS: usize = 3;
const MAX_TELEGRAM_RETRY_AFTER_SECS: u64 = 30;

#[derive(Debug, Clone)]
struct MultipartFileSpec {
    field: String,
    bytes: Vec<u8>,
    file_name: String,
    mime_type: String,
}

#[derive(Clone, Default)]
struct DeliveryFailureTracker(Arc<AtomicBool>);

#[derive(Clone, Default)]
struct CallbackAnswerTracker(Arc<AtomicBool>);

tokio::task_local! {
    static TELEGRAM_DELIVERY_FAILURE_TRACKER: DeliveryFailureTracker;
    static TELEGRAM_CALLBACK_ANSWER_TRACKER: CallbackAnswerTracker;
}

fn telegram_response_is_retryable(response: &Value) -> bool {
    let code = response
        .get("error_code")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    code == 429 || (500..=599).contains(&code)
}

fn telegram_retry_delay(response: &Value, attempt: usize) -> Duration {
    if let Some(seconds) = response
        .pointer("/parameters/retry_after")
        .and_then(Value::as_u64)
    {
        return Duration::from_secs(seconds.clamp(1, MAX_TELEGRAM_RETRY_AFTER_SECS));
    }
    Duration::from_millis(500_u64.saturating_mul(1_u64 << attempt.min(5)))
}

fn should_degrade_rich_response(response: &Value) -> bool {
    matches!(
        response.get("error_code").and_then(Value::as_i64),
        Some(400 | 404 | 405)
    )
}

fn telegram_error_allows_media_upload_fallback(error: &str) -> bool {
    error.contains("code=400:")
}

fn validate_media_group_items(media: &[InputMedia]) -> Result<(), String> {
    if !(2..=10).contains(&media.len()) {
        return Err(format!(
            "sendMediaGroup requires 2-10 media items, found {}",
            media.len()
        ));
    }

    enum Family {
        Visual,
        Audio,
        Document,
    }
    let family = match media.first() {
        Some(InputMedia::Photo { .. } | InputMedia::Video { .. }) => Family::Visual,
        Some(InputMedia::Audio { .. }) => Family::Audio,
        Some(InputMedia::Document { .. }) => Family::Document,
        Some(InputMedia::Animation { .. } | InputMedia::VoiceNote { .. }) => {
            return Err("sendMediaGroup does not accept animation or voice_note media".to_string())
        }
        None => unreachable!(),
    };

    for item in media {
        let compatible = match family {
            Family::Visual => matches!(item, InputMedia::Photo { .. } | InputMedia::Video { .. }),
            Family::Audio => matches!(item, InputMedia::Audio { .. }),
            Family::Document => matches!(item, InputMedia::Document { .. }),
        };
        if !compatible {
            return Err(
                "sendMediaGroup requires audio/document albums to contain only the same media type; photo/video albums may mix"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn multipart_delivery_fields(
    context: &TelegramDeliveryContext,
    include_ephemeral: bool,
    explicit_receiver_user_id: Option<i64>,
    reply_to_message_id: Option<i64>,
) -> Result<Vec<(String, String)>, String> {
    let mut fields = Vec::new();
    if let Some(thread_id) = context.message_thread_id {
        fields.push(("message_thread_id".to_string(), thread_id.to_string()));
    }
    if include_ephemeral {
        let receiver_user_id = explicit_receiver_user_id.or(context.receiver_user_id);
        if let Some(receiver_user_id) = receiver_user_id {
            fields.push((
                "ephemeral_message_parameters".to_string(),
                serde_json::to_string(&EphemeralMessageParameters {
                    receiver_user_id,
                    callback_query_id: context.callback_query_id.clone(),
                    replace_callback_query_message: None,
                })
                .map_err(|error| error.to_string())?,
            ));
        }
    }
    let reply_parameters = if let Some(ephemeral_message_id) = context.source_ephemeral_message_id {
        Some(ReplyParameters::ephemeral(ephemeral_message_id))
    } else {
        reply_to_message_id.map(ReplyParameters::new)
    };
    if let Some(reply_parameters) = reply_parameters {
        fields.push((
            "reply_parameters".to_string(),
            serde_json::to_string(&reply_parameters).map_err(|error| error.to_string())?,
        ));
    }
    Ok(fields)
}

fn input_media_source(media: &InputMedia) -> &str {
    match media {
        InputMedia::Photo { media, .. }
        | InputMedia::Video { media, .. }
        | InputMedia::Animation { media, .. }
        | InputMedia::Audio { media, .. }
        | InputMedia::Document { media, .. }
        | InputMedia::VoiceNote { media, .. } => media,
    }
}

fn set_input_media_source(media: &mut InputMedia, source: String) {
    match media {
        InputMedia::Photo { media, .. }
        | InputMedia::Video { media, .. }
        | InputMedia::Animation { media, .. }
        | InputMedia::Audio { media, .. }
        | InputMedia::Document { media, .. }
        | InputMedia::VoiceNote { media, .. } => *media = source,
    }
}
'''
    replace_once("src/bot/client.rs", anchor, helpers)

    impl_anchor = '''impl TelegramBotClient {
    pub async fn with_delivery_context<F, T>(context: TelegramDeliveryContext, future: F) -> T
'''
    impl_repl = '''impl TelegramBotClient {
    pub async fn with_delivery_tracking<F, T>(future: F) -> (T, bool)
    where
        F: std::future::Future<Output = T>,
    {
        let tracker = DeliveryFailureTracker::default();
        let result = TELEGRAM_DELIVERY_FAILURE_TRACKER
            .scope(tracker.clone(), future)
            .await;
        (result, tracker.0.load(Ordering::SeqCst))
    }

    pub async fn with_callback_answer_tracking<F, T>(future: F) -> (T, bool)
    where
        F: std::future::Future<Output = T>,
    {
        let tracker = CallbackAnswerTracker::default();
        let result = TELEGRAM_CALLBACK_ANSWER_TRACKER
            .scope(tracker.clone(), future)
            .await;
        (result, tracker.0.load(Ordering::SeqCst))
    }

    fn mark_terminal_delivery_failure() {
        let _ = TELEGRAM_DELIVERY_FAILURE_TRACKER.try_with(|tracker| {
            tracker.0.store(true, Ordering::SeqCst);
        });
    }

    fn terminal_delivery_failed() -> bool {
        TELEGRAM_DELIVERY_FAILURE_TRACKER
            .try_with(|tracker| tracker.0.load(Ordering::SeqCst))
            .unwrap_or(false)
    }

    fn restore_delivery_failure(checkpoint: bool) {
        if !checkpoint {
            let _ = TELEGRAM_DELIVERY_FAILURE_TRACKER.try_with(|tracker| {
                tracker.0.store(false, Ordering::SeqCst);
            });
        }
    }

    fn mark_callback_answered() {
        let _ = TELEGRAM_CALLBACK_ANSWER_TRACKER.try_with(|tracker| {
            tracker.0.store(true, Ordering::SeqCst);
        });
    }

    fn terminal_delivery_error<T>(error: String) -> Result<T, String> {
        Self::mark_terminal_delivery_failure();
        Err(error)
    }

    fn tracks_terminal_delivery(method: &str) -> bool {
        matches!(
            method,
            "sendMessage"
                | "sendLocation"
                | "editMessageText"
                | "editEphemeralMessageText"
                | "editEphemeralMessageCaption"
                | "editEphemeralMessageReplyMarkup"
                | "deleteMessage"
                | "deleteEphemeralMessage"
        )
    }

    pub async fn with_delivery_context<F, T>(context: TelegramDeliveryContext, future: F) -> T
'''
    replace_once("src/bot/client.rs", impl_anchor, impl_repl)


def fix_client_transport() -> None:
    post_raw_pattern = r'''    async fn post_json_raw\(&self, method: &str, payload: Value\) -> Result<Value, String> \{.*?\n    \}\n\n    async fn post_json\(&self, method: &str, payload: Value\) -> Result<Value, String> \{.*?\n    \}\n'''
    post_repl = r'''    async fn post_json_raw(&self, method: &str, payload: Value) -> Result<Value, String> {
        let url = format!("{}/{}", self.base_url, method);
        for attempt in 0..MAX_TELEGRAM_ATTEMPTS {
            match self.client.post(&url).json(&payload).send().await {
                Ok(resp) => match read_bounded_json_response(resp, MAX_TELEGRAM_RESPONSE_BYTES).await {
                    Ok(response) => {
                        if telegram_response_is_retryable(&response)
                            && attempt + 1 < MAX_TELEGRAM_ATTEMPTS
                        {
                            let delay = telegram_retry_delay(&response, attempt);
                            warn!(
                                "Transient Telegram API response for {method}; retrying attempt {}/{} after {:?}",
                                attempt + 2,
                                MAX_TELEGRAM_ATTEMPTS,
                                delay
                            );
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        return Ok(response);
                    }
                    Err(err_msg) if attempt + 1 < MAX_TELEGRAM_ATTEMPTS => {
                        warn!(
                            "Telegram response decode failed for {method}; retrying attempt {}/{}",
                            attempt + 2,
                            MAX_TELEGRAM_ATTEMPTS
                        );
                        tokio::time::sleep(Duration::from_millis(
                            500_u64.saturating_mul(1_u64 << attempt.min(5)),
                        ))
                        .await;
                        continue;
                    }
                    Err(err_msg) => {
                        error!("Failed to parse response JSON for {method}: {err_msg}");
                        return Err(format!(
                            "Failed to parse response JSON for {method}: {err_msg}"
                        ));
                    }
                },
                Err(error)
                    if (error.is_timeout() || error.is_connect())
                        && attempt + 1 < MAX_TELEGRAM_ATTEMPTS =>
                {
                    let delay = Duration::from_millis(
                        500_u64.saturating_mul(1_u64 << attempt.min(5)),
                    );
                    warn!(
                        "Transient Telegram transport failure for {method}; retrying attempt {}/{}",
                        attempt + 2,
                        MAX_TELEGRAM_ATTEMPTS
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(error) => {
                    let err_msg =
                        format!("HTTP error for {method}: {}", reqwest_error_kind(&error));
                    error!("{err_msg}");
                    return Err(err_msg);
                }
            }
        }
        Err(format!("Telegram request {method} exhausted retry attempts"))
    }

    async fn post_json(&self, method: &str, payload: Value) -> Result<Value, String> {
        let response = self.post_json_raw(method, payload).await?;
        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(response);
        }
        let error = Self::telegram_api_error(method, &response);
        warn!("{error}");
        if Self::tracks_terminal_delivery(method) {
            Self::mark_terminal_delivery_failure();
        }
        Err(error)
    }

    async fn post_multipart(
        &self,
        method: &str,
        fields: Vec<(String, String)>,
        files: Vec<MultipartFileSpec>,
    ) -> Result<Value, String> {
        let url = format!("{}/{}", self.base_url, method);
        for attempt in 0..MAX_TELEGRAM_ATTEMPTS {
            let mut form = Form::new();
            for (name, value) in &fields {
                form = form.text(name.clone(), value.clone());
            }
            for file in &files {
                let part = Part::bytes(file.bytes.clone())
                    .file_name(file.file_name.clone())
                    .mime_str(&file.mime_type)
                    .map_err(|error| error.to_string())?;
                form = form.part(file.field.clone(), part);
            }

            match self.client.post(&url).multipart(form).send().await {
                Ok(resp) => match read_bounded_json_response(resp, MAX_TELEGRAM_RESPONSE_BYTES).await {
                    Ok(response)
                        if telegram_response_is_retryable(&response)
                            && attempt + 1 < MAX_TELEGRAM_ATTEMPTS =>
                    {
                        let delay = telegram_retry_delay(&response, attempt);
                        warn!(
                            "Transient Telegram multipart response for {method}; retrying attempt {}/{} after {:?}",
                            attempt + 2,
                            MAX_TELEGRAM_ATTEMPTS,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Ok(response) if response.get("ok").and_then(Value::as_bool) == Some(true) => {
                        return Ok(response);
                    }
                    Ok(response) => return Err(Self::telegram_api_error(method, &response)),
                    Err(error) if attempt + 1 < MAX_TELEGRAM_ATTEMPTS => {
                        warn!(
                            "Telegram multipart response decode failed for {method}; retrying attempt {}/{}",
                            attempt + 2,
                            MAX_TELEGRAM_ATTEMPTS
                        );
                        tokio::time::sleep(Duration::from_millis(
                            500_u64.saturating_mul(1_u64 << attempt.min(5)),
                        ))
                        .await;
                    }
                    Err(error) => {
                        return Err(format!("{method} response decode error: {error}"));
                    }
                },
                Err(error)
                    if (error.is_timeout() || error.is_connect())
                        && attempt + 1 < MAX_TELEGRAM_ATTEMPTS =>
                {
                    tokio::time::sleep(Duration::from_millis(
                        500_u64.saturating_mul(1_u64 << attempt.min(5)),
                    ))
                    .await;
                }
                Err(error) => {
                    return Err(format!(
                        "{method} multipart error: {}",
                        reqwest_error_kind(&error)
                    ));
                }
            }
        }
        Err(format!("Telegram multipart request {method} exhausted retry attempts"))
    }
'''
    regex_once("src/bot/client.rs", post_raw_pattern, post_repl)


def replace_client_media_functions() -> None:
    photo_bytes = r'''    pub async fn send_photo_bytes(
        &self,
        chat_id: i64,
        photo_bytes: Vec<u8>,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        let (file_name, mime_type) = if photo_bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            ("image.png", "image/png")
        } else if photo_bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            ("image.jpg", "image/jpeg")
        } else if photo_bytes.starts_with(b"GIF87a") || photo_bytes.starts_with(b"GIF89a") {
            ("image.gif", "image/gif")
        } else if photo_bytes.len() >= 12
            && &photo_bytes[..4] == b"RIFF"
            && &photo_bytes[8..12] == b"WEBP"
        {
            ("image.webp", "image/webp")
        } else {
            return Self::terminal_delivery_error(
                "sendPhoto rejected bytes with an unsupported image signature".to_string(),
            );
        };
        let mut fields = vec![("chat_id".to_string(), chat_id.to_string())];
        if let Some(caption) = caption {
            fields.push(("caption".to_string(), caption.to_string()));
        }
        if let Some(parse_mode) = parse_mode {
            fields.push(("parse_mode".to_string(), parse_mode.to_string()));
        }
        if let Some(reply_markup) = reply_markup {
            fields.push(("reply_markup".to_string(), reply_markup.to_string()));
        }
        fields.extend(multipart_delivery_fields(
            &Self::current_delivery_context(),
            true,
            None,
            reply_to_message_id,
        )?);
        let result = self
            .post_multipart(
                "sendPhoto",
                fields,
                vec![MultipartFileSpec {
                    field: "photo".to_string(),
                    bytes: photo_bytes,
                    file_name: file_name.to_string(),
                    mime_type: mime_type.to_string(),
                }],
            )
            .await;
        match result {
            Ok(response) => Ok(response),
            Err(error) => Self::terminal_delivery_error(error),
        }
    }
'''
    regex_once(
        "src/bot/client.rs",
        r'    pub async fn send_photo_bytes\(.*?\n    \}\n\n    pub async fn download_media_bytes',
        photo_bytes + '\n    pub async fn download_media_bytes',
    )

    # Media-group validation + ephemeral-aware multipart retry.
    media_group = r'''    pub async fn send_media_group(
        &self,
        chat_id: i64,
        media: &[InputMedia],
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        validate_media_group_items(media)?;
        let media_json = serde_json::to_value(media).map_err(|error| error.to_string())?;
        let mut payload = json!({"chat_id": chat_id, "media": media_json});
        if let Some(reply_to_message_id) = reply_to_message_id {
            payload["reply_parameters"] = serde_json::to_value(ReplyParameters::new(reply_to_message_id))
                .unwrap_or(json!({}));
        }
        Self::apply_delivery_context(&mut payload, true);

        match self.post_json("sendMediaGroup", payload).await {
            Ok(response) => Ok(response),
            Err(error) if telegram_error_allows_media_upload_fallback(&error) => {
                let mut updated_media = Vec::with_capacity(media.len());
                let mut files = Vec::new();
                for (index, item) in media.iter().enumerate() {
                    let mut item = item.clone();
                    let target_url = input_media_source(&item).to_string();
                    if target_url.starts_with("http://") || target_url.starts_with("https://") {
                        if let Some((bytes, mime_type, file_name)) = self
                            .download_media_bytes(&target_url, MAX_TELEGRAM_DOWNLOAD_BYTES)
                            .await
                        {
                            let field = format!("file_{index}");
                            set_input_media_source(&mut item, format!("attach://{field}"));
                            files.push(MultipartFileSpec {
                                field,
                                bytes,
                                file_name,
                                mime_type,
                            });
                        }
                    }
                    updated_media.push(item);
                }
                if files.is_empty() {
                    return Self::terminal_delivery_error(error);
                }
                let mut fields = vec![
                    ("chat_id".to_string(), chat_id.to_string()),
                    (
                        "media".to_string(),
                        serde_json::to_string(&updated_media).map_err(|error| error.to_string())?,
                    ),
                ];
                fields.extend(multipart_delivery_fields(
                    &Self::current_delivery_context(),
                    true,
                    None,
                    reply_to_message_id,
                )?);
                match self.post_multipart("sendMediaGroup", fields, files).await {
                    Ok(response) => Ok(response),
                    Err(error) => Self::terminal_delivery_error(error),
                }
            }
            Err(error) => Self::terminal_delivery_error(error),
        }
    }
'''
    regex_once(
        "src/bot/client.rs",
        r'    pub async fn send_media_group\(.*?\n    \}\n\n    #\[allow\(clippy::too_many_arguments\)\]\n    pub async fn send_audio',
        media_group + '\n    #[allow(clippy::too_many_arguments)]\n    pub async fn send_audio',
    )

    def replace_media_method(name: str, next_marker: str, body: str) -> None:
        pattern = rf'    (?:#\[allow\(clippy::too_many_arguments\)\]\n    )?pub async fn {name}\(.*?\n    \}}\n\n    {next_marker}'
        regex_once("src/bot/client.rs", pattern, body + '\n\n    ' + next_marker)

    audio = r'''#[allow(clippy::too_many_arguments)]
    pub async fn send_audio(
        &self,
        chat_id: i64,
        audio: &str,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        title: Option<&str>,
        performer: Option<&str>,
        duration: Option<i32>,
        reply_markup: Option<Value>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        let mut payload = json!({"chat_id": chat_id, "audio": audio});
        if let Some(value) = caption { payload["caption"] = json!(value); }
        if let Some(value) = parse_mode { payload["parse_mode"] = json!(value); }
        if let Some(value) = title { payload["title"] = json!(value); }
        if let Some(value) = performer { payload["performer"] = json!(value); }
        if let Some(value) = duration { payload["duration"] = json!(value); }
        if let Some(value) = reply_markup.as_ref() { payload["reply_markup"] = value.clone(); }
        if let Some(value) = reply_to_message_id {
            payload["reply_parameters"] = serde_json::to_value(ReplyParameters::new(value)).unwrap_or(json!({}));
        }
        Self::apply_delivery_context(&mut payload, true);
        match self.post_json("sendAudio", payload).await {
            Ok(response) => Ok(response),
            Err(error)
                if telegram_error_allows_media_upload_fallback(&error)
                    && (audio.starts_with("http://") || audio.starts_with("https://")) =>
            {
                let Some((bytes, mime_type, file_name)) = self
                    .download_media_bytes(audio, MAX_TELEGRAM_DOWNLOAD_BYTES)
                    .await
                else {
                    return Self::terminal_delivery_error(error);
                };
                let mut fields = vec![("chat_id".to_string(), chat_id.to_string())];
                for (name, value) in [
                    ("caption", caption.map(str::to_string)),
                    ("parse_mode", parse_mode.map(str::to_string)),
                    ("title", title.map(str::to_string)),
                    ("performer", performer.map(str::to_string)),
                    ("duration", duration.map(|value| value.to_string())),
                    ("reply_markup", reply_markup.map(|value| value.to_string())),
                ] {
                    if let Some(value) = value { fields.push((name.to_string(), value)); }
                }
                fields.extend(multipart_delivery_fields(&Self::current_delivery_context(), true, None, reply_to_message_id)?);
                match self.post_multipart("sendAudio", fields, vec![MultipartFileSpec {
                    field: "audio".to_string(), bytes, file_name, mime_type,
                }]).await {
                    Ok(response) => Ok(response),
                    Err(error) => Self::terminal_delivery_error(error),
                }
            }
            Err(error) => Self::terminal_delivery_error(error),
        }
    }'''
    replace_media_method("send_audio", '#[allow(clippy::too_many_arguments)]\n    pub async fn send_voice', audio)

    voice = r'''#[allow(clippy::too_many_arguments)]
    pub async fn send_voice(
        &self,
        chat_id: i64,
        voice: &str,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        duration: Option<i32>,
        reply_markup: Option<Value>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        let mut payload = json!({"chat_id": chat_id, "voice": voice});
        if let Some(value) = caption { payload["caption"] = json!(value); }
        if let Some(value) = parse_mode { payload["parse_mode"] = json!(value); }
        if let Some(value) = duration { payload["duration"] = json!(value); }
        if let Some(value) = reply_markup.as_ref() { payload["reply_markup"] = value.clone(); }
        if let Some(value) = reply_to_message_id {
            payload["reply_parameters"] = serde_json::to_value(ReplyParameters::new(value)).unwrap_or(json!({}));
        }
        Self::apply_delivery_context(&mut payload, true);
        match self.post_json("sendVoice", payload).await {
            Ok(response) => Ok(response),
            Err(error)
                if telegram_error_allows_media_upload_fallback(&error)
                    && (voice.starts_with("http://") || voice.starts_with("https://")) =>
            {
                let Some((bytes, mime_type, file_name)) = self.download_media_bytes(voice, MAX_TELEGRAM_DOWNLOAD_BYTES).await else {
                    return Self::terminal_delivery_error(error);
                };
                let mut fields = vec![("chat_id".to_string(), chat_id.to_string())];
                if let Some(value) = caption { fields.push(("caption".to_string(), value.to_string())); }
                if let Some(value) = parse_mode { fields.push(("parse_mode".to_string(), value.to_string())); }
                if let Some(value) = duration { fields.push(("duration".to_string(), value.to_string())); }
                if let Some(value) = reply_markup { fields.push(("reply_markup".to_string(), value.to_string())); }
                fields.extend(multipart_delivery_fields(&Self::current_delivery_context(), true, None, reply_to_message_id)?);
                match self.post_multipart("sendVoice", fields, vec![MultipartFileSpec {
                    field: "voice".to_string(), bytes, file_name, mime_type,
                }]).await {
                    Ok(response) => Ok(response),
                    Err(error) => Self::terminal_delivery_error(error),
                }
            }
            Err(error) => Self::terminal_delivery_error(error),
        }
    }'''
    replace_media_method("send_voice", 'pub async fn send_video', voice)

    video = r'''pub async fn send_video(
        &self,
        chat_id: i64,
        video: &str,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        let mut payload = json!({"chat_id": chat_id, "video": video});
        if let Some(value) = caption { payload["caption"] = json!(value); }
        if let Some(value) = parse_mode { payload["parse_mode"] = json!(value); }
        if let Some(value) = reply_markup.as_ref() { payload["reply_markup"] = value.clone(); }
        if let Some(value) = reply_to_message_id {
            payload["reply_parameters"] = serde_json::to_value(ReplyParameters::new(value)).unwrap_or(json!({}));
        }
        Self::apply_delivery_context(&mut payload, true);
        match self.post_json("sendVideo", payload).await {
            Ok(response) => Ok(response),
            Err(error)
                if telegram_error_allows_media_upload_fallback(&error)
                    && (video.starts_with("http://") || video.starts_with("https://")) =>
            {
                let Some((bytes, mime_type, file_name)) = self.download_media_bytes(video, MAX_TELEGRAM_DOWNLOAD_BYTES).await else {
                    return Self::terminal_delivery_error(error);
                };
                let mut fields = vec![("chat_id".to_string(), chat_id.to_string())];
                if let Some(value) = caption { fields.push(("caption".to_string(), value.to_string())); }
                if let Some(value) = parse_mode { fields.push(("parse_mode".to_string(), value.to_string())); }
                if let Some(value) = reply_markup { fields.push(("reply_markup".to_string(), value.to_string())); }
                fields.extend(multipart_delivery_fields(&Self::current_delivery_context(), true, None, reply_to_message_id)?);
                match self.post_multipart("sendVideo", fields, vec![MultipartFileSpec {
                    field: "video".to_string(), bytes, file_name, mime_type,
                }]).await {
                    Ok(response) => Ok(response),
                    Err(error) => Self::terminal_delivery_error(error),
                }
            }
            Err(error) => Self::terminal_delivery_error(error),
        }
    }'''
    replace_media_method("send_video", 'pub async fn send_animation', video)

    animation = r'''pub async fn send_animation(
        &self,
        chat_id: i64,
        animation: &str,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        let mut payload = json!({"chat_id": chat_id, "animation": animation});
        if let Some(value) = caption { payload["caption"] = json!(value); }
        if let Some(value) = parse_mode { payload["parse_mode"] = json!(value); }
        if let Some(value) = reply_markup.as_ref() { payload["reply_markup"] = value.clone(); }
        if let Some(value) = reply_to_message_id {
            payload["reply_parameters"] = serde_json::to_value(ReplyParameters::new(value)).unwrap_or(json!({}));
        }
        Self::apply_delivery_context(&mut payload, true);
        match self.post_json("sendAnimation", payload).await {
            Ok(response) => Ok(response),
            Err(error)
                if telegram_error_allows_media_upload_fallback(&error)
                    && (animation.starts_with("http://") || animation.starts_with("https://")) =>
            {
                let Some((bytes, mime_type, file_name)) = self.download_media_bytes(animation, MAX_TELEGRAM_DOWNLOAD_BYTES).await else {
                    return Self::terminal_delivery_error(error);
                };
                let mut fields = vec![("chat_id".to_string(), chat_id.to_string())];
                if let Some(value) = caption { fields.push(("caption".to_string(), value.to_string())); }
                if let Some(value) = parse_mode { fields.push(("parse_mode".to_string(), value.to_string())); }
                if let Some(value) = reply_markup { fields.push(("reply_markup".to_string(), value.to_string())); }
                fields.extend(multipart_delivery_fields(&Self::current_delivery_context(), true, None, reply_to_message_id)?);
                match self.post_multipart("sendAnimation", fields, vec![MultipartFileSpec {
                    field: "animation".to_string(), bytes, file_name, mime_type,
                }]).await {
                    Ok(response) => Ok(response),
                    Err(error) => Self::terminal_delivery_error(error),
                }
            }
            Err(error) => Self::terminal_delivery_error(error),
        }
    }'''
    replace_media_method("send_animation", '#[allow(clippy::too_many_arguments)]\n    pub async fn send_location', animation)

    document = r'''pub async fn send_document(
        &self,
        chat_id: i64,
        document: &str,
        caption: Option<&str>,
        parse_mode: Option<&str>,
        reply_markup: Option<Value>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Value, String> {
        let mut payload = json!({"chat_id": chat_id, "document": document});
        if let Some(value) = caption { payload["caption"] = json!(value); }
        if let Some(value) = parse_mode { payload["parse_mode"] = json!(value); }
        if let Some(value) = reply_markup.as_ref() { payload["reply_markup"] = value.clone(); }
        if let Some(value) = reply_to_message_id {
            payload["reply_parameters"] = serde_json::to_value(ReplyParameters::new(value)).unwrap_or(json!({}));
        }
        Self::apply_delivery_context(&mut payload, true);
        match self.post_json("sendDocument", payload).await {
            Ok(response) => Ok(response),
            Err(error)
                if telegram_error_allows_media_upload_fallback(&error)
                    && (document.starts_with("http://") || document.starts_with("https://")) =>
            {
                let Some((bytes, mime_type, file_name)) = self.download_media_bytes(document, MAX_TELEGRAM_DOWNLOAD_BYTES).await else {
                    return Self::terminal_delivery_error(error);
                };
                let mut fields = vec![("chat_id".to_string(), chat_id.to_string())];
                if let Some(value) = caption { fields.push(("caption".to_string(), value.to_string())); }
                if let Some(value) = parse_mode { fields.push(("parse_mode".to_string(), value.to_string())); }
                if let Some(value) = reply_markup { fields.push(("reply_markup".to_string(), value.to_string())); }
                fields.extend(multipart_delivery_fields(&Self::current_delivery_context(), true, None, reply_to_message_id)?);
                match self.post_multipart("sendDocument", fields, vec![MultipartFileSpec {
                    field: "document".to_string(), bytes, file_name, mime_type,
                }]).await {
                    Ok(response) => Ok(response),
                    Err(error) => Self::terminal_delivery_error(error),
                }
            }
            Err(error) => Self::terminal_delivery_error(error),
        }
    }'''
    replace_media_method("send_document", 'pub async fn edit_message_text', document)

    # sendPhoto direct path: only use upload fallback for deterministic URL-fetch 400s.
    text = read("src/bot/client.rs")
    old = '''                if photo.starts_with("http://") || photo.starts_with("https://") {
                    info!("sendPhoto direct URL failed ({e}); attempting download-and-upload...");'''
    new = '''                if telegram_error_allows_media_upload_fallback(&e)
                    && (photo.starts_with("http://") || photo.starts_with("https://"))
                {
                    info!("sendPhoto direct URL failed ({e}); attempting download-and-upload...");'''
    if old not in text:
        raise SystemExit("sendPhoto fallback gate pattern missing")
    text = text.replace(old, new, 1)
    text = text.replace('                Err(e)\n            }\n        }\n    }\n\n    pub async fn send_media_group', '                Self::terminal_delivery_error(e)\n            }\n        }\n    }\n\n    pub async fn send_media_group', 1)
    write("src/bot/client.rs", text)


def fix_client_rich_and_ephemeral() -> None:
    # Draft: retry 429/5xx in raw transport, only degrade deterministic rich incompatibility.
    text = read("src/bot/client.rs")
    old = '''        if !is_ok {
            if res.get("error_code").and_then(|v| v.as_i64()) == Some(429) {
                return Ok(res);
            }
            // Fallback to sendMessageDraft
            let mut fallback_text = "Thinking...".to_string();'''
    new = '''        if !is_ok {
            if !should_degrade_rich_response(&res) {
                return Err(Self::telegram_api_error("sendRichMessageDraft", &res));
            }
            // Fallback to sendMessageDraft only for deterministic Rich incompatibility.
            let mut fallback_text = "Thinking...".to_string();'''
    if old not in text:
        raise SystemExit("sendRichMessageDraft fallback block missing")
    text = text.replace(old, new, 1)
    write("src/bot/client.rs", text)

    rich_fn = r'''    pub async fn send_rich_message(
        &self,
        chat_id: i64,
        rich_message: &InputRichMessage,
        reply_markup: Option<Value>,
        receiver_user_id: Option<i64>,
    ) -> Result<Value, String> {
        let failure_checkpoint = Self::terminal_delivery_failed();
        let validation = rich_message.validate();
        if validation.is_ok() {
            let rich_json = serde_json::to_value(rich_message).map_err(|error| error.to_string())?;
            let mut payload = json!({
                "chat_id": chat_id,
                "draft_id": 0,
                "rich_message": rich_json,
            });
            if let Some(reply_markup) = reply_markup.as_ref() {
                payload["reply_markup"] = reply_markup.clone();
            }
            if let Some(receiver_user_id) = receiver_user_id {
                payload["ephemeral_message_parameters"] = serde_json::to_value(
                    EphemeralMessageParameters {
                        receiver_user_id,
                        callback_query_id: Self::current_delivery_context().callback_query_id,
                        replace_callback_query_message: None,
                    },
                )
                .unwrap_or(json!({}));
            }
            Self::apply_delivery_context(&mut payload, true);

            match self.post_json_raw("sendRichMessage", payload).await {
                Ok(response) if response.get("ok").and_then(Value::as_bool) == Some(true) => {
                    Self::restore_delivery_failure(failure_checkpoint);
                    return Ok(response);
                }
                Ok(response) if should_degrade_rich_response(&response) => {
                    info!("Telegram rejected Rich Message; degrading to safe HTML.");
                }
                Ok(response) => {
                    return Self::terminal_delivery_error(Self::telegram_api_error(
                        "sendRichMessage",
                        &response,
                    ));
                }
                Err(error) => return Self::terminal_delivery_error(error),
            }
        } else if let Err(error) = validation {
            if rich_message.blocks.is_empty() {
                return Self::terminal_delivery_error(error);
            }
            info!("Rich Message validation required degradation: {error}");
        }

        let html_chunks = self.render_blocks_to_html_chunks(&rich_message.blocks, 3800);
        let total = html_chunks.len();
        let mut html_last = json!({ "ok": true });
        let mut html_failed = false;
        for (index, chunk) in html_chunks.into_iter().enumerate() {
            let is_last = index + 1 == total;
            match self
                .send_message(
                    chat_id,
                    &chunk,
                    Some("HTML"),
                    if is_last { reply_markup.clone() } else { None },
                    receiver_user_id,
                    None,
                )
                .await
            {
                Ok(response) => html_last = response,
                Err(error) => {
                    info!("HTML fallback failed ({error}); degrading to semantic plain text.");
                    html_failed = true;
                    break;
                }
            }
        }
        if !html_failed {
            Self::restore_delivery_failure(failure_checkpoint);
            return Ok(html_last);
        }

        let plain_chunks = self.render_blocks_to_plain_chunks(&rich_message.blocks, 4000);
        let total = plain_chunks.len();
        let mut plain_last = json!({ "ok": true });
        for (index, chunk) in plain_chunks.into_iter().enumerate() {
            let is_last = index + 1 == total;
            match self
                .send_message(
                    chat_id,
                    &chunk,
                    None,
                    if is_last { reply_markup.clone() } else { None },
                    receiver_user_id,
                    None,
                )
                .await
            {
                Ok(response) => plain_last = response,
                Err(error) => return Self::terminal_delivery_error(error),
            }
        }
        Self::restore_delivery_failure(failure_checkpoint);
        Ok(plain_last)
    }
'''
    regex_once(
        "src/bot/client.rs",
        r'    pub async fn send_rich_message\(.*?\n    \}\n\n    pub async fn set_my_commands',
        rich_fn + '\n    pub async fn set_my_commands',
    )

    # editRich should not degrade transient/auth failures to a second API method.
    text = read("src/bot/client.rs")
    anchor = '''        if res
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase().contains("message is not modified"))
            .unwrap_or(false)
        {
            return Ok(res);
        }

        // Fallback to editMessageText with HTML rendering'''
    repl = '''        if res
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_ascii_lowercase().contains("message is not modified"))
            .unwrap_or(false)
        {
            return Ok(res);
        }
        if !should_degrade_rich_response(&res) {
            return Self::terminal_delivery_error(Self::telegram_api_error(method, &res));
        }

        // Fallback to editMessageText with HTML rendering'''
    if anchor not in text:
        raise SystemExit("edit_rich_message degradation anchor missing")
    text = text.replace(anchor, repl, 1)
    write("src/bot/client.rs", text)

    # New-file upload support for editEphemeralMessageMedia.
    anchor = '''    #[allow(clippy::too_many_arguments)]
    pub async fn edit_ephemeral_message_caption('''
    method = r'''    pub async fn edit_ephemeral_message_media_bytes(
        &self,
        chat_id: i64,
        receiver_user_id: i64,
        ephemeral_message_id: i64,
        media: &InputMedia,
        bytes: Vec<u8>,
        file_name: &str,
        mime_type: &str,
        reply_markup: Option<Value>,
    ) -> Result<Value, String> {
        let mut media = media.clone();
        set_input_media_source(&mut media, "attach://media_file".to_string());
        let mut fields = vec![
            ("chat_id".to_string(), chat_id.to_string()),
            ("receiver_user_id".to_string(), receiver_user_id.to_string()),
            ("ephemeral_message_id".to_string(), ephemeral_message_id.to_string()),
            (
                "media".to_string(),
                serde_json::to_string(&media).map_err(|error| error.to_string())?,
            ),
        ];
        if let Some(reply_markup) = reply_markup {
            fields.push(("reply_markup".to_string(), reply_markup.to_string()));
        }
        match self
            .post_multipart(
                "editEphemeralMessageMedia",
                fields,
                vec![MultipartFileSpec {
                    field: "media_file".to_string(),
                    bytes,
                    file_name: file_name.to_string(),
                    mime_type: mime_type.to_string(),
                }],
            )
            .await
        {
            Ok(response) => Ok(response),
            Err(error) => Self::terminal_delivery_error(error),
        }
    }

'''
    replace_once("src/bot/client.rs", anchor, method + anchor)

    # Rich multipart: central bounded/retrying multipart transport + delivery context.
    rich_media_fn = r'''    pub async fn send_rich_message_with_media(
        &self,
        chat_id: i64,
        rich_message: &InputRichMessage,
        attached_files: Vec<(String, Vec<u8>, String)>,
        reply_markup: Option<Value>,
        receiver_user_id: Option<i64>,
    ) -> Result<Value, String> {
        if attached_files.is_empty() {
            return self
                .send_rich_message(chat_id, rich_message, reply_markup, receiver_user_id)
                .await;
        }
        rich_message.validate()?;
        let mut fields = vec![
            ("chat_id".to_string(), chat_id.to_string()),
            (
                "rich_message".to_string(),
                serde_json::to_string(rich_message).map_err(|error| error.to_string())?,
            ),
        ];
        if let Some(reply_markup) = reply_markup {
            fields.push(("reply_markup".to_string(), reply_markup.to_string()));
        }
        fields.extend(multipart_delivery_fields(
            &Self::current_delivery_context(),
            true,
            receiver_user_id,
            None,
        )?);
        let files = attached_files
            .into_iter()
            .map(|(field, bytes, mime_type)| MultipartFileSpec {
                file_name: field.clone(),
                field,
                bytes,
                mime_type,
            })
            .collect();
        match self.post_multipart("sendRichMessage", fields, files).await {
            Ok(response) => Ok(response),
            Err(error) => Self::terminal_delivery_error(error),
        }
    }
'''
    regex_once(
        "src/bot/client.rs",
        r'    pub async fn send_rich_message_with_media\(.*?\n    \}\n\n    pub async fn delete_message',
        rich_media_fn + '\n    pub async fn delete_message',
    )

    # Successful callback acknowledgement is tracked so malformed/stale branches get one fallback ack.
    old = '''        self.post_json("answerCallbackQuery", payload).await
    }

    pub async fn send_chat_action'''
    new = '''        let result = self.post_json("answerCallbackQuery", payload).await;
        if result.is_ok() {
            Self::mark_callback_answered();
        }
        result
    }

    pub async fn send_chat_action'''
    replace_once("src/bot/client.rs", old, new)


def fix_main_delivery_checkpoint() -> None:
    old = '''    let delivery_context = delivery_context_for_update(&update);
    TelegramBotClient::with_delivery_context(
        delivery_context,
        handle_update(bot, ai_service, user_last_image_prompt, access, update),
    )
    .await;

    if !ai::storage::mark_telegram_processed_async(update_id).await {
        warn!("Gagal menyelesaikan durable Telegram inbox update {update_id}");
    }'''
    new = '''    let callback_query_id = update.callback_query.as_ref().map(|query| query.id.clone());
    let delivery_context = delivery_context_for_update(&update);
    let operation = async {
        let (_, callback_answered) = TelegramBotClient::with_callback_answer_tracking(
            handle_update(bot, ai_service, user_last_image_prompt, access, update),
        )
        .await;
        if let Some(callback_query_id) = callback_query_id {
            if !callback_answered {
                let _ = bot
                    .answer_callback_query(
                        &callback_query_id,
                        Some("Aksi sudah kedaluwarsa atau tidak valid. Buka menu lagi."),
                        false,
                    )
                    .await;
            }
        }
    };
    let (_, terminal_delivery_failed) = TelegramBotClient::with_delivery_tracking(
        TelegramBotClient::with_delivery_context(delivery_context, operation),
    )
    .await;

    if terminal_delivery_failed {
        let error = "terminal Telegram outbound delivery failed after bounded retries".to_string();
        if !ai::storage::mark_telegram_delivery_failed_async(update_id, error).await {
            warn!("Gagal menyimpan status delivery_failed untuk Telegram update {update_id}");
        }
        return;
    }

    if !ai::storage::mark_telegram_processed_async(update_id).await {
        warn!("Gagal menyelesaikan durable Telegram inbox update {update_id}");
    }'''
    replace_once("src/main.rs", old, new)


def fix_cli_secrets() -> None:
    replace_once(
        "Cargo.toml",
        'crossterm = "0.27"\n',
        'crossterm = "0.27"\nrpassword = "7.3"\n',
    )
    text = read("src/cli.rs")
    pattern = re.compile(
        r'let mut (?P<var>[A-Za-z_]*(?:key|token)[A-Za-z_]*) = String::new\(\);\n(?P<indent>\s*)let _ = (?:reader|stdin)\.read_line\(&mut (?P=var)\);',
        re.I,
    )
    text, count = pattern.subn(
        lambda m: f'let {m.group("var")} = rpassword::read_password().unwrap_or_default();',
        text,
    )
    if count < 1:
        raise SystemExit("src/cli.rs: no key/token read_line secret input patterns replaced")
    write("src/cli.rs", text)


def fix_readme() -> None:
    text = read("README.md")
    old = '''- Telegram intake is durable **at-least-once** processing, not exactly-once. A claimed update keeps its payload until the completed checkpoint; startup returns abandoned `processing` rows to `pending`. Completed tombstones deduplicate Telegram redelivery. A crash after an external side effect but before the completion checkpoint can still repeat that effect, so the documentation intentionally does not claim exactly-once side effects.'''
    new = '''- Telegram intake is durable **at-least-once** processing, not exactly-once. A claimed update keeps its payload until the completed checkpoint; startup returns abandoned `processing` rows to `pending`. Completed tombstones deduplicate Telegram redelivery. Terminal outbound message/edit/delete failures are retried with bounded Telegram-aware backoff first; if delivery still fails, the inbox row is retained as `delivery_failed` instead of being falsely checkpointed as completed. A crash after an external side effect but before the completion checkpoint can still repeat that effect because Bot API send methods do not expose an application idempotency key, so the documentation intentionally does not claim exactly-once side effects.'''
    if old not in text:
        raise SystemExit("README reliability paragraph missing")
    text = text.replace(old, new, 1)
    retry_old = '''- Provider retry policy covers transient connect/timeout, HTTP 408/429/502/503/504, and honors `Retry-After`. Mid-stream interruption is preserved as a partial result rather than retried blindly.'''
    retry_new = retry_old + '''\n- Telegram JSON and multipart transports use bounded retries for connect/timeout, Bot API `429`, and `5xx`; `parameters.retry_after` is honored with a safety cap. Rich-to-HTML/plain and URL-to-upload degradation is reserved for deterministic compatibility/fetch failures rather than auth, rate-limit, server, or transport failures.'''
    if retry_old not in text:
        raise SystemExit("README retry paragraph missing")
    text = text.replace(retry_old, retry_new, 1)
    scope_anchor = '''xiaochat v0.3.0 memakai subset Telegram Bot API 10.3 yang relevan untuk UI/AI flow:\n'''
    if scope_anchor in text and "Community/subscription surfaces remain intentionally out of scope" not in text:
        text = text.replace(
            scope_anchor,
            scope_anchor + "\n> Community add/remove operations and `BotSubscriptionUpdated` / `Update.subscription` remain intentionally out of scope until XiaoAI needs those product surfaces.\n",
            1,
        )
    write("README.md", text)


def apply_fixes() -> None:
    fix_models()
    fix_stream()
    fix_storage()
    fix_client_helpers()
    fix_client_transport()
    replace_client_media_functions()
    fix_client_rich_and_ephemeral()
    fix_main_delivery_checkpoint()
    fix_cli_secrets()
    fix_readme()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=["tests", "fixes"])
    args = parser.parse_args()
    if args.phase == "tests":
        add_tests()
    else:
        apply_fixes()
