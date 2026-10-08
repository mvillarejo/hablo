//! OpenAI-compatible batch speech-to-text provider (`POST {base}/audio/transcriptions`).
//!
//! Audio is buffered in memory while recording; on stop it is wrapped as a mono
//! 16-bit WAV, split into chunks of at most `MAX_CHUNK_SECONDS`, and transcribed
//! one chunk after another. Defaults target local Ollama, but any compatible
//! endpoint (Groq, OpenAI) works by changing base URL, key and model.

use crate::transcriber::TranscriptionEvent;
use crate::vocabulary::{normalize_vocabulary, CustomVocabEntry};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Gemma 4 accepts at most 30 s of audio per request; keep a safety margin.
pub const MAX_CHUNK_SECONDS: u32 = 28;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const BYTES_PER_SAMPLE: usize = 2;

/// Endpoint settings for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

/// Per-session request options derived from the user's transcription settings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RequestOptions {
    pub language: Option<String>,
    pub prompt: Option<String>,
}

impl RequestOptions {
    /// `language` only when exactly one is configured (otherwise let the model
    /// auto-detect); `prompt` is the vocabulary terms joined with commas.
    pub fn from_session(languages: &[String], vocabulary: Vec<CustomVocabEntry>) -> Self {
        let languages: Vec<&String> = languages.iter().filter(|l| !l.trim().is_empty()).collect();
        let language = match languages.as_slice() {
            [only] => Some(only.trim().to_string()),
            _ => None,
        };
        let terms: Vec<String> = normalize_vocabulary(vocabulary)
            .into_iter()
            .map(|entry| entry.value.trim().to_string())
            .collect();
        let prompt = (!terms.is_empty()).then(|| terms.join(", "));
        Self { language, prompt }
    }
}

/// Everything needed to send one transcription request, minus the audio.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestParts {
    pub url: String,
    /// Text multipart fields, in order (the `file` part is added separately).
    pub fields: Vec<(&'static str, String)>,
    pub bearer: Option<String>,
}

pub fn build_request_parts(settings: &BatchSettings, options: &RequestOptions) -> RequestParts {
    let mut fields = vec![("model", settings.model.clone())];
    if let Some(language) = &options.language {
        fields.push(("language", language.clone()));
    }
    if let Some(prompt) = &options.prompt {
        fields.push(("prompt", prompt.clone()));
    }
    RequestParts {
        url: format!("{}/audio/transcriptions", trim_base_url(&settings.base_url)),
        fields,
        bearer: settings
            .api_key
            .as_ref()
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty()),
    }
}

fn trim_base_url(base_url: &str) -> &str {
    base_url.trim().trim_end_matches('/')
}

pub fn build_request(
    client: &reqwest::Client,
    parts: &RequestParts,
    wav: Vec<u8>,
) -> reqwest::RequestBuilder {
    let file = reqwest::multipart::Part::bytes(wav)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .expect("static mime type is valid");
    let mut form = reqwest::multipart::Form::new().part("file", file);
    for (name, value) in &parts.fields {
        form = form.text(*name, value.clone());
    }
    let mut request = client.post(&parts.url).multipart(form);
    if let Some(key) = &parts.bearer {
        request = request.bearer_auth(key);
    }
    request
}

/// Minimal RIFF/WAVE header for mono 16-bit PCM.
pub fn wav_bytes(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * BYTES_PER_SAMPLE as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&(BYTES_PER_SAMPLE as u16).to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Split PCM into chunks of at most `MAX_CHUNK_SECONDS`, on sample boundaries.
pub fn chunk_pcm(pcm: &[u8], sample_rate: u32) -> Vec<&[u8]> {
    let pcm = &pcm[..pcm.len() - pcm.len() % BYTES_PER_SAMPLE];
    let max_bytes = (sample_rate * MAX_CHUNK_SECONDS) as usize * BYTES_PER_SAMPLE;
    pcm.chunks(max_bytes.max(BYTES_PER_SAMPLE)).collect()
}

pub fn pcm_duration_seconds(pcm: &[u8], sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        return 0.0;
    }
    (pcm.len() / BYTES_PER_SAMPLE) as f64 / sample_rate as f64
}

/// Extract `text` from a transcription response. Empty text is `Ok("")`; the
/// caller decides whether the whole recording came back empty.
pub fn parse_transcription_response(body: &str) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        format!(
            "Unexpected response from transcription server: {}",
            snippet(body)
        )
    })?;
    match value.get("text").and_then(|text| text.as_str()) {
        Some(text) => Ok(text.trim().to_string()),
        None => Err(format!(
            "Transcription response had no text: {}",
            snippet(body)
        )),
    }
}

/// Whether `model` is listed in an OpenAI-style `GET /models` response.
pub fn models_response_contains(body: &str, model: &str) -> Result<bool, String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| format!("Unexpected /models response: {}", snippet(body)))?;
    let data = value
        .get("data")
        .and_then(|data| data.as_array())
        .ok_or_else(|| format!("Unexpected /models response: {}", snippet(body)))?;
    Ok(data
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|id| id.as_str()))
        .any(|id| id == model))
}

/// "Can't reach Ollama at http://localhost:11434 — is it running?"
pub fn unreachable_message(base_url: &str) -> String {
    let (name, origin) = match url::Url::parse(trim_base_url(base_url)) {
        Ok(url) => {
            let name = if url.port() == Some(11434) {
                "Ollama"
            } else {
                "the transcription server"
            };
            (name, url.origin().ascii_serialization())
        }
        Err(_) => ("the transcription server", base_url.to_string()),
    };
    format!("Can't reach {name} at {origin} — is it running?")
}

fn http_error_message(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| error_detail(&value))
        .unwrap_or_else(|| snippet(body));
    format!("Transcription server returned {status}: {detail}")
}

/// `{"error":"msg"}` or `{"error":{"message":"msg"}}`. Ollama may wrap the
/// upstream error as a JSON string inside `error`, so unwrap one level of that.
fn error_detail(value: &serde_json::Value) -> Option<String> {
    let error = value.get("error")?;
    if let Some(message) = error.get("message").and_then(|m| m.as_str()) {
        return Some(message.to_string());
    }
    let text = error.as_str()?;
    Some(
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|inner| error_detail(&inner))
            .unwrap_or_else(|| text.to_string()),
    )
}

fn request_error_message(base_url: &str, error: &reqwest::Error) -> String {
    if error.is_connect() {
        unreachable_message(base_url)
    } else if error.is_timeout() {
        "The transcription server took too long to respond".to_string()
    } else {
        format!("Transcription request failed: {error}")
    }
}

fn snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() > 200 {
        format!("{}…", trimmed.chars().take(200).collect::<String>())
    } else {
        trimmed.to_string()
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Transcribe a full recording: chunk, send sequentially, join with spaces.
/// Returns `Ok(None)` when there is no audio (no request is made).
pub async fn transcribe_pcm(
    client: &reqwest::Client,
    settings: &BatchSettings,
    options: &RequestOptions,
    pcm: &[u8],
    sample_rate: u32,
) -> Result<Option<String>, String> {
    let chunks = chunk_pcm(pcm, sample_rate);
    if chunks.is_empty() {
        return Ok(None);
    }
    let parts = build_request_parts(settings, options);
    let mut texts = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let response = build_request(client, &parts, wav_bytes(chunk, sample_rate))
            .send()
            .await
            .map_err(|e| request_error_message(&settings.base_url, &e))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| request_error_message(&settings.base_url, &e))?;
        if !status.is_success() {
            return Err(http_error_message(status, &body));
        }
        let text = parse_transcription_response(&body)?;
        if !text.is_empty() {
            texts.push(text);
        }
    }
    if texts.is_empty() {
        return Err("No speech was recognized in the recording".to_string());
    }
    Ok(Some(texts.join(" ")))
}

/// `GET {base}/models` and check the configured model is listed.
pub async fn test_connection(settings: &BatchSettings) -> Result<String, String> {
    let mut request = http_client().get(format!("{}/models", trim_base_url(&settings.base_url)));
    if let Some(key) = settings.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
        request = request.bearer_auth(key.trim());
    }
    let response = request
        .send()
        .await
        .map_err(|e| request_error_message(&settings.base_url, &e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| request_error_message(&settings.base_url, &e))?;
    if !status.is_success() {
        return Err(http_error_message(status, &body));
    }
    if models_response_contains(&body, &settings.model)? {
        Ok(format!("Connected. Model {} is available.", settings.model))
    } else {
        Err(format!(
            "Connected, but model {} isn't available. For Ollama run: ollama pull {}",
            settings.model, settings.model
        ))
    }
}

#[derive(Default)]
struct SessionState {
    settings: Option<BatchSettings>,
    options: RequestOptions,
    sample_rate: u32,
    buffer: Vec<u8>,
    session_id: Option<String>,
    transcription_tx: Option<broadcast::Sender<TranscriptionEvent>>,
}

#[derive(Clone, Default)]
pub struct BatchClient {
    state: Arc<Mutex<SessionState>>,
    task_handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl BatchClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn init_session(&self, settings: BatchSettings, options: RequestOptions) -> String {
        let session_id = format!("local-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.6fZ"));
        let (tx, _rx) = broadcast::channel::<TranscriptionEvent>(16);
        let mut state = self.state.lock().unwrap();
        state.settings = Some(settings);
        state.options = options;
        state.buffer.clear();
        state.session_id = Some(session_id.clone());
        state.transcription_tx = Some(tx);
        session_id
    }

    pub fn set_audio_format(&self, sample_rate: u32) {
        self.state.lock().unwrap().sample_rate = sample_rate;
    }

    pub fn send_audio(&self, audio_data: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .buffer
            .extend_from_slice(audio_data);
    }

    pub fn current_session_id(&self) -> Option<String> {
        self.state.lock().unwrap().session_id.clone()
    }

    pub fn subscribe_to_transcriptions(&self) -> broadcast::Receiver<TranscriptionEvent> {
        match &self.state.lock().unwrap().transcription_tx {
            Some(tx) => tx.subscribe(),
            None => broadcast::channel(1).1,
        }
    }

    /// Hand the buffered recording to a background task that transcribes it and
    /// emits `Final` (or `Error`) followed by `SessionEnded`.
    pub fn stop_recording(&self) {
        let (settings, options, pcm, sample_rate, tx) = {
            let mut state = self.state.lock().unwrap();
            (
                state.settings.clone(),
                state.options.clone(),
                std::mem::take(&mut state.buffer),
                state.sample_rate,
                state.transcription_tx.clone(),
            )
        };
        let (Some(settings), Some(tx)) = (settings, tx) else {
            return;
        };
        let handle = tokio::spawn(async move {
            let duration = pcm_duration_seconds(&pcm, sample_rate);
            let started = std::time::Instant::now();
            log::info!(
                "[batch-stt] transcribing {duration:.1}s of audio with {} at {}",
                settings.model,
                settings.base_url
            );
            match transcribe_pcm(&http_client(), &settings, &options, &pcm, sample_rate).await {
                Ok(Some(text)) => {
                    log::info!(
                        "[batch-stt] transcribed in {}ms ({} chars)",
                        started.elapsed().as_millis(),
                        text.chars().count()
                    );
                    let _ = tx.send(TranscriptionEvent::Final {
                        text,
                        start: 0.0,
                        end: duration,
                    });
                }
                Ok(None) => log::info!("[batch-stt] no audio captured; skipping request"),
                Err(message) => {
                    log::error!("[batch-stt] {message}");
                    let _ = tx.send(TranscriptionEvent::Error(message));
                }
            }
            let _ = tx.send(TranscriptionEvent::SessionEnded);
        });
        *self.task_handle.lock().unwrap() = Some(handle);
    }

    pub async fn close_session(&self) {
        let handle = self.task_handle.lock().unwrap().take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
        let mut state = self.state.lock().unwrap();
        *state = SessionState {
            sample_rate: state.sample_rate,
            ..SessionState::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(api_key: Option<&str>) -> BatchSettings {
        BatchSettings {
            base_url: "http://localhost:11434/v1/".to_string(),
            model: "gemma4:e4b".to_string(),
            api_key: api_key.map(str::to_string),
        }
    }

    fn vocab(values: &[&str]) -> Vec<CustomVocabEntry> {
        values
            .iter()
            .map(|value| CustomVocabEntry {
                value: value.to_string(),
                pronunciations: None,
                language: None,
                intensity: 0.5,
            })
            .collect()
    }

    fn langs(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn wav_header_is_44_bytes_and_describes_mono_16bit_pcm() {
        let pcm = vec![1u8, 2, 3, 4, 5, 6];
        let wav = wav_bytes(&pcm, 16_000);
        assert_eq!(wav.len(), 44 + pcm.len());
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1); // mono
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u16::from_le_bytes(wav[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
        assert_eq!(&wav[44..], &pcm[..]);
    }

    #[test]
    fn wav_header_uses_capture_rate() {
        let wav = wav_bytes(&[], 48_000);
        assert_eq!(wav.len(), 44);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 96_000);
    }

    const RATE: u32 = 16_000;
    const MAX_BYTES: usize = (RATE * MAX_CHUNK_SECONDS) as usize * 2;

    #[test]
    fn empty_buffer_yields_no_chunks() {
        assert!(chunk_pcm(&[], RATE).is_empty());
        // A lone odd byte is not a whole sample.
        assert!(chunk_pcm(&[7], RATE).is_empty());
    }

    #[test]
    fn exactly_28_seconds_is_one_chunk() {
        let pcm = vec![0u8; MAX_BYTES];
        let chunks = chunk_pcm(&pcm, RATE);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), MAX_BYTES);
    }

    #[test]
    fn one_sample_over_28_seconds_splits_into_two() {
        let pcm = vec![0u8; MAX_BYTES + 2];
        let chunks = chunk_pcm(&pcm, RATE);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), MAX_BYTES);
        assert_eq!(chunks[1].len(), 2);
    }

    #[test]
    fn long_recording_chunks_stay_within_limit_and_on_sample_boundaries() {
        let pcm = vec![0u8; MAX_BYTES * 3 + 1001];
        let chunks = chunk_pcm(&pcm, 44_100);
        assert!(chunks.iter().all(|c| c.len() % 2 == 0));
        assert!(chunks
            .iter()
            .all(|c| pcm_duration_seconds(c, 44_100) <= MAX_CHUNK_SECONDS as f64));
        assert_eq!(chunks.iter().map(|c| c.len()).sum::<usize>(), pcm.len() - 1);
    }

    #[test]
    fn duration_is_computed_from_samples() {
        assert_eq!(pcm_duration_seconds(&vec![0u8; 32_000], 16_000), 1.0);
        assert_eq!(pcm_duration_seconds(&[0u8; 4], 0), 0.0);
    }

    #[test]
    fn language_is_sent_only_with_exactly_one_language() {
        assert_eq!(
            RequestOptions::from_session(&langs(&["es"]), vec![]).language,
            Some("es".to_string())
        );
        assert_eq!(
            RequestOptions::from_session(&langs(&["en", "es"]), vec![]).language,
            None
        );
        assert_eq!(RequestOptions::from_session(&[], vec![]).language, None);
        assert_eq!(
            RequestOptions::from_session(&langs(&["en", " "]), vec![]).language,
            Some("en".to_string())
        );
    }

    #[test]
    fn prompt_joins_vocabulary_terms_and_skips_blanks() {
        let options = RequestOptions::from_session(&[], vocab(&["TTM", " ", " Kubernetes "]));
        assert_eq!(options.prompt, Some("TTM, Kubernetes".to_string()));
        assert_eq!(
            RequestOptions::from_session(&[], vocab(&[" "])).prompt,
            None
        );
    }

    #[test]
    fn request_parts_include_only_configured_fields() {
        let minimal = build_request_parts(&settings(None), &RequestOptions::default());
        assert_eq!(
            minimal.url,
            "http://localhost:11434/v1/audio/transcriptions"
        );
        assert_eq!(minimal.fields, vec![("model", "gemma4:e4b".to_string())]);
        assert_eq!(minimal.bearer, None);

        let full = build_request_parts(
            &settings(Some(" sk-test ")),
            &RequestOptions {
                language: Some("en".to_string()),
                prompt: Some("TTM, Ollama".to_string()),
            },
        );
        assert_eq!(
            full.fields,
            vec![
                ("model", "gemma4:e4b".to_string()),
                ("language", "en".to_string()),
                ("prompt", "TTM, Ollama".to_string()),
            ]
        );
        assert_eq!(full.bearer, Some("sk-test".to_string()));
    }

    #[test]
    fn bearer_header_is_set_only_when_a_key_is_configured() {
        let client = reqwest::Client::new();
        for (key, expected) in [
            (None, None),
            (Some(""), None),
            (Some("   "), None),
            (Some("gsk_abc"), Some("Bearer gsk_abc")),
        ] {
            let parts = build_request_parts(&settings(key), &RequestOptions::default());
            let request = build_request(&client, &parts, wav_bytes(&[0, 0], RATE))
                .build()
                .unwrap();
            assert_eq!(request.method(), reqwest::Method::POST);
            assert_eq!(
                request
                    .headers()
                    .get(reqwest::header::AUTHORIZATION)
                    .map(|v| v.to_str().unwrap()),
                expected,
                "key={key:?}"
            );
            assert!(request
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("multipart/form-data"));
        }
    }

    #[test]
    fn parses_text_from_response() {
        assert_eq!(
            parse_transcription_response(r#"{"text":" Hello world. "}"#),
            Ok("Hello world.".to_string())
        );
        assert_eq!(
            parse_transcription_response(r#"{"text":""}"#),
            Ok(String::new())
        );
        assert!(parse_transcription_response(r#"{"error":"model not found"}"#).is_err());
        assert!(parse_transcription_response(r#"{"text":null}"#).is_err());
        assert!(parse_transcription_response("not json").is_err());
    }

    #[test]
    fn models_response_lookup() {
        let body = r#"{"object":"list","data":[{"id":"gemma4:latest"},{"id":"gemma4:e4b"}]}"#;
        assert_eq!(models_response_contains(body, "gemma4:e4b"), Ok(true));
        assert_eq!(
            models_response_contains(body, "whisper-large-v3"),
            Ok(false)
        );
        assert!(models_response_contains(r#"{"object":"list"}"#, "x").is_err());
        assert!(models_response_contains("<html>", "x").is_err());
    }

    #[test]
    fn unreachable_message_names_ollama_on_its_default_port() {
        assert_eq!(
            unreachable_message("http://localhost:11434/v1"),
            "Can't reach Ollama at http://localhost:11434 — is it running?"
        );
        assert_eq!(
            unreachable_message("https://api.groq.com/openai/v1"),
            "Can't reach the transcription server at https://api.groq.com — is it running?"
        );
    }

    #[test]
    fn http_error_message_prefers_the_server_error_field() {
        let status = reqwest::StatusCode::NOT_FOUND;
        assert_eq!(
            http_error_message(status, r#"{"error":"model 'x' not found"}"#),
            "Transcription server returned 404 Not Found: model 'x' not found"
        );
        assert_eq!(
            http_error_message(status, r#"{"error":{"message":"bad key"}}"#),
            "Transcription server returned 404 Not Found: bad key"
        );
        // Shape Ollama returns when its audio backend rejects the file.
        assert_eq!(
            http_error_message(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"{\"error\":{\"code\":400,\"message\":\"Failed to load image or audio file\"}}"}"#
            ),
            "Transcription server returned 400 Bad Request: Failed to load image or audio file"
        );
    }

    #[tokio::test]
    async fn empty_recording_makes_no_request() {
        // Port 9 (discard) would fail to connect if a request were attempted.
        let unreachable = BatchSettings {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "m".to_string(),
            api_key: None,
        };
        let result = transcribe_pcm(
            &reqwest::Client::new(),
            &unreachable,
            &RequestOptions::default(),
            &[],
            RATE,
        )
        .await;
        assert_eq!(result, Ok(None));
    }

    #[tokio::test]
    async fn connection_refused_becomes_a_clear_error() {
        let unreachable = BatchSettings {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "m".to_string(),
            api_key: None,
        };
        let result = transcribe_pcm(
            &reqwest::Client::new(),
            &unreachable,
            &RequestOptions::default(),
            &[0u8; 320],
            RATE,
        )
        .await;
        assert_eq!(
            result,
            Err(
                "Can't reach the transcription server at http://127.0.0.1:9 — is it running?"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn stop_with_empty_buffer_emits_only_session_ended() {
        let client = BatchClient::new();
        client.set_audio_format(RATE);
        let id = client.init_session(settings(None), RequestOptions::default());
        assert!(id.starts_with("local-"));
        assert_eq!(client.current_session_id(), Some(id));
        let mut rx = client.subscribe_to_transcriptions();
        client.stop_recording();
        assert!(matches!(
            rx.recv().await.unwrap(),
            TranscriptionEvent::SessionEnded
        ));
        client.close_session().await;
        assert_eq!(client.current_session_id(), None);
    }

    /// Real end-to-end check against a local Ollama. Run with:
    /// `TTM_STT_WAV=/path/to/16k-mono.wav cargo test --manifest-path src-tauri/Cargo.toml -- --ignored ollama --nocapture`
    #[tokio::test]
    #[ignore]
    async fn ollama_transcribes_a_real_wav() {
        let path = std::env::var("TTM_STT_WAV").expect("set TTM_STT_WAV to a 16-bit mono WAV");
        let wav = std::fs::read(&path).unwrap();
        // Walk the RIFF chunks: `say` writes JUNK/FLLR chunks around fmt/data.
        let (mut at, mut sample_rate, mut data_at) = (12, 0u32, 0);
        while at + 8 <= wav.len() {
            let len = u32::from_le_bytes(wav[at + 4..at + 8].try_into().unwrap()) as usize;
            match &wav[at..at + 4] {
                b"fmt " => {
                    sample_rate = u32::from_le_bytes(wav[at + 12..at + 16].try_into().unwrap())
                }
                b"data" => {
                    data_at = at + 8;
                    break;
                }
                _ => {}
            }
            at += 8 + len + len % 2;
        }
        assert!(sample_rate > 0 && data_at > 0, "not a PCM WAV");
        let settings = BatchSettings {
            base_url: std::env::var("TTM_STT_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".to_string()),
            model: std::env::var("TTM_STT_MODEL").unwrap_or_else(|_| "gemma4:e4b".to_string()),
            api_key: std::env::var("TTM_STT_API_KEY").ok(),
        };
        println!("test_connection: {:?}", test_connection(&settings).await);
        let started = std::time::Instant::now();
        let text = transcribe_pcm(
            &http_client(),
            &settings,
            &RequestOptions::from_session(&["en".to_string()], vocab(&["TTM"])),
            &wav[data_at..],
            sample_rate,
        )
        .await
        .unwrap()
        .unwrap();
        println!(
            "transcript: {text:?}\nlatency: {}ms for {:.1}s of audio",
            started.elapsed().as_millis(),
            pcm_duration_seconds(&wav[data_at..], sample_rate)
        );
        assert!(!text.is_empty());
    }
}
