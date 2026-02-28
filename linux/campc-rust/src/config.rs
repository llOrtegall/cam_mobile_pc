use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    pub zoom: f32,
    pub fps: u32,
    pub rotation: u32,      // 0 | 90 | 180 | 270
    pub resolution: String, // "720p" | "1080p" | "480p"
    pub v4l2_device: String,
    pub adb_port: u16,
    pub preview_fps: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            fps: 30,
            rotation: 0,
            resolution: "720p".to_string(),
            v4l2_device: "/dev/video10".to_string(),
            adb_port: 5000,
            preview_fps: 15,
        }
    }
}

impl Config {
    fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join(".config")
            .join("campc")
            .join("config.toml")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(content) = toml::to_string_pretty(self) {
            let _ = std::fs::write(path, content);
        }
    }

    pub fn resolution_dims(&self) -> (u32, u32) {
        match self.resolution.as_str() {
            "1080p" => (1920, 1080),
            "480p" => (854, 480),
            _ => (1280, 720),
        }
    }
}
