use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("NyaRemoteControl").join("client")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostEntry {
    pub name: String,
    pub address: String,
    /// Pinned server certificate fingerprint (hex).
    #[serde(default)]
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// "office" | "game"
    pub mode: String,
    pub fullscreen: bool,
    /// Display id on the host; 0 = primary.
    pub display: u32,
    /// 0 = let the host decide.
    pub bitrate_kbps: u32,
    /// Ask for the highest bitrate the encoder allows (ignores `bitrate_kbps`).
    pub unlimited_bitrate: bool,
    /// "auto" | "quality" | "balanced" | "smooth" | "fixed"
    pub bitrate_policy: String,
    /// 0 = local monitor refresh rate.
    pub max_fps: u32,
    /// "auto" | "nvenc" | "qsv" | "amf" | "software"
    pub encoder: String,
    /// "auto" | "h264" | "hevc" | "av1"
    pub codec: String,
    /// "auto" | "420" | "444"
    pub chroma: String,
    pub audio: bool,
    pub clipboard: bool,
    /// Use hardware decoding when possible.
    pub hw_decode: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            mode: "office".into(),
            fullscreen: false,
            display: 0,
            bitrate_kbps: 0,
            unlimited_bitrate: false,
            bitrate_policy: "auto".into(),
            max_fps: 0,
            encoder: "auto".into(),
            codec: "auto".into(),
            chroma: "auto".into(),
            audio: true,
            clipboard: true,
            hw_decode: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub hosts: Vec<HostEntry>,
}

impl ClientConfig {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("client.toml");
        if !path.exists() {
            let c = Self::default();
            c.save(dir)?;
            return Ok(c);
        }
        let text = std::fs::read_to_string(&path)?;
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("client.toml"), toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Find by name or address.
    pub fn find(&self, key: &str) -> Option<&HostEntry> {
        self.hosts.iter().find(|h| h.name == key || h.address == key)
    }

    pub fn upsert(&mut self, entry: HostEntry) {
        if let Some(h) = self.hosts.iter_mut().find(|h| h.address == entry.address || h.name == entry.name) {
            *h = entry;
        } else {
            self.hosts.push(entry);
        }
    }
}
