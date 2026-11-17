//! Horizon's own graphics settings, beyond vanilla's options: kept in
//! `horizon-graphics.txt` next to `options.txt` so that file stays exactly
//! vanilla's. Every default reproduces vanilla's rendering; only choices the
//! player makes here depart from it.
use horizon_os::fs;
use std::{
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
};

pub const FILE: &str = "horizon-graphics.txt";
/// Written beside it before HDR output is switched on and removed once the first HDR frame is
/// presented. Found at the next start, it means HDR output took the game down, so HDR starts off.
pub const HDR_MARKER: &str = "horizon-hdr-pending";

/// What the system reports for the display HDR output goes to, in nits in HDR output's
/// scale: where its SDR white sits (the desktop's white) and the peak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reported {
    pub paper_white: u32,
    pub peak: u32,
}

static REPORTED: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Sets what the system reports (the renderer, when it opens its surface).
pub fn report(reported: Option<Reported>) {
    let (paper_white, peak) = reported.map_or((0, 0), |r| (r.paper_white, r.peak));
    REPORTED[0].store(paper_white, Ordering::Relaxed);
    REPORTED[1].store(peak, Ordering::Relaxed);
}
/// What the system reports, if it does.
pub fn reported() -> Option<Reported> {
    let [paper_white, peak] = REPORTED.each_ref().map(|n| n.load(Ordering::Relaxed));
    (peak > 0).then_some(Reported { paper_white, peak })
}

/// Simple-level presets. `Custom` leaves every option as the player set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Preset {
    Performance,
    Balanced,
    Quality,
    Ultra,
    #[default]
    Custom,
}
impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Performance,
        Preset::Balanced,
        Preset::Quality,
        Preset::Ultra,
        Preset::Custom,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Preset::Performance => "performance",
            Preset::Balanced => "balanced",
            Preset::Quality => "quality",
            Preset::Ultra => "ultra",
            Preset::Custom => "custom",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Preset::Performance => "Performance",
            Preset::Balanced => "Balanced",
            Preset::Quality => "Quality",
            Preset::Ultra => "Ultra",
            Preset::Custom => "Custom",
        }
    }
    fn parse(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|p| p.name() == s)
            .unwrap_or_default()
    }
}

/// How the swapchain presents frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PresentMode {
    /// Vanilla's choice: Fifo with VSync on, else the first of Immediate,
    /// Mailbox and Fifo the surface supports. The other modes apply only
    /// with VSync off: VSync on always presents with Fifo.
    #[default]
    Auto,
    Fifo,
    Mailbox,
    Immediate,
}
impl PresentMode {
    pub const ALL: [PresentMode; 4] = [
        PresentMode::Auto,
        PresentMode::Fifo,
        PresentMode::Mailbox,
        PresentMode::Immediate,
    ];
    pub fn name(self) -> &'static str {
        match self {
            PresentMode::Auto => "auto",
            PresentMode::Fifo => "fifo",
            PresentMode::Mailbox => "mailbox",
            PresentMode::Immediate => "immediate",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            PresentMode::Auto => "Auto (VSync)",
            PresentMode::Fifo => "FIFO",
            PresentMode::Mailbox => "Mailbox",
            PresentMode::Immediate => "Immediate",
        }
    }
    fn parse(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|p| p.name() == s)
            .unwrap_or_default()
    }
}

/// The graphics API the renderer runs on. Windows picks Vulkan, DirectX 12 or
/// OpenGL; Linux picks Vulkan or OpenGL; macOS always uses its one backend
/// (Metal). Applies at the next start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GraphicsApi {
    /// Vulkan, then DirectX 12 (Windows), then OpenGL when neither has a surface, adapter or device.
    #[default]
    Auto,
    Vulkan,
    Dx12,
    /// OpenGL, for GPUs with no Vulkan or DirectX 12 driver (Windows and Linux).
    OpenGl,
}
impl GraphicsApi {
    pub const ALL: [GraphicsApi; 4] = [
        GraphicsApi::Auto,
        GraphicsApi::Vulkan,
        GraphicsApi::Dx12,
        GraphicsApi::OpenGl,
    ];
    /// The APIs this system offers, in the order the setting cycles through
    /// them: DirectX 12 is Windows' third backend, and macOS has no choice.
    pub fn choices() -> &'static [GraphicsApi] {
        const WINDOWS: [GraphicsApi; 4] = [
            GraphicsApi::Auto,
            GraphicsApi::Vulkan,
            GraphicsApi::Dx12,
            GraphicsApi::OpenGl,
        ];
        const LINUX: [GraphicsApi; 3] =
            [GraphicsApi::Auto, GraphicsApi::Vulkan, GraphicsApi::OpenGl];
        const MACOS: [GraphicsApi; 1] = [GraphicsApi::Auto];
        if cfg!(windows) {
            &WINDOWS
        } else if cfg!(target_os = "macos") {
            &MACOS
        } else {
            &LINUX
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            GraphicsApi::Auto => "auto",
            GraphicsApi::Vulkan => "vulkan",
            GraphicsApi::Dx12 => "dx12",
            GraphicsApi::OpenGl => "opengl",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            GraphicsApi::Auto => "Auto",
            GraphicsApi::Vulkan => "Vulkan",
            GraphicsApi::Dx12 => "DirectX 12",
            GraphicsApi::OpenGl => "OpenGL",
        }
    }
    /// The API a name stands for, ignoring case; `None` for anything else.
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|api| api.name().eq_ignore_ascii_case(s.trim()))
    }
    /// `HORIZON_GRAPHICS_API` (auto, vulkan, dx12 or opengl) if set, else the setting.
    pub fn with_env(self) -> Self {
        std::env::var("HORIZON_GRAPHICS_API")
            .ok()
            .and_then(|v| Self::from_name(&v))
            .unwrap_or(self)
    }
}

/// What the frame limiter does in the last half millisecond before a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Pacing {
    /// Sleep the whole wait: no CPU spent spinning, a little less precise.
    #[default]
    Efficient,
    /// Vanilla's `FramerateLimiter`: sleep, then spin until the deadline.
    Precise,
}

/// The performance overlay drawn over the HUD.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    Off,
    /// Frames per second and frame time.
    Fps,
    /// The numbers and a graph of recent frame times.
    Graph,
}

/// How finely far terrain is drawn (`FarDetail` in the LOD options).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FarDetail {
    /// Each tile level starts at eight times its size from the camera.
    #[default]
    Full,
    /// Coarser levels start at half that distance: about a quarter of the
    /// far quads, for GPUs that far terrain slows down. Changes the picture.
    Reduced,
}
impl FarDetail {
    pub fn name(self) -> &'static str {
        match self {
            FarDetail::Full => "full",
            FarDetail::Reduced => "reduced",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            FarDetail::Full => "Full",
            FarDetail::Reduced => "Reduced",
        }
    }
    fn parse(s: &str) -> Self {
        if s == "reduced" {
            FarDetail::Reduced
        } else {
            FarDetail::Full
        }
    }
}

/// TAA upscaling: the world renders this much smaller than the resolution scale's size,
/// and TAA builds the full-size picture from the jittered frames (FSR's and DLSS's ratios).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Upscaling {
    #[default]
    Native,
    Quality,
    Balanced,
    Performance,
    UltraPerformance,
}
impl Upscaling {
    pub const ALL: [Upscaling; 5] = [
        Upscaling::Native,
        Upscaling::Quality,
        Upscaling::Balanced,
        Upscaling::Performance,
        Upscaling::UltraPerformance,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Upscaling::Native => "native",
            Upscaling::Quality => "quality",
            Upscaling::Balanced => "balanced",
            Upscaling::Performance => "performance",
            Upscaling::UltraPerformance => "ultraPerformance",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Upscaling::Native => "Native",
            Upscaling::Quality => "Quality",
            Upscaling::Balanced => "Balanced",
            Upscaling::Performance => "Performance",
            Upscaling::UltraPerformance => "Ultra Performance",
        }
    }
    /// Output pixels per rendered pixel, along each axis.
    pub fn ratio(self) -> f32 {
        match self {
            Upscaling::Native => 1.0,
            Upscaling::Quality => 1.5,
            Upscaling::Balanced => 1.7,
            Upscaling::Performance => 2.0,
            Upscaling::UltraPerformance => 3.0,
        }
    }
    fn parse(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|u| u.name() == s)
            .unwrap_or_default()
    }
}

static REDUCED_SUGGESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Whether the settings suggest Reduced far detail: set when the renderer
/// starts, on the GPUs `lod_capped` names (far terrain slows them most).
pub fn suggest_reduced_far_detail(on: bool) {
    REDUCED_SUGGESTED.store(on, std::sync::atomic::Ordering::Relaxed);
}
pub fn reduced_far_detail_suggested() -> bool {
    REDUCED_SUGGESTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The name the options screen waits on to bind `Graphics::hdr_key`.
pub const HDR_KEY: &str = "horizon.hdrSettings";
/// The Screenshot Quality choices, times the game's resolution.
pub const SCREENSHOT_QUALITIES: [u32; 4] = [1, 2, 4, 8];
static HDR_OFFERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Whether the display path offers HDR output (HDR10 or scRGB): set when the renderer
/// opens its surface; the settings open HDR's screen only then.
pub fn offer_hdr(on: bool) {
    HDR_OFFERED.store(on, std::sync::atomic::Ordering::Relaxed);
}
pub fn hdr_offered() -> bool {
    HDR_OFFERED.load(std::sync::atomic::Ordering::Relaxed)
}

/// The farthest LOD Distance, in chunks, on integrated GPUs other than
/// Apple's: they share the CPU's memory bandwidth, and past it far terrain
/// costs them more frame time than it is worth.
pub const INTEGRATED_LOD_DISTANCE: u32 = 128;
/// The LOD Distance a fresh install gets, in chunks. Far terrain is a fallback
/// horizon under the resident chunks, and the farther it reaches the wider its
/// coarsest cells must be: 96 chunks tops out at 2048-block roots, where
/// infinite terrain goes to 8192 and its last rings are one flat quad a cell,
/// which is exactly the soft blob. Infinite stays one switch away.
pub const DEFAULT_LOD_DISTANCE: u32 = 96;
/// Whether `INTEGRATED_LOD_DISTANCE` applies to this adapter. Apple's GPUs
/// are integrated too, with memory and bandwidth enough for every distance.
/// Software and virtual GPUs share the same limits as other integrated ones.
pub fn lod_capped(info: &wgpu::AdapterInfo) -> bool {
    if web_low_memory() {
        return true;
    }
    match info.device_type {
        wgpu::DeviceType::IntegratedGpu => !info.name.starts_with("Apple"),
        wgpu::DeviceType::Cpu | wgpu::DeviceType::VirtualGpu => true,
        _ => false,
    }
}

/// An 8 GB-or-less browser device (or a four-thread one): low-power Chromebooks
/// and tablets report `Other` for their GPU, so the adapter class alone would
/// pick the full shadow and bloom path there.
///
/// Cached in an atomic rather than a `OnceLock`: the worldgen and meshing
/// workers ask this too, and `OnceLock::get_or_init` *blocks* a loser, which on
/// the browser is `Atomics.wait cannot be called in this context` unless the
/// thread is allowed to wait. Losing the race here only means computing the
/// two JavaScript calls again.
pub fn web_low_memory() -> bool {
    static LOW: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(UNKNOWN);
    cached(&LOW, || {
        let memory = horizon_os::device_memory_gib();
        (memory > 0.0 && memory <= 8.0) || horizon_os::hardware_threads() <= 4
    })
}
/// A 4 GB-or-less browser device: the smallest Chromebooks, and the ones whose
/// tab the browser kills first.
pub fn web_tiny_memory() -> bool {
    static TINY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(UNKNOWN);
    cached(&TINY, || {
        let memory = horizon_os::device_memory_gib();
        (memory > 0.0 && memory <= 4.0) || horizon_os::hardware_threads() <= 2
    })
}
/// Not computed yet. The two cached answers; `0` and `1` are the values.
const UNKNOWN: u8 = 2;
/// `cell`'s answer, computing it at most once per thread that wins the race.
fn cached(cell: &std::sync::atomic::AtomicU8, answer: impl Fn() -> bool) -> bool {
    // Off the browser there is nothing to cache: the answer is a constant.
    if !cfg!(target_arch = "wasm32") {
        return false;
    }
    match cell.load(std::sync::atomic::Ordering::Relaxed) {
        UNKNOWN => {
            let value = answer();
            // A relaxed store: another thread seeing the old value just
            // recomputes, which is only two JavaScript reads.
            cell.store(u8::from(value), std::sync::atomic::Ordering::Relaxed);
            value
        }
        known => known == 1,
    }
}
/// A LOD distance as far terrain uses it on this adapter.
pub fn lod_distance_capped(distance: u32, info: &wgpu::AdapterInfo) -> u32 {
    let distance = distance.min(crate::lod::MAX_CHUNKS);
    if lod_capped(info) {
        distance.min(INTEGRATED_LOD_DISTANCE)
    } else {
        distance
    }
}
/// The automatic visual tier, chosen from the adapter when the renderer opens
/// (`VisualQuality::Auto`): integrated and software GPUs get the cheap path.
static AUTO_VISUAL_QUALITY: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(VisualQuality::High as u8);
pub fn set_auto_visual_quality(quality: VisualQuality) {
    AUTO_VISUAL_QUALITY.store(quality as u8, std::sync::atomic::Ordering::Relaxed);
}
pub fn auto_visual_quality() -> VisualQuality {
    match AUTO_VISUAL_QUALITY.load(std::sync::atomic::Ordering::Relaxed) {
        0 => VisualQuality::Auto,
        1 => VisualQuality::High,
        2 => VisualQuality::Medium,
        _ => VisualQuality::Low,
    }
}
/// The tier `Auto` picks for an adapter: Apple's integrated GPUs are fast
/// enough for the full look, other integrated and software ones are not.
pub fn auto_quality_for(info: &wgpu::AdapterInfo) -> VisualQuality {
    let cores = horizon_os::hardware_threads();
    // Browser adapters report `Other` for real hardware (and for software
    // rasterisers alike): the device type cannot choose the look there. The
    // page records `navigator.deviceMemory` and its thread count; use them so
    // an 8 GB Chromebook does not start on the full shadow/bloom path.
    if info.backend == wgpu::Backend::BrowserWebGpu {
        let name = info.name.to_ascii_lowercase();
        let memory = horizon_os::device_memory_gib();
        let software = info.device_type == wgpu::DeviceType::Cpu
            || name.contains("swiftshader")
            || name.contains("llvmpipe")
            || name.contains("software");
        let low_end = name.contains("intel")
            || name.contains("mali")
            || name.contains("adreno")
            || name.contains("powervr")
            || name.contains("chromebook");
        return if software || (memory > 0.0 && memory <= 4.0) || cores <= 2 {
            VisualQuality::Low
        } else if (memory > 0.0 && memory <= 8.0) || cores <= 4 || low_end {
            VisualQuality::Medium
        } else {
            VisualQuality::High
        };
    }
    match info.device_type {
        wgpu::DeviceType::DiscreteGpu => VisualQuality::High,
        wgpu::DeviceType::IntegratedGpu if info.name.starts_with("Apple") => VisualQuality::High,
        wgpu::DeviceType::IntegratedGpu if cores <= 4 => VisualQuality::Low,
        wgpu::DeviceType::IntegratedGpu => VisualQuality::Medium,
        wgpu::DeviceType::Cpu | wgpu::DeviceType::VirtualGpu | wgpu::DeviceType::Other => {
            VisualQuality::Low
        }
    }
}
impl Graphics {
    /// `lod_distance` as far terrain uses it on this adapter. Infinite terrain
    /// ignores both the slider and the integrated GPU's finite cap: its
    /// camera-centred plan costs the same number of nodes at any distance, and
    /// low-end machines get coarser far cells instead of a nearer horizon.
    pub fn lod_distance_on(&self, info: &wgpu::AdapterInfo) -> u32 {
        if self.lod_infinite {
            crate::lod::INFINITE_CHUNKS
        } else {
            lod_distance_capped(self.lod_distance, info)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualQuality {
    Auto,
    High,
    Medium,
    Low,
}
impl VisualQuality {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
    pub fn parse(value: &str) -> Self {
        match value {
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::Auto,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Graphics {
    pub preset: Preset,
    /// The built-in Miniature Shader profile: grade, atmosphere, sun shading,
    /// water glint, glow and the near sun shadow. On by default.
    pub visuals: bool,
    pub visual_quality: VisualQuality,
    /// Distant surface terrain range, in chunks; 0 disables LODs.
    pub lod_distance: u32,
    /// Seed-generated terrain to the far horizon, whatever `lod_distance`
    /// says. On by default; local worlds only (a remote server has no seed).
    pub lod_infinite: bool,
    /// How finely far terrain is drawn; Full unless the player picks Reduced.
    pub far_detail: FarDetail,
    /// Skip sections hidden behind closed ones (vanilla's occlusion graph).
    pub cave_culling: bool,
    /// Skip drawing entities hidden behind opaque terrain.
    pub entity_culling: bool,
    /// Meshed sections uploaded per frame; 0 uploads all that are ready.
    pub chunk_updates: u32,
    /// Section mesh bytes uploaded per frame, in KiB; 0 is unlimited.
    pub upload_budget_kib: u32,
    /// The block textures' video memory (their top mip level), in MiB; 0 leaves it to
    /// Horizon (`sprites::DEFAULT_TEXTURE_BUDGET`). Applies when resources next load.
    pub texture_budget_mib: u32,
    pub sky: bool,
    pub fog: bool,
    pub weather: bool,
    pub animated_textures: bool,
    /// Dithers the small mip levels of cutouts from 64 texels up, so a high-resolution pack's
    /// fine plants keep their coverage in the distance; off is vanilla's look.
    pub dither_cutout_mips: bool,
    pub present_mode: PresentMode,
    pub pacing: Pacing,
    /// Chunk worker threads; 0 uses every core but one. Applies at the next start.
    pub workers: u32,
    pub overlay: Overlay,
    /// World resolution as a percentage of the window's (50..=200).
    pub resolution_scale: u32,
    /// FXAA over the world.
    pub fxaa: bool,
    /// TAA over the world (FXAA is off while it is on).
    pub taa: bool,
    /// TAA upscaling; anything but `Native` comes with `taa` on.
    pub upscaling: Upscaling,
    /// F2 screenshots at this many times the game's resolution: 1, 2, 4 or 8.
    pub screenshot_quality: u32,
    /// HDR output (HDR10, or scRGB), where the display path offers it.
    pub hdr: bool,
    /// HDR paper white (where SDR white sits), in nits; 0 is Auto: the desktop's SDR
    /// white, as the system reports it ([`Graphics::paper_white_nits`]).
    pub hdr_paper_white: u32,
    /// HDR peak brightness the world's highlights reach, in nits; 0 is Auto:
    /// what the system reports for the display ([`Graphics::peak_nits`]).
    pub hdr_peak: u32,
    /// HDR black level: the display's darkest black, in hundredths of a nit (0 for
    /// an OLED's true black). Shadows are lifted onto it, and the HDR metadata says it.
    pub hdr_black_level: u32,
    /// HDR brightness of the HUD and menus, in percent of paper white (25..=100); the
    /// first-person hand stays at the world's.
    pub hdr_ui_brightness: u32,
    /// A vanilla key name (`HDR_KEY`): opens HDR's settings from the game. Unbound by default.
    pub hdr_key: String,
    /// Battery mode: at most 60 frames per second in a world and 30 in menus
    /// or behind other windows, and half the worker threads. Off by default;
    /// it changes when frames are drawn, never what they show or the ticks.
    pub battery: bool,
    /// Reuse the shader pack's last shadow map when sun, view, scene revision and
    /// pack uniforms still match. On by default: the key covers the sun and moon
    /// angle, the snapped camera and the scene revision, so standing still with
    /// the sun up redraws nothing, and any change to what casts or where the
    /// light is misses the cache. Vanilla loaders redraw every frame, which is
    /// most of a frame's cost here for an identical image.
    pub shadow_cache: bool,
    /// The graphics API (Windows only; see `GraphicsApi`).
    pub api: GraphicsApi,
    /// The shader pack picked in `shaderpacks/` (`shader_pack_list`); empty: none. Its key is
    /// not `shaderPack`, which versions before the clean-room rewrite saved and which stays
    /// ignored.
    pub shader_pack: String,
}
impl Default for Graphics {
    fn default() -> Self {
        let low = web_low_memory();
        Self {
            preset: Preset::Custom,
            visuals: true,
            visual_quality: VisualQuality::Auto,
            lod_distance: DEFAULT_LOD_DISTANCE,
            // Far terrain is a *fallback* horizon, not a second world: past
            // `DEFAULT_LOD_DISTANCE` chunks the cells are wider than a block and
            // read as ridges rather than as the ground. A bounded distance keeps
            // every cell inside the rings that still hold detail, which is what
            // the default is for; `lod_infinite` stays available for a horizon
            // that never ends.
            lod_infinite: false,
            // Full detail everywhere it fits. `Reduced` is a visible change
            // (coarser far cells), so it is only the 4 GB devices': there the
            // choice is that or the browser killing the tab. An 8 GB machine
            // keeps the horizon it has always drawn.
            far_detail: if web_tiny_memory() {
                FarDetail::Reduced
            } else {
                FarDetail::Full
            },
            cave_culling: true,
            entity_culling: false,
            chunk_updates: if low { 48 } else { 0 },
            upload_budget_kib: if low { 2048 } else { 0 },
            texture_budget_mib: if low { 64 } else { 0 },
            sky: true,
            fog: true,
            weather: true,
            animated_textures: true,
            dither_cutout_mips: false,
            present_mode: PresentMode::Auto,
            pacing: Pacing::Efficient,
            workers: 0,
            overlay: Overlay::Off,
            resolution_scale: if low { 80 } else { 100 },
            fxaa: low,
            taa: false,
            upscaling: Upscaling::Native,
            screenshot_quality: 1,
            hdr: false,
            hdr_paper_white: 0,
            hdr_peak: 0,
            hdr_black_level: 0,
            hdr_ui_brightness: 100,
            hdr_key: "key.keyboard.unknown".into(),
            battery: false,
            shadow_cache: false,
            api: GraphicsApi::Auto,
            shader_pack: if cfg!(target_arch = "wasm32") {
                String::new()
            } else {
                "MiniatureXL".into()
            },
        }
    }
}
fn bool_text(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}
impl Graphics {
    pub fn load(dir: &Path) -> Self {
        let mut g: Self = fs::read_to_string(dir.join(FILE))
            .map(|t| Self::parse(&t))
            .unwrap_or_default();
        if web_low_memory() {
            // A file written before the device profile existed must not put an
            // 8 GB Chromebook back on full resolution or an unbounded upload
            // queue just because it says so.
            g.resolution_scale = g.resolution_scale.min(80);
            g.upload_budget_kib = if g.upload_budget_kib == 0 {
                2048
            } else {
                g.upload_budget_kib.min(4096)
            };
            g.texture_budget_mib = if g.texture_budget_mib == 0 {
                64
            } else {
                g.texture_budget_mib.min(64)
            };
            if g.chunk_updates == 0 {
                g.chunk_updates = 48;
            } else {
                g.chunk_updates = g.chunk_updates.min(96);
            }
            if !g.taa {
                g.fxaa = true;
            }
        }
        if web_tiny_memory() {
            // 4 GB: the tab is the first thing the browser reclaims. Render
            // less far and at a lower internal resolution, which are the two
            // costs that scale with the frame rather than with a setting.
            g.far_detail = FarDetail::Reduced;
            g.resolution_scale = g.resolution_scale.min(66);
            // Half the texture set and half the per-frame upload: both are
            // video memory the tab cannot get back once it has taken it.
            g.texture_budget_mib = g.texture_budget_mib.min(32);
            g.upload_budget_kib = g.upload_budget_kib.min(1024);
            g.chunk_updates = g.chunk_updates.min(32);
        }
        if fs::remove_file(dir.join(HDR_MARKER)).is_ok() && g.hdr {
            g.hdr = false;
            g.save(dir);
            log::warn!(
                "the last start ended before HDR output showed a frame; HDR is off now (graphics option hdr, in {FILE})"
            );
        }
        g
    }
    pub fn parse(text: &str) -> Self {
        let mut g = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            let flag = |d: bool| match value {
                "true" => true,
                "false" => false,
                _ => d,
            };
            let number = |d: u32| value.parse().unwrap_or(d);
            match key.trim() {
                "preset" => g.preset = Preset::parse(value),
                "visuals" => g.visuals = flag(g.visuals),
                "visualQuality" => g.visual_quality = VisualQuality::parse(value),
                "lodDistance" => {
                    g.lod_distance = number(g.lod_distance).min(crate::lod::MAX_CHUNKS)
                }
                "lodInfinite" => g.lod_infinite = flag(g.lod_infinite),
                "farDetail" => g.far_detail = FarDetail::parse(value),
                "caveCulling" => g.cave_culling = flag(g.cave_culling),
                "entityCulling" => g.entity_culling = flag(g.entity_culling),
                "chunkUpdates" => g.chunk_updates = number(g.chunk_updates).min(512),
                "uploadBudgetKiB" => g.upload_budget_kib = number(g.upload_budget_kib).min(65536),
                "textureBudgetMiB" => {
                    g.texture_budget_mib = number(g.texture_budget_mib).min(16384);
                }
                "sky" => g.sky = flag(g.sky),
                "fog" => g.fog = flag(g.fog),
                "weather" => g.weather = flag(g.weather),
                "animatedTextures" => g.animated_textures = flag(g.animated_textures),
                "ditherCutoutMips" => g.dither_cutout_mips = flag(g.dither_cutout_mips),
                "presentMode" => g.present_mode = PresentMode::parse(value),
                "framePacing" => {
                    g.pacing = if value == "precise" {
                        Pacing::Precise
                    } else {
                        Pacing::Efficient
                    }
                }
                "workers" => g.workers = number(g.workers).min(64),
                "resolutionScale" => g.resolution_scale = number(g.resolution_scale).clamp(25, 200),
                "fxaa" => g.fxaa = flag(g.fxaa),
                "taa" => g.taa = flag(g.taa),
                "upscaling" => g.upscaling = Upscaling::parse(value),
                "screenshotQuality" => {
                    g.screenshot_quality = Some(number(0))
                        .filter(|q| SCREENSHOT_QUALITIES.contains(q))
                        .unwrap_or(1)
                }
                "hdr" => g.hdr = flag(g.hdr),
                "hdrPaperWhite" => {
                    g.hdr_paper_white = match number(g.hdr_paper_white) {
                        0 => 0,
                        n => n.clamp(10, 480),
                    }
                }
                "hdrPeak" => {
                    g.hdr_peak = match number(g.hdr_peak) {
                        0 => 0,
                        n => n.clamp(200, 2000),
                    }
                }
                "hdrBlackLevel" => g.hdr_black_level = number(g.hdr_black_level).min(100),
                "hdrUiBrightness" => {
                    g.hdr_ui_brightness = number(g.hdr_ui_brightness).clamp(25, 100)
                }
                "hdrKey" if value.starts_with("key.") => g.hdr_key = value.to_owned(),
                "batteryMode" => g.battery = flag(g.battery),
                "shadowCache" => g.shadow_cache = flag(g.shadow_cache),
                "graphicsApi" => g.api = GraphicsApi::from_name(value).unwrap_or_default(),
                "pickedShaderPack" => g.shader_pack = value.to_owned(),
                "overlay" => {
                    g.overlay = match value {
                        "fps" => Overlay::Fps,
                        "graph" => Overlay::Graph,
                        _ => Overlay::Off,
                    }
                }
                _ => {}
            }
        }
        g
    }
    pub fn serialize(&self) -> String {
        format!(
            "preset:{}\nvisuals:{}\nvisualQuality:{}\nlodDistance:{}\nlodInfinite:{}\nfarDetail:{}\ncaveCulling:{}\nentityCulling:{}\nchunkUpdates:{}\nuploadBudgetKiB:{}\ntextureBudgetMiB:{}\nsky:{}\nfog:{}\nweather:{}\nanimatedTextures:{}\nditherCutoutMips:{}\npresentMode:{}\nframePacing:{}\nworkers:{}\noverlay:{}\nresolutionScale:{}\nfxaa:{}\ntaa:{}\nupscaling:{}\nscreenshotQuality:{}\nhdr:{}\nhdrPaperWhite:{}\nhdrPeak:{}\nhdrBlackLevel:{}\nhdrUiBrightness:{}\nhdrKey:{}\nbatteryMode:{}\nshadowCache:{}\ngraphicsApi:{}\npickedShaderPack:{}\n",
            self.preset.name(),
            bool_text(self.visuals),
            self.visual_quality.name(),
            self.lod_distance,
            bool_text(self.lod_infinite),
            self.far_detail.name(),
            bool_text(self.cave_culling),
            bool_text(self.entity_culling),
            self.chunk_updates,
            self.upload_budget_kib,
            self.texture_budget_mib,
            bool_text(self.sky),
            bool_text(self.fog),
            bool_text(self.weather),
            bool_text(self.animated_textures),
            bool_text(self.dither_cutout_mips),
            self.present_mode.name(),
            match self.pacing {
                Pacing::Efficient => "efficient",
                Pacing::Precise => "precise",
            },
            self.workers,
            match self.overlay {
                Overlay::Off => "off",
                Overlay::Fps => "fps",
                Overlay::Graph => "graph",
            },
            self.resolution_scale,
            bool_text(self.fxaa),
            bool_text(self.taa),
            self.upscaling.name(),
            self.screenshot_quality,
            bool_text(self.hdr),
            self.hdr_paper_white,
            self.hdr_peak,
            self.hdr_black_level,
            self.hdr_ui_brightness,
            self.hdr_key,
            bool_text(self.battery),
            bool_text(self.shadow_cache),
            self.api.name(),
            self.shader_pack,
        )
    }
    /// Binds Horizon's graphics key `name` (`HDR_KEY`); false for another name.
    pub fn set_key(&mut self, name: &str, value: String) -> bool {
        match name {
            HDR_KEY => self.hdr_key = value,
            _ => return false,
        }
        true
    }
    pub fn save(&self, dir: &Path) {
        let _ = fs::write(dir.join(FILE), self.serialize());
    }
    /// Paper white, in nits: the player's, or on Auto the desktop's SDR white as the
    /// system reports it, else 203 (BT.2408's reference white).
    pub fn paper_white_nits(&self) -> u32 {
        match self.hdr_paper_white {
            0 => reported().map_or(203, |r| r.paper_white),
            n => n,
        }
    }
    /// The peak the world's highlights reach, in nits: the player's, or on Auto
    /// what the system reports, else 1000.
    pub fn peak_nits(&self) -> u32 {
        match self.hdr_peak {
            0 => reported().map_or(1000, |r| r.peak),
            n => n,
        }
    }
    /// The tier in effect: Auto resolves to the adapter's class, and the
    /// renderer gates its sun shadow pass and bloom on this, not on the
    /// setting's `Auto` (or on the adapter alone when the player pinned Low).
    pub fn effective_visual_quality(&self) -> VisualQuality {
        match self.visual_quality {
            VisualQuality::Auto => auto_visual_quality(),
            tier => tier,
        }
    }
    /// The Miniature Shader profile's parameters, owned by [`crate::visual`].
    pub fn visual_params(&self) -> crate::visual::Params {
        crate::visual::Params::for_quality(self.effective_visual_quality(), self.visuals)
    }
    /// The post profile uniform: x enabled, grade and glow in the rest.
    pub fn visual_uniform(&self) -> [f32; 8] {
        self.visual_params().uniform()
    }
    /// What the post pass adds: exposure and the light-shaft enable.
    pub fn post_uniform(&self) -> [f32; 4] {
        self.visual_params().post_uniform()
    }
    /// Worker threads for chunk generation and meshing.
    pub fn worker_threads(&self) -> usize {
        if self.workers > 0 {
            return self.workers as usize;
        }
        // Vanilla's count (Util.maxAllowedExecutorThreads: every core but
        // one). The pool never takes every core: a pool that fills them all
        // starves the render and server threads, and the frame rate collapses
        // while the workers saturate the CPU and its memory bandwidth. One
        // core left is enough with the workers behind those two threads
        // (horizon_threads: niced on Linux; on macOS and Windows the server
        // and render threads are raised above four cores, where vanilla
        // raises its server thread). Leaving two idled a core of a 4-thread
        // laptop while a world loaded: a teleport there took ~15% longer.
        let auto = horizon_os::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .max(1);
        if self.battery { auto.div_ceil(2) } else { auto }
    }
    /// The frame cap battery mode adds: 60 in a world, 30 in menus without a
    /// world or while another window has focus.
    pub fn battery_limit(&self, limit: i32, in_world: bool, focused: bool) -> i32 {
        if !self.battery {
            limit
        } else if in_world && focused {
            limit.min(60)
        } else {
            limit.min(30)
        }
    }
}

/// A preset's values for vanilla's options and Horizon's own.
pub fn apply_preset(preset: Preset, s: &mut crate::settings::Settings) {
    let (rd, sim, clouds, particles, shadows, blend, mipmaps, entity, leaves, weather_r) =
        match preset {
            Preset::Performance => (8, 6, "false", 2, false, 0, 0, 0.5, false, 3),
            Preset::Balanced => (12, 8, "fast", 1, true, 1, 2, 0.75, true, 5),
            Preset::Quality => (16, 12, "true", 0, true, 2, 4, 1.0, true, 10),
            Preset::Ultra => (24, 12, "true", 0, true, 3, 4, 1.5, true, 10),
            Preset::Custom => return,
        };
    s.render_distance = rd;
    s.simulation_distance = sim;
    s.clouds = clouds.into();
    s.particles = particles;
    s.entity_shadows = shadows;
    s.biome_blend = blend;
    s.mipmap_levels = mipmaps;
    s.entity_distance = entity;
    s.cutout_leaves = leaves;
    s.weather_radius = weather_r;
    s.graphics_preset = "custom".into();
    let g = &mut s.horizon;
    g.preset = preset;
    // Far terrain is most of a frame's geometry in infinite mode, so the cheap
    // presets reach less far and draw coarser cells. Performance used to leave
    // both alone, which is why it barely helped on the 8 GB browser tabs that
    // need it most: the horizon cost the same as on Ultra.
    //
    // None of them turn infinite back on: the default horizon is bounded (96
    // chunks) because past it the cells are wider than a block and read as one
    // flat quad, and a preset that silently restored an 8192-block ring undid
    // the setting the player is looking at. Infinite is an explicit choice.
    match preset {
        Preset::Performance => {
            g.lod_infinite = false;
            g.lod_distance = 64;
            g.far_detail = FarDetail::Reduced;
        }
        Preset::Balanced => {
            g.lod_infinite = false;
            g.lod_distance = 96;
            g.far_detail = FarDetail::Reduced;
        }
        Preset::Quality | Preset::Ultra => {
            g.lod_infinite = false;
            g.lod_distance = 128;
            g.far_detail = FarDetail::Full;
        }
        Preset::Custom => {}
    }
    g.cave_culling = true;
    g.animated_textures = true;
    g.chunk_updates = 0;
    g.upload_budget_kib = 0;
    g.sky = true;
    g.fog = true;
    g.weather = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reusing the last shadow map is off by default, deliberately.
    ///
    /// The cache's key is a revision of the scene, so any edit that changes it
    /// redraws — but a map that is reused while the key still matches cannot
    /// know about a draw list change the key does not carry, and the shadow
    /// then stays where it was when a block is placed. It is an opt-in for
    /// still scenes (`horizon-graphics.txt` `shadowCache`), not a default.
    #[test]
    fn the_shadow_map_is_redrawn_unless_asked_otherwise() {
        assert!(
            !Graphics::default().shadow_cache,
            "a fresh install redraws the shadow map every frame"
        );
        assert!(Graphics::parse("").shadow_cache == Graphics::default().shadow_cache);
        // The key exists and turns it on.
        assert!(Graphics::parse("shadowCache:true").shadow_cache);
        assert!(!Graphics::parse("shadowCache:false").shadow_cache);
    }

    /// The device tiers are thresholds on the page's own report, so the only
    /// thing to pin is that they nest: a 4 GB device must always take the
    /// cheaper path, and a 4 GB one is always an 8 GB one too. A regression
    /// that inverted or widened them would quietly give the smallest Chromebook
    /// the full-detail horizon it cannot hold.
    #[test]
    fn the_tiny_tier_is_the_cheaper_half_of_the_low_one() {
        // The wasm thresholds; the native build answers false to both.
        let low = |mib: f64, cores: u16| (mib > 0.0 && mib <= 8.0) || cores <= 4;
        let tiny = |mib: f64, cores: u16| (mib > 0.0 && mib <= 4.0) || cores <= 2;
        for mib in [0.5, 2.0, 4.0, 6.0, 8.0, 16.0, 32.0] {
            for cores in [1, 2, 4, 6, 8, 16] {
                assert!(
                    !tiny(mib, cores) || low(mib, cores),
                    "tiny must be a subset of low at {mib} GiB and {cores} cores"
                );
            }
        }
        // The reported 8 GB Chromebooks land where the profile expects.
        assert!(low(4.0, 4) && tiny(4.0, 4), "a 4 GB device takes both");
        assert!(low(8.0, 4) && !tiny(8.0, 4), "8 GB keeps full far detail");
        assert!(
            !low(16.0, 8) && !tiny(16.0, 8),
            "a real machine keeps it all"
        );
        // A two-thread machine is as memory-bound as a 4 GB one.
        assert!(tiny(16.0, 2), "two threads means the tiny profile");
    }

    /// Whatever the device, the picture is only reduced where the alternative
    /// is the tab being killed: `Reduced` far detail is a visible change, so
    /// the default keeps `Full` on anything with room for it.
    #[test]
    fn the_default_far_detail_follows_the_tiny_tier() {
        let g = Graphics::default();
        if web_tiny_memory() {
            assert_eq!(g.far_detail, FarDetail::Reduced);
        } else {
            assert_eq!(g.far_detail, FarDetail::Full);
        }
        if web_low_memory() {
            assert!(
                g.resolution_scale <= 80,
                "a low-memory device must not render at full scale"
            );
        }
    }

    #[test]
    fn the_api_choice_matches_the_system() {
        let choices = GraphicsApi::choices();
        if cfg!(windows) {
            assert_eq!(choices, GraphicsApi::ALL);
        } else if cfg!(target_os = "macos") {
            assert_eq!(choices, [GraphicsApi::Auto]);
        } else {
            assert_eq!(
                choices,
                [GraphicsApi::Auto, GraphicsApi::Vulkan, GraphicsApi::OpenGl]
            );
        }
    }

    #[test]
    fn visual_vibrance_is_on_by_default() {
        let g = Graphics::default();
        assert!(g.visuals);
        assert_eq!(g.visual_quality, VisualQuality::Auto);
        assert_eq!(g.visual_uniform()[0], 1.0);
        assert!(g.post_uniform()[0] > 0.0);
        let off = Graphics {
            visuals: false,
            ..Graphics::default()
        };
        assert_eq!(off.visual_uniform()[0], 0.0);
    }

    #[test]
    fn the_low_tier_keeps_the_grade_and_drops_the_bloom() {
        let tier = |quality| {
            Graphics {
                visual_quality: quality,
                ..Graphics::default()
            }
            .visual_uniform()
        };
        let (high, medium, low) = (
            tier(VisualQuality::High),
            tier(VisualQuality::Medium),
            tier(VisualQuality::Low),
        );
        // The pack's own glow, where the tier can hold its chain: strength,
        // threshold and halo at the pack's defaults.
        assert_eq!((high[4], high[5], high[6]), (1.0, 0.45, 1.0));
        assert_eq!(high, medium, "Medium holds the glow too");
        // Low allocates no glow chain, so it reads the pack's lighting only.
        assert_eq!((low[4], low[5], low[6]), (0.0, 0.0, 0.0));
        // The grade itself stays the pack's, not ours on top of its lighting.
        assert_eq!(low[1], 1.0);
        assert_eq!(low[2], 1.0);
        assert_eq!(low[0], 1.0);
    }

    #[test]
    fn round_trips() {
        let g = Graphics {
            preset: Preset::Balanced,
            visuals: false,
            visual_quality: VisualQuality::Low,
            lod_distance: 256,
            lod_infinite: false,
            far_detail: FarDetail::Reduced,
            cave_culling: false,
            entity_culling: true,
            chunk_updates: 12,
            upload_budget_kib: 4096,
            texture_budget_mib: 1536,
            sky: false,
            fog: false,
            weather: false,
            animated_textures: false,
            dither_cutout_mips: true,
            present_mode: PresentMode::Mailbox,
            pacing: Pacing::Precise,
            workers: 6,
            overlay: Overlay::Graph,
            resolution_scale: 75,
            fxaa: true,
            taa: true,
            upscaling: Upscaling::Balanced,
            screenshot_quality: 4,
            hdr: true,
            hdr_paper_white: 240,
            hdr_peak: 1400,
            hdr_black_level: 12,
            hdr_ui_brightness: 60,
            hdr_key: "key.keyboard.h".into(),
            battery: true,
            shadow_cache: true,
            api: GraphicsApi::Dx12,
            shader_pack: "Golden Hour".into(),
        };
        assert_eq!(Graphics::parse(&g.serialize()), g);
        assert_eq!(Graphics::parse(""), Graphics::default());
        // 0 is Auto; anything else is a peak the slider reaches.
        assert_eq!(Graphics::parse("hdrPeak:0\n").hdr_peak, 0);
        assert_eq!(Graphics::parse("hdrPeak:50\n").hdr_peak, 200);
        assert_eq!(Graphics::parse("hdrPaperWhite:0\n").hdr_paper_white, 0);
        assert_eq!(Graphics::parse("hdrPaperWhite:5\n").hdr_paper_white, 10);
        // Only the button's choices.
        assert_eq!(
            Graphics::parse("screenshotQuality:8\n").screenshot_quality,
            8
        );
        assert_eq!(
            Graphics::parse("screenshotQuality:3\n").screenshot_quality,
            1
        );
        assert_eq!(
            Graphics::parse("screenshotQuality:16\n").screenshot_quality,
            1
        );
        // The old menus' Simple / Advanced choice is ignored, and so is the
        // shader pack older versions saved.
        assert_eq!(Graphics::parse("level:advanced\n"), Graphics::default());
        assert_eq!(
            Graphics::parse("shaderPack:Pack.zip\n"),
            Graphics::default()
        );
        assert_eq!(
            Graphics::parse("workers:abc\nsky:maybe\ncullLeaves:true\nentityCulling:maybe\n"),
            Graphics::default()
        );
    }

    #[test]
    fn a_fresh_install_draws_a_bounded_lod_horizon() {
        // The default used to be infinite: a horizon to 4096 chunks, whose last
        // rings are 8192-block cells drawn as one flat quad each — the soft blob
        // at the edge of the world. 96 chunks keeps the coarsest roots inside
        // the rings that still carry shape.
        let g = Graphics::default();
        assert_eq!(g.lod_distance, DEFAULT_LOD_DISTANCE);
        assert_eq!(DEFAULT_LOD_DISTANCE, 96);
        assert!(!g.lod_infinite, "the default horizon is bounded");
        assert!(
            DEFAULT_LOD_DISTANCE <= 128,
            "an integrated GPU's cap must not lower the default"
        );
        // Infinite is still reachable, and still what an explicit choice gets.
        let endless = Graphics {
            lod_infinite: true,
            ..Graphics::default()
        };
        let discrete = wgpu::AdapterInfo {
            name: "NVIDIA GeForce RTX 4070".into(),
            vendor: 0,
            device: 0,
            device_type: wgpu::DeviceType::DiscreteGpu,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: None,
            limit_bucket: None,
        };
        assert_eq!(
            endless.lod_distance_on(&discrete),
            crate::lod::INFINITE_CHUNKS
        );
        // The bounded default is what a discrete GPU is actually asked for.
        assert_eq!(g.lod_distance_on(&discrete), DEFAULT_LOD_DISTANCE);
    }

    #[test]
    fn integrated_gpus_other_than_apple_cap_the_lod_distance() {
        let adapter = |name: &str, device_type| wgpu::AdapterInfo {
            name: name.into(),
            vendor: 0,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: None,
            limit_bucket: None,
        };
        let hd620 = adapter(
            "Intel(R) HD Graphics 620 (KBL GT2)",
            wgpu::DeviceType::IntegratedGpu,
        );
        let m4 = adapter("Apple M4 Pro", wgpu::DeviceType::IntegratedGpu);
        let discrete = adapter("NVIDIA GeForce RTX 4070", wgpu::DeviceType::DiscreteGpu);
        let software = adapter("llvmpipe (LLVM)", wgpu::DeviceType::Cpu);
        for (distance, capped) in [(0, 0), (64, 64), (128, 128), (256, 128), (1024, 128)] {
            let g = Graphics {
                lod_distance: distance,
                lod_infinite: false,
                ..Graphics::default()
            };
            assert_eq!(g.lod_distance_on(&hd620), capped);
            assert_eq!(g.lod_distance_on(&software), capped);
            assert_eq!(g.lod_distance_on(&m4), distance);
            assert_eq!(g.lod_distance_on(&discrete), distance);
        }
        assert!(
            crate::graphics_ui::rows(crate::graphics_ui::Level::Simple)
                .iter()
                .any(|row| matches!(row, crate::graphics_ui::Row::Setting(s)
                    if s.text.contains("fading into the sky"))),
            "the Infinite Terrain description names the horizon"
        );
    }

    #[test]
    fn infinite_terrain_is_opt_in_and_uncapped() {
        let hd620 = wgpu::AdapterInfo {
            name: "Intel(R) HD Graphics 620 (KBL GT2)".into(),
            vendor: 0,
            device: 0,
            device_type: wgpu::DeviceType::IntegratedGpu,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: None,
            limit_bucket: None,
        };
        let g = Graphics::default();
        // The default is a bounded horizon now: infinite reaches 8192-block
        // cells, whose last rings are one flat quad each — the soft blob.
        assert!(!g.lod_infinite);
        assert_eq!(g.lod_distance, DEFAULT_LOD_DISTANCE);
        assert_eq!(g.lod_distance_on(&hd620), DEFAULT_LOD_DISTANCE);
        // Infinite is still what an explicit choice gets, with no distance cap.
        let on = Graphics::parse("lodInfinite:true\n");
        assert!(on.lod_infinite);
        assert_eq!(on.lod_distance_on(&hd620), crate::lod::INFINITE_CHUNKS);
        // Explicit Off persists, and the finite slider returns.
        let off = Graphics::parse("lodInfinite:false\nlodDistance:256\n");
        assert!(!off.lod_infinite);
        assert_eq!(off.lod_distance_on(&hd620), INTEGRATED_LOD_DISTANCE);
        assert_eq!(Graphics::parse(&off.serialize()), off);
    }

    #[test]
    fn automatic_visual_quality_follows_the_adapter() {
        let adapter = |name: &str, device_type| wgpu::AdapterInfo {
            name: name.into(),
            vendor: 0,
            device: 0,
            device_type,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: wgpu::Backend::Vulkan,
            subgroup_min_size: 0,
            subgroup_max_size: 0,
            transient_saves_memory: None,
            limit_bucket: None,
        };
        assert_eq!(
            auto_quality_for(&adapter("NVIDIA RTX", wgpu::DeviceType::DiscreteGpu)),
            VisualQuality::High
        );
        assert_eq!(
            auto_quality_for(&adapter("Apple M4", wgpu::DeviceType::IntegratedGpu)),
            VisualQuality::High
        );
        assert_eq!(
            auto_quality_for(&adapter("llvmpipe", wgpu::DeviceType::Cpu)),
            VisualQuality::Low
        );
        // Auto resolves to the adapter's tier; a pinned tier ignores it.
        set_auto_visual_quality(VisualQuality::Low);
        let mut g = Graphics::default();
        assert_eq!(g.visual_uniform(), {
            g.visual_quality = VisualQuality::Low;
            g.visual_uniform()
        });
        g.visual_quality = VisualQuality::High;
        assert_ne!(
            crate::visual::Params::for_quality(g.effective_visual_quality(), g.visuals).shadow,
            {
                g.visual_quality = VisualQuality::Low;
                crate::visual::Params::for_quality(g.effective_visual_quality(), g.visuals).shadow
            }
        );
        set_auto_visual_quality(VisualQuality::High);
    }

    #[test]
    fn lod_distance_defaults_to_96_and_round_trips() {
        assert_eq!(Graphics::default().lod_distance, DEFAULT_LOD_DISTANCE);
        for distance in [0, 64, 128, 256, 512, 1024, 65_536, crate::lod::MAX_CHUNKS] {
            let g = Graphics::parse(&format!("lodDistance:{distance}\n"));
            assert_eq!(g.lod_distance, distance);
            assert_eq!(Graphics::parse(&g.serialize()), g);
        }
        assert_eq!(
            Graphics::parse("lodDistance:99999999\n").lod_distance,
            crate::lod::MAX_CHUNKS
        );
        // Invalid input keeps the default rather than clearing the horizon.
        assert_eq!(
            Graphics::parse("lodDistance:-1\n").lod_distance,
            DEFAULT_LOD_DISTANCE
        );
        assert_eq!(
            Graphics::parse("lodDistance:invalid\n").lod_distance,
            DEFAULT_LOD_DISTANCE
        );
    }

    #[test]
    fn far_detail_is_full_unless_reduced_is_picked() {
        assert_eq!(Graphics::default().far_detail, FarDetail::Full);
        assert_eq!(
            Graphics::parse("farDetail:reduced\n").far_detail,
            FarDetail::Reduced
        );
        for text in ["farDetail:full\n", "farDetail:other\n", ""] {
            assert_eq!(Graphics::parse(text).far_detail, FarDetail::Full);
        }
        // Far terrain is most of the frame's geometry, so the cheap presets
        // draw it coarser and the expensive ones keep every cell.
        for (preset, detail) in [
            (Preset::Performance, FarDetail::Reduced),
            (Preset::Balanced, FarDetail::Reduced),
            (Preset::Quality, FarDetail::Full),
            (Preset::Ultra, FarDetail::Full),
        ] {
            let mut s = crate::settings::Settings::default();
            apply_preset(preset, &mut s);
            assert_eq!(s.horizon.far_detail, detail, "{preset:?}");
        }
        // Custom leaves whatever the player picked alone.
        let mut s = crate::settings::Settings::default();
        s.horizon.far_detail = FarDetail::Reduced;
        apply_preset(Preset::Custom, &mut s);
        assert_eq!(s.horizon.far_detail, FarDetail::Reduced);
    }

    #[test]
    fn every_preset_keeps_the_animated_textures_on() {
        // The Performance preset used to turn them off, which froze the fire,
        // the water, the lava and the portals (#2064).
        assert!(Graphics::default().animated_textures);
        for preset in Preset::ALL {
            let mut s = crate::settings::Settings::default();
            apply_preset(preset, &mut s);
            assert!(
                s.horizon.animated_textures,
                "{preset:?} turned the animated textures off"
            );
        }
    }

    #[test]
    fn automatic_workers_leave_a_core_as_vanilla_does() {
        let g = Graphics::default();
        assert_eq!(g.workers, 0);
        let cores = horizon_os::thread::available_parallelism().map_or(2, |n| n.get());
        assert_eq!(g.worker_threads(), cores.saturating_sub(1).max(1));
        // A manual count is the player's own choice, kept as asked.
        let manual = Graphics { workers: 3, ..g };
        assert_eq!(manual.worker_threads(), 3);
    }

    #[test]
    fn battery_mode_only_lowers_the_frame_cap() {
        let mut g = Graphics::default();
        assert_eq!(g.battery_limit(260, true, true), 260);
        g.battery = true;
        assert_eq!(g.battery_limit(260, true, true), 60);
        assert_eq!(g.battery_limit(30, true, true), 30);
        assert_eq!(g.battery_limit(260, false, true), 30);
        assert_eq!(g.battery_limit(260, true, false), 30);
        assert_eq!(g.battery_limit(10, true, false), 10);
    }
}
