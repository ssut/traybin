//! System tray icon management

use anyhow::Result;
use crossbeam_channel::Sender;
use log::{debug, info};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use crate::AppMessage;

/// Track if left mouse button is pressed on tray
static TRAY_MOUSE_DOWN: AtomicBool = AtomicBool::new(false);

/// Store initial mouse position for drag detection
static TRAY_DRAG_START: Mutex<Option<(f64, f64)>> = Mutex::new(None);

/// Drag threshold in pixels
const DRAG_THRESHOLD: f64 = 5.0;

/// Shared state for window handle
pub static WINDOW_HWND: Mutex<Option<isize>> = Mutex::new(None);

/// Track window visibility
static WINDOW_VISIBLE: AtomicBool = AtomicBool::new(true);

/// Set the window handle for tray operations
pub fn set_window_hwnd(hwnd: isize) {
    *WINDOW_HWND.lock() = Some(hwnd);
}

/// Check if our window is currently the foreground (focused) window
#[cfg(windows)]
pub fn is_window_focused() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    if let Some(hwnd) = *WINDOW_HWND.lock() {
        unsafe {
            let foreground = GetForegroundWindow();
            foreground.0 as isize == hwnd
        }
    } else {
        false
    }
}

#[cfg(not(windows))]
pub fn is_window_focused() -> bool {
    false
}

/// Check if window is visible
pub fn is_window_visible() -> bool {
    WINDOW_VISIBLE.load(Ordering::SeqCst)
}

/// Hide the window
#[cfg(windows)]
pub fn hide_window() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    if let Some(hwnd) = *WINDOW_HWND.lock() {
        unsafe {
            let hwnd = HWND(hwnd as *mut std::ffi::c_void);
            let _ = ShowWindow(hwnd, SW_HIDE);
            WINDOW_VISIBLE.store(false, Ordering::SeqCst);
            info!("Window hidden");
        }
    }
}

#[cfg(not(windows))]
pub fn hide_window() {
    // Not implemented for non-Windows
}

/// Move window to the monitor where the cursor is located
#[cfg(windows)]
fn move_window_to_cursor_monitor() {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowRect, SetWindowPos, HWND_TOP, SWP_NOSIZE, SWP_NOZORDER,
    };

    if let Some(hwnd) = *WINDOW_HWND.lock() {
        unsafe {
            let hwnd = HWND(hwnd as *mut std::ffi::c_void);

            // Get cursor position
            let mut cursor_pos = POINT::default();
            if GetCursorPos(&mut cursor_pos).is_err() {
                return;
            }

            // Get monitor at cursor position
            let monitor = MonitorFromPoint(cursor_pos, MONITOR_DEFAULTTONEAREST);

            // Get monitor info
            let mut monitor_info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
                return;
            }

            // Get current window rect
            let mut window_rect = RECT::default();
            if GetWindowRect(hwnd, &mut window_rect).is_err() {
                return;
            }

            let window_width = window_rect.right - window_rect.left;
            let window_height = window_rect.bottom - window_rect.top;

            // Calculate centered position on the monitor
            let monitor_work = monitor_info.rcWork;
            let monitor_width = monitor_work.right - monitor_work.left;
            let monitor_height = monitor_work.bottom - monitor_work.top;

            let new_x = monitor_work.left + (monitor_width - window_width) / 2;
            let new_y = monitor_work.top + (monitor_height - window_height) / 2;

            // Move window to new position
            let _ = SetWindowPos(
                hwnd,
                HWND_TOP,
                new_x,
                new_y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER,
            );
            debug!(
                "Moved window to monitor at cursor position ({}, {})",
                new_x, new_y
            );
        }
    }
}

/// Show and activate the window using Windows API
#[cfg(windows)]
pub fn show_window() {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };

    // First move window to cursor's monitor
    move_window_to_cursor_monitor();

    if let Some(hwnd) = *WINDOW_HWND.lock() {
        unsafe {
            let hwnd = HWND(hwnd as *mut std::ffi::c_void);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            WINDOW_VISIBLE.store(true, Ordering::SeqCst);
            info!("Window shown and focused");
        }
    }
}

#[cfg(not(windows))]
pub fn show_window() {
    // Not implemented for non-Windows
}

/// Toggle window visibility - hide if focused, show if not
/// Returns true if window was shown, false if hidden
#[cfg(windows)]
pub fn toggle_window() -> bool {
    if is_window_focused() && is_window_visible() {
        info!("Window is focused, hiding");
        hide_window();
        false
    } else {
        info!("Window is not focused or hidden, showing");
        show_window();
        true
    }
}

#[cfg(not(windows))]
pub fn toggle_window() -> bool {
    show_window();
    true
}

pub struct TrayManager {
    _tray_icon: TrayIcon,
}

impl TrayManager {
    pub fn new(message_tx: Sender<AppMessage>) -> Result<Self> {
        info!("Creating tray icon...");

        let menu = Menu::new();
        let settings_item = MenuItem::new("Settings", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        menu.append_items(&[&settings_item, &PredefinedMenuItem::separator(), &quit_item])?;

        let icon = Self::generate_camera_icon()?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Traybin - Screenshot Manager")
            .with_icon(icon)
            .with_menu_on_left_click(false)
            .build()?;

        let menu_tx = message_tx.clone();
        let settings_id = settings_item.id().clone();
        let quit_id = quit_item.id().clone();

        std::thread::spawn(move || {
            let menu_receiver = MenuEvent::receiver();
            loop {
                if let Ok(event) = menu_receiver.recv() {
                    if event.id == settings_id {
                        show_window();
                        let _ = menu_tx.send(AppMessage::OpenSettings);
                    } else if event.id == quit_id {
                        info!("Quit requested from tray menu");
                        std::process::exit(0);
                    }
                }
            }
        });

        let click_tx = message_tx.clone();
        std::thread::spawn(move || {
            let tray_receiver = TrayIconEvent::receiver();
            loop {
                if let Ok(event) = tray_receiver.recv() {
                    match event {
                        TrayIconEvent::Click {
                            button: tray_icon::MouseButton::Left,
                            button_state: tray_icon::MouseButtonState::Down,
                            position,
                            ..
                        } => {
                            *TRAY_DRAG_START.lock() = Some((position.x, position.y));
                            TRAY_MOUSE_DOWN.store(true, Ordering::SeqCst);
                        }
                        TrayIconEvent::Click {
                            button: tray_icon::MouseButton::Left,
                            button_state: tray_icon::MouseButtonState::Up,
                            ..
                        } => {
                            if TRAY_MOUSE_DOWN.load(Ordering::SeqCst) {
                                TRAY_MOUSE_DOWN.store(false, Ordering::SeqCst);
                                *TRAY_DRAG_START.lock() = None;
                                let was_shown = toggle_window();
                                // If window was shown, send message to reset to main view
                                if was_shown {
                                    let _ = click_tx.send(AppMessage::ShowMainWindow);
                                }
                            }
                        }
                        TrayIconEvent::Move { position, .. } => {
                            if TRAY_MOUSE_DOWN.load(Ordering::SeqCst) {
                                if let Some((start_x, start_y)) = *TRAY_DRAG_START.lock() {
                                    let dx = position.x - start_x;
                                    let dy = position.y - start_y;
                                    let distance = (dx * dx + dy * dy).sqrt();

                                    if distance > DRAG_THRESHOLD {
                                        TRAY_MOUSE_DOWN.store(false, Ordering::SeqCst);
                                        *TRAY_DRAG_START.lock() = None;

                                        if let Some(latest_path) = crate::get_latest_screenshot() {
                                            info!("Starting tray drag with: {:?}", latest_path);
                                            crate::drag_drop::start_drag(&[latest_path]);
                                        } else {
                                            debug!("No screenshots available for tray drag");
                                        }
                                    }
                                }
                            }
                        }
                        TrayIconEvent::Leave { .. } => {
                            if TRAY_MOUSE_DOWN.load(Ordering::SeqCst) {
                                TRAY_MOUSE_DOWN.store(false, Ordering::SeqCst);
                                *TRAY_DRAG_START.lock() = None;

                                if let Some(latest_path) = crate::get_latest_screenshot() {
                                    info!("Starting tray drag (leave) with: {:?}", latest_path);
                                    crate::drag_drop::start_drag(&[latest_path]);
                                }
                            }
                        }
                        TrayIconEvent::DoubleClick {
                            button: tray_icon::MouseButton::Left,
                            ..
                        } => {
                            TRAY_MOUSE_DOWN.store(false, Ordering::SeqCst);
                            *TRAY_DRAG_START.lock() = None;
                            show_window();
                            let _ = click_tx.send(AppMessage::ToggleWindow);
                        }
                        _ => {}
                    }
                }
            }
        });

        info!("Tray icon created successfully");
        Ok(Self {
            _tray_icon: tray_icon,
        })
    }

    fn generate_camera_icon() -> Result<Icon> {
        let size = 32u32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];

        for y in 0..size {
            for x in 0..size {
                let idx = ((y * size + x) * 4) as usize;
                let fx = x as f32 / size as f32;
                let fy = y as f32 / size as f32;

                let in_body = fx > 0.1 && fx < 0.9 && fy > 0.25 && fy < 0.85;
                let cx = 0.5;
                let cy = 0.55;
                let r = 0.22;
                let dist = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                let in_lens = dist < r;
                let in_lens_inner = dist < r * 0.6;
                let in_flash = fx > 0.6 && fx < 0.8 && fy > 0.12 && fy < 0.28;

                if in_lens_inner {
                    rgba[idx] = 100;
                    rgba[idx + 1] = 180;
                    rgba[idx + 2] = 255;
                    rgba[idx + 3] = 255;
                } else if in_lens {
                    rgba[idx] = 40;
                    rgba[idx + 1] = 40;
                    rgba[idx + 2] = 50;
                    rgba[idx + 3] = 255;
                } else if in_body || in_flash {
                    rgba[idx] = 60;
                    rgba[idx + 1] = 60;
                    rgba[idx + 2] = 70;
                    rgba[idx + 3] = 255;
                } else {
                    rgba[idx] = 0;
                    rgba[idx + 1] = 0;
                    rgba[idx + 2] = 0;
                    rgba[idx + 3] = 0;
                }
            }
        }

        Icon::from_rgba(rgba, size, size)
            .map_err(|e| anyhow::anyhow!("Failed to create generated icon: {}", e))
    }
}
