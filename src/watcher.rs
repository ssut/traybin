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

                    // Check if organizer is enabled
                    let (organizer_enabled, organizer_format) = {
                        let s = settings.lock();
                        (s.organizer_enabled, s.organizer_format.clone())
                    };

                    if organizer_enabled {
                        // Organize the file (move to date-based subdirectory)
                        let path_clone = path.clone();
                        let base_dir = base_dir.to_path_buf();
                        let tx = tx.clone();

                        std::thread::spawn(move || {
                            // Small delay to ensure file is fully written
                            std::thread::sleep(Duration::from_millis(500));

                            match organizer::organize_file(
                                &path_clone,
                                &base_dir,
                                &organizer_format,
                            ) {
                                Ok(Some(new_path)) => {
                                    // File was moved, send the new path
                                    info!(
                                        "Organized screenshot: {:?} -> {:?}",
                                        path_clone, new_path
                                    );
                                    let _ = tx.send(AppMessage::NewScreenshot(new_path));
                                }
                                Ok(None) => {
                                    // File was already organized or in subdirectory
                                    let _ = tx.send(AppMessage::NewScreenshot(path_clone));
                                }
                                Err(e) => {
                                    // Organization failed, send original path
                                    error!("Failed to organize screenshot: {}", e);
                                    let _ = tx.send(AppMessage::NewScreenshot(path_clone));
                                }
                            }
                        });
                    } else {
                        let _ = tx.send(AppMessage::NewScreenshot(path.clone()));
                    }
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
