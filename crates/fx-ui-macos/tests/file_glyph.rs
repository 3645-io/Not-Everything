//! Contract tests for `fx_ui_macos::file_glyph`: which glyph a result row
//! shows (the owner's choice: images and documents by the search's
//! `pic:` and `doc:` lists, spreadsheets `xls`, `xlsx`, `numbers`, `csv`,
//! folders, everything else a document) and the glyphs' path data from the
//! Components board (a 20-unit grid, 1.5 stroke, round caps and joins).

use fx_query::syntax::macros::macro_extensions;
use fx_ui_macos::file_glyph::{
    FileKind, GRID, Glyph, PathOp, Point, SPREADSHEET_EXTENSIONS, STROKE_WIDTH, extension, flatten,
};

#[test]
fn folders_are_folders_whatever_their_name() {
    assert_eq!(FileKind::of("Invoices 2026", true), FileKind::Folder);
    assert_eq!(FileKind::of("Photos.app", true), FileKind::Folder);
    assert_eq!(FileKind::of("scan.png", true), FileKind::Folder);
}

#[test]
fn the_canvas_rows_get_the_canvas_glyphs() {
    let rows = [
        ("Invoice-2026-03.pdf", FileKind::Document),
        ("invoice-tracker-2026.xlsx", FileKind::Spreadsheet),
        ("invoice_2026_template.docx", FileKind::Document),
        ("invoice-2026-scan.png", FileKind::Image),
        ("Invoice-2026-draft.pages", FileKind::Document),
        ("Tax return 2025.pdf", FileKind::Document),
        ("Screenshot 2026-11-05 at 10.12.04.png", FileKind::Image),
        ("Budget 2026.numbers", FileKind::Spreadsheet),
        ("budget.rs", FileKind::Document),
        ("budget-chart.png", FileKind::Image),
        ("budget-template.xlsx", FileKind::Spreadsheet),
        ("Budget review Q3.key", FileKind::Document),
        ("README.md", FileKind::Document),
    ];
    for (name, kind) in rows {
        assert_eq!(FileKind::of(name, false), kind, "{name}");
    }
}

#[test]
fn every_picture_extension_is_an_image() {
    let pic = macro_extensions("pic").expect("pic: is a macro");
    for ext in pic {
        assert_eq!(
            FileKind::of(&format!("a.{ext}"), false),
            FileKind::Image,
            "{ext}"
        );
        let upper = ext.to_ascii_uppercase();
        assert_eq!(
            FileKind::of(&format!("a.{upper}"), false),
            FileKind::Image,
            "{upper}"
        );
    }
}

#[test]
fn spreadsheets_are_the_four_extensions_only() {
    assert_eq!(SPREADSHEET_EXTENSIONS, ["xls", "xlsx", "numbers", "csv"]);
    for ext in SPREADSHEET_EXTENSIONS {
        assert_eq!(
            FileKind::of(&format!("a.{ext}"), false),
            FileKind::Spreadsheet
        );
    }
    assert_eq!(FileKind::of("DATA.CSV", false), FileKind::Spreadsheet);
    // Other spreadsheet-like documents of the doc: list stay documents.
    for ext in ["xlsm", "xlsb", "tsv", "ods"] {
        assert_eq!(
            FileKind::of(&format!("a.{ext}"), false),
            FileKind::Document,
            "{ext}"
        );
    }
}

#[test]
fn every_other_document_and_unknown_file_is_a_document() {
    let doc = macro_extensions("doc").expect("doc: is a macro");
    for ext in doc.iter().filter(|e| !SPREADSHEET_EXTENSIONS.contains(e)) {
        assert_eq!(
            FileKind::of(&format!("a.{ext}"), false),
            FileKind::Document,
            "{ext}"
        );
    }
    for name in [
        "archive.zip",
        "song.mp3",
        "noext",
        ".png",
        "Makefile",
        "x.",
        "a.ÉPS",
    ] {
        assert_eq!(FileKind::of(name, false), FileKind::Document, "{name}");
    }
}

#[test]
fn extensions_follow_the_search_rule() {
    assert_eq!(extension("a.tar.gz"), "gz");
    assert_eq!(extension(".png"), "");
    assert_eq!(extension("noext"), "");
    assert_eq!(extension("x."), "");
    assert_eq!(extension(".hidden.txt"), "txt");
}

#[test]
fn kinds_name_their_glyphs() {
    let pairs: Vec<(&str, Glyph)> = FileKind::ALL
        .iter()
        .map(|k| (k.name(), k.glyph()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("Folder", Glyph::Folder),
            ("Document", Glyph::Document),
            ("Image", Glyph::Image),
            ("Spreadsheet", Glyph::Spreadsheet),
        ]
    );
}

#[test]
fn the_grid_and_stroke() {
    assert_eq!(GRID, 20.0);
    assert_eq!(STROKE_WIDTH, 1.5);
    assert_eq!(Glyph::Folder.stroke_width(), 1.5);
    // The sort chevrons are drawn with a 2-unit stroke (`.ne-th svg`).
    assert_eq!(Glyph::SortUp.stroke_width(), 2.0);
    assert_eq!(Glyph::SortDown.stroke_width(), 2.0);
}

fn p(x: f64, y: f64) -> Point {
    Point { x, y }
}

/// The on-curve points of a path, subpath by subpath: each move starts a
/// subpath; lines and curves add their end point; a close repeats the
/// subpath's start.
fn corners(ops: &[PathOp]) -> Vec<Vec<Point>> {
    let mut out: Vec<Vec<Point>> = Vec::new();
    for op in ops {
        match *op {
            PathOp::MoveTo(a) => out.push(vec![a]),
            PathOp::LineTo(a) | PathOp::CubicTo(_, _, a) => out.last_mut().unwrap().push(a),
            PathOp::Close => {
                let first = out.last().unwrap()[0];
                out.last_mut().unwrap().push(first);
            }
        }
    }
    out
}

fn close(a: &[Vec<Point>], b: &[Vec<Point>]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(s, t)| {
            s.len() == t.len()
                && s.iter()
                    .zip(t)
                    .all(|(u, v)| (u.x - v.x).abs() < 1e-9 && (u.y - v.y).abs() < 1e-9)
        })
}

#[test]
fn the_folder_glyph() {
    let want = vec![
        vec![
            p(2.75, 5.5),
            p(3.75, 4.5),
            p(7.35, 4.5),
            p(8.95, 6.25),
            p(16.25, 6.25),
            p(17.25, 7.25),
            p(17.25, 15.25),
            p(16.25, 16.25),
            p(3.75, 16.25),
            p(2.75, 15.25),
            p(2.75, 5.5),
        ],
        vec![p(2.75, 8.25), p(17.25, 8.25)],
    ];
    assert!(
        close(&corners(&Glyph::Folder.path()), &want),
        "{:?}",
        corners(&Glyph::Folder.path())
    );
}

#[test]
fn the_document_glyph() {
    let want = vec![
        vec![
            p(5.25, 2.75),
            p(11.25, 2.75),
            p(14.75, 6.25),
            p(14.75, 17.25),
            p(5.25, 17.25),
            p(5.25, 2.75),
        ],
        vec![p(11.25, 2.75), p(11.25, 6.25), p(14.75, 6.25)],
        vec![p(7.75, 10.25), p(12.25, 10.25)],
        vec![p(7.75, 13.25), p(12.25, 13.25)],
    ];
    assert!(close(&corners(&Glyph::Document.path()), &want));
    assert!(
        Glyph::Document
            .path()
            .iter()
            .all(|op| !matches!(op, PathOp::CubicTo(..)))
    );
}

fn rounded_rect(x: f64, y: f64, w: f64, h: f64, r: f64) -> Vec<Point> {
    vec![
        p(x + r, y),
        p(x + w - r, y),
        p(x + w, y + r),
        p(x + w, y + h - r),
        p(x + w - r, y + h),
        p(x + r, y + h),
        p(x, y + h - r),
        p(x, y + r),
        p(x + r, y),
        p(x + r, y),
    ]
}

fn circle(cx: f64, cy: f64, r: f64) -> Vec<Point> {
    vec![
        p(cx + r, cy),
        p(cx, cy + r),
        p(cx - r, cy),
        p(cx, cy - r),
        p(cx + r, cy),
        p(cx + r, cy),
    ]
}

#[test]
fn the_image_glyph() {
    let want = vec![
        rounded_rect(3.0, 3.75, 14.0, 12.5, 1.5),
        circle(7.25, 8.0, 1.25),
        vec![
            p(3.5, 14.25),
            p(7.25, 10.75),
            p(10.0, 13.25),
            p(12.25, 11.25),
            p(16.5, 15.0),
        ],
    ];
    assert!(
        close(&corners(&Glyph::Image.path()), &want),
        "{:?}",
        corners(&Glyph::Image.path())
    );
}

#[test]
fn the_spreadsheet_glyph() {
    let want = vec![
        rounded_rect(3.25, 3.0, 13.5, 14.0, 1.5),
        vec![p(3.25, 7.75), p(16.75, 7.75)],
        vec![p(3.25, 12.25), p(16.75, 12.25)],
        vec![p(8.25, 7.75), p(8.25, 17.0)],
    ];
    assert!(close(&corners(&Glyph::Spreadsheet.path()), &want));
}

#[test]
fn the_search_and_sort_glyphs() {
    let search = vec![circle(8.75, 8.75, 5.5), vec![p(13.0, 13.0), p(17.0, 17.0)]];
    assert!(close(&corners(&Glyph::Search.path()), &search));
    let up = vec![vec![p(6.0, 12.0), p(10.0, 8.0), p(14.0, 12.0)]];
    assert!(close(&corners(&Glyph::SortUp.path()), &up));
    let down = vec![vec![p(6.0, 8.0), p(10.0, 12.0), p(14.0, 8.0)]];
    assert!(close(&corners(&Glyph::SortDown.path()), &down));
}

#[test]
fn arcs_stay_on_their_circles() {
    // Flattened, every point of the search glyph's lens is 5.5 units from
    // its centre (the cubic arcs are within 0.03% of a circle).
    let lines = flatten(&Glyph::Search.path(), 0.01);
    for pt in &lines[0] {
        let d = ((pt.x - 8.75).powi(2) + (pt.y - 8.75).powi(2)).sqrt();
        assert!((d - 5.5).abs() < 0.005, "{pt:?} at {d}");
    }
    // The folder's top-left corner arc stays within its quarter circle.
    let folder = flatten(&Glyph::Folder.path(), 0.01);
    let corner: Vec<&Point> = folder[0]
        .iter()
        .filter(|pt| pt.x < 3.75 && pt.y < 5.5)
        .collect();
    assert!(!corner.is_empty());
    for pt in corner {
        let d = ((pt.x - 3.75).powi(2) + (pt.y - 5.5).powi(2)).sqrt();
        assert!((d - 1.0).abs() < 0.005, "{pt:?}");
    }
}

#[test]
fn every_glyph_fits_its_grid_with_its_stroke() {
    for glyph in Glyph::ALL {
        let half = glyph.stroke_width() / 2.0;
        for line in flatten(&glyph.path(), 0.01) {
            assert!(line.len() >= 2, "{glyph:?}");
            for pt in line {
                assert!(
                    pt.x - half >= 0.0 && pt.x + half <= GRID,
                    "{glyph:?} {pt:?}"
                );
                assert!(
                    pt.y - half >= 0.0 && pt.y + half <= GRID,
                    "{glyph:?} {pt:?}"
                );
            }
        }
    }
}
