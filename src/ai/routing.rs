use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Main,
    Vision,
    Video,
    AudioStt,
    ImageGeneration,
}

impl ModelRole {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "main" => Some(Self::Main),
            "vision" => Some(Self::Vision),
            "video" => Some(Self::Video),
            "audio_stt" | "audio-stt" | "stt" | "audio" => Some(Self::AudioStt),
            "image_gen" | "image-gen" | "image_generation" | "image-generation" | "image" => {
                Some(Self::ImageGeneration)
            }
            _ => None,
        }
    }

    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Vision => "vision",
            Self::Video => "video",
            Self::AudioStt => "audio_stt",
            Self::ImageGeneration => "image_gen",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Main => "Main Model",
            Self::Vision => "Vision Model",
            Self::Video => "Video Model",
            Self::AudioStt => "Audio STT Model",
            Self::ImageGeneration => "Image Generation Model",
        }
    }

    pub fn addon_roles() -> [Self; 4] {
        [Self::Vision, Self::Video, Self::AudioStt, Self::ImageGeneration]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelRoute {
    MainModel,
    Specific { provider_id: String, model: String },
    Disabled,
}

impl Default for ModelRoute {
    fn default() -> Self {
        Self::MainModel
    }
}

impl ModelRoute {
    pub fn display(&self) -> String {
        match self {
            Self::MainModel => "Main Model".to_string(),
            Self::Specific { provider_id, model } => format!("{provider_id} / {model}"),
            Self::Disabled => "Disabled".to_string(),
        }
    }

    pub fn referenced_provider(&self) -> Option<&str> {
        match self {
            Self::Specific { provider_id, .. } => Some(provider_id),
            Self::MainModel | Self::Disabled => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOrigin {
    Main,
    MainModel,
    Specific,
}

#[derive(Debug, Clone)]
pub struct ResolvedModelRoute {
    pub role: ModelRole,
    pub provider: crate::ai::storage::ProviderConfig,
    pub model: String,
    pub capability: crate::ai::storage::CapabilityRecord,
    pub route_origin: RouteOrigin,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRoutingConfig {
    pub version: u32,
    #[serde(default)]
    pub vision: ModelRoute,
    #[serde(default)]
    pub video: ModelRoute,
    #[serde(default)]
    pub audio_stt: ModelRoute,
    #[serde(default)]
    pub image_gen: ModelRoute,
}

impl Default for ModelRoutingConfig {
    fn default() -> Self {
        Self {
            version: 1,
            vision: ModelRoute::MainModel,
            video: ModelRoute::MainModel,
            audio_stt: ModelRoute::MainModel,
            image_gen: ModelRoute::MainModel,
        }
    }
}

impl ModelRoutingConfig {
    pub fn route(&self, role: ModelRole) -> Option<&ModelRoute> {
        match role {
            ModelRole::Main => None,
            ModelRole::Vision => Some(&self.vision),
            ModelRole::Video => Some(&self.video),
            ModelRole::AudioStt => Some(&self.audio_stt),
            ModelRole::ImageGeneration => Some(&self.image_gen),
        }
    }

    pub fn set_route(&mut self, role: ModelRole, route: ModelRoute) -> Result<(), String> {
        match role {
            ModelRole::Main => Err("Main Model is configured through the main model selector".into()),
            ModelRole::Vision => {
                self.vision = route;
                Ok(())
            }
            ModelRole::Video => {
                self.video = route;
                Ok(())
            }
            ModelRole::AudioStt => {
                self.audio_stt = route;
                Ok(())
            }
            ModelRole::ImageGeneration => {
                self.image_gen = route;
                Ok(())
            }
        }
    }

    pub fn roles_using_provider(&self, provider_id: &str) -> Vec<ModelRole> {
        ModelRole::addon_roles()
            .into_iter()
            .filter(|role| {
                self.route(*role)
                    .and_then(ModelRoute::referenced_provider)
                    .is_some_and(|id| id == provider_id)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_live_main_model_routes() {
        let config = ModelRoutingConfig::default();
        for role in ModelRole::addon_roles() {
            assert_eq!(config.route(role), Some(&ModelRoute::MainModel));
        }
    }

    #[test]
    fn specific_provider_dependencies_are_detected() {
        let mut config = ModelRoutingConfig::default();
        config
            .set_route(
                ModelRole::ImageGeneration,
                ModelRoute::Specific {
                    provider_id: "together".into(),
                    model: "flux".into(),
                },
            )
            .unwrap();
        assert_eq!(
            config.roles_using_provider("together"),
            vec![ModelRole::ImageGeneration]
        );
    }
}
