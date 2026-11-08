//! Contract tests for the platform traits in `fx-fs` (docs/PLAN.md,
//! "Platform traits").
//!
//! The traits must speak the `fx-model` types (`Entry`, `ResumeCursor`,
//! `Change`, `StartOutcome`) rather than redefining them: the mock
//! implementations below use the `fx_model` types directly, so they only
//! compile if the trait signatures use the very same types.
//!
//! The crate exposes a `Result<T>` alias (either over a crate `Error` type or
//! over `std::io::Error`); an `io::Error` must convert into it with `?`, so
//! platform walkers can propagate system-call failures directly.

use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fx_fs::{AccessState, ChangeSource, PermissionProbe, VolumeEnumerator};
use fx_model::{
    Change, Entry, EntryFlags, EntryKind, FileKey, ResumeCursor, StartOutcome, VolumeId,
};

fn entry(id: u64, parent: Option<u64>, name: &str, kind: EntryKind) -> Entry {
    let vol = VolumeId(1);
    Entry {
        key: FileKey::new(vol, id),
        parent: parent.map(|p| FileKey::new(vol, p)),
        name: name.to_string(),
        kind,
        flags: EntryFlags::empty(),
        size: 0,
        modified: 0,
        created: 0,
    }
}

/// An enumerator that emits a fixed, parent-before-child list.
struct FixedEnumerator {
    entries: Vec<Entry>,
}

impl VolumeEnumerator for FixedEnumerator {
    fn enumerate(&self, _root: &Path, sink: &mut dyn FnMut(Entry)) -> fx_fs::Result<()> {
        for e in &self.entries {
            sink(e.clone());
        }
        Ok(())
    }
}

/// An enumerator that fails with an I/O error, propagated with `?`.
struct FailingEnumerator;

fn failing_syscall() -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
}

impl VolumeEnumerator for FailingEnumerator {
    fn enumerate(&self, _root: &Path, _sink: &mut dyn FnMut(Entry)) -> fx_fs::Result<()> {
        failing_syscall()?;
        Ok(())
    }
}

#[test]
fn volume_enumerator_is_object_safe_and_feeds_the_sink() {
    let fixed = FixedEnumerator {
        entries: vec![
            entry(2, None, "", EntryKind::Dir),
            entry(10, Some(2), "Users", EntryKind::Dir),
            entry(11, Some(10), "notes.txt", EntryKind::File),
        ],
    };
    let enumerator: &dyn VolumeEnumerator = &fixed;

    let mut got: Vec<Entry> = Vec::new();
    let result = enumerator.enumerate(Path::new("/"), &mut |e| got.push(e));
    assert!(result.is_ok());
    assert_eq!(got, fixed.entries);
}

#[test]
fn volume_enumerator_errors_propagate_from_io() {
    let enumerator: Box<dyn VolumeEnumerator> = Box::new(FailingEnumerator);
    let mut count = 0usize;
    let result = enumerator.enumerate(Path::new("/"), &mut |_| count += 1);
    assert!(result.is_err());
    assert_eq!(count, 0);
}

/// A change source that replays a scripted list of batches.
struct ScriptedSource {
    batches: Vec<Vec<Change>>,
    last_event: u64,
    started: bool,
}

impl ChangeSource for ScriptedSource {
    fn start(&mut self, from: Option<&ResumeCursor>) -> fx_fs::Result<StartOutcome> {
        self.started = true;
        Ok(match from {
            None => StartOutcome::NoCursorSupport,
            Some(ResumeCursor::FsEvents { event_id, .. }) => {
                self.last_event = *event_id;
                StartOutcome::Resumed
            }
            Some(_) => StartOutcome::GapDetected,
        })
    }

    fn next_batch(&mut self, deadline: Instant) -> fx_fs::Result<Vec<Change>> {
        let _ = deadline;
        if self.batches.is_empty() {
            return Ok(Vec::new());
        }
        self.last_event += 1;
        Ok(self.batches.remove(0))
    }

    fn cursor(&self) -> ResumeCursor {
        ResumeCursor::FsEvents {
            event_id: self.last_event,
            db_uuid: [7; 16],
        }
    }
}

#[test]
fn change_source_is_object_safe_and_uses_model_types() {
    let mut source: Box<dyn ChangeSource> = Box::new(ScriptedSource {
        batches: vec![
            vec![
                Change::Upsert(entry(5, Some(2), "a.txt", EntryKind::File)),
                Change::CheckPath(PathBuf::from("/tmp/b")),
            ],
            vec![
                Change::Remove {
                    key: FileKey::new(VolumeId(1), 5),
                },
                Change::RescanSubtree(PathBuf::from("/tmp")),
                Change::RescanVolume,
            ],
        ],
        last_event: 0,
        started: false,
    });

    let saved = ResumeCursor::FsEvents {
        event_id: 41,
        db_uuid: [7; 16],
    };
    assert_eq!(source.start(Some(&saved)).unwrap(), StartOutcome::Resumed);

    let deadline = Instant::now() + Duration::from_millis(10);
    let first = source.next_batch(deadline).unwrap();
    assert_eq!(first.len(), 2);
    assert!(matches!(first[0], Change::Upsert(ref e) if e.name == "a.txt"));

    let second = source.next_batch(deadline).unwrap();
    assert_eq!(second.len(), 3);
    assert!(matches!(second[2], Change::RescanVolume));

    assert!(source.next_batch(deadline).unwrap().is_empty());
    assert_eq!(
        source.cursor(),
        ResumeCursor::FsEvents {
            event_id: 43,
            db_uuid: [7; 16]
        }
    );
}

#[test]
fn change_source_start_without_cursor() {
    let mut source = ScriptedSource {
        batches: Vec::new(),
        last_event: 0,
        started: false,
    };
    assert_eq!(
        ChangeSource::start(&mut source, None).unwrap(),
        StartOutcome::NoCursorSupport
    );
    assert!(source.started);
    let gap = ResumeCursor::None { snapshot_epoch: 3 };
    assert_eq!(
        ChangeSource::start(&mut source, Some(&gap)).unwrap(),
        StartOutcome::GapDetected
    );
}

/// A permission probe with a fixed answer that counts settings requests.
struct FixedProbe {
    state: AccessState,
    opened: Cell<u32>,
}

impl PermissionProbe for FixedProbe {
    fn full_access(&self) -> AccessState {
        self.state
    }

    fn open_settings(&self) {
        self.opened.set(self.opened.get() + 1);
    }
}

#[test]
fn permission_probe_is_object_safe() {
    for state in [
        AccessState::Granted,
        AccessState::Denied,
        AccessState::Unknown,
    ] {
        let probe = FixedProbe {
            state,
            opened: Cell::new(0),
        };
        let dyn_probe: &dyn PermissionProbe = &probe;
        assert_eq!(dyn_probe.full_access(), state);
        dyn_probe.open_settings();
        assert_eq!(probe.opened.get(), 1);
    }
}

#[test]
fn access_state_variants_are_distinct_values() {
    let granted = AccessState::Granted;
    let copy = granted; // Copy
    assert_eq!(granted, copy);
    assert_ne!(AccessState::Granted, AccessState::Denied);
    assert_ne!(AccessState::Granted, AccessState::Unknown);
    assert_ne!(AccessState::Denied, AccessState::Unknown);
    assert!(!format!("{:?}", AccessState::Denied).is_empty());
}
