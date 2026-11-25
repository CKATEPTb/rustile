use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{COLORREF, RECT},
    Graphics::Gdi::*,
    System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::{DRAWITEMSTRUCT, ODS_SELECTED},
        WindowsAndMessaging::*,
    },
};
use windows_sys::core::w;

use super::{
    menu::{Action, Row},
    scale,
};
use crate::settings::Command;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Colors {
    pub background: COLORREF,
    pub border: COLORREF,
    text: COLORREF,
    secondary: COLORREF,
    disabled: COLORREF,
    hover: COLORREF,
    hover_text: COLORREF,
    accent: COLORREF,
    on_accent: COLORREF,
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
                    hover: rgb(235, 237, 240),
                    hover_text: rgb(19, 22, 25),
                    accent: rgb(0, 100, 172),
                    on_accent: rgb(255, 255, 255),
                    dark: false,
                }
            }
        }
    }
}

pub(super) struct Fonts {
    regular: HFONT,
    small: HFONT,
    heading: HFONT,
    symbol: HFONT,
}

impl Fonts {
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

fn fill(dc: HDC, rect: &RECT, color: COLORREF, radius: i32) {
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

fn text(dc: HDC, mut rect: RECT, value: &str, font: HFONT, color: COLORREF, flags: u32) -> i32 {
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
    pub icon: HICON,
    pub keyboard: bool,
    pub enabled: bool,
}

impl Painter<'_> {
    pub fn row(&self, item: &DRAWITEMSTRUCT, row: &Row, header: bool, hover: bool, focus: bool) {
        let Colors {
            background,
            border,
            text: primary,
            secondary,
            disabled,
            hover: hot,
            hover_text,
            accent,
            on_accent,
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
        } else if row.enabled || matches!(row.action, Action::None) {
            primary
        } else {
            disabled
        };
        let sub = if selected { hover_text } else { secondary };
        if selected {
            fill(dc, &rc, hot, p(4));
        }

        let mid = (rc.top + rc.bottom) / 2;
        let left_gutter = header
            || !row.glyph.is_empty()
            || row.checked
            || matches!(row.action, Action::Apply(_));
        let mut label = RECT {
            left: p(if left_gutter { 40 } else { 12 }),
            right: rc.right - p(12),
            ..rc
        };
        if header && matches!(row.action, Action::None) {
            unsafe {
                DrawIconEx(
                    dc,
                    p(10),
                    mid - p(10),
                    self.icon,
                    p(20),
                    p(20),
                    0,
                    null_mut(),
                    DI_NORMAL,
                )
            };
        } else {
            let icon = if row.checked { "\u{e73e}" } else { row.glyph };
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
                    if row.checked && !selected { accent } else { fg },
                    DT_CENTER,
                );
            }
        }

        if matches!(row.action, Action::Open(_)) {
            // The chevron uses the same native icon family as the leading glyph.
            text(
                dc,
                RECT {
                    left: rc.right - p(29),
                    right: rc.right - p(7),
                    ..rc
                },
                "\u{e76c}",
                self.fonts.symbol,
                sub,
                DT_CENTER,
            );
            label.right -= p(20);
        }
        if matches!(row.action, Action::Apply(Command::Toggle)) {
            let right = rc.right - p(10);
            let pill = RECT {
                left: right - p(34),
                right,
                top: mid - p(9),
                bottom: mid + p(9),
            };
            fill(
                dc,
                &pill,
                if self.enabled { accent } else { secondary },
                p(9),
            );
            let x = if self.enabled {
                right - p(9)
            } else {
                pill.left + p(9)
            };
            fill(
                dc,
                &RECT {
                    left: x - p(5),
                    right: x + p(5),
                    top: mid - p(5),
                    bottom: mid + p(5),
                },
                on_accent,
                p(5),
            );
            label.right = pill.left - p(8);
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
