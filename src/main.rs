#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod app;
mod audio;
mod capture;
mod channels_file;
mod custom_provider;
mod folders;
mod gpu;
mod guide;
mod jellyfin;
mod link;
mod m3u;
mod mpv;
mod osd;
mod plex;
mod provider;
mod retro;
mod settings;
mod shaders;
mod sim;
mod splash;
mod web_channel;

use crate::app::CrtApp;
use crate::gpu::Gpu;
use std::path::PathBuf;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Fullscreen, Icon, Window, WindowId};

struct CliArgs {
    screenshot: Option<PathBuf>,
    preset: Option<usize>,
    zoom: Option<f32>,
    pattern: Option<usize>,
    mpv_src: Option<(String, f64)>,
    fullscreen: bool,
    sidebar_open: bool,
    pixel_perfect: bool,
    open_guide: bool,
    degauss_at: Option<f32>,
}

fn parse_args() -> CliArgs {
    let args: Vec<String> = std::env::args().collect();
    let mut screenshot: Option<PathBuf> = None;
    let mut preset: Option<usize> = None;
    let mut zoom: Option<f32> = None;
    let mut pattern: Option<usize> = None;
    let mut mpv_src: Option<String> = None;
    let mut start: f64 = 0.0;
    let fullscreen = args.iter().any(|a| a == "--fullscreen");
    let sidebar_open = args.iter().any(|a| a == "--sidebar");
    let pixel_perfect = args.iter().any(|a| a == "--pixel-perfect");
    let open_guide = args.iter().any(|a| a == "--guide");
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
        } else if args[i] == "--fullscreen" || args[i] == "--sidebar" || args[i] == "--pixel-perfect" || args[i] == "--guide" {
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
    CliArgs {
        screenshot,
        preset,
        zoom,
        pattern,
        mpv_src: mpv_src.map(|s| (s, start)),
        fullscreen,
        sidebar_open,
        pixel_perfect,
        open_guide,
        degauss_at,
    }
}

struct Runtime {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    gpu: Gpu,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    app: CrtApp,
    needs_reconfigure: bool,
}

struct Host {
    cli: Option<CliArgs>,
    rt: Option<Runtime>,
}

impl Runtime {
    fn init(event_loop: &ActiveEventLoop, cli: CliArgs) -> Result<Self, String> {
        let mut attrs = Window::default_attributes()
            .with_title("ViCTR — Virtual CRT Television Receiver")
            .with_inner_size(LogicalSize::new(1440.0, 900.0))
            .with_min_inner_size(LogicalSize::new(900.0, 600.0));
        if let Ok(icon) = Icon::from_rgba(about::icon_rgba(64), 64, 64) {
            attrs = attrs.with_window_icon(Some(icon));
        }
        if cli.screenshot.is_some() {
            attrs = attrs.with_active(false);
            #[cfg(target_os = "windows")]
            {
                attrs = attrs.with_window_level(winit::window::WindowLevel::AlwaysOnBottom);
            }
        }
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            attrs = WindowAttributesExtWayland::with_name(attrs, "victr", "victr");
            attrs = WindowAttributesExtX11::with_name(attrs, "victr", "victr");
        }

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("failed to create window: {e}"))?,
        );

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("failed to create wgpu surface: {e}"))?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: true,
                ..Default::default()
            }))
        })
        .map_err(|e| format!("no compatible GPU adapter found: {e}"))?;

        let limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("victr_device"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .map_err(|e| format!("failed to create GPU device: {e}"))?;

        let egui_ctx = egui::Context::default();
        let shot = cli.screenshot.is_some();
        let mut app = CrtApp::new(&egui_ctx, cli.screenshot, cli.preset, cli.zoom, cli.pattern, cli.mpv_src);
        if cli.sidebar_open {
            app.set_sidebar_locked(true);
        }
        if cli.pixel_perfect {
            app.request_pixel_perfect();
        }
        if cli.open_guide {
            app.request_open_guide();
        }
        if let Some(t) = cli.degauss_at {
            app.set_degauss_time(t);
        }
        if cli.fullscreen {
            app.start_fullscreen(!shot);
        }

        let size = window.inner_size();
        let monitor_hz = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            .map(|mh| mh as f32 / 1000.0);
        app.set_monitor_hz(monitor_hz);
        let gpu = Gpu::new(
            &device,
            &queue,
            &adapter,
            &surface,
            size.width.max(1),
            size.height.max(1),
            app.want_hdr(),
            app.prefer_pq(),
        );
        app.set_hdr_status(gpu.hdr_status());

        let max_tex_side = device.limits().max_texture_dimension_2d as usize;
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            Some(winit::window::Theme::Dark),
            Some(max_tex_side),
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            wgpu::TextureFormat::Rgba8Unorm,
            egui_wgpu::RendererOptions::default(),
        );

        Ok(Runtime {
            window,
            surface,
            adapter,
            device,
            queue,
            gpu,
            egui_ctx,
            egui_state,
            egui_renderer,
            app,
            needs_reconfigure: false,
        })
    }

    fn reconfigure(&mut self, width: u32, height: u32, want_hdr: bool, prefer_pq: bool) {
        if width == 0 || height == 0 {
            return;
        }
        self.gpu
            .reconfigure(&self.device, &self.adapter, &self.surface, width, height, want_hdr, prefer_pq);
        self.app.set_hdr_status(self.gpu.hdr_status());
        self.needs_reconfigure = false;
    }

    fn render(&mut self, event_loop: &ActiveEventLoop) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }

        // Reconfigure swapchain before acquiring if size or HDR mode changed.
        let (cur_w, cur_h) = self.gpu.surface_size();
        let want_hdr = self.app.want_hdr();
        let prefer_pq = self.app.prefer_pq();
        if self.needs_reconfigure
            || cur_w != size.width
            || cur_h != size.height
            || want_hdr != self.gpu.want_hdr()
            || prefer_pq != self.gpu.prefer_pq()
        {
            self.reconfigure(size.width, size.height, want_hdr, prefer_pq);
        }

        // Acquire the swapchain image FIRST (paces strictly to VSync) before stepping
        // the CRT rolling-scan clock, so real_dt is exact VSync-to-VSync and no slice is ever dropped.
        let surface_tex = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex) => tex,
            wgpu::CurrentSurfaceTexture::Suboptimal(tex) => {
                self.needs_reconfigure = true;
                tex
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.needs_reconfigure = true;
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => {
                self.window.request_redraw();
                return;
            }
        };

        self.app.sync_window_fullscreen(self.window.fullscreen().is_some());

        let raw_input = self.egui_state.take_egui_input(&self.window);
        let mut frame_out = None;
        let mut full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            frame_out = Some(self.app.run_ui(ui));
        });
        let Some(out) = frame_out else {
            full_output.textures_delta.clear();
            drop(surface_tex);
            return;
        };

        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);

        if let Some(fs) = out.fullscreen_cmd {
            self.window
                .set_fullscreen(if fs { Some(Fullscreen::Borderless(None)) } else { None });
        }
        if out.focus_cmd {
            self.window.focus_window();
        }
        if out.close_cmd {
            full_output.textures_delta.clear();
            drop(surface_tex);
            event_loop.exit();
            return;
        }
        if out.want_hdr != self.gpu.want_hdr() || out.prefer_pq != self.gpu.prefer_pq() {
            self.needs_reconfigure = true;
        }

        let ppp = full_output.pixels_per_point;
        let paint_jobs = self.egui_ctx.tessellate(full_output.shapes, ppp);
        for (id, deltas) in full_output.textures_delta.set.drain() {
            for delta in &deltas {
                self.egui_renderer.update_texture(&self.device, &self.queue, id, delta);
            }
        }

        let screen_desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [size.width, size.height],
            pixels_per_point: ppp,
        };

        // Compute CRT viewport rect in physical pixels (top-left origin).
        let crt_rect_px = [
            (out.crt_rect.left() * ppp).round().max(0.0),
            (out.crt_rect.top() * ppp).round().max(0.0),
            (out.crt_rect.width() * ppp).round().max(4.0),
            (out.crt_rect.height() * ppp).round().max(4.0),
        ];

        let surface_view = surface_tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("victr_frame_encoder"),
        });

        // 1. Run full linear HDR CRT physics pipeline
        self.gpu
            .render_crt(&self.device, &self.queue, &mut encoder, &out.fi, crt_rect_px);

        // 2. Render egui UI overlay into offscreen transparent Rgba8Unorm texture
        let extra_cmds = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_desc,
        );
        {
            let mut rp = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui_ui_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: self.gpu.ui_view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut rp, &paint_jobs, &screen_desc);
        }

        // 3. Present linear HDR CRT + UI overlay onto the native HDR/SDR swapchain
        self.gpu.present(&self.device, &mut encoder, &surface_view);

        self.queue
            .submit(extra_cmds.into_iter().chain(std::iter::once(encoder.finish())));
        // Notify winit right before present so Wayland attaches a vblank wl_surface::frame callback
        self.window.pre_present_notify();
        self.queue.present(surface_tex);

        for id in full_output.textures_delta.free.drain() {
            self.egui_renderer.free_texture(&id);
        }

        // 4. Headless / CLI screenshot capture (clean SDR PNG readback)
        if let Some(path) = out.take_screenshot {
            match self.gpu.capture_sdr_rgba8(&self.device, &self.queue) {
                Ok((w, h, rgba)) => {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    match image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8) {
                        Ok(()) => eprintln!("saved screenshot to {} ({:.0} fps)", path.display(), self.app.fps()),
                        Err(e) => eprintln!("could not save screenshot: {e}"),
                    }
                }
                Err(e) => eprintln!("screenshot readback failed: {e}"),
            }
            event_loop.exit();
            return;
        }

        // Schedule exactly one next frame paced to the compositor's vblank
        self.window.request_redraw();
    }
}

impl ApplicationHandler for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.rt.is_some() {
            return;
        }
        let Some(cli) = self.cli.take() else { return };
        match Runtime::init(event_loop, cli) {
            Ok(rt) => {
                rt.window.request_redraw();
                self.rt = Some(rt);
            }
            Err(e) => {
                eprintln!("ViCTR failed to start: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(rt) = &mut self.rt else { return };
        if rt.window.id() != window_id {
            return;
        }
        let resp = rt.egui_state.on_window_event(&rt.window, &event);
        if resp.repaint {
            rt.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => {
                // Drop Runtime (and CrtApp) explicitly so Jellyfin watch progress flushes before exit.
                self.rt = None;
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if new_size.width > 0 && new_size.height > 0 {
                    rt.reconfigure(new_size.width, new_size.height, rt.app.want_hdr(), rt.app.prefer_pq());
                    rt.window.request_redraw();
                }
            }
            WindowEvent::Moved(_) => {
                rt.gpu.refresh_hdr_info(&rt.surface, &rt.adapter);
                rt.app.set_hdr_status(rt.gpu.hdr_status());
                let hz = rt
                    .window
                    .current_monitor()
                    .and_then(|m| m.refresh_rate_millihertz())
                    .map(|mh| mh as f32 / 1000.0);
                rt.app.set_monitor_hz(hz);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let sz = rt.window.inner_size();
                if sz.width > 0 && sz.height > 0 {
                    rt.reconfigure(sz.width, sz.height, rt.app.want_hdr(), rt.app.prefer_pq());
                }
                let hz = rt
                    .window
                    .current_monitor()
                    .and_then(|m| m.refresh_rate_millihertz())
                    .map(|mh| mh as f32 / 1000.0);
                rt.app.set_monitor_hz(hz);
            }
            WindowEvent::RedrawRequested => {
                rt.render(event_loop);
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.rt = None;
    }
}

fn main() {
    let cli = parse_args();

    // Launched from the mpv shortcut while the TV is already open: hand it over.
    if cli.screenshot.is_none() {
        if let Some((src, start)) = &cli.mpv_src {
            if link::forward_to_running(src, *start) {
                return;
            }
        }
    }

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut host = Host { cli: Some(cli), rt: None };
    if let Err(e) = event_loop.run_app(&mut host) {
        eprintln!("event loop error: {e}");
    }
}
