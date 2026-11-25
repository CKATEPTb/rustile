use super::*;

pub(super) const KEY_EVENT: u32 = WM_APP + 91;

// The temporary hook runs on the editor's UI thread. Its messages belong to
// one recording only, even if the user immediately starts editing another row.
thread_local! {
    static TARGET: Cell<(HWND, usize)> = const { Cell::new((null_mut(), 0)) };
}

pub(super) struct Capture {
    hook: HHOOK,
    pub(super) generation: usize,
    pub(super) keys: Keys,
}

impl Capture {
    pub(super) fn start(hwnd: HWND) -> Result<Self, String> {
        let hook =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), GetModuleHandleW(null()), 0) };
        if hook.is_null() {
            return Err(format!(
                "Не удалось начать ввод сочетания. {}",
                io::Error::last_os_error()
            ));
        }
        let generation = TARGET.with(|target| {
            let generation = target.get().1.wrapping_add(1);
            target.set((hwnd, generation));
            generation
        });
        Ok(Self {
            hook,
            generation,
            keys: Keys::default(),
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        TARGET.with(|target| target.set((null_mut(), self.generation)));
        unsafe { UnhookWindowsHookEx(self.hook) };
    }
}

unsafe extern "system" fn on_key(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let (hwnd, generation) = TARGET.with(Cell::get);
            if !hwnd.is_null()
                && GetForegroundWindow() == hwnd
                && let Some(down) = is_key_down(message as u32)
            {
                let event = &*(data as *const KBDLLHOOKSTRUCT);
                let packed = event.vkCode as isize | ((down as isize) << 16);
                if PostMessageW(hwnd, KEY_EVENT, generation, packed) != 0 {
                    return 1;
                }
            }
        }
        CallNextHookEx(null_mut(), code, message, data)
    }
}

pub(super) fn is_key_down(message: u32) -> Option<bool> {
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
        WM_KEYUP | WM_SYSKEYUP => Some(false),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct Keys {
    held: Vec<u16>,
    chord: Option<Chord>,
    ambiguous: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Event {
    Unchanged,
    Changed,
    Cancel,
    Complete(Chord),
    Ambiguous,
}

fn modifier(key: u16) -> u32 {
    match key {
        VK_CONTROL | VK_LCONTROL | VK_RCONTROL => MOD_CONTROL,
        VK_MENU | VK_LMENU | VK_RMENU => MOD_ALT,
        VK_SHIFT | VK_LSHIFT | VK_RSHIFT => MOD_SHIFT,
        VK_LWIN | VK_RWIN => MOD_WIN,
        _ => 0,
    }
}

impl Keys {
    fn modifiers(&self) -> u32 {
        self.held.iter().fold(0, |mods, &key| mods | modifier(key))
    }

    pub(super) fn label(&self) -> String {
        if let Some(chord) = self.chord {
            chord.label()
        } else if self.held.is_empty() {
            "Нажмите клавиши".into()
        } else {
            Chord::modifier_labels(self.modifiers())
                .collect::<Vec<_>>()
                .join("+")
        }
    }

    pub(super) fn key(&mut self, key: u16, down: bool) -> Event {
        if key == VK_ESCAPE && down {
            return Event::Cancel;
        }
        if down {
            if self.held.contains(&key) {
                return Event::Unchanged;
            }
            self.held.push(key);
            let mods = self.modifiers();
            if modifier(key) == 0 {
                if self.chord.is_some_and(|c| c.key != key) {
                    self.ambiguous = true;
                } else {
                    self.chord = Some(Chord { mods, key });
                }
            } else if let Some(chord) = &mut self.chord
                && self.held.contains(&chord.key)
            {
                chord.mods |= mods;
            }
        } else if let Some(i) = self.held.iter().position(|&k| k == key) {
            self.held.swap_remove(i);
            if self.held.is_empty() {
                if self.ambiguous {
                    return Event::Ambiguous;
                }
                if let Some(chord) = self.chord {
                    return Event::Complete(chord);
                }
            }
        } else {
            return Event::Unchanged;
        }
        Event::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commits_only_after_all_keys_are_released_in_either_order() {
        for release in [[VK_LEFT, VK_LSHIFT, VK_RWIN], [VK_RWIN, VK_LSHIFT, VK_LEFT]] {
            let mut keys = Keys::default();
            for key in [VK_RWIN, VK_LSHIFT, VK_LEFT] {
                assert_eq!(keys.key(key, true), Event::Changed);
            }
            assert_eq!(keys.key(VK_LEFT, true), Event::Unchanged);
            assert_eq!(keys.key(release[0], false), Event::Changed);
            assert_eq!(keys.key(release[1], false), Event::Changed);
            assert_eq!(
                keys.key(release[2], false),
                Event::Complete(Chord {
                    mods: MOD_WIN | MOD_SHIFT,
                    key: VK_LEFT
                })
            );
        }
    }

    #[test]
    fn ignores_unused_modifiers_and_rejects_multiple_main_keys() {
        let mut keys = Keys::default();
        keys.key(VK_LCONTROL, true);
        assert_eq!(keys.key(VK_LCONTROL, false), Event::Changed);
        keys.key(VK_A, true);
        keys.key(VK_B, true);
        keys.key(VK_A, false);
        assert_eq!(keys.key(VK_B, false), Event::Ambiguous);
        assert_eq!(keys.key(VK_ESCAPE, true), Event::Cancel);
    }
}
