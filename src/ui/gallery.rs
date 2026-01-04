//! Image gallery grid with drag-drop support and date grouping

use chrono::{DateTime, Datelike, Local, NaiveDate};
use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::scroll::ScrollableElement;
use gpui_component::ActiveTheme;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Instant, SystemTime};

use crate::app::{format_file_size, GalleryAction, ScreenshotInfo, Sukusho};
use crate::drag_drop;
use crate::thumbnail::ThumbnailCache;

/// Flag to track if a gallery item was clicked (to prevent background deselection)
static ITEM_CLICKED: AtomicBool = AtomicBool::new(false);

/// Track last click for double-click detection (time, path)
static LAST_CLICK: StdMutex<Option<(Instant, PathBuf)>> = StdMutex::new(None);

/// Double-click time threshold in milliseconds
const DOUBLE_CLICK_TIME_MS: u128 = 500;

/// Date group category
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum DateGroup {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Earlier(String), // Month Year format
}

impl DateGroup {
    fn from_system_time(time: SystemTime) -> Self {
        let datetime: DateTime<Local> = time.into();
        let today = Local::now().date_naive();
        let date = datetime.date_naive();

        if date == today {
            DateGroup::Today
        } else if date == today.pred_opt().unwrap_or(today) {
            DateGroup::Yesterday
        } else if is_same_week(date, today) {
            DateGroup::ThisWeek
        } else if date.year() == today.year() && date.month() == today.month() {
            DateGroup::ThisMonth
        } else {
            DateGroup::Earlier(datetime.format("%B %Y").to_string())
        }
    }

    fn label(&self) -> String {
        match self {
            DateGroup::Today => "Today".to_string(),
            DateGroup::Yesterday => "Yesterday".to_string(),
            DateGroup::ThisWeek => "This Week".to_string(),
            DateGroup::ThisMonth => "This Month".to_string(),
            DateGroup::Earlier(s) => s.clone(),
        }
    }

    fn order(&self) -> u32 {
        match self {
            DateGroup::Today => 0,
            DateGroup::Yesterday => 1,
            DateGroup::ThisWeek => 2,
            DateGroup::ThisMonth => 3,
            DateGroup::Earlier(_) => 4,
        }
    }
}

fn is_same_week(date1: NaiveDate, date2: NaiveDate) -> bool {
    date1.iso_week() == date2.iso_week() && date1.year() == date2.year()
}

/// Group screenshots by date
fn group_by_date(screenshots: &[ScreenshotInfo]) -> Vec<(DateGroup, Vec<&ScreenshotInfo>)> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<(u32, String), (DateGroup, Vec<&ScreenshotInfo>)> = BTreeMap::new();

    for info in screenshots {
        let group = DateGroup::from_system_time(info.modified);
        let key = (group.order(), group.label());

        groups
            .entry(key)
            .or_insert_with(|| (group, Vec::new()))
            .1
            .push(info);
    }

    groups.into_values().collect()
}

/// Item data for gallery rendering
struct GalleryItemData {
    path: PathBuf,
    is_selected: bool,
    selected_paths: Vec<PathBuf>,
    size: u32,
    index: usize,
    file_size: u64,
    extension: String,
}

/// Build a gallery grid component with date grouping
pub fn gallery(
    screenshots: Vec<ScreenshotInfo>,
    filtered_paths: Option<Vec<PathBuf>>,
    selected: HashSet<PathBuf>,
    _thumbnail_cache: Arc<ThumbnailCache>,
    _columns: u32,
    thumbnail_size: u32,
    has_more: bool,
    cx: &mut Context<Sukusho>,
) -> impl IntoElement {
    let spacing = 8.0;

    // Filter screenshots if search is active
    let visible_screenshots = if let Some(filter) = filtered_paths {
        let filter_set: HashSet<_> = filter.into_iter().collect();
        screenshots
            .into_iter()
            .filter(|s| filter_set.contains(&s.path))
            .collect()
    } else {
        screenshots
    };

    if visible_screenshots.is_empty() {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No screenshots found. Screenshots will appear here when added to your Screenshots folder."),
            )
            .into_any_element();
    }

    // Group screenshots by date
    let groups = group_by_date(&visible_screenshots);

    // Build grouped content
    let mut content_children: Vec<AnyElement> = Vec::new();
    let mut global_index = 0usize;

    for (group, items) in groups {
        // Add group header
        content_children.push(
            div()
                .w_full()
                .pt_4()
                .pb_2()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, _, _| {
                        // Mark that a header was clicked (prevent background deselection)
                        ITEM_CLICKED.store(true, Ordering::SeqCst);
                    }),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(cx.theme().muted_foreground)
                        .child(group.label()),
                )
                .into_any_element(),
        );

        // Build items for this group
        let mut group_items: Vec<AnyElement> = Vec::new();
        for info in items {
            let is_selected = selected.contains(&info.path);
            let selected_paths: Vec<PathBuf> = if is_selected {
                selected.iter().cloned().collect()
            } else {
                vec![info.path.clone()]
            };

            let data = GalleryItemData {
                path: info.path.clone(),
                is_selected,
                selected_paths,
                size: thumbnail_size,
                index: global_index,
                file_size: info.file_size,
                extension: info.extension.clone(),
            };
            group_items.push(gallery_item(data, cx).into_any_element());
            global_index += 1;
        }

        // Add group items grid
        content_children.push(
            div()
                .w_full()
                .flex()
                .flex_wrap()
                .gap(px(spacing))
                .children(group_items)
                .into_any_element(),
        );
    }

    // Add infinite scroll trigger at the bottom
    if has_more {
        content_children.push(
            div()
                .id("infinite-scroll-trigger")
                .w_full()
                .h(px(50.0))
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading more...")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        // Mark that an item was clicked (prevent background deselection)
                        ITEM_CLICKED.store(true, Ordering::SeqCst);
                        this.handle_action(GalleryAction::LoadMore, cx);
                    }),
                )
                .into_any_element(),
        );
    }

    div()
        .id("gallery-scroll-container")
        .size_full()
        .overflow_y_scrollbar()
        // Trigger load more when scrolling near bottom
        .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
            // Load more when scrolling down (negative delta means scrolling down)
            let is_scrolling_down = match event.delta {
                ScrollDelta::Lines(delta) => delta.y < 0.0,
                ScrollDelta::Pixels(delta) => delta.y < px(0.0),
            };
            if is_scrolling_down && has_more {
                this.handle_action(GalleryAction::LoadMore, cx);
            }
        }))
        .child(
            div()
                .id("gallery-content")
                .w_full()
                .px_4()
                .pb_4()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _event: &MouseDownEvent, _, cx| {
                        // Check if an item was clicked (item handlers set this flag)
                        if !ITEM_CLICKED.swap(false, Ordering::SeqCst) {
                            // No item was clicked, so this is a background click
                            // Clear selection
                            this.handle_action(GalleryAction::ClearSelection, cx);
                        }
                    }),
                )
                .children(content_children),
        )
        .into_any_element()
}

/// Build a single gallery item with enhanced styling
fn gallery_item(data: GalleryItemData, cx: &mut Context<Sukusho>) -> impl IntoElement + use<> {
    let size_px = px(data.size as f32);
    let path = data.path;
    let path_for_dbl = path.clone();
    let path_for_ctx = path.clone();
    let path_for_checkbox = path.clone();
    let drag_paths = data.selected_paths.clone();
    let is_selected = data.is_selected;

    // Enhanced color scheme
    let bg_color = if is_selected {
        cx.theme().accent
    } else {
        cx.theme().secondary
    };

    let border_color = if is_selected {
        cx.theme().primary
    } else {
        cx.theme().border
    };

    let hover_border = cx.theme().primary;
    let hover_bg = cx.theme().muted;

    let file_badge = format!("{} | {}", data.extension, format_file_size(data.file_size));

    // Badge colors - semi-transparent black with white text for good contrast
    let badge_bg = gpui::hsla(0.0, 0.0, 0.0, 0.75);

    // Checkbox colors - circular design
    // Selected: solid blue with white check
    // Unselected: dimmed gray circle
    let checkbox_bg = if is_selected {
        gpui::hsla(210.0 / 360.0, 1.0, 0.42, 1.0) // Solid blue (#0078D4) when selected
    } else {
        gpui::hsla(0.0, 0.0, 0.2, 0.7) // Dimmed dark gray when unselected
    };
    let checkbox_border = if is_selected {
        gpui::hsla(0.0, 0.0, 1.0, 1.0) // White border when selected
    } else {
        gpui::hsla(0.0, 0.0, 0.6, 0.6) // Light gray border when unselected
    };

    div()
        .id(ElementId::Name(
            format!("gallery-item-{}", data.index).into(),
        ))
        .w(size_px)
        .h(size_px)
        .rounded(px(12.0))
        .bg(bg_color)
        .border_2()
        .border_color(border_color)
        .overflow_hidden()
        .cursor_pointer()
        // Enhanced shadow effect for depth
        .shadow_sm()
        .hover(move |s| s.border_color(hover_border).bg(hover_bg).shadow_md())
        .child(
            // Container for image and overlays
            div()
                .size_full()
                .relative()
                .child(
                    // Image centered with slight padding
                    div()
                        .size_full()
                        .p_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            img(path.clone())
                                .max_w_full()
                                .max_h_full()
                                .object_fit(ObjectFit::Contain),
                        ),
                )
                // Selection checkbox - always visible (circular design)
                .child(
                    div()
                        .id(ElementId::Name(format!("checkbox-{}", data.index).into()))
                        .absolute()
                        .top(px(6.0))
                        .left(px(6.0))
                        .w(px(20.0))
                        .h(px(20.0))
                        .rounded(px(10.0)) // Circular
                        .bg(checkbox_bg)
                        .border_1()
                        .border_color(checkbox_border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| {
                            s.bg(gpui::hsla(0.0, 0.0, 0.5, 0.8))
                                .border_color(gpui::rgb(0xFFFFFF))
                        })
                        .when(is_selected, |this| {
                            this.child(
                                div()
                                    .text_color(gpui::rgb(0xFFFFFF))
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .child("✓"),
                            )
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _event: &MouseDownEvent, _, cx| {
                                // Mark that an item was clicked (prevent background deselection)
                                ITEM_CLICKED.store(true, Ordering::SeqCst);
                                // Checkbox click = toggle selection (append/remove like Ctrl+click)
                                this.handle_action(
                                    GalleryAction::Select {
                                        path: path_for_checkbox.clone(),
                                        modifiers: Modifiers {
                                            control: true, // Act like Ctrl+click to toggle/append
                                            ..Default::default()
                                        },
                                    },
                                    cx,
                                );
                            }),
                        ),
                )
                .child(
                    // File format and size badge - enhanced styling
                    div()
                        .absolute()
                        .bottom(px(6.0))
                        .right(px(6.0))
                        .px(px(8.0))
                        .py(px(3.0))
                        .rounded(px(6.0))
                        .bg(badge_bg)
                        .text_color(gpui::rgb(0xFFFFFF))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(file_badge),
                ),
        )
        // Right click - context menu (for selected items or just clicked item)
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                // Mark that an item was clicked (prevent background deselection)
                ITEM_CLICKED.store(true, Ordering::SeqCst);
                // If the clicked item is selected, show context menu for all selected
                // Otherwise, show context menu for just the clicked item
                let paths = if this.is_path_selected(&path_for_ctx) && this.has_selection() {
                    this.get_selected_paths()
                } else {
                    vec![path_for_ctx.clone()]
                };
                this.handle_action(
                    GalleryAction::ContextMenu {
                        paths,
                        position: event.position,
                    },
                    cx,
                );
            }),
        )
        // Drag operation - use Windows DragDetect for threshold detection
        .on_mouse_down(
            MouseButton::Left,
            cx.listener({
                let drag_paths = drag_paths.clone();
                let path_for_select = path.clone();
                let path_for_dblclick = path_for_dbl.clone();
                move |this, event: &MouseDownEvent, _, cx| {
                    // Mark that an item was clicked (prevent background deselection)
                    ITEM_CLICKED.store(true, Ordering::SeqCst);

                    if drag_paths.is_empty() {
                        return;
                    }

                    // Check for double-click BEFORE drag detection
                    let now = Instant::now();
                    let is_double_click = {
                        let mut last_click = LAST_CLICK.lock().unwrap();
                        let is_dbl = if let Some((last_time, last_path)) = last_click.as_ref() {
                            let elapsed = now.duration_since(*last_time).as_millis();
                            elapsed < DOUBLE_CLICK_TIME_MS && last_path == &path_for_dblclick
                        } else {
                            false
                        };

                        if is_dbl {
                            // Clear last click on double-click
                            *last_click = None;
                        } else {
                            // Record this click
                            *last_click = Some((now, path_for_dblclick.clone()));
                        }
                        is_dbl
                    };

                    // If double-click detected, open file immediately and skip drag detection
                    if is_double_click {
                        log::info!("Double-click detected via timer, opening file: {:?}", path_for_dblclick);
                        this.handle_action(GalleryAction::Open(path_for_dblclick.clone()), cx);
                        return;
                    }

                    // NOT a double-click - proceed with normal drag detection (UNCHANGED)
                    // Use Windows DragDetect for threshold detection
                    // This is a modal function that returns true if user dragged past threshold
                    let should_drag = drag_drop::check_drag_threshold();

                    if should_drag {
                        log::info!(
                            "DragDetect returned true, starting native OLE drag with {} files",
                            drag_paths.len()
                        );
                        drag_drop::start_drag(&drag_paths);
                    } else {
                        // User just clicked without dragging - treat as selection
                        log::debug!("DragDetect returned false, treating as click");
                        this.handle_action(
                            GalleryAction::Select {
                                path: path_for_select.clone(),
                                modifiers: Modifiers {
                                    control: event.modifiers.control,
                                    alt: event.modifiers.alt,
                                    shift: event.modifiers.shift,
                                    platform: event.modifiers.platform,
                                    function: event.modifiers.function,
                                },
                            },
                            cx,
                        );
                    }
                }
            }),
        )
}

/// Show Windows shell context menu for multiple files
#[cfg(windows)]
pub fn show_shell_context_menu(paths: &[PathBuf]) {
    use crate::tray::WINDOW_HWND;
    use log::{debug, error, info};
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::UI::Shell::{
        BHID_SFUIObject, IContextMenu, IShellItem, SHCreateItemFromParsingName, CMINVOKECOMMANDINFO,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
        TrackPopupMenu, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
    };

    if paths.is_empty() {
        return;
    }

    info!("Opening context menu for {} files", paths.len());

    // Filter valid paths
    let valid_paths: Vec<_> = paths.iter().filter(|p| p.exists()).collect();
    if valid_paths.is_empty() {
        error!("No valid paths for context menu");
        return;
    }

    // Get window handle
    let hwnd = match *WINDOW_HWND.lock() {
        Some(h) => HWND(h as *mut std::ffi::c_void),
        None => {
            error!("No window handle available for context menu");
            return;
        }
    };

    unsafe {
        // Set foreground window to ensure menu shows
        let _ = SetForegroundWindow(hwnd);

        // Create shell items for all paths
        let mut shell_items: Vec<IShellItem> = Vec::new();
        for path in &valid_paths {
            let wide_path: Vec<u16> = OsStr::new(path)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            match SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None) {
                Ok(item) => shell_items.push(item),
                Err(e) => {
                    debug!("Failed to create shell item for {:?}: {:?}", path, e);
                }
            }
        }

        if shell_items.is_empty() {
            error!("No shell items created");
            return;
        }

        info!("Created {} shell items for context menu", shell_items.len());

        // Get context menu - use first item for now (multi-file support is complex)
        // TODO: Implement full multi-file context menu using IShellFolder::GetUIObjectOf
        let context_menu: IContextMenu = match shell_items[0].BindToHandler(None, &BHID_SFUIObject)
        {
            Ok(cm) => cm,
            Err(e) => {
                error!("Failed to get context menu: {:?}", e);
                return;
            }
        };

        debug!("Got IContextMenu successfully");

        // Create popup menu
        let hmenu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to create popup menu: {:?}", e);
                return;
            }
        };

        // Query context menu items
        if let Err(e) = context_menu.QueryContextMenu(
            hmenu,
            0,
            1,
            0x7FFF,
            windows::Win32::UI::Shell::CMF_NORMAL,
        ) {
            error!("Failed to query context menu: {:?}", e);
            let _ = DestroyMenu(hmenu);
            return;
        }

        // Get cursor position
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        info!("Showing context menu at ({}, {})", pt.x, pt.y);

        // Show menu and get selection
        let cmd = TrackPopupMenu(
            hmenu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );

        // Post WM_NULL to clear menu state
        let _ = PostMessageW(hwnd, WM_NULL, None, None);

        if cmd.0 != 0 {
            let mut invoke_info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                lpVerb: windows::core::PCSTR((cmd.0 as usize - 1) as *const u8),
                nShow: windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL.0 as i32,
                hwnd,
                ..Default::default()
            };
            if let Err(e) = context_menu.InvokeCommand(&mut invoke_info) {
                error!("Failed to invoke context menu command: {:?}", e);
            } else {
                info!("Context menu command executed successfully");
            }
        }

        let _ = DestroyMenu(hmenu);
    }
}

#[cfg(not(windows))]
pub fn show_shell_context_menu(_paths: &[PathBuf]) {
    // Not implemented for non-Windows
}
