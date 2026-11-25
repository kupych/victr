//! Provider-agnostic virtual cable TV domain types and multi-source lineup builder.
//!
//! All media sources (Jellyfin, Local Nested Folders, Plex, M3U8 Live Streams, and
//! Custom Integrators) emit `Channel` records into this unified model so the 90s Prevue
//! Cable Guide, Episode Picker, TV OSD, channel surfing, and startup handoff work
//! identically across every source.

use chrono::Utc;
use std::sync::Arc;

pub type ArtworkImage = Arc<(usize, usize, Vec<u8>)>;

#[derive(Clone, Debug)]
pub struct MediaItem {
    pub id: String,
    pub title: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub overview: String,
    pub runtime_secs: f64,
    pub played: bool,
    pub resume_secs: f64,
    pub artwork_id: String,
    /// Direct playable path or stream URL (local file path, Jellyfin/Plex HTTP stream, or HLS `.m3u8`).
    pub stream_url: String,
}

impl MediaItem {
    pub fn short_label(&self) -> String {
        match (self.season, self.episode) {
            (Some(s), Some(e)) => format!("S{:02}E{:02} {}", s, e, self.title),
            _ => self.title.clone(),
        }
    }

    pub fn runtime_mins(&self) -> u32 {
        ((self.runtime_secs / 60.0).round() as u32).max(1)
    }
}

#[derive(Clone, Debug)]
pub enum ChannelKind {
    Guide,
    Series {
        series_id: String,
        overview: String,
        is_continue: bool,
        is_favorite: bool,
        episodes: Vec<MediaItem>,
        next_up_episode_id: Option<String>,
        loaded_episodes: bool,
    },
    MovieGenre {
        genre: String,
        movies: Vec<MediaItem>,
    },
    /// Continuous online/HLS live stream channel (used by M3U8 & custom integrators).
    LiveStream {
        stream_url: String,
        group: String,
        now_title: String,
        overview: String,
    },
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub number: u16,
    /// Origin provider identifier: `"jellyfin"`, `"folders"`, `"plex"`, `"m3u"`, `"custom"`, `"channels"`.
    pub provider_id: String,
    /// Stable per-provider channel identity (series ID, folder path, genre, or stream URL).
    pub source_key: String,
    pub callsign: String,
    pub name: String,
    pub artwork_id: String,
    /// Semantic channel type used to auto-select a TV profile/preset (e.g. `"series"`, `"movies"`, `"live"`, `"web"`, `"vhs"`, `"broadcast"`).
    pub channel_type: String,
    /// Optional direct TV profile/preset override on this specific channel.
    pub profile_override: Option<String>,
    pub kind: ChannelKind,
}

/// Default semantic channel type derived from a channel's kind.
pub fn default_channel_type(kind: &ChannelKind) -> &'static str {
    match kind {
        ChannelKind::Guide => "guide",
        ChannelKind::Series { .. } => "series",
        ChannelKind::MovieGenre { .. } => "movies",
        ChannelKind::LiveStream { stream_url, .. } => {
            if stream_url.len() >= 6 && stream_url[..6].eq_ignore_ascii_case("web://") {
                "web"
            } else {
                "live"
            }
        }
    }
}

/// One program block on the 90s Cable Guide timeline.
#[derive(Clone, Debug)]
pub struct GridEntry {
    pub title: String,
    pub start_unix: i64,
    pub end_unix: i64,
    pub is_live_in_progress: bool,
}

/// Result of tuning into a channel.
#[derive(Clone, Debug)]
pub struct TuneTarget {
    pub channel_number: u16,
    pub provider_id: String,
    pub source_key: String,
    pub callsign: String,
    pub channel_name: String,
    pub channel_type: String,
    pub profile_override: Option<String>,
    pub item: MediaItem,
    pub stream_url: String,
    pub start_secs: f64,
    pub is_live_clock: bool,
}

/// Active viewing session across any provider (used for watch-progress reporting and OSD).
#[derive(Clone, Debug)]
pub struct ActiveWatch {
    pub channel_number: u16,
    pub provider_id: String,
    pub source_key: String,
    pub channel_type: String,
    pub item_id: String,
    pub title: String,
    pub callsign: String,
    pub duration_secs: f64,
    pub dwell_secs: f32,
    pub reported_start: bool,
    pub marked_played: bool,
    pub last_progress_secs: f32,
    pub last_pos: f64,
    pub last_paused: bool,
}

/// Short human-readable badge for the Service Panel UI.
pub fn source_badge(provider_id: &str) -> &'static str {
    if provider_id == "jellyfin" {
        "JF"
    } else if provider_id == "folders" {
        "DIR"
    } else if provider_id == "plex" {
        "PLX"
    } else if provider_id.starts_with("m3u") {
        "LIVE"
    } else if provider_id == "channels" {
        "CFG"
    } else if provider_id == "retro" {
        "GAME"
    } else if provider_id.starts_with("custom") {
        "EXT"
    } else {
        "TV"
    }
}

/// Generates a punchy 4-5 char 90s cable call-sign from a show, genre, or stream name.
pub fn make_callsign(name: &str) -> String {
    let cleaned = name
        .trim()
        .strip_prefix("The ")
        .or_else(|| name.trim().strip_prefix("the "))
        .unwrap_or(name.trim());
    let words: Vec<&str> = cleaned
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return "TV".into();
    }
    if words.len() == 1 {
        return words[0].chars().take(5).collect::<String>().to_ascii_uppercase();
    }
    if words.len() >= 3 {
        let acr: String = words.iter().filter_map(|w| w.chars().next()).take(5).collect();
        if acr.len() >= 3 {
            return acr.to_ascii_uppercase();
        }
    }
    let first_take = words[0].chars().count().min(3);
    let first: String = words[0].chars().take(first_take).collect();
    let second_take = 5usize.saturating_sub(first.len()).max(2);
    let second: String = words[1].chars().take(second_take).collect();
    format!("{first}{second}").chars().take(5).collect::<String>().to_ascii_uppercase()
}

// ------------------------------------------------------------------ Unified Lineup Merging

/// Combines channels from all enabled providers into a single classic cable lineup:
/// - `CH 01`: Program Guide
/// - Pinned channels (`ch.number >= 2`) keep their exact requested numbers first.
/// - Auto-numbered channels fill classic blocks while skipping pinned numbers:
///   - `CH 02+`: TV Series (Continue Watching / Favorites first, then alphabetical)
///   - `CH 50+` (or next round 10s block): 24/7 Movie Genre channels
///   - `CH 80+` (or next round 10s block): Live Stream / Web channels
pub fn merge_channel_lineup(raw: Vec<Channel>) -> Vec<Channel> {
    let mut used: std::collections::HashSet<u16> = std::collections::HashSet::new();
    used.insert(1);

    let mut pinned: Vec<Channel> = Vec::new();
    let mut series: Vec<Channel> = Vec::new();
    let mut movies: Vec<Channel> = Vec::new();
    let mut live: Vec<Channel> = Vec::new();

    for mut ch in raw {
        if ch.channel_type.trim().is_empty() {
            ch.channel_type = default_channel_type(&ch.kind).to_string();
        }
        if matches!(ch.kind, ChannelKind::Guide) {
            continue;
        }
        if ch.number >= 2 && used.insert(ch.number) {
            pinned.push(ch);
            continue;
        }
        ch.number = 0;
        match ch.kind {
            ChannelKind::Guide => {}
            ChannelKind::Series { .. } => series.push(ch),
            ChannelKind::MovieGenre { .. } => movies.push(ch),
            ChannelKind::LiveStream { .. } => live.push(ch),
        }
    }

    series.sort_by(|a, b| {
        let rank = |c: &Channel| match &c.kind {
            ChannelKind::Series { is_continue, is_favorite, .. } => {
                let r = if *is_continue {
                    0u8
                } else if *is_favorite {
                    1u8
                } else {
                    2u8
                };
                (r, c.name.to_lowercase(), c.provider_id.clone())
            }
            _ => (3, c.name.to_lowercase(), c.provider_id.clone()),
        };
        rank(a).cmp(&rank(b))
    });
    movies.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.provider_id.cmp(&b.provider_id)));

    let mut alloc_from = |start: u16| -> u16 {
        let mut n = start.max(2);
        while used.contains(&n) && n < u16::MAX {
            n += 1;
        }
        used.insert(n);
        n
    };

    let mut out = Vec::with_capacity(1 + pinned.len() + series.len() + movies.len() + live.len());
    out.push(Channel {
        number: 1,
        provider_id: "guide".into(),
        source_key: "guide".into(),
        callsign: "GUIDE".into(),
        name: "Program Guide".into(),
        artwork_id: String::new(),
        channel_type: "guide".into(),
        profile_override: None,
        kind: ChannelKind::Guide,
    });
    out.extend(pinned);

    let mut ch_num: u16 = 2;
    for mut ch in series {
        ch.number = alloc_from(ch_num);
        ch_num = ch.number.saturating_add(1);
        out.push(ch);
    }

    let mut movie_ch = if ch_num <= 50 { 50 } else { ((ch_num + 9) / 10) * 10 };
    for mut ch in movies {
        ch.number = alloc_from(movie_ch);
        movie_ch = ch.number.saturating_add(1);
        out.push(ch);
    }

    let next_after_movies = movie_ch.max(ch_num);
    let mut live_ch = if next_after_movies <= 80 { 80 } else { ((next_after_movies + 9) / 10) * 10 };
    for mut ch in live {
        ch.number = alloc_from(live_ch);
        live_ch = ch.number.saturating_add(1);
        out.push(ch);
    }

    out.sort_by_key(|c| c.number);
    out
}

/// Filtered view of channels respecting Favorites-only and Search filter (always keeping CH 01 Guide).
pub fn filter_visible_channels<'a>(channels: &'a [Channel], favorites_only: bool, filter_text: &str) -> Vec<&'a Channel> {
    let q = filter_text.trim().to_lowercase();
    channels
        .iter()
        .filter(|ch| match &ch.kind {
            ChannelKind::Guide => true,
            ChannelKind::Series { is_favorite, is_continue, .. } => {
                let fav_ok = !favorites_only || *is_favorite || *is_continue;
                let q_ok = q.is_empty()
                    || ch.name.to_lowercase().contains(&q)
                    || ch.callsign.to_lowercase().contains(&q)
                    || ch.provider_id.to_lowercase().contains(&q);
                fav_ok && q_ok
            }
            ChannelKind::MovieGenre { genre, .. } => {
                let q_ok = q.is_empty()
                    || genre.to_lowercase().contains(&q)
                    || ch.name.to_lowercase().contains(&q)
                    || ch.provider_id.to_lowercase().contains(&q);
                !favorites_only && q_ok
            }
            ChannelKind::LiveStream { group, now_title, .. } => {
                let q_ok = q.is_empty()
                    || ch.name.to_lowercase().contains(&q)
                    || ch.callsign.to_lowercase().contains(&q)
                    || group.to_lowercase().contains(&q)
                    || now_title.to_lowercase().contains(&q);
                !favorites_only && q_ok
            }
        })
        .collect()
}

// ------------------------------------------------------------------ Scheduling & Tuning

impl Channel {
    /// Returns all selectable items (episodes for Series, movies for MovieGenre).
    pub fn items(&self) -> &[MediaItem] {
        match &self.kind {
            ChannelKind::Series { episodes, .. } => episodes,
            ChannelKind::MovieGenre { movies, .. } => movies,
            ChannelKind::Guide | ChannelKind::LiveStream { .. } => &[],
        }
    }

    /// Index of the recommended/next-unwatched item for initializing the Episode Picker.
    pub fn default_item_index(&self, active_item_id: Option<&str>) -> usize {
        let items = self.items();
        if let Some(aid) = active_item_id {
            if let Some(pos) = items.iter().position(|i| i.id == aid) {
                return pos;
            }
        }
        match &self.kind {
            ChannelKind::Series { episodes, next_up_episode_id, .. } => {
                pick_next_episode_index(episodes, next_up_episode_id.as_deref()).unwrap_or(0)
            }
            ChannelKind::MovieGenre { movies, .. } => {
                let now = Utc::now().timestamp();
                if let Some((m, _)) = schedule_live_movie(movies, self.number, now) {
                    movies.iter().position(|x| x.id == m.id).unwrap_or(0)
                } else {
                    0
                }
            }
            ChannelKind::Guide | ChannelKind::LiveStream { .. } => 0,
        }
    }

    /// Builds a `TuneTarget` for a specific episode or movie picked by the user.
    pub fn tune_specific_item(&self, item: &MediaItem, from_start: bool) -> TuneTarget {
        let start_secs = if from_start || item.played { 0.0 } else { item.resume_secs.max(0.0) };
        TuneTarget {
            channel_number: self.number,
            provider_id: self.provider_id.clone(),
            source_key: self.source_key.clone(),
            callsign: self.callsign.clone(),
            channel_name: self.name.clone(),
            channel_type: self.effective_type().to_string(),
            profile_override: self.profile_override.clone(),
            stream_url: item.stream_url.clone(),
            start_secs,
            item: item.clone(),
            is_live_clock: false,
        }
    }

    pub fn effective_type(&self) -> &str {
        if self.channel_type.trim().is_empty() {
            default_channel_type(&self.kind)
        } else {
            self.channel_type.trim()
        }
    }

    /// Returns the previous episode before `current_item_id`.
    pub fn prev_before(&self, current_item_id: &str) -> Option<TuneTarget> {
        let items = self.items();
        let pos = items.iter().position(|e| e.id == current_item_id)?;
        let prev = items.get(pos.checked_sub(1)?)?;
        Some(self.tune_specific_item(prev, true))
    }

    /// Picks the episode, live-scheduled movie, or live stream to play right now.
    pub fn resolve_now(&self, now_unix: i64) -> Option<TuneTarget> {
        match &self.kind {
            ChannelKind::Guide => None,
            ChannelKind::Series { episodes, next_up_episode_id, .. } => {
                let item = pick_next_episode(episodes, next_up_episode_id.as_deref())?;
                Some(TuneTarget {
                    channel_number: self.number,
                    provider_id: self.provider_id.clone(),
                    source_key: self.source_key.clone(),
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    channel_type: self.effective_type().to_string(),
                    profile_override: self.profile_override.clone(),
                    stream_url: item.stream_url.clone(),
                    start_secs: item.resume_secs.max(0.0),
                    item: item.clone(),
                    is_live_clock: false,
                })
            }
            ChannelKind::MovieGenre { movies, .. } => {
                let (item, offset) = schedule_live_movie(movies, self.number, now_unix)?;
                Some(TuneTarget {
                    channel_number: self.number,
                    provider_id: self.provider_id.clone(),
                    source_key: self.source_key.clone(),
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    channel_type: self.effective_type().to_string(),
                    profile_override: self.profile_override.clone(),
                    stream_url: item.stream_url.clone(),
                    start_secs: offset,
                    item: item.clone(),
                    is_live_clock: true,
                })
            }
            ChannelKind::LiveStream { stream_url, now_title, overview, .. } => {
                let title = if now_title.is_empty() { self.name.clone() } else { now_title.clone() };
                let item = MediaItem {
                    id: self.source_key.clone(),
                    title,
                    season: None,
                    episode: None,
                    overview: overview.clone(),
                    runtime_secs: 0.0,
                    played: false,
                    resume_secs: 0.0,
                    artwork_id: self.artwork_id.clone(),
                    stream_url: stream_url.clone(),
                };
                Some(TuneTarget {
                    channel_number: self.number,
                    provider_id: self.provider_id.clone(),
                    source_key: self.source_key.clone(),
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    channel_type: self.effective_type().to_string(),
                    profile_override: self.profile_override.clone(),
                    stream_url: stream_url.clone(),
                    start_secs: 0.0,
                    item,
                    is_live_clock: true,
                })
            }
        }
    }

    /// Returns the next episode after `current_item_id` (for auto-advance on Series channels).
    pub fn next_after(&self, current_item_id: &str) -> Option<TuneTarget> {
        match &self.kind {
            ChannelKind::Series { episodes, .. } => {
                let pos = episodes.iter().position(|e| e.id == current_item_id)?;
                let next = episodes.get(pos + 1)?;
                Some(TuneTarget {
                    channel_number: self.number,
                    provider_id: self.provider_id.clone(),
                    source_key: self.source_key.clone(),
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    channel_type: self.effective_type().to_string(),
                    profile_override: self.profile_override.clone(),
                    stream_url: next.stream_url.clone(),
                    start_secs: 0.0,
                    item: next.clone(),
                    is_live_clock: false,
                })
            }
            ChannelKind::MovieGenre { movies, .. } => {
                let now = Utc::now().timestamp() + 2;
                let (item, _) = schedule_live_movie(movies, self.number, now)?;
                Some(TuneTarget {
                    channel_number: self.number,
                    provider_id: self.provider_id.clone(),
                    source_key: self.source_key.clone(),
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    channel_type: self.effective_type().to_string(),
                    profile_override: self.profile_override.clone(),
                    stream_url: item.stream_url.clone(),
                    start_secs: 0.0,
                    item: item.clone(),
                    is_live_clock: true,
                })
            }
            ChannelKind::Guide | ChannelKind::LiveStream { .. } => None,
        }
    }

    /// Computes the program blocks intersecting `[window_start, window_end]` for the 90s Guide grid.
    pub fn grid_entries(&self, window_start: i64, window_end: i64, now_unix: i64) -> Vec<GridEntry> {
        match &self.kind {
            ChannelKind::Guide => vec![GridEntry {
                title: "ViCTR CABLE GUIDE & PREVUE".into(),
                start_unix: window_start,
                end_unix: window_end,
                is_live_in_progress: true,
            }],
            ChannelKind::LiveStream { now_title, .. } => vec![GridEntry {
                title: if now_title.is_empty() { self.name.clone() } else { now_title.clone() },
                start_unix: window_start,
                end_unix: window_end,
                is_live_in_progress: true,
            }],
            ChannelKind::Series { episodes, next_up_episode_id, loaded_episodes, .. } => {
                if episodes.is_empty() {
                    let msg = if *loaded_episodes {
                        self.name.clone()
                    } else {
                        format!("{} (LOADING...)", self.name)
                    };
                    return vec![GridEntry {
                        title: msg,
                        start_unix: window_start,
                        end_unix: window_end,
                        is_live_in_progress: false,
                    }];
                }
                let start_idx = pick_next_episode_index(episodes, next_up_episode_id.as_deref()).unwrap_or(0);
                let slot_anchor = (now_unix / 1800) * 1800;
                let mut cur_t = slot_anchor;
                let mut out = Vec::new();
                let mut idx = start_idx;
                while cur_t < window_end && idx < episodes.len() && out.len() < 12 {
                    let ep = &episodes[idx];
                    let mins = (ep.runtime_secs / 60.0).max(15.0);
                    let blocks = ((mins / 30.0).ceil() as i64).max(1);
                    let end_t = cur_t + blocks * 1800;
                    if end_t > window_start {
                        out.push(GridEntry {
                            title: ep.short_label(),
                            start_unix: cur_t,
                            end_unix: end_t,
                            is_live_in_progress: false,
                        });
                    }
                    cur_t = end_t;
                    idx += 1;
                }
                out
            }
            ChannelKind::MovieGenre { movies, .. } => {
                if movies.is_empty() {
                    return Vec::new();
                }
                let order = deterministic_movie_order(movies, self.number);
                let total_secs: i64 = order.iter().map(|&i| movie_slot_secs(&movies[i])).sum::<i64>().max(1800);
                let cycle_start = (window_start / total_secs) * total_secs;
                let mut t = cycle_start;
                let mut out = Vec::new();
                let mut guard = 0;
                while t < window_end && guard < 200 {
                    for &mi in &order {
                        let m = &movies[mi];
                        let dur = movie_slot_secs(m);
                        let end = t + dur;
                        if end > window_start && t < window_end {
                            out.push(GridEntry {
                                title: m.title.clone(),
                                start_unix: t,
                                end_unix: end,
                                is_live_in_progress: now_unix >= t && now_unix < end,
                            });
                        }
                        t = end;
                        guard += 1;
                        if t >= window_end || guard >= 200 {
                            break;
                        }
                    }
                }
                out
            }
        }
    }

    /// Info text for the top-right Prevue Guide panel: `(title, meta, overview, artwork_id)`.
    pub fn preview_info(&self, now_unix: i64) -> (String, String, String, String) {
        match &self.kind {
            ChannelKind::Guide => (
                "ViCTR CABLE GUIDE".into(),
                "90s Virtual Cable TV Program Guide".into(),
                "Use arrow keys to browse channels & time slots, press ENTER to tune, E for episodes, or punch in a channel number (0-9).".into(),
                self.artwork_id.clone(),
            ),
            ChannelKind::Series { overview, episodes, next_up_episode_id, .. } => {
                let src = source_badge(&self.provider_id);
                if let Some(ep) = pick_next_episode(episodes, next_up_episode_id.as_deref()) {
                    let meta = if ep.resume_secs > 15.0 {
                        format!("{} · {} MIN · RESUME {:02}:{:02}", src, ep.runtime_mins(), ep.resume_secs as u64 / 60, ep.resume_secs as u64 % 60)
                    } else {
                        format!("{} · {} MIN · NEXT UP", src, ep.runtime_mins())
                    };
                    let desc = if !ep.overview.is_empty() { ep.overview.clone() } else { overview.clone() };
                    let art = if !self.artwork_id.is_empty() { self.artwork_id.clone() } else { ep.artwork_id.clone() };
                    (ep.short_label(), meta, desc, art)
                } else {
                    (self.name.clone(), format!("{src} · TV SERIES"), overview.clone(), self.artwork_id.clone())
                }
            }
            ChannelKind::MovieGenre { genre, movies } => {
                let src = source_badge(&self.provider_id);
                if let Some((m, offset)) = schedule_live_movie(movies, self.number, now_unix) {
                    let in_mins = (offset / 60.0) as u32;
                    let meta = format!("{} · {} · {} MIN · LIVE ({}M IN)", src, genre.to_ascii_uppercase(), m.runtime_mins(), in_mins);
                    let art = if !m.artwork_id.is_empty() { m.artwork_id.clone() } else { self.artwork_id.clone() };
                    (m.title.clone(), meta, m.overview.clone(), art)
                } else {
                    (genre.clone(), format!("{src} · MOVIE CHANNEL"), String::new(), self.artwork_id.clone())
                }
            }
            ChannelKind::LiveStream { group, now_title, overview, .. } => {
                let title = if now_title.is_empty() { self.name.clone() } else { now_title.clone() };
                let meta = if group.is_empty() {
                    "LIVE STREAM".into()
                } else {
                    format!("LIVE · {}", group.to_ascii_uppercase())
                };
                (title, meta, overview.clone(), self.artwork_id.clone())
            }
        }
    }
}

pub fn pick_next_episode_index(episodes: &[MediaItem], next_up_id: Option<&str>) -> Option<usize> {
    if episodes.is_empty() {
        return None;
    }
    if let Some(id) = next_up_id {
        if let Some(pos) = episodes.iter().position(|e| e.id == id && (!e.played || e.resume_secs > 10.0)) {
            return Some(pos);
        }
    }
    if let Some(pos) = episodes.iter().position(|e| e.resume_secs > 10.0 && !e.played) {
        return Some(pos);
    }
    episodes.iter().position(|e| !e.played).or(Some(0))
}

pub fn pick_next_episode<'a>(episodes: &'a [MediaItem], next_up_id: Option<&str>) -> Option<&'a MediaItem> {
    pick_next_episode_index(episodes, next_up_id).and_then(|i| episodes.get(i))
}

fn movie_slot_secs(m: &MediaItem) -> i64 {
    let raw = m.runtime_secs.max(1800.0);
    ((raw / 900.0).ceil() as i64).max(2) * 900
}

fn deterministic_movie_order(movies: &[MediaItem], ch_num: u16) -> Vec<usize> {
    let mut idxs: Vec<(u64, usize)> = movies
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (ch_num as u64).wrapping_mul(0x9e37_79b9);
            for b in m.id.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x1000_0000_01b3);
            }
            (h, i)
        })
        .collect();
    idxs.sort_by_key(|&(h, _)| h);
    idxs.into_iter().map(|(_, i)| i).collect()
}

/// Picks the movie currently "airing" on a 24/7 genre channel and the live seek offset in seconds.
pub fn schedule_live_movie(movies: &[MediaItem], ch_num: u16, now_unix: i64) -> Option<(&MediaItem, f64)> {
    if movies.is_empty() {
        return None;
    }
    let order = deterministic_movie_order(movies, ch_num);
    let total: i64 = order.iter().map(|&i| movie_slot_secs(&movies[i])).sum::<i64>().max(1);
    let mut pos = now_unix.rem_euclid(total);
    for &mi in &order {
        let m = &movies[mi];
        let slot = movie_slot_secs(m);
        if pos < slot {
            let max_seek = (m.runtime_secs - 15.0).max(0.0);
            let offset = (pos as f64).min(max_seek);
            return Some((m, offset));
        }
        pos -= slot;
    }
    let m = &movies[order[0]];
    Some((m, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_lineup_combines_multiple_providers_into_cable_blocks() {
        let ep = MediaItem {
            id: "ep1".into(),
            title: "Pilot".into(),
            season: Some(1),
            episode: Some(1),
            overview: String::new(),
            runtime_secs: 1500.0,
            played: false,
            resume_secs: 0.0,
            artwork_id: String::new(),
            stream_url: "/tmp/ep1.mkv".into(),
        };
        let mov = MediaItem {
            id: "m1".into(),
            title: "Die Hard".into(),
            season: None,
            episode: None,
            overview: String::new(),
            runtime_secs: 7800.0,
            played: false,
            resume_secs: 0.0,
            artwork_id: String::new(),
            stream_url: "/tmp/diehard.mkv".into(),
        };
        let raw = vec![
            Channel {
                number: 0,
                provider_id: "folders".into(),
                source_key: "/tv/batman".into(),
                callsign: "BATMN".into(),
                name: "Batman TAS".into(),
                artwork_id: String::new(),
                channel_type: "series".into(),
                profile_override: None,
                kind: ChannelKind::Series {
                    series_id: "/tv/batman".into(),
                    overview: String::new(),
                    is_continue: false,
                    is_favorite: false,
                    episodes: vec![ep.clone()],
                    next_up_episode_id: None,
                    loaded_episodes: true,
                },
            },
            Channel {
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
                    is_continue: true,
                    is_favorite: false,
                    episodes: vec![ep],
                    next_up_episode_id: None,
                    loaded_episodes: true,
                },
            },
            Channel {
                number: 0,
                provider_id: "folders".into(),
                source_key: "action".into(),
                callsign: "ACTIO".into(),
                name: "Action Movies".into(),
                artwork_id: String::new(),
                channel_type: "movies".into(),
                profile_override: None,
                kind: ChannelKind::MovieGenre { genre: "Action".into(), movies: vec![mov] },
            },
            Channel {
                number: 36,
                provider_id: "channels".into(),
                source_key: "ws4k".into(),
                callsign: "WS4K".into(),
                name: "WeatherStar 4000+".into(),
                artwork_id: String::new(),
                channel_type: "weather".into(),
                profile_override: None,
                kind: ChannelKind::LiveStream {
                    stream_url: "web://https://4000.weatherstar.dev/?kiosk=true".into(),
                    group: "Weather".into(),
                    now_title: "Local Forecast".into(),
                    overview: String::new(),
                },
            },
            Channel {
                number: 0,
                provider_id: "m3u".into(),
                source_key: "http://stream.example/live.m3u8".into(),
                callsign: "NEWS".into(),
                name: "Retro News".into(),
                artwork_id: String::new(),
                channel_type: "live".into(),
                profile_override: None,
                kind: ChannelKind::LiveStream {
                    stream_url: "http://stream.example/live.m3u8".into(),
                    group: "News".into(),
                    now_title: "24/7 Live".into(),
                    overview: String::new(),
                },
            },
        ];

        let lineup = merge_channel_lineup(raw);
        assert_eq!(lineup.len(), 6);
        assert_eq!(lineup[0].number, 1);
        assert_eq!(lineup[0].callsign, "GUIDE");
        // Continue Watching series sorted first regardless of provider
        assert_eq!(lineup[1].number, 2);
        assert_eq!(lineup[1].name, "The Simpsons");
        assert_eq!(lineup[2].number, 3);
        assert_eq!(lineup[2].name, "Batman TAS");
        // Pinned channel keeps its exact slot (CH 36)
        assert_eq!(lineup[3].number, 36);
        assert_eq!(lineup[3].name, "WeatherStar 4000+");
        // Movies start at CH 50
        assert_eq!(lineup[4].number, 50);
        assert_eq!(lineup[4].name, "Action Movies");
        // Live streams start at CH 80
        assert_eq!(lineup[5].number, 80);
        assert_eq!(lineup[5].name, "Retro News");
    }
}
