//! Global hotkey management for toggling the screenshot window

use crossbeam_channel::Sender;
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use log::{error, info, warn};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::tray::toggle_window;
use crate::AppMessage;

/// Global flag to track if hotkey is enabled at runtime
static HOTKEY_ENABLED: AtomicBool = AtomicBool::new(true);

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
        return None;
    }

    info!("Registered global hotkey: {}", hotkey_str);

    // Handle hotkey events in a background thread
    let hotkey_id = hotkey.id();
    std::thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        loop {
            if let Ok(event) = receiver.recv() {
                if event.id == hotkey_id && event.state == HotKeyState::Pressed {
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
