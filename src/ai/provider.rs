use chrono::Local;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;
use tracing::warn;

use crate::util::truncate_chars;

const MAX_PROVIDER_METADATA_BYTES: usize = 8 * 1024 * 1024;

async fn read_bounded_provider_json(response: reqwest::Response) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROVIDER_METADATA_BYTES as u64)
    {
        return Err("provider metadata response exceeded XiaoAI limits".to_string());
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "provider metadata stream failed".to_string())?;
        if bytes.len().saturating_add(chunk.len()) > MAX_PROVIDER_METADATA_BYTES {
            return Err("provider metadata response exceeded XiaoAI limits".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid provider metadata JSON: {error}"))
}

async fn read_bounded_provider_text(response: reqwest::Response, max_bytes: usize) -> String {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(Ok(chunk)) = stream.next().await {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).unwrap_or_default()
}

use super::capability::{
    get_model_capabilities_with_meta, model_metadata_key, ModelCapability, ModelMetadata,
};
use super::service::AIChatService;
use super::storage::{
    load_provider_store, persist_capability_registry, persist_provider_state, CapabilityRecord,
    ProviderConfig,
};

#[derive(Debug)]
enum CapabilityProbeResponse {
    Success(Value),
    Rejected,
    Unknown,
}

fn assistant_text(body: &Value) -> Option<String> {
    let content = body
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?;

    if let Some(text) = content.as_str() {
        return Some(text.trim().to_string());
    }

    let parts = content.as_array()?;
    let text = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn validate_text_probe(response: &CapabilityProbeResponse) -> Option<bool> {
    match response {
        CapabilityProbeResponse::Rejected => Some(false),
        CapabilityProbeResponse::Unknown => None,
        CapabilityProbeResponse::Success(body) => assistant_text(body)
            .filter(|text| !text.is_empty())
            .map(|_| true),
    }
}

fn validate_tools_probe(response: &CapabilityProbeResponse) -> Option<bool> {
    match response {
        CapabilityProbeResponse::Rejected => Some(false),
        CapabilityProbeResponse::Unknown => None,
        CapabilityProbeResponse::Success(body) => {
            let tool_calls = body
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("tool_calls"))
                .and_then(Value::as_array);
            match tool_calls {
                Some(calls)
                    if calls.iter().any(|call| {
                        call.get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                            == Some("xiao_capability_probe")
                    }) =>
                {
                    Some(true)
                }
                _ => None,
            }
        }
    }
}

fn validate_structured_probe(response: &CapabilityProbeResponse) -> Option<bool> {
    match response {
        CapabilityProbeResponse::Rejected => Some(false),
        CapabilityProbeResponse::Unknown => None,
        CapabilityProbeResponse::Success(body) => assistant_text(body)
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| {
                (value.get("xiao_probe").and_then(Value::as_bool) == Some(true)).then_some(true)
            }),
    }
}

fn validate_color_probe(response: &CapabilityProbeResponse, expected: &str) -> Option<bool> {
    match response {
        CapabilityProbeResponse::Rejected => Some(false),
        CapabilityProbeResponse::Unknown => None,
        CapabilityProbeResponse::Success(body) => {
            let text = assistant_text(body)?;
            let normalized = text
                .trim()
                .trim_matches(|ch: char| !ch.is_ascii_alphabetic())
                .to_ascii_lowercase();
            (normalized == expected).then_some(true)
        }
    }
}

fn combine_vision_probe_results(first: Option<bool>, second: Option<bool>) -> Option<bool> {
    if first == Some(false) || second == Some(false) {
        Some(false)
    } else if first == Some(true) && second == Some(true) {
        Some(true)
    } else {
        None
    }
}

fn vision_probe_payload(model: &str, png_base64: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": "Return only the dominant color visible in the image as one lowercase English word."
                },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/png;base64,{png_base64}"),
                        "detail": "low"
                    }
                }
            ]
        }],
        "stream": false,
        "max_tokens": 8
    })
}

impl AIChatService {
    pub async fn reload_provider_store(&self) -> bool {
        let store = match tokio::task::spawn_blocking(load_provider_store).await {
            Ok(store) => store,
            Err(err) => {
                warn!("Failed to reload provider store: {err}");
                return false;
            }
        };
        *self.provider_store.write().await = store;
        true
    }

    pub async fn has_configured_provider(&self, _user_id: i64) -> bool {
        !self.provider_store.read().await.providers.is_empty()
    }

    pub async fn get_user_providers(&self, _user_id: i64) -> Vec<ProviderConfig> {
        self.provider_store.read().await.providers.clone()
    }

    pub async fn telegram_model_whitelist(&self) -> Vec<String> {
        self.provider_store.read().await.telegram_models.clone()
    }

    pub async fn get_active_provider(&self, _user_id: i64) -> Option<ProviderConfig> {
        let store = self.provider_store.read().await;
        store
            .active_id
            .as_deref()
            .and_then(|id| store.providers.iter().find(|provider| provider.id == id))
            .cloned()
            .or_else(|| store.providers.first().cloned())
    }

    pub async fn set_active_provider(&self, _user_id: i64, provider_id: &str) -> bool {
        let (candidate, selected_provider) = {
            let store = self.provider_store.read().await;
            let Some(provider) = store
                .providers
                .iter()
                .find(|provider| provider.id == provider_id)
                .cloned()
            else {
                return false;
            };
            let mut candidate = store.clone();
            candidate.active_id = Some(provider_id.to_string());
            (candidate, provider)
        };
        if !persist_provider_state(candidate.clone()).await {
            return false;
        }
        *self.provider_store.write().await = candidate;
        if !selected_provider.active_model.is_empty()
            && self
                .capability_record(&selected_provider.endpoint, &selected_provider.active_model)
                .await
                .is_none()
        {
            let _ = self
                .probe_model_capabilities(&selected_provider, &selected_provider.active_model)
                .await;
        }
        true
    }

    pub async fn update_provider_models(
        &self,
        _user_id: i64,
        provider_id: &str,
        models: Vec<String>,
    ) -> bool {
        let candidate = {
            let store = self.provider_store.read().await;
            let mut candidate = store.clone();
            let Some(provider) = candidate
                .providers
                .iter_mut()
                .find(|provider| provider.id == provider_id)
            else {
                return false;
            };
            provider.models = models;
            if !provider
                .models
                .iter()
                .any(|model| model == &provider.active_model)
            {
                provider.active_model = provider.models.first().cloned().unwrap_or_default();
            }
            candidate
        };
        if !persist_provider_state(candidate.clone()).await {
            return false;
        }
        *self.provider_store.write().await = candidate;
        true
    }

    pub async fn get_provider_model_by_index(
        &self,
        _user_id: i64,
        provider_id: &str,
        index: usize,
    ) -> Option<String> {
        let store = self.provider_store.read().await;
        store
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)?
            .models
            .get(index)
            .cloned()
    }

    pub async fn set_provider_model(
        &self,
        _user_id: i64,
        provider_id: &str,
        model_name: &str,
    ) -> bool {
        let (candidate, selected_provider) = {
            let store = self.provider_store.read().await;
            let mut candidate = store.clone();
            let Some(provider) = candidate
                .providers
                .iter_mut()
                .find(|provider| provider.id == provider_id)
            else {
                return false;
            };
            if !provider.models.is_empty()
                && !provider.models.iter().any(|model| model == model_name)
            {
                return false;
            }
            provider.active_model = model_name.to_string();
            let selected_provider = provider.clone();
            candidate.active_id = Some(provider_id.to_string());
            (candidate, selected_provider)
        };

        if !persist_provider_state(candidate.clone()).await {
            return false;
        }
        *self.provider_store.write().await = candidate;
        if self
            .capability_record(&selected_provider.endpoint, model_name)
            .await
            .is_none()
        {
            let _ = self
                .probe_model_capabilities(&selected_provider, model_name)
                .await;
        }
        true
    }

    async fn run_capability_probe_request(
        &self,
        provider: &ProviderConfig,
        payload: Value,
    ) -> CapabilityProbeResponse {
        let url = format!(
            "{}/chat/completions",
            provider.endpoint.trim_end_matches('/')
        );
        let mut req = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(Duration::from_secs(20));
        if !provider.api_key.is_empty()
            && !["none", "-", "no", "null"]
                .iter()
                .any(|value| provider.api_key.eq_ignore_ascii_case(value))
        {
            req = req.header("Authorization", format!("Bearer {}", provider.api_key));
        }

        let response = match req.send().await {
            Ok(response) => response,
            Err(_) => return CapabilityProbeResponse::Unknown,
        };
        if response.status().is_success() {
            return match read_bounded_provider_json(response).await {
                Ok(body) => CapabilityProbeResponse::Success(body),
                Err(_) => CapabilityProbeResponse::Unknown,
            };
        }
        match response.status().as_u16() {
            400 | 404 | 405 | 415 | 422 => CapabilityProbeResponse::Rejected,
            _ => CapabilityProbeResponse::Unknown,
        }
    }

    pub async fn probe_model_capabilities(
        &self,
        provider: &ProviderConfig,
        model: &str,
    ) -> CapabilityRecord {
        const RED_PNG: &str =
            "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP8z0AaYCJR/aiGUQ1DSAMAQC4BH2bjRnMAAAAASUVORK5CYII=";
        const BLUE_PNG: &str =
            "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAGUlEQVR4nGNkYPjPQApgIkn1qIZRDUNKAwA+MAEfWiW9ygAAAABJRU5ErkJggg==";

        let text_probe = self
            .run_capability_probe_request(
                provider,
                json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "Reply with exactly OK."}],
                    "stream": false,
                    "max_tokens": 4
                }),
            )
            .await;
        let text = validate_text_probe(&text_probe);

        let tools_probe = self
            .run_capability_probe_request(
                provider,
                json!({
                    "model": model,
                    "messages": [{
                        "role": "user",
                        "content": "Call the xiao_capability_probe function now. Do not answer with normal text."
                    }],
                    "stream": false,
                    "max_tokens": 16,
                    "tools": [{
                        "type": "function",
                        "function": {
                            "name": "xiao_capability_probe",
                            "description": "No-op capability probe",
                            "parameters": {
                                "type": "object",
                                "properties": {},
                                "additionalProperties": false
                            }
                        }
                    }],
                    "tool_choice": {
                        "type": "function",
                        "function": {"name": "xiao_capability_probe"}
                    }
                }),
            )
            .await;
        let tools = validate_tools_probe(&tools_probe);

        let structured_probe = self
            .run_capability_probe_request(
                provider,
                json!({
                    "model": model,
                    "messages": [{
                        "role": "user",
                        "content": "Return exactly this JSON object: {\"xiao_probe\":true}"
                    }],
                    "stream": false,
                    "max_tokens": 16,
                    "response_format": {"type": "json_object"}
                }),
            )
            .await;
        let structured = validate_structured_probe(&structured_probe);

        let red_probe = self
            .run_capability_probe_request(provider, vision_probe_payload(model, RED_PNG))
            .await;
        let blue_probe = self
            .run_capability_probe_request(provider, vision_probe_payload(model, BLUE_PNG))
            .await;
        let image = combine_vision_probe_results(
            validate_color_probe(&red_probe, "red"),
            validate_color_probe(&blue_probe, "blue"),
        );

        let provider_id = provider.endpoint.trim_end_matches('/').to_string();
        let metadata = self
            .model_metadata
            .read()
            .await
            .get(&model_metadata_key(&provider_id, model))
            .cloned();
        let modalities = metadata
            .as_ref()
            .and_then(|meta| meta.modalities.as_deref())
            .unwrap_or("")
            .to_ascii_lowercase();

        let record = CapabilityRecord {
            provider_id: provider_id.clone(),
            provider_name: provider.name.clone(),
            model: model.to_string(),
            context_window: metadata.as_ref().and_then(|meta| meta.context_length),
            supports_text: text,
            supports_image: image.or_else(|| {
                (!modalities.is_empty()).then(|| {
                    modalities.contains("image")
                        || modalities.contains("vision")
                        || modalities.contains("multimodal")
                })
            }),
            supports_audio: if modalities.is_empty() {
                None
            } else {
                Some(modalities.contains("audio"))
            },
            supports_video: if modalities.is_empty() {
                None
            } else {
                Some(modalities.contains("video"))
            },
            supports_reasoning: None,
            supports_tools: tools,
            supports_structured_output: structured,
            supports_file_input: if modalities.is_empty() {
                None
            } else {
                Some(modalities.contains("file") || modalities.contains("document"))
            },
            source: "active capability probe + provider metadata".to_string(),
            details: vec![
                format!("text={text:?}"),
                format!("vision={image:?}"),
                format!("tools={tools:?}"),
                format!("structured_output={structured:?}"),
                if modalities.is_empty() {
                    "modalities=unknown".to_string()
                } else {
                    format!("modalities={modalities}")
                },
            ],
            checked_at: Local::now().to_rfc3339(),
        };

        let candidate = {
            let registry = self.capability_registry.read().await;
            let mut candidate = registry.clone();
            if let Some(existing) = candidate
                .models
                .iter_mut()
                .find(|entry| entry.provider_id == provider_id && entry.model == model)
            {
                *existing = record.clone();
            } else {
                candidate.models.push(record.clone());
            }
            candidate
        };
        if persist_capability_registry(candidate.clone()).await {
            *self.capability_registry.write().await = candidate;
        } else {
            warn!("Capability probe result was not published because persistence failed");
        }
        record
    }

    pub async fn capability_record(&self, endpoint: &str, model: &str) -> Option<CapabilityRecord> {
        let endpoint = endpoint.trim_end_matches('/');
        self.capability_registry
            .read()
            .await
            .models
            .iter()
            .find(|record| record.provider_id == endpoint && record.model == model)
            .cloned()
    }

    pub async fn resolved_model_capability(&self, endpoint: &str, model: &str) -> ModelCapability {
        let metadata = self
            .model_metadata
            .read()
            .await
            .get(&model_metadata_key(endpoint, model))
            .cloned();
        let mut capability = get_model_capabilities_with_meta(model, metadata.as_ref());
        if let Some(record) = self.capability_record(endpoint, model).await {
            capability.vision = record.supports_image == Some(true);
            capability.vision_desc = match record.supports_image {
                Some(true) => "✅ Verified by provider metadata/probe".to_string(),
                Some(false) => "❌ Rejected by provider metadata/probe".to_string(),
                None => "⚪ Unknown: provider did not prove vision support".to_string(),
            };
            capability.audio = record.supports_audio == Some(true);
            capability.audio_desc = match record.supports_audio {
                Some(true) => "✅ Published/verified by provider".to_string(),
                Some(false) => "❌ Provider reports/rejects audio input".to_string(),
                None => "⚪ Unknown: audio capability not proven".to_string(),
            };
            capability.video = record.supports_video == Some(true);
            capability.video_desc = match record.supports_video {
                Some(true) => "✅ Published/verified by provider".to_string(),
                Some(false) => "❌ Provider reports/rejects video input".to_string(),
                None => "⚪ Unknown: video capability not proven".to_string(),
            };
            capability.thinking = record.supports_reasoning == Some(true);
            capability.thinking_desc = match record.supports_reasoning {
                Some(true) => "✅ Provider metadata/probe indicates reasoning support".to_string(),
                Some(false) => "❌ Reasoning mode not supported".to_string(),
                None => "⚪ Unknown: reasoning capability not probed".to_string(),
            };
        }
        capability.documents = true;
        capability.docs_desc = "✅ Xiao extractor: text/code, PDF, DOCX, XLSX; scanned PDF uses vision when renderer is available".to_string();
        capability
    }

    pub async fn fetch_models_from_endpoint(
        &self,
        endpoint: &str,
        api_key: &str,
    ) -> (bool, Result<Vec<String>, String>) {
        let clean_endpoint = endpoint.trim().trim_end_matches('/');
        let url = format!("{clean_endpoint}/models");

        let mut req = self.client.get(&url).timeout(Duration::from_secs(15));
        let trimmed_key = api_key.trim();
        if !trimmed_key.is_empty()
            && !["none", "-", "no", "null"]
                .iter()
                .any(|k| trimmed_key.eq_ignore_ascii_case(k))
        {
            req = req.header("Authorization", format!("Bearer {trimmed_key}"));
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match read_bounded_provider_json(resp).await {
                        Ok(data) => {
                            let mut model_ids = Vec::new();
                            let mut meta_guard = self.model_metadata.write().await;

                            if let Some(data_arr) = data.get("data").and_then(|d| d.as_array()) {
                                for item in data_arr {
                                    if let Some(id_str) = item.get("id").and_then(|s| s.as_str()) {
                                        model_ids.push(id_str.to_string());

                                        let context_length = item
                                            .get("context_length")
                                            .and_then(|c| c.as_u64())
                                            .map(|u| u as usize);
                                        let modality = item
                                            .get("architecture")
                                            .and_then(|a| a.get("modality"))
                                            .or_else(|| item.get("modalities"))
                                            .map(|value| match value {
                                                Value::String(s) => s.clone(),
                                                Value::Array(values) => values
                                                    .iter()
                                                    .filter_map(Value::as_str)
                                                    .collect::<Vec<_>>()
                                                    .join(","),
                                                _ => String::new(),
                                            })
                                            .filter(|value| !value.is_empty());
                                        let max_comp = item
                                            .get("top_provider")
                                            .and_then(|p| p.get("max_completion_tokens"))
                                            .and_then(|m| m.as_u64())
                                            .map(|u| u as usize);

                                        meta_guard.insert(
                                            model_metadata_key(clean_endpoint, id_str),
                                            ModelMetadata {
                                                id: id_str.to_string(),
                                                name: item
                                                    .get("name")
                                                    .and_then(|s| s.as_str())
                                                    .map(|s| s.to_string()),
                                                context_length,
                                                modalities: modality,
                                                max_completion_tokens: max_comp,
                                            },
                                        );
                                    } else if let Some(s) = item.as_str() {
                                        model_ids.push(s.to_string());
                                    }
                                }
                            } else if let Some(data_obj) =
                                data.get("data").and_then(|d| d.as_object())
                            {
                                for k in data_obj.keys() {
                                    model_ids.push(k.to_string());
                                }
                            }

                            let provider_id = clean_endpoint.to_string();
                            let registry_candidate = {
                                let registry = self.capability_registry.read().await;
                                let mut candidate = registry.clone();
                                for model_id in &model_ids {
                                    let meta = meta_guard
                                        .get(&model_metadata_key(clean_endpoint, model_id));
                                    let modalities = meta
                                        .and_then(|m| m.modalities.as_deref())
                                        .unwrap_or("")
                                        .to_ascii_lowercase();
                                    let record = CapabilityRecord {
                                        provider_id: provider_id.clone(),
                                        provider_name: provider_id.clone(),
                                        model: model_id.clone(),
                                        context_window: meta.and_then(|m| m.context_length),
                                        supports_text: Some(true),
                                        supports_image: if modalities.is_empty() {
                                            None
                                        } else {
                                            Some(
                                                modalities.contains("image")
                                                    || modalities.contains("vision")
                                                    || modalities.contains("multimodal"),
                                            )
                                        },
                                        supports_audio: if modalities.is_empty() {
                                            None
                                        } else {
                                            Some(modalities.contains("audio"))
                                        },
                                        supports_video: if modalities.is_empty() {
                                            None
                                        } else {
                                            Some(modalities.contains("video"))
                                        },
                                        supports_reasoning: None,
                                        supports_tools: None,
                                        supports_structured_output: None,
                                        supports_file_input: None,
                                        source: "provider /models metadata".to_string(),
                                        details: if modalities.is_empty() {
                                            vec!["Input modality tidak dipublikasikan endpoint"
                                                .to_string()]
                                        } else {
                                            vec![format!("modalities: {modalities}")]
                                        },
                                        checked_at: Local::now().to_rfc3339(),
                                    };
                                    if let Some(existing) =
                                        candidate.models.iter_mut().find(|entry| {
                                            entry.provider_id == provider_id
                                                && entry.model == *model_id
                                        })
                                    {
                                        existing.provider_name = record.provider_name;
                                        existing.context_window =
                                            record.context_window.or(existing.context_window);
                                        if existing.supports_image.is_none() {
                                            existing.supports_image = record.supports_image;
                                        }
                                        if existing.supports_audio.is_none() {
                                            existing.supports_audio = record.supports_audio;
                                        }
                                        if existing.supports_video.is_none() {
                                            existing.supports_video = record.supports_video;
                                        }
                                        existing.checked_at = record.checked_at;
                                        if !record.details.is_empty() {
                                            existing.details.extend(record.details);
                                            existing.details.sort();
                                            existing.details.dedup();
                                        }
                                        if !existing.source.contains("active capability probe") {
                                            existing.source = record.source;
                                        }
                                    } else {
                                        candidate.models.push(record);
                                    }
                                }
                                candidate
                            };
                            drop(meta_guard);
                            if persist_capability_registry(registry_candidate.clone()).await {
                                *self.capability_registry.write().await = registry_candidate;
                            } else {
                                warn!("Provider model capability metadata was not published because persistence failed");
                            }

                            if !model_ids.is_empty() {
                                (true, Ok(model_ids))
                            } else {
                                (false, Err("Endpoint berhasil dihubungi, namun tidak ada daftar model yang dikembalikan (data kosong).".to_string()))
                            }
                        }
                        Err(e) => (
                            false,
                            Err(format!("Respon dari endpoint bukan JSON valid: {e}")),
                        ),
                    }
                } else if status.as_u16() == 401 || status.as_u16() == 403 {
                    (false, Err(format!("HTTP {} Unauthorized: Autentikasi gagal. Mohon periksa kembali API Key Anda.", status.as_u16())))
                } else if status.as_u16() == 404 {
                    (false, Err(format!("HTTP 404 Not Found: Path /models tidak ditemukan di {clean_endpoint}. Pastikan format endpoint URL benar (misal: https://api.openai.com/v1).")))
                } else {
                    let err_text = read_bounded_provider_text(resp, 64 * 1024).await;
                    (
                        false,
                        Err(format!(
                            "HTTP {}: {}",
                            status.as_u16(),
                            truncate_chars(&err_text, 150).as_str()
                        )),
                    )
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    (
                        false,
                        Err(format!(
                            "Koneksi timeout setelah 15 detik ke {clean_endpoint}."
                        )),
                    )
                } else if e.is_connect() {
                    (false, Err(format!("Gagal terhubung ke {clean_endpoint}. Pastikan host/domain benar dan server aktif.")))
                } else {
                    (false, Err(format!("Koneksi gagal: {e}")))
                }
            }
        }
    }

    pub async fn get_user_model(&self, user_id: i64) -> String {
        self.get_active_provider(user_id)
            .await
            .map(|provider| provider.active_model)
            .filter(|model| !model.is_empty())
            .unwrap_or_else(|| "gpt-4o".to_string())
    }

    pub async fn set_user_model(&self, user_id: i64, model: &str) -> bool {
        let Some(provider) = self.get_active_provider(user_id).await else {
            return false;
        };
        self.set_provider_model(user_id, &provider.id, model).await
    }

    // ==========================================
    // Multi-Session Management
    // ==========================================
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with_message(message: Value) -> CapabilityProbeResponse {
        CapabilityProbeResponse::Success(json!({
            "choices": [{"message": message}]
        }))
    }

    #[test]
    fn successful_http_without_tool_call_does_not_prove_tools() {
        let response = response_with_message(json!({"content": "OK"}));
        assert_eq!(validate_tools_probe(&response), None);
    }

    #[test]
    fn named_tool_call_proves_tools() {
        let response = response_with_message(json!({
            "content": null,
            "tool_calls": [{
                "type": "function",
                "function": {"name": "xiao_capability_probe", "arguments": "{}"}
            }]
        }));
        assert_eq!(validate_tools_probe(&response), Some(true));
    }

    #[test]
    fn structured_probe_requires_expected_json_behavior() {
        let good = response_with_message(json!({"content": "{\"xiao_probe\":true}"}));
        let ignored = response_with_message(json!({"content": "sure"}));
        assert_eq!(validate_structured_probe(&good), Some(true));
        assert_eq!(validate_structured_probe(&ignored), None);
    }

    #[test]
    fn vision_probe_requires_two_demonstrated_colors() {
        let red = response_with_message(json!({"content": "red"}));
        let blue = response_with_message(json!({"content": "blue"}));
        assert_eq!(validate_color_probe(&red, "red"), Some(true));
        assert_eq!(validate_color_probe(&blue, "blue"), Some(true));
        assert_eq!(
            combine_vision_probe_results(
                validate_color_probe(&red, "red"),
                validate_color_probe(&blue, "blue")
            ),
            Some(true)
        );
    }

    #[test]
    fn explicit_probe_rejection_is_unsupported() {
        assert_eq!(
            validate_tools_probe(&CapabilityProbeResponse::Rejected),
            Some(false)
        );
        assert_eq!(
            validate_structured_probe(&CapabilityProbeResponse::Rejected),
            Some(false)
        );
    }
}
