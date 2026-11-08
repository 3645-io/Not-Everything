//! Full-path matching without building every full path.
//!
//! A term that matches paths (`path:`, a `/` in the term, or the global
//! match-path toggle) is tested against each entry's full path: its
//! folders' names and its own joined with `/`, from a root down. Paths are
//! never stored, and building one per entry (copying the parent's path,
//! appending the name, folding the whole string) cost about 280 ms p95
//! over 2M entries. Two things make it cheap instead:
//!
//! # Plain path literals: a folder memo
//!
//! A plain substring term (no `ww:`, `start-with:`, `end-with:` or `wfn:`)
//! with a non-empty folded needle `N` of `n` bytes matches the folded path
//! of an entry `E` with parent folder `P` exactly when
//!
//! - `N` occurs in the folded path of `P` (an occurrence wholly inside the
//!   parent's path), or
//! - `N` occurs in the *window* `tail(P) + sep(P) + name(E)`, where `tail`
//!   is the last `n - 1` bytes (or more) of the folded path of `P` and
//!   `sep` the separator the path join puts between them (an occurrence
//!   that ends in `E`'s name starts at most `n - 1` bytes before it).
//!
//! The first condition is the same question asked of `P`, so the answer
//! propagates down the tree: [`PathMemo`] keeps, per search and per term,
//! one byte per entry with the answer (and the facts the separator rule
//! needs), filled lazily from the roots down. Each folder is decided once,
//! by whichever thread reaches it first, and every entry then costs one
//! memo lookup plus a short test of its own name:
//!
//! - When `N` has no `/`, the window test is a search of the name alone:
//!   an occurrence cannot span the separator, and the tail of a path whose
//!   join adds no separator is empty or ends with `/`.
//! - When `N` has a `/` and the name has none (every name but a root's),
//!   an occurrence that ends in the name has its last `/` at the join, so
//!   with `N = X/Z` (`/` the last slash) it is there exactly when the
//!   folder's path followed by the separator ends with `X/` and the name
//!   starts with `Z`. The first half is a fact about the folder alone, so
//!   the memo keeps it too, computed once per folder from the end of its
//!   path (rebuilt from the nearest folders' folded names, usually one or
//!   two). Per entry that leaves a prefix compare.
//! - Otherwise the window is built and searched.
//!
//! Folding distributes over the join: case is folded per `char`, accents
//! and apostrophes go per `char` too, and `/` is a starter that never
//! composes in NFC, so `fold(join(a, b)) == join(fold(a), fold(b))` with
//! the separator chosen from the unfolded path, as
//! [`Candidate::full_path`](crate::eval::Candidate::full_path) does.
//!
//! # Other path terms: shared folder prefixes
//!
//! Wildcards, regexes, the constrained literals and the `parent:` and
//! `depth:` functions still need the whole string. The per-thread
//! [`PathCache`] keeps each folder's full path, and its folded path per
//! folding, behind a reference count, so a child's path is its folder's
//! cached prefix plus its own (folded) name: one join per entry and no
//! refolding of the prefix.

use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

use memchr::memmem::Finder;

use crate::fold::{FoldOptions, fold_str};
use crate::matcher::SearchView;

/// Folders kept per thread in a [`PathCache`] before it is cleared.
const PATH_CACHE_LIMIT: usize = 1 << 16;

// Memo state bits.
/// The state has been computed.
const KNOWN: u8 = 1;
/// The needle occurs in the entry's folded full path.
const HIT: u8 = 2;
/// The entry's full path is empty (every name on its chain is empty).
const EMPTY: u8 = 4;
/// The entry's full path ends with `/`.
const SLASH_END: u8 = 8;
/// [`END_HIT`] has been computed.
const END_KNOWN: u8 = 16;
/// For a needle `X/Z` (`/` its last slash): the entry's folded full path,
/// followed by the separator a child's name would get, ends with `X/`.
const END_HIT: u8 = 32;

/// A plain path literal, as [`PathMemo`] needs it: the folded needle, its
/// finder and its folding.
#[derive(Debug)]
pub(crate) struct PathNeedle<'q> {
    pub needle: &'q str,
    pub finder: &'q Finder<'static>,
    pub fold: FoldOptions,
}

impl PathNeedle<'_> {
    /// The byte offset of the needle's last `/`, if it has one.
    fn last_slash(&self) -> Option<usize> {
        memchr::memrchr(b'/', self.needle.as_bytes())
    }
}

/// Per search and per plain path literal: one state byte per entry (see
/// the [module docs](self)), shared by the search's threads.
#[derive(Debug)]
pub(crate) struct PathMemo {
    states: Vec<AtomicU8>,
}

impl PathMemo {
    /// An empty memo for a view of `len` entry slots.
    pub fn new(len: usize) -> Self {
        PathMemo {
            states: std::iter::repeat_with(|| AtomicU8::new(0))
                .take(len)
                .collect(),
        }
    }

    fn load(&self, id: u32) -> u8 {
        // Acquire pairs with the Release store, so a thread that sees an
        // entry's state also sees the states of its folders.
        self.states[id as usize].load(Ordering::Acquire)
    }

    fn store(&self, id: u32, state: u8) {
        self.states[id as usize].store(state, Ordering::Release);
    }

    /// Whether the needle occurs in the folded full path of entry `id`.
    pub fn is_hit<V: SearchView + ?Sized>(
        &self,
        view: &V,
        needle: &PathNeedle<'_>,
        id: u32,
        scratch: &mut Scratch,
    ) -> bool {
        self.state(view, needle, id, scratch) & HIT != 0
    }

    /// The state of entry `id`, computing it (and its folders' states) on
    /// first use.
    fn state<V: SearchView + ?Sized>(
        &self,
        view: &V,
        needle: &PathNeedle<'_>,
        id: u32,
        scratch: &mut Scratch,
    ) -> u8 {
        let known = self.load(id);
        if known & KNOWN != 0 {
            return known;
        }
        // Climb to the nearest folder with a known state, or a root.
        let chain = &mut scratch.chain;
        chain.clear();
        let mut top_parent = None;
        let mut cur = id;
        loop {
            chain.push(cur);
            let Some(p) = view.parent(cur) else {
                break;
            };
            let state = self.load(p);
            if state & KNOWN != 0 {
                top_parent = Some((p, state));
                break;
            }
            if chain.len() > view.len() {
                // A parent cycle; no valid view has one. Treat it as a root.
                break;
            }
            cur = p;
        }
        // Decide each one from the top down.
        let mut parent = top_parent;
        let mut state = 0;
        for k in (0..scratch.chain.len()).rev() {
            let node = scratch.chain[k];
            state = self.compute(view, needle, node, parent, scratch);
            self.store(node, state);
            parent = Some((node, state));
        }
        state
    }

    /// The state of `node` given its parent's (`None` for a root).
    fn compute<V: SearchView + ?Sized>(
        &self,
        view: &V,
        needle: &PathNeedle<'_>,
        node: u32,
        parent: Option<(u32, u8)>,
        scratch: &mut Scratch,
    ) -> u8 {
        let raw = view.name(node);
        let name = folded_name(view, node, needle.fold);
        let Some((p, ps)) = parent else {
            // A root's full path is its name.
            let mut state = KNOWN;
            if raw.is_empty() {
                state |= EMPTY;
            }
            if raw.ends_with('/') {
                state |= SLASH_END;
            }
            if needle.finder.find(name.as_bytes()).is_some() {
                state |= HIT;
            }
            return state;
        };
        let mut state = KNOWN;
        let parent_empty = ps & EMPTY != 0;
        if parent_empty && raw.is_empty() {
            state |= EMPTY;
        }
        // An empty name adds a `/` to a non-empty parent path (or keeps its
        // final one); any other name ends the path.
        let slash_end = if raw.is_empty() {
            !parent_empty
        } else {
            raw.ends_with('/')
        };
        if slash_end {
            state |= SLASH_END;
        }
        let bytes = name.as_bytes();
        let hit = ps & HIT != 0
            || match needle.last_slash() {
                None => needle.finder.find(bytes).is_some(),
                Some(slash) if memchr::memchr(b'/', bytes).is_none() => {
                    let ps = self.ensure_end(view, needle, p, ps, scratch);
                    ps & END_HIT != 0 && bytes.starts_with(&needle.needle.as_bytes()[slash + 1..])
                }
                Some(_) => {
                    scratch.window.clear();
                    let need = needle.needle.len().saturating_sub(1);
                    self.push_tail(view, needle, p, need, scratch);
                    if has_separator(ps) {
                        scratch.window.push(b'/');
                    }
                    scratch.window.extend_from_slice(bytes);
                    needle.finder.find(&scratch.window).is_some()
                }
            };
        if hit {
            state |= HIT;
        }
        state
    }

    /// Folder `id`'s state (`state`, known) with [`END_HIT`] computed: for
    /// the needle `X/Z`, whether its folded path followed by the separator
    /// a child gets ends with `X/`. Computed once per folder from the end
    /// of its path.
    fn ensure_end<V: SearchView + ?Sized>(
        &self,
        view: &V,
        needle: &PathNeedle<'_>,
        id: u32,
        state: u8,
        scratch: &mut Scratch,
    ) -> u8 {
        if state & END_KNOWN != 0 {
            return state;
        }
        let Some(slash) = needle.last_slash() else {
            return state;
        };
        // With a separator the path must end with `X`; without one (the
        // path ends with `/`, or is empty) with `X/`.
        let tail = if has_separator(state) {
            &needle.needle.as_bytes()[..slash]
        } else {
            &needle.needle.as_bytes()[..=slash]
        };
        scratch.window.clear();
        if state & EMPTY == 0 {
            self.push_tail(view, needle, id, tail.len(), scratch);
        }
        let mut bits = END_KNOWN;
        if scratch.window.ends_with(tail) {
            bits |= END_HIT;
        }
        // States only gain bits, so racing threads agree.
        self.states[id as usize].fetch_or(bits, Ordering::AcqRel) | bits
    }

    /// Appends to the scratch window the end of folder `id`'s folded full
    /// path: at least its last `need` bytes, or all of it. Every folder
    /// above `id` has a known state.
    fn push_tail<V: SearchView + ?Sized>(
        &self,
        view: &V,
        needle: &PathNeedle<'_>,
        id: u32,
        need: usize,
        scratch: &mut Scratch,
    ) {
        let up = &mut scratch.up;
        up.clear();
        up.push(id);
        let mut have = folded_name(view, id, needle.fold).len();
        let mut cur = id;
        while have < need && up.len() <= view.len() {
            let Some(p) = view.parent(cur) else {
                break;
            };
            let ps = self.load(p);
            have += folded_name(view, p, needle.fold).len() + usize::from(has_separator(ps));
            up.push(p);
            cur = p;
        }
        for k in (0..scratch.up.len()).rev() {
            let node = scratch.up[k];
            scratch
                .window
                .extend_from_slice(folded_name(view, node, needle.fold).as_bytes());
            if k > 0 && has_separator(self.load(node)) {
                scratch.window.push(b'/');
            }
        }
    }
}

/// Whether joining a name onto a path with state `state` puts a `/`
/// between them: the path is not empty and does not end with one.
fn has_separator(state: u8) -> bool {
    state & (EMPTY | SLASH_END) == 0
}

/// Entry `id`'s name folded under `fold`, from the view's folded copy when
/// it holds that form.
pub(crate) fn folded_name<V: SearchView + ?Sized>(
    view: &V,
    id: u32,
    fold: FoldOptions,
) -> Cow<'_, str> {
    match view.folded(id, fold) {
        Some(f) => Cow::Borrowed(f),
        None => fold_str(view.name(id), fold),
    }
}

/// Per-thread scratch space for [`PathMemo`].
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    chain: Vec<u32>,
    up: Vec<u32>,
    window: Vec<u8>,
}

/// Joins a parent path and a name as
/// [`Candidate::full_path`](crate::eval::Candidate::full_path) does.
pub(crate) fn join(parent_path: &str, name: &str) -> String {
    let mut path = String::with_capacity(parent_path.len() + 1 + name.len());
    join_into(&mut path, parent_path, parent_path, name);
    path
}

/// Appends `parent_path` (or its folded form `prefix`) and `name` to
/// `out`, with the separator the join of the unfolded `parent_path` puts
/// between them.
fn join_into(out: &mut String, parent_path: &str, prefix: &str, name: &str) {
    out.push_str(prefix);
    if !parent_path.is_empty() && !parent_path.ends_with('/') {
        out.push('/');
    }
    out.push_str(name);
}

/// Full paths of folders for one thread during one search: unfolded, and
/// folded per folding, each shared by the folder's children.
#[derive(Debug, Default)]
pub(crate) struct PathCache {
    raw: HashMap<u32, Rc<str>>,
    folded: HashMap<(u32, FoldOptions), Rc<str>>,
    /// Scratch space for the folder memos.
    pub scratch: Scratch,
}

impl PathCache {
    /// Clears the cache when it holds more than its limit of folders.
    pub fn trim(&mut self) {
        if self.raw.len() + self.folded.len() > PATH_CACHE_LIMIT {
            self.raw.clear();
            self.folded.clear();
        }
    }

    /// The full path of folder `id`, built from its name and its folders'
    /// paths on first use.
    pub fn raw<V: SearchView + ?Sized>(&mut self, view: &V, id: u32) -> Rc<str> {
        if let Some(path) = self.raw.get(&id) {
            return Rc::clone(path);
        }
        // Walk up to a cached folder or a root, then build back down.
        let mut chain = vec![id];
        let mut base: Rc<str> = Rc::from("");
        let mut cur = id;
        while let Some(p) = view.parent(cur) {
            if let Some(known) = self.raw.get(&p) {
                base = Rc::clone(known);
                break;
            }
            if chain.len() > view.len() {
                // A parent cycle; no valid view has one. Stop climbing.
                break;
            }
            chain.push(p);
            cur = p;
        }
        for &node in chain.iter().rev() {
            let full: Rc<str> = Rc::from(join(&base, view.name(node)));
            self.raw.insert(node, Rc::clone(&full));
            base = full;
        }
        base
    }

    /// The full path of folder `id` folded under `fold`, folded once per
    /// folder.
    pub fn folded<V: SearchView + ?Sized>(
        &mut self,
        view: &V,
        id: u32,
        fold: FoldOptions,
    ) -> Rc<str> {
        if let Some(path) = self.folded.get(&(id, fold)) {
            return Rc::clone(path);
        }
        let raw = self.raw(view, id);
        let folded: Rc<str> = match fold_str(&raw, fold) {
            Cow::Borrowed(_) => raw,
            Cow::Owned(s) => Rc::from(s),
        };
        self.folded.insert((id, fold), Rc::clone(&folded));
        folded
    }

    /// The folded full path of entry `id` whose parent is `parent` (`None`
    /// for a root): the parent's cached folded path joined with the
    /// entry's folded name.
    pub fn folded_path<V: SearchView + ?Sized>(
        &mut self,
        view: &V,
        id: u32,
        parent: Option<u32>,
        fold: FoldOptions,
    ) -> String {
        let name = folded_name(view, id, fold);
        let Some(p) = parent else {
            return name.into_owned();
        };
        let raw = self.raw(view, p);
        let prefix = self.folded(view, p, fold);
        let mut path = String::with_capacity(prefix.len() + 1 + name.len());
        join_into(&mut path, &raw, &prefix, &name);
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::{Metadata, VecView};

    #[test]
    fn join_matches_the_reference_join() {
        use crate::eval::Candidate;
        for (parent, name) in [("", "a"), ("/", "a"), ("/x", "a"), ("/x/", ""), ("", "")] {
            assert_eq!(
                join(parent, name),
                Candidate::file(name, parent).full_path()
            );
        }
    }

    #[test]
    fn folded_paths_reuse_the_folders_prefix() {
        let mut v = VecView::new();
        let root = v.push("/", None, true, Metadata::default());
        let users = v.push("Users", Some(root), true, Metadata::default());
        let me = v.push("Zoë", Some(users), true, Metadata::default());
        let file = v.push("Résumé.PDF", Some(me), false, Metadata::default());
        let mut cache = PathCache::default();
        let fold = FoldOptions::for_match(false, false);
        assert_eq!(&*cache.raw(&v, me), "/Users/Zoë");
        assert_eq!(
            cache.folded_path(&v, file, Some(me), fold),
            "/users/zoe/resume.pdf"
        );
        assert_eq!(cache.folded_path(&v, root, None, fold), "/");
        assert_eq!(cache.folded_path(&v, users, Some(root), fold), "/users");
    }

    #[test]
    fn the_memo_decides_each_folder_from_its_parent() {
        let mut v = VecView::new();
        let root = v.push("/", None, true, Metadata::default());
        let a = v.push("Resources", Some(root), true, Metadata::default());
        let b = v.push("src", Some(a), true, Metadata::default());
        let c = v.push("main.rs", Some(b), false, Metadata::default());
        let d = v.push("other", Some(root), false, Metadata::default());
        let fold = FoldOptions::for_term("resources/", false, false);
        let needle = fold_str("resources/", fold).into_owned();
        let finder = Finder::new(needle.as_bytes()).into_owned();
        let pn = PathNeedle {
            needle: &needle,
            finder: &finder,
            fold,
        };
        let memo = PathMemo::new(v.len());
        let mut scratch = Scratch::default();
        assert!(memo.is_hit(&v, &pn, c, &mut scratch));
        assert!(memo.is_hit(&v, &pn, b, &mut scratch));
        assert!(!memo.is_hit(&v, &pn, a, &mut scratch));
        assert!(!memo.is_hit(&v, &pn, d, &mut scratch));
        assert!(!memo.is_hit(&v, &pn, root, &mut scratch));
        assert_eq!(memo.load(root) & SLASH_END, SLASH_END);
        assert_eq!(memo.load(a) & (EMPTY | SLASH_END), 0);
    }
}
