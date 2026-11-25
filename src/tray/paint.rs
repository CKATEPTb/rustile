mod toggle;

use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{COLORREF, RECT},
    Graphics::Gdi::*,
    System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_SELECTED},
        WindowsAndMessaging::*,
    },
};
use windows_sys::core::w;

use super::{
    HEADER_ICON_DIP,
    menu::{Action, Row},
    scale,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Colors {
    pub background: COLORREF,
    pub border: COLORREF,
    pub(super) text: COLORREF,
    pub(super) secondary: COLORREF,
    pub(super) disabled: COLORREF,
    pub(super) control: COLORREF,
    pub(super) hover: COLORREF,
    pub(super) hover_text: COLORREF,
    pub(super) accent: COLORREF,
    pub(super) on_accent: COLORREF,
    pub dark: bool,
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    u32::from_le_bytes([r, g, b, 0])
}

impl Colors {
    pub fn current() -> Self {
        unsafe {
            let mut hc = HIGHCONTRASTW {
                cbSize: size_of::<HIGHCONTRASTW>() as u32,
                ..Default::default()
            };
            SystemParametersInfoW(SPI_GETHIGHCONTRAST, hc.cbSize, (&raw mut hc).cast(), 0);
            if hc.dwFlags & HCF_HIGHCONTRASTON != 0 {
                return Self {
                    background: GetSysColor(COLOR_MENU),
                    border: GetSysColor(COLOR_MENUTEXT),
                    text: GetSysColor(COLOR_MENUTEXT),
                    secondary: GetSysColor(COLOR_MENUTEXT),
                    disabled: GetSysColor(COLOR_GRAYTEXT),
                    control: GetSysColor(COLOR_MENU),
                    hover: GetSysColor(COLOR_HIGHLIGHT),
                    hover_text: GetSysColor(COLOR_HIGHLIGHTTEXT),
                    accent: GetSysColor(COLOR_HIGHLIGHT),
                    on_accent: GetSysColor(COLOR_HIGHLIGHTTEXT),
                    dark: false,
                };
            }
            let mut light = 1u32;
            let mut bytes = size_of::<u32>() as u32;
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                null_mut(),
                (&raw mut light).cast(),
                &mut bytes,
            );
            if light == 0 {
                Self {
                    background: rgb(43, 45, 49),
                    border: rgb(69, 72, 78),
                    text: rgb(246, 247, 249),
                    secondary: rgb(199, 201, 207),
                    disabled: rgb(147, 150, 156),
                    control: rgb(53, 55, 59),
                    hover: rgb(60, 63, 68),
                    hover_text: rgb(255, 255, 255),
                    accent: rgb(108, 199, 255),
                    on_accent: rgb(18, 41, 57),
                    dark: true,
                }
            } else {
                Self {
                    background: rgb(249, 249, 250),
                    border: rgb(212, 214, 218),
                    text: rgb(29, 31, 35),
                    secondary: rgb(86, 90, 97),
                    disabled: rgb(112, 115, 119),
                    control: rgb(255, 255, 255),
                    hover: rgb(235, 237, 240),
                    hover_text: rgb(19, 22, 25),
                    accent: rgb(0, 100, 172),
                    on_accent: rgb(255, 255, 255),
                    dark: false,
                }
            }
        }
    }

    pub fn edit_colors(self) -> (COLORREF, COLORREF) {
        (self.text, self.control)
    }
}

pub(super) struct Fonts {
    pub(super) regular: HFONT,
    pub(super) small: HFONT,
    pub(super) heading: HFONT,
    pub(super) symbol: HFONT,
}

impl Fonts {
    pub fn value_font(&self) -> HFONT {
        self.small
    }

    pub fn new(dpi: u32) -> Self {
        let font = |dip, weight, name| unsafe {
            CreateFontW(
                -scale(dip, dpi),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET.into(),
                0,
                0,
                CLEARTYPE_QUALITY.into(),
                0,
                name,
            )
        };
        Self {
            regular: font(14, 400, w!("Segoe UI")),
            small: font(13, 400, w!("Segoe UI")),
            heading: font(16, 600, w!("Segoe UI")),
            symbol: font(16, 400, w!("Segoe Fluent Icons")),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for font in [self.regular, self.small, self.heading, self.symbol] {
            if !font.is_null() {
                unsafe { DeleteObject(font) };
            }
        }
    }
}

pub(super) fn fill(dc: HDC, rect: &RECT, color: COLORREF, radius: i32) {
    unsafe {
        let brush = GetStockObject(DC_BRUSH);
        SetDCBrushColor(dc, color);
        if radius == 0 {
            FillRect(dc, rect, brush);
        } else {
            let old_brush = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            RoundRect(
                dc,
                rect.left,
                rect.top,
                rect.right + 1,
                rect.bottom + 1,
                radius * 2,
                radius * 2,
            );
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
        }
    }
}

pub(super) fn text(
    dc: HDC,
    mut rect: RECT,
    value: &str,
    font: HFONT,
    color: COLORREF,
    flags: u32,
) -> i32 {
    let wide: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        let old = SelectObject(dc, font);
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            wide.as_ptr(),
            wide.len() as i32,
            &mut rect,
            DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | flags,
        );
        SelectObject(dc, old);
    }
    rect.right - rect.left
}

pub(super) struct Painter<'a> {
    pub dpi: u32,
    pub colors: Colors,
    pub fonts: &'a Fonts,
    pub header_icon: HICON,
    pub keyboard: bool,
}

// Use the same rectangles for native children, the editor and owner drawing.
// Accept either tray-client or row-client coordinates so DPI rounding is identical.
pub(super) fn stepper_rects(row: RECT, dpi: u32, separator: bool) -> [RECT; 3] {
    let p = |n| scale(n, dpi);
    let right = (row.right - p(6)).max(row.left);
    let width = right - row.left;
    let arrow = p(28).min(width / 4);
    let value = p(48).min(width - 2 * arrow);
    let content_top = (row.top + if separator { p(9) } else { 0 }).min(row.bottom);
    let height = p(28).min(row.bottom - content_top);
    let top = content_top + (row.bottom - content_top - height) / 2;
    let segment = |left, right| RECT {
        left,
        right,
        top,
        bottom: top + height,
    };
    [
        segment(right - 2 * arrow - value, right - arrow - value),
        segment(right - arrow - value, right - arrow),
        segment(right - arrow, right),
    ]
}

fn inset(rc: RECT, by: i32) -> RECT {
    RECT {
        left: rc.left + by,
        top: rc.top + by,
        right: rc.right - by,
        bottom: rc.bottom - by,
    }
}

impl Painter<'_> {
    fn control(&self, dc: HDC, rc: RECT, surface: COLORREF) {
        let radius = scale(4, self.dpi);
        fill(dc, &rc, self.colors.border, radius);
        let stroke = scale(1, self.dpi);
        fill(dc, &inset(rc, stroke), surface, radius - stroke);
    }

    pub fn arrow(&self, item: &DRAWITEMSTRUCT, increasing: bool, hover: bool, focus: bool) {
        let p = |n| scale(n, self.dpi);
        let dc = item.hDC;
        let rc = item.rcItem;
        let c = self.colors;
        let disabled = item.itemState & ODS_DISABLED != 0;
        let active = !disabled && (hover || item.itemState & ODS_SELECTED != 0);
        fill(dc, &rc, c.background, 0);
        // The child clips the inner, square end. Joined with the value and its
        // other arrow, the three HWNDs share a single rounded outer edge.
        let mut surface = rc;
        if increasing {
            surface.left -= p(6);
        } else {
            surface.right += p(6);
        }
        self.control(dc, surface, if active { c.hover } else { c.control });
        let ink = if disabled {
            c.disabled
        } else if active {
            c.hover_text
        } else {
            c.text
        };
        unsafe { SetBkMode(dc, TRANSPARENT as i32) };
        text(
            dc,
            rc,
            if increasing { "\u{e76c}" } else { "\u{e76b}" },
            self.fonts.symbol,
            ink,
            DT_CENTER,
        );
        if focus && self.keyboard {
            unsafe { DrawFocusRect(dc, &inset(rc, p(3))) };
        }
    }

    pub fn row(&self, item: &DRAWITEMSTRUCT, row: &Row, header: bool, hover: bool, focus: bool) {
        let Colors {
            background,
            border,
            text: primary,
            secondary,
            disabled,
            hover: hot,
            hover_text,
            ..
        } = self.colors;
        let p = |n| scale(n, self.dpi);
        let dc = item.hDC;
        let mut rc = item.rcItem;
        unsafe { SetBkMode(dc, TRANSPARENT as i32) };
        fill(dc, &rc, background, 0);
        if row.separator {
            fill(
                dc,
                &RECT {
                    left: p(6),
                    right: rc.right - p(6),
                    top: p(4),
                    bottom: p(4) + 1,
                },
                border,
                0,
            );
            rc.top += p(9);
        }
        let interactive = row.enabled && !matches!(row.action, Action::None);
        let selected = interactive
            && (hover || item.itemState & ODS_SELECTED != 0 || (focus && self.keyboard));
        let fg = if selected {
            hover_text
        } else if row.enabled || header {
            primary
        } else {
            disabled
        };
        let sub = if selected { hover_text } else { secondary };
        if selected {
            fill(dc, &rc, hot, p(4));
        }

        let mid = (rc.top + rc.bottom) / 2;
        let left_gutter = header || !row.glyph.is_empty() || matches!(row.action, Action::Apply(_));
        let mut label = RECT {
            left: p(if left_gutter { 40 } else { 12 }),
            right: rc.right - p(12),
            ..rc
        };
        if header && matches!(row.action, Action::None) {
            let size = p(HEADER_ICON_DIP);
            unsafe {
                DrawIconEx(
                    dc,
                    p(10),
                    mid - size / 2,
                    self.header_icon,
                    size,
                    size,
                    0,
                    null_mut(),
                    DI_NORMAL,
                )
            };
        } else {
            let icon = row.glyph;
            if !icon.is_empty() {
                text(
                    dc,
                    RECT {
                        left: p(4),
                        right: p(36),
                        ..rc
                    },
                    icon,
                    self.fonts.symbol,
                    fg,
                    DT_CENTER,
                );
            }
        }

        if let Some(checked) = row.checked {
            let left = rc.right - p(10) - p(34);
            self.draw_toggle(dc, left, mid, selected, checked);
            label.right = left - p(8);
        } else if matches!(row.action, Action::Adjust(_)) {
            let [left, value, right] = stepper_rects(item.rcItem, self.dpi, row.separator);
            self.control(
                dc,
                RECT {
                    right: right.right,
                    ..left
                },
                self.colors.control,
            );
            text(
                dc,
                value,
                &row.detail,
                self.fonts.value_font(),
                primary,
                DT_CENTER,
            );
            label.right = left.left - p(8);
        } else if !row.detail.is_empty() {
            let width = text(dc, label, &row.detail, self.fonts.small, sub, DT_CALCRECT);
            text(dc, label, &row.detail, self.fonts.small, sub, DT_RIGHT);
            label.right -= width + p(12);
        }
        text(
            dc,
            label,
            &row.label,
            if header {
                self.fonts.heading
            } else {
                self.fonts.regular
            },
            fg,
            DT_LEFT | DT_END_ELLIPSIS,
        );
        if interactive && focus && self.keyboard {
            let ring = RECT {
                left: rc.left + 2,
                top: rc.top + 2,
                right: rc.right - 2,
                bottom: rc.bottom - 2,
            };
            unsafe { DrawFocusRect(dc, &ring) };
        }
    }
}

pub(super) fn background(dc: HDC, rect: &RECT, colors: Colors) {
    fill(dc, rect, colors.background, 0);
}

#[cfg(test)]
mod switch_test;
