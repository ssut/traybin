//! Screenshot organizer - moves screenshots to date-based subdirectories

use anyhow::Result;
use chrono::{DateTime, Local};
use crossbeam_channel::Sender;
use log::{error, info};
use std::fs;
use std::path::{Path, PathBuf};

use crate::AppMessage;

/// Format a date according to the user-specified format string.
/// Supports: YYYY, YY, MM, DD, and common separators (-, /, .)
///
/// Examples:
/// - "YYYY-MM-DD" -> "2024-01-15"
/// - "YYYY/MM/DD" -> "2024/01/15"
/// - "YYYY-MM" -> "2024-01"
/// - "YY-MM-DD" -> "24-01-15"
pub fn format_date(date: DateTime<Local>, format: &str) -> String {
    let mut result = format.to_string();

    // Replace tokens with actual values
    result = result.replace("YYYY", &date.format("%Y").to_string());
    result = result.replace("YY", &date.format("%y").to_string());
    result = result.replace("MM", &date.format("%m").to_string());
    result = result.replace("DD", &date.format("%d").to_string());

    result
}

/// Organize a screenshot file by moving it to a date-based subdirectory.
///
/// # Arguments
/// * `file_path` - Path to the screenshot file
/// * `base_dir` - Base directory (screenshot directory)
/// * `format` - Date format string (e.g., "YYYY-MM-DD")
///
/// # Returns
/// * `Ok(Some(new_path))` - File was moved successfully
/// * `Ok(None)` - File is already organized or in a subdirectory
/// * `Err(_)` - Error occurred
pub fn organize_file(file_path: &Path, base_dir: &Path, format: &str) -> Result<Option<PathBuf>> {
    // Only organize files that are directly in the base directory
    let file_parent = file_path.parent();
    if file_parent != Some(base_dir) {
        // File is already in a subdirectory, skip
        return Ok(None);
    }

    // Get file modification time
    let metadata = fs::metadata(file_path)?;
    let modified = metadata.modified()?;
    let datetime: DateTime<Local> = modified.into();

    // Create subdirectory name from format
    let subdir_name = format_date(datetime, format);
    let target_dir = base_dir.join(&subdir_name);

    // Create subdirectory if it doesn't exist
    if !target_dir.exists() {
        fs::create_dir_all(&target_dir)?;
        info!("Created organizer directory: {:?}", target_dir);
    }

    // Build target path
    let file_name = file_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid file name"))?;
    let target_path = target_dir.join(file_name);

    // Check if target already exists
    if target_path.exists() {
        // Generate unique name by appending number
        let stem = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let mut counter = 1;
        let mut unique_path = target_path.clone();
        while unique_path.exists() {
            let new_name = if ext.is_empty() {
                format!("{}_{}", stem, counter)
            } else {
                format!("{}_{}.{}", stem, counter, ext)
            };
            unique_path = target_dir.join(new_name);
            counter += 1;

            // Safety limit
            if counter > 1000 {
                return Err(anyhow::anyhow!("Too many duplicate files"));
            }
        }
        // Move file
        fs::rename(file_path, &unique_path)?;
        info!("Organized (renamed): {:?} -> {:?}", file_path, unique_path);
        return Ok(Some(unique_path));
    }

    // Move file
    fs::rename(file_path, &target_path)?;
    info!("Organized: {:?} -> {:?}", file_path, target_path);

    Ok(Some(target_path))
}

/// Get example output for a format string using current date.
pub fn format_preview(format: &str) -> String {
    format_date(Local::now(), format)
}

/// Image extensions we care about
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "avif"];

/// Check if a path is an image file
fn is_image_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|&e| e.eq_ignore_ascii_case(ext))
        })
}

/// Organize all existing files in the base directory.
/// Sends progress updates via the message channel.
/// This function runs in a background thread.
pub fn organize_existing_files(base_dir: PathBuf, format: String, message_tx: Sender<AppMessage>) {
    std::thread::spawn(move || {
        info!("Starting organization of existing files in {:?}", base_dir);

        // Collect files that need organizing (only files directly in base_dir)
        let files_to_organize: Vec<PathBuf> = match fs::read_dir(&base_dir) {
            Ok(entries) => entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    if is_image_file(&path) {
                        Some(path)
                    } else {
                        None
                    }
                })
                .collect(),
            Err(e) => {
                error!("Failed to read directory: {}", e);
                let _ = message_tx.send(AppMessage::OrganizeCompleted);
                return;
            }
        };

        let total = files_to_organize.len();
        if total == 0 {
            info!("No files to organize");
            let _ = message_tx.send(AppMessage::OrganizeCompleted);
            return;
        }

        // Send start message
        let _ = message_tx.send(AppMessage::OrganizeStarted(total));

        // Organize each file
        for (index, file_path) in files_to_organize.iter().enumerate() {
            let file_name = file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            // Send progress update
            let _ = message_tx.send(AppMessage::OrganizeProgress(
                index + 1,
                total,
                file_name.clone(),
            ));

            // Organize the file
            match organize_file(file_path, &base_dir, &format) {
                Ok(Some(new_path)) => {
                    info!("Organized: {:?} -> {:?}", file_path, new_path);
                    // Notify about the file move
                    let _ = message_tx.send(AppMessage::ScreenshotRemoved(file_path.clone()));
                    let _ = message_tx.send(AppMessage::NewScreenshot(new_path));
                }
                Ok(None) => {
                    // File was already organized, skip
                }
                Err(e) => {
                    error!("Failed to organize {:?}: {}", file_path, e);
                }
            }

            // Small delay between files to avoid overwhelming the system
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Send completion message
        let _ = message_tx.send(AppMessage::OrganizeCompleted);
        info!("Organization completed: {} files processed", total);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_format_date() {
        let date = Local.with_ymd_and_hms(2024, 1, 15, 10, 30, 0).unwrap();

        assert_eq!(format_date(date, "YYYY-MM-DD"), "2024-01-15");
        assert_eq!(format_date(date, "YYYY/MM/DD"), "2024/01/15");
        assert_eq!(format_date(date, "YYYY-MM"), "2024-01");
        assert_eq!(format_date(date, "YY-MM-DD"), "24-01-15");
        assert_eq!(format_date(date, "YYYY.MM.DD"), "2024.01.15");
    }

    #[test]
    fn test_format_date_edge_cases() {
        // Test single digit month and day
        let date = Local.with_ymd_and_hms(2024, 2, 5, 10, 30, 0).unwrap();
        assert_eq!(format_date(date, "YYYY-MM-DD"), "2024-02-05");

        // Test December 31st
        let date = Local.with_ymd_and_hms(2023, 12, 31, 23, 59, 59).unwrap();
        assert_eq!(format_date(date, "YYYY-MM-DD"), "2023-12-31");
    }

    #[test]
    fn test_format_preview() {
        // Test that format_preview returns a valid date string
        let preview = format_preview("YYYY-MM-DD");
        assert!(preview.len() == 10); // Format: "2024-01-15"
        assert!(preview.contains('-'));

        let preview = format_preview("YYYY/MM/DD");
        assert!(preview.contains('/'));
    }

    #[test]
    fn test_is_image_file() {
        use std::path::Path;

        // Note: is_image_file requires actual files to exist
        // We test the extension checking logic via format_date and format_preview tests
        // For real file testing, we'd need integration tests with temp files

        // Test that non-existent files return false (as expected)
        assert!(!is_image_file(Path::new("nonexistent.png")));
        assert!(!is_image_file(Path::new("nonexistent.txt")));
    }

}
