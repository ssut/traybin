//! Application settings and persistence

use anyhow::Result;
use directories::ProjectDirs;
use log::info;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Supported conversion formats
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversionFormat {
    WebP,
    Jpeg,
}

impl Default for ConversionFormat {
    fn default() -> Self {
        ConversionFormat::WebP
    }
}

impl ConversionFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ConversionFormat::WebP => "webp",
            ConversionFormat::Jpeg => "jpg",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ConversionFormat::WebP => "WebP",
            ConversionFormat::Jpeg => "JPEG",
        }
    }
}

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Directory to watch for screenshots
    pub screenshot_directory: PathBuf,

    /// Number of columns in the gallery grid
    pub grid_columns: u32,

    /// Thumbnail size in pixels
    pub thumbnail_size: u32,

    /// Auto-convert new screenshots
    pub auto_convert_webp: bool,

    /// Conversion format (WebP or JPEG)
    #[serde(default)]
    pub conversion_format: ConversionFormat,

    /// Conversion quality (0-100)
    pub webp_quality: u32,

    /// Window width
    pub window_width: f32,

    /// Window height
    pub window_height: f32,

    /// Global hotkey enabled
    #[serde(default = "default_hotkey_enabled")]
    pub hotkey_enabled: bool,

    /// Global hotkey string (e.g., "Ctrl+Shift+S")
    #[serde(default = "default_hotkey")]
    pub hotkey: String,

    /// Screenshot organizer enabled
    #[serde(default)]
    pub organizer_enabled: bool,

    /// Screenshot organizer date format (e.g., "YYYY-MM-DD", "YYYY/MM/DD", "YYYY-MM")
    #[serde(default = "default_organizer_format")]
    pub organizer_format: String,

    /// Vector search indexing enabled
    #[serde(default)]
    pub indexing_enabled: bool,

    /// Indexing CPU mode ("normal" or "fast")
    #[serde(default = "default_cpu_mode")]
    pub indexing_cpu_mode: String,

    /// Whether embedding models have been downloaded
    #[serde(default)]
    pub models_downloaded: bool,

    /// Last indexed image count (for stats display)
    #[serde(default)]
    pub last_indexed_count: usize,
}

fn default_hotkey_enabled() -> bool {
    true
}

fn default_hotkey() -> String {
    "Ctrl+Shift+S".to_string()
}

fn default_organizer_format() -> String {
    "YYYY-MM-DD".to_string()
}

fn default_cpu_mode() -> String {
    "normal".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            screenshot_directory: Self::default_screenshot_directory(),
            grid_columns: 4,
            thumbnail_size: 150,
            auto_convert_webp: false,
            conversion_format: ConversionFormat::WebP,
            webp_quality: 85,
            window_width: 850.0,
            window_height: 650.0,
            hotkey_enabled: true,
            hotkey: "Ctrl+Shift+S".to_string(),
            organizer_enabled: false,
            organizer_format: "YYYY-MM-DD".to_string(),
            indexing_enabled: false,
            indexing_cpu_mode: "normal".to_string(),
            models_downloaded: false,
            last_indexed_count: 0,
        }
    }
}

impl Settings {
    /// Get the default Windows screenshot directory
    fn default_screenshot_directory() -> PathBuf {
        if let Some(user_dirs) = directories::UserDirs::new() {
            if let Some(pictures) = user_dirs.picture_dir() {
                let screenshots = pictures.join("Screenshots");
                if screenshots.exists() {
                    return screenshots;
                }
            }
        }

        dirs::home_dir()
            .map(|h| h.join("Pictures").join("Screenshots"))
            .unwrap_or_else(|| PathBuf::from("C:\\Users\\Public\\Pictures\\Screenshots"))
    }

    /// Get the config file path
    pub fn config_path() -> Option<PathBuf> {
        ProjectDirs::from("com", "sukusho", "Sukusho")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    /// Load settings from disk
    pub fn load() -> Result<Self> {
        let path = Self::config_path()
            .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;

        if !path.exists() {
            info!("No settings file found, using defaults");
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)?;
        let settings: Self = serde_json::from_str(&content)?;

        info!("Loaded settings from {:?}", path);
        Ok(settings)
    }

    /// Save settings to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()
            .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        fs::write(&path, content)?;

        info!("Saved settings to {:?}", path);
        Ok(())
    }
}

mod dirs {
    use std::path::PathBuf;

    pub fn home_dir() -> Option<PathBuf> {
        directories::UserDirs::new().map(|d| d.home_dir().to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversion_format_default() {
        let format = ConversionFormat::default();
        assert_eq!(format, ConversionFormat::WebP);
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
    fn test_settings_default() {
        let settings = Settings::default();

        // Check default values
        assert_eq!(settings.grid_columns, 4);
        assert_eq!(settings.thumbnail_size, 150);
        assert_eq!(settings.auto_convert_webp, false);
        assert_eq!(settings.conversion_format, ConversionFormat::WebP);
        assert_eq!(settings.webp_quality, 85);
        assert_eq!(settings.window_width, 850.0);
        assert_eq!(settings.window_height, 650.0);
        assert_eq!(settings.hotkey_enabled, true);
        assert_eq!(settings.hotkey, "Ctrl+Shift+S");
        assert_eq!(settings.organizer_enabled, false);
        assert_eq!(settings.organizer_format, "YYYY-MM-DD");
    }

    #[test]
    fn test_settings_serialization() {
        let settings = Settings::default();

        // Serialize to JSON
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("grid_columns"));
        assert!(json.contains("auto_convert_webp"));
        assert!(json.contains("organizer_enabled"));

        // Deserialize back
        let deserialized: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.grid_columns, settings.grid_columns);
        assert_eq!(deserialized.auto_convert_webp, settings.auto_convert_webp);
        assert_eq!(deserialized.organizer_enabled, settings.organizer_enabled);
    }

    #[test]
    fn test_settings_with_custom_values() {
        let json = r#"{
            "screenshot_directory": "/custom/path",
            "grid_columns": 6,
            "thumbnail_size": 200,
            "auto_convert_webp": true,
            "conversion_format": "Jpeg",
            "webp_quality": 90,
            "window_width": 1024.0,
            "window_height": 768.0,
            "hotkey_enabled": false,
            "hotkey": "Ctrl+Alt+S",
            "organizer_enabled": true,
            "organizer_format": "YYYY/MM/DD"
        }"#;

        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.grid_columns, 6);
        assert_eq!(settings.thumbnail_size, 200);
        assert_eq!(settings.auto_convert_webp, true);
        assert_eq!(settings.conversion_format, ConversionFormat::Jpeg);
        assert_eq!(settings.webp_quality, 90);
        assert_eq!(settings.hotkey_enabled, false);
        assert_eq!(settings.hotkey, "Ctrl+Alt+S");
        assert_eq!(settings.organizer_enabled, true);
        assert_eq!(settings.organizer_format, "YYYY/MM/DD");
    }

    #[test]
    fn test_conversion_format_roundtrip() {
        // Test WebP
        let format = ConversionFormat::WebP;
        let json = serde_json::to_string(&format).unwrap();
        let deserialized: ConversionFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ConversionFormat::WebP);

        // Test Jpeg
        let format = ConversionFormat::Jpeg;
        let json = serde_json::to_string(&format).unwrap();
        let deserialized: ConversionFormat = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, ConversionFormat::Jpeg);
    }

    #[test]
    fn test_quality_bounds() {
        let settings = Settings::default();

        // Default quality should be in valid range
        assert!(settings.webp_quality >= 1);
        assert!(settings.webp_quality <= 100);
    }
}
