//! Plex Media Server provider for ViCTR's unified 90s Cable Guide.
//!
//! - Config persistence (`plex.json` in platform config dir: Server URL + `X-Plex-Token`)
//! - Async background HTTP calls (`ureq` + `serde_json`) for Server identity, Library sections,
//!   On Deck / Continue Watching, TV Series, Episodes (`/allLeaves`), Movies (grouped by Genre),
//!   and transcoded/direct Artwork
//! - Direct-stream playback URLs (`/library/parts/...` with `X-Plex-Token`)
//! - Watch-progress & watched sync (`/:/timeline`, `/:/scrobble`, `/:/unscrobble`) with >20 s dwell gate

use crate::provider::{
    ActiveWatch, ArtworkImage, Channel, ChannelKind, MediaItem, TuneTarget, make_callsign,
    pick_next_episode_index,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ Config

fn default_auto_refresh_mins() -> u32 {
    10
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlexConfig {
    pub server_url: String,
    pub token: String,
    #[serde(default = "default_auto_refresh_mins")]
    pub auto_refresh_mins: u32,
}

impl Default for PlexConfig {
    fn default() -> Self {
        Self {
            server_url: String::new(),
            token: String::new(),
            auto_refresh_mins: default_auto_refresh_mins(),
        }
    }
}

impl PlexConfig {
    pub fn normalized_url(&self) -> String {
        self.server_url.trim().trim_end_matches('/').to_string()
    }

    pub fn is_complete(&self) -> bool {
        !self.normalized_url().is_empty() && !self.token.trim().is_empty()
    }
}

pub fn config_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("plex.json")))
}

pub fn load_config_from(path: &Path) -> PlexConfig {
    let Ok(text) = std::fs::read_to_string(path) else { return PlexConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_config_to(path: &Path, cfg: &PlexConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save Plex config: {e}"))
}

pub fn load_config() -> PlexConfig {
    config_path().map(|p| load_config_from(&p)).unwrap_or_default()
}

pub fn save_config(cfg: &PlexConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    save_config_to(&path, cfg)
}

// ------------------------------------------------------------------ HTTP Client

fn make_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(std::time::Duration::from_secs(6))
        .timeout(std::time::Duration::from_secs(30))
        .build()
}

fn make_fast_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(std::time::Duration::from_millis(1500))
        .timeout(std::time::Duration::from_millis(2500))
        .build()
}

fn resolve_redirect_url(cur_url: &str, location: &str) -> String {
    let loc = location.trim();
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return loc.to_string();
    }
    if let Some(scheme_end) = cur_url.find("://") {
        let after_scheme = &cur_url[scheme_end + 3..];
        let origin_len = after_scheme
            .find('/')
            .map(|p| scheme_end + 3 + p)
            .unwrap_or(cur_url.len());
        let origin = &cur_url[..origin_len];
        if loc.starts_with('/') {
            return format!("{origin}{loc}");
        }
        let base_dir = match cur_url[origin_len..].rfind('/') {
            Some(p) => &cur_url[..origin_len + p + 1],
            None => return format!("{origin}/{loc}"),
        };
        return format!("{base_dir}{loc}");
    }
    loc.to_string()
}

fn describe_http_err(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, resp) => {
            let status_text = resp.status_text().to_string();
            let body = resp.into_string().unwrap_or_default();
            let snippet: String = body.trim().chars().take(120).collect();
            if snippet.is_empty() {
                format!("{code} {status_text}")
            } else {
                format!("{code} {status_text}: {snippet}")
            }
        }
        ureq::Error::Transport(t) => format!("network error: {t}"),
    }
}

fn send_plex_req(agent: &ureq::Agent, method: &str, url: &str, token: &str) -> Result<ureq::Response, ureq::Error> {
    let mut cur_url = url.to_string();
    let tok = token.trim();
    for _ in 0..5 {
        let resp = agent
            .request(method, &cur_url)
            .set("Accept", "application/json")
            .set("X-Plex-Token", tok)
            .set("X-Plex-Client-Identifier", "victr-crt-receiver")
            .set("X-Plex-Product", "ViCTR")
            .set("X-Plex-Version", "0.1.0")
            .set("X-Plex-Device", "ViCTR CRT")
            .call()?;
        let status = resp.status();
        if (300..=399).contains(&status) {
            if let Some(loc) = resp.header("location") {
                cur_url = resolve_redirect_url(&cur_url, loc);
                continue;
            }
        }
        return Ok(resp);
    }
    agent
        .request(method, &cur_url)
        .set("Accept", "application/json")
        .set("X-Plex-Token", tok)
        .set("X-Plex-Client-Identifier", "victr-crt-receiver")
        .set("X-Plex-Product", "ViCTR")
        .set("X-Plex-Version", "0.1.0")
        .set("X-Plex-Device", "ViCTR CRT")
        .call()
}

fn get_json<T: for<'de> Deserialize<'de>>(agent: &ureq::Agent, url: &str, token: &str) -> Result<T, String> {
    let resp = send_plex_req(agent, "GET", url, token)
        .map_err(|e| format!("Plex request failed: {}", describe_http_err(e)))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("Plex request failed: HTTP {status} {}", resp.status_text()));
    }
    resp.into_json::<T>().map_err(|e| format!("Invalid Plex JSON: {e}"))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

pub fn build_stream_url(base: &str, token: &str, part_key: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    let tok = percent_encode(token.trim());
    if part_key.is_empty() {
        return String::new();
    }
    let sep = if part_key.contains('?') { '&' } else { '?' };
    format!("{b}{part_key}{sep}download=0&X-Plex-Token={tok}")
}

// ------------------------------------------------------------------ Plex JSON Schema

#[derive(Deserialize)]
struct PlexEnvelope {
    #[serde(rename = "MediaContainer")]
    container: PlexContainer,
}

#[derive(Deserialize, Default)]
struct PlexContainer {
    #[serde(rename = "friendlyName", default)]
    friendly_name: String,
    #[serde(rename = "Directory", default)]
    directories: Vec<PlexDirectory>,
    #[serde(rename = "Metadata", default)]
    metadata: Vec<PlexMetadata>,
}

#[derive(Deserialize)]
struct PlexDirectory {
    #[serde(default)]
    key: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    title: String,
}

#[derive(Deserialize, Default)]
struct PlexTag {
    #[serde(default)]
    tag: String,
}

#[derive(Deserialize, Default)]
struct PlexPart {
    #[serde(default)]
    key: String,
}

#[derive(Deserialize, Default)]
struct PlexMedia {
    #[serde(rename = "Part", default)]
    parts: Vec<PlexPart>,
}

#[derive(Deserialize, Default)]
struct PlexMetadata {
    #[serde(rename = "ratingKey", default)]
    rating_key: String,
    #[serde(rename = "grandparentRatingKey", default)]
    grandparent_rating_key: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(rename = "parentIndex", default)]
    parent_index: Option<i32>,
    #[serde(default)]
    index: Option<i32>,
    #[serde(default)]
    duration: Option<i64>,
    #[serde(rename = "viewOffset", default)]
    view_offset: Option<i64>,
    #[serde(rename = "viewCount", default)]
    view_count: Option<i64>,
    #[serde(default)]
    art: Option<String>,
    #[serde(default)]
    thumb: Option<String>,
    #[serde(rename = "grandparentArt", default)]
    grandparent_art: Option<String>,
    #[serde(rename = "grandparentThumb", default)]
    grandparent_thumb: Option<String>,
    #[serde(rename = "Genre", default)]
    genres: Vec<PlexTag>,
    #[serde(rename = "Media", default)]
    media: Vec<PlexMedia>,
}

impl PlexMetadata {
    fn first_part_key(&self) -> &str {
        self.media
            .iter()
            .flat_map(|m| m.parts.iter())
            .map(|p| p.key.as_str())
            .find(|k| !k.is_empty())
            .unwrap_or("")
    }

    fn preferred_art(&self, fallback: &str) -> String {
        for cand in [
            self.art.as_deref(),
            self.grandparent_art.as_deref(),
            self.thumb.as_deref(),
            self.grandparent_thumb.as_deref(),
        ] {
            if let Some(s) = cand.filter(|s| !s.is_empty()) {
                return s.to_string();
            }
        }
        fallback.to_string()
    }

    fn to_media_item(&self, base: &str, token: &str, fallback_art: &str, default_runtime_secs: f64) -> MediaItem {
        let runtime_secs = self.duration.unwrap_or(0) as f64 / 1000.0;
        let resume_secs = self.view_offset.unwrap_or(0) as f64 / 1000.0;
        let played = self.view_count.unwrap_or(0) > 0;
        let stream_url = build_stream_url(base, token, self.first_part_key());
        MediaItem {
            id: self.rating_key.clone(),
            title: self.title.clone(),
            season: self.parent_index.and_then(|n| u32::try_from(n).ok()),
            episode: self.index.and_then(|n| u32::try_from(n).ok()),
            overview: self.summary.clone(),
            runtime_secs: if runtime_secs > 10.0 { runtime_secs } else { default_runtime_secs },
            played,
            resume_secs,
            artwork_id: self.preferred_art(fallback_art),
            stream_url,
        }
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

// ------------------------------------------------------------------ Library Fetching

pub fn fetch_series_episodes(server_url: &str, token: &str, rating_key: &str) -> Result<Vec<MediaItem>, String> {
    let base = server_url.trim().trim_end_matches('/');
    let agent = make_agent();
    let url = format!("{base}/library/metadata/{rating_key}/allLeaves");
    let env: PlexEnvelope = get_json(&agent, &url, token)?;
    let mut eps: Vec<MediaItem> = env
        .container
        .metadata
        .iter()
        .filter(|m| !m.rating_key.is_empty() && !m.first_part_key().is_empty())
        .map(|m| m.to_media_item(base, token, "", 1800.0))
        .collect();
    sort_episodes(&mut eps);
    Ok(eps)
}

pub fn fetch_lineup(cfg: &PlexConfig) -> Result<(String, Vec<Channel>), String> {
    let base = cfg.normalized_url();
    let tok = cfg.token.trim();
    if base.is_empty() || tok.is_empty() {
        return Err("Enter both Plex Server URL and X-Plex-Token.".into());
    }
    let agent = make_agent();

    // 1. Server friendly name (also validates reachability & token)
    let root: PlexEnvelope = get_json(&agent, &format!("{base}/"), tok)?;
    let server_name = if root.container.friendly_name.is_empty() {
        "Plex Server".to_string()
    } else {
        root.container.friendly_name
    };

    // 2. On Deck (Continue Watching) episodes
    let on_deck_env: PlexEnvelope = get_json(&agent, &format!("{base}/library/onDeck"), tok).unwrap_or(PlexEnvelope {
        container: PlexContainer::default(),
    });
    let mut next_up_by_series: HashMap<String, MediaItem> = HashMap::new();
    let mut next_up_order: HashMap<String, usize> = HashMap::new();
    for (idx, ep) in on_deck_env.container.metadata.iter().enumerate() {
        if let Some(sid) = &ep.grandparent_rating_key {
            if !ep.first_part_key().is_empty() {
                next_up_by_series
                    .entry(sid.clone())
                    .or_insert_with(|| ep.to_media_item(&base, tok, "", 1800.0));
                next_up_order.entry(sid.clone()).or_insert(idx);
            }
        }
    }

    // 3. Library sections
    let sections: PlexEnvelope = get_json(&agent, &format!("{base}/library/sections"), tok)?;
    let mut raw_shows: Vec<PlexMetadata> = Vec::new();
    let mut by_genre: BTreeMap<String, Vec<MediaItem>> = BTreeMap::new();

    for sec in &sections.container.directories {
        if sec.key.is_empty() {
            continue;
        }
        if sec.kind.eq_ignore_ascii_case("show") {
            if let Ok(env) = get_json::<PlexEnvelope>(&agent, &format!("{base}/library/sections/{}/all?type=2", sec.key), tok) {
                raw_shows.extend(env.container.metadata);
            }
        } else if sec.kind.eq_ignore_ascii_case("movie") {
            if let Ok(env) = get_json::<PlexEnvelope>(&agent, &format!("{base}/library/sections/{}/all?type=1", sec.key), tok) {
                let fallback_genre = if sec.title.trim().is_empty() {
                    "Movies".to_string()
                } else {
                    sec.title.trim().to_string()
                };
                for m in env.container.metadata {
                    if m.rating_key.is_empty() || m.first_part_key().is_empty() {
                        continue;
                    }
                    let item = m.to_media_item(&base, tok, "", 5400.0);
                    if m.genres.is_empty() {
                        by_genre.entry(fallback_genre.clone()).or_default().push(item);
                    } else {
                        for g in &m.genres {
                            let tag = g.tag.trim();
                            if !tag.is_empty() {
                                by_genre.entry(tag.to_string()).or_default().push(item.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // Sort shows so On Deck shows pre-fetch episodes first
    raw_shows.sort_by(|a, b| {
        let ra = next_up_order.get(&a.rating_key).copied().unwrap_or(usize::MAX);
        let rb = next_up_order.get(&b.rating_key).copied().unwrap_or(usize::MAX);
        ra.cmp(&rb).then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });

    let mut channels = Vec::new();
    for (i, s) in raw_shows.into_iter().enumerate() {
        if s.rating_key.is_empty() {
            continue;
        }
        let is_continue = next_up_order.contains_key(&s.rating_key);
        let next_ep = next_up_by_series.remove(&s.rating_key);
        let next_ep_id = next_ep.as_ref().map(|e| e.id.clone());
        let art = s.preferred_art("");
        let (episodes, loaded) = if i < 8 {
            match fetch_series_episodes(&base, tok, &s.rating_key) {
                Ok(eps) if !eps.is_empty() => (eps, true),
                _ => (next_ep.into_iter().collect(), false),
            }
        } else {
            (next_ep.into_iter().collect(), false)
        };
        channels.push(Channel {
            number: 0,
            provider_id: "plex".into(),
            source_key: s.rating_key.clone(),
            callsign: make_callsign(&s.title),
            name: s.title,
            artwork_id: art,
            kind: ChannelKind::Series {
                series_id: s.rating_key,
                overview: s.summary,
                is_continue,
                is_favorite: false,
                episodes,
                next_up_episode_id: next_ep_id,
                loaded_episodes: loaded,
            },
        });
    }

    for (genre, movies) in by_genre {
        if movies.is_empty() {
            continue;
        }
        let art = movies
            .iter()
            .find(|m| !m.artwork_id.is_empty())
            .map(|m| m.artwork_id.clone())
            .unwrap_or_default();
        let ch_name = if genre.to_ascii_lowercase().contains("movie") {
            genre.clone()
        } else {
            format!("{genre} Movies")
        };
        channels.push(Channel {
            number: 0,
            provider_id: "plex".into(),
            source_key: format!("genre:{genre}"),
            callsign: make_callsign(&genre),
            name: ch_name,
            artwork_id: art,
            kind: ChannelKind::MovieGenre { genre, movies },
        });
    }

    Ok((server_name, channels))
}

pub fn trigger_library_refresh(server_url: &str, token: &str) -> Result<(), String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || token.trim().is_empty() {
        return Err("Plex server not configured.".into());
    }
    let agent = make_agent();
    let resp = send_plex_req(&agent, "GET", &format!("{base}/library/sections/all/refresh"), token)
        .map_err(|e| format!("Plex library refresh failed: {}", describe_http_err(e)))?;
    if !(200..300).contains(&resp.status()) {
        return Err(format!("Plex library refresh failed: HTTP {} {}", resp.status(), resp.status_text()));
    }
    Ok(())
}

pub fn fetch_artwork(server_url: &str, token: &str, art_path: &str) -> Option<ArtworkImage> {
    if art_path.is_empty() {
        return None;
    }
    let base = server_url.trim().trim_end_matches('/');
    let tok = token.trim();
    let agent = make_agent();
    let encoded = percent_encode(art_path);
    let urls = [
        format!("{base}/photo/:/transcode?width=320&height=240&minSize=1&upscale=1&url={encoded}"),
        format!("{base}{art_path}"),
    ];
    for url in urls {
        if let Ok(resp) = send_plex_req(&agent, "GET", &url, tok) {
            if !(200..300).contains(&resp.status()) {
                continue;
            }
            let mut bytes = Vec::new();
            if resp.into_reader().read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                if let Ok(img) = image::load_from_memory(&bytes) {
                    let thumb = img.thumbnail(320, 240).to_rgba8();
                    let (w, h) = thumb.dimensions();
                    if w > 0 && h > 0 {
                        return Some(Arc::new((w as usize, h as usize, thumb.into_raw())));
                    }
                }
            }
        }
    }
    None
}

// ------------------------------------------------------------------ Watch Progress Sync

fn report_timeline_with_agent(
    agent: &ureq::Agent,
    server_url: &str,
    token: &str,
    rating_key: &str,
    state: &str,
    pos_secs: f64,
    dur_secs: f64,
) {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || token.trim().is_empty() || rating_key.is_empty() {
        return;
    }
    let pos_ms = (pos_secs.max(0.0) * 1000.0) as u64;
    let dur_ms = (dur_secs.max(0.0) * 1000.0) as u64;
    let key_enc = percent_encode(&format!("/library/metadata/{rating_key}"));
    let url = format!(
        "{base}/:/timeline?ratingKey={rating_key}&key={key_enc}&state={state}&time={pos_ms}&duration={dur_ms}&identifier=com.plexapp.plugins.library"
    );
    let _ = send_plex_req(agent, "GET", &url, token);
}

fn scrobble_with_agent(
    agent: &ureq::Agent,
    server_url: &str,
    token: &str,
    rating_key: &str,
    played: bool,
) -> Result<(), String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || token.trim().is_empty() || rating_key.is_empty() {
        return Err("Plex server not configured.".into());
    }
    let action = if played { "scrobble" } else { "unscrobble" };
    let url = format!("{base}/:/{action}?identifier=com.plexapp.plugins.library&key={rating_key}");
    let resp = send_plex_req(agent, "GET", &url, token)
        .map_err(|e| format!("Plex {action} failed: {}", describe_http_err(e)))?;
    if !(200..300).contains(&resp.status()) {
        return Err(format!("Plex {action} failed: HTTP {} {}", resp.status(), resp.status_text()));
    }
    Ok(())
}

// ------------------------------------------------------------------ Async Manager

enum PlexEvent {
    LineupLoaded(Result<(String, Vec<Channel>), String>),
    EpisodesLoaded(String, Result<Vec<MediaItem>, String>),
    ScanTriggered(Result<(), String>),
    PlayedSynced(Result<(), String>),
    ArtworkLoaded(String, ArtworkImage),
}

pub struct PlexManager {
    pub cfg: PlexConfig,
    pub server_name: String,
    pub channels: Vec<Channel>,
    pub connected: bool,
    pub busy: bool,
    pub status: String,
    pub last_error: String,
    pub active_watch: Option<ActiveWatch>,
    pub last_sync_at: Option<std::time::Instant>,
    background_refresh: bool,
    pending_played_writes: usize,
    episodes_loading: HashMap<String, bool>,
    episodes_failed_at: HashMap<String, std::time::Instant>,
    artwork_cache: HashMap<String, Option<ArtworkImage>>,
    events: Arc<Mutex<Vec<PlexEvent>>>,
}

impl PlexManager {
    pub fn new_offline() -> Self {
        Self {
            cfg: PlexConfig::default(),
            server_name: String::new(),
            channels: Vec::new(),
            connected: false,
            busy: false,
            status: String::new(),
            last_error: String::new(),
            active_watch: None,
            last_sync_at: None,
            background_refresh: false,
            pending_played_writes: 0,
            episodes_loading: HashMap::new(),
            episodes_failed_at: HashMap::new(),
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
        if mgr.cfg.is_complete() {
            mgr.connect_and_load();
        }
        mgr
    }

    pub fn connect_and_load(&mut self) {
        let _ = save_config(&self.cfg);
        let cfg = self.cfg.clone();
        let ev = self.events.clone();
        self.episodes_loading.clear();
        self.episodes_failed_at.clear();
        self.background_refresh = false;
        self.busy = true;
        self.last_error.clear();
        self.last_sync_at = Some(std::time::Instant::now());
        self.status = "Loading Plex channels…".into();
        std::thread::spawn(move || {
            let res = fetch_lineup(&cfg);
            ev.lock().unwrap().push(PlexEvent::LineupLoaded(res));
        });
    }

    pub fn refresh_in_background(&mut self) {
        if self.busy || !self.cfg.is_complete() {
            return;
        }
        let cfg = self.cfg.clone();
        let ev = self.events.clone();
        self.background_refresh = true;
        self.busy = true;
        self.last_sync_at = Some(std::time::Instant::now());
        std::thread::spawn(move || {
            let res = fetch_lineup(&cfg);
            ev.lock().unwrap().push(PlexEvent::LineupLoaded(res));
        });
    }

    pub fn scan_server_library(&mut self) {
        let url = self.cfg.normalized_url();
        let tok = self.cfg.token.clone();
        let ev = self.events.clone();
        self.busy = true;
        self.status = "Requesting Plex library refresh…".into();
        std::thread::spawn(move || {
            let res = trigger_library_refresh(&url, &tok);
            if res.is_ok() {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            ev.lock().unwrap().push(PlexEvent::ScanTriggered(res));
        });
    }

    pub fn disconnect(&mut self) {
        self.finish_watch();
        self.channels.clear();
        self.artwork_cache.clear();
        self.episodes_loading.clear();
        self.episodes_failed_at.clear();
        self.connected = false;
        self.server_name.clear();
        self.last_sync_at = None;
        self.background_refresh = false;
        self.status = "Disconnected.".into();
    }

    pub fn last_sync_label(&self) -> Option<String> {
        let t = self.last_sync_at?;
        let secs = t.elapsed().as_secs();
        if secs < 45 {
            Some("just now".into())
        } else if secs < 3600 {
            Some(format!("{}m ago", (secs + 30) / 60))
        } else {
            Some(format!("{}h ago", secs / 3600))
        }
    }

    pub fn reload_series_episodes(&mut self, series_id: &str) {
        if series_id.is_empty() {
            return;
        }
        self.episodes_loading.remove(series_id);
        self.episodes_failed_at.remove(series_id);
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
        if self
            .episodes_failed_at
            .get(series_id)
            .is_some_and(|t| t.elapsed().as_secs() < 15)
        {
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
        let tok = self.cfg.token.clone();
        let sid = series_id.to_string();
        let ev = self.events.clone();
        std::thread::spawn(move || {
            let res = fetch_series_episodes(&url, &tok, &sid);
            ev.lock().unwrap().push(PlexEvent::EpisodesLoaded(sid, res));
        });
    }

    pub fn request_artwork(&mut self, art_path: &str) -> Option<ArtworkImage> {
        if art_path.is_empty() {
            return None;
        }
        if let Some(cached) = self.artwork_cache.get(art_path) {
            return cached.clone();
        }
        self.artwork_cache.insert(art_path.to_string(), None);
        let url = self.cfg.normalized_url();
        let tok = self.cfg.token.clone();
        let path = art_path.to_string();
        let ev = self.events.clone();
        std::thread::spawn(move || {
            if let Some(img) = fetch_artwork(&url, &tok, &path) {
                ev.lock().unwrap().push(PlexEvent::ArtworkLoaded(path, img));
            }
        });
        None
    }

    pub fn set_item_played(&mut self, item_id: &str, played: bool) {
        for ch in &mut self.channels {
            match &mut ch.kind {
                ChannelKind::Series { episodes, next_up_episode_id, .. } => {
                    if let Some(ep) = episodes.iter_mut().find(|e| e.id == item_id) {
                        ep.played = played;
                        if played {
                            ep.resume_secs = 0.0;
                        }
                        *next_up_episode_id = pick_next_episode_index(episodes, None).map(|i| episodes[i].id.clone());
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
        if let Some(w) = &mut self.active_watch {
            if w.item_id == item_id {
                w.marked_played = played;
            }
        }
        let url = self.cfg.normalized_url();
        let tok = self.cfg.token.clone();
        let id = item_id.to_string();
        let ev = self.events.clone();
        if !url.is_empty() && !tok.trim().is_empty() {
            self.pending_played_writes += 1;
            std::thread::spawn(move || {
                let res = scrobble_with_agent(&make_agent(), &url, &tok, &id, played);
                ev.lock().unwrap().push(PlexEvent::PlayedSynced(res));
            });
        }
    }

    pub fn begin_watch(&mut self, target: &TuneTarget) {
        self.finish_watch();
        if target.provider_id != "plex" {
            return;
        }
        self.active_watch = Some(ActiveWatch {
            channel_number: target.channel_number,
            provider_id: target.provider_id.clone(),
            source_key: target.source_key.clone(),
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

    /// Updates watch dwell timer & reports timeline/scrobble to Plex in background.
    /// Returns `true` if local channel watched state changed.
    pub fn tick_watch(&mut self, dt: f32, pos: f64, dur: f64, paused: bool) -> bool {
        let Some(w) = self.active_watch.as_mut() else { return false };
        if !paused {
            w.dwell_secs += dt.clamp(0.0, 0.25);
        }
        w.last_pos = pos;
        w.last_paused = paused;
        if dur > 10.0 {
            w.duration_secs = dur;
        }
        if w.dwell_secs < 20.0 {
            return false;
        }

        let url = self.cfg.normalized_url();
        let tok = self.cfg.token.clone();
        let item_id = w.item_id.clone();
        let duration = w.duration_secs;
        let state = if paused { "paused" } else { "playing" };

        if !w.reported_start {
            w.reported_start = true;
            w.last_progress_secs = w.dwell_secs;
            std::thread::spawn(move || {
                report_timeline_with_agent(&make_agent(), &url, &tok, &item_id, state, pos, duration);
            });
            return false;
        }

        if w.dwell_secs - w.last_progress_secs >= 10.0 {
            w.last_progress_secs = w.dwell_secs;
            let u2 = url.clone();
            let t2 = tok.clone();
            let id2 = item_id.clone();
            std::thread::spawn(move || {
                report_timeline_with_agent(&make_agent(), &u2, &t2, &id2, state, pos, duration);
            });
        }

        if !w.marked_played && w.duration_secs > 30.0 && pos >= w.duration_secs * 0.90 {
            w.marked_played = true;
            let id_mark = item_id.clone();
            for ch in &mut self.channels {
                if let ChannelKind::Series { episodes, next_up_episode_id, .. } = &mut ch.kind {
                    if let Some(idx) = episodes.iter().position(|e| e.id == id_mark) {
                        episodes[idx].played = true;
                        episodes[idx].resume_secs = 0.0;
                        *next_up_episode_id = episodes.get(idx + 1).map(|e| e.id.clone());
                    }
                }
            }
            std::thread::spawn(move || {
                let _ = scrobble_with_agent(&make_agent(), &url, &tok, &item_id, true);
            });
            return true;
        }
        false
    }

    pub fn finish_watch(&mut self) {
        let _ = self.finish_watch_inner(false);
    }

    pub fn finish_watch_async_joinable(&mut self) -> Option<std::thread::JoinHandle<()>> {
        self.finish_watch_inner(false)
    }

    pub fn finish_watch_sync(&mut self) {
        let _ = self.finish_watch_inner(true);
    }

    fn finish_watch_inner(&mut self, blocking: bool) -> Option<std::thread::JoinHandle<()>> {
        let Some(w) = self.active_watch.take() else { return None };
        if !w.reported_start {
            return None;
        }
        let url = self.cfg.normalized_url();
        let tok = self.cfg.token.clone();
        let item_id = w.item_id;
        let pos = w.last_pos;
        let dur = w.duration_secs;
        let finished_90 = !w.marked_played && dur > 30.0 && (pos / dur) >= 0.90;

        if !w.marked_played {
            for ch in &mut self.channels {
                if let ChannelKind::Series { episodes, .. } = &mut ch.kind {
                    if let Some(ep) = episodes.iter_mut().find(|e| e.id == item_id) {
                        if finished_90 {
                            ep.played = true;
                            ep.resume_secs = 0.0;
                        } else {
                            ep.resume_secs = pos;
                        }
                    }
                }
            }
        }

        let flush = move || {
            let agent = make_fast_agent();
            if finished_90 {
                let _ = scrobble_with_agent(&agent, &url, &tok, &item_id, true);
            }
            if !w.marked_played || finished_90 {
                report_timeline_with_agent(&agent, &url, &tok, &item_id, "stopped", pos, dur);
            }
        };

        if blocking {
            flush();
            None
        } else {
            Some(std::thread::spawn(flush))
        }
    }

    /// Drains background worker events and triggers periodic auto-sync.
    /// Returns `true` if channels or artwork changed.
    pub fn poll(&mut self) -> bool {
        if self.connected
            && !self.busy
            && self.pending_played_writes == 0
            && self.cfg.auto_refresh_mins > 0
            && self
                .last_sync_at
                .is_some_and(|t| t.elapsed().as_secs() >= self.cfg.auto_refresh_mins as u64 * 60)
        {
            self.refresh_in_background();
        }
        let pending: Vec<PlexEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        if pending.is_empty() {
            return false;
        }
        let mut changed = false;
        for ev in pending {
            match ev {
                PlexEvent::LineupLoaded(res) => {
                    self.busy = false;
                    let was_bg = std::mem::take(&mut self.background_refresh);
                    match res {
                        Ok((srv_name, chs)) => {
                            let n_series = chs.iter().filter(|c| matches!(c.kind, ChannelKind::Series { .. })).count();
                            let n_genres = chs.iter().filter(|c| matches!(c.kind, ChannelKind::MovieGenre { .. })).count();
                            self.server_name = srv_name;
                            self.channels = chs;
                            self.connected = true;
                            self.episodes_loading.clear();
                            self.last_sync_at = Some(std::time::Instant::now());
                            self.status = format!("{} · {n_series} series, {n_genres} movie genres", self.server_name);
                        }
                        Err(e) => {
                            if was_bg {
                                self.status = format!("Auto-sync skipped: {e}");
                            } else {
                                self.connected = false;
                                self.status = e;
                            }
                        }
                    }
                    changed = true;
                }
                PlexEvent::EpisodesLoaded(sid, res) => {
                    self.episodes_loading.remove(&sid);
                    match res {
                        Ok(eps) => {
                            self.episodes_failed_at.remove(&sid);
                            for ch in &mut self.channels {
                                if let ChannelKind::Series {
                                    series_id,
                                    episodes,
                                    next_up_episode_id,
                                    loaded_episodes,
                                    ..
                                } = &mut ch.kind
                                {
                                    if series_id == &sid {
                                        *episodes = eps.clone();
                                        *loaded_episodes = true;
                                        let valid_nu = next_up_episode_id.as_deref().is_some_and(|id| {
                                            episodes.iter().any(|e| e.id == id && (!e.played || e.resume_secs > 10.0))
                                        });
                                        if !valid_nu {
                                            *next_up_episode_id =
                                                pick_next_episode_index(episodes, None).map(|i| episodes[i].id.clone());
                                        }
                                        changed = true;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            self.episodes_failed_at.insert(sid, std::time::Instant::now());
                            self.status = format!("Plex episode fetch failed: {e}");
                            changed = true;
                        }
                    }
                }
                PlexEvent::ScanTriggered(res) => {
                    self.busy = false;
                    match res {
                        Ok(()) => {
                            self.status = "Plex refresh triggered — reloading guide…".into();
                            self.connect_and_load();
                        }
                        Err(e) => {
                            self.status = e;
                        }
                    }
                    changed = true;
                }
                PlexEvent::PlayedSynced(res) => {
                    self.pending_played_writes = self.pending_played_writes.saturating_sub(1);
                    match res {
                        Ok(()) => self.last_error.clear(),
                        Err(msg) => {
                            self.last_error = msg;
                            changed = true;
                        }
                    }
                }
                PlexEvent::ArtworkLoaded(p, img) => {
                    self.artwork_cache.insert(p, Some(img));
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
    fn plex_lineup_episodes_and_scrobble_roundtrip() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        let srv = std::thread::spawn(move || {
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                assert!(req.contains("X-Plex-Token: secret-tok"), "missing Plex token header: {req}");
                assert!(req.contains("Accept: application/json"), "missing JSON accept header: {req}");

                let body = if first_line.starts_with("GET / ") {
                    r#"{"MediaContainer":{"friendlyName":"Living Room Plex"}}"#
                } else if first_line.starts_with("GET /library/onDeck ") {
                    r#"{"MediaContainer":{"Metadata":[{"ratingKey":"102","grandparentRatingKey":"100","title":"Bart the Genius","parentIndex":1,"index":2,"duration":1380000,"viewOffset":120000,"Media":[{"Part":[{"key":"/library/parts/102/file.mkv"}]}]}]}}"#
                } else if first_line.starts_with("GET /library/sections ") {
                    r#"{"MediaContainer":{"Directory":[{"key":"1","type":"show","title":"TV Shows"},{"key":"2","type":"movie","title":"Movies"}]}}"#
                } else if first_line.starts_with("GET /library/sections/1/all?type=2 ") {
                    r#"{"MediaContainer":{"Metadata":[{"ratingKey":"100","title":"The Simpsons","summary":"Springfield family.","art":"/library/metadata/100/art/1"}]}}"#
                } else if first_line.starts_with("GET /library/sections/2/all?type=1 ") {
                    r#"{"MediaContainer":{"Metadata":[{"ratingKey":"200","title":"Blade Runner","summary":"Replicants.","duration":7020000,"Genre":[{"tag":"Sci-Fi"}],"Media":[{"Part":[{"key":"/library/parts/200/br.mkv"}]}]}]}}"#
                } else if first_line.starts_with("GET /library/metadata/100/allLeaves ") {
                    r#"{"MediaContainer":{"Metadata":[
                        {"ratingKey":"102","title":"Bart the Genius","parentIndex":1,"index":2,"duration":1380000,"viewOffset":120000,"Media":[{"Part":[{"key":"/library/parts/102/file.mkv"}]}]},
                        {"ratingKey":"101","title":"Simpsons Roasting","parentIndex":1,"index":1,"duration":1380000,"viewCount":1,"Media":[{"Part":[{"key":"/library/parts/101/file.mkv"}]}]}
                    ]}}"#
                } else if first_line.starts_with("GET /:/scrobble?identifier=com.plexapp.plugins.library&key=102 ") {
                    "{}"
                } else if first_line.starts_with("GET /:/unscrobble?identifier=com.plexapp.plugins.library&key=102 ") {
                    "{}"
                } else {
                    "{}"
                };

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let cfg = PlexConfig {
            server_url: base.clone(),
            token: "secret-tok".into(),
            auto_refresh_mins: 10,
        };
        let (srv_name, chs) = fetch_lineup(&cfg).expect("fetch_lineup should succeed");
        assert_eq!(srv_name, "Living Room Plex");
        assert_eq!(chs.len(), 2);

        let simpsons = chs.iter().find(|c| c.name == "The Simpsons").unwrap();
        assert_eq!(simpsons.provider_id, "plex");
        let eps = simpsons.items();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].id, "101");
        assert!(eps[0].played);
        assert_eq!(eps[1].id, "102");
        assert!(!eps[1].played);
        assert!((eps[1].resume_secs - 120.0).abs() < 0.1);
        assert!(eps[1].stream_url.contains("/library/parts/102/file.mkv"));
        assert!(eps[1].stream_url.contains("X-Plex-Token=secret-tok"));

        let next = simpsons.resolve_now(0).unwrap();
        assert_eq!(next.item.id, "102");
        assert!((next.start_secs - 120.0).abs() < 0.1);

        let scifi = chs.iter().find(|c| c.name == "Sci-Fi Movies").unwrap();
        assert_eq!(scifi.items().len(), 1);
        assert_eq!(scifi.items()[0].title, "Blade Runner");

        let agent = make_agent();
        scrobble_with_agent(&agent, &base, "secret-tok", "102", true).unwrap();
        scrobble_with_agent(&agent, &base, "secret-tok", "102", false).unwrap();

        srv.join().unwrap();
    }
}
