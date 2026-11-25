use std::{
    mem::size_of,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM},
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        HiDpi::GetDpiForWindow,
        WindowsAndMessaging::*,
    },
};
use windows_sys::core::{PCWSTR, w};

use crate::{
    layout::{Id, Rect},
    native,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Palette {
    line: [u8; 4],
    rim: [u8; 4],
    fill_alpha: u8,
}

impl Palette {
    const NORMAL: Self = Self {
        line: [171, 195, 218, 217],
        rim: [51, 72, 94, 88],
        fill_alpha: 0,
    };

    fn current() -> Self {
        unsafe {
            let mut hc = HIGHCONTRASTW {
                cbSize: size_of::<HIGHCONTRASTW>() as u32,
                ..Default::default()
            };
            SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, (&raw mut hc).cast(), 0);
            if hc.dwFlags & HCF_HIGHCONTRASTON == 0 {
                return Self::NORMAL;
            }
            let system = |index| {
                let [r, g, b, _] = GetSysColor(index).to_le_bytes();
                [r, g, b, 255]
            };
            Self {
                line: system(COLOR_HIGHLIGHT),
                rim: system(COLOR_HIGHLIGHTTEXT),
                fill_alpha: 0,
            }
        }
    }
}

pub(super) fn resize_edge_color() -> u32 {
    let [r, g, b, _] = Palette::current().line;
    u32::from_le_bytes([r, g, b, 0])
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Shape {
    w: i32,
    h: i32,
    dpi: u32,
    radius_dip: i32,
    thickness: u8,
    inset: bool,
    palette: Palette,
}

impl Shape {
    fn drop_preview(rect: Rect, thickness: u8, dpi: u32, mut palette: Palette) -> Self {
        palette.fill_alpha = palette.line[3] / 6;
        Self {
            w: rect.w,
            h: rect.h,
            dpi,
            radius_dip: 8,
            thickness: thickness.max(1),
            inset: true,
            palette,
        }
    }

    fn scale(self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn radius(self) -> f32 {
        (self.radius_dip as f32 * self.scale()).min(self.w.min(self.h) as f32 / 2.0)
    }

    // These four non-overlapping strips leave the centre physically empty.
    // Coordinates are relative to the actual visible window frame.
    fn strips(self, work: Rect) -> [Rect; 4] {
        // A flush layout draws the entire outline inside the frame so it
        // cannot paint over a neighboring tile.
        let outer = if self.inset {
            0
        } else {
            (1.25 * self.scale() + 0.5).ceil() as i32
        };
        let offset = if self.inset { 1.25 } else { 0.0 };
        let inner = ((f32::from(self.thickness) + offset) * self.scale() + 0.5).ceil() as i32;
        let corner = (self.radius().ceil() as i32 + inner).min(self.h / 2);
        let span = self.w + 2 * outer;
        let side_width = outer + inner;
        [
            Rect {
                x: -outer,
                y: -outer,
                w: span,
                h: corner + outer,
            },
            Rect {
                x: -outer,
                y: self.h - corner,
                w: span,
                h: corner + outer,
            },
            Rect {
                x: -outer,
                y: corner,
                w: side_width,
                h: self.h - 2 * corner,
            },
            Rect {
                x: self.w - inner,
                y: corner,
                w: side_width,
                h: self.h - 2 * corner,
            },
        ]
        .map(|r| {
            let x = r.x.max(work.x);
            let y = r.y.max(work.y);
            Rect {
                x,
                y,
                w: (r.x + r.w).min(work.x + work.w).saturating_sub(x).max(0),
                h: (r.y + r.h).min(work.y + work.h).saturating_sub(y).max(0),
            }
        })
    }

    // Analytic coverage at pixel centres. Distance zero follows the DWM
    // contour; the main stroke straddles it, with a translucent lip outside.
    fn pixel(self, x: i32, y: i32) -> u32 {
        let radius = self.radius();
        let local_x = x as f32 + 0.5;
        let local_y = y as f32 + 0.5;
        let dx = radius - local_x.min(self.w as f32 - local_x);
        let dy = radius - local_y.min(self.h as f32 - local_y);
        let distance = if dx > 0.0 && dy > 0.0 {
            (dx * dx + dy * dy).sqrt() - radius
        } else {
            dx.max(dy) - radius
        };
        let coverage = |limit: f32| (0.5 + limit * self.scale() - distance).clamp(0.0, 1.0);
        let offset = if self.inset { 1.25 } else { 0.0 };
        let edge = coverage(0.35 - offset);
        let interior = coverage(0.05 - f32::from(self.thickness) - offset);
        let ink = (edge - interior) * f32::from(self.palette.line[3])
            + interior * f32::from(self.palette.fill_alpha);
        let rim = (coverage(1.25 - offset) - edge) * f32::from(self.palette.rim[3]);
        let channel = |n: usize| {
            ((f32::from(self.palette.line[n]) * ink + f32::from(self.palette.rim[n]) * rim) / 255.0)
                .round() as u8
        };
        u32::from_le_bytes([
            channel(2),
            channel(1),
            channel(0),
            (ink + rim).round() as u8,
        ])
    }

    fn paint(self, strip: Rect, horizontal: bool, pixels: &mut [u32]) {
        let width = strip.w as usize;
        if !horizontal {
            for (x, p) in pixels[..width].iter_mut().enumerate() {
                *p = self.pixel(strip.x + x as i32, strip.y);
            }
            for y in 1..strip.h as usize {
                pixels.copy_within(0..width, y * width);
            }
            return;
        }
        let corner = self
            .radius()
            .max((f32::from(self.thickness) + 1.5) * self.scale())
            .ceil() as i32
            + 1;
        let start = (corner - strip.x).clamp(0, strip.w) as usize;
        let end = (self.w - corner - strip.x).clamp(start as i32, strip.w) as usize;
        for (y, row) in pixels.chunks_exact_mut(width).enumerate() {
            let y = strip.y + y as i32;
            row[start..end].fill(self.pixel(self.w / 2, y));
            for (x, p) in row[..start].iter_mut().enumerate() {
                *p = self.pixel(strip.x + x as i32, y);
            }
            for (x, p) in row[end..].iter_mut().enumerate() {
                *p = self.pixel(strip.x + (end + x) as i32, y);
            }
        }
    }
}

#[derive(Default)]
pub struct FocusOutline {
    windows: [HWND; 4],
    raster: Option<(Shape, [Rect; 4])>,
    target: Option<(Id, Rect, bool)>,
}

impl FocusOutline {
    pub fn show(&mut self, id: Id, thickness: u8, inset: bool) {
        if thickness == 0 {
            self.hide();
            return;
        }
        let Some((frame, work)) = native::outline_frame(id) else {
            self.hide();
            return;
        };
        if self.windows[0].is_null() && !self.create() {
            self.hide();
            return;
        }
        let shape = Shape {
            w: frame.w,
            h: frame.h,
            dpi: unsafe { GetDpiForWindow(self.windows[0]) }.max(96),
            thickness,
            inset,
            radius_dip: native::corner_radius_dip(id),
            palette: Palette::current(),
        };
        let strips = shape.strips(Rect {
            x: work.x - frame.x,
            y: work.y - frame.y,
            ..work
        });
        let redraw = self.raster != Some((shape, strips));
        // Equal ranks between our own strips do not matter; their pixels never overlap.
        let mut anchor = unsafe { GetWindow(id as HWND, GW_HWNDPREV) };
        let mut adjacent = 0;
        while adjacent < self.windows.len() && self.windows.contains(&anchor) {
            adjacent += 1;
            anchor = unsafe { GetWindow(anchor, GW_HWNDPREV) };
        }
        let is_topmost =
            |hwnd| unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST != 0 };
        let topmost = is_topmost(id as HWND);
        if !redraw && self.target == Some((id, frame, topmost)) && adjacent == self.windows.len() {
            return;
        }
        // An explicit HWND insertion inherits its z-order band. At a band boundary
        // position each strip at the head of the target's own band instead.
        let band_boundary = anchor.is_null() || topmost != is_topmost(anchor);
        if band_boundary {
            anchor = if topmost {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            };
        }
        for (i, (&hwnd, strip)) in self.windows.iter().zip(strips).enumerate() {
            if strip.w == 0 || strip.h == 0 {
                unsafe { ShowWindow(hwnd, SW_HIDE) };
                continue;
            }
            let position = POINT {
                x: frame.x + strip.x,
                y: frame.y + strip.y,
            };
            if redraw && !upload(hwnd, shape, strip, i < 2, position) {
                self.hide();
                return;
            }
            let flags = SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW | SWP_NOSIZE;
            if unsafe { SetWindowPos(hwnd, anchor, position.x, position.y, 0, 0, flags) } == 0 {
                self.hide();
                return;
            }
            if !band_boundary {
                anchor = hwnd;
            }
        }
        self.raster = Some((shape, strips));
        self.target = Some((id, frame, topmost));
    }

    fn create(&mut self) -> bool {
        unsafe {
            let class = overlay_class(w!("Rustile.FocusOutline"));
            for hwnd in &mut self.windows {
                *hwnd = overlay_window(&class);
                if hwnd.is_null() {
                    return false;
                }
            }
            // Only one of the four windows forwards broadcast theme messages.
            SetWindowLongPtrW(self.windows[0], GWLP_USERDATA, 1);
            true
        }
    }

    pub fn hide(&mut self) {
        for hwnd in &mut self.windows {
            if !hwnd.is_null() {
                unsafe { DestroyWindow(*hwnd) };
                *hwnd = null_mut();
            }
        }
        self.target = None;
        self.raster = None;
    }
}

impl Drop for FocusOutline {
    fn drop(&mut self) {
        self.hide();
    }
}

#[derive(Default)]
pub struct DropPreview {
    hwnd: HWND,
    raster: Option<Shape>,
    visible_at: Option<(Id, Rect)>,
}

impl DropPreview {
    pub fn show(&mut self, source: Id, rect: Rect, thickness: u8) {
        if source == 0 || rect.w <= 0 || rect.h <= 0 {
            self.hide();
            return;
        }
        // The grid lives on the primary monitor even when the dragged window
        // straddles a differently scaled display.
        let shape = Shape::drop_preview(rect, thickness, native::system_dpi(), Palette::current());
        if self.raster == Some(shape) && self.visible_at == Some((source, rect)) {
            return;
        }
        if self.hwnd.is_null() {
            self.hwnd = overlay_window(&overlay_class(w!("Rustile.DropPreview")));
            if self.hwnd.is_null() {
                return;
            }
        }
        let pos = POINT {
            x: rect.x,
            y: rect.y,
        };
        if self.raster != Some(shape) {
            let local = Rect { x: 0, y: 0, ..rect };
            if !upload(self.hwnd, shape, local, true, pos) {
                self.hide();
                return;
            }
            self.raster = Some(shape);
        }
        // Upload while hidden on the first show. Reposition an identical
        // surface without allocating another full-tile bitmap.
        let flags = SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW | SWP_NOSIZE;
        if unsafe { SetWindowPos(self.hwnd, HWND_TOPMOST, pos.x, pos.y, 0, 0, flags) } == 0 {
            self.hide();
            return;
        }
        self.visible_at = Some((source, rect));
    }

    pub fn hide(&mut self) {
        if self.visible_at.take().is_some() {
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }
}

impl Drop for DropPreview {
    fn drop(&mut self) {
        if !self.hwnd.is_null() {
            unsafe { DestroyWindow(self.hwnd) };
        }
    }
}

fn overlay_class(name: PCWSTR) -> WNDCLASSW {
    unsafe {
        let class = WNDCLASSW {
            hInstance: GetModuleHandleW(null()),
            lpszClassName: name,
            lpfnWndProc: Some(outline_proc),
            ..Default::default()
        };
        RegisterClassW(&class);
        class
    }
}

fn overlay_window(class: &WNDCLASSW) -> HWND {
    unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.lpszClassName,
            w!(""),
            WS_POPUP | WS_DISABLED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            class.hInstance,
            null(),
        )
    }
}

fn upload(hwnd: HWND, shape: Shape, strip: Rect, horizontal: bool, position: POINT) -> bool {
    unsafe {
        let dc = CreateCompatibleDC(null_mut());
        if dc.is_null() {
            return false;
        }
        let mut bits = null_mut();
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: strip.w,
                biHeight: -strip.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        let mut ok = false;
        if !bitmap.is_null() && !bits.is_null() {
            let old = SelectObject(dc, bitmap);
            if !old.is_null() {
                let pixels = std::slice::from_raw_parts_mut(
                    bits.cast(),
                    strip.w as usize * strip.h as usize,
                );
                shape.paint(strip, horizontal, pixels);
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let size = SIZE {
                    cx: strip.w,
                    cy: strip.h,
                };
                ok = UpdateLayeredWindow(
                    hwnd,
                    null_mut(),
                    &position,
                    &size,
                    dc,
                    &POINT::default(),
                    0,
                    &blend,
                    ULW_ALPHA,
                ) != 0;
                SelectObject(dc, old);
            }
        }
        if !bitmap.is_null() {
            DeleteObject(bitmap);
        }
        DeleteDC(dc);
        ok
    }
}

unsafe extern "system" fn outline_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE
            if unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } == 1 =>
        {
            unsafe {
                PostThreadMessageW(
                    GetCurrentThreadId(),
                    crate::runtime::OUTLINE_SETTINGS_CHANGED,
                    0,
                    0,
                )
            };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_tints_the_destination_inside_a_rounded_visible_stroke() {
        let tile = Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 60,
        };
        let contrast = Palette {
            line: [255, 255, 0, 255],
            rim: [0, 0, 0, 255],
            fill_alpha: 0,
        };
        for dpi in [96, 144, 192] {
            for palette in [Palette::NORMAL, contrast] {
                let shape = Shape::drop_preview(tile, 0, dpi, palette);
                let mut pixels = vec![0; (tile.w * tile.h) as usize];
                shape.paint(tile, true, &mut pixels);
                let at = |x: i32, y: i32| pixels[(y * tile.w + x) as usize].to_le_bytes();
                assert_eq!(at(0, 0), [0; 4]);
                assert_eq!(at(79, 59), [0; 4]);
                let fill = at(40, 30);
                assert_eq!(fill[3], palette.line[3] / 6);
                for (channel, color) in fill[..3].iter().rev().zip(palette.line) {
                    assert_eq!(
                        u16::from(*channel),
                        (u16::from(color) * u16::from(fill[3]) + 127) / 255
                    );
                }
                assert!(at(40, (dpi / 96) as i32)[3] > 3 * fill[3]);
                assert!(
                    (0..tile.w / 2)
                        .map(|x| at(x, 0)[3])
                        .any(|a| a > 0 && a < at(40, 0)[3])
                );
                assert!(pixels.iter().all(|pixel| {
                    let [b, g, r, a] = pixel.to_le_bytes();
                    r <= a && g <= a && b <= a
                }));
            }
        }
    }

    #[test]
    fn thick_flush_outlines_grow_inward_at_both_dpis() {
        for dpi in [96, 144] {
            let thin = Shape {
                w: 200,
                h: 120,
                dpi,
                radius_dip: 8,
                thickness: 1,
                inset: true,
                palette: Palette::NORMAL,
            };
            let thick = Shape {
                thickness: 4,
                ..thin
            };
            assert_eq!(thick.pixel(70, -1), 0);
            assert_eq!(thick.pixel(-1, 40), 0);
            let inward = (3.0 * thick.scale()) as i32;
            assert_eq!(thin.pixel(70, inward), 0);
            assert_ne!(thick.pixel(70, inward), 0);
            assert_eq!(thick.pixel(0, 0), 0);
        }
    }

    #[test]
    fn contour_tracks_square_small_and_round_windows_at_both_dpis() {
        for dpi in [96, 144] {
            let shape = Shape {
                w: 240,
                h: 180,
                dpi,
                thickness: 1,
                inset: false,
                radius_dip: 8,
                palette: Palette::NORMAL,
            };
            let small = Shape {
                radius_dip: 4,
                ..shape
            };
            let square = Shape {
                radius_dip: 0,
                ..shape
            };
            assert_eq!(shape.pixel(0, 0), 0);
            assert_ne!(square.pixel(0, 0), 0);
            let diagonal = if dpi == 96 { 1 } else { 2 };
            assert!(small.pixel(diagonal, diagonal) != shape.pixel(diagonal, diagonal));
            assert_ne!(shape.pixel(80, 0), 0);
            assert_eq!(shape.pixel(80, 5), 0);
            assert_eq!(shape.pixel(80, -5), 0);
            let curve: Vec<_> = (0..16)
                .map(|x| shape.pixel(x, 1).to_le_bytes()[3])
                .collect();
            assert!(curve.iter().any(|&a| a > 0 && a < 100));
            for pixel in (0..25).flat_map(|y| (0..25).map(move |x| shape.pixel(x, y))) {
                let [b, g, r, a] = pixel.to_le_bytes();
                assert!(b <= a && g <= a && r <= a);
            }
        }
    }

    #[test]
    fn perimeter_strips_match_the_curve_including_clipped_screen_edges() {
        for dpi in [96, 144] {
            let shape = Shape {
                w: 312,
                h: 226,
                dpi,
                thickness: 1,
                inset: false,
                radius_dip: 8,
                palette: Palette::NORMAL,
            };
            for work in [
                Rect {
                    x: -100,
                    y: -100,
                    w: 640,
                    h: 480,
                },
                Rect {
                    x: 0,
                    y: 0,
                    w: 312,
                    h: 226,
                },
            ] {
                let strips = shape.strips(work);
                let mut samples = 0;
                for (i, strip) in strips.into_iter().enumerate() {
                    let mut bitmap = vec![0; (strip.w * strip.h) as usize];
                    shape.paint(strip, i < 2, &mut bitmap);
                    for y in 0..strip.h {
                        for x in 0..strip.w {
                            assert_eq!(
                                bitmap[(y * strip.w + x) as usize],
                                shape.pixel(strip.x + x, strip.y + y)
                            );
                        }
                    }
                    samples += bitmap.len();
                }
                assert!(samples < (shape.w * shape.h / 4) as usize);
            }
        }
    }
}
