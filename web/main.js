// ViCTR Web (onvic.tr) — Thin ES-module shell over victr-web WASM + WebGPU.
// Supports direct HLS/MP4/image streams, Owncast & PeerTube auto-resolve,
// shareable ?m3u= & ?feed=channels.json lineups with 90s Prevue Cable Guide (G),
// localStorage watched/favorite/last-episode memory, and ?embed=1 mode.
import init, { VictrWeb } from "./pkg/victr_web.js";

const WATCH_STORAGE_KEY = "victr_web_watch_state_v1";

const DEFAULT_LINEUP = [
  {
    number: 80,
    id: "nhk-world-live",
    name: "NHK World-Japan",
    callsign: "NHK",
    kind: "live",
    group: "News & Culture",
    now_title: "24/7 Live Broadcast",
    overview: "Free international public broadcast from Japan — news, documentaries, and culture.",
    stream_url: "https://nhkwlive-ojp.akamaized.net/hls/live/2003459/nhkwlive-ojp-en/index.m3u8",
  },
  {
    number: 81,
    id: "dw-news-live",
    name: "DW News",
    callsign: "DW",
    kind: "live",
    group: "News & Culture",
    now_title: "24/7 International News",
    overview: "Free 24/7 English international news and documentary broadcast from Deutsche Welle.",
    stream_url: "https://dwamdstream102.akamaized.net/hls/live/2015525/dwstream102/index.m3u8",
  },
];

const $ = (id) => document.getElementById(id);

const canvas = $("crt");
const video = $("video");
const drawerTab = $("drawer-tab");
const remoteEl = $("remote");
const soundPill = $("sound-pill");
const toastEl = $("toast");

let victr = null;
let hls = null;
let mediaStream = null;
let audioCtx = null;
let audioSource = null;
let audioProc = null;
let audioUnlocked = false;
let toastTimer = null;
let proxyBase = "";
let activeFeedUrl = "";
let activeM3uUrl = "";
let lineupDefs = [...DEFAULT_LINEUP];

let currentSource = {
  type: "pattern",
  key: "demo_scene",
  url: "",
  title: "Demo Scene",
  ch: 0,
  callsign: "LIVE",
  sourceKey: "",
  itemId: "",
  itemPlayed: false,
  itemFavorite: false,
  hasItems: false,
};

function showToast(msg) {
  toastEl.textContent = msg;
  toastEl.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.add("hidden"), 2400);
}

function showWebGpuError(msg) {
  $("webgpu-error-msg").textContent = msg;
  $("webgpu-error").classList.remove("hidden");
}

function resizeCanvas() {
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const w = Math.max(1, Math.floor(window.innerWidth * dpr));
  const h = Math.max(1, Math.floor(window.innerHeight * dpr));
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  if (victr) victr.resize(w, h);
}

function proxiedUrl(rawUrl) {
  if (!proxyBase) return rawUrl;
  return proxyBase + encodeURIComponent(rawUrl);
}

function persistWatchState() {
  if (!victr) return;
  try {
    localStorage.setItem(WATCH_STORAGE_KEY, victr.export_watch_state_json());
  } catch {
    // Private browsing or storage quota disabled
  }
}

function loadPersistedWatchState() {
  if (!victr) return;
  try {
    const raw = localStorage.getItem(WATCH_STORAGE_KEY);
    if (raw) victr.import_watch_state_json(raw);
  } catch {}
}

// ---- WebAudio Analog TV DSP Bridge
function ensureAudioGraph() {
  if (!victr) return;
  if (!audioCtx) {
    const AudioContextCtor = window.AudioContext || window.webkitAudioContext;
    if (!AudioContextCtor) return;
    audioCtx = new AudioContextCtor({ sampleRate: 48000 });
    audioProc = audioCtx.createScriptProcessor(1024, 2, 2);
    audioProc.onaudioprocess = (ev) => {
      if (!victr) return;
      const inL = ev.inputBuffer.getChannelData(0);
      const inR = ev.inputBuffer.numberOfChannels > 1 ? ev.inputBuffer.getChannelData(1) : inL;
      const outL = ev.outputBuffer.getChannelData(0);
      const outR = ev.outputBuffer.getChannelData(1);
      victr.process_audio(inL, inR, outL, outR);
    };
    audioProc.connect(audioCtx.destination);
    try {
      audioSource = audioCtx.createMediaElementSource(video);
      audioSource.connect(audioProc);
    } catch {}
  }
  if (audioCtx && audioCtx.state === "suspended") {
    audioCtx.resume().catch(() => {});
  }
  video.muted = false;
  audioUnlocked = true;
  soundPill.classList.add("hidden");
}

function stopCurrentStream() {
  if (hls) {
    hls.destroy();
    hls = null;
  }
  if (mediaStream) {
    mediaStream.getTracks().forEach((t) => t.stop());
    mediaStream = null;
  }
  video.pause();
  video.srcObject = null;
  video.removeAttribute("src");
  video.load();
}

function isImageUrl(url) {
  return /\.(png|jpe?g|webp|gif|bmp)(\?.*)?$/i.test(url);
}

function isHlsUrl(url) {
  return /\.m3u8(\?.*)?$/i.test(url);
}

function isVideoFileUrl(url) {
  return /\.(mp4|webm|ogv|m4v|mov)(\?.*)?$/i.test(url);
}

function isJsonFeedUrl(url) {
  return /\.json(\?.*)?$/i.test(url);
}

function isM3uPlaylistUrl(url) {
  return /\.m3u(\?.*)?$/i.test(url);
}

// ---- M3U / M3U8 Multi-Channel Playlist Parser
function parseM3uPlaylist(text) {
  const lines = text.split(/\r?\n/);
  const out = [];
  let pending = null;
  for (const raw of lines) {
    const line = raw.trim();
    if (!line) continue;
    if (line.startsWith("#EXTINF:")) {
      const commaIdx = line.indexOf(",");
      const attrs = commaIdx >= 0 ? line.slice(8, commaIdx) : line.slice(8);
      const title = commaIdx >= 0 ? line.slice(commaIdx + 1).trim() : "";
      const attr = (key) => {
        const m = attrs.match(new RegExp(`${key}="([^"]*)"`, "i"));
        return m ? m[1].trim() : "";
      };
      pending = {
        name: title || attr("tvg-name") || "Live Channel",
        callsign: attr("tvg-id").replace(/[^A-Za-z0-9]/g, "").slice(0, 5),
        group: attr("group-title") || "Live",
        number: parseInt(attr("tvg-chno") || "0", 10) || undefined,
      };
    } else if (!line.startsWith("#")) {
      const meta = pending || { name: line.replace(/^https?:\/\//i, "").split("/")[0], group: "Live" };
      out.push({
        number: meta.number,
        name: meta.name,
        callsign: meta.callsign || "",
        kind: "live",
        group: meta.group || "Live",
        now_title: meta.name,
        stream_url: line,
      });
      pending = null;
    }
  }
  return out;
}

// ---- Indie Streamer Auto-Resolvers (PeerTube & Owncast)
async function resolveIndieUrl(rawUrl) {
  const url = rawUrl.trim();
  if (isHlsUrl(url) || isVideoFileUrl(url) || isImageUrl(url) || url.startsWith("blob:")) {
    return { streamUrl: url };
  }
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return { streamUrl: url };
  }

  // 1. PeerTube watch link (/w/<id> or /videos/watch/<id>)
  const ptMatch = parsed.pathname.match(/^\/(?:w|videos\/watch)\/([A-Za-z0-9_-]+)/);
  if (ptMatch) {
    try {
      const api = `${parsed.origin}/api/v1/videos/${ptMatch[1]}`;
      const resp = await fetch(proxiedUrl(api));
      if (resp.ok) {
        const data = await resp.json();
        const hlsUrl = data.streamingPlaylists?.[0]?.playlistUrl;
        const mp4Url = data.files?.[0]?.fileUrl || data.streamingPlaylists?.[0]?.files?.[0]?.fileUrl;
        const streamUrl = hlsUrl || mp4Url;
        if (streamUrl) {
          return {
            streamUrl,
            title: data.name || data.channel?.displayName || "PeerTube",
            callsign: (data.channel?.displayName || "PTUBE").replace(/[^A-Za-z0-9]/g, "").slice(0, 5),
            overview: data.description || "",
          };
        }
      }
    } catch {}
  }

  // 2. Owncast instance root URL (probes /api/status)
  if (parsed.pathname === "/" || parsed.pathname === "" || parsed.pathname === "/embed/video") {
    try {
      const resp = await fetch(proxiedUrl(`${parsed.origin}/api/status`));
      if (resp.ok) {
        const st = await resp.json();
        if (typeof st.online === "boolean") {
          return {
            streamUrl: `${parsed.origin}/hls/stream.m3u8`,
            title: st.streamTitle || st.name || parsed.hostname,
            callsign: (st.name || "CAST").replace(/[^A-Za-z0-9]/g, "").slice(0, 5),
            offline: !st.online,
            offlineDetail: st.offlineMessage || "OWNCAST STREAM CURRENTLY OFFLINE",
          };
        }
      }
    } catch {}
  }

  return { streamUrl: url };
}

async function loadM3uFeed(url) {
  try {
    showToast("Loading M3U lineup…");
    const resp = await fetch(proxiedUrl(url));
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const text = await resp.text();
    const defs = parseM3uPlaylist(text);
    if (!defs.length) throw new Error("No channels found in M3U playlist");
    activeM3uUrl = url;
    activeFeedUrl = "";
    lineupDefs = defs;
    victr.set_lineup_json(JSON.stringify(lineupDefs));
    showToast(`Loaded ${defs.length} channels — press G for Guide!`);
    return true;
  } catch (err) {
    showToast(`M3U load failed: ${err.message || err}`);
    return false;
  }
}

async function loadJsonFeed(url) {
  try {
    showToast("Loading channel feed…");
    const resp = await fetch(proxiedUrl(url));
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const data = await resp.json();
    const rawList = Array.isArray(data) ? data : Array.isArray(data.channels) ? data.channels : [];
    if (!rawList.length) throw new Error("No channels in feed JSON");
    activeFeedUrl = url;
    activeM3uUrl = "";
    lineupDefs = rawList;
    victr.set_lineup_json(JSON.stringify(lineupDefs));
    showToast(`Loaded ${rawList.length} channels — press G for Guide!`);
    return true;
  } catch (err) {
    showToast(`Feed load failed: ${err.message || err}`);
    return false;
  }
}

async function tuneUrl(rawUrl, opts = {}) {
  const trimmed = (rawUrl || "").trim();
  if (!trimmed || !victr) return;

  // Allow pasting a .json feed or .m3u lineup directly into the Tune bar!
  if (!opts.skipFeedDetect) {
    if (isJsonFeedUrl(trimmed)) {
      if (await loadJsonFeed(trimmed)) {
        if (!victr.is_guide_active()) victr.toggle_guide();
        syncUiFromEngine();
      }
      return;
    }
    if (isM3uPlaylistUrl(trimmed)) {
      if (await loadM3uFeed(trimmed)) {
        if (!victr.is_guide_active()) victr.toggle_guide();
        syncUiFromEngine();
      }
      return;
    }
  }

  stopCurrentStream();
  victr.switch_channel_effect();

  const chNum = opts.ch || 80;
  let callsign = (opts.callsign || "LIVE").toUpperCase().slice(0, 5);
  let title = opts.title || trimmed.replace(/^https?:\/\//i, "").split("/")[0];
  let url = trimmed;

  // Show Teletext while probing Owncast/PeerTube or buffering
  victr.set_teletext(true, false, chNum, callsign, title, trimmed, "ACQUIRING CARRIER...");

  const resolved = await resolveIndieUrl(trimmed);
  url = resolved.streamUrl || trimmed;
  if (resolved.title && !opts.title) title = resolved.title;
  if (resolved.callsign && !opts.callsign) callsign = resolved.callsign.toUpperCase();

  currentSource = {
    type: "url",
    url: trimmed,
    title,
    ch: chNum,
    callsign,
    sourceKey: opts.sourceKey || "",
    itemId: opts.itemId || "",
    itemPlayed: Boolean(opts.itemPlayed),
    itemFavorite: Boolean(opts.itemFavorite),
    hasItems: Boolean(opts.hasItems),
  };
  $("stream-url").value = trimmed;
  victr.set_active_context(chNum, currentSource.sourceKey, currentSource.itemId, `CH ${String(chNum).padStart(2, "0")} ${callsign} · ${title}`);
  persistWatchState();
  victr.show_osd_channel(chNum, callsign, title);
  syncUiFromEngine();

  if (resolved.offline) {
    victr.set_teletext(true, true, chNum, callsign, title, trimmed, resolved.offlineDetail || "STREAM OFFLINE");
    return;
  }

  if (isImageUrl(url)) {
    victr.set_teletext(true, false, chNum, callsign, title, url, "LOADING IMAGE...");
    const img = new Image();
    img.crossOrigin = "anonymous";
    img.onload = () => {
      victr.upload_image_element(img);
      victr.set_teletext(false, false, chNum, callsign, title, url, "");
    };
    img.onerror = () => {
      victr.set_teletext(true, true, chNum, callsign, title, url, "IMAGE LOAD OR CORS BLOCKED");
    };
    img.src = proxiedUrl(url);
    return;
  }

  const effectiveUrl = proxiedUrl(url);
  const startSecs = Number(opts.startSecs || 0);

  const onPlaying = () => {
    victr.set_teletext(false, false, chNum, callsign, title, url, "");
    if (!audioUnlocked) soundPill.classList.remove("hidden");
  };
  const onFail = (detail) => {
    victr.set_teletext(true, true, chNum, callsign, title, url, detail || "STREAM OFFLINE OR CORS BLOCKED");
  };

  video.onplaying = onPlaying;
  video.onerror = () => onFail("VIDEO DECODE OR CORS BLOCKED");
  video.onloadedmetadata = () => {
    if (startSecs > 1 && Number.isFinite(video.duration) && startSecs < video.duration - 5) {
      video.currentTime = startSecs;
    }
  };
  video.onended = () => {
    if (currentSource.ch > 0 && currentSource.itemId) {
      victr.toggle_item_watched(currentSource.itemId);
      persistWatchState();
      stepEpisode(1);
    }
  };

  if (isHlsUrl(url) && window.Hls && window.Hls.isSupported()) {
    hls = new window.Hls({
      lowLatencyMode: false,
      backBufferLength: 15,
      maxBufferLength: 12,
      capLevelToPlayerSize: false,
    });
    hls.on(window.Hls.Events.MANIFEST_PARSED, (_e, data) => {
      const levels = data.levels || [];
      let best = -1;
      for (let i = 0; i < levels.length; i++) {
        if ((levels[i].height || 0) <= 480) best = i;
      }
      if (best >= 0) hls.currentLevel = best;
      video.play().catch(() => {
        video.muted = true;
        video.play().catch(() => {});
        soundPill.classList.remove("hidden");
      });
    });
    hls.on(window.Hls.Events.ERROR, (_e, data) => {
      if (data.fatal) {
        onFail(data.details ? `HLS: ${data.details.toUpperCase()}` : "HLS STREAM INACTIVE");
      }
    });
    hls.loadSource(effectiveUrl);
    hls.attachMedia(video);
  } else {
    video.src = effectiveUrl;
    video.play().catch(() => {
      video.muted = true;
      video.play().catch(() => {});
      soundPill.classList.remove("hidden");
    });
  }
}

function applyTuneTarget(t) {
  if (!t || !t.stream_url) return;
  if (t.profile_override) {
    victr.set_preset_by_name(t.profile_override);
  }
  tuneUrl(t.stream_url, {
    ch: t.channel_number,
    callsign: t.callsign,
    title: t.item_title || t.channel_name,
    sourceKey: t.source_key,
    itemId: t.item_id,
    itemPlayed: t.item_played,
    itemFavorite: t.item_favorite,
    hasItems: t.has_items,
    startSecs: t.start_secs || 0,
    skipFeedDetect: true,
  });
}

function handleGuideActionJson(raw) {
  if (!raw || raw === "{}") return;
  const res = JSON.parse(raw);
  if (res.action === "tune" && res.target) {
    applyTuneTarget(res.target);
  }
  syncUiFromEngine();
}

function tuneChannelNumber(chNum) {
  if (!victr || chNum <= 0) return;
  handleGuideActionJson(victr.resolve_channel_json(chNum));
}

function surfChannel(delta) {
  const next = victr.surf_channel_num(delta);
  if (next > 0) tuneChannelNumber(next);
}

function stepEpisode(delta) {
  if (!currentSource.ch || !currentSource.itemId) return;
  const raw = victr.step_episode_json(currentSource.ch, currentSource.itemId, delta);
  if (raw && raw !== "{}") {
    applyTuneTarget(JSON.parse(raw));
  } else {
    victr.show_osd_action(delta >= 0 ? "LAST ITEM" : "FIRST ITEM");
  }
}

function tunePattern(key, label) {
  stopCurrentStream();
  currentSource = {
    type: "pattern",
    key,
    url: "",
    title: label,
    ch: 0,
    callsign: "TEST",
    sourceKey: "",
    itemId: "",
    itemPlayed: false,
    itemFavorite: false,
    hasItems: false,
  };
  victr.switch_channel_effect();
  victr.set_teletext(false, false, 0, "", "", "", "");
  victr.set_pattern(key);
  victr.show_osd_action(label.toUpperCase());
  syncUiFromEngine();
}

async function startTabMirror() {
  if (!navigator.mediaDevices || !navigator.mediaDevices.getDisplayMedia) {
    showToast("Tab/window mirroring is not supported in this browser.");
    return;
  }
  try {
    const stream = await navigator.mediaDevices.getDisplayMedia({
      video: { frameRate: { ideal: 60 } },
      audio: true,
    });
    stopCurrentStream();
    mediaStream = stream;
    video.srcObject = stream;
    await video.play();
    currentSource = {
      type: "mirror",
      url: "",
      title: "AV 1 · Live Mirror",
      ch: 0,
      callsign: "AV1",
      sourceKey: "",
      itemId: "",
      itemPlayed: false,
      itemFavorite: false,
      hasItems: false,
    };
    victr.switch_channel_effect();
    victr.set_teletext(false, false, 0, "AV1", "Live Mirror", "", "");
    victr.show_osd_channel(0, "AV1", "LIVE CAPTURE");
    stream.getVideoTracks()[0]?.addEventListener("ended", () => {
      victr.show_osd_action("AV1 DISCONNECTED");
      tunePattern("demo_scene", "Demo Scene");
    });
    syncUiFromEngine();
    showToast("Mirroring live tab/window onto CRT!");
  } catch (err) {
    if (err?.name !== "AbortError" && err?.name !== "NotAllowedError") {
      showToast(`Mirror error: ${err}`);
    }
  }
}

async function loadLocalFile(file) {
  if (!file || !victr) return;
  if (file.type.startsWith("image/")) {
    stopCurrentStream();
    const bmp = await createImageBitmap(file);
    victr.switch_channel_effect();
    victr.upload_image_bitmap(bmp);
    bmp.close();
    victr.show_osd_action(file.name.slice(0, 22).toUpperCase());
    currentSource = { type: "file", url: "", title: file.name, ch: 0, callsign: "IMG", sourceKey: "", itemId: "", itemPlayed: false, itemFavorite: false, hasItems: false };
    syncUiFromEngine();
  } else if (file.type.startsWith("video/")) {
    const blobUrl = URL.createObjectURL(file);
    tuneUrl(blobUrl, { ch: 0, callsign: "VCR", title: file.name, skipFeedDetect: true });
    currentSource.type = "file";
  } else if (/\.m3u8?$/i.test(file.name)) {
    const text = await file.text();
    const defs = parseM3uPlaylist(text);
    if (defs.length) {
      lineupDefs = defs;
      victr.set_lineup_json(JSON.stringify(lineupDefs));
      if (!victr.is_guide_active()) victr.toggle_guide();
      showToast(`Loaded ${defs.length} channels from ${file.name}`);
      syncUiFromEngine();
    }
  } else if (/\.json$/i.test(file.name)) {
    const data = JSON.parse(await file.text());
    const rawList = Array.isArray(data) ? data : Array.isArray(data.channels) ? data.channels : [];
    if (rawList.length) {
      lineupDefs = rawList;
      victr.set_lineup_json(JSON.stringify(lineupDefs));
      if (!victr.is_guide_active()) victr.toggle_guide();
      showToast(`Loaded ${rawList.length} channels from ${file.name}`);
      syncUiFromEngine();
    }
  } else {
    showToast("Drop an MP4/WebM video, image, .m3u playlist, or channels.json.");
  }
}

function buildShareUrl() {
  const s = JSON.parse(victr.get_settings_json());
  const out = new URL(window.location.origin + window.location.pathname);
  if (activeFeedUrl) {
    out.searchParams.set("feed", activeFeedUrl);
    if (currentSource.ch > 1) out.searchParams.set("ch", String(currentSource.ch));
  } else if (activeM3uUrl) {
    out.searchParams.set("m3u", activeM3uUrl);
    if (currentSource.ch > 1) out.searchParams.set("ch", String(currentSource.ch));
  } else if (currentSource.type === "url" && currentSource.url) {
    out.searchParams.set("url", currentSource.url);
    if (currentSource.title) out.searchParams.set("title", currentSource.title);
    if (currentSource.ch) out.searchParams.set("ch", String(currentSource.ch));
    if (currentSource.callsign) out.searchParams.set("tag", currentSource.callsign);
  } else if (currentSource.type === "pattern") {
    out.searchParams.set("pattern", currentSource.key);
  }
  out.searchParams.set("preset", String(s.preset_index));
  if (s.crop_43) out.searchParams.set("crop", "1");
  if (s.vcr) out.searchParams.set("vcr", "1");
  if (s.input !== "composite") out.searchParams.set("input", s.input);
  if (!s.show_bezel) out.searchParams.set("bezel", "0");
  return out.toString();
}

function syncUiFromEngine() {
  if (!victr) return;
  const s = JSON.parse(victr.get_settings_json());
  $("sel-preset").value = String(s.preset_index);
  $("sel-input").value = s.input;
  $("sel-mask").value = s.mask;
  $("chk-vcr").checked = s.vcr;
  $("chk-bezel").checked = s.show_bezel;
  $("chk-chaos").checked = s.chaos;
  $("chk-speaker").checked = s.tv_speaker;
  $("chk-mono").checked = s.mono;
  $("sl-bright").value = String(s.brightness);
  $("sl-contrast").value = String(s.contrast);
  $("sl-color").value = String(s.color);
  $("sl-curv").value = String(s.curvature);
  $("sl-snr").value = String(s.signal_db);
  $("sl-vol").value = String(s.volume);
  $("btn-power").classList.toggle("active", s.power_on);
  $("btn-crop").classList.toggle("active", s.crop_43);
  $("btn-guide").classList.toggle("active", s.guide_active);
  $("btn-guide").textContent = s.guide_active ? "Exit Guide (G)" : "Cable Guide (G)";
  $("btn-mute").textContent = s.muted ? "Unmute" : "Mute";
  $("btn-whack").classList.toggle("hidden", !s.chaos);
  $("gpu-badge").textContent = `${s.backend} · ${s.hdr_active ? "HDR" : "SDR"} · ${s.screen_inches.toFixed(1)}"`;

  const itemBar = $("active-item-bar");
  itemBar.classList.toggle("hidden", !currentSource.hasItems || !currentSource.itemId);
  if (currentSource.hasItems && currentSource.itemId) {
    $("btn-watched").textContent = currentSource.itemPlayed ? "Watched" : "Mark Watched";
    $("btn-watched").classList.toggle("active", currentSource.itemPlayed);
    $("btn-fav").classList.toggle("active", currentSource.itemFavorite);
  }
}

async function applyUrlParamsOnBoot() {
  const params = new URLSearchParams(window.location.search);
  proxyBase = params.get("proxy") || "";
  if (params.get("embed") === "1" || params.get("embed") === "true") {
    document.body.classList.add("embed-mode");
  }

  loadPersistedWatchState();
  victr.set_lineup_json(JSON.stringify(lineupDefs));

  const presetParam = params.get("preset");
  if (presetParam) victr.set_preset_by_name(presetParam);

  const patch = {};
  if (params.has("input")) patch.input = params.get("input");
  if (params.has("vcr")) patch.vcr = params.get("vcr") === "1" || params.get("vcr") === "true";
  if (params.has("bezel")) patch.show_bezel = !(params.get("bezel") === "0" || params.get("bezel") === "false");
  if (params.has("chaos")) patch.chaos = params.get("chaos") === "1" || params.get("chaos") === "true";
  if (Object.keys(patch).length > 0) victr.apply_settings_patch(JSON.stringify(patch));

  if (params.get("crop") === "1" || params.get("crop") === "true") {
    victr.set_crop_43(true);
  }

  // Load multi-channel ?feed= or ?m3u= if provided
  if (params.get("feed")) {
    const ok = await loadJsonFeed(params.get("feed"));
    if (ok) {
      const wantCh = parseInt(params.get("ch") || "0", 10);
      if (wantCh > 1) tuneChannelNumber(wantCh);
      else victr.toggle_guide();
      syncUiFromEngine();
      return;
    }
  } else if (params.get("m3u")) {
    const ok = await loadM3uFeed(params.get("m3u"));
    if (ok) {
      const wantCh = parseInt(params.get("ch") || "0", 10);
      if (wantCh > 1) tuneChannelNumber(wantCh);
      else victr.toggle_guide();
      syncUiFromEngine();
      return;
    }
  }

  const slug = window.location.pathname.replace(/^\/+|\/+$/g, "").toLowerCase();
  const explicitUrl = params.get("url") || params.get("src");
  if (explicitUrl) {
    await tuneUrl(explicitUrl, {
      ch: parseInt(params.get("ch") || "80", 10) || 80,
      callsign: params.get("tag") || "LIVE",
      title: params.get("title") || "",
    });
  } else if (slug === "guide" || params.get("guide") === "1") {
    victr.toggle_guide();
  } else if (slug === "nhk") {
    tuneChannelNumber(80);
  } else if (slug === "dw") {
    tuneChannelNumber(81);
  } else if (slug === "bars" || params.get("pattern") === "color_bars") {
    tunePattern("color_bars", "Color Bars");
  } else if (slug === "demo" || params.get("pattern") === "demo_scene") {
    tunePattern("demo_scene", "Demo Scene");
  } else if (params.has("pattern")) {
    tunePattern(params.get("pattern"), params.get("pattern"));
  } else {
    tuneChannelNumber(80);
  }
  syncUiFromEngine();
}

function wireControls() {
  const presets = JSON.parse(VictrWeb.preset_names_json());
  const selPreset = $("sel-preset");
  presets.forEach((name, idx) => {
    const opt = document.createElement("option");
    opt.value = String(idx);
    opt.textContent = name;
    selPreset.appendChild(opt);
  });

  const unlockOnce = () => ensureAudioGraph();
  window.addEventListener("pointerdown", unlockOnce, { once: true });
  window.addEventListener("keydown", unlockOnce, { once: true });
  soundPill.addEventListener("click", () => {
    ensureAudioGraph();
    victr?.show_osd_action("TV SOUND ON");
  });

  drawerTab.addEventListener("click", () => document.body.classList.toggle("drawer-open"));
  $("btn-drawer-close").addEventListener("click", () => document.body.classList.remove("drawer-open"));
  $("btn-remote-hdr").addEventListener("click", () => remoteEl.classList.toggle("hidden"));
  $("btn-remote-close").addEventListener("click", () => remoteEl.classList.add("hidden"));

  $("stream-form").addEventListener("submit", (e) => {
    e.preventDefault();
    ensureAudioGraph();
    tuneUrl($("stream-url").value);
  });

  $("btn-guide").addEventListener("click", () => {
    ensureAudioGraph();
    victr.toggle_guide();
    syncUiFromEngine();
  });

  $("btn-list").addEventListener("click", () => {
    victr.guide_open_picker();
    syncUiFromEngine();
  });
  $("btn-watched").addEventListener("click", () => {
    if (!currentSource.itemId) return;
    currentSource.itemPlayed = victr.toggle_item_watched(currentSource.itemId);
    persistWatchState();
    syncUiFromEngine();
  });
  $("btn-fav").addEventListener("click", () => {
    if (!currentSource.itemId) return;
    currentSource.itemFavorite = victr.toggle_item_favorite(currentSource.itemId, currentSource.title);
    persistWatchState();
    syncUiFromEngine();
  });
  $("btn-next").addEventListener("click", () => stepEpisode(1));

  document.querySelectorAll(".chip").forEach((el) => {
    el.addEventListener("click", () => {
      ensureAudioGraph();
      const s = el.dataset.slug;
      if (s === "nhk") tuneChannelNumber(80);
      else if (s === "dw") tuneChannelNumber(81);
      else if (s === "demo") tunePattern("demo_scene", "Demo Scene");
      else if (s === "bars") tunePattern("color_bars", "Color Bars");
    });
  });

  const copyShare = async () => {
    const link = buildShareUrl();
    try {
      await navigator.clipboard.writeText(link);
      showToast("Shareable onvic.tr link copied!");
      victr.show_osd_action("LINK COPIED");
    } catch {
      window.prompt("Copy your shareable CRT link:", link);
    }
  };
  $("btn-share").addEventListener("click", copyShare);
  $("btn-mirror").addEventListener("click", () => {
    ensureAudioGraph();
    startTabMirror();
  });
  $("file-input").addEventListener("change", (e) => {
    ensureAudioGraph();
    const file = e.target.files?.[0];
    if (file) loadLocalFile(file);
  });

  $("btn-power").addEventListener("click", () => { victr.toggle_power(); syncUiFromEngine(); });
  $("btn-degauss").addEventListener("click", () => victr.degauss());
  $("btn-crop").addEventListener("click", () => { victr.toggle_crop_43(); syncUiFromEngine(); });
  $("btn-pp").addEventListener("click", () => {
    const inches = victr.snap_pixel_perfect();
    syncUiFromEngine();
    showToast(`1:1 pixel-perfect phosphors (${inches.toFixed(1)}")`);
  });
  $("btn-reset-view").addEventListener("click", () => victr.reset_view());
  $("btn-fs").addEventListener("click", () => {
    if (document.fullscreenElement) document.exitFullscreen();
    else document.documentElement.requestFullscreen?.();
  });
  $("btn-whack").addEventListener("click", () => victr.whack());

  selPreset.addEventListener("change", () => {
    victr.set_preset(parseInt(selPreset.value, 10));
    syncUiFromEngine();
  });
  const bindPatch = (id, key, transform = (el) => el.value) => {
    $(id).addEventListener("input", (e) => {
      victr.apply_settings_patch(JSON.stringify({ [key]: transform(e.target) }));
      syncUiFromEngine();
    });
  };
  bindPatch("sel-input", "input");
  bindPatch("sel-mask", "mask");
  bindPatch("chk-vcr", "vcr", (el) => el.checked);
  bindPatch("chk-bezel", "show_bezel", (el) => el.checked);
  bindPatch("chk-chaos", "chaos", (el) => el.checked);
  bindPatch("chk-speaker", "tv_speaker", (el) => el.checked);
  bindPatch("chk-mono", "mono", (el) => el.checked);
  bindPatch("sl-bright", "brightness", (el) => parseFloat(el.value));
  bindPatch("sl-contrast", "contrast", (el) => parseFloat(el.value));
  bindPatch("sl-color", "color", (el) => parseFloat(el.value));
  bindPatch("sl-curv", "curvature", (el) => parseFloat(el.value));
  bindPatch("sl-snr", "signal_db", (el) => parseFloat(el.value));

  $("btn-mute").addEventListener("click", () => { ensureAudioGraph(); victr.toggle_mute(); syncUiFromEngine(); });
  $("sl-vol").addEventListener("input", (e) => { ensureAudioGraph(); victr.set_volume(parseFloat(e.target.value)); syncUiFromEngine(); });

  remoteEl.querySelectorAll("[data-act]").forEach((btn) => {
    btn.addEventListener("click", () => {
      ensureAudioGraph();
      switch (btn.dataset.act) {
        case "power": victr.toggle_power(); syncUiFromEngine(); break;
        case "mute": victr.toggle_mute(); syncUiFromEngine(); break;
        case "info": victr.show_osd_channel(currentSource.ch, currentSource.callsign, currentSource.title); break;
        case "guide": victr.toggle_guide(); syncUiFromEngine(); break;
        case "list": victr.guide_open_picker(); syncUiFromEngine(); break;
        case "watched":
          if (victr.is_guide_active()) {
            victr.guide_toggle_selected_watched();
          } else if (currentSource.itemId) {
            currentSource.itemPlayed = victr.toggle_item_watched(currentSource.itemId);
          }
          persistWatchState();
          syncUiFromEngine();
          break;
        case "ch-prev": surfChannel(-1); break;
        case "ch-next": surfChannel(1); break;
        case "vol-down": victr.adjust_volume(-0.05); syncUiFromEngine(); break;
        case "vol-up": victr.adjust_volume(0.05); syncUiFromEngine(); break;
        case "play":
          if (video.paused) { video.play().catch(() => {}); victr.show_osd_action("PLAY >"); }
          else { video.pause(); victr.show_osd_action("PAUSE ||"); }
          break;
        case "retry":
          if (currentSource.type === "url") tuneUrl(currentSource.url, currentSource);
          break;
        case "crop": victr.toggle_crop_43(); syncUiFromEngine(); break;
        case "pp": victr.snap_pixel_perfect(); syncUiFromEngine(); break;
        case "degauss": victr.degauss(); break;
        case "fs":
          if (document.fullscreenElement) document.exitFullscreen();
          else document.documentElement.requestFullscreen?.();
          break;
        case "share": copyShare(); break;
      }
    });
  });

  // Canvas zoom / pan / click-to-tune inside 90s Prevue Cable Guide
  let dragging = false;
  let moved = false;
  let lastX = 0;
  let lastY = 0;
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    if (victr.is_guide_active()) {
      victr.guide_move(e.deltaY > 0 ? 1 : -1);
      return;
    }
    const factor = e.deltaY < 0 ? 1.12 : 1 / 1.12;
    victr.zoom_by(factor);
  }, { passive: false });
  canvas.addEventListener("pointerdown", (e) => {
    dragging = true;
    moved = false;
    lastX = e.clientX;
    lastY = e.clientY;
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    if (Math.hypot(e.clientX - lastX, e.clientY - lastY) > 4) moved = true;
    if (victr.is_guide_active()) return;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    victr.pan_by((e.clientX - lastX) * dpr, (e.clientY - lastY) * dpr);
    lastX = e.clientX;
    lastY = e.clientY;
  });
  canvas.addEventListener("pointerup", (e) => {
    if (!moved && victr.is_guide_active()) {
      const rect = canvas.getBoundingClientRect();
      const nx = (e.clientX - rect.left) / Math.max(1, rect.width);
      const ny = (e.clientY - rect.top) / Math.max(1, rect.height);
      handleGuideActionJson(victr.guide_click_json(nx, ny));
    }
    dragging = false;
  });
  canvas.addEventListener("dblclick", () => {
    if (!victr.is_guide_active()) victr.reset_view();
  });

  // Drag & drop files anywhere on window
  const dropEl = $("drop-overlay");
  let dragDepth = 0;
  window.addEventListener("dragenter", (e) => { e.preventDefault(); dragDepth++; dropEl.classList.remove("hidden"); });
  window.addEventListener("dragover", (e) => e.preventDefault());
  window.addEventListener("dragleave", () => { dragDepth = Math.max(0, dragDepth - 1); if (dragDepth === 0) dropEl.classList.add("hidden"); });
  window.addEventListener("drop", (e) => {
    e.preventDefault();
    dragDepth = 0;
    dropEl.classList.add("hidden");
    const file = e.dataTransfer?.files?.[0];
    if (file) { ensureAudioGraph(); loadLocalFile(file); }
  });

  // Keyboard TV remote + Guide controls
  window.addEventListener("keydown", (e) => {
    const tag = document.activeElement?.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;

    // Live `/` search typing inside Guide
    if (victr.is_guide_searching()) {
      if (e.key === "Escape") { victr.guide_close_or_back(); return; }
      if (e.key === "Backspace") { e.preventDefault(); victr.guide_search_pop(); return; }
      if (e.key === "Enter") { e.preventDefault(); handleGuideActionJson(victr.guide_confirm_json()); return; }
      if (e.key === "ArrowUp") { e.preventDefault(); victr.guide_move(-1); return; }
      if (e.key === "ArrowDown") { e.preventDefault(); victr.guide_move(1); return; }
      if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.preventDefault();
        victr.guide_search_push(e.key);
        return;
      }
      return;
    }

    if (e.key === "Escape") {
      if (!remoteEl.classList.contains("hidden")) { remoteEl.classList.add("hidden"); return; }
      if (victr.guide_close_or_back()) { syncUiFromEngine(); return; }
      document.body.classList.remove("drawer-open");
      return;
    }
    if (e.key === "?") { e.preventDefault(); remoteEl.classList.toggle("hidden"); return; }
    if ((e.ctrlKey || e.metaKey) && e.key === "1") { e.preventDefault(); victr.snap_pixel_perfect(); syncUiFromEngine(); return; }
    if (e.shiftKey && (e.key === "Z" || e.key === "z")) { e.preventDefault(); victr.toggle_crop_43(); syncUiFromEngine(); return; }
    if (e.ctrlKey || e.metaKey || e.altKey) return;

    if (e.key === "/" && victr.is_guide_active()) { e.preventDefault(); victr.guide_search_start(); return; }
    if (e.key === "*") {
      e.preventDefault();
      if (victr.is_guide_active()) {
        victr.guide_toggle_selected_favorite();
      } else if (currentSource.itemId) {
        currentSource.itemFavorite = victr.toggle_item_favorite(currentSource.itemId, currentSource.title);
      }
      persistWatchState();
      syncUiFromEngine();
      return;
    }
    if (/^[0-9]$/.test(e.key)) {
      const ch = victr.push_digit(e.key);
      if (ch > 0) tuneChannelNumber(ch);
      return;
    }

    if (victr.is_guide_active()) {
      if (e.key === "ArrowUp") { e.preventDefault(); victr.guide_move(-1); return; }
      if (e.key === "ArrowDown") { e.preventDefault(); victr.guide_move(1); return; }
      if (e.key === "ArrowLeft") { e.preventDefault(); victr.guide_shift_h(-1); return; }
      if (e.key === "ArrowRight") { e.preventDefault(); victr.guide_shift_h(1); return; }
      if (e.key === "Backspace") { e.preventDefault(); victr.guide_close_or_back(); return; }
      if (e.key === "Enter") {
        e.preventDefault();
        const dialed = victr.commit_digits();
        if (dialed > 0) tuneChannelNumber(dialed);
        else handleGuideActionJson(victr.guide_confirm_json());
        return;
      }
    } else if (e.key === "Enter") {
      const dialed = victr.commit_digits();
      if (dialed > 0) { tuneChannelNumber(dialed); return; }
    }

    switch (e.key) {
      case "g": case "G": victr.toggle_guide(); syncUiFromEngine(); break;
      case "e": case "E": victr.guide_open_picker(); syncUiFromEngine(); break;
      case "w": case "W":
        if (victr.is_guide_active()) {
          victr.guide_toggle_selected_watched();
        } else if (currentSource.itemId) {
          currentSource.itemPlayed = victr.toggle_item_watched(currentSource.itemId);
        }
        persistWatchState();
        syncUiFromEngine();
        break;
      case "n": case "N": stepEpisode(1); break;
      case "p": case "P": stepEpisode(-1); break;
      case "m": case "M": victr.toggle_mute(); syncUiFromEngine(); break;
      case "-": victr.adjust_volume(-0.05); syncUiFromEngine(); break;
      case "=": case "+": victr.adjust_volume(0.05); syncUiFromEngine(); break;
      case "[": case "PageDown": surfChannel(-1); break;
      case "]": case "PageUp": surfChannel(1); break;
      case "d": case "D": victr.degauss(); break;
      case "k": case "K": victr.whack(); break;
      case "i": case "I": victr.show_osd_channel(currentSource.ch, currentSource.callsign, currentSource.title); break;
      case "r": case "R":
        if (currentSource.type === "url") tuneUrl(currentSource.url, currentSource);
        break;
      case "f": case "F":
        if (document.fullscreenElement) document.exitFullscreen();
        else document.documentElement.requestFullscreen?.();
        break;
      case " ":
        e.preventDefault();
        if (video.src || video.srcObject) {
          if (video.paused) { video.play().catch(() => {}); victr.show_osd_action("PLAY >"); }
          else { video.pause(); victr.show_osd_action("PAUSE ||"); }
        }
        break;
    }
  });
}

async function boot() {
  if (!navigator.gpu) {
    showWebGpuError("This browser does not expose WebGPU (navigator.gpu is unavailable).");
    return;
  }
  try {
    await init();
    resizeCanvas();
    victr = await VictrWeb.create(canvas);
  } catch (err) {
    showWebGpuError(String(err));
    return;
  }

  window.addEventListener("resize", resizeCanvas);
  wireControls();
  await applyUrlParamsOnBoot();

  let lastTs = performance.now();
  let lastResumeSave = performance.now();

  // Standard Gamepad API polling for couch-TV Guide/Picker navigation
  let gpPrev = 0;
  let gpRepeatDir = 0;
  let gpRepeatNext = 0;
  let gpChordFired = false;
  function pollBrowserGamepad(now) {
    if (!navigator.getGamepads) return;
    const pads = navigator.getGamepads();
    let mask = 0;
    for (const p of pads) {
      if (!p || !p.connected) continue;
      const b = p.buttons || [];
      const down = (i) => Boolean(b[i] && (b[i].pressed || b[i].value > 0.5));
      if (down(0)) mask |= 1 << 0;  // A / Cross (Confirm)
      if (down(1)) mask |= 1 << 1;  // B / Circle (Back)
      if (down(2)) mask |= 1 << 2;  // X / Square (List / Watched)
      if (down(3)) mask |= 1 << 3;  // Y / Triangle (Favorite)
      if (down(4)) mask |= 1 << 4;  // LB
      if (down(5)) mask |= 1 << 5;  // RB
      if (down(8)) mask |= 1 << 6;  // Select / Back
      if (down(9)) mask |= 1 << 7;  // Start
      if (down(12) || (p.axes?.[1] ?? 0) < -0.5) mask |= 1 << 8;  // Up
      if (down(13) || (p.axes?.[1] ?? 0) > 0.5)  mask |= 1 << 9;  // Down
      if (down(14) || (p.axes?.[0] ?? 0) < -0.5) mask |= 1 << 10; // Left
      if (down(15) || (p.axes?.[0] ?? 0) > 0.5)  mask |= 1 << 11; // Right
      if (down(16)) mask |= 1 << 12; // Home / Guide
    }
    const pressed = mask & ~gpPrev;
    gpPrev = mask;
    const DIR_MASK = (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11);
    const dirBits = mask & DIR_MASK;
    let dirFire = pressed & DIR_MASK;
    if (dirBits === 0) {
      gpRepeatDir = 0;
    } else if (dirFire !== 0) {
      gpRepeatDir = dirBits;
      gpRepeatNext = now + 300;
    } else if (dirBits === gpRepeatDir && now >= gpRepeatNext) {
      dirFire = dirBits;
      gpRepeatNext = now + 85;
    }

    const chord = (mask & ((1 << 6) | (1 << 7))) === ((1 << 6) | (1 << 7));
    if (!chord) gpChordFired = false;
    const chordFire = chord && !gpChordFired;
    if (chordFire) gpChordFired = true;

    if ((pressed & (1 << 12)) || chordFire) {
      ensureAudioGraph();
      victr.toggle_guide();
      syncUiFromEngine();
      return;
    }

    if (victr.is_guide_active()) {
      if (pressed & (1 << 1)) { victr.guide_close_or_back(); syncUiFromEngine(); return; }
      if (dirFire & (1 << 8)) victr.guide_move(-1);
      else if (dirFire & (1 << 9)) victr.guide_move(1);
      else if (pressed & (1 << 4)) victr.guide_move(-6);
      else if (pressed & (1 << 5)) victr.guide_move(6);
      if (dirFire & (1 << 10)) victr.guide_shift_h(-1);
      else if (dirFire & (1 << 11)) victr.guide_shift_h(1);
      if (pressed & (1 << 3)) { victr.guide_toggle_selected_favorite(); persistWatchState(); }
      if (pressed & (1 << 2)) {
        if (!victr.guide_toggle_selected_watched()) victr.guide_open_picker();
        persistWatchState();
      }
      if (pressed & ((1 << 0) | (1 << 7))) {
        handleGuideActionJson(victr.guide_confirm_json());
      }
    } else {
      if (pressed & ((1 << 6) | (1 << 7))) { ensureAudioGraph(); victr.toggle_guide(); syncUiFromEngine(); return; }
      if (pressed & (1 << 4)) surfChannel(-1);
      if (pressed & (1 << 5)) surfChannel(1);
      if (pressed & (1 << 3)) { victr.guide_open_picker(); syncUiFromEngine(); }
      if (pressed & (1 << 2)) victr.show_osd_channel(currentSource.ch, currentSource.callsign, currentSource.title);
      if (pressed & (1 << 1)) { victr.toggle_mute(); syncUiFromEngine(); }
      if (pressed & (1 << 0)) {
        if (video.src || video.srcObject) {
          if (video.paused) { video.play().catch(() => {}); victr.show_osd_action("PLAY >"); }
          else { video.pause(); victr.show_osd_action("PAUSE ||"); }
        }
      }
      if (dirFire & (1 << 8)) { victr.adjust_volume(0.05); syncUiFromEngine(); }
      else if (dirFire & (1 << 9)) { victr.adjust_volume(-0.05); syncUiFromEngine(); }
    }
  }

  function frame(now) {
    const dt = (now - lastTs) / 1000;
    lastTs = now;
    pollBrowserGamepad(now);
    const dialed = victr.poll_digit_commit();
    if (dialed > 0) tuneChannelNumber(dialed);

    if (!victr.is_guide_active() && video.readyState >= 2 && video.videoWidth > 0 && video.videoHeight > 0 && !video.ended) {
      victr.upload_video_frame(video);
    }
    if (currentSource.hasItems && currentSource.itemId && !video.paused && Number.isFinite(video.duration) && now - lastResumeSave > 15000) {
      lastResumeSave = now;
      victr.save_item_resume(currentSource.itemId, video.currentTime, video.duration);
      persistWatchState();
    }
    try {
      victr.tick_and_render(dt);
    } catch (err) {
      console.error("ViCTR frame error:", err);
    }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

boot();
