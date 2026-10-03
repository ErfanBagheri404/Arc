//! DirectComposition/D3D11 renderer with a safe degraded mode.
//!
//! The renderer owns the GPU objects, but never owns application state. A device
//! failure is non-fatal: counters continue working and the window remains usable.
//!
//! Contract (do not change without updating `app::run`):
//! - `new(hwnd)` once, after the window exists.
//! - `resize(physical_w, physical_h)` whenever the island geometry changes.
//! - `present(frame, dpi_scale, dt_seconds)` exactly once per frame the app loop
//!   asks for; the renderer must not present on its own.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;

use windows::core::{Error, Interface, BOOL, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_WINDING, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
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
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_BOLD,
    DWRITE_FONT_WEIGHT_REGULAR, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1, IDXGISwapChain3,
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use crate::core::geom::{squircle_segments, CornerRadii, Rect, Rgba};
use crate::core::scene::{Align, Frame, Node, TextStyle, Weight};

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

/// Cache key for a DirectWrite text format. Sizes are stored as bits so two sizes
/// 12 and 12.0000001 do not collide through float equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FormatKey {
    size_bits: u32,
    weight: i32,
    tabular: bool,
}

/// D2D renders onto a premultiplied surface, so a brush colour must be
/// premultiplied by its own alpha against black — which is exactly
/// `(r*a, g*a, b*a, a)`.
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
    value.encode_utf16().collect()
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
    }
}

/// The point each of the four corner segments departs from. Must mirror the
/// start of the matching segment in [`squircle_segments`], or the outline closes
/// with visible kinks between corners.
fn corner_start(rect: &Rect, radii: CornerRadii, index: usize) -> (f32, f32) {
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
) -> windows::core::Result<ID2D1RenderTarget> {
    let surface: IDXGISurface = step!("GetBuffer", unsafe { swapchain.GetBuffer(0) });
    let props = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
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
    formats: HashMap<FormatKey, IDWriteTextFormat>,
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
            formats: HashMap::new(),
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
        // Explicit sRGB rather than the driver's default (risks T3).
        if let Ok(swapchain3) = swapchain.cast::<IDXGISwapChain3>() {
            let _ = unsafe { swapchain3.SetColorSpace1(DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709) };
            let _ = unsafe { swapchain3.SetMaximumFrameLatency(1) };
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

        self.swapchain = Some(swapchain);
        self.dcomp = Some(dcomp);
        self.dcomp_target = Some(dcomp_target);
        self.visual = Some(visual);
        self.d2d_factory = Some(d2d);
        self.dwrite = Some(dwrite);
        Ok(())
    }

    /// Recreate the swapchain for a new size. Idempotent for an unchanged size.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height && !self.degraded {
            return;
        }
        self.rebuild(width, height);
    }

    /// Draw one frame and present it.
    pub fn present(&mut self, frame: &Frame, _scale: f32, dt: f32) {
        self.probe.presents = self.probe.presents.saturating_add(1);
        self.probe.last_node_count = frame.scene.count();
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
        let target = match acquire_target(&d2d, &swapchain) {
            Ok(pair) => pair,
            Err(error) => {
                log::error!("arc: could not bind a render target to the back buffer ({error})");
                self.degraded = true;
                return;
            }
        };

        unsafe { target.BeginDraw() };
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
        frame.scene.walk(&mut |node| self.draw_node(&target, node));
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

        // Sync interval 1: the loop already parks when idle, so a queued frame
        // should not be dropped on the floor before it is seen.
        let result = unsafe { swapchain.Present(1, Default::default()) };
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
            } => {
                // Phase 2 wires WIC decoding in; until then every handle renders as a
                // placeholder so the layout is still reviewable.
                let _ = handle;
                self.fill_squircle(target, *rect, *radii, Rgba::rgb(0.16, 0.16, 0.18));
            }
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
            Node::Glyph { rect, color, .. } => {
                // Real glyph outlines land in Phase 2; a flat fill keeps the slot
                // visible and correctly sized.
                self.fill_squircle(target, *rect, CornerRadii::uniform(2.0), *color)
            }
            Node::Group { children, .. } => {
                for child in children {
                    self.draw_node(target, child);
                }
            }
        }
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
        let Some(factory) = self.d2d_factory.clone() else {
            return;
        };
        let Ok(path) = (unsafe { factory.CreatePathGeometry() }) else {
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
        unsafe { target.FillGeometry(&path, &brush, None) };
    }

    fn brush(&self, target: &ID2D1RenderTarget, color: Rgba) -> Option<ID2D1SolidColorBrush> {
        let color = premultiply(color);
        // Brushes are cheap relative to a frame, and caching by colour would need
        // a float-keyed map for no measurable win at this node count.
        unsafe { target.CreateSolidColorBrush(&color, None).ok() }
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
        let utf16 = wide(text);
        unsafe {
            target.DrawText(
                &utf16,
                &format,
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        }
    }

    fn text_format(&mut self, style: &TextStyle) -> Option<IDWriteTextFormat> {
        let key = format_key(style);
        if let Some(cached) = self.formats.get(&key) {
            return Some(cached.clone());
        }
        let factory = self.dwrite.as_ref()?;
        let family = wide("Segoe UI Variable Display");
        let format = unsafe {
            factory
                .CreateTextFormat(
                    PCWSTR(family.as_ptr()),
                    None,
                    windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT(key.weight),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    style.size.max(1.0),
                    PCWSTR::null(),
                )
                .ok()?
        };
        unsafe {
            let _ = format.SetTextAlignment(match style.align {
                Align::Start => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::End => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            // Vertically centred: the UI hands out boxes, not baselines.
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
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

fn point((x, y): (f32, f32)) -> windows_numerics::Vector2 {
    windows_numerics::Vector2::new(x, y)
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

    #[test]
    fn probe_starts_at_zero_presents() {
        let r = Renderer::new(NonNull::dangling()).expect("renderer");
        assert_eq!(r.probe().presents, 0);
        assert_eq!(r.probe().avg_frame_ms, None);
    }

    #[test]
    fn present_counts_frames_and_nodes() {
        let mut r = Renderer::new(NonNull::dangling()).expect("renderer");
        r.present(&frame(), 1.0, 1.0 / 60.0);
        r.present(&frame(), 1.0, 1.0 / 60.0);
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
        r.present(&frame(), 1.0, 1.0 / 60.0);
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
