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

mod outline;
pub use outline::FocusOutline;

// A layered transparent popup passes mouse input to the actual app, even
// across thread boundaries. It exists only while resizing a tile.
#[derive(Default)]
pub struct ResizeHud {
    label: HWND,
    edge: HWND,
}

impl ResizeHud {
    pub fn show(&mut self, target: Id, tile: Rect, edge: Option<Direction>) {
        unsafe {
            if self.label.is_null() {
                let class = WNDCLASSW {
                    hInstance: GetModuleHandleW(std::ptr::null()),
                    lpszClassName: w!("Rustile.ResizeHint"),
                    lpfnWndProc: Some(paint_hint),
                    ..Default::default()
                };
                RegisterClassW(&class);
                self.label = popup(&class);
                self.edge = popup(&class);
            }
            if self.label.is_null() {
                return;
            }
            let dpi = GetDpiForWindow(self.label).max(96) as i32;
            let scale = |n: i32| (n * dpi + 48) / 96;
            let mut label = format!("Размер   {} × {}", tile.w, tile.h);
            if tile.w >= scale(390) {
                label.push_str("    ·    Enter — готово    Esc — отмена");
            }
            let wide: Vec<_> = label.encode_utf16().chain([0]).collect();
            SetWindowTextW(self.label, wide.as_ptr());
            let width = scale(390).min(tile.w);
            let height = scale(32).min(tile.h);
            position(
                self.label,
                tile.x + (tile.w - width) / 2,
                tile.y + (tile.h - height) / 2,
                width,
                height,
            );
            InvalidateRect(self.label, std::ptr::null(), 0);

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
        for window in [&mut self.label, &mut self.edge] {
            if !window.is_null() {
                unsafe { DestroyWindow(*window) };
                *window = null_mut();
            }
        }
    }
}

fn popup(class: &WNDCLASSW) -> HWND {
    unsafe {
        let hwnd = CreateWindowExW(
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
                let mut text = [0u16; 150];
                let len = GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32);
                let brush = if len == 0 {
                    SetDCBrushColor(dc, outline::resize_edge_color());
                    GetStockObject(DC_BRUSH)
                } else {
                    GetSysColorBrush(COLOR_MENU)
                };
                FillRect(dc, &bounds, brush);
                if len != 0 {
                    let mut system_font = LOGFONTW::default();
                    SystemParametersInfoForDpi(
                        SPI_GETICONTITLELOGFONT,
                        size_of::<LOGFONTW>() as u32,
                        (&raw mut system_font).cast(),
                        0,
                        GetDpiForWindow(hwnd),
                    );
                    let font = CreateFontIndirectW(&system_font);
                    let previous = SelectObject(dc, font);
                    SetTextColor(dc, GetSysColor(COLOR_MENUTEXT));
                    SetBkMode(dc, TRANSPARENT as i32);
                    DrawTextW(
                        dc,
                        text.as_ptr(),
                        len,
                        &mut bounds,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
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
