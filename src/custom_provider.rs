//! Custom Integrators provider for ViCTR's unified 90s Cable Guide.
//!
//! Lets anyone plug in arbitrary media or live-stream sources without recompiling ViCTR,
//! using one clean JSON schema across two zero-friction mechanisms:
//! 1. **Drop-in executable scripts / binaries** placed in `<config_dir>/victr/providers/`
//!    (e.g. Python, Bash, Node, Go, Rust) that print a ViCTR JSON lineup to stdout.
//! 2. **HTTP / HTTPS JSON feed endpoints** configured in `custom_providers.json`
//!    (with optional Bearer/API authorization header).
//!
//! ### JSON Schema (v1)
//! ```json
//! {
//!   "provider_name": "My Custom Channels",
//!   "channels": [
//!     {
//!       "id": "retro-live",
//!       "name": "Retro Live TV",
//!       "callsign": "RETRO",
//!       "kind": "live",
//!       "stream_url": "https://example.com/live.m3u8",
//!       "group": "Live",
//!       "now_title": "Saturday Morning Block",
//!       "overview": "24/7 stream",
//!       "artwork_url": "https://example.com/logo.png"
//!     },
//!     {
//!       "id": "show-1",
//!       "name": "Public Domain Show",
//!       "kind": "series",
//!       "items": [
//!         { "id": "ep1", "title": "Pilot", "season": 1, "episode": 1, "stream_url": "https://...", "runtime_secs": 1500 }
//!       ]
//!     }
//!   ]
//! }
//! ```

use crate::provider::{ArtworkImage, Channel, ChannelKind, MediaItem, make_callsign, pick_next_episode_index};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ Config

fn default_true() -> bool {
    true
}

fn default_refresh_mins() -> u32 {
    15
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomEndpoint {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auth_header: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CustomWatchState {
    #[serde(default)]
    pub played: bool,
    #[serde(default)]
    pub resume_secs: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CustomProvidersConfig {
    #[serde(default)]
    pub endpoints: Vec<CustomEndpoint>,
    /// File names inside `<config_dir>/victr/providers/` that the user disabled.
    #[serde(default)]
    pub disabled_scripts: HashSet<String>,
    #[serde(default)]
    pub watch_state: HashMap<String, CustomWatchState>,
    #[serde(default = "default_refresh_mins")]
    pub auto_refresh_mins: u32,
}

impl Default for CustomProvidersConfig {
    fn default() -> Self {
        Self {
            endpoints: Vec::new(),
            disabled_scripts: HashSet::new(),
            watch_state: HashMap::new(),
            auto_refresh_mins: default_refresh_mins(),
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("custom_providers.json")))
}

pub fn providers_dir() -> Option<PathBuf> {
    crate::settings::presets_dir().and_then(|d| d.parent().map(|p| p.join("providers")))
}

pub fn load_config_from(path: &Path) -> CustomProvidersConfig {
    let Ok(text) = std::fs::read_to_string(path) else { return CustomProvidersConfig::default() };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_config_to(path: &Path, cfg: &CustomProvidersConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Couldn't create config folder: {e}"))?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("Couldn't save custom providers config: {e}"))
}

pub fn load_config() -> CustomProvidersConfig {
    config_path().map(|p| load_config_from(&p)).unwrap_or_default()
}

pub fn save_config(cfg: &CustomProvidersConfig) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    save_config_to(&path, cfg)
}

// ------------------------------------------------------------------ JSON Schema

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum CustomPayload {
    Envelope {
        #[serde(default)]
        provider_name: String,
        #[serde(default)]
        channels: Vec<CustomChannelDto>,
    },
    BareList(Vec<CustomChannelDto>),
}

#[derive(Clone, Debug, Deserialize)]
struct CustomChannelDto {
    #[serde(default)]
    number: Option<u16>,
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default)]
    callsign: String,
    /// `"live"`, `"web"`, `"series"`, or `"movies"` (inferred automatically if omitted).
    #[serde(default)]
    kind: String,
    /// Semantic channel type mapped to a TV profile (e.g. `"broadcast"`, `"vhs"`).
    #[serde(rename = "type", default)]
    channel_type: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    stream_url: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    genre: String,
    #[serde(default)]
    now_title: String,
    #[serde(default)]
    overview: String,
    #[serde(default)]
    artwork_url: String,
    #[serde(default)]
    is_favorite: bool,
    #[serde(default)]
    items: Vec<CustomMediaItemDto>,
}

#[derive(Clone, Debug, Deserialize)]
struct CustomMediaItemDto {
    #[serde(default)]
    id: String,
    title: String,
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<u32>,
    #[serde(default)]
    overview: String,
    #[serde(default)]
    runtime_secs: Option<f64>,
    #[serde(default)]
    played: Option<bool>,
    #[serde(default)]
    resume_secs: Option<f64>,
    #[serde(default)]
    artwork_url: String,
    stream_url: String,
}

pub fn parse_custom_provider_json(
    source_tag: &str,
    json_text: &str,
    watch_state: &HashMap<String, CustomWatchState>,
) -> Result<(String, Vec<Channel>), String> {
    let parsed: CustomPayload =
        serde_json::from_str(json_text.trim()).map_err(|e| format!("{source_tag}: invalid JSON ({e})"))?;
    let (provider_name, dtos) = match parsed {
        CustomPayload::Envelope { provider_name, channels } => (provider_name, channels),
        CustomPayload::BareList(channels) => (String::new(), channels),
    };
    let prov_label = if provider_name.trim().is_empty() {
        source_tag.to_string()
    } else {
        provider_name.trim().to_string()
    };

    let mut out = Vec::new();
    for (idx, dto) in dtos.into_iter().enumerate() {
        let name = dto.name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let ch_id = if dto.id.trim().is_empty() {
            format!("{source_tag}:{idx}:{name}")
        } else {
            format!("{source_tag}:{}", dto.id.trim())
        };
        let callsign = if !dto.callsign.trim().is_empty() {
            dto.callsign.trim().chars().take(5).collect::<String>().to_ascii_uppercase()
        } else {
            make_callsign(&name)
        };
        let kind_norm = dto.kind.trim().to_ascii_lowercase();
        let inferred = if !kind_norm.is_empty() {
            kind_norm.as_str()
        } else if !dto.stream_url.trim().is_empty() && dto.items.is_empty() {
            "live"
        } else if dto.items.iter().any(|i| i.season.is_some() || i.episode.is_some()) {
            "series"
        } else {
            "movies"
        };

        let ch_kind = match inferred {
            "live" | "stream" | "livestream" | "web" => {
                if dto.stream_url.trim().is_empty() {
                    continue;
                }
                let stream_url = if inferred == "web" {
                    crate::web_channel::ensure_web_prefix(dto.stream_url.trim())
                } else {
                    dto.stream_url.trim().to_string()
                };
                let group = if !dto.group.trim().is_empty() {
                    dto.group.trim().to_string()
                } else {
                    prov_label.clone()
                };
                let now_title = if !dto.now_title.trim().is_empty() {
                    dto.now_title.trim().to_string()
                } else {
                    name.clone()
                };
                ChannelKind::LiveStream {
                    stream_url,
                    group,
                    now_title,
                    overview: dto.overview.trim().to_string(),
                }
            }
            "series" | "show" | "tv" => {
                let episodes: Vec<MediaItem> = dto
                    .items
                    .into_iter()
                    .enumerate()
                    .filter(|(_, m)| !m.stream_url.trim().is_empty())
                    .map(|(mi, m)| {
                        let item_id = if m.id.trim().is_empty() {
                            format!("{ch_id}:ep{mi}")
                        } else {
                            format!("{source_tag}:{}", m.id.trim())
                        };
                        let local_ws = watch_state.get(&item_id);
                        let played = local_ws.map(|w| w.played).or(m.played).unwrap_or(false);
                        let resume_secs = local_ws
                            .map(|w| w.resume_secs)
                            .or(m.resume_secs)
                            .unwrap_or(0.0);
                        MediaItem {
                            id: item_id,
                            title: m.title,
                            season: m.season,
                            episode: m.episode,
                            overview: m.overview,
                            runtime_secs: m.runtime_secs.filter(|&r| r > 10.0).unwrap_or(1800.0),
                            played,
                            resume_secs,
                            artwork_id: if m.artwork_url.is_empty() {
                                dto.artwork_url.clone()
                            } else {
                                m.artwork_url
                            },
                            stream_url: m.stream_url.trim().to_string(),
                        }
                    })
                    .collect();
                if episodes.is_empty() {
                    continue;
                }
                let next_up = pick_next_episode_index(&episodes, None).map(|i| episodes[i].id.clone());
                let is_continue = episodes.iter().any(|e| e.resume_secs > 15.0 || e.played)
                    && episodes.iter().any(|e| !e.played);
                ChannelKind::Series {
                    series_id: ch_id.clone(),
                    overview: dto.overview.trim().to_string(),
                    is_continue,
                    is_favorite: dto.is_favorite,
                    episodes,
                    next_up_episode_id: next_up,
                    loaded_episodes: true,
                }
            }
            _ => {
                let movies: Vec<MediaItem> = dto
                    .items
                    .into_iter()
                    .enumerate()
                    .filter(|(_, m)| !m.stream_url.trim().is_empty())
                    .map(|(mi, m)| {
                        let item_id = if m.id.trim().is_empty() {
                            format!("{ch_id}:m{mi}")
                        } else {
                            format!("{source_tag}:{}", m.id.trim())
                        };
                        let local_ws = watch_state.get(&item_id);
                        MediaItem {
                            id: item_id,
                            title: m.title,
                            season: None,
                            episode: None,
                            overview: m.overview,
                            runtime_secs: m.runtime_secs.filter(|&r| r > 10.0).unwrap_or(5400.0),
                            played: local_ws.map(|w| w.played).or(m.played).unwrap_or(false),
                            resume_secs: local_ws.map(|w| w.resume_secs).or(m.resume_secs).unwrap_or(0.0),
                            artwork_id: if m.artwork_url.is_empty() {
                                dto.artwork_url.clone()
                            } else {
                                m.artwork_url
                            },
                            stream_url: m.stream_url.trim().to_string(),
                        }
                    })
                    .collect();
                if movies.is_empty() {
                    continue;
                }
                let genre = if !dto.genre.trim().is_empty() {
                    dto.genre.trim().to_string()
                } else {
                    name.clone()
                };
                ChannelKind::MovieGenre { genre, movies }
            }
        };

        let ch_type = if dto.channel_type.trim().is_empty() {
            crate::provider::default_channel_type(&ch_kind).to_string()
        } else {
            dto.channel_type.trim().to_string()
        };
        out.push(Channel {
            number: dto.number.filter(|&n| n >= 2).unwrap_or(0),
            provider_id: "custom".into(),
            source_key: ch_id,
            callsign,
            name,
            artwork_id: dto.artwork_url.trim().to_string(),
            channel_type: ch_type,
            profile_override: dto.profile.filter(|s| !s.trim().is_empty()),
            kind: ch_kind,
        });
    }

    Ok((prov_label, out))
}

// ------------------------------------------------------------------ Plugin Discovery & Execution

#[derive(Clone, Debug)]
pub struct DiscoveredScript {
    pub file_name: String,
    pub path: PathBuf,
}

fn is_candidate_plugin_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return false };
    let low = name.to_ascii_lowercase();
    if name.starts_with('.')
        || low.starts_with("readme")
        || low.ends_with(".disabled")
        || low.ends_with(".txt")
        || low.ends_with(".md")
        || low.ends_with(".json")
        || low.ends_with(".example")
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.permissions().mode() & 0o111 != 0 {
                return true;
            }
        }
    }
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("sh" | "py" | "js" | "rb" | "exe" | "bat" | "cmd" | "ps1")
    )
}

pub fn discover_scripts_in(dir: &Path) -> Vec<DiscoveredScript> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if is_candidate_plugin_file(&path) {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                out.push(DiscoveredScript {
                    file_name: name.to_string(),
                    path,
                });
            }
        }
    }
    out.sort_by(|a, b| a.file_name.to_lowercase().cmp(&b.file_name.to_lowercase()));
    out
}

fn build_plugin_command(path: &Path) -> Command {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    #[cfg(unix)]
    let is_exec = std::fs::metadata(path)
        .map(|m| {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode() & 0o111 != 0
        })
        .unwrap_or(false);
    #[cfg(not(unix))]
    let is_exec = matches!(ext.as_deref(), Some("exe" | "bat" | "cmd"));

    let mut cmd = if is_exec {
        Command::new(path)
    } else {
        match ext.as_deref() {
            Some("py") => {
                let mut c = Command::new("python3");
                c.arg(path);
                c
            }
            Some("sh") => {
                let mut c = Command::new("sh");
                c.arg(path);
                c
            }
            Some("js") => {
                let mut c = Command::new("node");
                c.arg(path);
                c
            }
            Some("ps1") => {
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]).arg(path);
                c
            }
            _ => Command::new(path),
        }
    };
    cmd.env("VICTR_PROVIDER_API", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = providers_dir().and_then(|d| d.parent().map(|p| p.to_path_buf())) {
        cmd.env("VICTR_CONFIG_DIR", dir);
    }
    cmd
}

/// Runs a drop-in provider executable/script with a hard 15-second timeout and returns stdout JSON.
pub fn run_script_plugin(path: &Path, timeout: Duration) -> Result<String, String> {
    let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("plugin");
    let mut child = build_plugin_command(path)
        .spawn()
        .map_err(|e| format!("{label}: failed to launch ({e})"))?;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(ref mut r) = stdout_pipe {
            let _ = r.take(4 * 1024 * 1024).read_to_string(&mut s);
        }
        s
    });
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(ref mut r) = stderr_pipe {
            let _ = r.take(64 * 1024).read_to_string(&mut s);
        }
        s
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("{label}: timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(e) => return Err(format!("{label}: wait error ({e})")),
        }
    };

    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if !status.success() {
        let snip: String = stderr.trim().chars().take(120).collect();
        return Err(if snip.is_empty() {
            format!("{label}: exited with {status}")
        } else {
            format!("{label}: {snip}")
        });
    }
    Ok(stdout)
}

fn fetch_endpoint_json(ep: &CustomEndpoint) -> Result<String, String> {
    let url = ep.url.trim();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(6))
        .timeout(Duration::from_secs(15))
        .build();
    let mut req = agent
        .get(url)
        .set("Accept", "application/json")
        .set("User-Agent", "ViCTR/0.1.0");
    if !ep.auth_header.trim().is_empty() {
        req = req.set("Authorization", ep.auth_header.trim());
    }
    let resp = req
        .call()
        .map_err(|e| format!("{}: HTTP fetch failed ({e})", ep.name))?;
    let mut out = String::new();
    resp.into_reader()
        .take(4 * 1024 * 1024)
        .read_to_string(&mut out)
        .map_err(|e| format!("{}: read error ({e})", ep.name))?;
    Ok(out)
}

/// Writes a documented example plugin & schema README into `<config_dir>/victr/providers/`.
pub fn create_example_plugin() -> Result<PathBuf, String> {
    let dir = providers_dir().ok_or_else(|| "Couldn't locate config folder.".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't create providers folder: {e}"))?;

    let readme = r#"# ViCTR Custom Provider Plugins

Drop any executable script or binary in this folder (`.sh`, `.py`, `.js`, or compiled binary).
ViCTR executes each enabled plugin on startup and refresh, reading JSON from stdout.

## JSON Schema (v1)

```json
{
  "provider_name": "My Custom Channels",
  "channels": [
    {
      "id": "demo-live",
      "name": "Retro Live Stream",
      "callsign": "RETRO",
      "kind": "live",
      "stream_url": "https://example.com/hls/live.m3u8",
      "group": "Live",
      "now_title": "Now Playing Title",
      "overview": "Optional description shown in the 90s Prevue Guide.",
      "artwork_url": "https://example.com/logo.png"
    },
    {
      "id": "demo-series",
      "name": "My Web Series",
      "kind": "series",
      "items": [
        {
          "id": "ep1",
          "title": "Episode One",
          "season": 1,
          "episode": 1,
          "stream_url": "https://example.com/ep1.mp4",
          "runtime_secs": 1500
        }
      ]
    }
  ]
}
```

To try the included template, rename `example-channel.sh.disabled` to `example-channel.sh`
and edit the `stream_url` to any live `.m3u8` or video stream.
"#;
    let _ = std::fs::write(dir.join("README.md"), readme);

    let example_path = dir.join("example-channel.sh.disabled");
    let script = r#"#!/bin/sh
# Rename this file to remove `.disabled` and point `stream_url` at any live .m3u8 or media URL!
cat <<'EOF'
{
  "provider_name": "Example Script",
  "channels": [
    {
      "id": "example-live",
      "name": "Example Live TV",
      "callsign": "EXMPL",
      "kind": "live",
      "stream_url": "https://example.com/stream/master.m3u8",
      "group": "Custom",
      "now_title": "Live Broadcast",
      "overview": "Generated dynamically by example-channel.sh"
    }
  ]
}
EOF
"#;
    std::fs::write(&example_path, script).map_err(|e| format!("Couldn't write example plugin: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&example_path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(dir)
}

pub fn load_all_custom_channels(
    cfg: &CustomProvidersConfig,
    plugins_dir: Option<&Path>,
) -> (Vec<DiscoveredScript>, Vec<Channel>, Vec<String>) {
    let scripts = plugins_dir.map(discover_scripts_in).unwrap_or_default();
    let mut channels = Vec::new();
    let mut errors = Vec::new();

    // 1. Drop-in scripts / binaries
    for sc in &scripts {
        if cfg.disabled_scripts.contains(&sc.file_name) {
            continue;
        }
        match run_script_plugin(&sc.path, Duration::from_secs(15)) {
            Ok(json) => match parse_custom_provider_json(&sc.file_name, &json, &cfg.watch_state) {
                Ok((_, chs)) => channels.extend(chs),
                Err(e) => errors.push(e),
            },
            Err(e) => errors.push(e),
        }
    }

    // 2. HTTP JSON feed endpoints
    for ep in &cfg.endpoints {
        if !ep.enabled || ep.url.trim().is_empty() {
            continue;
        }
        let tag = if ep.name.trim().is_empty() {
            ep.url.trim()
        } else {
            ep.name.trim()
        };
        match fetch_endpoint_json(ep) {
            Ok(json) => match parse_custom_provider_json(tag, &json, &cfg.watch_state) {
                Ok((_, chs)) => channels.extend(chs),
                Err(e) => errors.push(e),
            },
            Err(e) => errors.push(e),
        }
    }

    (scripts, channels, errors)
}

// ------------------------------------------------------------------ Async Manager

enum CustomEvent {
    Loaded(Vec<DiscoveredScript>, Vec<Channel>, Vec<String>),
    ArtworkLoaded(String, ArtworkImage),
}

pub struct CustomProviderManager {
    pub cfg: CustomProvidersConfig,
    pub scripts: Vec<DiscoveredScript>,
    pub channels: Vec<Channel>,
    pub busy: bool,
    pub status: String,
    pub last_error: String,
    pub last_sync_at: Option<Instant>,
    artwork_cache: HashMap<String, Option<ArtworkImage>>,
    events: Arc<Mutex<Vec<CustomEvent>>>,
}

impl CustomProviderManager {
    pub fn new_offline() -> Self {
        Self {
            cfg: CustomProvidersConfig::default(),
            scripts: Vec::new(),
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
        let scripts = providers_dir().map(|d| discover_scripts_in(&d)).unwrap_or_default();
        let has_enabled_scripts = scripts.iter().any(|s| !cfg.disabled_scripts.contains(&s.file_name));
        let has_enabled_eps = cfg.endpoints.iter().any(|e| e.enabled && !e.url.trim().is_empty());
        let mut mgr = Self {
            cfg,
            scripts,
            ..Self::new_offline()
        };
        if has_enabled_scripts || has_enabled_eps {
            mgr.refresh_async();
        }
        mgr
    }

    pub fn has_enabled_sources(&self) -> bool {
        self.scripts.iter().any(|s| !self.cfg.disabled_scripts.contains(&s.file_name))
            || self.cfg.endpoints.iter().any(|e| e.enabled && !e.url.trim().is_empty())
    }

    pub fn add_endpoint(&mut self, name: String, url: String, auth_header: String) {
        let u = url.trim().to_string();
        if u.is_empty() {
            return;
        }
        let clean_name = if name.trim().is_empty() {
            u.clone()
        } else {
            name.trim().to_string()
        };
        self.cfg.endpoints.push(CustomEndpoint {
            name: clean_name,
            url: u,
            auth_header: auth_header.trim().to_string(),
            enabled: true,
        });
        let _ = save_config(&self.cfg);
        self.refresh_async();
    }

    pub fn remove_endpoint(&mut self, idx: usize) {
        if idx < self.cfg.endpoints.len() {
            self.cfg.endpoints.remove(idx);
            let _ = save_config(&self.cfg);
            self.refresh_async();
        }
    }

    pub fn set_script_enabled(&mut self, file_name: &str, enabled: bool) {
        if enabled {
            self.cfg.disabled_scripts.remove(file_name);
        } else {
            self.cfg.disabled_scripts.insert(file_name.to_string());
        }
        let _ = save_config(&self.cfg);
        self.refresh_async();
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
        if duration_secs > 30.0 && pos_secs >= duration_secs * 0.90 {
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

    pub fn refresh_async(&mut self) {
        let cfg = self.cfg.clone();
        let dir = providers_dir();
        let ev = self.events.clone();
        self.busy = true;
        self.last_error.clear();
        self.last_sync_at = Some(Instant::now());
        self.status = "Running custom providers…".into();
        std::thread::spawn(move || {
            let (scripts, chs, errs) = load_all_custom_channels(&cfg, dir.as_deref());
            ev.lock().unwrap().push(CustomEvent::Loaded(scripts, chs, errs));
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
            if let Some(img) = crate::m3u::fetch_artwork(&url) {
                ev.lock().unwrap().push(CustomEvent::ArtworkLoaded(url, img));
            }
        });
        None
    }

    pub fn poll(&mut self) -> bool {
        if !self.busy
            && self.has_enabled_sources()
            && self.cfg.auto_refresh_mins > 0
            && self
                .last_sync_at
                .is_some_and(|t| t.elapsed().as_secs() >= self.cfg.auto_refresh_mins as u64 * 60)
        {
            self.refresh_async();
        }
        let pending: Vec<CustomEvent> = std::mem::take(&mut *self.events.lock().unwrap());
        if pending.is_empty() {
            return false;
        }
        let mut changed = false;
        for ev in pending {
            match ev {
                CustomEvent::Loaded(scripts, chs, errs) => {
                    self.busy = false;
                    self.last_sync_at = Some(Instant::now());
                    self.scripts = scripts;
                    let n = chs.len();
                    self.channels = chs;
                    self.last_error = errs.join(" | ");
                    self.status = if n > 0 {
                        format!("{n} custom channel{}", if n == 1 { "" } else { "s" })
                    } else if self.has_enabled_sources() {
                        "No channels returned by custom providers.".into()
                    } else {
                        String::new()
                    };
                    changed = true;
                }
                CustomEvent::ArtworkLoaded(u, img) => {
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
    fn parses_custom_provider_json_envelope_and_bare_array() {
        let json = r#"{
            "provider_name": "Retro Feed",
            "channels": [
                {
                    "id": "mtv",
                    "name": "80s Music Videos",
                    "callsign": "MTV80",
                    "kind": "live",
                    "stream_url": "https://example.com/80s.m3u8",
                    "now_title": "Take On Me"
                },
                {
                    "id": "toons",
                    "name": "Classic Toons",
                    "items": [
                        { "id": "e1", "title": "Pilot", "season": 1, "episode": 1, "stream_url": "https://example.com/e1.mp4", "runtime_secs": 900 }
                    ]
                },
                {
                    "id": "scifi",
                    "name": "Public Domain Sci-Fi",
                    "genre": "Sci-Fi",
                    "items": [
                        { "id": "m1", "title": "Plan 9", "stream_url": "https://example.com/p9.mp4", "runtime_secs": 4740 }
                    ]
                }
            ]
        }"#;
        let (pname, chs) = parse_custom_provider_json("retro", json, &HashMap::new()).unwrap();
        assert_eq!(pname, "Retro Feed");
        assert_eq!(chs.len(), 3);
        assert!(matches!(chs[0].kind, ChannelKind::LiveStream { .. }));
        assert!(matches!(chs[1].kind, ChannelKind::Series { .. }));
        assert!(matches!(chs[2].kind, ChannelKind::MovieGenre { .. }));

        // Also supports bare JSON array shorthand
        let bare = r#"[{"name": "Bare Live", "stream_url": "https://example.com/live.m3u8"}]"#;
        let (_, chs2) = parse_custom_provider_json("bare", bare, &HashMap::new()).unwrap();
        assert_eq!(chs2.len(), 1);
        assert!(matches!(chs2[0].kind, ChannelKind::LiveStream { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn runs_drop_in_executable_script_provider() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("victr-custom-script-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let script = dir.join("demo.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '{\"provider_name\":\"Shell TV\",\"channels\":[{\"id\":\"c1\",\"name\":\"Shell Stream\",\"stream_url\":\"https://example.com/s.m3u8\",\"now_title\":\"Live from sh\"}]}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Ignored disabled file
        std::fs::write(dir.join("ignored.sh.disabled"), "#!/bin/sh\necho bad\n").unwrap();

        let cfg = CustomProvidersConfig::default();
        let (scripts, chs, errs) = load_all_custom_channels(&cfg, Some(&dir));
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].file_name, "demo.sh");
        assert_eq!(chs.len(), 1);
        assert_eq!(chs[0].name, "Shell Stream");
        match &chs[0].kind {
            ChannelKind::LiveStream { now_title, .. } => assert_eq!(now_title, "Live from sh"),
            other => panic!("expected LiveStream, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fetches_http_json_custom_provider_endpoint() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/channels.json");

        let srv = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains("Authorization: Bearer abc123"), "missing auth header: {req}");
            let body = r#"{"provider_name":"HTTP Feed","channels":[{"id":"h1","name":"Web TV","stream_url":"http://example/tv.m3u8","group":"Web"}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });

        let cfg = CustomProvidersConfig {
            endpoints: vec![CustomEndpoint {
                name: "My Feed".into(),
                url,
                auth_header: "Bearer abc123".into(),
                enabled: true,
            }],
            ..Default::default()
        };
        let (_, chs, errs) = load_all_custom_channels(&cfg, None);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        assert_eq!(chs.len(), 1);
        assert_eq!(chs[0].name, "Web TV");
        srv.join().unwrap();
    }
}
