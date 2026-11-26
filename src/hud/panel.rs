use std::{ptr::null_mut, slice};

use windows_sys::Win32::{
    Foundation::{COLORREF, HWND, POINT, RECT, SIZE},
    Graphics::Gdi::*,
    UI::WindowsAndMessaging::*,
};

use super::guide::Guide;
use crate::layout::Rect;
use crate::tray::{Colors, Fonts, load_app_icon};

struct MemoryDc(HDC);

impl Drop for MemoryDc {
    fn drop(&mut self) {
        unsafe { DeleteDC(self.0) };
    }
}

pub(super) struct Panel {
    pub(super) width: i32,
    pub(super) height: i32,
    dc: MemoryDc,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc.0, self.previous);
            DeleteObject(self.bitmap);
        }
    }
}

struct Label {
    text: Vec<u16>,
    font: HFONT,
    color: COLORREF,
    bounds: RECT,
    flags: u32,
}

impl Label {
    fn new(text: &str, font: HFONT, color: COLORREF) -> Self {
        Self {
            text: text.encode_utf16().collect(),
            font,
            color,
            bounds: RECT::default(),
            flags: DT_LEFT | DT_NOPREFIX | DT_WORDBREAK,
        }
    }

    fn draw(&self, dc: HDC, bounds: &mut RECT, flags: u32) {
        unsafe {
            let old = SelectObject(dc, self.font);
            DrawTextW(
                dc,
                self.text.as_ptr(),
                self.text.len() as i32,
                bounds,
                flags,
            );
            SelectObject(dc, old);
        }
    }

    fn measure(&self, dc: HDC, width: i32) -> SIZE {
        let mut rect = RECT {
            right: width.max(1),
            ..RECT::default()
        };
        self.draw(dc, &mut rect, self.flags | DT_CALCRECT);
        SIZE {
            cx: rect.right - rect.left,
            cy: rect.bottom - rect.top,
        }
    }
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

struct PanelLayout {
    width: i32,
    height: i32,
    labels: Vec<Label>,
    badges: Vec<RECT>,
    divider: Option<RECT>,
    logo: RECT,
}

impl PanelLayout {
    fn measure(
        guide: &Guide,
        work: Rect,
        dc: &MemoryDc,
        fonts: &Fonts,
        colors: Colors,
        p: impl Fn(i32) -> i32,
    ) -> Self {
        let pad = p(17).min(work.w / 10);
        let gap = p(4);
        let cap_pad = p(7);
        let icon = p(20);
        let mut title = Label::new(Guide::title(), fonts.heading, colors.text);
        let mut width = p(328).max(2 * pad + icon + p(9) + title.measure(dc.0, work.w).cx);
        let mut rows = Vec::new();
        for (index, row) in guide.rows.iter().enumerate() {
            let footer = index + 1 == guide.rows.len();
            let action = Label::new(
                &row.action,
                fonts.regular,
                if footer {
                    colors.secondary
                } else {
                    colors.text
                },
            );
            let mut caps = Vec::new();
            for (connector, key) in row.keycaps() {
                if !connector.is_empty() {
                    let mark = Label::new(connector, fonts.small, colors.secondary);
                    let measured = mark.measure(dc.0, work.w).cx;
                    caps.push((mark, measured, 0));
                }
                let cap = Label::new(&key, fonts.small, colors.text);
                let measured = cap.measure(dc.0, work.w).cx + 2 * cap_pad;
                caps.push((cap, measured, cap_pad));
            }
            let cap_width =
                caps.iter().map(|(_, w, _)| w).sum::<i32>() + gap * (caps.len() as i32 - 1).max(0);
            let label_width = action.measure(dc.0, work.w).cx;
            width = width.max(2 * pad + label_width + p(20) + cap_width);
            rows.push((action, label_width, caps, cap_width));
        }
        let width = width.min(work.w);
        let inner = (width - 2 * pad).max(1);
        let title_x = pad + icon + p(9);
        let title_height = title.measure(dc.0, (width - title_x - pad).max(1)).cy;
        let header_y = p(14);
        let header_height = icon.max(title_height);
        title.bounds = rect(
            title_x,
            header_y + (header_height - title_height) / 2,
            width - title_x - pad,
            title_height,
        );
        let mut labels = vec![title];
        let mut badges = Vec::new();
        let mut divider = None;
        let mut y = header_y + header_height + p(8);
        let count = rows.len();
        for (index, (mut action, label_width, caps, cap_width)) in rows.into_iter().enumerate() {
            if index + 1 == count && index > 0 {
                y += p(5);
                divider = Some(rect(pad, y, inner, p(1).max(1)));
                y += p(6);
            }
            let stacked = !caps.is_empty() && label_width + cap_width + p(12) > inner;
            let label_space = if stacked || caps.is_empty() {
                inner
            } else {
                inner - cap_width - p(12)
            };
            let label_height = action.measure(dc.0, label_space).cy;
            let mut key_x = if stacked {
                pad
            } else {
                width - pad - cap_width
            };
            let mut key_y = if stacked { y + label_height + p(3) } else { y };
            let mut band_height = 0;
            let mut caps = caps.into_iter().peekable();
            while let Some((mut cap, natural_w, inset)) = caps.next() {
                let w = natural_w.min(inner);
                // Keep a connector with the key it introduces when the row wraps.
                let group_w = if inset == 0 {
                    caps.peek().map_or(w, |(_, next, _)| w + gap + next)
                } else {
                    w
                };
                if stacked && key_x > pad && key_x + group_w > width - pad {
                    key_y += band_height + p(3);
                    key_x = pad;
                    band_height = 0;
                }
                let text_w = (w - 2 * inset).max(1);
                cap.flags = DT_CENTER | DT_WORDBREAK | DT_NOPREFIX;
                let text_h = cap.measure(dc.0, text_w).cy;
                let h = p(23).max(text_h + p(4));
                cap.bounds = rect(key_x + inset, key_y + (h - text_h) / 2, text_w, text_h);
                if inset != 0 {
                    badges.push(rect(key_x, key_y, w, h));
                }
                labels.push(cap);
                key_x += w + gap;
                band_height = band_height.max(h);
            }
            let bottom = (y + label_height).max(key_y + band_height);
            action.bounds = rect(
                pad,
                if stacked {
                    y
                } else {
                    y + (bottom - y - label_height) / 2
                },
                label_space,
                label_height,
            );
            labels.push(action);
            y = bottom + p(6);
        }
        Self {
            width,
            height: (y + p(9)).min(work.h),
            labels,
            badges,
            divider,
            logo: rect(pad, header_y + (header_height - icon) / 2, icon, icon),
        }
    }
}

impl Panel {
    pub(super) fn render(
        guide: &Guide,
        dpi: u32,
        work: Rect,
        colors: Colors,
        translucent: bool,
    ) -> Option<Self> {
        if work.w <= 0 || work.h <= 0 {
            return None;
        }
        let dc = MemoryDc(unsafe { CreateCompatibleDC(null_mut()) });
        if dc.0.is_null() {
            return None;
        }
        let p = |dip: i32| (dip * dpi as i32 + 48) / 96;
        let fonts = Fonts::new(dpi);
        let PanelLayout {
            width,
            height,
            labels,
            badges,
            divider,
            logo,
        } = PanelLayout::measure(guide, work, &dc, &fonts, colors, p);
        let panel = Self::bitmap(dc, width, height)?;

        // GDI does not preserve destination alpha. Draw white text into the
        // DIB as a coverage mask, then composite it onto the tinted surface.
        // Grayscale coverage avoids ClearType colour fringes on moving backdrops.
        unsafe {
            SetBkMode(panel.dc.0, TRANSPARENT as i32);
            SetTextColor(panel.dc.0, 0x00ff_ffff);
        }
        for label in &labels {
            let mut bounds = label.bounds;
            label.draw(panel.dc.0, &mut bounds, label.flags);
        }
        unsafe { GdiFlush() };
        let pixels = unsafe { slice::from_raw_parts_mut(panel.bits, (width * height) as usize) };
        let mask: Vec<u8> = pixels
            .iter()
            .map(|v| {
                let [b, g, r, _] = v.to_le_bytes();
                ((u32::from(r) * 54 + u32::from(g) * 183 + u32::from(b) * 19 + 128) >> 8) as u8
            })
            .collect();
        pixels.fill(0);
        let mut surface = Surface {
            width,
            height,
            pixels,
        };
        let opaque = !translucent || colors.high_contrast;
        surface.rounded(
            rect(0, 0, width, height),
            p(10),
            colors.background,
            if opaque { 255 } else { 222 },
            colors.border,
        );
        for badge in badges {
            surface.rounded(badge, p(5), colors.control, 255, colors.border);
        }
        if let Some(line) = divider {
            surface.paint(line, |_, _, pixel| {
                *pixel = over(*pixel, colors.border, 190)
            });
        }
        for label in labels {
            surface.paint(label.bounds, |x, y, pixel| {
                *pixel = over(*pixel, label.color, mask[(y * width + x) as usize]);
            });
        }
        panel.logo(logo.left, logo.top, logo.right - logo.left);
        Some(panel)
    }

    fn bitmap(dc: MemoryDc, width: i32, height: i32) -> Option<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc.0, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() {
                return None;
            }
            let previous = SelectObject(dc.0, bitmap);
            if previous.is_null() {
                DeleteObject(bitmap);
                return None;
            }
            // The DIB initially supplies a black mask for the text.
            let bits = bits.cast::<u32>();
            bits.write_bytes(0, (width * height) as usize);
            Some(Self {
                width,
                height,
                dc,
                bitmap,
                previous,
                bits,
            })
        }
    }

    fn logo(&self, x: i32, y: i32, size: i32) {
        if size <= 0 || x + size > self.width || y + size > self.height {
            return;
        }
        unsafe {
            let icon = load_app_icon(size, size);
            if !icon.is_null() {
                DrawIconEx(self.dc.0, x, y, icon, size, size, 0, null_mut(), DI_NORMAL);
                GdiFlush();
                DestroyIcon(icon);
            }
        }
    }

    pub(super) fn present(&self, hwnd: HWND, x: i32, y: i32) -> bool {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
            BlendFlags: 0,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                null_mut(),
                &POINT { x, y },
                &SIZE {
                    cx: self.width,
                    cy: self.height,
                },
                self.dc.0,
                &POINT::default(),
                0,
                &blend,
                ULW_ALPHA,
            ) != 0
        }
    }

    #[cfg(test)]
    pub(super) fn pixels(&self) -> &[u32] {
        unsafe { slice::from_raw_parts(self.bits, (self.width * self.height) as usize) }
    }
}

struct Surface<'a> {
    width: i32,
    height: i32,
    pixels: &'a mut [u32],
}

impl Surface<'_> {
    fn paint(&mut self, bounds: RECT, mut paint: impl FnMut(i32, i32, &mut u32)) {
        for y in bounds.top.max(0)..bounds.bottom.min(self.height) {
            for x in bounds.left.max(0)..bounds.right.min(self.width) {
                paint(x, y, &mut self.pixels[(y * self.width + x) as usize]);
            }
        }
    }

    fn rounded(&mut self, bounds: RECT, radius: i32, fill: COLORREF, alpha: u8, border: COLORREF) {
        let radius =
            radius.min((bounds.right - bounds.left).min(bounds.bottom - bounds.top) / 2) as f32;
        self.paint(bounds, |x, y, pixel| {
            let dx = radius - ((x - bounds.left).min(bounds.right - x - 1) as f32 + 0.5);
            let dy = radius - ((y - bounds.top).min(bounds.bottom - y - 1) as f32 + 0.5);
            let distance = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius;
            let coverage = (0.5 - distance).clamp(0.0, 1.0);
            let stroke = coverage - (-0.5 - distance).clamp(0.0, 1.0);
            *pixel = over(*pixel, fill, (coverage * f32::from(alpha)).round() as u8);
            *pixel = over(*pixel, border, (stroke * 255.0).round() as u8);
        });
    }
}

// GDI colours use COLORREF (R,G,B); layered DIB pixels are premultiplied B,G,R,A.
fn over(background: u32, color: COLORREF, alpha: u8) -> u32 {
    if alpha == 0 {
        return background;
    }
    let [r, g, b, _] = color.to_le_bytes();
    let source = [b, g, r, 255];
    if alpha == 255 {
        return u32::from_le_bytes(source);
    }
    let dest = background.to_le_bytes();
    let a = u32::from(alpha);
    u32::from_le_bytes(std::array::from_fn(|i| {
        ((u32::from(source[i]) * a + u32::from(dest[i]) * (255 - a) + 127) / 255) as u8
    }))
}
