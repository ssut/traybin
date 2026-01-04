//! Main application state and UI

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::scroll::ScrollableElement;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{h_flex, v_flex, ActiveTheme, StyledExt};
use log::info;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::clipboard;
use crate::convert;
use crate::settings::{ConversionFormat, Settings};
use crate::thumbnail::ThumbnailCache;
use crate::ui::gallery;
use crate::{set_latest_screenshot, AppMessage, AppState};

/// Start native window drag using Windows API
#[cfg(windows)]
fn start_window_drag(_window: &mut Window) {
    use crate::tray::WINDOW_HWND;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, HTCAPTION, WM_NCLBUTTONDOWN};

    if let Some(hwnd) = *WINDOW_HWND.lock() {
        unsafe {
            // Release mouse capture first
            let _ = ReleaseCapture();
            // Send message to start window drag (simulate title bar click)
            SendMessageW(
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
    use windows::core::PWSTR;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileDialog, IShellItem, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
    };

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
}

/// Main application view
pub struct TrayBin {
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

    /// Current grid columns
    grid_columns: u32,

    /// Current thumbnail size
    thumbnail_size: u32,

    /// Focus handle for keyboard events
    focus_handle: FocusHandle,

    /// Slider state for thumbnail size
    thumbnail_slider: Entity<SliderState>,

    /// Slider state for WebP quality
    quality_slider: Entity<SliderState>,

    /// Whether we're recording a new hotkey
    recording_hotkey: bool,
}

impl TrayBin {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app_state = cx.global::<AppState>();
        let settings = app_state.settings.lock().clone();

        // Create slider for thumbnail size (80-300px)
        let thumbnail_slider = cx.new(|_| {
            SliderState::new()
                .min(80.0)
                .max(300.0)
                .default_value(settings.thumbnail_size as f32)
                .step(10.0)
        });

        // Create slider for WebP quality (1-100)
        let quality_slider = cx.new(|_| {
            SliderState::new()
                .min(1.0)
                .max(100.0)
                .default_value(settings.webp_quality as f32)
                .step(5.0)
        });

        // Subscribe to thumbnail slider changes
        cx.subscribe(&thumbnail_slider, |this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(value) = event;
            let new_size = value.start() as u32;
            this.thumbnail_size = new_size;
            {
                let app_state = cx.global::<AppState>();
                let mut settings = app_state.settings.lock();
                settings.thumbnail_size = new_size;
                let _ = settings.save();
            }
            cx.notify();
        })
        .detach();

        // Subscribe to quality slider changes
        cx.subscribe(&quality_slider, |_this, _, event: &SliderEvent, cx| {
            let SliderEvent::Change(value) = event;
            let new_quality = value.start() as u32;
            {
                let app_state = cx.global::<AppState>();
                let mut settings = app_state.settings.lock();
                settings.webp_quality = new_quality;
                let _ = settings.save();
            }
            cx.notify();
        })
        .detach();

        Self {
            all_screenshots: Vec::new(),
            visible_count: PAGE_SIZE,
            selected: HashSet::new(),
            last_selected: None,
            thumbnail_cache: Arc::new(ThumbnailCache::new(500)),
            settings_open: false,
            grid_columns: settings.grid_columns,
            thumbnail_size: settings.thumbnail_size,
            focus_handle: cx.focus_handle(),
            thumbnail_slider,
            quality_slider,
            recording_hotkey: false,
        }
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
                AppMessage::NewScreenshot(path) => {
                    self.add_screenshot(path, cx);
                }
                AppMessage::ScreenshotRemoved(path) => {
                    self.remove_screenshot(&path, cx);
                }
                AppMessage::ToggleWindow => {
                    info!("Toggle window requested - activating window");
                    window.activate_window();
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
            }
        }

        // If there are more messages, schedule another render to process them
        if has_more {
            cx.notify();
        }
    }

    /// Add a new screenshot
    fn add_screenshot(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
                        let _ = message_tx.send(AppMessage::NewScreenshot(output_path));
                    }
                    Err(e) => {
                        log::error!("Failed to convert to {:?}: {}", format, e);
                        // Still add the original PNG if conversion failed
                        let _ = message_tx.send(AppMessage::NewScreenshot(path_clone));
                    }
                }
            });
            // Don't add the PNG yet - wait for conversion
            return;
        }

        if let Some(info) = ScreenshotInfo::from_path(path) {
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
        }
    }

    /// Remove a screenshot
    fn remove_screenshot(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        self.all_screenshots.retain(|s| s.path != *path);
        self.selected.remove(path);
        self.thumbnail_cache.invalidate(path);
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

impl Render for TrayBin {
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
                    // ESC - minimize window or cancel recording
                    "escape" => {
                        if this.recording_hotkey {
                            this.recording_hotkey = false;
                            cx.notify();
                        } else {
                            window.minimize_window();
                        }
                    }
                    // Ctrl+C - copy selected files to clipboard
                    "c" if event.keystroke.modifiers.control => {
                        if !this.selected.is_empty() {
                            let files: Vec<_> = this.selected.iter().cloned().collect();
                            if clipboard::copy_files_to_clipboard(&files) {
                                info!("Copied {} files to clipboard", files.len());
                            }
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
                                            .child("Screenshots"),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_1()
                                            .rounded(px(12.0))
                                            .bg(cx.theme().muted)
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{} / {}", visible_count, total_count)),
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
                                                .child(format!("{} selected", selected_count)),
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
    }
}

impl TrayBin {
    fn render_gallery(&self, has_more: bool, cx: &mut Context<Self>) -> impl IntoElement {
        gallery(
            self.visible_screenshots().to_vec(),
            self.selected.clone(),
            Arc::clone(&self.thumbnail_cache),
            self.grid_columns,
            self.thumbnail_size,
            has_more,
            cx,
        )
    }

    fn render_settings(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let app_state = cx.global::<AppState>();
        let settings = app_state.settings.lock().clone();
        let auto_convert = settings.auto_convert_webp;
        let conversion_format = settings.conversion_format;
        let thumbnail_size = self.thumbnail_size;
        let quality = settings.webp_quality;
        let hotkey_str = settings.hotkey.clone();
        let recording_hotkey = self.recording_hotkey;

        v_flex()
            .size_full()
            .p_4()
            .gap_4()
            .overflow_y_scrollbar()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Settings"),
                    )
                    .child(
                        Button::new("back")
                            .label("Go back")
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_open = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_6()
                    .w_full()
                    .max_w(px(600.0))
                    // Screenshot Directory with Browse button
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Screenshot Directory"),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .px_3()
                                            .py_2()
                                            .rounded(px(6.0))
                                            .bg(cx.theme().muted)
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .overflow_hidden()
                                            .child(
                                                settings
                                                    .screenshot_directory
                                                    .to_string_lossy()
                                                    .to_string(),
                                            ),
                                    )
                                    .child(
                                        Button::new("browse-dir")
                                            .label("Browse...")
                                            .ghost()
                                            .on_click(cx.listener(|_this, _, _, cx| {
                                                // Open folder picker in a thread to avoid blocking
                                                let tx = {
                                                    let app_state = cx.global::<AppState>();
                                                    app_state.message_tx.clone()
                                                };
                                                std::thread::spawn(move || {
                                                    if let Some(path) = pick_folder() {
                                                        let _ = tx.send(
                                                            AppMessage::ChangeDirectory(path),
                                                        );
                                                    }
                                                });
                                            })),
                                    ),
                            ),
                    )
                    // Thumbnail Size with Slider
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Thumbnail Size"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!("{}px", thumbnail_size)),
                                    ),
                            )
                            .child(Slider::new(&self.thumbnail_slider)),
                    )
                    // Auto-convert toggle
                    .child(
                        h_flex()
                            .id("convert-toggle")
                            .gap_3()
                            .items_center()
                            .cursor_pointer()
                            .on_click(cx.listener(|_this, _, _, cx| {
                                {
                                    let app_state = cx.global::<AppState>();
                                    let mut settings = app_state.settings.lock();
                                    settings.auto_convert_webp = !settings.auto_convert_webp;
                                    let _ = settings.save();
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .w(px(20.0))
                                    .h(px(20.0))
                                    .rounded(px(4.0))
                                    .border_2()
                                    .border_color(if auto_convert {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().border
                                    })
                                    .bg(if auto_convert {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().background
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(auto_convert, |this: Div| {
                                        this.child(
                                            div()
                                                .text_color(cx.theme().primary_foreground)
                                                .text_xs()
                                                .child("✓"),
                                        )
                                    }),
                            )
                            .child(
                                v_flex()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Auto-convert Screenshots"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Automatically convert new PNG screenshots"),
                                    ),
                            ),
                    )
                    // Conversion format selector (only show if auto-convert enabled)
                    .when(auto_convert, |this: Div| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Conversion Format"),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        // WebP button
                                        .child(
                                            div()
                                                .id("format-webp")
                                                .px_4()
                                                .py_2()
                                                .rounded(px(6.0))
                                                .cursor_pointer()
                                                .border_2()
                                                .border_color(
                                                    if conversion_format == ConversionFormat::WebP {
                                                        cx.theme().primary
                                                    } else {
                                                        cx.theme().border
                                                    },
                                                )
                                                .bg(
                                                    if conversion_format == ConversionFormat::WebP {
                                                        cx.theme().primary
                                                    } else {
                                                        cx.theme().background
                                                    },
                                                )
                                                .text_color(
                                                    if conversion_format == ConversionFormat::WebP {
                                                        cx.theme().primary_foreground
                                                    } else {
                                                        cx.theme().foreground
                                                    },
                                                )
                                                .text_sm()
                                                .on_click(cx.listener(|_this, _, _, cx| {
                                                    {
                                                        let app_state = cx.global::<AppState>();
                                                        let mut settings =
                                                            app_state.settings.lock();
                                                        settings.conversion_format =
                                                            ConversionFormat::WebP;
                                                        let _ = settings.save();
                                                    }
                                                    cx.notify();
                                                }))
                                                .child("WebP"),
                                        )
                                        // JPEG button
                                        .child(
                                            div()
                                                .id("format-jpeg")
                                                .px_4()
                                                .py_2()
                                                .rounded(px(6.0))
                                                .cursor_pointer()
                                                .border_2()
                                                .border_color(
                                                    if conversion_format == ConversionFormat::Jpeg {
                                                        cx.theme().primary
                                                    } else {
                                                        cx.theme().border
                                                    },
                                                )
                                                .bg(
                                                    if conversion_format == ConversionFormat::Jpeg {
                                                        cx.theme().primary
                                                    } else {
                                                        cx.theme().background
                                                    },
                                                )
                                                .text_color(
                                                    if conversion_format == ConversionFormat::Jpeg {
                                                        cx.theme().primary_foreground
                                                    } else {
                                                        cx.theme().foreground
                                                    },
                                                )
                                                .text_sm()
                                                .on_click(cx.listener(|_this, _, _, cx| {
                                                    {
                                                        let app_state = cx.global::<AppState>();
                                                        let mut settings =
                                                            app_state.settings.lock();
                                                        settings.conversion_format =
                                                            ConversionFormat::Jpeg;
                                                        let _ = settings.save();
                                                    }
                                                    cx.notify();
                                                }))
                                                .child("JPEG"),
                                        ),
                                ),
                        )
                    })
                    // Quality slider (only show if auto-convert enabled)
                    .when(auto_convert, |this: Div| {
                        this.child(
                            v_flex()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(FontWeight::MEDIUM)
                                                .child("Quality"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("{}", quality)),
                                        ),
                                )
                                .child(Slider::new(&self.quality_slider)),
                        )
                    })
                    // Global Hotkey section
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .id("hotkey-toggle")
                                    .gap_3()
                                    .items_center()
                                    .cursor_pointer()
                                    .on_click(cx.listener(|_this, _, _, cx| {
                                        {
                                            let app_state = cx.global::<AppState>();
                                            let mut settings = app_state.settings.lock();
                                            settings.hotkey_enabled = !settings.hotkey_enabled;
                                            let _ = settings.save();
                                        }
                                        cx.notify();
                                    }))
                                    .child({
                                        let hotkey_enabled = settings.hotkey_enabled;
                                        div()
                                            .w(px(20.0))
                                            .h(px(20.0))
                                            .rounded(px(4.0))
                                            .border_2()
                                            .border_color(if hotkey_enabled {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().border
                                            })
                                            .bg(if hotkey_enabled {
                                                cx.theme().primary
                                            } else {
                                                cx.theme().background
                                            })
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(hotkey_enabled, |this: Div| {
                                                this.child(
                                                    div()
                                                        .text_color(cx.theme().primary_foreground)
                                                        .text_xs()
                                                        .child("✓"),
                                                )
                                            })
                                    })
                                    .child(
                                        v_flex()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("Global Hotkey"),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child("Press hotkey to show window"),
                                            ),
                                    ),
                            )
                            // Hotkey input with Record button
                            .when(settings.hotkey_enabled, |this: Div| {
                                this.child(
                                    v_flex()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(if recording_hotkey {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().muted_foreground
                                                })
                                                .child(if recording_hotkey {
                                                    "Press your hotkey combination now... (ESC to cancel)"
                                                } else {
                                                    "Click Record, then press your hotkey combination"
                                                }),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .items_center()
                                                .child(
                                                    div()
                                                        .w(px(200.0))
                                                        .px_3()
                                                        .py_2()
                                                        .rounded(px(6.0))
                                                        .border_2()
                                                        .border_color(if recording_hotkey {
                                                            cx.theme().primary
                                                        } else {
                                                            cx.theme().border
                                                        })
                                                        .bg(if recording_hotkey {
                                                            cx.theme().accent
                                                        } else {
                                                            cx.theme().muted
                                                        })
                                                        .text_sm()
                                                        .font_medium()
                                                        .child(if recording_hotkey {
                                                            "Recording...".to_string()
                                                        } else {
                                                            format!("Current: {}", hotkey_str)
                                                        }),
                                                )
                                                .child(
                                                    Button::new("record-hotkey")
                                                        .label(if recording_hotkey { "Cancel" } else { "Record" })
                                                        .map(|btn| {
                                                            if recording_hotkey {
                                                                btn.ghost()
                                                            } else {
                                                                btn.primary()
                                                            }
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.recording_hotkey = !this.recording_hotkey;
                                                            cx.notify();
                                                        })),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("Tip: Common shortcuts: Ctrl+Shift+S, Ctrl+Shift+A, F12, Win+Shift+S"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("Requires restart to apply changes"),
                                        ),
                                )
                            }),
                    )
                    // Reset to defaults button
                    .child(
                        div().mt_4().child(
                            Button::new("reset-settings")
                                .ghost()
                                .label("Reset to Defaults")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let (grid_columns, thumbnail_size, webp_quality) = {
                                        let app_state = cx.global::<AppState>();
                                        let mut settings = app_state.settings.lock();
                                        *settings = Settings::default();
                                        let _ = settings.save();
                                        (
                                            settings.grid_columns,
                                            settings.thumbnail_size,
                                            settings.webp_quality,
                                        )
                                    };
                                    this.grid_columns = grid_columns;
                                    this.thumbnail_size = thumbnail_size;
                                    // Update slider states
                                    this.thumbnail_slider.update(cx, |state, cx| {
                                        state.set_value(thumbnail_size as f32, window, cx);
                                    });
                                    this.quality_slider.update(cx, |state, cx| {
                                        state.set_value(webp_quality as f32, window, cx);
                                    });
                                    cx.notify();
                                })),
                        ),
                    ),
            )
    }
}
