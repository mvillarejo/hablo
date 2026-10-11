//! OpenAI-compatible batch speech-to-text provider (`POST {base}/audio/transcriptions`).
//!
//! Audio is cut into chunks of whole sentences while the user is still
//! speaking (`find_cut`): after `MIN_CUT_SECONDS`, the next sentence pause ends
//! a chunk; breath and comma pauses never do. Each chunk is wrapped as a mono 16-bit
//! WAV and handed to a sequential queue (`ChunkQueue`) that keeps one request in
//! flight at a time. On release only the remainder is left to send; the texts
//! are joined in order and pasted once. Defaults target local Ollama, but any
//! compatible endpoint (Groq, OpenAI) works by changing base URL, key and model.

use crate::transcriber::TranscriptionEvent;
use crate::vocabulary::{normalize_vocabulary, CustomVocabEntry};
use futures_util::future::BoxFuture;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;

/// Gemma 4 accepts at most 30 s of audio per request; keep a safety margin.
/// No request is ever larger than this, merges included.
pub const MAX_CHUNK_SECONDS: u32 = 28;
/// Cut only once this much audio has built up since the last cut.
pub const MIN_CUT_SECONDS: usize = 6;
/// No sentence pause by now: cut at the longest pause seen so far.
pub const LONGEST_PAUSE_SECONDS: usize = 15;
/// Hard cap per chunk, only reached by speech with no pauses at all.
pub const MAX_CUT_SECONDS: usize = 25;
/// Energy-VAD analysis frame.
const FRAME_MS: usize = 20;
/// A quiet run this long is a pause; shorter gaps are between words.
const MIN_PAUSE_MS: usize = 160;
/// Sentence-pause threshold before the speaker's own pauses say otherwise…
const SENTENCE_PAUSE_MS: usize = 500;
/// …and the range the adaptive threshold is clamped to.
const SENTENCE_PAUSE_RANGE_MS: (usize, usize) = (400, 800);
/// Two pause lengths at least this far apart (as a ratio) are different kinds.
const PAUSE_KIND_RATIO: f64 = 1.6;
/// Pause lengths this close are a tie (20 ms frames plus energy jitter);
/// the longest-pause fallback then takes the later one.
const PAUSE_TIE_MS: usize = 40;
/// Pauses remembered from earlier chunks of the same dictation.
const PAUSE_HISTORY: usize = 24;
/// Hard-cap fallback: cut in the middle of the quietest window this long…
const QUIET_WINDOW_MS: usize = 100;
/// …within the last this-many milliseconds before the cap.
const QUIET_SEARCH_MS: usize = 2_000;
/// Chunks with less voiced audio than this are merged into a neighbour.
const MIN_VOICED_MS: usize = 1_000;
/// Frame RMS below this is always silence (about -48 dBFS).
const SILENCE_FLOOR: f32 = 0.004;
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

fn frame_bytes(sample_rate: u32) -> usize {
    (sample_rate as usize * FRAME_MS / 1000).max(1) * BYTES_PER_SAMPLE
}

/// RMS (0.0–1.0) of each whole `FRAME_MS` frame; a trailing partial frame is ignored.
fn frame_rms(pcm: &[u8], sample_rate: u32) -> Vec<f32> {
    let frame = frame_bytes(sample_rate);
    pcm.chunks_exact(frame)
        .map(|bytes| {
            let sum: f64 = bytes
                .as_chunks::<BYTES_PER_SAMPLE>()
                .0
                .iter()
                .map(|s| {
                    let v = i16::from_le_bytes(*s) as f64 / 32_768.0;
                    v * v
                })
                .sum();
            (sum / (frame / BYTES_PER_SAMPLE) as f64).sqrt() as f32
        })
        .collect()
}

/// Adaptive silence threshold: twice the noise floor (5th percentile frame) or
/// 8 % of loud speech (95th percentile), whichever is higher, but at most half
/// the speech level (so a buffer with few pauses still finds them) and never
/// below `SILENCE_FLOOR`.
fn threshold_from_frames(rms: &[f32]) -> f32 {
    if rms.is_empty() {
        return SILENCE_FLOOR;
    }
    let mut sorted = rms.to_vec();
    sorted.sort_by(f32::total_cmp);
    let percentile = |p: usize| sorted[(sorted.len() - 1) * p / 100];
    let loud = percentile(95);
    (percentile(5) * 2.0)
        .max(loud * 0.08)
        .min(loud * 0.5)
        .max(SILENCE_FLOOR)
}

/// The silence threshold `find_cut` would use for `pcm`.
pub fn silence_threshold(pcm: &[u8], sample_rate: u32) -> f32 {
    threshold_from_frames(&frame_rms(pcm, sample_rate))
}

/// Milliseconds of `pcm` whose frame energy reaches `threshold`.
pub fn voiced_ms(pcm: &[u8], sample_rate: u32, threshold: f32) -> usize {
    frame_rms(pcm, sample_rate)
        .iter()
        .filter(|rms| **rms >= threshold)
        .count()
        * FRAME_MS
}

/// Quiet runs of at least `MIN_PAUSE_MS`, as `(start_frame, frames)`. A run
/// still open at the end of `rms` is included with its length so far.
fn pauses(rms: &[f32], threshold: f32) -> Vec<(usize, usize)> {
    let min = MIN_PAUSE_MS / FRAME_MS;
    let mut out = Vec::new();
    let mut start = None;
    for (index, level) in rms.iter().enumerate() {
        match (*level < threshold, start) {
            (true, None) => start = Some(index),
            (false, Some(begin)) => {
                if index - begin >= min {
                    out.push((begin, index - begin));
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start.filter(|begin| rms.len() - begin >= min) {
        out.push((begin, rms.len() - begin));
    }
    out
}

/// Lengths (ms) of the pauses wholly inside `pcm`: a run touching either end
/// is half of a cut pause and says nothing about the speaker. Feeds the
/// history `find_cut` adapts to.
pub fn pause_lengths_ms(pcm: &[u8], sample_rate: u32) -> Vec<usize> {
    let rms = frame_rms(pcm, sample_rate);
    pauses(&rms, threshold_from_frames(&rms))
        .into_iter()
        .filter(|(start, len)| *start > 0 && start + len < rms.len())
        .map(|(_, len)| len * FRAME_MS)
        .collect()
}

/// How long a pause must be to end a sentence for this speaker, from the
/// pause lengths seen so far. When the pauses fall into two groups (breath or
/// comma vs sentence), the threshold sits between them; with one group, a
/// group of short pauses pushes it above them, a group of long ones leaves the
/// default. Clamped to `SENTENCE_PAUSE_RANGE_MS`.
pub fn sentence_pause_ms(seen_ms: &[usize]) -> usize {
    let mut sorted = seen_ms.to_vec();
    sorted.sort_unstable();
    let ratio = |w: &[usize]| w[1] as f64 / w[0].max(1) as f64;
    let estimate = match sorted
        .windows(2)
        .filter(|w| ratio(w) >= PAUSE_KIND_RATIO)
        .max_by(|a, b| ratio(a).total_cmp(&ratio(b)))
    {
        Some(w) => ((w[0] * w[1]) as f64).sqrt() as usize,
        None => match sorted.get(sorted.len() / 2) {
            Some(&median) if median < SENTENCE_PAUSE_MS => {
                ((median as f64 * PAUSE_KIND_RATIO) as usize).max(SENTENCE_PAUSE_MS)
            }
            _ => SENTENCE_PAUSE_MS,
        },
    };
    estimate.clamp(SENTENCE_PAUSE_RANGE_MS.0, SENTENCE_PAUSE_RANGE_MS.1)
}

/// Where to end the next chunk of `pcm` (audio since the last cut), as a byte
/// offset on a sample boundary, or `None` to keep accumulating. `earlier_ms`
/// are pause lengths from earlier chunks of the same dictation.
///
/// Chunks are whole sentences: a pause is a quiet run of `MIN_PAUSE_MS`, and
/// only a sentence pause (`sentence_pause_ms`, adapted to the speaker) is a cut
/// point; breath and comma pauses never are. After `MIN_CUT_SECONDS`, the
/// first sentence pause ends the chunk, cut in its middle. With none by
/// `LONGEST_PAUSE_SECONDS`, it cuts at the longest pause seen so far (the later
/// one when lengths are within `PAUSE_TIE_MS`), or at the next pause after that. Only speech with no pause at
/// all reaches `MAX_CUT_SECONDS`, cut in the quietest `QUIET_WINDOW_MS` of the
/// last `QUIET_SEARCH_MS`. A cut needs `MIN_VOICED_MS` of voiced audio before
/// it, so a near-silent stretch is merged into the next chunk.
pub fn find_cut(pcm: &[u8], sample_rate: u32, earlier_ms: &[usize]) -> Option<usize> {
    if sample_rate == 0 {
        return None;
    }
    let frame = frame_bytes(sample_rate);
    let frames = |ms: usize| ms / FRAME_MS;
    let min_frame = frames(MIN_CUT_SECONDS * 1000);
    let longest_frame = frames(LONGEST_PAUSE_SECONDS * 1000);
    let max_frame = frames(MAX_CUT_SECONDS * 1000);
    let min_pause = frames(MIN_PAUSE_MS);
    if pcm.len() / frame < min_frame {
        return None;
    }
    let rms = frame_rms(&pcm[..pcm.len().min(max_frame * frame)], sample_rate);
    let threshold = threshold_from_frames(&rms);
    let enough_voice = frames(MIN_VOICED_MS);
    let skip = earlier_ms.len().saturating_sub(PAUSE_HISTORY);
    let mut seen: Vec<usize> = earlier_ms[skip..].to_vec();
    let mut sentence = frames(sentence_pause_ms(&seen));
    // Closed pauses that could end a chunk: (start, frames, voiced before).
    let mut candidates: Vec<(usize, usize, usize)> = Vec::new();
    let mut voiced = 0;
    let mut run: Option<(usize, usize)> = None; // (start, voiced before)
    let cut_at = |start: usize, len: usize| start + len / 2;
    for (index, level) in rms.iter().enumerate() {
        if index == longest_frame {
            let open = run.map(|(start, before)| (start, index - start, before));
            let usable: Vec<(usize, usize, usize)> = candidates
                .iter()
                .copied()
                .chain(open.filter(|(_, len, _)| *len >= min_pause))
                .filter(|(start, len, before)| {
                    cut_at(*start, *len) >= min_frame && *before >= enough_voice
                })
                .collect();
            let max_len = usable.iter().map(|(_, len, _)| *len).max().unwrap_or(0);
            let longest = usable
                .iter()
                .rfind(|(_, len, _)| len + frames(PAUSE_TIE_MS) >= max_len);
            if let Some(&(start, len, _)) = longest {
                return Some(cut_at(start, len) * frame);
            }
        }
        if *level >= threshold {
            if let Some((start, before)) = run.take() {
                let len = index - start;
                if len >= min_pause {
                    seen.push(len * FRAME_MS);
                    sentence = frames(sentence_pause_ms(
                        &seen[seen.len().saturating_sub(PAUSE_HISTORY)..],
                    ));
                    candidates.push((start, len, before));
                }
            }
            voiced += 1;
            continue;
        }
        let (start, before) = *run.get_or_insert((index, voiced));
        let len = index + 1 - start;
        // A sentence pause, or after the longest-pause deadline any pause.
        let due = len == sentence || (index >= longest_frame && len == min_pause);
        if due && cut_at(start, len) >= min_frame && before >= enough_voice {
            return Some(cut_at(start, len) * frame);
        }
    }
    if rms.len() < max_frame {
        return None;
    }
    let window = frames(QUIET_WINDOW_MS);
    let window_energy = |start: usize| rms[start..start + window].iter().sum::<f32>();
    let quietest = (max_frame - frames(QUIET_SEARCH_MS)..=max_frame - window)
        .min_by(|a, b| window_energy(*a).total_cmp(&window_energy(*b)))?;
    Some((quietest + window / 2) * frame)
}

/// Split a recording into whole-sentence pieces of at most `MAX_CUT_SECONDS`
/// with the same rules as live cutting (whole samples only; a trailing odd byte
/// is dropped). `earlier_ms` is the dictation's pause history.
pub fn split_long<'a>(pcm: &'a [u8], sample_rate: u32, earlier_ms: &[usize]) -> Vec<&'a [u8]> {
    let mut rest = &pcm[..pcm.len() - pcm.len() % BYTES_PER_SAMPLE];
    let max_bytes = sample_rate as usize * MAX_CUT_SECONDS * BYTES_PER_SAMPLE;
    let mut history = earlier_ms.to_vec();
    let mut pieces = Vec::new();
    while max_bytes > 0 && rest.len() > max_bytes {
        let cut = find_cut(rest, sample_rate, &history).unwrap_or(max_bytes);
        let (head, tail) = rest.split_at(cut);
        history.extend(pause_lengths_ms(head, sample_rate));
        pieces.push(head);
        rest = tail;
    }
    if !rest.is_empty() {
        pieces.push(rest);
    }
    pieces
}

/// What to do with the audio left over when the key is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemainderPlan {
    /// No whole sample left.
    Nothing,
    /// Send it, split at pauses if it is still long.
    Send,
    /// Near-silent: append it to the last chunk still waiting in the queue.
    MergeIntoQueued,
    /// No voiced audio at all and earlier chunks carry the speech: skip it.
    Drop,
}

/// Decide how to handle the remainder. `threshold` is the session's silence
/// threshold (from the last cut) when there is one; `queued_tail_len` is the
/// size of the last chunk not yet picked up by the queue.
pub fn plan_remainder(
    remainder: &[u8],
    sample_rate: u32,
    threshold: Option<f32>,
    earlier_chunks: usize,
    queued_tail_len: Option<usize>,
) -> RemainderPlan {
    if remainder.len() < BYTES_PER_SAMPLE {
        return RemainderPlan::Nothing;
    }
    if earlier_chunks == 0 {
        return RemainderPlan::Send;
    }
    let threshold = threshold.unwrap_or_else(|| silence_threshold(remainder, sample_rate));
    let voiced = voiced_ms(remainder, sample_rate, threshold);
    if voiced >= MIN_VOICED_MS {
        return RemainderPlan::Send;
    }
    let max_bytes = (sample_rate * MAX_CHUNK_SECONDS) as usize * BYTES_PER_SAMPLE;
    if queued_tail_len.is_some_and(|len| len + remainder.len() <= max_bytes) {
        RemainderPlan::MergeIntoQueued
    } else if voiced == 0 {
        RemainderPlan::Drop
    } else {
        RemainderPlan::Send
    }
}

/// Chunk texts in order, trimmed, empties skipped, joined with single spaces.
pub fn join_texts(texts: &[String]) -> String {
    texts
        .iter()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
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

/// Transcribe one chunk; returns its (possibly empty) text.
async fn transcribe_chunk(
    client: &reqwest::Client,
    parts: &RequestParts,
    base_url: &str,
    wav: Vec<u8>,
) -> Result<String, String> {
    let response = build_request(client, parts, wav)
        .send()
        .await
        .map_err(|e| request_error_message(base_url, &e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| request_error_message(base_url, &e))?;
    if !status.is_success() {
        return Err(http_error_message(status, &body));
    }
    parse_transcription_response(&body)
}

/// Sends one chunk of PCM and resolves to its text. Boxed so the queue can be
/// driven by a fake in tests.
pub type TranscribeFn =
    Arc<dyn Fn(Vec<u8>) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;

/// The real transcriber: `POST {base}/audio/transcriptions` per chunk.
pub fn http_transcriber(
    settings: &BatchSettings,
    options: &RequestOptions,
    sample_rate: u32,
) -> TranscribeFn {
    let client = http_client();
    let parts = Arc::new(build_request_parts(settings, options));
    let base_url: Arc<str> = settings.base_url.clone().into();
    Arc::new(move |pcm| {
        let (client, parts, base_url) = (client.clone(), parts.clone(), base_url.clone());
        Box::pin(async move {
            transcribe_chunk(&client, &parts, &base_url, wav_bytes(&pcm, sample_rate)).await
        })
    })
}

/// Keep a warmed-up model loaded this long.
const WARMUP_KEEP_ALIVE: &str = "5m";
/// At most one warm-up per this interval.
const WARMUP_INTERVAL: Duration = Duration::from_secs(30);

/// Origin (`http://host:port`) of `base_url` when it points at Ollama: a
/// loopback host (localhost, 127.0.0.1, ::1) or Ollama's port 11434. `None`
/// for anything else, so cloud providers that bill per request never get one.
pub fn ollama_origin(base_url: &str) -> Option<String> {
    let url = url::Url::parse(trim_base_url(base_url)).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let loopback = match url.host()? {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(ip) => ip == std::net::Ipv4Addr::LOCALHOST,
        url::Host::Ipv6(ip) => ip == std::net::Ipv6Addr::LOCALHOST,
    };
    (loopback || url.port() == Some(11434)).then(|| url.origin().ascii_serialization())
}

/// Ollama's documented "load a model" call: `POST /api/generate` with no prompt.
pub fn warmup_request(
    client: &reqwest::Client,
    origin: &str,
    model: &str,
) -> reqwest::RequestBuilder {
    client
        .post(format!("{origin}/api/generate"))
        .json(&serde_json::json!({ "model": model, "keep_alive": WARMUP_KEEP_ALIVE }))
}

/// Rate limit for warm-ups.
pub struct WarmupGate(Mutex<Option<Instant>>);

impl WarmupGate {
    pub const fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// True (and records `now`) when no warm-up fired in the last `WARMUP_INTERVAL`.
    pub fn try_acquire(&self, now: Instant) -> bool {
        let mut last = self.0.lock().unwrap();
        if last.is_some_and(|last| now.saturating_duration_since(last) < WARMUP_INTERVAL) {
            return false;
        }
        *last = Some(now);
        true
    }
}

static WARMUP_GATE: WarmupGate = WarmupGate::new();

/// Fire-and-forget model load on a local Ollama, so the model is warm by the
/// time the first chunk arrives. Never blocks and never fails dictation.
/// Returns whether a request was fired.
pub fn warm_up_ollama(settings: &BatchSettings) -> bool {
    let Some(origin) = ollama_origin(&settings.base_url) else {
        return false;
    };
    if !WARMUP_GATE.try_acquire(Instant::now()) {
        log::debug!("[batch-stt] warm-up skipped: one fired in the last 30s");
        return false;
    }
    let request = warmup_request(&http_client(), &origin, &settings.model);
    let model = settings.model.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        match request.send().await {
            Ok(response) if response.status().is_success() => log::info!(
                "[batch-stt] warm-up: {model} ready in {}ms",
                started.elapsed().as_millis()
            ),
            Ok(response) => log::info!("[batch-stt] warm-up: {} for {model}", response.status()),
            Err(error) => log::info!("[batch-stt] warm-up failed for {model}: {error}"),
        }
    });
    true
}

#[derive(Default)]
struct QueueState {
    /// Chunks not yet picked up by the worker, oldest first.
    waiting: VecDeque<Vec<u8>>,
    /// The key was released: no more chunks will arrive.
    finished: bool,
    /// Set on drop; nothing is emitted afterwards.
    cancelled: bool,
}

/// Sequential transcription queue: one worker, one request in flight, results
/// kept in order. On finish it emits `Final` (or `Error`) then `SessionEnded`.
/// Dropping the queue cancels it: pending chunks and late results are discarded
/// and no further event is emitted.
pub struct ChunkQueue {
    state: Arc<Mutex<QueueState>>,
    wake: Arc<Notify>,
    handle: JoinHandle<()>,
}

impl ChunkQueue {
    pub fn spawn(
        transcribe: TranscribeFn,
        sample_rate: u32,
        tx: broadcast::Sender<TranscriptionEvent>,
    ) -> Self {
        let state = Arc::new(Mutex::new(QueueState::default()));
        let wake = Arc::new(Notify::new());
        let handle = tokio::spawn(run_queue(
            state.clone(),
            wake.clone(),
            transcribe,
            sample_rate,
            tx,
        ));
        Self {
            state,
            wake,
            handle,
        }
    }

    pub fn push(&self, chunk: Vec<u8>) {
        self.state.lock().unwrap().waiting.push_back(chunk);
        self.wake.notify_one();
    }

    /// Queue the remainder per `plan_remainder` (split into whole sentences
    /// with the dictation's `pauses_ms` history) and mark the queue finished.
    pub fn finish(
        &self,
        remainder: &[u8],
        sample_rate: u32,
        threshold: Option<f32>,
        earlier_chunks: usize,
        pauses_ms: &[usize],
    ) -> RemainderPlan {
        let mut state = self.state.lock().unwrap();
        let plan = plan_remainder(
            remainder,
            sample_rate,
            threshold,
            earlier_chunks,
            state.waiting.back().map(Vec::len),
        );
        match plan {
            RemainderPlan::Send => state.waiting.extend(
                split_long(remainder, sample_rate, pauses_ms)
                    .into_iter()
                    .map(<[u8]>::to_vec),
            ),
            RemainderPlan::MergeIntoQueued => {
                let whole = remainder.len() - remainder.len() % BYTES_PER_SAMPLE;
                if let Some(tail) = state.waiting.back_mut() {
                    tail.extend_from_slice(&remainder[..whole]);
                }
            }
            RemainderPlan::Drop | RemainderPlan::Nothing => {}
        }
        state.finished = true;
        drop(state);
        self.wake.notify_one();
        plan
    }
}

impl Drop for ChunkQueue {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.cancelled = true;
            state.waiting.clear();
        }
        self.handle.abort();
    }
}

/// Emit `event` unless the queue was cancelled. The check and the send happen
/// under the queue lock, so nothing slips out after a cancel.
fn emit(
    state: &Mutex<QueueState>,
    tx: &broadcast::Sender<TranscriptionEvent>,
    event: TranscriptionEvent,
) -> bool {
    let state = state.lock().unwrap();
    if state.cancelled {
        return false;
    }
    let _ = tx.send(event);
    true
}

async fn run_queue(
    state: Arc<Mutex<QueueState>>,
    wake: Arc<Notify>,
    transcribe: TranscribeFn,
    sample_rate: u32,
    tx: broadcast::Sender<TranscriptionEvent>,
) {
    let mut texts = Vec::new();
    let mut sent_seconds = 0.0;
    loop {
        let (next, finished) = {
            let mut state = state.lock().unwrap();
            if state.cancelled {
                return;
            }
            (state.waiting.pop_front(), state.finished)
        };
        let Some(chunk) = next else {
            if finished {
                break;
            }
            wake.notified().await;
            continue;
        };
        let duration = pcm_duration_seconds(&chunk, sample_rate);
        let (index, start) = (texts.len() + 1, sent_seconds);
        sent_seconds += duration;
        let started = Instant::now();
        match transcribe(chunk).await {
            Ok(text) => {
                log::info!(
                    "[batch-stt] chunk #{index} [{start:.2}s-{sent_seconds:.2}s] transcribed in {}ms ({} chars)",
                    started.elapsed().as_millis(),
                    text.chars().count()
                );
                texts.push(text);
            }
            Err(message) => {
                log::error!("[batch-stt] chunk #{index}: {message}");
                emit(&state, &tx, TranscriptionEvent::Error(message));
                emit(&state, &tx, TranscriptionEvent::SessionEnded);
                return;
            }
        }
    }
    let text = join_texts(&texts);
    let event = if texts.is_empty() {
        log::info!("[batch-stt] no audio captured; skipping request");
        None
    } else if text.is_empty() {
        Some(TranscriptionEvent::Error(
            "No speech was recognized in the recording".to_string(),
        ))
    } else {
        log::info!(
            "[batch-stt] {} chunk(s) joined ({} chars)",
            texts.len(),
            text.chars().count()
        );
        Some(TranscriptionEvent::Final {
            text,
            start: 0.0,
            end: sent_seconds,
        })
    };
    if let Some(event) = event {
        emit(&state, &tx, event);
    }
    emit(&state, &tx, TranscriptionEvent::SessionEnded);
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
    /// Audio since the last cut.
    buffer: Vec<u8>,
    /// Seconds of audio already handed to the queue (for logs).
    cut_seconds: f64,
    /// Silence threshold of the last cut chunk, reused to judge the remainder.
    threshold: Option<f32>,
    /// Chunks handed to the queue while recording.
    chunks: usize,
    /// Pause lengths (ms) from the chunks cut so far: the speaker's rhythm.
    pauses: Vec<usize>,
    queue: Option<ChunkQueue>,
    session_id: Option<String>,
    transcription_tx: Option<broadcast::Sender<TranscriptionEvent>>,
}

#[derive(Clone, Default)]
pub struct BatchClient {
    state: Arc<Mutex<SessionState>>,
    #[cfg(test)]
    transcriber: Option<TranscribeFn>,
}

impl BatchClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test seam: drive the queue with a fake instead of HTTP.
    #[cfg(test)]
    fn with_transcriber(transcriber: TranscribeFn) -> Self {
        Self {
            transcriber: Some(transcriber),
            ..Self::default()
        }
    }

    fn transcriber_for(&self, settings: &BatchSettings, state: &SessionState) -> TranscribeFn {
        #[cfg(test)]
        if let Some(transcriber) = &self.transcriber {
            return transcriber.clone();
        }
        http_transcriber(settings, &state.options, state.sample_rate)
    }

    /// The session's queue, started on first use.
    fn queue<'a>(&self, state: &'a mut SessionState) -> Option<&'a ChunkQueue> {
        if state.queue.is_none() {
            let settings = state.settings.as_ref()?;
            let tx = state.transcription_tx.clone()?;
            let transcriber = self.transcriber_for(settings, state);
            state.queue = Some(ChunkQueue::spawn(transcriber, state.sample_rate, tx));
        }
        state.queue.as_ref()
    }

    pub fn init_session(&self, settings: BatchSettings, options: RequestOptions) -> String {
        let session_id = format!("local-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.6fZ"));
        let (tx, _rx) = broadcast::channel::<TranscriptionEvent>(16);
        let mut state = self.state.lock().unwrap();
        // Replacing the state drops (cancels) any previous queue.
        *state = SessionState {
            settings: Some(settings),
            options,
            sample_rate: state.sample_rate,
            session_id: Some(session_id.clone()),
            transcription_tx: Some(tx),
            ..SessionState::default()
        };
        session_id
    }

    pub fn set_audio_format(&self, sample_rate: u32) {
        self.state.lock().unwrap().sample_rate = sample_rate;
    }

    /// Buffer audio and, during a session, hand each finished chunk to the
    /// queue so it is transcribed while the user keeps talking.
    pub fn send_audio(&self, audio_data: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.buffer.extend_from_slice(audio_data);
        if state.transcription_tx.is_none() {
            return;
        }
        let rate = state.sample_rate;
        while let Some(cut) = find_cut(&state.buffer, rate, &state.pauses) {
            let chunk: Vec<u8> = state.buffer.drain(..cut).collect();
            let duration = pcm_duration_seconds(&chunk, rate);
            log::info!(
                "[batch-stt] cut chunk #{} at {:.2}s-{:.2}s while recording",
                state.chunks + 1,
                state.cut_seconds,
                state.cut_seconds + duration
            );
            state.threshold = Some(silence_threshold(&chunk, rate));
            state.pauses.extend(pause_lengths_ms(&chunk, rate));
            let skip = state.pauses.len().saturating_sub(PAUSE_HISTORY);
            state.pauses.drain(..skip);
            state.cut_seconds += duration;
            state.chunks += 1;
            let Some(queue) = self.queue(&mut state) else {
                return;
            };
            queue.push(chunk);
        }
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

    /// Key released: queue the remainder and let the queue finish, emitting
    /// `Final` (or `Error`) followed by `SessionEnded`.
    pub fn stop_recording(&self) {
        let mut state = self.state.lock().unwrap();
        let (Some(settings), Some(tx)) = (state.settings.clone(), state.transcription_tx.clone())
        else {
            return;
        };
        let remainder = std::mem::take(&mut state.buffer);
        let rate = state.sample_rate;
        let remainder_seconds = pcm_duration_seconds(&remainder, rate);
        if state.queue.is_none() && remainder.len() < BYTES_PER_SAMPLE {
            log::info!("[batch-stt] no audio captured; skipping request");
            let _ = tx.send(TranscriptionEvent::SessionEnded);
            return;
        }
        let (threshold, earlier, pauses) = (state.threshold, state.chunks, state.pauses.clone());
        let recorded = state.cut_seconds + remainder_seconds;
        let Some(queue) = self.queue(&mut state) else {
            return;
        };
        let plan = queue.finish(&remainder, rate, threshold, earlier, &pauses);
        log::info!(
            "[batch-stt] released after {recorded:.1}s with {} at {}: {earlier} chunk(s) already queued, {remainder_seconds:.1}s remainder -> {plan:?}",
            settings.model,
            settings.base_url
        );
    }

    /// Cancel: drop pending chunks and any late result (dropping the queue
    /// aborts its worker), then reset the session.
    pub async fn close_session(&self) {
        let queue = {
            let mut state = self.state.lock().unwrap();
            let queue = state.queue.take();
            *state = SessionState {
                sample_rate: state.sample_rate,
                ..SessionState::default()
            };
            queue
        };
        drop(queue);
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

    /// Synthetic "speech": a 220 Hz tone.
    fn tone(seconds: f64, amplitude: f32, rate: u32) -> Vec<u8> {
        let n = (seconds * rate as f64).round() as usize;
        (0..n)
            .flat_map(|i| {
                let t = i as f32 / rate as f32;
                let v = (t * 220.0 * std::f32::consts::TAU).sin() * amplitude;
                ((v * 32_767.0) as i16).to_le_bytes()
            })
            .collect()
    }

    /// Room noise at `amplitude` (deterministic LCG).
    fn noise(seconds: f64, amplitude: f32, rate: u32) -> Vec<u8> {
        let mut seed = 0x2545_f491u32;
        let n = (seconds * rate as f64).round() as usize;
        (0..n)
            .flat_map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let v = ((seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amplitude;
                ((v * 32_767.0) as i16).to_le_bytes()
            })
            .collect()
    }

    fn quiet(seconds: f64, rate: u32) -> Vec<u8> {
        noise(seconds, 0.001, rate)
    }

    fn speech(seconds: f64, rate: u32) -> Vec<u8> {
        tone(seconds, 0.3, rate)
    }

    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    fn at_seconds(bytes: usize, rate: u32) -> f64 {
        bytes as f64 / (rate as f64 * 2.0)
    }

    /// Speech with gaps: `(speech_s, gap_s)` pairs, gaps as near-silence.
    fn spoken(parts: &[(f64, f64)], rate: u32) -> Vec<u8> {
        parts
            .iter()
            .flat_map(|(talk, gap)| [speech(*talk, rate), quiet(*gap, rate)])
            .collect::<Vec<_>>()
            .concat()
    }

    fn cut_seconds(pcm: &[u8], earlier: &[usize]) -> Option<f64> {
        find_cut(pcm, RATE, earlier).map(|cut| {
            assert_eq!(cut % 2, 0, "cut on a sample boundary");
            at_seconds(cut, RATE)
        })
    }

    #[test]
    fn no_cut_before_six_seconds_even_at_sentence_pauses() {
        let pcm = spoken(&[(2.5, 1.0), (2.0, 0.39)], RATE);
        assert_eq!(cut_seconds(&pcm, &[]), None);
        assert_eq!(find_cut(&[], RATE, &[]), None);
        assert_eq!(find_cut(&pcm, 0, &[]), None);
    }

    #[test]
    fn sentence_pause_threshold_adapts_to_the_speaker_within_a_range() {
        for (case, seen, expected) in [
            ("nothing seen yet", vec![], 500),
            ("comma 250 vs sentence 700", vec![250, 700, 260, 240], 426),
            ("slow speaker 550 vs 1200", vec![550, 1_200, 560], 800),
            ("fast speaker 200 vs 420", vec![200, 420, 180], 400),
            ("only commas push it up", vec![300, 320, 340], 512),
            ("only sentence pauses keep the default", vec![700, 720], 500),
            ("very long pauses clamp to 800", vec![300, 3_000], 800),
        ] {
            assert_eq!(sentence_pause_ms(&seen), expected, "{case}");
        }
    }

    #[test]
    fn cuts_in_the_middle_of_the_first_sentence_pause_after_six_seconds() {
        // 0.7 s pause at 3.0 s is too early; the 0.25 s comma at 7.7 s is not a
        // cut point; the 0.7 s pause at 9.95 s is.
        let pcm = spoken(&[(3.0, 0.7), (4.0, 0.25), (2.0, 0.7), (3.0, 0.0)], RATE);
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((10.1..10.25).contains(&at), "cut at {at}");
    }

    #[test]
    fn never_cuts_at_a_short_gap_when_a_long_one_is_in_the_window() {
        // Commas every ~1.5 s from 6 s on, one sentence end at 13 s.
        let pcm = spoken(
            &[
                (6.2, 0.25),
                (1.5, 0.25),
                (1.5, 0.25),
                (1.5, 0.25),
                (1.0, 0.7),
                (3.0, 0.25),
                (1.0, 0.0),
            ],
            RATE,
        );
        let sentence_end = 6.2 + 0.25 + (1.5 + 0.25) * 3.0 + 1.0;
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!(
            (sentence_end..sentence_end + 0.7).contains(&at),
            "cut at {at}, sentence ends at {sentence_end}"
        );
    }

    #[test]
    fn slow_speakers_long_commas_are_not_sentence_ends() {
        // Commas of 0.55 s would pass a fixed 0.5 s rule; this speaker's
        // sentence pauses are 1.2 s, so only those cut.
        let pcm = spoken(
            &[(1.5, 0.55), (1.5, 1.2), (3.0, 0.55), (2.0, 1.2), (1.0, 0.0)],
            RATE,
        );
        let comma = 1.5 + 0.55 + 1.5 + 1.2 + 3.0;
        let sentence_end = comma + 0.55 + 2.0;
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!(at > comma + 0.55, "cut at the comma: {at}");
        assert!(
            (sentence_end..sentence_end + 1.2).contains(&at),
            "cut at {at}"
        );
    }

    #[test]
    fn fast_speakers_short_sentence_pauses_still_cut() {
        // 0.42 s sentence pauses would miss a fixed 0.5 s rule.
        let pcm = spoken(
            &[(2.0, 0.2), (2.0, 0.42), (2.5, 0.2), (1.0, 0.42), (1.0, 0.0)],
            RATE,
        );
        let sentence_end = 2.0 + 0.2 + 2.0 + 0.42 + 2.5 + 0.2 + 1.0;
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!(
            (sentence_end..sentence_end + 0.42).contains(&at),
            "cut at {at}"
        );
    }

    #[test]
    fn earlier_chunks_teach_the_speakers_rhythm() {
        // Alone, a 0.6 s pause is a sentence end; after hearing 0.45 s commas
        // and 1.4 s sentence ends, it is not.
        let pcm = spoken(&[(6.5, 0.6), (2.0, 1.4), (1.0, 0.0)], RATE);
        let alone = cut_seconds(&pcm, &[]).expect("cut");
        assert!((6.5..7.1).contains(&alone), "cut at {alone}");
        let learned = cut_seconds(&pcm, &[450, 1_400, 460, 1_380]).expect("cut");
        assert!((9.1..10.5).contains(&learned), "cut at {learned}");
    }

    #[test]
    fn without_a_sentence_pause_by_15s_cuts_at_the_longest_pause() {
        let pcm = spoken(
            &[
                (6.5, 0.25),
                (2.0, 0.36),
                (2.0, 0.25),
                (2.0, 0.26),
                (3.0, 0.0),
            ],
            RATE,
        );
        let longest = 6.5 + 0.25 + 2.0;
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((longest..longest + 0.36).contains(&at), "cut at {at}");
        // Before 15 s nothing is decided.
        let early = &pcm[..(14.9 * RATE as f64) as usize * 2];
        assert_eq!(cut_seconds(early, &[]), None);
    }

    #[test]
    fn equal_pauses_prefer_the_later_one() {
        let pcm = spoken(&[(7.0, 0.3), (3.0, 0.3), (3.0, 0.2), (3.0, 0.0)], RATE);
        let later = 7.0 + 0.3 + 3.0;
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((later..later + 0.3).contains(&at), "cut at {at}");
    }

    #[test]
    fn pauses_within_40ms_are_a_tie_and_the_later_one_wins() {
        // A 440 ms comma, then a 420 ms sentence end: one frame apart is noise.
        let pcm = spoken(&[(6.5, 0.44), (2.0, 0.42), (2.0, 0.3), (6.0, 0.0)], RATE);
        let later = 6.5 + 0.44 + 2.0;
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((later..later + 0.42).contains(&at), "cut at {at}");
        // 80 ms shorter is a real difference: the longer, earlier pause wins.
        let pcm = spoken(&[(6.5, 0.44), (2.0, 0.36), (2.0, 0.3), (6.0, 0.0)], RATE);
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((6.5..6.94).contains(&at), "cut at {at}");
    }

    #[test]
    fn after_15s_the_next_pause_of_any_kind_cuts() {
        let pcm = spoken(&[(16.0, 0.2), (3.0, 0.0)], RATE);
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((16.0..16.2).contains(&at), "cut at {at}");
    }

    #[test]
    fn pause_detection_adapts_to_background_noise() {
        // Noise at ~-40 dBFS is above the absolute floor but far below speech.
        let pcm = concat(&[speech(7.0, RATE), noise(0.7, 0.02, RATE), speech(2.0, RATE)]);
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((7.2..7.35).contains(&at), "cut at {at}");
    }

    #[test]
    fn hard_cap_only_for_speech_with_no_pauses_at_all() {
        // No pause anywhere; a dip at 20 s is outside the last-2 s window, the
        // 100 ms dip at 24.0 s is inside it.
        let pcm = concat(&[
            speech(20.0, RATE),
            tone(0.1, 0.01, RATE),
            speech(3.9, RATE),
            tone(0.1, 0.02, RATE),
            speech(2.0, RATE),
        ]);
        let short = &pcm[..(24.9 * RATE as f64) as usize * 2];
        assert_eq!(cut_seconds(short, &[]), None);
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((24.0..24.1).contains(&at), "cut at {at}");
        let at = cut_seconds(&speech(30.0, RATE), &[]).expect("cut");
        assert!((23.0..=25.0).contains(&at), "cut at {at}");
    }

    #[test]
    fn a_near_silent_chunk_is_merged_into_the_next_one() {
        // 0.5 s of speech then a long pause: not enough voiced audio to send
        // alone, so the cut waits for the next sentence end.
        let pcm = spoken(&[(0.5, 8.0), (3.0, 0.7), (1.0, 0.0)], RATE);
        let at = cut_seconds(&pcm, &[]).expect("cut");
        assert!((11.5..12.2).contains(&at), "cut at {at}");
    }

    #[test]
    fn cuts_land_on_sample_boundaries_at_other_capture_rates() {
        for rate in [44_100, 48_000] {
            let pcm = spoken(&[(6.5, 0.7), (1.0, 0.0)], rate);
            let cut = find_cut(&pcm, rate, &[]).expect("cut");
            assert_eq!(cut % 2, 0);
            let at = at_seconds(cut, rate);
            assert!((6.7..6.8).contains(&at), "rate {rate}: cut at {at}");
        }
    }

    #[test]
    fn pause_lengths_skip_runs_that_touch_the_edges() {
        let pcm = concat(&[
            quiet(0.3, RATE),
            spoken(&[(1.0, 0.25), (1.0, 0.7), (1.0, 0.1), (1.0, 0.4)], RATE),
        ]);
        let lengths = pause_lengths_ms(&pcm, RATE);
        assert_eq!(lengths.len(), 2, "{lengths:?}");
        assert!(
            (240..=260).contains(&lengths[0]) && (680..=720).contains(&lengths[1]),
            "{lengths:?}"
        );
    }

    #[test]
    fn split_long_keeps_short_recordings_whole() {
        assert!(split_long(&[], RATE, &[]).is_empty());
        assert!(split_long(&[7], RATE, &[]).is_empty());
        let pcm = concat(&[speech(24.0, RATE), vec![1]]);
        let pieces = split_long(&pcm, RATE, &[]);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].len(), pcm.len() - 1);
    }

    #[test]
    fn split_long_cuts_at_sentence_ends_within_the_cap() {
        let pcm = concat(&[
            spoken(&[(4.0, 0.25), (4.0, 0.7), (5.0, 0.25), (3.0, 0.7)], RATE),
            speech(30.0, RATE),
            vec![9],
        ]);
        let pieces = split_long(&pcm, RATE, &[]);
        assert!(pieces.len() >= 3, "{} pieces", pieces.len());
        let first = at_seconds(pieces[0].len(), RATE);
        assert!((8.25..8.95).contains(&first), "first piece {first}");
        assert!(pieces.iter().all(|p| p.len() % 2 == 0));
        assert!(pieces
            .iter()
            .all(|p| pcm_duration_seconds(p, RATE) <= MAX_CUT_SECONDS as f64));
        assert_eq!(pieces.iter().map(|p| p.len()).sum::<usize>(), pcm.len() - 1);
    }

    #[test]
    fn remainder_plan_covers_send_merge_and_drop() {
        let speech_threshold = Some(silence_threshold(&speech(5.0, RATE), RATE).max(0.02));
        let silent = quiet(0.6, RATE);
        let word = concat(&[speech(0.4, RATE), quiet(0.4, RATE)]);
        let sentence = speech(2.0, RATE);
        let small_tail = Some(RATE as usize * 2 * 12);
        let big_tail = Some(RATE as usize * 2 * 28);
        use RemainderPlan::*;
        for (case, remainder, earlier, tail, expected) in [
            ("nothing left", vec![], 1, small_tail, Nothing),
            ("lone odd byte", vec![3], 1, small_tail, Nothing),
            ("whole utterance", silent.clone(), 0, None, Send),
            ("a real sentence", sentence.clone(), 2, small_tail, Send),
            (
                "silence, chunk waiting",
                silent.clone(),
                2,
                small_tail,
                MergeIntoQueued,
            ),
            (
                "a word, chunk waiting",
                word.clone(),
                2,
                small_tail,
                MergeIntoQueued,
            ),
            ("silence, nothing waiting", silent.clone(), 2, None, Drop),
            (
                "silence, waiting chunk too big",
                silent.clone(),
                2,
                big_tail,
                Drop,
            ),
            ("a word, nothing waiting", word.clone(), 2, None, Send),
        ] {
            assert_eq!(
                plan_remainder(&remainder, RATE, speech_threshold, earlier, tail),
                expected,
                "{case}"
            );
        }
    }

    #[test]
    fn texts_are_trimmed_and_joined_with_single_spaces() {
        let texts = |v: &[&str]| v.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(
            join_texts(&texts(&[" Hello there. ", "", "  How are you? "])),
            "Hello there. How are you?"
        );
        assert_eq!(join_texts(&texts(&[])), "");
        assert_eq!(join_texts(&texts(&["  ", "\n"])), "");
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
    async fn connection_refused_becomes_a_clear_error() {
        // Port 9 (discard) refuses connections.
        let unreachable = BatchSettings {
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: "m".to_string(),
            api_key: None,
        };
        let transcribe = http_transcriber(&unreachable, &RequestOptions::default(), RATE);
        assert_eq!(
            transcribe(vec![0u8; 320]).await,
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

    // --- Queue and session (no network: a fake transcriber) ---

    #[derive(Clone, Default)]
    struct Probe {
        lengths: Arc<Mutex<Vec<usize>>>,
        in_flight: Arc<std::sync::atomic::AtomicUsize>,
        max_in_flight: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Probe {
        fn lengths(&self) -> Vec<usize> {
            self.lengths.lock().unwrap().clone()
        }

        async fn wait_for_calls(&self, calls: usize) {
            for _ in 0..400 {
                if self.lengths.lock().unwrap().len() >= calls {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("transcriber was not called {calls} time(s)");
        }
    }

    /// Records each call, then waits on `gate` (if any) and answers `reply(n)`
    /// for the n-th call. Earlier calls are slower, so ordering bugs show up.
    fn fake(
        probe: &Probe,
        gate: Option<Arc<tokio::sync::Semaphore>>,
        reply: fn(usize) -> Result<String, String>,
    ) -> TranscribeFn {
        use std::sync::atomic::Ordering::SeqCst;
        let probe = probe.clone();
        Arc::new(move |pcm| {
            let (probe, gate) = (probe.clone(), gate.clone());
            Box::pin(async move {
                let n = {
                    let mut lengths = probe.lengths.lock().unwrap();
                    lengths.push(pcm.len());
                    lengths.len()
                };
                let now = probe.in_flight.fetch_add(1, SeqCst) + 1;
                probe.max_in_flight.fetch_max(now, SeqCst);
                if let Some(gate) = gate {
                    gate.acquire().await.unwrap().forget();
                }
                tokio::time::sleep(Duration::from_millis(30 / n as u64)).await;
                probe.in_flight.fetch_sub(1, SeqCst);
                reply(n)
            })
        })
    }

    fn numbered(n: usize) -> Result<String, String> {
        Ok(format!(" c{n} "))
    }

    /// Events up to and including `SessionEnded`, or until the channel closes.
    async fn events(rx: &mut broadcast::Receiver<TranscriptionEvent>) -> Vec<TranscriptionEvent> {
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(event)) => {
                    let end = matches!(event, TranscriptionEvent::SessionEnded);
                    out.push(event);
                    if end {
                        return out;
                    }
                }
                Ok(Err(broadcast::error::RecvError::Closed)) => return out,
                Ok(Err(error)) => panic!("{error}"),
                Err(_) => panic!("timed out waiting for events: {out:?}"),
            }
        }
    }

    fn final_text(events: &[TranscriptionEvent]) -> Option<String> {
        events.iter().find_map(|event| match event {
            TranscriptionEvent::Final { text, .. } => Some(text.clone()),
            _ => None,
        })
    }

    fn feed(client: &BatchClient, pcm: &[u8]) {
        // The capture bridge sends 100 ms pieces.
        for piece in pcm.chunks(RATE as usize * 2 / 10) {
            client.send_audio(piece);
        }
    }

    #[tokio::test]
    async fn queue_keeps_order_with_one_request_in_flight() {
        let probe = Probe::default();
        let (tx, mut rx) = broadcast::channel(16);
        let queue = ChunkQueue::spawn(fake(&probe, None, numbered), RATE, tx);
        for len in [3_200, 6_400, 9_600] {
            queue.push(vec![0; len]);
        }
        assert_eq!(
            queue.finish(&[], RATE, None, 3, &[]),
            RemainderPlan::Nothing
        );
        let events = events(&mut rx).await;
        assert_eq!(final_text(&events).as_deref(), Some("c1 c2 c3"));
        assert!(matches!(
            events.last(),
            Some(TranscriptionEvent::SessionEnded)
        ));
        assert_eq!(probe.lengths(), vec![3_200, 6_400, 9_600]);
        assert_eq!(
            probe
                .max_in_flight
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn queue_merges_a_silent_remainder_into_the_waiting_chunk() {
        let probe = Probe::default();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (tx, mut rx) = broadcast::channel(16);
        let queue = ChunkQueue::spawn(fake(&probe, Some(gate.clone()), numbered), RATE, tx);
        let (first, second) = (speech(10.5, RATE), speech(11.0, RATE));
        queue.push(first.clone());
        probe.wait_for_calls(1).await; // first is in flight, second waits
        queue.push(second.clone());
        let tail = quiet(0.5, RATE);
        let threshold = Some(silence_threshold(&second, RATE));
        assert_eq!(
            queue.finish(&tail, RATE, threshold, 2, &[]),
            RemainderPlan::MergeIntoQueued
        );
        gate.add_permits(10);
        let events = events(&mut rx).await;
        assert_eq!(final_text(&events).as_deref(), Some("c1 c2"));
        assert_eq!(
            probe.lengths(),
            vec![first.len(), second.len() + tail.len()]
        );
    }

    #[tokio::test]
    async fn queue_error_stops_the_queue_and_is_reported() {
        let probe = Probe::default();
        let (tx, mut rx) = broadcast::channel(16);
        let reply = |n| {
            if n == 2 {
                Err("model crashed".to_string())
            } else {
                numbered(n)
            }
        };
        let queue = ChunkQueue::spawn(fake(&probe, None, reply), RATE, tx);
        for _ in 0..3 {
            queue.push(vec![0; 3_200]);
        }
        queue.finish(&[], RATE, None, 3, &[]);
        let events = events(&mut rx).await;
        assert!(
            matches!(&events[..], [TranscriptionEvent::Error(m), TranscriptionEvent::SessionEnded] if m == "model crashed"),
            "{events:?}"
        );
        assert_eq!(probe.lengths().len(), 2, "the third chunk is never sent");
    }

    #[tokio::test]
    async fn queue_with_only_empty_texts_reports_no_speech() {
        let probe = Probe::default();
        let (tx, mut rx) = broadcast::channel(16);
        let queue = ChunkQueue::spawn(fake(&probe, None, |_| Ok("  ".to_string())), RATE, tx);
        queue.push(vec![0; 3_200]);
        queue.finish(&[], RATE, None, 1, &[]);
        let events = events(&mut rx).await;
        assert!(
            matches!(&events[..], [TranscriptionEvent::Error(m), TranscriptionEvent::SessionEnded] if m.contains("No speech")),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn dropping_the_queue_discards_pending_chunks_and_late_results() {
        let probe = Probe::default();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (tx, mut rx) = broadcast::channel(16);
        let queue = ChunkQueue::spawn(fake(&probe, Some(gate.clone()), numbered), RATE, tx);
        queue.push(vec![0; 3_200]);
        queue.push(vec![0; 3_200]);
        probe.wait_for_calls(1).await;
        queue.finish(&speech(2.0, RATE), RATE, None, 2, &[]);
        drop(queue); // Esc / error / too-short: cancel
        gate.add_permits(10); // the in-flight request "returns" late
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(events(&mut rx).await.is_empty(), "nothing after a cancel");
        assert_eq!(probe.lengths().len(), 1, "pending chunks are never sent");
    }

    #[tokio::test]
    async fn session_cuts_while_recording_and_joins_on_release() {
        let probe = Probe::default();
        let client = BatchClient::with_transcriber(fake(&probe, None, numbered));
        client.set_audio_format(RATE);
        client.init_session(settings(None), RequestOptions::default());
        let mut rx = client.subscribe_to_transcriptions();
        feed(&client, &spoken(&[(4.0, 0.25), (3.0, 0.7)], RATE));
        // Chunk 1 goes out while the key is still held.
        probe.wait_for_calls(1).await;
        feed(&client, &concat(&[speech(4.0, RATE), quiet(0.2, RATE)]));
        client.stop_recording();
        let events = events(&mut rx).await;
        assert_eq!(final_text(&events).as_deref(), Some("c1 c2"));
        let lengths = probe.lengths();
        assert_eq!(lengths.len(), 2);
        let first = at_seconds(lengths[0], RATE);
        assert!(
            (7.45..7.55).contains(&first),
            "first chunk ends mid sentence pause: {first}s"
        );
        let total = at_seconds(lengths.iter().sum(), RATE);
        assert!(
            (12.14..12.16).contains(&total),
            "all audio sent once: {total}s"
        );
    }

    #[tokio::test]
    async fn session_short_utterance_is_a_single_request_on_release() {
        let probe = Probe::default();
        let client = BatchClient::with_transcriber(fake(&probe, None, numbered));
        client.set_audio_format(RATE);
        client.init_session(settings(None), RequestOptions::default());
        let mut rx = client.subscribe_to_transcriptions();
        let pcm = concat(&[speech(3.0, RATE), quiet(0.5, RATE)]);
        feed(&client, &pcm);
        assert!(probe.lengths().is_empty(), "nothing sent before release");
        client.stop_recording();
        assert_eq!(final_text(&events(&mut rx).await).as_deref(), Some("c1"));
        assert_eq!(probe.lengths(), vec![pcm.len()]);
    }

    #[tokio::test]
    async fn session_drops_a_silent_tail_once_the_last_chunk_is_sent() {
        let probe = Probe::default();
        let client = BatchClient::with_transcriber(fake(&probe, None, numbered));
        client.set_audio_format(RATE);
        client.init_session(settings(None), RequestOptions::default());
        let mut rx = client.subscribe_to_transcriptions();
        feed(&client, &concat(&[speech(11.0, RATE), quiet(0.6, RATE)]));
        probe.wait_for_calls(1).await;
        tokio::time::sleep(Duration::from_millis(50)).await; // chunk 1 done
        feed(&client, &quiet(1.0, RATE));
        client.stop_recording();
        assert_eq!(final_text(&events(&mut rx).await).as_deref(), Some("c1"));
        assert_eq!(probe.lengths().len(), 1, "no request for pure silence");
    }

    #[tokio::test]
    async fn closing_mid_flight_never_emits_a_result() {
        for released in [false, true] {
            let probe = Probe::default();
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            let client = BatchClient::with_transcriber(fake(&probe, Some(gate.clone()), numbered));
            client.set_audio_format(RATE);
            client.init_session(settings(None), RequestOptions::default());
            let mut rx = client.subscribe_to_transcriptions();
            feed(
                &client,
                &concat(&[speech(11.0, RATE), quiet(0.6, RATE), speech(2.0, RATE)]),
            );
            probe.wait_for_calls(1).await;
            if released {
                client.stop_recording();
            }
            client.close_session().await;
            gate.add_permits(10);
            tokio::time::sleep(Duration::from_millis(60)).await;
            assert!(events(&mut rx).await.is_empty(), "released={released}");
            assert_eq!(probe.lengths().len(), 1, "released={released}");
        }
    }

    // --- Ollama warm-up ---

    #[test]
    fn ollama_is_detected_by_loopback_host_or_port() {
        for (url, expected) in [
            ("http://localhost:11434/v1", Some("http://localhost:11434")),
            ("http://LOCALHOST:8080/v1/", Some("http://localhost:8080")),
            ("http://127.0.0.1:11434/v1", Some("http://127.0.0.1:11434")),
            ("http://[::1]:11434/v1", Some("http://[::1]:11434")),
            (
                "http://192.168.1.20:11434/v1",
                Some("http://192.168.1.20:11434"),
            ),
            ("https://api.groq.com/openai/v1", None),
            ("https://api.openai.com/v1", None),
            ("http://127.0.0.2:8000/v1", None),
            ("http://localhost.evil.example/v1", None),
            ("ftp://localhost/v1", None),
            ("not a url", None),
        ] {
            assert_eq!(ollama_origin(url).as_deref(), expected, "{url}");
        }
    }

    #[test]
    fn warmup_request_loads_the_model_without_a_prompt() {
        let request = warmup_request(
            &reqwest::Client::new(),
            "http://localhost:11434",
            "gemma4:e2b",
        )
        .build()
        .unwrap();
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(
            request.url().as_str(),
            "http://localhost:11434/api/generate"
        );
        let body: serde_json::Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "model": "gemma4:e2b", "keep_alive": "5m" })
        );
        assert!(request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none());
    }

    #[test]
    fn warmup_fires_at_most_once_per_30_seconds() {
        let gate = WarmupGate::new();
        let start = Instant::now();
        assert!(gate.try_acquire(start));
        assert!(!gate.try_acquire(start + Duration::from_secs(1)));
        assert!(!gate.try_acquire(start + Duration::from_secs(29)));
        assert!(gate.try_acquire(start + Duration::from_secs(30)));
        assert!(!gate.try_acquire(start + Duration::from_secs(31)));
    }

    #[tokio::test]
    async fn warmup_is_never_sent_to_a_cloud_provider() {
        for base_url in [
            "https://api.groq.com/openai/v1",
            "https://api.openai.com/v1",
        ] {
            assert!(!warm_up_ollama(&BatchSettings {
                base_url: base_url.to_string(),
                model: "whisper-large-v3".to_string(),
                api_key: Some("key".to_string()),
            }));
        }
    }

    /// Sample rate and PCM data of a 16-bit mono WAV. Walks the RIFF chunks:
    /// `say`/`afconvert` write JUNK/FLLR chunks around fmt/data.
    fn wav_pcm(wav: &[u8]) -> (u32, &[u8]) {
        let (mut at, mut sample_rate) = (12, 0u32);
        while at + 8 <= wav.len() {
            let len = u32::from_le_bytes(wav[at + 4..at + 8].try_into().unwrap()) as usize;
            match &wav[at..at + 4] {
                b"fmt " => {
                    sample_rate = u32::from_le_bytes(wav[at + 12..at + 16].try_into().unwrap())
                }
                b"data" if sample_rate > 0 => {
                    let end = (at + 8 + len).min(wav.len());
                    return (sample_rate, &wav[at + 8..end]);
                }
                _ => {}
            }
            at += 8 + len + len % 2;
        }
        panic!("not a PCM WAV");
    }

    /// Realistic check with macOS `say`: comma pauses (`[[slnc 250]]`) and
    /// sentence ends (`[[slnc 700]]`) through the live path (100 ms pieces into
    /// `BatchClient`). Every cut must land in a sentence-end pause. Skips where
    /// `say`/`afconvert` are unavailable.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn say_dictation_is_cut_only_at_sentence_ends() {
        const SENTENCES: [&str; 6] = [
            "Okay team, [[slnc 250]] here is the plan for this week.",
            "First, [[slnc 250]] we finish the incident review and share the timeline with everyone involved.",
            "Second, [[slnc 250]] the QA team publishes the automation coverage numbers, [[slnc 250]] broken down by product.",
            "Third, [[slnc 250]] the data team agrees the scope for the real time dashboards.",
            "Finally, [[slnc 250]] security runs the phishing simulation on the twentieth.",
            "Thanks everyone.",
        ];
        let dir = std::env::temp_dir().join(format!("ttm-say-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (aiff, wav) = (dir.join("say.aiff"), dir.join("say.wav"));
        let text = SENTENCES.join(" [[slnc 700]] ");
        let made = std::process::Command::new("say")
            .arg("-o")
            .arg(&aiff)
            .arg(&text)
            .status()
            .is_ok_and(|s| s.success())
            && std::process::Command::new("afconvert")
                .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
                .arg(&aiff)
                .arg(&wav)
                .status()
                .is_ok_and(|s| s.success());
        if !made {
            println!("skipped: `say`/`afconvert` unavailable");
            return;
        }
        let bytes = std::fs::read(&wav).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let (rate, pcm) = wav_pcm(&bytes);

        // Oracle: sentence ends are the long pauses of the whole clip.
        let rms = frame_rms(pcm, rate);
        let all = pauses(&rms, threshold_from_frames(&rms));
        let sentence_ends: Vec<(f64, f64)> = all
            .iter()
            .filter(|(_, len)| len * FRAME_MS >= 600)
            .map(|(start, len)| {
                let (a, b) = (*start as f64 * 0.02, (start + len) as f64 * 0.02);
                (a, b)
            })
            .collect();
        assert_eq!(sentence_ends.len(), SENTENCES.len() - 1, "pauses: {all:?}");

        let probe = Probe::default();
        let client = BatchClient::with_transcriber(fake(&probe, None, numbered));
        client.set_audio_format(rate);
        client.init_session(settings(None), RequestOptions::default());
        let mut rx = client.subscribe_to_transcriptions();
        for piece in pcm.chunks(rate as usize * 2 / 10) {
            client.send_audio(piece);
            // Let the queue work as it would while the user talks.
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        client.stop_recording();
        events(&mut rx).await;

        let mut at = 0.0;
        let lengths = probe.lengths();
        for (index, len) in lengths.iter().enumerate() {
            let start = at;
            at += at_seconds(*len, rate);
            let done = sentence_ends
                .iter()
                .filter(|(start, _)| *start <= at)
                .count();
            let last = index + 1 == lengths.len();
            println!(
                "chunk {} [{start:6.2}s - {at:6.2}s] ends after sentence {}{}",
                index + 1,
                if last { SENTENCES.len() } else { done },
                if last { " (release)" } else { "" }
            );
            if !last {
                assert!(
                    sentence_ends.iter().any(|(a, b)| (*a..*b).contains(&at)),
                    "cut at {at:.2}s is not in a sentence-end pause {sentence_ends:?}"
                );
            }
        }
        assert!(lengths.len() >= 3, "expected cuts in a {at:.1}s clip");
    }

    /// Real end-to-end check of incremental transcription against a local
    /// Ollama: feeds a WAV in real time (100 ms pieces), so chunks are cut and
    /// transcribed while "recording", then releases and waits for the text.
    /// `TTM_STT_WAV=/path/to/16k-mono.wav [TTM_STT_REF=/path/to/ref.txt] cargo test --manifest-path src-tauri/Cargo.toml -- --ignored ollama --nocapture`
    #[tokio::test]
    #[ignore]
    async fn ollama_transcribes_a_real_wav() {
        let path = std::env::var("TTM_STT_WAV").expect("set TTM_STT_WAV to a 16-bit mono WAV");
        let wav = std::fs::read(&path).unwrap();
        let (sample_rate, pcm) = wav_pcm(&wav);
        let settings = BatchSettings {
            base_url: std::env::var("TTM_STT_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".to_string()),
            model: std::env::var("TTM_STT_MODEL").unwrap_or_else(|_| "gemma4:e2b".to_string()),
            api_key: std::env::var("TTM_STT_API_KEY").ok(),
        };
        let options = RequestOptions::from_session(&["en".to_string()], vocab(&["TTM"]));
        println!("test_connection: {:?}", test_connection(&settings).await);

        // Wrap the real transcriber to time each chunk.
        let real = http_transcriber(&settings, &options, sample_rate);
        let offset = Arc::new(Mutex::new(0.0f64));
        let timed: TranscribeFn = Arc::new(move |chunk| {
            let (real, offset) = (real.clone(), offset.clone());
            Box::pin(async move {
                let duration = pcm_duration_seconds(&chunk, sample_rate);
                let start = {
                    let mut offset = offset.lock().unwrap();
                    *offset += duration;
                    *offset - duration
                };
                let started = Instant::now();
                let result = real(chunk).await;
                println!(
                    "chunk [{start:6.2}s - {:6.2}s] {duration:5.2}s audio -> {:5}ms: {result:?}",
                    start + duration,
                    started.elapsed().as_millis()
                );
                result
            })
        });
        let client = BatchClient::with_transcriber(timed);
        client.set_audio_format(sample_rate);
        println!("warm-up fired: {}", warm_up_ollama(&settings)); // what a key press does
        client.init_session(settings.clone(), options);
        let mut rx = client.subscribe_to_transcriptions();
        let piece = sample_rate as usize * 2 / 10;
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        for chunk in pcm.chunks(piece) {
            tick.tick().await;
            client.send_audio(chunk);
        }
        let released = Instant::now();
        client.stop_recording();
        let events = events_with_timeout(&mut rx, Duration::from_secs(300)).await;
        let text = final_text(&events).unwrap_or_else(|| panic!("no text: {events:?}"));
        println!(
            "release -> text: {}ms for {:.1}s of audio\ntranscript: {text}",
            released.elapsed().as_millis(),
            pcm_duration_seconds(pcm, sample_rate)
        );
        if let Ok(reference) = std::env::var("TTM_STT_REF") {
            println!(
                "reference:  {}",
                std::fs::read_to_string(reference).unwrap().trim()
            );
        }
        assert!(!text.is_empty());
    }

    async fn events_with_timeout(
        rx: &mut broadcast::Receiver<TranscriptionEvent>,
        limit: Duration,
    ) -> Vec<TranscriptionEvent> {
        let mut out = Vec::new();
        while let Ok(Ok(event)) = tokio::time::timeout(limit, rx.recv()).await {
            let end = matches!(event, TranscriptionEvent::SessionEnded);
            out.push(event);
            if end {
                break;
            }
        }
        out
    }
}
