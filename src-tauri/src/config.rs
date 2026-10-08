// File-based config manager for API key storage
use crate::audio::AudioDeviceSelection;
use crate::vocabulary::{default_custom_vocabulary, CustomVocabEntry};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static CONFIG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn config_mutex() -> &'static Mutex<()> {
    CONFIG_LOCK.get_or_init(|| Mutex::new(()))
}

/// Default endpointing (seconds) used when the field is absent.
pub const DEFAULT_ENDPOINTING: f64 = 0.1;

/// Default activation mode used when the field is absent.
pub const DEFAULT_ACTIVATION_MODE: &str = "push-to-talk";

/// Transcription provider defaults. Gladia stays the default so existing
/// configs keep working; the batch defaults target local Ollama.
pub const DEFAULT_PROVIDER: &str = crate::transcriber::PROVIDER_GLADIA;
pub const DEFAULT_STT_BASE_URL: &str = "http://localhost:11434/v1";
pub const DEFAULT_STT_MODEL: &str = "gemma4:e4b";

fn default_hotkey() -> String {
    if cfg!(target_os = "macos") {
        "Fn".to_string()
    } else {
        "Ctrl".to_string()
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    hotkey: Option<String>,
    languages: Option<Vec<String>>,
    code_switching: Option<bool>,
    /// Whether to leave the final transcription on the clipboard after a
    /// dictation session (instead of restoring the user's original clipboard).
    copy_to_clipboard: Option<bool>,
    /// How the trigger key starts a dictation: "push-to-talk" (hold) or
    /// "toggle" (press once to start, once to stop).
    activation_mode: Option<String>,
    custom_vocabulary: Option<Vec<CustomVocabEntry>>,
    /// Endpointing duration (seconds of silence before an utterance is
    /// considered final). Missing (older config files) defaults to 0.1 —
    /// see `endpointing`.
    endpointing: Option<f64>,
    /// Input selection policy. Older versions did not persist this setting;
    /// those installs intentionally migrate to the latency-safe automatic mode.
    audio_device_selection: Option<AudioDeviceSelection>,
    /// One-time flag: whether we've cleared the stale macOS Accessibility (TCC)
    /// entry left by older ad-hoc-signed builds. See `is_tcc_reset_done`.
    accessibility_tcc_reset_done: Option<bool>,
    /// Whether the native Accessibility prompt has been shown at least once.
    accessibility_prompted: Option<bool>,
    /// One-time TCC cleanup generation; bumped when a new migration reset is required.
    accessibility_cleanup_generation: Option<u32>,
    /// The app version that last ran. Used to seed vocabulary on upgrade and for logging.
    #[serde(skip_serializing_if = "Option::is_none")]
    installed_version: Option<String>,
    /// Transcription provider: "gladia" (live streaming) or "openai_compat" (batch).
    provider: Option<String>,
    /// Base URL of the OpenAI-compatible STT endpoint (e.g. local Ollama).
    stt_base_url: Option<String>,
    stt_model: Option<String>,
    /// Optional bearer key for the OpenAI-compatible endpoint (unused by local Ollama).
    #[serde(skip_serializing_if = "Option::is_none")]
    stt_api_key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: None,
            hotkey: Some(default_hotkey()),
            languages: Some(vec!["en".to_string()]),
            code_switching: Some(false),
            copy_to_clipboard: Some(false),
            activation_mode: Some(DEFAULT_ACTIVATION_MODE.to_string()),
            custom_vocabulary: Some(default_custom_vocabulary()),
            endpointing: Some(DEFAULT_ENDPOINTING),
            audio_device_selection: Some(AudioDeviceSelection::Automatic),
            accessibility_tcc_reset_done: Some(false),
            accessibility_prompted: Some(false),
            accessibility_cleanup_generation: Some(0),
            installed_version: None,
            provider: Some(DEFAULT_PROVIDER.to_string()),
            stt_base_url: Some(DEFAULT_STT_BASE_URL.to_string()),
            stt_model: Some(DEFAULT_STT_MODEL.to_string()),
            stt_api_key: None,
        }
    }
}

impl Config {
    /// Replace `None` with sensible defaults for fields that should never be null on disk.
    fn fill_defaults(&mut self) {
        let defaults = Config::default();
        if self.hotkey.is_none() {
            self.hotkey = defaults.hotkey;
        }
        if self.languages.is_none() {
            self.languages = defaults.languages;
        }
        if self.code_switching.is_none() {
            self.code_switching = defaults.code_switching;
        }
        if self.copy_to_clipboard.is_none() {
            self.copy_to_clipboard = defaults.copy_to_clipboard;
        }
        if self.activation_mode.is_none() {
            self.activation_mode = defaults.activation_mode;
        }
        if self.custom_vocabulary.is_none() {
            self.custom_vocabulary = defaults.custom_vocabulary;
        }
        if self.endpointing.is_none() {
            self.endpointing = defaults.endpointing;
        }
        if self.audio_device_selection.is_none() {
            self.audio_device_selection = defaults.audio_device_selection;
        }
        if self.accessibility_tcc_reset_done.is_none() {
            self.accessibility_tcc_reset_done = defaults.accessibility_tcc_reset_done;
        }
        if self.accessibility_prompted.is_none() {
            self.accessibility_prompted = defaults.accessibility_prompted;
        }
        if self.accessibility_cleanup_generation.is_none() {
            self.accessibility_cleanup_generation = defaults.accessibility_cleanup_generation;
        }
        if self.provider.is_none() {
            self.provider = defaults.provider;
        }
        if self.stt_base_url.is_none() {
            self.stt_base_url = defaults.stt_base_url;
        }
        if self.stt_model.is_none() {
            self.stt_model = defaults.stt_model;
        }
    }
}

pub fn get_config_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| {
        let fallback = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
        PathBuf::from(fallback)
    });
    base.join("ttm").join("config.json")
}

fn with_config(f: impl FnOnce(&mut Config)) -> Result<(), String> {
    let _guard = config_mutex().lock().unwrap();
    with_config_at(&get_config_path(), f)
}

fn with_config_at(path: &Path, f: impl FnOnce(&mut Config)) -> Result<(), String> {
    let mut config = load_config_from_path(path)?;
    f(&mut config);
    write_config_to_path(path, &config)
}

fn read_config<T>(f: impl FnOnce(&Config) -> T) -> Result<T, String> {
    let _guard = config_mutex().lock().unwrap();
    Ok(f(&load_config_inner()?))
}

fn load_config_inner() -> Result<Config, String> {
    load_config_from_path(&get_config_path())
}

fn load_config_from_path(path: &Path) -> Result<Config, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(error) => {
            let message = format!(
                "Failed to read existing config at {}: {error}",
                path.display()
            );
            log::error!("[config] {message}");
            return Err(message);
        }
    };

    let mut config: Config = serde_json::from_str(&text).map_err(|error| {
        let message = format!(
            "Failed to parse existing config at {}: {error}",
            path.display()
        );
        log::error!("[config] {message}");
        message
    })?;
    config.fill_defaults();
    Ok(config)
}

fn write_config_to_path(path: &Path, config: &Config) -> Result<(), String> {
    let mut config = config.clone();
    preserve_custom_vocabulary(path, &mut config);
    config.fill_defaults();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, json).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    fs::rename(&temp_path, path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Keep vocabulary on disk when a partial config write omits it.
fn preserve_custom_vocabulary(path: &Path, config: &mut Config) {
    if config.custom_vocabulary.is_some() {
        return;
    }
    if !path.exists() {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    if let Some(vocab) = value.get("custom_vocabulary") {
        if let Ok(parsed) = serde_json::from_value::<Vec<CustomVocabEntry>>(vocab.clone()) {
            if !parsed.is_empty() {
                config.custom_vocabulary = Some(parsed);
            }
        }
    }
}

/// Endpointing in seconds. Defaults to `DEFAULT_ENDPOINTING` when absent.
/// Used internally (e.g. as the fallback in `init_gladia_session`); the
/// `get_endpointing` Tauri command wraps this for the frontend.
pub fn endpointing() -> Result<f64, String> {
    read_config(|config| config.endpointing.unwrap_or(DEFAULT_ENDPOINTING))
}

/// Whether the final transcription should be left on the clipboard after a
/// dictation session. Defaults to `false` when absent. The
/// `get_copy_to_clipboard` Tauri command wraps this for the frontend.
pub fn copy_to_clipboard() -> Result<bool, String> {
    read_config(|config| config.copy_to_clipboard.unwrap_or(false))
}

pub fn audio_device_selection() -> Result<AudioDeviceSelection, String> {
    read_config(|config| {
        config
            .audio_device_selection
            .clone()
            .unwrap_or(AudioDeviceSelection::Automatic)
    })
}

/// Provider and OpenAI-compatible endpoint settings, as exchanged with the UI.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SttSettings {
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

impl SttSettings {
    fn from_config(config: &Config) -> Self {
        Self {
            provider: config
                .provider
                .clone()
                .unwrap_or_else(|| DEFAULT_PROVIDER.to_string()),
            base_url: config
                .stt_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_STT_BASE_URL.to_string()),
            model: config
                .stt_model
                .clone()
                .unwrap_or_else(|| DEFAULT_STT_MODEL.to_string()),
            api_key: config.stt_api_key.clone().unwrap_or_default(),
        }
    }

    pub fn batch_settings(&self) -> crate::batch_stt::BatchSettings {
        crate::batch_stt::BatchSettings {
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            api_key: Some(self.api_key.clone()).filter(|key| !key.trim().is_empty()),
        }
    }
}

/// Trim and validate settings coming from the UI before they reach disk.
pub fn validate_stt_settings(settings: SttSettings) -> Result<SttSettings, String> {
    let provider = settings.provider.trim().to_string();
    if provider != crate::transcriber::PROVIDER_GLADIA
        && provider != crate::transcriber::PROVIDER_OPENAI_COMPAT
    {
        return Err(format!("Unknown transcription provider: {provider}"));
    }
    let base_url = settings.base_url.trim().trim_end_matches('/').to_string();
    let parsed = url::Url::parse(&base_url)
        .map_err(|_| format!("Base URL is not a valid URL: {base_url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Base URL must start with http:// or https://".to_string());
    }
    let model = settings.model.trim().to_string();
    if model.is_empty() {
        return Err("Model is required".to_string());
    }
    Ok(SttSettings {
        provider,
        base_url,
        model,
        api_key: settings.api_key.trim().to_string(),
    })
}

pub fn stt_settings() -> Result<SttSettings, String> {
    read_config(SttSettings::from_config)
}

fn save_stt_settings_at(path: &Path, settings: SttSettings) -> Result<(), String> {
    let settings = validate_stt_settings(settings)?;
    with_config_at(path, |config| {
        config.provider = Some(settings.provider);
        config.stt_base_url = Some(settings.base_url);
        config.stt_model = Some(settings.model);
        config.stt_api_key = Some(settings.api_key).filter(|key| !key.is_empty());
    })
}

pub fn save_custom_vocabulary(vocabulary: Vec<CustomVocabEntry>) -> Result<(), String> {
    with_config(|config| {
        config.custom_vocabulary = Some(vocabulary);
    })
}

pub fn get_custom_vocabulary() -> Result<Vec<CustomVocabEntry>, String> {
    read_config(|config| config.custom_vocabulary.clone().unwrap_or_default())
}

/// Seed the built-in default vocabulary when the list is missing or empty.
pub fn seed_default_vocabulary_if_empty() -> Result<bool, String> {
    let mut seeded = false;
    with_config(|config| {
        let empty = config
            .custom_vocabulary
            .as_ref()
            .is_none_or(|entries| entries.is_empty());
        if empty {
            config.custom_vocabulary = Some(default_custom_vocabulary());
            seeded = true;
        }
    })?;
    Ok(seeded)
}

/// Whether the one-time macOS Accessibility TCC reset has already run.
/// Missing field (older config files) is treated as "not done".
pub fn is_tcc_reset_done() -> Result<bool, String> {
    read_config(|config| config.accessibility_tcc_reset_done.unwrap_or(false))
}

pub fn mark_tcc_reset_done() -> Result<(), String> {
    with_config(|config| {
        config.accessibility_tcc_reset_done = Some(true);
    })
}

/// The app version recorded on the last run, or `None` for a fresh install.
pub fn get_installed_version() -> Result<Option<String>, String> {
    read_config(|config| config.installed_version.clone())
}

pub fn set_installed_version(version: &str) -> Result<(), String> {
    with_config(|config| {
        config.installed_version = Some(version.to_string());
    })
}

/// Whether the native Accessibility prompt has been shown.
pub fn is_accessibility_prompted() -> Result<bool, String> {
    read_config(|config| config.accessibility_prompted.unwrap_or(false))
}

pub fn mark_accessibility_prompted() -> Result<(), String> {
    with_config(|config| {
        config.accessibility_prompted = Some(true);
    })
}

pub fn get_cleanup_generation() -> Result<u32, String> {
    read_config(|config| config.accessibility_cleanup_generation.unwrap_or(0))
}

pub fn set_cleanup_generation(generation: u32) -> Result<(), String> {
    with_config(|config| {
        config.accessibility_cleanup_generation = Some(generation);
    })
}

// --- Tauri commands ---
// These own their persistence logic directly (no internal wrapper indirection),
// mirroring the command style in `vocabulary.rs`.

#[tauri::command]
pub async fn save_api_key(api_key: String) -> Result<(), String> {
    let result = with_config(|config| {
        config.api_key = Some(api_key);
    });
    match &result {
        Ok(()) => log::info!("[config] API key save succeeded (present=true)"),
        Err(error) => log::error!("[config] API key save failed: {error}"),
    }
    result
}

#[tauri::command]
pub async fn get_api_key() -> Result<Option<String>, String> {
    let result = read_config(|config| config.api_key.clone());
    match &result {
        Ok(api_key) => log::info!(
            "[config] API key load succeeded (present={})",
            api_key.as_ref().is_some_and(|key| !key.is_empty())
        ),
        Err(error) => log::error!("[config] API key load failed: {error}"),
    }
    result
}

#[tauri::command]
pub async fn delete_api_key() -> Result<(), String> {
    let _guard = config_mutex().lock().unwrap();
    delete_api_key_at(&get_config_path())
}

fn delete_api_key_at(path: &Path) -> Result<(), String> {
    with_config_at(path, |config| {
        config.api_key = None;
    })
}

#[tauri::command]
pub async fn reset_corrupted_config(confirmed: bool) -> Result<String, String> {
    let _guard = config_mutex().lock().unwrap();
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    reset_corrupted_config_at(&get_config_path(), confirmed, &timestamp)
        .map(|path| path.to_string_lossy().into_owned())
}

fn reset_corrupted_config_at(
    path: &Path,
    confirmed: bool,
    timestamp: &str,
) -> Result<PathBuf, String> {
    if !confirmed {
        return Err("Settings reset requires explicit user confirmation.".into());
    }

    if load_config_from_path(path).is_ok() {
        return Err("Settings reset refused because the configuration is readable.".into());
    }

    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("config");
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("json");
    let backup_path = path.with_file_name(format!("{stem}.broken-{timestamp}.{extension}"));
    if backup_path.exists() {
        return Err(format!(
            "Settings backup already exists at {}.",
            backup_path.display()
        ));
    }

    fs::rename(path, &backup_path).map_err(|error| {
        format!(
            "Failed to back up unreadable settings from {} to {}: {error}",
            path.display(),
            backup_path.display()
        )
    })?;

    if let Err(write_error) = write_config_to_path(path, &Config::default()) {
        let temp_path = path.with_extension("tmp");
        if temp_path.is_file() {
            let _ = fs::remove_file(&temp_path);
        }
        if let Err(restore_error) = fs::rename(&backup_path, path) {
            let message = format!(
                "Failed to create fresh settings ({write_error}) and failed to restore the backup ({restore_error}). Backup remains at {}.",
                backup_path.display()
            );
            log::error!("[config] {message}");
            return Err(message);
        }

        let message = format!(
            "Failed to create fresh settings; the original file was restored: {write_error}"
        );
        log::error!("[config] {message}");
        return Err(message);
    }

    log::info!(
        "[config] user-confirmed settings reset succeeded; unreadable config backed up to {}",
        backup_path.display()
    );
    Ok(backup_path)
}

#[tauri::command]
pub async fn save_hotkey(hotkey: String) -> Result<(), String> {
    let _guard = config_mutex().lock().unwrap();
    save_hotkey_at(&get_config_path(), &hotkey)
}

fn save_hotkey_at(path: &Path, hotkey: &str) -> Result<(), String> {
    crate::hotkey::validate_hotkey(hotkey)?;
    let hotkey = crate::hotkey::canonical_hotkey(hotkey);
    log::info!("[hotkey] saving shortcut config: {hotkey:?}");
    with_config_at(path, |config| {
        config.hotkey = Some(hotkey);
    })
}

#[tauri::command]
pub async fn save_stt_settings(settings: SttSettings) -> Result<(), String> {
    let _guard = config_mutex().lock().unwrap();
    let provider = settings.provider.clone();
    let result = save_stt_settings_at(&get_config_path(), settings);
    match &result {
        Ok(()) => log::info!("[config] transcription provider saved ({provider})"),
        Err(error) => log::error!("[config] transcription provider save failed: {error}"),
    }
    result
}

#[tauri::command]
pub async fn get_stt_settings() -> Result<SttSettings, String> {
    stt_settings()
}

#[tauri::command]
pub async fn get_hotkey() -> Result<Option<String>, String> {
    read_config(|config| config.hotkey.clone())
}

#[tauri::command]
pub async fn save_language_settings(
    languages: Vec<String>,
    code_switching: bool,
) -> Result<(), String> {
    with_config(|config| {
        config.languages = Some(languages);
        config.code_switching = Some(code_switching);
    })
}

#[tauri::command]
pub async fn get_language_settings() -> Result<(Option<Vec<String>>, Option<bool>), String> {
    read_config(|config| (config.languages.clone(), config.code_switching))
}

#[tauri::command]
pub async fn save_audio_device_selection(selection: AudioDeviceSelection) -> Result<(), String> {
    with_config(|config| {
        config.audio_device_selection = Some(selection);
    })
}

#[tauri::command]
pub async fn get_audio_device_selection() -> Result<AudioDeviceSelection, String> {
    audio_device_selection()
}

#[tauri::command]
pub async fn save_endpointing(endpointing: f64) -> Result<(), String> {
    with_config(|config| {
        config.endpointing = Some(endpointing.clamp(0.05, 1.0));
    })
}

#[tauri::command]
pub async fn get_endpointing() -> Result<f64, String> {
    endpointing()
}

#[tauri::command]
pub async fn save_copy_to_clipboard(enabled: bool) -> Result<(), String> {
    with_config(|config| {
        config.copy_to_clipboard = Some(enabled);
    })
}

#[tauri::command]
pub async fn get_copy_to_clipboard() -> Result<bool, String> {
    copy_to_clipboard()
}

#[tauri::command]
pub async fn save_activation_mode(mode: String) -> Result<(), String> {
    if mode != "toggle" && mode != DEFAULT_ACTIVATION_MODE {
        return Err(format!("Unknown activation mode: {mode}"));
    }
    with_config(|config| {
        config.activation_mode = Some(mode);
    })
}

#[tauri::command]
pub async fn get_activation_mode() -> Result<String, String> {
    read_config(|config| {
        config
            .activation_mode
            .clone()
            .unwrap_or_else(|| DEFAULT_ACTIVATION_MODE.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestConfigDir(PathBuf);

    impl TestConfigDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("ttm-config-test-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("config.json")
        }
    }

    impl Drop for TestConfigDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn default_config_serializes_without_nulls() {
        let json = serde_json::to_string_pretty(&Config::default()).unwrap();
        assert!(!json.contains("null"));
        assert!(!json.contains("api_key"));
        assert!(!json.contains("installed_version"));
    }

    #[test]
    fn explicit_nulls_are_filled_on_load() {
        let json = r#"{
            "api_key": null,
            "hotkey": null,
            "languages": null,
            "code_switching": null,
            "copy_to_clipboard": null,
            "custom_vocabulary": null,
            "endpointing": null,
            "accessibility_tcc_reset_done": null,
            "accessibility_prompted": null,
            "accessibility_cleanup_generation": null,
            "installed_version": null
        }"#;
        let mut config: Config = serde_json::from_str(json).unwrap();
        config.fill_defaults();
        assert_eq!(config.hotkey.as_deref(), Some(default_hotkey().as_str()));
        assert_eq!(config.languages, Some(vec!["en".to_string()]));
        assert_eq!(config.code_switching, Some(false));
        assert_eq!(config.copy_to_clipboard, Some(false));
        assert_eq!(config.custom_vocabulary, Some(default_custom_vocabulary()));
        assert_eq!(config.endpointing, Some(DEFAULT_ENDPOINTING));
        assert_eq!(
            config.audio_device_selection,
            Some(AudioDeviceSelection::Automatic)
        );
        assert_eq!(config.accessibility_tcc_reset_done, Some(false));
        assert_eq!(config.accessibility_prompted, Some(false));
        assert_eq!(config.accessibility_cleanup_generation, Some(0));
        assert!(config.api_key.is_none());
        assert!(config.installed_version.is_none());
    }

    #[test]
    fn stt_settings_default_to_gladia_with_ollama_endpoint() {
        let settings = SttSettings::from_config(&Config::default());
        assert_eq!(
            settings,
            SttSettings {
                provider: "gladia".to_string(),
                base_url: "http://localhost:11434/v1".to_string(),
                model: "gemma4:e4b".to_string(),
                api_key: String::new(),
            }
        );
        let json = serde_json::to_string(&Config::default()).unwrap();
        assert!(!json.contains("stt_api_key"));
    }

    #[test]
    fn old_config_without_provider_fields_loads_with_defaults() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        // Shape of a config written before the provider setting existed.
        fs::write(
            &path,
            r#"{"api_key":"gladia-secret","hotkey":"Fn","languages":["en","es"],"endpointing":0.3}"#,
        )
        .unwrap();

        let config = load_config_from_path(&path).unwrap();
        let settings = SttSettings::from_config(&config);
        assert_eq!(settings.provider, "gladia");
        assert_eq!(settings.base_url, DEFAULT_STT_BASE_URL);
        assert_eq!(settings.model, DEFAULT_STT_MODEL);
        assert_eq!(settings.api_key, "");
        assert_eq!(config.api_key.as_deref(), Some("gladia-secret"));
        assert_eq!(config.endpointing, Some(0.3));
    }

    #[test]
    fn explicit_null_provider_fields_are_filled_on_load() {
        let mut config: Config = serde_json::from_str(
            r#"{"provider":null,"stt_base_url":null,"stt_model":null,"stt_api_key":null}"#,
        )
        .unwrap();
        config.fill_defaults();
        assert_eq!(config.provider.as_deref(), Some("gladia"));
        assert_eq!(config.stt_base_url.as_deref(), Some(DEFAULT_STT_BASE_URL));
        assert_eq!(config.stt_model.as_deref(), Some(DEFAULT_STT_MODEL));
        assert!(config.stt_api_key.is_none());
    }

    fn stt(provider: &str, base_url: &str, model: &str, api_key: &str) -> SttSettings {
        SttSettings {
            provider: provider.to_string(),
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }

    #[test]
    fn validate_stt_settings_normalizes_input() {
        assert_eq!(
            validate_stt_settings(stt(
                " openai_compat ",
                " https://api.groq.com/openai/v1/ ",
                " whisper-large-v3 ",
                " gsk_x "
            )),
            Ok(stt(
                "openai_compat",
                "https://api.groq.com/openai/v1",
                "whisper-large-v3",
                "gsk_x"
            ))
        );
    }

    #[test]
    fn validate_stt_settings_rejects_bad_input() {
        for bad in [
            stt("whisper", DEFAULT_STT_BASE_URL, "m", ""),
            stt("openai_compat", "localhost:11434/v1", "m", ""),
            stt("openai_compat", "ftp://localhost/v1", "m", ""),
            stt("openai_compat", "not a url", "m", ""),
            stt("openai_compat", "", "m", ""),
            stt("openai_compat", DEFAULT_STT_BASE_URL, "  ", ""),
        ] {
            assert!(validate_stt_settings(bad.clone()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn saving_stt_settings_round_trips_and_preserves_gladia_key() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        write_config_to_path(
            &path,
            &Config {
                api_key: Some("gladia-secret".to_string()),
                ..Config::default()
            },
        )
        .unwrap();

        save_stt_settings_at(
            &path,
            stt(
                "openai_compat",
                "http://localhost:11434/v1/",
                "gemma4:e4b",
                "",
            ),
        )
        .unwrap();
        let loaded = load_config_from_path(&path).unwrap();
        assert_eq!(
            SttSettings::from_config(&loaded),
            stt(
                "openai_compat",
                "http://localhost:11434/v1",
                "gemma4:e4b",
                ""
            )
        );
        assert!(loaded.stt_api_key.is_none());
        assert_eq!(loaded.api_key.as_deref(), Some("gladia-secret"));

        // Invalid input is rejected and leaves the file untouched.
        assert!(save_stt_settings_at(&path, stt("bogus", "x", "", "")).is_err());
        assert_eq!(
            SttSettings::from_config(&load_config_from_path(&path).unwrap()).provider,
            "openai_compat"
        );
    }

    #[test]
    fn batch_settings_omit_blank_key() {
        assert_eq!(
            stt("openai_compat", "http://h/v1", "m", "")
                .batch_settings()
                .api_key,
            None
        );
        assert_eq!(
            stt("openai_compat", "http://h/v1", "m", "k")
                .batch_settings()
                .api_key,
            Some("k".to_string())
        );
    }

    #[test]
    fn accessibility_prompted_defaults_to_false() {
        let config = Config::default();
        assert_eq!(config.accessibility_prompted, Some(false));
    }

    #[test]
    fn accessibility_cleanup_generation_defaults_to_zero() {
        let config = Config::default();
        assert_eq!(config.accessibility_cleanup_generation, Some(0));
    }

    #[test]
    fn missing_audio_selection_migrates_to_automatic() {
        let mut config: Config = serde_json::from_str("{}").unwrap();
        config.fill_defaults();
        assert_eq!(
            config.audio_device_selection,
            Some(AudioDeviceSelection::Automatic)
        );
    }

    #[test]
    fn config_with_api_key_serializes_without_nulls() {
        let mut config = Config {
            api_key: Some("test-key".to_string()),
            ..Config::default()
        };
        config.fill_defaults();
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(!json.contains("null"));
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.api_key.as_deref(), Some("test-key"));
        assert_eq!(loaded.hotkey.as_deref(), Some(default_hotkey().as_str()));
    }

    #[test]
    fn seed_default_vocabulary_if_empty_skips_non_empty() {
        let existing = vec![CustomVocabEntry {
            value: "acme".to_string(),
            pronunciations: None,
            language: None,
            intensity: 0.5,
        }];
        let config = Config {
            custom_vocabulary: Some(existing.clone()),
            ..Config::default()
        };
        let empty = config
            .custom_vocabulary
            .as_ref()
            .is_none_or(|entries| entries.is_empty());
        assert!(!empty);
        assert_ne!(config.custom_vocabulary, Some(default_custom_vocabulary()));
        assert_eq!(config.custom_vocabulary, Some(existing));
    }

    #[test]
    fn missing_config_loads_defaults() {
        let dir = TestConfigDir::new();
        let config = load_config_from_path(&dir.config_path()).unwrap();

        assert!(config.api_key.is_none());
        assert_eq!(config.hotkey.as_deref(), Some(default_hotkey().as_str()));
    }

    #[test]
    fn malformed_config_returns_error_without_modifying_file() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let malformed = r#"{"api_key":"secret""#;
        fs::write(&path, malformed).unwrap();

        assert!(load_config_from_path(&path).is_err());
        assert!(with_config_at(&path, |config| {
            config.hotkey = Some("CmdRight".to_string());
        })
        .is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), malformed);
    }

    #[test]
    fn unreadable_config_path_returns_error() {
        let dir = TestConfigDir::new();

        assert!(load_config_from_path(&dir.0).is_err());
        assert!(dir.0.is_dir());
    }

    #[test]
    fn updating_another_setting_preserves_api_key() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let config = Config {
            api_key: Some("secret".to_string()),
            hotkey: Some("Fn".to_string()),
            ..Config::default()
        };
        write_config_to_path(&path, &config).unwrap();

        with_config_at(&path, |config| {
            config.hotkey = Some("CmdRight".to_string());
        })
        .unwrap();

        let loaded = load_config_from_path(&path).unwrap();
        assert_eq!(loaded.api_key.as_deref(), Some("secret"));
        assert_eq!(loaded.hotkey.as_deref(), Some("CmdRight"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn section_hotkey_round_trips_as_stable_token() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();

        save_hotkey_at(&path, "Section").unwrap();
        assert_eq!(
            load_config_from_path(&path).unwrap().hotkey.as_deref(),
            Some("Section")
        );

        // The literal character is accepted but persisted as the token.
        save_hotkey_at(&path, "§").unwrap();
        assert_eq!(
            load_config_from_path(&path).unwrap().hotkey.as_deref(),
            Some("Section")
        );

        // A bare typing key is rejected and leaves the stored hotkey untouched.
        assert!(save_hotkey_at(&path, "A").is_err());
        assert_eq!(
            load_config_from_path(&path).unwrap().hotkey.as_deref(),
            Some("Section")
        );
    }

    #[test]
    fn deleting_api_key_preserves_other_settings() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let config = Config {
            api_key: Some("secret".to_string()),
            hotkey: Some("CmdRight".to_string()),
            installed_version: Some("1.0.0".to_string()),
            ..Config::default()
        };
        write_config_to_path(&path, &config).unwrap();

        delete_api_key_at(&path).unwrap();

        let loaded = load_config_from_path(&path).unwrap();
        assert!(loaded.api_key.is_none());
        assert_eq!(loaded.hotkey.as_deref(), Some("CmdRight"));
        assert_eq!(loaded.installed_version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn corrupted_config_reset_requires_explicit_confirmation() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let malformed = r#"{"api_key":"secret""#;
        fs::write(&path, malformed).unwrap();

        let result = reset_corrupted_config_at(&path, false, "20260831T120000000Z");

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), malformed);
    }

    #[test]
    fn corrupted_config_reset_refuses_a_readable_config() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let config = Config {
            api_key: Some("secret".to_string()),
            ..Config::default()
        };
        write_config_to_path(&path, &config).unwrap();

        let result = reset_corrupted_config_at(&path, true, "20260831T120000000Z");

        assert!(result.is_err());
        assert_eq!(
            load_config_from_path(&path).unwrap().api_key.as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn corrupted_config_reset_backs_up_the_original_and_writes_defaults() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let malformed = r#"{"api_key":"secret""#;
        fs::write(&path, malformed).unwrap();

        let backup = reset_corrupted_config_at(&path, true, "20260831T120000000Z").unwrap();

        assert_eq!(fs::read_to_string(&backup).unwrap(), malformed);
        let reset = load_config_from_path(&path).unwrap();
        assert!(reset.api_key.is_none());
        assert_eq!(reset.hotkey.as_deref(), Some(default_hotkey().as_str()));
    }

    #[test]
    fn corrupted_config_reset_restores_the_original_when_default_write_fails() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let malformed = r#"{"api_key":"secret""#;
        fs::write(&path, malformed).unwrap();
        fs::create_dir(path.with_extension("tmp")).unwrap();

        let result = reset_corrupted_config_at(&path, true, "20260831T120000000Z");

        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), malformed);
        assert!(!dir
            .0
            .join("config.broken-20260831T120000000Z.json")
            .exists());
    }
}
