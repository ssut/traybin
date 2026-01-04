//! File system watcher for screenshot directory

use anyhow::Result;
use crossbeam_channel::Sender;
use log::{debug, error, info, warn};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::convert;
use crate::organizer;
use crate::settings::Settings;
use crate::AppMessage;

/// Image extensions we care about
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "avif"];

pub struct ScreenshotWatcher {
    directory: PathBuf,
    message_tx: Sender<AppMessage>,
    settings: Arc<Mutex<Settings>>,
}

impl ScreenshotWatcher {
    pub fn new(
        directory: PathBuf,
        message_tx: Sender<AppMessage>,
        settings: Arc<Mutex<Settings>>,
    ) -> Self {
        Self {
            directory,
            message_tx,
            settings,
        }
    }

    /// Run the watcher (blocking)
    pub fn run(self) -> Result<()> {
        info!("Starting file watcher for: {:?}", self.directory);

        // Ensure directory exists
        if !self.directory.exists() {
            warn!(
                "Screenshot directory does not exist, creating: {:?}",
                self.directory
            );
            std::fs::create_dir_all(&self.directory)?;
        }

        // Scan existing files first (includes subdirectories for organized files)
        self.scan_existing_files()?;

        // Create debounced watcher
        let tx = self.message_tx.clone();
        let base_dir = self.directory.clone();
        let settings = Arc::clone(&self.settings);
        let mut debouncer = new_debouncer(
            Duration::from_millis(200),
            None,
            move |result: DebounceEventResult| {
                Self::handle_debounced_events(result, &tx, &base_dir, &settings);
            },
        )?;

        // Watch the directory recursively to detect deletions in subdirectories
        debouncer.watch(&self.directory, RecursiveMode::Recursive)?;

        info!("File watcher started successfully");

        // Keep the thread alive
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

    /// Scan existing files in the directory (recursive to include organized subdirectories)
    fn scan_existing_files(&self) -> Result<()> {
        info!("Scanning existing screenshots...");
        let mut count = 0;
        let mut files = Vec::new();

        // Recursive scan function
        fn scan_dir(dir: &Path, files: &mut Vec<PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        // Recurse into subdirectories
                        scan_dir(&path, files);
                    } else if ScreenshotWatcher::is_image_file(&path) {
                        files.push(path);
                    }
                }
            }
        }

        scan_dir(&self.directory, &mut files);

        // Sort by modified time (newest first)
        files.sort_by(|a, b| {
            let a_time = std::fs::metadata(a).and_then(|m| m.modified()).ok();
            let b_time = std::fs::metadata(b).and_then(|m| m.modified()).ok();
            b_time.cmp(&a_time)
        });

        for path in files {
            debug!("Found existing screenshot: {:?}", path);
            let _ = self.message_tx.send(AppMessage::NewScreenshot(path));
            count += 1;
        }

        info!("Found {} existing screenshots", count);
        Ok(())
    }

    /// Handle debounced file system events
    fn handle_debounced_events(
        result: DebounceEventResult,
        tx: &Sender<AppMessage>,
        base_dir: &Path,
        settings: &Arc<Mutex<Settings>>,
    ) {
        match result {
            Ok(events) => {
                for event in events {
                    Self::process_event(&event, tx, base_dir, settings);
                }
            }
            Err(errors) => {
                for e in errors {
                    error!("File watcher error: {:?}", e);
                }
            }
        }
    }

    /// Process a single debounced event
    fn process_event(
        event: &notify_debouncer_full::DebouncedEvent,
        tx: &Sender<AppMessage>,
        base_dir: &Path,
        settings: &Arc<Mutex<Settings>>,
    ) {
        use notify::EventKind;

        for path in &event.paths {
            // For Remove events, file no longer exists so we only check extension
            // For other events, we check if it's actually a file
            let dominated_event = match &event.kind {
                EventKind::Remove(_) => Self::has_image_extension(path),
                _ => Self::is_image_file(path),
            };

            if !dominated_event {
                continue;
            }

            match &event.kind {
                EventKind::Create(_) => {
                    info!("New screenshot detected: {:?}", path);

                    // Check if organizer and/or auto-convert is enabled
                    let (organizer_enabled, organizer_format, auto_convert, conversion_format, quality) = {
                        let s = settings.lock();
                        (
                            s.organizer_enabled,
                            s.organizer_format.clone(),
                            s.auto_convert_webp,
                            s.conversion_format,
                            s.webp_quality,
                        )
                    };

                    // Process in background thread
                    let path_clone = path.clone();
                    let base_dir = base_dir.to_path_buf();
                    let tx = tx.clone();

                    std::thread::spawn(move || {
                        // Small delay to ensure file is fully written
                        std::thread::sleep(Duration::from_millis(500));

                        let mut current_path = path_clone.clone();

                        // Step 1: Auto-convert if enabled (PNG -> WebP/JPEG)
                        if auto_convert && convert::is_convertible(&current_path) {
                            info!("Auto-converting screenshot: {:?}", current_path);
                            match convert::convert_image(&current_path, conversion_format, quality) {
                                Ok(new_path) => {
                                    info!("Converted: {:?} -> {:?}", current_path, new_path);
                                    current_path = new_path;
                                }
                                Err(e) => {
                                    error!("Failed to convert screenshot: {}", e);
                                }
                            }
                        }

                        // Step 2: Organize if enabled (move to date-based subdirectory)
                        if organizer_enabled {
                            match organizer::organize_file(
                                &current_path,
                                &base_dir,
                                &organizer_format,
                            ) {
                                Ok(Some(new_path)) => {
                                    info!("Organized: {:?} -> {:?}", current_path, new_path);
                                    current_path = new_path;
                                }
                                Ok(None) => {
                                    // Already organized or in subdirectory
                                }
                                Err(e) => {
                                    error!("Failed to organize screenshot: {}", e);
                                }
                            }
                        }

                        // Send final path to UI
                        let _ = tx.send(AppMessage::NewScreenshot(current_path));
                    });
                }
                EventKind::Remove(_) => {
                    info!("Screenshot removed: {:?}", path);
                    let _ = tx.send(AppMessage::ScreenshotRemoved(path.clone()));
                }
                EventKind::Modify(_) => {
                    // Modification might mean the file is fully written
                    debug!("Screenshot modified: {:?}", path);
                }
                _ => {}
            }
        }
    }

    /// Check if a path is an image file we care about (file must exist)
    fn is_image_file(path: &Path) -> bool {
        if !path.is_file() {
            return false;
        }
        Self::has_image_extension(path)
    }

    /// Check if a path has an image extension (doesn't check if file exists)
    /// Used for Remove events where the file no longer exists
    fn has_image_extension(path: &Path) -> bool {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                IMAGE_EXTENSIONS
                    .iter()
                    .any(|&e| e.eq_ignore_ascii_case(ext))
            })
    }
}
