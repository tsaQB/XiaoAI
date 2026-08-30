use base64::Engine;
use chrono::Local;
use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
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

fn tiny_silent_wav() -> Vec<u8> {
    const SAMPLE_RATE: u32 = 8_000;
    const SAMPLES: usize = 2_000;
    let data_len = (SAMPLES * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0);
    wav
}

use super::capability::{
    get_model_capabilities_with_meta, model_metadata_key, ModelCapability, ModelMetadata,
};
use super::routing::{ModelRole, ModelRoute, ResolvedModelRoute, RouteOrigin};
use super::service::AIChatService;
use super::storage::{
    load_provider_store, persist_capability_registry, persist_model_routing,
    persist_provider_state, CapabilityEvidence, CapabilityEvidenceSource, CapabilityKind,
    CapabilityRecord, CapabilityState, EvidenceFreshness, ProbeEvent, ProbeOutcome, ProviderConfig,
};

#[derive(Debug)]
enum CapabilityProbeResponse {
    Success(Value),
    Rejected,
    Unknown(ProbeOutcome),
}

impl CapabilityProbeResponse {
    fn outcome(&self, validator: Option<bool>) -> ProbeOutcome {
        match (self, validator) {
            (_, Some(true)) => ProbeOutcome::Supported,
            (Self::Rejected, _) | (_, Some(false)) => ProbeOutcome::Unsupported,
            (Self::Unknown(outcome), _) => *outcome,
            (Self::Success(_), None) => ProbeOutcome::Inconclusive,
        }
    }
}

fn capability_state(value: Option<bool>) -> CapabilityState {
    match value {
        Some(true) => CapabilityState::Supported,
        Some(false) => CapabilityState::Unsupported,
        None => CapabilityState::Unknown,
    }
}

fn catalog_presence_text_chat_claim() -> Option<bool> {
    // Being present in GET /models is catalog evidence only.
    None
}

fn normalized_modalities(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => (!value.trim().is_empty()).then(|| value.trim().to_string()),
        Value::Array(values) => {
            let joined = values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(",");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn normalize_provider_model_metadata(item: &Value) -> Option<ModelMetadata> {
    let id = item.get("id").and_then(Value::as_str)?.to_string();
    let context_length = item
        .get("context_length")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let modalities = item
        .get("architecture")
        .and_then(|architecture| architecture.get("modality"))
        .or_else(|| item.get("modalities"))
        .and_then(normalized_modalities);
    let max_completion_tokens = item
        .get("top_provider")
        .and_then(|provider| provider.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .map(|value| value as usize);

    Some(ModelMetadata {
        id,
        name: item.get("name").and_then(Value::as_str).map(str::to_string),
        context_length,
        modalities,
        max_completion_tokens,
    })
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
        CapabilityProbeResponse::Unknown(_) => None,
        CapabilityProbeResponse::Success(body) => assistant_text(body)
            .filter(|text| !text.is_empty())
            .map(|_| true),
    }
}

fn validate_endpoint_acceptance(response: &CapabilityProbeResponse) -> Option<bool> {
    match response {
        CapabilityProbeResponse::Rejected => Some(false),
        CapabilityProbeResponse::Unknown(_) => None,
        CapabilityProbeResponse::Success(_) => Some(true),
    }
}

fn validate_tools_probe(response: &CapabilityProbeResponse) -> Option<bool> {
    match response {
        CapabilityProbeResponse::Rejected => Some(false),
        CapabilityProbeResponse::Unknown(_) => None,
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
        CapabilityProbeResponse::Unknown(_) => None,
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
        CapabilityProbeResponse::Unknown(_) => None,
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

    pub async fn model_routing_config(&self) -> super::routing::ModelRoutingConfig {
        self.model_routing.read().await.clone()
    }

    pub async fn set_model_route(&self, role: ModelRole, route: ModelRoute) -> Result<(), String> {
        if role == ModelRole::Main {
            return Err(
                "Main Model is changed through `xiao model` or Telegram /model".to_string(),
            );
        }
        if let ModelRoute::Specific { provider_id, model } = &route {
            let store = self.provider_store.read().await;
            let provider = store
                .providers
                .iter()
                .find(|provider| &provider.id == provider_id)
                .ok_or_else(|| format!("Provider '{provider_id}' not found"))?;
            if provider.models.is_empty() || !provider.models.iter().any(|entry| entry == model) {
                return Err(format!(
                    "Model '{model}' is not present in provider '{}' catalog; refresh/probe the provider first",
                    provider.name
                ));
            }
        }

        let candidate = {
            let current = self.model_routing.read().await;
            let mut candidate = current.clone();
            candidate.set_route(role, route)?;
            candidate
        };
        if !persist_model_routing(candidate.clone()).await {
            return Err(
                "model routing persistence failed; runtime state was not changed".to_string(),
            );
        }
        *self.model_routing.write().await = candidate;
        Ok(())
    }

    pub async fn provider_route_dependencies(&self, provider_id: &str) -> Vec<ModelRole> {
        self.model_routing
            .read()
            .await
            .roles_using_provider(provider_id)
    }

    fn capability_ttl(record: &CapabilityRecord, capability: CapabilityKind) -> Duration {
        if record.evidence.iter().any(|evidence| {
            evidence.capability == capability
                && evidence.source == CapabilityEvidenceSource::ActiveProbe
        }) {
            Duration::from_secs(7 * 24 * 60 * 60)
        } else {
            Duration::from_secs(6 * 60 * 60)
        }
    }

    fn capability_is_fresh(record: &CapabilityRecord, capability: CapabilityKind) -> bool {
        record.freshness_for(capability, Self::capability_ttl(record, capability))
            == EvidenceFreshness::Fresh
    }

    fn required_capability_is_fresh(
        role: ModelRole,
        record: &CapabilityRecord,
        origin: RouteOrigin,
    ) -> bool {
        match role {
            ModelRole::Main => Self::capability_is_fresh(record, CapabilityKind::TextChat),
            ModelRole::Vision => Self::capability_is_fresh(record, CapabilityKind::ImageInput),
            ModelRole::Video => Self::capability_is_fresh(record, CapabilityKind::VideoInput),
            ModelRole::ImageGeneration => {
                Self::capability_is_fresh(record, CapabilityKind::ImageGeneration)
            }
            ModelRole::AudioStt if origin == RouteOrigin::MainModel => {
                let audio = record.state_for(CapabilityKind::AudioInput);
                let stt = record.state_for(CapabilityKind::AudioTranscription);
                (audio == CapabilityState::Supported
                    && Self::capability_is_fresh(record, CapabilityKind::AudioInput))
                    || (stt == CapabilityState::Supported
                        && Self::capability_is_fresh(record, CapabilityKind::AudioTranscription))
                    || (audio == CapabilityState::Unsupported
                        && stt == CapabilityState::Unsupported
                        && Self::capability_is_fresh(record, CapabilityKind::AudioInput)
                        && Self::capability_is_fresh(record, CapabilityKind::AudioTranscription))
            }
            ModelRole::AudioStt => {
                Self::capability_is_fresh(record, CapabilityKind::AudioTranscription)
            }
        }
    }

    fn required_capability_state(
        role: ModelRole,
        record: &CapabilityRecord,
        origin: RouteOrigin,
    ) -> CapabilityState {
        match role {
            ModelRole::Main => record.state_for(CapabilityKind::TextChat),
            ModelRole::Vision => record.state_for(CapabilityKind::ImageInput),
            ModelRole::Video => record.state_for(CapabilityKind::VideoInput),
            ModelRole::ImageGeneration => record.state_for(CapabilityKind::ImageGeneration),
            ModelRole::AudioStt if origin == RouteOrigin::MainModel => {
                if record.state_for(CapabilityKind::AudioInput) == CapabilityState::Supported
                    || record.state_for(CapabilityKind::AudioTranscription)
                        == CapabilityState::Supported
                {
                    CapabilityState::Supported
                } else if record.state_for(CapabilityKind::AudioInput)
                    == CapabilityState::Unsupported
                    && record.state_for(CapabilityKind::AudioTranscription)
                        == CapabilityState::Unsupported
                {
                    CapabilityState::Unsupported
                } else {
                    CapabilityState::Unknown
                }
            }
            ModelRole::AudioStt => record.state_for(CapabilityKind::AudioTranscription),
        }
    }

    pub async fn resolve_model_route_unchecked(
        &self,
        role: ModelRole,
    ) -> Result<ResolvedModelRoute, String> {
        let store = self.provider_store.read().await;
        let main_provider = store
            .active_id
            .as_deref()
            .and_then(|id| store.providers.iter().find(|provider| provider.id == id))
            .or_else(|| store.providers.first())
            .cloned()
            .ok_or_else(|| "No AI provider is configured".to_string())?;
        let main_model = main_provider.active_model.trim().to_string();
        if main_model.is_empty() {
            return Err("Main Model is not selected".to_string());
        }

        let (provider, model, route_origin) = if role == ModelRole::Main {
            (main_provider, main_model, RouteOrigin::Main)
        } else {
            let routing = self.model_routing.read().await;
            match routing
                .route(role)
                .cloned()
                .unwrap_or(ModelRoute::MainModel)
            {
                ModelRoute::MainModel => (main_provider, main_model, RouteOrigin::MainModel),
                ModelRoute::Disabled => return Err(format!("{} is Disabled", role.display_name())),
                ModelRoute::Specific { provider_id, model } => {
                    let provider = store
                        .providers
                        .iter()
                        .find(|provider| provider.id == provider_id)
                        .cloned()
                        .ok_or_else(|| format!("Provider '{provider_id}' not found"))?;
                    if provider.models.is_empty()
                        || !provider.models.iter().any(|entry| entry == &model)
                    {
                        return Err(format!(
                            "Model '{model}' is no longer present in provider '{}' catalog",
                            provider.name
                        ));
                    }
                    (provider, model, RouteOrigin::Specific)
                }
            }
        };
        drop(store);

        let capability = self
            .capability_record(&provider.endpoint, &model)
            .await
            .unwrap_or_else(|| CapabilityRecord {
                provider_id: provider.endpoint.trim_end_matches('/').to_string(),
                provider_name: provider.name.clone(),
                model: model.clone(),
                ..CapabilityRecord::default()
            });

        Ok(ResolvedModelRoute {
            provider,
            model,
            capability,
            route_origin,
        })
    }

    pub async fn resolve_model_route(&self, role: ModelRole) -> Result<ResolvedModelRoute, String> {
        let resolved = self.resolve_model_route_unchecked(role).await?;
        if resolved.capability.checked_at.is_empty()
            || !Self::required_capability_is_fresh(
                role,
                &resolved.capability,
                resolved.route_origin,
            )
        {
            return Err(format!(
                "{} capability is Unknown or stale for {} / {}; run a capability probe",
                role.display_name(),
                resolved.provider.name,
                resolved.model
            ));
        }
        match Self::required_capability_state(role, &resolved.capability, resolved.route_origin) {
            CapabilityState::Supported => Ok(resolved),
            CapabilityState::Unsupported => Err(format!(
                "{} is explicitly Unsupported by {} / {}",
                role.display_name(),
                resolved.provider.name,
                resolved.model
            )),
            CapabilityState::Unknown => Err(format!(
                "{} capability is Unknown for {} / {}; Xiao fails closed",
                role.display_name(),
                resolved.provider.name,
                resolved.model
            )),
        }
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
            if !provider.models.is_empty()
                && !provider.models.iter().any(|model| model == model_name)
            {
                return false;
            }
            provider.active_model = model_name.to_string();
            candidate.active_id = Some(provider_id.to_string());
            candidate
        };

        if !persist_provider_state(candidate.clone()).await {
            return false;
        }
        *self.provider_store.write().await = candidate;
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
            Err(error) if error.is_timeout() => {
                return CapabilityProbeResponse::Unknown(ProbeOutcome::Timeout)
            }
            Err(error) if error.is_connect() => {
                return CapabilityProbeResponse::Unknown(ProbeOutcome::NetworkError)
            }
            Err(_) => return CapabilityProbeResponse::Unknown(ProbeOutcome::NetworkError),
        };
        if response.status().is_success() {
            return match read_bounded_provider_json(response).await {
                Ok(body) => CapabilityProbeResponse::Success(body),
                Err(_) => CapabilityProbeResponse::Unknown(ProbeOutcome::Inconclusive),
            };
        }
        let status = response.status().as_u16();
        let body = read_bounded_provider_text(response, 64 * 1024)
            .await
            .to_ascii_lowercase();
        match status {
            401 | 403 => CapabilityProbeResponse::Unknown(ProbeOutcome::AuthFailed),
            429 => CapabilityProbeResponse::Unknown(ProbeOutcome::RateLimited),
            500..=599 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProviderError),
            404 | 405 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProtocolMismatch),
            400 | 415 | 422
                if body.contains("unsupported")
                    || body.contains("not supported")
                    || body.contains("does not support")
                    || body.contains("unsupported content")
                    || body.contains("unsupported modality") =>
            {
                CapabilityProbeResponse::Rejected
            }
            400 | 415 | 422 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProtocolMismatch),
            _ => CapabilityProbeResponse::Unknown(ProbeOutcome::Inconclusive),
        }
    }

    async fn run_audio_input_probe_request(
        &self,
        provider: &ProviderConfig,
        model: &str,
    ) -> CapabilityProbeResponse {
        let encoded = base64::engine::general_purpose::STANDARD.encode(tiny_silent_wav());
        self.run_capability_probe_request(
            provider,
            json!({
                "model": model,
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Reply with exactly OK after processing this short audio sample."},
                        {"type": "input_audio", "input_audio": {"data": encoded, "format": "wav"}}
                    ]
                }],
                "stream": false,
                "max_tokens": 4
            }),
        )
        .await
    }

    async fn run_transcription_probe_request(
        &self,
        provider: &ProviderConfig,
        model: &str,
    ) -> CapabilityProbeResponse {
        let url = format!(
            "{}/audio/transcriptions",
            provider.endpoint.trim_end_matches('/')
        );
        let part = match Part::bytes(tiny_silent_wav())
            .file_name("xiao-capability-probe.wav")
            .mime_str("audio/wav")
        {
            Ok(part) => part,
            Err(_) => return CapabilityProbeResponse::Unknown(ProbeOutcome::Inconclusive),
        };
        let form = Form::new()
            .part("file", part)
            .text("model", model.to_string());
        let mut request = self
            .client
            .post(url)
            .multipart(form)
            .timeout(Duration::from_secs(20));
        if !provider.api_key.is_empty()
            && !["none", "-", "no", "null"]
                .iter()
                .any(|value| provider.api_key.eq_ignore_ascii_case(value))
        {
            request = request.header("Authorization", format!("Bearer {}", provider.api_key));
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) if error.is_timeout() => {
                return CapabilityProbeResponse::Unknown(ProbeOutcome::Timeout)
            }
            Err(error) if error.is_connect() => {
                return CapabilityProbeResponse::Unknown(ProbeOutcome::NetworkError)
            }
            Err(_) => return CapabilityProbeResponse::Unknown(ProbeOutcome::NetworkError),
        };
        if response.status().is_success() {
            return match read_bounded_provider_json(response).await {
                Ok(body) if body.get("text").and_then(Value::as_str).is_some() => {
                    CapabilityProbeResponse::Success(body)
                }
                Ok(_) | Err(_) => CapabilityProbeResponse::Unknown(ProbeOutcome::Inconclusive),
            };
        }
        let status = response.status().as_u16();
        let body = read_bounded_provider_text(response, 64 * 1024)
            .await
            .to_ascii_lowercase();
        match status {
            401 | 403 => CapabilityProbeResponse::Unknown(ProbeOutcome::AuthFailed),
            429 => CapabilityProbeResponse::Unknown(ProbeOutcome::RateLimited),
            500..=599 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProviderError),
            404 | 405 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProtocolMismatch),
            400 | 415 | 422
                if body.contains("unsupported")
                    || body.contains("not supported")
                    || body.contains("does not support")
                    || body.contains("unsupported modality") =>
            {
                CapabilityProbeResponse::Rejected
            }
            400 | 415 | 422 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProtocolMismatch),
            _ => CapabilityProbeResponse::Unknown(ProbeOutcome::Inconclusive),
        }
    }

    async fn run_image_generation_probe_request(
        &self,
        provider: &ProviderConfig,
        model: &str,
    ) -> CapabilityProbeResponse {
        let url = format!(
            "{}/images/generations",
            provider.endpoint.trim_end_matches('/')
        );
        let mut request = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .json(&json!({
                "model": model,
                "prompt": "A simple solid gray square. Capability probe.",
                "n": 1,
                "response_format": "b64_json"
            }))
            .timeout(Duration::from_secs(120));
        if !provider.api_key.is_empty()
            && !["none", "-", "no", "null"]
                .iter()
                .any(|value| provider.api_key.eq_ignore_ascii_case(value))
        {
            request = request.header("Authorization", format!("Bearer {}", provider.api_key));
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) if error.is_timeout() => {
                return CapabilityProbeResponse::Unknown(ProbeOutcome::Timeout)
            }
            Err(error) if error.is_connect() => {
                return CapabilityProbeResponse::Unknown(ProbeOutcome::NetworkError)
            }
            Err(_) => return CapabilityProbeResponse::Unknown(ProbeOutcome::NetworkError),
        };
        if response.status().is_success() {
            return match read_bounded_provider_json(response).await {
                Ok(body)
                    if body
                        .get("data")
                        .and_then(Value::as_array)
                        .and_then(|data| data.first())
                        .is_some_and(|item| {
                            item.get("b64_json").and_then(Value::as_str).is_some()
                                || item
                                    .get("url")
                                    .and_then(Value::as_str)
                                    .and_then(|url| url::Url::parse(url).ok())
                                    .is_some_and(|url| matches!(url.scheme(), "http" | "https"))
                        }) =>
                {
                    CapabilityProbeResponse::Success(body)
                }
                Ok(_) | Err(_) => CapabilityProbeResponse::Unknown(ProbeOutcome::Inconclusive),
            };
        }
        let status = response.status().as_u16();
        let body = read_bounded_provider_text(response, 64 * 1024)
            .await
            .to_ascii_lowercase();
        match status {
            401 | 403 => CapabilityProbeResponse::Unknown(ProbeOutcome::AuthFailed),
            429 => CapabilityProbeResponse::Unknown(ProbeOutcome::RateLimited),
            500..=599 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProviderError),
            404 | 405 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProtocolMismatch),
            400 | 415 | 422
                if body.contains("unsupported")
                    || body.contains("not supported")
                    || body.contains("does not support")
                    || body.contains("unsupported image generation") =>
            {
                CapabilityProbeResponse::Rejected
            }
            400 | 415 | 422 => CapabilityProbeResponse::Unknown(ProbeOutcome::ProtocolMismatch),
            _ => CapabilityProbeResponse::Unknown(ProbeOutcome::Inconclusive),
        }
    }

    pub async fn probe_image_generation_active_with_observer<F>(
        &self,
        role: ModelRole,
        mut observer: F,
    ) -> Result<CapabilityRecord, String>
    where
        F: FnMut(ProbeEvent),
    {
        let route = self.resolve_model_route_unchecked(role).await?;
        observer(ProbeEvent::Started {
            capability: CapabilityKind::ImageGeneration,
        });
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::ImageGeneration,
            message: "Running explicit active image-generation probe; this may consume provider credits...".to_string(),
        });
        let response = self
            .run_image_generation_probe_request(&route.provider, &route.model)
            .await;
        let value = validate_endpoint_acceptance(&response);
        let outcome = response.outcome(value);
        observer(ProbeEvent::Completed {
            capability: CapabilityKind::ImageGeneration,
            outcome,
        });

        let checked_at = Local::now().to_rfc3339();
        let provider_id = route.provider.endpoint.trim_end_matches('/').to_string();
        let mut record = self
            .capability_record(&route.provider.endpoint, &route.model)
            .await
            .unwrap_or_else(|| CapabilityRecord {
                provider_id: provider_id.clone(),
                provider_name: route.provider.name.clone(),
                model: route.model.clone(),
                ..CapabilityRecord::default()
            });
        record.provider_id = provider_id.clone();
        record.provider_name = route.provider.name.clone();
        record.model = route.model.clone();
        record.supports_image_generation = value;
        record.checked_at = checked_at.clone();
        record.evidence.retain(|evidence| {
            evidence.capability != CapabilityKind::ImageGeneration
                || evidence.source != CapabilityEvidenceSource::ActiveProbe
        });
        record.evidence.push(CapabilityEvidence {
            capability: CapabilityKind::ImageGeneration,
            source: CapabilityEvidenceSource::ActiveProbe,
            outcome: capability_state(value),
            checked_at,
            detail: Some(format!("explicit active probe={outcome:?}")),
        });

        observer(ProbeEvent::Progress {
            capability: CapabilityKind::ImageGeneration,
            message: "Persisting capability registry...".to_string(),
        });
        let candidate = {
            let registry = self.capability_registry.read().await;
            let mut candidate = registry.clone();
            if let Some(existing) = candidate
                .models
                .iter_mut()
                .find(|entry| entry.provider_id == provider_id && entry.model == route.model)
            {
                *existing = record.clone();
            } else {
                candidate.models.push(record.clone());
            }
            candidate
        };
        let saved = persist_capability_registry(candidate.clone()).await;
        if saved {
            *self.capability_registry.write().await = candidate;
        }
        observer(ProbeEvent::Persistence { saved });
        observer(ProbeEvent::Finished);
        if saved {
            Ok(record)
        } else {
            Err(
                "image-generation probe result was not published because persistence failed"
                    .to_string(),
            )
        }
    }

    pub async fn probe_model_capabilities(
        &self,
        provider: &ProviderConfig,
        model: &str,
    ) -> CapabilityRecord {
        self.probe_model_capabilities_with_observer(provider, model, |_| {})
            .await
    }

    pub async fn probe_model_capabilities_with_observer<F>(
        &self,
        provider: &ProviderConfig,
        model: &str,
        mut observer: F,
    ) -> CapabilityRecord
    where
        F: FnMut(ProbeEvent),
    {
        const RED_PNG: &str =
            "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP8z0AaYCJR/aiGUQ1DSAMAQC4BH2bjRnMAAAAASUVORK5CYII=";
        const BLUE_PNG: &str =
            "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAGUlEQVR4nGNkYPjPQApgIkn1qIZRDUNKAwA+MAEfWiW9ygAAAABJRU5ErkJggg==";

        observer(ProbeEvent::Progress {
            capability: CapabilityKind::TextChat,
            message: "Checking provider metadata...".to_string(),
        });

        observer(ProbeEvent::Started {
            capability: CapabilityKind::TextChat,
        });
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::TextChat,
            message: "Probing text chat...".to_string(),
        });
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
        observer(ProbeEvent::Completed {
            capability: CapabilityKind::TextChat,
            outcome: text_probe.outcome(text),
        });

        observer(ProbeEvent::Started {
            capability: CapabilityKind::Tools,
        });
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::Tools,
            message: "Probing function call...".to_string(),
        });
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
        observer(ProbeEvent::Completed {
            capability: CapabilityKind::Tools,
            outcome: tools_probe.outcome(tools),
        });

        observer(ProbeEvent::Started {
            capability: CapabilityKind::StructuredOutput,
        });
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::StructuredOutput,
            message: "Probing JSON structured output...".to_string(),
        });
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
        observer(ProbeEvent::Completed {
            capability: CapabilityKind::StructuredOutput,
            outcome: structured_probe.outcome(structured),
        });

        observer(ProbeEvent::Started {
            capability: CapabilityKind::ImageInput,
        });
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::ImageInput,
            message: "Vision 1/2: identifying red image...".to_string(),
        });
        let red_probe = self
            .run_capability_probe_request(provider, vision_probe_payload(model, RED_PNG))
            .await;
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::ImageInput,
            message: "Vision 2/2: identifying blue image...".to_string(),
        });
        let blue_probe = self
            .run_capability_probe_request(provider, vision_probe_payload(model, BLUE_PNG))
            .await;
        let image = combine_vision_probe_results(
            validate_color_probe(&red_probe, "red"),
            validate_color_probe(&blue_probe, "blue"),
        );
        let vision_outcome = if image == Some(true) {
            ProbeOutcome::Supported
        } else if image == Some(false) {
            ProbeOutcome::Unsupported
        } else {
            match (&red_probe, &blue_probe) {
                (CapabilityProbeResponse::Unknown(outcome), _) => *outcome,
                (_, CapabilityProbeResponse::Unknown(outcome)) => *outcome,
                _ => ProbeOutcome::Inconclusive,
            }
        };
        observer(ProbeEvent::Completed {
            capability: CapabilityKind::ImageInput,
            outcome: vision_outcome,
        });

        observer(ProbeEvent::Started {
            capability: CapabilityKind::AudioInput,
        });
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::AudioInput,
            message: "Probing native Main-compatible audio input with a bounded WAV sample..."
                .to_string(),
        });
        let audio_input_probe = self.run_audio_input_probe_request(provider, model).await;
        let probed_audio_input = validate_text_probe(&audio_input_probe);
        observer(ProbeEvent::Completed {
            capability: CapabilityKind::AudioInput,
            outcome: audio_input_probe.outcome(probed_audio_input),
        });

        observer(ProbeEvent::Started {
            capability: CapabilityKind::AudioTranscription,
        });
        observer(ProbeEvent::Progress {
            capability: CapabilityKind::AudioTranscription,
            message: "Probing audio/transcriptions with a tiny bounded WAV sample...".to_string(),
        });
        let transcription_probe = self.run_transcription_probe_request(provider, model).await;
        let audio_transcription = validate_endpoint_acceptance(&transcription_probe);
        observer(ProbeEvent::Completed {
            capability: CapabilityKind::AudioTranscription,
            outcome: transcription_probe.outcome(audio_transcription),
        });

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
        let checked_at = Local::now().to_rfc3339();
        let metadata_image_input = (modalities.contains("image")
            || modalities.contains("vision")
            || modalities.contains("multimodal"))
        .then_some(true);
        let metadata_audio_input = modalities.contains("audio").then_some(true);
        let video_input = modalities.contains("video").then_some(true);
        let native_file_input =
            (modalities.contains("file") || modalities.contains("document")).then_some(true);
        let image_input = image.or(metadata_image_input);
        let audio_input = probed_audio_input.or(metadata_audio_input);

        observer(ProbeEvent::Started {
            capability: CapabilityKind::VideoInput,
        });
        if video_input == Some(true) {
            observer(ProbeEvent::Completed {
                capability: CapabilityKind::VideoInput,
                outcome: ProbeOutcome::Supported,
            });
        } else {
            observer(ProbeEvent::Skipped {
                capability: CapabilityKind::VideoInput,
                reason: "No verified video metadata and no portable safe active video sample for this provider; remains Unknown.".to_string(),
            });
        }
        observer(ProbeEvent::Skipped {
            capability: CapabilityKind::ImageGeneration,
            reason: "Active image-generation probe can spend credits and is never run automatically; passive evidence only.".to_string(),
        });

        let mut evidence = vec![
            CapabilityEvidence {
                capability: CapabilityKind::TextChat,
                source: CapabilityEvidenceSource::ActiveProbe,
                outcome: capability_state(text),
                checked_at: checked_at.clone(),
                detail: Some(format!("probe={:?}", text_probe.outcome(text))),
            },
            CapabilityEvidence {
                capability: CapabilityKind::ImageInput,
                source: CapabilityEvidenceSource::ActiveProbe,
                outcome: capability_state(image),
                checked_at: checked_at.clone(),
                detail: Some("two-image red/blue semantic probe".to_string()),
            },
            CapabilityEvidence {
                capability: CapabilityKind::Tools,
                source: CapabilityEvidenceSource::ActiveProbe,
                outcome: capability_state(tools),
                checked_at: checked_at.clone(),
                detail: Some(format!("probe={:?}", tools_probe.outcome(tools))),
            },
            CapabilityEvidence {
                capability: CapabilityKind::StructuredOutput,
                source: CapabilityEvidenceSource::ActiveProbe,
                outcome: capability_state(structured),
                checked_at: checked_at.clone(),
                detail: Some(format!("probe={:?}", structured_probe.outcome(structured))),
            },
            CapabilityEvidence {
                capability: CapabilityKind::AudioInput,
                source: CapabilityEvidenceSource::ActiveProbe,
                outcome: capability_state(probed_audio_input),
                checked_at: checked_at.clone(),
                detail: Some(format!(
                    "probe={:?}",
                    audio_input_probe.outcome(probed_audio_input)
                )),
            },
            CapabilityEvidence {
                capability: CapabilityKind::AudioTranscription,
                source: CapabilityEvidenceSource::ActiveProbe,
                outcome: capability_state(audio_transcription),
                checked_at: checked_at.clone(),
                detail: Some(format!(
                    "probe={:?}",
                    transcription_probe.outcome(audio_transcription)
                )),
            },
        ];
        for (capability, value) in [
            (CapabilityKind::ImageInput, metadata_image_input),
            (CapabilityKind::AudioInput, metadata_audio_input),
            (CapabilityKind::VideoInput, video_input),
            (CapabilityKind::NativeFileInput, native_file_input),
        ] {
            if let Some(value) = value {
                evidence.push(CapabilityEvidence {
                    capability,
                    source: CapabilityEvidenceSource::ProviderMetadata,
                    outcome: if value {
                        CapabilityState::Supported
                    } else {
                        CapabilityState::Unsupported
                    },
                    checked_at: checked_at.clone(),
                    detail: Some(format!("modalities={modalities}")),
                });
            }
        }

        let mut record = CapabilityRecord {
            provider_id: provider_id.clone(),
            provider_name: provider.name.clone(),
            model: model.to_string(),
            context_window: metadata.as_ref().and_then(|meta| meta.context_length),
            supports_text_chat: text,
            supports_image_input: image_input,
            supports_image_generation: None,
            supports_image_editing: None,
            supports_audio_input: audio_input,
            supports_audio_transcription: audio_transcription,
            supports_video_input: video_input,
            supports_native_file_input: native_file_input,
            supports_reasoning: None,
            supports_tools: tools,
            supports_structured_output: structured,
            evidence,
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
                "image_generation=unknown (active probe is explicit only)".to_string(),
                format!("audio_transcription={audio_transcription:?}"),
            ],
            checked_at,
        };

        if let Some(previous) = self.capability_record(&provider.endpoint, model).await {
            record.supports_image_generation = previous.supports_image_generation;
            record.supports_image_editing = previous.supports_image_editing;
            record.supports_reasoning = previous.supports_reasoning;
            record
                .evidence
                .extend(previous.evidence.into_iter().filter(|evidence| {
                    matches!(
                        evidence.capability,
                        CapabilityKind::ImageGeneration
                            | CapabilityKind::ImageEditing
                            | CapabilityKind::Reasoning
                    )
                }));
        }

        observer(ProbeEvent::Progress {
            capability: CapabilityKind::TextChat,
            message: "Persisting capability registry...".to_string(),
        });
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
        let saved = persist_capability_registry(candidate.clone()).await;
        if saved {
            *self.capability_registry.write().await = candidate;
        } else {
            warn!("Capability probe result was not published because persistence failed");
        }
        observer(ProbeEvent::Persistence { saved });
        observer(ProbeEvent::Finished);
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
            capability.vision = record.supports_image_input == Some(true);
            capability.vision_desc = match record.supports_image_input {
                Some(true) => "✅ Verified by provider metadata/probe".to_string(),
                Some(false) => "❌ Rejected by provider metadata/probe".to_string(),
                None => "⚪ Unknown: provider did not prove vision support".to_string(),
            };
            capability.audio = record.supports_audio_input == Some(true);
            capability.audio_desc = match record.supports_audio_input {
                Some(true) => "✅ Published/verified by provider".to_string(),
                Some(false) => "❌ Provider reports/rejects audio input".to_string(),
                None => "⚪ Unknown: audio capability not proven".to_string(),
            };
            capability.video = record.supports_video_input == Some(true);
            capability.video_desc = match record.supports_video_input {
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
                                    if let Some(metadata) = normalize_provider_model_metadata(item)
                                    {
                                        model_ids.push(metadata.id.clone());
                                        meta_guard.insert(
                                            model_metadata_key(clean_endpoint, &metadata.id),
                                            metadata,
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
                                        // Catalog presence is not proof that chat/completions works.
                                        supports_text_chat: catalog_presence_text_chat_claim(),
                                        supports_image_input: (modalities.contains("image")
                                            || modalities.contains("vision")
                                            || modalities.contains("multimodal"))
                                        .then_some(true),
                                        supports_image_generation: None,
                                        supports_image_editing: None,
                                        supports_audio_input: modalities
                                            .contains("audio")
                                            .then_some(true),
                                        supports_audio_transcription: None,
                                        supports_video_input: modalities
                                            .contains("video")
                                            .then_some(true),
                                        supports_reasoning: None,
                                        supports_tools: None,
                                        supports_structured_output: None,
                                        supports_native_file_input: (modalities.contains("file")
                                            || modalities.contains("document"))
                                        .then_some(true),
                                        evidence: Vec::new(),
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
                                        if existing.supports_image_input.is_none() {
                                            existing.supports_image_input =
                                                record.supports_image_input;
                                        }
                                        if existing.supports_audio_input.is_none() {
                                            existing.supports_audio_input =
                                                record.supports_audio_input;
                                        }
                                        if existing.supports_video_input.is_none() {
                                            existing.supports_video_input =
                                                record.supports_video_input;
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

    #[test]
    fn catalog_presence_does_not_claim_text_chat() {
        assert_eq!(catalog_presence_text_chat_claim(), None);
    }

    #[test]
    fn provider_metadata_normalizer_handles_observed_openai_compatible_shapes() {
        let metadata = normalize_provider_model_metadata(&json!({
            "id": "model-a",
            "name": "Model A",
            "context_length": 131072,
            "architecture": {"modality": "text+image"},
            "top_provider": {"max_completion_tokens": 8192}
        }))
        .unwrap();
        assert_eq!(metadata.id, "model-a");
        assert_eq!(metadata.context_length, Some(131072));
        assert_eq!(metadata.modalities.as_deref(), Some("text+image"));
        assert_eq!(metadata.max_completion_tokens, Some(8192));

        let metadata = normalize_provider_model_metadata(&json!({
            "id": "model-b",
            "modalities": ["text", "audio", "video"]
        }))
        .unwrap();
        assert_eq!(metadata.modalities.as_deref(), Some("text,audio,video"));
    }

    #[test]
    fn transient_probe_failures_remain_unknown_outcomes() {
        for outcome in [
            ProbeOutcome::Timeout,
            ProbeOutcome::RateLimited,
            ProbeOutcome::ProviderError,
            ProbeOutcome::AuthFailed,
            ProbeOutcome::ProtocolMismatch,
            ProbeOutcome::NetworkError,
            ProbeOutcome::Inconclusive,
        ] {
            assert_eq!(
                CapabilityProbeResponse::Unknown(outcome).outcome(None),
                outcome
            );
        }
    }
}
