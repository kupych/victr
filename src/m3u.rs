//! M3U / M3U8 Live Streams & Arbitrary Online Channels provider for ViCTR.
//!
//! Supports two complementary workflows:
//! 1. **Single Online Stream Channels** (`name` + `.m3u8` / video URL + optional group/logo/callsign):
//!    Automatically probes HLS `.m3u8` manifests on refresh to extract live metadata
//!    (`#EXT-X-DATERANGE` titles, `#EXTINF` program titles, stream resolution/bitrate).
//! 2. **M3U / M3U8 Channel Playlists** (HTTP/HTTPS URL or local `.m3u`/`.m3u8` file):
//!    Parses `#EXTINF` attributes (`tvg-name`, `tvg-logo`, `group-title`, `callsign`, `description`)
//!    and `#EXTGRP` groups into individual live channels on `CH 80+`.
//!    If a playlist URL turns out to be a direct HLS media/master manifest (`#EXT-X-TARGETDURATION` /
//!    `#EXT-X-STREAM-INF`), it is handled cleanly as a single live channel rather than splitting segments.

use crate::provider::{ArtworkImage, Channel, ChannelKind, make_callsign};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ Config

fn default_true() -> bool {
    true
}

fn default_refresh_mins() -> u32 {
    15
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M3uChannelEntry {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub callsign: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub logo_url: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M3uPlaylistSource {
    pub url_or_path: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M3uConfig {
    #[serde(default)]
    pub channels: Vec<M3uChannelEntry>,
    #[serde(default)]
    pub playlists: Vec<M3uPlaylistSource>,
    #[serde(default = "default_refresh_mins")]
    pub auto_refresh_mins: u32,
}

impl Default for M3uConfig {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            playlists: Vec::new(),
            auto_refresh_mins: default_refresh_mins(),
        }
    }
}

impl M3uConfig {
    pub fn has_enabled_sources(&self) -> bool {
        self.channels.iter().any(|c| c.enabled && !c.url.trim().is_empty())
            || self.playlists.iter().any(|p| p.enabled && !p.url_or_path.trim().is_empty())
    }
}

pub fn config_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("m3u.json")))
}

pub fn load_config_from(path: &Path) -> M3uConfig {
    let Ok(text) = std::fs::read_to_string(path) else { return M3uConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_config_to(path: &Path, cfg: &M3uConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save M3U config: {e}"))
}

pub fn load_config() -> M3uConfig {
    config_path().map(|p| load_config_from(&p)).unwrap_or_default()
}

pub fn save_config(cfg: &M3uConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    save_config_to(&path, cfg)
}

// ------------------------------------------------------------------ HTTP Helpers

fn make_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
}

fn fetch_text_source(url_or_path: &str) -> Result<String, String> {
    let trimmed = url_or_path.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let agent = make_agent();
        let resp = agent
            .get(trimmed)
            .set("User-Agent", "ViCTR/0.1.0")
            .call()
            .map_err(|e| format!("Failed to fetch {trimmed}: {e}"))?;
        let mut out = String::new();
        resp.into_reader()
            .take(4 * 1024 * 1024)
            .read_to_string(&mut out)
            .map_err(|e| format!("Failed reading {trimmed}: {e}"))?;
        Ok(out)
    } else {
        std::fs::read_to_string(trimmed).map_err(|e| format!("Couldn't read {trimmed}: {e}"))
    }
}

fn resolve_relative_url(base: &str, target: &str) -> String {
    let t = target.trim();
    if crate::web_channel::is_web_channel_url(t)
        || t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("rtmp://")
        || t.starts_with("rtsp://")
    {
        return t.to_string();
    }
    if base.starts_with("http://") || base.starts_with("https://") {
        if let Some(scheme_end) = base.find("://") {
            let after = &base[scheme_end + 3..];
            let origin_len = after.find('/').map(|p| scheme_end + 3 + p).unwrap_or(base.len());
            let origin = &base[..origin_len];
            if t.starts_with('/') {
                return format!("{origin}{t}");
            }
            if let Some(slash) = base[origin_len..].rfind('/') {
                return format!("{}{}", &base[..origin_len + slash + 1], t);
            }
            return format!("{origin}/{t}");
        }
    } else {
        let p = Path::new(t);
        if p.is_absolute() {
            return t.to_string();
        }
        if let Some(parent) = Path::new(base).parent() {
            return parent.join(p).to_string_lossy().to_string();
        }
    }
    t.to_string()
}

// ------------------------------------------------------------------ M3U / M3U8 Parser & HLS Metadata Probe

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HlsMetadata {
    pub now_title: String,
    pub overview: String,
    pub logo_url: String,
    pub group: String,
}

/// Returns `true` if `content` looks like an HLS media or master stream manifest
/// rather than an IPTV channel list.
pub fn is_hls_stream_manifest(content: &str) -> bool {
    for line in content.lines().take(64) {
        let t = line.trim();
        if t.starts_with("#EXT-X-STREAM-INF:")
            || t.starts_with("#EXT-X-TARGETDURATION:")
            || t.starts_with("#EXT-X-MEDIA-SEQUENCE:")
            || t.starts_with("#EXT-X-PLAYLIST-TYPE:")
        {
            return true;
        }
    }
    false
}

/// Extracts key="value" or key=value pairs from an `#EXTINF` or `#EXT-X-DATERANGE` / `#EXT-X-STREAM-INF` header.
pub fn parse_m3u_attrs(header: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let bytes = header.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        while i < len && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        let k_start = i;
        while i < len && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() && bytes[i] != b',' {
            i += 1;
        }
        if i >= len || bytes[i] != b'=' {
            // Skip token without '='
            while i < len && !bytes[i].is_ascii_whitespace() && bytes[i] != b',' {
                i += 1;
            }
            continue;
        }
        let key = header[k_start..i].trim().to_ascii_lowercase();
        i += 1; // skip '='
        if i >= len {
            break;
        }
        let val = if bytes[i] == b'"' {
            i += 1;
            let v_start = i;
            while i < len && bytes[i] != b'"' {
                i += 1;
            }
            let v = header[v_start..i].to_string();
            if i < len && bytes[i] == b'"' {
                i += 1;
            }
            v
        } else {
            let v_start = i;
            while i < len && !bytes[i].is_ascii_whitespace() && bytes[i] != b',' {
                i += 1;
            }
            header[v_start..i].to_string()
        };
        if !key.is_empty() {
            map.insert(key, val);
        }
    }
    map
}

/// Splits an `#EXTINF:` line into `(attrs_part, display_title)` by locating the first comma outside quotes.
fn split_extinf_line(rest: &str) -> (&str, &str) {
    let mut in_quotes = false;
    for (idx, ch) in rest.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => return (&rest[..idx], rest[idx + 1..].trim()),
            _ => {}
        }
    }
    (rest, "")
}

/// Extracts live title, stream info, and optional custom metadata tags from an HLS `.m3u8` manifest.
pub fn extract_hls_metadata(content: &str) -> HlsMetadata {
    let mut meta = HlsMetadata::default();
    let mut best_res = String::new();
    let mut best_bw: u64 = 0;
    let mut last_extinf_title = String::new();
    let mut last_daterange_title = String::new();

    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("#EXT-X-DATERANGE:") {
            let attrs = parse_m3u_attrs(rest);
            for k in ["x-title", "title", "x-program-title", "scte35-cmd"] {
                if let Some(v) = attrs.get(k).filter(|v| !v.trim().is_empty()) {
                    last_daterange_title = v.trim().to_string();
                    break;
                }
            }
            if meta.overview.is_empty() {
                if let Some(v) = attrs.get("x-description").or_else(|| attrs.get("description")) {
                    meta.overview = v.trim().to_string();
                }
            }
        } else if let Some(rest) = t.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = parse_m3u_attrs(rest);
            let bw = attrs.get("bandwidth").and_then(|b| b.parse::<u64>().ok()).unwrap_or(0);
            if bw >= best_bw {
                best_bw = bw;
                if let Some(res) = attrs.get("resolution") {
                    best_res = res.clone();
                }
            }
            if meta.now_title.is_empty() {
                if let Some(n) = attrs.get("name").or_else(|| attrs.get("x-title")) {
                    meta.now_title = n.trim().to_string();
                }
            }
        } else if let Some(rest) = t.strip_prefix("#EXTINF:") {
            let (_, title) = split_extinf_line(rest);
            if !title.is_empty() && !title.eq_ignore_ascii_case("no desc") {
                last_extinf_title = title.to_string();
            }
        } else if let Some(rest) = t.strip_prefix("#EXT-X-PROGRAM-TITLE:") {
            let v = rest.trim().trim_matches('"');
            if !v.is_empty() {
                last_daterange_title = v.to_string();
            }
        } else if let Some(rest) = t.strip_prefix("#EXTM3U") {
            let attrs = parse_m3u_attrs(rest);
            if let Some(logo) = attrs.get("tvg-logo").or_else(|| attrs.get("x-tvg-logo")) {
                meta.logo_url = logo.clone();
            }
            if let Some(grp) = attrs.get("group-title") {
                meta.group = grp.clone();
            }
        }
    }

    if !last_daterange_title.is_empty() {
        meta.now_title = last_daterange_title;
    } else if !last_extinf_title.is_empty() {
        meta.now_title = last_extinf_title;
    }

    if meta.overview.is_empty() {
        let mut parts = Vec::new();
        if !best_res.is_empty() {
            parts.push(format!("HLS {best_res}"));
        }
        if best_bw > 0 {
            parts.push(format!("{:.1} Mbps", best_bw as f64 / 1_000_000.0));
        }
        if !parts.is_empty() {
            meta.overview = format!("Live HLS stream · {}", parts.join(" · "));
        }
    }
    meta
}

/// Parses an `.m3u` / `.m3u8` channel playlist (or single HLS stream manifest) into `Channel`s.
pub fn parse_m3u_channels(content: &str, source_ref: &str, fallback_label: &str) -> Vec<Channel> {
    if is_hls_stream_manifest(content) {
        let meta = extract_hls_metadata(content);
        let name = if !fallback_label.trim().is_empty() {
            fallback_label.trim().to_string()
        } else if !meta.now_title.is_empty() {
            meta.now_title.clone()
        } else {
            "Live Stream".to_string()
        };
        let now_title = if meta.now_title.is_empty() { name.clone() } else { meta.now_title };
        let overview = if meta.overview.is_empty() {
            format!("Live HLS stream · {source_ref}")
        } else {
            meta.overview
        };
        return vec![Channel {
            number: 0,
            provider_id: "m3u".into(),
            source_key: source_ref.to_string(),
            callsign: make_callsign(&name),
            name,
            artwork_id: meta.logo_url,
            kind: ChannelKind::LiveStream {
                stream_url: source_ref.to_string(),
                group: if meta.group.is_empty() { "Live".into() } else { meta.group },
                now_title,
                overview,
            },
        }];
    }

    let mut channels = Vec::new();
    let mut pending_name = String::new();
    let mut pending_attrs: HashMap<String, String> = HashMap::new();
    let mut pending_group = String::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let (attrs_part, title) = split_extinf_line(rest);
            pending_attrs = parse_m3u_attrs(attrs_part);
            pending_name = title.to_string();
            if let Some(g) = pending_attrs.get("group-title") {
                if !g.is_empty() {
                    pending_group = g.clone();
                }
            }
        } else if let Some(grp) = line.strip_prefix("#EXTGRP:") {
            pending_group = grp.trim().to_string();
        } else if line.starts_with('#') {
            continue;
        } else {
            let mut stream_url = resolve_relative_url(source_ref, line);
            let is_web_attr = pending_attrs
                .get("type")
                .or_else(|| pending_attrs.get("kind"))
                .is_some_and(|v| v.eq_ignore_ascii_case("web"));
            if is_web_attr {
                stream_url = crate::web_channel::ensure_web_prefix(&stream_url);
            }
            let name = if !pending_name.is_empty() {
                std::mem::take(&mut pending_name)
            } else if let Some(tvg) = pending_attrs.get("tvg-name").filter(|s| !s.is_empty()) {
                tvg.clone()
            } else {
                format!("Channel {}", channels.len() + 1)
            };
            let callsign = pending_attrs
                .get("callsign")
                .or_else(|| pending_attrs.get("tvg-chno"))
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().chars().take(5).collect::<String>().to_ascii_uppercase())
                .unwrap_or_else(|| make_callsign(&name));
            let logo = pending_attrs
                .get("tvg-logo")
                .cloned()
                .unwrap_or_default();
            let group = if !pending_group.is_empty() {
                std::mem::take(&mut pending_group)
            } else if !fallback_label.trim().is_empty() {
                fallback_label.trim().to_string()
            } else {
                "Live".to_string()
            };
            let now_title = pending_attrs
                .get("now-playing")
                .or_else(|| pending_attrs.get("tvg-title"))
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| name.clone());
            let overview = pending_attrs
                .get("description")
                .or_else(|| pending_attrs.get("overview"))
                .cloned()
                .unwrap_or_else(|| format!("{group} · Live online stream"));

            channels.push(Channel {
                number: 0,
                provider_id: "m3u".into(),
                source_key: stream_url.clone(),
                callsign,
                name,
                artwork_id: logo,
                kind: ChannelKind::LiveStream {
                    stream_url,
                    group,
                    now_title,
                    overview,
                },
            });
            pending_attrs.clear();
        }
    }

    channels
}

/// Probes an `.m3u8` URL (best-effort) to populate live metadata (`now_title`, `overview`, `logo_url`).
fn probe_single_channel_m3u8(url: &str) -> Option<HlsMetadata> {
    if crate::web_channel::is_web_channel_url(url) {
        return None;
    }
    let low = url.to_ascii_lowercase();
    if !low.contains(".m3u8") && !low.contains("m3u") {
        return None;
    }
    let text = fetch_text_source(url).ok()?;
    if !is_hls_stream_manifest(&text) {
        return None;
    }
    let mut meta = extract_hls_metadata(&text);
    // If it's a master playlist pointing to a variant media playlist, peek at the first variant for live #EXTINF/#EXT-X-DATERANGE tags!
    if meta.now_title.is_empty() && text.contains("#EXT-X-STREAM-INF:") {
        let mut next_is_variant = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with("#EXT-X-STREAM-INF:") {
                next_is_variant = true;
            } else if next_is_variant && !t.is_empty() && !t.starts_with('#') {
                let var_url = resolve_relative_url(url, t);
                if let Ok(var_text) = fetch_text_source(&var_url) {
                    let vmeta = extract_hls_metadata(&var_text);
                    if !vmeta.now_title.is_empty() {
                        meta.now_title = vmeta.now_title;
                    }
                    if meta.overview.is_empty() && !vmeta.overview.is_empty() {
                        meta.overview = vmeta.overview;
                    }
                }
                break;
            }
        }
    }
    Some(meta)
}

pub fn load_all_m3u_channels(cfg: &M3uConfig) -> (Vec<Channel>, Vec<String>) {
    let mut channels = Vec::new();
    let mut errors = Vec::new();

    // 1. User-defined single live channels
    for entry in &cfg.channels {
        if !entry.enabled || entry.url.trim().is_empty() {
            continue;
        }
        let url = entry.url.trim().to_string();
        let probed = probe_single_channel_m3u8(&url);
        let name = if entry.name.trim().is_empty() {
            probed
                .as_ref()
                .filter(|m| !m.now_title.is_empty())
                .map(|m| m.now_title.clone())
                .unwrap_or_else(|| "Live Stream".into())
        } else {
            entry.name.trim().to_string()
        };
        let callsign = if !entry.callsign.trim().is_empty() {
            entry.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase()
        } else {
            make_callsign(&name)
        };
        let group = if !entry.group.trim().is_empty() {
            entry.group.trim().to_string()
        } else {
            probed
                .as_ref()
                .filter(|m| !m.group.is_empty())
                .map(|m| m.group.clone())
                .unwrap_or_else(|| "Live".into())
        };
        let logo = if !entry.logo_url.trim().is_empty() {
            entry.logo_url.trim().to_string()
        } else {
            probed.as_ref().map(|m| m.logo_url.clone()).unwrap_or_default()
        };
        let now_title = probed
            .as_ref()
            .filter(|m| !m.now_title.is_empty())
            .map(|m| m.now_title.clone())
            .unwrap_or_else(|| name.clone());
        let overview = probed
            .as_ref()
            .filter(|m| !m.overview.is_empty())
            .map(|m| m.overview.clone())
            .unwrap_or_else(|| {
                if crate::web_channel::is_web_channel_url(&url) {
                    format!("{group} · Live web channel")
                } else {
                    format!("{group} · Live stream")
                }
            });

        channels.push(Channel {
            number: 0,
            provider_id: "m3u".into(),
            source_key: url.clone(),
            callsign,
            name,
            artwork_id: logo,
            kind: ChannelKind::LiveStream {
                stream_url: url,
                group,
                now_title,
                overview,
            },
        });
    }

    // 2. M3U / M3U8 playlists (remote URLs or local files)
    for pl in &cfg.playlists {
        if !pl.enabled || pl.url_or_path.trim().is_empty() {
            continue;
        }
        let src = pl.url_or_path.trim();
        let label = if !pl.label.trim().is_empty() {
            pl.label.trim().to_string()
        } else {
            Path::new(src)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Playlist")
                .to_string()
        };
        match fetch_text_source(src) {
            Ok(text) => {
                channels.extend(parse_m3u_channels(&text, src, &label));
            }
            Err(e) => errors.push(e),
        }
    }

    (channels, errors)
}

pub fn fetch_artwork(url_or_path: &str) -> Option<ArtworkImage> {
    let s = url_or_path.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = if s.starts_with("http://") || s.starts_with("https://") {
        let resp = make_agent().get(s).set("User-Agent", "ViCTR/0.1.0").call().ok()?;
        let mut b = Vec::new();
        resp.into_reader().take(2 * 1024 * 1024).read_to_end(&mut b).ok()?;
        b
    } else {
        std::fs::read(s).ok()?
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let thumb = img.thumbnail(320, 240).to_rgba8();
    let (w, h) = thumb.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some(Arc::new((w as usize, h as usize, thumb.into_raw())))
}

// ------------------------------------------------------------------ Async Manager

enum M3uEvent {
    Loaded(Vec<Channel>, Vec<String>),
    ArtworkLoaded(String, ArtworkImage),
}

pub struct M3uManager {
    pub cfg: M3uConfig,
    pub channels: Vec<Channel>,
    pub busy: bool,
    pub status: String,
    pub last_error: String,
    pub last_sync_at: Option<std::time::Instant>,
    artwork_cache: HashMap<String, Option<ArtworkImage>>,
    events: Arc<Mutex<Vec<M3uEvent>>>,
}

impl M3uManager {
    pub fn new_offline() -> Self {
        Self {
            cfg: M3uConfig::default(),
            channels: Vec::new(),
            busy: false,
            status: String::new(),
            last_error: String::new(),
            last_sync_at: None,
            artwork_cache: HashMap::new(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn new() -> Self {
        let cfg = load_config();
        let mut mgr = Self {
            cfg,
            ..Self::new_offline()
        };
        if mgr.cfg.has_enabled_sources() {
            mgr.refresh_async();
        }
        mgr
    }

    pub fn add_channel(&mut self, name: String, url: String, group: String, logo_url: String) {
        let u = url.trim().to_string();
        if u.is_empty() {
            return;
        }
        self.cfg.channels.push(M3uChannelEntry {
            name: name.trim().to_string(),
            url: u,
            callsign: String::new(),
            group: group.trim().to_string(),
            logo_url: logo_url.trim().to_string(),
            enabled: true,
        });
        let _ = save_config(&self.cfg);
        self.refresh_async();
    }

    pub fn remove_channel(&mut self, idx: usize) {
        if idx < self.cfg.channels.len() {
            self.cfg.channels.remove(idx);
            let _ = save_config(&self.cfg);
            self.refresh_async();
        }
    }

    pub fn add_playlist(&mut self, url_or_path: String, label: String) {
        let src = url_or_path.trim().to_string();
        if src.is_empty() {
            return;
        }
        if self.cfg.playlists.iter().any(|p| p.url_or_path == src) {
            self.status = "Playlist already added.".into();
            return;
        }
        self.cfg.playlists.push(M3uPlaylistSource {
            url_or_path: src,
            label: label.trim().to_string(),
            enabled: true,
        });
        let _ = save_config(&self.cfg);
        self.refresh_async();
    }

    pub fn remove_playlist(&mut self, idx: usize) {
        if idx < self.cfg.playlists.len() {
            self.cfg.playlists.remove(idx);
            let _ = save_config(&self.cfg);
            self.refresh_async();
        }
    }

    pub fn refresh_async(&mut self) {
        let cfg = self.cfg.clone();
        let ev = self.events.clone();
        self.busy = true;
        self.last_error.clear();
        self.last_sync_at = Some(std::time::Instant::now());
        self.status = if cfg.has_enabled_sources() {
            "Loading M3U8 live streams…".into()
        } else {
            String::new()
        };
        std::thread::spawn(move || {
            let (chs, errs) = load_all_m3u_channels(&cfg);
            ev.lock().unwrap().push(M3uEvent::Loaded(chs, errs));
        });
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
            if let Some(img) = fetch_artwork(&url) {
                ev.lock().unwrap().push(M3uEvent::ArtworkLoaded(url, img));
            }
        });
        None
    }

    pub fn poll(&mut self) -> bool {
        if !self.busy
            && self.cfg.has_enabled_sources()
            && self.cfg.auto_refresh_mins > 0
            && self
                .last_sync_at
                .is_some_and(|t| t.elapsed().as_secs() >= self.cfg.auto_refresh_mins as u64 * 60)
        {
            self.refresh_async();
        }
        let pending: Vec<M3uEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        if pending.is_empty() {
            return false;
        }
        let mut changed = false;
        for ev in pending {
            match ev {
                M3uEvent::Loaded(chs, errs) => {
                    self.busy = false;
                    self.last_sync_at = Some(std::time::Instant::now());
                    let n = chs.len();
                    self.channels = chs;
                    self.last_error = errs.join(" | ");
                    self.status = if n > 0 {
                        format!("{n} live channel{}", if n == 1 { "" } else { "s" })
                    } else if self.cfg.has_enabled_sources() {
                        "No live channels found.".into()
                    } else {
                        String::new()
                    };
                    changed = true;
                }
                M3uEvent::ArtworkLoaded(u, img) => {
                    self.artwork_cache.insert(u, Some(img));
                    changed = true;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_m3u_playlist_with_extinf_attributes_and_groups() {
        let playlist = r#"#EXTM3U
#EXTINF:-1 tvg-id="nasa" tvg-name="NASA TV" tvg-logo="https://img.example/nasa.png" group-title="Science" callsign="NASA",NASA Public TV
https://stream.example/nasa/master.m3u8
#EXTGRP:Music
#EXTINF:-1 description="24/7 Synthwave",Synthwave Radio TV
live/synth.m3u8
"#;
        let chs = parse_m3u_channels(playlist, "https://tv.example/lists/main.m3u", "My IPTV");
        assert_eq!(chs.len(), 2);

        assert_eq!(chs[0].provider_id, "m3u");
        assert_eq!(chs[0].name, "NASA Public TV");
        assert_eq!(chs[0].callsign, "NASA");
        assert_eq!(chs[0].artwork_id, "https://img.example/nasa.png");
        match &chs[0].kind {
            ChannelKind::LiveStream { stream_url, group, .. } => {
                assert_eq!(stream_url, "https://stream.example/nasa/master.m3u8");
                assert_eq!(group, "Science");
            }
            other => panic!("expected LiveStream, got {other:?}"),
        }

        assert_eq!(chs[1].name, "Synthwave Radio TV");
        match &chs[1].kind {
            ChannelKind::LiveStream { stream_url, group, overview, .. } => {
                assert_eq!(stream_url, "https://tv.example/lists/live/synth.m3u8");
                assert_eq!(group, "Music");
                assert_eq!(overview, "24/7 Synthwave");
            }
            other => panic!("expected LiveStream, got {other:?}"),
        }

        // Web channel inside an M3U playlist via `type="web"` or `web://`
        let web_pl = "#EXTM3U\n#EXTINF:-1 type=\"web\" group-title=\"Weather\" callsign=\"WS4K\",WeatherStar 4000+\nhttps://4000.weatherstar.dev/?kiosk=true\n";
        let wchs = parse_m3u_channels(web_pl, "local.m3u", "Web");
        assert_eq!(wchs.len(), 1);
        assert_eq!(wchs[0].callsign, "WS4K");
        match &wchs[0].kind {
            ChannelKind::LiveStream { stream_url, .. } => {
                assert_eq!(stream_url, "web://https://4000.weatherstar.dev/?kiosk=true");
            }
            other => panic!("expected LiveStream, got {other:?}"),
        }
    }

    #[test]
    fn detects_hls_media_playlist_and_extracts_live_metadata() {
        let manifest = r#"#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:6
#EXT-X-MEDIA-SEQUENCE:42
#EXT-X-DATERANGE:ID="ep1",X-TITLE="Late Night Retro Marathon",X-DESCRIPTION="Classic 90s commercials and bumpers"
#EXTINF:6.000,Late Night Retro Marathon
seg0042.ts
#EXTINF:6.000,Late Night Retro Marathon
seg0043.ts
"#;
        assert!(is_hls_stream_manifest(manifest));
        let meta = extract_hls_metadata(manifest);
        assert_eq!(meta.now_title, "Late Night Retro Marathon");
        assert_eq!(meta.overview, "Classic 90s commercials and bumpers");

        // Passing an HLS stream manifest into parse_m3u_channels emits 1 live channel, not 2 .ts segments!
        let chs = parse_m3u_channels(manifest, "https://retro.example/live.m3u8", "Retro TV");
        assert_eq!(chs.len(), 1);
        assert_eq!(chs[0].name, "Retro TV");
        match &chs[0].kind {
            ChannelKind::LiveStream { stream_url, now_title, overview, .. } => {
                assert_eq!(stream_url, "https://retro.example/live.m3u8");
                assert_eq!(now_title, "Late Night Retro Marathon");
                assert_eq!(overview, "Classic 90s commercials and bumpers");
            }
            other => panic!("expected LiveStream, got {other:?}"),
        }
    }

    #[test]
    fn fetches_remote_playlist_and_probes_hls_channel_over_http() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        let srv = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first = req.lines().next().unwrap_or("");
                let body = if first.starts_with("GET /live.m3u8 ") {
                    "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.0,News at Nine\nseg1.ts\n"
                } else {
                    "#EXTM3U\n#EXTINF:-1 group-title=\"Public\",Public Access\nhttp://example.invalid/pa.m3u8\n"
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.apple.mpegurl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let cfg = M3uConfig {
            channels: vec![M3uChannelEntry {
                name: "Action News".into(),
                url: format!("{base}/live.m3u8"),
                callsign: "NEWS9".into(),
                group: "News".into(),
                logo_url: String::new(),
                enabled: true,
            }],
            playlists: vec![M3uPlaylistSource {
                url_or_path: format!("{base}/channels.m3u"),
                label: "Local List".into(),
                enabled: true,
            }],
            auto_refresh_mins: 15,
        };
        let (chs, errs) = load_all_m3u_channels(&cfg);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        assert_eq!(chs.len(), 2);
        assert_eq!(chs[0].callsign, "NEWS9");
        match &chs[0].kind {
            ChannelKind::LiveStream { now_title, .. } => assert_eq!(now_title, "News at Nine"),
            other => panic!("expected LiveStream, got {other:?}"),
        }
        assert_eq!(chs[1].name, "Public Access");
        srv.join().unwrap();
    }
}
