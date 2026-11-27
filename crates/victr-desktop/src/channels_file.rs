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
use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// `"live"`, `"web"`, `"series"`, `"movies"`, `"folder"`, `"game"`, `"capture"`, or `"block"`.
    #[serde(default)]
    pub kind: String,
    /// Semantic channel type mapped through `type_profiles` (e.g. `"broadcast"`, `"vhs"`, `"weather"`, `"animated"`).
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
    /// Show/channel names to pull into a `kind: "block"` channel (matched across Jellyfin, Plex, Folders, etc.).
    #[serde(default)]
    pub shows: Vec<String>,
    /// Optional genre keywords to pull into a `kind: "block"` channel (e.g. `["Animation"]`).
    #[serde(default)]
    pub genres: Vec<String>,
    /// Rotation mode for `kind: "block"`: `"alternate"` (round-robin, default), `"shuffle"`, or `"sequential"`.
    #[serde(default)]
    pub mode: String,
    /// For `kind: "block"`: `true` (default) = drop in live on a 24/7 broadcast clock; `false` = episodic next-unwatched from 0:00.
    #[serde(default = "default_true")]
    pub live: bool,
    /// For `kind: "block"`: if `true`, hides the individual matched source channels from the main lineup.
    #[serde(default)]
    pub hide_sources: bool,
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

fn is_puzzlescript_def(def: &FileChannelDef) -> bool {
    let kind_norm = def.kind.trim().to_ascii_lowercase();
    matches!(kind_norm.as_str(), "puzzlescript" | "puzzlescript_gallery" | "ps")
        || (!def.stream_url.trim().is_empty()
            && crate::web_channel::is_puzzlescript_gallery_url(&def.stream_url))
}

fn is_pico8_def(def: &FileChannelDef) -> bool {
    let kind_norm = def.kind.trim().to_ascii_lowercase();
    matches!(kind_norm.as_str(), "pico8" | "pico-8" | "pico8_bbs" | "splore")
        || (!def.stream_url.trim().is_empty()
            && crate::web_channel::is_pico8_gallery_url(&def.stream_url))
}

/// Builds inline `Channel` records from `cfg.channels`.
#[allow(dead_code)]
pub fn build_inline_channels(cfg: &ChannelsFileConfig) -> Vec<Channel> {
    build_inline_channels_with_galleries(cfg, &HashMap::new(), &HashMap::new())
}

/// Builds inline `Channel` records from `cfg.channels`, populating any `kind: "puzzlescript"`
/// or `kind: "pico8"` gallery entries from their respective caches.
pub fn build_inline_channels_with_galleries(
    cfg: &ChannelsFileConfig,
    puzzlescript_cache: &HashMap<String, Vec<MediaItem>>,
    pico8_cache: &HashMap<String, Vec<MediaItem>>,
) -> Vec<Channel> {
    let mut out = Vec::new();
    for (idx, def) in cfg.channels.iter().enumerate() {
        if !def.enabled {
            continue;
        }
        let name = def.name.trim().to_string();
        let kind_norm = def.kind.trim().to_ascii_lowercase();
        // Programming blocks (`kind: "block"` / `"mix"` / `"playlist"`) are built in `build_block_channels`
        // once all Jellyfin, Plex, and Local Folder channels have been collected.
        if matches!(kind_norm.as_str(), "block" | "mix" | "playlist")
            || (!def.shows.is_empty() && kind_norm.is_empty())
        {
            continue;
        }
        let ch_id = if def.id.trim().is_empty() {
            format!("cfg:{idx}:{}", if name.is_empty() { "ch" } else { &name })
        } else {
            format!("cfg:{}", def.id.trim())
        };

        // Special case: PuzzleScript Gallery channel (`kind: "puzzlescript"` or Gallery URL)
        if is_puzzlescript_def(def) {
            let ps_name = if name.is_empty() { "PuzzleScript Gallery".to_string() } else { name };
            let callsign = if !def.callsign.trim().is_empty() {
                def.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase()
            } else {
                "PZLS".to_string()
            };
            let dat_url = crate::web_channel::puzzlescript_dat_url_for(&def.stream_url);
            let ch_kind = if let Some(cached) = puzzlescript_cache.get(&dat_url).filter(|v| !v.is_empty()) {
                let mut episodes = cached.clone();
                if def.mode.trim().eq_ignore_ascii_case("shuffle") {
                    episodes.sort_by_key(|m| {
                        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                        for b in m.id.bytes().chain(ps_name.bytes()) {
                            h ^= b as u64;
                            h = h.wrapping_mul(0x0100_0000_01b3);
                        }
                        h
                    });
                }
                let next_up = episodes.first().map(|m| m.id.clone());
                ChannelKind::Series {
                    series_id: ch_id.clone(),
                    overview: if def.overview.trim().is_empty() {
                        format!(
                            "Interactive PuzzleScript Gallery ({} games) · Press E to browse · Arrows/WASD/Gamepad to play · X action · Z undo · R restart",
                            episodes.len()
                        )
                    } else {
                        def.overview.trim().to_string()
                    },
                    is_continue: false,
                    is_favorite: def.is_favorite,
                    episodes,
                    next_up_episode_id: next_up,
                    loaded_episodes: true,
                }
            } else {
                ChannelKind::LiveStream {
                    stream_url: crate::web_channel::ensure_web_prefix(
                        "https://www.puzzlescript.net/Gallery/index.html",
                    ),
                    group: "Games".into(),
                    now_title: "Loading PuzzleScript Gallery…".into(),
                    overview: "Fetching games from puzzlescript.net/games_dat.js…".into(),
                }
            };
            let ch_type = if def.channel_type.trim().is_empty() {
                "web".to_string()
            } else {
                def.channel_type.trim().to_string()
            };
            out.push(Channel {
                number: def.number.filter(|&n| n >= 2).unwrap_or(0),
                provider_id: "channels".into(),
                source_key: ch_id,
                callsign,
                name: ps_name,
                artwork_id: def.artwork_url.trim().to_string(),
                channel_type: ch_type,
                profile_override: def.profile.clone().filter(|s| !s.trim().is_empty()),
                kind: ch_kind,
            });
            continue;
        }

        // Special case: PICO-8 BBS Gallery channel (`kind: "pico8"` / `"splore"` or Lexaloffle BBS URL)
        if is_pico8_def(def) {
            let p8_name = if name.is_empty() { "PICO-8 BBS".to_string() } else { name };
            let callsign = if !def.callsign.trim().is_empty() {
                def.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase()
            } else {
                "PICO8".to_string()
            };
            let lister_url = crate::web_channel::pico8_lister_url_for(&def.stream_url, &def.mode);
            let ch_kind = if let Some(cached) = pico8_cache.get(&lister_url).filter(|v| !v.is_empty()) {
                let mut episodes = cached.clone();
                if def.mode.trim().eq_ignore_ascii_case("shuffle") {
                    episodes.sort_by_key(|m| {
                        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
                        for b in m.id.bytes().chain(p8_name.bytes()) {
                            h ^= b as u64;
                            h = h.wrapping_mul(0x0100_0000_01b3);
                        }
                        h
                    });
                }
                let next_up = episodes.first().map(|m| m.id.clone());
                ChannelKind::Series {
                    series_id: ch_id.clone(),
                    overview: if def.overview.trim().is_empty() {
                        format!(
                            "Interactive PICO-8 BBS Arcade ({} carts) · Press E to browse · / search · * favorite · Arrows + Z/X + Gamepad to play",
                            episodes.len()
                        )
                    } else {
                        def.overview.trim().to_string()
                    },
                    is_continue: false,
                    is_favorite: def.is_favorite,
                    episodes,
                    next_up_episode_id: next_up,
                    loaded_episodes: true,
                }
            } else {
                ChannelKind::LiveStream {
                    stream_url: crate::web_channel::ensure_web_prefix(
                        "https://www.lexaloffle.com/bbs/?cat=7#sub=2&mode=carts&orderby=featured",
                    ),
                    group: "Games".into(),
                    now_title: "Loading PICO-8 BBS…".into(),
                    overview: "Fetching featured PICO-8 carts from lexaloffle.com/bbs…".into(),
                }
            };
            let ch_type = if def.channel_type.trim().is_empty() {
                "web".to_string()
            } else {
                def.channel_type.trim().to_string()
            };
            out.push(Channel {
                number: def.number.filter(|&n| n >= 2).unwrap_or(0),
                provider_id: "channels".into(),
                source_key: ch_id,
                callsign,
                name: p8_name,
                artwork_id: def.artwork_url.trim().to_string(),
                channel_type: ch_type,
                profile_override: def.profile.clone().filter(|s| !s.trim().is_empty()),
                kind: ch_kind,
            });
            continue;
        }

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
            } else if crate::retro::is_retro_url(&def.stream_url) {
                "game"
            } else if crate::capture::is_capture_url(&def.stream_url) {
                "capture"
            } else {
                "live"
            }
        } else if !def.path.trim().is_empty() && crate::retro::is_rom_file(Path::new(def.path.trim())) {
            "game"
        } else if def.items.iter().any(|i| i.season.is_some() || i.episode.is_some()) {
            "series"
        } else {
            "movies"
        };

        let ch_kind = match inferred {
            "live" | "stream" | "livestream" | "web" | "game" | "retro" | "capture" => {
                let raw_src = if !def.stream_url.trim().is_empty() {
                    def.stream_url.trim()
                } else {
                    def.path.trim()
                };
                if raw_src.is_empty() {
                    continue;
                }
                let stream_url = if inferred == "web" {
                    crate::web_channel::ensure_web_prefix(raw_src)
                } else if inferred == "game" || inferred == "retro" {
                    if crate::retro::is_retro_url(raw_src) {
                        raw_src.to_string()
                    } else {
                        crate::retro::make_retro_url(raw_src, None)
                    }
                } else if inferred == "capture" {
                    if crate::capture::is_capture_url(raw_src) {
                        raw_src.to_string()
                    } else {
                        format!("capture://window/{raw_src}")
                    }
                } else {
                    raw_src.to_string()
                };
                ChannelKind::LiveStream {
                    stream_url,
                    group: if def.group.trim().is_empty() {
                        if inferred == "game" || inferred == "retro" || inferred == "capture" {
                            "Game".into()
                        } else {
                            "Live".into()
                        }
                    } else {
                        def.group.trim().to_string()
                    },
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

/// Output from assembling `kind: "block"` channels against the combined provider lineup.
pub struct BlockBuildResult {
    pub channels: Vec<Channel>,
    /// `(provider_id, series_id)` pairs whose episodes haven't been fetched yet from Jellyfin/Plex.
    pub pending_episode_fetches: Vec<(String, String)>,
    /// `(provider_id, source_key)` pairs requested to be hidden (`hide_sources: true`).
    pub hidden_source_keys: Vec<(String, String)>,
}

fn match_source_for_query<'a>(query: &str, sources: &'a [Channel]) -> Option<&'a Channel> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    let low = q.to_ascii_lowercase();
    // 1. Exact case-insensitive name or callsign match first
    if let Some(ch) = sources.iter().find(|c| {
        !matches!(c.kind, ChannelKind::Guide | ChannelKind::LiveStream { .. } | ChannelKind::Block { .. })
            && (c.name.eq_ignore_ascii_case(q) || c.callsign.eq_ignore_ascii_case(q))
    }) {
        return Some(ch);
    }
    // 2. Substring match on name or source_key
    sources.iter().find(|c| {
        !matches!(c.kind, ChannelKind::Guide | ChannelKind::LiveStream { .. } | ChannelKind::Block { .. })
            && (c.name.to_ascii_lowercase().contains(&low)
                || c.source_key.to_ascii_lowercase().contains(&low))
    })
}

fn format_block_item(show_name: &str, show_art: &str, ep: &MediaItem) -> MediaItem {
    let ep_part = ep.short_label();
    let title = if ep_part.to_ascii_lowercase().starts_with(&show_name.to_ascii_lowercase()) {
        ep_part
    } else {
        format!("{show_name} · {ep_part}")
    };
    MediaItem {
        id: ep.id.clone(),
        title,
        season: None,
        episode: None,
        overview: ep.overview.clone(),
        runtime_secs: if ep.runtime_secs > 60.0 { ep.runtime_secs } else { 1500.0 },
        played: ep.played,
        resume_secs: ep.resume_secs,
        artwork_id: if !ep.artwork_id.is_empty() {
            ep.artwork_id.clone()
        } else {
            show_art.to_string()
        },
        stream_url: ep.stream_url.clone(),
    }
}

/// Builds `kind: "block"` / `"mix"` channels by pulling episodes from matching Jellyfin, Plex,
/// Local Folder, or Custom channels and interleaving them (`alternate`, `shuffle`, or `sequential`).
pub fn build_block_channels(cfg: &ChannelsFileConfig, source_channels: &[Channel]) -> BlockBuildResult {
    let mut res = BlockBuildResult {
        channels: Vec::new(),
        pending_episode_fetches: Vec::new(),
        hidden_source_keys: Vec::new(),
    };

    for (idx, def) in cfg.channels.iter().enumerate() {
        if !def.enabled {
            continue;
        }
        let kind_norm = def.kind.trim().to_ascii_lowercase();
        let is_block = matches!(kind_norm.as_str(), "block" | "mix" | "playlist")
            || (!def.shows.is_empty() && kind_norm.is_empty());
        if !is_block {
            continue;
        }
        let name = def.name.trim().to_string();
        if name.is_empty() {
            continue;
        }

        // Collect matched source channels in user-specified order without duplicates
        let mut matched: Vec<&Channel> = Vec::new();
        let mut seen_keys: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
        for q in &def.shows {
            if let Some(ch) = match_source_for_query(q, source_channels) {
                if seen_keys.insert((ch.provider_id.clone(), ch.source_key.clone())) {
                    matched.push(ch);
                }
            }
        }
        for gq in &def.genres {
            let low = gq.trim().to_ascii_lowercase();
            if low.is_empty() {
                continue;
            }
            for ch in source_channels {
                if matches!(ch.kind, ChannelKind::Guide | ChannelKind::LiveStream { .. } | ChannelKind::Block { .. }) {
                    continue;
                }
                let hit = match &ch.kind {
                    ChannelKind::MovieGenre { genre, .. } => genre.to_ascii_lowercase().contains(&low),
                    ChannelKind::Series { overview, .. } => {
                        ch.name.to_ascii_lowercase().contains(&low)
                            || ch.channel_type.to_ascii_lowercase().contains(&low)
                            || overview.to_ascii_lowercase().contains(&low)
                    }
                    _ => false,
                };
                if hit && seen_keys.insert((ch.provider_id.clone(), ch.source_key.clone())) {
                    matched.push(ch);
                }
            }
        }

        let mut pending_loads = false;
        let mut per_show: Vec<Vec<MediaItem>> = Vec::new();
        let mut primary_provider: Option<String> = None;
        let mut primary_art: String = def.artwork_url.trim().to_string();

        for ch in &matched {
            if primary_provider.is_none() {
                primary_provider = Some(ch.provider_id.clone());
            }
            if primary_art.is_empty() && !ch.artwork_id.is_empty() {
                primary_art = ch.artwork_id.clone();
            }
            if def.hide_sources {
                res.hidden_source_keys.push((ch.provider_id.clone(), ch.source_key.clone()));
            }
            if let ChannelKind::Series { series_id, loaded_episodes: false, .. } = &ch.kind {
                pending_loads = true;
                if ch.provider_id == "jellyfin" || ch.provider_id == "plex" {
                    let pair = (ch.provider_id.clone(), series_id.clone());
                    if !res.pending_episode_fetches.contains(&pair) {
                        res.pending_episode_fetches.push(pair);
                    }
                }
            }
            let eps: Vec<MediaItem> = ch
                .items()
                .iter()
                .map(|ep| format_block_item(&ch.name, &ch.artwork_id, ep))
                .collect();
            if !eps.is_empty() {
                per_show.push(eps);
            }
        }

        // Also allow optional explicit inline `def.items` appended as an extra pool
        if !def.items.is_empty() {
            let inline_eps: Vec<MediaItem> = def
                .items
                .iter()
                .enumerate()
                .filter(|(_, it)| !it.stream_url.trim().is_empty())
                .map(|(i, it)| MediaItem {
                    id: if it.id.trim().is_empty() {
                        format!("blk:{idx}:{i}")
                    } else {
                        it.id.trim().to_string()
                    },
                    title: it.title.trim().to_string(),
                    season: it.season,
                    episode: it.episode,
                    overview: it.overview.clone(),
                    runtime_secs: it.runtime_secs.unwrap_or(1500.0),
                    played: false,
                    resume_secs: 0.0,
                    artwork_id: it.artwork_url.trim().to_string(),
                    stream_url: it.stream_url.trim().to_string(),
                })
                .collect();
            if !inline_eps.is_empty() {
                per_show.push(inline_eps);
            }
        }

        let mode_raw = def.mode.trim().to_ascii_lowercase();
        let mode = match mode_raw.as_str() {
            "shuffle" | "random" => "shuffle",
            "sequential" | "serial" | "marathon" => "sequential",
            _ => "alternate",
        };

        let mut combined: Vec<MediaItem> = Vec::new();
        match mode {
            "alternate" => {
                let max_len = per_show.iter().map(|v| v.len()).max().unwrap_or(0);
                for round in 0..max_len {
                    for show_eps in &per_show {
                        if let Some(ep) = show_eps.get(round) {
                            combined.push(ep.clone());
                        }
                    }
                }
            }
            "shuffle" => {
                for show_eps in per_show {
                    combined.extend(show_eps);
                }
                let seed_num = def.number.unwrap_or(idx as u16 + 20) as u64;
                let mut keyed: Vec<(u64, MediaItem)> = combined
                    .into_iter()
                    .map(|item| {
                        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed_num.wrapping_mul(0x9e37_79b9);
                        for b in name.bytes().chain(item.id.bytes()).chain(item.title.bytes()) {
                            h ^= b as u64;
                            h = h.wrapping_mul(0x1000_0000_01b3);
                        }
                        (h, item)
                    })
                    .collect();
                keyed.sort_by_key(|(h, _)| *h);
                combined = keyed.into_iter().map(|(_, it)| it).collect();
            }
            _ => {
                for show_eps in per_show {
                    combined.extend(show_eps);
                }
            }
        }

        let ch_id = if def.id.trim().is_empty() {
            format!("cfg:block:{idx}:{name}")
        } else {
            format!("cfg:{}", def.id.trim())
        };
        let callsign = if !def.callsign.trim().is_empty() {
            def.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase()
        } else {
            make_callsign(&name)
        };
        let overview = if !def.overview.trim().is_empty() {
            def.overview.trim().to_string()
        } else if !matched.is_empty() {
            let list: Vec<&str> = matched.iter().map(|c| c.name.as_str()).collect();
            format!("Rotating block: {}", list.join(", "))
        } else {
            "Custom programming block".into()
        };
        let ch_type = if def.channel_type.trim().is_empty() {
            "block".to_string()
        } else {
            def.channel_type.trim().to_string()
        };

        res.channels.push(Channel {
            number: def.number.filter(|&n| n >= 2).unwrap_or(0),
            provider_id: primary_provider.unwrap_or_else(|| "channels".into()),
            source_key: ch_id,
            callsign,
            name,
            artwork_id: primary_art,
            channel_type: ch_type,
            profile_override: def.profile.clone().filter(|s| !s.trim().is_empty()),
            kind: ChannelKind::Block {
                mode: mode.into(),
                live_clock: def.live,
                overview,
                is_favorite: def.is_favorite,
                items: combined,
                pending_loads,
            },
        });
    }

    res
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
                match &mut ch.kind {
                    ChannelKind::Series { is_favorite, .. } | ChannelKind::Block { is_favorite, .. } => {
                        *is_favorite = fav;
                    }
                    _ => {}
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
    PuzzleScriptLoaded(String, Vec<MediaItem>),
    Pico8Loaded(String, Vec<MediaItem>),
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
    puzzlescript_cache: HashMap<String, Vec<MediaItem>>,
    puzzlescript_fetching: HashSet<String>,
    pico8_cache: HashMap<String, Vec<MediaItem>>,
    pico8_fetching: HashSet<String>,
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
            puzzlescript_cache: HashMap::new(),
            puzzlescript_fetching: HashSet::new(),
            pico8_cache: HashMap::new(),
            pico8_fetching: HashSet::new(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn new() -> Self {
        let mut mgr = Self::new_offline();
        mgr.reload_from_disk();
        mgr
    }

    fn ensure_web_galleries(&mut self) {
        let mut wanted_ps: Vec<String> = Vec::new();
        let mut wanted_p8: Vec<String> = Vec::new();
        for def in &self.cfg.channels {
            if !def.enabled {
                continue;
            }
            if is_puzzlescript_def(def) {
                let url = crate::web_channel::puzzlescript_dat_url_for(&def.stream_url);
                if !wanted_ps.contains(&url) {
                    wanted_ps.push(url);
                }
            } else if is_pico8_def(def) {
                let url = crate::web_channel::pico8_lister_url_for(&def.stream_url, &def.mode);
                if !wanted_p8.contains(&url) {
                    wanted_p8.push(url);
                }
            }
        }
        let ps_cache_path = crate::web_channel::puzzlescript_cache_path();
        for dat_url in wanted_ps {
            if !self.puzzlescript_cache.contains_key(&dat_url) {
                if let Some(cp) = ps_cache_path.as_deref() {
                    if let Some(cached) = crate::web_channel::load_cached_puzzlescript_gallery(&dat_url, cp) {
                        self.puzzlescript_cache.insert(dat_url.clone(), cached);
                    }
                }
            }
            if self.puzzlescript_fetching.insert(dat_url.clone()) {
                let ev = self.events.clone();
                let cp = ps_cache_path.clone();
                std::thread::spawn(move || {
                    if let Ok(items) = crate::web_channel::fetch_puzzlescript_gallery(&dat_url, cp.as_deref()) {
                        ev.lock().unwrap().push(FileEvent::PuzzleScriptLoaded(dat_url, items));
                    }
                });
            }
        }
        let p8_cache_path = crate::web_channel::pico8_cache_path();
        for lister_url in wanted_p8 {
            if !self.pico8_cache.contains_key(&lister_url) {
                if let Some(cp) = p8_cache_path.as_deref() {
                    if let Some(cached) = crate::web_channel::load_cached_pico8_gallery(cp) {
                        self.pico8_cache.insert(lister_url.clone(), cached);
                    }
                }
            }
            if self.pico8_fetching.insert(lister_url.clone()) {
                let ev = self.events.clone();
                let cp = p8_cache_path.clone();
                std::thread::spawn(move || {
                    if let Ok(items) = crate::web_channel::fetch_pico8_gallery(&lister_url, cp.as_deref()) {
                        ev.lock().unwrap().push(FileEvent::Pico8Loaded(lister_url, items));
                    }
                });
            }
        }
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
                self.cfg = cfg;
                self.ensure_web_galleries();
                self.channels = build_inline_channels_with_galleries(
                    &self.cfg,
                    &self.puzzlescript_cache,
                    &self.pico8_cache,
                );
                let n_ch = self.channels.len();
                let n_ov = self.cfg.overrides.len();
                let n_tp = self.cfg.type_profiles.len();
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
        self.ensure_web_galleries();
        self.channels = build_inline_channels_with_galleries(
            &self.cfg,
            &self.puzzlescript_cache,
            &self.pico8_cache,
        );
        let n_ch = self.channels.len();
        let n_ov = self.cfg.overrides.len();
        let n_tp = self.cfg.type_profiles.len();
        self.last_error.clear();
        self.status = format!("{n_ch} channels, {n_ov} overrides, {n_tp} type profiles");
        Ok(())
    }
}

/// Curated pack of free, legal, always-on starter channels so a brand-new install
/// is immediately fun to surf right out of the box.
pub fn starter_channel_defs() -> Vec<FileChannelDef> {
    vec![
        FileChannelDef {
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
            overview: "Classic 1990s Weather Channel emulator (edit URL to add your local forecast permalink).".into(),
            artwork_url: String::new(),
            is_favorite: false,
            enabled: true,
            items: Vec::new(),
            shows: Vec::new(),
            genres: Vec::new(),
            mode: String::new(),
            live: true,
            hide_sources: false,
        },
        FileChannelDef {
            number: Some(70),
            id: "pico8-bbs-arcade".into(),
            name: "PICO-8 BBS Arcade".into(),
            callsign: "PICO8".into(),
            kind: "pico8".into(),
            channel_type: "web".into(),
            profile: None,
            stream_url: String::new(),
            path: String::new(),
            group: "Games".into(),
            genre: String::new(),
            now_title: "Featured Carts".into(),
            overview: "Playable PICO-8 indie arcade carts right on the CRT — press E to browse games.".into(),
            artwork_url: String::new(),
            is_favorite: false,
            enabled: true,
            items: Vec::new(),
            shows: Vec::new(),
            genres: Vec::new(),
            mode: String::new(),
            live: true,
            hide_sources: false,
        },
        FileChannelDef {
            number: Some(71),
            id: "puzzlescript-gallery".into(),
            name: "PuzzleScript Gallery".into(),
            callsign: "PZLS".into(),
            kind: "puzzlescript".into(),
            channel_type: "web".into(),
            profile: None,
            stream_url: String::new(),
            path: String::new(),
            group: "Games".into(),
            genre: String::new(),
            now_title: "Indie Puzzle Arcade".into(),
            overview: "Hundreds of instant-play retro puzzle games — press E to browse, Arrows + X/Z to play.".into(),
            artwork_url: String::new(),
            is_favorite: false,
            enabled: true,
            items: Vec::new(),
            shows: Vec::new(),
            genres: Vec::new(),
            mode: String::new(),
            live: true,
            hide_sources: false,
        },
        FileChannelDef {
            number: Some(80),
            id: "nhk-world-live".into(),
            name: "NHK World-Japan".into(),
            callsign: "NHK".into(),
            kind: "live".into(),
            channel_type: "live".into(),
            profile: None,
            stream_url: "https://nhkwlive-ojp.akamaized.net/hls/live/2003459/nhkwlive-ojp-en/index.m3u8".into(),
            path: String::new(),
            group: "News & Culture".into(),
            genre: String::new(),
            now_title: "24/7 Live Broadcast".into(),
            overview: "Free international public broadcast from Japan — news, documentaries, and culture.".into(),
            artwork_url: String::new(),
            is_favorite: false,
            enabled: true,
            items: Vec::new(),
            shows: Vec::new(),
            genres: Vec::new(),
            mode: String::new(),
            live: true,
            hide_sources: false,
        },
        FileChannelDef {
            number: Some(81),
            id: "dw-news-live".into(),
            name: "DW News".into(),
            callsign: "DW".into(),
            kind: "live".into(),
            channel_type: "live".into(),
            profile: None,
            stream_url: "https://dwamdstream102.akamaized.net/hls/live/2015525/dwstream102/index.m3u8".into(),
            path: String::new(),
            group: "News & Culture".into(),
            genre: String::new(),
            now_title: "24/7 International News".into(),
            overview: "Free 24/7 English international news and documentary broadcast from Deutsche Welle.".into(),
            artwork_url: String::new(),
            is_favorite: false,
            enabled: true,
            items: Vec::new(),
            shows: Vec::new(),
            genres: Vec::new(),
            mode: String::new(),
            live: true,
            hide_sources: false,
        },
    ]
}

impl ChannelsFileManager {
    /// Merges the free starter channel pack into `channels.json` without duplicating
    /// any channels already present. Returns how many new channels were added.
    pub fn install_starter_pack(&mut self) -> Result<usize, String> {
        if self.cfg.type_profiles.is_empty() {
            self.cfg.type_profiles.insert("series".into(), String::new());
            self.cfg.type_profiles.insert("movies".into(), "Worn VHS rental".into());
            self.cfg.type_profiles.insert("live".into(), String::new());
            self.cfg.type_profiles.insert("web".into(), String::new());
        }
        let mut added = 0usize;
        for def in starter_channel_defs() {
            let exists = self.cfg.channels.iter().any(|c| {
                (!def.id.is_empty() && c.id.eq_ignore_ascii_case(&def.id))
                    || (!def.stream_url.is_empty() && c.stream_url.eq_ignore_ascii_case(&def.stream_url))
                    || c.name.eq_ignore_ascii_case(&def.name)
            });
            if !exists {
                self.cfg.channels.push(def);
                added += 1;
            }
        }
        if added > 0 || !self.file_exists {
            self.save_to_disk()?;
        }
        Ok(added)
    }

    pub fn upsert_inline_channel(&mut self, idx: Option<usize>, def: FileChannelDef) -> Result<(), String> {
        match idx {
            Some(i) if i < self.cfg.channels.len() => {
                self.cfg.channels[i] = def;
            }
            _ => {
                self.cfg.channels.push(def);
            }
        }
        self.save_to_disk()
    }

    pub fn remove_inline_channel(&mut self, idx: usize) -> Result<(), String> {
        if idx < self.cfg.channels.len() {
            self.cfg.channels.remove(idx);
            self.save_to_disk()?;
        }
        Ok(())
    }

    /// Inserts or updates a per-channel override matched by `match_text` + `provider`.
    pub fn upsert_override(&mut self, rule: ChannelOverride) -> Result<(), String> {
        let key_m = rule.match_text.trim().to_ascii_lowercase();
        let key_p = rule.provider.trim().to_ascii_lowercase();
        let is_noop = rule.number.is_none()
            && rule.name.as_deref().map(str::trim).unwrap_or("").is_empty()
            && rule.callsign.as_deref().map(str::trim).unwrap_or("").is_empty()
            && rule.channel_type.as_deref().map(str::trim).unwrap_or("").is_empty()
            && rule.profile.as_deref().map(str::trim).unwrap_or("").is_empty()
            && rule.favorite.is_none()
            && !rule.hidden;
        if let Some(pos) = self.cfg.overrides.iter().position(|r| {
            r.match_text.trim().to_ascii_lowercase() == key_m
                && r.provider.trim().to_ascii_lowercase() == key_p
        }) {
            if is_noop {
                self.cfg.overrides.remove(pos);
            } else {
                self.cfg.overrides[pos] = rule;
            }
        } else if !is_noop {
            self.cfg.overrides.push(rule);
        }
        self.save_to_disk()
    }

    pub fn set_channel_hidden(&mut self, match_text: &str, provider: &str, hidden: bool) -> Result<(), String> {
        let key_m = match_text.trim().to_ascii_lowercase();
        let key_p = provider.trim().to_ascii_lowercase();
        let mut rule = self
            .cfg
            .overrides
            .iter()
            .find(|r| {
                r.match_text.trim().to_ascii_lowercase() == key_m
                    && r.provider.trim().to_ascii_lowercase() == key_p
            })
            .cloned()
            .unwrap_or_else(|| ChannelOverride {
                match_text: match_text.trim().to_string(),
                provider: provider.trim().to_string(),
                ..Default::default()
            });
        rule.hidden = hidden;
        self.upsert_override(rule)
    }

    pub fn find_override(&self, match_text: &str, provider: &str) -> Option<&ChannelOverride> {
        let key_m = match_text.trim().to_ascii_lowercase();
        let key_p = provider.trim().to_ascii_lowercase();
        self.cfg.overrides.iter().find(|r| {
            r.match_text.trim().to_ascii_lowercase() == key_m
                && r.provider.trim().to_ascii_lowercase() == key_p
        })
    }
}

impl ChannelsFileManager {
    pub fn create_starter_file(&mut self) -> Result<PathBuf, String> {
        let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
        if !self.file_exists {
            let _ = self.install_starter_pack()?;
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
                FileEvent::PuzzleScriptLoaded(url, items) => {
                    self.puzzlescript_cache.insert(url, items);
                    self.channels = build_inline_channels_with_galleries(
                        &self.cfg,
                        &self.puzzlescript_cache,
                        &self.pico8_cache,
                    );
                    changed = true;
                }
                FileEvent::Pico8Loaded(url, items) => {
                    self.pico8_cache.insert(url, items);
                    self.channels = build_inline_channels_with_galleries(
                        &self.cfg,
                        &self.puzzlescript_cache,
                        &self.pico8_cache,
                    );
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

    #[test]
    fn block_channel_alternates_shows_on_live_clock_and_supports_browsing() {
        let json = r#"{
            "channels": [
                {
                    "number": 20,
                    "callsign": "TOONS",
                    "name": "Animated Block",
                    "kind": "block",
                    "type": "broadcast",
                    "mode": "alternate",
                    "live": true,
                    "hide_sources": true,
                    "shows": ["The Simpsons", "Futurama"]
                }
            ]
        }"#;
        let cfg: ChannelsFileConfig = serde_json::from_str(json).unwrap();
        let mk_ep = |id: &str, s: u32, e: u32, title: &str| MediaItem {
            id: id.into(),
            title: title.into(),
            season: Some(s),
            episode: Some(e),
            overview: String::new(),
            runtime_secs: 1320.0,
            played: false,
            resume_secs: 0.0,
            artwork_id: String::new(),
            stream_url: format!("/media/{id}.mkv"),
        };
        let sources = vec![
            Channel {
                number: 0,
                provider_id: "jellyfin".into(),
                source_key: "jf-simpsons".into(),
                callsign: "SIMPS".into(),
                name: "The Simpsons".into(),
                artwork_id: "art-simps".into(),
                channel_type: "series".into(),
                profile_override: None,
                kind: ChannelKind::Series {
                    series_id: "jf-simpsons".into(),
                    overview: String::new(),
                    is_continue: false,
                    is_favorite: false,
                    episodes: vec![
                        mk_ep("s1", 1, 1, "Simpsons Roasting"),
                        mk_ep("s2", 1, 2, "Bart the Genius"),
                    ],
                    next_up_episode_id: None,
                    loaded_episodes: true,
                },
            },
            Channel {
                number: 0,
                provider_id: "jellyfin".into(),
                source_key: "jf-futurama".into(),
                callsign: "FUTUR".into(),
                name: "Futurama".into(),
                artwork_id: "art-futur".into(),
                channel_type: "series".into(),
                profile_override: None,
                kind: ChannelKind::Series {
                    series_id: "jf-futurama".into(),
                    overview: String::new(),
                    is_continue: false,
                    is_favorite: false,
                    episodes: vec![
                        mk_ep("f1", 1, 1, "Space Pilot 3000"),
                        mk_ep("f2", 1, 2, "The Series Has Landed"),
                    ],
                    next_up_episode_id: None,
                    loaded_episodes: true,
                },
            },
        ];

        let res = build_block_channels(&cfg, &sources);
        assert_eq!(res.channels.len(), 1);
        assert_eq!(res.hidden_source_keys.len(), 2);
        let blk = &res.channels[0];
        assert_eq!(blk.number, 20);
        assert_eq!(blk.callsign, "TOONS");
        // Round-robin interleaved: Simpsons Ep1 -> Futurama Ep1 -> Simpsons Ep2 -> Futurama Ep2
        let titles: Vec<&str> = blk.items().iter().map(|i| i.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "The Simpsons · S01E01 Simpsons Roasting",
                "Futurama · S01E01 Space Pilot 3000",
                "The Simpsons · S01E02 Bart the Genius",
                "Futurama · S01E02 The Series Has Landed",
            ]
        );
        // Live drop-in works at any timestamp, and next_after rolls cleanly to the next show
        let target = blk.resolve_now(1_700_000_123).expect("block should resolve live");
        assert!(target.is_live_clock);
        let next = blk.next_after("s1").expect("next_after should advance to Futurama");
        assert_eq!(next.item.id, "f1");
    }

    #[test]
    fn starter_channel_pack_builds_clean_inline_lineup() {
        let defs = starter_channel_defs();
        assert!(defs.len() >= 5);
        let cfg = ChannelsFileConfig {
            channels: defs,
            ..Default::default()
        };
        let built = build_inline_channels(&cfg);
        assert_eq!(built.len(), cfg.channels.len());
        assert!(built.iter().any(|c| c.callsign == "WS4K" && c.number == 36));
        assert!(built.iter().any(|c| c.callsign == "PICO8" && c.number == 70));
        assert!(built.iter().any(|c| c.callsign == "PZLS" && c.number == 71));
    }
}
