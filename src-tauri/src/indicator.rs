//! Floating "listening" indicator: a small always-on-top pill that shows the
//! live mic level while dictating and a spinner while transcribing. In the
//! "always visible" mode it stays up between dictations with a dimmed mic.
//!
//! The window is declared in `tauri.conf.json` (label `indicator`, hidden,
//! `focus: false`, `focusable: false`) like Parley's recorder widget. On macOS
//! it is turned into a non-activating panel and shown with
//! `orderFrontRegardless`, so neither showing nor dragging it ever activates
//! TTM: the paste has to land in the user's frontmost app.

use crate::config::{self, IndicatorMode, IndicatorPosition};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, LogicalPosition, Manager, PhysicalPosition, WebviewWindow};

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
/// A dragged position is saved once the window stops moving for this long.
const SAVE_DELAY: Duration = Duration::from_millis(300);
/// Moves reported this soon after we placed the window ourselves are ours.
const OWN_MOVE_GRACE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Hidden,
    /// Shown between dictations in the "always visible" mode.
    Idle,
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

/// What the pill shows when no dictation is running.
pub fn resting_phase(mode: IndicatorMode) -> Phase {
    match mode {
        IndicatorMode::Always => Phase::Idle,
        IndicatorMode::Off | IndicatorMode::WhileDictating => Phase::Hidden,
    }
}

/// Next phase given the mode and the current one. An error only shows when it
/// interrupts a visible dictation, so an unrelated error (a failed key test)
/// never pops up.
pub fn next_phase(mode: IndicatorMode, current: Phase, requested: Phase) -> Phase {
    let rest = resting_phase(mode);
    match (mode, current, requested) {
        (IndicatorMode::Off, _, _) => Phase::Hidden,
        (_, Phase::Hidden | Phase::Idle | Phase::Error, Phase::Error) => rest,
        (_, _, Phase::Hidden | Phase::Idle) => rest,
        (_, _, requested) => requested,
    }
}

/// A screen's bounds in logical px: (x, y, width, height).
pub type ScreenRect = (f64, f64, f64, f64);

/// The saved top-left, if the pill's centre still lands on a connected
/// screen; `None` falls back to the default placement.
pub fn restore_position(
    saved: Option<IndicatorPosition>,
    screens: &[ScreenRect],
) -> Option<IndicatorPosition> {
    let saved = saved?;
    let (cx, cy) = (saved.x + WIDTH / 2.0, saved.y + HEIGHT / 2.0);
    screens
        .iter()
        .any(|&(x, y, w, h)| cx >= x && cx < x + w && cy >= y && cy < y + h)
        .then_some(saved)
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

struct State {
    /// What the window shows.
    shown: Phase,
    /// What the dictation last asked for, kept so a mode change applies at once.
    requested: Phase,
    mode: IndicatorMode,
    /// When we last placed the window ourselves (see `OWN_MOVE_GRACE`).
    placed_at: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    shown: Phase::Hidden,
    requested: Phase::Hidden,
    mode: IndicatorMode::WhileDictating,
    placed_at: None,
});
static GENERATION: AtomicU64 = AtomicU64::new(0);
static MOVE_GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn current_phase() -> Phase {
    STATE.lock().unwrap().shown
}

/// One-time window setup: on macOS the window becomes a non-activating panel
/// (so a click or drag never activates TTM), joins full-screen Spaces and
/// stays out of the window cycle. Drags are saved as the pill's position.
pub fn setup(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        log::warn!("[indicator] window not found");
        return;
    };
    #[cfg(target_os = "macos")]
    {
        panel::convert(&window);
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
    let moved = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::Moved(position) = event {
            on_moved(&moved, *position);
        }
    });
}

/// Whether the user is holding the primary mouse button, i.e. this move is a
/// drag and not us (or macOS, after a display change) moving the window.
fn user_is_dragging() -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        let buttons: usize = objc2::msg_send![objc2::class!(NSEvent), pressedMouseButtons];
        buttons & 1 == 1
    }
    #[cfg(not(target_os = "macos"))]
    true
}

/// Save a drag once the window settles.
fn on_moved(window: &WebviewWindow, position: PhysicalPosition<i32>) {
    let ours = STATE
        .lock()
        .unwrap()
        .placed_at
        .is_some_and(|at| at.elapsed() < OWN_MOVE_GRACE);
    if ours || !user_is_dragging() {
        return;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let logical = position.to_logical::<f64>(scale);
    let saved = IndicatorPosition {
        x: logical.x,
        y: logical.y,
    };
    let generation = MOVE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SAVE_DELAY).await;
        if MOVE_GENERATION.load(Ordering::SeqCst) != generation {
            return;
        }
        match config::save_indicator_position(Some(saved)) {
            Ok(()) => log::info!("[indicator] moved to ({:.0}, {:.0})", saved.x, saved.y),
            Err(error) => log::warn!("[indicator] failed to save position: {error}"),
        }
    });
}

/// Screens in logical px, each converted with its own scale factor.
fn screens(app: &AppHandle) -> Vec<ScreenRect> {
    app.available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|monitor| {
            let scale = monitor.scale_factor();
            let position = monitor.position().to_logical::<f64>(scale);
            let size = monitor.size().to_logical::<f64>(scale);
            (position.x, position.y, size.width, size.height)
        })
        .collect()
}

/// Where the pill goes (top-left, logical px): the saved spot, or bottom
/// centre of the screen under the cursor when there is none or it is no
/// longer on a connected screen.
fn target(app: &AppHandle) -> Option<LogicalPosition<f64>> {
    STATE.lock().unwrap().placed_at = Some(Instant::now());
    let saved = config::indicator_position().ok().flatten();
    if let Some(spot) = restore_position(saved, &screens(app)) {
        return Some(LogicalPosition::new(spot.x, spot.y));
    }
    let monitor = app
        .cursor_position()
        .ok()
        .and_then(|p| app.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten())?;
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let (x, y) = bottom_centre(
        (area.position.x, area.position.y),
        (area.size.width, area.size.height),
        scale,
    );
    Some(PhysicalPosition::new(x, y).to_logical(scale))
}

/// Move (when `at` is set) and show the indicator without taking focus from
/// the frontmost app (Parley's `show_widget` pattern): `orderFrontRegardless`
/// orders the window in without making it key or activating TTM. On macOS the
/// move happens in the same main-thread call, so the pill never flashes at its
/// old spot first (tauri's `set_position` lands asynchronously).
fn show(window: &WebviewWindow, at: Option<LogicalPosition<f64>>) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(ns_window) = window.ns_window() {
            let ns_window = ns_window as usize;
            let _ = window.run_on_main_thread(move || unsafe {
                let ns_window = ns_window as *mut objc2::runtime::AnyObject;
                if let Some(at) = at {
                    // Cocoa's origin is the bottom-left of the main display (as in tao).
                    let main_height = core_graphics::display::CGDisplay::main().pixels_high();
                    let point = CocoaPoint {
                        x: at.x,
                        y: main_height as f64 - at.y,
                    };
                    let _: () = objc2::msg_send![ns_window, setFrameTopLeftPoint: point];
                }
                let _: () = objc2::msg_send![ns_window, orderFrontRegardless];
            });
            return;
        }
    }
    if let Some(at) = at {
        let _ = window.set_position(at);
    }
    let _ = window.show();
}

/// `NSPoint` for `setFrameTopLeftPoint:`.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CocoaPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
unsafe impl objc2::Encode for CocoaPoint {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

/// Apply a requested phase under `mode`: position + show, update, or hide.
/// An error tint goes back to rest after `ERROR_LINGER` unless another phase
/// arrives first.
pub fn apply(app: &AppHandle, requested: Phase, mode: IndicatorMode) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    let (phase, was_hidden) = {
        let mut state = STATE.lock().unwrap();
        state.requested = requested;
        state.mode = mode;
        let phase = next_phase(mode, state.shown, requested);
        if phase == state.shown {
            return;
        }
        let was_hidden = state.shown == Phase::Hidden;
        state.shown = phase;
        (phase, was_hidden)
    };
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = app.emit_to(LABEL, "indicator-state", phase);
    if phase == Phase::Hidden {
        let _ = window.hide();
        return;
    }
    show(&window, if was_hidden { target(app) } else { None });
    if phase == Phase::Error {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(ERROR_LINGER).await;
            if GENERATION.load(Ordering::SeqCst) == generation {
                let mode = STATE.lock().unwrap().mode;
                apply(&app, Phase::Hidden, mode);
            }
        });
    }
}

/// Switch mode now: show, hide or keep the pill for the dictation in progress.
pub fn set_mode(app: &AppHandle, mode: IndicatorMode) {
    let requested = STATE.lock().unwrap().requested;
    apply(app, requested, mode);
}

/// Forget the dragged spot and, if the pill is up, move it back now.
pub fn reset_position(app: &AppHandle) -> Result<(), String> {
    config::save_indicator_position(None)?;
    if current_phase() != Phase::Hidden {
        if let Some(window) = app.get_webview_window(LABEL) {
            show(&window, target(app));
        }
    }
    Ok(())
}

/// The indicator page asks for the phase on load, in case it missed an event.
#[tauri::command]
pub fn get_indicator_phase() -> Phase {
    current_phase()
}

/// The indicator as a non-activating NSPanel (Parley's `widget_window::panel`,
/// minus the keyboard mode: the pill never takes keys). A regular NSWindow
/// activates TTM when clicked; a non-activating panel can be dragged while
/// the user's app stays frontmost.
#[cfg(target_os = "macos")]
mod panel {
    use objc2_06::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2_06::{msg_send, sel};
    use std::sync::OnceLock;
    use tauri::WebviewWindow;

    /// `NSWindowStyleMaskNonactivatingPanel`.
    const NONACTIVATING_PANEL: usize = 1 << 7;
    /// tao's window class keeps this one ivar; the panel class declares it
    /// too so the object layout (and tao's `set_focusable`) stay valid.
    const TAO_IVAR: &std::ffi::CStr = c"focusable";

    extern "C-unwind" fn refuse(_this: &AnyObject, _sel: Sel) -> Bool {
        Bool::NO
    }

    /// `TTMIndicatorPanel`: an NSPanel that never becomes key or main.
    pub(super) fn panel_class() -> Option<&'static AnyClass> {
        static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
        *CLASS.get_or_init(|| {
            if let Some(existing) = AnyClass::get(c"TTMIndicatorPanel") {
                return Some(existing);
            }
            let mut builder = ClassBuilder::new(c"TTMIndicatorPanel", AnyClass::get(c"NSPanel")?)?;
            builder.add_ivar::<Bool>(TAO_IVAR);
            // SAFETY: the signatures match the selectors (`-(BOOL)…`).
            unsafe {
                builder.add_method(
                    sel!(canBecomeKeyWindow),
                    refuse as extern "C-unwind" fn(_, _) -> _,
                );
                builder.add_method(
                    sel!(canBecomeMainWindow),
                    refuse as extern "C-unwind" fn(_, _) -> _,
                );
            }
            Some(builder.register())
        })
    }

    /// The panel class can stand in for `class` only if the object layout is
    /// identical; otherwise the window stays a regular one.
    pub(super) fn same_layout(class: &AnyClass, panel: &AnyClass) -> bool {
        let offset = |c: &AnyClass| c.instance_variable(TAO_IVAR).map(|i| i.offset());
        class.instance_size() == panel.instance_size() && offset(class) == offset(panel)
    }

    pub(super) fn convert(window: &WebviewWindow) {
        let ns_window = match window.ns_window() {
            Ok(w) => w as usize,
            Err(e) => {
                log::warn!("[indicator] no native window: {e}");
                return;
            }
        };
        let result = window.run_on_main_thread(move || {
            // SAFETY: on the main thread, with the live NSWindow of the pill.
            match unsafe { convert_on_main(ns_window as *mut AnyObject) } {
                Ok(()) => log::info!("[indicator] pill is a non-activating panel"),
                Err(e) => log::warn!("[indicator] pill stays a regular window: {e}"),
            }
        });
        if let Err(e) = result {
            log::warn!("[indicator] {e}");
        }
    }

    unsafe fn convert_on_main(ns_window: *mut AnyObject) -> Result<(), String> {
        let window = unsafe { ns_window.as_ref() }.ok_or("null window")?;
        let panel = panel_class().ok_or("can't declare the panel class")?;
        let current = window.class();
        if !std::ptr::eq(current, panel) {
            if !same_layout(current, panel) {
                return Err(format!(
                    "{:?} doesn't have NSPanel's layout",
                    current.name()
                ));
            }
            // SAFETY: same instance size and ivar layout (checked above), and
            // NSPanel is an NSWindow subclass.
            unsafe { AnyObject::set_class(window, panel) };
        }
        unsafe {
            let mask: usize = msg_send![window, styleMask];
            let _: () = msg_send![window, setStyleMask: mask | NONACTIVATING_PANEL];
            let _: () = msg_send![window, setHidesOnDeactivate: Bool::NO];
            // The style mask alone doesn't update the window server once the
            // window exists; this (private, so guarded) call does.
            let prevents: Bool =
                msg_send![window, respondsToSelector: sel!(_setPreventsActivation:)];
            if prevents.as_bool() {
                let _: () = msg_send![window, _setPreventsActivation: Bool::YES];
            }
        }
        Ok(())
    }
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
        let mode = IndicatorMode::WhileDictating;
        assert_eq!(next_phase(mode, Listening, Error), Error);
        assert_eq!(next_phase(mode, Transcribing, Error), Error);
        assert_eq!(next_phase(mode, Hidden, Error), Hidden);
        assert_eq!(next_phase(mode, Error, Error), Hidden);
        assert_eq!(next_phase(mode, Error, Listening), Listening);
        assert_eq!(next_phase(mode, Listening, Transcribing), Transcribing);
        assert_eq!(next_phase(mode, Transcribing, Hidden), Hidden);
    }

    #[test]
    fn always_visible_rests_on_idle_and_off_never_shows() {
        use Phase::*;
        let always = IndicatorMode::Always;
        assert_eq!(resting_phase(always), Idle);
        assert_eq!(next_phase(always, Hidden, Hidden), Idle);
        assert_eq!(next_phase(always, Idle, Listening), Listening);
        assert_eq!(next_phase(always, Listening, Transcribing), Transcribing);
        assert_eq!(next_phase(always, Transcribing, Hidden), Idle);
        assert_eq!(next_phase(always, Listening, Error), Error);
        assert_eq!(next_phase(always, Error, Hidden), Idle);
        // An unrelated error while idle leaves the mic glyph alone.
        assert_eq!(next_phase(always, Idle, Error), Idle);
        assert_eq!(next_phase(always, Error, Error), Idle);

        assert_eq!(resting_phase(IndicatorMode::WhileDictating), Hidden);
        assert_eq!(
            next_phase(IndicatorMode::WhileDictating, Idle, Hidden),
            Hidden
        );

        let off = IndicatorMode::Off;
        assert_eq!(resting_phase(off), Hidden);
        for current in [Hidden, Idle, Listening, Transcribing, Error] {
            for requested in [Hidden, Idle, Listening, Transcribing, Error] {
                assert_eq!(next_phase(off, current, requested), Hidden);
            }
        }
    }

    #[test]
    fn saved_position_is_kept_only_while_it_is_on_a_screen() {
        let laptop = (0.0, 0.0, 1512.0, 982.0);
        let left_monitor = (-1920.0, -200.0, 1920.0, 1080.0);
        let at = |x, y| Some(IndicatorPosition { x, y });

        assert_eq!(restore_position(None, &[laptop]), None);
        assert_eq!(
            restore_position(at(700.0, 900.0), &[laptop]),
            at(700.0, 900.0)
        );
        // On the external monitor while it is plugged in...
        assert_eq!(
            restore_position(at(-900.0, 300.0), &[laptop, left_monitor]),
            at(-900.0, 300.0)
        );
        // ...and the default placement once it is unplugged.
        assert_eq!(restore_position(at(-900.0, 300.0), &[laptop]), None);
        // Partly off the edge is fine while the pill's centre is on screen.
        assert_eq!(
            restore_position(at(-40.0, 10.0), &[laptop]),
            at(-40.0, 10.0)
        );
        assert_eq!(restore_position(at(-80.0, 10.0), &[laptop]), None);
        assert_eq!(restore_position(at(700.0, 980.0), &[laptop]), None);
        assert_eq!(restore_position(at(700.0, 900.0), &[]), None);
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
        assert_eq!(serde_json::to_string(&Phase::Idle).unwrap(), "\"idle\"");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn panel_class_is_an_nspanel_with_the_tao_window_layout() {
        use objc2_06::runtime::{AnyClass, Bool, ClassBuilder};
        let panel = panel::panel_class().expect("panel class");
        assert_eq!(panel.superclass().unwrap().name(), c"NSPanel");
        assert!(std::ptr::eq(panel, panel::panel_class().unwrap()));
        let mut like_tao =
            ClassBuilder::new(c"TTMTestTaoWindow", AnyClass::get(c"NSWindow").unwrap()).unwrap();
        like_tao.add_ivar::<Bool>(c"focusable");
        assert!(panel::same_layout(like_tao.register(), panel));
        let mut bigger =
            ClassBuilder::new(c"TTMTestBigWindow", AnyClass::get(c"NSWindow").unwrap()).unwrap();
        bigger.add_ivar::<Bool>(c"focusable");
        bigger.add_ivar::<u64>(c"extra");
        assert!(!panel::same_layout(bigger.register(), panel));
    }
}
