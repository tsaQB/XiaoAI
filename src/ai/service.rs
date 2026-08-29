#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use chrono::Local;
use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{error, warn};

use crate::timeline::{
    classify_text_activity, generate_contextual_stages, ExecutionTimeline, ProgressActivity,
    ProgressState,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub active_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderStore {
    pub active_id: Option<String>,
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub telegram_models: Vec<String>,
}

pub fn get_providers_store_path() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return std::path::Path::new(&home).join(".xiao_providers.json");
    }
    std::path::Path::new(".xiao_providers.json").to_path_buf()
}

pub fn load_provider_store() -> ProviderStore {
    let p = get_providers_store_path();
    if p.exists() {
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Ok(store) = serde_json::from_str::<ProviderStore>(&content) {
                return store;
            }
        }
    }
    ProviderStore::default()
}

pub fn save_provider_store(store: &ProviderStore) -> std::io::Result<()> {
    let p = get_providers_store_path();
    let json_str = serde_json::to_string_pretty(store)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(p, json_str)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: usize,
    pub name: String,
    pub messages: Vec<ChatMessage>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapability {
    pub model_name: String,
    pub family: String,
    pub provider_icon: String,
    pub context_limit: usize,
    pub context_str: String,
    pub vision: bool,
    pub vision_desc: String,
    pub video: bool,
    pub video_desc: String,
    pub documents: bool,
    pub docs_desc: String,
    pub audio: bool,
    pub audio_desc: String,
    pub thinking: bool,
    pub thinking_desc: String,
    pub strengths: String,
}

pub fn get_model_capabilities(model_name: &str) -> ModelCapability {
    let m = model_name.to_lowercase();

    if m.contains("gemini") {
        let (context_limit, context_str) = if m.contains("pro") || m.contains("2m") {
            (2097152, "2,000,000 tokens (2M Masif)".to_string())
        } else {
            (1048576, "1,048,576 tokens (1M Masif)".to_string())
        };
        let thinking = m.contains("high") || m.contains("pro") || m.contains("3.7") || m.contains("3.6");

        ModelCapability {
            model_name: model_name.to_string(),
            family: "Google Gemini 3.x Multimodal".to_string(),
            provider_icon: "✨".to_string(),
            context_limit,
            context_str,
            vision: true,
            vision_desc: "✅ Didukung Penuh (Ultra High-Res OCR & Vision)".to_string(),
            video: true,
            video_desc: "✅ Native Video Vision (Analisis Video Langsung)".to_string(),
            documents: true,
            docs_desc: "✅ Didukung Penuh (Membaca file PDF/TXT/Code)".to_string(),
            audio: true,
            audio_desc: "✅ Native Audio (Bisa langsung mendengar pesan suara)".to_string(),
            thinking,
            thinking_desc: "✅ Adaptive Thinking & High Reasoning Engine".to_string(),
            strengths: "Konteks masif (1M-2M), video & audio langsung, kecepatan tinggi, penalaran multimodal adaptif".to_string(),
        }
    } else if m.contains("claude") {
        ModelCapability {
            model_name: model_name.to_string(),
            family: "Anthropic Claude 4.x / 3.x".to_string(),
            provider_icon: "🧠".to_string(),
            context_limit: 200000,
            context_str: "200,000 tokens (200k Luas)".to_string(),
            vision: true,
            vision_desc: "✅ Didukung Penuh (Diagram, Arsitektur UI & OCR)".to_string(),
            video: false,
            video_desc: "❌ Belum Didukung Langsung".to_string(),
            documents: true,
            docs_desc: "✅ Didukung Penuh (Analisis Dokumen Kompleks & Codebase)".to_string(),
            audio: false,
            audio_desc: "❌ Belum Didukung Langsung (Ketik via teks / butuh Whisper)".to_string(),
            thinking: true,
            thinking_desc: "✅ Extended Thinking & Chain-of-Thought".to_string(),
            strengths: "Kualitas penulisan prosa alami, pemahaman instruksi kompleks, arsitektur & refactoring kode tingkat lanjut".to_string(),
        }
    } else if ["gpt", "codex", "o1", "o3"].iter().any(|k| m.contains(k)) {
        let (context_limit, context_str) = if m.contains("sol") || m.contains("terra") || m.contains("luna") || m.contains("256k") {
            (256000, "256,000 tokens (256k Luas)".to_string())
        } else if m.contains("mini") {
            (128000, "128,000 tokens (128k)".to_string())
        } else {
            (128000, "128,000 tokens (128k Standar)".to_string())
        };

        let vision = !(m.contains("codex") && m.contains("spark"));
        let audio = m.contains("audio") || m.contains("realtime");

        ModelCapability {
            model_name: model_name.to_string(),
            family: "OpenAI GPT-5.x / Next-Gen".to_string(),
            provider_icon: "❇️".to_string(),
            context_limit,
            context_str,
            vision,
            vision_desc: if vision { "✅ Didukung Penuh (Visi Gambar & Analisis Grafis)".to_string() } else { "❌ Model Khusus Kode".to_string() },
            video: false,
            video_desc: "❌ Belum Didukung Langsung".to_string(),
            documents: true,
            docs_desc: "✅ Didukung Penuh (Membaca Dokumen & Kode)".to_string(),
            audio,
            audio_desc: if audio { "✅ Native Audio Supported".to_string() } else { "❌ Belum Didukung Langsung (Ketik via teks)".to_string() },
            thinking: true,
            thinking_desc: "✅ Next-Gen CoT Reasoning & Code Synthesis".to_string(),
            strengths: "Presisi logika matematika, sintesis & perbaikan kode, manipulasi data terstruktur".to_string(),
        }
    } else if m.contains("minimax") {
        ModelCapability {
            model_name: model_name.to_string(),
            family: "MiniMax Multimodal (M3 / 01 / Text)".to_string(),
            provider_icon: "🦁".to_string(),
            context_limit: 245760,
            context_str: "245,760 tokens (245k Luas)".to_string(),
            vision: true,
            vision_desc: "✅ Didukung Penuh (Visual OCR & Vision)".to_string(),
            video: true,
            video_desc: "✅ Native Video & Vision Sequences Supported".to_string(),
            documents: true,
            docs_desc: "✅ Didukung Penuh (Dokumen Teks, PDF & Code)".to_string(),
            audio: false,
            audio_desc: "❌ Belum Didukung Langsung (Ketik via teks / Whisper)".to_string(),
            thinking: false,
            thinking_desc: "Standar (High Efficiency)".to_string(),
            strengths: "Pemahaman sekuens visual & video, konteks panjang 245k, performa respons cepat".to_string(),
        }
    } else if m.contains("qwen") {
        let is_video = m.contains("vl") || m.contains("qvq") || m.contains("vision") || m.contains("video");
        let is_thinking = m.contains("qvq") || m.contains("think") || m.contains("r1") || m.contains("reason");
        ModelCapability {
            model_name: model_name.to_string(),
            family: "Qwen 2.5 / 2.0 (Alibaba)".to_string(),
            provider_icon: "👑".to_string(),
            context_limit: 131072,
            context_str: "131,072 tokens (128k Luas)".to_string(),
            vision: true,
            vision_desc: "✅ Didukung (Qwen-VL Multimodal)".to_string(),
            video: is_video,
            video_desc: if is_video { "✅ Native Video Vision Supported".to_string() } else { "❌ Belum Didukung Langsung".to_string() },
            documents: true,
            docs_desc: "✅ Didukung (Dokumen Teks & Codebase)".to_string(),
            audio: false,
            audio_desc: "❌ Belum Didukung Langsung".to_string(),
            thinking: is_thinking,
            thinking_desc: if is_thinking { "✅ QVQ / CoT Visual Reasoning".to_string() } else { "Standar (Direct Prompting)".to_string() },
            strengths: "Keunggulan visual reasoning (Qwen VL/QVQ), coding, instruksi multibahasa tingkat tinggi".to_string(),
        }
    } else if m.contains("deepseek") {
        let vision = m.contains("vl") || m.contains("vision");
        let thinking = m.contains("r1") || m.contains("think") || m.contains("reason");

        ModelCapability {
            model_name: model_name.to_string(),
            family: "DeepSeek AI (V3 / R1)".to_string(),
            provider_icon: "🐋".to_string(),
            context_limit: 128000,
            context_str: "128,000 tokens (128k)".to_string(),
            vision,
            vision_desc: if vision { "✅ Didukung (DeepSeek-VL)".to_string() } else { "❌ Model Teks Murni".to_string() },
            video: false,
            video_desc: "❌ Tidak Didukung".to_string(),
            documents: true,
            docs_desc: "✅ Didukung (Dokumen Teks & Kode)".to_string(),
            audio: false,
            audio_desc: "❌ Tidak Didukung (Ketik via teks)".to_string(),
            thinking,
            thinking_desc: if thinking { "✅ DeepSeek-R1 Deep Reasoning CoT".to_string() } else { "Standar (Direct Prompting)".to_string() },
            strengths: "Kemampuan matematika murni, algoritma pemrograman, logika penalaran terbuka".to_string(),
        }
    } else {
        let has_video = m.contains("video") || m.contains("vl") || m.contains("vision") || m.contains("omni") || m.contains("qvq") || m.contains("pixtral") || m.contains("internvl") || m.contains("cogvlm") || m.contains("m3") || m.contains("m2");
        let has_audio = m.contains("audio") || m.contains("voice") || m.contains("realtime") || m.contains("omni");
        let has_thinking = m.contains("think") || m.contains("reason") || m.contains("r1") || m.contains("qvq");

        ModelCapability {
            model_name: model_name.to_string(),
            family: "OpenAI-Compatible Multimodal Model".to_string(),
            provider_icon: "⚡".to_string(),
            context_limit: 128000,
            context_str: "128,000 tokens (128k)".to_string(),
            vision: true,
            vision_desc: "✅ Didukung (Multimodal Image)".to_string(),
            video: has_video,
            video_desc: if has_video { "✅ Native Video Vision Supported".to_string() } else { "❌ Belum Didukung Langsung".to_string() },
            documents: true,
            docs_desc: "✅ Didukung (Dokumen Teks & PDF)".to_string(),
            audio: has_audio,
            audio_desc: if has_audio { "✅ Native Audio Supported".to_string() } else { "❌ Belum Didukung Langsung".to_string() },
            thinking: has_thinking,
            thinking_desc: if has_thinking { "✅ Reasoning Chain-of-Thought".to_string() } else { "Standar (Direct Prompting)".to_string() },
            strengths: "Pemrosesan multimodal, penalaran kontekstual, dan pemahaman konten luas".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelMetadata {
    pub id: String,
    pub name: Option<String>,
    pub context_length: Option<usize>,
    pub modalities: Option<String>,
    pub max_completion_tokens: Option<usize>,
}

fn format_number_with_commas(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

pub fn get_model_capabilities_with_meta(model_name: &str, meta: Option<&ModelMetadata>) -> ModelCapability {
    let mut cap = get_model_capabilities(model_name);

    if let Some(m) = meta {
        if let Some(ctx) = m.context_length {
            if ctx > 0 {
                cap.context_limit = ctx;
                let formatted_ctx = format_number_with_commas(ctx);
                cap.context_str = if ctx >= 1_000_000 {
                    format!("{} tokens ({}M Masif)", formatted_ctx, ctx / 1_000_000)
                } else if ctx >= 1_000 {
                    format!("{} tokens ({}k)", formatted_ctx, ctx / 1_000)
                } else {
                    format!("{ctx} tokens")
                };
            }
        }
        if let Some(ref mod_str) = m.modalities {
            let mod_low = mod_str.to_lowercase();
            if mod_low.contains("image") || mod_low.contains("multimodal") {
                cap.vision = true;
                cap.vision_desc = "✅ Didukung (Endpoint Architecture Modality)".to_string();
            }
            if mod_low.contains("video") {
                cap.video = true;
                cap.video_desc = "✅ Native Video Vision Supported (Endpoint Modality)".to_string();
            }
            if mod_low.contains("audio") {
                cap.audio = true;
                cap.audio_desc = "✅ Native Audio Supported (Endpoint Modality)".to_string();
            }
        }
    }

    cap
}

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

#[derive(Clone)]
pub struct AIChatService {
    client: Client,
    user_models: Arc<RwLock<HashMap<i64, String>>>,
    user_sessions: Arc<RwLock<HashMap<i64, Vec<ChatSession>>>>,
    active_session_idx: Arc<RwLock<HashMap<i64, usize>>>,
    pub user_waiting_rename: Arc<RwLock<HashMap<i64, usize>>>,
    pub user_rename_msg_id: Arc<RwLock<HashMap<i64, i64>>>,
    pub user_session_msg_id: Arc<RwLock<HashMap<i64, i64>>>,
    user_providers: Arc<RwLock<HashMap<i64, Vec<ProviderConfig>>>>,
    active_provider_id: Arc<RwLock<HashMap<i64, String>>>,
    pub user_wizard_state: Arc<RwLock<HashMap<i64, HashMap<String, String>>>>,
    pub model_metadata: Arc<RwLock<HashMap<String, ModelMetadata>>>,
}

impl AIChatService {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            client,
            user_models: Arc::new(RwLock::new(HashMap::new())),
            user_sessions: Arc::new(RwLock::new(HashMap::new())),
            active_session_idx: Arc::new(RwLock::new(HashMap::new())),
            user_waiting_rename: Arc::new(RwLock::new(HashMap::new())),
            user_rename_msg_id: Arc::new(RwLock::new(HashMap::new())),
            user_session_msg_id: Arc::new(RwLock::new(HashMap::new())),
            user_providers: Arc::new(RwLock::new(HashMap::new())),
            active_provider_id: Arc::new(RwLock::new(HashMap::new())),
            user_wizard_state: Arc::new(RwLock::new(HashMap::new())),
            model_metadata: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ==========================================
    // Provider Management
    // ==========================================

    pub fn get_global_provider(&self) -> Option<ProviderConfig> {
        let store = load_provider_store();
        if let Some(ref aid) = store.active_id {
            if let Some(p) = store.providers.iter().find(|p| &p.id == aid) {
                return Some(p.clone());
            }
        }
        store.providers.first().cloned()
    }

    pub async fn has_configured_provider(&self, user_id: i64) -> bool {
        let providers = self.user_providers.read().await;
        if let Some(list) = providers.get(&user_id) {
            if !list.is_empty() {
                return true;
            }
        }
        self.get_global_provider().is_some()
    }

    pub async fn get_user_providers(&self, user_id: i64) -> Vec<ProviderConfig> {
        let providers = self.user_providers.read().await;
        if let Some(list) = providers.get(&user_id) {
            if !list.is_empty() {
                return list.clone();
            }
        }
        let store = load_provider_store();
        if !store.providers.is_empty() {
            store.providers
        } else if let Some(global) = self.get_global_provider() {
            vec![global]
        } else {
            Vec::new()
        }
    }

    pub async fn get_active_provider(&self, user_id: i64) -> Option<ProviderConfig> {
        let providers = self.user_providers.read().await;
        if let Some(list) = providers.get(&user_id) {
            if !list.is_empty() {
                let active_ids = self.active_provider_id.read().await;
                if let Some(active_id) = active_ids.get(&user_id) {
                    for p in list {
                        if &p.id == active_id {
                            return Some(p.clone());
                        }
                    }
                }
                return Some(list[0].clone());
            }
        }
        self.get_global_provider()
    }

    pub async fn set_active_provider(&self, user_id: i64, provider_id: &str) -> bool {
        let mut store = load_provider_store();
        if let Some(p) = store.providers.iter().find(|p| p.id == provider_id) {
            store.active_id = Some(provider_id.to_string());
            let _ = save_provider_store(&store);
            let _ = std::fs::write(".env", format!(
                "BOT_TOKEN={}\nAI_ENDPOINT={}\nAI_API_KEY={}\nAI_MODEL={}\n",
                std::env::var("BOT_TOKEN").unwrap_or_default(),
                p.endpoint,
                p.api_key,
                p.active_model
            ));
        }

        let providers = self.user_providers.read().await;
        if let Some(list) = providers.get(&user_id) {
            if list.iter().any(|p| p.id == provider_id) {
                drop(providers);
                self.active_provider_id
                    .write()
                    .await
                    .insert(user_id, provider_id.to_string());
                return true;
            }
        }
        drop(providers);
        self.active_provider_id
            .write()
            .await
            .insert(user_id, provider_id.to_string());
        true
    }

    pub async fn add_user_provider(
        &self,
        user_id: i64,
        endpoint: &str,
        api_key: &str,
        alias: &str,
        models: Vec<String>,
    ) -> ProviderConfig {
        use rand::Rng;
        let random_suffix: String = rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(6)
            .map(char::from)
            .collect();
        let provider_id = format!("prov_{}", random_suffix.to_lowercase());

        let default_model = models.first().cloned().unwrap_or_else(|| "gpt-4o".to_string());
        let provider = ProviderConfig {
            id: provider_id.clone(),
            name: if alias.trim().is_empty() { "Custom Provider".to_string() } else { alias.trim().to_string() },
            endpoint: endpoint.trim().trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
            models,
            active_model: default_model,
        };

        {
            let mut store = load_provider_store();
            store.providers.push(provider.clone());
            store.active_id = Some(provider_id.clone());
            let _ = save_provider_store(&store);
        }

        {
            let mut providers = self.user_providers.write().await;
            providers.entry(user_id).or_default().push(provider.clone());
        }

        self.active_provider_id
            .write()
            .await
            .insert(user_id, provider_id);

        provider
    }

    pub async fn remove_user_provider(&self, user_id: i64, provider_id: &str) -> bool {
        let mut store = load_provider_store();
        if let Some(pos) = store.providers.iter().position(|p| p.id == provider_id) {
            store.providers.remove(pos);
            if store.active_id.as_deref() == Some(provider_id) {
                store.active_id = store.providers.first().map(|p| p.id.clone());
            }
            let _ = save_provider_store(&store);
        }

        let mut providers = self.user_providers.write().await;
        if let Some(list) = providers.get_mut(&user_id) {
            if let Some(pos) = list.iter().position(|p| p.id == provider_id) {
                list.remove(pos);
                let new_active = list.first().map(|p| p.id.clone());
                drop(providers);

                let mut active_ids = self.active_provider_id.write().await;
                if let Some(first_id) = new_active {
                    active_ids.insert(user_id, first_id);
                } else {
                    active_ids.remove(&user_id);
                }
                return true;
            }
        }
        false
    }

    pub async fn update_provider_models(&self, user_id: i64, provider_id: &str, models: Vec<String>) {
        let mut store = load_provider_store();
        if let Some(p) = store.providers.iter_mut().find(|p| p.id == provider_id) {
            p.models = models.clone();
            let _ = save_provider_store(&store);
        }

        let mut providers = self.user_providers.write().await;
        let list = providers.entry(user_id).or_default();
        if let Some(p) = list.iter_mut().find(|p| p.id == provider_id) {
            p.models = models;
        } else if let Some(mut global) = self.get_global_provider() {
            global.models = models;
            list.push(global);
        }
    }

    pub async fn get_provider_model_by_index(&self, user_id: i64, provider_id: &str, index: usize) -> Option<String> {
        let providers = self.get_user_providers(user_id).await;
        let target = providers.into_iter().find(|p| p.id == provider_id)?;
        target.models.get(index).cloned()
    }

    pub async fn set_provider_model(&self, user_id: i64, provider_id: &str, model_name: &str) -> bool {
        let mut store = load_provider_store();
        let mut matched_endpoint = String::new();
        let mut matched_key = String::new();

        if let Some(p) = store.providers.iter_mut().find(|p| p.id == provider_id) {
            p.active_model = model_name.to_string();
            matched_endpoint = p.endpoint.clone();
            matched_key = p.api_key.clone();
        }

        if !matched_endpoint.is_empty() {
            store.active_id = Some(provider_id.to_string());
            let _ = save_provider_store(&store);
            let _ = std::fs::write(".env", format!(
                "BOT_TOKEN={}\nAI_ENDPOINT={}\nAI_API_KEY={}\nAI_MODEL={}\n",
                std::env::var("BOT_TOKEN").unwrap_or_default(),
                matched_endpoint,
                matched_key,
                model_name
            ));
        }

        let mut providers = self.user_providers.write().await;
        if let Some(list) = providers.get_mut(&user_id) {
            for p in list.iter_mut() {
                if p.id == provider_id {
                    p.active_model = model_name.to_string();
                }
            }
        }
        self.active_provider_id
            .write()
            .await
            .insert(user_id, provider_id.to_string());
        self.user_models
            .write()
            .await
            .insert(user_id, model_name.to_string());
        true
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
        if !trimmed_key.is_empty() && !["none", "-", "no", "null"].iter().any(|k| trimmed_key.eq_ignore_ascii_case(k)) {
            req = req.header("Authorization", format!("Bearer {trimmed_key}"));
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.json::<Value>().await {
                        Ok(data) => {
                            let mut model_ids = Vec::new();
                            let mut meta_guard = self.model_metadata.write().await;

                            if let Some(data_arr) = data.get("data").and_then(|d| d.as_array()) {
                                for item in data_arr {
                                    if let Some(id_str) = item.get("id").and_then(|s| s.as_str()) {
                                        model_ids.push(id_str.to_string());

                                        let context_length = item.get("context_length").and_then(|c| c.as_u64()).map(|u| u as usize);
                                        let modality = item.get("architecture")
                                            .and_then(|a| a.get("modality"))
                                            .and_then(|m| m.as_str())
                                            .map(|s| s.to_string());
                                        let max_comp = item.get("top_provider")
                                            .and_then(|p| p.get("max_completion_tokens"))
                                            .and_then(|m| m.as_u64())
                                            .map(|u| u as usize);

                                        meta_guard.insert(id_str.to_string(), ModelMetadata {
                                            id: id_str.to_string(),
                                            name: item.get("name").and_then(|s| s.as_str()).map(|s| s.to_string()),
                                            context_length,
                                            modalities: modality,
                                            max_completion_tokens: max_comp,
                                        });
                                    } else if let Some(s) = item.as_str() {
                                        model_ids.push(s.to_string());
                                    }
                                }
                            } else if let Some(data_obj) = data.get("data").and_then(|d| d.as_object()) {
                                for k in data_obj.keys() {
                                    model_ids.push(k.to_string());
                                }
                            }

                            if !model_ids.is_empty() {
                                (true, Ok(model_ids))
                            } else {
                                (false, Err("Endpoint berhasil dihubungi, namun tidak ada daftar model yang dikembalikan (data kosong).".to_string()))
                            }
                        }
                        Err(e) => (false, Err(format!("Respon dari endpoint bukan JSON valid: {e}"))),
                    }
                } else if status.as_u16() == 401 || status.as_u16() == 403 {
                    (false, Err(format!("HTTP {} Unauthorized: Autentikasi gagal. Mohon periksa kembali API Key Anda.", status.as_u16())))
                } else if status.as_u16() == 404 {
                    (false, Err(format!("HTTP 404 Not Found: Path /models tidak ditemukan di {clean_endpoint}. Pastikan format endpoint URL benar (misal: https://api.openai.com/v1).")))
                } else {
                    let err_text = resp.text().await.unwrap_or_default();
                    (false, Err(format!("HTTP {}: {}", status.as_u16(), &err_text[..err_text.len().min(150)])))
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    (false, Err(format!("Koneksi timeout setelah 15 detik ke {clean_endpoint}.")))
                } else if e.is_connect() {
                    (false, Err(format!("Gagal terhubung ke {clean_endpoint}. Pastikan host/domain benar dan server aktif.")))
                } else {
                    (false, Err(format!("Koneksi gagal: {e}")))
                }
            }
        }
    }

    pub async fn get_user_model(&self, user_id: i64) -> String {
        if let Some(prov) = self.get_active_provider(user_id).await {
            if !prov.active_model.is_empty() {
                return prov.active_model;
            }
        }
        let models = self.user_models.read().await;
        models.get(&user_id).cloned().unwrap_or_else(|| "gpt-4o".to_string())
    }

    pub async fn set_user_model(&self, user_id: i64, model: &str) {
        if let Some(prov) = self.get_active_provider(user_id).await {
            self.set_provider_model(user_id, &prov.id, model).await;
        }
        self.user_models.write().await.insert(user_id, model.to_string());
    }

    // ==========================================
    // Multi-Session Management
    // ==========================================

    pub async fn get_sessions(&self, user_id: i64) -> Vec<ChatSession> {
        let mut sessions_map = self.user_sessions.write().await;
        let list = sessions_map.entry(user_id).or_insert_with(|| {
            let now_str = Local::now().format("%d %b %H:%M").to_string();
            vec![ChatSession {
                id: 1,
                name: format!("Session {now_str}"),
                messages: Vec::new(),
                created_at: now_str,
            }]
        });
        list.clone()
    }

    pub async fn get_active_session_index(&self, user_id: i64) -> usize {
        let _ = self.get_sessions(user_id).await;
        let mut active_map = self.active_session_idx.write().await;
        let idx = *active_map.entry(user_id).or_insert(0);

        let sessions_map = self.user_sessions.read().await;
        if let Some(list) = sessions_map.get(&user_id) {
            if idx >= list.len() {
                let fixed = list.len().saturating_sub(1);
                active_map.insert(user_id, fixed);
                return fixed;
            }
        }
        idx
    }

    pub async fn get_active_session(&self, user_id: i64) -> ChatSession {
        let sessions = self.get_sessions(user_id).await;
        let idx = self.get_active_session_index(user_id).await;
        sessions.get(idx).cloned().unwrap_or_else(|| sessions[0].clone())
    }

    pub async fn create_new_session(&self, user_id: i64, custom_name: Option<&str>) -> ChatSession {
        let mut sessions_map = self.user_sessions.write().await;
        let list = sessions_map.entry(user_id).or_default();
        let now_str = Local::now().format("%d %b %H:%M").to_string();
        let new_id = list.len() + 1;
        let name = custom_name
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Session {now_str}"));

        let session = ChatSession {
            id: new_id,
            name,
            messages: Vec::new(),
            created_at: now_str,
        };
        list.push(session.clone());

        let new_idx = list.len() - 1;
        drop(sessions_map);

        self.active_session_idx.write().await.insert(user_id, new_idx);
        session
    }

    pub async fn switch_session(&self, user_id: i64, index: usize) -> bool {
        let sessions = self.get_sessions(user_id).await;
        if index < sessions.len() {
            self.active_session_idx.write().await.insert(user_id, index);
            return true;
        }
        false
    }

    pub async fn remove_session(&self, user_id: i64, index: usize) -> bool {
        let mut sessions_map = self.user_sessions.write().await;
        if let Some(list) = sessions_map.get_mut(&user_id) {
            if index < list.len() {
                list.remove(index);
                if list.is_empty() {
                    let now_str = Local::now().format("%d %b %H:%M").to_string();
                    list.push(ChatSession {
                        id: 1,
                        name: format!("Session {now_str}"),
                        messages: Vec::new(),
                        created_at: now_str,
                    });
                    self.active_session_idx.write().await.insert(user_id, 0);
                } else {
                    let mut active_map = self.active_session_idx.write().await;
                    let curr = active_map.get(&user_id).copied().unwrap_or(0);
                    if curr >= list.len() {
                        active_map.insert(user_id, list.len() - 1);
                    } else if curr == index {
                        active_map.insert(user_id, index.saturating_sub(1));
                    }
                }
                return true;
            }
        }
        false
    }

    pub async fn rename_session(&self, user_id: i64, index: usize, new_name: &str) -> bool {
        let mut sessions_map = self.user_sessions.write().await;
        if let Some(list) = sessions_map.get_mut(&user_id) {
            if let Some(sess) = list.get_mut(index) {
                let trimmed = new_name.trim();
                sess.name = trimmed[..trimmed.len().min(60)].to_string();
                return true;
            }
        }
        false
    }

    pub async fn clear_history(&self, user_id: i64) {
        let idx = self.get_active_session_index(user_id).await;
        let mut sessions_map = self.user_sessions.write().await;
        if let Some(list) = sessions_map.get_mut(&user_id) {
            if let Some(sess) = list.get_mut(idx) {
                sess.messages.clear();
            }
        }
    }

    pub async fn get_context_stats(&self, user_id: i64) -> ContextStats {
        let active_sess = self.get_active_session(user_id).await;
        let active_model = self.get_user_model(user_id).await;
        let metadata = self.model_metadata.read().await.get(&active_model).cloned();
        let cap = get_model_capabilities_with_meta(&active_model, metadata.as_ref());
        let limit_tokens = cap.context_limit;
        let limit_str = cap.context_str.clone();

        let mut total_chars = 0;
        let mut msg_stats = Vec::new();

        for (i, m) in active_sess.messages.iter().enumerate() {
            let c_str = match &m.content {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            let chars = c_str.len();
            let toks = (chars / 3).max(1);
            total_chars += chars;

            let preview = if c_str.len() > 90 {
                let mut end = 90;
                while end < c_str.len() && !c_str.is_char_boundary(end) {
                    end += 1;
                }
                format!("{}...", &c_str[..end])
            } else {
                c_str.clone()
            };

            msg_stats.push(ContextMessageItem {
                index: i + 1,
                role: m.role.clone(),
                preview,
                chars,
                tokens: toks,
            });
        }

        let total_tokens = total_chars / 3;
        let usage_pct = ((total_tokens as f64 / limit_tokens.max(1) as f64) * 100.0).min(100.0);

        let mut filled_blocks = (usage_pct / 10.0).floor() as usize;
        if usage_pct > 0.0 && filled_blocks == 0 {
            filled_blocks = 1;
        }
        filled_blocks = filled_blocks.min(10);
        let bar = format!("{}{}", "█".repeat(filled_blocks), "░".repeat(10 - filled_blocks));

        ContextStats {
            session_name: active_sess.name,
            session_id: active_sess.id,
            created_at: active_sess.created_at,
            model_name: active_model,
            capabilities: cap,
            limit_tokens,
            limit_str,
            total_messages: active_sess.messages.len(),
            total_turns: (active_sess.messages.len() + 1) / 2,
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
        let provider = match self.get_active_provider(user_id).await {
            Some(p) if !p.endpoint.is_empty() => p,
            _ => return (false, Err("Provider belum dikonfigurasi. Silakan jalankan /provider terlebih dahulu.".to_string())),
        };

        let stt_url = format!("{}/audio/transcriptions", provider.endpoint);
        let part = match Part::bytes(audio_bytes).file_name(file_name.to_string()).mime_str("audio/ogg") {
            Ok(p) => p,
            Err(e) => return (false, Err(format!("Multipart part error: {e}"))),
        };

        let form = Form::new().part("file", part).text("model", "whisper-1");
        let mut req = self.client.post(&stt_url).multipart(form).timeout(Duration::from_secs(45));

        if !provider.api_key.is_empty() && !["none", "-", "no"].iter().any(|k| provider.api_key.eq_ignore_ascii_case(k)) {
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
                    (false, Err(format!("HTTP {}: {}", status.as_u16(), &err_txt[..err_txt.len().min(100)])))
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
        prompt: &str,
        timeline: Option<&Arc<ExecutionTimeline>>,
        image_bytes: Option<Vec<u8>>,
        mime_type: Option<&str>,
        doc_text: Option<&str>,
        doc_name: Option<&str>,
        audio_bytes: Option<Vec<u8>>,
        audio_mime: Option<&str>,
        video_bytes: Option<Vec<u8>>,
        video_mime: Option<&str>,
        video_duration: Option<i32>,
    ) -> (Option<String>, String) {
        let provider = match self.get_active_provider(user_id).await {
            Some(p) if !p.endpoint.is_empty() => p,
            _ => {
                return (
                    None,
                    "👋 <b>Hi, selamat datang di XiaoAI!</b>\n\n⚠️ <i>AI Provider belum dikonfigurasi.</i>\nSilakan jalankan perintah <code>xiao provider add</code> di terminal.".to_string(),
                );
            }
        };

        let user_m = self.get_user_model(user_id).await;
        let model = if !user_m.is_empty() {
            user_m
        } else if !provider.active_model.is_empty() {
            provider.active_model.clone()
        } else {
            provider.models.first().cloned().unwrap_or_else(|| "gpt-4o".to_string())
        };

        let active_sess = self.get_active_session(user_id).await;
        let mut history: Vec<Value> = active_sess
            .messages
            .iter()
            .map(|m| json!({ "role": m.role, "content": m.content }))
            .collect();

        if history.len() > 10 {
            let start = history.len() - 10;
            history = history[start..].to_vec();
        }

        // Contextual stage classification
        let _contextual_stages = if video_bytes.is_some() {
            vec![
                ("Watching", ProgressActivity::Watching),
                ("Thinking", ProgressActivity::Thinking),
                ("Writing", ProgressActivity::Writing),
            ]
        } else if image_bytes.is_some() {
            vec![
                ("Looking", ProgressActivity::Looking),
                ("Thinking", ProgressActivity::Thinking),
                ("Writing", ProgressActivity::Writing),
            ]
        } else if doc_text.is_some() {
            vec![
                ("Reading", ProgressActivity::Reading),
                ("Thinking", ProgressActivity::Thinking),
                ("Writing", ProgressActivity::Writing),
            ]
        } else if audio_bytes.is_some() {
            vec![
                ("Listening", ProgressActivity::Listening),
                ("Thinking", ProgressActivity::Thinking),
                ("Writing", ProgressActivity::Writing),
            ]
        } else {
            generate_contextual_stages(prompt, &model)
        };

        let mut clean_prompt = prompt.trim().to_string();
        if let Some(doc) = doc_text {
            let d_name = doc_name.unwrap_or("Dokumen");
            let doc_header = format!("[Dokumen Terlampir: {d_name}]\n{}\n\n", doc.trim());
            clean_prompt = if clean_prompt.is_empty() {
                format!("{doc_header}Baca, analisis, dan jelaskan isi dokumen ini.")
            } else {
                format!("{doc_header}{clean_prompt}")
            };
        } else if video_bytes.is_some() && clean_prompt.is_empty() {
            let dur_str = video_duration.map(|d| format!(" ({d} detik)")).unwrap_or_default();
            clean_prompt = format!("Tonton dan analisis rekaman video ini{dur_str} secara mendalam. Jelaskan isi visual, alur peristiwa, teks di layar, dan suara di dalamnya.");
        } else if image_bytes.is_some() && clean_prompt.is_empty() {
            clean_prompt = "Jelaskan dan analisis gambar ini secara detail.".to_string();
        } else if audio_bytes.is_some() && clean_prompt.is_empty() {
            clean_prompt = "Dengarkan rekaman suara ini dan jawab pertanyaan atau instruksi di dalamnya secara lengkap.".to_string();
        }

        let p_low = clean_prompt.to_lowercase();
        let needs_think = ["logika", "teka-teki", "puzzle", "riddle", "analisis mendalam", "hitung", "rumus", "derivat", "audit", "debug"]
            .iter()
            .any(|k| p_low.contains(k))
            || model.to_lowercase().contains("thinking");

        let enhanced_prompt = if needs_think {
            format!(
                "Tuliskan analisis penalaranmu di dalam tag <think>...</think> terlebih dahulu jika diperlukan, lalu berikan jawabanmu dengan format yang rapi dan elegan.\n\n\
                Pesan/Pertanyaan: {clean_prompt}"
            )
        } else {
            clean_prompt.clone()
        };

        let mut messages = vec![json!({
            "role": "system",
            "content": "Kamu adalah asisten AI yang cerdas, komunikatif, dan ramah. \
                        Kamu memiliki kemampuan penglihatan visual multimodal yang andal untuk menganalisis rekaman video, mengenali gambar/foto, membaca dokumen teks/PDF, serta mendengarkan pesan suara audio. \
                        Gunakan gaya bahasa yang alami dan format teks yang elegan. \
                        Jika membuat tabel atau data berkolom, SELALU gunakan format standar Markdown Table (| Kolom 1 | Kolom 2 |\n| :--- | :--- |) dan hindari menggambar border ASCII manual, karena sistem akan secara otomatis merendernya sebagai tabel native Telegram interaktif (InputRichBlockTable)."
        })];

        messages.extend(history);

        if let Some(v_bytes) = video_bytes {
            use base64::Engine;
            let b64_vid = base64::engine::general_purpose::STANDARD.encode(&v_bytes);
            let v_m = video_mime.unwrap_or("video/mp4");
            let data_url = format!("data:{v_m};base64,{b64_vid}");
            messages.push(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": enhanced_prompt },
                    { "type": "image_url", "image_url": { "url": data_url } }
                ]
            }));
        } else if let Some(i_bytes) = image_bytes {
            use base64::Engine;
            let b64_img = base64::engine::general_purpose::STANDARD.encode(&i_bytes);
            let i_m = mime_type.unwrap_or("image/jpeg");
            let data_url = format!("data:{i_m};base64,{b64_img}");
            messages.push(json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": enhanced_prompt },
                    { "type": "image_url", "image_url": { "url": data_url, "detail": "auto" } }
                ]
            }));
        } else if let Some(a_bytes) = audio_bytes {
            use base64::Engine;
            let b64_audio = base64::engine::general_purpose::STANDARD.encode(&a_bytes);
            let fmt = if audio_mime.unwrap_or("").contains("ogg") { "ogg" } else { "mp3" };
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

        let url = format!("{}/chat/completions", provider.endpoint);
        let m_lower = model.to_lowercase();
        let max_output_tokens: usize = if m_lower.contains("gemini") {
            65536
        } else if m_lower.contains("claude") {
            64000
        } else if ["o1", "o3", "gpt-4o", "gpt-5", "sol", "terra", "luna"].iter().any(|k| m_lower.contains(k)) {
            65536
        } else {
            16384
        };

        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&json!({
                "model": model,
                "messages": messages,
                "stream": true,
                "max_tokens": max_output_tokens,
            }))
            .timeout(Duration::from_secs(180));

        if !provider.api_key.is_empty() && !["none", "-", "no"].iter().any(|k| provider.api_key.eq_ignore_ascii_case(k)) {
            req = req.header("Authorization", format!("Bearer {}", provider.api_key));
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                error!("Error sending AI completion request: {e}");
                if let Some(tl) = timeline {
                    tl.fail_current(e.to_string()).await;
                    tl.sync_draft(true).await;
                }
                return (None, format!("⚠️ Terjadi kendala saat memproses jawaban AI: {e}"));
            }
        };

        if !resp.status().is_success() {
            let status_code = resp.status().as_u16();
            let err_txt = resp.text().await.unwrap_or_default();
            error!("AI endpoint returned status {status_code}: {err_txt}");
            if let Some(tl) = timeline {
                tl.fail_current(format!("API status {status_code}")).await;
                tl.sync_draft(true).await;
            }
            return (None, format!("⚠️ Gagal menghubungi AI proxy: {status_code}"));
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut accumulated_raw = String::new();
        let mut accumulated_reasoning = String::new();
        let mut last_activity: Option<ProgressActivity> = None;
        let mut has_started_answer = false;

        while let Some(item) = stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(e) => {
                    warn!("Stream chunk error: {e}");
                    break;
                }
            };

            let chunk_str = String::from_utf8_lossy(&bytes);
            buffer.push_str(&chunk_str);

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].trim().to_string();
                buffer.drain(..=newline_pos);

                if !line.starts_with("data: ") {
                    continue;
                }
                let line_data = &line[6..].trim();
                if *line_data == "[DONE]" {
                    break;
                }

                if let Ok(data) = serde_json::from_str::<Value>(line_data) {
                    if let Some(choice) = data.get("choices").and_then(|c| c.get(0)) {
                        if let Some(delta) = choice.get("delta") {
                            let content_chunk = delta.get("content").and_then(|s| s.as_str()).unwrap_or("");
                            let reasoning_chunk = delta.get("reasoning_content").and_then(|s| s.as_str()).unwrap_or("");

                            if !reasoning_chunk.is_empty() {
                                accumulated_reasoning.push_str(reasoning_chunk);
                                if let Some(tl) = timeline {
                                    let mut window_start = accumulated_reasoning.len().saturating_sub(150);
                                    while window_start > 0 && !accumulated_reasoning.is_char_boundary(window_start) {
                                        window_start += 1;
                                    }
                                    let act = classify_text_activity(&accumulated_reasoning[window_start..]);
                                    if Some(act) != last_activity {
                                        last_activity = Some(act);
                                        tl.add_action(act.display_name(), Some(act)).await;
                                        tl.sync_draft(false).await;
                                    }
                                }
                            }

                            if !content_chunk.is_empty() {
                                accumulated_raw.push_str(content_chunk);

                                if accumulated_raw.contains("<think>") {
                                    if !accumulated_raw.contains("</think>") {
                                        if let Some(tl) = timeline {
                                            if let Some(after) = accumulated_raw.split("<think>").nth(1) {
                                                let mut window_start = after.len().saturating_sub(150);
                                                while window_start > 0 && !after.is_char_boundary(window_start) {
                                                    window_start += 1;
                                                }
                                                let act = classify_text_activity(&after[window_start..]);
                                                if Some(act) != last_activity {
                                                    last_activity = Some(act);
                                                    tl.add_action(act.display_name(), Some(act)).await;
                                                    tl.sync_draft(false).await;
                                                }
                                            }
                                        }
                                    } else if !has_started_answer {
                                        has_started_answer = true;
                                        last_activity = Some(ProgressActivity::Writing);
                                        if let Some(tl) = timeline {
                                            tl.add_action("Writing", Some(ProgressActivity::Writing)).await;
                                            tl.sync_draft(false).await;
                                        }
                                    }
                                } else if !has_started_answer {
                                    has_started_answer = true;
                                    last_activity = Some(ProgressActivity::Writing);
                                    if let Some(tl) = timeline {
                                        tl.add_action("Writing", Some(ProgressActivity::Writing)).await;
                                        tl.sync_draft(false).await;
                                    }
                                }
                            }
                        }
                    }
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
                answer_text = think_re.replace_all(&accumulated_raw, "").trim().to_string();
            } else if accumulated_raw.contains("<think>") {
                let parts: Vec<&str> = accumulated_raw.split("<think>").collect();
                let before_think = parts[0].trim();
                let inside_think = parts.get(1).copied().unwrap_or("").trim();
                if !inside_think.is_empty() && before_think.is_empty() {
                    thinking_text = Some(inside_think.to_string());
                    answer_text = inside_think.to_string();
                } else {
                    thinking_text = Some(inside_think.to_string());
                    answer_text = before_think.to_string();
                }
            }
        }

        let tag_clean_re = regex::Regex::new(r"(?i)</?think>").unwrap();
        answer_text = tag_clean_re.replace_all(&answer_text, "").trim().to_string();

        if answer_text.is_empty() {
            if let Some(ref th) = thinking_text {
                answer_text = tag_clean_re.replace_all(th, "").trim().to_string();
            }
        }

        if answer_text.is_empty() {
            answer_text = "Maaf, respon AI kosong untuk permintaan ini.".to_string();
        }

        if let Some(tl) = timeline {
            if !has_started_answer {
                tl.add_action("Writing", Some(ProgressActivity::Writing)).await;
                tl.sync_draft(true).await;
            }
            tl.finish_all(ProgressState::Done).await;
        }

        // Auto-update session name on first turn
        let idx = self.get_active_session_index(user_id).await;
        let mut sessions_map = self.user_sessions.write().await;
        if let Some(list) = sessions_map.get_mut(&user_id) {
            if let Some(sess) = list.get_mut(idx) {
                if sess.messages.is_empty() && sess.name.starts_with("Session ") {
                    let clean_title = prompt.trim().replace('\n', " ");
                    let short_title = if clean_title.len() > 35 {
                        let mut end = 32;
                        while end < clean_title.len() && !clean_title.is_char_boundary(end) {
                            end += 1;
                        }
                        format!("{}...", &clean_title[..end])
                    } else {
                        clean_title
                    };
                    if !short_title.is_empty() {
                        sess.name = short_title;
                    }
                }
                sess.messages.push(ChatMessage {
                    role: "user".to_string(),
                    content: Value::String(prompt.to_string()),
                });
                sess.messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: Value::String(answer_text.clone()),
                });
            }
        }

        (thinking_text, answer_text)
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
                let gen_url = format!("{}/images/generations", p.endpoint);
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

                if !p.api_key.is_empty() && !["none", "-", "no"].iter().any(|k| p.api_key.eq_ignore_ascii_case(k)) {
                    req = req.header("Authorization", format!("Bearer {}", p.api_key));
                }

                if let Ok(resp) = req.send().await {
                    if resp.status().is_success() {
                        if let Ok(res_json) = resp.json::<Value>().await {
                            if let Some(data) = res_json.get("data").and_then(|d| d.get(0)) {
                                if let Some(b64_str) = data.get("b64_json").and_then(|s| s.as_str()) {
                                    use base64::Engine;
                                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64_str) {
                                        return (true, Some(bytes), format!("OpenAI Compatible ({})", p.name));
                                    }
                                } else if let Some(img_url) = data.get("url").and_then(|s| s.as_str()) {
                                    if let Ok(img_resp) = self.client.get(img_url).send().await {
                                        if img_resp.status().is_success() {
                                            if let Ok(bytes) = img_resp.bytes().await {
                                                return (true, Some(bytes.to_vec()), format!("OpenAI Compatible ({})", p.name));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Universal High-Performance Fallback Engine (FLUX.1 via Pollinations)
        let encoded_prompt = urlencoding::encode(clean_prompt);
        let poll_url = format!(
            "https://image.pollinations.ai/prompt/{}?width={}&height={}&model=flux&nologo=true&enhance=true",
            encoded_prompt, width, height
        );

        match self.client.get(&poll_url).timeout(Duration::from_secs(60)).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    match resp.bytes().await {
                        Ok(bytes) if bytes.len() > 1000 => (true, Some(bytes.to_vec()), "FLUX.1 (Ultra HD)".to_string()),
                        _ => (false, None, "Respon gambar rusak atau terlalu kecil.".to_string()),
                    }
                } else {
                    (false, None, format!("HTTP Error {} saat membuat gambar.", status.as_u16()))
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    (false, None, "Waktu generate gambar habis (Timeout). Silakan coba lagi.".to_string())
                } else {
                    (false, None, format!("Gagal membuat gambar: {e}"))
                }
            }
        }
    }
}
