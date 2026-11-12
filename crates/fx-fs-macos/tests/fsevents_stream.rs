//! Per-volume FSEvents stream in `fx_fs_macos::fsevents::stream` (macOS only).
//!
//! Contract pinned here:
//! - `StreamConfig::new(watch_path)` (any `PathBuf`) with public fields
//!   `watch_path: PathBuf`, `path_filter: Option<PathBuf>` (absolute prefix;
//!   default `None`), `latency: Duration` (default 10 ms) and
//!   `no_defer: bool`.
//! - `VolumeStream::start(config, from: Option<&fx_model::ResumeCursor>)
//!   -> io::Result<(VolumeStream, fx_model::StartOutcome)>`, deciding the
//!   start with `decide_start` on the watched volume's UUID and current
//!   event ID. `cursor(&self) -> fx_model::ResumeCursor` is always the
//!   `FsEvents` variant. (The tests wrap both in a local `FsEventsCursor`
//!   view for readability.)
//! - `dropped_before_cursor(&self) -> u64` counts replayed events at or
//!   before the resume cursor that the stream dropped instead of delivering.
//! - `RawEvent { path: PathBuf, flags: EventFlags, rescan_subtree: bool,
//!   id: u64, received: Instant }`
//!   with absolute, real paths (e.g. `/private/var/...`, the firmlinked form,
//!   not `/System/Volumes/Data/...`).
//! - With a `path_filter`, only events at or below the prefix are delivered,
//!   except the HistoryDone marker, which is always delivered through the
//!   channel as a `RawEvent` whose flags contain `EventFlags::HISTORY_DONE`,
//!   and (run 2.1) the events `flags::admit` lets through from outside it:
//!   volume-level flags on any path, and events on a folder above the
//!   prefix that rescan, mount over, create, remove or rename it.
//! - `scope_root(&self) -> &Path` (run 2.1) is the root of what the stream
//!   delivers: the normalised path filter, or `mount_point()` without one.
//!   It is the `volume_root` `FsEventsSource` passes to `classify`.
//! - `recv_timeout(&self, Duration) -> Result<RawEvent, mpsc::RecvTimeoutError>`,
//!   `try_iter(&self)` yielding `RawEvent`s, `cursor(&self) -> FsEventsCursor`
//!   (the volume UUID plus an event ID; never the SinceNow sentinel, usable
//!   after `stop`), `history_done(&self) -> bool` (true at once when the
//!   stream starts from now), `flush_sync(&self)` and `stop(&mut self)`.
//!   `Drop` stops and joins; `VolumeStream` is `Send`.
//! - While replaying (before HistoryDone) `cursor().event_id` stays at the
//!   replay start (the saved cursor's ID), because replay arrives in path
//!   order, not ID order. The cursor and the `history_done` flag change
//!   together, so reading `cursor()` and then seeing `history_done() ==
//!   false` means the cursor was taken before HistoryDone. After the marker
//!   the cursor is at least the highest replayed ID.
//! - `RawEvent::flags` is the exact kernel word. During replay the stream
//!   sets the separate `rescan_subtree` field on directory renames
//!   (`ITEM_RENAMED | ITEM_IS_DIR`) instead of adding flag bits.
//! - `mount_point(&self) -> &Path` is the watched volume's mount path in the
//!   stream's normalised form (unchanged by `normalise_firmlinks`), the
//!   `volume_root` to pass to `flags::classify`.
//!
//! Every test works in its own directory under `std::env::temp_dir()`,
//! filters events to it, and deletes nothing outside it.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fx_fs_macos::fsevents::flags::{EventAction, EventFlags, admit, classify};
use fx_fs_macos::fsevents::stream::{
    RawEvent, StreamConfig, VolumeStream as RealStream, normalise_firmlinks,
};
use fx_fs_macos::fsevents::volume::{current_event_id, device_of, volume_uuid};
use fx_model::{ResumeCursor, StartOutcome};

/// Test-local view of an FSEvents `ResumeCursor` with named fields, so the
/// assertions below can read the event ID and UUID directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FsEventsCursor {
    event_id: u64,
    db_uuid: [u8; 16],
}

impl FsEventsCursor {
    fn from_resume(c: ResumeCursor) -> FsEventsCursor {
        match c {
            ResumeCursor::FsEvents { event_id, db_uuid } => FsEventsCursor { event_id, db_uuid },
            other => panic!("FSEvents stream returned a non-FSEvents cursor: {other:?}"),
        }
    }

    fn to_resume(self) -> ResumeCursor {
        ResumeCursor::FsEvents {
            event_id: self.event_id,
            db_uuid: self.db_uuid,
        }
    }
}

/// Thin test wrapper over the real stream: `start` takes the test cursor
/// and `cursor()` returns it; everything else derefs to the real stream.
struct VolumeStream(RealStream);

impl VolumeStream {
    fn start(
        config: StreamConfig,
        from: Option<&FsEventsCursor>,
    ) -> std::io::Result<(VolumeStream, StartOutcome)> {
        let from = from.map(|c| c.to_resume());
        let (stream, outcome) = RealStream::start(config, from.as_ref())?;
        Ok((VolumeStream(stream), outcome))
    }

    fn cursor(&self) -> FsEventsCursor {
        FsEventsCursor::from_resume(self.0.cursor())
    }
}

impl std::ops::Deref for VolumeStream {
    type Target = RealStream;
    fn deref(&self) -> &RealStream {
        &self.0
    }
}

impl std::ops::DerefMut for VolumeStream {
    fn deref_mut(&mut self) -> &mut RealStream {
        &mut self.0
    }
}

const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const REPLAY_TIMEOUT: Duration = Duration::from_secs(10);

/// A unique temp directory (canonicalised, so `/var` reads `/private/var`)
/// that removes itself, and only itself, on drop.
struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> TempRoot {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after 1970")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "fx-fsevents-stream-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp root");
        TempRoot(fs::canonicalize(&dir).expect("canonicalize temp root"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn filtered_config(root: &Path) -> StreamConfig {
    StreamConfig {
        path_filter: Some(root.to_path_buf()),
        ..StreamConfig::new(root.to_path_buf())
    }
}

fn root_uuid(root: &Path) -> [u8; 16] {
    volume_uuid(device_of(root).expect("device_of")).expect("data volume has an FSEvents UUID")
}

fn is_marker(ev: &RawEvent) -> bool {
    ev.flags.contains(EventFlags::HISTORY_DONE)
}

/// The cursor, if it was provably taken before HistoryDone: it is read
/// first, and `history_done()` is still false afterwards.
fn cursor_while_replaying(stream: &VolumeStream) -> Option<FsEventsCursor> {
    let cursor = stream.cursor();
    (!stream.history_done()).then_some(cursor)
}

/// Waits for an event at `path` whose flags contain `want` and that was
/// received at or after `since`. Every event seen must respect the filter.
fn wait_for(
    stream: &VolumeStream,
    root: &Path,
    path: &Path,
    want: EventFlags,
    since: Instant,
) -> RawEvent {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        match stream.recv_timeout(left) {
            Ok(ev) => {
                assert!(
                    admit(&ev.path, ev.flags, Some(root)),
                    "event outside the filter: {} ({:#x})",
                    ev.path.display(),
                    ev.flags.bits()
                );
                if ev.path == path && ev.flags.contains(want) && ev.received >= since {
                    return ev;
                }
            }
            Err(RecvTimeoutError::Timeout) => panic!(
                "no event with flags {:#x} for {} within {EVENT_TIMEOUT:?}",
                want.bits(),
                path.display()
            ),
            Err(RecvTimeoutError::Disconnected) => panic!("stream channel disconnected"),
        }
    }
}

fn assert_fast(ev: &RawEvent, op_started: Instant, what: &str) {
    let latency = ev.received.saturating_duration_since(op_started);
    assert!(
        latency < Duration::from_secs(1),
        "{what}: latency {latency:?} is not under 1 s"
    );
}

#[test]
fn config_defaults() {
    let root = TempRoot::new("config");
    let config = StreamConfig::new(root.path().to_path_buf());
    assert_eq!(config.watch_path, root.path());
    assert_eq!(config.path_filter, None);
    assert_eq!(config.latency, Duration::from_millis(10));
}

#[test]
fn fresh_start_is_since_now_with_history_done() {
    let root = TempRoot::new("fresh");
    let (mut stream, outcome) =
        VolumeStream::start(filtered_config(root.path()), None).expect("start");
    assert_eq!(outcome, StartOutcome::NoCursorSupport);
    assert!(stream.history_done(), "SinceNow has no history to replay");
    assert_eq!(
        stream.dropped_before_cursor(),
        0,
        "a SinceNow stream has no cursor to drop events before"
    );

    let cursor = stream.cursor();
    assert_eq!(cursor.db_uuid, root_uuid(root.path()));
    assert_ne!(cursor.event_id, u64::MAX, "cursor must not be SinceNow");
    assert!(cursor.event_id <= current_event_id());

    stream.flush_sync();
    stream.stop();
    let after = stream.cursor();
    assert_eq!(after.db_uuid, cursor.db_uuid);
    assert!(after.event_id >= cursor.event_id);
}

#[test]
fn live_events_are_reported_quickly_with_item_flags() {
    let root = TempRoot::new("live");
    let dir = root.path();
    let config = StreamConfig {
        no_defer: true,
        ..filtered_config(dir)
    };
    let (stream, outcome) = VolumeStream::start(config, None).expect("start");
    assert_eq!(outcome, StartOutcome::NoCursorSupport);
    // Let the stream settle before the first change.
    thread::sleep(Duration::from_millis(200));

    let file = dir.join("live.txt");
    let t = Instant::now();
    fs::write(&file, b"hello").expect("create");
    let ev = wait_for(&stream, dir, &file, EventFlags::ITEM_CREATED, t);
    assert!(
        ev.flags.contains(EventFlags::ITEM_IS_FILE),
        "{:#x}",
        ev.flags.bits()
    );
    assert!(!ev.flags.contains(EventFlags::ITEM_IS_DIR));
    assert_fast(&ev, t, "create");

    let sub = dir.join("sub");
    let t = Instant::now();
    fs::create_dir(&sub).expect("mkdir");
    let ev = wait_for(&stream, dir, &sub, EventFlags::ITEM_CREATED, t);
    assert!(
        ev.flags.contains(EventFlags::ITEM_IS_DIR),
        "{:#x}",
        ev.flags.bits()
    );
    assert_fast(&ev, t, "mkdir");

    let t = Instant::now();
    fs::write(&file, b"hello, modified").expect("modify");
    let ev = wait_for(&stream, dir, &file, EventFlags::ITEM_MODIFIED, t);
    assert!(ev.flags.contains(EventFlags::ITEM_IS_FILE));
    assert_fast(&ev, t, "modify");

    let renamed = sub.join("renamed.txt");
    let t = Instant::now();
    fs::rename(&file, &renamed).expect("rename");
    let old = wait_for(&stream, dir, &file, EventFlags::ITEM_RENAMED, t);
    assert_fast(&old, t, "rename (old name)");
    let new = wait_for(&stream, dir, &renamed, EventFlags::ITEM_RENAMED, t);
    assert!(new.flags.contains(EventFlags::ITEM_IS_FILE));
    assert_fast(&new, t, "rename (new name)");

    let t = Instant::now();
    fs::remove_file(&renamed).expect("delete");
    let ev = wait_for(&stream, dir, &renamed, EventFlags::ITEM_REMOVED, t);
    assert_fast(&ev, t, "delete");

    let cursor = stream.cursor();
    assert!(
        cursor.event_id >= ev.id,
        "cursor {} < last id {}",
        cursor.event_id,
        ev.id
    );
    assert_eq!(cursor.db_uuid, root_uuid(dir));
}

/// Scripted change set with a model of the expected final tree.
struct Script {
    root: PathBuf,
    touched: BTreeSet<PathBuf>,
    /// Absolute path -> `None` for a directory, file contents otherwise.
    model: BTreeMap<PathBuf, Option<Vec<u8>>>,
    ops: usize,
}

impl Script {
    fn new(root: &Path) -> Script {
        Script {
            root: root.to_path_buf(),
            touched: BTreeSet::new(),
            model: BTreeMap::new(),
            ops: 0,
        }
    }

    fn abs(&mut self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        self.touched.insert(p.clone());
        p
    }

    fn mkdir(&mut self, rel: &str) {
        let p = self.abs(rel);
        fs::create_dir(&p).expect("mkdir");
        self.model.insert(p, None);
        self.ops += 1;
    }

    fn create(&mut self, rel: &str) {
        let p = self.abs(rel);
        let data = rel.as_bytes().to_vec();
        fs::write(&p, &data).expect("create");
        self.model.insert(p, Some(data));
        self.ops += 1;
    }

    fn modify(&mut self, rel: &str) {
        let p = self.abs(rel);
        let mut data = fs::read(&p).expect("read for modify");
        data.extend_from_slice(b"+modified");
        fs::write(&p, &data).expect("modify");
        self.model.insert(p, Some(data));
        self.ops += 1;
    }

    fn rename(&mut self, from: &str, to: &str) {
        let src = self.abs(from);
        let dst = self.abs(to);
        fs::rename(&src, &dst).expect("rename");
        let moved: Vec<PathBuf> = self
            .model
            .keys()
            .filter(|k| k.starts_with(&src))
            .cloned()
            .collect();
        for old in moved {
            let value = self.model.remove(&old).expect("model entry");
            let tail = old.strip_prefix(&src).expect("prefix");
            let new = if tail.as_os_str().is_empty() {
                dst.clone()
            } else {
                dst.join(tail)
            };
            self.model.insert(new, value);
        }
        self.ops += 1;
    }

    fn remove_file(&mut self, rel: &str) {
        let p = self.abs(rel);
        fs::remove_file(&p).expect("remove file");
        self.model.remove(&p);
        self.ops += 1;
    }

    fn remove_dir(&mut self, rel: &str) {
        let p = self.abs(rel);
        fs::remove_dir(&p).expect("remove empty dir");
        self.model.remove(&p);
        self.ops += 1;
    }

    /// About 200 operations: nested dirs, creates, modifies, file and
    /// directory renames, deletes, and short-lived files.
    fn run(&mut self) {
        for i in 0..5 {
            self.mkdir(&format!("d{i}"));
            self.mkdir(&format!("d{i}/n"));
            self.mkdir(&format!("d{i}/n/m"));
        }
        for i in 0..5 {
            for j in 0..6 {
                self.create(&format!("d{i}/f{j}.txt"));
            }
            for j in 0..10 {
                self.create(&format!("d{i}/n/m/g{j}.dat"));
            }
        }
        for i in 0..5 {
            for j in 0..4 {
                self.rename(&format!("d{i}/f{j}.txt"), &format!("d{i}/r{j}.txt"));
                self.modify(&format!("d{i}/r{j}.txt"));
            }
        }
        // Directory renames, including a move into another directory.
        self.rename("d0/n", "d0/n2");
        self.rename("d1", "d1r");
        self.rename("d2/n/m", "d2/m2");
        self.rename("d3", "d4/d3moved");
        for j in 0..5 {
            self.remove_file(&format!("d0/n2/m/g{j}.dat"));
            self.remove_file(&format!("d1r/n/m/g{j}.dat"));
        }
        for j in 0..10 {
            self.remove_file(&format!("d2/m2/g{j}.dat"));
        }
        self.remove_dir("d2/m2");
        for j in 0..4 {
            self.remove_file(&format!("d4/d3moved/r{j}.txt"));
        }
        for k in 0..20 {
            self.create(&format!("t{k}.tmp"));
            self.remove_file(&format!("t{k}.tmp"));
        }
        self.mkdir("d1r/new");
        self.mkdir("d1r/new/deep");
        for k in 0..3 {
            self.create(&format!("d1r/new/deep/z{k}.txt"));
        }
    }
}

fn disk_state(dir: &Path, out: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() {
            out.insert(path.clone(), None);
            disk_state(&path, out);
        } else {
            out.insert(path.clone(), Some(fs::read(&path).expect("read file")));
        }
    }
}

#[test]
fn offline_changes_replay_exactly_before_history_done() {
    let root = TempRoot::new("replay");
    let dir = root.path();
    let uuid = root_uuid(dir);

    // 1. Take a cursor from a running stream, then stop it.
    let saved = {
        let (mut stream, outcome) =
            VolumeStream::start(filtered_config(dir), None).expect("first start");
        assert_eq!(outcome, StartOutcome::NoCursorSupport);
        stream.flush_sync();
        let cursor = stream.cursor();
        stream.stop();
        assert_eq!(stream.cursor(), cursor, "cursor stays usable after stop");
        cursor
    };
    assert_eq!(saved.db_uuid, uuid);
    assert_ne!(saved.event_id, u64::MAX);

    // 2. Change the tree while no stream runs.
    let mut script = Script::new(dir);
    script.run();
    assert!(script.ops >= 200, "script ran only {} ops", script.ops);
    let mut on_disk = BTreeMap::new();
    disk_state(dir, &mut on_disk);
    assert_eq!(
        on_disk, script.model,
        "final on-disk state matches the script"
    );
    // Give fseventsd a moment to take in the last changes.
    thread::sleep(Duration::from_millis(300));

    // 3. Resume from the saved cursor and collect everything up to the
    //    HistoryDone marker.
    let restored = FsEventsCursor::from_resume(saved.to_resume());
    let (stream, outcome) =
        VolumeStream::start(filtered_config(dir), Some(&restored)).expect("resume start");
    assert_eq!(outcome, StartOutcome::Resumed);

    let deadline = Instant::now() + REPLAY_TIMEOUT;
    let mut history = Vec::new();
    let mut checked_mid_replay = 0usize;
    loop {
        if let Some(c) = cursor_while_replaying(&stream) {
            assert_eq!(c.db_uuid, uuid);
            assert_eq!(
                c.event_id, saved.event_id,
                "cursor moved off the replay start before HistoryDone"
            );
            checked_mid_replay += 1;
        }
        let left = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        match stream.recv_timeout(left) {
            Ok(ev) if is_marker(&ev) => break,
            Ok(ev) => history.push(ev),
            Err(RecvTimeoutError::Timeout) => panic!(
                "no HistoryDone within {REPLAY_TIMEOUT:?} ({} events so far)",
                history.len()
            ),
            Err(RecvTimeoutError::Disconnected) => panic!("stream channel disconnected"),
        }
    }
    assert!(stream.history_done(), "history_done() after the marker");

    // Every delivered event respects the filter. Replayed events at or
    // before the cursor are not delivered: the stream drops them and counts
    // them in `dropped_before_cursor()`, which is asserted on instead of the
    // delivered IDs.
    for ev in &history {
        assert!(
            admit(&ev.path, ev.flags, Some(dir)),
            "event outside the filter: {} ({:#x})",
            ev.path.display(),
            ev.flags.bits()
        );
    }
    let dropped = stream.dropped_before_cursor();
    let delivered_at_or_before = history.iter().filter(|e| e.id <= saved.event_id).count();
    assert_eq!(
        delivered_at_or_before, 0,
        "events at or before the cursor must be counted, not delivered (dropped_before_cursor = {dropped})"
    );
    eprintln!(
        "replay: {} events delivered, {dropped} dropped before the cursor",
        history.len()
    );

    // The flags word is the exact kernel word: the stream never adds bits
    // to it. The rescan it adds to directory renames during replay is the
    // separate `rescan_subtree` field.
    for ev in &history {
        if ev.rescan_subtree && !ev.flags.contains(EventFlags::MUST_SCAN_SUB_DIRS) {
            assert!(
                ev.flags
                    .contains(EventFlags::ITEM_RENAMED | EventFlags::ITEM_IS_DIR),
                "rescan_subtree set on a non-directory-rename event {:#x} at {}",
                ev.flags.bits(),
                ev.path.display()
            );
        }
    }

    // Every scripted path is reported (directly, or covered by a subtree
    // rescan of one of its ancestors).
    let reported: BTreeSet<&Path> = history.iter().map(|e| e.path.as_path()).collect();
    let rescanned: Vec<&Path> = history
        .iter()
        .filter(|e| e.flags.contains(EventFlags::MUST_SCAN_SUB_DIRS) || e.rescan_subtree)
        .map(|e| e.path.as_path())
        .collect();
    let missing: Vec<&PathBuf> = script
        .touched
        .iter()
        .filter(|p| !reported.contains(p.as_path()))
        .filter(|p| !rescanned.iter().any(|r| p.starts_with(r)))
        .collect();
    assert!(
        missing.is_empty(),
        "{} of {} scripted paths not replayed, e.g. {:?}",
        missing.len(),
        script.touched.len(),
        &missing[..missing.len().min(10)]
    );

    // How strong the check above is: how many scripted paths were reported
    // directly, and how many were covered only by the rescan the stream
    // adds to directory renames during replay (`RawEvent::flags` docs).
    let renamed_dir = EventFlags::ITEM_RENAMED | EventFlags::ITEM_IS_DIR;
    let added_rescans: Vec<&Path> = history
        .iter()
        .filter(|e| {
            e.rescan_subtree
                && !e.flags.contains(EventFlags::MUST_SCAN_SUB_DIRS)
                && e.flags.contains(renamed_dir)
        })
        .map(|e| e.path.as_path())
        .collect();
    let other_rescans: Vec<&Path> = history
        .iter()
        .filter(|e| e.flags.contains(EventFlags::MUST_SCAN_SUB_DIRS))
        .map(|e| e.path.as_path())
        .collect();
    let direct = script
        .touched
        .iter()
        .filter(|p| reported.contains(p.as_path()))
        .count();
    let only_added_rescan = script
        .touched
        .iter()
        .filter(|p| !reported.contains(p.as_path()))
        .filter(|p| !other_rescans.iter().any(|r| p.starts_with(r)))
        .filter(|p| added_rescans.iter().any(|r| p.starts_with(r)))
        .count();
    eprintln!(
        "offline replay: {} scripted paths, {direct} reported directly, \
         {only_added_rescan} covered only by an added directory-rename rescan, \
         {} replayed events, cursor checked {checked_mid_replay} times before HistoryDone",
        script.touched.len(),
        history.len()
    );

    // Nothing outside the scripted set (the touched paths, their ancestors
    // and the root itself) is reported.
    let mut allowed: BTreeSet<&Path> = BTreeSet::new();
    allowed.insert(dir);
    for p in &script.touched {
        for a in p.ancestors().take_while(|a| a.starts_with(dir)) {
            allowed.insert(a);
        }
    }
    // Events above the filter that `admit` let through rescan the whole
    // directory; they are not scripted paths.
    let extra: Vec<&&Path> = reported
        .iter()
        .filter(|p| p.starts_with(dir) && !allowed.contains(**p))
        .collect();
    assert!(extra.is_empty(), "unexpected replayed paths: {extra:?}");

    let max_id = history
        .iter()
        .map(|e| e.id)
        .max()
        .expect("history is not empty");
    let cursor = stream.cursor();
    assert_eq!(cursor.db_uuid, uuid);
    assert!(
        cursor.event_id >= max_id,
        "cursor {} < {max_id}",
        cursor.event_id
    );
}

/// Writes `n` files under `dir` while no stream runs, after taking a cursor.
fn cursor_then_offline_files(dir: &Path, n: usize) -> FsEventsCursor {
    let saved = {
        let (mut stream, _) = VolumeStream::start(filtered_config(dir), None).expect("first start");
        stream.flush_sync();
        let cursor = stream.cursor();
        stream.stop();
        cursor
    };
    for i in 0..n {
        let sub = dir.join(format!("d{:02}", i % 20));
        fs::create_dir_all(&sub).expect("mkdir");
        fs::write(sub.join(format!("f{i:04}.txt")), b"x").expect("write");
    }
    thread::sleep(Duration::from_millis(300));
    saved
}

#[test]
fn cursor_stays_at_saved_id_when_stopped_before_history_done() {
    let root = TempRoot::new("cursor-mid-replay");
    let dir = root.path();
    let saved = cursor_then_offline_files(dir, 400);

    // Read the cursor right after the resume start, then stop at once.
    let (mut stream, outcome) =
        VolumeStream::start(filtered_config(dir), Some(&saved)).expect("resume start");
    assert_eq!(outcome, StartOutcome::Resumed);
    let early = cursor_while_replaying(&stream);
    stream.stop();
    let after_stop = stream.cursor();
    let done_at_stop = stream.history_done();

    assert_eq!(after_stop.db_uuid, saved.db_uuid);
    assert!(
        after_stop.event_id >= saved.event_id,
        "cursor {} went below the saved {}",
        after_stop.event_id,
        saved.event_id
    );
    if let Some(c) = &early {
        assert_eq!(*c, saved, "cursor before HistoryDone is the replay start");
    }
    if !done_at_stop {
        // Stopped mid-replay: resuming must not skip what was not replayed.
        assert_eq!(
            after_stop, saved,
            "cursor after a stop before HistoryDone is the replay start"
        );
    }
    eprintln!(
        "mid-replay cursor: observed before HistoryDone at start = {}, \
         history done at stop = {done_at_stop}",
        early.is_some()
    );

    // Whatever happened, the cursor still resumes and replays the offline
    // files again (nothing is lost by stopping early).
    let (stream, outcome) =
        VolumeStream::start(filtered_config(dir), Some(&after_stop)).expect("second resume");
    assert_eq!(outcome, StartOutcome::Resumed);
    let deadline = Instant::now() + REPLAY_TIMEOUT;
    let mut seen = 0usize;
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        match stream.recv_timeout(left) {
            Ok(ev) if is_marker(&ev) => break,
            Ok(_) => seen += 1,
            Err(e) => panic!("no HistoryDone on second resume: {e:?}"),
        }
    }
    if !done_at_stop {
        assert!(
            seen > 0,
            "second resume replayed nothing after an early stop"
        );
    }
}

#[test]
fn mount_point_is_the_normalised_volume_root_for_classify() {
    let root = TempRoot::new("mount");
    let dir = root.path();
    let (stream, _) = VolumeStream::start(filtered_config(dir), None).expect("start");
    let mount = stream.mount_point().to_path_buf();
    assert!(mount.is_absolute(), "mount point {}", mount.display());
    assert_eq!(
        normalise_firmlinks(&mount),
        mount,
        "mount point is in the stream's normalised form"
    );
    assert_eq!(
        device_of(&mount).expect("device_of mount"),
        device_of(dir).expect("device_of dir"),
        "mount point {} is on the watched volume",
        mount.display()
    );
    assert_eq!(
        classify(&mount, EventFlags::UNMOUNT, &mount),
        EventAction::RescanVolume
    );
    assert_eq!(
        classify(dir, EventFlags::UNMOUNT, &mount),
        EventAction::CheckPath(dir.to_path_buf())
    );
}

#[test]
fn tampered_uuid_is_a_gap() {
    let root = TempRoot::new("gap-uuid");
    let uuid = root_uuid(root.path());
    let mut other = uuid;
    other[0] ^= 0xff;
    let tampered = FsEventsCursor {
        event_id: current_event_id(),
        db_uuid: other,
    };
    let (stream, outcome) =
        VolumeStream::start(filtered_config(root.path()), Some(&tampered)).expect("start");
    assert_eq!(outcome, StartOutcome::GapDetected);
    assert!(stream.history_done(), "a gap streams from now");
    let cursor = stream.cursor();
    assert_eq!(cursor.db_uuid, uuid, "the new cursor carries the real UUID");
    assert_ne!(cursor.event_id, u64::MAX);
}

#[test]
fn event_id_ahead_is_a_gap() {
    let root = TempRoot::new("gap-ahead");
    let uuid = root_uuid(root.path());
    let ahead = FsEventsCursor {
        event_id: u64::MAX,
        db_uuid: uuid,
    };
    let (stream, outcome) =
        VolumeStream::start(filtered_config(root.path()), Some(&ahead)).expect("start");
    assert_eq!(outcome, StartOutcome::GapDetected);
    assert!(stream.history_done(), "a gap streams from now");
    let cursor = stream.cursor();
    assert_eq!(cursor.db_uuid, uuid);
    assert_ne!(cursor.event_id, u64::MAX);

    // The stream still delivers live events after a gap.
    let file = root.path().join("after-gap.txt");
    let t = Instant::now();
    fs::write(&file, b"x").expect("create");
    let ev = wait_for(&stream, root.path(), &file, EventFlags::ITEM_CREATED, t);
    assert_fast(&ev, t, "create after gap");
}

#[test]
fn drop_while_events_flow_does_not_hang_or_crash() {
    let root = TempRoot::new("drop");
    let dir = root.path().to_path_buf();

    let running = Arc::new(AtomicBool::new(true));
    let writer = {
        let running = Arc::clone(&running);
        let dir = dir.clone();
        thread::spawn(move || {
            let mut n = 0u64;
            while running.load(Ordering::Relaxed) {
                let p = dir.join(format!("churn{}.txt", n % 16));
                let _ = fs::write(&p, n.to_le_bytes());
                if n % 3 == 0 {
                    let _ = fs::remove_file(&p);
                }
                n += 1;
                thread::sleep(Duration::from_micros(200));
            }
        })
    };

    for cycle in 0..6 {
        let (stream, _) = VolumeStream::start(filtered_config(&dir), None).expect("start");
        if cycle % 2 == 0 {
            // Wait until events are actually flowing.
            stream
                .recv_timeout(EVENT_TIMEOUT)
                .unwrap_or_else(|e| panic!("cycle {cycle}: no event while churning: {e:?}"));
            stream.flush_sync();
            let _drained = stream.try_iter().count();
        }
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            drop(stream);
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("cycle {cycle}: dropping the stream hung"));
    }

    running.store(false, Ordering::Relaxed);
    writer.join().expect("writer thread");
}

#[test]
fn renaming_a_folder_above_the_filter_is_delivered_as_a_rescan_of_it() {
    // Run 2.1: the filter used to drop every event outside it, so moving a
    // folder above the filtered root went unseen.
    let root = TempRoot::new("above-filter");
    let outer = root.path().join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(&inner).expect("mkdir");
    fs::write(inner.join("a.txt"), b"x").expect("write");
    let (stream, _) = VolumeStream::start(filtered_config(&inner), None).expect("start");
    assert_eq!(
        stream.scope_root(),
        inner.as_path(),
        "the filter is the scope root"
    );
    // Let the stream settle, then move the folder above the filter.
    stream.flush_sync();
    thread::sleep(Duration::from_millis(200));
    let since = Instant::now();
    let moved = root.path().join("outer_moved");
    fs::rename(&outer, &moved).expect("rename outer");

    let ev = wait_for(&stream, &inner, &outer, EventFlags::ITEM_RENAMED, since);
    assert_eq!(
        classify(&ev.path, ev.flags, stream.scope_root()),
        EventAction::RescanSubtree(inner.clone()),
        "a folder above the filter that moved rescans the filtered root ({:#x})",
        ev.flags.bits()
    );
    fs::rename(&moved, &outer).expect("rename back");
}

#[test]
fn scope_root_is_the_mount_point_without_a_filter() {
    let root = TempRoot::new("scope-mount");
    let (stream, _) =
        VolumeStream::start(StreamConfig::new(root.path().to_path_buf()), None).expect("start");
    assert_eq!(stream.scope_root(), stream.mount_point());
}
