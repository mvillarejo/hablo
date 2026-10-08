//! Provider-agnostic transcription surface: Gladia live streaming or an
//! OpenAI-compatible batch endpoint (local Ollama by default).

use crate::batch_stt::{BatchClient, BatchSettings, RequestOptions};
use crate::config::{Profile, ProfileSettings};
use crate::gladia::GladiaClient;
use crate::vocabulary::CustomVocabEntry;
use serde::Serialize;
use tokio::sync::broadcast;

pub const PROVIDER_GLADIA: &str = "gladia";
pub const PROVIDER_OPENAI_COMPAT: &str = "openai_compat";

#[derive(Debug, Clone, Serialize)]
pub enum TranscriptionEvent {
    Partial(String),
    Final {
        text: String,
        start: f64,
        end: f64,
    },
    /// The session ended abnormally (network drop, protocol error). Carries a
    /// human-readable reason so the frontend can surface it. A normal,
    /// server-initiated close emits `SessionEnded` only — never `Error`.
    Error(String),
    SessionEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Gladia,
    OpenAiCompat,
}

impl Provider {
    pub fn for_profile(settings: &ProfileSettings) -> Self {
        match settings {
            ProfileSettings::Gladia { .. } => Provider::Gladia,
            ProfileSettings::OpenAiCompat { .. } => Provider::OpenAiCompat,
        }
    }
}

/// Inputs for one dictation session; each provider uses the fields it needs.
#[derive(Debug)]
pub struct SessionParams {
    pub api_key: String,
    pub languages: Option<Vec<String>>,
    pub code_switching: bool,
    pub custom_vocabulary: Vec<CustomVocabEntry>,
    pub endpointing: f64,
    pub region: &'static str,
    pub batch: Option<BatchSettings>,
}

impl SessionParams {
    /// Session inputs for `profile` plus the shared settings. A Gladia region of
    /// `"auto"` resolves to `detected_region` (timezone-based).
    pub fn for_profile(
        profile: &Profile,
        languages: Option<Vec<String>>,
        custom_vocabulary: Vec<CustomVocabEntry>,
        detected_region: &'static str,
    ) -> Self {
        match &profile.settings {
            ProfileSettings::Gladia {
                api_key,
                region,
                endpointing,
                code_switching,
            } => SessionParams {
                api_key: api_key.clone(),
                languages,
                code_switching: *code_switching,
                custom_vocabulary,
                endpointing: *endpointing,
                region: match region.as_str() {
                    "eu-west" => "eu-west",
                    "us-west" => "us-west",
                    _ => detected_region,
                },
                batch: None,
            },
            ProfileSettings::OpenAiCompat { .. } => SessionParams {
                api_key: String::new(),
                languages,
                code_switching: false,
                custom_vocabulary,
                endpointing: crate::config::DEFAULT_ENDPOINTING,
                region: detected_region,
                batch: profile.settings.batch_settings(),
            },
        }
    }
}

pub enum Transcriber {
    Gladia(GladiaClient),
    OpenAiCompat(BatchClient),
}

impl Transcriber {
    pub fn for_provider(provider: Provider) -> Self {
        match provider {
            Provider::Gladia => Transcriber::Gladia(GladiaClient::new()),
            Provider::OpenAiCompat => Transcriber::OpenAiCompat(BatchClient::new()),
        }
    }

    pub fn provider(&self) -> Provider {
        match self {
            Transcriber::Gladia(_) => Provider::Gladia,
            Transcriber::OpenAiCompat(_) => Provider::OpenAiCompat,
        }
    }

    /// Switch to `provider`, closing the current session first. Keeps the
    /// existing client (and any live Gladia session) when it already matches.
    pub async fn ensure_provider(&mut self, provider: Provider) {
        if self.provider() != provider {
            let _ = self.close_session().await;
            *self = Transcriber::for_provider(provider);
        }
    }

    pub async fn set_audio_format(&self, sample_rate: u32, channels: u32, bit_depth: u32) {
        match self {
            Transcriber::Gladia(c) => c.set_audio_format(sample_rate, channels, bit_depth).await,
            Transcriber::OpenAiCompat(c) => c.set_audio_format(sample_rate),
        }
    }

    pub async fn init_session(&self, params: SessionParams) -> Result<String, String> {
        match self {
            Transcriber::Gladia(c) => c
                .init_session(
                    &params.api_key,
                    params.languages,
                    params.code_switching,
                    params.custom_vocabulary,
                    params.endpointing,
                    params.region,
                )
                .await
                .map_err(|e| e.to_string()),
            Transcriber::OpenAiCompat(c) => {
                let batch = params
                    .batch
                    .ok_or_else(|| "Missing OpenAI-compatible endpoint settings".to_string())?;
                let options = RequestOptions::from_session(
                    params.languages.as_deref().unwrap_or_default(),
                    params.custom_vocabulary,
                );
                Ok(c.init_session(batch, options))
            }
        }
    }

    pub async fn send_audio(&self, audio_data: &[u8]) -> Result<(), String> {
        match self {
            Transcriber::Gladia(c) => c.send_audio(audio_data).await.map_err(|e| e.to_string()),
            Transcriber::OpenAiCompat(c) => {
                c.send_audio(audio_data);
                Ok(())
            }
        }
    }

    pub async fn stop_recording(&self) -> Result<(), String> {
        match self {
            Transcriber::Gladia(c) => c.stop_recording().await.map_err(|e| e.to_string()),
            Transcriber::OpenAiCompat(c) => {
                c.stop_recording();
                Ok(())
            }
        }
    }

    pub async fn close_session(&self) -> Result<(), String> {
        match self {
            Transcriber::Gladia(c) => c.close_session().await.map_err(|e| e.to_string()),
            Transcriber::OpenAiCompat(c) => {
                c.close_session().await;
                Ok(())
            }
        }
    }

    pub async fn subscribe_to_transcriptions(&self) -> broadcast::Receiver<TranscriptionEvent> {
        match self {
            Transcriber::Gladia(c) => c.subscribe_to_transcriptions().await,
            Transcriber::OpenAiCompat(c) => c.subscribe_to_transcriptions(),
        }
    }

    pub async fn current_session_id(&self) -> Option<String> {
        match self {
            Transcriber::Gladia(c) => c.current_session_id().await,
            Transcriber::OpenAiCompat(c) => c.current_session_id(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(settings: ProfileSettings) -> Profile {
        Profile {
            id: "p".to_string(),
            name: "P".to_string(),
            settings,
        }
    }

    fn gladia(region: &str) -> Profile {
        profile(ProfileSettings::Gladia {
            api_key: "g-key".to_string(),
            region: region.to_string(),
            endpointing: 0.4,
            code_switching: true,
        })
    }

    fn local() -> Profile {
        profile(ProfileSettings::OpenAiCompat {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "gemma4:e4b".to_string(),
            api_key: String::new(),
        })
    }

    #[test]
    fn provider_follows_the_profile_kind() {
        assert_eq!(
            Provider::for_profile(&gladia("auto").settings),
            Provider::Gladia
        );
        assert_eq!(
            Provider::for_profile(&local().settings),
            Provider::OpenAiCompat
        );
    }

    #[test]
    fn session_params_come_from_a_gladia_profile() {
        let langs = Some(vec!["en".to_string()]);
        let params = SessionParams::for_profile(&gladia("auto"), langs.clone(), vec![], "eu-west");
        assert_eq!(params.api_key, "g-key");
        assert_eq!(params.endpointing, 0.4);
        assert!(params.code_switching);
        assert_eq!(params.region, "eu-west");
        assert_eq!(params.languages, langs);
        assert!(params.batch.is_none());
        let params = SessionParams::for_profile(&gladia("us-west"), None, vec![], "eu-west");
        assert_eq!(params.region, "us-west");
        let params = SessionParams::for_profile(&gladia("eu-west"), None, vec![], "us-west");
        assert_eq!(params.region, "eu-west");
    }

    #[test]
    fn session_params_come_from_an_openai_compat_profile() {
        let params = SessionParams::for_profile(&local(), None, vec![], "eu-west");
        let batch = params.batch.expect("batch settings");
        assert_eq!(batch.base_url, "http://localhost:11434/v1");
        assert_eq!(batch.model, "gemma4:e4b");
        assert_eq!(batch.api_key, None);
        assert!(params.api_key.is_empty());
    }

    #[tokio::test]
    async fn switching_active_profile_kind_swaps_the_client() {
        let mut transcriber = Transcriber::for_provider(Provider::Gladia);
        for (profile, expected) in [
            (local(), Provider::OpenAiCompat),
            (gladia("auto"), Provider::Gladia),
        ] {
            transcriber
                .ensure_provider(Provider::for_profile(&profile.settings))
                .await;
            assert_eq!(transcriber.provider(), expected);
        }
    }

    #[tokio::test]
    async fn batch_session_without_endpoint_settings_is_an_error() {
        let transcriber = Transcriber::for_provider(Provider::OpenAiCompat);
        let mut params = SessionParams::for_profile(&local(), None, vec![], "eu-west");
        params.batch = None;
        assert!(transcriber.init_session(params).await.is_err());
    }

    #[test]
    fn for_provider_builds_the_matching_client() {
        assert!(matches!(
            Transcriber::for_provider(Provider::Gladia),
            Transcriber::Gladia(_)
        ));
        assert!(matches!(
            Transcriber::for_provider(Provider::OpenAiCompat),
            Transcriber::OpenAiCompat(_)
        ));
    }

    #[tokio::test]
    async fn ensure_provider_swaps_only_when_it_differs() {
        let mut transcriber = Transcriber::for_provider(Provider::Gladia);
        transcriber.ensure_provider(Provider::Gladia).await;
        assert_eq!(transcriber.provider(), Provider::Gladia);
        transcriber.ensure_provider(Provider::OpenAiCompat).await;
        assert_eq!(transcriber.provider(), Provider::OpenAiCompat);
        transcriber.ensure_provider(Provider::Gladia).await;
        assert_eq!(transcriber.provider(), Provider::Gladia);
    }
}
