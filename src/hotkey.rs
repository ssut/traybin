//! Global hotkey management for toggling the screenshot window

use crossbeam_channel::Sender;
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use log::{error, info, warn};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;

use crate::tray::toggle_window;
use crate::AppMessage;

/// Global flag to track if hotkey is enabled at runtime
static HOTKEY_ENABLED: AtomicBool = AtomicBool::new(true);

/// Current registered hotkey ID
static CURRENT_HOTKEY_ID: AtomicU32 = AtomicU32::new(0);

/// Current registered hotkey (for unregistering)
static CURRENT_HOTKEY: Mutex<Option<HotKey>> = Mutex::new(None);

/// Thread-safe wrapper for GlobalHotKeyManager
/// SAFETY: GlobalHotKeyManager must only be accessed from the main thread
struct HotKeyManagerWrapper(GlobalHotKeyManager);

// SAFETY: We ensure all access happens on the main thread via message passing
unsafe impl Send for HotKeyManagerWrapper {}
unsafe impl Sync for HotKeyManagerWrapper {}

/// Global manager reference for runtime hotkey updates
static HOTKEY_MANAGER: OnceLock<Mutex<HotKeyManagerWrapper>> = OnceLock::new();

/// Initialize global hotkey manager with custom hotkey string
/// IMPORTANT: Must be called from main thread before GPUI app starts
/// The manager is stored globally for runtime hotkey updates
pub fn init_global_hotkey(_message_tx: Sender<AppMessage>, hotkey_str: &str) -> bool {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to create global hotkey manager: {:?}", e);
            return false;
        }
    };

    // Parse the hotkey string
    let (modifiers, code) = match parse_hotkey_string(hotkey_str) {
        Some((m, c)) => (m, c),
        None => {
            warn!(
                "Invalid hotkey string '{}', using default Ctrl+Shift+S",
                hotkey_str
            );
            (Modifiers::CONTROL | Modifiers::SHIFT, Code::KeyS)
        }
    };

    let hotkey = HotKey::new(Some(modifiers), code);

    if let Err(e) = manager.register(hotkey) {
        error!("Failed to register hotkey {}: {:?}", hotkey_str, e);
        return false;
    }

    info!("Registered global hotkey: {}", hotkey_str);

    // Store the hotkey ID and hotkey for later updates
    let hotkey_id = hotkey.id();
    CURRENT_HOTKEY_ID.store(hotkey_id, Ordering::SeqCst);
    *CURRENT_HOTKEY.lock() = Some(hotkey);

    // Store manager globally for runtime updates
    let _ = HOTKEY_MANAGER.set(Mutex::new(HotKeyManagerWrapper(manager)));

    // Handle hotkey events in a background thread
    // This thread checks CURRENT_HOTKEY_ID dynamically to support runtime changes
    std::thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        loop {
            if let Ok(event) = receiver.recv() {
                let current_id = CURRENT_HOTKEY_ID.load(Ordering::SeqCst);
                if event.id == current_id && event.state == HotKeyState::Pressed {
                    if HOTKEY_ENABLED.load(Ordering::SeqCst) {
                        info!("Global hotkey pressed - toggling window");
                        toggle_window();
                    } else {
                        warn!("Global hotkey pressed but disabled");
                    }
                }
            }
        }
    });

    true
}

/// Update the global hotkey to a new key combination
/// This performs runtime re-registration of the hotkey
pub fn update_hotkey(new_hotkey_str: &str) -> bool {
    info!("Updating hotkey to: {}", new_hotkey_str);

    // Parse the new hotkey string
    let (modifiers, code) = match parse_hotkey_string(new_hotkey_str) {
        Some((m, c)) => (m, c),
        None => {
            error!("Invalid hotkey string: {}", new_hotkey_str);
            return false;
        }
    };

    let new_hotkey = HotKey::new(Some(modifiers), code);

    // Get the manager
    let manager_cell = match HOTKEY_MANAGER.get() {
        Some(m) => m,
        None => {
            error!("Hotkey manager not initialized");
            return false;
        }
    };

    let mut manager_guard = manager_cell.lock();
    let manager = &mut manager_guard.0;

    // Unregister the old hotkey
    {
        let mut old_hotkey_guard = CURRENT_HOTKEY.lock();
        if let Some(old_hotkey) = old_hotkey_guard.take() {
            if let Err(e) = manager.unregister(old_hotkey) {
                warn!("Failed to unregister old hotkey: {:?}", e);
                // Continue anyway - might already be unregistered
            } else {
                info!("Unregistered old hotkey");
            }
        }
    }

    // Register the new hotkey
    if let Err(e) = manager.register(new_hotkey) {
        error!("Failed to register new hotkey {}: {:?}", new_hotkey_str, e);
        return false;
    }

    // Update the stored hotkey info
    let new_id = new_hotkey.id();
    CURRENT_HOTKEY_ID.store(new_id, Ordering::SeqCst);
    *CURRENT_HOTKEY.lock() = Some(new_hotkey);

    info!("Successfully updated hotkey to: {}", new_hotkey_str);
    true
}

/// Enable or disable the hotkey
#[allow(dead_code)]
pub fn set_hotkey_enabled(enabled: bool) {
    HOTKEY_ENABLED.store(enabled, Ordering::SeqCst);
    info!("Hotkey enabled: {}", enabled);
}

/// Parse a hotkey string like "Ctrl+Shift+S" into components
/// Returns (modifiers, key_code) if valid
pub fn parse_hotkey_string(s: &str) -> Option<(Modifiers, Code)> {
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() {
        return None;
    }

    let mut modifiers = Modifiers::empty();
    let mut key_code = None;

    for part in parts {
        match part.to_uppercase().as_str() {
            "CTRL" | "CONTROL" => modifiers |= Modifiers::CONTROL,
            "SHIFT" => modifiers |= Modifiers::SHIFT,
            "ALT" => modifiers |= Modifiers::ALT,
            "WIN" | "SUPER" | "META" => modifiers |= Modifiers::META,
            key => {
                // Parse key code
                key_code = match key {
                    "A" => Some(Code::KeyA),
                    "B" => Some(Code::KeyB),
                    "C" => Some(Code::KeyC),
                    "D" => Some(Code::KeyD),
                    "E" => Some(Code::KeyE),
                    "F" => Some(Code::KeyF),
                    "G" => Some(Code::KeyG),
                    "H" => Some(Code::KeyH),
                    "I" => Some(Code::KeyI),
                    "J" => Some(Code::KeyJ),
                    "K" => Some(Code::KeyK),
                    "L" => Some(Code::KeyL),
                    "M" => Some(Code::KeyM),
                    "N" => Some(Code::KeyN),
                    "O" => Some(Code::KeyO),
                    "P" => Some(Code::KeyP),
                    "Q" => Some(Code::KeyQ),
                    "R" => Some(Code::KeyR),
                    "S" => Some(Code::KeyS),
                    "T" => Some(Code::KeyT),
                    "U" => Some(Code::KeyU),
                    "V" => Some(Code::KeyV),
                    "W" => Some(Code::KeyW),
                    "X" => Some(Code::KeyX),
                    "Y" => Some(Code::KeyY),
                    "Z" => Some(Code::KeyZ),
                    "0" => Some(Code::Digit0),
                    "1" => Some(Code::Digit1),
                    "2" => Some(Code::Digit2),
                    "3" => Some(Code::Digit3),
                    "4" => Some(Code::Digit4),
                    "5" => Some(Code::Digit5),
                    "6" => Some(Code::Digit6),
                    "7" => Some(Code::Digit7),
                    "8" => Some(Code::Digit8),
                    "9" => Some(Code::Digit9),
                    "F1" => Some(Code::F1),
                    "F2" => Some(Code::F2),
                    "F3" => Some(Code::F3),
                    "F4" => Some(Code::F4),
                    "F5" => Some(Code::F5),
                    "F6" => Some(Code::F6),
                    "F7" => Some(Code::F7),
                    "F8" => Some(Code::F8),
                    "F9" => Some(Code::F9),
                    "F10" => Some(Code::F10),
                    "F11" => Some(Code::F11),
                    "F12" => Some(Code::F12),
                    "SPACE" => Some(Code::Space),
                    "TAB" => Some(Code::Tab),
                    "ENTER" | "RETURN" => Some(Code::Enter),
                    "BACKSPACE" => Some(Code::Backspace),
                    "DELETE" => Some(Code::Delete),
                    "INSERT" => Some(Code::Insert),
                    "HOME" => Some(Code::Home),
                    "END" => Some(Code::End),
                    "PAGEUP" => Some(Code::PageUp),
                    "PAGEDOWN" => Some(Code::PageDown),
                    "UP" => Some(Code::ArrowUp),
                    "DOWN" => Some(Code::ArrowDown),
                    "LEFT" => Some(Code::ArrowLeft),
                    "RIGHT" => Some(Code::ArrowRight),
                    "`" | "BACKQUOTE" => Some(Code::Backquote),
                    _ => None,
                };
            }
        }
    }

    key_code.map(|code| (modifiers, code))
}
