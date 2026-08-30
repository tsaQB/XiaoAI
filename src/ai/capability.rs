use serde::{Deserialize, Serialize};

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
        let thinking =
            m.contains("high") || m.contains("pro") || m.contains("3.7") || m.contains("3.6");

        ModelCapability {
            model_name: model_name.to_string(),
            family: "Google Gemini 3.x Multimodal".to_string(),
            provider_icon: "✨".to_string(),
            context_limit,
            context_str,
            vision: true,
            vision_desc: "⚠️ Perkiraan dari nama model: vision kemungkinan didukung; verifikasi lewat metadata/probe provider".to_string(),
            video: true,
            video_desc: "⚠️ Perkiraan dari nama model: video mungkin didukung; verifikasi provider".to_string(),
            documents: true,
            docs_desc: "✅ Pipeline Xiao: TXT/MD/JSON/kode, PDF, DOCX, XLSX; PDF scan memakai vision jika renderer tersedia".to_string(),
            audio: true,
            audio_desc: "⚠️ Perkiraan dari nama model: audio mungkin didukung; verifikasi provider".to_string(),
            thinking,
            thinking_desc: "⚠️ Reasoning diperkirakan dari nama model; chain-of-thought tidak ditampilkan".to_string(),
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
            vision_desc: "⚠️ Perkiraan dari nama model: vision kemungkinan didukung; verifikasi provider".to_string(),
            video: false,
            video_desc: "❌ Belum Didukung Langsung".to_string(),
            documents: true,
            docs_desc: "✅ Pipeline Xiao: TXT/MD/JSON/kode, PDF, DOCX, XLSX; PDF scan memakai vision jika renderer tersedia".to_string(),
            audio: false,
            audio_desc: "❌ Belum Didukung Langsung (Ketik via teks / butuh Whisper)".to_string(),
            thinking: true,
            thinking_desc: "⚠️ Reasoning mungkin tersedia; chain-of-thought tidak ditampilkan".to_string(),
            strengths: "Kualitas penulisan prosa alami, pemahaman instruksi kompleks, arsitektur & refactoring kode tingkat lanjut".to_string(),
        }
    } else if ["gpt", "codex", "o1", "o3"].iter().any(|k| m.contains(k)) {
        let (context_limit, context_str) =
            if m.contains("sol") || m.contains("terra") || m.contains("luna") || m.contains("256k")
            {
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
            vision_desc: if vision {
                "⚠️ Perkiraan dari nama model: vision kemungkinan didukung; verifikasi provider"
                    .to_string()
            } else {
                "❌ Model Khusus Kode".to_string()
            },
            video: false,
            video_desc: "❌ Belum Didukung Langsung".to_string(),
            documents: true,
            docs_desc: "✅ Pipeline Xiao: TXT/MD/JSON/kode, PDF, DOCX, XLSX; PDF scan memakai vision jika renderer tersedia".to_string(),
            audio,
            audio_desc: if audio {
                "⚠️ Perkiraan dari nama model: audio mungkin didukung; verifikasi provider"
                    .to_string()
            } else {
                "❌ Belum Didukung Langsung (Ketik via teks)".to_string()
            },
            thinking: true,
            thinking_desc:
                "⚠️ Reasoning diperkirakan dari nama model; chain-of-thought tidak ditampilkan"
                    .to_string(),
            strengths:
                "Presisi logika matematika, sintesis & perbaikan kode, manipulasi data terstruktur"
                    .to_string(),
        }
    } else if m.contains("minimax") {
        ModelCapability {
            model_name: model_name.to_string(),
            family: "MiniMax Multimodal (M3 / 01 / Text)".to_string(),
            provider_icon: "🦁".to_string(),
            context_limit: 245760,
            context_str: "245,760 tokens (245k Luas)".to_string(),
            vision: true,
            vision_desc:
                "⚠️ Perkiraan dari nama model: vision kemungkinan didukung; verifikasi provider"
                    .to_string(),
            video: true,
            video_desc: "⚠️ Perkiraan dari nama model: video mungkin didukung; verifikasi provider"
                .to_string(),
            documents: true,
            docs_desc: "✅ Pipeline Xiao: TXT/MD/JSON/kode, PDF, DOCX, XLSX; PDF scan memakai vision jika renderer tersedia".to_string(),
            audio: false,
            audio_desc: "❌ Belum Didukung Langsung (Ketik via teks / Whisper)".to_string(),
            thinking: false,
            thinking_desc: "Standar (High Efficiency)".to_string(),
            strengths:
                "Pemahaman sekuens visual & video, konteks panjang 245k, performa respons cepat"
                    .to_string(),
        }
    } else if m.contains("qwen") {
        let is_video =
            m.contains("vl") || m.contains("qvq") || m.contains("vision") || m.contains("video");
        let is_thinking =
            m.contains("qvq") || m.contains("think") || m.contains("r1") || m.contains("reason");
        ModelCapability {
            model_name: model_name.to_string(),
            family: "Qwen 2.5 / 2.0 (Alibaba)".to_string(),
            provider_icon: "👑".to_string(),
            context_limit: 131072,
            context_str: "131,072 tokens (128k Luas)".to_string(),
            vision: true,
            vision_desc: "⚠️ Perkiraan nama model (Qwen-VL); verifikasi provider".to_string(),
            video: is_video,
            video_desc: if is_video { "⚠️ Perkiraan nama model: video mungkin didukung; verifikasi provider".to_string() } else { "❌ Belum Didukung Langsung".to_string() },
            documents: true,
            docs_desc: "✅ Pipeline Xiao: TXT/MD/JSON/kode, PDF, DOCX, XLSX; PDF scan memakai vision jika renderer tersedia".to_string(),
            audio: false,
            audio_desc: "❌ Belum Didukung Langsung".to_string(),
            thinking: is_thinking,
            thinking_desc: if is_thinking { "⚠️ Reasoning diperkirakan dari nama model; chain-of-thought tidak ditampilkan".to_string() } else { "Standar (Direct Prompting)".to_string() },
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
            vision_desc: if vision {
                "⚠️ Perkiraan nama model (DeepSeek-VL); verifikasi provider".to_string()
            } else {
                "❌ Model Teks Murni".to_string()
            },
            video: false,
            video_desc: "❌ Tidak Didukung".to_string(),
            documents: true,
            docs_desc: "✅ Pipeline Xiao: TXT/MD/JSON/kode, PDF, DOCX, XLSX; PDF scan memakai vision jika renderer tersedia".to_string(),
            audio: false,
            audio_desc: "❌ Tidak Didukung (Ketik via teks)".to_string(),
            thinking,
            thinking_desc: if thinking {
                "⚠️ Reasoning diperkirakan dari nama model; chain-of-thought tidak ditampilkan"
                    .to_string()
            } else {
                "Standar (Direct Prompting)".to_string()
            },
            strengths:
                "Kemampuan matematika murni, algoritma pemrograman, logika penalaran terbuka"
                    .to_string(),
        }
    } else {
        let has_video = m.contains("video")
            || m.contains("vl")
            || m.contains("vision")
            || m.contains("omni")
            || m.contains("qvq")
            || m.contains("pixtral")
            || m.contains("internvl")
            || m.contains("cogvlm")
            || m.contains("m3")
            || m.contains("m2");
        let has_audio = m.contains("audio")
            || m.contains("voice")
            || m.contains("realtime")
            || m.contains("omni");
        let has_thinking =
            m.contains("think") || m.contains("reason") || m.contains("r1") || m.contains("qvq");

        ModelCapability {
            model_name: model_name.to_string(),
            family: "OpenAI-Compatible Model (capability unknown until probed)".to_string(),
            provider_icon: "⚡".to_string(),
            context_limit: 128000,
            context_str: "128,000 tokens (128k)".to_string(),
            vision: false,
            vision_desc: "⚪ Unknown: metadata/probe provider belum membuktikan dukungan vision".to_string(),
            video: has_video,
            video_desc: if has_video { "⚠️ Perkiraan nama model: video mungkin didukung; verifikasi provider".to_string() } else { "❌ Belum Didukung Langsung".to_string() },
            documents: true,
            docs_desc: "✅ Pipeline Xiao: TXT/MD/JSON/kode, PDF, DOCX, XLSX; PDF scan memakai vision jika renderer tersedia".to_string(),
            audio: has_audio,
            audio_desc: if has_audio { "⚠️ Perkiraan dari nama model: audio mungkin didukung; verifikasi provider".to_string() } else { "❌ Belum Didukung Langsung".to_string() },
            thinking: has_thinking,
            thinking_desc: if has_thinking { "⚠️ Reasoning mungkin tersedia; chain-of-thought tidak ditampilkan".to_string() } else { "Standar (Direct Prompting)".to_string() },
            strengths: "Kemampuan model belum dipastikan; gunakan metadata/probe provider sebagai sumber utama".to_string(),
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

pub fn model_metadata_key(endpoint: &str, model: &str) -> String {
    format!("{}::{}", endpoint.trim_end_matches('/'), model)
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

pub fn get_model_capabilities_with_meta(
    model_name: &str,
    meta: Option<&ModelMetadata>,
) -> ModelCapability {
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
            cap.vision = mod_low.contains("image")
                || mod_low.contains("vision")
                || mod_low.contains("multimodal");
            cap.video = mod_low.contains("video");
            cap.audio = mod_low.contains("audio");
            cap.vision_desc = if cap.vision {
                "✅ Dipublikasikan oleh metadata endpoint"
            } else {
                "⚪ Tidak dipublikasikan oleh metadata endpoint"
            }
            .to_string();
            cap.video_desc = if cap.video {
                "✅ Dipublikasikan oleh metadata endpoint"
            } else {
                "⚪ Tidak dipublikasikan oleh metadata endpoint"
            }
            .to_string();
            cap.audio_desc = if cap.audio {
                "✅ Dipublikasikan oleh metadata endpoint"
            } else {
                "⚪ Tidak dipublikasikan oleh metadata endpoint"
            }
            .to_string();
        }
    }

    cap
}
