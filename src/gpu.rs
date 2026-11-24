//! Native wgpu CRT physics pipeline & HDR swapchain presenter:
//! source -> baseband -> channel -> decode -> apl -> burn-in -> beam/mask ->
//! phosphor persistence / rolling scan -> eye integration -> glass blur ->
//! linear HDR CRT composite -> native HDR/SDR + egui UI presentation.

use crate::shaders;
use bytemuck::{Pod, Zeroable};

pub const SW: i32 = 910;
pub const NL: i32 = 263;
const BURN_W: u32 = 512;
const BURN_H: u32 = 384;

/// Everything one frame of rendering needs, computed on the CPU.
#[derive(Clone, Default)]
pub struct FrameInput {
    pub new_field: bool,
    pub lines: Vec<f32>, // NL * 2 * 4
    pub u: Uniforms,
    pub image: Option<(u64, std::sync::Arc<(usize, usize, Vec<u8>)>)>,
    /// Optional 640x480 RGBA8 post-decoder CRT TV OSD (green/white channel & volume overlay).
    pub osd: Option<(u64, std::sync::Arc<Vec<u8>>)>,
    pub clear_burn: bool,
    pub burn_now: bool,
    pub reset_trails: bool,
}

#[derive(Clone, Default)]
pub struct Uniforms {
    pub time: f32,
    pub frame: u32,
    pub pattern: i32,
    pub flat: [f32; 3],
    pub src_w: i32,
    pub src_h: i32,
    pub field: i32,
    pub interlaced: i32,
    pub field_phase: f32,
    // baseband
    pub vcr: i32,
    pub luma_sigma: f32,
    pub i_sigma: f32,
    pub q_sigma: f32,
    pub chroma_delay: f32,
    pub vcr_sharp: f32,
    pub tape_noise: f32,
    pub chroma_noise: f32,
    pub dropout: f32,
    // channel
    pub input: i32,
    pub snow: f32,
    pub noise: f32,
    pub ghost_amp: f32,
    pub ghost_delay: f32,
    pub interf_amp: f32,
    pub interf_freq: f32,
    pub hum: f32,
    pub impulse: f32,
    pub fine_tune: f32,
    // decode
    pub comb: i32,
    pub contrast: f32,
    pub brightness: f32,
    pub color: f32,
    pub tint: f32,
    pub sharp: f32,
    pub killer: f32,
    pub rx_chroma_sigma: f32,
    // view
    pub view: [f32; 3],
    pub curv: f32,
    pub corner: f32,
    // beam
    pub hsize: f32,
    pub vsize: f32,
    pub hpos: f32,
    pub vpos: f32,
    pub pin: f32,
    pub breath: f32,
    pub vphase: f32,
    pub field_shift: f32,
    pub overscan: f32,
    pub ripple: f32,
    pub conv_r: [f32; 2],
    pub conv_b: [f32; 2],
    pub conv_dyn: f32,
    pub spot: f32,
    pub spot_h: f32,
    pub bloom: f32,
    pub gamma: f32,
    pub focus: [f32; 3],
    pub emission: [f32; 3],
    pub cutoff: [f32; 3],
    pub imax: [f32; 3],
    pub mask: i32,
    pub pitch: f32,
    pub bm: f32,
    pub mask_strength: f32,
    pub anti_moire: f32,
    pub wire: f32,
    pub purity: f32,
    pub purity_seed: [f32; 2],
    pub degauss: [f32; 3],
    pub burn_amt: f32,
    pub burn_rate: f32,
    pub sat: f32,
    pub collapse: [f32; 4],
    pub abl: f32,
    // persistence
    pub decay: [f32; 3],
    pub frac: [f32; 3],
    pub eye_k: f32,
    // rolling scan
    pub scan: i32,
    pub scan_phase: f32,
    pub scan_w: f32,
    pub tau_fast: [f32; 3],
    pub tau_slow: [f32; 3],
    pub scan_a: f32,
    pub eye_k_frame: f32,
    // glass
    pub phos: [f32; 9],
    pub diffuse: f32,
    pub halation: f32,
    pub diffuse_sigma_px: f32,
    pub halation_sigma_px: f32,
    pub ambient: f32,
    pub reflect: f32,
    pub vignette: f32,
    pub exposure: f32,
    pub glass: [f32; 3],
    pub bezel: i32,
    pub hdr_paper_white: f32,
    pub hdr_peak_nits: f32,
    pub hdr_ui_nits: f32,
}

/// Active output swapchain encoding mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceMode {
    /// 8-bit SDR unorm swapchain (gamma-encoded).
    SdrUnorm,
    /// 8-bit SDR sRGB swapchain (linear-encoded, hardware OETF).
    SdrSrgb,
    /// Native HDR scRGB (Rgba16Float, ExtendedSrgbLinear, 1.0 = 80 nits) on DX12 / Vulkan.
    ScRgb,
    /// Native HDR Metal EDR (Rgba16Float, ExtendedSrgbLinear, 1.0 = SDR white) on macOS.
    MetalEdr,
    /// Native HDR10 PQ (Rgb10a2Unorm, Bt2100Pq, 10-bit ST.2084 PQ) on Vulkan / DX12.
    Hdr10Pq,
}

impl SurfaceMode {
    pub fn is_hdr(self) -> bool {
        matches!(self, SurfaceMode::ScRgb | SurfaceMode::MetalEdr | SurfaceMode::Hdr10Pq)
    }

    pub fn shader_mode(self) -> f32 {
        match self {
            SurfaceMode::SdrUnorm => 0.0,
            SurfaceMode::SdrSrgb => 1.0,
            SurfaceMode::ScRgb => 2.0,
            SurfaceMode::MetalEdr => 3.0,
            SurfaceMode::Hdr10Pq => 4.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SurfaceMode::SdrUnorm => "SDR (8-bit)",
            SurfaceMode::SdrSrgb => "SDR (8-bit sRGB)",
            SurfaceMode::ScRgb => "HDR scRGB (16-bit float)",
            SurfaceMode::MetalEdr => "HDR Metal EDR (16-bit float)",
            SurfaceMode::Hdr10Pq => "HDR10 PQ (10-bit BT.2020)",
        }
    }
}

/// Live swapchain & display HDR telemetry shown in the UI.
#[derive(Clone, Debug)]
pub struct HdrStatus {
    pub backend: String,
    pub adapter_name: String,
    pub supported: bool,
    pub has_scrgb: bool,
    pub has_pq: bool,
    pub active_mode: SurfaceMode,
    pub reported_peak_nits: Option<f32>,
    pub reported_sdr_nits: Option<f32>,
    pub reported_edr_headroom: Option<f32>,
}

impl Default for HdrStatus {
    fn default() -> Self {
        Self {
            backend: "wgpu".into(),
            adapter_name: String::new(),
            supported: false,
            has_scrgb: false,
            has_pq: false,
            active_mode: SurfaceMode::SdrUnorm,
            reported_peak_nits: None,
            reported_sdr_nits: None,
            reported_edr_headroom: None,
        }
    }
}

impl HdrStatus {
    pub fn summary(&self) -> String {
        let mut s = format!("{} · {}", self.active_mode.label(), self.backend);
        if let Some(peak) = self.reported_peak_nits {
            s.push_str(&format!(" · display peak ~{peak:.0} nits"));
        } else if let Some(hr) = self.reported_edr_headroom {
            s.push_str(&format!(" · EDR headroom {hr:.1}×"));
        }
        s
    }
}

/// Packed 16-byte-aligned uniform block matching `struct Uniforms` in `shaders.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct GpuUniforms {
    p0: [f32; 4],
    u0: [u32; 4],
    i0: [i32; 4],
    i1: [i32; 4],
    flat_reset: [f32; 4],
    bb0: [f32; 4],
    bb1: [f32; 4],
    ch0: [f32; 4],
    ch1: [f32; 4],
    dec0: [f32; 4],
    dec1: [f32; 4],
    view_curv: [f32; 4],
    vp0: [f32; 4],
    geom0: [f32; 4],
    geom1: [f32; 4],
    geom2: [f32; 4],
    conv: [f32; 4],
    tube0: [f32; 4],
    tube1: [f32; 4],
    tube2: [f32; 4],
    degauss_abl: [f32; 4],
    focus: [f32; 4],
    emission: [f32; 4],
    cutoff: [f32; 4],
    imax: [f32; 4],
    collapse: [f32; 4],
    pers0: [f32; 4],
    pers1: [f32; 4],
    scan0: [f32; 4],
    scan1: [f32; 4],
    scan2: [f32; 4],
    phos0: [f32; 4],
    phos1: [f32; 4],
    phos2: [f32; 4],
    glass0: [f32; 4],
    glass1: [f32; 4],
    glass2: [f32; 4],
    hdr0: [f32; 4],
    crt_rect: [f32; 4],
    blur_p: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<GpuUniforms>() == 40 * 16);

const UNIFORM_SLOTS: usize = 6; // 0: main, 1..=4: 4 glass blur passes, 5: SDR screenshot present

fn align_up(val: usize, align: usize) -> usize {
    let a = align.max(1);
    (val + a - 1) & !(a - 1)
}

struct Target {
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

struct ViewTargets {
    w: u32,
    h: u32,
    e: [Target; 2],
    e_cur: usize,
    sc: Target,
    tail: [Target; 2],
    eye: [Target; 2],
    q: [Target; 3],
    crt_out: Target,
    cur: usize,
}

pub struct Gpu {
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    tex_bgl: wgpu::BindGroupLayout,
    global_bg: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    uniform_stride: usize,
    uniform_staging: Vec<u8>,

    p_source: wgpu::RenderPipeline,
    p_base: wgpu::RenderPipeline,
    p_chan: wgpu::RenderPipeline,
    p_decode: wgpu::RenderPipeline,
    p_apl: wgpu::RenderPipeline,
    p_burn: wgpu::RenderPipeline,
    p_beam: wgpu::RenderPipeline,
    p_tail: wgpu::RenderPipeline,
    p_scan: wgpu::RenderPipeline,
    p_eye: wgpu::RenderPipeline,
    p_down: wgpu::RenderPipeline,
    p_blur: wgpu::RenderPipeline,
    p_comp: wgpu::RenderPipeline,
    p_present: wgpu::RenderPipeline,
    p_present_format: wgpu::TextureFormat,
    p_present_sdr_rgba8: wgpu::RenderPipeline,

    dummy_view: wgpu::TextureView,
    lines_tex: wgpu::Texture,
    osd_tex: wgpu::Texture,
    osd_id: u64,
    img_tex: wgpu::Texture,
    img_view: wgpu::TextureView,
    img_id: u64,
    img_aspect: f32,
    has_img: bool,

    src: Option<Target>,
    base: Target,
    chan: Target,
    drive: Target,
    apl: Target,
    burn: [Target; 2],
    burn_cur: usize,
    view: Option<ViewTargets>,

    ui_tex: Target,
    surface_config: wgpu::SurfaceConfiguration,
    surface_mode: SurfaceMode,
    hdr_supported: bool,
    has_scrgb: bool,
    has_pq: bool,
    want_hdr: bool,
    prefer_pq: bool,
    backend_name: String,
    adapter_name: String,
    reported_peak_nits: Option<f32>,
    reported_sdr_nits: Option<f32>,
    reported_edr_headroom: Option<f32>,
}

fn make_target(device: &wgpu::Device, label: &str, w: u32, h: u32, format: wgpu::TextureFormat) -> Target {
    let w = w.max(1);
    let h = h.max(1);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    Target { _tex: tex, view, w, h }
}

fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    entry: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(entry),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn choose_surface_config(
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
    width: u32,
    height: u32,
    want_hdr: bool,
    prefer_pq: bool,
) -> (wgpu::SurfaceConfiguration, SurfaceMode, bool, bool) {
    let caps = surface.get_capabilities(adapter);
    let backend = adapter.get_info().backend;
    let is_metal = backend == wgpu::Backend::Metal;

    let has_scrgb = caps
        .color_spaces(wgpu::TextureFormat::Rgba16Float)
        .contains(wgpu::SurfaceColorSpaces::EXTENDED_SRGB_LINEAR);
    let has_pq = caps
        .color_spaces(wgpu::TextureFormat::Rgb10a2Unorm)
        .contains(wgpu::SurfaceColorSpaces::BT2100_PQ);

    let (format, color_space, mode) = if want_hdr && prefer_pq && has_pq {
        (wgpu::TextureFormat::Rgb10a2Unorm, wgpu::SurfaceColorSpace::Bt2100Pq, SurfaceMode::Hdr10Pq)
    } else if want_hdr && has_scrgb {
        let mode = if is_metal { SurfaceMode::MetalEdr } else { SurfaceMode::ScRgb };
        (wgpu::TextureFormat::Rgba16Float, wgpu::SurfaceColorSpace::ExtendedSrgbLinear, mode)
    } else if want_hdr && has_pq {
        (wgpu::TextureFormat::Rgb10a2Unorm, wgpu::SurfaceColorSpace::Bt2100Pq, SurfaceMode::Hdr10Pq)
    } else {
        // Prefer non-sRGB unorm so our own 2.2 gamma + dither + egui compositing is direct,
        // falling back cleanly to whatever SDR format the surface offers.
        let sdr_fmt = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .or_else(|| caps.formats.first().copied())
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm);
        let mode = if sdr_fmt.is_srgb() { SurfaceMode::SdrSrgb } else { SurfaceMode::SdrUnorm };
        (sdr_fmt, wgpu::SurfaceColorSpace::Auto, mode)
    };

    let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
        wgpu::PresentMode::Fifo
    } else {
        caps.present_modes.first().copied().unwrap_or(wgpu::PresentMode::Fifo)
    };
    let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        wgpu::CompositeAlphaMode::Opaque
    } else {
        caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto)
    };

    let cfg = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space,
        width: width.max(1),
        height: height.max(1),
        present_mode,
        desired_maximum_frame_latency: 1,
        alpha_mode,
        view_formats: vec![],
    };
    (cfg, mode, has_scrgb, has_pq)
}

impl Gpu {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        adapter: &wgpu::Adapter,
        surface: &wgpu::Surface<'_>,
        width: u32,
        height: u32,
        want_hdr: bool,
        prefer_pq: bool,
    ) -> Self {
        let info = adapter.get_info();
        let backend_name = match info.backend {
            wgpu::Backend::Vulkan => "Vulkan",
            wgpu::Backend::Dx12 => "DirectX 12",
            wgpu::Backend::Metal => "Metal",
            wgpu::Backend::Gl => "OpenGL ES",
            wgpu::Backend::BrowserWebGpu => "WebGPU",
            wgpu::Backend::Noop => "Noop",
        }
        .to_string();

        let (surface_config, surface_mode, has_scrgb, has_pq) =
            choose_surface_config(surface, adapter, width, height, want_hdr, prefer_pq);
        let hdr_supported = has_scrgb || has_pq;
        surface.configure(device, &surface_config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("victr_crt_shaders"),
            source: wgpu::ShaderSource::Wgsl(shaders::WGSL.into()),
        });

        let align = device.limits().min_uniform_buffer_offset_alignment as usize;
        let uniform_stride = align_up(std::mem::size_of::<GpuUniforms>(), align);
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crt_uniforms"),
            size: (uniform_stride * UNIFORM_SLOTS) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let s_linear = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("s_linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let s_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("s_nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let s_repeat_x = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("s_repeat_x"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let lines_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lines_tex"),
            size: wgpu::Extent3d { width: NL as u32, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lines_view = lines_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let osd_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("osd_tex"),
            size: wgpu::Extent3d {
                width: crate::osd::OSD_W as u32,
                height: crate::osd::OSD_H as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let osd_view = osd_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let global_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt_global_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<GpuUniforms>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crt_tex_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let global_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crt_global_bg"),
            layout: &global_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniform_buf,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<GpuUniforms>() as u64),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&s_linear) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&s_nearest) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&s_repeat_x) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&lines_view) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&osd_view) },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("crt_pipeline_layout"),
            bind_group_layouts: &[Some(&global_bgl), Some(&tex_bgl)],
            immediate_size: 0,
        });

        let hdr_fmt = wgpu::TextureFormat::Rgba16Float;
        let p_source = make_pipeline(device, &pipeline_layout, &shader, "fs_source", hdr_fmt);
        let p_base = make_pipeline(device, &pipeline_layout, &shader, "fs_baseband", hdr_fmt);
        let p_chan = make_pipeline(device, &pipeline_layout, &shader, "fs_channel", hdr_fmt);
        let p_decode = make_pipeline(device, &pipeline_layout, &shader, "fs_decode", hdr_fmt);
        let p_apl = make_pipeline(device, &pipeline_layout, &shader, "fs_apl", hdr_fmt);
        let p_burn = make_pipeline(device, &pipeline_layout, &shader, "fs_burn", hdr_fmt);
        let p_beam = make_pipeline(device, &pipeline_layout, &shader, "fs_beam", hdr_fmt);
        let p_tail = make_pipeline(device, &pipeline_layout, &shader, "fs_tail", hdr_fmt);
        let p_scan = make_pipeline(device, &pipeline_layout, &shader, "fs_scan", hdr_fmt);
        let p_eye = make_pipeline(device, &pipeline_layout, &shader, "fs_eye", hdr_fmt);
        let p_down = make_pipeline(device, &pipeline_layout, &shader, "fs_down", hdr_fmt);
        let p_blur = make_pipeline(device, &pipeline_layout, &shader, "fs_blur", hdr_fmt);
        let p_comp = make_pipeline(device, &pipeline_layout, &shader, "fs_composite", hdr_fmt);
        let p_present_format = surface_config.format;
        let p_present = make_pipeline(device, &pipeline_layout, &shader, "fs_present", p_present_format);
        let p_present_sdr_rgba8 = make_pipeline(
            device,
            &pipeline_layout,
            &shader,
            "fs_present",
            wgpu::TextureFormat::Rgba8Unorm,
        );

        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dummy_1x1"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            dummy_tex.as_image_copy(),
            &[0, 0, 0, 255],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let dummy_view = dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let img_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("img_tex"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            img_tex.as_image_copy(),
            &[20, 20, 20, 255],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let img_view = img_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let base = make_target(device, "base", SW as u32, NL as u32, hdr_fmt);
        let chan = make_target(device, "chan", SW as u32, NL as u32, hdr_fmt);
        let drive = make_target(device, "drive", SW as u32, NL as u32, hdr_fmt);
        let apl = make_target(device, "apl", 1, 1, hdr_fmt);
        let burn = [
            make_target(device, "burn0", BURN_W, BURN_H, hdr_fmt),
            make_target(device, "burn1", BURN_W, BURN_H, hdr_fmt),
        ];
        let ui_tex = make_target(device, "egui_ui_tex", width.max(1), height.max(1), wgpu::TextureFormat::Rgba8Unorm);

        let mut gpu = Gpu {
            shader,
            pipeline_layout,
            tex_bgl,
            global_bg,
            uniform_buf,
            uniform_stride,
            uniform_staging: vec![0u8; uniform_stride * UNIFORM_SLOTS],
            p_source,
            p_base,
            p_chan,
            p_decode,
            p_apl,
            p_burn,
            p_beam,
            p_tail,
            p_scan,
            p_eye,
            p_down,
            p_blur,
            p_comp,
            p_present,
            p_present_format,
            p_present_sdr_rgba8,
            dummy_view,
            lines_tex,
            osd_tex,
            osd_id: 0,
            img_tex,
            img_view,
            img_id: 0,
            img_aspect: 4.0 / 3.0,
            has_img: false,
            src: None,
            base,
            chan,
            drive,
            apl,
            burn,
            burn_cur: 0,
            view: None,
            ui_tex,
            surface_config,
            surface_mode,
            hdr_supported,
            has_scrgb,
            has_pq,
            want_hdr,
            prefer_pq,
            backend_name,
            adapter_name: info.name,
            reported_peak_nits: None,
            reported_sdr_nits: None,
            reported_edr_headroom: None,
        };
        gpu.refresh_hdr_info(surface, adapter);
        gpu
    }

    pub fn refresh_hdr_info(&mut self, surface: &wgpu::Surface<'_>, adapter: &wgpu::Adapter) {
        let info = surface.display_hdr_info(adapter);
        self.reported_peak_nits = info.luminance.and_then(|l| l.max_nits).filter(|v| *v > 50.0);
        self.reported_sdr_nits = info.luminance.and_then(|l| l.sdr_white_nits).filter(|v| *v > 40.0);
        self.reported_edr_headroom = info.headroom.and_then(|h| h.current).filter(|v| *v > 1.01);
    }

    pub fn hdr_status(&self) -> HdrStatus {
        HdrStatus {
            backend: self.backend_name.clone(),
            adapter_name: self.adapter_name.clone(),
            supported: self.hdr_supported,
            has_scrgb: self.has_scrgb,
            has_pq: self.has_pq,
            active_mode: self.surface_mode,
            reported_peak_nits: self.reported_peak_nits,
            reported_sdr_nits: self.reported_sdr_nits,
            reported_edr_headroom: self.reported_edr_headroom,
        }
    }

    pub fn want_hdr(&self) -> bool {
        self.want_hdr
    }

    pub fn prefer_pq(&self) -> bool {
        self.prefer_pq
    }

    pub fn surface_size(&self) -> (u32, u32) {
        (self.surface_config.width, self.surface_config.height)
    }

    pub fn ui_view(&self) -> &wgpu::TextureView {
        &self.ui_tex.view
    }

    pub fn reconfigure(
        &mut self,
        device: &wgpu::Device,
        adapter: &wgpu::Adapter,
        surface: &wgpu::Surface<'_>,
        width: u32,
        height: u32,
        want_hdr: bool,
        prefer_pq: bool,
    ) {
        let w = width.max(1);
        let h = height.max(1);
        let (cfg, mode, has_scrgb, has_pq) = choose_surface_config(surface, adapter, w, h, want_hdr, prefer_pq);
        self.surface_config = cfg;
        self.surface_mode = mode;
        self.has_scrgb = has_scrgb;
        self.has_pq = has_pq;
        self.hdr_supported = has_scrgb || has_pq;
        self.want_hdr = want_hdr;
        self.prefer_pq = prefer_pq;
        surface.configure(device, &self.surface_config);

        if self.surface_config.format != self.p_present_format {
            self.p_present_format = self.surface_config.format;
            self.p_present = make_pipeline(
                device,
                &self.pipeline_layout,
                &self.shader,
                "fs_present",
                self.p_present_format,
            );
        }
        if self.ui_tex.w != w || self.ui_tex.h != h {
            self.ui_tex = make_target(device, "egui_ui_tex", w, h, wgpu::TextureFormat::Rgba8Unorm);
        }
        self.refresh_hdr_info(surface, adapter);
    }

    fn ensure_src(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        if let Some(s) = &self.src {
            if s.w == w && s.h == h {
                return;
            }
        }
        self.src = Some(make_target(device, "src", w, h, wgpu::TextureFormat::Rgba16Float));
    }

    fn ensure_view(&mut self, device: &wgpu::Device, w: u32, h: u32) {
        let w = w.max(4);
        let h = h.max(4);
        if let Some(v) = &self.view {
            if v.w == w && v.h == h {
                return;
            }
        }
        let qw = (w / 4).max(1);
        let qh = (h / 4).max(1);
        let fmt = wgpu::TextureFormat::Rgba16Float;
        self.view = Some(ViewTargets {
            w,
            h,
            e: [make_target(device, "e0", w, h, fmt), make_target(device, "e1", w, h, fmt)],
            e_cur: 0,
            sc: make_target(device, "sc", w, h, fmt),
            tail: [make_target(device, "tail0", w, h, fmt), make_target(device, "tail1", w, h, fmt)],
            eye: [make_target(device, "eye0", w, h, fmt), make_target(device, "eye1", w, h, fmt)],
            q: [
                make_target(device, "q0", qw, qh, fmt),
                make_target(device, "q1", qw, qh, fmt),
                make_target(device, "q2", qw, qh, fmt),
            ],
            crt_out: make_target(device, "crt_out", w, h, fmt),
            cur: 0,
        });
    }

    fn make_tex_bg(
        &self,
        device: &wgpu::Device,
        t0: &wgpu::TextureView,
        t1: &wgpu::TextureView,
        t2: &wgpu::TextureView,
        t3: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.tex_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(t0) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(t1) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(t2) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(t3) },
            ],
        })
    }

    fn run_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        tex_bg: &wgpu::BindGroup,
        slot: usize,
    ) {
        let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::DontCare(unsafe { wgpu::LoadOpDontCare::enabled() }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(pipeline);
        rp.set_bind_group(0, &self.global_bg, &[(slot * self.uniform_stride) as u32]);
        rp.set_bind_group(1, tex_bg, &[]);
        rp.draw(0..3, 0..1);
    }

    fn clear_target(encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let _rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear_target"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    fn write_slot(&mut self, slot: usize, u: &GpuUniforms) {
        let off = slot * self.uniform_stride;
        let bytes = bytemuck::bytes_of(u);
        self.uniform_staging[off..off + bytes.len()].copy_from_slice(bytes);
    }

    fn build_uniforms(&self, fi: &FrameInput, crt_rect_px: [f32; 4], q_px: [f32; 2]) -> GpuUniforms {
        let u = &fi.u;
        let vw = crt_rect_px[2].max(4.0);
        let vh = crt_rect_px[3].max(4.0);
        let tail_gain = if u.scan == 0 { 1.0 } else { 0.0 };
        let eye_k_eff = if fi.reset_trails {
            1.0
        } else if u.scan == 0 {
            u.eye_k
        } else {
            u.eye_k_frame
        };
        let burn_rate_eff = if fi.burn_now { 0.35 } else { u.burn_rate };
        let p = &u.phos;

        GpuUniforms {
            p0: [u.time, u.field_phase, self.img_aspect, burn_rate_eff],
            u0: [u.frame, u.pattern.max(0) as u32, u.src_w.max(1) as u32, u.src_h.max(1) as u32],
            i0: [u.field, u.interlaced, u.vcr, self.has_img as i32],
            i1: [u.input, u.comb, u.mask, u.bezel],
            flat_reset: [u.flat[0], u.flat[1], u.flat[2], if fi.reset_trails { 1.0 } else { 0.0 }],
            bb0: [u.luma_sigma, u.i_sigma, u.q_sigma, u.chroma_delay],
            bb1: [u.vcr_sharp, u.tape_noise, u.chroma_noise, u.dropout],
            ch0: [u.snow, u.noise, u.ghost_amp, u.ghost_delay],
            ch1: [u.interf_amp, u.interf_freq, u.hum, u.impulse],
            dec0: [u.fine_tune, u.contrast, u.brightness, u.color],
            dec1: [u.tint, u.sharp, u.killer, u.rx_chroma_sigma],
            view_curv: [u.view[0], u.view[1], u.view[2], u.curv],
            vp0: [vw, vh, u.corner, u.hsize],
            geom0: [u.vsize, u.hpos, u.vpos, u.pin],
            geom1: [u.breath, u.vphase, u.field_shift, u.overscan],
            geom2: [u.ripple, u.conv_dyn, u.spot, u.spot_h],
            conv: [u.conv_r[0], u.conv_r[1], u.conv_b[0], u.conv_b[1]],
            tube0: [u.bloom, u.gamma, u.pitch, u.bm],
            tube1: [u.mask_strength, u.anti_moire, u.wire, u.purity],
            tube2: [u.purity_seed[0], u.purity_seed[1], u.burn_amt, u.sat],
            degauss_abl: [u.degauss[0], u.degauss[1], u.degauss[2], u.abl],
            focus: [u.focus[0], u.focus[1], u.focus[2], 0.0],
            emission: [u.emission[0], u.emission[1], u.emission[2], 0.0],
            cutoff: [u.cutoff[0], u.cutoff[1], u.cutoff[2], 0.0],
            imax: [u.imax[0], u.imax[1], u.imax[2], 0.0],
            collapse: u.collapse,
            pers0: [u.decay[0], u.decay[1], u.decay[2], u.eye_k],
            pers1: [u.frac[0], u.frac[1], u.frac[2], tail_gain],
            scan0: [u.tau_fast[0], u.tau_fast[1], u.tau_fast[2], u.scan_phase],
            scan1: [u.tau_slow[0], u.tau_slow[1], u.tau_slow[2], u.scan_w],
            scan2: [u.scan_a, eye_k_eff, u.scan as f32, if fi.osd.is_some() { 1.0 } else { 0.0 }],
            phos0: [p[0], p[1], p[2], 0.0],
            phos1: [p[3], p[4], p[5], 0.0],
            phos2: [p[6], p[7], p[8], 0.0],
            glass0: [u.diffuse, u.halation, u.ambient, u.reflect],
            glass1: [u.vignette, u.exposure, u.diffuse_sigma_px, u.halation_sigma_px],
            glass2: [u.glass[0], u.glass[1], u.glass[2], 0.0],
            hdr0: [self.surface_mode.shader_mode(), u.hdr_paper_white, u.hdr_peak_nits, u.hdr_ui_nits],
            crt_rect: crt_rect_px,
            blur_p: [1.0, 0.0, q_px[0], q_px[1]],
        }
    }

    /// Runs the full CRT physics simulation pipeline into `view.crt_out` (`Rgba16Float` linear HDR).
    pub fn render_crt(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        fi: &FrameInput,
        crt_rect_px: [f32; 4],
    ) {
        let vw = (crt_rect_px[2].round() as u32).max(4);
        let vh = (crt_rect_px[3].round() as u32).max(4);

        // Upload updated post-decoder TV OSD if active & changed.
        if let Some((id, rgba)) = &fi.osd {
            if *id != self.osd_id && rgba.len() == crate::osd::OSD_W * crate::osd::OSD_H * 4 {
                self.osd_id = *id;
                queue.write_texture(
                    self.osd_tex.as_image_copy(),
                    rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some((crate::osd::OSD_W * 4) as u32),
                        rows_per_image: Some(crate::osd::OSD_H as u32),
                    },
                    wgpu::Extent3d {
                        width: crate::osd::OSD_W as u32,
                        height: crate::osd::OSD_H as u32,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        // Upload new user image or video frame if changed.
        if let Some((id, img)) = &fi.image {
            if *id != self.img_id {
                self.img_id = *id;
                let (w, h, data) = &**img;
                let w = (*w as u32).max(1);
                let h = (*h as u32).max(1);
                let tex = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("img_tex"),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    tex.as_image_copy(),
                    data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                );
                self.img_view = tex.create_view(&wgpu::TextureViewDescriptor::default());
                self.img_tex = tex;
                self.img_aspect = w as f32 / h as f32;
                self.has_img = true;
            }
        }

        self.ensure_view(device, vw, vh);
        self.ensure_src(device, fi.u.src_w.max(1) as u32, fi.u.src_h.max(1) as u32);

        let q_px = {
            let v = self.view.as_ref().unwrap();
            [v.q[0].w as f32, v.q[0].h as f32]
        };

        // Populate all 6 dynamic uniform slots in one upload.
        let base_u = self.build_uniforms(fi, crt_rect_px, q_px);
        self.write_slot(0, &base_u);

        let blur_specs = [
            ([1.0f32, 0.0f32], fi.u.diffuse_sigma_px),
            ([0.0, 1.0], fi.u.diffuse_sigma_px),
            ([1.0, 0.0], fi.u.halation_sigma_px),
            ([0.0, 1.0], fi.u.halation_sigma_px),
        ];
        for (idx, (dir, sigma)) in blur_specs.into_iter().enumerate() {
            let mut bu = base_u;
            bu.blur_p = [dir[0], dir[1], q_px[0], q_px[1]];
            bu.glass1[2] = sigma;
            self.write_slot(idx + 1, &bu);
        }

        // Slot 5: SDR unorm mode for PNG screenshot readback
        let mut shot_u = base_u;
        shot_u.hdr0[0] = SurfaceMode::SdrUnorm.shader_mode();
        self.write_slot(5, &shot_u);

        queue.write_buffer(&self.uniform_buf, 0, &self.uniform_staging);

        if fi.clear_burn {
            for t in &self.burn {
                Self::clear_target(encoder, &t.view);
            }
        }

        let u = &fi.u;
        if fi.new_field {
            // Upload CPU line-timing / sync table (NL x 2 Rgba32Float).
            if fi.lines.len() == (NL * 2 * 4) as usize {
                queue.write_texture(
                    self.lines_tex.as_image_copy(),
                    bytemuck::cast_slice(&fi.lines),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some((NL * 4 * 4) as u32),
                        rows_per_image: Some(2),
                    },
                    wgpu::Extent3d { width: NL as u32, height: 2, depth_or_array_layers: 1 },
                );
            }

            // 1. source
            let src_view = &self.src.as_ref().unwrap().view;
            let bg = self.make_tex_bg(device, &self.img_view, &self.dummy_view, &self.dummy_view, &self.dummy_view);
            self.run_pass(encoder, src_view, &self.p_source, &bg, 0);

            // 2. baseband
            let bg = self.make_tex_bg(device, src_view, &self.dummy_view, &self.dummy_view, &self.dummy_view);
            self.run_pass(encoder, &self.base.view, &self.p_base, &bg, 0);

            // 3. channel
            let bg = self.make_tex_bg(device, &self.base.view, &self.dummy_view, &self.dummy_view, &self.dummy_view);
            self.run_pass(encoder, &self.chan.view, &self.p_chan, &bg, 0);

            // 4. decode
            let bg = self.make_tex_bg(device, &self.chan.view, &self.dummy_view, &self.dummy_view, &self.dummy_view);
            self.run_pass(encoder, &self.drive.view, &self.p_decode, &bg, 0);

            // 4b. APL (1x1 average picture level)
            let bg = self.make_tex_bg(device, &self.drive.view, &self.dummy_view, &self.dummy_view, &self.dummy_view);
            self.run_pass(encoder, &self.apl.view, &self.p_apl, &bg, 0);

            // 5. burn-in
            if u.burn_rate > 0.0 || fi.burn_now {
                let (prev, next) = (self.burn_cur, 1 - self.burn_cur);
                let bg = self.make_tex_bg(
                    device,
                    &self.burn[prev].view,
                    &self.drive.view,
                    &self.dummy_view,
                    &self.dummy_view,
                );
                self.run_pass(encoder, &self.burn[next].view, &self.p_burn, &bg, 0);
                self.burn_cur = next;
            }

            // 6. beam + mask
            let e_next = 1 - self.view.as_ref().unwrap().e_cur;
            {
                let bg = self.make_tex_bg(
                    device,
                    &self.drive.view,
                    &self.dummy_view,
                    &self.burn[self.burn_cur].view,
                    &self.apl.view,
                );
                let dst = &self.view.as_ref().unwrap().e[e_next].view;
                self.run_pass(encoder, dst, &self.p_beam, &bg, 0);
            }
            self.view.as_mut().unwrap().e_cur = e_next;

            if u.scan == 0 {
                // 7. phosphor afterglow (per-field mode)
                let (e_cur, cur, nxt) = {
                    let v = self.view.as_ref().unwrap();
                    (v.e_cur, v.cur, 1 - v.cur)
                };
                {
                    let v = self.view.as_ref().unwrap();
                    let bg = self.make_tex_bg(device, &v.e[e_cur].view, &v.tail[cur].view, &self.dummy_view, &self.dummy_view);
                    self.run_pass(encoder, &v.tail[nxt].view, &self.p_tail, &bg, 0);
                }
                self.eye_and_glass(device, encoder, false);
            }
        }

        if u.scan != 0 {
            // 7b. rolling scan
            {
                let v = self.view.as_ref().unwrap();
                let bg = self.make_tex_bg(
                    device,
                    &v.e[v.e_cur].view,
                    &v.e[1 - v.e_cur].view,
                    &self.dummy_view,
                    &self.dummy_view,
                );
                self.run_pass(encoder, &v.sc.view, &self.p_scan, &bg, 0);
            }
            self.eye_and_glass(device, encoder, true);
        }

        // 9. composite into linear HDR CRT target
        {
            let v = self.view.as_ref().unwrap();
            let bg = self.make_tex_bg(
                device,
                &v.eye[v.cur].view,
                &v.q[0].view,
                &v.q[2].view,
                &self.dummy_view,
            );
            self.run_pass(encoder, &v.crt_out.view, &self.p_comp, &bg, 0);
        }
    }

    fn eye_and_glass(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, use_scan: bool) {
        let nxt = {
            let v = self.view.as_ref().unwrap();
            let cur = v.cur;
            let nxt = 1 - cur;
            let e_view = if use_scan { &v.sc.view } else { &v.e[v.e_cur].view };
            let t_view = if use_scan { &v.sc.view } else { &v.tail[nxt].view };

            // eye integration
            let bg = self.make_tex_bg(device, e_view, t_view, &v.eye[cur].view, &self.dummy_view);
            self.run_pass(encoder, &v.eye[nxt].view, &self.p_eye, &bg, 0);

            // quarter-res downsample
            let bg = self.make_tex_bg(device, &v.eye[nxt].view, &self.dummy_view, &self.dummy_view, &self.dummy_view);
            self.run_pass(encoder, &v.q[0].view, &self.p_down, &bg, 0);

            // 4-pass separable diffusion + halation blur (uniform slots 1..=4)
            let blur_io = [(0usize, 1usize, 1usize), (1, 0, 2), (0, 1, 3), (1, 2, 4)];
            for (from, to, slot) in blur_io {
                let bg = self.make_tex_bg(device, &v.q[from].view, &self.dummy_view, &self.dummy_view, &self.dummy_view);
                self.run_pass(encoder, &v.q[to].view, &self.p_blur, &bg, slot);
            }
            nxt
        };
        self.view.as_mut().unwrap().cur = nxt;
    }

    /// Presents linear HDR CRT + premultiplied sRGB egui UI overlay to the native swapchain.
    pub fn present(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        surface_view: &wgpu::TextureView,
    ) {
        let crt_view = self.view.as_ref().map(|v| &v.crt_out.view).unwrap_or(&self.dummy_view);
        let bg = self.make_tex_bg(device, crt_view, &self.ui_tex.view, &self.dummy_view, &self.dummy_view);
        self.run_pass(encoder, surface_view, &self.p_present, &bg, 0);
    }

    /// Renders an SDR sRGB `Rgba8Unorm` copy of the full window (CRT + UI) and reads it back for PNG export.
    pub fn capture_sdr_rgba8(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<(u32, u32, Vec<u8>), String> {
        let w = self.surface_config.width.max(1);
        let h = self.surface_config.height.max(1);
        let target = make_target(device, "screenshot_rgba8", w, h, wgpu::TextureFormat::Rgba8Unorm);

        let bytes_per_row = align_up((w * 4) as usize, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize) as u32;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot_readback"),
            size: (bytes_per_row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("screenshot_encoder"),
        });
        let crt_view = self.view.as_ref().map(|v| &v.crt_out.view).unwrap_or(&self.dummy_view);
        let bg = self.make_tex_bg(device, crt_view, &self.ui_tex.view, &self.dummy_view, &self.dummy_view);
        self.run_pass(&mut encoder, &target.view, &self.p_present_sdr_rgba8, &bg, 5);
        encoder.copy_texture_to_buffer(
            target._tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .map_err(|e| format!("screenshot channel error: {e}"))?
            .map_err(|e| format!("screenshot map error: {e}"))?;

        let mapped = slice
            .get_mapped_range()
            .map_err(|e| format!("screenshot map range error: {e}"))?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as usize {
            let start = row * bytes_per_row as usize;
            let row_slice = &mapped[start..start + (w * 4) as usize];
            out.extend_from_slice(row_slice);
        }
        drop(mapped);
        readback.unmap();
        Ok((w, h, out))
    }
}
