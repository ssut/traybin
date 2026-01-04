// Hide console window by default (tray app), unless --console flag is used
#![windows_subsystem = "windows"]
#![recursion_limit = "256"]

mod app;
mod clipboard;
mod convert;
mod drag_drop;
mod hotkey;
mod organizer;
mod settings;
mod thumbnail;
mod tray;
mod ui;
mod watcher;

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use gpui::*;
use log::{error, info, warn};
use parking_lot::Mutex;
use single_instance::SingleInstance;
use std::path::PathBuf;
use std::sync::Arc;

use crate::app::TrayBin;
use crate::hotkey::init_global_hotkey;
use crate::settings::Settings;
use crate::tray::TrayManager;
use crate::watcher::ScreenshotWatcher;

/// Allocate a console window for debugging output (Windows only)
#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS};
    
    unsafe {
        // Try to attach to parent console first (when run from cmd/powershell)
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            // If no parent console, allocate a new one
            let _ = AllocConsole();
        }
    }
}

#[cfg(not(windows))]
fn attach_console() {
    // No-op on non-Windows platforms
}

/// Messages sent from background threads to the UI
#[derive(Debug, Clone)]
pub enum AppMessage {
    /// New screenshot detected
    NewScreenshot(PathBuf),
    /// Screenshot removed
    ScreenshotRemoved(PathBuf),
    /// Toggle window visibility (from tray click)
    ToggleWindow,
    /// Show main window (not settings) from tray icon click
    ShowMainWindow,
    /// Open settings
    OpenSettings,
    /// Change screenshot directory
    ChangeDirectory(PathBuf),
    /// Request latest screenshot path (for tray drag)
    RequestLatestScreenshot,
    /// Organization started with total file count
    OrganizeStarted(usize),
    /// Organization progress update (current, total, current_file)
    OrganizeProgress(usize, usize, String),
    /// Organization completed
    OrganizeCompleted,
    /// Conversion started with total file count
    ConvertStarted(usize),
    /// Conversion progress update (current, total, current_file)
    ConvertProgress(usize, usize, String),
    /// Conversion completed
    ConvertCompleted,
    /// Quit application
    Quit,
}

/// Shared latest screenshot path for tray icon drag
pub static LATEST_SCREENSHOT: parking_lot::Mutex<Option<PathBuf>> = parking_lot::Mutex::new(None);

/// Set the latest screenshot path
pub fn set_latest_screenshot(path: Option<PathBuf>) {
    *LATEST_SCREENSHOT.lock() = path;
}

/// Get the latest screenshot path
pub fn get_latest_screenshot() -> Option<PathBuf> {
    LATEST_SCREENSHOT.lock().clone()
}

/// Global application state shared across threads
pub struct AppState {
    pub settings: Arc<Mutex<Settings>>,
    pub message_tx: Sender<AppMessage>,
    pub message_rx: Receiver<AppMessage>,
    pub tray_manager: Arc<Mutex<Option<TrayManager>>>,
}

impl Global for AppState {}

fn main() -> Result<()> {
    // Check for --console flag to enable debug console
    let args: Vec<String> = std::env::args().collect();
    let console_mode = args.iter().any(|arg| arg == "--console" || arg == "-c");
    
    if console_mode {
        attach_console();
    }
    
    // Initialize logging - write to file in console mode for easier debugging
    let log_level = if console_mode { "debug" } else { "info" };
    
    if console_mode {
        // Log to file for easier reading
        let log_file = std::fs::File::create("traybin_debug.log").expect("Failed to create log file");
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level))
            .target(env_logger::Target::Pipe(Box::new(log_file)))
            .init();
        println!("=== TrayBin Debug Console ===");
        println!("Logging to: traybin_debug.log");
        println!("Logging level: {}", log_level);
    } else {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level)).init();
    }
    
    info!("Starting Traybin...");

    // Single instance check - prevent multiple copies from running
    let instance = SingleInstance::new("traybin-screenshot-manager").unwrap();
    if !instance.is_single() {
        warn!("Another instance of Traybin is already running");
        return Ok(());
    }
    info!("Single instance check passed");

    // Load settings
    let settings = Settings::load().unwrap_or_default();
    let screenshot_dir = settings.screenshot_directory.clone();
    let window_width = settings.window_width;
    let window_height = settings.window_height;
    
    // Wrap settings in Arc<Mutex> for sharing across threads
    let settings = Arc::new(Mutex::new(settings));

    // Create message channels
    let (message_tx, message_rx) = unbounded::<AppMessage>();

    // Initialize OLE for Windows APIs (required for drag-drop)
    // OleInitialize is required instead of CoInitializeEx for DoDragDrop to work
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Ole::OleInitialize;
        let result = OleInitialize(None);
        if result.is_err() {
            warn!("OleInitialize failed: {:?}", result);
        } else {
            info!("OLE initialized successfully");
        }
    }

    // Create tray icon before starting gpui
    let tray_message_tx = message_tx.clone();
    let tray_manager = TrayManager::new(tray_message_tx)?;

    // Initialize global hotkey with custom setting
    let hotkey_message_tx = message_tx.clone();
    let (hotkey_str, hotkey_enabled) = {
        let s = settings.lock();
        (s.hotkey.clone(), s.hotkey_enabled)
    };
    if hotkey_enabled {
        if !init_global_hotkey(hotkey_message_tx, &hotkey_str) {
            warn!("Failed to initialize global hotkey");
        }
    } else {
        info!("Global hotkey disabled in settings");
    }

    // Start file watcher in background thread
    let watcher_tx = message_tx.clone();
    let watcher_dir = screenshot_dir.clone();
    let watcher_settings = Arc::clone(&settings);
    std::thread::spawn(move || {
        if let Err(e) = ScreenshotWatcher::new(watcher_dir, watcher_tx, watcher_settings).run() {
            error!("File watcher error: {}", e);
        }
    });

    // Run the GPUI application
    let app = Application::new();

    app.run(move |cx: &mut App| {
        // Initialize gpui-component
        gpui_component::init(cx);

        // Store app state globally
        cx.set_global(AppState {
            settings: Arc::clone(&settings),
            message_tx,
            message_rx,
            tray_manager: Arc::new(Mutex::new(Some(tray_manager))),
        });

        // Open main window - popup style (no taskbar, no titlebar)
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(window_width), px(window_height)),
                cx,
            ))),
            // No titlebar for popup-style window
            titlebar: None,
            focus: true,
            show: true,
            // Make it a popup-style window (no taskbar entry)
            kind: WindowKind::PopUp,
            // Enable dragging for borderless popup windows on Windows
            is_movable: true,
            ..Default::default()
        };

        let _window_handle = cx
            .open_window(window_options, |window, cx| {
                // Set dark mode theme for a richer visual experience
                gpui_component::theme::Theme::change(
                    gpui_component::theme::ThemeMode::Dark,
                    Some(window),
                    cx,
                );

                // Get HWND and store it for tray operations
                #[cfg(windows)]
                {
                    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                    if let Ok(handle) = window.window_handle() {
                        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                            let hwnd_value = win32.hwnd.get() as isize;
                            tray::set_window_hwnd(hwnd_value);
                            info!("Window HWND captured: {}", hwnd_value);
                        }
                    }
                }

                let view = cx.new(|cx| TrayBin::new(window, cx));
                cx.new(|cx| gpui_component::Root::new(view, window, cx))
            })
            .expect("Failed to open window");

        // Don't quit when last window closes - we're a tray app
        let _ = cx.on_app_quit(|_cx| async {
            info!("App quit requested");
        });

        info!("Traybin started successfully");
    });

    info!("Traybin shutting down...");
    Ok(())
}
