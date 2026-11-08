//! Contract tests for `fx_ui_macos::theme`: every 3645 token in Light and
//! Dark, the four shadows and their `CALayer` form, type styles, spacing,
//! radii and the component metrics the canvas uses. The expected values are
//! transcribed from the design system's `tokens.json` and the canvas's
//! `neu.css`, independently of the module.

use fx_ui_macos::theme::{
    self, CaLayerShadow, NamedWeight, RADIUS_LG, RADIUS_MD, RADIUS_PILL, RADIUS_SM, Rgb, Rgba,
    SPACE, Shadow, ShadowToken, Theme, Token, TypeStyle, WeightMix, metrics, pill_radius, styles,
    weight_mix,
};

/// `tokens.json`, colour tokens: name, Light, Dark.
const TOKENS: [(&str, u32, u32); 24] = [
    ("ground", 0xd9e1e4, 0x393939),
    ("ground-dark-born", 0x393939, 0x161718),
    ("ink", 0x555555, 0x8d8f93),
    ("glass", 0xffffff, 0xffffff),
    ("text", 0x555555, 0xffffff),
    ("green", 0x18b05a, 0x18b05a),
    ("green-light", 0x3fdc84, 0x3fdc84),
    ("green-dark", 0x0f8a45, 0x0f8a45),
    ("green-deep", 0x15a052, 0x15a052),
    ("panel", 0xf2f5f6, 0x454545),
    ("well", 0xe3e9eb, 0x2c2c2c),
    ("ink-strong", 0x232527, 0xffffff),
    ("muted", 0x5d6164, 0xb8bbbf),
    ("line", 0xcfd8dc, 0x555555),
    ("edge", 0x7b858a, 0x9b9ea2),
    ("on-green", 0x161718, 0x161718),
    ("green-ink", 0x0b6e36, 0x3fdc84),
    ("green-wash", 0xd3f0df, 0x1d4a30),
    ("vol", 0x232527, 0xffffff),
    ("on-vol", 0xf2f5f6, 0x2c2c2c),
    ("caution-wash", 0xfbefd5, 0x3d3218),
    ("caution-ink", 0x6e4600, 0xf3c776),
    ("caution-edge", 0xe2c27f, 0x6b5626),
    ("danger-ink", 0xb42318, 0xff9b91),
];

#[test]
fn every_colour_token_in_both_themes() {
    assert_eq!(Token::ALL.len(), TOKENS.len());
    for (token, (name, light, dark)) in Token::ALL.iter().zip(TOKENS) {
        assert_eq!(token.name(), name);
        assert_eq!(Theme::Light.color(*token), Rgb(light), "{name} light");
        assert_eq!(Theme::Dark.color(*token), Rgb(dark), "{name} dark");
        assert_eq!(Token::from_name(name), Some(*token));
    }
    assert_eq!(Token::from_name("teal"), None);
}

#[test]
fn aliases_resolve_to_their_targets() {
    for theme in Theme::ALL {
        let c = |t| theme.color(t);
        assert_eq!(c(Token::Vol), c(Token::InkStrong), "{theme:?}");
    }
    assert_eq!(
        Theme::Light.color(Token::Text),
        Theme::Light.color(Token::Ink)
    );
    assert_eq!(
        Theme::Dark.color(Token::Text),
        Theme::Dark.color(Token::Glass)
    );
    assert_eq!(
        Theme::Light.color(Token::OnVol),
        Theme::Light.color(Token::Panel)
    );
    assert_eq!(
        Theme::Dark.color(Token::OnVol),
        Theme::Dark.color(Token::Well)
    );
}

#[test]
fn themes_by_name_and_appearance() {
    assert_eq!(Theme::ALL, [Theme::Light, Theme::Dark]);
    assert_eq!(Theme::for_dark(false), Theme::Light);
    assert_eq!(Theme::for_dark(true), Theme::Dark);
    assert!(Theme::Dark.is_dark() && !Theme::Light.is_dark());
    assert_eq!(Theme::Light.name(), "light");
    assert_eq!(Theme::Dark.name(), "dark");
}

#[test]
fn rgb_helpers() {
    assert_eq!(Rgb(0x18b05a).bytes(), [0x18, 0xb0, 0x5a]);
    assert_eq!(Rgb(0x18b05a).hex(), "#18b05a");
    assert_eq!(Rgb(0xffffff).components(), [1.0, 1.0, 1.0]);
    assert_eq!(Rgba::new(0x364950, 0.18).rgb, Rgb(0x364950));
    assert_eq!(Rgba::opaque(Rgb(0x123456)), Rgba::new(0x123456, 1.0));
}

fn outer(x: f64, y: f64, blur: f64, rgb: u32, alpha: f64) -> Shadow {
    Shadow {
        inset: false,
        x,
        y,
        blur,
        spread: 0.0,
        color: Rgba::new(rgb, alpha),
    }
}

fn inset(x: f64, y: f64, blur: f64, rgb: u32, alpha: f64) -> Shadow {
    Shadow {
        inset: true,
        ..outer(x, y, blur, rgb, alpha)
    }
}

#[test]
fn the_four_shadows_in_both_themes() {
    let cases: [(ShadowToken, Theme, Vec<Shadow>); 8] = [
        (
            ShadowToken::Raised,
            Theme::Light,
            vec![
                outer(-3.0, -3.0, 8.0, 0xffffff, 0.85),
                outer(3.0, 3.0, 10.0, 0x364950, 0.18),
            ],
        ),
        (
            ShadowToken::Raised,
            Theme::Dark,
            vec![
                outer(-2.0, -2.0, 6.0, 0xffffff, 0.05),
                outer(3.0, 3.0, 10.0, 0x000000, 0.5),
            ],
        ),
        (
            ShadowToken::Pressed,
            Theme::Light,
            vec![
                inset(2.0, 2.0, 5.0, 0x364950, 0.16),
                inset(-2.0, -2.0, 5.0, 0xffffff, 0.9),
            ],
        ),
        (
            ShadowToken::Pressed,
            Theme::Dark,
            vec![
                inset(2.0, 2.0, 5.0, 0x000000, 0.5),
                inset(-1.0, -1.0, 3.0, 0xffffff, 0.06),
            ],
        ),
        (
            ShadowToken::Float,
            Theme::Light,
            vec![outer(0.0, 12.0, 32.0, 0x161718, 0.22)],
        ),
        (
            ShadowToken::Float,
            Theme::Dark,
            vec![outer(0.0, 12.0, 32.0, 0x000000, 0.55)],
        ),
        (
            ShadowToken::Key,
            Theme::Light,
            vec![
                outer(-1.0, -1.0, 2.0, 0xffffff, 0.9),
                outer(1.0, 1.0, 3.0, 0x364950, 0.28),
            ],
        ),
        (
            ShadowToken::Key,
            Theme::Dark,
            vec![
                outer(-1.0, -1.0, 2.0, 0xffffff, 0.06),
                outer(1.0, 1.0, 3.0, 0x000000, 0.55),
            ],
        ),
    ];
    for (token, theme, expected) in cases {
        assert_eq!(
            theme.shadow(token),
            expected.as_slice(),
            "{token:?} {theme:?}"
        );
    }
    let names: Vec<&str> = ShadowToken::ALL.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        [
            "shadow-raised",
            "shadow-pressed",
            "shadow-float",
            "shadow-key"
        ]
    );
}

#[test]
fn shadows_print_as_the_design_writes_them() {
    let css: Vec<String> = Theme::Light
        .shadow(ShadowToken::Raised)
        .iter()
        .map(Shadow::css)
        .collect();
    assert_eq!(
        css,
        [
            "-3px -3px 8px rgba(255, 255, 255, 0.85)",
            "3px 3px 10px rgba(54, 73, 80, 0.18)"
        ]
    );
    assert_eq!(
        Theme::Dark.shadow(ShadowToken::Pressed)[1].css(),
        "inset -1px -1px 3px rgba(255, 255, 255, 0.06)"
    );
    assert_eq!(
        Shadow::ring(2.0, Rgba::new(0x0b6e36, 1.0)).css(),
        "0px 0px 0px 2px rgba(11, 110, 54, 1)"
    );
}

#[test]
fn css_blur_is_a_gaussian_of_half_the_radius() {
    assert_eq!(outer(0.0, 12.0, 32.0, 0, 0.5).sigma(), 16.0);
    assert_eq!(outer(1.0, 1.0, 3.0, 0, 0.5).sigma(), 1.5);
}

#[test]
fn outer_shadows_convert_to_calayer_shadows() {
    // y runs up in AppKit, so a CSS shadow 3px down has offset -3.
    assert_eq!(
        outer(3.0, 3.0, 10.0, 0x364950, 0.18).ca_layer(),
        Some(CaLayerShadow {
            offset_x: 3.0,
            offset_y: -3.0,
            radius: 5.0,
            color: Rgb(0x364950),
            opacity: 0.18,
        })
    );
    assert_eq!(
        Theme::Dark.shadow(ShadowToken::Float)[0].ca_layer(),
        Some(CaLayerShadow {
            offset_x: 0.0,
            offset_y: -12.0,
            radius: 16.0,
            color: Rgb(0x000000),
            opacity: 0.55,
        })
    );
    // A CALayer has no inset shadow and no spread.
    assert_eq!(inset(2.0, 2.0, 5.0, 0, 0.5).ca_layer(), None);
    assert_eq!(Shadow::ring(2.0, Rgba::new(0, 1.0)).ca_layer(), None);
}

#[test]
fn spacing_and_radii() {
    assert_eq!(SPACE, [4.0, 8.0, 12.0, 16.0, 24.0, 32.0]);
    assert_eq!(theme::SPACE_1, 4.0);
    assert_eq!(theme::SPACE_4, 16.0);
    assert_eq!(theme::SPACE_6, 32.0);
    assert_eq!((RADIUS_SM, RADIUS_MD, RADIUS_LG), (6.0, 10.0, 16.0));
    assert_eq!(RADIUS_PILL, 999.0);
    assert_eq!(pill_radius(28.0), 14.0);
    assert_eq!(pill_radius(10.0), 5.0);
}

fn style(size: f64, line: f64, weight: u16) -> TypeStyle {
    TypeStyle {
        size,
        line_height: line,
        weight,
        tracking_em: 0.0,
        caps: false,
        tabular: false,
        mono: false,
    }
}

#[test]
fn the_design_systems_type_styles() {
    let caps = |s: TypeStyle, em: f64| TypeStyle {
        tracking_em: em,
        caps: true,
        ..s
    };
    let tabular = |s: TypeStyle| TypeStyle { tabular: true, ..s };
    let cases = [
        (styles::TITLE_APP, style(15.0, 20.0, 700)),
        (
            styles::TITLE_DISPLAY,
            TypeStyle {
                tracking_em: -0.01,
                ..style(24.0, 30.0, 700)
            },
        ),
        (styles::TITLE_SECTION, style(20.0, 26.0, 700)),
        (styles::TEXT_BODY, style(16.0, 24.0, 400)),
        (styles::TEXT_BODY_STRONG, style(16.0, 24.0, 650)),
        (styles::TEXT_INPUT, tabular(style(17.0, 24.0, 650))),
        (styles::TEXT_SMALL, style(14.0, 20.0, 400)),
        (styles::TEXT_CAPTION, style(13.0, 18.0, 400)),
        (styles::LABEL_EYEBROW, caps(style(12.0, 16.0, 700), 0.06)),
        (styles::LABEL_GROUP, caps(style(12.0, 16.0, 800), 0.08)),
        (styles::LABEL_TAG, caps(style(11.0, 14.0, 700), 0.04)),
        (styles::NUM_RESULT, tabular(style(14.0, 22.0, 800))),
    ];
    for (i, (got, want)) in cases.into_iter().enumerate() {
        assert_eq!(got, want, "style {i}");
    }
}

#[test]
fn the_canvas_component_type_styles() {
    let caps = |s: TypeStyle, em: f64| TypeStyle {
        tracking_em: em,
        caps: true,
        ..s
    };
    let tabular = |s: TypeStyle| TypeStyle { tabular: true, ..s };
    let mono = |s: TypeStyle| TypeStyle { mono: true, ..s };
    let cases = [
        (styles::WINDOW_TEXT, style(14.0, 20.0, 400)),
        (
            styles::WINDOW_TITLE,
            TypeStyle {
                tracking_em: -0.01,
                ..style(15.0, 20.0, 700)
            },
        ),
        (styles::CHANNEL_TAG, caps(style(11.0, 14.0, 700), 0.04)),
        (styles::SEARCH_TEXT, style(17.0, 20.0, 400)),
        (styles::TOGGLE, style(14.0, 20.0, 650)),
        (styles::TOGGLE_MONO, mono(style(13.0, 20.0, 650))),
        (styles::COLUMN_HEADER, caps(style(12.0, 16.0, 700), 0.06)),
        (styles::ROW_NAME, style(14.0, 20.0, 500)),
        (styles::ROW_HIT, style(14.0, 20.0, 800)),
        (styles::ROW_TEXT, style(14.0, 20.0, 400)),
        (styles::ROW_FIGURES, tabular(style(14.0, 20.0, 400))),
        (styles::EMPTY_TITLE, style(20.0, 26.0, 700)),
        (styles::EMPTY_TEXT, style(16.0, 24.0, 400)),
        (
            styles::SYNTAX_LABEL,
            TypeStyle {
                tracking_em: 0.08,
                ..style(12.0, 16.0, 800)
            },
        ),
        (styles::SYNTAX_CODE, mono(style(13.0, 20.0, 400))),
        (styles::SYNTAX_TEXT, style(14.0, 20.0, 400)),
        (styles::REGEX_ERROR, style(14.0, 20.0, 650)),
        (styles::STATUS_TEXT, style(13.0, 18.0, 400)),
        (styles::STATUS_FIGURE, tabular(style(13.0, 18.0, 700))),
        (styles::STATUS_INDEX, tabular(style(13.0, 18.0, 400))),
        (styles::KEY_CAP, style(12.0, 16.0, 700)),
        (styles::BANNER_TEXT, style(14.0, 20.0, 400)),
        (styles::BANNER_LEAD, style(14.0, 20.0, 700)),
        (styles::BUTTON, style(14.0, 20.0, 650)),
        (styles::SECTION_TITLE, caps(style(12.0, 16.0, 700), 0.06)),
        (styles::SETTING_LABEL, style(15.0, 22.0, 650)),
        (styles::SETTING_CAPTION, style(13.0, 18.0, 400)),
        (styles::SETTING_NOTE, style(13.0, 18.0, 400)),
        (styles::FOOTNOTE, style(12.0, 16.0, 400)),
        (styles::SEGMENT, style(14.0, 20.0, 700)),
        (styles::ICON_NAME, style(13.0, 18.0, 650)),
        (styles::TAG, caps(style(11.0, 14.0, 700), 0.04)),
    ];
    for (i, (got, want)) in cases.into_iter().enumerate() {
        assert_eq!(got, want, "component style {i}");
    }
}

#[test]
fn letter_spacing_in_points() {
    assert_eq!(styles::LABEL_EYEBROW.kern(), 12.0 * 0.06);
    assert_eq!(styles::WINDOW_TITLE.kern(), -0.15);
    assert_eq!(styles::TEXT_BODY.kern(), 0.0);
}

#[test]
fn css_weights_map_onto_the_named_system_weights() {
    let named = |w| WeightMix {
        lower: w,
        upper: w,
        t: 0.0,
    };
    assert_eq!(weight_mix(400), named(NamedWeight::Regular));
    assert_eq!(weight_mix(500), named(NamedWeight::Medium));
    assert_eq!(weight_mix(700), named(NamedWeight::Bold));
    assert_eq!(weight_mix(800), named(NamedWeight::Heavy));
    // 650 lies halfway between Semibold and Bold; the system may round it.
    assert_eq!(
        weight_mix(650),
        WeightMix {
            lower: NamedWeight::Semibold,
            upper: NamedWeight::Bold,
            t: 0.5,
        }
    );
    assert_eq!(weight_mix(50), named(NamedWeight::UltraLight));
    assert_eq!(weight_mix(1000), named(NamedWeight::Black));
    assert_eq!(styles::TOGGLE.weight_mix(), weight_mix(650));
}

#[test]
fn component_metrics_from_the_canvas() {
    use metrics::*;
    assert_eq!((WINDOW_WIDTH, WINDOW_HEIGHT), (1280.0, 800.0));
    assert_eq!((WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT), (760.0, 480.0));
    assert_eq!(TOPBAR_PADDING, [10.0, 16.0, 12.0, 16.0]);
    assert_eq!((TITLE_ROW_HEIGHT, TITLE_ROW_GAP), (24.0, 10.0));
    assert_eq!(TOOLBAR_GAP, 12.0);
    assert_eq!(
        (SEARCH_HEIGHT, SEARCH_TEXT_X, SEARCH_GLYPH_X),
        (46.0, 42.0, 14.0)
    );
    assert_eq!((CLEAR_SIZE, CLEAR_INSET), (38.0, 4.0));
    assert_eq!(FOCUS_RING_WIDTH, 2.0);
    assert_eq!(
        (TOGGLE_HEIGHT, TOGGLE_MIN_WIDTH, TOGGLE_PADDING_X),
        (40.0, 48.0, 14.0)
    );
    assert_eq!((TOGGLE_GAP, SELECTED_BORDER), (8.0, 2.0));
    assert_eq!(CARD_MARGIN, 16.0);
    assert_eq!((HEADER_HEIGHT, HEADER_PADDING_X), (44.0, 16.0));
    assert_eq!(
        (ROW_HEIGHT, ROW_GAP, ROW_PADDING_X, ROWS_INSET),
        (40.0, 2.0, 6.0, 8.0)
    );
    assert_eq!(COLUMN_GAP, 16.0);
    assert_eq!(NAME_COLUMN, (220.0, 1.25));
    assert_eq!(PATH_COLUMN, (200.0, 1.5));
    assert_eq!((SIZE_COLUMN, DATE_COLUMN), (88.0, 156.0));
    assert_eq!(GRID_MIN_WIDTH, 760.0);
    assert_eq!((GLYPH_SIZE, GLYPH_GAP, SORT_GLYPH_SIZE), (18.0, 10.0, 14.0));
    assert_eq!(SORT_GLYPH_STROKE, 2.0);
    assert_eq!(EMPTY_MAX_WIDTH, 600.0);
    assert_eq!(EMPTY_PADDING_TOP, 56.0);
    assert_eq!((CODE_PADDING_X, CODE_PADDING_Y), (8.0, 2.0));
    assert_eq!(STATUS_PADDING, [10.0, 24.0, 12.0, 24.0]);
    assert_eq!((STATUS_GAP_X, HINT_GAP, HINT_KEY_GAP), (24.0, 16.0, 6.0));
    assert_eq!(
        (KEY_HEIGHT, KEY_MIN_WIDTH, KEY_PADDING_X, KEY_GAP),
        (22.0, 22.0, 6.0, 3.0)
    );
    assert_eq!((DOT_SIZE, DOT_RING), (8.0, 3.0));
    assert_eq!(
        (PROGRESS_WIDTH, PROGRESS_HEIGHT, PROGRESS_GAP),
        (180.0, 10.0, 10.0)
    );
    assert_eq!(
        (CHANNEL_TAG_HEIGHT, CHANNEL_TAG_PADDING_X, TITLE_GAP),
        (18.0, 7.0, 8.0)
    );
    assert_eq!(BANNER_PADDING, [12.0, 16.0]);
    assert_eq!((BUTTON_PADDING, BUTTON_GAP), ([6.0, 12.0], 8.0));
    assert_eq!(
        (SETTINGS_WIDTH, SETTINGS_PADDING, SETTINGS_GAP),
        (640.0, [16.0, 16.0, 32.0], 16.0)
    );
    assert_eq!((CARD_PADDING, SETTING_ROW_PADDING), (16.0, 12.0));
    assert_eq!(
        (SWITCH_WIDTH, SWITCH_HEIGHT, KNOB_SIZE, KNOB_INSET),
        (46.0, 28.0, 20.0, 3.0)
    );
    assert_eq!(KNOB_TRAVEL, 18.0);
    assert_eq!(
        (SEGMENT_PADDING, SEGMENT_RADIUS, SEGMENT_BORDER),
        (2.0, 5.0, 1.5)
    );
    assert_eq!(
        (ICON_SIZE, ICON_PICK_MIN_HEIGHT, ICON_COLUMNS),
        (72.0, 140.0, 3)
    );
}
