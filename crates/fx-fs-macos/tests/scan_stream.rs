//! The streaming scan (run 2.17): `scan_streaming` and the `MacScanner`
//! built on it send every entry once, each after its parent, and the same
//! entries as the collecting `scan`, with one walker thread or eight.
//!
//! Tests only ever create and remove their own temporary directory.

#![cfg(target_os = "macos")]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::time::{SystemTime, UNIX_EPOCH};

use fx_bench::{TreeShape, generate_tree, materialize};
use fx_fs::VolumeEnumerator;
use fx_fs_macos::enumerator::MacScanner;
use fx_fs_macos::scan::{ScanOptions, ScannedEntry, scan, scan_streaming};
use fx_model::FileKey;

/// A fresh temporary directory, removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "fx-stream-{tag}-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create unique temp dir");
        let path = fs::canonicalize(&path).expect("canonicalize temp dir");
        TempDir { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A materialized synthetic tree of 4,000 entries; returns its root.
fn fixture(tmp: &TempDir) -> PathBuf {
    let tree = generate_tree(4_000, 0x57e4_0001, TreeShape::default());
    materialize(&tree, &tmp.path, None).expect("materialize tree");
    tmp.path.join("root")
}

fn key_of(e: &ScannedEntry) -> (u32, u64) {
    (e.dev, e.file_id)
}

#[test]
fn the_streaming_scan_sends_each_entry_once_after_its_parent() {
    let tmp = TempDir::new("order");
    let root = fixture(&tmp);
    let collected = scan(&root, &ScanOptions::default())
        .unwrap()
        .entries
        .unwrap();
    let want: HashSet<(u32, u64)> = collected.iter().map(key_of).collect();
    let root_key = {
        let md = fs::metadata(&root).unwrap();
        use std::os::unix::fs::MetadataExt;
        (md.dev() as u32, md.ino())
    };
    for threads in [1, 8] {
        let opts = ScanOptions {
            threads,
            ..ScanOptions::default()
        };
        let (tx, rx) = sync_channel(4);
        let (root_ref, opts_ref) = (&root, &opts);
        let stats = std::thread::scope(|s| {
            // The walk owns the sender, so the channel closes when it ends.
            let walk = s.spawn(move || scan_streaming(root_ref, opts_ref, &tx));
            let mut sent = HashSet::new();
            sent.insert(root_key);
            let mut count = 0usize;
            for batch in rx.iter() {
                for e in batch {
                    assert!(
                        sent.contains(&e.parent_key),
                        "{threads} threads: {:?} arrived before its parent",
                        e.name
                    );
                    assert!(
                        sent.insert(key_of(&e)),
                        "{threads} threads: {:?} sent twice",
                        e.name
                    );
                    count += 1;
                }
            }
            let stats = walk.join().unwrap().unwrap();
            sent.remove(&root_key);
            assert_eq!(sent, want, "{threads} threads: the same entries as scan");
            assert_eq!(count, want.len());
            stats
        });
        assert_eq!(stats.entries as usize, want.len());
    }
}

#[test]
fn the_mac_scanner_streams_every_entry_after_its_parent() {
    let tmp = TempDir::new("scanner");
    let root = fixture(&tmp);
    let collected = scan(&root, &ScanOptions::default())
        .unwrap()
        .entries
        .unwrap();
    let scanner = MacScanner::new(ScanOptions::default());
    let mut seen: HashMap<FileKey, Option<FileKey>> = HashMap::new();
    let mut order_ok = true;
    let root_key = {
        let md = fs::metadata(&root).unwrap();
        use std::os::unix::fs::MetadataExt;
        FileKey::new(fx_model::VolumeId(md.dev() as u32), md.ino())
    };
    scanner
        .enumerate(&root, &mut |e| {
            let parent = e.parent.expect("every listed entry has a parent");
            if parent != root_key && !seen.contains_key(&parent) {
                order_ok = false;
            }
            assert!(seen.insert(e.key, e.parent).is_none(), "sent twice");
        })
        .unwrap();
    assert!(order_ok, "an entry reached the sink before its parent");
    assert_eq!(seen.len(), collected.len());
}
