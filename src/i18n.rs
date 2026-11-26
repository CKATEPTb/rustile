use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Globalization::{
    GetUserDefaultUILanguage, GetUserPreferredUILanguages, LCIDToLocaleName, MUI_LANGUAGE_NAME,
};

include!(concat!(env!("OUT_DIR"), "/messages.rs"));

// Column order in locales/*.tsv.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Language {
    En,
    Ru,
    De,
    Fr,
    It,
    Es,
    #[serde(rename = "zh-Hans")]
    ZhHans,
    Ja,
    Pt,
    Ko,
    Pl,
    Tr,
}

impl Language {
    pub(crate) const ALL: [Self; 12] = [
        Self::En,
        Self::Ru,
        Self::De,
        Self::Fr,
        Self::It,
        Self::Es,
        Self::ZhHans,
        Self::Ja,
        Self::Pt,
        Self::Ko,
        Self::Pl,
        Self::Tr,
    ];

    pub(crate) const fn native_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Ru => "Русский",
            Self::De => "Deutsch",
            Self::Fr => "Français",
            Self::It => "Italiano",
            Self::Es => "Español",
            Self::ZhHans => "简体中文",
            Self::Ja => "日本語",
            Self::Pt => "Português (Brasil)",
            Self::Ko => "한국어",
            Self::Pl => "Polski",
            Self::Tr => "Türkçe",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        let tag = tag.to_ascii_lowercase();
        let mut parts = tag.split('-');
        Some(match parts.next()? {
            "en" => Self::En,
            "ru" => Self::Ru,
            "de" => Self::De,
            "fr" => Self::Fr,
            "it" => Self::It,
            "es" => Self::Es,
            "ja" => Self::Ja,
            "pt" => Self::Pt,
            "ko" => Self::Ko,
            "pl" => Self::Pl,
            "tr" => Self::Tr,
            "zh" => match parts.next() {
                Some("hant" | "cht" | "tw" | "hk" | "mo") => return None,
                Some("hans" | "chs" | "cn" | "sg" | "my") | None => Self::ZhHans,
                _ => return None,
            },
            _ => return None,
        })
    }
}

fn preferred(buffer: &[u16]) -> Option<Language> {
    buffer
        .split(|&unit| unit == 0)
        .take_while(|name| !name.is_empty())
        .find_map(|name| Language::from_tag(&String::from_utf16_lossy(name)))
}

fn windows_language() -> Language {
    #[cfg(test)]
    if let Some(language) = TEST_SYSTEM_LANGUAGE.get() {
        return *language;
    }
    let (mut count, mut size) = (0, 0);
    if unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut size,
        )
    } != 0
        && size > 0
    {
        let mut names = vec![0; size as usize];
        if unsafe {
            GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                names.as_mut_ptr(),
                &mut size,
            )
        } != 0
        {
            return preferred(&names).unwrap_or(Language::En);
        }
    }
    let mut name = [0; 85]; // LOCALE_NAME_MAX_LENGTH
    let id = unsafe { GetUserDefaultUILanguage() };
    let len = unsafe { LCIDToLocaleName(u32::from(id), name.as_mut_ptr(), name.len() as i32, 0) };
    if len > 0
        && let Some(language) = preferred(&name)
    {
        return language;
    }
    Language::En
}

static ACTIVE: OnceLock<AtomicUsize> = OnceLock::new();

pub(crate) fn set_language(language: Option<Language>) {
    // Re-read Windows when the user returns to automatic selection.
    let index = language.unwrap_or_else(windows_language) as usize;
    ACTIVE
        .get_or_init(|| AtomicUsize::new(index))
        .store(index, Ordering::Relaxed);
}

pub(crate) fn current() -> Language {
    #[cfg(test)]
    if let Some(language) = TEST_LANGUAGE.get() {
        return language;
    }
    let active = ACTIVE.get_or_init(|| AtomicUsize::new(windows_language() as usize));
    Language::ALL[active.load(Ordering::Relaxed)]
}

pub(crate) fn tr(message: Msg) -> &'static str {
    translation(message, current() as usize)
}

fn translation(message: Msg, language: usize) -> &'static str {
    let (text, offsets) = CATALOGS[language];
    let index = message as usize;
    &text[offsets[index] as usize..offsets[index + 1] as usize]
}

type WideCatalog = Vec<OnceLock<Box<[u16]>>>;
static WIDE: [OnceLock<WideCatalog>; 12] = [const { OnceLock::new() }; 12];

// Some Win32 controls retain this pointer for their entire lifetime.
pub(crate) fn wide(message: Msg) -> *const u16 {
    let language = current() as usize;
    let catalog =
        WIDE[language].get_or_init(|| (0..MESSAGE_COUNT).map(|_| OnceLock::new()).collect());
    catalog[message as usize]
        .get_or_init(|| {
            translation(message, language)
                .encode_utf16()
                .chain([0])
                .collect()
        })
        .as_ptr()
}

pub(crate) fn render(message: Msg, args: &[(&str, &str)]) -> String {
    let mut template = tr(message);
    let mut text = String::with_capacity(template.len());
    while let Some((literal, tail)) = template.split_once('{') {
        let (name, rest) = tail.split_once('}').expect("validated catalog placeholder");
        let value = args
            .iter()
            .find(|(key, _)| *key == name)
            .expect("missing translation argument")
            .1;
        text.push_str(literal);
        text.push_str(value);
        template = rest;
    }
    text.push_str(template);
    text
}

#[cfg(test)]
thread_local! { static TEST_LANGUAGE: std::cell::Cell<Option<Language>> = const { std::cell::Cell::new(None) }; }

#[cfg(test)]
static TEST_SYSTEM_LANGUAGE: OnceLock<Language> = OnceLock::new();

#[cfg(test)]
pub(crate) fn set_test_process_language(tag: &str) {
    TEST_SYSTEM_LANGUAGE
        .set(Language::from_tag(tag).expect("unsupported smoke-test language"))
        .expect("the test process must choose its language before creating windows");
}

#[cfg(test)]
pub(crate) fn with_language<T>(language: Language, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<Language>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_LANGUAGE.set(self.0);
        }
    }
    let _restore = Restore(TEST_LANGUAGE.replace(Some(language)));
    run()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_display_languages_in_windows_order() {
        use Language::*;
        for (tag, lang) in [
            ("en-GB", En),
            ("ru-RU", Ru),
            ("de-CH", De),
            ("fr-CA", Fr),
            ("it-IT", It),
            ("es-MX", Es),
            ("zh-CN", ZhHans),
            ("ZH-Hans-HK", ZhHans),
            ("ja-JP", Ja),
            ("pt-BR", Pt),
            ("pt-PT", Pt),
            ("ko-KR", Ko),
            ("pl-PL", Pl),
            ("tr-TR", Tr),
        ] {
            assert_eq!(Language::from_tag(tag), Some(lang));
        }
        let choose = |list: &str| preferred(&list.encode_utf16().collect::<Vec<_>>()).unwrap_or(En);
        assert_eq!(choose("zh-Hant-CN\0zh-TW\0uk-UA\0fr-CA\0de-DE\0\0"), Fr);
        assert_eq!(choose("zh-SG\0en-US\0\0"), ZhHans);
        assert_eq!(choose("nl-NL\0\0"), En);
        assert_eq!(choose("\0\0"), En);
    }

    #[test]
    fn win32_text_is_stable_and_formatting_treats_arguments_as_data() {
        let message = Msg::SystemDefaults;
        let english = with_language(Language::En, || {
            assert_eq!(
                render(message, &[("detail", "{title}")]),
                "Default settings applied.\n\n{title}"
            );
            wide(message)
        });
        let japanese = with_language(Language::Ja, || {
            // Win32 may still hold a tooltip pointer from the previous language.
            let expected: Vec<_> = "Default settings applied.\n\n{detail}"
                .encode_utf16()
                .chain([0])
                .collect();
            assert_eq!(
                unsafe { std::slice::from_raw_parts(english, expected.len()) },
                expected
            );
            assert_ne!(wide(message), english);
            wide(message)
        });
        with_language(Language::En, || {
            let expected: Vec<_> = tr(message).encode_utf16().chain([0]).collect();
            assert_eq!(wide(message), english);
            assert_eq!(
                unsafe { std::slice::from_raw_parts(english, expected.len()) },
                expected
            );
        });
        with_language(Language::Ja, || assert_eq!(wide(message), japanese));
    }
}
