//! Contract tests for `ExtensionTable` and `ExtId` (docs/PLAN.md, data-layout
//! table: "Extension ID, 2 B, makes `ext:` an integer compare").
//!
//! Contract assumed here:
//! - `intern(name)` takes a whole file name and returns the `ExtId` of the
//!   part after its last '.', ASCII-lowercased. `ExtId(0)` means "no
//!   extension" (no dot, a dot-file such as `.bashrc`, or a trailing dot).
//! - `lookup(ext)` takes an extension without the dot, case-insensitively,
//!   and never interns.
//! - `as_str(id)` returns `Some(lowercased ext)` for real IDs and `None` for
//!   `ExtId(0)` and `ExtId::OVERFLOW`.
//! - `len()` counts distinct real extensions (not the "none" ID).
//! - Once the 16-bit ID space is used up, new extensions map to
//!   `ExtId::OVERFLOW`, which callers treat as "compare by name".

use std::collections::HashSet;

use fx_index::{ExtId, ExtensionTable};

const NONE: ExtId = ExtId(0);

#[test]
fn new_table_is_empty() {
    let table = ExtensionTable::new();
    assert_eq!(table.len(), 0);
    assert_eq!(table.lookup("txt"), None);
}

#[test]
fn ext_id_is_two_bytes() {
    assert_eq!(std::mem::size_of::<ExtId>(), 2);
}

#[test]
fn overflow_id_is_reserved_and_distinct_from_none() {
    assert_ne!(ExtId::OVERFLOW, NONE);
}

#[test]
fn names_without_extension_get_id_zero() {
    let mut table = ExtensionTable::new();
    for name in ["Makefile", "README", "a", "file.", "dir.d.", "."] {
        assert_eq!(table.intern(name), NONE, "{name:?} has no extension");
    }
    assert_eq!(table.len(), 0, "no-extension names must not be interned");
    assert_eq!(table.as_str(NONE), None);
}

#[test]
fn dot_files_have_no_extension() {
    let mut table = ExtensionTable::new();
    for name in [".bashrc", ".gitignore", ".DS_Store", ".config"] {
        assert_eq!(table.intern(name), NONE, "{name:?} is a dot-file");
    }
    assert_eq!(table.len(), 0);
    // `.bashrc` must not have registered "bashrc" as an extension.
    assert_eq!(table.lookup("bashrc"), None);
}

#[test]
fn dot_file_with_a_further_dot_uses_the_last_part() {
    let mut table = ExtensionTable::new();
    let json = table.intern(".eslintrc.json");
    assert_ne!(json, NONE);
    assert_eq!(table.as_str(json), Some("json"));
    assert_eq!(table.intern("package.json"), json);
}

#[test]
fn multi_dot_names_use_the_part_after_the_last_dot() {
    let mut table = ExtensionTable::new();
    let gz = table.intern("a.tar.gz");
    assert_ne!(gz, NONE);
    assert_eq!(table.as_str(gz), Some("gz"));
    assert_eq!(table.lookup("gz"), Some(gz));
    assert_eq!(table.lookup("tar"), None, "only the last part is interned");
    assert_eq!(table.lookup("tar.gz"), None);
    assert_eq!(table.intern("backup.2024.01.gz"), gz);
}

#[test]
fn extensions_are_ascii_case_folded() {
    let mut table = ExtensionTable::new();
    let jpg = table.intern("photo.JPG");
    assert_ne!(jpg, NONE);
    assert_eq!(table.intern("photo.jpg"), jpg);
    assert_eq!(table.intern("other.Jpg"), jpg);
    assert_eq!(table.intern("x.jPG"), jpg);
    assert_eq!(table.as_str(jpg), Some("jpg"));
    assert_eq!(table.lookup("jpg"), Some(jpg));
    assert_eq!(table.lookup("JPG"), Some(jpg));
    assert_eq!(table.lookup("JpG"), Some(jpg));
    assert_eq!(table.len(), 1);
}

#[test]
fn non_ascii_extensions_fold_only_ascii_letters() {
    let mut table = ExtensionTable::new();
    let a = table.intern("file.\u{c4}BC"); // "ÄBC"
    let b = table.intern("other.\u{c4}bc"); // "Äbc"
    assert_ne!(a, NONE);
    assert_eq!(a, b);
    assert_eq!(table.as_str(a), Some("\u{c4}bc"));
    // Non-ASCII letters are not folded: "äbc" is a different extension.
    let c = table.intern("third.\u{e4}bc");
    assert_ne!(c, a);
    assert_eq!(table.len(), 2);
}

#[test]
fn distinct_extensions_get_distinct_nonzero_ids() {
    let mut table = ExtensionTable::new();
    let exts = ["txt", "rs", "toml", "md", "png", "jpg", "pdf", "gz"];
    let ids: Vec<ExtId> = exts
        .iter()
        .map(|e| table.intern(&format!("file.{e}")))
        .collect();
    let unique: HashSet<ExtId> = ids.iter().copied().collect();
    assert_eq!(unique.len(), exts.len());
    assert!(!unique.contains(&NONE));
    assert!(!unique.contains(&ExtId::OVERFLOW));
    assert_eq!(table.len(), exts.len());
    for (e, id) in exts.iter().zip(ids.iter()) {
        assert_eq!(table.as_str(*id), Some(*e));
        assert_eq!(table.lookup(e), Some(*id));
    }
}

#[test]
fn lookup_does_not_intern() {
    let mut table = ExtensionTable::new();
    table.intern("a.txt");
    assert_eq!(table.lookup("zip"), None);
    assert_eq!(table.len(), 1);
    assert_eq!(table.lookup("zip"), None);
}

#[test]
fn ids_are_stable_across_later_interning() {
    let mut table = ExtensionTable::new();
    let txt = table.intern("notes.txt");
    let rs = table.intern("main.rs");
    for i in 0..1_000 {
        table.intern(&format!("f.e{i}"));
    }
    assert_eq!(table.intern("again.TXT"), txt);
    assert_eq!(table.intern("lib.rs"), rs);
    assert_eq!(table.lookup("txt"), Some(txt));
    assert_eq!(table.as_str(txt), Some("txt"));
    assert_eq!(table.as_str(rs), Some("rs"));
    assert_eq!(table.len(), 1_002);
}

#[test]
fn overflow_maps_new_extensions_to_reserved_id() {
    let mut table = ExtensionTable::new();
    let first = table.intern("first.keep");

    // Far more distinct extensions than a u16 can name.
    let total = 70_000u32;
    let mut ids = Vec::with_capacity(total as usize);
    for i in 0..total {
        ids.push(table.intern(&format!("f.x{i}")));
    }

    // The ID space holds at most 65,535 real extensions (0 is "none").
    assert!(
        table.len() <= 65_535,
        "len {} exceeds u16 space",
        table.len()
    );
    // At least 65,534 real IDs are available before overflow kicks in.
    assert!(
        table.len() >= 65_534,
        "len {} overflowed early",
        table.len()
    );

    let real: Vec<ExtId> = ids
        .iter()
        .copied()
        .filter(|id| *id != ExtId::OVERFLOW)
        .collect();
    assert!(real.iter().all(|id| *id != NONE));
    let unique: HashSet<ExtId> = real.iter().copied().collect();
    assert_eq!(unique.len(), real.len(), "real IDs must stay distinct");
    assert!(!unique.contains(&first));

    // Everything past the ID space overflows, and nothing maps to "none".
    assert!(ids.iter().all(|id| *id != NONE));
    assert!(ids[65_535..].iter().all(|id| *id == ExtId::OVERFLOW));
    // Overflow happens only at the end, never interleaved with real IDs.
    let first_overflow = ids.iter().position(|id| *id == ExtId::OVERFLOW).unwrap();
    assert!(
        ids[first_overflow..]
            .iter()
            .all(|id| *id == ExtId::OVERFLOW)
    );

    // Interning an overflowed extension again stays OVERFLOW and adds nothing.
    let len = table.len();
    assert_eq!(table.intern("again.x69999"), ExtId::OVERFLOW);
    assert_eq!(table.intern("brand.new"), ExtId::OVERFLOW);
    assert_eq!(table.len(), len);
    assert_eq!(table.as_str(ExtId::OVERFLOW), None);

    // Lookup of an overflowed extension never returns some other real ID.
    let looked = table.lookup("x69999");
    assert!(
        looked.is_none() || looked == Some(ExtId::OVERFLOW),
        "overflowed extension looked up as {looked:?}"
    );

    // IDs issued before overflow are untouched.
    assert_eq!(table.intern("again.KEEP"), first);
    assert_eq!(table.as_str(first), Some("keep"));
    assert_eq!(table.intern("f.x0"), ids[0]);
    assert_eq!(table.as_str(ids[0]), Some("x0"));
    // Names without an extension are still "none" after overflow.
    assert_eq!(table.intern("Makefile"), NONE);
    assert_eq!(table.intern(".bashrc"), NONE);
}
