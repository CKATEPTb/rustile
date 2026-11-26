use super::{guide::Guide, panel::Panel};
use crate::{layout::Rect, settings::Settings, tray::Colors};

const WORK: Rect = Rect {
    x: -980,
    y: 32,
    w: 980,
    h: 740,
};

#[test]
fn localized_resize_hints_reflow_on_a_narrow_screen() {
    use crate::i18n::{Language, with_language};
    let colors = Colors::current();
    let narrow = Rect { w: 300, ..WORK };
    for (language, heading) in [
        (Language::De, "Fenstergröße ändern"),
        (Language::Ja, "ウィンドウのサイズ変更"),
        (Language::ZhHans, "调整窗口大小"),
    ] {
        with_language(language, || {
            let guide = Guide::new(&Settings::default());
            let accessible = guide.accessible_text();
            assert!(accessible.starts_with(heading));
            assert!(accessible.contains("Shift+Left"));
            let wide_panel = Panel::render(&guide, 144, WORK, colors, false).unwrap();
            let narrow_panel = Panel::render(&guide, 144, narrow, colors, false).unwrap();
            assert_eq!(narrow_panel.width, narrow.w);
            assert!(narrow_panel.height > wide_panel.height && narrow_panel.height < narrow.h);
        });
    }
}

#[test]
fn hint_has_clear_corners_crisp_ink_and_respects_transparency_settings() {
    let guide = Guide::new(&Settings::default());
    for dpi in [96, 144] {
        for (dark, translucent, high_contrast) in [
            (true, true, false),
            (false, true, false),
            (true, false, false),
            (false, true, true),
        ] {
            let mut colors = Colors::themed(dark);
            colors.high_contrast = high_contrast;
            let panel = Panel::render(&guide, dpi, WORK, colors, translucent).unwrap();
            let pixels = panel.pixels();
            let w = panel.width as usize;
            let h = panel.height as usize;
            assert_eq!(pixels.len(), w * h);
            for point in [0, w - 1, (h - 1) * w, h * w - 1] {
                assert_eq!(pixels[point], 0);
            }
            let [r, g, b, _] = colors.text.to_le_bytes();
            let mut ink = 0;
            let mut translucent_body = 0;
            for (i, &pixel) in pixels.iter().enumerate() {
                let [pb, pg, pr, a] = pixel.to_le_bytes();
                assert!(pb <= a && pg <= a && pr <= a, "pixel must be premultiplied");
                if a == 255
                    && [pb.abs_diff(b), pg.abs_diff(g), pr.abs_diff(r)]
                        .into_iter()
                        .all(|difference| difference <= 12)
                {
                    ink += 1;
                }
                let (x, y) = (i % w, i / w);
                if (w / 10..9 * w / 10).contains(&x) && (h / 10..9 * h / 10).contains(&y) && a < 255
                {
                    translucent_body += 1;
                }
            }
            assert!(ink > 20, "expected opaque foreground glyphs at {dpi} DPI");
            if translucent && !high_contrast {
                assert!(translucent_body > w * h / 5);
            } else {
                assert_eq!(translucent_body, 0);
            }
        }
    }
}

#[test]
fn hint_scales_with_dpi_and_fits_remapped_keys_in_the_work_area() {
    let guide = Guide::new(&Settings::default());
    let colors = Colors::current();
    let at_96 = Panel::render(&guide, 96, WORK, colors, true).unwrap();
    let at_144 = Panel::render(&guide, 144, WORK, colors, true).unwrap();
    for (small, large) in [(at_96.width, at_144.width), (at_96.height, at_144.height)] {
        assert!((small * 13 / 10..=small * 17 / 10).contains(&large));
    }
    // At high DPI a single row of English direction caps no longer fits.
    // A narrow panel should reflow all three rows and still show the footer.
    let narrow_work = Rect { w: 260, ..WORK };
    let wrapped = Panel::render(&guide, 144, narrow_work, colors, true).unwrap();
    assert_eq!(wrapped.width, narrow_work.w);
    assert!(wrapped.height > at_144.height && wrapped.height < narrow_work.h);
    let custom = toml::from_str("shortcuts.grow_left = { mods = 0, key = 117 }\nshortcuts.shrink_up = { mods = 6, key = 35 }").unwrap();
    let work = Rect { w: 340, ..WORK };
    let narrow = Panel::render(&Guide::new(&custom), 144, work, colors, true).unwrap();
    assert!(narrow.height > at_144.height);
    assert!(narrow.width <= work.w && narrow.height <= work.h);
}
