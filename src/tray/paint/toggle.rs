use windows_sys::Win32::{
    Foundation::COLORREF,
    Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HDC, SetDIBitsToDevice},
};

use super::{Painter, scale};

fn bgr(color: COLORREF) -> [u8; 4] {
    let [r, g, b, _] = color.to_le_bytes();
    [b, g, r, 0]
}

fn blend(from: [u8; 4], to: [u8; 4], coverage: f32) -> [u8; 4] {
    if coverage <= 0.0 {
        return from;
    }
    if coverage >= 1.0 {
        return to;
    }
    std::array::from_fn(|i| {
        let start = f32::from(from[i]);
        (start + (f32::from(to[i]) - start) * coverage).round() as u8
    })
}

impl Painter<'_> {
    pub(super) fn draw_toggle(&self, dc: HDC, left: i32, mid: i32, selected: bool) {
        let colors = self.colors;
        let surface = if selected {
            colors.hover
        } else {
            colors.background
        };
        let mut track = if self.enabled {
            colors.accent
        } else {
            colors.secondary
        };
        let mut thumb = colors.on_accent;
        if track == surface {
            track = if selected {
                colors.hover_text
            } else {
                colors.text
            };
            thumb = surface;
        } else if thumb == track {
            thumb = surface;
        }
        let (surface, track, thumb) = (bgr(surface), bgr(track), bgr(thumb));
        let width = scale(34, self.dpi);
        let height = scale(18, self.dpi);
        let radius = height as f32 / 2.0;
        let right_center = width as f32 - radius;
        let thumb_center = if self.enabled { right_center } else { radius };
        let thumb_radius = 5.0 * self.dpi as f32 / 96.0;

        // Sample both curves at physical pixel centers. Only the one-pixel
        // band crossing each contour blends with the color underneath it.
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            let dy = y as f32 + 0.5 - radius;
            for x in 0..width {
                let px = x as f32 + 0.5;
                let thumb_dx = px - thumb_center;
                let thumb_coverage = thumb_radius + 0.5 - (thumb_dx * thumb_dx + dy * dy).sqrt();
                let pixel = if thumb_coverage > 0.0 {
                    blend(track, thumb, thumb_coverage)
                } else {
                    let dx = px - px.clamp(radius, right_center);
                    let track_coverage = radius + 0.5 - (dx * dx + dy * dy).sqrt();
                    blend(surface, track, track_coverage)
                };
                pixels.push(pixel);
            }
        }
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
            SetDIBitsToDevice(
                dc,
                left,
                mid - height / 2,
                width as u32,
                height as u32,
                0,
                0,
                0,
                height as u32,
                pixels.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
            );
        }
    }
}
