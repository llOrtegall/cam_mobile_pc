use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionMode {
    #[default]
    Wifi,
    Usb,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Config {
    pub fps: u32,
    pub rotation: u32,      // 0 | 90 | 180 | 270
    pub v4l2_device: String,
    pub adb_port: u16,
    pub preview_fps: u32,
    pub connection_mode: ConnectionMode,
    /// Manual WiFi IP override. Empty string = auto-discover via UDP beacon.
    pub wifi_ip: String,
    /// Preview zoom factor (1.0 = fit-to-canvas, up to 4.0).
    /// Purely a GUI setting — V4L2 output is always full 1920×1080.
    pub zoom: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            fps: 30,
            rotation: 0,
            v4l2_device: "/dev/video10".to_string(),
            adb_port: 5000,
            preview_fps: 15,
            connection_mode: ConnectionMode::Wifi,
            wifi_ip: String::new(),
            zoom: 1.0,
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
}
