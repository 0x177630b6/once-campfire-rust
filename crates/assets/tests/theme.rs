//! Hermes fork: the product theme's own checks (`overrides/hermes/theme.css`, docs/hermes-theme.md).
//!
//! The palette is written three times (light, dark for the switch, dark for the phone's setting),
//! and upstream's `--lch-*` must stay `L C H` triplets of the same colours: these tests keep the
//! copies in step. They also check that the self-hosted fonts are where the stylesheet points, are
//! digested and served, and ship with their licences.

use std::collections::BTreeMap;

fn theme() -> String {
    std::fs::read_to_string(format!("{}/overrides/hermes/theme.css", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// The declarations between `header` (a line of the file) and the next line that is only `}`.
fn block(css: &str, header: &str) -> Vec<String> {
    let start = css.lines().position(|line| line == header).unwrap_or_else(|| panic!("no `{header}` block"));
    css.lines()
        .skip(start + 1)
        .take_while(|line| line.trim() != "}")
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// `--name: value;` declarations of a block, comments dropped.
fn declarations(lines: &[String]) -> BTreeMap<String, String> {
    lines
        .iter()
        .filter(|line| line.starts_with("--"))
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            let value = value.split(';').next().unwrap().trim();
            (name.to_string(), value.to_string())
        })
        .collect()
}

const LIGHT: &str = ":root {";
const DARK_SWITCH: &str = r#":root[data-theme="dark"] {"#;
const DARK_SYSTEM: &str = r#"  :root:not([data-theme="light"]) {"#;

#[test]
fn the_two_dark_blocks_are_the_same() {
    let css = theme();
    assert_eq!(block(&css, DARK_SWITCH), block(&css, DARK_SYSTEM));
    // The system block sits in the media query, the switch block outside it, and light explicit
    // wins over a dark phone (`:not([data-theme="light"])`).
    assert!(css.contains("@media (prefers-color-scheme: dark) {\n  :root:not([data-theme=\"light\"]) {"));
}

#[test]
fn every_palette_token_has_a_light_and_a_dark_value() {
    let css = theme();
    let light = declarations(&block(&css, LIGHT));
    let dark = declarations(&block(&css, DARK_SWITCH));
    assert!(light.len() > 60, "{}", light.len());
    assert_eq!(light.keys().collect::<Vec<_>>(), dark.keys().collect::<Vec<_>>());
    for name in ["--lch-black", "--lch-white", "--lch-red", "--ink", "--bg-surface", "--icon-invert"] {
        assert!(light.contains_key(name), "{name}");
    }
}

/// sRGB hex to OKLCH (Björn Ottosson's matrices), as `(L in %, C, H in degrees)`.
fn oklch(hex: &str) -> (f64, f64, f64) {
    let channel = |i: usize| {
        let c = f64::from(u8::from_str_radix(&hex[1 + 2 * i..3 + 2 * i], 16).unwrap()) / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (channel(0), channel(1), channel(2));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let lightness = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
    let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
    let b = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
    (lightness * 100.0, a.hypot(b), b.atan2(a).to_degrees().rem_euclid(360.0))
}

#[test]
fn upstream_triplets_are_the_oklch_of_the_palette() {
    let css = theme();
    for header in [LIGHT, DARK_SWITCH] {
        let lines = block(&css, header);
        let tokens = declarations(&lines);
        let triplets: Vec<&String> = lines.iter().filter(|line| line.starts_with("--lch-")).collect();
        assert_eq!(triplets.len(), 11, "{header}");
        for line in triplets {
            // --lch-white: 95.60% 0.0115 84.58; /* #f4f0e8 --bg-surface */
            let (name, rest) = line.split_once(':').unwrap();
            let (value, comment) = rest.split_once(';').unwrap();
            let parts: Vec<&str> = value.split_whitespace().collect();
            let (l, c, h) = (
                parts[0].trim_end_matches('%').parse::<f64>().unwrap(),
                parts[1].parse::<f64>().unwrap(),
                parts[2].parse::<f64>().unwrap(),
            );
            let comment: Vec<&str> = comment.trim().trim_start_matches("/*").trim_end_matches("*/").split_whitespace().collect();
            let (hex, token) = (comment[0], comment[1]);
            assert_eq!(tokens.get(token).map(String::as_str), Some(hex), "{header} {name}: {token} isn't {hex}");
            let (el, ec, eh) = oklch(hex);
            assert!((l - el).abs() < 0.01 && (c - ec).abs() < 0.0002, "{header} {name}: {value} vs {el:.2}% {ec:.4} {eh:.2}");
            assert!(ec < 0.0005 || ((h - eh + 180.0).rem_euclid(360.0) - 180.0).abs() < 0.02, "{header} {name}: hue {h} vs {eh:.2}");
        }
    }
}

#[test]
fn the_fonts_are_self_hosted_digested_and_licensed() {
    let css = theme();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("overrides/hermes");
    let urls: Vec<&str> = css.match_indices("url(\"").map(|(at, _)| css[at + 5..].split('"').next().unwrap()).collect();
    assert_eq!(urls.len(), 8, "{urls:?}");
    assert!(!css.contains("googleapis") && !css.contains("gstatic"));
    for url in &urls {
        assert!(url.starts_with("fonts/") && url.ends_with(".woff2"), "{url}");
        assert!(dir.join(url).is_file(), "{url}");
    }
    for licence in ["OFL-libre-caslon-text.txt", "OFL-instrument-sans.txt", "OFL-jetbrains-mono.txt"] {
        let text = std::fs::read_to_string(dir.join("fonts").join(licence)).unwrap();
        assert!(text.contains("SIL OPEN FONT LICENSE Version 1.1"), "{licence}");
    }

    // Compiled: every url() points at the font's digested path, which is served as a font.
    let served = campfire_assets::serve(&campfire_assets::StaticRequest {
        method: "GET",
        path: &campfire_assets::stylesheet_path("hermes/theme.css"),
        ..Default::default()
    })
    .unwrap();
    let compiled = String::from_utf8_lossy(&served.body).into_owned();
    for url in &urls {
        let digested = campfire_assets::asset_path(&format!("hermes/{url}"));
        assert!(digested.starts_with("/assets/hermes/fonts/") && digested != format!("/assets/hermes/{url}"), "{digested}");
        assert!(compiled.contains(&format!("url(\"{digested}\")")), "{digested}");
        let font =
            campfire_assets::serve(&campfire_assets::StaticRequest { method: "GET", path: &digested, ..Default::default() }).unwrap();
        assert_eq!(font.header("content-type"), Some("font/woff2"), "{digested}");
    }
}
