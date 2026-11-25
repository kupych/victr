//! Nested Local Folders provider for ViCTR's unified 90s Cable Guide.
//!
//! Recursively scans user-configured local directories for video files:
//! - Nested TV show trees (`Show/Season 01/S01E01 - Pilot.mkv`, `Cartoons/90s/Show/1x02.mp4`, etc.)
//!   automatically become per-series channels with clean season/episode parsing.
//! - Movie folders (`Action/Die Hard.mkv` or `Movies/Blade Runner (1982)/Blade Runner.mkv`)
//!   automatically become 24/7 genre/collection movie channels.
//! - Local artwork (`poster.*`, `folder.*`, `fanart.*`, `cover.*`, `backdrop.*`) is loaded on demand.
//! - Watched status & resume timestamps persist locally in `folders.json`.

use crate::provider::{ArtworkImage, Channel, ChannelKind, MediaItem, make_callsign, pick_next_episode_index};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "webm", "avi", "mov", "m4v", "mpg", "mpeg", "ts", "flv", "wmv", "ogv", "3gp", "vob",
];
const ART_NAMES: &[&str] = &[
    "fanart", "backdrop", "poster", "folder", "cover", "thumb", "banner",
];
const ART_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp"];

// ------------------------------------------------------------------ Config & Persistence

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FolderRoot {
    pub path: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LocalWatchState {
    #[serde(default)]
    pub played: bool,
    #[serde(default)]
    pub resume_secs: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FoldersConfig {
    #[serde(default)]
    pub roots: Vec<FolderRoot>,
    #[serde(default)]
    pub watch_state: HashMap<String, LocalWatchState>,
}

pub fn config_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("folders.json")))
}

pub fn load_config_from(path: &Path) -> FoldersConfig {
    let Ok(text) = std::fs::read_to_string(path) else { return FoldersConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_config_to(path: &Path, cfg: &FoldersConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save folders config: {e}"))
}

pub fn load_config() -> FoldersConfig {
    config_path().map(|p| load_config_from(&p)).unwrap_or_default()
}

pub fn save_config(cfg: &FoldersConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    save_config_to(&path, cfg)
}

// ------------------------------------------------------------------ Filename & Folder Parsing

fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| VIDEO_EXTS.iter().any(|v| v.eq_ignore_ascii_case(ext)))
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('@') || name.eq_ignore_ascii_case("System Volume Information")
}

/// Parses folder names like `"Season 1"`, `"Season 02"`, `"S01"`, `"S1"`, `"Specials"`, `"Season 0"`.
pub fn parse_season_dir_name(name: &str) -> Option<u32> {
    let lower = name.trim().to_ascii_lowercase();
    if lower == "specials" || lower == "extras" {
        return Some(0);
    }
    if let Some(rest) = lower.strip_prefix("season") {
        let digits: String = rest.trim_start_matches([' ', '_', '-', '.']).chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
    }
    if let Some(rest) = lower.strip_prefix('s') {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            return rest.parse().ok();
        }
    }
    None
}

/// Parses `(season, episode, clean_title)` from a video file stem.
pub fn parse_episode_filename(stem: &str, folder_season: Option<u32>) -> (Option<u32>, Option<u32>, String) {
    let bytes = stem.as_bytes();
    let len = bytes.len();

    // 1) Look for SxxExx / s1e02
    for i in 0..len {
        if (bytes[i] == b'S' || bytes[i] == b's') && i + 3 < len {
            let mut j = i + 1;
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < len && (bytes[j] == b'E' || bytes[j] == b'e') {
                let mut k = j + 1;
                while k < len && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 {
                    let s_num: Option<u32> = stem[i + 1..j].parse().ok();
                    let e_num: Option<u32> = stem[j + 1..k].parse().ok();
                    // Skip any multi-ep suffix like E03 or -E03
                    let mut after = k;
                    if after + 1 < len && (bytes[after] == b'E' || bytes[after] == b'e' || bytes[after] == b'-') {
                        let mut m = after + 1;
                        if m < len && (bytes[m] == b'E' || bytes[m] == b'e') {
                            m += 1;
                        }
                        let d0 = m;
                        while m < len && bytes[m].is_ascii_digit() {
                            m += 1;
                        }
                        if m > d0 {
                            after = m;
                        }
                    }
                    let raw_title = clean_media_title(&stem[after..]);
                    let title = if raw_title.is_empty() {
                        format!("Episode {}", e_num.unwrap_or(1))
                    } else {
                        raw_title
                    };
                    return (s_num, e_num, title);
                }
            }
        }
    }

    // 2) Look for 1x02 / 01x02
    for i in 0..len {
        if bytes[i].is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let mut j = i;
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < len && (bytes[j] == b'x' || bytes[j] == b'X') && j - i <= 2 {
                let mut k = j + 1;
                while k < len && bytes[k].is_ascii_digit() {
                    k += 1;
                }
                if k > j + 1 && k - (j + 1) <= 3 && (k == len || !bytes[k].is_ascii_alphanumeric()) {
                    let s_num: Option<u32> = stem[i..j].parse().ok();
                    let e_num: Option<u32> = stem[j + 1..k].parse().ok();
                    let raw_title = clean_media_title(&stem[k..]);
                    let title = if raw_title.is_empty() {
                        format!("Episode {}", e_num.unwrap_or(1))
                    } else {
                        raw_title
                    };
                    return (s_num, e_num, title);
                }
            }
        }
    }

    // 3) Leading episode number inside a Season folder: "01 - Title" / "02. Title"
    if let Some(fs) = folder_season {
        let trimmed = stem.trim();
        let d_end = trimmed.bytes().take_while(|b| b.is_ascii_digit()).count();
        if (1..=3).contains(&d_end) {
            let rest = &trimmed[d_end..];
            if rest.is_empty() || rest.starts_with([' ', '-', '_', '.']) {
                if let Ok(e_num) = trimmed[..d_end].parse::<u32>() {
                    let raw_title = clean_media_title(rest);
                    let title = if raw_title.is_empty() {
                        format!("Episode {e_num}")
                    } else {
                        raw_title
                    };
                    return (Some(fs), Some(e_num), title);
                }
            }
        }
    }

    (folder_season, None, clean_media_title(stem))
}

fn clean_media_title(raw: &str) -> String {
    let mut s = raw
        .replace(['.', '_'], " ")
        .trim_matches(|c: char| c.is_whitespace() || c == '-' || c == '–' || c == ':')
        .to_string();

    // Strip bracketed release tags like [1080p], [x265], [SubGroup]
    let mut stripped = String::with_capacity(s.len());
    let mut depth = 0usize;
    for ch in s.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => stripped.push(ch),
            _ => {}
        }
    }
    s = stripped;

    // Stop before common scene resolution/codec tokens
    const STOP_TOKENS: &[&str] = &[
        "2160p", "1080p", "720p", "480p", "x264", "x265", "h264", "h265", "hevc", "av1",
        "bluray", "bdrip", "dvdrip", "webrip", "web-dl", "webdl", "hdtv", "remux", "aac", "ac3", "dts",
    ];
    let mut words = Vec::new();
    for w in s.split_whitespace() {
        let low = w.to_ascii_lowercase();
        if STOP_TOKENS.iter().any(|t| *t == low) {
            break;
        }
        words.push(w);
    }
    words
        .join(" ")
        .trim_matches(|c: char| c.is_whitespace() || c == '-' || c == '–' || c == ':')
        .to_string()
}

fn find_folder_artwork(dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else { return String::new() };
    let mut best: Option<(usize, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
        if !ART_EXTS.iter().any(|a| a.eq_ignore_ascii_case(ext)) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let low = stem.to_ascii_lowercase();
        if let Some(rank) = ART_NAMES.iter().position(|n| *n == low) {
            if best.as_ref().is_none_or(|(b, _)| rank < *b) {
                best = Some((rank, path));
            }
        }
    }
    best.map(|(_, p)| p.to_string_lossy().to_string()).unwrap_or_default()
}

// ------------------------------------------------------------------ Recursive Folder Scanner

fn build_media_item(
    path: &Path,
    folder_season: Option<u32>,
    fallback_art: &str,
    default_runtime_secs: f64,
    watch_state: &HashMap<String, LocalWatchState>,
) -> MediaItem {
    let path_str = path.to_string_lossy().to_string();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Video");
    let (season, episode, title) = parse_episode_filename(stem, folder_season);
    let ws = watch_state.get(&path_str);
    MediaItem {
        id: path_str.clone(),
        title: if title.is_empty() { stem.to_string() } else { title },
        season,
        episode,
        overview: path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|n| format!("Local file · {n}"))
            .unwrap_or_else(|| "Local video file".into()),
        runtime_secs: default_runtime_secs,
        played: ws.map(|w| w.played).unwrap_or(false),
        resume_secs: ws.map(|w| w.resume_secs).unwrap_or(0.0),
        artwork_id: fallback_art.to_string(),
        stream_url: path_str,
    }
}

fn sort_episodes(eps: &mut [MediaItem]) {
    eps.sort_by(|a, b| {
        let sa = match a.season {
            Some(0) => u32::MAX - 1,
            Some(n) => n,
            None => u32::MAX,
        };
        let sb = match b.season {
            Some(0) => u32::MAX - 1,
            Some(n) => n,
            None => u32::MAX,
        };
        let ea = a.episode.unwrap_or(u32::MAX);
        let eb = b.episode.unwrap_or(u32::MAX);
        sa.cmp(&sb)
            .then_with(|| ea.cmp(&eb))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
}

fn collect_videos_recursive(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, Option<u32>)>, inherited_season: Option<u32>) {
    if depth > 5 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if is_hidden_name(name) {
            continue;
        }
        if path.is_file() && is_video_file(&path) {
            out.push((path, inherited_season));
        } else if path.is_dir() {
            let next_season = parse_season_dir_name(name).or(inherited_season);
            collect_videos_recursive(&path, depth + 1, out, next_season);
        }
    }
}

/// Inspects a directory node and emits `Series` and/or `MovieGenre` channels.
fn scan_dir_node(
    dir: &Path,
    display_name: &str,
    depth: usize,
    watch_state: &HashMap<String, LocalWatchState>,
    out_channels: &mut Vec<Channel>,
) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut direct_videos: Vec<PathBuf> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if is_hidden_name(name) {
            continue;
        }
        if path.is_file() && is_video_file(&path) {
            direct_videos.push(path);
        } else if path.is_dir() {
            subdirs.push(path);
        }
    }
    direct_videos.sort();
    subdirs.sort();

    let art = find_folder_artwork(dir);

    // Check if any immediate subdirectory is a Season folder -> this directory is a TV Series!
    let has_season_subdirs = subdirs
        .iter()
        .any(|d| d.file_name().and_then(|n| n.to_str()).and_then(parse_season_dir_name).is_some());

    if has_season_subdirs {
        let mut raw_eps: Vec<(PathBuf, Option<u32>)> = Vec::new();
        for v in direct_videos {
            raw_eps.push((v, None));
        }
        for sub in subdirs {
            let s_num = sub.file_name().and_then(|n| n.to_str()).and_then(parse_season_dir_name);
            collect_videos_recursive(&sub, 0, &mut raw_eps, s_num);
        }
        if !raw_eps.is_empty() {
            let mut episodes: Vec<MediaItem> = raw_eps
                .iter()
                .map(|(p, sn)| build_media_item(p, *sn, &art, 1800.0, watch_state))
                .collect();
            sort_episodes(&mut episodes);
            let is_continue = episodes.iter().any(|e| e.resume_secs > 15.0 || e.played)
                && episodes.iter().any(|e| !e.played);
            let next_up = pick_next_episode_index(&episodes, None).map(|i| episodes[i].id.clone());
            let key = dir.to_string_lossy().to_string();
            out_channels.push(Channel {
                number: 0,
                provider_id: "folders".into(),
                source_key: key.clone(),
                callsign: make_callsign(display_name),
                name: display_name.to_string(),
                artwork_id: art,
                kind: ChannelKind::Series {
                    series_id: key,
                    overview: format!("Local folder · {} episodes", episodes.len()),
                    is_continue,
                    is_favorite: false,
                    episodes,
                    next_up_episode_id: next_up,
                    loaded_episodes: true,
                },
            });
        }
        return;
    }

    // Check if direct videos look like episodes (SxxExx / 1x02) -> emit as a Series channel
    let direct_episodic_count = direct_videos
        .iter()
        .filter(|p| {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let (s, e, _) = parse_episode_filename(stem, None);
            s.is_some() && e.is_some()
        })
        .count();

    let mut loose_movies: Vec<MediaItem> = Vec::new();
    if !direct_videos.is_empty() {
        if direct_episodic_count > 0 && direct_episodic_count * 2 >= direct_videos.len() {
            let mut episodes: Vec<MediaItem> = direct_videos
                .iter()
                .map(|p| build_media_item(p, None, &art, 1800.0, watch_state))
                .collect();
            sort_episodes(&mut episodes);
            let is_continue = episodes.iter().any(|e| e.resume_secs > 15.0 || e.played)
                && episodes.iter().any(|e| !e.played);
            let next_up = pick_next_episode_index(&episodes, None).map(|i| episodes[i].id.clone());
            let key = dir.to_string_lossy().to_string();
            out_channels.push(Channel {
                number: 0,
                provider_id: "folders".into(),
                source_key: key.clone(),
                callsign: make_callsign(display_name),
                name: display_name.to_string(),
                artwork_id: art.clone(),
                kind: ChannelKind::Series {
                    series_id: key,
                    overview: format!("Local folder · {} episodes", episodes.len()),
                    is_continue,
                    is_favorite: false,
                    episodes,
                    next_up_episode_id: next_up,
                    loaded_episodes: true,
                },
            });
        } else {
            for v in &direct_videos {
                loose_movies.push(build_media_item(v, None, &art, 5400.0, watch_state));
            }
        }
    }

    // Recurse into subdirectories:
    // If a subdir has ONLY a single non-episodic video file and no nested dirs, treat it as a movie
    // folder (e.g. `Action/Blade Runner (1982)/Blade Runner.mkv`) and group it into this folder's Movie channel!
    for sub in subdirs {
        let sub_name = sub
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Folder")
            .to_string();
        if let Some(single_movie) = try_single_movie_dir(&sub, watch_state) {
            loose_movies.push(single_movie);
        } else {
            scan_dir_node(&sub, &sub_name, depth + 1, watch_state, out_channels);
        }
    }

    if !loose_movies.is_empty() {
        loose_movies.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        let genre = display_name.to_string();
        let ch_name = if genre.to_ascii_lowercase().contains("movie") {
            genre.clone()
        } else {
            format!("{genre} Movies")
        };
        let key = format!("movies:{}", dir.to_string_lossy());
        let first_art = loose_movies
            .iter()
            .find(|m| !m.artwork_id.is_empty())
            .map(|m| m.artwork_id.clone())
            .unwrap_or(art);
        out_channels.push(Channel {
            number: 0,
            provider_id: "folders".into(),
            source_key: key,
            callsign: make_callsign(&genre),
            name: ch_name,
            artwork_id: first_art,
            kind: ChannelKind::MovieGenre {
                genre,
                movies: loose_movies,
            },
        });
    }
}

/// Returns `Some(MediaItem)` if `dir` is a leaf movie folder containing exactly 1 non-episodic video.
fn try_single_movie_dir(dir: &Path, watch_state: &HashMap<String, LocalWatchState>) -> Option<MediaItem> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut videos = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if is_hidden_name(name) {
            continue;
        }
        if p.is_dir() {
            return None;
        }
        if p.is_file() && is_video_file(&p) {
            videos.push(p);
        }
    }
    if videos.len() != 1 {
        return None;
    }
    let v = &videos[0];
    let stem = v.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let (s, e, _) = parse_episode_filename(stem, None);
    if s.is_some() && e.is_some() {
        return None;
    }
    let art = find_folder_artwork(dir);
    let mut item = build_media_item(v, None, &art, 5400.0, watch_state);
    if let Some(folder_title) = dir.file_name().and_then(|n| n.to_str()) {
        let clean = clean_media_title(folder_title);
        if !clean.is_empty() {
            item.title = clean;
        }
    }
    Some(item)
}

pub fn scan_roots(cfg: &FoldersConfig) -> Vec<Channel> {
    let mut channels = Vec::new();
    for root in &cfg.roots {
        if !root.enabled || root.path.trim().is_empty() {
            continue;
        }
        let path = Path::new(root.path.trim());
        if !path.is_dir() {
            continue;
        }
        let label = if !root.label.trim().is_empty() {
            root.label.trim().to_string()
        } else {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Local")
                .to_string()
        };
        scan_dir_node(path, &label, 0, &cfg.watch_state, &mut channels);
    }
    channels
}

fn load_local_artwork(path: &str) -> Option<ArtworkImage> {
    if path.is_empty() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let thumb = img.thumbnail(320, 240).to_rgba8();
    let (w, h) = thumb.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some(Arc::new((w as usize, h as usize, thumb.into_raw())))
}

// ------------------------------------------------------------------ Async Manager

enum FoldersEvent {
    Scanned(Vec<Channel>),
    ArtworkLoaded(String, ArtworkImage),
}

pub struct FoldersManager {
    pub cfg: FoldersConfig,
    pub channels: Vec<Channel>,
    pub busy: bool,
    pub status: String,
    artwork_cache: HashMap<String, Option<ArtworkImage>>,
    events: Arc<Mutex<Vec<FoldersEvent>>>,
}

impl FoldersManager {
    pub fn new_offline() -> Self {
        Self {
            cfg: FoldersConfig::default(),
            channels: Vec::new(),
            busy: false,
            status: String::new(),
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
        if mgr.cfg.roots.iter().any(|r| r.enabled) {
            mgr.rescan_async();
        }
        mgr
    }

    pub fn add_root(&mut self, path: String) {
        let trimmed = path.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        if self.cfg.roots.iter().any(|r| r.path == trimmed) {
            self.status = "Folder already added.".into();
            return;
        }
        let label = Path::new(&trimmed)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Local")
            .to_string();
        self.cfg.roots.push(FolderRoot {
            path: trimmed,
            label,
            enabled: true,
        });
        let _ = save_config(&self.cfg);
        self.rescan_async();
    }

    pub fn remove_root(&mut self, idx: usize) {
        if idx < self.cfg.roots.len() {
            self.cfg.roots.remove(idx);
            let _ = save_config(&self.cfg);
            self.rescan_async();
        }
    }

    pub fn rescan_async(&mut self) {
        let cfg = self.cfg.clone();
        let ev = self.events.clone();
        self.busy = true;
        self.status = "Scanning local folders…".into();
        std::thread::spawn(move || {
            let chs = scan_roots(&cfg);
            ev.lock().unwrap().push(FoldersEvent::Scanned(chs));
        });
    }

    pub fn set_item_played(&mut self, item_id: &str, played: bool) {
        let entry = self.cfg.watch_state.entry(item_id.to_string()).or_default();
        entry.played = played;
        if played {
            entry.resume_secs = 0.0;
        }
        for ch in &mut self.channels {
            match &mut ch.kind {
                ChannelKind::Series { episodes, next_up_episode_id, is_continue, .. } => {
                    if let Some(ep) = episodes.iter_mut().find(|e| e.id == item_id) {
                        ep.played = played;
                        if played {
                            ep.resume_secs = 0.0;
                        }
                        *next_up_episode_id = pick_next_episode_index(episodes, None).map(|i| episodes[i].id.clone());
                        *is_continue = episodes.iter().any(|e| e.resume_secs > 15.0 || e.played)
                            && episodes.iter().any(|e| !e.played);
                    }
                }
                ChannelKind::MovieGenre { movies, .. } => {
                    if let Some(m) = movies.iter_mut().find(|m| m.id == item_id) {
                        m.played = played;
                        if played {
                            m.resume_secs = 0.0;
                        }
                    }
                }
                ChannelKind::Guide | ChannelKind::LiveStream { .. } => {}
            }
        }
        let _ = save_config(&self.cfg);
    }

    pub fn save_resume(&mut self, item_id: &str, pos_secs: f64, duration_secs: f64) {
        if item_id.is_empty() {
            return;
        }
        let finished = duration_secs > 30.0 && pos_secs >= duration_secs * 0.90;
        if finished {
            self.set_item_played(item_id, true);
            return;
        }
        let entry = self.cfg.watch_state.entry(item_id.to_string()).or_default();
        entry.resume_secs = pos_secs.max(0.0);
        for ch in &mut self.channels {
            if let ChannelKind::Series { episodes, is_continue, .. } = &mut ch.kind {
                if let Some(ep) = episodes.iter_mut().find(|e| e.id == item_id) {
                    ep.resume_secs = pos_secs.max(0.0);
                    *is_continue = true;
                }
            }
        }
        let _ = save_config(&self.cfg);
    }

    pub fn request_artwork(&mut self, path: &str) -> Option<ArtworkImage> {
        if path.is_empty() {
            return None;
        }
        if let Some(cached) = self.artwork_cache.get(path) {
            return cached.clone();
        }
        self.artwork_cache.insert(path.to_string(), None);
        let p = path.to_string();
        let ev = self.events.clone();
        std::thread::spawn(move || {
            if let Some(img) = load_local_artwork(&p) {
                ev.lock().unwrap().push(FoldersEvent::ArtworkLoaded(p, img));
            }
        });
        None
    }

    /// Drains background scan & artwork results. Returns `true` if channels changed.
    pub fn poll(&mut self) -> bool {
        let pending: Vec<FoldersEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        if pending.is_empty() {
            return false;
        }
        let mut channels_changed = false;
        for ev in pending {
            match ev {
                FoldersEvent::Scanned(chs) => {
                    self.busy = false;
                    let n_series = chs.iter().filter(|c| matches!(c.kind, ChannelKind::Series { .. })).count();
                    let n_movies = chs.iter().filter(|c| matches!(c.kind, ChannelKind::MovieGenre { .. })).count();
                    self.channels = chs;
                    self.status = if self.cfg.roots.is_empty() {
                        String::new()
                    } else {
                        format!("{n_series} series, {n_movies} movie folders")
                    };
                    channels_changed = true;
                }
                FoldersEvent::ArtworkLoaded(p, img) => {
                    self.artwork_cache.insert(p, Some(img));
                }
            }
        }
        channels_changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_episode_filenames_and_seasons() {
        assert_eq!(parse_season_dir_name("Season 02"), Some(2));
        assert_eq!(parse_season_dir_name("S03"), Some(3));
        assert_eq!(parse_season_dir_name("Specials"), Some(0));

        assert_eq!(
            parse_episode_filename("The.Simpsons.S01E02.Bart.the.Genius.1080p.WEB-DL", None),
            (Some(1), Some(2), "Bart the Genius".into())
        );
        assert_eq!(
            parse_episode_filename("1x05 - Homer's Night Out [720p]", None),
            (Some(1), Some(5), "Homer's Night Out".into())
        );
        assert_eq!(
            parse_episode_filename("03 - Homer's Odyssey", Some(1)),
            (Some(1), Some(3), "Homer's Odyssey".into())
        );
    }

    #[test]
    fn scans_nested_series_and_movie_folders() {
        let root = std::env::temp_dir().join(format!("victr-folders-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);

        // Nested TV show with seasons
        let s1 = root.join("Cartoons/90s/Batman TAS/Season 01");
        let s2 = root.join("Cartoons/90s/Batman TAS/Season 02");
        std::fs::create_dir_all(&s1).unwrap();
        std::fs::create_dir_all(&s2).unwrap();
        std::fs::write(s1.join("S01E02 - Christmas With The Joker.mkv"), b"").unwrap();
        std::fs::write(s1.join("S01E01 - On Leather Wings.mkv"), b"").unwrap();
        std::fs::write(s2.join("01 - Sideshow.mp4"), b"").unwrap();

        // Movie category with both loose files and per-movie subfolders
        let movies = root.join("Sci-Fi");
        let blade = movies.join("Blade Runner (1982)");
        std::fs::create_dir_all(&blade).unwrap();
        std::fs::write(blade.join("Blade.Runner.1080p.mkv"), b"").unwrap();
        std::fs::write(movies.join("Alien.1979.BluRay.mp4"), b"").unwrap();

        let cfg = FoldersConfig {
            roots: vec![FolderRoot {
                path: root.to_string_lossy().to_string(),
                label: "Media".into(),
                enabled: true,
            }],
            watch_state: HashMap::new(),
        };
        let chs = scan_roots(&cfg);
        assert_eq!(chs.len(), 2, "expected 1 series + 1 movie folder channel: {chs:?}");

        let series = chs.iter().find(|c| c.name == "Batman TAS").expect("Batman TAS series");
        assert_eq!(series.provider_id, "folders");
        let eps = series.items();
        assert_eq!(eps.len(), 3);
        assert_eq!(eps[0].short_label(), "S01E01 On Leather Wings");
        assert_eq!(eps[1].short_label(), "S01E02 Christmas With The Joker");
        assert_eq!(eps[2].short_label(), "S02E01 Sideshow");

        let scifi = chs.iter().find(|c| c.name == "Sci-Fi Movies").expect("Sci-Fi Movies channel");
        assert_eq!(scifi.items().len(), 2);
        let titles: Vec<&str> = scifi.items().iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, vec!["Alien 1979", "Blade Runner (1982)"]);

        let _ = std::fs::remove_dir_all(&root);
    }
}
