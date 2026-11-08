//! Folder renames and reparents are applied in place (same entry ID, so the
//! writer records them with `Batch::moved`) rather than as a remove plus a
//! re-insert of the whole subtree.

mod common;

use common::*;
use fx_model::StartOutcome;

#[test]
fn folder_rename_and_reparent_keep_entry_ids() {
    let tree = FakeTree::new();
    tree.mkdir("/vol/a");
    tree.mkdir("/vol/a/sub");
    tree.touch("/vol/a/sub/f.txt", 3);
    tree.mkdir("/vol/b");
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());

    let sub = tree.key_of("/vol/a/sub").unwrap();
    let f = tree.key_of("/vol/a/sub/f.txt").unwrap();
    let snap = engine.snapshot();
    let sub_id = snap.index().lookup(sub).unwrap();
    let f_id = snap.index().lookup(f).unwrap();

    // Old path reported first, as FSEvents does for a rename.
    tree.rename("/vol/a/sub", "/vol/b/renamed");
    source.push(vec![check("/vol/a/sub"), check("/vol/b/renamed")]);
    let snap = wait_for_tree(&engine, &tree, "reparented folder indexed");
    assert_eq!(snap.index().lookup(sub), Some(sub_id), "folder kept its ID");
    assert_eq!(snap.index().lookup(f), Some(f_id), "child kept its ID");
    assert_eq!(rel_path(&snap, f).as_deref(), Some("b/renamed/f.txt"));
    assert_orders_match(&snap);

    // New path reported first.
    tree.rename("/vol/b/renamed", "/vol/top");
    source.push(vec![check("/vol/top"), check("/vol/b/renamed")]);
    let snap = wait_for_tree(&engine, &tree, "folder moved to the root");
    assert_eq!(snap.index().lookup(sub), Some(sub_id), "folder kept its ID");
    assert_eq!(snap.index().lookup(f), Some(f_id), "child kept its ID");
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn replaced_path_removes_the_old_entry() {
    let tree = FakeTree::new();
    tree.mkdir("/vol/x");
    tree.touch("/vol/x/inner", 1);
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());

    // A folder replaced by a file with the same name.
    tree.remove("/vol/x");
    tree.touch("/vol/x", 9);
    source.push(vec![check("/vol/x")]);
    let snap = wait_for_tree(&engine, &tree, "replaced path indexed");
    assert_orders_match(&snap);

    // And a file replaced by a new folder with contents.
    tree.remove("/vol/x");
    tree.mkdir("/vol/x");
    tree.touch("/vol/x/y", 2);
    source.push(vec![check("/vol/x")]);
    let snap = wait_for_tree(&engine, &tree, "new folder and contents indexed");
    assert_orders_match(&snap);
    engine.shutdown();
}

#[test]
fn create_under_an_unreported_new_folder_checks_the_parent() {
    let tree = FakeTree::new();
    let source = FakeSource::new(StartOutcome::Resumed);
    let engine = start(&tree, &source, tree.build_index(), config());

    tree.mkdir("/vol/new");
    tree.mkdir("/vol/new/deeper");
    tree.touch("/vol/new/deeper/file", 4);
    source.push(vec![check("/vol/new/deeper/file")]);
    let snap = wait_for_tree(&engine, &tree, "missing parents indexed");
    assert_orders_match(&snap);
    engine.shutdown();
}
