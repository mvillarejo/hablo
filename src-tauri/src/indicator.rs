//! Floating "listening" indicator: a small always-on-top pill that shows the
//! live mic level while dictating and a spinner while transcribing.
//!
//! The window is declared in `tauri.conf.json` (label `indicator`, hidden,
//! `focus: false`, `focusable: false`) like Parley's recorder widget, and is
//! shown with `orderFrontRegardless` so it never becomes key and never
//! activates TTM: the paste has to land in the user's frontmost app.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

pub const LABEL: &str = "indicator";
/// Window size in logical pixels (the pill plus room for its shadow).
const WIDTH: f64 = 120.0;
const HEIGHT: f64 = 48.0;
/// Gap between the window and the bottom of the work area (above the Dock).
const BOTTOM_MARGIN: f64 = 16.0;
/// How long the error tint stays before the pill hides.
const ERROR_LINGER: Duration = Duration::from_millis(1_200);
/// Mic-level events are emitted at most this often (25 Hz).
const LEVEL_INTERVAL: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Hidden,
    Listening,
    Transcribing,
    Error,
}

/// Map the tray activity the frontend reports to an indicator phase.
pub fn phase_for_activity(activity: &str) -> Phase {
    match activity {
        "starting" | "recording" => Phase::Listening,
        "finalizing" => Phase::Transcribing,
        "error" => Phase::Error,
        _ => Phase::Hidden,
    }
}

/// Next phase given the current one. An error only shows when it interrupts a
/// visible dictation, so an unrelated error (a failed key test) never pops up.
pub fn next_phase(current: Phase, requested: Phase) -> Phase {
    match (current, requested) {
        (Phase::Hidden | Phase::Error, Phase::Error) => Phase::Hidden,
        (_, requested) => requested,
    }
}

/// Top-left (physical px) that centres the window horizontally in `work_area`
/// and sits `BOTTOM_MARGIN` above its bottom edge.
pub fn bottom_centre(
    work_position: (i32, i32),
    work_size: (u32, u32),
    scale_factor: f64,
) -> (i32, i32) {
    let width = (WIDTH * scale_factor).round() as i32;
    let height = ((HEIGHT + BOTTOM_MARGIN) * scale_factor).round() as i32;
    (
        work_position.0 + (work_size.0 as i32 - width) / 2,
        work_position.1 + work_size.1 as i32 - height,
    )
}

/// Meter level in 0..=1 from a chunk: RMS mapped from -60 dBFS..0 dBFS.
/// (Ported from Parley's `recorder::level`.)
pub fn level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| {
            let v = s as f64 / i16::MAX as f64;
            v * v
        })
        .sum();
    let rms = (sum / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    (((db + 60.0) / 60.0).clamp(0.0, 1.0)) as f32
}

/// Smooths per-callback levels (fast attack, slow release) and throttles
/// emission to `LEVEL_INTERVAL`.
#[derive(Debug, Default)]
pub struct LevelMeter {
    value: f32,
    last_emit: Option<Instant>,
}

impl LevelMeter {
    /// Feed one capture chunk (16-bit LE mono PCM); returns a level to emit
    /// when the throttle interval has passed.
    pub fn observe(&mut self, pcm: &[u8], now: Instant) -> Option<f32> {
        let samples: Vec<i16> = pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        let target = level(&samples);
        let rate = if target > self.value { 0.6 } else { 0.15 };
        self.value += (target - self.value) * rate;
        let due = self
            .last_emit
            .is_none_or(|last| now.saturating_duration_since(last) >= LEVEL_INTERVAL);
        due.then(|| {
            self.last_emit = Some(now);
            self.value
        })
    }
}

static CURRENT: Mutex<Phase> = Mutex::new(Phase::Hidden);
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn current_phase() -> Phase {
    *CURRENT.lock().unwrap()
}

/// One-time window setup: clicks pass through, and on macOS it also joins
/// full-screen Spaces and stays out of the window cycle.
pub fn setup(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        log::warn!("[indicator] window not found");
        return;
    };
    let _ = window.set_ignore_cursor_events(true);
    #[cfg(target_os = "macos")]
    if let Ok(ns_window) = window.ns_window() {
        let ns_window = ns_window as usize;
        let _ = window.run_on_main_thread(move || unsafe {
            let ns_window = ns_window as *mut objc2::runtime::AnyObject;
            // CanJoinAllSpaces | Stationary | IgnoresCycle | FullScreenAuxiliary
            let behavior: usize = 1 | (1 << 4) | (1 << 6) | (1 << 8);
            let _: () = objc2::msg_send![ns_window, setCollectionBehavior: behavior];
        });
    }
}

/// Move to the screen under the cursor, bottom centre.
fn position(app: &AppHandle, window: &WebviewWindow) {
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let area = monitor.work_area();
    let (x, y) = bottom_centre(
        (area.position.x, area.position.y),
        (area.size.width, area.size.height),
        monitor.scale_factor(),
    );
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Show the indicator without taking focus from the frontmost app (Parley's
/// `show_widget` pattern): `orderFrontRegardless` orders the window in without
/// making it key or activating TTM.
fn show(window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(ns_window) = window.ns_window() {
            let ns_window = ns_window as usize;
            let _ = window.run_on_main_thread(move || unsafe {
                let ns_window = ns_window as *mut objc2::runtime::AnyObject;
                let _: () = objc2::msg_send![ns_window, orderFrontRegardless];
            });
            return;
        }
    }
    let _ = window.show();
}

/// Apply a requested phase: position + show, update, or hide. An error tint
/// hides itself after `ERROR_LINGER` unless another phase arrives first.
pub fn apply(app: &AppHandle, requested: Phase, enabled: bool) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let phase = {
        let mut current = CURRENT.lock().unwrap();
        let phase = if enabled {
            next_phase(*current, requested)
        } else {
            Phase::Hidden
        };
        if phase == *current {
            return;
        }
        if *current == Phase::Hidden {
            position(app, &window);
        }
        *current = phase;
        phase
    };
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = app.emit_to(LABEL, "indicator-state", phase);
    if phase == Phase::Hidden {
        let _ = window.hide();
        return;
    }
    show(&window);
    if phase == Phase::Error {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(ERROR_LINGER).await;
            if GENERATION.load(Ordering::SeqCst) == generation {
                apply(&app, Phase::Hidden, true);
            }
        });
    }
}

/// The indicator page asks for the phase on load, in case it missed an event.
#[tauri::command]
pub fn get_indicator_phase() -> Phase {
    current_phase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_maps_rms_to_a_meter() {
        assert_eq!(level(&[]), 0.0);
        assert_eq!(level(&[0; 100]), 0.0);
        assert!((level(&[i16::MAX; 100]) - 1.0).abs() < 1e-3);
        let quiet = level(&[33; 100]); // ~ -60 dBFS
        assert!(quiet < 0.02, "{quiet}");
        let mid = level(&[3277; 100]); // ~ -20 dBFS
        assert!((mid - 0.667).abs() < 0.01, "{mid}");
    }

    fn pcm(value: i16, samples: usize) -> Vec<u8> {
        (0..samples).flat_map(|_| value.to_le_bytes()).collect()
    }

    #[test]
    fn meter_attacks_fast_releases_slowly_and_throttles_to_25hz() {
        let mut meter = LevelMeter::default();
        let start = Instant::now();
        let loud = pcm(16_000, 160);
        let first = meter.observe(&loud, start).expect("first level is emitted");
        assert!(first > 0.5, "fast attack: {first}");
        assert_eq!(
            meter.observe(&loud, start + Duration::from_millis(10)),
            None
        );
        assert_eq!(
            meter.observe(&loud, start + Duration::from_millis(30)),
            None
        );
        let peak = meter
            .observe(&loud, start + Duration::from_millis(40))
            .unwrap();
        let after_silence = meter
            .observe(&pcm(0, 160), start + Duration::from_millis(80))
            .unwrap();
        assert!(
            after_silence > peak * 0.7 && after_silence < peak,
            "slow release: {peak} -> {after_silence}"
        );
        let mut silent = LevelMeter::default();
        assert_eq!(silent.observe(&pcm(0, 160), start), Some(0.0));
    }

    #[test]
    fn activities_map_to_phases() {
        for (activity, phase) in [
            ("starting", Phase::Listening),
            ("recording", Phase::Listening),
            ("finalizing", Phase::Transcribing),
            ("error", Phase::Error),
            ("idle", Phase::Hidden),
            ("bogus", Phase::Hidden),
        ] {
            assert_eq!(phase_for_activity(activity), phase, "{activity}");
        }
    }

    #[test]
    fn errors_only_show_when_they_interrupt_a_dictation() {
        use Phase::*;
        assert_eq!(next_phase(Listening, Error), Error);
        assert_eq!(next_phase(Transcribing, Error), Error);
        assert_eq!(next_phase(Hidden, Error), Hidden);
        assert_eq!(next_phase(Error, Error), Hidden);
        assert_eq!(next_phase(Error, Listening), Listening);
        assert_eq!(next_phase(Listening, Transcribing), Transcribing);
        assert_eq!(next_phase(Transcribing, Hidden), Hidden);
    }

    #[test]
    fn sits_bottom_centre_above_the_dock() {
        // 1512x949 logical work area at 2x, starting below the menu bar.
        assert_eq!(
            bottom_centre((0, 50), (3024, 1898), 2.0),
            (1512 - 120, 50 + 1898 - 128)
        );
        // A second screen to the left at 1x.
        assert_eq!(
            bottom_centre((-1920, 0), (1920, 1040), 1.0),
            (-1920 + 900, 1040 - 64)
        );
    }

    #[test]
    fn phase_serializes_for_the_frontend() {
        assert_eq!(
            serde_json::to_string(&Phase::Transcribing).unwrap(),
            "\"transcribing\""
        );
    }
}
