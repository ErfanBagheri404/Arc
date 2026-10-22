//! DirectComposition/D3D11 renderer with a safe degraded mode.
//!
//! The renderer owns the GPU objects, but never owns application state. A device
//! failure is non-fatal: counters continue working and the window remains usable.
//!
//! Contract (do not change without updating `app::run`):
//! - `new(hwnd)` once, after the window exists.
//! - `resize(physical_w, physical_h)` whenever the island geometry changes.
//! - `present(frame, dpi_scale, dt_seconds, body_offset_x, sync)` exactly once per
//!   frame the app loop asks for; the renderer must not present on its own.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;

use std::os::windows::ffi::OsStrExt;
use windows::core::{Error, Interface, BOOL, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_WINDING, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1Geometry, ID2D1Layer, ID2D1RenderTarget,
    ID2D1PathGeometry, ID2D1SolidColorBrush, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFactory5, IDWriteFontCollection, IDWriteTextFormat,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_REGULAR, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICFormatConverter,
    IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::SHCreateMemStream;

use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1, IDXGISwapChain3,
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Matrix3x2;

use crate::core::geom::{notch_metrics, squircle_segments, CornerRadii, Rect, Rgba};
use crate::core::imagedb;
use crate::core::scene::{Align, Frame, ImageHandle, Node, TextStyle, Weight};

/// Runtime counters for the debug overlay and the release perf gate
/// (idle presents must be 0; see docs/05-ARCHITECTURE.md §9).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PerfProbe {
    /// Frames handed to `present` since start.
    pub presents: u64,
    /// Nodes in the last presented frame.
    pub last_node_count: usize,
    /// Rolling average frame time in milliseconds, or `None` when nothing animated.
    pub avg_frame_ms: Option<f32>,
}

/// TEMP DEBUG: renderer-internal stage timings (acquire / draw / Present).
pub static DBG_NODES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static DBG_ACQ_US: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static DBG_ENDDRAW_US: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
pub static DBG_RS_PRESENT_US: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Cache key for a DirectWrite text format. Sizes are stored as bits so two sizes
/// 12 and 12.0000001 do not collide through float equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FormatKey {
    size_bits: u32,
    weight: i32,
    tabular: bool,
    mono: bool,
}

/// D2D renders onto a premultiplied surface, so a brush colour must be
/// premultiplied by its own alpha against black — which is exactly
/// `(r*a, g*a, b*a, a)`.
fn color_key(color: &D2D1_COLOR_F) -> [u32; 4] {
    [color.r.to_bits(), color.g.to_bits(), color.b.to_bits(), color.a.to_bits()]
}

/// Identity of a filled outline: rect bits, radius bits, and a tag so a glyph
/// and a squircle that happen to share a rect never collide in the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GeomKey([u32; 9]);

fn geometry_key(rect: &Rect, radii: &CornerRadii, tag: u32) -> GeomKey {
    GeomKey([
        rect.x.to_bits(),
        rect.y.to_bits(),
        rect.w.to_bits(),
        rect.h.to_bits(),
        radii.top_left.to_bits(),
        radii.top_right.to_bits(),
        radii.bottom_right.to_bits(),
        radii.bottom_left.to_bits(),
        tag,
    ])
}

fn premultiply(c: Rgba) -> D2D1_COLOR_F {
    let c = c.clamped();
    D2D1_COLOR_F {
        r: c.r * c.a,
        g: c.g * c.a,
        b: c.b * c.a,
        a: c.a,
    }
}

fn clamp_progress(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn wide(value: &str) -> Vec<u16> {
    // PCWSTR and friends are null-terminated; without the NUL the callee reads
    // past the end and every CreateTextFormat / DrawText gets garbage.
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn format_key(style: &TextStyle) -> FormatKey {
    let weight = match style.weight {
        Weight::Regular => DWRITE_FONT_WEIGHT_REGULAR.0,
        Weight::Semibold => DWRITE_FONT_WEIGHT_SEMI_BOLD.0,
        Weight::Bold => DWRITE_FONT_WEIGHT_BOLD.0,
    };
    FormatKey {
        size_bits: style.size.max(0.0).to_bits(),
        weight,
        tabular: style.tabular,
        mono: style.mono,
    }
}

// The point each of the four corner segments departs from.
/// start of the matching segment in [`squircle_segments`], or the outline closes
/// with visible kinks between corners.
fn corner_start(rect: &Rect, radii: CornerRadii, index: usize) -> (f32, f32) {
    // A negative top radius is the concave wing, which moves the body's sides
    // inward and reorders the segments: index 0 is the top-right wing (its
    // only predecessor being the top edge), and the body's left side comes
    // last. `notch_metrics` is the one source of truth for that geometry.
    if radii.top_left < 0.0 || radii.top_right < 0.0 {
        let (body_l, body_r, bl, br, wl, _wr) = notch_metrics(rect, radii);
        return match index {
            0 => (rect.max_x(), rect.y),
            1 => (body_r, rect.max_y() - br),
            2 => (body_l + bl, rect.max_y()),
            _ => (body_l, rect.y + wl),
        };
    }
    match index {
        0 => (rect.x + radii.top_left, rect.y),
        1 => (rect.x, rect.max_y() - radii.bottom_left),
        2 => (rect.max_x() - radii.bottom_right, rect.max_y()),
        _ => (rect.max_x(), rect.y + radii.top_right),
    }
}

/// One operation of a squircle outline, as data. `fill_squircle` replays this
/// against a Direct2D geometry sink; keeping it as a value lets a test pin the
/// path's shape (one continuous figure) without a GPU.
#[derive(Debug, Clone, Copy, PartialEq)]
enum OutlineStep {
    Begin((f32, f32)),
    Line((f32, f32)),
    Bezier((f32, f32), (f32, f32), (f32, f32)),
    End,
}

/// Walk the four corner curves as ONE figure: open at the first corner, join each
/// curve to the next with a straight edge, close along the last edge.
///
/// Emitting a figure per corner instead fills four leaf-shaped slivers, which is
/// how the island once rendered as nothing at all.
fn outline_steps(rect: &Rect, radii: CornerRadii) -> Vec<OutlineStep> {
    let mut steps = Vec::with_capacity(6);
    steps.push(OutlineStep::Begin(corner_start(rect, radii, 0)));
    for (index, seg) in squircle_segments(rect, radii.clamped(rect.w, rect.h))
        .iter()
        .enumerate()
    {
        if index > 0 {
            steps.push(OutlineStep::Line(corner_start(rect, radii, index)));
        }
        steps.push(OutlineStep::Bezier(seg.c1, seg.c2, seg.to));
    }
    steps.push(OutlineStep::End);
    steps
}

/// Names the failing chain step in the log. A silent `Err` here means an invisible
/// island with no clue why, which is the one failure mode that cannot be debugged
/// from the outside.
macro_rules! step {
    ($what:literal, $expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(error) => {
                log::error!("arc: GPU step {} failed: {error}", $what);
                return Err(error);
            }
        }
    };
}

/// Bind a Direct2D render target to the swapchain's current back buffer.
///
/// Flip-model swapchains require every reference to a back buffer to be released
/// before `Present`, or the buffers cannot rotate: `Present` still returns `S_OK`
/// and the composition silently never updates. Holding one render target for the
/// life of the window is therefore a no-op bug, so the target is acquired before
/// drawing and dropped before presenting.
fn acquire_target(
    d2d: &ID2D1Factory,
    swapchain: &IDXGISwapChain1,
    scale: f32,
) -> windows::core::Result<ID2D1RenderTarget> {
    let surface: IDXGISurface = step!("GetBuffer", unsafe { swapchain.GetBuffer(0) });
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        // The scene is authored in DIPs, the back buffer is physical pixels.
        // Reporting the real DPI is what makes 185 DIPs land on 231 px, so the
        // island is centred and sharp instead of top-left pinned and undersized.
        dpiX: 96.0 * scale,
        dpiY: 96.0 * scale,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let target = step!("CreateDxgiSurfaceRenderTarget", unsafe {
        d2d.CreateDxgiSurfaceRenderTarget(&surface, &props)
    });
    Ok(target)
}

/// Replays a scene onto the overlay's swapchain.
pub struct Renderer {
    hwnd: HWND,
    width: u32,
    height: u32,
    swapchain: Option<IDXGISwapChain1>,
    dcomp: Option<IDCompositionDevice>,
    dcomp_target: Option<IDCompositionTarget>,
    visual: Option<IDCompositionVisual>,
    d2d_factory: Option<ID2D1Factory>,
    dwrite: Option<IDWriteFactory>,
    /// Collection holding the bundled proportional face. `None` when the
    /// asset is missing or the font stack could not be built; the family
    /// chain then falls through to the system fonts.
    font_collection: Option<IDWriteFontCollection>,
    formats: HashMap<FormatKey, IDWriteTextFormat>,
    /// WIC factory, built lazily on the first image draw. `None` until then, and
    /// permanently `None` if the platform has no imaging stack.
    wic: Option<IWICImagingFactory>,
    /// CPU-side decode cache, keyed by the handle `ui` interned the bytes under.
    /// Holds the converted 32bpp source, not a GPU bitmap: the render target is
    /// rebuilt from the swapchain every frame, so a device bitmap cached across
    /// frames lands in the wrong resource domain. `None` is a cached *failure* —
    /// a handle that failed once must not be re-decoded every frame.
    images: HashMap<ImageHandle, Option<IWICFormatConverter>>,
    /// Solid brushes keyed by the exact bits of the premultiplied colour. A
    /// brush is device-dependent, not target-dependent, so one outlives every
    /// per-frame render target the swapchain hands us. Creating one per node
    /// per frame was the idle burn: a 40 px pill cost ~20 ms of CPU a second.
    /// Animated fades mint new colours every frame, so the map is dropped
    /// wholesale once it grows past `64` rather than leaking.
    brushes: HashMap<[u32; 4], ID2D1SolidColorBrush>,
    /// Filled path geometries keyed by the exact shape that produced them. A
    /// committed geometry cannot be re-`Open`ed, so the only way to avoid
    /// rebuilding an unchanged outline every frame is to keep the finished
    /// geometry and `FillGeometry` it directly. Animating shapes miss (correct,
    /// they changed); every static node in the scene hits.
    geom_cache: HashMap<GeomKey, ID2D1PathGeometry>,
    /// Scratch UTF-16 buffer for text runs: cleared per draw, never reallocated.
    scratch_utf16: Vec<u16>,
    probe: PerfProbe,
    /// True when the GPU chain could not be built (RDP, no GPU, CI). Everything
    /// except drawing still works, so the app runs and tests pass anywhere.
    degraded: bool,
    /// Set once the device is lost; drawing stops but counters keep counting.
    dead: bool,
    warned: bool,
}

impl Renderer {
    /// Build the device chain against `hwnd`. Always `Some`: a machine without a
    /// usable GPU gets a degraded renderer rather than a failed app.
    pub fn new(hwnd: NonNull<c_void>) -> Option<Self> {
        let mut renderer = Self {
            hwnd: HWND(hwnd.as_ptr()),
            width: 1,
            height: 1,
            swapchain: None,
            dcomp: None,
            dcomp_target: None,
            visual: None,
            d2d_factory: None,
            dwrite: None,
            font_collection: None,
            formats: HashMap::new(),
            wic: None,
            images: HashMap::new(),
            brushes: HashMap::new(),
            geom_cache: HashMap::new(),
            scratch_utf16: Vec::with_capacity(64),
            probe: PerfProbe::default(),
            degraded: true,
            dead: false,
            warned: false,
        };
        renderer.rebuild(1, 1);
        Some(renderer)
    }

    fn rebuild(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.release_chain();
        if let Err(error) = self.build_chain() {
            self.degraded = true;
            if !self.warned {
                log::error!("arc: GPU chain unavailable, running without drawing ({error})");
                self.warned = true;
            }
        } else {
            self.degraded = false;
        }
    }

    /// Drop every GPU object. Reused by `Drop`, so it must tolerate being called
    /// on a half-built renderer.
    fn release_chain(&mut self) {
        self.swapchain = None;
        self.visual = None;
        self.dcomp_target = None;
        self.dcomp = None;
        self.d2d_factory = None;
        self.formats.clear();
        self.geom_cache.clear();
    }

    fn build_chain(&mut self) -> windows::core::Result<()> {
        let mut device = None;
        let mut level = D3D_FEATURE_LEVEL(0);
        let mut context = None;
        // Hardware first; WARP keeps the app alive on machines with no usable GPU
        // driver, which is what makes this path safe in CI and over RDP.
        let attempt = unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )
        };
        crate::app::dbg_spawn_log(&format!(
            "D3D hw_ok={} level={}",
            attempt.is_ok(),
            level.0
        ));
        if attempt.is_err() {
            device = None;
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    Some(&mut level),
                    Some(&mut context),
                )?;
            }
        }
        let device = device.ok_or(Error::from_thread())?;
        let dxgi: IDXGIDevice = step!("IDXGIDevice", device.cast());
        let adapter = step!("GetAdapter", unsafe { dxgi.GetAdapter() });
        if let Ok(desc) = unsafe { adapter.GetDesc() } {
            let name: String = desc
                .Description
                .iter()
                .take_while(|c| **c != 0)
                .map(|c| char::from_u32(*c as u32).unwrap_or('?'))
                .collect();
            crate::app::dbg_spawn_log(&format!("adapter {name}"));
        }
        let factory: IDXGIFactory2 = step!("IDXGIFactory2", unsafe { adapter.GetParent() });

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: self.width,
            Height: self.height,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: BOOL(0),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        let swapchain = step!("CreateSwapChainForComposition", unsafe {
            factory.CreateSwapChainForComposition(&device, &desc, None)
        });
        // The composition surface is sampled by DComp, so a queued frame
        // must not sit in the back buffer longer than one present, and
        // the chain must never report lost just because we asked it to.
        // Explicit sRGB rather than the driver's default (risks T3).
        if let Ok(swapchain3) = swapchain.cast::<IDXGISwapChain3>() {
            let _ = unsafe { swapchain3.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709) };
            let _ = unsafe { swapchain3.SetMaximumFrameLatency(3) };
        }

        let dcomp: IDCompositionDevice = step!("DCompositionCreateDevice", unsafe {
            DCompositionCreateDevice(&dxgi)
        });
        let dcomp_target = step!("CreateTargetForHwnd", unsafe {
            dcomp.CreateTargetForHwnd(self.hwnd, true)
        });
        let visual = step!("CreateVisual", unsafe { dcomp.CreateVisual() });
        unsafe {
            visual.SetContent(&swapchain)?;
            dcomp_target.SetRoot(Some(&visual))?;
            dcomp.Commit()?;
        }

        let d2d: ID2D1Factory =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dwrite: IDWriteFactory = step!("DWriteCreateFactory", unsafe {
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)
        });

        // Inter, bundled (OFL): Apple's SF faces ship under a licence that
        // forbids redistribution, so the stack's reference font is a
        // stand-in. Held in its own collection rather than registered with
        // the system, so nothing leaks outside this process.
        let font_collection = dwrite
            .cast::<IDWriteFactory5>()
            .ok()
            .and_then(|f| bundled_collection(&f));

        self.font_collection = font_collection;
        self.swapchain = Some(swapchain);
        self.dcomp = Some(dcomp);
        self.dcomp_target = Some(dcomp_target);
        self.visual = Some(visual);
        self.d2d_factory = Some(d2d);
        self.dwrite = Some(dwrite);
        Ok(())
    }

    /// Resize the back buffers for a new size. Idempotent for an unchanged size.
    ///
    /// `ResizeBuffers` on the live swapchain, never a device rebuild: the
    /// island animates its size every frame while it morphs, and tearing down
    /// the D3D device, the composition visual and the DComp target that often
    /// is what makes the morph flicker. Nothing here needs the device recreated.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height && !self.degraded {
            return;
        }
        self.width = width;
        self.height = height;
        // No chain yet (or a degraded one): a full build is the only path.
        let Some(swapchain) = self.swapchain.clone() else {
            self.rebuild(width, height);
            return;
        };
        // Flip-model back buffers cannot rotate while a reference is alive,
        // so drop every cached render target before handing the chain back.
        self.formats.clear();
        // SAFETY: our own swapchain; no buffer references are outstanding.
        let resized = unsafe {
            swapchain.ResizeBuffers(
                0,
                width,
                height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )
        };
        if resized.is_err() {
            // A failed resize can leave the chain unusable; rebuild once.
            self.rebuild(width, height);
        }
    }

    /// Draw one frame and present it.
    ///
    /// `offset_x` shifts the whole scene right, in logical units. The window is
    /// a fixed frame (see `app::run`), so the morphing island is *centred* by
    /// translating the scene instead of resizing the HWND — a `SetWindowPos` +
    /// `ResizeBuffers` every animating frame is what made the expand stutter.
    pub fn present(&mut self, frame: &Frame, scale: f32, dt: f32, offset_x: f32, offset_y: f32, sync: bool) {
        let _ = (
            DBG_ACQ_US.load(std::sync::atomic::Ordering::Relaxed),
            DBG_ENDDRAW_US.load(std::sync::atomic::Ordering::Relaxed),
            DBG_RS_PRESENT_US.load(std::sync::atomic::Ordering::Relaxed),
        );
        self.probe.presents = self.probe.presents.saturating_add(1);
        self.probe.last_node_count = frame.scene.count();
        DBG_NODES.store(self.probe.last_node_count, std::sync::atomic::Ordering::Relaxed);
        let ms = dt.max(0.0) * 1000.0;
        self.probe.avg_frame_ms = Some(match self.probe.avg_frame_ms {
            Some(prev) => prev * 0.9 + ms * 0.1,
            None => ms,
        });

        if self.degraded || self.dead {
            return;
        }
        // Drop the previous frame's render target *before* touching the swapchain:
        // a flip-model back buffer cannot rotate while a reference is alive.
        let (Some(swapchain), Some(d2d)) = (self.swapchain.clone(), self.d2d_factory.clone())
        else {
            return;
        };
        let dbg_a0 = std::time::Instant::now();
        let target = match acquire_target(&d2d, &swapchain, scale) {
            Ok(pair) => pair,
            Err(error) => {
                log::error!("arc: could not bind a render target to the back buffer ({error})");
                self.degraded = true;
                return;
            }
        };

        DBG_ACQ_US.store(
            dbg_a0.elapsed().as_micros() as usize,
            std::sync::atomic::Ordering::Relaxed,
        );
        let dbg_b0 = std::time::Instant::now();
        unsafe { target.BeginDraw() };
        if offset_x != 0.0 || offset_y != 0.0 {
            // The render target is DPI-scaled, so a translation in logical
            // units lands in device pixels without further conversion.
            let _ = unsafe { target.SetTransform(&Matrix3x2::translation(offset_x, offset_y)) };
        }
        unsafe {
            // Transparent clear every frame: the body is drawn at alpha 1 and
            // everything outside it must stay see-through.
            let transparent = D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            };
            target.Clear(Some(&transparent));
        }
        if std::env::var("ARC_DBG_EMPTY").is_err() {
            frame.scene.walk(&mut |node| self.draw_node(&target, node));
        }
        let drawn = unsafe { target.EndDraw(None, None) };
        // Release the back buffer before presenting; see `acquire_target`.
        drop(target);
        if let Err(error) = drawn {
            let code = error.code();
            log::error!("arc: draw failed ({error})");
            if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
                self.dead = true;
            }
            return;
        }

        // Sync interval 1 only while a spring is live: that is what keeps the
        // morph tear-free. An idle frame (the pet's 5 Hz loop) must NOT wait for
        // a vblank — blocking there cost ~28 ms per repaint, ~25 % of a core,
        // for a 40 px pill nobody is interacting with. Sync 0 returns as soon as
        // the buffer is queued, and the loop's own pacing is the frame clock.
        let interval = u32::from(sync);
        DBG_ENDDRAW_US.store(
            dbg_b0.elapsed().as_micros() as usize,
            std::sync::atomic::Ordering::Relaxed,
        );
        let dbg_p0 = std::time::Instant::now();
        let result = unsafe { swapchain.Present(interval, Default::default()) };
        DBG_RS_PRESENT_US.store(
            dbg_p0.elapsed().as_micros() as usize,
            std::sync::atomic::Ordering::Relaxed,
        );
        if !result.is_ok() {
            log::warn!("arc: Present failed: {result:?} (0x{:08X})", result.0);
        }
        if result == DXGI_ERROR_DEVICE_REMOVED || result == DXGI_ERROR_DEVICE_RESET {
            log::error!("arc: device lost while presenting");
            self.dead = true;
        }
    }

    fn draw_node(&mut self, target: &ID2D1RenderTarget, node: &Node) {
        match node {
            Node::RoundRect { rect, radii, fill } => {
                self.fill_squircle(target, *rect, *radii, *fill)
            }
            Node::Text { rect, text, style } => self.draw_text(target, *rect, text, style),
            Node::Image {
                rect,
                handle,
                radii,
            } => self.draw_image(target, *rect, *handle, *radii),
            Node::Bar {
                rect,
                progress,
                track,
                fill,
                radii,
            } => {
                self.fill_squircle(target, *rect, *radii, *track);
                let progress = clamp_progress(*progress);
                if progress > 0.0 {
                    self.fill_squircle(
                        target,
                        Rect::new(rect.x, rect.y, rect.w * progress, rect.h),
                        *radii,
                        *fill,
                    );
                }
            }
            Node::Sprite {
                rect,
                handle,
                src,
                opacity,
            } => self.draw_sprite(target, *rect, *handle, *src, *opacity),
            Node::Glyph { rect, name, color } => self.fill_glyph(target, *rect, name, *color),
            Node::Arc {
                rect,
                progress,
                track,
                fill,
                thickness,
            } => self.fill_arc(target, *rect, *progress, *track, *fill, *thickness),
            Node::Group { children, .. } => {
                for child in children {
                    self.draw_node(target, child);
                }
            }
        }
    }

    /// Paint one named glyph as a filled path. Only the four transport names
    /// exist; an unknown name draws nothing (fail-visible would be a blob).
    fn fill_glyph(&mut self, target: &ID2D1RenderTarget, rect: Rect, name: &str, color: Rgba) {
        if color.a <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let Some(tris) = glyph_tris(name, &rect) else {
            return;
        };
        let Some(brush) = self.brush(target, color) else {
            return;
        };
        let key = geometry_key(&rect, &CornerRadii::uniform(0.0), 1);
        let path = if let Some(path) = self.cached_geometry(&key) {
            path
        } else {
            let Some(path) = self.new_geometry() else {
                return;
            };
            let Ok(sink) = (unsafe { path.Open() }) else {
                return;
            };
            unsafe {
                sink.SetFillMode(D2D1_FILL_MODE_WINDING);
                for tri in &tris {
                    sink.BeginFigure(p2(tri[0]), D2D1_FIGURE_BEGIN_FILLED);
                    sink.AddLine(p2(tri[1]));
                    sink.AddLine(p2(tri[2]));
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                }
                if sink.Close().is_err() {
                    return;
                }
            }
            self.cache_geometry(key, &path);
            path
        };
        unsafe {
            target.FillGeometry(&path, &brush, None);
        }
    }

    /// A finished geometry for `key`, or `None` on a miss (caller builds it).
    ///
    /// A cache hit is already committed, so callers must `FillGeometry` it
    /// *without* `Open`ing it — re-opening a committed geometry is the E_INVALIDARG
    /// that made the pill vanish after its first frame.
    fn cached_geometry(&self, key: &GeomKey) -> Option<ID2D1PathGeometry> {
        self.geom_cache.get(key).cloned()
    }

    fn new_geometry(&mut self) -> Option<ID2D1PathGeometry> {
        let factory = self.d2d_factory.clone()?;
        unsafe { factory.CreatePathGeometry() }.ok()
    }

    /// Store a freshly built geometry. Dropped wholesale past the cap so an
    /// endlessly animating scene cannot grow the map without bound.
    fn cache_geometry(&mut self, key: GeomKey, path: &ID2D1PathGeometry) {
        if self.geom_cache.len() >= 128 {
            self.geom_cache.clear();
        }
        self.geom_cache.insert(key, path.clone());
    }

    fn fill_squircle(
        &mut self,
        target: &ID2D1RenderTarget,
        rect: Rect,
        radii: CornerRadii,
        color: Rgba,
    ) {
        if color.a <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let Some(brush) = self.brush(target, color) else {
            return;
        };
        let key = geometry_key(&rect, &radii, 0);
        let path = if let Some(path) = self.cached_geometry(&key) {
            path
        } else {
            let Some(path) = self.new_geometry() else {
                return;
            };
            let Ok(sink) = (unsafe { path.Open() }) else {
                return;
            };
            unsafe {
                sink.SetFillMode(D2D1_FILL_MODE_WINDING);
                for step in outline_steps(&rect, radii) {
                    match step {
                        OutlineStep::Begin(p) => sink.BeginFigure(point(p), D2D1_FIGURE_BEGIN_FILLED),
                        OutlineStep::Line(p) => sink.AddLine(point(p)),
                        OutlineStep::Bezier(c1, c2, to) => sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                            point1: point(c1),
                            point2: point(c2),
                            point3: point(to),
                        }),
                        OutlineStep::End => sink.EndFigure(D2D1_FIGURE_END_CLOSED),
                    }
                }
                if sink.Close().is_err() {
                    return;
                }
            }
            self.cache_geometry(key, &path);
            path
        };
        unsafe { target.FillGeometry(&path, &brush, None) };
    }

    /// Paint a progress ring: an elliptical band hugging `rect`, with a dark
    /// track and a fill that runs from the top clockwise to `progress`.
    fn fill_arc(
        &mut self,
        target: &ID2D1RenderTarget,
        rect: Rect,
        progress: f32,
        track: Rgba,
        fill: Rgba,
        thickness: f32,
    ) {
        if rect.w <= 0.0 || rect.h <= 0.0 || thickness <= 0.0 {
            return;
        }
        let cx = rect.x + rect.w * 0.5;
        let cy = rect.y + rect.h * 0.5;
        let rx = (rect.w * 0.5 - thickness * 0.5).max(0.0);
        let ry = (rect.h * 0.5 - thickness * 0.5).max(0.0);
        let outer_rx = rx + thickness * 0.5;
        let outer_ry = ry + thickness * 0.5;
        let inner_rx = (rx - thickness * 0.5).max(0.0);
        let inner_ry = (ry - thickness * 0.5).max(0.0);
        const SEGS: usize = 48;
        // Track: full ring.
        if track.a > 0.0 {
            if let Some(brush) = self.brush(target, track) {
                if let Some(path) = self.new_geometry() {
                    if let Ok(sink) = unsafe { path.Open() } {
                        unsafe {
                            sink.SetFillMode(D2D1_FILL_MODE_WINDING);
                            // outer clockwise 0..2π
                            sink.BeginFigure(point((cx + outer_rx, cy)), D2D1_FIGURE_BEGIN_FILLED);
                            for i in 1..=SEGS {
                                let a = (i as f32 / SEGS as f32) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                                // start at top (-π/2), go clockwise => increasing angle
                                let th = -std::f32::consts::FRAC_PI_2 + (i as f32 / SEGS as f32) * std::f32::consts::TAU;
                                sink.AddLine(point((cx + outer_rx * th.cos(), cy + outer_ry * th.sin())));
                            }
                            // inner clockwise-back 2π..0
                            for i in (0..=SEGS).rev() {
                                let th = -std::f32::consts::FRAC_PI_2 + (i as f32 / SEGS as f32) * std::f32::consts::TAU;
                                sink.AddLine(point((cx + inner_rx * th.cos(), cy + inner_ry * th.sin())));
                            }
                            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                            let _ = sink.Close();
                        }
                        unsafe { target.FillGeometry(&path, &brush, None) };
                    }
                }
            }
        }
        let progress = clamp_progress(progress);
        if progress <= 0.0 || fill.a <= 0.0 {
            return;
        }
        // Fill: partial ring from top clockwise.
        if let Some(brush) = self.brush(target, fill) {
            if let Some(path) = self.new_geometry() {
                if let Ok(sink) = unsafe { path.Open() } {
                    unsafe {
                        sink.SetFillMode(D2D1_FILL_MODE_WINDING);
                        let n = ((progress * SEGS as f32).round() as usize).clamp(1, SEGS);
                        sink.BeginFigure(point((cx + outer_rx * (-std::f32::consts::FRAC_PI_2).cos(), cy + outer_ry * (-std::f32::consts::FRAC_PI_2).sin())), D2D1_FIGURE_BEGIN_FILLED);
                        for i in 1..=n {
                            let th = -std::f32::consts::FRAC_PI_2 + (i as f32 / SEGS as f32) * std::f32::consts::TAU;
                            sink.AddLine(point((cx + outer_rx * th.cos(), cy + outer_ry * th.sin())));
                        }
                        // inner back along the same span
                        for i in (0..=n).rev() {
                            let th = -std::f32::consts::FRAC_PI_2 + (i as f32 / SEGS as f32) * std::f32::consts::TAU;
                            sink.AddLine(point((cx + inner_rx * th.cos(), cy + inner_ry * th.sin())));
                        }
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        let _ = sink.Close();
                    }
                    unsafe { target.FillGeometry(&path, &brush, None) };
                }
            }
        }
    }
    /// Decode `bytes` into a 32bpp premultiplied WIC source, cached by `handle`.
    /// initialises COM on the thread that calls `present`, so this does not.
    fn decoded(&mut self, handle: ImageHandle) -> Option<IWICFormatConverter> {
        if let Some(cached) = self.images.get(&handle) {
            return cached.clone();
        }
        let decoded = self.decode(handle);
        self.images.insert(handle, decoded.clone());
        decoded
    }

    fn decode(&mut self, handle: ImageHandle) -> Option<IWICFormatConverter> {
        let bytes = imagedb::bytes(handle)?;
        if bytes.is_empty() {
            return None;
        }
        if self.wic.is_none() {
            // Build it once. A machine with no imaging stack keeps `None` and the
            // placeholder fill, rather than retrying CoCreateInstance per frame.
            self.wic = unsafe {
                CoCreateInstance::<_, IWICImagingFactory>(
                    &CLSID_WICImagingFactory,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
                .ok()
            };
        }
        let wic = self.wic.clone()?;
        unsafe {
            let stream = SHCreateMemStream(Some(&bytes))?;
            let decoder = wic
                .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
                .ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let converter = wic.CreateFormatConverter().ok()?;
            converter
                .Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None::<&IWICPalette>,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .ok()?;
            Some(converter)
        }
    }

    /// Draw a decoded image, clipped to the same squircle outline the fills use so
    /// album art corners match the island's.
    fn draw_image(
        &mut self,
        target: &ID2D1RenderTarget,
        rect: Rect,
        handle: ImageHandle,
        radii: CornerRadii,
    ) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let Some(source) = self.decoded(handle) else {
            self.fill_squircle(target, rect, radii, Rgba::rgb(0.16, 0.16, 0.18));
            return;
        };
        let properties = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let Ok(bitmap) = (unsafe { target.CreateBitmapFromWicBitmap(&source, Some(&properties)) })
        else {
            self.fill_squircle(target, rect, radii, Rgba::rgb(0.16, 0.16, 0.18));
            return;
        };

        // Art is not a fill: a squircle mask needs a geometry layer, because a
        // bitmap draws as one opaque quad and the layer is what cuts its corners.
        let Some(factory) = self.d2d_factory.clone() else {
            return;
        };
        let Ok(path) = (unsafe { factory.CreatePathGeometry() }) else {
            return;
        };
        if let Ok(sink) = unsafe { path.Open() } {
            unsafe {
                sink.SetFillMode(D2D1_FILL_MODE_WINDING);
                for step in outline_steps(&rect, radii) {
                    match step {
                        OutlineStep::Begin(p) => {
                            sink.BeginFigure(point(p), D2D1_FIGURE_BEGIN_FILLED)
                        }
                        OutlineStep::Line(p) => sink.AddLine(point(p)),
                        OutlineStep::Bezier(c1, c2, to) => sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                            point1: point(c1),
                            point2: point(c2),
                            point3: point(to),
                        }),
                        OutlineStep::End => sink.EndFigure(D2D1_FIGURE_END_CLOSED),
                    }
                }
                let _ = sink.Close();
            }
        }

        let layer: Option<ID2D1Layer> = unsafe { target.CreateLayer(None).ok() };
        let Some(layer) = layer else {
            unsafe {
                target.DrawBitmap(
                    &bitmap,
                    Some(&to_rect(rect)),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                )
            };
            return;
        };
        let parameters = D2D1_LAYER_PARAMETERS {
            contentBounds: to_rect(rect),
            geometricMask: std::mem::ManuallyDrop::new(path.cast::<ID2D1Geometry>().ok()),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        unsafe {
            target.PushLayer(&parameters, &layer);
            target.DrawBitmap(
                &bitmap,
                Some(&to_rect(rect)),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
            target.PopLayer();
        }
    }

    /// Draw one frame of a spritesheet: the same bitmap path as `draw_image`,
    /// but `src` crops the sheet to the frame. Used by the idle-notch pet,
    /// where the sheet is one atlas and the frame is a 192x208 cell.
    /// `opacity` rides on `DrawBitmap` itself — the sheet has no fill colour
    /// to fade — and the node is skipped at 0, mirroring the glyph branch.
    fn draw_sprite(
        &mut self,
        target: &ID2D1RenderTarget,
        rect: Rect,
        handle: ImageHandle,
        src: Rect,
        opacity: f32,
    ) {
        if rect.w <= 0.0 || rect.h <= 0.0 || src.w <= 0.0 || src.h <= 0.0 || opacity <= 0.0 {
            return;
        }
        let Some(source) = self.decoded(handle) else {
            return;
        };
        let properties = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let Ok(bitmap) = (unsafe { target.CreateBitmapFromWicBitmap(&source, Some(&properties)) })
        else {
            return;
        };
        let crop = D2D_RECT_F {
            left: src.x,
            top: src.y,
            right: src.max_x(),
            bottom: src.max_y(),
        };
        unsafe {
            target.DrawBitmap(
                &bitmap,
                Some(&to_rect(rect)),
                opacity.clamp(0.0, 1.0),
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                Some(&crop),
            )
        };
    }

    fn brush(&mut self, target: &ID2D1RenderTarget, color: Rgba) -> Option<ID2D1SolidColorBrush> {
        let color = premultiply(color);
        let key = color_key(&color);
        if let Some(cached) = self.brushes.get(&key) {
            return Some(cached.clone());
        }
        let brush = unsafe { target.CreateSolidColorBrush(&color, None).ok() };
        if let Some(brush) = &brush {
            if self.brushes.len() >= 64 {
                self.brushes.clear();
            }
            self.brushes.insert(key, brush.clone());
        }
        brush
    }

    fn draw_text(&mut self, target: &ID2D1RenderTarget, rect: Rect, text: &str, style: &TextStyle) {
        let Some(format) = self.text_format(style) else {
            return;
        };
        let Some(brush) = self.brush(target, style.color) else {
            return;
        };
        let layout = D2D_RECT_F {
            left: rect.x,
            top: rect.y,
            right: rect.max_x(),
            bottom: rect.max_y(),
        };
        // DrawText with a fixed rect would wrap or overflow silently, so any
        // caller that hands us more text than the box holds gets an ellipsis.
        // This is what stops a long album title or host name from bleeding
        // outside its row. Trimming lives on the cached format (set once in
        // `text_format`), so a frame pays nothing for it.
        // DrawText takes a slice, so it gets the length the text actually has --
        // the NUL that wide() appends for PCWSTR is not part of it.
        self.scratch_utf16.clear();
        self.scratch_utf16.extend(text.encode_utf16());
        unsafe {
            target.DrawText(
                &self.scratch_utf16,
                &format,
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }

    fn text_format(&mut self, style: &TextStyle) -> Option<IDWriteTextFormat> {
        let key = format_key(style);
        if let Some(cached) = self.formats.get(&key) {
            return Some(cached.clone());
        }
        let factory = self.dwrite.as_ref()?;
        // "Segoe UI Variable Display" is absent on some Win10/11 SKUs, and a
        // missing family fails the whole CreateTextFormat call -- so walk down
        // to plain Segoe UI instead of rendering nothing.
        // Proportional runs resolve against the bundled collection so
        // "Inter" is found; the mono chain stays on the system set.
        // DirectWrite rejects a null locale name with E_INVALIDARG, so the
        // locale has to be a real, empty-terminated string even when we want the
        // user default.
        let locale = wide("");
        let mut last_err = None;
        let mut format = None;
        // Inter resolves only through the bundled collection; when that
        // build failed the family chain starts at Segoe UI so every run
        // still gets a real format.
        let families: [&str; 5] = if style.mono {
            [
                "Cascadia Mono",
                "Cascadia Code",
                "Consolas",
                "Courier New",
                "Arial",
            ]
        } else if self.font_collection.is_some() {
            [
                "Inter",
                "Segoe UI Variable Display",
                "Segoe UI Variable",
                "Segoe UI",
                "Segoe UI",
            ]
        } else {
            [
                "Segoe UI Variable Display",
                "Segoe UI Variable",
                "Segoe UI",
                "Segoe UI",
                "Segoe UI",
            ]
        };
        let collection = if style.mono {
            None
        } else {
            self.font_collection.as_ref()
        };
        for family_name in families {
            let family = wide(family_name);
            match unsafe {
                factory.CreateTextFormat(
                    PCWSTR(family.as_ptr()),
                    collection,
                    windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT(key.weight),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    style.size.max(1.0),
                    PCWSTR(locale.as_ptr()),
                )
            } {
                Ok(f) => {
                    format = Some(f);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let format = match format {
            Some(f) => f,
            None => {
                // One warn per frame would flood; the format is never going to
                // appear mid-run, so report once and stay quiet.
                if !self.warned {
                    self.warned = true;
                    log::warn!("arc: CreateTextFormat failed: {last_err:?}");
                }
                return None;
            }
        };
        unsafe {
            let _ = format.SetTextAlignment(match style.align {
                Align::Start => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::End => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            // Vertically centred: the UI hands out boxes, not baselines.
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            // Tab labels must never wrap mid-word: the UI measures with an
            // approximation while DirectWrite advances the real glyphs, so the
            // layout box can come out a hair too narrow (see "Clipboard").
            let _ = format.SetWordWrapping(
                windows::Win32::Graphics::DirectWrite::DWRITE_WORD_WRAPPING_NO_WRAP,
            );
            // Word-ellipsis trimming, once per cached format: DrawText with a
            // fixed rect would wrap or bleed for any caller that hands us more
            // text than the box holds. The sign belongs to the factory.
            // CHARACTER, not WORD: word trimming only cuts at word boundaries, so
            // a single long token with no spaces (`GhatleAmd[PianoVersion]`, a
            // hash, a path) finds no boundary inside the box, is left untrimmed,
            // and runs past the panel edge. Character granularity always fits the
            // box, which is the invariant the UI relies on (`fit` clamps the rect,
            // so an untrimmed run escapes the visible body).
            if let Some(dwrite) = self.dwrite.as_ref() {
                if let Ok(sign) = dwrite.CreateEllipsisTrimmingSign(&format) {
                    let _ = format.SetTrimming(
                        &windows::Win32::Graphics::DirectWrite::DWRITE_TRIMMING {
                            granularity: windows::Win32::Graphics::DirectWrite::DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                            delimiter: 0,
                            delimiterCount: 0,
                        },
                        &sign,
                    );
                }
            }
        }
        self.formats.insert(key, format.clone());
        Some(format)
    }

    /// Live counters for the perf gates. Unused by the app until the debug
    /// overlay lands (phase 4); tests assert the idle-present budget directly.
    #[cfg(test)]
    pub fn probe(&self) -> PerfProbe {
        self.probe
    }

    /// True when the GPU chain is not live; the debug overlay surfaces this.
    #[allow(dead_code)]
    pub fn degraded(&self) -> bool {
        self.degraded
    }
}

/// Build a DirectWrite collection holding just the bundled face.
///
/// GDI's `AddFontResourceExW(FR_PRIVATE)` and `AddFontMemResourceEx` are
/// the obvious route, but DirectWrite's system collection does not surface
/// process-private fonts, so `CreateTextFormat(.., None, ..)` never resolves
/// the family. A font set built from the file and wrapped in its own
/// collection is the supported path, and it keeps the face out of the
/// system-wide namespace.
/// Needs Factory5 for the builder that accepts a font file (Factory3's
/// builder only takes face references and existing sets).
fn bundled_collection(factory: &IDWriteFactory5) -> Option<IDWriteFontCollection> {
    // ponytail: path is relative to the process CWD; packaging should embed
    // the bytes and use an in-memory font file loader instead.
    let path = std::fs::canonicalize("assets/fonts/Inter.ttf").ok()?;
    let wpath: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: every handle is an owned COM reference kept alive by the
    // returned collection; `wpath` outlives the call.
    unsafe {
        let file = factory
            .CreateFontFileReference(PCWSTR(wpath.as_ptr()), None)
            .ok()?;
        let builder = factory.CreateFontSetBuilder().ok()?;
        builder.AddFontFile(&file).ok()?;
        let set = builder.CreateFontSet().ok()?;
        factory
            .CreateFontCollectionFromFontSet(&set)
            .ok()?
            .cast()
            .ok()
    }
}

/// Convert a scene rect to the D2D float rect. Shared by text and image drawing
/// so both hit the same bounds.
fn to_rect(rect: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: rect.x,
        top: rect.y,
        right: rect.max_x(),
        bottom: rect.max_y(),
    }
}

fn point((x, y): (f32, f32)) -> windows_numerics::Vector2 {
    windows_numerics::Vector2::new(x, y)
}

fn p2(p: [f32; 2]) -> windows_numerics::Vector2 {
    point((p[0], p[1]))
}

/// Transport glyph outlines as triangles inside `rect`. Each glyph is centered
/// with a slight optical shift right (play/next lean forward, like SF Symbols).
/// `pause` is a degenerate quad drawn as two skinny triangles.
fn glyph_tris(name: &str, rect: &Rect) -> Option<Vec<[[f32; 2]; 3]>> {
    let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
    let (cx, cy) = (x + w * 0.5, y + h * 0.5);
    let s = w.min(h) * 0.5; // half-size of the glyph box
    // Tab icons are defined on a 0..1 box.
    let px = |nx: f32| x + w * nx;
    let py = |ny: f32| y + h * ny;
    // Monoline pen. A dock icon is a STROKE, not a pile of quads: round caps
    // and round joins are what let 17 px line art read as an icon instead of as
    // scribble, and one stroke weight across the set is what makes nine icons
    // look like one family. `pen` strokes a polyline (open or closed) at `sw`
    // px; `circle` strokes a circle through the same pen.
    let dot = |v: &mut Vec<[[f32; 2]; 3]>, p: [f32; 2], r: f32| {
        let c = [px(p[0]), py(p[1])];
        let n = 10;
        for k in 0..n {
            let a0 = (k as f32) * std::f32::consts::TAU / n as f32;
            let a1 = ((k + 1) as f32) * std::f32::consts::TAU / n as f32;
            v.push([
                c,
                [c[0] + r * a0.cos(), c[1] + r * a0.sin()],
                [c[0] + r * a1.cos(), c[1] + r * a1.sin()],
            ]);
        }
    };
    let line = |v: &mut Vec<[[f32; 2]; 3]>, a: [f32; 2], b: [f32; 2], sw: f32| {
        let (ax, ay) = (px(a[0]), py(a[1]));
        let (bx, by) = (px(b[0]), py(b[1]));
        let dx = bx - ax;
        let dy = by - ay;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            return;
        }
        let (hx, hy) = (-dy / len * sw * 0.5, dx / len * sw * 0.5);
        v.push([[ax + hx, ay + hy], [bx + hx, by + hy], [bx - hx, by - hy]]);
        v.push([[ax + hx, ay + hy], [bx - hx, by - hy], [ax - hx, ay - hy]]);
    };
    let pen = |v: &mut Vec<[[f32; 2]; 3]>, pts: &[[f32; 2]], closed: bool, sw: f32| {
        let r = sw * 0.5;
        for i in 0..pts.len() {
            dot(v, pts[i], r);
            if i + 1 < pts.len() {
                line(v, pts[i], pts[i + 1], sw);
            } else if closed {
                line(v, pts[i], pts[0], sw);
            }
        }
    };
    let circle = |v: &mut Vec<[[f32; 2]; 3]>, cxn: f32, cyn: f32, r: f32, sw: f32| {
        let n = 20;
        let pts: Vec<[f32; 2]> = (0..n)
            .map(|i| {
                let a = (i as f32) * std::f32::consts::TAU / n as f32;
                [cxn + r * a.cos(), cyn + r * a.sin()]
            })
            .collect();
        pen(v, &pts, true, sw);
    };
    let tris: Vec<[[f32; 2]; 3]> = match name {
        "play" => vec![[
            [cx - s * 0.5 + s * 0.15, cy - s * 0.72],
            [cx - s * 0.5 + s * 0.15, cy + s * 0.72],
            [cx + s * 0.72, cy],
        ]],
        "pause" => vec![
            // left bar
            [
                [cx - s * 0.38, cy - s * 0.72],
                [cx - s * 0.38, cy + s * 0.72],
                [cx - s * 0.10, cy + s * 0.72],
            ],
            [
                [cx - s * 0.38, cy - s * 0.72],
                [cx - s * 0.10, cy + s * 0.72],
                [cx - s * 0.10, cy - s * 0.72],
            ],
            // right bar
            [
                [cx + s * 0.10, cy - s * 0.72],
                [cx + s * 0.10, cy + s * 0.72],
                [cx + s * 0.38, cy + s * 0.72],
            ],
            [
                [cx + s * 0.10, cy - s * 0.72],
                [cx + s * 0.38, cy + s * 0.72],
                [cx + s * 0.38, cy - s * 0.72],
            ],
        ],
        "prev" => vec![
            // bar at left, then left-pointing triangle
            [
                [cx - s * 0.62, cy - s * 0.72],
                [cx - s * 0.62, cy + s * 0.72],
                [cx - s * 0.36, cy + s * 0.72],
            ],
            [
                [cx - s * 0.62, cy - s * 0.72],
                [cx - s * 0.36, cy + s * 0.72],
                [cx - s * 0.36, cy - s * 0.72],
            ],
            [
                [cx + s * 0.62, cy - s * 0.72],
                [cx - s * 0.36, cy],
                [cx + s * 0.62, cy + s * 0.72],
            ],
        ],
        "next" => vec![
            [
                [cx + s * 0.36, cy - s * 0.72],
                [cx + s * 0.36, cy + s * 0.72],
                [cx + s * 0.62, cy + s * 0.72],
            ],
            [
                [cx + s * 0.36, cy - s * 0.72],
                [cx + s * 0.62, cy + s * 0.72],
                [cx + s * 0.62, cy - s * 0.72],
            ],
            [
                [cx - s * 0.62, cy - s * 0.72],
                [cx + s * 0.36, cy],
                [cx - s * 0.62, cy + s * 0.72],
            ],
        ],
        // Tab + corner icons. One stroke weight, round caps, geometry on the
        // 24 px grid taken from Lucide's paths (music, bar-chart-3, hourglass,
        // clipboard, droplet, zap, calendar, library, terminal, cog) — shapes
        // already proven legible at 16-24 px, which the previous
        // hand-approximated segments were not.
        "note" => {
            let sw = 1.15;
            let mut v = Vec::new();
            circle(&mut v, 0.25, 0.75, 0.125, sw);
            circle(&mut v, 0.75, 0.667, 0.125, sw);
            pen(
                &mut v,
                &[[0.375, 0.75], [0.375, 0.208], [0.875, 0.125], [0.875, 0.667]],
                false,
                sw,
            );
            v
        }
        "chart" => {
            let sw = 1.15;
            let mut v = Vec::new();
            pen(&mut v, &[[0.125, 0.125], [0.125, 0.875], [0.875, 0.875]], false, sw);
            pen(&mut v, &[[0.333, 0.708], [0.333, 0.583]], false, sw);
            pen(&mut v, &[[0.542, 0.708], [0.542, 0.208]], false, sw);
            pen(&mut v, &[[0.75, 0.708], [0.75, 0.375]], false, sw);
            v
        }
        "hourglass" => {
            let sw = 1.15;
            let mut v = Vec::new();
            pen(&mut v, &[[0.167, 0.083], [0.833, 0.083]], false, sw);
            pen(&mut v, &[[0.167, 0.917], [0.833, 0.917]], false, sw);
            // The walls pinch toward a waist but do NOT meet: one shared vertex
            // there round-joins into a blob at 17 px.
            pen(&mut v, &[[0.208, 0.083], [0.5, 0.44], [0.208, 0.917]], false, sw);
            pen(&mut v, &[[0.792, 0.083], [0.5, 0.56], [0.792, 0.917]], false, sw);
            v
        }
        "clipboard" => {
            let sw = 1.15;
            let mut v = Vec::new();
            pen(
                &mut v,
                &[
                    [0.333, 0.167],
                    [0.167, 0.167],
                    [0.167, 0.833],
                    [0.25, 0.917],
                    [0.75, 0.917],
                    [0.833, 0.833],
                    [0.833, 0.167],
                    [0.667, 0.167],
                ],
                false,
                sw,
            );
            pen(
                &mut v,
                &[
                    [0.375, 0.083],
                    [0.625, 0.083],
                    [0.667, 0.125],
                    [0.667, 0.25],
                    [0.333, 0.25],
                    [0.333, 0.125],
                    [0.375, 0.083],
                ],
                false,
                sw,
            );
            pen(&mut v, &[[0.333, 0.5], [0.667, 0.5]], false, sw);
            pen(&mut v, &[[0.333, 0.708], [0.667, 0.708]], false, sw);
            v
        }
        "drop" => {
            let sw = 1.15;
            let mut v = Vec::new();
            // A point on top, a round belly below: ONE closed pen path, not a
            // triangle roof glued onto a full circle.
            let mut pts = vec![[0.5, 0.083]];
            for i in 0..=8 {
                let a = (0.5 - (i as f32) / 8.0) * std::f32::consts::PI;
                pts.push([0.5 + 0.29 * a.sin(), 0.583 + 0.29 * a.cos()]);
            }
            pen(&mut v, &pts, true, sw);
            v
        }
        "bolt" => {
            let sw = 1.2;
            let mut v = Vec::new();
            // Lucide's zap outline.
            pen(
                &mut v,
                &[
                    [0.542, 0.083],
                    [0.125, 0.583],
                    [0.5, 0.583],
                    [0.458, 0.917],
                    [0.875, 0.417],
                    [0.5, 0.417],
                ],
                true,
                sw,
            );
            v
        }
        "calendar" => {
            let sw = 1.15;
            let mut v = Vec::new();
            pen(
                &mut v,
                &[[0.125, 0.208], [0.875, 0.208], [0.875, 0.875], [0.125, 0.875]],
                true,
                sw,
            );
            pen(&mut v, &[[0.125, 0.375], [0.875, 0.375]], false, sw);
            pen(&mut v, &[[0.333, 0.125], [0.333, 0.292]], false, sw);
            pen(&mut v, &[[0.667, 0.125], [0.667, 0.292]], false, sw);
            v
        }
        "tray" => {
            let sw = 1.15;
            let mut v = Vec::new();
            // Lucide's library: one leaning volume plus three upright ones.
            // Two outlined boxes on a rail were unreadable at 17 px.
            pen(&mut v, &[[0.667, 0.25], [0.833, 0.833]], false, sw);
            pen(&mut v, &[[0.5, 0.25], [0.5, 0.833]], false, sw);
            pen(&mut v, &[[0.333, 0.333], [0.333, 0.833]], false, sw);
            pen(&mut v, &[[0.167, 0.167], [0.167, 0.833]], false, sw);
            v
        }
        "terminal" => {
            let sw = 1.15;
            let mut v = Vec::new();
            // Lucide's terminal: a prompt and a rule. The old boxed version
            // read as a window.
            pen(&mut v, &[[0.167, 0.708], [0.417, 0.5], [0.167, 0.292]], false, sw);
            pen(&mut v, &[[0.5, 0.792], [0.833, 0.792]], false, sw);
            v
        }
        "settings" => {
            let sw = 1.15;
            let mut v = Vec::new();
            // A cog is an OUTLINE whose teeth break the rim. Spokes on a ring
            // read as a sun; a stroked 6-tooth polygon reads as a gear.
            let teeth = 6;
            let mut pts = Vec::new();
            for i in 0..teeth * 2 {
                let r = if i % 2 == 0 { 0.42 } else { 0.30 };
                let a = (i as f32) * std::f32::consts::TAU / (teeth * 2) as f32;
                pts.push([0.5 + r * a.cos(), 0.5 + r * a.sin()]);
            }
            pen(&mut v, &pts, true, sw);
            circle(&mut v, 0.5, 0.5, 0.10, sw);
            v
        }
        "pin" => {
            let sw = 1.2;
            let mut v = Vec::new();
            // Thumbtack seen from the side: cap, shoulders, then the taper to a
            // point. One closed outline so every join stays round.
            pen(
                &mut v,
                &[
                    [0.5, 0.92],
                    [0.17, 0.46],
                    [0.33, 0.30],
                    [0.33, 0.10],
                    [0.67, 0.10],
                    [0.67, 0.30],
                    [0.83, 0.46],
                ],
                true,
                sw,
            );
            v
        }
            _ => return None,
        };
    Some(tris)
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Release in reverse creation order; each field drops its own COM ref, so
        // the swapchain cannot outlive the composition target that displays it.
        self.release_chain();
        self.dwrite = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::scene::{Node, Scene};

    /// Every transport glyph must be a non-empty set of closed triangles that
    /// stay inside their rect. A vertex outside the rect paints over the
    /// neighbouring button; an empty set is a button the user can click but
    /// never see. Unknown names draw nothing.
    #[test]
    fn transport_glyphs_fit_their_rect() {
        let rect = Rect::new(100.0, 50.0, 22.0, 22.0);
        for name in ["play", "pause", "prev", "next"] {
            let tris = glyph_tris(name, &rect).unwrap_or_else(|| panic!("{name} has no geometry"));
            assert!(!tris.is_empty(), "{name} draws nothing");
            for tri in &tris {
                for &[x, y] in tri {
                    assert!(
                        (rect.x - 0.001..=rect.x + rect.w + 0.001).contains(&x)
                            && (rect.y - 0.001..=rect.y + rect.h + 0.001).contains(&y),
                        "{name} vertex ({x},{y}) escapes its rect"
                    );
                }
            }
        }
        assert!(glyph_tris("seek", &rect).is_none());
    }

    /// The pause glyph must read as TWO bars. Its inner edges used to sit
    /// 0.04·s apart (~0.4 px at 22 px), so the bars fused into one solid block
    /// at every size. Assert a real gap between the left and right bar.
    #[test]
    fn pause_glyph_has_a_visible_gap_between_its_bars() {
        let rect = Rect::new(100.0, 50.0, 22.0, 22.0);
        let tris = glyph_tris("pause", &rect).expect("pause geometry");
        let cx = rect.x + rect.w * 0.5;
        let left_max = tris
            .iter()
            .flat_map(|t| t.iter())
            .map(|p| p[0])
            .filter(|x| *x < cx)
            .fold(f32::MIN, f32::max);
        let right_min = tris
            .iter()
            .flat_map(|t| t.iter())
            .map(|p| p[0])
            .filter(|x| *x > cx)
            .fold(f32::MAX, f32::min);
        let gap = right_min - left_max;
        assert!(
            gap >= 1.5,
            "pause bars are {gap:.2} px apart at 22 px — they fuse into one block"
        );
    }

    /// Every tab icon must be a non-empty set of triangles that stay inside
    /// the rect they are drawn into, or a click paints over the neighbour.
    #[test]
    fn tab_icons_fit_their_rect() {
        let rect = Rect::new(100.0, 50.0, 11.0, 11.0);
        for name in [
            "note", "chart", "hourglass", "clipboard", "drop", "bolt", "calendar", "tray",
            "terminal",
        ] {
            let tris = glyph_tris(name, &rect).unwrap_or_else(|| panic!("{name} has no geometry"));
            assert!(!tris.is_empty(), "{name} draws nothing");
            for tri in &tris {
                for &[px, py] in tri {
                    assert!(
                        (rect.x - 0.001..=rect.x + rect.w + 0.001).contains(&px)
                            && (rect.y - 0.001..=rect.y + rect.h + 0.001).contains(&py),
                        "{name} vertex ({px},{py}) escapes the tab rect {rect:?}"
                    );
                }
            }
        }
    }

    /// The tab icons are MONOLINE art: strokes, not solid plates. Each must
    /// still put real ink on the box — an empty or hairline glyph is an
    /// invisible tab — and stay inside its rect.
    #[test]
    fn line_art_tab_icons_have_ink() {
        let rect = Rect::new(0.0, 0.0, 14.0, 14.0);
        for name in [
            "note",
            "chart",
            "hourglass",
            "clipboard",
            "drop",
            "bolt",
            "calendar",
            "tray",
            "terminal",
            "settings",
            "pin",
        ] {
            let tris = glyph_tris(name, &rect).unwrap_or_else(|| panic!("{name} has no geometry"));
            let area: f32 = tris
                .iter()
                .map(|t| {
                    let (a, b, c) = (t[0], t[1], t[2]);
                    ((b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])).abs() * 0.5
                })
                .sum();
            let cover = area / (rect.w * rect.h);
            assert!(
                (0.04..0.75).contains(&cover),
                "{name} covers {cover:.2} of its box — a monoline glyph should be stroke-thin but visible"
            );
        }
    }

    fn frame() -> Frame {
        Frame {
            size: (185.0, 32.0),
            scene: Scene::new()
                .push(Node::RoundRect {
                    rect: Rect::new(0.0, 0.0, 185.0, 32.0),
                    radii: CornerRadii::uniform(16.0),
                    fill: Rgba::BLACK,
                })
                .clone(),
        }
    }

    /// The bundled face is the whole point of the asset: if the collection
    /// cannot resolve "Inter", every proportional run silently falls back to
    /// Segoe UI and the app loses its reference look with no other symptom.
    #[test]
    fn bundled_inter_is_visible_to_directwrite() {
        let factory: IDWriteFactory =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.expect("dwrite factory");
        let factory5 = factory.cast::<IDWriteFactory5>().expect("factory5");
        let collection = bundled_collection(&factory5).expect("bundled collection");

        let name = wide("Inter");
        let (mut index, mut exists) = (0u32, windows::core::BOOL(0));
        unsafe {
            collection
                .FindFamilyName(PCWSTR(name.as_ptr()), &mut index, &mut exists)
                .expect("FindFamilyName");
        }
        assert!(exists.as_bool(), "bundled collection must expose Inter");
    }

    #[test]
    fn probe_starts_at_zero_presents() {
        let r = Renderer::new(NonNull::dangling()).expect("renderer");
        assert_eq!(r.probe().presents, 0);
        assert_eq!(r.probe().avg_frame_ms, None);
    }

    #[test]
    fn present_counts_frames_and_nodes() {
        let mut r = Renderer::new(NonNull::dangling()).expect("renderer");
        r.present(&frame(), 1.0, 1.0 / 60.0, 0.0, 0.0, true);
        r.present(&frame(), 1.0, 1.0 / 60.0, 0.0, 0.0, true);
        assert_eq!(r.probe().presents, 2);
        assert_eq!(r.probe().last_node_count, 1);
        assert!(r.probe().avg_frame_ms.is_some());
    }

    #[test]
    fn resize_is_safe_to_repeat() {
        let mut r = Renderer::new(NonNull::dangling()).expect("renderer");
        r.resize(185, 32);
        r.resize(185, 32);
        assert_eq!(r.probe().presents, 0);
    }

    #[test]
    fn degenerate_sizes_are_clamped_not_fatal() {
        let mut r = Renderer::new(NonNull::dangling()).expect("renderer");
        r.resize(0, 0);
        assert_eq!((r.width, r.height), (1, 1));
        r.present(&frame(), 1.0, 1.0 / 60.0, 0.0, 0.0, true);
        assert_eq!(
            r.probe().presents,
            1,
            "a 0x0 size must still count the frame"
        );
    }

    #[test]
    fn premultiply_matches_a_black_composite() {
        let c = premultiply(Rgba::rgba(0.5, 0.25, 1.0, 0.5));
        assert!((c.r - 0.25).abs() < 1e-6);
        assert!((c.g - 0.125).abs() < 1e-6);
        assert!((c.b - 0.5).abs() < 1e-6);
        assert!((c.a - 0.5).abs() < 1e-6);
        // Out-of-range channels must be clamped before use, not fed to D2D.
        let wild = premultiply(Rgba::rgba(2.0, -1.0, 0.5, 1.0));
        assert_eq!((wild.r, wild.g, wild.b), (1.0, 0.0, 0.5));
    }

    #[test]
    fn progress_is_clamped_including_nan() {
        assert_eq!(clamp_progress(-0.5), 0.0);
        assert_eq!(clamp_progress(1.5), 1.0);
        assert_eq!(clamp_progress(f32::NAN), 0.0);
        assert_eq!(clamp_progress(0.25), 0.25);
    }

    #[test]
    fn format_keys_distinguish_size_weight_and_tabular() {
        let numeric = TextStyle::numeric(12.0);
        assert_eq!(format_key(&numeric), format_key(&TextStyle::numeric(12.0)));
        assert_ne!(format_key(&numeric), format_key(&TextStyle::numeric(13.0)));
        assert_ne!(format_key(&numeric), format_key(&TextStyle::title()));
        let mut proportional = numeric.clone();
        proportional.tabular = false;
        assert_ne!(format_key(&numeric), format_key(&proportional));
    }

    #[test]
    fn corner_starts_match_the_squircle_segments() {
        // The renderer draws the straight edge from segment N's endpoint to
        // segment N+1's start; if these disagree the outline has a visible kink.
        let rect = Rect::new(4.0, 6.0, 200.0, 80.0);
        let radii = CornerRadii::uniform(20.0);
        let segments = squircle_segments(&rect, radii);
        for index in 0..segments.len() {
            let start = corner_start(&rect, radii, index);
            let previous = segments[(index + segments.len() - 1) % segments.len()].to;
            assert!(
                (start.0 - previous.0).abs() < 1e-3 || (start.1 - previous.1).abs() < 1e-3,
                "corner {index} does not share an axis with the previous endpoint"
            );
        }
    }

    #[test]
    fn measure_falls_back_and_stays_monotonic() {
        // The UI-level approximation must be sane without a GPU and grow
        // with the string; DirectWrite takes over when it is live.
        let style = TextStyle::numeric(12.0);
        let short = crate::ui::text::measure("00:00", &style);
        let long = crate::ui::text::measure("00:00:00", &style);
        assert!(short > 0.0, "a clock must measure wider than nothing");
        assert!(long > short, "more characters must measure wider");
    }

    #[test]
    fn outline_is_one_continuous_figure() {
        let rect = Rect::new(0.0, 0.0, 231.0, 40.0);
        let steps = outline_steps(&rect, CornerRadii::uniform(12.0));

        assert!(
            matches!(steps[0], OutlineStep::Begin(_)),
            "must open a figure"
        );
        assert!(
            matches!(steps[steps.len() - 1], OutlineStep::End),
            "must close the figure"
        );
        let begins = steps
            .iter()
            .filter(|s| matches!(s, OutlineStep::Begin(_)))
            .count();
        let ends = steps
            .iter()
            .filter(|s| matches!(s, OutlineStep::End))
            .count();
        assert_eq!((begins, ends), (1, 1), "exactly one figure");

        // Four curves, each joined to the next by a straight edge: the third
        // operation is a Line only when a curve precedes it.
        let curves = steps
            .iter()
            .filter(|s| matches!(s, OutlineStep::Bezier(..)))
            .count();
        let lines = steps
            .iter()
            .filter(|s| matches!(s, OutlineStep::Line(_)))
            .count();
        assert_eq!(
            (curves, lines),
            (4, 3),
            "4 curves, 3 joins; closure is implicit"
        );

        // Every curve endpoint must meet the next join or, for the last, the start.
        let start = match steps[0] {
            OutlineStep::Begin(p) => p,
            _ => unreachable!(),
        };
        let mut last_to = start;
        let mut joins = 0;
        for step in &steps {
            match step {
                OutlineStep::Line(p) => {
                    let at = last_to;
                    assert!(
                        (at.0 - p.0).abs() < 0.001 || (at.1 - p.1).abs() < 0.001,
                        "join must start where the previous curve ended: {at:?} vs {p:?}"
                    );
                    joins += 1;
                    last_to = *p;
                }
                OutlineStep::Bezier(_, _, to) => last_to = *to,
                _ => {}
            }
        }
        // Closure: final endpoint connects back along an edge to the start.
        assert!(
            (last_to.0 - start.0).abs() < 0.001 || (last_to.1 - start.1).abs() < 0.001,
            "closing edge must be axis-aligned: {last_to:?} vs {start:?}"
        );
        assert_eq!(joins, 3);
    }
}
