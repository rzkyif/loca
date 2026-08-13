// Slimmed app state: the client no longer supervises sidecars or runs a
// download queue (those are owned by tomat-core). Only Tauri-specific state
// remains: window visibility, current global shortcut, input shortcuts,
// system-volume restore marker, region-capture target.

use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};

/// What a press should trigger, decided by [`PttState::begin_press`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PressStart {
    /// A prior hold was never closed (its release was missed by the platform's
    /// coarse release polling); this new press proves the key went up, so the
    /// caller must emit `shortcut-hold-end` for it before starting this press.
    pub close_orphaned_hold: bool,
    /// Whether to arm the hold timer for this press (push-to-talk mode).
    pub run_ptt: bool,
    /// Hold threshold captured for this press.
    pub hold_ms: u64,
    /// This press's generation, so a fired timer can confirm it's still current.
    pub generation: u64,
}

/// The terminal event a release should emit, from [`PttState::end_press`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressEnd {
    Tap,
    HoldEnd,
}

/// Push-to-talk config + in-flight press state for the window toggle shortcut.
///
/// The tap-vs-hold decision is made HERE in Rust, not in the webview, because
/// the OS "released" signal reaches JS over the WebView2 IPC pipe on the webview
/// UI thread, which stalls exactly while the cursor is over the (interactive,
/// non-clickthrough) window - so a quick tap's release could land after the JS
/// hold timer had already fired and (wrongly) started STT. Rust learns of the
/// press/release promptly and off that thread, so it classifies correctly and
/// emits a single semantic event (`shortcut-tap` / `shortcut-hold-start` /
/// `shortcut-hold-end`) the UI just reacts to.
///
/// A release always emits a terminal event (even in non-PTT mode, where JS just
/// resets its ring and ignores it) so the UI's hold flag can never strand true.
/// Releases are idempotent on `down`, because on Windows each `WM_HOTKEY` spawns
/// its own release-polling thread and several can report the final key-up.
pub struct PttState {
    /// Whether the window shortcut is in push-to-talk mode (pushed from JS via
    /// `set_ptt_config`). When false, a press is a plain window toggle; the hold
    /// timer isn't armed, but a release still emits a (JS-ignored) terminal.
    pub push_to_talk: bool,
    /// Hold threshold in ms: release before this is a tap, at/after it is a hold.
    pub hold_ms: u64,
    /// Bumped on every press and every release so a stale hold timer spawned by
    /// an earlier press can't fire against a newer (or already-classified) one.
    pub generation: u64,
    /// Whether the shortcut is currently physically held.
    pub down: bool,
    /// Whether the hold timer already fired for the current press (so the
    /// release knows to emit `shortcut-hold-end` rather than `shortcut-tap`).
    pub hold_started: bool,
}

impl Default for PttState {
    fn default() -> Self {
        Self {
            // Mirror the app's default `stt.activation` ("push-to-talk") so a
            // press landing in the tiny window before JS pushes `set_ptt_config`
            // is still classified; JS re-asserts the real value on attach.
            push_to_talk: true,
            hold_ms: 250,
            generation: 0,
            down: false,
            hold_started: false,
        }
    }
}

impl PttState {
    /// Register a press and return what it should trigger.
    pub fn begin_press(&mut self) -> PressStart {
        // A still-set `hold_started` means the previous press's release was
        // never observed; this press proves the key went up, so flag it for a
        // close-out. (Only PTT holds ever set `hold_started`.)
        let close_orphaned_hold = self.hold_started;
        self.generation = self.generation.wrapping_add(1);
        self.down = true;
        self.hold_started = false;
        PressStart {
            close_orphaned_hold,
            run_ptt: self.push_to_talk,
            hold_ms: self.hold_ms,
            generation: self.generation,
        }
    }

    /// Called when a press's hold timer elapses. Returns true if it actually
    /// started the hold (still the current press, still held, not already
    /// started), meaning the caller should emit `shortcut-hold-start`.
    pub fn try_start_hold(&mut self, generation: u64) -> bool {
        if self.generation == generation && self.down && !self.hold_started {
            self.hold_started = true;
            true
        } else {
            false
        }
    }

    /// Register a release. Returns the terminal event to emit, or `None` for a
    /// duplicate/late release of an already-classified press.
    pub fn end_press(&mut self) -> Option<PressEnd> {
        if !self.down {
            return None;
        }
        self.down = false;
        // Invalidate any pending hold timer for this press.
        self.generation = self.generation.wrapping_add(1);
        let was_hold = self.hold_started;
        self.hold_started = false;
        Some(if was_hold {
            PressEnd::HoldEnd
        } else {
            PressEnd::Tap
        })
    }
}

pub struct AppStateInner {
    /// Tracked main-window visibility. Single source of truth shared across
    /// the tray icon, the global shortcut, the close-to-tray handler, and
    /// the show/hide commands so they can't drift out of sync.
    pub visible: AtomicBool,
    /// Currently registered global toggle-window shortcut, so we can
    /// unregister it before applying a new one. `None` when disabled.
    pub current_shortcut: Mutex<Option<String>>,
    /// System output volume captured the first time we lower it for STT
    /// auto-volume during a listening session. `Some(v)` means a restore
    /// is owed; `None` means we haven't lowered (or have already restored).
    pub saved_volume: Mutex<Option<u8>>,
    /// Currently registered input shortcuts (event name + accelerator).
    /// Cleared on UserInput unmount.
    pub input_shortcuts: Mutex<Vec<(String, String)>>,
    /// Push-to-talk config + in-flight press state for the window toggle
    /// shortcut. See [`PttState`].
    pub ptt: Mutex<PttState>,
    /// The xcap monitor id the next region-capture invocation should crop
    /// against. Set by the JS helper before showing the overlay window;
    /// read by the overlay page on mount. Defaults to "primary".
    pub region_capture_target: Mutex<String>,
    /// PID of a local core THIS client session spawned via `start_local_core`
    /// (the service-less "on-demand" mode). `0` means we never spawned one
    /// (either a background service owns the core, or it was already up when we
    /// probed). Only set on a real spawn, so a non-zero value means "we own this
    /// core's liveness"; on app exit we stop exactly that PID, so quitting the
    /// client also stops a core it started. Never stops a core we didn't launch.
    pub spawned_core_pid: AtomicU32,
    /// The tray menu's "Start Core" / "Stop Core" item. Held so the webview can
    /// relabel and show/hide it as the local core's state changes (a tray menu
    /// is built once, then mutated). `None` until the tray is built.
    #[cfg(desktop)]
    pub tray_core_item: Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>,
    /// Whether the local core was running as of the webview's last push. Decides
    /// which intent the tray item emits, so the label and the action can't drift.
    #[cfg(desktop)]
    pub tray_core_running: AtomicBool,
}

#[derive(Clone)]
pub struct AppState(pub Arc<AppStateInner>);

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn fresh() -> AppStateInner {
        AppStateInner {
            visible: AtomicBool::new(true),
            current_shortcut: Mutex::new(None),
            saved_volume: Mutex::new(None),
            input_shortcuts: Mutex::new(Vec::new()),
            ptt: Mutex::new(PttState::default()),
            region_capture_target: Mutex::new("primary".to_string()),
            spawned_core_pid: AtomicU32::new(0),
            #[cfg(desktop)]
            tray_core_item: Mutex::new(None),
            #[cfg(desktop)]
            tray_core_running: AtomicBool::new(false),
        }
    }

    #[test]
    fn visible_atomic_round_trips() {
        let s = fresh();
        assert!(s.visible.load(Ordering::SeqCst));
        s.visible.store(false, Ordering::SeqCst);
        assert!(!s.visible.load(Ordering::SeqCst));
    }

    fn ptt(push_to_talk: bool) -> PttState {
        PttState {
            push_to_talk,
            hold_ms: 250,
            ..PttState::default()
        }
    }

    #[test]
    fn quick_tap_emits_tap_and_disarms_timer() {
        let mut p = ptt(true);
        let start = p.begin_press();
        assert!(start.run_ptt);
        assert!(!start.close_orphaned_hold);
        // Release before the hold timer fires.
        assert_eq!(p.end_press(), Some(PressEnd::Tap));
        // The timer, firing late for that generation, must be a no-op.
        assert!(!p.try_start_hold(start.generation));
    }

    #[test]
    fn hold_then_release_emits_hold_end() {
        let mut p = ptt(true);
        let start = p.begin_press();
        assert!(p.try_start_hold(start.generation));
        assert_eq!(p.end_press(), Some(PressEnd::HoldEnd));
    }

    #[test]
    fn duplicate_release_is_ignored() {
        let mut p = ptt(true);
        p.begin_press();
        assert_eq!(p.end_press(), Some(PressEnd::Tap));
        // A second release from another polling thread must not re-emit.
        assert_eq!(p.end_press(), None);
    }

    #[test]
    fn non_ptt_release_still_emits_terminal() {
        // So JS always gets an event to reset its ring, even in manual/sticky.
        let mut p = ptt(false);
        let start = p.begin_press();
        assert!(!start.run_ptt);
        assert_eq!(p.end_press(), Some(PressEnd::Tap));
    }

    #[test]
    fn new_press_closes_orphaned_hold() {
        let mut p = ptt(true);
        let first = p.begin_press();
        assert!(p.try_start_hold(first.generation));
        // First press's release was missed (no end_press). The next press must
        // flag the orphaned hold for close-out.
        let second = p.begin_press();
        assert!(second.close_orphaned_hold);
        // And the fresh press classifies cleanly on its own release.
        assert_eq!(p.end_press(), Some(PressEnd::Tap));
    }

    #[test]
    fn stale_timer_from_prior_press_does_not_start_hold() {
        let mut p = ptt(true);
        let first = p.begin_press();
        assert_eq!(p.end_press(), Some(PressEnd::Tap));
        let _second = p.begin_press();
        // The first press's timer fires late: wrong generation, no hold.
        assert!(!p.try_start_hold(first.generation));
    }

    #[test]
    fn current_shortcut_mutex_swaps_value() {
        let s = fresh();
        assert!(s.current_shortcut.lock().unwrap().is_none());
        *s.current_shortcut.lock().unwrap() = Some("Cmd+Space".to_string());
        assert_eq!(
            s.current_shortcut.lock().unwrap().as_deref(),
            Some("Cmd+Space")
        );
    }

    #[test]
    fn saved_volume_is_optional_and_settable() {
        let s = fresh();
        assert!(s.saved_volume.lock().unwrap().is_none());
        *s.saved_volume.lock().unwrap() = Some(75);
        assert_eq!(*s.saved_volume.lock().unwrap(), Some(75));
        *s.saved_volume.lock().unwrap() = None;
        assert!(s.saved_volume.lock().unwrap().is_none());
    }

    #[test]
    fn input_shortcuts_starts_empty_and_appends() {
        let s = fresh();
        assert!(s.input_shortcuts.lock().unwrap().is_empty());
        s.input_shortcuts
            .lock()
            .unwrap()
            .push(("submit".into(), "Enter".into()));
        assert_eq!(s.input_shortcuts.lock().unwrap().len(), 1);
    }

    #[test]
    fn region_capture_target_defaults_primary() {
        let s = fresh();
        assert_eq!(*s.region_capture_target.lock().unwrap(), "primary");
    }
}
