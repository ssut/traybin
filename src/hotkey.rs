//! Global hotkey management for toggling the screenshot window

use crossbeam_channel::Sender;
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use log::{error, info, warn};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::tray::toggle_window;
use crate::AppMessage;

/// Global flag to track if hotkey is enabled at runtime
static HOTKEY_ENABLED: AtomicBool = AtomicBool::new(true);

/// Current hotkey ID for event matching (0 means accept any)
static CURRENT_HOTKEY_ID: AtomicU32 = AtomicU32::new(0);

/// Flag to accept any hotkey event (used after re-registration)
static ACCEPT_ANY_HOTKEY: AtomicBool = AtomicBool::new(false);

/// Pending hotkey update request
static PENDING_HOTKEY: Mutex<Option<String>> = Mutex::new(None);

/// Initialize global hotkey manager with custom hotkey string
pub fn init_global_hotkey(
    _message_tx: Sender<AppMessage>,
    hotkey_str: &str,
) -> Option<GlobalHotKeyManager> {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to create global hotkey manager: {:?}", e);
            return None;
        }
    };

    // Parse and register the hotkey
    let (modifiers, code) = match parse_hotkey_string(hotkey_str) {
        Some((m, c)) => (m, c),
        None => {
            warn!(
                "Invalid hotkey string '{}', using default Ctrl+Alt+S",
                hotkey_str
            );
            (Modifiers::CONTROL | Modifiers::ALT, Code::KeyS)
        }
    };

    let hotkey = HotKey::new(Some(modifiers), code);

    if let Err(e) = manager.register(hotkey) {
        error!("Failed to register hotkey {}: {:?}", hotkey_str, e);
        return None;
    }

    info!("Registered global hotkey: {}", hotkey_str);
    CURRENT_HOTKEY_ID.store(hotkey.id(), Ordering::SeqCst);

    // Start event listener thread
    std::thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        loop {
            if let Ok(event) = receiver.recv() {
                // Check if this is our hotkey or if we're accepting any
                let current_id = CURRENT_HOTKEY_ID.load(Ordering::SeqCst);
                let accept_any = ACCEPT_ANY_HOTKEY.load(Ordering::SeqCst);

                if event.state == HotKeyState::Pressed && (event.id == current_id || accept_any) {
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

    Some(manager)
}

/// Update the global hotkey to a new key combination
/// Note: Due to thread-safety issues with GlobalHotKeyManager, this requires an app restart
/// to fully take effect. The new hotkey is saved and will be used on next startup.
pub fn update_hotkey(new_hotkey_str: &str) -> bool {
    info!(
        "Hotkey change requested to: {} (requires app restart for full effect)",
        new_hotkey_str
    );

    // Store the pending hotkey
    *PENDING_HOTKEY.lock() = Some(new_hotkey_str.to_string());

    // For now, we'll try to register with a new manager on the main thread
    // This may or may not work depending on the Windows message loop
    match GlobalHotKeyManager::new() {
        Ok(manager) => {
            let (modifiers, code) = match parse_hotkey_string(new_hotkey_str) {
                Some((m, c)) => (m, c),
                None => {
                    error!("Invalid hotkey string: {}", new_hotkey_str);
                    return false;
                }
            };

            let hotkey = HotKey::new(Some(modifiers), code);

            if let Err(e) = manager.register(hotkey) {
                error!("Failed to register new hotkey: {:?}", e);
                return false;
            }

            // Update the ID and accept any hotkey temporarily
            CURRENT_HOTKEY_ID.store(hotkey.id(), Ordering::SeqCst);
            ACCEPT_ANY_HOTKEY.store(true, Ordering::SeqCst);

            info!("New hotkey registered: {}", new_hotkey_str);

            // Keep the manager alive by leaking it (not ideal but works)
            std::mem::forget(manager);

            true
        }
        Err(e) => {
            error!("Failed to create hotkey manager: {:?}", e);
            false
        }
    }
}

/// Enable or disable the hotkey
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
