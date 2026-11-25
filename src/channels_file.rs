//! Hand-editable `channels.json` lineup file, per-channel overrides, and Channel Type -> TV Profile automation.
//!
//! Lives at `<config_dir>/victr/channels.json` and hot-reloads automatically whenever saved on disk.
//!
//! ### What you can put in `channels.json`
//! - **`type_profiles`**: Maps semantic channel types (`"series"`, `"movies"`, `"live"`, `"web"`,
//!   or custom tags like `"broadcast"`, `"vhs"`, `"weather"`) to any saved ViCTR user preset or built-in preset name.
//!   When you tune a channel, ViCTR automatically switches the TV profile to match!
//! - **`default_profile`**: Optional fallback TV profile for channels whose type isn't mapped.
//! - **`channels`**: Inline channel definitions (`live`, `web`, `series`, `movies`, or `folder`) with optional
//!   pinned `number`, `callsign`, `type`, and direct `profile` override.
//! - **`overrides`**: Match any channel from Jellyfin, Plex, Local Folders, M3U8, or Custom Integrators
//!   to pin its channel number, rename it, assign a `type` (e.g. `"vhs"`), set a `profile`, favorite, or hide it.

use crate::provider::{
    ArtworkImage, Channel, ChannelKind, MediaItem, default_channel_type, make_callsign,
    pick_next_episode_index,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChannelsFileConfig {
    /// Maps channel type (e.g. "broadcast", "movies", "vhs", "live", "web") -> TV preset/profile name.
    #[serde(default)]
    pub type_profiles: BTreeMap<String, String>,
    /// Optional fallback TV profile/preset name when a channel's type has no entry in `type_profiles`.
    #[serde(default)]
    pub default_profile: String,
    /// Inline channels defined directly in `channels.json`.
    #[serde(default)]
    pub channels: Vec<FileChannelDef>,
    /// Override rules applied to channels from every provider (Jellyfin, Plex, Folders, M3U8, Custom).
    #[serde(default)]
    pub overrides: Vec<ChannelOverride>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileChannelDef {
    /// Optional pinned channel number (e.g. `36`). Omit or set `0` for automatic placement.
    #[serde(default)]
    pub number: Option<u16>,
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub callsign: String,
    /// `"live"`, `"web"`, `"series"`, `"movies"`, or `"folder"`.
    #[serde(default)]
    pub kind: String,
    /// Semantic channel type mapped through `type_profiles` (e.g. `"broadcast"`, `"vhs"`, `"weather"`).
    #[serde(rename = "type", default)]
    pub channel_type: String,
    /// Optional direct TV preset/profile override for this channel.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub stream_url: String,
    /// Optional local folder path when `kind == "folder"`.
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub genre: String,
    #[serde(default)]
    pub now_title: String,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub artwork_url: String,
    #[serde(default)]
    pub is_favorite: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub items: Vec<FileMediaItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileMediaItem {
    #[serde(default)]
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub season: Option<u32>,
    #[serde(default)]
    pub episode: Option<u32>,
    #[serde(default)]
    pub overview: String,
    #[serde(default)]
    pub runtime_secs: Option<f64>,
    #[serde(default)]
    pub artwork_url: String,
    pub stream_url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChannelOverride {
    /// Matches channel name, callsign, or source_key (case-insensitive exact or substring, or `"*"` for all).
    #[serde(rename = "match", default)]
    pub match_text: String,
    /// Optional provider filter: `"jellyfin"`, `"plex"`, `"folders"`, `"m3u"`, `"custom"`, `"channels"`.
    #[serde(default)]
    pub provider: String,
    /// Optional kind filter: `"series"`, `"movies"`, `"live"`, `"web"`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub number: Option<u16>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub callsign: Option<String>,
    #[serde(rename = "type", default)]
    pub channel_type: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub favorite: Option<bool>,
    #[serde(default)]
    pub hidden: bool,
}

pub fn config_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("channels.json")))
}

pub fn load_config_from(path: &Path) -> Result<ChannelsFileConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid channels.json: {e}"))
}

pub fn save_config_to(path: &Path, cfg: &ChannelsFileConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save channels.json: {e}"))
}

/// Builds inline `Channel` records from `cfg.channels`.
pub fn build_inline_channels(cfg: &ChannelsFileConfig) -> Vec<Channel> {
    let mut out = Vec::new();
    for (idx, def) in cfg.channels.iter().enumerate() {
        if !def.enabled {
            continue;
        }
        let name = def.name.trim().to_string();
        let kind_norm = def.kind.trim().to_ascii_lowercase();
        let ch_id = if def.id.trim().is_empty() {
            format!("cfg:{idx}:{}", if name.is_empty() { "ch" } else { &name })
        } else {
            format!("cfg:{}", def.id.trim())
        };

        // Special case: `kind: "folder"` scans a local folder and tags all resulting channels
        if kind_norm == "folder" {
            if def.path.trim().is_empty() {
                continue;
            }
            let fcfg = crate::folders::FoldersConfig {
                roots: vec![crate::folders::FolderRoot {
                    path: def.path.trim().to_string(),
                    label: name.clone(),
                    enabled: true,
                }],
                watch_state: HashMap::new(),
            };
            let mut scanned = crate::folders::scan_roots(&fcfg);
            for (si, ch) in scanned.iter_mut().enumerate() {
                if !def.channel_type.trim().is_empty() {
                    ch.channel_type = def.channel_type.trim().to_string();
                }
                if def.profile.is_some() {
                    ch.profile_override = def.profile.clone();
                }
                if si == 0 {
                    if let Some(n) = def.number.filter(|&n| n >= 2) {
                        ch.number = n;
                    }
                    if !def.callsign.trim().is_empty() {
                        ch.callsign = def.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase();
                    }
                }
            }
            out.extend(scanned);
            continue;
        }

        if name.is_empty() {
            continue;
        }
        let callsign = if !def.callsign.trim().is_empty() {
            def.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase()
        } else {
            make_callsign(&name)
        };
        let inferred = if !kind_norm.is_empty() {
            kind_norm.as_str()
        } else if !def.stream_url.trim().is_empty() && def.items.is_empty() {
            if crate::web_channel::is_web_channel_url(&def.stream_url) {
                "web"
            } else {
                "live"
            }
        } else if def.items.iter().any(|i| i.season.is_some() || i.episode.is_some()) {
            "series"
        } else {
            "movies"
        };

        let ch_kind = match inferred {
            "live" | "stream" | "livestream" | "web" => {
                if def.stream_url.trim().is_empty() {
                    continue;
                }
                let stream_url = if inferred == "web" {
                    crate::web_channel::ensure_web_prefix(def.stream_url.trim())
                } else {
                    def.stream_url.trim().to_string()
                };
                ChannelKind::LiveStream {
                    stream_url,
                    group: if def.group.trim().is_empty() { "Live".into() } else { def.group.trim().to_string() },
                    now_title: if def.now_title.trim().is_empty() { name.clone() } else { def.now_title.trim().to_string() },
                    overview: def.overview.trim().to_string(),
                }
            }
            "series" | "show" | "tv" => {
                let episodes: Vec<MediaItem> = def
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| !m.stream_url.trim().is_empty())
                    .map(|(mi, m)| MediaItem {
                        id: if m.id.trim().is_empty() { format!("{ch_id}:ep{mi}") } else { m.id.trim().to_string() },
                        title: m.title.clone(),
                        season: m.season,
                        episode: m.episode,
                        overview: m.overview.clone(),
                        runtime_secs: m.runtime_secs.filter(|&r| r > 10.0).unwrap_or(1800.0),
                        played: false,
                        resume_secs: 0.0,
                        artwork_id: if m.artwork_url.is_empty() { def.artwork_url.clone() } else { m.artwork_url.clone() },
                        stream_url: m.stream_url.trim().to_string(),
                    })
                    .collect();
                if episodes.is_empty() {
                    continue;
                }
                let next_up = pick_next_episode_index(&episodes, None).map(|i| episodes[i].id.clone());
                ChannelKind::Series {
                    series_id: ch_id.clone(),
                    overview: def.overview.trim().to_string(),
                    is_continue: false,
                    is_favorite: def.is_favorite,
                    episodes,
                    next_up_episode_id: next_up,
                    loaded_episodes: true,
                }
            }
            _ => {
                let movies: Vec<MediaItem> = def
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| !m.stream_url.trim().is_empty())
                    .map(|(mi, m)| MediaItem {
                        id: if m.id.trim().is_empty() { format!("{ch_id}:m{mi}") } else { m.id.trim().to_string() },
                        title: m.title.clone(),
                        season: None,
                        episode: None,
                        overview: m.overview.clone(),
                        runtime_secs: m.runtime_secs.filter(|&r| r > 10.0).unwrap_or(5400.0),
                        played: false,
                        resume_secs: 0.0,
                        artwork_id: if m.artwork_url.is_empty() { def.artwork_url.clone() } else { m.artwork_url.clone() },
                        stream_url: m.stream_url.trim().to_string(),
                    })
                    .collect();
                if movies.is_empty() {
                    continue;
                }
                let genre = if def.genre.trim().is_empty() { name.clone() } else { def.genre.trim().to_string() };
                ChannelKind::MovieGenre { genre, movies }
            }
        };

        let ch_type = if def.channel_type.trim().is_empty() {
            default_channel_type(&ch_kind).to_string()
        } else {
            def.channel_type.trim().to_string()
        };

        out.push(Channel {
            number: def.number.filter(|&n| n >= 2).unwrap_or(0),
            provider_id: "channels".into(),
            source_key: ch_id,
            callsign,
            name,
            artwork_id: def.artwork_url.trim().to_string(),
            channel_type: ch_type,
            profile_override: def.profile.clone().filter(|s| !s.trim().is_empty()),
            kind: ch_kind,
        });
    }
    out
}

fn rule_matches_channel(rule: &ChannelOverride, ch: &Channel) -> bool {
    if !rule.provider.trim().is_empty() && !ch.provider_id.eq_ignore_ascii_case(rule.provider.trim()) {
        return false;
    }
    if !rule.kind.trim().is_empty() {
        let ch_kind_str = default_channel_type(&ch.kind);
        if !ch_kind_str.eq_ignore_ascii_case(rule.kind.trim()) {
            return false;
        }
    }
    let m = rule.match_text.trim();
    if m.is_empty() || m == "*" {
        return true;
    }
    let low = m.to_ascii_lowercase();
    ch.name.to_ascii_lowercase().contains(&low)
        || ch.callsign.eq_ignore_ascii_case(m)
        || ch.source_key.to_ascii_lowercase().contains(&low)
}

/// Applies `cfg.overrides` across all channels (pinning numbers, renaming, assigning types/profiles, hiding).
pub fn apply_overrides(channels: &mut Vec<Channel>, cfg: &ChannelsFileConfig) {
    if cfg.overrides.is_empty() {
        return;
    }
    channels.retain_mut(|ch| {
        for rule in &cfg.overrides {
            if !rule_matches_channel(rule, ch) {
                continue;
            }
            if rule.hidden {
                return false;
            }
            if let Some(n) = rule.number.filter(|&n| n >= 2) {
                ch.number = n;
            }
            if let Some(ref new_name) = rule.name {
                if !new_name.trim().is_empty() {
                    ch.name = new_name.trim().to_string();
                }
            }
            if let Some(ref new_cs) = rule.callsign {
                if !new_cs.trim().is_empty() {
                    ch.callsign = new_cs.trim().chars().take(5).collect::<String>().to_ascii_uppercase();
                }
            }
            if let Some(ref new_type) = rule.channel_type {
                if !new_type.trim().is_empty() {
                    ch.channel_type = new_type.trim().to_string();
                }
            }
            if let Some(ref new_prof) = rule.profile {
                if !new_prof.trim().is_empty() {
                    ch.profile_override = Some(new_prof.trim().to_string());
                }
            }
            if let Some(fav) = rule.favorite {
                if let ChannelKind::Series { is_favorite, .. } = &mut ch.kind {
                    *is_favorite = fav;
                }
            }
        }
        true
    });
}

/// Resolves the TV profile/preset name for a tuned channel:
/// 1. Explicit `profile_override` on the channel
/// 2. Case-insensitive match for `channel_type` in `cfg.type_profiles`
/// 3. Optional `cfg.default_profile`
pub fn resolve_profile_name(
    cfg: &ChannelsFileConfig,
    channel_type: &str,
    profile_override: Option<&str>,
) -> Option<String> {
    if let Some(ov) = profile_override.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(ov.to_string());
    }
    let want = channel_type.trim();
    if !want.is_empty() {
        for (k, v) in &cfg.type_profiles {
            if k.trim().eq_ignore_ascii_case(want) && !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    let def = cfg.default_profile.trim();
    if !def.is_empty() {
        return Some(def.to_string());
    }
    None
}

// ------------------------------------------------------------------ Hot-Reloading Manager

enum FileEvent {
    ArtworkLoaded(String, ArtworkImage),
}

pub struct ChannelsFileManager {
    pub cfg: ChannelsFileConfig,
    pub channels: Vec<Channel>,
    pub file_exists: bool,
    pub status: String,
    pub last_error: String,
    last_mtime: Option<SystemTime>,
    last_check: Instant,
    artwork_cache: HashMap<String, Option<ArtworkImage>>,
    events: Arc<Mutex<Vec<FileEvent>>>,
}

impl ChannelsFileManager {
    pub fn new_offline() -> Self {
        Self {
            cfg: ChannelsFileConfig::default(),
            channels: Vec::new(),
            file_exists: false,
            status: String::new(),
            last_error: String::new(),
            last_mtime: None,
            last_check: Instant::now(),
            artwork_cache: HashMap::new(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn new() -> Self {
        let mut mgr = Self::new_offline();
        mgr.reload_from_disk();
        mgr
    }

    pub fn reload_from_disk(&mut self) -> bool {
        let Some(path) = config_path() else { return false };
        let Ok(meta) = std::fs::metadata(&path) else {
            self.file_exists = false;
            return false;
        };
        self.file_exists = true;
        self.last_mtime = meta.modified().ok();
        match load_config_from(&path) {
            Ok(cfg) => {
                self.channels = build_inline_channels(&cfg);
                let n_ch = self.channels.len();
                let n_ov = cfg.overrides.len();
                let n_tp = cfg.type_profiles.len();
                self.cfg = cfg;
                self.last_error.clear();
                self.status = format!("{n_ch} channels, {n_ov} overrides, {n_tp} type profiles");
                true
            }
            Err(e) => {
                self.last_error = e;
                false
            }
        }
    }

    pub fn save_to_disk(&mut self) -> Result<(), String> {
        let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
        save_config_to(&path, &self.cfg)?;
        self.file_exists = true;
        self.last_mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        self.channels = build_inline_channels(&self.cfg);
        let n_ch = self.channels.len();
        let n_ov = self.cfg.overrides.len();
        let n_tp = self.cfg.type_profiles.len();
        self.last_error.clear();
        self.status = format!("{n_ch} channels, {n_ov} overrides, {n_tp} type profiles");
        Ok(())
    }

    pub fn create_starter_file(&mut self) -> Result<PathBuf, String> {
        let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
        if !self.file_exists {
            if self.cfg.type_profiles.is_empty() {
                self.cfg.type_profiles.insert("series".into(), String::new());
                self.cfg.type_profiles.insert("movies".into(), "Worn VHS rental".into());
                self.cfg.type_profiles.insert("live".into(), String::new());
                self.cfg.type_profiles.insert("web".into(), String::new());
            }
            if self.cfg.channels.is_empty() {
                self.cfg.channels.push(FileChannelDef {
                    number: Some(36),
                    id: "weatherstar-4000".into(),
                    name: "WeatherStar 4000+".into(),
                    callsign: "WS4K".into(),
                    kind: "web".into(),
                    channel_type: "web".into(),
                    profile: None,
                    stream_url: crate::web_channel::WEATHERSTAR_PRESET_URL.into(),
                    path: String::new(),
                    group: "Weather".into(),
                    genre: String::new(),
                    now_title: "Local Forecast".into(),
                    overview: "Classic 1990s Weather Channel emulator (edit URL to add your latLonQuery permalink).".into(),
                    artwork_url: String::new(),
                    is_favorite: false,
                    enabled: true,
                    items: Vec::new(),
                });
            }
            if self.cfg.overrides.is_empty() {
                self.cfg.overrides.push(ChannelOverride {
                    match_text: "Movies".into(),
                    provider: String::new(),
                    kind: "movies".into(),
                    number: None,
                    name: None,
                    callsign: None,
                    channel_type: Some("movies".into()),
                    profile: None,
                    favorite: None,
                    hidden: false,
                });
            }
            self.save_to_disk()?;
        }
        Ok(path)
    }

    pub fn set_type_profile(&mut self, channel_type: String, profile_name: String) {
        let key = channel_type.trim().to_ascii_lowercase();
        if key.is_empty() {
            return;
        }
        self.cfg.type_profiles.insert(key, profile_name.trim().to_string());
        let _ = self.save_to_disk();
    }

    pub fn remove_type_profile(&mut self, channel_type: &str) {
        self.cfg.type_profiles.remove(channel_type);
        let _ = self.save_to_disk();
    }

    pub fn resolve_profile(&self, channel_type: &str, profile_override: Option<&str>) -> Option<String> {
        resolve_profile_name(&self.cfg, channel_type, profile_override)
    }

    pub fn request_artwork(&mut self, art_url: &str) -> Option<ArtworkImage> {
        if art_url.is_empty() {
            return None;
        }
        if let Some(cached) = self.artwork_cache.get(art_url) {
            return cached.clone();
        }
        self.artwork_cache.insert(art_url.to_string(), None);
        let url = art_url.to_string();
        let ev = self.events.clone();
        std::thread::spawn(move || {
            if let Some(img) = crate::m3u::fetch_artwork(&url) {
                ev.lock().unwrap().push(FileEvent::ArtworkLoaded(url, img));
            }
        });
        None
    }

    /// Checks for hot-reload edits on `channels.json` (~1 Hz) and drains artwork events.
    /// Returns `(lineup_changed, hot_reloaded)`.
    pub fn poll(&mut self) -> (bool, bool) {
        let mut changed = false;
        let mut hot_reloaded = false;

        let pending: Vec<FileEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        for ev in pending {
            match ev {
                FileEvent::ArtworkLoaded(u, img) => {
                    self.artwork_cache.insert(u, Some(img));
                    changed = true;
                }
            }
        }

        if self.last_check.elapsed() >= Duration::from_secs(1) {
            self.last_check = Instant::now();
            if let Some(path) = config_path() {
                let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
                if mtime.is_some() && mtime != self.last_mtime {
                    if self.reload_from_disk() {
                        changed = true;
                        hot_reloaded = true;
                    }
                }
            }
        }

        (changed, hot_reloaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::merge_channel_lineup;

    #[test]
    fn channels_json_inline_overrides_and_type_profile_resolution() {
        let json = r#"{
            "type_profiles": {
                "broadcast": "Broadcast",
                "vhs": "VHS",
                "movies": "VHS"
            },
            "default_profile": "Broadcast",
            "channels": [
                {
                    "number": 36,
                    "name": "WeatherStar 4000+",
                    "callsign": "WS4K",
                    "kind": "web",
                    "type": "broadcast",
                    "stream_url": "https://4000.weatherstar.dev/?kiosk=true"
                }
            ],
            "overrides": [
                { "match": "The Simpsons", "number": 4, "type": "broadcast" },
                { "match": "Sci-Fi", "kind": "movies", "type": "vhs" },
                { "match": "Boring Channel", "hidden": true }
            ]
        }"#;
        let cfg: ChannelsFileConfig = serde_json::from_str(json).unwrap();
        let mut raw = build_inline_channels(&cfg);
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].number, 36);
        assert_eq!(raw[0].channel_type, "broadcast");
        match &raw[0].kind {
            ChannelKind::LiveStream { stream_url, .. } => {
                assert_eq!(stream_url, "web://https://4000.weatherstar.dev/?kiosk=true");
            }
            other => panic!("expected LiveStream, got {other:?}"),
        }

        // Simulate channels coming from Jellyfin
        raw.push(Channel {
            number: 0,
            provider_id: "jellyfin".into(),
            source_key: "jf-simpsons".into(),
            callsign: "SIMPS".into(),
            name: "The Simpsons".into(),
            artwork_id: String::new(),
            channel_type: "series".into(),
            profile_override: None,
            kind: ChannelKind::Series {
                series_id: "jf-simpsons".into(),
                overview: String::new(),
                is_continue: false,
                is_favorite: false,
                episodes: Vec::new(),
                next_up_episode_id: None,
                loaded_episodes: true,
            },
        });
        raw.push(Channel {
            number: 0,
            provider_id: "jellyfin".into(),
            source_key: "genre:Sci-Fi".into(),
            callsign: "SCIFI".into(),
            name: "Sci-Fi Movies".into(),
            artwork_id: String::new(),
            channel_type: "movies".into(),
            profile_override: None,
            kind: ChannelKind::MovieGenre { genre: "Sci-Fi".into(), movies: Vec::new() },
        });
        raw.push(Channel {
            number: 0,
            provider_id: "m3u".into(),
            source_key: "boring".into(),
            callsign: "BORE".into(),
            name: "Boring Channel".into(),
            artwork_id: String::new(),
            channel_type: "live".into(),
            profile_override: None,
            kind: ChannelKind::LiveStream {
                stream_url: "http://example/b.m3u8".into(),
                group: String::new(),
                now_title: String::new(),
                overview: String::new(),
            },
        });

        apply_overrides(&mut raw, &cfg);
        let lineup = merge_channel_lineup(raw);
        // Guide (1), Simpsons (pinned 4), WeatherStar (pinned 36), Sci-Fi Movies (50); Boring hidden!
        let nums: Vec<(u16, &str)> = lineup.iter().map(|c| (c.number, c.name.as_str())).collect();
        assert_eq!(
            nums,
            vec![
                (1, "Program Guide"),
                (4, "The Simpsons"),
                (36, "WeatherStar 4000+"),
                (50, "Sci-Fi Movies"),
            ]
        );

        assert_eq!(
            resolve_profile_name(&cfg, &lineup[1].channel_type, lineup[1].profile_override.as_deref()).as_deref(),
            Some("Broadcast")
        );
        assert_eq!(
            resolve_profile_name(&cfg, &lineup[3].channel_type, lineup[3].profile_override.as_deref()).as_deref(),
            Some("VHS")
        );
        assert_eq!(
            resolve_profile_name(&cfg, "unknown_type", Some("Studio broadcast monitor")).as_deref(),
            Some("Studio broadcast monitor")
        );
        assert_eq!(
            resolve_profile_name(&cfg, "unknown_type", None).as_deref(),
            Some("Broadcast")
        );
    }
}
