//! Platform-agnostic core of **ViCTR (Virtual CRT Television Receiver)**.
//!
//! Contains the physically based CRT raster/NTSC/phosphor simulation, WebGPU/wgpu
//! render pipeline and WGSL shaders, analog TV/VCR/RF audio DSP, 90s Prevue Cable
//! Guide renderer, TV OSD pixel canvas, settings/presets, and provider-agnostic
//! channel domain model.
//!
//! Intentionally free of desktop windowing (`winit`), UI (`egui`), subprocesses,
//! dynamic library loading, and blocking HTTP so it can be shared cleanly between
//! `victr-desktop` and future targets such as `victr-web` (WASM + WebGPU).

pub mod audio_dsp;
pub mod gpu;
pub mod guide;
pub mod osd;
pub mod provider;
pub mod settings;
pub mod shaders;
pub mod sim;
pub mod splash;
pub mod teletext;
