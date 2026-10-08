// File-based config manager for API key storage
use crate::audio::AudioDeviceSelection;
use crate::transcriber::{PROVIDER_GLADIA, PROVIDER_OPENAI_COMPAT};
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

/// Defaults for the local Ollama profile a fresh install starts with.
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
    /// Legacy (pre-profiles) Gladia key. Read for migration, never written.
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    hotkey: Option<String>,
    languages: Option<Vec<String>>,
    /// Legacy: now per Gladia profile. Read for migration, never written.
    #[serde(skip_serializing_if = "Option::is_none")]
    code_switching: Option<bool>,
    /// Whether to leave the final transcription on the clipboard after a
    /// dictation session (instead of restoring the user's original clipboard).
    copy_to_clipboard: Option<bool>,
    /// How the trigger key starts a dictation: "push-to-talk" (hold) or
    /// "toggle" (press once to start, once to stop).
    activation_mode: Option<String>,
    custom_vocabulary: Option<Vec<CustomVocabEntry>>,
    /// Legacy: now per Gladia profile. Read for migration, never written.
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// Legacy single provider choice ("gladia" | "openai_compat"). Read for
    /// migration, never written.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    /// Legacy OpenAI-compatible endpoint fields. Read for migration, never written.
    #[serde(skip_serializing_if = "Option::is_none")]
    stt_base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stt_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stt_api_key: Option<String>,
    /// Saved transcription setups; exactly one is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    profiles: Option<Vec<Profile>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_profile_id: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: None,
            hotkey: Some(default_hotkey()),
            languages: Some(vec!["en".to_string()]),
            code_switching: None,
            copy_to_clipboard: Some(false),
            activation_mode: Some(DEFAULT_ACTIVATION_MODE.to_string()),
            custom_vocabulary: Some(default_custom_vocabulary()),
            endpointing: None,
            audio_device_selection: Some(AudioDeviceSelection::Automatic),
            accessibility_tcc_reset_done: Some(false),
            accessibility_prompted: Some(false),
            accessibility_cleanup_generation: Some(0),
            installed_version: None,
            provider: None,
            stt_base_url: None,
            stt_model: None,
            stt_api_key: None,
            profiles: None,
            active_profile_id: None,
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
        if self.copy_to_clipboard.is_none() {
            self.copy_to_clipboard = defaults.copy_to_clipboard;
        }
        if self.activation_mode.is_none() {
            self.activation_mode = defaults.activation_mode;
        }
        if self.custom_vocabulary.is_none() {
            self.custom_vocabulary = defaults.custom_vocabulary;
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
        self.migrate_profiles();
    }

    /// Build `profiles` from the legacy single-provider fields, then drop those
    /// fields so they are no longer written. Deterministic ids make this
    /// idempotent even when a migrated config is read without being saved.
    fn migrate_profiles(&mut self) {
        if self
            .profiles
            .as_ref()
            .is_none_or(|profiles| profiles.is_empty())
        {
            let legacy_provider = self.provider.as_deref().map(str::trim);
            let gladia_key = self
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty());
            let base_url = self
                .stt_base_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let model = self
                .stt_model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let stt_key = self
                .stt_api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty());
            let batch_customised = base_url
                .is_some_and(|url| url.trim_end_matches('/') != DEFAULT_STT_BASE_URL)
                || model.is_some_and(|model| model != DEFAULT_STT_MODEL)
                || stt_key.is_some();

            let mut profiles = Vec::new();
            if gladia_key.is_some() || legacy_provider == Some(PROVIDER_GLADIA) {
                profiles.push(Profile {
                    id: DEFAULT_GLADIA_PROFILE_ID.to_string(),
                    name: DEFAULT_GLADIA_PROFILE_NAME.to_string(),
                    settings: ProfileSettings::Gladia {
                        api_key: gladia_key.unwrap_or_default().to_string(),
                        region: GLADIA_REGION_AUTO.to_string(),
                        endpointing: clamp_endpointing(
                            self.endpointing.unwrap_or(DEFAULT_ENDPOINTING),
                        ),
                        code_switching: self.code_switching.unwrap_or(false),
                    },
                });
            }
            if legacy_provider == Some(PROVIDER_OPENAI_COMPAT)
                || batch_customised
                || profiles.is_empty()
            {
                profiles.push(Profile {
                    id: DEFAULT_LOCAL_PROFILE_ID.to_string(),
                    name: DEFAULT_LOCAL_PROFILE_NAME.to_string(),
                    settings: ProfileSettings::OpenAiCompat {
                        base_url: base_url
                            .unwrap_or(DEFAULT_STT_BASE_URL)
                            .trim_end_matches('/')
                            .to_string(),
                        model: model.unwrap_or(DEFAULT_STT_MODEL).to_string(),
                        api_key: stt_key.unwrap_or_default().to_string(),
                    },
                });
            }
            let active = match legacy_provider {
                Some(PROVIDER_OPENAI_COMPAT) => DEFAULT_LOCAL_PROFILE_ID,
                _ => profiles[0].id.as_str(),
            };
            self.active_profile_id = Some(active.to_string());
            self.profiles = Some(profiles);
        }

        // Repair a hand-edited active id rather than bricking dictation.
        let profiles = self.profiles.as_ref().expect("profiles set above");
        let active_exists = self
            .active_profile_id
            .as_ref()
            .is_some_and(|id| profiles.iter().any(|profile| &profile.id == id));
        if !active_exists {
            self.active_profile_id = Some(profiles[0].id.clone());
        }

        self.api_key = None;
        self.code_switching = None;
        self.endpointing = None;
        self.provider = None;
        self.stt_base_url = None;
        self.stt_model = None;
        self.stt_api_key = None;
    }

    fn profiles(&self) -> &[Profile] {
        self.profiles.as_deref().unwrap_or_default()
    }

    fn profiles_mut(&mut self) -> &mut Vec<Profile> {
        self.profiles.get_or_insert_with(Vec::new)
    }

    fn profiles_state(&self) -> ProfilesState {
        ProfilesState {
            profiles: self.profiles().to_vec(),
            active_profile_id: self.active_profile_id.clone().unwrap_or_default(),
        }
    }

    fn active_profile(&self) -> Result<Profile, String> {
        let id = self.active_profile_id.as_deref().unwrap_or_default();
        self.profiles()
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| "No active transcription profile".to_string())
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
            let mut config = Config::default();
            config.fill_defaults();
            return Ok(config);
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

// --- Transcription profiles ---

pub const GLADIA_REGION_AUTO: &str = "auto";
const GLADIA_REGIONS: [&str; 3] = [GLADIA_REGION_AUTO, "eu-west", "us-west"];
const MIN_ENDPOINTING: f64 = 0.05;
const MAX_ENDPOINTING: f64 = 1.0;
const MAX_PROFILE_NAME_CHARS: usize = 60;
pub const DEFAULT_GLADIA_PROFILE_ID: &str = "gladia";
pub const DEFAULT_GLADIA_PROFILE_NAME: &str = "Gladia";
pub const DEFAULT_LOCAL_PROFILE_ID: &str = "local-ollama";
pub const DEFAULT_LOCAL_PROFILE_NAME: &str = "Local Ollama";

fn clamp_endpointing(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(MIN_ENDPOINTING, MAX_ENDPOINTING)
    } else {
        DEFAULT_ENDPOINTING
    }
}

/// Kind-specific part of a profile. Serialized with a `kind` tag of
/// `"gladia"` or `"openai_compat"`; every field is required and typed, so
/// explicit nulls and wrong types are rejected at the IPC boundary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum ProfileSettings {
    #[serde(rename = "gladia")]
    Gladia {
        api_key: String,
        /// `"auto"` (timezone-based), `"eu-west"` or `"us-west"`.
        region: String,
        /// Seconds of silence before Gladia finalises an utterance.
        endpointing: f64,
        code_switching: bool,
    },
    #[serde(rename = "openai_compat")]
    OpenAiCompat {
        base_url: String,
        model: String,
        /// Optional bearer key (unused by local Ollama).
        #[serde(default)]
        api_key: String,
    },
}

impl ProfileSettings {
    pub fn batch_settings(&self) -> Option<crate::batch_stt::BatchSettings> {
        match self {
            ProfileSettings::OpenAiCompat {
                base_url,
                model,
                api_key,
            } => Some(crate::batch_stt::BatchSettings {
                base_url: base_url.clone(),
                model: model.clone(),
                api_key: Some(api_key.clone()).filter(|key| !key.trim().is_empty()),
            }),
            ProfileSettings::Gladia { .. } => None,
        }
    }

    /// Trim and validate before anything reaches disk.
    fn normalized(self) -> Result<Self, String> {
        match self {
            ProfileSettings::Gladia {
                api_key,
                region,
                endpointing,
                code_switching,
            } => {
                let region = region.trim().to_string();
                if !GLADIA_REGIONS.contains(&region.as_str()) {
                    return Err(format!(
                        "Unknown Gladia region: {region} (use auto, eu-west or us-west)"
                    ));
                }
                if !endpointing.is_finite()
                    || !(MIN_ENDPOINTING..=MAX_ENDPOINTING).contains(&endpointing)
                {
                    return Err(format!(
                        "Endpointing must be between {MIN_ENDPOINTING} and {MAX_ENDPOINTING} seconds"
                    ));
                }
                Ok(ProfileSettings::Gladia {
                    api_key: api_key.trim().to_string(),
                    region,
                    endpointing,
                    code_switching,
                })
            }
            ProfileSettings::OpenAiCompat {
                base_url,
                model,
                api_key,
            } => {
                let base_url = base_url.trim().trim_end_matches('/').to_string();
                let parsed = url::Url::parse(&base_url)
                    .map_err(|_| format!("Base URL is not a valid URL: {base_url}"))?;
                if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                    return Err("Base URL must start with http:// or https://".to_string());
                }
                let model = model.trim().to_string();
                if model.is_empty() {
                    return Err("Model is required".to_string());
                }
                Ok(ProfileSettings::OpenAiCompat {
                    base_url,
                    model,
                    api_key: api_key.trim().to_string(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub settings: ProfileSettings,
}

/// A profile as sent by the UI on create (the backend assigns the id).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewProfile {
    pub name: String,
    #[serde(flatten)]
    pub settings: ProfileSettings,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProfilesState {
    pub profiles: Vec<Profile>,
    pub active_profile_id: String,
}

/// Trimmed, non-empty, and unique (case-insensitive) among the other profiles.
fn validate_profile_name(
    name: &str,
    profiles: &[Profile],
    exclude_id: Option<&str>,
) -> Result<String, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Profile name is required".to_string());
    }
    if name.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(format!(
            "Profile name must be at most {MAX_PROFILE_NAME_CHARS} characters"
        ));
    }
    let key = name.to_lowercase();
    let taken = profiles
        .iter()
        .filter(|profile| Some(profile.id.as_str()) != exclude_id)
        .any(|profile| profile.name.trim().to_lowercase() == key);
    if taken {
        return Err(format!("A profile named \"{name}\" already exists"));
    }
    Ok(name)
}

fn generate_profile_id(profiles: &[Profile]) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let id = format!("p-{nanos:x}-{:x}", COUNTER.fetch_add(1, Ordering::Relaxed));
        if !profiles.iter().any(|profile| profile.id == id) {
            return id;
        }
    }
}

/// `base`, or `base 2`, `base 3`… — the first name not already taken.
fn unique_profile_name(base: &str, profiles: &[Profile]) -> String {
    (1..)
        .map(|n| {
            if n == 1 {
                base.to_string()
            } else {
                format!("{base} {n}")
            }
        })
        .find(|candidate| validate_profile_name(candidate, profiles, None).is_ok())
        .expect("an unused name always exists")
}

fn create_profile_in(config: &mut Config, new: NewProfile) -> Result<Profile, String> {
    let name = validate_profile_name(&new.name, config.profiles(), None)?;
    let settings = new.settings.normalized()?;
    let profile = Profile {
        id: generate_profile_id(config.profiles()),
        name,
        settings,
    };
    config.profiles_mut().push(profile.clone());
    Ok(profile)
}

fn update_profile_in(config: &mut Config, profile: Profile) -> Result<(), String> {
    let index = config
        .profiles()
        .iter()
        .position(|existing| existing.id == profile.id)
        .ok_or_else(|| format!("Unknown profile: {}", profile.id))?;
    let name = validate_profile_name(&profile.name, config.profiles(), Some(&profile.id))?;
    let settings = profile.settings.normalized()?;
    config.profiles_mut()[index] = Profile {
        id: profile.id,
        name,
        settings,
    };
    Ok(())
}

fn delete_profile_in(config: &mut Config, id: &str) -> Result<(), String> {
    let index = config
        .profiles()
        .iter()
        .position(|profile| profile.id == id)
        .ok_or_else(|| format!("Unknown profile: {id}"))?;
    if config.profiles().len() <= 1 {
        return Err("You can't delete the last profile".to_string());
    }
    if config.active_profile_id.as_deref() == Some(id) {
        return Err("Switch to another profile before deleting the active one".to_string());
    }
    config.profiles_mut().remove(index);
    Ok(())
}

fn set_active_profile_in(config: &mut Config, id: &str) -> Result<(), String> {
    if !config.profiles().iter().any(|profile| profile.id == id) {
        return Err(format!("Unknown profile: {id}"));
    }
    config.active_profile_id = Some(id.to_string());
    Ok(())
}

/// Onboarding / Home key field: store the key on the active Gladia profile,
/// else the first Gladia profile, else a new one; then make it active.
fn save_gladia_key_in(config: &mut Config, api_key: &str) -> Result<(), String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("API key is required".to_string());
    }
    let is_gladia = |profile: &Profile| matches!(profile.settings, ProfileSettings::Gladia { .. });
    let target = config
        .active_profile()
        .ok()
        .filter(is_gladia)
        .or_else(|| config.profiles().iter().find(|p| is_gladia(p)).cloned());
    let id = match target {
        Some(mut profile) => {
            if let ProfileSettings::Gladia { api_key: key, .. } = &mut profile.settings {
                *key = api_key.to_string();
            }
            let id = profile.id.clone();
            update_profile_in(config, profile)?;
            id
        }
        None => {
            let name = unique_profile_name(DEFAULT_GLADIA_PROFILE_NAME, config.profiles());
            create_profile_in(
                config,
                NewProfile {
                    name,
                    settings: ProfileSettings::Gladia {
                        api_key: api_key.to_string(),
                        region: GLADIA_REGION_AUTO.to_string(),
                        endpointing: DEFAULT_ENDPOINTING,
                        code_switching: false,
                    },
                },
            )?
            .id
        }
    };
    set_active_profile_in(config, &id)
}

/// Run a fallible edit; the file is only written when `f` succeeds.
fn try_with_config_at<T>(
    path: &Path,
    f: impl FnOnce(&mut Config) -> Result<T, String>,
) -> Result<T, String> {
    let mut config = load_config_from_path(path)?;
    let value = f(&mut config)?;
    write_config_to_path(path, &config)?;
    Ok(value)
}

fn edit_profiles_at(
    path: &Path,
    f: impl FnOnce(&mut Config) -> Result<(), String>,
) -> Result<ProfilesState, String> {
    try_with_config_at(path, |config| {
        f(config)?;
        Ok(config.profiles_state())
    })
}

fn edit_profiles(
    f: impl FnOnce(&mut Config) -> Result<(), String>,
) -> Result<ProfilesState, String> {
    let _guard = config_mutex().lock().unwrap();
    edit_profiles_at(&get_config_path(), f)
}

pub fn profiles_state() -> Result<ProfilesState, String> {
    read_config(Config::profiles_state)
}

pub fn active_profile() -> Result<Profile, String> {
    read_config(Config::active_profile)?
}

pub fn profile_by_id(id: &str) -> Result<Profile, String> {
    read_config(|config| {
        config
            .profiles()
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
            .ok_or_else(|| format!("Unknown profile: {id}"))
    })?
}

pub fn create_profile(new: NewProfile) -> Result<ProfilesState, String> {
    edit_profiles(|config| create_profile_in(config, new).map(|_| ()))
}

pub fn update_profile(profile: Profile) -> Result<ProfilesState, String> {
    edit_profiles(|config| update_profile_in(config, profile))
}

pub fn delete_profile(id: &str) -> Result<ProfilesState, String> {
    edit_profiles(|config| delete_profile_in(config, id))
}

pub fn set_active_profile(id: &str) -> Result<ProfilesState, String> {
    edit_profiles(|config| set_active_profile_in(config, id))
}

pub fn save_gladia_key(api_key: &str) -> Result<ProfilesState, String> {
    edit_profiles(|config| save_gladia_key_in(config, api_key))
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
    crate::hotkey::validate_hotkey(&hotkey)?;
    log::info!("[hotkey] saving shortcut config: {hotkey:?}");
    with_config(|config| {
        config.hotkey = Some(hotkey);
    })
}

#[tauri::command]
pub async fn get_hotkey() -> Result<Option<String>, String> {
    read_config(|config| config.hotkey.clone())
}

#[tauri::command]
pub async fn save_language_settings(languages: Vec<String>) -> Result<(), String> {
    with_config(|config| {
        config.languages = Some(languages);
    })
}

#[tauri::command]
pub async fn get_language_settings() -> Result<Option<Vec<String>>, String> {
    read_config(|config| config.languages.clone())
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
        assert_eq!(config.copy_to_clipboard, Some(false));
        assert_eq!(config.custom_vocabulary, Some(default_custom_vocabulary()));
        // Moved into the profile: the legacy fields stay unset.
        assert!(config.code_switching.is_none());
        assert!(config.endpointing.is_none());
        assert_eq!(config.profiles_state(), default_profiles_state());
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
    fn fresh_config_defaults_to_local_ollama_without_a_key_field() {
        let mut config = Config::default();
        config.fill_defaults();
        assert_eq!(config.profiles_state(), default_profiles_state());
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("stt_api_key"));
        assert!(!json.contains("\"provider\""));
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
        assert_eq!(
            config.profiles_state(),
            ProfilesState {
                profiles: vec![gladia_profile(
                    "gladia",
                    "Gladia",
                    "gladia-secret",
                    0.3,
                    false
                )],
                active_profile_id: "gladia".to_string(),
            }
        );
        assert_eq!(
            config.languages,
            Some(vec!["en".to_string(), "es".to_string()])
        );
    }

    #[test]
    fn explicit_null_provider_fields_are_filled_on_load() {
        let mut config: Config = serde_json::from_str(
            r#"{"provider":null,"stt_base_url":null,"stt_model":null,"stt_api_key":null}"#,
        )
        .unwrap();
        config.fill_defaults();
        assert_eq!(config.profiles_state(), default_profiles_state());
        assert!(config.provider.is_none());
        assert!(config.stt_api_key.is_none());
    }

    fn batch(base_url: &str, model: &str, api_key: &str) -> ProfileSettings {
        ProfileSettings::OpenAiCompat {
            base_url: base_url.to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }

    fn gladia(api_key: &str, region: &str, endpointing: f64) -> ProfileSettings {
        ProfileSettings::Gladia {
            api_key: api_key.to_string(),
            region: region.to_string(),
            endpointing,
            code_switching: false,
        }
    }

    fn gladia_profile(
        id: &str,
        name: &str,
        api_key: &str,
        endpointing: f64,
        code_switching: bool,
    ) -> Profile {
        Profile {
            id: id.to_string(),
            name: name.to_string(),
            settings: ProfileSettings::Gladia {
                api_key: api_key.to_string(),
                region: "auto".to_string(),
                endpointing,
                code_switching,
            },
        }
    }

    fn local_profile(base_url: &str, model: &str, api_key: &str) -> Profile {
        Profile {
            id: "local-ollama".to_string(),
            name: "Local Ollama".to_string(),
            settings: batch(base_url, model, api_key),
        }
    }

    fn default_profiles_state() -> ProfilesState {
        ProfilesState {
            profiles: vec![local_profile(DEFAULT_STT_BASE_URL, DEFAULT_STT_MODEL, "")],
            active_profile_id: "local-ollama".to_string(),
        }
    }

    fn new_profile(name: &str, settings: ProfileSettings) -> NewProfile {
        NewProfile {
            name: name.to_string(),
            settings,
        }
    }

    #[test]
    fn profile_settings_normalize_input() {
        assert_eq!(
            batch(
                " https://api.groq.com/openai/v1/ ",
                " whisper-large-v3 ",
                " gsk_x "
            )
            .normalized(),
            Ok(batch(
                "https://api.groq.com/openai/v1",
                "whisper-large-v3",
                "gsk_x"
            ))
        );
        assert_eq!(
            gladia(" key ", " eu-west ", 0.3).normalized(),
            Ok(gladia("key", "eu-west", 0.3))
        );
    }

    #[test]
    fn profile_settings_reject_bad_input() {
        for bad in [
            batch("localhost:11434/v1", "m", ""),
            batch("ftp://localhost/v1", "m", ""),
            batch("not a url", "m", ""),
            batch("", "m", ""),
            batch("http://", "m", ""),
            batch(DEFAULT_STT_BASE_URL, "  ", ""),
            batch(DEFAULT_STT_BASE_URL, "", ""),
            gladia("k", "mars-north", 0.1),
            gladia("k", "", 0.1),
            gladia("k", "auto", 0.0),
            gladia("k", "auto", 1.5),
            gladia("k", "auto", f64::NAN),
            gladia("k", "auto", f64::INFINITY),
        ] {
            assert!(bad.clone().normalized().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn batch_settings_omit_blank_key() {
        assert_eq!(
            batch("http://h/v1", "m", "")
                .batch_settings()
                .unwrap()
                .api_key,
            None
        );
        assert_eq!(
            batch("http://h/v1", "m", "k")
                .batch_settings()
                .unwrap()
                .api_key,
            Some("k".to_string())
        );
        assert!(gladia("k", "auto", 0.1).batch_settings().is_none());
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
        assert_eq!(
            loaded.active_profile(),
            Ok(gladia_profile(
                "gladia",
                "Gladia",
                "test-key",
                DEFAULT_ENDPOINTING,
                false
            ))
        );
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
        assert_eq!(
            loaded.active_profile().unwrap().settings,
            gladia("secret", "auto", DEFAULT_ENDPOINTING)
        );
        assert_eq!(loaded.hotkey.as_deref(), Some("CmdRight"));
    }

    #[test]
    fn deleting_a_profile_preserves_other_settings() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        let config = Config {
            api_key: Some("secret".to_string()),
            provider: Some("openai_compat".to_string()),
            hotkey: Some("CmdRight".to_string()),
            installed_version: Some("1.0.0".to_string()),
            ..Config::default()
        };
        write_config_to_path(&path, &config).unwrap();

        let state = edit_profiles_at(&path, |config| delete_profile_in(config, "gladia")).unwrap();

        assert_eq!(state, default_profiles_state());
        let loaded = load_config_from_path(&path).unwrap();
        assert_eq!(loaded.profiles_state(), default_profiles_state());
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
            load_config_from_path(&path)
                .unwrap()
                .active_profile()
                .unwrap()
                .settings,
            gladia("secret", "auto", DEFAULT_ENDPOINTING)
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
        assert_eq!(reset.profiles_state(), default_profiles_state());
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

    // --- Profile migration ---

    fn migrate(json: &str) -> Config {
        let mut config: Config = serde_json::from_str(json).unwrap();
        config.fill_defaults();
        config
    }

    #[test]
    fn migrates_legacy_gladia_only() {
        let config = migrate(
            r#"{"provider":"gladia","api_key":" g-key ","endpointing":0.4,"code_switching":true}"#,
        );
        assert_eq!(
            config.profiles_state(),
            ProfilesState {
                profiles: vec![gladia_profile("gladia", "Gladia", "g-key", 0.4, true)],
                active_profile_id: "gladia".to_string(),
            }
        );
    }

    #[test]
    fn migrates_legacy_gladia_provider_without_key() {
        let config = migrate(r#"{"provider":"gladia"}"#);
        assert_eq!(
            config.profiles_state(),
            ProfilesState {
                profiles: vec![gladia_profile(
                    "gladia",
                    "Gladia",
                    "",
                    DEFAULT_ENDPOINTING,
                    false
                )],
                active_profile_id: "gladia".to_string(),
            }
        );
    }

    #[test]
    fn migrates_legacy_openai_compat_only() {
        let config = migrate(
            r#"{"provider":"openai_compat","stt_base_url":"http://10.0.0.5:11434/v1/","stt_model":"whisper","stt_api_key":"k"}"#,
        );
        assert_eq!(
            config.profiles_state(),
            ProfilesState {
                profiles: vec![local_profile("http://10.0.0.5:11434/v1", "whisper", "k")],
                active_profile_id: "local-ollama".to_string(),
            }
        );
    }

    #[test]
    fn migrates_legacy_both_and_keeps_the_selected_provider_active() {
        let json = r#"{"provider":"openai_compat","api_key":"g-key","stt_base_url":"http://localhost:11434/v1","stt_model":"gemma4:e4b"}"#;
        let config = migrate(json);
        assert_eq!(
            config.profiles_state(),
            ProfilesState {
                profiles: vec![
                    gladia_profile("gladia", "Gladia", "g-key", DEFAULT_ENDPOINTING, false),
                    local_profile(DEFAULT_STT_BASE_URL, DEFAULT_STT_MODEL, ""),
                ],
                active_profile_id: "local-ollama".to_string(),
            }
        );

        // Gladia selected with a customised batch endpoint: both, Gladia active.
        let config =
            migrate(r#"{"provider":"gladia","api_key":"g-key","stt_model":"whisper-large-v3"}"#);
        let state = config.profiles_state();
        assert_eq!(state.active_profile_id, "gladia");
        assert_eq!(
            state.profiles[1],
            local_profile(DEFAULT_STT_BASE_URL, "whisper-large-v3", "")
        );
    }

    #[test]
    fn legacy_gladia_default_with_untouched_batch_fields_creates_only_gladia() {
        let config = migrate(
            r#"{"provider":"gladia","api_key":"g","stt_base_url":"http://localhost:11434/v1","stt_model":"gemma4:e4b"}"#,
        );
        assert_eq!(config.profiles().len(), 1);
        assert_eq!(config.profiles()[0].id, "gladia");
    }

    #[test]
    fn migrates_fresh_install_to_local_ollama() {
        let dir = TestConfigDir::new();
        let config = load_config_from_path(&dir.config_path()).unwrap();
        assert_eq!(config.profiles_state(), default_profiles_state());
        assert_eq!(migrate("{}").profiles_state(), default_profiles_state());
    }

    #[test]
    fn already_migrated_config_is_left_alone() {
        let json = r#"{
            "profiles": [
                {"id":"a","name":"Work Gladia","kind":"gladia","api_key":"k","region":"us-west","endpointing":0.2,"code_switching":true},
                {"id":"b","name":"Groq","kind":"openai_compat","base_url":"https://api.groq.com/openai/v1","model":"whisper-large-v3","api_key":"gsk"}
            ],
            "active_profile_id": "b",
            "provider": "gladia",
            "api_key": "stale-legacy"
        }"#;
        let config = migrate(json);
        let state = config.profiles_state();
        assert_eq!(state.active_profile_id, "b");
        assert_eq!(state.profiles.len(), 2);
        assert_eq!(state.profiles[0].name, "Work Gladia");
        // Stale legacy fields are ignored and dropped.
        assert!(config.api_key.is_none() && config.provider.is_none());
    }

    #[test]
    fn migration_is_idempotent_and_persisted_without_legacy_fields() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        fs::write(
            &path,
            r#"{"provider":"openai_compat","api_key":"g-key","stt_model":"whisper","endpointing":0.3,"code_switching":true,"hotkey":"Fn"}"#,
        )
        .unwrap();

        let first = load_config_from_path(&path).unwrap().profiles_state();
        let second = load_config_from_path(&path).unwrap().profiles_state();
        assert_eq!(first, second, "reads before any save agree");

        with_config_at(&path, |_| {}).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        for legacy in [
            "provider",
            "stt_base_url",
            "stt_model",
            "stt_api_key",
            "api_key",
            "endpointing",
            "code_switching",
        ] {
            assert!(value.get(legacy).is_none(), "{legacy} still written");
        }
        assert_eq!(
            load_config_from_path(&path).unwrap().profiles_state(),
            first
        );

        // Migrating an already-migrated config again changes nothing.
        let mut again = load_config_from_path(&path).unwrap();
        again.migrate_profiles();
        again.migrate_profiles();
        assert_eq!(again.profiles_state(), first);
        with_config_at(&path, |_| {}).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn unknown_active_id_on_disk_is_repaired_to_the_first_profile() {
        let config = migrate(
            r#"{"profiles":[{"id":"x","name":"X","kind":"openai_compat","base_url":"http://h/v1","model":"m"}],"active_profile_id":"gone"}"#,
        );
        assert_eq!(config.profiles_state().active_profile_id, "x");
        assert_eq!(config.profiles()[0].settings, batch("http://h/v1", "m", ""));
    }

    // --- Profile JSON shape (IPC + disk) ---

    #[test]
    fn profile_serializes_with_a_kind_tag() {
        let value = serde_json::to_value(gladia_profile("g", "G", "k", 0.1, false)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"id":"g","name":"G","kind":"gladia","api_key":"k","region":"auto","endpointing":0.1,"code_switching":false})
        );
        let value = serde_json::to_value(local_profile("http://h/v1", "m", "")).unwrap();
        assert_eq!(value["kind"], "openai_compat");
        assert_eq!(value["base_url"], "http://h/v1");
    }

    #[test]
    fn profile_json_rejects_nulls_wrong_types_and_unknown_kinds() {
        let valid_gladia = serde_json::json!({"id":"g","name":"G","kind":"gladia","api_key":"k","region":"auto","endpointing":0.1,"code_switching":false});
        let valid_batch = serde_json::json!({"id":"b","name":"B","kind":"openai_compat","base_url":"http://h/v1","model":"m","api_key":""});
        assert!(serde_json::from_value::<Profile>(valid_gladia.clone()).is_ok());
        assert!(serde_json::from_value::<Profile>(valid_batch.clone()).is_ok());

        let cases: Vec<(serde_json::Value, &str, serde_json::Value)> = vec![
            (valid_gladia.clone(), "id", serde_json::Value::Null),
            (valid_gladia.clone(), "name", serde_json::Value::Null),
            (valid_gladia.clone(), "kind", serde_json::Value::Null),
            (valid_gladia.clone(), "api_key", serde_json::Value::Null),
            (valid_gladia.clone(), "region", serde_json::Value::Null),
            (valid_gladia.clone(), "endpointing", serde_json::Value::Null),
            (
                valid_gladia.clone(),
                "code_switching",
                serde_json::Value::Null,
            ),
            (valid_batch.clone(), "base_url", serde_json::Value::Null),
            (valid_batch.clone(), "model", serde_json::Value::Null),
            (valid_batch.clone(), "api_key", serde_json::Value::Null),
            (valid_gladia.clone(), "name", serde_json::json!(42)),
            (valid_gladia.clone(), "kind", serde_json::json!("whisper")),
            (
                valid_gladia.clone(),
                "endpointing",
                serde_json::json!("0.1"),
            ),
            (
                valid_gladia.clone(),
                "code_switching",
                serde_json::json!("yes"),
            ),
            (valid_gladia.clone(), "api_key", serde_json::json!(["k"])),
            (valid_batch.clone(), "model", serde_json::json!(false)),
            (valid_batch.clone(), "base_url", serde_json::json!({"u":1})),
        ];
        for (mut value, field, bad) in cases {
            value[field] = bad.clone();
            assert!(
                serde_json::from_value::<Profile>(value).is_err(),
                "{field} = {bad} accepted"
            );
        }
        // Missing required fields are rejected too (no silent defaults over IPC).
        for field in ["region", "endpointing", "code_switching", "api_key", "kind"] {
            let mut value = valid_gladia.clone();
            value.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Profile>(value).is_err(), "{field}");
        }
        let mut value = valid_batch.clone();
        value.as_object_mut().unwrap().remove("model");
        assert!(serde_json::from_value::<Profile>(value).is_err());
        // The optional batch key may be omitted.
        let mut value = valid_batch;
        value.as_object_mut().unwrap().remove("api_key");
        assert!(serde_json::from_value::<Profile>(value).is_ok());
        // New profiles never carry a client-chosen id.
        let new: NewProfile =
            serde_json::from_value(serde_json::json!({"name":"N","kind":"openai_compat","base_url":"http://h/v1","model":"m"}))
                .unwrap();
        assert_eq!(new.name, "N");
    }

    // --- Profile CRUD ---

    fn two_profiles() -> Config {
        migrate(r#"{"provider":"gladia","api_key":"g-key","stt_model":"whisper"}"#)
    }

    #[test]
    fn create_profile_validates_and_assigns_a_unique_id() {
        let mut config = two_profiles();
        let created = create_profile_in(
            &mut config,
            new_profile(
                "  Groq ",
                batch("https://api.groq.com/openai/v1/", "whisper-large-v3", "gsk"),
            ),
        )
        .unwrap();
        assert_eq!(created.name, "Groq");
        assert!(created.id.starts_with("p-"));
        assert_eq!(
            created.settings,
            batch("https://api.groq.com/openai/v1", "whisper-large-v3", "gsk")
        );
        let other = create_profile_in(
            &mut config,
            new_profile("Groq 2", batch("http://h/v1", "m", "")),
        )
        .unwrap();
        assert_ne!(created.id, other.id);
        assert_eq!(config.profiles().len(), 4);
        // Creating never changes the active profile.
        assert_eq!(config.active_profile_id.as_deref(), Some("gladia"));
    }

    #[test]
    fn create_profile_rejects_invalid_input_without_storing_it() {
        let mut config = two_profiles();
        for (name, settings) in [
            ("", batch("http://h/v1", "m", "")),
            ("   ", batch("http://h/v1", "m", "")),
            ("gladia", batch("http://h/v1", "m", "")),
            ("  LOCAL ollama ", batch("http://h/v1", "m", "")),
            ("New", batch("ftp://h/v1", "m", "")),
            ("New", batch("http://h/v1", " ", "")),
            ("New", gladia("k", "moon", 0.1)),
            ("New", gladia("k", "auto", 2.0)),
        ] {
            assert!(
                create_profile_in(&mut config, new_profile(name, settings.clone())).is_err(),
                "{name:?} {settings:?}"
            );
        }
        let too_long = "x".repeat(61);
        assert!(create_profile_in(
            &mut config,
            new_profile(&too_long, batch("http://h/v1", "m", ""))
        )
        .is_err());
        assert_eq!(config.profiles().len(), 2);
    }

    #[test]
    fn update_profile_validates_including_duplicate_names() {
        let mut config = two_profiles();
        let mut gladia_p = config.profiles()[0].clone();
        // Renaming to its own name with different case is fine.
        gladia_p.name = " GLADIA ".to_string();
        update_profile_in(&mut config, gladia_p.clone()).unwrap();
        assert_eq!(config.profiles()[0].name, "GLADIA");

        gladia_p.name = "local OLLAMA".to_string();
        assert!(update_profile_in(&mut config, gladia_p.clone()).is_err());
        gladia_p.name = String::new();
        assert!(update_profile_in(&mut config, gladia_p.clone()).is_err());

        // Partial edits still go through validation.
        let mut local = config.profiles()[1].clone();
        local.settings = batch("not a url", "m", "");
        assert!(update_profile_in(&mut config, local.clone()).is_err());
        local.settings = batch("http://h/v1", "", "");
        assert!(update_profile_in(&mut config, local.clone()).is_err());
        assert_eq!(
            config.profiles()[1].settings,
            batch(DEFAULT_STT_BASE_URL, "whisper", "")
        );

        // Kind can change on update.
        local.settings = gladia("k2", "eu-west", 0.5);
        update_profile_in(&mut config, local).unwrap();
        assert_eq!(config.profiles()[1].settings, gladia("k2", "eu-west", 0.5));

        let mut unknown = config.profiles()[0].clone();
        unknown.id = "nope".to_string();
        assert!(update_profile_in(&mut config, unknown).is_err());
    }

    #[test]
    fn delete_rejects_active_last_and_unknown() {
        let mut config = two_profiles();
        assert_eq!(
            delete_profile_in(&mut config, "gladia"),
            Err("Switch to another profile before deleting the active one".to_string())
        );
        assert!(delete_profile_in(&mut config, "nope").is_err());
        delete_profile_in(&mut config, "local-ollama").unwrap();
        assert_eq!(config.profiles().len(), 1);
        assert_eq!(
            delete_profile_in(&mut config, "gladia"),
            Err("You can't delete the last profile".to_string())
        );
        assert_eq!(config.profiles().len(), 1);
    }

    #[test]
    fn set_active_rejects_unknown_ids() {
        let mut config = two_profiles();
        assert!(set_active_profile_in(&mut config, "nope").is_err());
        assert!(set_active_profile_in(&mut config, "").is_err());
        assert_eq!(config.active_profile_id.as_deref(), Some("gladia"));
        set_active_profile_in(&mut config, "local-ollama").unwrap();
        assert_eq!(config.active_profile().unwrap().id, "local-ollama");
    }

    #[test]
    fn save_gladia_key_updates_or_creates_a_gladia_profile_and_activates_it() {
        // Fresh install: creates "Gladia" and activates it.
        let mut config = migrate("{}");
        save_gladia_key_in(&mut config, "  new-key ").unwrap();
        let active = config.active_profile().unwrap();
        assert_eq!(active.name, "Gladia");
        assert_eq!(
            active.settings,
            gladia("new-key", "auto", DEFAULT_ENDPOINTING)
        );
        assert_eq!(config.profiles().len(), 2);

        // Existing Gladia profile (inactive): updated in place, then activated.
        let mut config = two_profiles();
        set_active_profile_in(&mut config, "local-ollama").unwrap();
        save_gladia_key_in(&mut config, "rotated").unwrap();
        assert_eq!(config.profiles().len(), 2);
        assert_eq!(config.active_profile().unwrap().id, "gladia");
        assert_eq!(
            config.active_profile().unwrap().settings,
            gladia("rotated", "auto", DEFAULT_ENDPOINTING)
        );

        // A name clash with a non-Gladia profile gets a numbered name.
        let mut config = migrate("{}");
        let mut local = config.profiles()[0].clone();
        local.name = "Gladia".to_string();
        update_profile_in(&mut config, local).unwrap();
        save_gladia_key_in(&mut config, "k").unwrap();
        assert_eq!(config.active_profile().unwrap().name, "Gladia 2");

        assert!(save_gladia_key_in(&mut config, "   ").is_err());
    }

    #[test]
    fn failed_profile_writes_leave_the_file_untouched() {
        let dir = TestConfigDir::new();
        let path = dir.config_path();
        write_config_to_path(&path, &two_profiles()).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        assert!(edit_profiles_at(&path, |c| delete_profile_in(c, "gladia")).is_err());
        assert!(edit_profiles_at(&path, |c| set_active_profile_in(c, "nope")).is_err());
        assert!(edit_profiles_at(&path, |c| create_profile_in(
            c,
            new_profile("Gladia", batch("http://h/v1", "m", ""))
        )
        .map(|_| ()))
        .is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);

        let state = edit_profiles_at(&path, |c| set_active_profile_in(c, "local-ollama")).unwrap();
        assert_eq!(state.active_profile_id, "local-ollama");
        assert_eq!(
            load_config_from_path(&path).unwrap().profiles_state(),
            state
        );
    }
}
