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
    /// Virtual screens to create on the host (0 = none, up to 4).
    pub vd_count: u32,
    /// Switch the host's physical displays off (with at least one virtual screen).
    pub physical_off: bool,
    /// Block the host's own keyboard and mouse.
    pub block_input: bool,
    /// Virtual display size: "window" (follows this window) | "screen" | "fixed"
    pub vd_size: String,
    pub vd_width: u32,
    pub vd_height: u32,
    /// Give the virtual display this computer's display scaling.
    pub vd_scale: bool,
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
            vd_count: 0,
            physical_off: false,
            block_input: false,
            vd_size: "window".into(),
            vd_width: 1920,
            vd_height: 1080,
            vd_scale: true,
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

    /// Rename host `i`. Names are how hosts are picked on the command line, so
    /// they must stay non-empty and unique.
    pub fn rename(&mut self, i: usize, name: &str) -> Result<(), &'static str> {
        let name = name.trim();
        if name.is_empty() {
            return Err("名称不能为空");
        }
        if self.hosts.iter().enumerate().any(|(j, h)| j != i && (h.name == name || h.address == name)) {
            return Err("已有同名的被控端");
        }
        let h = self.hosts.get_mut(i).ok_or("被控端不存在")?;
        h.name = name.to_owned();
        Ok(())
    }

    /// Save a host after connecting. Matched by address; a name that another
    /// host already uses gets a number appended instead of replacing it.
    pub fn upsert(&mut self, mut entry: HostEntry) {
        if let Some(i) = self.hosts.iter().position(|h| h.address == entry.address) {
            let name_free = !entry.name.trim().is_empty() && !self.hosts.iter().any(|o| o.name == entry.name.trim());
            let h = &mut self.hosts[i];
            h.fingerprint = entry.fingerprint;
            if name_free {
                h.name = entry.name.trim().to_owned();
            }
            return;
        }
        let base = if entry.name.trim().is_empty() { entry.address.clone() } else { entry.name.trim().to_owned() };
        entry.name = base.clone();
        let mut n = 2;
        while self.hosts.iter().any(|h| h.name == entry.name) {
            entry.name = format!("{base} ({n})");
            n += 1;
        }
        self.hosts.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, address: &str) -> HostEntry {
        HostEntry { name: name.into(), address: address.into(), fingerprint: String::new() }
    }

    #[test]
    fn upsert_and_rename() {
        let mut c = ClientConfig::default();
        c.upsert(host("pc", "10.0.0.1"));
        c.upsert(host("pc", "10.0.0.2"));
        assert_eq!(c.hosts[1].name, "pc (2)");
        // Reconnecting keeps a user-chosen name.
        c.rename(0, " office ").unwrap();
        c.upsert(HostEntry { fingerprint: "ab".into(), ..host("pc (2)", "10.0.0.1") });
        assert_eq!((c.hosts[0].name.as_str(), c.hosts[0].fingerprint.as_str()), ("office", "ab"));
        assert!(c.rename(0, "pc (2)").is_err());
        assert!(c.rename(0, "  ").is_err());
        assert!(c.rename(1, "10.0.0.1").is_err());
    }
}
