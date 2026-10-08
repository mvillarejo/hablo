//! Provider-agnostic transcription surface: Gladia live streaming or an
//! OpenAI-compatible batch endpoint (local Ollama by default).

use crate::batch_stt::{BatchClient, BatchSettings, RequestOptions};
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
    /// Unknown values fall back to Gladia so a hand-edited config never bricks dictation.
    pub fn from_config(value: &str) -> Self {
        match value {
            PROVIDER_OPENAI_COMPAT => Provider::OpenAiCompat,
            PROVIDER_GLADIA => Provider::Gladia,
            other => {
                log::warn!("[transcriber] unknown provider {other:?}; using Gladia");
                Provider::Gladia
            }
        }
    }
}

/// Inputs for one dictation session; each provider uses the fields it needs.
pub struct SessionParams {
    pub api_key: String,
    pub languages: Option<Vec<String>>,
    pub code_switching: bool,
    pub custom_vocabulary: Vec<CustomVocabEntry>,
    pub endpointing: f64,
    pub region: &'static str,
    pub batch: BatchSettings,
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
                let options = RequestOptions::from_session(
                    params.languages.as_deref().unwrap_or_default(),
                    params.custom_vocabulary,
                );
                Ok(c.init_session(params.batch, options))
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

    #[test]
    fn provider_parses_config_values() {
        assert_eq!(Provider::from_config("gladia"), Provider::Gladia);
        assert_eq!(
            Provider::from_config("openai_compat"),
            Provider::OpenAiCompat
        );
        assert_eq!(Provider::from_config(""), Provider::Gladia);
        assert_eq!(Provider::from_config("whisper.cpp"), Provider::Gladia);
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
