mod about;
mod app;
mod audio;
mod gpu;
mod link;
mod mpv;
mod settings;
mod shaders;
mod sim;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut screenshot: Option<PathBuf> = None;
    let mut preset: Option<usize> = None;
    let mut zoom: Option<f32> = None;
    let mut pattern: Option<usize> = None;
    let mut mpv_src: Option<String> = None;
    let mut start: f64 = 0.0;
    let fullscreen = args.iter().any(|a| a == "--fullscreen");
    let sidebar_open = args.iter().any(|a| a == "--sidebar");
    let degauss_at: Option<f32> = args
        .windows(2)
        .find(|w| w[0] == "--degauss")
        .and_then(|w| w[1].parse().ok());
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--screenshot" && i + 1 < args.len() {
            screenshot = Some(PathBuf::from(&args[i + 1]));
            i += 1;
        } else if args[i] == "--preset" && i + 1 < args.len() {
            preset = args[i + 1].parse().ok();
            i += 1;
        } else if args[i] == "--fullscreen" || args[i] == "--sidebar" {
            // handled above
        } else if args[i] == "--degauss" && i + 1 < args.len() {
            i += 1;
        } else if args[i] == "--start" && i + 1 < args.len() {
            start = args[i + 1].parse().unwrap_or(0.0);
            i += 1;
        } else if args[i] == "--mpv" && i + 1 < args.len() {
            mpv_src = Some(args[i + 1].clone());
            i += 1;
        } else if args[i] == "--pattern" && i + 1 < args.len() {
            pattern = args[i + 1].parse().ok();
            i += 1;
        } else if args[i] == "--zoom" && i + 1 < args.len() {
            zoom = args[i + 1].parse().ok();
            i += 1;
        } else if !args[i].starts_with("--") && mpv_src.is_none() {
            // a file handed over by the apps menu or a file manager ("Open with")
            mpv_src = Some(args[i].clone());
        } else if let Some(p) = args[i].strip_prefix("--screenshot=") {
            screenshot = Some(PathBuf::from(p));
        }
        i += 1;
    }

    // Launched from the mpv shortcut while the TV is already open: hand it over.
    if screenshot.is_none() {
        if let Some(src) = &mpv_src {
            if link::forward_to_running(src, start) {
                return Ok(());
            }
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Phosphor Lab — CRT physics simulator")
        .with_inner_size([1440.0, 900.0])
        .with_min_inner_size([900.0, 600.0])
        .with_app_id("phosphor-lab")
        .with_icon(egui::IconData { rgba: about::icon_rgba(64), width: 64, height: 64 });
    if screenshot.is_some() {
        viewport = viewport.with_active(false);
        #[cfg(target_os = "windows")]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }

    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    if screenshot.is_some() {
        options.event_loop_builder = Some(Box::new(|b| {
            use eframe::egui_winit::winit::platform::macos::EventLoopBuilderExtMacOS;
            b.with_activate_ignoring_other_apps(false);
        }));
    }

    eframe::run_native(
        "Phosphor Lab",
        options,
        Box::new(move |cc| {
            let shot = screenshot.is_some();
            let mut a = app::CrtApp::new(cc, screenshot, preset, zoom, pattern, mpv_src.map(|s| (s, start)));
            if sidebar_open {
                a.set_sidebar_locked(true);
            }
            if let Some(t) = degauss_at {
                a.set_degauss_time(t);
            }
            if fullscreen {
                // screenshot runs only hide the settings; they never take over the screen
                a.start_fullscreen(&cc.egui_ctx, !shot);
            }
            Ok(Box::new(a))
        }),
    )
}
