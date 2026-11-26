use super::*;
use std::collections::HashMap;

// The editor can have arbitrarily many alternative bindings. Keep GDI and
// video memory bounded even after visiting every page on a high-DPI monitor.
const MAX_BITMAPS: usize = 256;
const MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Content {
    Binding { hwnd: HWND, action: usize },
    Control(i32),
    PageNumber(usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Key {
    pub content: Content,
    pub state: u32,
    pub hovered: bool,
    pub selected: bool,
    pub recording: bool,
}

struct Image {
    bitmap: HBITMAP,
    width: i32,
    height: i32,
    last_used: u64,
}

impl Image {
    fn bytes(&self) -> u64 {
        self.width as u64 * self.height as u64 * 4
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        unsafe { DeleteObject(self.bitmap) };
    }
}

#[derive(Default)]
pub(super) struct Cache {
    dc: HDC,
    images: HashMap<Key, Image>,
    bytes: u64,
    clock: u64,
    pub section_widths: [Option<i32>; view::GROUPS.len()],
}

impl Cache {
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn invalidate_action(&mut self, action: Shortcut) {
        self.images.retain(|key, image| {
            if matches!(key.content, Content::Binding { action: i, .. } if i == action.index()) {
                self.bytes -= image.bytes();
                false
            } else {
                true
            }
        });
    }

    fn remove(&mut self, key: Key) {
        if let Some(image) = self.images.remove(&key) {
            self.bytes -= image.bytes();
        }
    }

    /// Draw once at the device's actual pixel density. A hit copies pixels
    /// without scaling, preserving the ClearType output of the native fonts.
    pub(super) fn blit(
        &mut self,
        item: &DRAWITEMSTRUCT,
        key: Key,
        render: impl FnOnce(&DRAWITEMSTRUCT),
    ) -> bool {
        let r = item.rcItem;
        let width = r.right - r.left;
        let height = r.bottom - r.top;
        if width <= 0 || height <= 0 {
            return true;
        }
        let bytes = width as u64 * height as u64 * 4;
        if bytes > MAX_BYTES {
            return false;
        }
        if self.dc.is_null() {
            self.dc = unsafe { CreateCompatibleDC(item.hDC) };
            if self.dc.is_null() {
                return false;
            }
        }
        self.clock += 1;
        let hit = self
            .images
            .get(&key)
            .is_some_and(|image| image.width == width && image.height == height);
        if !hit {
            self.remove(key);
            while self.images.len() >= MAX_BITMAPS || self.bytes + bytes > MAX_BYTES {
                let oldest = self.images.iter().min_by_key(|(_, image)| image.last_used);
                if let Some((&oldest, _)) = oldest {
                    self.remove(oldest);
                }
            }
            let bitmap = unsafe { CreateCompatibleBitmap(item.hDC, width, height) };
            if bitmap.is_null() {
                return false;
            }
            self.images.insert(
                key,
                Image {
                    bitmap,
                    width,
                    height,
                    last_used: self.clock,
                },
            );
            self.bytes += bytes;
        }
        let image = self.images.get_mut(&key).unwrap();
        image.last_used = self.clock;
        let old = unsafe { SelectObject(self.dc, image.bitmap) };
        if old.is_null() {
            self.remove(key);
            return false;
        }
        if !hit {
            render(&DRAWITEMSTRUCT {
                hDC: self.dc,
                rcItem: RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                },
                ..*item
            });
        }
        let copied = unsafe {
            let result = BitBlt(
                item.hDC, r.left, r.top, width, height, self.dc, 0, 0, SRCCOPY,
            );
            SelectObject(self.dc, old);
            result != 0
        };
        if !copied {
            self.remove(key);
        }
        copied
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        if !self.dc.is_null() {
            unsafe { DeleteDC(self.dc) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_a_binding_reuses_its_pixels_until_the_binding_changes() {
        let action = Shortcut::ALL[0];
        let key = Key {
            content: Content::Binding {
                hwnd: null_mut(),
                action: action.index(),
            },
            state: 0,
            hovered: false,
            selected: false,
            recording: false,
        };
        // A memory DC and a monochrome surface exercise the actual GDI blit
        // without opening a window or depending on the desktop's colours.
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        let bitmap = unsafe { CreateBitmap(32, 32, 1, 1, null()) };
        assert!(!dc.is_null() && !bitmap.is_null());
        let previous = unsafe { SelectObject(dc, bitmap) };
        assert!(!previous.is_null());
        let mut item = DRAWITEMSTRUCT {
            hDC: dc,
            rcItem: RECT {
                left: 0,
                top: 0,
                right: 8,
                bottom: 8,
            },
            ..Default::default()
        };
        let mut cache = Cache::default();
        let white = 0x00ff_ffff;
        assert!(cache.blit(&item, key, |i| paint::fill(i.hDC, &i.rcItem, white, 0)));
        item.rcItem.left += 12;
        item.rcItem.right += 12;
        assert!(cache.blit(&item, key, |_| panic!(
            "moving an unchanged binding repainted it"
        )));
        assert_eq!(unsafe { GetPixel(dc, 15, 2) }, white);
        cache.invalidate_action(action);
        assert!(cache.blit(&item, key, |i| paint::fill(i.hDC, &i.rcItem, 0, 0)));
        assert_eq!(unsafe { GetPixel(dc, 15, 2) }, 0);
        assert_eq!(unsafe { GetPixel(dc, 2, 2) }, white);
        unsafe {
            SelectObject(dc, previous);
            DeleteObject(bitmap);
            DeleteDC(dc);
        }
    }
}
