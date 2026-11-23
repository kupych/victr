//! Jellyfin integration for ViCTR's 90s Cable Channel Guide:
//! - Config persistence (`jellyfin.json` in platform config dir, Server URL + API key + User ID)
//! - Async background HTTP calls (`ureq` + `serde_json`) for Users, Series, Episodes, Movies, and Artwork
//! - Virtual cable channel builder (Ch 01 Guide, Ch 02+ TV Series, Ch 50+ Movie Genres)
//! - Hybrid scheduling: Next Unwatched + auto-advance for Series, 24/7 deterministic broadcast clock for Movies
//! - Watch-progress & watched-status reporting (>20s dwell gate, 90% completion mark)

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ Config

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JellyfinConfig {
    pub server_url: String,
    pub api_key: String,
    pub user_id: String,
    pub user_name: String,
    #[serde(default)]
    pub favorites_only: bool,
    #[serde(default)]
    pub filter_text: String,
}

impl JellyfinConfig {
    pub fn normalized_url(&self) -> String {
        self.server_url.trim().trim_end_matches('/').to_string()
    }

    pub fn is_complete(&self) -> bool {
        !self.normalized_url().is_empty() && !self.api_key.trim().is_empty() && !self.user_id.trim().is_empty()
    }
}

pub fn config_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("jellyfin.json")))
}

pub fn load_config() -> JellyfinConfig {
    let Some(path) = config_path() else { return JellyfinConfig::default() };
    let Ok(text) = std::fs::read_to_string(path) else { return JellyfinConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_config(cfg: &JellyfinConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save Jellyfin config: {e}"))
}

// ------------------------------------------------------------------ Domain types

#[derive(Clone, Debug)]
pub struct JellyfinUser {
    pub id: String,
    pub name: String,
}

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
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub number: u16,
    pub callsign: String,
    pub name: String,
    pub artwork_id: String,
    pub kind: ChannelKind,
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
    pub callsign: String,
    pub channel_name: String,
    pub item: MediaItem,
    pub stream_url: String,
    pub start_secs: f64,
    pub is_live_clock: bool,
}

pub type ArtworkImage = Arc<(usize, usize, Vec<u8>)>;

/// Generates a punchy 4-6 char 90s cable call-sign from a show or genre name.
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
    // If multi-word, use acronym if 3+ words, else first 4-5 letters of first word or initials
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

// ------------------------------------------------------------------ Scheduling

impl Channel {
    /// Returns all selectable items (episodes for Series, movies for MovieGenre).
    pub fn items(&self) -> &[MediaItem] {
        match &self.kind {
            ChannelKind::Series { episodes, .. } => episodes,
            ChannelKind::MovieGenre { movies, .. } => movies,
            ChannelKind::Guide => &[],
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
            ChannelKind::Guide => 0,
        }
    }

    /// Builds a `TuneTarget` for a specific episode or movie picked by the user.
    pub fn tune_specific_item(
        &self,
        item: &MediaItem,
        server_url: &str,
        api_key: &str,
        from_start: bool,
    ) -> TuneTarget {
        let start_secs = if from_start || item.played { 0.0 } else { item.resume_secs.max(0.0) };
        TuneTarget {
            channel_number: self.number,
            callsign: self.callsign.clone(),
            channel_name: self.name.clone(),
            stream_url: build_stream_url(server_url, api_key, &item.id),
            start_secs,
            item: item.clone(),
            is_live_clock: false,
        }
    }

    /// Returns the previous episode before `current_item_id` (for Series / Movie channels).
    pub fn prev_before(&self, current_item_id: &str, server_url: &str, api_key: &str) -> Option<TuneTarget> {
        let items = self.items();
        let pos = items.iter().position(|e| e.id == current_item_id)?;
        let prev = items.get(pos.checked_sub(1)?)?;
        Some(self.tune_specific_item(prev, server_url, api_key, true))
    }

    /// Picks the episode or live-scheduled movie to play right now.
    pub fn resolve_now(&self, server_url: &str, api_key: &str, now_unix: i64) -> Option<TuneTarget> {
        match &self.kind {
            ChannelKind::Guide => None,
            ChannelKind::Series { episodes, next_up_episode_id, .. } => {
                let item = pick_next_episode(episodes, next_up_episode_id.as_deref())?;
                Some(TuneTarget {
                    channel_number: self.number,
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    stream_url: build_stream_url(server_url, api_key, &item.id),
                    start_secs: item.resume_secs.max(0.0),
                    item: item.clone(),
                    is_live_clock: false,
                })
            }
            ChannelKind::MovieGenre { movies, .. } => {
                let (item, offset) = schedule_live_movie(movies, self.number, now_unix)?;
                Some(TuneTarget {
                    channel_number: self.number,
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    stream_url: build_stream_url(server_url, api_key, &item.id),
                    start_secs: offset,
                    item: item.clone(),
                    is_live_clock: true,
                })
            }
        }
    }

    /// Returns the next episode after `current_item_id` (for auto-advance on Series channels).
    pub fn next_after(&self, current_item_id: &str, server_url: &str, api_key: &str) -> Option<TuneTarget> {
        match &self.kind {
            ChannelKind::Series { episodes, .. } => {
                let pos = episodes.iter().position(|e| e.id == current_item_id)?;
                let next = episodes.get(pos + 1)?;
                Some(TuneTarget {
                    channel_number: self.number,
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    stream_url: build_stream_url(server_url, api_key, &next.id),
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
                    callsign: self.callsign.clone(),
                    channel_name: self.name.clone(),
                    stream_url: build_stream_url(server_url, api_key, &item.id),
                    start_secs: 0.0,
                    item: item.clone(),
                    is_live_clock: true,
                })
            }
            ChannelKind::Guide => None,
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
                // Anchor the Next Unwatched episode at the current half-hour boundary, followed by subsequent episodes.
                let slot_anchor = (now_unix / 1800) * 1800;
                let mut cur_t = slot_anchor;
                let mut out = Vec::new();
                let mut idx = start_idx;
                while cur_t < window_end && idx < episodes.len() && out.len() < 12 {
                    let ep = &episodes[idx];
                    // Snap durations to neat 30-min grid multiples like classic TV Guide
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
                // Walk from the cycle start covering window_start..window_end
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

    /// Info text for the top-right Prevue Guide panel.
    pub fn preview_info(&self, now_unix: i64) -> (String, String, String, String) {
        match &self.kind {
            ChannelKind::Guide => (
                "ViCTR CABLE GUIDE".into(),
                "90s Cable TV Channel Guide for Jellyfin".into(),
                "Use arrow keys to browse channels & time slots, press ENTER to tune, or punch in a channel number (0-9).".into(),
                self.artwork_id.clone(),
            ),
            ChannelKind::Series { overview, episodes, next_up_episode_id, .. } => {
                if let Some(ep) = pick_next_episode(episodes, next_up_episode_id.as_deref()) {
                    let meta = if ep.resume_secs > 15.0 {
                        format!("{} MIN · RESUME {:02}:{:02}", ep.runtime_mins(), ep.resume_secs as u64 / 60, ep.resume_secs as u64 % 60)
                    } else {
                        format!("{} MIN · NEXT UP", ep.runtime_mins())
                    };
                    let desc = if !ep.overview.is_empty() { ep.overview.clone() } else { overview.clone() };
                    let art = if !self.artwork_id.is_empty() { self.artwork_id.clone() } else { ep.artwork_id.clone() };
                    (ep.short_label(), meta, desc, art)
                } else {
                    (self.name.clone(), "TV SERIES".into(), overview.clone(), self.artwork_id.clone())
                }
            }
            ChannelKind::MovieGenre { genre, movies } => {
                if let Some((m, offset)) = schedule_live_movie(movies, self.number, now_unix) {
                    let in_mins = (offset / 60.0) as u32;
                    let meta = format!("{} · {} MIN · LIVE ({}M IN)", genre.to_ascii_uppercase(), m.runtime_mins(), in_mins);
                    let art = if !m.artwork_id.is_empty() { m.artwork_id.clone() } else { self.artwork_id.clone() };
                    (m.title.clone(), meta, m.overview.clone(), art)
                } else {
                    (genre.clone(), "MOVIE CHANNEL".into(), String::new(), self.artwork_id.clone())
                }
            }
        }
    }
}

fn pick_next_episode_index(episodes: &[MediaItem], next_up_id: Option<&str>) -> Option<usize> {
    if episodes.is_empty() {
        return None;
    }
    if let Some(id) = next_up_id {
        if let Some(pos) = episodes.iter().position(|e| e.id == id) {
            return Some(pos);
        }
    }
    if let Some(pos) = episodes.iter().position(|e| e.resume_secs > 10.0) {
        return Some(pos);
    }
    episodes.iter().position(|e| !e.played).or(Some(0))
}

fn pick_next_episode<'a>(episodes: &'a [MediaItem], next_up_id: Option<&str>) -> Option<&'a MediaItem> {
    pick_next_episode_index(episodes, next_up_id).and_then(|i| episodes.get(i))
}

fn movie_slot_secs(m: &MediaItem) -> i64 {
    let raw = m.runtime_secs.max(1800.0);
    // Pad slightly to 15-min broadcast boundary like cable movie channels
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

pub fn build_stream_url(server_url: &str, api_key: &str, item_id: &str) -> String {
    let base = server_url.trim().trim_end_matches('/');
    format!("{base}/Videos/{item_id}/stream?static=true&api_key={api_key}")
}

// ------------------------------------------------------------------ Jellyfin HTTP Client

fn make_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(6))
        .timeout(std::time::Duration::from_secs(30))
        .build()
}

fn auth_header(api_key: &str) -> String {
    format!(
        "MediaBrowser Client=\"ViCTR\", Device=\"ViCTR CRT\", DeviceId=\"victr-crt-receiver\", Version=\"0.1.0\", Token=\"{}\"",
        api_key.trim()
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawUser {
    id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawItemsResp {
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawUserData {
    #[serde(default)]
    played: bool,
    #[serde(default)]
    is_favorite: bool,
    #[serde(default)]
    playback_position_ticks: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawItem {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    series_id: Option<String>,
    #[serde(default)]
    parent_index_number: Option<u32>,
    #[serde(default)]
    index_number: Option<u32>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    run_time_ticks: Option<i64>,
    #[serde(default)]
    location_type: Option<String>,
    #[serde(default)]
    user_data: Option<RawUserData>,
}

impl RawItem {
    fn to_media_item(&self, fallback_art: &str) -> MediaItem {
        let ud = self.user_data.as_ref();
        let runtime_secs = self.run_time_ticks.unwrap_or(0) as f64 / 10_000_000.0;
        let resume_secs = ud.map(|u| u.playback_position_ticks as f64 / 10_000_000.0).unwrap_or(0.0);
        let art = self.series_id.clone().unwrap_or_else(|| {
            if fallback_art.is_empty() { self.id.clone() } else { fallback_art.to_string() }
        });
        MediaItem {
            id: self.id.clone(),
            title: self.name.clone(),
            season: self.parent_index_number,
            episode: self.index_number,
            overview: self.overview.clone().unwrap_or_default(),
            runtime_secs: if runtime_secs > 10.0 { runtime_secs } else { 1800.0 },
            played: ud.map(|u| u.played).unwrap_or(false),
            resume_secs,
            artwork_id: art,
        }
    }
}

fn get_json<T: for<'de> Deserialize<'de>>(agent: &ureq::Agent, url: &str, api_key: &str) -> Result<T, String> {
    let resp = agent
        .get(url)
        .set("X-Emby-Token", api_key.trim())
        .set("Authorization", &auth_header(api_key))
        .call()
        .map_err(|e| format!("Jellyfin request failed: {e}"))?;
    resp.into_json::<T>().map_err(|e| format!("Invalid Jellyfin JSON: {e}"))
}

pub fn fetch_users(server_url: &str, api_key: &str) -> Result<Vec<JellyfinUser>, String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() {
        return Err("Enter both Server URL and API Key.".into());
    }
    let agent = make_agent();
    let raw: Vec<RawUser> = get_json(&agent, &format!("{base}/Users"), api_key)?;
    Ok(raw.into_iter().map(|u| JellyfinUser { id: u.id, name: u.name }).collect())
}

pub fn fetch_series_episodes(server_url: &str, api_key: &str, user_id: &str, series_id: &str) -> Result<Vec<MediaItem>, String> {
    let base = server_url.trim().trim_end_matches('/');
    let agent = make_agent();
    // Query `/Users/{uid}/Items?ParentId={series_id}&IncludeItemTypes=Episode&Recursive=true`
    // so every real episode file (all seasons, specials, un-numbered episodes) is included.
    let items_url = format!(
        "{base}/Users/{user_id}/Items?ParentId={series_id}&IncludeItemTypes=Episode&Recursive=true&IsMissing=false&Fields=Overview,RunTimeTicks,UserData&EnableUserData=true&SortBy=ParentIndexNumber,IndexNumber,SortName&SortOrder=Ascending"
    );
    let mut raw = match get_json::<RawItemsResp>(&agent, &items_url, api_key) {
        Ok(r) if !r.items.is_empty() => r.items,
        _ => {
            let shows_url = format!(
                "{base}/Shows/{series_id}/Episodes?UserId={user_id}&IsMissing=false&Fields=Overview,RunTimeTicks,UserData&EnableUserData=true"
            );
            get_json::<RawItemsResp>(&agent, &shows_url, api_key)?.items
        }
    };
    raw.retain(|r| r.location_type.as_deref() != Some("Virtual"));
    // Sort cleanly by (season, episode, title), putting Season 0 specials after regular seasons
    raw.sort_by(|a, b| {
        let sa = match a.parent_index_number {
            Some(0) => u32::MAX - 1,
            Some(n) => n,
            None => u32::MAX,
        };
        let sb = match b.parent_index_number {
            Some(0) => u32::MAX - 1,
            Some(n) => n,
            None => u32::MAX,
        };
        sa.cmp(&sb)
            .then_with(|| a.index_number.unwrap_or(u32::MAX).cmp(&b.index_number.unwrap_or(u32::MAX)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(raw.iter().map(|r| r.to_media_item(series_id)).collect())
}

pub fn trigger_library_scan(server_url: &str, api_key: &str) -> Result<(), String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() {
        return Err("Jellyfin server not configured.".into());
    }
    let agent = make_agent();
    agent
        .post(&format!("{base}/Library/Refresh"))
        .set("X-Emby-Token", api_key.trim())
        .set("Authorization", &auth_header(api_key))
        .call()
        .map_err(|e| format!("Library scan request failed: {e}"))?;
    Ok(())
}

pub fn fetch_artwork(server_url: &str, api_key: &str, item_id: &str) -> Option<ArtworkImage> {
    if item_id.is_empty() {
        return None;
    }
    let base = server_url.trim().trim_end_matches('/');
    let agent = make_agent();
    let urls = [
        format!("{base}/Items/{item_id}/Images/Backdrop/0?maxWidth=320&quality=80&api_key={}", api_key.trim()),
        format!("{base}/Items/{item_id}/Images/Primary?maxWidth=260&quality=80&api_key={}", api_key.trim()),
    ];
    for url in urls {
        if let Ok(resp) = agent.get(&url).call() {
            let mut bytes = Vec::new();
            if resp.into_reader().read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let rgba = img.to_rgba8();
                    let (w, h) = rgba.dimensions();
                    if w > 0 && h > 0 {
                        return Some(Arc::new((w as usize, h as usize, rgba.into_raw())));
                    }
                }
            }
        }
    }
    None
}

/// Builds the full channel lineup from a Jellyfin server:
/// - Ch 01: Program Guide
/// - Ch 02+: TV Series (Continue Watching / Favorites first, then alphabetical)
/// - Ch 50+ (or next round block): Movie Genre channels
pub fn fetch_lineup(cfg: &JellyfinConfig) -> Result<Vec<Channel>, String> {
    let base = cfg.normalized_url();
    let key = cfg.api_key.trim();
    let uid = cfg.user_id.trim();
    if base.is_empty() || key.is_empty() || uid.is_empty() {
        return Err("Jellyfin Server URL, API Key, and User are required.".into());
    }
    let agent = make_agent();

    // 1. Continue Watching / Next Up episodes
    let next_up_url = format!("{base}/Shows/NextUp?UserId={uid}&Limit=60&Fields=Overview,RunTimeTicks,UserData");
    let next_up: RawItemsResp = get_json(&agent, &next_up_url, key).unwrap_or(RawItemsResp { items: Vec::new() });
    let mut next_up_by_series: HashMap<String, MediaItem> = HashMap::new();
    let mut next_up_order: HashMap<String, usize> = HashMap::new();
    for (idx, ep) in next_up.items.iter().enumerate() {
        if let Some(sid) = &ep.series_id {
            next_up_by_series.entry(sid.clone()).or_insert_with(|| ep.to_media_item(sid));
            next_up_order.entry(sid.clone()).or_insert(idx);
        }
    }

    // 2. All TV Series
    let series_url = format!(
        "{base}/Users/{uid}/Items?IncludeItemTypes=Series&Recursive=true&Fields=Overview,Genres,UserData&SortBy=SortName&SortOrder=Ascending"
    );
    let raw_series: RawItemsResp = get_json(&agent, &series_url, key)?;

    let mut series_entries: Vec<(usize, bool, String, RawItem)> = raw_series
        .items
        .into_iter()
        .map(|s| {
            let nu_rank = next_up_order.get(&s.id).copied().unwrap_or(usize::MAX);
            let fav = s.user_data.as_ref().map(|u| u.is_favorite).unwrap_or(false);
            let sort_name = s.name.to_lowercase();
            (nu_rank, fav, sort_name, s)
        })
        .collect();
    series_entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.2.cmp(&b.2)));

    // 3. All Movies -> grouped by Genre
    let movies_url = format!(
        "{base}/Users/{uid}/Items?IncludeItemTypes=Movie&Recursive=true&Fields=Overview,Genres,RunTimeTicks,UserData&SortBy=SortName&SortOrder=Ascending"
    );
    let raw_movies: RawItemsResp = get_json(&agent, &movies_url, key)?;
    let mut by_genre: BTreeMap<String, Vec<MediaItem>> = BTreeMap::new();
    for m in raw_movies.items {
        let item = m.to_media_item(&m.id);
        if m.genres.is_empty() {
            by_genre.entry("Movies".to_string()).or_default().push(item);
        } else {
            for g in m.genres {
                let g = g.trim().to_string();
                if !g.is_empty() {
                    by_genre.entry(g).or_default().push(item.clone());
                }
            }
        }
    }

    let mut channels = Vec::new();
    channels.push(Channel {
        number: 1,
        callsign: "GUIDE".into(),
        name: "Program Guide".into(),
        artwork_id: String::new(),
        kind: ChannelKind::Guide,
    });

    let mut ch_num: u16 = 2;
    for (i, (nu_rank, fav, _, s)) in series_entries.into_iter().enumerate() {
        let is_continue = nu_rank != usize::MAX;
        let next_ep = next_up_by_series.remove(&s.id);
        let next_ep_id = next_ep.as_ref().map(|e| e.id.clone());
        // Pre-fetch full episodes for top 8 series right away so the initial guide grid is rich!
        let (episodes, loaded) = if i < 8 {
            match fetch_series_episodes(&base, key, uid, &s.id) {
                Ok(eps) if !eps.is_empty() => (eps, true),
                _ => (next_ep.into_iter().collect(), false),
            }
        } else {
            (next_ep.into_iter().collect(), false)
        };
        channels.push(Channel {
            number: ch_num,
            callsign: make_callsign(&s.name),
            name: s.name,
            artwork_id: s.id.clone(),
            kind: ChannelKind::Series {
                series_id: s.id,
                overview: s.overview.unwrap_or_default(),
                is_continue,
                is_favorite: fav,
                episodes,
                next_up_episode_id: next_ep_id,
                loaded_episodes: loaded,
            },
        });
        ch_num = ch_num.saturating_add(1);
    }

    // Movie genre channels start at Ch 50 (or next round 10s block if >= 50 series)
    let mut movie_ch = if ch_num <= 50 { 50 } else { ((ch_num + 9) / 10) * 10 };
    for (genre, movies) in by_genre {
        if movies.is_empty() {
            continue;
        }
        let art = movies[0].artwork_id.clone();
        channels.push(Channel {
            number: movie_ch,
            callsign: make_callsign(&genre),
            name: format!("{genre} Movies"),
            artwork_id: art,
            kind: ChannelKind::MovieGenre { genre, movies },
        });
        movie_ch = movie_ch.saturating_add(1);
    }

    Ok(channels)
}

// ------------------------------------------------------------------ Playback Progress Sync

fn report_session(server_url: &str, api_key: &str, endpoint: &str, item_id: &str, pos_secs: f64, paused: bool) {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() || item_id.is_empty() {
        return;
    }
    let ticks = (pos_secs.max(0.0) * 10_000_000.0) as i64;
    let body = serde_json::json!({
        "ItemId": item_id,
        "PlaySessionId": format!("victr-{}", item_id),
        "PositionTicks": ticks,
        "IsPaused": paused,
        "CanSeek": true,
        "PlayMethod": "DirectStream"
    });
    let agent = make_agent();
    let _ = agent
        .post(&format!("{base}{endpoint}"))
        .set("X-Emby-Token", api_key.trim())
        .set("Authorization", &auth_header(api_key))
        .send_json(body);
}

fn mark_played(server_url: &str, api_key: &str, user_id: &str, item_id: &str) {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() || user_id.is_empty() || item_id.is_empty() {
        return;
    }
    let agent = make_agent();
    let _ = agent
        .post(&format!("{base}/Users/{user_id}/PlayedItems/{item_id}"))
        .set("X-Emby-Token", api_key.trim())
        .set("Authorization", &auth_header(api_key))
        .call();
}

fn mark_unplayed(server_url: &str, api_key: &str, user_id: &str, item_id: &str) {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() || user_id.is_empty() || item_id.is_empty() {
        return;
    }
    let agent = make_agent();
    let _ = agent
        .delete(&format!("{base}/Users/{user_id}/PlayedItems/{item_id}"))
        .set("X-Emby-Token", api_key.trim())
        .set("Authorization", &auth_header(api_key))
        .call();
}

// ------------------------------------------------------------------ Async Manager

/// Async messages returned from background Jellyfin worker threads.
pub enum JellyfinEvent {
    UsersLoaded(Result<Vec<JellyfinUser>, String>),
    LineupLoaded(Result<Vec<Channel>, String>),
    EpisodesLoaded(String, Result<Vec<MediaItem>, String>),
    LibraryScanTriggered(Result<(), String>),
    ArtworkLoaded(String, ArtworkImage),
}

pub struct ActiveWatch {
    pub channel_number: u16,
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

pub struct JellyfinManager {
    pub cfg: JellyfinConfig,
    pub users: Vec<JellyfinUser>,
    pub channels: Vec<Channel>,
    pub connected: bool,
    pub busy: bool,
    pub status: String,
    pub active_watch: Option<ActiveWatch>,
    pub artwork_cache: HashMap<String, Option<ArtworkImage>>,
    episodes_loading: HashMap<String, bool>,
    events: Arc<Mutex<Vec<JellyfinEvent>>>,
}

impl JellyfinManager {
    pub fn new_offline() -> Self {
        Self {
            cfg: JellyfinConfig::default(),
            users: Vec::new(),
            channels: Vec::new(),
            connected: false,
            busy: false,
            status: String::new(),
            active_watch: None,
            artwork_cache: HashMap::new(),
            episodes_loading: HashMap::new(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn new() -> Self {
        let cfg = load_config();
        let mut mgr = Self {
            cfg,
            ..Self::new_offline()
        };
        if mgr.cfg.is_complete() {
            mgr.connect_and_load();
        }
        mgr
    }

    pub fn fetch_users_async(&mut self) {
        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let ev = self.events.clone();
        self.busy = true;
        self.status = "Connecting to Jellyfin…".into();
        std::thread::spawn(move || {
            let res = fetch_users(&url, &key);
            ev.lock().unwrap().push(JellyfinEvent::UsersLoaded(res));
        });
    }

    pub fn connect_and_load(&mut self) {
        let _ = save_config(&self.cfg);
        let cfg = self.cfg.clone();
        let ev = self.events.clone();
        self.episodes_loading.clear();
        self.artwork_cache.clear();
        self.busy = true;
        self.status = "Loading Jellyfin channels…".into();
        std::thread::spawn(move || {
            let res = fetch_lineup(&cfg);
            ev.lock().unwrap().push(JellyfinEvent::LineupLoaded(res));
        });
    }

    pub fn scan_server_library(&mut self) {
        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let ev = self.events.clone();
        self.busy = true;
        self.status = "Requesting Jellyfin server library scan…".into();
        std::thread::spawn(move || {
            let res = trigger_library_scan(&url, &key);
            ev.lock().unwrap().push(JellyfinEvent::LibraryScanTriggered(res));
        });
    }

    pub fn disconnect(&mut self) {
        self.finish_watch();
        self.channels.clear();
        self.connected = false;
        self.status = "Disconnected.".into();
    }

    pub fn reload_series_episodes(&mut self, series_id: &str) {
        if series_id.is_empty() {
            return;
        }
        self.episodes_loading.remove(series_id);
        for ch in &mut self.channels {
            if let ChannelKind::Series { series_id: sid, loaded_episodes, .. } = &mut ch.kind {
                if sid == series_id {
                    *loaded_episodes = false;
                }
            }
        }
        self.ensure_series_episodes(series_id);
    }

    pub fn ensure_series_episodes(&mut self, series_id: &str) {
        if series_id.is_empty() || self.episodes_loading.contains_key(series_id) {
            return;
        }
        let needs = self.channels.iter().any(|c| match &c.kind {
            ChannelKind::Series { series_id: sid, loaded_episodes, .. } => sid == series_id && !loaded_episodes,
            _ => false,
        });
        if !needs {
            return;
        }
        self.episodes_loading.insert(series_id.to_string(), true);
        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let uid = self.cfg.user_id.clone();
        let sid = series_id.to_string();
        let ev = self.events.clone();
        std::thread::spawn(move || {
            let res = fetch_series_episodes(&url, &key, &uid, &sid);
            ev.lock().unwrap().push(JellyfinEvent::EpisodesLoaded(sid, res));
        });
    }

    pub fn request_artwork(&mut self, item_id: &str) -> Option<ArtworkImage> {
        if item_id.is_empty() {
            return None;
        }
        if let Some(cached) = self.artwork_cache.get(item_id) {
            return cached.clone();
        }
        // Mark as in-flight (None) so we only fetch once
        self.artwork_cache.insert(item_id.to_string(), None);
        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let id = item_id.to_string();
        let ev = self.events.clone();
        std::thread::spawn(move || {
            if let Some(img) = fetch_artwork(&url, &key, &id) {
                ev.lock().unwrap().push(JellyfinEvent::ArtworkLoaded(id, img));
            }
        });
        None
    }

    /// Drains background worker results. Returns true if lineup or artwork changed.
    pub fn poll(&mut self) -> bool {
        let pending: Vec<JellyfinEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        if pending.is_empty() {
            return false;
        }
        let mut changed = false;
        for ev in pending {
            match ev {
                JellyfinEvent::UsersLoaded(res) => {
                    self.busy = false;
                    match res {
                        Ok(users) => {
                            self.users = users;
                            if self.cfg.user_id.is_empty() {
                                if let Some(first) = self.users.first() {
                                    self.cfg.user_id = first.id.clone();
                                    self.cfg.user_name = first.name.clone();
                                }
                            }
                            self.status = format!("Found {} user(s).", self.users.len());
                            if self.cfg.is_complete() {
                                self.connect_and_load();
                            }
                        }
                        Err(e) => {
                            self.status = e;
                        }
                    }
                    changed = true;
                }
                JellyfinEvent::LineupLoaded(res) => {
                    self.busy = false;
                    match res {
                        Ok(chs) => {
                            let n_series = chs.iter().filter(|c| matches!(c.kind, ChannelKind::Series { .. })).count();
                            let n_genres = chs.iter().filter(|c| matches!(c.kind, ChannelKind::MovieGenre { .. })).count();
                            self.channels = chs;
                            self.connected = true;
                            self.episodes_loading.clear();
                            self.status = format!("Connected · {n_series} series, {n_genres} movie genres");
                        }
                        Err(e) => {
                            self.connected = false;
                            self.status = e;
                        }
                    }
                    changed = true;
                }
                JellyfinEvent::EpisodesLoaded(sid, res) => {
                    self.episodes_loading.remove(&sid);
                    match res {
                        Ok(eps) => {
                            for ch in &mut self.channels {
                                if let ChannelKind::Series { series_id, episodes, loaded_episodes, .. } = &mut ch.kind {
                                    if series_id == &sid {
                                        *episodes = eps.clone();
                                        *loaded_episodes = true;
                                        changed = true;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            self.status = format!("Episode fetch failed: {e}");
                            changed = true;
                        }
                    }
                }
                JellyfinEvent::LibraryScanTriggered(res) => {
                    self.busy = false;
                    match res {
                        Ok(()) => {
                            self.status = "Jellyfin library scan triggered — reloading guide…".into();
                            self.connect_and_load();
                        }
                        Err(e) => {
                            self.status = e;
                        }
                    }
                    changed = true;
                }
                JellyfinEvent::ArtworkLoaded(id, img) => {
                    self.artwork_cache.insert(id, Some(img));
                    changed = true;
                }
            }
        }
        changed
    }

    /// Filtered view of channels respecting Favorites-only and Search filter (always keeping CH 01 Guide).
    pub fn visible_channels(&self) -> Vec<&Channel> {
        let q = self.cfg.filter_text.trim().to_lowercase();
        let fav_only = self.cfg.favorites_only;
        self.channels
            .iter()
            .filter(|ch| match &ch.kind {
                ChannelKind::Guide => true,
                ChannelKind::Series { is_favorite, is_continue, .. } => {
                    let fav_ok = !fav_only || *is_favorite || *is_continue;
                    let q_ok = q.is_empty()
                        || ch.name.to_lowercase().contains(&q)
                        || ch.callsign.to_lowercase().contains(&q);
                    fav_ok && q_ok
                }
                ChannelKind::MovieGenre { genre, .. } => {
                    let q_ok = q.is_empty()
                        || genre.to_lowercase().contains(&q)
                        || ch.name.to_lowercase().contains(&q);
                    !fav_only && q_ok
                }
            })
            .collect()
    }

    pub fn channel_by_number(&self, number: u16) -> Option<&Channel> {
        self.channels.iter().find(|c| c.number == number)
    }

    /// Marks an episode or movie as watched/unwatched locally and syncs to Jellyfin in background.
    pub fn set_item_played(&mut self, channel_number: u16, item_id: &str, played: bool) {
        for ch in &mut self.channels {
            if ch.number != channel_number {
                continue;
            }
            match &mut ch.kind {
                ChannelKind::Series { episodes, next_up_episode_id, .. } => {
                    if let Some(ep) = episodes.iter_mut().find(|e| e.id == item_id) {
                        ep.played = played;
                        if played {
                            ep.resume_secs = 0.0;
                        }
                    }
                    // Recompute Next Up cleanly: first episode with resume > 10s, else first unplayed
                    *next_up_episode_id = episodes
                        .iter()
                        .find(|e| e.resume_secs > 10.0)
                        .or_else(|| episodes.iter().find(|e| !e.played))
                        .or_else(|| episodes.first())
                        .map(|e| e.id.clone());
                }
                ChannelKind::MovieGenre { movies, .. } => {
                    if let Some(m) = movies.iter_mut().find(|m| m.id == item_id) {
                        m.played = played;
                        if played {
                            m.resume_secs = 0.0;
                        }
                    }
                }
                ChannelKind::Guide => {}
            }
        }
        if let Some(w) = &mut self.active_watch {
            if w.item_id == item_id {
                w.marked_played = played;
            }
        }
        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let uid = self.cfg.user_id.clone();
        let id = item_id.to_string();
        if !url.is_empty() && !key.trim().is_empty() && !uid.is_empty() {
            std::thread::spawn(move || {
                if played {
                    mark_played(&url, &key, &uid, &id);
                } else {
                    mark_unplayed(&url, &key, &uid, &id);
                }
            });
        }
    }

    pub fn begin_watch(&mut self, target: &TuneTarget) {
        self.finish_watch();
        self.active_watch = Some(ActiveWatch {
            channel_number: target.channel_number,
            item_id: target.item.id.clone(),
            title: target.item.short_label(),
            callsign: target.callsign.clone(),
            duration_secs: target.item.runtime_secs,
            dwell_secs: 0.0,
            reported_start: false,
            marked_played: target.item.played,
            last_progress_secs: 0.0,
            last_pos: target.start_secs,
            last_paused: false,
        });
    }

    /// Updates watch dwell timer & sends Jellyfin progress/watched callbacks in background.
    pub fn tick_watch(&mut self, dt: f32, pos: f64, dur: f64, paused: bool) {
        let Some(w) = self.active_watch.as_mut() else { return };
        if !paused {
            w.dwell_secs += dt.clamp(0.0, 0.25);
        }
        w.last_pos = pos;
        w.last_paused = paused;
        if dur > 10.0 {
            w.duration_secs = dur;
        }

        // Wait 20 seconds of actual viewing before reporting to Jellyfin so surfing stays clean.
        if w.dwell_secs < 20.0 {
            return;
        }

        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let uid = self.cfg.user_id.clone();
        let item_id = w.item_id.clone();

        if !w.reported_start {
            w.reported_start = true;
            w.last_progress_secs = w.dwell_secs;
            std::thread::spawn(move || {
                report_session(&url, &key, "/Sessions/Playing", &item_id, pos, paused);
            });
            return;
        }

        // Periodic progress every 10 seconds
        if w.dwell_secs - w.last_progress_secs >= 10.0 {
            w.last_progress_secs = w.dwell_secs;
            let u2 = url.clone();
            let k2 = key.clone();
            let id2 = item_id.clone();
            std::thread::spawn(move || {
                report_session(&u2, &k2, "/Sessions/Playing/Progress", &id2, pos, paused);
            });
        }

        // Mark watched at 90% completion (matches Jellyfin default threshold)
        if !w.marked_played && w.duration_secs > 30.0 && pos >= w.duration_secs * 0.90 {
            w.marked_played = true;
            let ch_num = w.channel_number;
            let id_mark = item_id.clone();
            // Also update local episode played flag so the guide advances immediately!
            for ch in &mut self.channels {
                if ch.number == ch_num {
                    if let ChannelKind::Series { episodes, next_up_episode_id, .. } = &mut ch.kind {
                        if let Some(idx) = episodes.iter().position(|e| e.id == id_mark) {
                            episodes[idx].played = true;
                            episodes[idx].resume_secs = 0.0;
                            *next_up_episode_id = episodes.get(idx + 1).map(|e| e.id.clone());
                        }
                    }
                }
            }
            std::thread::spawn(move || {
                mark_played(&url, &key, &uid, &item_id);
            });
        }
    }

    pub fn finish_watch(&mut self) {
        let Some(w) = self.active_watch.take() else { return };
        // Update local resume position if watched >20s and not yet finished
        if w.reported_start && !w.marked_played {
            for ch in &mut self.channels {
                if ch.number == w.channel_number {
                    if let ChannelKind::Series { episodes, .. } = &mut ch.kind {
                        if let Some(ep) = episodes.iter_mut().find(|e| e.id == w.item_id) {
                            ep.resume_secs = w.last_pos;
                        }
                    }
                }
            }
            let url = self.cfg.normalized_url();
            let key = self.cfg.api_key.clone();
            let item_id = w.item_id;
            let pos = w.last_pos;
            std::thread::spawn(move || {
                report_session(&url, &key, "/Sessions/Playing/Stopped", &item_id, pos, false);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callsign_generation_is_clean() {
        assert_eq!(make_callsign("Seinfeld"), "SEINF");
        assert_eq!(make_callsign("The X-Files"), "XFILE");
        assert_eq!(make_callsign("Star Trek: The Next Generation"), "STTNG");
        assert_eq!(make_callsign("Sci-Fi"), "SCIFI");
    }

    #[test]
    fn live_movie_schedule_is_deterministic_and_within_runtime() {
        let movies = vec![
            MediaItem {
                id: "m1".into(),
                title: "Blade Runner".into(),
                season: None,
                episode: None,
                overview: String::new(),
                runtime_secs: 7020.0,
                played: false,
                resume_secs: 0.0,
                artwork_id: "m1".into(),
            },
            MediaItem {
                id: "m2".into(),
                title: "Alien".into(),
                season: None,
                episode: None,
                overview: String::new(),
                runtime_secs: 7000.0,
                played: false,
                resume_secs: 0.0,
                artwork_id: "m2".into(),
            },
        ];
        let (a, off_a) = schedule_live_movie(&movies, 50, 1_700_000_123).unwrap();
        let (b, off_b) = schedule_live_movie(&movies, 50, 1_700_000_123).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(off_a, off_b);
        assert!(off_a >= 0.0 && off_a <= a.runtime_secs);
    }

    #[test]
    fn toggle_played_updates_next_up_and_prev_next() {
        let mut mgr = JellyfinManager::new_offline();
        let eps = vec![
            MediaItem {
                id: "e1".into(),
                title: "Pilot".into(),
                season: Some(1),
                episode: Some(1),
                overview: String::new(),
                runtime_secs: 1500.0,
                played: false,
                resume_secs: 0.0,
                artwork_id: "s1".into(),
            },
            MediaItem {
                id: "e2".into(),
                title: "Second".into(),
                season: Some(1),
                episode: Some(2),
                overview: String::new(),
                runtime_secs: 1500.0,
                played: false,
                resume_secs: 0.0,
                artwork_id: "s1".into(),
            },
        ];
        mgr.channels.push(Channel {
            number: 2,
            callsign: "SHOW".into(),
            name: "Show".into(),
            artwork_id: "s1".into(),
            kind: ChannelKind::Series {
                series_id: "s1".into(),
                overview: String::new(),
                is_continue: false,
                is_favorite: false,
                episodes: eps,
                next_up_episode_id: Some("e1".into()),
                loaded_episodes: true,
            },
        });
        mgr.set_item_played(2, "e1", true);
        let ch = mgr.channel_by_number(2).unwrap();
        let next = ch.resolve_now("http://x", "k", 0).unwrap();
        assert_eq!(next.item.id, "e2");
        let prev = ch.prev_before("e2", "http://x", "k").unwrap();
        assert_eq!(prev.item.id, "e1");
        mgr.set_item_played(2, "e1", false);
        let ch = mgr.channel_by_number(2).unwrap();
        let back = ch.resolve_now("http://x", "k", 0).unwrap();
        assert_eq!(back.item.id, "e1");
    }
}
