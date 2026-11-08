//! The interface's line glyphs (the canvas's FileGlyph set): which
//! glyph a result row shows, and every glyph's path.
//!
//! **Choice** ([`FileKind::of`], the owner's choice): a folder
//! shows the Folder glyph; a file whose extension is `xls`, `xlsx`,
//! `numbers` or `csv` the Spreadsheet glyph; one in the search's `pic:`
//! list (`fx_query`'s type macros) the Image glyph; every other file,
//! including the rest of the `doc:` list, the Document glyph. Extensions
//! are compared without regard to ASCII case, and found as the search finds
//! them ([`extension`]). Glyphs are chosen from the name only, never from
//! the file, so nothing is read or downloaded.
//!
//! **Paths** ([`Glyph::path`]): the Components board's SVG paths on a
//! 20-unit grid ([`GRID`]) with y running down, drawn with a 1.5-unit stroke
//! ([`STROKE_WIDTH`]; 2 for the sort chevrons), round caps and round joins,
//! never filled. Arcs are written as cubic Béziers (a quarter circle per
//! curve). `crate::surface` rasterises them into template images; the view
//! tints them `muted`, or `green-ink` on the selected row.

#![forbid(unsafe_code)]

use fx_query::syntax::macros::macro_extensions;

use crate::FixedList;

/// The glyphs' coordinate grid: 20 × 20 units (an SVG `viewBox` of
/// `0 0 20 20`).
pub const GRID: f64 = 20.0;

/// The glyphs' stroke, in grid units.
pub const STROKE_WIDTH: f64 = 1.5;

/// The extensions shown as spreadsheets.
pub const SPREADSHEET_EXTENSIONS: [&str; 4] = ["xls", "xlsx", "numbers", "csv"];

/// The kind of item a result row is, as far as its glyph goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// A folder (packages such as `.app` bundles included).
    Folder,
    /// Any file that is not an image or a spreadsheet.
    Document,
    /// A file in the search's `pic:` list.
    Image,
    /// An `xls`, `xlsx`, `numbers` or `csv` file.
    Spreadsheet,
}

impl FileKind {
    /// Every kind, in the Components board's order.
    pub const ALL: FixedList<FileKind, 4> = FixedList([
        FileKind::Folder,
        FileKind::Document,
        FileKind::Image,
        FileKind::Spreadsheet,
    ]);

    /// The kind of the entry named `name`.
    pub fn of(name: &str, is_folder: bool) -> FileKind {
        if is_folder {
            return FileKind::Folder;
        }
        let ext = extension(name);
        let is = |list: &[&str]| list.iter().any(|e| e.eq_ignore_ascii_case(ext));
        if ext.is_empty() {
            FileKind::Document
        } else if is(&SPREADSHEET_EXTENSIONS) {
            FileKind::Spreadsheet
        } else if is(macro_extensions("pic").unwrap_or_default()) {
            FileKind::Image
        } else {
            FileKind::Document
        }
    }

    /// The kind's name on the Components board.
    pub const fn name(self) -> &'static str {
        match self {
            FileKind::Folder => "Folder",
            FileKind::Document => "Document",
            FileKind::Image => "Image",
            FileKind::Spreadsheet => "Spreadsheet",
        }
    }

    /// The glyph a row of this kind shows.
    pub const fn glyph(self) -> Glyph {
        match self {
            FileKind::Folder => Glyph::Folder,
            FileKind::Document => Glyph::Document,
            FileKind::Image => Glyph::Image,
            FileKind::Spreadsheet => Glyph::Spreadsheet,
        }
    }
}

/// The text after the last `.` of a name, or `""` when there is none or the
/// only `.` starts the name: the rule of the search's `ext:` filter.
pub fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i + 1..],
        _ => "",
    }
}

/// A point on the glyph grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Rightwards.
    pub x: f64,
    /// Downwards.
    pub y: f64,
}

/// One step of a glyph's path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathOp {
    /// Starts a subpath.
    MoveTo(Point),
    /// A straight line.
    LineTo(Point),
    /// A cubic Bézier: two control points, then the end.
    CubicTo(Point, Point, Point),
    /// Closes the subpath with a line back to its start.
    Close,
}

/// A glyph of the interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// A folder (Components board, FileGlyph).
    Folder,
    /// A document.
    Document,
    /// An image.
    Image,
    /// A spreadsheet.
    Spreadsheet,
    /// The lens inside the search field.
    Search,
    /// The ascending sort chevron in a column header.
    SortUp,
    /// The descending sort chevron.
    SortDown,
}

impl Glyph {
    /// Every glyph.
    pub const ALL: FixedList<Glyph, 7> = FixedList([
        Glyph::Folder,
        Glyph::Document,
        Glyph::Image,
        Glyph::Spreadsheet,
        Glyph::Search,
        Glyph::SortUp,
        Glyph::SortDown,
    ]);

    /// The glyph's stroke in grid units: [`STROKE_WIDTH`], or 2 for the
    /// sort chevrons (`.ne-th svg { stroke-width: 2 }`).
    pub const fn stroke_width(self) -> f64 {
        match self {
            Glyph::SortUp | Glyph::SortDown => 2.0,
            _ => STROKE_WIDTH,
        }
    }

    /// The glyph's path on the [`GRID`]. The SVG it transcribes is quoted
    /// for each glyph.
    pub fn path(self) -> Vec<PathOp> {
        let mut b = PathBuilder::default();
        match self {
            Glyph::Folder => {
                // M2.75 5.5a1 1 0 0 1 1-1h3.6l1.6 1.75h7.3a1 1 0 0 1 1 1v8
                // a1 1 0 0 1-1 1H3.75a1 1 0 0 1-1-1z  M2.75 8.25h14.5
                b.move_to(2.75, 5.5)
                    .arc_to(3.75, 4.5, 3.75, 5.5)
                    .line_to(7.35, 4.5)
                    .line_to(8.95, 6.25)
                    .line_to(16.25, 6.25)
                    .arc_to(17.25, 7.25, 16.25, 7.25)
                    .line_to(17.25, 15.25)
                    .arc_to(16.25, 16.25, 16.25, 15.25)
                    .line_to(3.75, 16.25)
                    .arc_to(2.75, 15.25, 3.75, 15.25)
                    .close();
                b.move_to(2.75, 8.25).line_to(17.25, 8.25);
            }
            Glyph::Document => {
                // M5.25 2.75h6l3.5 3.5v11h-9.5z  M11.25 2.75v3.5h3.5
                // M7.75 10.25h4.5M7.75 13.25h4.5
                b.move_to(5.25, 2.75)
                    .line_to(11.25, 2.75)
                    .line_to(14.75, 6.25)
                    .line_to(14.75, 17.25)
                    .line_to(5.25, 17.25)
                    .close();
                b.move_to(11.25, 2.75)
                    .line_to(11.25, 6.25)
                    .line_to(14.75, 6.25);
                b.move_to(7.75, 10.25).line_to(12.25, 10.25);
                b.move_to(7.75, 13.25).line_to(12.25, 13.25);
            }
            Glyph::Image => {
                // <rect x="3" y="3.75" width="14" height="12.5" rx="1.5">
                // <circle cx="7.25" cy="8" r="1.25">
                // M3.5 14.25l3.75-3.5 2.75 2.5 2.25-2 4.25 3.75
                b.rounded_rect(3.0, 3.75, 14.0, 12.5, 1.5);
                b.circle(7.25, 8.0, 1.25);
                b.move_to(3.5, 14.25)
                    .line_to(7.25, 10.75)
                    .line_to(10.0, 13.25)
                    .line_to(12.25, 11.25)
                    .line_to(16.5, 15.0);
            }
            Glyph::Spreadsheet => {
                // <rect x="3.25" y="3" width="13.5" height="14" rx="1.5">
                // M3.25 7.75h13.5M3.25 12.25h13.5M8.25 7.75v9.25
                b.rounded_rect(3.25, 3.0, 13.5, 14.0, 1.5);
                b.move_to(3.25, 7.75).line_to(16.75, 7.75);
                b.move_to(3.25, 12.25).line_to(16.75, 12.25);
                b.move_to(8.25, 7.75).line_to(8.25, 17.0);
            }
            Glyph::Search => {
                // <circle cx="8.75" cy="8.75" r="5.5">  M13 13l4 4
                b.circle(8.75, 8.75, 5.5);
                b.move_to(13.0, 13.0).line_to(17.0, 17.0);
            }
            Glyph::SortUp => {
                // M6 12l4-4 4 4
                b.move_to(6.0, 12.0).line_to(10.0, 8.0).line_to(14.0, 12.0);
            }
            Glyph::SortDown => {
                // M6 8l4 4 4-4
                b.move_to(6.0, 8.0).line_to(10.0, 12.0).line_to(14.0, 8.0);
            }
        }
        b.ops
    }
}

/// The control-point distance of a quarter-circle cubic, per unit radius.
const KAPPA: f64 = 0.552_284_749_830_793_4;

/// Builds a path, SVG style.
#[derive(Default)]
struct PathBuilder {
    ops: Vec<PathOp>,
    current: Option<Point>,
}

impl PathBuilder {
    fn move_to(&mut self, x: f64, y: f64) -> &mut Self {
        let p = Point { x, y };
        self.ops.push(PathOp::MoveTo(p));
        self.current = Some(p);
        self
    }

    fn line_to(&mut self, x: f64, y: f64) -> &mut Self {
        let p = Point { x, y };
        self.ops.push(PathOp::LineTo(p));
        self.current = Some(p);
        self
    }

    /// A quarter circle from the current point to (`x`, `y`) about the
    /// centre (`cx`, `cy`).
    fn arc_to(&mut self, x: f64, y: f64, cx: f64, cy: f64) -> &mut Self {
        let start = self.current.expect("an arc follows a point");
        let end = Point { x, y };
        // The tangent at the start points the way the end lies from the
        // centre, and at the end back the way the start lies.
        let c1 = Point {
            x: start.x + KAPPA * (end.x - cx),
            y: start.y + KAPPA * (end.y - cy),
        };
        let c2 = Point {
            x: end.x + KAPPA * (start.x - cx),
            y: end.y + KAPPA * (start.y - cy),
        };
        self.ops.push(PathOp::CubicTo(c1, c2, end));
        self.current = Some(end);
        self
    }

    fn close(&mut self) -> &mut Self {
        self.ops.push(PathOp::Close);
        self
    }

    /// An SVG `<rect>` with `rx`: clockwise from the top edge's left end.
    fn rounded_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64) -> &mut Self {
        let (right, bottom) = (x + w, y + h);
        self.move_to(x + r, y)
            .line_to(right - r, y)
            .arc_to(right, y + r, right - r, y + r)
            .line_to(right, bottom - r)
            .arc_to(right - r, bottom, right - r, bottom - r)
            .line_to(x + r, bottom)
            .arc_to(x, bottom - r, x + r, bottom - r)
            .line_to(x, y + r)
            .arc_to(x + r, y, x + r, y + r)
            .close()
    }

    /// An SVG `<circle>`: clockwise from its rightmost point.
    fn circle(&mut self, cx: f64, cy: f64, r: f64) -> &mut Self {
        self.move_to(cx + r, cy)
            .arc_to(cx, cy + r, cx, cy)
            .arc_to(cx - r, cy, cx, cy)
            .arc_to(cx, cy - r, cx, cy)
            .arc_to(cx + r, cy, cx, cy)
            .close()
    }
}

/// The path as polylines, one per subpath (a closed subpath ends at its
/// start), with curves split until each piece is within about `tolerance`
/// grid units of the curve.
pub fn flatten(ops: &[PathOp], tolerance: f64) -> Vec<Vec<Point>> {
    let mut lines: Vec<Vec<Point>> = Vec::new();
    for op in ops {
        match *op {
            PathOp::MoveTo(p) => lines.push(vec![p]),
            PathOp::LineTo(p) => {
                if let Some(line) = lines.last_mut() {
                    line.push(p);
                }
            }
            PathOp::CubicTo(c1, c2, end) => {
                let Some(line) = lines.last_mut() else {
                    continue;
                };
                let start = *line.last().expect("a subpath has a start");
                // The control polygon's length bounds the curve's; split it
                // so each chord's sagitta stays under the tolerance.
                let hull = dist(start, c1) + dist(c1, c2) + dist(c2, end);
                let steps =
                    ((hull / (8.0 * tolerance.max(1e-6)).sqrt()).ceil() as usize).clamp(1, 256);
                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    line.push(cubic_at(start, c1, c2, end, t));
                }
            }
            PathOp::Close => {
                if let Some(line) = lines.last_mut() {
                    let first = line[0];
                    line.push(first);
                }
            }
        }
    }
    lines
}

fn dist(a: Point, b: Point) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn cubic_at(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Point {
        x: a * p0.x + b * p1.x + c * p2.x + d * p3.x,
        y: a * p0.y + b * p1.y + c * p2.y + d * p3.y,
    }
}
