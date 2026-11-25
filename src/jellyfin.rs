//! Jellyfin integration for ViCTR's 90s Cable Channel Guide:
//! - Config persistence (`jellyfin.json` in platform config dir, Server URL + API key + User ID)
//! - Async background HTTP calls (`ureq` + `serde_json`) for Users, Series, Episodes, Movies, and Artwork
//! - Virtual cable channel builder (Ch 01 Guide, Ch 02+ TV Series, Ch 50+ Movie Genres)
//! - Hybrid scheduling: Next Unwatched + auto-advance for Series, 24/7 deterministic broadcast clock for Movies
//! - Watch-progress & watched-status reporting (>20s dwell gate, 90% completion mark)

use crate::provider::{
    ActiveWatch, ArtworkImage, Channel, ChannelKind, MediaItem, TuneTarget, make_callsign,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ Config

fn default_auto_refresh_mins() -> u32 {
    10
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JellyfinConfig {
    pub server_url: String,
    pub api_key: String,
    pub user_id: String,
    pub user_name: String,
    #[serde(default)]
    pub favorites_only: bool,
    #[serde(default)]
    pub filter_text: String,
    /// Periodic background library sync interval in minutes (0 = manual only).
    #[serde(default = "default_auto_refresh_mins")]
    pub auto_refresh_mins: u32,
}

impl Default for JellyfinConfig {
    fn default() -> Self {
        Self {
            server_url: String::new(),
            api_key: String::new(),
            user_id: String::new(),
            user_name: String::new(),
            favorites_only: false,
            filter_text: String::new(),
            auto_refresh_mins: default_auto_refresh_mins(),
        }
    }
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

pub fn build_stream_url(server_url: &str, api_key: &str, item_id: &str) -> String {
    let base = server_url.trim().trim_end_matches('/');
    format!("{base}/Videos/{item_id}/stream?static=true&api_key={api_key}")
}

// ------------------------------------------------------------------ Jellyfin HTTP Client

fn make_agent() -> ureq::Agent {
    // Handle redirects manually so 301/302/307/308 (e.g. HTTP -> HTTPS reverse proxies)
    // preserve POST/DELETE methods, JSON bodies, and Jellyfin auth headers instead of
    // ureq returning an empty 308 response or downgrading POST to GET.
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

fn auth_header_no_token() -> &'static str {
    "MediaBrowser Client=\"ViCTR\", Device=\"ViCTR CRT\", DeviceId=\"victr-crt-receiver\", Version=\"0.1.0\""
}

fn auth_header(api_key: &str) -> String {
    format!("{}, Token=\"{}\"", auth_header_no_token(), api_key.trim())
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

/// Sends an HTTP request while following up to 5 redirects (301/302/303/307/308)
/// and preserving the HTTP method, Jellyfin auth headers, and JSON/empty body.
fn send_jellyfin_req(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    api_key: Option<&str>,
    json_body: Option<&serde_json::Value>,
) -> Result<ureq::Response, ureq::Error> {
    let mut cur_url = url.to_string();
    for _ in 0..5 {
        let mut req = agent.request(method, &cur_url);
        if let Some(key) = api_key {
            let k = key.trim();
            req = req.set("X-Emby-Token", k).set("Authorization", &auth_header(k));
        } else {
            req = req
                .set("Authorization", auth_header_no_token())
                .set("X-Emby-Authorization", auth_header_no_token());
        }
        let resp = if let Some(body) = json_body {
            req.send_json(body.clone())?
        } else if method.eq_ignore_ascii_case("POST") {
            req.send_bytes(&[])?
        } else {
            req.call()?
        };
        let status = resp.status();
        if (300..=399).contains(&status) {
            if let Some(loc) = resp.header("location") {
                cur_url = resolve_redirect_url(&cur_url, loc);
                continue;
            }
        }
        return Ok(resp);
    }
    // Final attempt without further redirects
    let mut req = agent.request(method, &cur_url);
    if let Some(key) = api_key {
        let k = key.trim();
        req = req.set("X-Emby-Token", k).set("Authorization", &auth_header(k));
    } else {
        req = req
            .set("Authorization", auth_header_no_token())
            .set("X-Emby-Authorization", auth_header_no_token());
    }
    if let Some(body) = json_body {
        req.send_json(body.clone())
    } else if method.eq_ignore_ascii_case("POST") {
        req.send_bytes(&[])
    } else {
        req.call()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawAuthResp {
    access_token: String,
    user: RawUser,
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

fn verify_played_resp(res: Result<ureq::Response, ureq::Error>, expected: bool) -> Result<(), String> {
    let resp = res.map_err(describe_http_err)?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status} {}", resp.status_text()));
    }
    if status == 204 {
        return Ok(());
    }
    let body = resp.into_string().unwrap_or_default();
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if let Ok(ud) = serde_json::from_str::<RawUserData>(trimmed) {
        if ud.played == expected {
            Ok(())
        } else {
            Err(format!("HTTP {status} returned Played={} (expected {expected})", ud.played))
        }
    } else {
        let snippet: String = trimmed.chars().take(100).collect();
        Err(format!("HTTP {status} unexpected body: {snippet}"))
    }
}

pub fn authenticate_by_name(server_url: &str, username: &str, password: &str) -> Result<(String, JellyfinUser), String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || username.trim().is_empty() {
        return Err("Enter both Server URL and Username.".into());
    }
    let agent = make_agent();
    let body = serde_json::json!({
        "Username": username.trim(),
        "Pw": password,
    });
    let resp = send_jellyfin_req(
        &agent,
        "POST",
        &format!("{base}/Users/AuthenticateByName"),
        None,
        Some(&body),
    )
    .map_err(|e| format!("Sign-in failed: {}", describe_http_err(e)))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("Sign-in failed: HTTP {status} {}", resp.status_text()));
    }
    let text = resp.into_string().unwrap_or_default();
    let parsed: RawAuthResp = serde_json::from_str(&text).map_err(|e| {
        let snippet: String = text.trim().chars().take(120).collect();
        format!("Invalid Jellyfin auth JSON (HTTP {status}): {e} [{snippet}]")
    })?;
    Ok((parsed.access_token, JellyfinUser { id: parsed.user.id, name: parsed.user.name }))
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
    parent_index_number: Option<i32>,
    #[serde(default)]
    index_number: Option<i32>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    run_time_ticks: Option<i64>,
    #[serde(default)]
    location_type: Option<String>,
    #[serde(default)]
    is_folder: bool,
    #[serde(default)]
    user_data: Option<RawUserData>,
}

impl RawItem {
    fn to_media_item(&self, base: &str, api_key: &str, fallback_art: &str) -> MediaItem {
        let ud = self.user_data.as_ref();
        let runtime_secs = self.run_time_ticks.unwrap_or(0) as f64 / 10_000_000.0;
        let resume_secs = ud.map(|u| u.playback_position_ticks as f64 / 10_000_000.0).unwrap_or(0.0);
        let art = self.series_id.clone().unwrap_or_else(|| {
            if fallback_art.is_empty() { self.id.clone() } else { fallback_art.to_string() }
        });
        MediaItem {
            id: self.id.clone(),
            title: self.name.clone(),
            season: self.parent_index_number.and_then(|n| u32::try_from(n).ok()),
            episode: self.index_number.and_then(|n| u32::try_from(n).ok()),
            overview: self.overview.clone().unwrap_or_default(),
            runtime_secs: if runtime_secs > 10.0 { runtime_secs } else { 1800.0 },
            played: ud.map(|u| u.played).unwrap_or(false),
            resume_secs,
            artwork_id: art,
            stream_url: build_stream_url(base, api_key, &self.id),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawScheduledTask {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    current_progress_percentage: Option<f64>,
}

fn get_json<T: for<'de> Deserialize<'de>>(agent: &ureq::Agent, url: &str, api_key: &str) -> Result<T, String> {
    let resp = send_jellyfin_req(agent, "GET", url, Some(api_key), None)
        .map_err(|e| format!("Jellyfin request failed: {}", describe_http_err(e)))?;
    let status = resp.status();
    if !(200..300).contains(&status) {
        return Err(format!("Jellyfin request failed: HTTP {status} {}", resp.status_text()));
    }
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

fn merge_and_sort_episodes(
    base: &str,
    api_key: &str,
    series_id: &str,
    primary: Vec<RawItem>,
    secondary: Vec<RawItem>,
) -> Vec<MediaItem> {
    let mut index_by_id: HashMap<String, usize> = HashMap::new();
    let mut raw: Vec<RawItem> = Vec::with_capacity(primary.len() + secondary.len());
    for item in primary.into_iter().chain(secondary.into_iter()) {
        if item.is_folder || item.location_type.as_deref() == Some("Virtual") {
            continue;
        }
        if let Some(&idx) = index_by_id.get(&item.id) {
            // Merge UserData if the second endpoint carried watched/resume info missing from the first.
            if let Some(ud_new) = item.user_data {
                match &mut raw[idx].user_data {
                    Some(ud_cur) => {
                        ud_cur.played |= ud_new.played;
                        ud_cur.is_favorite |= ud_new.is_favorite;
                        ud_cur.playback_position_ticks = ud_cur.playback_position_ticks.max(ud_new.playback_position_ticks);
                    }
                    None => raw[idx].user_data = Some(ud_new),
                }
            }
        } else {
            index_by_id.insert(item.id.clone(), raw.len());
            raw.push(item);
        }
    }
    // Sort cleanly by (season, episode, title), putting Season 0 specials & unnumbered after regular seasons
    raw.sort_by(|a, b| {
        let sa = match a.parent_index_number.and_then(|n| u32::try_from(n).ok()) {
            Some(0) => u32::MAX - 1,
            Some(n) => n,
            None => u32::MAX,
        };
        let sb = match b.parent_index_number.and_then(|n| u32::try_from(n).ok()) {
            Some(0) => u32::MAX - 1,
            Some(n) => n,
            None => u32::MAX,
        };
        let ea = a.index_number.and_then(|n| u32::try_from(n).ok()).unwrap_or(u32::MAX);
        let eb = b.index_number.and_then(|n| u32::try_from(n).ok()).unwrap_or(u32::MAX);
        sa.cmp(&sb)
            .then_with(|| ea.cmp(&eb))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    raw.iter().map(|r| r.to_media_item(base, api_key, series_id)).collect()
}

pub fn fetch_series_episodes(server_url: &str, api_key: &str, user_id: &str, series_id: &str) -> Result<Vec<MediaItem>, String> {
    let base = server_url.trim().trim_end_matches('/');
    let agent = make_agent();
    // Query both `/Shows/{id}/Episodes` AND `/Items?userId={uid}&ParentId={id}` (with legacy `/Users/{uid}/Items` fallback)
    // and merge by ID so every real episode file and its exact UserData (watched/resume state) shows up across Jellyfin 10.8–10.11+.
    let shows_url = format!(
        "{base}/Shows/{series_id}/Episodes?userId={user_id}&IsMissing=false&Fields=Overview,RunTimeTicks,UserData&EnableUserData=true"
    );
    let items_v2_url = format!(
        "{base}/Items?userId={user_id}&ParentId={series_id}&IncludeItemTypes=Episode,Video&Recursive=true&IsMissing=false&Fields=Overview,RunTimeTicks,UserData&EnableUserData=true"
    );
    let items_legacy_url = format!(
        "{base}/Users/{user_id}/Items?ParentId={series_id}&IncludeItemTypes=Episode,Video&Recursive=true&IsMissing=false&Fields=Overview,RunTimeTicks,UserData&EnableUserData=true"
    );
    let r_shows = get_json::<RawItemsResp>(&agent, &shows_url, api_key);
    let r_items = get_json::<RawItemsResp>(&agent, &items_v2_url, api_key)
        .or_else(|_| get_json::<RawItemsResp>(&agent, &items_legacy_url, api_key));
    match (r_shows, r_items) {
        (Ok(a), Ok(b)) => Ok(merge_and_sort_episodes(base, api_key, series_id, a.items, b.items)),
        (Ok(a), Err(_)) => Ok(merge_and_sort_episodes(base, api_key, series_id, a.items, Vec::new())),
        (Err(_), Ok(b)) => Ok(merge_and_sort_episodes(base, api_key, series_id, b.items, Vec::new())),
        (Err(e), Err(_)) => Err(e),
    }
}

pub fn trigger_library_scan(server_url: &str, api_key: &str) -> Result<(), String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() {
        return Err("Jellyfin server not configured.".into());
    }
    let agent = make_agent();
    let resp = send_jellyfin_req(&agent, "POST", &format!("{base}/Library/Refresh"), Some(api_key), None)
        .map_err(|e| format!("Library scan request failed: {}", describe_http_err(e)))?;
    if !(200..300).contains(&resp.status()) {
        return Err(format!("Library scan request failed: HTTP {} {}", resp.status(), resp.status_text()));
    }
    Ok(())
}

/// Triggers Jellyfin's asynchronous library scan and waits until the `RefreshLibrary`
/// scheduled task finishes (reporting progress updates along the way) before returning.
fn trigger_and_wait_library_scan(
    server_url: &str,
    api_key: &str,
    events: &Arc<Mutex<Vec<JellyfinEvent>>>,
) -> Result<(), String> {
    trigger_library_scan(server_url, api_key)?;
    let base = server_url.trim().trim_end_matches('/');
    let agent = make_agent();
    // Give Jellyfin a moment to spin up the scan task before polling `/ScheduledTasks`.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let tasks_url = format!("{base}/ScheduledTasks");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    let mut saw_running = false;
    while std::time::Instant::now() < deadline {
        match get_json::<Vec<RawScheduledTask>>(&agent, &tasks_url, api_key) {
            Ok(tasks) => {
                let scan_task = tasks.iter().find(|t| {
                    t.key.as_deref() == Some("RefreshLibrary")
                        || t.name.to_lowercase().contains("scan media library")
                });
                match scan_task {
                    Some(t) if t.state.eq_ignore_ascii_case("Running") => {
                        saw_running = true;
                        let msg = match t.current_progress_percentage {
                            Some(pct) => format!("Jellyfin scanning library ({:.0}%)…", pct.clamp(0.0, 100.0)),
                            None => "Jellyfin scanning library…".to_string(),
                        };
                        events.lock().unwrap().push(JellyfinEvent::ScanProgress(msg));
                        std::thread::sleep(std::time::Duration::from_millis(1500));
                    }
                    _ => {
                        if saw_running {
                            break;
                        }
                        // Brief extra settle wait in case scan finished immediately or hadn't marked Running yet.
                        std::thread::sleep(std::time::Duration::from_millis(1200));
                        break;
                    }
                }
            }
            Err(_) => {
                // Non-admin API keys may not have access to /ScheduledTasks; wait briefly then reload.
                std::thread::sleep(std::time::Duration::from_secs(2));
                break;
            }
        }
    }
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
        if let Ok(resp) = send_jellyfin_req(&agent, "GET", &url, Some(api_key), None) {
            if !(200..300).contains(&resp.status()) {
                continue;
            }
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
            next_up_by_series
                .entry(sid.clone())
                .or_insert_with(|| ep.to_media_item(&base, key, sid));
            next_up_order.entry(sid.clone()).or_insert(idx);
        }
    }

    // 2. All TV Series (prefer Jellyfin 10.9+ `/Items?userId=`, fallback to legacy `/Users/{uid}/Items`)
    let series_v2_url = format!(
        "{base}/Items?userId={uid}&IncludeItemTypes=Series&Recursive=true&Fields=Overview,Genres,UserData&EnableUserData=true&SortBy=SortName&SortOrder=Ascending"
    );
    let series_legacy_url = format!(
        "{base}/Users/{uid}/Items?IncludeItemTypes=Series&Recursive=true&Fields=Overview,Genres,UserData&EnableUserData=true&SortBy=SortName&SortOrder=Ascending"
    );
    let raw_series: RawItemsResp = get_json(&agent, &series_v2_url, key)
        .or_else(|_| get_json(&agent, &series_legacy_url, key))?;

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
    let movies_v2_url = format!(
        "{base}/Items?userId={uid}&IncludeItemTypes=Movie&Recursive=true&Fields=Overview,Genres,RunTimeTicks,UserData&EnableUserData=true&SortBy=SortName&SortOrder=Ascending"
    );
    let movies_legacy_url = format!(
        "{base}/Users/{uid}/Items?IncludeItemTypes=Movie&Recursive=true&Fields=Overview,Genres,RunTimeTicks,UserData&EnableUserData=true&SortBy=SortName&SortOrder=Ascending"
    );
    let raw_movies: RawItemsResp = get_json(&agent, &movies_v2_url, key)
        .or_else(|_| get_json(&agent, &movies_legacy_url, key))?;
    let mut by_genre: BTreeMap<String, Vec<MediaItem>> = BTreeMap::new();
    for m in raw_movies.items {
        let item = m.to_media_item(&base, key, &m.id);
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
            number: 0,
            provider_id: "jellyfin".into(),
            source_key: s.id.clone(),
            callsign: make_callsign(&s.name),
            name: s.name,
            artwork_id: s.id.clone(),
            channel_type: "series".into(),
            profile_override: None,
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
    }

    for (genre, movies) in by_genre {
        if movies.is_empty() {
            continue;
        }
        let art = movies[0].artwork_id.clone();
        channels.push(Channel {
            number: 0,
            provider_id: "jellyfin".into(),
            source_key: format!("genre:{genre}"),
            callsign: make_callsign(&genre),
            name: format!("{genre} Movies"),
            artwork_id: art,
            channel_type: "movies".into(),
            profile_override: None,
            kind: ChannelKind::MovieGenre { genre, movies },
        });
    }

    Ok(channels)
}

// ------------------------------------------------------------------ Playback Progress Sync

fn report_session_with_agent(
    agent: &ureq::Agent,
    server_url: &str,
    api_key: &str,
    user_id: &str,
    endpoint: &str,
    item_id: &str,
    pos_secs: f64,
    paused: bool,
) {
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
    let _ = send_jellyfin_req(agent, "POST", &format!("{base}{endpoint}"), Some(api_key), Some(&body));

    // Also persist PlaybackPositionTicks directly on the user's UserData record when saving progress / stopping.
    if !user_id.is_empty() && endpoint != "/Sessions/Playing" {
        let ud_body = serde_json::json!({ "PlaybackPositionTicks": ticks });
        let v2 = send_jellyfin_req(
            agent,
            "POST",
            &format!("{base}/UserItems/{item_id}/UserData?userId={user_id}"),
            Some(api_key),
            Some(&ud_body),
        );
        if v2.map(|r| !(200..300).contains(&r.status())).unwrap_or(true) {
            let _ = send_jellyfin_req(
                agent,
                "POST",
                &format!("{base}/Users/{user_id}/Items/{item_id}/UserData"),
                Some(api_key),
                Some(&ud_body),
            );
        }
    }
}

fn report_session(server_url: &str, api_key: &str, user_id: &str, endpoint: &str, item_id: &str, pos_secs: f64, paused: bool) {
    report_session_with_agent(&make_agent(), server_url, api_key, user_id, endpoint, item_id, pos_secs, paused);
}

fn mark_played_with_agent(
    agent: &ureq::Agent,
    server_url: &str,
    api_key: &str,
    user_id: &str,
    item_id: &str,
) -> Result<(), String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() || user_id.is_empty() || item_id.is_empty() {
        return Err("Jellyfin server not configured.".into());
    }
    let mut errs = Vec::new();
    // 1) Jellyfin 10.9+ UserPlayedItems
    let r1 = verify_played_resp(
        send_jellyfin_req(
            agent,
            "POST",
            &format!("{base}/UserPlayedItems/{item_id}?userId={user_id}"),
            Some(api_key),
            None,
        ),
        true,
    );
    match r1 {
        Ok(()) => return Ok(()),
        Err(e) => errs.push(format!("UserPlayedItems: {e}")),
    }
    // 2) Legacy Users/{uid}/PlayedItems/{id}
    let r2 = verify_played_resp(
        send_jellyfin_req(
            agent,
            "POST",
            &format!("{base}/Users/{user_id}/PlayedItems/{item_id}"),
            Some(api_key),
            None,
        ),
        true,
    );
    match r2 {
        Ok(()) => return Ok(()),
        Err(e) => errs.push(format!("PlayedItems: {e}")),
    }
    // 3) Jellyfin 10.9+ direct UserData update
    let ud = serde_json::json!({ "Played": true, "PlaybackPositionTicks": 0 });
    let r3 = verify_played_resp(
        send_jellyfin_req(
            agent,
            "POST",
            &format!("{base}/UserItems/{item_id}/UserData?userId={user_id}"),
            Some(api_key),
            Some(&ud),
        ),
        true,
    );
    match r3 {
        Ok(()) => return Ok(()),
        Err(e) => errs.push(format!("UserItems/UserData: {e}")),
    }
    // 4) Legacy Users/{uid}/Items/{id}/UserData
    let r4 = verify_played_resp(
        send_jellyfin_req(
            agent,
            "POST",
            &format!("{base}/Users/{user_id}/Items/{item_id}/UserData"),
            Some(api_key),
            Some(&ud),
        ),
        true,
    );
    match r4 {
        Ok(()) => Ok(()),
        Err(e) => {
            errs.push(format!("Users/Items/UserData: {e}"));
            let msg = format!("Mark watched rejected by Jellyfin ({})", errs.join(" | "));
            eprintln!("[ViCTR Jellyfin] {msg}");
            Err(msg)
        }
    }
}

fn mark_played(server_url: &str, api_key: &str, user_id: &str, item_id: &str) -> Result<(), String> {
    mark_played_with_agent(&make_agent(), server_url, api_key, user_id, item_id)
}

fn mark_unplayed(server_url: &str, api_key: &str, user_id: &str, item_id: &str) -> Result<(), String> {
    let base = server_url.trim().trim_end_matches('/');
    if base.is_empty() || api_key.trim().is_empty() || user_id.is_empty() || item_id.is_empty() {
        return Err("Jellyfin server not configured.".into());
    }
    let agent = make_agent();
    let mut errs = Vec::new();
    let r1 = verify_played_resp(
        send_jellyfin_req(
            &agent,
            "DELETE",
            &format!("{base}/UserPlayedItems/{item_id}?userId={user_id}"),
            Some(api_key),
            None,
        ),
        false,
    );
    match r1 {
        Ok(()) => return Ok(()),
        Err(e) => errs.push(format!("UserPlayedItems: {e}")),
    }
    let r2 = verify_played_resp(
        send_jellyfin_req(
            &agent,
            "DELETE",
            &format!("{base}/Users/{user_id}/PlayedItems/{item_id}"),
            Some(api_key),
            None,
        ),
        false,
    );
    match r2 {
        Ok(()) => return Ok(()),
        Err(e) => errs.push(format!("PlayedItems: {e}")),
    }
    let ud = serde_json::json!({ "Played": false });
    let r3 = verify_played_resp(
        send_jellyfin_req(
            &agent,
            "POST",
            &format!("{base}/UserItems/{item_id}/UserData?userId={user_id}"),
            Some(api_key),
            Some(&ud),
        ),
        false,
    );
    match r3 {
        Ok(()) => return Ok(()),
        Err(e) => errs.push(format!("UserItems/UserData: {e}")),
    }
    let r4 = verify_played_resp(
        send_jellyfin_req(
            &agent,
            "POST",
            &format!("{base}/Users/{user_id}/Items/{item_id}/UserData"),
            Some(api_key),
            Some(&ud),
        ),
        false,
    );
    match r4 {
        Ok(()) => Ok(()),
        Err(e) => {
            errs.push(format!("Users/Items/UserData: {e}"));
            let msg = format!("Mark unwatched rejected by Jellyfin ({})", errs.join(" | "));
            eprintln!("[ViCTR Jellyfin] {msg}");
            Err(msg)
        }
    }
}

// ------------------------------------------------------------------ Async Manager

/// Async messages returned from background Jellyfin worker threads.
pub enum JellyfinEvent {
    UsersLoaded(Result<Vec<JellyfinUser>, String>),
    AuthCompleted(Result<(String, JellyfinUser), String>),
    LineupLoaded(Result<Vec<Channel>, String>),
    EpisodesLoaded(String, Result<Vec<MediaItem>, String>),
    ScanProgress(String),
    LibraryScanTriggered(Result<(), String>),
    PlayedSynced(Result<(), String>),
    ArtworkLoaded(String, ArtworkImage),
}

pub struct JellyfinManager {
    pub cfg: JellyfinConfig,
    pub users: Vec<JellyfinUser>,
    pub channels: Vec<Channel>,
    pub connected: bool,
    pub busy: bool,
    pub status: String,
    pub last_error: String,
    pub active_watch: Option<ActiveWatch>,
    pub artwork_cache: HashMap<String, Option<ArtworkImage>>,
    pub last_sync_at: Option<std::time::Instant>,
    background_refresh: bool,
    pending_played_writes: usize,
    episodes_loading: HashMap<String, bool>,
    episodes_failed_at: HashMap<String, std::time::Instant>,
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
            last_error: String::new(),
            active_watch: None,
            artwork_cache: HashMap::new(),
            last_sync_at: None,
            background_refresh: false,
            pending_played_writes: 0,
            episodes_loading: HashMap::new(),
            episodes_failed_at: HashMap::new(),
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
        self.last_error.clear();
        self.status = "Connecting to Jellyfin…".into();
        std::thread::spawn(move || {
            let res = fetch_users(&url, &key);
            ev.lock().unwrap().push(JellyfinEvent::UsersLoaded(res));
        });
    }

    /// Signs in with a Jellyfin username & password to obtain a user-scoped access token
    /// (password is never saved to disk; only the returned AccessToken is persisted).
    pub fn sign_in_async(&mut self, username: String, password: String) {
        let url = self.cfg.normalized_url();
        let ev = self.events.clone();
        self.busy = true;
        self.last_error.clear();
        self.status = format!("Signing in to Jellyfin as {}…", username.trim());
        std::thread::spawn(move || {
            let res = authenticate_by_name(&url, &username, &password);
            ev.lock().unwrap().push(JellyfinEvent::AuthCompleted(res));
        });
    }

    pub fn connect_and_load(&mut self) {
        let _ = save_config(&self.cfg);
        let cfg = self.cfg.clone();
        let ev = self.events.clone();
        self.episodes_loading.clear();
        self.episodes_failed_at.clear();
        self.background_refresh = false;
        self.busy = true;
        self.last_sync_at = Some(std::time::Instant::now());
        self.status = "Loading Jellyfin channels…".into();
        std::thread::spawn(move || {
            let res = fetch_lineup(&cfg);
            ev.lock().unwrap().push(JellyfinEvent::LineupLoaded(res));
        });
    }

    /// Quiet periodic background refresh: keeps existing channels & artwork visible
    /// while fetching updates, and leaves the session connected on transient errors.
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
            ev.lock().unwrap().push(JellyfinEvent::LineupLoaded(res));
        });
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

    pub fn scan_server_library(&mut self) {
        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let ev = self.events.clone();
        self.busy = true;
        self.status = "Requesting Jellyfin server library scan…".into();
        std::thread::spawn(move || {
            let res = trigger_and_wait_library_scan(&url, &key, &ev);
            ev.lock().unwrap().push(JellyfinEvent::LibraryScanTriggered(res));
        });
    }

    pub fn disconnect(&mut self) {
        self.finish_watch();
        self.channels.clear();
        self.artwork_cache.clear();
        self.episodes_loading.clear();
        self.episodes_failed_at.clear();
        self.connected = false;
        self.last_sync_at = None;
        self.background_refresh = false;
        self.status = "Disconnected.".into();
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

    /// Drains background worker results and triggers periodic auto-sync.
    /// Returns true if lineup or artwork changed.
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
                JellyfinEvent::AuthCompleted(res) => {
                    self.busy = false;
                    match res {
                        Ok((token, user)) => {
                            self.cfg.api_key = token;
                            self.cfg.user_id = user.id.clone();
                            self.cfg.user_name = user.name.clone();
                            self.users = vec![user];
                            self.last_error.clear();
                            self.status = format!("Signed in as {} — loading guide…", self.cfg.user_name);
                            self.connect_and_load();
                        }
                        Err(e) => {
                            self.last_error = e.clone();
                            self.status = e;
                        }
                    }
                    changed = true;
                }
                JellyfinEvent::LineupLoaded(res) => {
                    self.busy = false;
                    let was_bg = std::mem::take(&mut self.background_refresh);
                    match res {
                        Ok(chs) => {
                            let n_series = chs.iter().filter(|c| matches!(c.kind, ChannelKind::Series { .. })).count();
                            let n_genres = chs.iter().filter(|c| matches!(c.kind, ChannelKind::MovieGenre { .. })).count();
                            self.channels = chs;
                            self.connected = true;
                            self.episodes_loading.clear();
                            self.last_sync_at = Some(std::time::Instant::now());
                            self.status = format!("Connected · {n_series} series, {n_genres} movie genres");
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
                JellyfinEvent::EpisodesLoaded(sid, res) => {
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
                                        // Refresh Next Up if it was missing or is now marked watched
                                        let valid_nu = next_up_episode_id.as_deref().is_some_and(|id| {
                                            episodes.iter().any(|e| e.id == id && (!e.played || e.resume_secs > 10.0))
                                        });
                                        if !valid_nu {
                                            *next_up_episode_id = episodes
                                                .iter()
                                                .find(|e| e.resume_secs > 10.0 && !e.played)
                                                .or_else(|| episodes.iter().find(|e| !e.played))
                                                .or_else(|| episodes.first())
                                                .map(|e| e.id.clone());
                                        }
                                        changed = true;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            self.episodes_failed_at.insert(sid, std::time::Instant::now());
                            self.status = format!("Episode fetch failed: {e}");
                            changed = true;
                        }
                    }
                }
                JellyfinEvent::ScanProgress(msg) => {
                    self.status = msg;
                    changed = true;
                }
                JellyfinEvent::LibraryScanTriggered(res) => {
                    self.busy = false;
                    match res {
                        Ok(()) => {
                            self.status = "Jellyfin scan complete — reloading guide…".into();
                            self.connect_and_load();
                        }
                        Err(e) => {
                            self.status = e;
                        }
                    }
                    changed = true;
                }
                JellyfinEvent::PlayedSynced(res) => {
                    self.pending_played_writes = self.pending_played_writes.saturating_sub(1);
                    match res {
                        Ok(()) => {
                            self.last_error.clear();
                        }
                        Err(msg) => {
                            self.last_error = msg;
                            changed = true;
                        }
                    }
                }
                JellyfinEvent::ArtworkLoaded(id, img) => {
                    self.artwork_cache.insert(id, Some(img));
                    changed = true;
                }
            }
        }
        changed
    }

    /// Marks an episode or movie as watched/unwatched locally and syncs to Jellyfin in background.
    pub fn set_item_played(&mut self, item_id: &str, played: bool) {
        for ch in &mut self.channels {
            match &mut ch.kind {
                ChannelKind::Series { episodes, next_up_episode_id, .. } => {
                    if let Some(ep) = episodes.iter_mut().find(|e| e.id == item_id) {
                        ep.played = played;
                        if played {
                            ep.resume_secs = 0.0;
                        }
                        // Recompute Next Up cleanly: first episode with resume > 10s, else first unplayed
                        *next_up_episode_id = episodes
                            .iter()
                            .find(|e| e.resume_secs > 10.0 && !e.played)
                            .or_else(|| episodes.iter().find(|e| !e.played))
                            .or_else(|| episodes.first())
                            .map(|e| e.id.clone());
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
        let key = self.cfg.api_key.clone();
        let uid = self.cfg.user_id.clone();
        let id = item_id.to_string();
        let ev = self.events.clone();
        if !url.is_empty() && !key.trim().is_empty() && !uid.is_empty() {
            self.pending_played_writes += 1;
            std::thread::spawn(move || {
                let res = if played {
                    mark_played(&url, &key, &uid, &id)
                } else {
                    mark_unplayed(&url, &key, &uid, &id)
                };
                ev.lock().unwrap().push(JellyfinEvent::PlayedSynced(res));
            });
        }
    }

    pub fn begin_watch(&mut self, target: &TuneTarget) {
        self.finish_watch();
        if target.provider_id != "jellyfin" {
            return;
        }
        self.active_watch = Some(ActiveWatch {
            channel_number: target.channel_number,
            provider_id: target.provider_id.clone(),
            source_key: target.source_key.clone(),
            channel_type: target.channel_type.clone(),
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
    /// Returns `true` if local channel watched state changed (so unified lineup can refresh).
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

        // Wait 20 seconds of actual viewing before reporting to Jellyfin so surfing stays clean.
        if w.dwell_secs < 20.0 {
            return false;
        }

        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let uid = self.cfg.user_id.clone();
        let item_id = w.item_id.clone();

        if !w.reported_start {
            w.reported_start = true;
            w.last_progress_secs = w.dwell_secs;
            std::thread::spawn(move || {
                report_session(&url, &key, &uid, "/Sessions/Playing", &item_id, pos, paused);
            });
            return false;
        }

        // Periodic progress every 10 seconds
        if w.dwell_secs - w.last_progress_secs >= 10.0 {
            w.last_progress_secs = w.dwell_secs;
            let u2 = url.clone();
            let k2 = key.clone();
            let uid2 = uid.clone();
            let id2 = item_id.clone();
            std::thread::spawn(move || {
                report_session(&u2, &k2, &uid2, "/Sessions/Playing/Progress", &id2, pos, paused);
            });
        }

        // Mark watched at 90% completion (matches Jellyfin default threshold)
        if !w.marked_played && w.duration_secs > 30.0 && pos >= w.duration_secs * 0.90 {
            w.marked_played = true;
            let id_mark = item_id.clone();
            // Also update local episode played flag so the guide advances immediately!
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
                let _ = mark_played(&url, &key, &uid, &item_id);
            });
            return true;
        }
        false
    }

    pub fn finish_watch(&mut self) {
        let _ = self.finish_watch_inner(false);
    }

    /// Starts flushing watch progress on a background thread with a short timeout and returns
    /// its join handle so graceful shutdown can overlap the HTTP request with the CRT collapse.
    pub fn finish_watch_async_joinable(&mut self) -> Option<std::thread::JoinHandle<()>> {
        self.finish_watch_inner(false)
    }

    /// Synchronously flushes the current watch position to Jellyfin (with a short timeout)
    /// so quitting ViCTR never drops the final progress/stopped report.
    pub fn finish_watch_sync(&mut self) {
        let _ = self.finish_watch_inner(true);
    }

    fn finish_watch_inner(&mut self, blocking: bool) -> Option<std::thread::JoinHandle<()>> {
        let Some(w) = self.active_watch.take() else { return None };
        if !w.reported_start {
            return None;
        }
        let url = self.cfg.normalized_url();
        let key = self.cfg.api_key.clone();
        let uid = self.cfg.user_id.clone();
        let item_id = w.item_id;
        let pos = w.last_pos;
        let finished_90 = !w.marked_played && w.duration_secs > 30.0 && (pos / w.duration_secs) >= 0.90;

        // Update local resume/played state immediately
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

        let flush = move |fast: bool| {
            let agent = if fast { make_fast_agent() } else { make_agent() };
            if finished_90 {
                let _ = mark_played_with_agent(&agent, &url, &key, &uid, &item_id);
            }
            if !w.marked_played || finished_90 {
                report_session_with_agent(&agent, &url, &key, &uid, "/Sessions/Playing/Stopped", &item_id, pos, false);
            }
        };

        if blocking {
            flush(true);
            None
        } else {
            Some(std::thread::spawn(move || flush(true)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{pick_next_episode_index, schedule_live_movie};

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
                stream_url: "http://x/m1".into(),
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
                stream_url: "http://x/m2".into(),
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
                stream_url: "http://x/e1".into(),
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
                stream_url: "http://x/e2".into(),
            },
        ];
        mgr.channels.push(Channel {
            number: 2,
            provider_id: "jellyfin".into(),
            source_key: "s1".into(),
            callsign: "SHOW".into(),
            name: "Show".into(),
            artwork_id: "s1".into(),
            channel_type: "series".into(),
            profile_override: None,
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
        mgr.set_item_played("e1", true);
        let ch = &mgr.channels[0];
        let next = ch.resolve_now(0).unwrap();
        assert_eq!(next.item.id, "e2");
        let prev = ch.prev_before("e2").unwrap();
        assert_eq!(prev.item.id, "e1");
        mgr.set_item_played("e1", false);
        let ch = &mgr.channels[0];
        let back = ch.resolve_now(0).unwrap();
        assert_eq!(back.item.id, "e1");
    }

    #[test]
    fn merge_episodes_handles_negative_indices_virtual_and_dedup() {
        let json1 = r#"{"Items":[
            {"Id":"e2","Name":"Bart the Genius","ParentIndexNumber":1,"IndexNumber":2},
            {"Id":"v1","Name":"Missing Ep","ParentIndexNumber":1,"IndexNumber":3,"LocationType":"Virtual"},
            {"Id":"neg","Name":"Unmatched Ep","ParentIndexNumber":-1,"IndexNumber":-1}
        ]}"#;
        let json2 = r#"{"Items":[
            {"Id":"e1","Name":"Simpsons Roasting","ParentIndexNumber":1,"IndexNumber":1,"UserData":{"Played":true}},
            {"Id":"e2","Name":"Bart the Genius","ParentIndexNumber":1,"IndexNumber":2,"UserData":{"Played":true}},
            {"Id":"fld","Name":"Season 1","IsFolder":true}
        ]}"#;
        let r1: RawItemsResp = serde_json::from_str(json1).unwrap();
        let r2: RawItemsResp = serde_json::from_str(json2).unwrap();
        let merged = merge_and_sort_episodes("http://x", "k", "simpsons", r1.items, r2.items);
        let ids: Vec<&str> = merged.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["e1", "e2", "neg"]);
        assert!(merged[0].played && merged[1].played, "UserData.Played should survive merging");
        assert!(!merged[2].played);
        assert_eq!(merged[2].season, None);
        assert_eq!(merged[2].episode, None);
        // Stale next_up_id pointing to an already-watched episode must be skipped in favor of first unwatched
        assert_eq!(pick_next_episode_index(&merged, Some("e1")), Some(2));
    }

    #[test]
    fn http_308_redirect_preserves_post_body_and_auth() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        let srv = std::thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut tmp = [0u8; 1024];
                let mut header_end = None;
                let mut content_len = 0usize;
                loop {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&tmp[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(pos + 4);
                            let hdr = String::from_utf8_lossy(&raw[..pos]);
                            for line in hdr.lines() {
                                if let Some(v) = line.strip_prefix("Content-Length:").or_else(|| line.strip_prefix("content-length:")) {
                                    content_len = v.trim().parse().unwrap_or(0);
                                }
                            }
                        }
                    }
                    if let Some(he) = header_end {
                        if raw.len() >= he + content_len {
                            break;
                        }
                    }
                }
                let req = String::from_utf8_lossy(&raw);
                let first_line = req.lines().next().unwrap_or("");
                if first_line.starts_with("POST /Users/AuthenticateByName ") {
                    let resp = format!(
                        "HTTP/1.1 308 Permanent Redirect\r\nLocation: http://{addr}/final/Users/AuthenticateByName\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(resp.as_bytes());
                } else if first_line.starts_with("POST /final/Users/AuthenticateByName ") {
                    assert!(req.contains("\"Username\":\"homer\""), "POST JSON body must survive 308 redirect: {req}");
                    let body = r#"{"AccessToken":"tok123","User":{"Id":"u1","Name":"Homer"}}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                } else if first_line.starts_with("POST /UserPlayedItems/ep1?userId=u1 ") {
                    let resp = format!(
                        "HTTP/1.1 308 Permanent Redirect\r\nLocation: /final/UserPlayedItems/ep1?userId=u1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(resp.as_bytes());
                } else if first_line.starts_with("POST /final/UserPlayedItems/ep1?userId=u1 ") {
                    assert!(req.contains("tok123"), "Auth header must survive 308 redirect: {req}");
                    let body = r#"{"Played":true,"PlaybackPositionTicks":0}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(resp.as_bytes());
                }
            }
        });

        let (tok, user) = authenticate_by_name(&base, "homer", "donut").expect("POST login must survive 308 redirect");
        assert_eq!(tok, "tok123");
        assert_eq!(user.id, "u1");

        mark_played(&base, &tok, &user.id, "ep1").expect("POST mark_played must survive 308 redirect");
        srv.join().unwrap();
    }
}
