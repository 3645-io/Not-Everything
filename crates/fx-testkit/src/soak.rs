//! A seeded stream of random file operations for the soak and the fuzz
//! test, and the helpers that compare an index with ground truth.
//!
//! [`Planner`] draws [`Step`]s, each one or more [`Op`]s, from a seed, and
//! keeps its own model of the tree they build, so it never names a path
//! that doesn't exist or is taken, and [`Planner::listing`] says exactly
//! what the tree holds after every step. The same stream drives
//! `crates/fx-engine/examples/soak.rs` (the real engine over a real folder,
//! for hours) and `crates/fx-engine/tests/fuzz.rs` (the writer over a fake
//! file system, many seeds in seconds).
//!
//! # The stream
//!
//! Most steps are one operation, drawn by weight: create a file or a
//! folder, rewrite a file in place, replace a file with a new one (write a
//! temporary file, then rename it over the old one, as editors save),
//! rename a file or folder in its folder, change only the letter case of a
//! name, move a file or folder to another folder (never into itself), delete
//! a file or a whole folder, move a folder in from outside the watched
//! folder, or move a file or folder out of it (as to the Trash). The other
//! steps are composites: a burst of operations back to back, a deep chain of
//! new folders with files, a swap of two names in one folder (three
//! renames), and deleting a path and creating the other kind of entry at it.
//!
//! [`Limits`] caps the number of entries (a step that would pass the cap
//! deletes instead) and the depth of every path, so paths stay well inside
//! `PATH_MAX` on macOS.
//!
//! Every new name starts with a counter (`17-report.txt`), so no two
//! entries ever have names that differ only in letter case or Unicode
//! normalization, and no name starts with a dot. The rest is built from
//! small word and extension tables, in lower case, Title Case or with a
//! number; about one name in 24 is non-ASCII instead, from a table in both
//! NFC and NFD forms. Names are at most [`MAX_NAME_BASE`] bytes after the
//! counter.
//!
//! Randomness comes from SplitMix64 (public domain; Steele, Lea and Flood,
//! written here from its published description), so a seed gives the same
//! stream on every machine. Std only.
//!
//! # Applying it
//!
//! [`apply`] runs one operation under a real folder with std only. It only
//! ever touches what the planner made: creates are exclusive (which also
//! refuses a symlink in the way), a rename checks that its new name is free
//! first because `rename(2)` replaces files, a replace only goes over a
//! regular file, and deletes never follow symlinks. Folders moved in are
//! built in a separate folder outside the watched one first, and entries
//! moved out land there and are removed at once.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Dir,
    File,
}

/// What the planner's model holds: relative path (with `/`) to kind and
/// size in bytes (0 for folders).
pub type Listing = BTreeMap<String, (Kind, u64)>;

/// One entry of a folder moved in from outside: its path relative to that
/// folder (parents listed before their children), kind and, for a file,
/// length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewEntry {
    pub path: String,
    pub kind: Kind,
    pub len: usize,
}

/// One file operation. Paths are relative to the watched folder, with `/`
/// separators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Create an empty folder.
    Mkdir { path: String },
    /// Create a new file holding `len` bytes.
    Create { path: String, len: usize },
    /// Rewrite an existing file in place with `len` bytes (same file, new
    /// size and modification time).
    Write { path: String, len: usize },
    /// Write a new file `tmp` (in the same folder) with `len` bytes, then
    /// rename it over the existing file `path`.
    Replace {
        tmp: String,
        path: String,
        len: usize,
    },
    /// Rename or move a file or folder; `to` is free, or differs from
    /// `from` only in letter case.
    Rename { from: String, to: String, dir: bool },
    /// Delete a file, or a folder with everything in it.
    Delete { path: String, dir: bool },
    /// Build a folder outside the watched folder, then move it in at `to`.
    Import { to: String, entries: Vec<NewEntry> },
    /// Move a file or folder out of the watched folder (then remove it
    /// there).
    Export { path: String, dir: bool },
}

impl Op {
    /// The paths a file system watcher reports for this operation, as the
    /// macOS FSEvents source does with file-level events, each with whether
    /// a folder was renamed into or out of it (which that source turns into
    /// a subtree rescan, because FSEvents reports nothing for the entries
    /// below a moved folder). A folder delete reports only the folder: a
    /// watcher may report the entries inside it too, but need not (a move
    /// to the Trash doesn't).
    pub fn reported(&self) -> Vec<(&str, bool)> {
        match self {
            Op::Mkdir { path }
            | Op::Create { path, .. }
            | Op::Write { path, .. }
            | Op::Delete { path, .. } => vec![(path, false)],
            Op::Replace { tmp, path, .. } => vec![(tmp, false), (path, false)],
            Op::Rename { from, to, dir } => vec![(from, *dir), (to, *dir)],
            Op::Import { to, .. } => vec![(to, true)],
            Op::Export { path, dir } => vec![(path, *dir)],
        }
    }

    /// How many single operations this counts as in the soak's tally: one,
    /// plus one per entry built inside a folder moved in.
    pub fn weight(&self) -> u64 {
        match self {
            Op::Import { entries, .. } => 1 + entries.len() as u64,
            _ => 1,
        }
    }
}

/// What kind of step a [`Step`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StepKind {
    /// One operation.
    Single,
    /// Several random operations back to back.
    Burst,
    /// A chain of new nested folders, with a few files at each level.
    DeepTree,
    /// Two entries of one folder swap names (three renames).
    Swap,
    /// A path is deleted and created again as a file or a folder.
    Recreate,
}

/// One step of the stream: operations applied back to back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub kind: StepKind,
    pub ops: Vec<Op>,
}

/// The planner's caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Entries (files and folders, not counting the watched folder) alive
    /// at once; at least 2.
    pub max_entries: usize,
    /// Components of the deepest path; at least 2.
    pub max_depth: usize,
    /// The largest file written, in bytes.
    pub max_file_bytes: usize,
    /// The most operations in one burst; at least 2.
    pub max_burst: usize,
}

impl Limits {
    /// `max_entries` entries, 12 levels, files of up to 256 bytes, bursts
    /// of up to 32 operations. With names of at most 40 bytes, a path stays
    /// under 500 bytes below the watched folder.
    pub fn new(max_entries: usize) -> Limits {
        Limits {
            max_entries,
            max_depth: 12,
            max_file_bytes: 256,
            max_burst: 32,
        }
    }
}

/// The longest name base in bytes, before the counter prefix.
pub const MAX_NAME_BASE: usize = 32;

/// One name in this many is non-ASCII.
const NON_ASCII_ONE_IN: u64 = 24;

/// Non-ASCII name bases, in both Unicode normalization forms where they
/// differ, so the index meets decomposed names as well as composed ones.
const NON_ASCII: &[&str] = &[
    "caf\u{e9}.txt",
    "cafe\u{301}.txt",
    "\u{c5}ngstr\u{f6}m",
    "A\u{30a}ngstro\u{308}m",
    "na\u{ef}ve r\u{e9}sum\u{e9}.pdf",
    "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30d5}\u{30a1}\u{30a4}\u{30eb}",
    "\u{d55c}\u{ad6d}\u{c5b4} \u{bb38}\u{c11c}.hwp",
    "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}.doc",
    "\u{3b5}\u{3bb}\u{3bb}\u{3b7}\u{3bd}\u{3b9}\u{3ba}\u{3ac}",
    "photos \u{1f4c1}",
];

/// Words names are built from.
const WORDS: &[&str] = &[
    "report",
    "draft",
    "notes",
    "invoice",
    "photo",
    "backup",
    "project",
    "budget",
    "summary",
    "archive",
    "design",
    "export",
    "screen",
    "recording",
    "contract",
    "letter",
    "meeting",
    "plan",
    "release",
    "build",
    "cache",
    "config",
    "data",
    "index",
    "log",
    "sample",
    "test",
    "theme",
    "track",
    "video",
    "music",
    "scan",
    "receipt",
    "travel",
    "family",
    "work",
    "client",
    "source",
    "assets",
    "final",
];

/// File extensions; the empty one leaves a name without.
const EXTENSIONS: &[&str] = &[
    "txt", "pdf", "png", "jpg", "md", "rs", "json", "zip", "mov", "docx", "xlsx", "html", "csv",
    "", "tar.gz", "heic", "swift", "log",
];

/// SplitMix64: a seeded stream of 64-bit numbers.
#[derive(Clone, Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n` (`n` at least 1), by multiply and shift; the bias
    /// is below 2^-50 for the small ranges used here.
    fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next_u64()) * u128::from(n.max(1))) >> 64) as u64
    }

    fn index(&mut self, len: usize) -> usize {
        self.below(len as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.index(items.len())]
    }

    /// An item drawn with the weights of `table`.
    fn weighted<'a, T>(&mut self, table: &'a [(T, u32)]) -> &'a T {
        let total: u64 = table.iter().map(|(_, w)| u64::from(*w)).sum();
        let mut roll = self.below(total);
        for (item, w) in table {
            if roll < u64::from(*w) {
                return item;
            }
            roll -= u64::from(*w);
        }
        &table[table.len() - 1].0
    }
}

/// The model's ID of the watched folder.
const ROOT: u64 = 0;

#[derive(Clone, Debug)]
struct Node {
    parent: u64,
    name: String,
    kind: Kind,
    len: usize,
    children: Vec<u64>,
    /// Index in `Planner::dirs` or `Planner::files`.
    slot: usize,
}

/// The single operations, drawn by weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    CreateFile,
    Mkdir,
    Write,
    Replace,
    RenameFile,
    CaseRename,
    MoveFile,
    RenameDir,
    MoveDir,
    DeleteFile,
    DeleteDir,
    Import,
    Export,
}

const SINGLE: &[(Pick, u32)] = &[
    (Pick::CreateFile, 24),
    (Pick::Mkdir, 8),
    (Pick::Write, 10),
    (Pick::Replace, 6),
    (Pick::RenameFile, 7),
    (Pick::CaseRename, 2),
    (Pick::MoveFile, 8),
    (Pick::RenameDir, 3),
    (Pick::MoveDir, 4),
    (Pick::DeleteFile, 12),
    (Pick::DeleteDir, 3),
    (Pick::Import, 2),
    (Pick::Export, 2),
];

impl Pick {
    fn grows(self) -> bool {
        matches!(self, Pick::CreateFile | Pick::Mkdir | Pick::Import)
    }

    fn needs_file(self) -> bool {
        matches!(
            self,
            Pick::Write | Pick::Replace | Pick::RenameFile | Pick::MoveFile | Pick::DeleteFile
        )
    }

    fn needs_dir(self) -> bool {
        matches!(self, Pick::RenameDir | Pick::MoveDir | Pick::DeleteDir)
    }
}

/// The operation stream for one seed, with its model of the tree. See the
/// module documentation.
#[derive(Clone, Debug)]
pub struct Planner {
    limits: Limits,
    rng: Rng,
    nodes: HashMap<u64, Node>,
    /// Live folders, not counting the watched folder.
    dirs: Vec<u64>,
    files: Vec<u64>,
    next_node: u64,
    next_name: u64,
}

impl Planner {
    /// A stream for `seed` over an empty folder.
    ///
    /// # Panics
    ///
    /// Panics if a limit is below its documented minimum.
    pub fn new(seed: u64, limits: Limits) -> Planner {
        assert!(limits.max_entries >= 2, "max_entries must be at least 2");
        assert!(limits.max_depth >= 2, "max_depth must be at least 2");
        assert!(limits.max_burst >= 2, "max_burst must be at least 2");
        let mut nodes = HashMap::new();
        nodes.insert(
            ROOT,
            Node {
                parent: ROOT,
                name: String::new(),
                kind: Kind::Dir,
                len: 0,
                children: Vec::new(),
                slot: 0,
            },
        );
        Planner {
            limits,
            rng: Rng::new(seed),
            nodes,
            dirs: Vec::new(),
            files: Vec::new(),
            next_node: 1,
            next_name: 0,
        }
    }

    /// The planner's limits.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Files and folders alive after the steps so far.
    pub fn live_entries(&self) -> usize {
        self.dirs.len() + self.files.len()
    }

    /// Every entry the tree holds after the steps so far.
    pub fn listing(&self) -> Listing {
        let mut out = Listing::new();
        for (&id, node) in &self.nodes {
            if id == ROOT {
                continue;
            }
            let size = if node.kind == Kind::File {
                node.len as u64
            } else {
                0
            };
            out.insert(self.path(id), (node.kind, size));
        }
        out
    }

    /// The next step.
    pub fn next_step(&mut self) -> Step {
        let roll = self.rng.below(100);
        let composite = match roll {
            0..4 => self.burst().map(|ops| (StepKind::Burst, ops)),
            4..7 => self.deep_tree().map(|ops| (StepKind::DeepTree, ops)),
            7..9 => self.swap().map(|ops| (StepKind::Swap, ops)),
            9..12 => self.recreate().map(|ops| (StepKind::Recreate, ops)),
            _ => None,
        };
        let (kind, ops) = composite.unwrap_or_else(|| (StepKind::Single, vec![self.single()]));
        Step { kind, ops }
    }

    // ----------------------------------------------------------- the model

    fn node(&self, id: u64) -> &Node {
        &self.nodes[&id]
    }

    fn path(&self, id: u64) -> String {
        let mut parts = Vec::new();
        let mut cur = id;
        while cur != ROOT {
            let n = self.node(cur);
            parts.push(n.name.as_str());
            cur = n.parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// The path of a new entry `name` in folder `parent`.
    fn child_path(&self, parent: u64, name: &str) -> String {
        if parent == ROOT {
            name.to_string()
        } else {
            format!("{}/{name}", self.path(parent))
        }
    }

    /// Components of the entry's path; 0 for the watched folder.
    fn depth(&self, id: u64) -> usize {
        let mut d = 0;
        let mut cur = id;
        while cur != ROOT {
            d += 1;
            cur = self.node(cur).parent;
        }
        d
    }

    /// Levels the entry spans: 1 for a file or an empty folder.
    fn height(&self, id: u64) -> usize {
        let n = self.node(id);
        1 + n
            .children
            .iter()
            .map(|&c| self.height(c))
            .max()
            .unwrap_or(0)
    }

    /// Whether `id` is `ancestor` or lies below it.
    fn is_within(&self, id: u64, ancestor: u64) -> bool {
        let mut cur = id;
        loop {
            if cur == ancestor {
                return true;
            }
            if cur == ROOT {
                return false;
            }
            cur = self.node(cur).parent;
        }
    }

    fn add(&mut self, parent: u64, name: String, kind: Kind, len: usize) -> u64 {
        let id = self.next_node;
        self.next_node += 1;
        let slot = match kind {
            Kind::Dir => {
                self.dirs.push(id);
                self.dirs.len() - 1
            }
            Kind::File => {
                self.files.push(id);
                self.files.len() - 1
            }
        };
        self.nodes.insert(
            id,
            Node {
                parent,
                name,
                kind,
                len,
                children: Vec::new(),
                slot,
            },
        );
        self.nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children
            .push(id);
        id
    }

    fn detach(&mut self, id: u64) {
        let parent = self.node(id).parent;
        let kids = &mut self.nodes.get_mut(&parent).expect("parent exists").children;
        let at = kids.iter().position(|&c| c == id).expect("listed child");
        kids.swap_remove(at);
    }

    fn unslot(&mut self, id: u64) {
        let (kind, slot) = {
            let n = self.node(id);
            (n.kind, n.slot)
        };
        let list = match kind {
            Kind::Dir => &mut self.dirs,
            Kind::File => &mut self.files,
        };
        list.swap_remove(slot);
        if let Some(&moved) = list.get(slot) {
            self.nodes.get_mut(&moved).expect("live node").slot = slot;
        }
    }

    fn remove_subtree(&mut self, id: u64) {
        self.detach(id);
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            stack.extend(self.node(cur).children.iter().copied());
            self.unslot(cur);
            self.nodes.remove(&cur);
        }
    }

    fn reparent(&mut self, id: u64, parent: u64, name: String) {
        self.detach(id);
        let n = self.nodes.get_mut(&id).expect("live node");
        n.parent = parent;
        n.name = name;
        self.nodes
            .get_mut(&parent)
            .expect("parent exists")
            .children
            .push(id);
    }

    // ------------------------------------------------------------ drawing

    fn new_name(&mut self, kind: Kind) -> String {
        let id = self.next_name;
        self.next_name += 1;
        let base = if self.rng.below(NON_ASCII_ONE_IN) == 0 {
            (*self.rng.pick(NON_ASCII)).to_string()
        } else {
            let first = *self.rng.pick(WORDS);
            let mut base = match self.rng.below(4) {
                0 => first.to_string(),
                1 => format!("{first}_{}", self.rng.pick(WORDS)),
                2 => format!(
                    "{}{} {}",
                    first[..1].to_ascii_uppercase(),
                    &first[1..],
                    self.rng.pick(WORDS)
                ),
                _ => format!("{first}-{}", self.rng.below(10_000)),
            };
            let ext = *self.rng.pick(EXTENSIONS);
            if kind == Kind::File && !ext.is_empty() {
                base.push('.');
                base.push_str(ext);
            }
            base
        };
        let mut cut = base.len().min(MAX_NAME_BASE);
        while !base.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{id}-{}", &base[..cut])
    }

    fn len(&mut self) -> usize {
        if self.rng.below(4) == 0 {
            0
        } else {
            1 + self.rng.index(self.limits.max_file_bytes.max(1))
        }
    }

    fn random_file(&mut self) -> u64 {
        self.files[self.rng.index(self.files.len())]
    }

    fn random_dir(&mut self) -> u64 {
        self.dirs[self.rng.index(self.dirs.len())]
    }

    fn random_entry(&mut self) -> u64 {
        let i = self.rng.index(self.live_entries());
        if i < self.files.len() {
            self.files[i]
        } else {
            self.dirs[i - self.files.len()]
        }
    }

    /// A folder (the watched folder one time in eight) whose depth leaves
    /// room for `extra` more levels below it.
    fn pick_parent(&mut self, extra: usize) -> u64 {
        for _ in 0..8 {
            let p = if self.dirs.is_empty() || self.rng.below(8) == 0 {
                ROOT
            } else {
                self.random_dir()
            };
            if self.depth(p) + extra <= self.limits.max_depth {
                return p;
            }
        }
        ROOT
    }

    fn headroom(&self) -> usize {
        self.limits.max_entries.saturating_sub(self.live_entries())
    }

    fn single(&mut self) -> Op {
        let full = self.headroom() == 0;
        let mut pick = *self.rng.weighted(SINGLE);
        if full && pick.grows() {
            pick = if self.rng.below(4) == 0 {
                Pick::DeleteDir
            } else {
                Pick::DeleteFile
            };
        }
        if pick.needs_file() && self.files.is_empty() {
            pick = if full {
                Pick::DeleteDir
            } else {
                Pick::CreateFile
            };
        }
        if pick.needs_dir() && self.dirs.is_empty() {
            pick = if full { Pick::DeleteFile } else { Pick::Mkdir };
        }
        if matches!(pick, Pick::CaseRename | Pick::Export) && self.live_entries() == 0 {
            pick = Pick::CreateFile;
        }
        match pick {
            Pick::CreateFile => self.create(Kind::File),
            Pick::Mkdir => self.create(Kind::Dir),
            Pick::Write => {
                let f = self.random_file();
                let len = self.len();
                self.nodes.get_mut(&f).expect("live file").len = len;
                Op::Write {
                    path: self.path(f),
                    len,
                }
            }
            Pick::Replace => {
                let f = self.random_file();
                let len = self.len();
                let tmp_name = self.new_name(Kind::File);
                let tmp = self.child_path(self.node(f).parent, &tmp_name);
                self.nodes.get_mut(&f).expect("live file").len = len;
                Op::Replace {
                    tmp,
                    path: self.path(f),
                    len,
                }
            }
            Pick::RenameFile => {
                let f = self.random_file();
                self.rename_in_place(f)
            }
            Pick::RenameDir => {
                let d = self.random_dir();
                self.rename_in_place(d)
            }
            Pick::CaseRename => {
                let e = self.random_entry();
                let name = &self.node(e).name;
                let flipped = flip_case(name);
                if flipped == *name {
                    return self.rename_in_place(e);
                }
                let from = self.path(e);
                let parent = self.node(e).parent;
                self.reparent(e, parent, flipped);
                Op::Rename {
                    from,
                    to: self.path(e),
                    dir: self.node(e).kind == Kind::Dir,
                }
            }
            Pick::MoveFile => {
                let f = self.random_file();
                let target = self.pick_parent(1);
                self.move_to(f, target)
            }
            Pick::MoveDir => {
                let d = self.random_dir();
                let h = self.height(d);
                for _ in 0..8 {
                    let target = self.pick_parent(h);
                    if !self.is_within(target, d) {
                        return self.move_to(d, target);
                    }
                }
                self.rename_in_place(d)
            }
            Pick::DeleteFile => {
                let f = self.random_file();
                let path = self.path(f);
                self.remove_subtree(f);
                Op::Delete { path, dir: false }
            }
            Pick::DeleteDir => {
                let d = self.random_dir();
                let path = self.path(d);
                self.remove_subtree(d);
                Op::Delete { path, dir: true }
            }
            Pick::Import => self.import(),
            Pick::Export => {
                let e = self.random_entry();
                let path = self.path(e);
                let dir = self.node(e).kind == Kind::Dir;
                self.remove_subtree(e);
                Op::Export { path, dir }
            }
        }
    }

    fn create(&mut self, kind: Kind) -> Op {
        let parent = self.pick_parent(1);
        let name = self.new_name(kind);
        let path = self.child_path(parent, &name);
        match kind {
            Kind::Dir => {
                self.add(parent, name, Kind::Dir, 0);
                Op::Mkdir { path }
            }
            Kind::File => {
                let len = self.len();
                self.add(parent, name, Kind::File, len);
                Op::Create { path, len }
            }
        }
    }

    fn rename_in_place(&mut self, id: u64) -> Op {
        let from = self.path(id);
        let (parent, kind) = {
            let n = self.node(id);
            (n.parent, n.kind)
        };
        let name = self.new_name(kind);
        self.reparent(id, parent, name);
        Op::Rename {
            from,
            to: self.path(id),
            dir: kind == Kind::Dir,
        }
    }

    /// Moves `id` into `target`, keeping its name half the time (names are
    /// unique, so it is always free there).
    fn move_to(&mut self, id: u64, target: u64) -> Op {
        let from = self.path(id);
        let kind = self.node(id).kind;
        let name = if self.rng.below(2) == 0 {
            self.node(id).name.clone()
        } else {
            self.new_name(kind)
        };
        if target == self.node(id).parent && name == self.node(id).name {
            return self.rename_in_place(id);
        }
        self.reparent(id, target, name);
        Op::Rename {
            from,
            to: self.path(id),
            dir: kind == Kind::Dir,
        }
    }

    fn import(&mut self) -> Op {
        // Up to 5 files at the top, then a chain of up to 3 folders with up
        // to 3 files each, within the entry cap.
        let mut budget = self.headroom().saturating_sub(1);
        let chain = self.rng.index(4).min(self.limits.max_depth - 1);
        let target = self.pick_parent(1 + chain.max(1));
        let room = self.limits.max_depth - self.depth(target) - 1;
        let chain = chain.min(room.saturating_sub(1));
        let name = self.new_name(Kind::Dir);
        let to = self.child_path(target, &name);
        let top = self.add(target, name, Kind::Dir, 0);
        let mut entries = Vec::new();
        let mut folder = top;
        let mut rel = String::new();
        for level in 0..=chain {
            if level > 0 {
                if budget == 0 {
                    break;
                }
                let name = self.new_name(Kind::Dir);
                rel = if rel.is_empty() {
                    name.clone()
                } else {
                    format!("{rel}/{name}")
                };
                folder = self.add(folder, name, Kind::Dir, 0);
                entries.push(NewEntry {
                    path: rel.clone(),
                    kind: Kind::Dir,
                    len: 0,
                });
                budget -= 1;
            }
            let files = self.rng.index(if level == 0 { 6 } else { 4 });
            for _ in 0..files.min(budget) {
                let name = self.new_name(Kind::File);
                let len = self.len();
                let path = if rel.is_empty() {
                    name.clone()
                } else {
                    format!("{rel}/{name}")
                };
                self.add(folder, name, Kind::File, len);
                entries.push(NewEntry {
                    path,
                    kind: Kind::File,
                    len,
                });
                budget -= 1;
            }
        }
        Op::Import { to, entries }
    }

    fn burst(&mut self) -> Option<Vec<Op>> {
        let n = 2 + self.rng.index(self.limits.max_burst - 1);
        Some((0..n).map(|_| self.single()).collect())
    }

    fn deep_tree(&mut self) -> Option<Vec<Op>> {
        let base = self.pick_parent(3);
        let room = self.limits.max_depth - self.depth(base);
        // Folders at depths base+1 ..= base+n, files one level further down.
        let want = 3 + self.rng.index(10);
        let n = want.min(room.saturating_sub(1)).min(self.headroom());
        if n < 2 {
            return None;
        }
        let mut budget = self.headroom() - n;
        let mut ops = Vec::new();
        let mut folder = base;
        for _ in 0..n {
            let name = self.new_name(Kind::Dir);
            let path = self.child_path(folder, &name);
            folder = self.add(folder, name, Kind::Dir, 0);
            ops.push(Op::Mkdir { path });
            for _ in 0..self.rng.index(3).min(budget) {
                let name = self.new_name(Kind::File);
                let len = self.len();
                let path = self.child_path(folder, &name);
                self.add(folder, name, Kind::File, len);
                ops.push(Op::Create { path, len });
                budget -= 1;
            }
        }
        Some(ops)
    }

    fn swap(&mut self) -> Option<Vec<Op>> {
        for _ in 0..8 {
            let folder = self.pick_parent(1);
            let count = self.node(folder).children.len();
            if count < 2 {
                continue;
            }
            let i = self.rng.index(count);
            let mut j = self.rng.index(count - 1);
            if j >= i {
                j += 1;
            }
            let kids = &self.node(folder).children;
            let (a, b) = (kids[i], kids[j]);
            let (pa, pb) = (self.path(a), self.path(b));
            let (na, nb) = (self.node(a).name.clone(), self.node(b).name.clone());
            let (da, db) = (
                self.node(a).kind == Kind::Dir,
                self.node(b).kind == Kind::Dir,
            );
            let tmp_name = self.new_name(self.node(a).kind);
            let tmp = self.child_path(folder, &tmp_name);
            self.reparent(a, folder, nb);
            self.reparent(b, folder, na);
            return Some(vec![
                Op::Rename {
                    from: pa.clone(),
                    to: tmp.clone(),
                    dir: da,
                },
                Op::Rename {
                    from: pb.clone(),
                    to: pa,
                    dir: db,
                },
                Op::Rename {
                    from: tmp,
                    to: pb,
                    dir: da,
                },
            ]);
        }
        None
    }

    fn recreate(&mut self) -> Option<Vec<Op>> {
        if self.live_entries() == 0 {
            return None;
        }
        let e = self.random_entry();
        let path = self.path(e);
        let (parent, name, dir) = {
            let n = self.node(e);
            (n.parent, n.name.clone(), n.kind == Kind::Dir)
        };
        self.remove_subtree(e);
        let second = if self.rng.below(2) == 0 {
            self.add(parent, name, Kind::Dir, 0);
            Op::Mkdir { path: path.clone() }
        } else {
            let len = self.len();
            self.add(parent, name, Kind::File, len);
            Op::Create {
                path: path.clone(),
                len,
            }
        };
        Some(vec![Op::Delete { path, dir }, second])
    }
}

/// The name with its ASCII letters in the other case: upper case when it
/// has a lower-case letter, else lower case.
fn flip_case(name: &str) -> String {
    if name.bytes().any(|b| b.is_ascii_lowercase()) {
        name.to_ascii_uppercase()
    } else {
        name.to_ascii_lowercase()
    }
}

// ------------------------------------------------------------------ apply

/// What a created or rewritten file holds, repeated up to its length.
const CONTENT: &[u8] = b"not-everything soak\n";

fn content(len: usize) -> Vec<u8> {
    CONTENT.iter().copied().cycle().take(len).collect()
}

fn create_file(path: &Path, len: usize) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(&content(len))
}

fn exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn refuse(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, what.to_string())
}

/// `path` must be a regular file (not a folder or a symlink).
fn require_file(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}

/// `to` must be free, unless it names `from` itself in another letter case
/// (a case-insensitive volume finds the same entry under both names).
fn require_free(from: Option<&Path>, to: &Path) -> io::Result<()> {
    match fs::symlink_metadata(to) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(found) => {
            let same = from.is_some_and(|from| same_entry(from, &found));
            if same {
                Ok(())
            } else {
                Err(refuse("the new name is already taken"))
            }
        }
    }
}

/// Whether `path` is the entry `found` describes.
#[cfg(unix)]
fn same_entry(path: &Path, found: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).is_ok_and(|m| m.ino() == found.ino() && m.dev() == found.dev())
}

/// Without inode numbers a taken name is never treated as the same entry.
#[cfg(not(unix))]
fn same_entry(_path: &Path, _found: &fs::Metadata) -> bool {
    false
}

/// The last component of a relative path.
fn base_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
}

/// Applies one operation under `root`. `outside` is a folder outside
/// `root` (not watched) where folders moved in are built and entries moved
/// out are removed. See the module documentation for the guards.
///
/// Errors: any I/O error, or a refusal when the operation would replace
/// something the planner didn't make.
pub fn apply(root: &Path, outside: &Path, op: &Op) -> io::Result<()> {
    match op {
        Op::Mkdir { path } => fs::create_dir(root.join(path)),
        Op::Create { path, len } => create_file(&root.join(path), *len),
        Op::Write { path, len } => {
            let path = root.join(path);
            require_file(&path)?;
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)?
                .write_all(&content(*len))
        }
        Op::Replace { tmp, path, len } => {
            let path = root.join(path);
            require_file(&path)?;
            let tmp = root.join(tmp);
            create_file(&tmp, *len)?;
            // rename(2) replaces the regular file checked above.
            fs::rename(&tmp, &path)
        }
        Op::Rename { from, to, .. } => {
            let (from, to) = (root.join(from), root.join(to));
            require_free(Some(&from), &to)?;
            fs::rename(&from, &to)
        }
        Op::Delete { path, dir } => {
            let path = root.join(path);
            if *dir {
                // Never follows symlinks; removes only the folder's own tree.
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            }
        }
        Op::Import { to, entries } => {
            let target = root.join(to);
            require_free(None, &target)?;
            let staged: PathBuf = outside.join(base_name(to));
            fs::create_dir(&staged)?;
            for e in entries {
                let p = staged.join(&e.path);
                match e.kind {
                    Kind::Dir => fs::create_dir(&p)?,
                    Kind::File => create_file(&p, e.len)?,
                }
            }
            require_free(None, &target)?;
            fs::rename(&staged, &target)
        }
        Op::Export { path, dir } => {
            let source = root.join(path);
            let staged = outside.join(base_name(path));
            if exists(&staged)? {
                return Err(refuse("the folder outside already holds that name"));
            }
            fs::rename(&source, &staged)?;
            if *dir {
                fs::remove_dir_all(&staged)
            } else {
                fs::remove_file(&staged)
            }
        }
    }
}

// ------------------------------------------------------------ comparison

/// The paths where two listings differ.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// In the expected listing only.
    pub missing: Vec<String>,
    /// In the actual listing only.
    pub extra: Vec<String>,
    /// In both, with different values.
    pub changed: Vec<String>,
}

impl Diff {
    /// The number of differing paths.
    pub fn count(&self) -> usize {
        self.missing.len() + self.extra.len() + self.changed.len()
    }

    /// Whether the listings are equal.
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }
}

/// Compares `got` (for example the index) with `want` (ground truth).
pub fn diff<V: PartialEq>(want: &BTreeMap<String, V>, got: &BTreeMap<String, V>) -> Diff {
    let mut d = Diff::default();
    for (k, v) in want {
        match got.get(k) {
            None => d.missing.push(k.clone()),
            Some(g) if g != v => d.changed.push(k.clone()),
            Some(_) => {}
        }
    }
    d.extra = got
        .keys()
        .filter(|k| !want.contains_key(*k))
        .cloned()
        .collect();
    d
}

/// Parses a duration given as whole seconds (`90`, `90s`), minutes (`20m`)
/// or hours (`24h`).
pub fn parse_duration(text: &str) -> Option<Duration> {
    let (digits, unit) = match text.char_indices().last()? {
        (i, 's') => (&text[..i], 1),
        (i, 'm') => (&text[..i], 60),
        (i, 'h') => (&text[..i], 3_600),
        _ => (text, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    Some(Duration::from_secs(n.checked_mul(unit)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_case_turns_letters_only() {
        assert_eq!(flip_case("7-report.txt"), "7-REPORT.TXT");
        assert_eq!(flip_case("7-REPORT.TXT"), "7-report.txt");
        assert_eq!(flip_case("7-\u{65e5}"), "7-\u{65e5}");
    }

    #[test]
    fn names_are_cut_at_a_character_boundary() {
        let mut p = Planner::new(3, Limits::new(10));
        for _ in 0..2_000 {
            let name = p.new_name(Kind::File);
            let (prefix, base) = name.split_once('-').unwrap();
            assert!(prefix.bytes().all(|b| b.is_ascii_digit()));
            assert!(base.len() <= MAX_NAME_BASE, "{name}");
            assert!(!base.is_empty());
        }
    }
}
