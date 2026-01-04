//! Main application state and UI

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::WindowExt;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::notification::{Notification, NotificationType};
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use log::{error, info};
use rust_i18n::t;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// Settings page tabs
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SettingsPage {
    #[default]
    General,
    Conversion,
    Indexing,
    Hotkey,
    About,
}

use crate::clipboard;
use crate::convert;
use crate::organizer;
use crate::settings::ConversionFormat;
use crate::thumbnail::ThumbnailCache;
use crate::ui::gallery;
use crate::{AppMessage, AppState, set_latest_screenshot};
use fastembed;

/// App version
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// App name
const APP_NAME: &str = "Sukusho";

/// Global prewarmed text embedding model (single shared instance for all searches)
static PREWARMED_TEXT_MODEL: parking_lot::Mutex<Option<Arc<Mutex<fastembed::TextEmbedding>>>> =
    parking_lot::Mutex::new(None);

/// Global prewarmed vision embedding model (single shared instance for all indexing)
static PREWARMED_VISION_MODEL: parking_lot::Mutex<Option<Arc<Mutex<fastembed::ImageEmbedding>>>> =
    parking_lot::Mutex::new(None);

/// Start native window drag using Windows API
#[cfg(windows)]
fn start_window_drag(_window: &mut Window) {
    use crate::tray::WINDOW_HWND;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{HTCAPTION, PostMessageW, WM_NCLBUTTONDOWN};

    if let Some(hwnd) = *WINDOW_HWND.lock() {
        unsafe {
            // Release mouse capture first
            let _ = ReleaseCapture();
            // Post message to start window drag (asynchronous to avoid RefCell conflicts)
            let _ = PostMessageW(
                HWND(hwnd as *mut std::ffi::c_void),
                WM_NCLBUTTONDOWN,
                windows::Win32::Foundation::WPARAM(HTCAPTION as usize),
                windows::Win32::Foundation::LPARAM(0),
            );
        }
    }
}

#[cfg(not(windows))]
fn start_window_drag(_window: &mut Window) {
    // Not implemented for non-Windows
}

/// Open Windows folder picker dialog
#[cfg(windows)]
pub fn pick_folder() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        FOS_PICKFOLDERS, FileOpenDialog, IFileDialog, IShellItem, SIGDN_FILESYSPATH,
    };
    use windows::core::PWSTR;

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let dialog: IFileDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;

        // Set options to pick folders
        let mut options = dialog.GetOptions().ok()?;
        options |= FOS_PICKFOLDERS;
        dialog.SetOptions(options).ok()?;

        // Show dialog
        if dialog.Show(None).is_err() {
            CoUninitialize();
            return None;
        }

        // Get result
        let result: IShellItem = dialog.GetResult().ok()?;
        let path_ptr: PWSTR = result.GetDisplayName(SIGDN_FILESYSPATH).ok()?;

        // Convert to PathBuf
        let len = (0..).take_while(|&i| *path_ptr.0.add(i) != 0).count();
        let slice = std::slice::from_raw_parts(path_ptr.0, len);
        let path = PathBuf::from(OsString::from_wide(slice));

        windows::Win32::System::Com::CoTaskMemFree(Some(path_ptr.0 as *const _));
        CoUninitialize();

        Some(path)
    }
}

#[cfg(not(windows))]
pub fn pick_folder() -> Option<PathBuf> {
    None
}

/// Number of items to load per page
const PAGE_SIZE: usize = 50;

/// Screenshot metadata
#[derive(Debug, Clone)]
pub struct ScreenshotInfo {
    pub path: PathBuf,
    #[allow(dead_code)]
    pub filename: String,
    pub modified: SystemTime,
    pub file_size: u64,
    /// File extension (uppercase, e.g., "PNG", "WEBP", "JPEG")
    pub extension: String,
}

impl ScreenshotInfo {
    pub fn from_path(path: PathBuf) -> Option<Self> {
        let metadata = std::fs::metadata(&path).ok()?;
        let filename = path.file_name()?.to_string_lossy().to_string();
        let modified = metadata.modified().ok()?;
        let file_size = metadata.len();
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_uppercase())
            .unwrap_or_default();

        Some(Self {
            path,
            filename,
            modified,
            file_size,
            extension,
        })
    }
}

/// Format file size in human readable format (using IEC binary units)
pub fn format_file_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;

    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{} B", bytes)
    }
}

/// Click action from gallery items
#[derive(Debug, Clone)]
pub enum GalleryAction {
    /// Single click - select item
    Select { path: PathBuf, modifiers: Modifiers },
    /// Double click - open with default app
    Open(PathBuf),
    /// Right click - show context menu (includes all selected paths)
    ContextMenu {
        paths: Vec<PathBuf>,
        position: Point<Pixels>,
    },
    /// Start drag operation
    #[allow(dead_code)]
    StartDrag(Vec<PathBuf>),
    /// Load more items (infinite scroll)
    LoadMore,
    /// Clear all selections (when clicking blank space)
    ClearSelection,
}

/// Main application view
pub struct Sukusho {
    /// All screenshot paths (sorted by modification time, newest first)
    all_screenshots: Vec<ScreenshotInfo>,

    /// Currently visible screenshots (paginated)
    visible_count: usize,

    /// Selected screenshot paths
    selected: HashSet<PathBuf>,

    /// Last selected item for shift-click range selection
    last_selected: Option<PathBuf>,

    /// Thumbnail cache
    thumbnail_cache: Arc<ThumbnailCache>,

    /// Whether settings panel is open
    settings_open: bool,

    /// Current settings page
    settings_page: SettingsPage,

    /// Current grid columns
    grid_columns: u32,

    /// Current thumbnail size
    thumbnail_size: u32,

    /// Focus handle for keyboard events
    focus_handle: FocusHandle,

    /// Search input state
    search_input: Entity<InputState>,

    /// Whether search input has focus
    search_input_focused: bool,

    /// Whether we're recording a new hotkey
    recording_hotkey: bool,

    /// Whether we're currently organizing files
    organizing: bool,

    /// Organization progress (current, total)
    organize_progress: (usize, usize),

    /// Current file being organized
    organize_current_file: String,

    /// Whether we're currently converting files
    converting: bool,

    /// Conversion progress (current, total)
    convert_progress: (usize, usize),

    /// Current file being converted
    convert_current_file: String,

    /// Whether we're currently downloading models
    downloading_models: bool,

    /// Model download progress (current, total)
    model_download_progress: (usize, usize),

    /// Whether models have been downloaded
    models_downloaded: bool,

    /// Whether we're currently indexing files
    indexing: bool,

    /// Indexing progress (current, total)
    index_progress: (usize, usize),

    /// Current file being indexed
    index_current_file: String,

    /// Search query
    search_query: String,

    /// Search results (None = show all, Some = filtered)
    search_results: Option<Vec<PathBuf>>,

    /// Index statistics
    #[allow(dead_code)]
    index_stats: crate::indexer::IndexStats,

    /// Toast notification manager
    toast_manager: crate::ui::ToastManager,
}

impl Sukusho {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app_state = cx.global::<AppState>();
        let settings = app_state.settings.lock().clone();

        // Create search input state
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(&t!("app.search.placeholder").to_string())
        });

        // Subscribe to search input events
        cx.subscribe_in(&search_input, window, |this, state, event, _window, cx| {
            match event {
                InputEvent::Focus => {
                    this.search_input_focused = true;
                }
                InputEvent::Blur => {
                    this.search_input_focused = false;
                }
                InputEvent::Change => {
                    // Use the state parameter directly (no RefCell borrow of this.search_input)
                    let text = state.read(cx).value().to_string();
                    this.search_query = text.clone();

                    // Clear search results if query is empty
                    if text.is_empty() {
                        this.search_results = None;
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    // Use the state parameter directly (no RefCell borrow of this.search_input)
                    let query = state.read(cx).value().to_string();
                    if !query.is_empty() {
                        info!("Starting search for: {}", query);

                        // Get message channel and config
                        let tx = {
                            let app_state = cx.global::<AppState>();
                            app_state.message_tx.clone()
                        };
                        let config = {
                            let app_state = cx.global::<AppState>();
                            let settings = app_state.settings.lock();
                            let db_path = crate::settings::Settings::config_path()
                                .unwrap()
                                .parent()
                                .unwrap()
                                .join("vector_index.db");
                            crate::indexer::IndexConfig {
                                db_path,
                                cpu_mode: if settings.indexing_cpu_mode == "fast" {
                                    crate::indexer::CpuMode::Fast
                                } else {
                                    crate::indexer::CpuMode::Normal
                                },
                                screenshot_dir: settings.screenshot_directory.clone(),
                            }
                        };

                        // Use prewarmed model if available, otherwise load fresh
                        if let Some(text_model) = PREWARMED_TEXT_MODEL.lock().clone() {
                            info!("Using prewarmed model for search");
                            crate::indexer::search_images(
                                query.to_string(),
                                config,
                                text_model,
                                tx,
                                100,
                            );
                        } else {
                            info!("Loading model for search (not prewarmed)");
                            // Load text model and perform search in background
                            std::thread::spawn(move || {
                                let cache_dir = crate::settings::Settings::config_path()
                                    .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                                    .unwrap_or_else(|| PathBuf::from("."));
                                let cache_dir = cache_dir.join(".fastembed_cache");

                                match fastembed::TextEmbedding::try_new(
                                    fastembed::InitOptions::new(
                                        fastembed::EmbeddingModel::NomicEmbedTextV15,
                                    )
                                    .with_cache_dir(cache_dir)
                                    .with_show_download_progress(false),
                                ) {
                                    Ok(model) => {
                                        let text_model = Arc::new(Mutex::new(model));
                                        crate::indexer::search_images(
                                            query.to_string(),
                                            config,
                                            text_model,
                                            tx,
                                            100,
                                        );
                                    }
                                    Err(e) => {
                                        error!("Failed to load text model for search: {}", e);
                                    }
                                }
                            });
                        }
                    }
                }
            }
        })
        .detach();

        let app = Self {
            all_screenshots: Vec::new(),
            visible_count: PAGE_SIZE,
            selected: HashSet::new(),
            last_selected: None,
            thumbnail_cache: Arc::new(ThumbnailCache::new(500)),
            settings_open: false,
            settings_page: SettingsPage::default(),
            grid_columns: settings.grid_columns,
            thumbnail_size: settings.thumbnail_size,
            focus_handle: cx.focus_handle(),
            search_input,
            search_input_focused: false,
            recording_hotkey: false,
            organizing: false,
            organize_progress: (0, 0),
            organize_current_file: String::new(),
            converting: false,
            convert_progress: (0, 0),
            convert_current_file: String::new(),
            downloading_models: false,
            model_download_progress: (0, 0),
            models_downloaded: settings.models_downloaded,
            indexing: false,
            index_progress: (0, 0),
            index_current_file: String::new(),
            search_query: String::new(),
            search_results: None,
            index_stats: crate::indexer::IndexStats::default(),
            toast_manager: crate::ui::ToastManager::new(),
        };

        // Prewarm models if indexing is enabled (creates SINGLE shared model instances)
        if settings.indexing_enabled && settings.models_downloaded {
            info!("Prewarming embedding models (single shared instances)...");
            let cache_dir = crate::settings::Settings::config_path()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."));
            let cache_dir = cache_dir.join(".fastembed_cache");

            // Load models in background thread (blocking operation)
            // The models are wrapped in Arc<Mutex<>> so they can be shared across threads
            std::thread::spawn(move || {
                info!("Loading vision embedding model in background...");
                match fastembed::ImageEmbedding::try_new(
                    fastembed::ImageInitOptions::new(
                        fastembed::ImageEmbeddingModel::NomicEmbedVisionV15,
                    )
                    .with_cache_dir(cache_dir.clone())
                    .with_show_download_progress(false),
                ) {
                    Ok(model) => {
                        info!("Vision embedding model loaded successfully - setting global state");
                        // Create a SINGLE Arc<Mutex<>> wrapped model that will be shared
                        let vision_model = Arc::new(Mutex::new(model));
                        // Store in global static for access from indexing function
                        *PREWARMED_VISION_MODEL.lock() = Some(vision_model);
                        info!("Vision model prewarmed and ready for indexing");
                    }
                    Err(e) => {
                        error!("Failed to prewarm vision embedding model: {}", e);
                    }
                }

                info!("Loading text embedding model in background...");
                match fastembed::TextEmbedding::try_new(
                    fastembed::InitOptions::new(fastembed::EmbeddingModel::NomicEmbedTextV15)
                        .with_cache_dir(cache_dir)
                        .with_show_download_progress(false),
                ) {
                    Ok(model) => {
                        info!("Text embedding model loaded successfully - setting global state");
                        // Create a SINGLE Arc<Mutex<>> wrapped model that will be shared
                        let text_model = Arc::new(Mutex::new(model));
                        // Store in global static for access from search function
                        *PREWARMED_TEXT_MODEL.lock() = Some(text_model);
                        info!("Text model prewarmed and ready for search");
                    }
                    Err(e) => {
                        error!("Failed to prewarm text embedding model: {}", e);
                    }
                }
            });
        }

        app
    }

    /// Convert a keystroke to a hotkey string
    fn keystroke_to_hotkey_string(keystroke: &Keystroke) -> Option<String> {
        let mut parts = Vec::new();

        if keystroke.modifiers.control {
            parts.push("Ctrl");
        }
        if keystroke.modifiers.shift {
            parts.push("Shift");
        }
        if keystroke.modifiers.alt {
            parts.push("Alt");
        }
        if keystroke.modifiers.platform {
            parts.push("Win");
        }

        // Get the key name
        let key = keystroke.key.as_str();

        // Skip if only modifier keys are pressed
        let is_modifier_only = matches!(key, "control" | "shift" | "alt" | "meta" | "super" | "");

        if is_modifier_only {
            return None;
        }

        // Convert key to display format
        let key_display = match key {
            "space" => "Space",
            "tab" => "Tab",
            "enter" => "Enter",
            "backspace" => "Backspace",
            "delete" => "Delete",
            "insert" => "Insert",
            "home" => "Home",
            "end" => "End",
            "pageup" => "PageUp",
            "pagedown" => "PageDown",
            "up" => "Up",
            "down" => "Down",
            "left" => "Left",
            "right" => "Right",
            "`" => "`",
            k if k.starts_with('f') && k.len() <= 3 => k, // F1-F12
            k if k.len() == 1 => k,                       // Single char keys
            _ => return None,                             // Unknown key
        };

        parts.push(key_display);

        if parts.len() < 2 {
            // Require at least one modifier + key, or just function keys
            let key_upper = key_display.to_uppercase();
            if !key_upper.starts_with('F') || key_upper.len() > 3 {
                return None;
            }
        }

        Some(
            parts
                .join("+")
                .to_uppercase()
                .replace("CTRL", "Ctrl")
                .replace("SHIFT", "Shift")
                .replace("ALT", "Alt")
                .replace("WIN", "Win"),
        )
    }

    /// Maximum messages to process per render cycle (prevents UI blocking)
    const MAX_MESSAGES_PER_FRAME: usize = 20;

    /// Process incoming messages from background threads
    fn process_messages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Update toast manager to remove expired toasts
        self.toast_manager.update();

        // Collect messages up to limit to avoid blocking UI
        let messages: Vec<AppMessage> = {
            let app_state = cx.global::<AppState>();
            let mut msgs = Vec::new();
            while msgs.len() < Self::MAX_MESSAGES_PER_FRAME {
                match app_state.message_rx.try_recv() {
                    Ok(msg) => msgs.push(msg),
                    Err(_) => break,
                }
            }
            msgs
        };

        // If there are more messages pending, schedule another render
        let has_more = {
            let app_state = cx.global::<AppState>();
            !app_state.message_rx.is_empty()
        };

        // Now process collected messages
        for msg in messages {
            match msg {
                AppMessage::NewScreenshot(path, should_auto_index) => {
                    self.add_screenshot(path, should_auto_index, cx);
                }
                AppMessage::ScreenshotRemoved(path) => {
                    self.remove_screenshot(&path, cx);
                }
                AppMessage::ToggleWindow => {
                    info!("Toggle window requested - activating window");
                    window.activate_window();
                    cx.notify();
                }
                AppMessage::ShowMainWindow => {
                    info!("Show main window requested - closing settings if open");
                    self.settings_open = false;
                    cx.notify();
                }
                AppMessage::OpenSettings => {
                    self.settings_open = true;
                    cx.notify();
                }
                AppMessage::ChangeDirectory(new_dir) => {
                    info!("Changing screenshot directory to: {:?}", new_dir);
                    // Update settings
                    {
                        let app_state = cx.global::<AppState>();
                        let mut settings = app_state.settings.lock();
                        settings.screenshot_directory = new_dir;
                        let _ = settings.save();
                    }
                    // Clear current screenshots and reload
                    self.all_screenshots.clear();
                    self.selected.clear();
                    self.visible_count = PAGE_SIZE;
                    // Note: Would need to restart watcher for new directory
                    // For now, user needs to restart app
                    cx.notify();
                }
                AppMessage::Quit => {
                    info!("Quit requested");
                    cx.quit();
                }
                AppMessage::RequestLatestScreenshot => {
                    // Update the latest screenshot from current state
                    if let Some(latest) = self.all_screenshots.first() {
                        set_latest_screenshot(Some(latest.path.clone()));
                    }
                }
                AppMessage::OrganizeStarted(total) => {
                    info!("Organization started: {} files", total);
                    self.organizing = true;
                    self.organize_progress = (0, total);
                    self.organize_current_file = String::new();
                    cx.notify();
                }
                AppMessage::OrganizeProgress(current, total, file) => {
                    self.organize_progress = (current, total);
                    self.organize_current_file = file;
                    cx.notify();
                }
                AppMessage::OrganizeCompleted => {
                    info!("Organization completed");
                    self.organizing = false;
                    self.organize_progress = (0, 0);
                    self.organize_current_file = String::new();
                    cx.notify();
                }
                AppMessage::ConvertStarted(total) => {
                    info!("Conversion started: {} files", total);
                    self.converting = true;
                    self.convert_progress = (0, total);
                    self.convert_current_file = String::new();
                    cx.notify();
                }
                AppMessage::ConvertProgress(current, total, file) => {
                    self.convert_progress = (current, total);
                    self.convert_current_file = file;
                    cx.notify();
                }
                AppMessage::ConvertCompleted => {
                    info!("Conversion completed");
                    self.converting = false;
                    self.convert_progress = (0, 0);
                    self.convert_current_file = String::new();
                    cx.notify();
                }
                AppMessage::ModelDownloadProgress(current, total, model) => {
                    info!("Model download progress: {}/{} ({})", current, total, model);
                    self.downloading_models = true;
                    self.model_download_progress = (current, total);
                    cx.notify();
                }
                AppMessage::ModelDownloadCompleted => {
                    info!("Model download completed");
                    self.downloading_models = false;
                    self.models_downloaded = true;

                    // Text model will be loaded on-demand when search is triggered

                    // Save to settings
                    {
                        let app_state = cx.global::<AppState>();
                        let mut settings = app_state.settings.lock();
                        settings.models_downloaded = true;
                        let _ = settings.save();
                    }

                    // Show notification
                    window.push_notification(
                        Notification::new()
                            .message(&t!("notifications.models.download_success").to_string())
                            .with_type(NotificationType::Success),
                        cx,
                    );

                    cx.notify();
                }
                AppMessage::ModelDownloadFailed(error) => {
                    error!("Model download failed: {}", error);
                    self.downloading_models = false;

                    // Auto-disable indexing
                    {
                        let app_state = cx.global::<AppState>();
                        let mut settings = app_state.settings.lock();
                        settings.indexing_enabled = false;
                        let _ = settings.save();
                    }

                    // Show error notification
                    window.push_notification(
                        Notification::new()
                            .message(&t!("notifications.models.download_failed", error = error).to_string())
                            .with_type(NotificationType::Error),
                        cx,
                    );

                    cx.notify();
                }
                AppMessage::IndexStarted(total) => {
                    info!("Indexing started: {} files", total);
                    self.indexing = true;
                    self.index_progress = (0, total);
                    self.index_current_file = String::new();
                    cx.notify();
                }
                AppMessage::IndexProgress(current, total, file) => {
                    self.index_progress = (current, total);
                    self.index_current_file = file;
                    cx.notify();
                }
                AppMessage::IndexCompleted(newly_indexed_count) => {
                    info!(
                        "Indexing completed: {} new images indexed",
                        newly_indexed_count
                    );
                    self.indexing = false;
                    self.index_progress = (0, 0);
                    self.index_current_file = String::new();

                    // Query database for actual total indexed count
                    let (screenshot_dir, cpu_mode) = {
                        let app_state = cx.global::<AppState>();
                        let settings = app_state.settings.lock();
                        (
                            settings.screenshot_directory.clone(),
                            settings.indexing_cpu_mode.clone(),
                        )
                    };

                    let db_path = crate::settings::Settings::config_path()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("vector_index.db");

                    // Get total count from database in background
                    let settings_arc = {
                        let app_state = cx.global::<AppState>();
                        Arc::clone(&app_state.settings)
                    };

                    std::thread::spawn(move || {
                        let config = crate::indexer::IndexConfig {
                            db_path,
                            cpu_mode: if cpu_mode == "fast" {
                                crate::indexer::CpuMode::Fast
                            } else {
                                crate::indexer::CpuMode::Normal
                            },
                            screenshot_dir,
                        };

                        if let Ok(total_count) = crate::indexer::get_indexed_count(&config) {
                            let mut settings = settings_arc.lock();
                            settings.last_indexed_count = total_count;
                            let _ = settings.save();
                            info!("Total indexed count updated: {}", total_count);
                        }
                    });

                    cx.notify();
                }
                AppMessage::IndexFailed(error) => {
                    error!("Indexing failed: {}", error);
                    self.indexing = false;

                    // Show error notification
                    window.push_notification(
                        Notification::new()
                            .message(&t!("notifications.indexing.failed", error = error).to_string())
                            .with_type(NotificationType::Error),
                        cx,
                    );

                    cx.notify();
                }
                AppMessage::SearchQuery(query) => {
                    info!("Search query: {}", query);
                    self.search_query = query.clone();

                    if query.is_empty() {
                        // Clear search
                        self.search_results = None;
                        cx.notify();
                    } else if let Some(text_model) = PREWARMED_TEXT_MODEL.lock().clone() {
                        // Spawn search in background
                        let app_state = cx.global::<AppState>();
                        let message_tx = app_state.message_tx.clone();
                        let settings = app_state.settings.lock();
                        let screenshot_dir = settings.screenshot_directory.clone();
                        let config_path = crate::settings::Settings::config_path()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .join("vector_index.db");

                        let config = crate::indexer::IndexConfig {
                            db_path: config_path,
                            cpu_mode: if settings.indexing_cpu_mode == "fast" {
                                crate::indexer::CpuMode::Fast
                            } else {
                                crate::indexer::CpuMode::Normal
                            },
                            screenshot_dir,
                        };

                        crate::indexer::search_images(query, config, text_model, message_tx, 100);
                    }
                }
                AppMessage::SearchResults(paths) => {
                    info!("Search results: {} images", paths.len());
                    self.search_results = if paths.is_empty() { None } else { Some(paths) };
                    cx.notify();
                }
                AppMessage::CopiedToClipboard(count) => {
                    info!("Showing clipboard toast for {} items", count);
                    // Show toast notification
                    let message = if count == 1 {
                        t!("notifications.copied_to_clipboard.one").to_string()
                    } else {
                        t!("notifications.copied_to_clipboard.other", count = count).to_string()
                    };
                    self.toast_manager.show(message);
                    cx.notify();
                }
            }
        }

        // If there are more messages, schedule another render to process them
        if has_more {
            cx.notify();
        }
    }

    /// Add a new screenshot
    fn add_screenshot(&mut self, path: PathBuf, should_auto_index: bool, cx: &mut Context<Self>) {
        if self.all_screenshots.iter().any(|s| s.path == path) {
            return;
        }

        // Check if we should auto-convert
        let (auto_convert, format, quality, message_tx) = {
            let app_state = cx.global::<AppState>();
            let settings = app_state.settings.lock();
            (
                settings.auto_convert_webp,
                settings.conversion_format,
                settings.webp_quality,
                app_state.message_tx.clone(),
            )
        };

        // If auto-convert is enabled and this is a PNG, convert it
        if auto_convert && convert::is_convertible(&path) {
            info!("Auto-converting new screenshot to {:?}: {:?}", format, path);
            let path_clone = path.clone();
            std::thread::spawn(move || {
                // Small delay to ensure the file is fully written
                std::thread::sleep(std::time::Duration::from_millis(500));

                match convert::convert_image(&path_clone, format, quality) {
                    Ok(output_path) => {
                        info!("{:?} conversion successful: {:?}", format, output_path);
                        // Notify about the new file (the remove is handled in convert)
                        // The watcher will pick up the new file automatically
                        // We send a remove for the old path since convert deleted it
                        let _ = message_tx.send(AppMessage::ScreenshotRemoved(path_clone));
                        let _ = message_tx
                            .send(AppMessage::NewScreenshot(output_path, should_auto_index));
                    }
                    Err(e) => {
                        log::error!("Failed to convert to {:?}: {}", format, e);
                        // Still add the original PNG if conversion failed
                        let _ = message_tx
                            .send(AppMessage::NewScreenshot(path_clone, should_auto_index));
                    }
                }
            });
            // Don't add the PNG yet - wait for conversion
            return;
        }

        if let Some(info) = ScreenshotInfo::from_path(path.clone()) {
            let insert_pos = self
                .all_screenshots
                .iter()
                .position(|s| s.modified < info.modified)
                .unwrap_or(self.all_screenshots.len());

            // If inserted at position 0, this is the newest screenshot
            if insert_pos == 0 {
                set_latest_screenshot(Some(info.path.clone()));
            }

            self.all_screenshots.insert(insert_pos, info);
            cx.notify();

            // Auto-index the new screenshot if indexing is enabled and this is a truly new screenshot
            if should_auto_index {
                let (
                    indexing_enabled,
                    models_downloaded,
                    screenshot_dir,
                    indexing_cpu_mode,
                    indexing,
                ) = {
                    let app_state = cx.global::<AppState>();
                    let settings = app_state.settings.lock();
                    (
                        settings.indexing_enabled,
                        settings.models_downloaded,
                        settings.screenshot_directory.clone(),
                        settings.indexing_cpu_mode.clone(),
                        self.indexing,
                    )
                };

                if indexing_enabled && models_downloaded && !indexing {
                    info!("Auto-indexing new screenshot: {:?}", path);
                    let tx = {
                        let app_state = cx.global::<AppState>();
                        app_state.message_tx.clone()
                    };
                    let db_path = crate::settings::Settings::config_path()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .join("vector_index.db");
                    let config = crate::indexer::IndexConfig {
                        db_path,
                        cpu_mode: if indexing_cpu_mode == "fast" {
                            crate::indexer::CpuMode::Fast
                        } else {
                            crate::indexer::CpuMode::Normal
                        },
                        screenshot_dir,
                    };
                    // Get prewarmed models for instant indexing (no loading needed)
                    let vision_model = PREWARMED_VISION_MODEL.lock().clone();
                    let text_model = PREWARMED_TEXT_MODEL.lock().clone();
                    // Index only new files (force_all = false) with prewarmed models
                    crate::indexer::start_indexing(config, tx, false, vision_model, text_model);
                }
            }
        }
    }

    /// Remove a screenshot
    fn remove_screenshot(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        self.all_screenshots.retain(|s| s.path != *path);
        self.selected.remove(path);
        self.thumbnail_cache.invalidate(path);

        // Cleanup vector DB if indexing is enabled
        let (indexing_enabled, screenshot_dir, indexing_cpu_mode) = {
            let app_state = cx.global::<AppState>();
            let settings = app_state.settings.lock();
            (
                settings.indexing_enabled,
                settings.screenshot_directory.clone(),
                settings.indexing_cpu_mode.clone(),
            )
        };

        if indexing_enabled {
            let db_path = crate::settings::Settings::config_path()
                .unwrap()
                .parent()
                .unwrap()
                .join("vector_index.db");
            let config = crate::indexer::IndexConfig {
                db_path,
                cpu_mode: if indexing_cpu_mode == "fast" {
                    crate::indexer::CpuMode::Fast
                } else {
                    crate::indexer::CpuMode::Normal
                },
                screenshot_dir,
            };
            // Remove from vector DB in background
            crate::indexer::remove_from_index(path.clone(), config);
        }

        cx.notify();
    }

    /// Handle gallery actions
    pub fn handle_action(&mut self, action: GalleryAction, cx: &mut Context<Self>) {
        match action {
            GalleryAction::Select { path, modifiers } => {
                self.handle_select(path, modifiers, cx);
            }
            GalleryAction::Open(path) => {
                self.open_file(&path);
            }
            GalleryAction::ContextMenu { paths, position } => {
                self.show_context_menu(&paths, position, cx);
            }
            GalleryAction::StartDrag(paths) => {
                self.start_drag(&paths);
            }
            GalleryAction::LoadMore => {
                self.load_more(cx);
            }
            GalleryAction::ClearSelection => {
                if !self.selected.is_empty() {
                    self.selected.clear();
                    self.last_selected = None;
                    cx.notify();
                }
            }
        }
    }

    /// Handle selection with modifiers
    fn handle_select(&mut self, path: PathBuf, modifiers: Modifiers, cx: &mut Context<Self>) {
        if modifiers.control {
            // Ctrl+click: toggle selection
            if self.selected.contains(&path) {
                self.selected.remove(&path);
            } else {
                self.selected.insert(path.clone());
            }
            self.last_selected = Some(path);
        } else if modifiers.shift {
            // Shift+click: range selection
            if let Some(last) = &self.last_selected {
                let last_idx = self.all_screenshots.iter().position(|s| &s.path == last);
                let current_idx = self.all_screenshots.iter().position(|s| s.path == path);

                if let (Some(start), Some(end)) = (last_idx, current_idx) {
                    let (start, end) = if start <= end {
                        (start, end)
                    } else {
                        (end, start)
                    };
                    for i in start..=end {
                        if i < self.all_screenshots.len() {
                            self.selected.insert(self.all_screenshots[i].path.clone());
                        }
                    }
                }
            } else {
                self.selected.clear();
                self.selected.insert(path.clone());
                self.last_selected = Some(path);
            }
        } else {
            // Normal click: single selection
            self.selected.clear();
            self.selected.insert(path.clone());
            self.last_selected = Some(path);
        }
        cx.notify();
    }

    /// Open file with default application
    fn open_file(&self, path: &PathBuf) {
        info!("Opening file: {:?}", path);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &path.to_string_lossy()])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
        #[cfg(not(windows))]
        {
            let _ = open::that(path);
        }
    }

    /// Show Windows context menu for files
    fn show_context_menu(
        &self,
        paths: &[PathBuf],
        _position: Point<Pixels>,
        _cx: &mut Context<Self>,
    ) {
        info!("Context menu for {} files", paths.len());
        #[cfg(windows)]
        {
            // Context menu MUST run on UI thread (same thread that owns the window)
            // This will block the UI while the menu is open, but that's expected behavior
            crate::ui::show_shell_context_menu(paths);
        }
    }

    /// Start native drag operation
    fn start_drag(&self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        info!("Starting drag with {} files", paths.len());
        // Note: This is called from the drag handler,
        // actual drag is initiated there
    }

    /// Load more items for infinite scroll
    fn load_more(&mut self, cx: &mut Context<Self>) {
        let new_count = (self.visible_count + PAGE_SIZE).min(self.all_screenshots.len());
        if new_count > self.visible_count {
            self.visible_count = new_count;
            cx.notify();
        }
    }

    /// Get currently visible screenshots
    fn visible_screenshots(&self) -> &[ScreenshotInfo] {
        let end = self.visible_count.min(self.all_screenshots.len());
        &self.all_screenshots[..end]
    }

    /// Check if there are more items to load
    fn has_more(&self) -> bool {
        self.visible_count < self.all_screenshots.len()
    }

    /// Get selected paths for context menu
    pub fn get_selected_paths(&self) -> Vec<PathBuf> {
        self.selected.iter().cloned().collect()
    }

    /// Check if a path is selected
    pub fn is_path_selected(&self, path: &PathBuf) -> bool {
        self.selected.contains(path)
    }

    /// Check if any items are selected
    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }
}

impl Render for Sukusho {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Process any pending messages
        self.process_messages(window, cx);

        let total_count = self.all_screenshots.len();
        let visible_count = self.visible_screenshots().len();
        let selected_count = self.selected.len();
        let settings_open = self.settings_open;
        let has_more = self.has_more();

        v_flex()
            .id("main-container")
            .size_full()
            // Semi-transparent background to allow acrylic blur to show through
            .bg(gpui::hsla(0.0, 0.0, 0.08, 0.85))
            .track_focus(&self.focus_handle)
            // Keyboard shortcuts
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // Skip handling if search input has focus
                if this.search_input_focused {
                    return;
                }

                // Handle hotkey recording
                if this.recording_hotkey {
                    // ESC cancels recording
                    if event.keystroke.key.as_str() == "escape" {
                        this.recording_hotkey = false;
                        cx.notify();
                        return;
                    }

                    // Try to convert keystroke to hotkey string
                    if let Some(hotkey_str) = Self::keystroke_to_hotkey_string(&event.keystroke) {
                        info!("Recorded hotkey: {}", hotkey_str);
                        // Save the new hotkey and re-register it
                        {
                            let app_state = cx.global::<AppState>();
                            let mut settings = app_state.settings.lock();
                            settings.hotkey = hotkey_str.clone();
                            let _ = settings.save();
                        }
                        // Update the global hotkey registration
                        crate::hotkey::update_hotkey(&hotkey_str);
                        this.recording_hotkey = false;
                        cx.notify();
                    }
                    return;
                }

                match event.keystroke.key.as_str() {
                    // ESC - clear selection, close settings, or minimize window
                    "escape" => {
                        if this.recording_hotkey {
                            this.recording_hotkey = false;
                            cx.notify();
                        } else if !this.selected.is_empty() {
                            // Clear selection if items are selected
                            this.selected.clear();
                            this.last_selected = None;
                            cx.notify();
                        } else if this.settings_open {
                            // Close settings if open
                            this.settings_open = false;
                            cx.notify();
                        } else {
                            // Minimize window
                            window.minimize_window();
                        }
                    }
                    // Ctrl+C - copy selected files to clipboard
                    "c" if event.keystroke.modifiers.control => {
                        if !this.selected.is_empty() {
                            let files: Vec<_> = this.selected.iter().cloned().collect();
                            let count = files.len();
                            info!("Attempting to copy {} files to clipboard", count);
                            if clipboard::copy_files_to_clipboard(&files) {
                                info!("Successfully copied {} files to clipboard", count);
                                // Send message to show notification (will be handled in process_messages)
                                let app_state = cx.global::<AppState>();
                                let _ = app_state.message_tx.send(AppMessage::CopiedToClipboard(count));
                            } else {
                                error!("Failed to copy files to clipboard");
                            }
                        } else {
                            info!("No files selected for clipboard copy");
                        }
                    }
                    // Ctrl+A - select all visible
                    "a" if event.keystroke.modifiers.control => {
                        let paths: Vec<_> = this
                            .visible_screenshots()
                            .iter()
                            .map(|i| i.path.clone())
                            .collect();
                        this.selected.clear();
                        for path in paths {
                            this.selected.insert(path);
                        }
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            // Header bar with window controls - enhanced styling
            .child(
                div()
                    .id("header-container")
                    .w_full()
                    .border_b_1()
                    .border_color(gpui::hsla(0.0, 0.0, 0.3, 0.3))
                    // Semi-transparent header for acrylic effect
                    .bg(gpui::hsla(0.0, 0.0, 0.12, 0.8))
                    .child(
                        h_flex()
                            .w_full()
                            .px_4()
                            .py_3()
                            .gap_3()
                            .items_center()
                            // Title area - draggable using native Windows API
                            .child(
                                h_flex()
                                    .id("header-drag-area")
                                    .flex_1()
                                    .gap_3()
                                    .items_center()
                                    .h(px(28.0))
                                    .cursor(CursorStyle::Arrow)
                                    // Use native Windows drag on mouse down
                                    .on_mouse_down(MouseButton::Left, |_, window, _cx| {
                                        start_window_drag(window);
                                    })
                                    // App icon/logo placeholder
                                    .child(
                                        div()
                                            .w(px(24.0))
                                            .h(px(24.0))
                                            .rounded(px(6.0))
                                            .bg(cx.theme().primary)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(cx.theme().primary_foreground)
                                            .text_xs()
                                            .font_weight(FontWeight::BOLD)
                                            .child("📷"),
                                    )
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::BOLD)
                                            .text_color(cx.theme().foreground)
                                            .child(t!("app.header.title").to_string()),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded(px(12.0))
                                            .bg(cx.theme().muted)
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!("app.header.counter", visible = visible_count, total = total_count).to_string()),
                                    )
                                    .when(selected_count > 0, |this| {
                                        this.child(
                                            div()
                                                .px_2()
                                                .py_1()
                                                .rounded(px(12.0))
                                                .bg(cx.theme().primary)
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(cx.theme().primary_foreground)
                                                .child(t!("app.header.selected", count = selected_count).to_string()),
                                        )
                                    }),
                            )
                            // Settings button (opens settings / goes back)
                            .child(
                                div()
                                    .id("settings-btn")
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .rounded(px(8.0))
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(cx.theme().muted)
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|s| {
                                        s.bg(cx.theme().accent)
                                            .text_color(cx.theme().accent_foreground)
                                    })
                                    .active(|s| {
                                        s.bg(cx.theme().primary)
                                            .text_color(cx.theme().primary_foreground)
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = !this.settings_open;
                                        cx.notify();
                                    }))
                                    .child(if settings_open { "←" } else { "⚙" }),
                            )
                            // Minimize button
                            .child(
                                div()
                                    .id("minimize-btn")
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .rounded(px(8.0))
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(cx.theme().muted)
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|s| {
                                        s.bg(gpui::rgb(0xE53935)).text_color(gpui::rgb(0xFFFFFF))
                                    })
                                    .active(|s| s.bg(gpui::rgb(0xC62828)))
                                    .on_click(|_, window, _cx| {
                                        window.minimize_window();
                                    })
                                    .child("—"),
                            ),
                    ),
            )
            .child(
                // Main content area
                div()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(if settings_open {
                        self.render_settings(cx).into_any_element()
                    } else {
                        self.render_gallery(has_more, cx).into_any_element()
                    }),
            )
            // Render toast overlay at bottom center
            .child(self.toast_manager.render())
    }
}

impl Sukusho {
    fn render_gallery(&self, has_more: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let search_enabled = self.models_downloaded;
        let has_search_results = self.search_results.is_some();

        v_flex()
            .size_full()
            // Search bar (only show if models are downloaded)
            .when(search_enabled, |el| {
                el.child(
                    h_flex()
                        .w_full()
                        .px_4()
                        .py_3()
                        .bg(cx.theme().background)
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .w_full()
                                .px_4()
                                .gap_2()
                                .items_center()
                                .child(Input::new(&self.search_input).flex_1())
                                .when(has_search_results, |el| {
                                    el.child(
                                        Button::new("clear-search")
                                            .small()
                                            .ghost()
                                            .label(&t!("app.search.clear_button").to_string())
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.search_input.update(cx, |input, cx| {
                                                    input.set_value("", window, cx);
                                                });
                                                this.search_query.clear();
                                                this.search_results = None;
                                                cx.notify();
                                            })),
                                    )
                                }),
                        ),
                )
            })
            // Gallery
            .child(gallery(
                self.visible_screenshots().to_vec(),
                self.search_results.clone(),
                self.selected.clone(),
                Arc::clone(&self.thumbnail_cache),
                self.grid_columns,
                self.thumbnail_size,
                has_more,
                cx,
            ))
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let app_state = cx.global::<AppState>();
        let settings = app_state.settings.lock().clone();
        let current_page = self.settings_page;

        // Pre-compute tab labels to avoid temporary value issues
        let tab_general = t!("settings.tabs.general").to_string();
        let tab_conversion = t!("settings.tabs.conversion").to_string();
        let tab_indexing = t!("settings.tabs.indexing").to_string();
        let tab_hotkey = t!("settings.tabs.hotkey").to_string();
        let tab_about = t!("settings.tabs.about").to_string();

        h_flex()
            .size_full()
            // Sidebar
            .child(
                v_flex()
                    .w(px(160.0))
                    .min_w(px(160.0))
                    .max_w(px(160.0))
                    .h_full()
                    .py_2()
                    .px_2()
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(self.render_settings_tab(
                        &tab_general,
                        SettingsPage::General,
                        current_page,
                        cx,
                    ))
                    .child(self.render_settings_tab(
                        &tab_conversion,
                        SettingsPage::Conversion,
                        current_page,
                        cx,
                    ))
                    .child(self.render_settings_tab(
                        &tab_indexing,
                        SettingsPage::Indexing,
                        current_page,
                        cx,
                    ))
                    .child(self.render_settings_tab(
                        &tab_hotkey,
                        SettingsPage::Hotkey,
                        current_page,
                        cx,
                    ))
                    .child(self.render_settings_tab(
                        &tab_about,
                        SettingsPage::About,
                        current_page,
                        cx,
                    )),
            )
            // Content area
            .child(
                div()
                    .id("settings-content")
                    .flex_1()
                    .h_full()
                    .overflow_scroll()
                    .p_4()
                    .child(match current_page {
                        SettingsPage::General => self
                            .render_general_settings(&settings, cx)
                            .into_any_element(),
                        SettingsPage::Conversion => self
                            .render_conversion_settings(&settings, cx)
                            .into_any_element(),
                        SettingsPage::Indexing => self
                            .render_indexing_settings(&settings, cx)
                            .into_any_element(),
                        SettingsPage::Hotkey => self
                            .render_hotkey_settings(&settings, cx)
                            .into_any_element(),
                        SettingsPage::About => self.render_about_settings(cx).into_any_element(),
                    }),
            )
    }

    fn render_settings_tab(
        &self,
        label: &str,
        page: SettingsPage,
        current: SettingsPage,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_active = page == current;
        let tab_id = format!("tab-{}", label.to_lowercase());
        div()
            .id(SharedString::from(tab_id))
            .w_full()
            .px_3()
            .py_2()
            .cursor_pointer()
            .text_sm()
            .rounded(px(6.0))
            .mb_1()
            .when(is_active, |s| {
                s.bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .font_weight(FontWeight::MEDIUM)
            })
            .when(!is_active, |s| {
                s.text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().muted))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.settings_page = page;
                cx.notify();
            }))
            .child(label.to_string())
    }

    fn render_setting_row(
        &self,
        label: &str,
        description: Option<&str>,
        control: impl IntoElement,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_1()
            .mb_4()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child(label.to_string()),
                    )
                    .child(control),
            )
            .when_some(description, |s, desc| {
                s.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(desc.to_string()),
                )
            })
    }

    fn render_section_header(&self, title: &str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .text_base()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(cx.theme().foreground)
            .mb_3()
            .child(title.to_string())
    }

    fn render_general_settings(
        &self,
        settings: &crate::settings::Settings,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let screenshot_dir = settings.screenshot_directory.to_string_lossy().to_string();
        let screenshot_dir_path = settings.screenshot_directory.clone();
        let thumbnail_size = self.thumbnail_size;
        let organizer_enabled = settings.organizer_enabled;
        let organizer_format = settings.organizer_format.clone();
        let format_preview = organizer::format_preview(&organizer_format);
        let organizing = self.organizing;
        let organize_progress = self.organize_progress;
        let organize_current_file = self.organize_current_file.clone();

        // Pre-compute strings to avoid temporary value issues
        let language_title = t!("settings.general.language.title").to_string();
        let language_label = t!("settings.general.language.label").to_string();
        let language_desc = t!("settings.general.language.desc").to_string();
        let screenshot_dir_title = t!("settings.general.screenshot_dir.title").to_string();
        let browse_label = t!("common.button.browse").to_string();
        let organizer_title = t!("settings.general.organizer.title").to_string();
        let organizer_enable_label = t!("settings.general.organizer.enable_label").to_string();
        let organizer_enable_desc = t!("settings.general.organizer.enable_desc").to_string();

        // Get current language
        let current_lang = crate::i18n_helpers::current_language();

        v_flex()
            .w_full()
            .gap_2()
            // Language
            .child(self.render_section_header(&language_title, cx))
            .child(
                self.render_setting_row(
                    &language_label,
                    Some(&language_desc),
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("lang-en")
                                .small()
                                .when(current_lang == "en", |b| b.primary())
                                .when(current_lang != "en", |b| b.outline())
                                .label("English")
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    crate::i18n_helpers::change_language("en");
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.language = Some("en".to_string());
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                }))
                        )
                        .child(
                            Button::new("lang-ko")
                                .small()
                                .when(current_lang == "ko", |b| b.primary())
                                .when(current_lang != "ko", |b| b.outline())
                                .label("한국어")
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    crate::i18n_helpers::change_language("ko");
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.language = Some("ko".to_string());
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                }))
                        )
                        .child(
                            Button::new("lang-ja")
                                .small()
                                .when(current_lang == "ja", |b| b.primary())
                                .when(current_lang != "ja", |b| b.outline())
                                .label("日本語")
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    crate::i18n_helpers::change_language("ja");
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.language = Some("ja".to_string());
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        ),
                    cx,
                )
            )
            // Screenshot Directory
            .child(self.render_section_header(&screenshot_dir_title, cx))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .mb_4()
                    .child(
                        div()
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded(px(6.0))
                            .bg(cx.theme().muted)
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .overflow_x_hidden()
                            .child(screenshot_dir),
                    )
                    .child(
                        Button::new("browse-dir")
                            .label(&browse_label)
                            .small()
                            .outline()
                            .on_click(|_, _, cx| {
                                let tx = {
                                    let app_state = cx.global::<AppState>();
                                    app_state.message_tx.clone()
                                };
                                std::thread::spawn(move || {
                                    if let Some(path) = pick_folder() {
                                        let _ = tx.send(AppMessage::ChangeDirectory(path));
                                    }
                                });
                            }),
                    ),
            )
            // Screenshot Organizer
            .child(self.render_section_header(&organizer_title, cx))
            .child(
                self.render_setting_row(
                    &organizer_enable_label,
                    if organizing {
                        None
                    } else {
                        Some(&organizer_enable_desc)
                    },
                    Switch::new("organizer-enable")
                        .checked(organizer_enabled)
                        .disabled(organizing)
                        .on_click({
                            let format = organizer_format.clone();
                            let base_dir = screenshot_dir_path.clone();
                            cx.listener(move |this, checked: &bool, _, cx| {
                                {
                                    let app_state = cx.global::<AppState>();
                                    let mut settings = app_state.settings.lock();
                                    settings.organizer_enabled = *checked;
                                    let _ = settings.save();
                                }
                                // If enabling, organize existing files
                                if *checked && !this.organizing {
                                    let tx = {
                                        let app_state = cx.global::<AppState>();
                                        app_state.message_tx.clone()
                                    };
                                    organizer::organize_existing_files(
                                        base_dir.clone(),
                                        format.clone(),
                                        tx,
                                    );
                                }
                                cx.notify();
                            })
                        }),
                    cx,
                ),
            )
            // Progress bar when organizing
            .when(organizing, |el| {
                let (current, total) = organize_progress;
                let progress_pct = if total > 0 {
                    (current as f32 / total as f32) * 100.0
                } else {
                    0.0
                };
                el.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .mb_4()
                        .child(
                            // Progress bar container
                            div()
                                .w_full()
                                .h(px(8.0))
                                .rounded(px(4.0))
                                .bg(cx.theme().muted)
                                .overflow_hidden()
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(progress_pct / 100.0))
                                        .bg(cx.theme().primary)
                                        .rounded(px(4.0)),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .max_w(px(200.0))
                                        .overflow_x_hidden()
                                        .child(if organize_current_file.is_empty() {
                                            t!("settings.general.organizer.progress.preparing").to_string()
                                        } else {
                                            organize_current_file
                                        }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("settings.general.organizer.progress.status", current = current, total = total).to_string()),
                                ),
                        ),
                )
            })
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .mb_4()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(t!("settings.general.organizer.format_label").to_string()),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(
                                        Button::new("fmt-ymd")
                                            .small()
                                            .when(organizer_format == "YYYY-MM-DD", |s| s.primary())
                                            .when(organizer_format != "YYYY-MM-DD", |s| s.outline())
                                            .label(&t!("settings.general.organizer.format_ymd").to_string())
                                            .on_click(cx.listener(|_this, _, _, cx| {
                                                {
                                                    let app_state = cx.global::<AppState>();
                                                    let mut settings = app_state.settings.lock();
                                                    settings.organizer_format =
                                                        "YYYY-MM-DD".to_string();
                                                    let _ = settings.save();
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("fmt-ym")
                                            .small()
                                            .when(organizer_format == "YYYY-MM", |s| s.primary())
                                            .when(organizer_format != "YYYY-MM", |s| s.outline())
                                            .label(&t!("settings.general.organizer.format_ym").to_string())
                                            .on_click(cx.listener(|_this, _, _, cx| {
                                                {
                                                    let app_state = cx.global::<AppState>();
                                                    let mut settings = app_state.settings.lock();
                                                    settings.organizer_format =
                                                        "YYYY-MM".to_string();
                                                    let _ = settings.save();
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("fmt-ymd-slash")
                                            .small()
                                            .when(organizer_format == "YYYY/MM/DD", |s| s.primary())
                                            .when(organizer_format != "YYYY/MM/DD", |s| s.outline())
                                            .label(&t!("settings.general.organizer.format_ymd_slash").to_string())
                                            .on_click(cx.listener(|_this, _, _, cx| {
                                                {
                                                    let app_state = cx.global::<AppState>();
                                                    let mut settings = app_state.settings.lock();
                                                    settings.organizer_format =
                                                        "YYYY/MM/DD".to_string();
                                                    let _ = settings.save();
                                                }
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("settings.general.organizer.format_preview", preview = format_preview).to_string()),
                    ),
            )
            // Display Settings
            .child(self.render_section_header(&t!("settings.general.display.title").to_string(), cx))
            .child(
                self.render_setting_row(
                    &t!("settings.general.display.thumbnail_size_label").to_string(),
                    Some(&t!("settings.general.display.thumbnail_size_desc").to_string()),
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("thumb-minus")
                                .ghost()
                                .compact()
                                .label("-")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let new_size = (this.thumbnail_size as i32 - 10).max(80) as u32;
                                    this.thumbnail_size = new_size;
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.thumbnail_size = new_size;
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .w(px(60.0))
                                .text_center()
                                .px_2()
                                .py_1()
                                .rounded(px(4.0))
                                .bg(cx.theme().muted)
                                .text_sm()
                                .child(t!("settings.general.display.thumbnail_size_value", size = thumbnail_size).to_string()),
                        )
                        .child(
                            Button::new("thumb-plus")
                                .ghost()
                                .compact()
                                .label("+")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let new_size = (this.thumbnail_size + 10).min(300);
                                    this.thumbnail_size = new_size;
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.thumbnail_size = new_size;
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        ),
                    cx,
                ),
            )
    }

    fn render_conversion_settings(
        &self,
        settings: &crate::settings::Settings,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let auto_convert = settings.auto_convert_webp;
        let format = settings.conversion_format;
        let quality = settings.webp_quality;
        let converting = self.converting;
        let convert_progress = self.convert_progress;
        let convert_current_file = self.convert_current_file.clone();

        v_flex()
            .w_full()
            .gap_2()
            .child(self.render_section_header(&t!("settings.conversion.auto_convert.title").to_string(), cx))
            // Auto-convert toggle
            .child(
                self.render_setting_row(
                    &t!("settings.conversion.auto_convert.enable_label").to_string(),
                    Some(&t!("settings.conversion.auto_convert.enable_desc").to_string()),
                    Switch::new("auto-convert")
                        .checked(auto_convert)
                        .on_click(cx.listener(|_this, checked: &bool, _, cx| {
                            {
                                let app_state = cx.global::<AppState>();
                                let mut settings = app_state.settings.lock();
                                settings.auto_convert_webp = *checked;
                                let _ = settings.save();
                            }
                            cx.notify();
                        })),
                    cx,
                ),
            )
            // Format selection
            .child(
                self.render_setting_row(
                    &t!("settings.conversion.format.label").to_string(),
                    Some(&t!("settings.conversion.format.desc").to_string()),
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("fmt-webp")
                                .small()
                                .when(format == ConversionFormat::WebP, |s| s.primary())
                                .when(format != ConversionFormat::WebP, |s| s.outline())
                                .label(&t!("settings.conversion.format.webp").to_string())
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.conversion_format = ConversionFormat::WebP;
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("fmt-jpeg")
                                .small()
                                .when(format == ConversionFormat::Jpeg, |s| s.primary())
                                .when(format != ConversionFormat::Jpeg, |s| s.outline())
                                .label(&t!("settings.conversion.format.jpeg").to_string())
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.conversion_format = ConversionFormat::Jpeg;
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        ),
                    cx,
                ),
            )
            // Quality
            .child(
                self.render_setting_row(
                    &t!("settings.conversion.quality.label").to_string(),
                    Some(&t!("settings.conversion.quality.desc").to_string()),
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("qual-minus")
                                .ghost()
                                .compact()
                                .label("-")
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.webp_quality =
                                            (settings.webp_quality as i32 - 5).max(1) as u32;
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .w(px(50.0))
                                .text_center()
                                .px_2()
                                .py_1()
                                .rounded(px(4.0))
                                .bg(cx.theme().muted)
                                .text_sm()
                                .child(format!("{}", quality)),
                        )
                        .child(
                            Button::new("qual-plus")
                                .ghost()
                                .compact()
                                .label("+")
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.webp_quality =
                                            (settings.webp_quality + 5).min(100);
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        ),
                    cx,
                ),
            )
            // Progress bar when converting
            .when(converting, |el| {
                let (current, total) = convert_progress;
                let progress_pct = if total > 0 {
                    (current as f32 / total as f32) * 100.0
                } else {
                    0.0
                };
                el.child(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .mb_4()
                        .child(
                            // Progress bar container
                            div()
                                .w_full()
                                .h(px(8.0))
                                .rounded(px(4.0))
                                .bg(cx.theme().muted)
                                .overflow_hidden()
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(progress_pct / 100.0))
                                        .bg(cx.theme().primary)
                                        .rounded(px(4.0)),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .max_w(px(200.0))
                                        .overflow_x_hidden()
                                        .child(if convert_current_file.is_empty() {
                                            t!("settings.conversion.progress.preparing").to_string()
                                        } else {
                                            convert_current_file
                                        }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("settings.conversion.progress.status", current = current, total = total).to_string()),
                                ),
                        ),
                )
            })
    }

    fn render_indexing_settings(
        &self,
        settings: &crate::settings::Settings,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let indexing_enabled = settings.indexing_enabled;
        let cpu_mode = settings.indexing_cpu_mode.clone();
        let indexed_count = settings.last_indexed_count;

        // Pre-compute strings to avoid temporary value issues
        let indexing_title = t!("settings.indexing.title").to_string();
        let indexing_enable_label = t!("settings.indexing.enable_label").to_string();
        let indexing_enable_desc = t!("settings.indexing.enable_desc").to_string();

        v_flex()
            .w_full()
            .gap_2()
            // Enable Image Indexing toggle
            .child(self.render_section_header(&indexing_title, cx))
            .child(
                self.render_setting_row(
                    &indexing_enable_label,
                    if self.downloading_models || self.indexing {
                        None
                    } else {
                        Some(&indexing_enable_desc)
                    },
                    Switch::new("indexing-enable")
                        .checked(indexing_enabled)
                        .disabled(self.downloading_models || self.indexing)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            {
                                let app_state = cx.global::<AppState>();
                                let mut settings = app_state.settings.lock();
                                settings.indexing_enabled = *checked;
                                let _ = settings.save();
                            }
                            // If enabling and models not downloaded, trigger download
                            if *checked && !this.models_downloaded {
                                let tx = {
                                    let app_state = cx.global::<AppState>();
                                    app_state.message_tx.clone()
                                };
                                let config = {
                                    let app_state = cx.global::<AppState>();
                                    let settings = app_state.settings.lock();
                                    let db_path = crate::settings::Settings::config_path()
                                        .unwrap()
                                        .parent()
                                        .unwrap()
                                        .join("vector_index.db");
                                    crate::indexer::IndexConfig {
                                        db_path,
                                        cpu_mode: if settings.indexing_cpu_mode == "fast" {
                                            crate::indexer::CpuMode::Fast
                                        } else {
                                            crate::indexer::CpuMode::Normal
                                        },
                                        screenshot_dir: settings.screenshot_directory.clone(),
                                    }
                                };
                                // Get prewarmed models if available
                                let vision_model = PREWARMED_VISION_MODEL.lock().clone();
                                let text_model = PREWARMED_TEXT_MODEL.lock().clone();
                                crate::indexer::start_indexing(config, tx, false, vision_model, text_model);
                            }
                            cx.notify();
                        })),
                    cx,
                ),
            )
            // Model download status (always show if downloading or downloaded)
            .when(self.downloading_models || self.models_downloaded, |el| {
                if self.downloading_models {
                    let (current, total) = self.model_download_progress;
                    // Extract model info from the third parameter which now contains detailed description
                    let progress_pct = if total > 0 {
                        (current as f32 / total as f32) * 100.0
                    } else {
                        0.0
                    };
                    el.child(
                        v_flex()
                            .w_full()
                            .gap_2()
                            .mb_4()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(t!("settings.indexing.model_status.title").to_string()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("settings.indexing.model_status.loading", current = current, total = total).to_string()),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(6.0))
                                    .rounded(px(3.0))
                                    .bg(cx.theme().muted)
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(progress_pct / 100.0))
                                            .bg(cx.theme().primary)
                                            .rounded(px(3.0)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("settings.indexing.model_status.loading_percent", percent = progress_pct as u32).to_string()),
                            )
                    )
                } else {
                    el.child(
                        v_flex()
                            .w_full()
                            .gap_1()
                            .mb_4()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(t!("settings.indexing.model_status.title").to_string()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if PREWARMED_TEXT_MODEL.lock().is_some() && PREWARMED_VISION_MODEL.lock().is_some() {
                                        t!("settings.indexing.model_status.online").to_string()
                                    } else {
                                        t!("settings.indexing.model_status.ready").to_string()
                                    }),
                            )
                    )
                }
            })
            // CPU Mode selection (always show, but disable when off or busy)
            .child(self.render_section_header(&t!("settings.indexing.settings_title").to_string(), cx))
            .child(
                self.render_setting_row(
                    &t!("settings.indexing.cpu_mode.label").to_string(),
                    Some(&t!("settings.indexing.cpu_mode.desc").to_string()),
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("cpu-normal")
                                .small()
                                .when(cpu_mode == "normal", |s| s.primary())
                                .when(cpu_mode != "normal", |s| s.outline())
                                .label(&t!("settings.indexing.cpu_mode.normal").to_string())
                                .disabled(!indexing_enabled || self.downloading_models || self.indexing)
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.indexing_cpu_mode = "normal".to_string();
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("cpu-fast")
                                .small()
                                .when(cpu_mode == "fast", |s| s.primary())
                                .when(cpu_mode != "fast", |s| s.outline())
                                .label(&t!("settings.indexing.cpu_mode.fast").to_string())
                                .disabled(!indexing_enabled || self.downloading_models || self.indexing)
                                .on_click(cx.listener(|_this, _, _, cx| {
                                    {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        settings.indexing_cpu_mode = "fast".to_string();
                                        let _ = settings.save();
                                    }
                                    cx.notify();
                                })),
                        ),
                    cx,
                )
            )
            // Indexing progress
            .when(self.indexing, |el| {
                let (current, total) = self.index_progress;
                let progress_pct = if total > 0 {
                    (current as f32 / total as f32) * 100.0
                } else {
                    0.0
                };
                el.child(self.render_section_header(&t!("settings.indexing.progress.title").to_string(), cx))
                    .child(
                        v_flex()
                            .w_full()
                            .gap_2()
                            .mb_4()
                            .child(
                                div()
                                    .w_full()
                                    .h(px(8.0))
                                    .rounded(px(4.0))
                                    .bg(cx.theme().muted)
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(progress_pct / 100.0))
                                            .bg(cx.theme().primary)
                                            .rounded(px(4.0)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .w_full()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .max_w(px(200.0))
                                            .overflow_x_hidden()
                                            .child(if self.index_current_file.is_empty() {
                                                t!("settings.indexing.progress.status_text").to_string()
                                            } else {
                                                self.index_current_file.clone()
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!("settings.indexing.progress.status", current = current, total = total).to_string()),
                                    ),
                            ),
                    )
            })
            // Index stats and manual re-index button (always show if models downloaded, regardless of toggle)
            .when(self.models_downloaded, |el| {
                el.child(self.render_section_header(&t!("settings.indexing.index_status.title").to_string(), cx))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .mb_4()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("settings.indexing.index_status.count", count = indexed_count).to_string()),
                            )
                            .child(
                                Button::new("index-new-button")
                                    .small()
                                    .outline()
                                    .label(&t!("settings.indexing.index_status.button").to_string())
                                    .disabled(!indexing_enabled || self.indexing || self.downloading_models)
                                    .on_click(cx.listener(|_this, _, _, cx| {
                                        let tx = {
                                            let app_state = cx.global::<AppState>();
                                            app_state.message_tx.clone()
                                        };
                                        let config = {
                                            let app_state = cx.global::<AppState>();
                                            let settings = app_state.settings.lock();
                                            let db_path = crate::settings::Settings::config_path()
                                                .unwrap()
                                                .parent()
                                                .unwrap()
                                                .join("vector_index.db");
                                            crate::indexer::IndexConfig {
                                                db_path,
                                                cpu_mode: if settings.indexing_cpu_mode == "fast" {
                                                    crate::indexer::CpuMode::Fast
                                                } else {
                                                    crate::indexer::CpuMode::Normal
                                                },
                                                screenshot_dir: settings.screenshot_directory.clone(),
                                            }
                                        };
                                        // Get prewarmed models if available
                                        let vision_model = PREWARMED_VISION_MODEL.lock().clone();
                                        let text_model = PREWARMED_TEXT_MODEL.lock().clone();
                                        crate::indexer::start_indexing(config, tx, false, vision_model, text_model);  // false = only new files
                                        cx.notify();
                                    })),
                            )
                    )
            })
    }

    fn render_hotkey_settings(
        &self,
        settings: &crate::settings::Settings,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let hotkey_enabled = settings.hotkey_enabled;
        let hotkey_str = settings.hotkey.clone();
        let recording = self.recording_hotkey;

        v_flex()
            .w_full()
            .gap_2()
            .child(self.render_section_header(&t!("settings.hotkey.title").to_string(), cx))
            // Enable toggle
            .child(
                self.render_setting_row(
                    &t!("settings.hotkey.enable_label").to_string(),
                    Some(&t!("settings.hotkey.enable_desc").to_string()),
                    Switch::new("hotkey-enable")
                        .checked(hotkey_enabled)
                        .on_click(cx.listener(|_this, checked: &bool, _, cx| {
                            {
                                let app_state = cx.global::<AppState>();
                                let mut settings = app_state.settings.lock();
                                settings.hotkey_enabled = *checked;
                                let _ = settings.save();
                            }
                            cx.notify();
                        })),
                    cx,
                ),
            )
            // Current hotkey display + record button
            .child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .mb_4()
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().foreground)
                                    .child(t!("settings.hotkey.current_label").to_string()),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .px_3()
                                            .py_1()
                                            .rounded(px(6.0))
                                            .bg(if recording {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().muted
                                            })
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(if recording {
                                                cx.theme().primary_foreground
                                            } else {
                                                cx.theme().foreground
                                            })
                                            .child(if recording {
                                                t!("settings.hotkey.recording").to_string()
                                            } else {
                                                hotkey_str
                                            }),
                                    )
                                    .child(
                                        Button::new("record-hotkey")
                                            .small()
                                            .when(recording, |s| s.danger())
                                            .when(!recording, |s| s.outline())
                                            .label(&if recording { t!("settings.hotkey.cancel_button").to_string() } else { t!("settings.hotkey.record_button").to_string() })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.recording_hotkey = !this.recording_hotkey;
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("settings.hotkey.examples").to_string()),
                    ),
            )
    }

    fn render_about_settings(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_4()
            .items_center()
            .py_8()
            // App Icon
            .child(
                div()
                    .w(px(80.0))
                    .h(px(80.0))
                    .rounded(px(16.0))
                    .bg(cx.theme().primary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(48.0))
                    .child("📷"),
            )
            // App Name
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .child(APP_NAME),
            )
            // Version
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings.about.version", version = APP_VERSION).to_string()),
            )
            // Description
            .child(
                div()
                    .max_w(px(300.0))
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings.about.description").to_string()),
            )
            // Links
            .child(
                h_flex()
                    .gap_2()
                    .mt_4()
                    .child(
                        Button::new("github")
                            .outline()
                            .small()
                            .label(&t!("settings.about.github_button").to_string())
                            .on_click(|_, _, cx| {
                                cx.open_url("https://github.com/ssut/sukusho");
                            }),
                    ),
            )
            // Copyright
            .child(
                div()
                    .mt_4()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings.about.made_with").to_string()),
            )
    }
}
