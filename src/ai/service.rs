#![allow(dead_code)]

use chrono::Local;
use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Mutex, RwLock};
use tracing::{error, warn};

use crate::attachments::{
    decode_user_content, delete_session_attachments, encode_user_content, load_attachment,
    persist_attachment,
};
use crate::util::{truncate_chars, truncate_chars_with_ellipsis};

use super::http::{is_retryable_status, retry_delay, MAX_PROVIDER_ATTEMPTS};
use super::stream::{SseDecoder, StreamEvent};
use crate::timeline::{ExecutionTimeline, ProgressActivity, ProgressState};

use super::capability::model_metadata_key;
pub use super::capability::{ModelCapability, ModelMetadata};

use super::storage::{
    allocate_session_id_db_async, append_session_messages_db_async, delete_session_db_async,
    ensure_session_identity_v2_db_async, load_active_session_id_db_async, load_app_setting_async,
    load_sessions_db_async, replace_session_messages_db_async, save_active_session_db_async,
    save_session_metadata_db_async,
};
pub use super::storage::{
    load_app_setting, load_capability_registry, load_provider_store, save_app_setting,
    save_provider_store, CapabilityRecord, CapabilityRegistry, ChatMessage, ChatSession,
    ProviderConfig, ProviderStore,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMessageItem {
    pub index: usize,
    pub role: String,
    pub preview: String,
    pub chars: usize,
    pub tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextStats {
    pub session_name: String,
    pub session_id: usize,
    pub created_at: String,
    pub model_name: String,
    pub capabilities: ModelCapability,
    pub limit_tokens: usize,
    pub limit_str: String,
    pub total_messages: usize,
    pub total_turns: usize,
    pub total_tokens: usize,
    pub total_chars: usize,
    pub usage_pct: f64,
    pub progress_bar: String,
    pub messages_breakdown: Vec<ContextMessageItem>,
}

fn estimate_text_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4).max(1)
}

fn estimate_stored_content_tokens(content: &Value) -> usize {
    if let Some(persisted) = decode_user_content(content) {
        let media_cost = persisted.attachments.len().saturating_mul(1_500);
        return estimate_text_tokens(&persisted.text).saturating_add(media_cost);
    }
    match content {
        Value::String(text) => estimate_text_tokens(text),
        value => estimate_text_tokens(&value.to_string()),
    }
}

const MAX_GENERATED_IMAGE_BYTES: usize = 20 * 1024 * 1024;

fn is_unsafe_remote_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
        }
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

fn validate_generated_image_bytes(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_GENERATED_IMAGE_BYTES {
        return Err("generated image exceeded XiaoAI byte limits".to_string());
    }
    let supported = bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP");
    if !supported {
        return Err("generated image response has an unsupported file signature".to_string());
    }
    Ok(())
}

async fn download_generated_image(url: &str) -> Result<Vec<u8>, String> {
    let parsed = url::Url::parse(url).map_err(|_| "provider returned an invalid image URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("provider image URL must use http or https".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "provider image URL has no host".to_string())?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "provider image URL has no usable port".to_string())?;

    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "provider image host could not be resolved".to_string())?
        .collect::<Vec<_>>();
    if resolved.is_empty() || resolved.iter().any(|addr| is_unsafe_remote_ip(addr.ip())) {
        return Err("provider image URL resolved to a blocked network address".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, resolved[0])
        .build()
        .map_err(|_| "failed to build bounded image downloader".to_string())?;
    let response = client
        .get(parsed)
        .send()
        .await
        .map_err(|_| "provider image download failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "provider image download returned status {}",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_GENERATED_IMAGE_BYTES as u64)
    {
        return Err("provider image exceeded XiaoAI byte limits".to_string());
    }
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("image/"))
    {
        return Err("provider image URL did not return an image content type".to_string());
    }

    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "provider image stream failed".to_string())?;
        if bytes.len().saturating_add(chunk.len()) > MAX_GENERATED_IMAGE_BYTES {
            return Err("provider image exceeded XiaoAI byte limits".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    validate_generated_image_bytes(&bytes)?;
    Ok(bytes)
}

fn provider_url(endpoint: &str, path: &str) -> String {
    format!(
        "{}/{}",
        endpoint.trim().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn max_output_tokens_for_model(model: &str) -> usize {
    let lower = model.to_ascii_lowercase();
    if lower.contains("claude") {
        64_000
    } else if lower.contains("gemini")
        || ["o1", "o3", "gpt-4o", "gpt-5", "sol", "terra", "luna"]
            .iter()
            .any(|needle| lower.contains(needle))
    {
        65_536
    } else {
        16_384
    }
}

type GenerationKey = (i64, i64);
type GenerationCancelSender = watch::Sender<bool>;
type ActiveGenerations = Arc<RwLock<HashMap<GenerationKey, GenerationCancelSender>>>;

pub struct GenerationInput<'a> {
    pub prompt: &'a str,
    pub timeline: Option<&'a Arc<ExecutionTimeline>>,
    pub image_bytes: Option<Vec<u8>>,
    pub document_images: Option<Vec<Vec<u8>>>,
    pub mime_type: Option<&'a str>,
    pub doc_text: Option<&'a str>,
    pub doc_name: Option<&'a str>,
    pub audio_bytes: Option<Vec<u8>>,
    pub audio_mime: Option<&'a str>,
    pub video_bytes: Option<Vec<u8>>,
    pub video_mime: Option<&'a str>,
    pub video_duration: Option<i32>,
}

#[derive(Clone)]
pub struct AIChatService {
    pub(super) client: Client,
    user_sessions: Arc<RwLock<HashMap<i64, Vec<ChatSession>>>>,
    active_session_id: Arc<RwLock<HashMap<i64, usize>>>,
    generation_locks: Arc<RwLock<HashMap<i64, Arc<Mutex<()>>>>>,
    session_locks: Arc<RwLock<HashMap<i64, Arc<Mutex<()>>>>>,
    active_generations: ActiveGenerations,
    pub user_waiting_rename: Arc<RwLock<HashMap<i64, usize>>>,
    pub user_rename_msg_id: Arc<RwLock<HashMap<i64, i64>>>,
    pub user_session_msg_id: Arc<RwLock<HashMap<i64, i64>>>,
    pub(super) provider_store: Arc<RwLock<ProviderStore>>,
    pub(super) capability_registry: Arc<RwLock<CapabilityRegistry>>,
    pub user_wizard_state: Arc<RwLock<HashMap<i64, HashMap<String, String>>>>,
    pub model_metadata: Arc<RwLock<HashMap<String, ModelMetadata>>>,
}

impl AIChatService {
    pub fn new() -> Self {
        for key in [
            "BOT_TOKEN",
            "AI_ENDPOINT",
            "AI_API_KEY",
            "AI_MODEL",
            "OWNER_USER_ID",
            "ALLOWED_CHAT_IDS",
            "IMAGE_FALLBACK_PROVIDER",
        ] {
            if load_app_setting(key).is_none() {
                if let Ok(value) = std::env::var(key) {
                    if !value.trim().is_empty() {
                        let _ = save_app_setting(key, &value);
                    }
                }
            }
        }
        let provider_store = load_provider_store();
        let capability_registry = load_capability_registry();
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            user_sessions: Arc::new(RwLock::new(HashMap::new())),
            active_session_id: Arc::new(RwLock::new(HashMap::new())),
            generation_locks: Arc::new(RwLock::new(HashMap::new())),
            session_locks: Arc::new(RwLock::new(HashMap::new())),
            active_generations: Arc::new(RwLock::new(HashMap::new())),
            user_waiting_rename: Arc::new(RwLock::new(HashMap::new())),
            user_rename_msg_id: Arc::new(RwLock::new(HashMap::new())),
            user_session_msg_id: Arc::new(RwLock::new(HashMap::new())),
            provider_store: Arc::new(RwLock::new(provider_store)),
            capability_registry: Arc::new(RwLock::new(capability_registry)),
            user_wizard_state: Arc::new(RwLock::new(HashMap::new())),
            model_metadata: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ==========================================
    // Provider Management
    // ==========================================
    //
    // XiaoAI is intentionally single-owner. Provider configuration therefore
    // has one owner-global source of truth instead of a pseudo multi-user cache
    // layered over global SQLite settings.

    async fn rehydrate_history_content(
        &self,
        user_id: i64,
        session_id: usize,
        value: &Value,
        capability: Option<&CapabilityRecord>,
    ) -> Value {
        let Some(persisted) = decode_user_content(value) else {
            return value.clone();
        };

        let mut text = persisted.text;
        let mut parts = Vec::new();
        let mut total_loaded = 0usize;
        for attachment in persisted.attachments {
            let allowed = match attachment.kind.as_str() {
                "image" | "document_page" => capability
                    .and_then(|record| record.supports_image)
                    .unwrap_or(false),
                "audio" => capability
                    .and_then(|record| record.supports_audio)
                    .unwrap_or(false),
                "video" => capability
                    .and_then(|record| record.supports_video)
                    .unwrap_or(false),
                _ => false,
            };
            if !allowed {
                text.push_str(&format!(
                    "\n[Attachment '{}' omitted because current model capability is unsupported/unknown.]",
                    attachment.name.as_deref().unwrap_or(&attachment.kind)
                ));
                continue;
            }

            let bytes = match load_attachment(user_id, session_id, &attachment).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    warn!("Unable to reload persisted attachment: {err}");
                    text.push_str("\n[Previously attached media is no longer available.]");
                    continue;
                }
            };
            total_loaded = total_loaded.saturating_add(bytes.len());
            if total_loaded > 12 * 1024 * 1024 {
                text.push_str(
                    "\n[Older attachments omitted because the history media budget was reached.]",
                );
                break;
            }

            use base64::Engine;
            match attachment.kind.as_str() {
                "image" | "document_page" => {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{encoded}", attachment.mime_type),
                            "detail": "auto"
                        }
                    }));
                }
                "audio" => {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                    let format = if attachment.mime_type.contains("ogg") {
                        "ogg"
                    } else if attachment.mime_type.contains("wav") {
                        "wav"
                    } else {
                        "mp3"
                    };
                    parts.push(json!({
                        "type": "input_audio",
                        "input_audio": {"data": encoded, "format": format}
                    }));
                }
                "video" => {
                    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": {
                            "url": format!("data:{};base64,{encoded}", attachment.mime_type)
                        }
                    }));
                }
                _ => {}
            }
        }

        if parts.is_empty() {
            Value::String(text)
        } else {
            let mut content = vec![json!({"type": "text", "text": text})];
            content.extend(parts);
            Value::Array(content)
        }
    }

    async fn session_lock(&self, user_id: i64) -> Arc<Mutex<()>> {
        if let Some(lock) = self.session_locks.read().await.get(&user_id).cloned() {
            return lock;
        }
        let mut locks = self.session_locks.write().await;
        locks
            .entry(user_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn get_sessions(&self, user_id: i64) -> Vec<ChatSession> {
        if let Some(list) = self.user_sessions.read().await.get(&user_id).cloned() {
            return list;
        }

        let init_lock = self.session_lock(user_id).await;
        let _guard = init_lock.lock().await;
        if let Some(list) = self.user_sessions.read().await.get(&user_id).cloned() {
            return list;
        }

        let mut existing = load_sessions_db_async(user_id).await;
        if existing.is_empty() {
            let Some(session_id) = allocate_session_id_db_async(user_id).await else {
                warn!("Session initialization deferred because durable ID allocation failed");
                return Vec::new();
            };
            let now_str = Local::now().format("%d %b %H:%M").to_string();
            let session = ChatSession {
                id: session_id,
                name: format!("Session {now_str}"),
                messages: Vec::new(),
                created_at: now_str,
            };
            if !save_session_metadata_db_async(user_id, session.clone()).await {
                warn!("Session initialization deferred because metadata persistence failed");
                return Vec::new();
            }
            existing.push(session);
        }
        let _ = ensure_session_identity_v2_db_async(user_id, existing.clone()).await;
        self.user_sessions
            .write()
            .await
            .insert(user_id, existing.clone());
        existing
    }

    pub async fn get_active_session_id(&self, user_id: i64) -> Option<usize> {
        let sessions = self.get_sessions(user_id).await;
        if sessions.is_empty() {
            return None;
        }
        if let Some(id) = self.active_session_id.read().await.get(&user_id).copied() {
            if sessions.iter().any(|session| session.id == id) {
                return Some(id);
            }
        }

        let stored_id = load_active_session_id_db_async(user_id)
            .await
            .filter(|id| sessions.iter().any(|session| session.id == *id))
            .unwrap_or(sessions[0].id);

        self.active_session_id
            .write()
            .await
            .insert(user_id, stored_id);
        let _ = save_active_session_db_async(user_id, stored_id).await;
        Some(stored_id)
    }

    pub async fn get_active_session_index(&self, user_id: i64) -> usize {
        let sessions = self.get_sessions(user_id).await;
        let Some(active_id) = self.get_active_session_id(user_id).await else {
            return 0;
        };
        sessions
            .iter()
            .position(|session| session.id == active_id)
            .unwrap_or(0)
    }

    pub async fn get_active_session(&self, user_id: i64) -> Option<ChatSession> {
        let sessions = self.get_sessions(user_id).await;
        let active_id = self.get_active_session_id(user_id).await?;
        sessions
            .iter()
            .find(|session| session.id == active_id)
            .cloned()
    }

    pub async fn create_new_session(
        &self,
        user_id: i64,
        custom_name: Option<&str>,
    ) -> Option<ChatSession> {
        let _ = self.get_sessions(user_id).await;
        let now_str = Local::now().format("%d %b %H:%M").to_string();
        let new_id = allocate_session_id_db_async(user_id).await?;
        let name = custom_name
            .map(|value| truncate_chars(value.trim(), 60))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("Session {now_str}"));
        let session = ChatSession {
            id: new_id,
            name,
            messages: Vec::new(),
            created_at: now_str,
        };

        if !save_session_metadata_db_async(user_id, session.clone()).await {
            return None;
        }
        {
            let mut sessions_map = self.user_sessions.write().await;
            sessions_map
                .entry(user_id)
                .or_default()
                .push(session.clone());
        }
        self.active_session_id.write().await.insert(user_id, new_id);
        let _ = save_active_session_db_async(user_id, new_id).await;
        Some(session)
    }

    pub async fn switch_session_by_id(&self, user_id: i64, session_id: usize) -> bool {
        let sessions = self.get_sessions(user_id).await;
        if sessions.iter().any(|session| session.id == session_id) {
            self.active_session_id
                .write()
                .await
                .insert(user_id, session_id);
            let _ = save_active_session_db_async(user_id, session_id).await;
            return true;
        }
        false
    }

    pub async fn switch_session(&self, user_id: i64, index: usize) -> bool {
        let sessions = self.get_sessions(user_id).await;
        let Some(session_id) = sessions.get(index).map(|session| session.id) else {
            return false;
        };
        self.switch_session_by_id(user_id, session_id).await
    }

    pub async fn remove_session_by_id(&self, user_id: i64, session_id: usize) -> bool {
        let Some(active_id) = self.get_active_session_id(user_id).await else {
            return false;
        };
        let sessions = self.get_sessions(user_id).await;
        if !sessions.iter().any(|session| session.id == session_id) {
            return false;
        }
        let replacement = if sessions.len() == 1 {
            let Some(replacement_id) = allocate_session_id_db_async(user_id).await else {
                warn!(
                    "Refusing to remove the last session because replacement ID allocation failed"
                );
                return false;
            };
            let now_str = Local::now().format("%d %b %H:%M").to_string();
            let replacement = ChatSession {
                id: replacement_id,
                name: format!("Session {now_str}"),
                messages: Vec::new(),
                created_at: now_str,
            };
            if !save_session_metadata_db_async(user_id, replacement.clone()).await {
                warn!("Refusing to remove the last session because replacement persistence failed");
                return false;
            }
            Some(replacement)
        } else {
            None
        };

        let new_active_id = {
            let mut sessions_map = self.user_sessions.write().await;
            let Some(list) = sessions_map.get_mut(&user_id) else {
                return false;
            };
            let Some(current_index) = list.iter().position(|session| session.id == session_id)
            else {
                return false;
            };
            let removed = list.remove(current_index);
            if let Some(replacement) = replacement.clone() {
                let replacement_id = replacement.id;
                list.push(replacement);
                replacement_id
            } else if removed.id == active_id || !list.iter().any(|session| session.id == active_id)
            {
                let target_index = current_index.min(list.len().saturating_sub(1));
                list[target_index].id
            } else {
                active_id
            }
        };

        let _ = delete_session_db_async(user_id, session_id).await;
        delete_session_attachments(user_id, session_id).await;
        self.active_session_id
            .write()
            .await
            .insert(user_id, new_active_id);
        let _ = save_active_session_db_async(user_id, new_active_id).await;
        true
    }

    pub async fn remove_session(&self, user_id: i64, index: usize) -> bool {
        let sessions = self.get_sessions(user_id).await;
        let Some(session_id) = sessions.get(index).map(|session| session.id) else {
            return false;
        };
        self.remove_session_by_id(user_id, session_id).await
    }

    pub async fn rename_session_by_id(
        &self,
        user_id: i64,
        session_id: usize,
        new_name: &str,
    ) -> bool {
        let name = truncate_chars(new_name.trim(), 60);
        if name.is_empty() {
            return false;
        }
        let updated = {
            let mut sessions_map = self.user_sessions.write().await;
            let Some(list) = sessions_map.get_mut(&user_id) else {
                return false;
            };
            let Some(session) = list.iter_mut().find(|session| session.id == session_id) else {
                return false;
            };
            session.name = name;
            session.clone()
        };
        save_session_metadata_db_async(user_id, updated).await
    }

    pub async fn rename_session(&self, user_id: i64, index: usize, new_name: &str) -> bool {
        let sessions = self.get_sessions(user_id).await;
        let Some(session_id) = sessions.get(index).map(|session| session.id) else {
            return false;
        };
        self.rename_session_by_id(user_id, session_id, new_name)
            .await
    }

    pub async fn clear_history(&self, user_id: i64) {
        let Some(active_id) = self.get_active_session_id(user_id).await else {
            return;
        };
        let cleared = {
            let mut sessions_map = self.user_sessions.write().await;
            sessions_map.get_mut(&user_id).and_then(|list| {
                list.iter_mut()
                    .find(|session| session.id == active_id)
                    .map(|session| {
                        session.messages.clear();
                        session.clone()
                    })
            })
        };
        if let Some(session) = cleared {
            let _ = replace_session_messages_db_async(user_id, session).await;
            delete_session_attachments(user_id, active_id).await;
        }
    }

    pub async fn generation_lock(&self, user_id: i64) -> Arc<Mutex<()>> {
        if let Some(lock) = self.generation_locks.read().await.get(&user_id).cloned() {
            return lock;
        }
        let mut locks = self.generation_locks.write().await;
        locks
            .entry(user_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn begin_generation(&self, chat_id: i64, draft_id: i64) -> watch::Receiver<bool> {
        let (sender, receiver) = watch::channel(false);
        self.active_generations
            .write()
            .await
            .insert((chat_id, draft_id), sender);
        receiver
    }

    pub async fn cancel_generation(&self, chat_id: i64, draft_id: i64) -> bool {
        let sender = self
            .active_generations
            .read()
            .await
            .get(&(chat_id, draft_id))
            .cloned();
        sender
            .map(|sender| sender.send(true).is_ok())
            .unwrap_or(false)
    }

    pub async fn end_generation(&self, chat_id: i64, draft_id: i64) {
        self.active_generations
            .write()
            .await
            .remove(&(chat_id, draft_id));
    }

    pub async fn cancel_all_generations(&self) {
        let senders: Vec<GenerationCancelSender> = self
            .active_generations
            .read()
            .await
            .values()
            .cloned()
            .collect();
        for sender in senders {
            let _ = sender.send(true);
        }
    }

    pub async fn get_context_stats(&self, user_id: i64) -> ContextStats {
        let active_sess = self
            .get_active_session(user_id)
            .await
            .unwrap_or(ChatSession {
                id: 0,
                name: "Storage unavailable".to_string(),
                messages: Vec::new(),
                created_at: "-".to_string(),
            });
        let active_model = self.get_user_model(user_id).await;
        let endpoint = self
            .get_active_provider(user_id)
            .await
            .map(|provider| provider.endpoint)
            .unwrap_or_default();
        let cap = self
            .resolved_model_capability(&endpoint, &active_model)
            .await;
        let limit_tokens = cap.context_limit;
        let limit_str = cap.context_str.clone();

        let mut total_chars = 0;
        let mut msg_stats = Vec::new();

        for (i, m) in active_sess.messages.iter().enumerate() {
            let c_str = match &m.content {
                Value::String(s) => s.clone(),
                value => decode_user_content(value)
                    .map(|persisted| {
                        if persisted.attachments.is_empty() {
                            persisted.text
                        } else {
                            format!(
                                "{}\n[{} persisted attachment(s)]",
                                persisted.text,
                                persisted.attachments.len()
                            )
                        }
                    })
                    .unwrap_or_else(|| value.to_string()),
            };
            let chars = c_str.chars().count();
            let toks = estimate_text_tokens(&c_str);
            total_chars += chars;

            let preview = truncate_chars_with_ellipsis(&c_str, 90);

            msg_stats.push(ContextMessageItem {
                index: i + 1,
                role: m.role.clone(),
                preview,
                chars,
                tokens: toks,
            });
        }

        let total_tokens = active_sess
            .messages
            .iter()
            .map(|message| estimate_stored_content_tokens(&message.content))
            .sum();
        let usage_pct = ((total_tokens as f64 / limit_tokens.max(1) as f64) * 100.0).min(100.0);

        let mut filled_blocks = (usage_pct / 10.0).floor() as usize;
        if usage_pct > 0.0 && filled_blocks == 0 {
            filled_blocks = 1;
        }
        filled_blocks = filled_blocks.min(10);
        let bar = format!(
            "{}{}",
            "█".repeat(filled_blocks),
            "░".repeat(10 - filled_blocks)
        );

        ContextStats {
            session_name: active_sess.name,
            session_id: active_sess.id,
            created_at: active_sess.created_at,
            model_name: active_model,
            capabilities: cap,
            limit_tokens,
            limit_str,
            total_messages: active_sess.messages.len(),
            total_turns: active_sess.messages.len().div_ceil(2),
            total_tokens,
            total_chars,
            usage_pct,
            progress_bar: bar,
            messages_breakdown: msg_stats,
        }
    }

    // ==========================================
    // Audio Transcription (Whisper)
    // ==========================================

    pub async fn transcribe_audio(
        &self,
        user_id: i64,
        audio_bytes: Vec<u8>,
        file_name: &str,
    ) -> (bool, Result<String, String>) {
        let provider =
            match self.get_active_provider(user_id).await {
                Some(p) if !p.endpoint.is_empty() => p,
                _ => return (
                    false,
                    Err(
                        "Provider belum dikonfigurasi. Silakan jalankan /provider terlebih dahulu."
                            .to_string(),
                    ),
                ),
            };

        let stt_url = provider_url(&provider.endpoint, "audio/transcriptions");
        let part = match Part::bytes(audio_bytes)
            .file_name(file_name.to_string())
            .mime_str("audio/ogg")
        {
            Ok(p) => p,
            Err(e) => return (false, Err(format!("Multipart part error: {e}"))),
        };

        let form = Form::new().part("file", part).text("model", "whisper-1");
        let mut req = self
            .client
            .post(&stt_url)
            .multipart(form)
            .timeout(Duration::from_secs(45));

        if !provider.api_key.is_empty()
            && !["none", "-", "no"]
                .iter()
                .any(|k| provider.api_key.eq_ignore_ascii_case(k))
        {
            req = req.header("Authorization", format!("Bearer {}", provider.api_key));
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.json::<Value>().await {
                        Ok(data) => {
                            if let Some(text) = data.get("text").and_then(|t| t.as_str()) {
                                if !text.trim().is_empty() {
                                    return (true, Ok(text.trim().to_string()));
                                }
                            }
                            (false, Err("Hasil transkripsi kosong.".to_string()))
                        }
                        Err(e) => (false, Err(format!("Invalid transcription JSON: {e}"))),
                    }
                } else if status.as_u16() == 404 {
                    (false, Err("ENDPOINT_NOT_SUPPORTED".to_string()))
                } else {
                    let err_txt = resp.text().await.unwrap_or_default();
                    (
                        false,
                        Err(format!(
                            "HTTP {}: {}",
                            status.as_u16(),
                            truncate_chars(&err_txt, 100).as_str()
                        )),
                    )
                }
            }
            Err(e) => (false, Err(format!("Audio transcription error: {e}"))),
        }
    }

    // ==========================================
    // Streaming Chat Completions & Reasoning
    // ==========================================

    pub async fn generate_response(
        &self,
        user_id: i64,
        input: GenerationInput<'_>,
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> (Option<String>, String, bool) {
        let GenerationInput {
            prompt,
            timeline,
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
        } = input;
        let provider = match self.get_active_provider(user_id).await {
            Some(p) if !p.endpoint.is_empty() => p,
            _ => {
                return (
                    None,
                    "👋 <b>Hi, selamat datang di XiaoAI!</b>\n\n⚠️ <i>AI Provider belum dikonfigurasi.</i>\nSilakan jalankan perintah <code>xiao provider add</code> di terminal.".to_string(),
                    false,
                );
            }
        };

        let user_m = self.get_user_model(user_id).await;
        let model = if !user_m.is_empty() {
            user_m
        } else if !provider.active_model.is_empty() {
            provider.active_model.clone()
        } else {
            provider
                .models
                .first()
                .cloned()
                .unwrap_or_else(|| "gpt-4o".to_string())
        };

        let capability = self.capability_record(&provider.endpoint, &model).await;
        if let Some(record) = capability.as_ref() {
            if document_images
                .as_ref()
                .is_some_and(|pages| !pages.is_empty())
                && record.supports_image == Some(false)
            {
                return (
                    None,
                    format!(
                        "Endpoint '{}' tidak mendukung vision yang diperlukan untuk membaca PDF scan pada model '{}'.",
                        provider.name, model
                    ),
                    false,
                );
            }
            if video_bytes.is_some() && record.supports_video == Some(false) {
                return (
                    None,
                    format!(
                        "Endpoint '{}' tidak mendukung input video untuk model '{}'.",
                        provider.name, model
                    ),
                    false,
                );
            }
            if audio_bytes.is_some() && record.supports_audio == Some(false) {
                return (
                    None,
                    format!(
                        "Endpoint '{}' tidak mendukung input audio untuk model '{}'.",
                        provider.name, model
                    ),
                    false,
                );
            }
        }

        let Some(active_sess) = self.get_active_session(user_id).await else {
            return (
                None,
                "Penyimpanan session sedang tidak tersedia. XiaoAI tidak akan membuat ID session sementara yang berisiko dipakai ulang. Coba lagi setelah storage pulih.".to_string(),
                false,
            );
        };
        let request_session_id = active_sess.id;

        let mut clean_prompt = prompt.trim().to_string();
        if let Some(doc) = doc_text {
            let d_name = doc_name.unwrap_or("Dokumen");
            let doc_header = format!("[Dokumen Terlampir: {d_name}]\n{}\n\n", doc.trim());
            clean_prompt = if clean_prompt.is_empty() {
                format!("{doc_header}Baca, analisis, dan jelaskan isi dokumen ini.")
            } else {
                format!("{doc_header}{clean_prompt}")
            };
        } else if document_images
            .as_ref()
            .is_some_and(|pages| !pages.is_empty())
            && clean_prompt.is_empty()
        {
            let d_name = doc_name.unwrap_or("PDF scan");
            clean_prompt = format!(
                "Baca dan analisis halaman hasil render dari dokumen '{d_name}'. Lakukan OCR visual pada teks yang terlihat dan jelaskan isi dokumen secara akurat."
            );
        } else if video_bytes.is_some() && clean_prompt.is_empty() {
            let dur_str = video_duration
                .map(|d| format!(" ({d} detik)"))
                .unwrap_or_default();
            clean_prompt = format!("Tonton dan analisis rekaman video ini{dur_str} secara mendalam. Jelaskan isi visual, alur peristiwa, teks di layar, dan suara di dalamnya.");
        } else if image_bytes.is_some() && clean_prompt.is_empty() {
            clean_prompt = "Jelaskan dan analisis gambar ini secara detail.".to_string();
        } else if audio_bytes.is_some() && clean_prompt.is_empty() {
            clean_prompt = "Dengarkan rekaman suara ini dan jawab pertanyaan atau instruksi di dalamnya secara lengkap.".to_string();
        }

        let resolved_capability = self
            .resolved_model_capability(&provider.endpoint, &model)
            .await;
        let metadata_max_completion_tokens = self
            .model_metadata
            .read()
            .await
            .get(&model_metadata_key(&provider.endpoint, &model))
            .and_then(|metadata| metadata.max_completion_tokens);
        let mut max_output_tokens = max_output_tokens_for_model(&model)
            .min(resolved_capability.context_limit.saturating_div(2).max(1));
        if let Some(limit) = metadata_max_completion_tokens.filter(|limit| *limit > 0) {
            max_output_tokens = max_output_tokens.min(limit);
        }
        let max_prompt_tokens = resolved_capability
            .context_limit
            .saturating_sub(max_output_tokens)
            .saturating_sub(2_048)
            .max(1);
        if estimate_text_tokens(&clean_prompt) > max_prompt_tokens {
            let max_chars = max_prompt_tokens.saturating_mul(4);
            clean_prompt = truncate_chars(&clean_prompt, max_chars);
            clean_prompt.push_str("\n\n[Input dipotong Xiao agar muat di context window model.]");
        }
        let enhanced_prompt = clean_prompt.clone();

        let reserved_tokens = max_output_tokens
            .saturating_add(estimate_text_tokens(&enhanced_prompt))
            .saturating_add(2_048);
        let history_budget = resolved_capability
            .context_limit
            .saturating_sub(reserved_tokens);

        let mut selected_history = Vec::new();
        let mut used_history_tokens = 0usize;
        for message in active_sess.messages.iter().rev().take(50) {
            let estimated = estimate_stored_content_tokens(&message.content).saturating_add(8);
            if !selected_history.is_empty()
                && used_history_tokens.saturating_add(estimated) > history_budget
            {
                break;
            }
            if estimated > history_budget && selected_history.is_empty() {
                continue;
            }
            used_history_tokens = used_history_tokens.saturating_add(estimated);
            selected_history.push(message);
        }
        selected_history.reverse();

        let mut history = Vec::with_capacity(selected_history.len());
        for message in selected_history {
            let content = if message.role == "user" {
                self.rehydrate_history_content(
                    user_id,
                    request_session_id,
                    &message.content,
                    capability.as_ref(),
                )
                .await
            } else {
                message.content.clone()
            };
            history.push(json!({ "role": message.role, "content": content }));
        }

        let mut messages = vec![json!({
            "role": "system",
            "content": "Kamu adalah asisten AI yang cerdas, komunikatif, dan ramah. \
                        Gunakan input multimodal hanya ketika input tersebut benar-benar disediakan dan endpoint/model mendukungnya. \
                        Dokumen Xiao diekstrak menjadi teks bila memungkinkan; PDF scan dapat diberikan sebagai halaman hasil render untuk OCR visual. \
                        Lakukan penalaran secara internal dan berikan hanya jawaban yang berguna bagi pengguna; jangan menampilkan chain-of-thought tersembunyi. \
                        Gunakan gaya bahasa yang alami dan format teks yang elegan. \
                        Jika membuat tabel atau data berkolom, gunakan Markdown Table standar agar Xiao dapat merendernya sebagai tabel Telegram."
        })];

        messages.extend(history);

        if let Some(pages) = document_images.as_ref().filter(|pages| !pages.is_empty()) {
            use base64::Engine;
            let mut content = vec![json!({ "type": "text", "text": enhanced_prompt })];
            for page in pages {
                let encoded = base64::engine::general_purpose::STANDARD.encode(page);
                content.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/png;base64,{encoded}"),
                        "detail": "high"
                    }
                }));
            }
            messages.push(json!({ "role": "user", "content": content }));
        } else if let Some(v_bytes) = video_bytes.as_ref() {
            use base64::Engine;
            let b64_vid = base64::engine::general_purpose::STANDARD.encode(v_bytes);
            let v_m = video_mime.unwrap_or("video/mp4");
            let data_url = format!("data:{v_m};base64,{b64_vid}");
            messages.push(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": enhanced_prompt },
                    { "type": "image_url", "image_url": { "url": data_url } }
                ]
            }));
        } else if let Some(i_bytes) = image_bytes.as_ref() {
            use base64::Engine;
            let b64_img = base64::engine::general_purpose::STANDARD.encode(i_bytes);
            let i_m = mime_type.unwrap_or("image/jpeg");
            let data_url = format!("data:{i_m};base64,{b64_img}");
            messages.push(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": enhanced_prompt },
                    { "type": "image_url", "image_url": { "url": data_url, "detail": "auto" } }
                ]
            }));
        } else if let Some(a_bytes) = audio_bytes.as_ref() {
            use base64::Engine;
            let b64_audio = base64::engine::general_purpose::STANDARD.encode(a_bytes);
            let fmt = if audio_mime.unwrap_or("").contains("ogg") {
                "ogg"
            } else {
                "mp3"
            };
            messages.push(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": enhanced_prompt },
                    { "type": "input_audio", "input_audio": { "data": b64_audio, "format": fmt } }
                ]
            }));
        } else {
            messages.push(json!({
                "role": "user",
                "content": enhanced_prompt
            }));
        }

        let url = provider_url(&provider.endpoint, "chat/completions");
        let mut payload = json!({
            "model": model,
            "messages": messages,
            "stream": true,
        });
        if metadata_max_completion_tokens.is_some() {
            payload["max_completion_tokens"] = json!(max_output_tokens);
        } else {
            payload["max_tokens"] = json!(max_output_tokens);
        }

        let use_auth = !provider.api_key.is_empty()
            && !["none", "-", "no"]
                .iter()
                .any(|k| provider.api_key.eq_ignore_ascii_case(k));

        let mut response = None;
        let mut terminal_transport_failure = false;
        for attempt in 0..MAX_PROVIDER_ATTEMPTS {
            let mut req = self
                .client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&payload)
                .timeout(Duration::from_secs(180));
            if use_auth {
                req = req.header("Authorization", format!("Bearer {}", provider.api_key));
            }

            let mut send_future = Box::pin(req.send());
            let send_result = tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        if let Some(tl) = timeline {
                            tl.fail_current("Stopped by user".to_string()).await;
                            tl.stop_ticker();
                        }
                        return (None, "⏹️ Generasi dihentikan oleh pengguna.".to_string(), true);
                    }
                    send_future.as_mut().await
                }
                result = send_future.as_mut() => result,
            };

            match send_result {
                Ok(resp)
                    if is_retryable_status(resp.status())
                        && attempt + 1 < MAX_PROVIDER_ATTEMPTS =>
                {
                    let status = resp.status();
                    let delay = retry_delay(resp.headers(), attempt);
                    let _ = resp.bytes().await;
                    warn!(
                        "Transient provider status {}; retrying attempt {}/{}",
                        status.as_u16(),
                        attempt + 2,
                        MAX_PROVIDER_ATTEMPTS
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        changed = cancel_rx.changed() => {
                            if changed.is_ok() && *cancel_rx.borrow() {
                                if let Some(tl) = timeline {
                                    tl.fail_current("Stopped by user".to_string()).await;
                                    tl.stop_ticker();
                                }
                                return (None, "⏹️ Generasi dihentikan oleh pengguna.".to_string(), true);
                            }
                        }
                    }
                }
                Ok(resp) => {
                    response = Some(resp);
                    break;
                }
                Err(e) => {
                    let retryable_transport = e.is_timeout() || e.is_connect();
                    if retryable_transport && attempt + 1 < MAX_PROVIDER_ATTEMPTS {
                        let delay =
                            Duration::from_millis(500_u64.saturating_mul(1_u64 << attempt.min(5)));
                        warn!(
                            "Transient provider transport failure; retrying attempt {}/{}",
                            attempt + 2,
                            MAX_PROVIDER_ATTEMPTS
                        );
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            changed = cancel_rx.changed() => {
                                if changed.is_ok() && *cancel_rx.borrow() {
                                    if let Some(tl) = timeline {
                                        tl.fail_current("Stopped by user".to_string()).await;
                                        tl.stop_ticker();
                                    }
                                    return (None, "⏹️ Generasi dihentikan oleh pengguna.".to_string(), true);
                                }
                            }
                        }
                    } else {
                        error!(
                            "Error sending AI completion request: {}",
                            if e.is_timeout() {
                                "timeout"
                            } else {
                                "transport failure"
                            }
                        );
                        terminal_transport_failure = true;
                        break;
                    }
                }
            }
        }

        let Some(resp) = response else {
            if let Some(tl) = timeline {
                tl.fail_current("Provider connection failed".to_string())
                    .await;
                tl.sync_draft(true).await;
            }
            return (
                None,
                if terminal_transport_failure {
                    "⚠️ Terjadi kendala saat memproses jawaban AI.".to_string()
                } else {
                    "⚠️ Provider tidak merespons setelah beberapa percobaan.".to_string()
                },
                false,
            );
        };

        if !resp.status().is_success() {
            let status_code = resp.status().as_u16();
            let _ = resp.text().await;
            error!("AI endpoint returned status {status_code}");
            if let Some(tl) = timeline {
                tl.fail_current(format!("API status {status_code}")).await;
                tl.sync_draft(true).await;
            }
            return (
                None,
                format!("⚠️ Gagal menghubungi AI proxy: {status_code}"),
                false,
            );
        }

        let mut stream = resp.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut accumulated_raw = String::new();
        let mut accumulated_reasoning = String::new();
        let mut has_started_answer = false;

        let mut cancelled = false;
        let mut stream_interrupted = false;
        let mut stream_done = false;
        while !stream_done {
            let mut next_future = Box::pin(stream.next());
            let next_item = tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_ok() && *cancel_rx.borrow() {
                        cancelled = true;
                        None
                    } else {
                        next_future.as_mut().await
                    }
                }
                item = next_future.as_mut() => item,
            };

            let Some(item) = next_item else {
                break;
            };
            let bytes = match item {
                Ok(bytes) => bytes,
                Err(_) => {
                    stream_interrupted = true;
                    warn!("AI response stream interrupted");
                    break;
                }
            };

            let events = match decoder.push(&bytes) {
                Ok(events) => events,
                Err(error) => {
                    stream_interrupted = true;
                    warn!("AI response SSE decode failed: {error}");
                    break;
                }
            };
            for event in events {
                match event {
                    StreamEvent::Done => {
                        stream_done = true;
                        break;
                    }
                    StreamEvent::Json(data) => {
                        let Some(delta) = data
                            .get("choices")
                            .and_then(|choices| choices.get(0))
                            .and_then(|choice| choice.get("delta"))
                        else {
                            continue;
                        };

                        if let Some(reasoning_chunk) =
                            delta.get("reasoning_content").and_then(Value::as_str)
                        {
                            accumulated_reasoning.push_str(reasoning_chunk);
                        }

                        let content_chunk =
                            delta.get("content").and_then(Value::as_str).unwrap_or("");
                        if content_chunk.is_empty() {
                            continue;
                        }
                        accumulated_raw.push_str(content_chunk);

                        let visible_partial =
                            if let Some(close_pos) = accumulated_raw.rfind("</think>") {
                                accumulated_raw[close_pos + "</think>".len()..].trim()
                            } else if accumulated_raw.contains("<think>") {
                                ""
                            } else {
                                accumulated_raw.trim()
                            };

                        if !visible_partial.is_empty() {
                            if !has_started_answer {
                                has_started_answer = true;
                                if let Some(tl) = timeline {
                                    tl.add_action("Writing", Some(ProgressActivity::Writing))
                                        .await;
                                }
                            }
                            if let Some(tl) = timeline {
                                tl.set_partial_answer(visible_partial).await;
                                tl.sync_draft(false).await;
                            }
                        }
                    }
                }
            }
        }

        if !stream_done && !cancelled && !stream_interrupted {
            match decoder.finish() {
                Ok(events) => {
                    for event in events {
                        if matches!(event, StreamEvent::Done) {
                            stream_done = true;
                        }
                    }
                    if !stream_done {
                        stream_interrupted = true;
                    }
                }
                Err(error) => {
                    warn!("AI response SSE final decode failed: {error}");
                    stream_interrupted = true;
                }
            }
        }

        // Post-process final output
        let mut thinking_text = None;
        let mut answer_text = accumulated_raw.clone();

        if !accumulated_reasoning.is_empty() {
            thinking_text = Some(accumulated_reasoning.trim().to_string());
            answer_text = accumulated_raw.trim().to_string();
        } else {
            let think_re = regex::Regex::new(r"(?s)<think>(.*?)</think>").unwrap();
            if let Some(caps) = think_re.captures(&accumulated_raw) {
                thinking_text = caps.get(1).map(|m| m.as_str().trim().to_string());
                answer_text = think_re
                    .replace_all(&accumulated_raw, "")
                    .trim()
                    .to_string();
            } else if accumulated_raw.contains("<think>") {
                let parts: Vec<&str> = accumulated_raw.split("<think>").collect();
                let before_think = parts[0].trim();
                let inside_think = parts.get(1).copied().unwrap_or("").trim();
                thinking_text = (!inside_think.is_empty()).then(|| inside_think.to_string());
                answer_text = before_think.to_string();
            }
        }

        let tag_clean_re = regex::Regex::new(r"(?i)</?think>").unwrap();
        answer_text = tag_clean_re
            .replace_all(&answer_text, "")
            .trim()
            .to_string();

        if cancelled {
            if answer_text.trim().is_empty() {
                answer_text = "⏹️ Generasi dihentikan oleh pengguna.".to_string();
            } else {
                answer_text.push_str("\n\n_⏹️ Generasi dihentikan oleh pengguna._");
            }
        } else if stream_interrupted {
            if answer_text.trim().is_empty() {
                answer_text = "⚠️ Stream provider terputus sebelum jawaban diterima.".to_string();
            } else {
                answer_text
                    .push_str("\n\n_⚠️ Stream provider terputus; jawaban mungkin tidak lengkap._");
            }
        } else if answer_text.is_empty() {
            answer_text = "Maaf, respon AI kosong untuk permintaan ini.".to_string();
        }

        if let Some(tl) = timeline {
            tl.set_partial_answer(&answer_text).await;
            if cancelled {
                tl.fail_current("Stopped by user".to_string()).await;
                tl.stop_ticker();
            } else if stream_interrupted {
                tl.fail_current("Provider stream interrupted".to_string())
                    .await;
                tl.sync_draft(true).await;
            } else {
                if !has_started_answer {
                    tl.add_action("Writing", Some(ProgressActivity::Writing))
                        .await;
                    tl.sync_draft(true).await;
                }
                tl.finish_all(ProgressState::Done).await;
            }
        }

        // Cancelled/interrupted output is presentation-only. Do not make a
        // partial answer canonical history: retry/follow-up context must only
        // see completed assistant turns.
        if cancelled || stream_interrupted {
            return (thinking_text, answer_text, cancelled);
        }

        // Persist only to the stable session that originated this request. Multimodal
        // attachments are stored outside SQLite and referenced from the user message so
        // follow-up turns can rehydrate the original media without bloating the database.
        let mut attachment_refs = Vec::new();
        if let Some(pages) = document_images.as_ref() {
            for (index, page) in pages.iter().enumerate() {
                let page_name = format!(
                    "{} page {}",
                    doc_name.unwrap_or("PDF scan"),
                    index.saturating_add(1)
                );
                match persist_attachment(
                    user_id,
                    request_session_id,
                    "document_page",
                    "image/png",
                    Some(&page_name),
                    page,
                )
                .await
                {
                    Ok(reference) => attachment_refs.push(reference),
                    Err(err) => warn!("Failed to persist rendered PDF page: {err}"),
                }
            }
        } else if let Some(bytes) = image_bytes.as_ref() {
            match persist_attachment(
                user_id,
                request_session_id,
                "image",
                mime_type.unwrap_or("image/jpeg"),
                None,
                bytes,
            )
            .await
            {
                Ok(reference) => attachment_refs.push(reference),
                Err(err) => warn!("Failed to persist image attachment: {err}"),
            }
        } else if let Some(bytes) = audio_bytes.as_ref() {
            match persist_attachment(
                user_id,
                request_session_id,
                "audio",
                audio_mime.unwrap_or("audio/ogg"),
                None,
                bytes,
            )
            .await
            {
                Ok(reference) => attachment_refs.push(reference),
                Err(err) => warn!("Failed to persist audio attachment: {err}"),
            }
        } else if let Some(bytes) = video_bytes.as_ref() {
            match persist_attachment(
                user_id,
                request_session_id,
                "video",
                video_mime.unwrap_or("video/mp4"),
                None,
                bytes,
            )
            .await
            {
                Ok(reference) => attachment_refs.push(reference),
                Err(err) => warn!("Failed to persist video attachment: {err}"),
            }
        }

        let user_message = ChatMessage {
            role: "user".to_string(),
            content: encode_user_content(&clean_prompt, attachment_refs),
        };
        let assistant_message = ChatMessage {
            role: "assistant".to_string(),
            content: Value::String(answer_text.clone()),
        };
        let appended = [user_message.clone(), assistant_message.clone()];
        let persisted_session = {
            let mut sessions_map = self.user_sessions.write().await;
            sessions_map.get_mut(&user_id).and_then(|list| {
                list.iter_mut()
                    .find(|session| session.id == request_session_id)
                    .map(|session| {
                        if session.messages.is_empty() && session.name.starts_with("Session ") {
                            let clean_title = prompt.trim().replace('\n', " ");
                            let short_title = truncate_chars_with_ellipsis(&clean_title, 32);
                            if !short_title.is_empty() {
                                session.name = short_title;
                            }
                        }
                        session.messages.extend(appended.iter().cloned());
                        session.clone()
                    })
            })
        };
        if let Some(session) = persisted_session {
            let _ = append_session_messages_db_async(user_id, session, appended.to_vec()).await;
        } else {
            warn!("Discarding late AI result for deleted session {request_session_id}");
        }

        (thinking_text, answer_text, cancelled)
    }

    // ==========================================
    // Image Generation (Custom Provider / FLUX.1 Fallback)
    // ==========================================

    pub async fn generate_image(
        &self,
        user_id: i64,
        prompt: &str,
        width: usize,
        height: usize,
    ) -> (bool, Option<Vec<u8>>, String) {
        let clean_prompt = prompt.trim();
        let provider = self.get_active_provider(user_id).await;

        // 1. Try Custom Provider /images/generations endpoint
        if let Some(ref p) = provider {
            if !p.endpoint.is_empty() {
                let gen_url = provider_url(&p.endpoint, "images/generations");
                let mut req = self
                    .client
                    .post(&gen_url)
                    .header("Content-Type", "application/json")
                    .json(&json!({
                        "prompt": clean_prompt,
                        "n": 1,
                        "size": format!("{width}x{height}"),
                        "response_format": "b64_json"
                    }))
                    .timeout(Duration::from_secs(45));

                if !p.api_key.is_empty()
                    && !["none", "-", "no"]
                        .iter()
                        .any(|k| p.api_key.eq_ignore_ascii_case(k))
                {
                    req = req.header("Authorization", format!("Bearer {}", p.api_key));
                }

                if let Ok(resp) = req.send().await {
                    if resp.status().is_success() {
                        if let Ok(res_json) = resp.json::<Value>().await {
                            if let Some(data) = res_json.get("data").and_then(|d| d.get(0)) {
                                if let Some(b64_str) = data.get("b64_json").and_then(|s| s.as_str())
                                {
                                    use base64::Engine;
                                    if let Ok(bytes) =
                                        base64::engine::general_purpose::STANDARD.decode(b64_str)
                                    {
                                        if validate_generated_image_bytes(&bytes).is_ok() {
                                            return (
                                                true,
                                                Some(bytes),
                                                format!("OpenAI Compatible ({})", p.name),
                                            );
                                        }
                                    }
                                } else if let Some(img_url) =
                                    data.get("url").and_then(|s| s.as_str())
                                {
                                    match download_generated_image(img_url).await {
                                        Ok(bytes) => {
                                            return (
                                                true,
                                                Some(bytes),
                                                format!("OpenAI Compatible ({})", p.name),
                                            );
                                        }
                                        Err(error) => {
                                            warn!("Rejected provider image URL: {error}");
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Optional external fallback. Disabled by default to avoid silently
        // sending user prompts to a provider they did not select.
        let fallback = match std::env::var("IMAGE_FALLBACK_PROVIDER") {
            Ok(value) => value,
            Err(_) => load_app_setting_async("IMAGE_FALLBACK_PROVIDER")
                .await
                .unwrap_or_else(|| "none".to_string()),
        };
        if !fallback.eq_ignore_ascii_case("pollinations") {
            return (
                false,
                None,
                "Provider aktif tidak menghasilkan gambar dan fallback eksternal dinonaktifkan. Set IMAGE_FALLBACK_PROVIDER=pollinations untuk opt-in.".to_string(),
            );
        }

        let encoded_prompt = urlencoding::encode(clean_prompt);
        let poll_url = format!(
            "https://image.pollinations.ai/prompt/{}?width={}&height={}&model=flux&nologo=true&enhance=true",
            encoded_prompt, width, height
        );

        match self
            .client
            .get(&poll_url)
            .timeout(Duration::from_secs(60))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.bytes().await {
                        Ok(bytes) if bytes.len() > 1000 => {
                            (true, Some(bytes.to_vec()), "FLUX.1 (Ultra HD)".to_string())
                        }
                        _ => (
                            false,
                            None,
                            "Respon gambar rusak atau terlalu kecil.".to_string(),
                        ),
                    }
                } else {
                    (
                        false,
                        None,
                        format!("HTTP Error {} saat membuat gambar.", status.as_u16()),
                    )
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    (
                        false,
                        None,
                        "Waktu generate gambar habis (Timeout). Silakan coba lagi.".to_string(),
                    )
                } else {
                    (false, None, format!("Gagal membuat gambar: {e}"))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: usize) -> ChatSession {
        ChatSession {
            id,
            name: format!("Session {id}"),
            messages: Vec::new(),
            created_at: "now".to_string(),
        }
    }

    #[test]
    fn legacy_active_index_maps_to_stable_session_id() {
        let sessions = vec![session(3), session(8), session(20)];
        assert_eq!(
            crate::ai::storage::legacy_active_session_id(Some(1), &sessions),
            Some(8)
        );
        assert_eq!(
            crate::ai::storage::legacy_active_session_id(Some(99), &sessions),
            Some(3)
        );
    }

    #[test]
    fn session_id_counter_never_reuses_deleted_high_water_mark() {
        assert_eq!(crate::ai::storage::compute_next_session_id(Some(21), 8), 21);
        assert_eq!(crate::ai::storage::compute_next_session_id(Some(4), 8), 9);
        assert_eq!(crate::ai::storage::compute_next_session_id(None, 8), 9);
    }
}
