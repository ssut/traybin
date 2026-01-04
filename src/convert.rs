//! Image conversion utilities

use anyhow::{Context, Result};
use filetime::{set_file_mtime, FileTime};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::io::Reader as ImageReader;
use log::{error, info};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::settings::ConversionFormat;

/// Convert an image to the specified format
///
/// Returns the path to the new file if successful.
/// The original file is deleted after successful conversion.
/// Preserves the original file's modification timestamp.
pub fn convert_image(
    source_path: &Path,
    format: ConversionFormat,
    quality: u32,
) -> Result<PathBuf> {
    info!(
        "Converting to {:?}: {:?} (quality: {})",
        format, source_path, quality
    );

    // Only convert PNG files
    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if !ext.eq_ignore_ascii_case("png") {
        anyhow::bail!("Only PNG files can be converted");
    }

    // Wait a bit to ensure the source file is fully written
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Get original file's modification time BEFORE reading
    let original_mtime = fs::metadata(source_path).and_then(|m| m.modified()).ok();

    // Read the source image
    let img = ImageReader::open(source_path)
        .context("Failed to open source image")?
        .decode()
        .context("Failed to decode source image")?;

    // Create output path with appropriate extension
    let output_path = source_path.with_extension(format.extension());

    // Create output file
    let output_file = fs::File::create(&output_path).context(format!(
        "Failed to create output {} file",
        format.display_name()
    ))?;

    let mut writer = BufWriter::new(output_file);

    // Encode based on format
    match format {
        ConversionFormat::WebP => {
            // Use lossless encoding (image crate 0.24 doesn't support lossy quality setting directly)
            let encoder = WebPEncoder::new_lossless(&mut writer);
            img.write_with_encoder(encoder)
                .context("Failed to encode WebP image")?;
        }
        ConversionFormat::Jpeg => {
            // JPEG supports quality setting (1-100)
            let encoder = JpegEncoder::new_with_quality(&mut writer, quality.clamp(1, 100) as u8);
            img.write_with_encoder(encoder)
                .context("Failed to encode JPEG image")?;
        }
    }

    // Ensure buffer is flushed to disk
    writer.flush().context("Failed to flush output file")?;
    drop(writer);

    // Verify the file was created successfully and has content
    let output_meta = fs::metadata(&output_path).context("Output file not created")?;
    if output_meta.len() == 0 {
        anyhow::bail!("Output file is empty");
    }

    // Preserve original file's modification time on the new file
    if let Some(mtime) = original_mtime {
        let file_time = FileTime::from_system_time(mtime);
        if let Err(e) = set_file_mtime(&output_path, file_time) {
            error!("Failed to preserve modification time: {}", e);
        } else {
            info!("Preserved original modification time on output file");
        }
    }

    let original_size = fs::metadata(source_path).map(|m| m.len()).unwrap_or(0);
    let output_size = output_meta.len();

    info!(
        "{} conversion complete: {:?} -> {:?} ({} bytes -> {} bytes, {:.1}% of original)",
        format.display_name(),
        source_path,
        output_path,
        original_size,
        output_size,
        (output_size as f64 / original_size as f64) * 100.0
    );

    // Delete the original file after successful conversion
    if let Err(e) = fs::remove_file(source_path) {
        error!(
            "Failed to delete original file after conversion: {:?} - {}",
            source_path, e
        );
    } else {
        info!("Deleted original file: {:?}", source_path);
    }

    Ok(output_path)
}

/// Check if a file is a PNG that can be converted
pub fn is_convertible(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_convertible() {
        assert!(is_convertible(Path::new("test.png")));
        assert!(is_convertible(Path::new("test.PNG")));
        assert!(!is_convertible(Path::new("test.jpg")));
        assert!(!is_convertible(Path::new("test.webp")));
    }

    #[test]
    fn test_is_convertible_edge_cases() {
        // Test mixed case
        assert!(is_convertible(Path::new("test.PnG")));
        assert!(is_convertible(Path::new("test.pNg")));

        // Test files with multiple dots
        assert!(is_convertible(Path::new("test.backup.png")));
        assert!(!is_convertible(Path::new("test.backup.jpg")));

        // Test files without extensions
        assert!(!is_convertible(Path::new("test")));

        // Test other image formats (should not be convertible)
        assert!(!is_convertible(Path::new("test.gif")));
        assert!(!is_convertible(Path::new("test.bmp")));
        assert!(!is_convertible(Path::new("test.avif")));
    }

    #[test]
    fn test_conversion_format_extension() {
        assert_eq!(ConversionFormat::WebP.extension(), "webp");
        assert_eq!(ConversionFormat::Jpeg.extension(), "jpg");
    }

    #[test]
    fn test_conversion_format_display_name() {
        assert_eq!(ConversionFormat::WebP.display_name(), "WebP");
        assert_eq!(ConversionFormat::Jpeg.display_name(), "JPEG");
    }

    #[test]
    fn test_conversion_format_default() {
        let default = ConversionFormat::default();
        assert_eq!(default, ConversionFormat::WebP);
    }
}
