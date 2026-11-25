use std::{mem::size_of, ptr::null_mut};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{HiDpi::*, WindowsAndMessaging::*},
};
use windows_sys::core::w;

use crate::layout::{Direction, Id, Rect};
use crate::native;
use crate::settings::{Settings, Shortcut};

mod outline;
pub use outline::{DropPreview, FocusOutline};

// A layered transparent popup passes mouse input to the actual app, even
// across thread boundaries. It exists only while resizing a tile.
#[derive(Default)]
pub struct ResizeHud {
    label: HWND,
    edge: HWND,
    hint: Option<Hint>,
}

struct Hint {
    settings: Settings,
    dpi: i32,
    work: Rect,
    width: i32,
    height: i32,
}

fn hint_lines(settings: &Settings) -> Vec<String> {
    use Shortcut::*;
    let mut lines = Vec::new();
    for (actions, default_hint) in [
        (
            [GrowLeft, GrowRight, GrowUp, GrowDown],
            "Стрелки — сдвиг границы",
        ),
        (
            [ShrinkLeft, ShrinkRight, ShrinkUp, ShrinkDown],
            "Shift+стрелки — обратно",
        ),
    ] {
        if actions
            .iter()
            .all(|&action| settings.binding(action) == Some(action.default_chord()))
        {
            lines.push(default_hint.to_owned());
        } else {
            for action in actions {
                if let Some(chord) = settings.binding(action) {
                    lines.push(format!(
                        "{} — {}",
                        chord.label(),
                        action.label().to_lowercase()
                    ));
                }
            }
        }
    }
    let exits: Vec<_> = [Confirm, Cancel]
        .into_iter()
        .filter_map(|action| settings.binding(action).map(|chord| chord.label()))
        .collect();
    if !exits.is_empty() {
        lines.push(format!("{} — выход", exits.join(" / ")));
    } else {
        lines.push("Выберите другое окно для выхода".into());
    }
    lines
}

impl Hint {
    fn new(hwnd: HWND, settings: Settings, dpi: i32, work: Rect) -> Self {
        let wide: Vec<_> = hint_lines(&settings)
            .join("\r\n")
            .encode_utf16()
            .chain([0])
            .collect();
        let margin = dpi / 8;
        let mut bounds = RECT {
            right: (work.w - 2 * margin).max(1),
            ..Default::default()
        };
        unsafe {
            let dc = GetDC(hwnd);
            let font = hint_font(hwnd);
            let previous = SelectObject(dc, font);
            DrawTextW(
                dc,
                wide.as_ptr(),
                -1,
                &mut bounds,
                DT_CALCRECT | DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
            );
            SelectObject(dc, previous);
            DeleteObject(font);
            ReleaseDC(hwnd, dc);
            SetWindowTextW(hwnd, wide.as_ptr());
        }
        Self {
            settings,
            dpi,
            work,
            width: (bounds.right - bounds.left + margin * 2).min(work.w),
            height: (bounds.bottom - bounds.top + margin).min(work.h),
        }
    }
}

impl ResizeHud {
    pub fn show(&mut self, target: Id, tile: Rect, edge: Option<Direction>, settings: &Settings) {
        unsafe {
            if self.label.is_null() {
                let class = WNDCLASSW {
                    hInstance: GetModuleHandleW(std::ptr::null()),
                    lpszClassName: w!("Rustile.ResizeHint"),
                    lpfnWndProc: Some(paint_hint),
                    ..Default::default()
                };
                RegisterClassW(&class);
                self.label = popup(&class, tile);
                self.edge = popup(&class, tile);
            }
            if self.label.is_null() {
                return;
            }
            let dpi = GetDpiForWindow(self.label).max(96) as i32;
            let scale = |n: i32| (n * dpi + 48) / 96;
            if self
                .hint
                .as_ref()
                .is_none_or(|hint| hint.dpi != dpi || hint.settings != *settings)
            {
                self.hint = Some(Hint::new(
                    self.label,
                    *settings,
                    dpi,
                    native::work_area().unwrap_or(tile),
                ));
                InvalidateRect(self.label, std::ptr::null(), 0);
            }
            let hint = self.hint.as_ref().unwrap();
            // On a very narrow tile the popup can straddle a divider. Keep
            // its shortcuts fully readable, including long custom key names.
            let work = hint.work;
            position(
                self.label,
                (tile.x + (tile.w - hint.width) / 2).clamp(work.x, work.x + work.w - hint.width),
                (tile.y + (tile.h - hint.height) / 2).clamp(work.y, work.y + work.h - hint.height),
                hint.width,
                hint.height,
            );

            let Some(edge) = edge else {
                if !self.edge.is_null() {
                    ShowWindow(self.edge, SW_HIDE);
                }
                return;
            };
            let line = scale(2);
            // The movable edge meets the curved focus stroke at its tangents.
            let radius = scale(native::corner_radius_dip(target)).min(tile.w.min(tile.h) / 2);
            let bounds = match edge {
                Direction::Left => (tile.x, tile.y + radius, line, tile.h - 2 * radius),
                Direction::Right => (
                    tile.x + tile.w - line,
                    tile.y + radius,
                    line,
                    tile.h - 2 * radius,
                ),
                Direction::Up => (tile.x + radius, tile.y, tile.w - 2 * radius, line),
                Direction::Down => (
                    tile.x + radius,
                    tile.y + tile.h - line,
                    tile.w - 2 * radius,
                    line,
                ),
            };
            if !self.edge.is_null() {
                position(self.edge, bounds.0, bounds.1, bounds.2, bounds.3);
            }
        }
    }

    pub fn hide(&mut self) {
        self.hint = None;
        for window in [&mut self.label, &mut self.edge] {
            if !window.is_null() {
                unsafe { DestroyWindow(*window) };
                *window = null_mut();
            }
        }
    }
}

fn popup(class: &WNDCLASSW, tile: Rect) -> HWND {
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.lpszClassName,
            w!(""),
            WS_POPUP | WS_DISABLED,
            tile.x,
            tile.y,
            0,
            0,
            null_mut(),
            null_mut(),
            class.hInstance,
            std::ptr::null(),
        );
        if !hwnd.is_null() {
            SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
        }
        hwnd
    }
}

fn position(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

fn hint_font(hwnd: HWND) -> HFONT {
    unsafe {
        let mut font = LOGFONTW::default();
        SystemParametersInfoForDpi(
            SPI_GETICONTITLELOGFONT,
            size_of::<LOGFONTW>() as u32,
            (&raw mut font).cast(),
            0,
            GetDpiForWindow(hwnd),
        );
        CreateFontIndirectW(&font)
    }
}

unsafe extern "system" fn paint_hint(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCHITTEST => HTTRANSPARENT as LRESULT,
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut bounds = RECT::default();
                GetClientRect(hwnd, &mut bounds);
                let len = GetWindowTextLengthW(hwnd);
                let brush = if len == 0 {
                    SetDCBrushColor(dc, outline::resize_edge_color());
                    GetStockObject(DC_BRUSH)
                } else {
                    GetSysColorBrush(COLOR_MENU)
                };
                FillRect(dc, &bounds, brush);
                if len != 0 {
                    let mut text = vec![0u16; len as usize + 1];
                    GetWindowTextW(hwnd, text.as_mut_ptr(), len + 1);
                    let font = hint_font(hwnd);
                    let previous = SelectObject(dc, font);
                    SetTextColor(dc, GetSysColor(COLOR_MENUTEXT));
                    SetBkMode(dc, TRANSPARENT as i32);
                    let margin = GetDpiForWindow(hwnd).max(96) as i32 / 8;
                    let mut content = bounds;
                    InflateRect(&mut content, -margin, -(margin / 2));
                    DrawTextW(
                        dc,
                        text.as_ptr(),
                        len,
                        &mut content,
                        DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
                    );
                    SelectObject(dc, previous);
                    DeleteObject(font);
                    FrameRect(dc, &bounds, GetSysColorBrush(COLOR_WINDOWFRAME));
                }
                EndPaint(hwnd, &paint);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_hint_describes_the_actual_bindings_and_both_exits() {
        assert_eq!(
            hint_lines(&Settings::default()),
            [
                "Стрелки — сдвиг границы",
                "Shift+стрелки — обратно",
                "Enter / Esc — выход"
            ]
        );
        let settings: Settings = toml::from_str(
            "shortcuts.grow_left = { mods = 0, key = 117 }\n\
             shortcuts.grow_up = 'disabled'\n\
             shortcuts.confirm = { mods = 0, key = 32 }\n\
             shortcuts.cancel = 'disabled'",
        )
        .unwrap();
        assert_eq!(
            hint_lines(&settings),
            [
                "F6 — влево",
                "→ — вправо",
                "↓ — вниз",
                "Shift+стрелки — обратно",
                "Пробел — выход"
            ]
        );
    }
}
