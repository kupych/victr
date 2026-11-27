// ViCTR Web (onvic.tr) — Thin ES-module shell over victr-web WASM + WebGPU.
import init, { VictrWeb } from "./pkg/victr_web.js";

const STARTER_CHANNELS = [
  {
    slug: "nhk",
    num: 80,
    callsign: "NHK",
    name: "NHK World-Japan",
    url: "https://nhkwlive-ojp.akamaized.net/hls/live/2003459/nhkwlive-ojp-en/index.m3u8",
  },
  {
    slug: "dw",
    num: 81,
    callsign: "DW",
    name: "DW News",
    url: "https://dwamdstream102.akamaized.net/hls/live/2015525/dwstream102/index.m3u8",
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
let currentSource = { type: "pattern", key: "demo_scene", url: "", title: "Demo Scene", ch: 80, callsign: "LIVE" };
let activeStarterIdx = -1;
let toastTimer = null;
let proxyBase = "";

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
    } catch {
      // Cross-origin video without CORS cannot be tapped into WebAudio; fallback to direct element audio
    }
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

function tuneUrl(rawUrl, opts = {}) {
  const url = (rawUrl || "").trim();
  if (!url || !victr) return;
  stopCurrentStream();
  victr.switch_channel_effect();

  const chNum = opts.ch || 80;
  const callsign = (opts.callsign || "LIVE").toUpperCase().slice(0, 5);
  const title = opts.title || url.replace(/^https?:\/\//i, "").split("/")[0];

  currentSource = { type: "url", url, title, ch: chNum, callsign };
  $("stream-url").value = url;
  victr.show_osd_channel(chNum, callsign, title);

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

  // Video / HLS stream -> show authentic 1980s Teletext carrier screen while buffering
  victr.set_teletext(true, false, chNum, callsign, title, url, "ACQUIRING CARRIER...");
  const effectiveUrl = proxiedUrl(url);

  const onPlaying = () => {
    victr.set_teletext(false, false, chNum, callsign, title, url, "");
    if (!audioUnlocked) soundPill.classList.remove("hidden");
  };
  const onFail = (detail) => {
    victr.set_teletext(true, true, chNum, callsign, title, url, detail || "STREAM OFFLINE OR CORS BLOCKED");
  };

  video.onplaying = onPlaying;
  video.onerror = () => onFail("VIDEO DECODE / CORS ERROR");

  if (isHlsUrl(url) && window.Hls && window.Hls.isSupported()) {
    hls = new window.Hls({
      lowLatencyMode: false,
      backBufferLength: 15,
      maxBufferLength: 12,
      capLevelToPlayerSize: false,
    });
    // Prefer <=480p rendition to match 480i CRT raster cleanly when available
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

function tuneStarter(index) {
  const len = STARTER_CHANNELS.length;
  activeStarterIdx = ((index % len) + len) % len;
  const ch = STARTER_CHANNELS[activeStarterIdx];
  document.querySelectorAll(".chip").forEach((el) => el.classList.toggle("active", el.dataset.slug === ch.slug));
  tuneUrl(ch.url, { ch: ch.num, callsign: ch.callsign, title: ch.name });
}

function tunePattern(key, label) {
  stopCurrentStream();
  activeStarterIdx = -1;
  currentSource = { type: "pattern", key, url: "", title: label, ch: 0, callsign: "TEST" };
  victr.switch_channel_effect();
  victr.set_teletext(false, false, 0, "", "", "", "");
  victr.set_pattern(key);
  victr.show_osd_action(label.toUpperCase());
  document.querySelectorAll(".chip").forEach((el) => {
    const match = (key === "demo_scene" && el.dataset.slug === "demo") || (key === "color_bars" && el.dataset.slug === "bars");
    el.classList.toggle("active", match);
  });
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
    currentSource = { type: "mirror", url: "", title: "AV 1 · Live Mirror", ch: 0, callsign: "AV1" };
    victr.switch_channel_effect();
    victr.set_teletext(false, false, 0, "AV1", "Live Mirror", "", "");
    victr.show_osd_channel(0, "AV1", "LIVE CAPTURE");
    stream.getVideoTracks()[0]?.addEventListener("ended", () => {
      victr.show_osd_action("AV1 DISCONNECTED");
      tunePattern("demo_scene", "Demo Scene");
    });
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
    currentSource = { type: "file", url: "", title: file.name, ch: 0, callsign: "IMG" };
  } else if (file.type.startsWith("video/")) {
    const blobUrl = URL.createObjectURL(file);
    tuneUrl(blobUrl, { ch: 0, callsign: "VCR", title: file.name });
    currentSource = { type: "file", url: "", title: file.name, ch: 0, callsign: "VCR" };
  } else {
    showToast("Drop an MP4/WebM video or PNG/JPG/WebP image.");
  }
}

function buildShareUrl() {
  const s = JSON.parse(victr.get_settings_json());
  const out = new URL(window.location.origin + window.location.pathname);
  if (currentSource.type === "url" && currentSource.url) {
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
  $("btn-mute").textContent = s.muted ? "Unmute" : "Mute";
  $("btn-whack").classList.toggle("hidden", !s.chaos);
  $("gpu-badge").textContent = `${s.backend} · ${s.hdr_active ? "HDR" : "SDR"} · ${s.screen_inches.toFixed(1)}"`;
}

function applyUrlParamsOnBoot() {
  const params = new URLSearchParams(window.location.search);
  proxyBase = params.get("proxy") || "";

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

  // Support both ?url=... and clean shareable path slugs (/nhk, /dw, /demo, /bars)
  const slug = window.location.pathname.replace(/^\/+|\/+$/g, "").toLowerCase();
  const explicitUrl = params.get("url") || params.get("src");
  if (explicitUrl) {
    tuneUrl(explicitUrl, {
      ch: parseInt(params.get("ch") || "80", 10) || 80,
      callsign: params.get("tag") || "LIVE",
      title: params.get("title") || "",
    });
  } else if (slug === "nhk") {
    tuneStarter(0);
  } else if (slug === "dw") {
    tuneStarter(1);
  } else if (slug === "bars" || params.get("pattern") === "color_bars") {
    tunePattern("color_bars", "Color Bars");
  } else if (slug === "demo" || params.get("pattern") === "demo_scene") {
    tunePattern("demo_scene", "Demo Scene");
  } else if (params.has("pattern")) {
    tunePattern(params.get("pattern"), params.get("pattern"));
  } else {
    // Default landing experience: tune NHK World live right away so it feels like turning on a TV!
    tuneStarter(0);
  }
  syncUiFromEngine();
}

function wireControls() {
  // Populate presets dropdown
  const presets = JSON.parse(VictrWeb.preset_names_json());
  const selPreset = $("sel-preset");
  presets.forEach((name, idx) => {
    const opt = document.createElement("option");
    opt.value = String(idx);
    opt.textContent = name;
    selPreset.appendChild(opt);
  });

  // Unlock WebAudio on first user gesture
  const unlockOnce = () => ensureAudioGraph();
  window.addEventListener("pointerdown", unlockOnce, { once: true });
  window.addEventListener("keydown", unlockOnce, { once: true });
  soundPill.addEventListener("click", () => {
    ensureAudioGraph();
    victr?.show_osd_action("TV SOUND ON");
  });

  // Drawer toggle
  drawerTab.addEventListener("click", () => document.body.classList.toggle("drawer-open"));
  $("btn-drawer-close").addEventListener("click", () => document.body.classList.remove("drawer-open"));
  $("btn-remote-hdr").addEventListener("click", () => remoteEl.classList.toggle("hidden"));
  $("btn-remote-close").addEventListener("click", () => remoteEl.classList.add("hidden"));

  // Stream form & starter chips
  $("stream-form").addEventListener("submit", (e) => {
    e.preventDefault();
    ensureAudioGraph();
    tuneUrl($("stream-url").value);
  });

  document.querySelectorAll(".chip").forEach((el) => {
    el.addEventListener("click", () => {
      ensureAudioGraph();
      const s = el.dataset.slug;
      if (s === "nhk") tuneStarter(0);
      else if (s === "dw") tuneStarter(1);
      else if (s === "demo") tunePattern("demo_scene", "Demo Scene");
      else if (s === "bars") tunePattern("color_bars", "Color Bars");
    });
  });

  // Share link, Mirror Tab, File open
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

  // Receiver buttons
  $("btn-power").addEventListener("click", () => {
    victr.toggle_power();
    syncUiFromEngine();
  });
  $("btn-degauss").addEventListener("click", () => victr.degauss());
  $("btn-crop").addEventListener("click", () => {
    victr.toggle_crop_43();
    syncUiFromEngine();
  });
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

  // Preset & knobs
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

  $("btn-mute").addEventListener("click", () => {
    ensureAudioGraph();
    victr.toggle_mute();
    syncUiFromEngine();
  });
  $("sl-vol").addEventListener("input", (e) => {
    ensureAudioGraph();
    victr.set_volume(parseFloat(e.target.value));
    syncUiFromEngine();
  });

  // Clickable Remote buttons
  remoteEl.querySelectorAll("[data-act]").forEach((btn) => {
    btn.addEventListener("click", () => {
      ensureAudioGraph();
      switch (btn.dataset.act) {
        case "power": victr.toggle_power(); syncUiFromEngine(); break;
        case "mute": victr.toggle_mute(); syncUiFromEngine(); break;
        case "info": victr.show_osd_channel(currentSource.ch, currentSource.callsign, currentSource.title); break;
        case "ch-prev": tuneStarter(activeStarterIdx < 0 ? 0 : activeStarterIdx - 1); break;
        case "ch-next": tuneStarter(activeStarterIdx < 0 ? 0 : activeStarterIdx + 1); break;
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

  // Canvas zoom / pan / double-click reset
  let dragging = false;
  let lastX = 0;
  let lastY = 0;
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.12 : 1 / 1.12;
    victr.zoom_by(factor);
  }, { passive: false });
  canvas.addEventListener("pointerdown", (e) => {
    dragging = true;
    lastX = e.clientX;
    lastY = e.clientY;
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    victr.pan_by((e.clientX - lastX) * dpr, (e.clientY - lastY) * dpr);
    lastX = e.clientX;
    lastY = e.clientY;
  });
  canvas.addEventListener("pointerup", () => { dragging = false; });
  canvas.addEventListener("dblclick", () => victr.reset_view());

  // Drag & drop files anywhere on window
  const dropEl = $("drop-overlay");
  let dragDepth = 0;
  window.addEventListener("dragenter", (e) => {
    e.preventDefault();
    dragDepth++;
    dropEl.classList.remove("hidden");
  });
  window.addEventListener("dragover", (e) => e.preventDefault());
  window.addEventListener("dragleave", () => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) dropEl.classList.add("hidden");
  });
  window.addEventListener("drop", (e) => {
    e.preventDefault();
    dragDepth = 0;
    dropEl.classList.add("hidden");
    const file = e.dataTransfer?.files?.[0];
    if (file) {
      ensureAudioGraph();
      loadLocalFile(file);
    }
  });

  // Keyboard remote shortcuts
  window.addEventListener("keydown", (e) => {
    const tag = document.activeElement?.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;

    if (e.key === "Escape") {
      remoteEl.classList.add("hidden");
      document.body.classList.remove("drawer-open");
      return;
    }
    if (e.key === "?") {
      e.preventDefault();
      remoteEl.classList.toggle("hidden");
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key === "1") {
      e.preventDefault();
      victr.snap_pixel_perfect();
      syncUiFromEngine();
      return;
    }
    if (e.shiftKey && (e.key === "Z" || e.key === "z")) {
      e.preventDefault();
      victr.toggle_crop_43();
      syncUiFromEngine();
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey) return;

    switch (e.key) {
      case "m": case "M": victr.toggle_mute(); syncUiFromEngine(); break;
      case "-": victr.adjust_volume(-0.05); syncUiFromEngine(); break;
      case "=": case "+": victr.adjust_volume(0.05); syncUiFromEngine(); break;
      case "[": tuneStarter(activeStarterIdx < 0 ? 0 : activeStarterIdx - 1); break;
      case "]": tuneStarter(activeStarterIdx < 0 ? 0 : activeStarterIdx + 1); break;
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
  applyUrlParamsOnBoot();

  let lastTs = performance.now();
  function frame(now) {
    const dt = (now - lastTs) / 1000;
    lastTs = now;
    if (video.readyState >= 2 && video.videoWidth > 0 && video.videoHeight > 0 && !video.ended) {
      victr.upload_video_frame(video);
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
