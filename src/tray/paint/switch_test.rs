use super::*;
use crate::{
    settings::{Command, Settings},
    startup::Startup,
    tray::menu,
};

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
}

impl Surface {
    fn new(width: i32, height: i32) -> Self {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null());
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            assert!(!bitmap.is_null());
            assert!(!SelectObject(dc, bitmap).is_null());
            Self { dc, bitmap }
        }
    }

    fn pixel(&self, x: i32, y: i32) -> COLORREF {
        unsafe { GetPixel(self.dc, x, y) }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.dc);
            DeleteObject(self.bitmap);
        }
    }
}

#[test]
fn mosaic_switch_smooths_both_curves_on_the_current_row_surface() {
    // Orthogonal colour channels distinguish each blend from stray background or text pixels.
    let colors = Colors {
        background: rgb(255, 0, 0),
        hover: rgb(255, 0, 255),
        secondary: rgb(0, 128, 0),
        accent: rgb(0, 255, 0),
        on_accent: rgb(0, 0, 255),
        ..Colors::current()
    };
    for dpi in [96, 120, 144, 192] {
        let p = |dip| scale(dip, dpi);
        let (width, height) = (p(240), p(38));
        let surface = Surface::new(width, height);
        let item = DRAWITEMSTRUCT {
            hDC: surface.dc,
            rcItem: RECT {
                right: width,
                bottom: height,
                ..Default::default()
            },
            ..Default::default()
        };
        let fonts = Fonts::new(dpi);
        let right = width - p(10);
        let left = right - p(34);
        let cy = height / 2;
        let settings = Settings::default();
        for settings in [settings, settings.changed(Command::Toggle).unwrap()] {
            let enabled = settings.enabled;
            let row = menu::rows(settings, Some(Startup::Off))
                .into_iter()
                .find(|row| matches!(row.action, Action::Apply(Command::Toggle)))
                .unwrap();
            let track = if enabled {
                colors.accent
            } else {
                colors.secondary
            };
            let near = left + p(9);
            let far = right - p(9);
            let (thumb_x, opposite_x) = if enabled { (far, near) } else { (near, far) };
            let painter = Painter {
                dpi,
                colors,
                fonts: &fonts,
                header_icon: null_mut(),
                keyboard: false,
            };
            for hover in [false, true] {
                painter.row(&item, &row, false, hover, false);
                let background = if hover {
                    colors.hover
                } else {
                    colors.background
                };
                assert_eq!(surface.pixel(right + p(2), cy), background);
                assert_eq!(surface.pixel(opposite_x, cy), track);
                assert_eq!(surface.pixel(thumb_x, cy), colors.on_accent);

                let mut track_blends = 0;
                let mut thumb_blends = 0;
                for y in cy - p(10)..=cy + p(10) {
                    for x in left..right {
                        let [r, g, b, _] = surface.pixel(x, y).to_le_bytes();
                        if (x - opposite_x).abs() <= p(9) && r > 0 && g > 0 {
                            assert_eq!(b, if hover { r } else { 0 });
                            track_blends += 1;
                        }
                        if (x - thumb_x).abs() <= p(6) && r == 0 && g > 0 && b > 0 {
                            thumb_blends += 1;
                        }
                    }
                }
                let state = format!("{dpi} DPI, enabled={enabled}, hover={hover}");
                assert!(track_blends >= 4, "aliased track at {state}");
                assert!(thumb_blends >= 4, "aliased thumb at {state}");
            }
        }
    }
}
