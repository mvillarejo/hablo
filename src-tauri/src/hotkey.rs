use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::hotkey_layout;

#[cfg(target_os = "macos")]
mod macos_fn {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    const NS_FLAGS_CHANGED_MASK: u64 = 1 << 12; // NSEventMaskFlagsChanged

    pub struct FnKeyTap {
        running: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        /// NSObject* returned by addGlobalMonitorForEventsMatchingMask (if NSEvent path was used)
        #[allow(dead_code)]
        ns_monitor: Arc<std::sync::Mutex<Option<*mut c_void>>>,
    }

    unsafe impl Send for FnKeyTap {}
    unsafe impl Sync for FnKeyTap {}

    impl FnKeyTap {
        /// Try to start a modifier-key monitor watching the given flag `mask`
        /// (a device-dependent modifier mask, e.g. Right-Cmd = 0x10, or Fn = 0x800000).
        /// Returns Ok(Self) if the monitor is running, Err(reason) otherwise.
        pub fn start<F: Fn(bool) + Send + Sync + 'static>(
            mask: u64,
            callback: F,
        ) -> Result<Self, String> {
            let cb = Arc::new(callback);
            let running = Arc::new(AtomicBool::new(true));
            #[allow(clippy::arc_with_non_send_sync)]
            let ns_monitor: Arc<std::sync::Mutex<Option<*mut c_void>>> =
                Arc::new(std::sync::Mutex::new(None));

            // Channel so the spawned thread can report whether the tap was created
            let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();

            let running_clone = running.clone();
            let cb_clone = cb.clone();

            let thread = std::thread::spawn(move || {
                // Try NSEvent global monitor first (more reliable on modern macOS for Globe key)
                match Self::try_nsevent_monitor(mask, cb_clone.clone(), running_clone.clone()) {
                    Ok(monitor_ptr) => {
                        tx.send(Ok(())).ok();
                        // Keep thread alive; the handler fires on the main run loop,
                        // so we just need to pump the CF run loop here so events flow.
                        Self::pump_runloop(running_clone);
                        // Cleanup: remove monitor
                        Self::remove_nsevent_monitor(monitor_ptr);
                        return;
                    }
                    Err(ns_err) => {
                        log::warn!("NSEvent monitor failed ({ns_err}), falling back to CGEventTap");
                    }
                }

                // Fallback: CGEventTap
                match Self::try_cg_event_tap(mask, cb_clone, running_clone.clone()) {
                    Ok(()) => {
                        tx.send(Ok(())).ok();
                        // pump_runloop already ran inside try_cg_event_tap
                    }
                    Err(cg_err) => {
                        tx.send(Err(format!(
                            "Both NSEvent and CGEventTap failed. \
                             CGEventTap error: {cg_err}. \
                             Accessibility permission is likely not granted."
                        )))
                        .ok();
                    }
                }
            });

            // Wait up to 3s for the thread to report success/failure
            let result = rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| "Timeout waiting for Fn key monitor to initialize".to_string())?;

            result.map(|()| Self {
                running,
                thread: Some(thread),
                ns_monitor,
            })
        }

        fn try_nsevent_monitor(
            mask: u64,
            cb: Arc<dyn Fn(bool) + Send + Sync>,
            _running: Arc<AtomicBool>,
        ) -> Result<*mut c_void, String> {
            use block2::RcBlock;
            use objc2::msg_send;
            use objc2::runtime::{AnyClass, AnyObject};

            let ns_event_cls = AnyClass::get("NSEvent").ok_or("NSEvent class not found")?;

            let was_pressed = Arc::new(AtomicBool::new(false));
            let was_pressed_clone = was_pressed.clone();
            let cb_clone = cb.clone();

            let handler = RcBlock::new(move |event: *mut AnyObject| {
                let modifier_flags: u64 = unsafe { msg_send![event, modifierFlags] };
                let fn_down = (modifier_flags & mask) != 0;
                let prev = was_pressed_clone.swap(fn_down, Ordering::SeqCst);
                if fn_down && !prev {
                    cb_clone(true);
                } else if !fn_down && prev {
                    cb_clone(false);
                }
            });

            let monitor: *mut AnyObject = unsafe {
                msg_send![
                    ns_event_cls,
                    addGlobalMonitorForEventsMatchingMask: NS_FLAGS_CHANGED_MASK
                    handler: &*handler
                ]
            };

            if monitor.is_null() {
                return Err("addGlobalMonitorForEventsMatchingMask returned nil".into());
            }

            Ok(monitor as *mut c_void)
        }

        fn remove_nsevent_monitor(monitor: *mut c_void) {
            use objc2::msg_send;
            use objc2::runtime::AnyClass;
            if !monitor.is_null() {
                let cls = AnyClass::get("NSEvent").unwrap();
                unsafe {
                    let _: () =
                        msg_send![cls, removeMonitor: monitor as *mut objc2::runtime::AnyObject];
                }
            }
        }

        fn try_cg_event_tap(
            mask: u64,
            cb: Arc<dyn Fn(bool) + Send + Sync>,
            running: Arc<AtomicBool>,
        ) -> Result<(), String> {
            use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
            use core_graphics::event::{
                CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
                CGEventType,
            };

            let was_pressed = Arc::new(AtomicBool::new(false));

            let tap = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                vec![CGEventType::FlagsChanged],
                move |_proxy, _event_type, event: &CGEvent| {
                    let flags = event.get_flags();
                    let fn_down = (flags.bits() & mask) != 0;
                    let prev = was_pressed.swap(fn_down, Ordering::SeqCst);
                    if fn_down && !prev {
                        cb(true);
                    } else if !fn_down && prev {
                        cb(false);
                    }
                    None
                },
            )
            .map_err(|()| "CGEventTap::new failed — Accessibility permission required")?;

            unsafe {
                let source = tap
                    .mach_port
                    .create_runloop_source(0)
                    .expect("failed to create runloop source");
                CFRunLoop::get_current().add_source(&source, kCFRunLoopCommonModes);
                tap.enable();
            }

            Self::pump_runloop(running);
            Ok(())
        }

        fn pump_runloop(running: Arc<AtomicBool>) {
            use core_foundation::runloop::{CFRunLoop, CFRunLoopRunResult};
            while running.load(Ordering::SeqCst) {
                let result = CFRunLoop::run_in_mode(
                    unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                    Duration::from_millis(250),
                    false,
                );
                // When the run loop has no sources/timers (the NSEvent global-monitor
                // path — its handler fires on the main run loop, not this thread),
                // run_in_mode returns Finished immediately instead of waiting the
                // timeout. Sleep so we don't busy-spin a CPU core at 100%.
                if result == CFRunLoopRunResult::Finished {
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }

        /// Start a tap on one non-modifier key or extra mouse button (`target`). Its
        /// down/up events are swallowed so the key never types and the button never
        /// clicks, and autorepeat KeyDowns are dropped so holding it yields exactly one
        /// press and one release. Other keys and buttons pass through untouched.
        /// Needs a filtering (non-listen-only) CGEventTap, hence Accessibility.
        pub fn start_target<F: Fn(bool) + Send + Sync + 'static>(
            target: super::TapTarget,
            callback: F,
        ) -> Result<Self, String> {
            let running = Arc::new(AtomicBool::new(true));
            let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
            let running_clone = running.clone();
            let thread = std::thread::spawn(move || {
                Self::run_target_tap(target, callback, running_clone, tx);
            });

            let result = rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| "Timeout waiting for key tap to initialize".to_string())?;

            #[allow(clippy::arc_with_non_send_sync)]
            let ns_monitor = Arc::new(std::sync::Mutex::new(None));
            result.map(|()| Self {
                running,
                thread: Some(thread),
                ns_monitor,
            })
        }

        fn run_target_tap<F: Fn(bool) + Send + Sync + 'static>(
            target: super::TapTarget,
            cb: F,
            running: Arc<AtomicBool>,
            tx: std::sync::mpsc::Sender<Result<(), String>>,
        ) {
            use super::TapTarget;
            use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
            use core_graphics::event::{
                CGEvent, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
                CGEventType, EventField,
            };

            let event_types = match target {
                TapTarget::Key(_) => vec![CGEventType::KeyDown, CGEventType::KeyUp],
                TapTarget::MouseButton(_) => {
                    vec![CGEventType::OtherMouseDown, CGEventType::OtherMouseUp]
                }
            };
            let was_down = AtomicBool::new(false);
            let tap = CGEventTap::new(
                CGEventTapLocation::Session,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                event_types,
                move |_proxy, event_type, event: &CGEvent| {
                    let key_down = match event_type {
                        CGEventType::KeyDown | CGEventType::OtherMouseDown => true,
                        CGEventType::KeyUp | CGEventType::OtherMouseUp => false,
                        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                            log::warn!("[hotkey] key tap disabled by macOS; re-enabling");
                            return None;
                        }
                        _ => return None,
                    };
                    let (field, wanted) = match target {
                        TapTarget::Key(keycode) => {
                            (EventField::KEYBOARD_EVENT_KEYCODE, i64::from(keycode))
                        }
                        TapTarget::MouseButton(button) => {
                            (EventField::MOUSE_EVENT_BUTTON_NUMBER, i64::from(button))
                        }
                    };
                    if event.get_integer_value_field(field) != wanted {
                        return None;
                    }
                    // Mouse buttons don't autorepeat; the field only exists on key events.
                    let autorepeat = matches!(target, TapTarget::Key(_))
                        && event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT)
                            != 0;
                    let prev = was_down.load(Ordering::SeqCst);
                    if let Some(pressed) = super::key_edge(key_down, autorepeat, prev) {
                        was_down.store(pressed, Ordering::SeqCst);
                        cb(pressed);
                    }
                    // Swallow every event for the hotkey (autorepeats too) so it never
                    // types or clicks (no browser back/forward, no middle-click paste).
                    // core-graphics 0.24 treats a `None` return as pass-through and
                    // can't return NULL, so the event is nulled out instead.
                    event.set_type(CGEventType::Null);
                    None
                },
            );
            let tap = match tap {
                Ok(tap) => tap,
                Err(()) => {
                    tx.send(Err(
                        "CGEventTap::new failed — Accessibility permission required".into(),
                    ))
                    .ok();
                    return;
                }
            };

            unsafe {
                let source = tap
                    .mach_port
                    .create_runloop_source(0)
                    .expect("failed to create runloop source");
                CFRunLoop::get_current().add_source(&source, kCFRunLoopCommonModes);
                tap.enable();
            }
            tx.send(Ok(())).ok();

            while running.load(Ordering::SeqCst) {
                CFRunLoop::run_in_mode(
                    unsafe { core_foundation::runloop::kCFRunLoopDefaultMode },
                    Duration::from_millis(250),
                    false,
                );
                // macOS disables a filtering tap after a callback timeout or certain
                // user input; enabling an already-enabled tap is a no-op.
                tap.enable();
            }
        }

        pub fn stop(&mut self) {
            self.running.store(false, Ordering::SeqCst);
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }

    impl Drop for FnKeyTap {
        fn drop(&mut self) {
            self.stop();
        }
    }
}

pub struct HotkeyManager {
    current_hotkey: Arc<std::sync::Mutex<String>>,
    active: Arc<AtomicBool>,
    #[cfg(target_os = "macos")]
    fn_tap: Arc<std::sync::Mutex<Option<macos_fn::FnKeyTap>>>,
}

impl HotkeyManager {
    pub fn new() -> Self {
        Self {
            current_hotkey: Arc::new(std::sync::Mutex::new("Fn".to_string())),
            active: Arc::new(AtomicBool::new(false)),
            #[cfg(target_os = "macos")]
            fn_tap: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn register(&self, hotkey: &str, app_handle: AppHandle) -> Result<(), String> {
        self.unregister(&app_handle)?;

        log::info!("[hotkey] registering shortcut config: {hotkey:?}");

        {
            let mut current = self.current_hotkey.lock().unwrap();
            *current = hotkey.to_string();
        }
        self.active.store(true, Ordering::SeqCst);

        self.register_impl(hotkey, app_handle).map_err(|e| {
            log::error!("[hotkey] failed to register {hotkey:?}: {e}");
            e
        })?;

        log::info!(
            "[hotkey] registered {hotkey:?} via {}",
            registration_method(hotkey)
        );
        Ok(())
    }

    /// On macOS, a lone modifier (incl. left/right variants and Fn) is watched
    /// natively via a flags-changed tap; everything else (composites) goes through
    /// the global-shortcut plugin.
    #[cfg(target_os = "macos")]
    fn register_impl(&self, hotkey: &str, app_handle: AppHandle) -> Result<(), String> {
        if let Some(mask) = single_modifier_mask(hotkey) {
            log::info!("[hotkey] using modifier tap for {hotkey:?} (mask=0x{mask:x})");
            self.register_modifier_tap(hotkey, mask, app_handle)
        } else if let Some(target) = tap_target(hotkey) {
            log::info!("[hotkey] using key tap for {hotkey:?} ({target:?})");
            self.register_key_tap(hotkey, target, app_handle)
        } else {
            log::info!("[hotkey] using global-shortcut plugin for {hotkey:?}");
            self.register_modifier_key(hotkey, app_handle)
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn register_impl(&self, hotkey: &str, app_handle: AppHandle) -> Result<(), String> {
        if hotkey == "Fn" {
            return Err("Fn key is only supported on macOS".to_string());
        }
        if mouse_button_number(hotkey).is_some() {
            return Err(MOUSE_MACOS_ONLY.to_string());
        }
        log::info!("[hotkey] using global-shortcut plugin for {hotkey:?}");
        // Single modifiers can't be watched natively here; register_modifier_key
        // falls back to the Modifier+Space workaround.
        self.register_modifier_key(hotkey, app_handle)
    }

    pub fn unregister(&self, app_handle: &AppHandle) -> Result<(), String> {
        self.active.store(false, Ordering::SeqCst);

        #[cfg(target_os = "macos")]
        {
            let mut tap = self.fn_tap.lock().unwrap();
            if let Some(mut t) = tap.take() {
                t.stop();
            }
        }

        // Unregister any global shortcut plugin shortcuts
        let _ = app_handle.global_shortcut().unregister_all();

        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn register_modifier_tap(
        &self,
        hotkey: &str,
        mask: u64,
        app_handle: AppHandle,
    ) -> Result<(), String> {
        let hotkey_label = hotkey.to_string();
        let handle = app_handle.clone();
        let tap = macos_fn::FnKeyTap::start(mask, move |pressed| {
            if pressed {
                log::info!(
                    "[hotkey] modifier tap: pressed hotkey={hotkey_label:?} mask=0x{mask:x}"
                );
                let _ = handle.emit("hotkey-pressed", ());
            } else {
                log::info!(
                    "[hotkey] modifier tap: released hotkey={hotkey_label:?} mask=0x{mask:x}"
                );
                let _ = handle.emit("hotkey-released", ());
            }
        })?;

        let mut fn_tap = self.fn_tap.lock().unwrap();
        *fn_tap = Some(tap);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn register_key_tap(
        &self,
        hotkey: &str,
        target: TapTarget,
        app_handle: AppHandle,
    ) -> Result<(), String> {
        let hotkey_label = hotkey.to_string();
        let tap = macos_fn::FnKeyTap::start_target(target, move |pressed| {
            let state = if pressed { "pressed" } else { "released" };
            log::info!("[hotkey] key tap: {state} hotkey={hotkey_label:?} {target:?}");
            let event = if pressed {
                "hotkey-pressed"
            } else {
                "hotkey-released"
            };
            let _ = app_handle.emit(event, ());
        })?;

        let mut fn_tap = self.fn_tap.lock().unwrap();
        *fn_tap = Some(tap);
        Ok(())
    }

    fn register_modifier_key(&self, key: &str, app_handle: AppHandle) -> Result<(), String> {
        use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

        let shortcut = if key.contains('+') {
            parse_combo_shortcut(key)?
        } else {
            // A lone modifier — possibly side-qualified (e.g. "CmdRight"). The plugin
            // can't bind a bare modifier, so map it to Modifier+Space as a fallback.
            let base = key.trim_end_matches("Left").trim_end_matches("Right");
            let modifiers = match base {
                "Ctrl" => Modifiers::CONTROL,
                "Cmd" => Modifiers::META,
                "Alt" => Modifiers::ALT,
                "Shift" => Modifiers::SHIFT,
                _ => return Err(format!("Unsupported hotkey: {}", key)),
            };
            Shortcut::new(Some(modifiers), Code::Space)
        };

        log::info!("[hotkey] global-shortcut binding: config={key:?} physical={shortcut:?}");

        let configured = key.to_string();
        let handle = app_handle.clone();
        app_handle
            .global_shortcut()
            .on_shortcut(shortcut, move |_app, shortcut, event| {
                use tauri_plugin_global_shortcut::ShortcutState;
                match event.state {
                    ShortcutState::Pressed => {
                        log::info!(
                            "[hotkey] global-shortcut: pressed config={configured:?} physical={shortcut:?}"
                        );
                        let _ = handle.emit("hotkey-pressed", ());
                    }
                    ShortcutState::Released => {
                        log::info!(
                            "[hotkey] global-shortcut: released config={configured:?} physical={shortcut:?}"
                        );
                        let _ = handle.emit("hotkey-released", ());
                    }
                }
            })
            .map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "macos")]
fn registration_method(hotkey: &str) -> &'static str {
    if single_modifier_mask(hotkey).is_some() {
        "modifier-tap"
    } else if bare_key_keycode(hotkey).is_some() {
        "key-tap"
    } else if mouse_button_number(hotkey).is_some() {
        "mouse-button-tap"
    } else if hotkey.contains('+') {
        "global-shortcut (combo)"
    } else {
        "global-shortcut (modifier+Space fallback)"
    }
}

#[cfg(not(target_os = "macos"))]
fn registration_method(hotkey: &str) -> &'static str {
    if hotkey.contains('+') {
        "global-shortcut (combo)"
    } else {
        "global-shortcut (modifier+Space fallback)"
    }
}

impl Default for HotkeyManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a single-modifier hotkey token to its macOS device-dependent flag mask
/// (`NX_DEVICE*KEYMASK` from IOLLEvent.h). Left/right are distinct. Returns `None`
/// for composites or any token that isn't a lone modifier.
#[cfg(target_os = "macos")]
fn single_modifier_mask(hotkey: &str) -> Option<u64> {
    let mask = match hotkey {
        "Fn" => 0x0080_0000, // NSEventModifierFlagFunction / kCGEventFlagMaskSecondaryFn
        "CtrlLeft" => 0x0000_0001,
        "ShiftLeft" => 0x0000_0002,
        "ShiftRight" => 0x0000_0004,
        "CmdLeft" => 0x0000_0008,
        "CmdRight" => 0x0000_0010,
        "AltLeft" => 0x0000_0020,
        "AltRight" => 0x0000_0040,
        "CtrlRight" => 0x0000_2000,
        _ => return None,
    };
    Some(mask)
}

/// Allowlist of non-modifier keys that may be bound on their own (no modifier),
/// mapped to their macOS virtual keycodes (`kVK_*` in HIToolbox Events.h). Keys
/// that type text in normal use (letters, digits, Space…) are deliberately absent.
#[cfg(target_os = "macos")]
fn bare_key_keycode(hotkey: &str) -> Option<u16> {
    let keycode = match hotkey {
        "Section" | "§" => 0x0a, // kVK_ISO_Section
        "F13" => 0x69,
        "F14" => 0x6b,
        "F15" => 0x71,
        "F16" => 0x6a,
        "F17" => 0x40,
        "F18" => 0x4f,
        "F19" => 0x50,
        _ => return None,
    };
    Some(keycode)
}

/// Maps an extra-mouse-button token ("Mouse3" middle, "Mouse4" back, "Mouse5"
/// forward … "Mouse8") to its CoreGraphics button number, which is 0-based (left
/// = 0), so `MouseN` → N-1. Left and right click (Mouse1/Mouse2) are never allowed:
/// swallowing them would make the mouse unusable.
fn mouse_button_number(hotkey: &str) -> Option<u8> {
    let n: u8 = hotkey.strip_prefix("Mouse")?.parse().ok()?;
    // Reject "Mouse04"/"Mouse+4" so only the canonical token is ever stored.
    if !(3..=8).contains(&n) || hotkey != format!("Mouse{n}") {
        return None;
    }
    Some(n - 1)
}

#[cfg(not(target_os = "macos"))]
const MOUSE_MACOS_ONLY: &str = "Mouse-button hotkeys are macOS-only for now";

/// What a filtering CGEventTap watches: a non-modifier key (macOS virtual keycode)
/// or an extra mouse button (CoreGraphics button number).
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TapTarget {
    Key(u16),
    MouseButton(u8),
}

#[cfg(target_os = "macos")]
fn tap_target(hotkey: &str) -> Option<TapTarget> {
    bare_key_keycode(hotkey)
        .map(TapTarget::Key)
        .or_else(|| mouse_button_number(hotkey).map(TapTarget::MouseButton))
}

/// Hotkey edge for an event on the watched key: `Some(true)` on the first KeyDown,
/// `Some(false)` on the KeyUp that ends it, `None` for autorepeat KeyDowns and
/// KeyUps with no matching press. A hold therefore gives exactly one start and stop.
#[cfg(target_os = "macos")]
fn key_edge(key_down: bool, autorepeat: bool, was_down: bool) -> Option<bool> {
    match (key_down, autorepeat, was_down) {
        (true, false, false) => Some(true),
        (false, _, true) => Some(false),
        _ => None,
    }
}

/// Canonical form to persist: the literal "§" is stored as the layout-independent
/// "Section" token. Anything else is returned unchanged.
pub fn canonical_hotkey(hotkey: &str) -> String {
    if hotkey == "§" {
        "Section".to_string()
    } else {
        hotkey.to_string()
    }
}

/// Validate a hotkey string before persisting or registering it.
pub fn validate_hotkey(hotkey: &str) -> Result<(), String> {
    let hotkey = hotkey.trim();
    if hotkey.is_empty() {
        return Err("Hotkey cannot be empty".into());
    }

    if hotkey == "Fn" {
        #[cfg(not(target_os = "macos"))]
        return Err("Fn key is only supported on macOS".into());
        #[cfg(target_os = "macos")]
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    if single_modifier_mask(hotkey).is_some() || tap_target(hotkey).is_some() {
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    if mouse_button_number(hotkey).is_some() {
        return Err(MOUSE_MACOS_ONLY.into());
    }

    if hotkey.contains('+') {
        parse_combo_shortcut(hotkey)?;
        return Ok(());
    }

    let base = hotkey.trim_end_matches("Left").trim_end_matches("Right");
    match base {
        "Ctrl" | "Cmd" | "Alt" | "Shift" => Ok(()),
        _ => Err(format!("Unsupported hotkey: {hotkey}")),
    }
}

fn parse_combo_shortcut(combo: &str) -> Result<tauri_plugin_global_shortcut::Shortcut, String> {
    use tauri_plugin_global_shortcut::{Modifiers, Shortcut};

    let parts: Vec<&str> = combo.split('+').collect();
    if parts.len() < 2 {
        return Err(format!(
            "Shortcut needs at least modifier+key, got: {combo}"
        ));
    }

    let mut mods = Modifiers::empty();
    let mut key_part: Option<&str> = None;

    for part in &parts {
        match part.to_lowercase().as_str() {
            "cmd" | "super" | "meta" | "command" => mods |= Modifiers::SUPER,
            "shift" => mods |= Modifiers::SHIFT,
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            _ => {
                if key_part.is_some() {
                    return Err(format!("Multiple non-modifier keys in shortcut: {combo}"));
                }
                key_part = Some(part);
            }
        }
    }

    let key_str = key_part.ok_or_else(|| format!("No non-modifier key in shortcut: {combo}"))?;

    let code = hotkey_layout::resolve_logical_key(key_str)?;
    log::info!(
        "[hotkey] parsed combo {combo:?}: logical_key={key_str:?} physical_code={code:?} modifiers={mods:?}"
    );

    if mods.is_empty() {
        return Err(format!(
            "Shortcut must include at least one modifier: {combo}"
        ));
    }

    Ok(Shortcut::new(Some(mods), code))
}

#[cfg(test)]
mod validation_tests {
    use super::validate_hotkey;

    #[test]
    fn rejects_empty_hotkey() {
        assert!(validate_hotkey("").is_err());
        assert!(validate_hotkey("   ").is_err());
    }

    #[test]
    fn rejects_multiple_non_modifier_keys() {
        assert!(validate_hotkey("Ctrl+A+B").is_err());
    }

    #[test]
    fn accepts_composite_with_stable_key() {
        assert!(validate_hotkey("Ctrl+Space").is_ok());
    }

    #[test]
    fn accepts_composite_with_letter() {
        assert!(validate_hotkey("Cmd+A").is_ok());
    }

    #[test]
    fn accepts_lone_ctrl_modifier() {
        assert!(validate_hotkey("Ctrl").is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_fn_on_macos() {
        assert!(validate_hotkey("Fn").is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_side_qualified_modifier_on_macos() {
        assert!(validate_hotkey("CmdRight").is_ok());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn rejects_fn_off_macos() {
        assert!(validate_hotkey("Fn").is_err());
    }

    #[test]
    fn rejects_unknown_token() {
        assert!(validate_hotkey("NotARealHotkey").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_allowlisted_bare_keys_on_macos() {
        for hotkey in ["Section", "§", "F13", "F19"] {
            assert!(validate_hotkey(hotkey).is_ok(), "{hotkey}");
        }
    }

    #[test]
    fn rejects_bare_typing_keys() {
        for hotkey in ["A", "a", "1", "0", "Space", "Enter", "F12", ";"] {
            assert!(validate_hotkey(hotkey).is_err(), "{hotkey}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn accepts_extra_mouse_buttons_on_macos() {
        for hotkey in ["Mouse3", "Mouse4", "Mouse5", "Mouse8"] {
            assert!(validate_hotkey(hotkey).is_ok(), "{hotkey}");
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn rejects_mouse_buttons_off_macos_with_clear_error() {
        let err = validate_hotkey("Mouse4").unwrap_err();
        assert!(err.contains("macOS-only"), "{err}");
    }

    #[test]
    fn rejects_left_right_click_and_malformed_mouse_tokens() {
        for hotkey in [
            "Mouse1",
            "Mouse2",
            "Mouse0",
            "Mouse9",
            "Mouse",
            "Mouse04",
            "Mouse+4",
            "mouse4",
            "Mouse-1",
            "MouseX",
            "Cmd+Mouse4",
        ] {
            assert!(validate_hotkey(hotkey).is_err(), "{hotkey}");
        }
    }

    #[test]
    fn canonicalizes_section_alias() {
        use super::canonical_hotkey;
        assert_eq!(canonical_hotkey("§"), "Section");
        assert_eq!(canonical_hotkey("Section"), "Section");
        assert_eq!(canonical_hotkey("Cmd+§"), "Cmd+§");
    }
}

#[cfg(test)]
mod mouse_button_tests {
    use super::mouse_button_number;

    #[test]
    fn mouse_tokens_map_to_zero_based_cg_button_numbers() {
        assert_eq!(mouse_button_number("Mouse3"), Some(2));
        assert_eq!(mouse_button_number("Mouse4"), Some(3));
        assert_eq!(mouse_button_number("Mouse5"), Some(4));
        assert_eq!(mouse_button_number("Mouse8"), Some(7));
    }

    #[test]
    fn left_right_click_and_garbage_are_not_mouse_hotkeys() {
        for hotkey in [
            "Mouse1", "Mouse2", "Mouse0", "Mouse9", "Mouse256", "Mouse", "Mouse04", "Mouse+4",
            "mouse4", "MOUSE4", " Mouse4", "Mouse4 ", "Mouse-3", "F13", "Section", "",
        ] {
            assert_eq!(mouse_button_number(hotkey), None, "{hotkey:?}");
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod bare_key_tests {
    use super::{bare_key_keycode, key_edge, registration_method, tap_target, TapTarget};

    #[test]
    fn section_maps_to_iso_section_keycode() {
        assert_eq!(bare_key_keycode("Section"), Some(0x0a));
        assert_eq!(bare_key_keycode("§"), Some(0x0a));
        assert_eq!(bare_key_keycode("F13"), Some(0x69));
        assert_eq!(bare_key_keycode("A"), None);
        assert_eq!(bare_key_keycode("Space"), None);
    }

    #[test]
    fn registration_method_reports_path() {
        assert_eq!(registration_method("Section"), "key-tap");
        assert_eq!(registration_method("§"), "key-tap");
        assert_eq!(registration_method("F15"), "key-tap");
        assert_eq!(registration_method("Mouse4"), "mouse-button-tap");
        assert_ne!(registration_method("Mouse1"), "mouse-button-tap");
        assert_eq!(registration_method("Fn"), "modifier-tap");
        assert_eq!(registration_method("Ctrl+Space"), "global-shortcut (combo)");
    }

    #[test]
    fn tap_target_routes_keys_and_mouse_buttons() {
        assert_eq!(tap_target("Section"), Some(TapTarget::Key(0x0a)));
        assert_eq!(tap_target("F13"), Some(TapTarget::Key(0x69)));
        assert_eq!(tap_target("Mouse3"), Some(TapTarget::MouseButton(2)));
        assert_eq!(tap_target("Mouse5"), Some(TapTarget::MouseButton(4)));
        assert_eq!(tap_target("Mouse2"), None);
        assert_eq!(tap_target("Fn"), None);
        assert_eq!(tap_target("Ctrl+Space"), None);
    }

    #[test]
    fn mouse_button_hold_gives_one_start_and_one_stop() {
        // Mouse buttons never autorepeat: down, up → one press, one release.
        assert_eq!(edges(&[(true, false), (false, false)]), vec![true, false]);
    }

    /// Feed (key_down, autorepeat) events through key_edge, tracking state like the tap.
    fn edges(events: &[(bool, bool)]) -> Vec<bool> {
        let mut down = false;
        let mut out = Vec::new();
        for &(key_down, autorepeat) in events {
            if let Some(pressed) = key_edge(key_down, autorepeat, down) {
                down = pressed;
                out.push(pressed);
            }
        }
        out
    }

    #[test]
    fn hold_with_autorepeat_gives_one_start_and_one_stop() {
        let hold = [
            (true, false),
            (true, true),
            (true, true),
            (true, true),
            (false, false),
        ];
        assert_eq!(edges(&hold), vec![true, false]);
    }

    #[test]
    fn two_taps_give_two_presses_for_toggle_mode() {
        let taps = [(true, false), (false, false), (true, false), (false, false)];
        assert_eq!(edges(&taps), vec![true, false, true, false]);
    }

    #[test]
    fn stray_release_and_repeat_without_press_are_ignored() {
        assert_eq!(edges(&[(false, false), (true, true)]), Vec::<bool>::new());
    }
}
