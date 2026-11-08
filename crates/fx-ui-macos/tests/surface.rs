//! Contract tests for `fx_ui_macos::surface`: the canvas's surfaces in each
//! theme, their nine-part template layouts, the CSS shadow maths checked
//! against the Gaussian it approximates, and the glyph rasteriser.

use fx_ui_macos::file_glyph::Glyph;
use fx_ui_macos::surface::{
    Border, Corners, Insets, Pixmap, STRETCH, SizeClass, Surface, SurfaceStyle, TemplateLayout,
    render, render_glyph,
};
use fx_ui_macos::theme::{Rgb, Rgba, Shadow, ShadowToken, Theme, Token};

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

#[test]
fn styles_come_from_the_tokens() {
    for theme in Theme::ALL {
        let c = |t| Rgba::opaque(theme.color(t));
        let shadows = |t| theme.shadow(t).to_vec();
        let panel = Surface::Panel.style(theme);
        assert_eq!(
            panel,
            SurfaceStyle {
                fill: Some(c(Token::Panel)),
                border: None,
                corners: Corners::all(16.0),
                shadows: shadows(ShadowToken::Raised),
            }
        );
        let top = Surface::TopBar.style(theme);
        assert_eq!(top.corners, Corners::bottom(16.0));
        assert_eq!(top.shadows, shadows(ShadowToken::Float));
        let field = Surface::Field.style(theme);
        assert_eq!(field.fill, Some(c(Token::Well)));
        assert_eq!(
            field.border,
            Some(Border {
                width: 1.0,
                color: c(Token::Edge),
            })
        );
        assert_eq!(field.shadows, shadows(ShadowToken::Pressed));
        let focused = Surface::FieldFocused.style(theme);
        assert_eq!(focused.border.unwrap().color, c(Token::GreenInk));
        let mut ring = shadows(ShadowToken::Pressed);
        ring.push(Shadow::ring(2.0, c(Token::GreenInk)));
        assert_eq!(focused.shadows, ring);
        for selected in [Surface::ChipOn, Surface::RowSelected, Surface::Recording] {
            let s = selected.style(theme);
            assert_eq!(s.fill, Some(c(Token::GreenWash)), "{selected:?}");
            assert_eq!(
                s.border,
                Some(Border {
                    width: 2.0,
                    color: c(Token::GreenInk),
                })
            );
            assert_eq!(s.corners, Corners::all(10.0));
            assert_eq!(s.shadows, shadows(ShadowToken::Pressed));
        }
        assert_eq!(
            Surface::ChipOff.style(theme).shadows,
            shadows(ShadowToken::Raised)
        );
        let key = Surface::Key.style(theme);
        assert_eq!(
            (key.fill, key.corners),
            (Some(c(Token::Panel)), Corners::all(6.0))
        );
        assert_eq!(key.shadows, shadows(ShadowToken::Key));
        assert_eq!(Surface::SwitchOn.style(theme).fill, Some(c(Token::Green)));
        assert_eq!(Surface::KnobOn.style(theme).fill, Some(c(Token::Glass)));
        assert_eq!(
            Surface::KnobOff.style(theme).shadows,
            shadows(ShadowToken::Key)
        );
        assert_eq!(
            Surface::PrimaryButton.style(theme).fill,
            Some(c(Token::Green))
        );
        assert_eq!(
            Surface::Dot.style(theme).shadows,
            [Shadow::ring(3.0, c(Token::GreenWash))]
        );
        assert_eq!(Surface::ChannelBeta.style(theme).fill, Some(c(Token::Vol)));
        // The shortcut recorder under the pointer (`.rec.is-hover`): the
        // row hover's well, 10 round.
        assert_eq!(
            Surface::RecorderHover.style(theme),
            Surface::RowHover.style(theme)
        );
        let callout = Surface::Callout.style(theme);
        assert_eq!(callout.fill, Some(c(Token::CautionWash)));
        assert_eq!(callout.border.unwrap().color, c(Token::CautionEdge));
    }
}

#[test]
fn flat_surfaces_need_no_image() {
    let flat = [
        Surface::RowHover,
        Surface::SegmentOn,
        Surface::IconPickHover,
        Surface::RecorderHover,
        Surface::Callout,
        Surface::ChannelRc,
        Surface::ChannelBeta,
    ];
    for s in Surface::ALL {
        assert_eq!(
            !s.style(Theme::Light).needs_image(),
            flat.contains(&s),
            "{s:?}"
        );
    }
}

#[test]
fn radii_clamp_as_css_does() {
    assert_eq!(Corners::all(999.0).clamped(46.0, 28.0), Corners::all(14.0));
    assert_eq!(Corners::all(999.0).clamped(10.0, 180.0), Corners::all(5.0));
    assert_eq!(Corners::all(16.0).clamped(400.0, 300.0), Corners::all(16.0));
    assert_eq!(
        Corners::bottom(16.0).clamped(1280.0, 102.0),
        Corners::bottom(16.0)
    );
    assert_eq!(Corners::all(10.0).clamped(12.0, 40.0), Corners::all(6.0));
}

// ---------------------------------------------------------------------------
// Template layouts
// ---------------------------------------------------------------------------

#[test]
fn a_panel_stretches_both_ways() {
    let style = Surface::Panel.style(Theme::Light);
    // Raised (light): the farthest shadow is 3px 3px 10px: 3 + 3 × 5 = 18.
    assert_eq!(style.reach(), 18.0);
    let l = TemplateLayout::new(&style, 400.0, 300.0);
    // A corner is its 16-point radius plus that reach.
    let cap = 34.0;
    assert!(l.stretch_x && l.stretch_y);
    assert_eq!(
        (l.width, l.height),
        (2.0 * cap + STRETCH, 2.0 * cap + STRETCH)
    );
    // Up and left the white shadow reaches 3 + 3 × 4 = 15, down and right
    // the dark one 3 + 3 × 5 = 18.
    assert_eq!(
        l.outsets,
        Insets {
            top: 15.0,
            left: 15.0,
            bottom: 18.0,
            right: 18.0,
        }
    );
    assert_eq!(
        l.cap_insets,
        Insets {
            top: 15.0 + cap,
            left: 15.0 + cap,
            bottom: 18.0 + cap,
            right: 18.0 + cap,
        }
    );
    assert_eq!((l.image_width(), l.image_height()), (103.0, 103.0));
    assert_eq!(
        l.size_class(),
        SizeClass {
            width: None,
            height: None,
        }
    );
    // Every panel size shares the template.
    assert_eq!(TemplateLayout::new(&style, 1_280.0, 640.0), l);
    let [x, y, w, h] = l.contents_center(false);
    assert_eq!([x, w], [49.0 / 103.0, 2.0 / 103.0]);
    assert_eq!([y, h], [52.0 / 103.0, 2.0 / 103.0]);
    assert_eq!(l.contents_center(true)[1], 49.0 / 103.0);
}

#[test]
fn a_row_stretches_across_and_keeps_its_height() {
    let style = Surface::RowSelected.style(Theme::Dark);
    let l = TemplateLayout::new(&style, 900.0, 40.0);
    // Pressed: 2 + 3 × 2.5 = 9.5 past a 10-point radius: a 20-point cap.
    assert!(l.stretch_x && !l.stretch_y);
    assert_eq!((l.width, l.height), (42.0, 40.0));
    assert_eq!(l.outsets, Insets::default());
    assert_eq!(
        l.cap_insets,
        Insets {
            top: 20.0,
            left: 20.0,
            bottom: 20.0,
            right: 20.0,
        }
    );
    assert_eq!(
        l.size_class(),
        SizeClass {
            width: None,
            height: Some(320),
        }
    );
    assert_eq!(
        l.contents_center(false),
        [20.0 / 42.0, 0.0, 2.0 / 42.0, 1.0]
    );
}

#[test]
fn small_parts_are_rendered_at_their_own_size() {
    let key = Surface::Key.style(Theme::Light);
    let l = TemplateLayout::new(&key, 22.0, 22.0);
    assert!(!l.stretch_x && !l.stretch_y);
    assert_eq!((l.width, l.height), (22.0, 22.0));
    // Key (light): -1px -1px 2px up-left (1 + 3) and 1px 1px 3px down-right
    // (1 + 4.5, rounded up).
    assert_eq!(
        l.outsets,
        Insets {
            top: 4.0,
            left: 4.0,
            bottom: 6.0,
            right: 6.0,
        }
    );
    // A wide key (Space) stretches across.
    assert!(TemplateLayout::new(&key, 50.0, 22.0).stretch_x);
    let switch = TemplateLayout::new(&Surface::SwitchOff.style(Theme::Light), 46.0, 28.0);
    assert!(!switch.stretch_x && !switch.stretch_y);
    assert_eq!(switch.corners, Corners::all(14.0));
    // Sizes round to whole points.
    assert_eq!(TemplateLayout::new(&key, 21.6, 22.2), l);
}

#[test]
fn exact_layouts_never_stretch() {
    let l = TemplateLayout::exact(&Surface::Panel.style(Theme::Light), 200.0, 160.0);
    assert!(!l.stretch_x && !l.stretch_y);
    assert_eq!((l.image_width(), l.image_height()), (233.0, 193.0));
}

// ---------------------------------------------------------------------------
// Pixels
// ---------------------------------------------------------------------------

/// The standard normal distribution function, from Abramowitz and Stegun's
/// erf approximation 7.1.26 (error below 1.5e-7).
fn phi(x: f64) -> f64 {
    let z = x / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z.abs());
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    0.5 * (1.0 + if z >= 0.0 { erf } else { -erf })
}

/// Premultiplied RGBA from 0 to 1.
type Px = [f64; 4];

fn over(dst: Px, rgb: Rgb, alpha: f64) -> Px {
    let [r, g, b] = rgb.components();
    let keep = 1.0 - alpha;
    [
        r * alpha + dst[0] * keep,
        g * alpha + dst[1] * keep,
        b * alpha + dst[2] * keep,
        alpha + dst[3] * keep,
    ]
}

fn assert_near(p: &Pixmap, x: usize, y: usize, want: Px, tolerance: u8, what: &str) {
    let got = p.pixel(x, y);
    let want = want.map(|c| (c * 255.0).round() as u8);
    assert!(
        got.iter()
            .zip(want)
            .all(|(g, w)| g.abs_diff(w) <= tolerance),
        "{what} at ({x}, {y}): got {got:?}, want {want:?}"
    );
}

#[test]
fn raised_shadows_are_gaussians_of_half_the_blur() {
    // Far from the corners each shadow's edge is a step blurred by a
    // Gaussian of σ = blur / 2: its coverage at distance d inside the
    // shadow's offset edge is Φ(d / σ).
    let style = Surface::Panel.style(Theme::Light);
    let l = TemplateLayout::exact(&style, 200.0, 200.0);
    let p = render(&style, &l, 1.0);
    assert_eq!((p.width, p.height), (233, 233));
    let mid = 15 + 100;
    // The panel's middle is its fill.
    assert_near(&p, 15 + 100, mid, [0.949, 0.961, 0.965, 1.0], 1, "fill");
    assert_eq!(p.color(115, mid), ([0xf2, 0xf5, 0xf6], 255));
    let white = Rgb(0xffffff);
    let dark = Rgb(0x364950);
    for k in 0..15 {
        let d = k as f64 + 0.5;
        // Right of the right edge: the dark shadow (3px right, σ 5) under
        // the white one (3px left, σ 4).
        let mut want = over([0.0; 4], dark, 0.18 * phi((3.0 - d) / 5.0));
        want = over(want, white, 0.85 * phi((-3.0 - d) / 4.0));
        assert_near(&p, 15 + 200 + k, mid, want, 3, "right edge");
        // Left of the left edge, mirrored.
        let mut want = over([0.0; 4], dark, 0.18 * phi((-3.0 - d) / 5.0));
        want = over(want, white, 0.85 * phi((3.0 - d) / 4.0));
        assert_near(&p, 15 - 1 - k, mid, want, 3, "left edge");
    }
    // Past the reach nothing is drawn.
    assert_eq!(p.pixel(0, 0), [0, 0, 0, 0]);
    assert_eq!(p.pixel(232, 232)[3], 0);
}

#[test]
fn pressed_shadows_sit_inside_the_padding_box() {
    let style = Surface::Field.style(Theme::Light);
    let l = TemplateLayout::exact(&style, 200.0, 46.0);
    let p = render(&style, &l, 2.0);
    assert_eq!((p.width, p.height), (400, 92));
    let x = 200;
    // The 1-point border is the edge colour, over the inset shadows.
    assert_eq!(p.color(x, 0), ([0x7b, 0x85, 0x8a], 255));
    assert_eq!(p.color(x, 1), ([0x7b, 0x85, 0x8a], 255));
    // Inside the top edge: the well, the white highlight (-2px -2px 5px:
    // its edge 2 points above the padding box, σ 2.5) and on top the dark
    // shadow (2px 2px 5px: its edge 2 points below).
    let well = over([0.0; 4], Rgb(0xe3e9eb), 1.0);
    for row in 2..14 {
        let d = (row as f64 + 0.5) / 2.0 - 1.0; // points below the padding edge
        let mut want = over(well, Rgb(0xffffff), 0.9 * phi((-2.0 - d) / 2.5));
        want = over(want, Rgb(0x364950), 0.16 * phi((2.0 - d) / 2.5));
        assert_near(&p, x, row, want, 3, "top inner edge");
    }
    // The middle of the field is the well.
    assert_eq!(p.color(x, 46), ([0xe3, 0xe9, 0xeb], 255));
    // Nothing outside: the field has no outer shadow, and its corners are
    // round.
    assert_eq!(l.outsets, Insets::default());
    assert_eq!(p.pixel(0, 0), [0, 0, 0, 0]);
    assert_eq!(p.pixel(399, 91), [0, 0, 0, 0]);
}

#[test]
fn the_focus_ring_is_two_points_of_green_ink() {
    let style = Surface::FieldFocused.style(Theme::Dark);
    let l = TemplateLayout::exact(&style, 120.0, 46.0);
    assert_eq!(
        l.outsets,
        Insets {
            top: 2.0,
            left: 2.0,
            bottom: 2.0,
            right: 2.0,
        }
    );
    let p = render(&style, &l, 1.0);
    let green_ink = [0x3f, 0xdc, 0x84];
    let mid = 2 + 23;
    // The ring, then the rim, both green-ink.
    for x in [0, 1, 2, 122, 123] {
        assert_eq!(p.color(x, mid), (green_ink, 255), "x {x}");
    }
    assert_eq!(p.color(60, 0), (green_ink, 255));
    // Inside: the well (dark), darkened at the top by the inset shadow.
    assert_eq!(p.color(62, mid).0, [0x2c, 0x2c, 0x2c]);
}

fn column(p: &Pixmap, x: usize) -> Vec<[u8; 4]> {
    (0..p.height).map(|y| p.pixel(x, y)).collect()
}

fn same(a: &[[u8; 4]], b: &[[u8; 4]], tolerance: u8) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(u, v)| u.iter().zip(v).all(|(x, y)| x.abs_diff(*y) <= tolerance))
}

#[test]
fn a_stretched_template_draws_what_an_exact_render_draws() {
    for (surface, theme, w, h) in [
        (Surface::Panel, Theme::Light, 260.0, 180.0),
        (Surface::Panel, Theme::Dark, 260.0, 180.0),
        (Surface::RowSelected, Theme::Light, 300.0, 40.0),
        (Surface::ChipOff, Theme::Dark, 120.0, 40.0),
        (Surface::TopBar, Theme::Light, 400.0, 102.0),
    ] {
        let style = surface.style(theme);
        let t = TemplateLayout::new(&style, w, h);
        assert!(t.stretch_x, "{surface:?}");
        let e = TemplateLayout::exact(&style, w, h);
        for scale in [1.0, 2.0] {
            let tp = render(&style, &t, scale);
            let ep = render(&style, &e, scale);
            let s = |pt: f64| (pt * scale) as usize;
            let (left, right) = (s(t.cap_insets.left), s(t.cap_insets.right));
            let (top, bottom) = if t.stretch_y {
                (s(t.cap_insets.top), s(t.cap_insets.bottom))
            } else {
                (tp.height, 0)
            };
            // A column of the template stands for a column of the exact
            // image: its top cap, then (when the height stretches) its
            // middle row repeated, then its bottom cap.
            let stretched = |x: usize| -> Vec<[u8; 4]> {
                let c = column(&tp, x);
                let mut out = c[..top].to_vec();
                if t.stretch_y {
                    out.extend(std::iter::repeat_n(c[top], ep.height - top - bottom));
                    out.extend_from_slice(&c[tp.height - bottom..]);
                }
                out
            };
            // The exact image's stretched span is the template's middle
            // column; its caps are the template's caps.
            let mid = stretched(left);
            for x in left..ep.width - right {
                assert!(
                    same(&column(&ep, x), &mid, 1),
                    "{surface:?} column {x} at {scale}x"
                );
            }
            for x in 0..left {
                assert!(
                    same(&column(&ep, x), &stretched(x), 1),
                    "{surface:?} cap {x} at {scale}x"
                );
            }
            for k in 1..=right {
                let (ex, tx) = (ep.width - k, tp.width - k);
                assert!(
                    same(&column(&ep, ex), &stretched(tx), 1),
                    "{surface:?} cap -{k} at {scale}x"
                );
            }
        }
    }
}

#[test]
fn every_surface_renders_in_both_themes() {
    for surface in Surface::ALL {
        for theme in Theme::ALL {
            let style = surface.style(theme);
            let l = TemplateLayout::new(&style, 160.0, 40.0);
            for scale in [1.0, 2.0] {
                let p = render(&style, &l, scale);
                assert_eq!(p.width, (l.image_width() * scale) as usize);
                assert_eq!(p.height, (l.image_height() * scale) as usize);
                assert_eq!(p.data.len(), p.width * p.height * 4);
                // The element's centre is the surface's fill.
                let (cx, cy) = (
                    ((l.outsets.left + l.width / 2.0) * scale) as usize,
                    ((l.outsets.top + l.height / 2.0) * scale) as usize,
                );
                let fill = style.fill.expect("every surface has a fill");
                let (rgb, a) = p.color(cx, cy);
                assert_eq!(a, 255, "{surface:?} {theme:?}");
                let want = fill.rgb.bytes();
                assert!(
                    rgb.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 1),
                    "{surface:?} {theme:?}: {rgb:?} vs {want:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Glyphs
// ---------------------------------------------------------------------------

#[test]
fn glyphs_are_strokes_in_a_template_mask() {
    // At 20 points and 1x a grid unit is a pixel.
    let folder = render_glyph(Glyph::Folder, 20.0, 1.0);
    assert_eq!((folder.width, folder.height), (20, 20));
    // The left side runs along x = 2.75, 1.5 wide: pixel column 2 is
    // covered, column 1 is not, column 3 half.
    assert_eq!(folder.pixel(2, 11), [0, 0, 0, 255]);
    assert_eq!(folder.pixel(1, 11), [0, 0, 0, 0]);
    assert_eq!(folder.pixel(3, 11), [0, 0, 0, 128]);
    // Inside the folder nothing is filled.
    assert_eq!(folder.pixel(10, 12)[3], 0);

    let doc = render_glyph(Glyph::Document, 20.0, 1.0);
    // The first text line, y = 10.25 from x = 7.75 to 12.25.
    assert_eq!(doc.pixel(10, 10), [0, 0, 0, 255]);
    assert_eq!(doc.pixel(10, 8)[3], 0);
    // Round caps reach half the stroke past the line's ends.
    assert!(doc.pixel(7, 10)[3] > 100);
    assert_eq!(doc.pixel(6, 10)[3], 0);

    // Every glyph at the row's 18 points and 2x: 36 × 36, black only.
    for glyph in Glyph::ALL {
        let p = render_glyph(glyph, 18.0, 2.0);
        assert_eq!((p.width, p.height), (36, 36));
        assert!(p.data.chunks(4).all(|px| px[..3] == [0, 0, 0]), "{glyph:?}");
        let inked = p.data.chunks(4).filter(|px| px[3] > 0).count();
        assert!(inked > 60 && inked < 36 * 36 / 2, "{glyph:?}: {inked}");
    }
}
