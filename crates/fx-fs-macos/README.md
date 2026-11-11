# fx-fs-macos

macOS file-system source for Not-Everything. It holds the volume scanner,
a parallel, read-only directory walker built on `getattrlistbulk(2)` with
the scanner policy (exclusions, mounts, firmlinks, cloud files), the
FSEvents change source (a per-volume change stream with a resume cursor),
and the Full Disk Access probe.

- `src/attrbuf.rs`: pure Rust and builds on every host. It decodes
  `getattrlistbulk` result buffers and holds the descend policy
  (`descend_decision`). Tested by `tests/attr_parse.rs` on any OS.
- `src/scan.rs` (macOS only): the walker, `scan(root, &ScanOptions)`. Tested
  by `tests/walker.rs` against a `std::fs` ground truth on trees written by
  `fx-bench materialize`.
- `src/fsevents/`: the FSEvents change source, described in
  [FSEvents](#fsevents-the-fsevents-module) below. The flag and cursor code
  builds on every host (`tests/fsevents_flags.rs`,
  `tests/fsevents_cursor.rs`); the binding and the stream are macOS only
  (`tests/fsevents_volume.rs`, `tests/fsevents_stream.rs`).
- `src/fda.rs`: the Full Disk Access probe and the non-FDA exclusion set,
  described in [Full Disk Access probe](#full-disk-access-probe-the-fda-module)
  below. Tested by `tests/fda_probe.rs`.
- `examples/scan_bench.rs`: the throughput benchmark described below.
- `examples/fsevents_bench.rs`: the FSEvents latency and replay benchmark,
  described in [Benchmark: `fsevents_bench`](#benchmark-fsevents_bench).

On other operating systems the walker and the FSEvents binding are compiled
out, and the crate builds with only the parser and the pure FSEvents flag
and cursor code. The only third-party dependency is `libc`
(MIT OR Apache-2.0), and it is used only on macOS. The FSEvents binding is
hand-written `extern "C"` declarations against the CoreServices and
CoreFoundation frameworks and libdispatch (from libSystem); no binding crate
is used.

## Walker design

### Attributes requested

Each directory is opened with `O_RDONLY | O_DIRECTORY | O_NOFOLLOW |
O_CLOEXEC` and read with `getattrlistbulk` into a reusable per-worker buffer
(256 KiB by default). The first call on each directory uses a 16 KiB window,
since most directories are small. The walker requests:

| Group  | Attribute                 | Used for                                  |
|--------|---------------------------|-------------------------------------------|
| common | `ATTR_CMN_RETURNED_ATTRS` | which attributes are valid in each record |
| common | `ATTR_CMN_NAME`           | entry name                                |
| common | `ATTR_CMN_ERROR`          | per-entry errors                          |
| common | `ATTR_CMN_OBJTYPE`        | file / directory / symlink / other        |
| common | `ATTR_CMN_MODTIME`        | modification time                         |
| common | `ATTR_CMN_FLAGS`          | BSD flags, including `SF_DATALESS`        |
| common | `ATTR_CMN_FILEID`         | file ID (inode number)                    |
| dir    | `ATTR_DIR_MOUNTSTATUS`    | mount-point detection                     |
| file   | `ATTR_FILE_DATALENGTH`    | file size (data fork length)              |

`FSOPT_PACK_INVAL_ATTRS` keeps the record layout fixed, so the parser can
rely on `ATTR_CMN_RETURNED_ATTRS` to know which fields are valid.

Worker threads share a LIFO queue of directories. With one thread the walk
runs on the calling thread and takes no locks. Paths longer than `PATH_MAX`
are opened one component at a time with `openat`.

### Mount points: why `/System/Volumes/Data` is not scanned twice

On macOS the startup disk has two volumes: the read-only System volume
mounted at `/`, and the Data volume mounted at `/System/Volumes/Data`.
Firmlinks such as `/Users` and `/Applications` make the Data volume's
contents visible under `/`. A naive walk of `/` would list every user file
twice: once through the firmlinks and again under `/System/Volumes/Data`.

The walker lists a mount point below the root as an entry but never enters
it, so a scan of `/` covers the Data volume only through its firmlinks.
Firmlinked directories are not mount points, so they are scanned normally.
Mount points are recognized in two ways:

1. `ATTR_DIR_MOUNTSTATUS` with `DIR_MNTSTATUS_MNTPOINT` set, and
2. the kernel's mount table (`getfsstat`), read once at scan start. This is
   needed because APFS's `getattrlistbulk` reports mount status 0 for some
   mount points, `/System/Volumes/Data` among them.

The same rule keeps other volumes (`/Volumes/...`, disk images, `/dev`) out
of a scan of `/`. Skipped mount points are counted in
`ScanStats::skipped_mount_points`. The scan root itself may be a mount
point; only mount points strictly below it are skipped.

### Symlinks

Symlinks are reported as entries and never followed: the descend policy
never enters them, and every directory is opened with `O_NOFOLLOW`.

### Dataless (cloud-only) files and folders

- At scan start the process's dataless-file materialization policy is
  switched off with `setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
  IOPOL_SCOPE_PROCESS, IOPOL_MATERIALIZE_DATALESS_FILES_OFF)`, so reading
  metadata can never start a download from iCloud Drive or a File Provider.
  The policy stays off for the rest of the process.
- Directories with `SF_DATALESS` in their flags are listed but never
  entered, because listing their children would materialize them. They are
  counted in `ScanStats::skipped_dataless`.

### Error handling

Only a failure to open or read the scan root returns `Err`. Below the root,
a directory that cannot be opened or read (`EACCES`, `EPERM` for
TCC-protected folders without Full Disk Access, `EDEADLK` for dataless
items under the materialization policy, `ENOENT` when the directory
disappears during the scan, and so on) adds 1 to `ScanStats::errors`, and
the walk continues. Entries that come back with `ATTR_CMN_ERROR` set, or
records that cannot be decoded, are counted the same way.

### Read-only guarantee

The walker only opens directories for reading and lists them. It never
creates, writes, deletes, renames or moves anything, and it never triggers
cloud downloads. `tests/walker.rs` checks that a scan leaves the tree
unchanged.

### Scanner policy

- **Exclusions**: `ScanOptions::exclusions` (`fx_fs::Exclusions`, default
  excludes nothing) is checked for every entry before descending, with the
  absolute path. `SkipSubtree` directories are never opened; `SkipEntry`
  entries are not emitted; traverse-only ancestors of include-only roots are
  walked but not emitted. *Hidden* = name starts with `.` or `UF_HIDDEN`;
  *system* = `SF_RESTRICTED` or `/System` and everything under it.
- **Mounts**: a mount point below the root (by `ATTR_DIR_MOUNTSTATUS` or the
  `getfsstat` table) is entered only if `volumes::mount_policy` says
  `Index`, i.e. only the scan root's volume in the MVP.
- **Firmlinks**: `/System/Volumes/Data` is never entered from above, so `/`
  yields each file once (`skipped_firmlink_aliases`, or
  `skipped_mount_points` when the mount table lists it).
- **Cloud**: materialization is switched off at scan start and read back
  into `ScanStats::materialization_disabled`; `SF_DATALESS` folders are
  listed, never entered; `EDEADLK`/`EACCES`/`EPERM` on open are counted
  and skipped, never retried.
- **`MacScanner`** (`enumerator` module) implements
  `fx_fs::VolumeEnumerator`, emitting `fx_model::Entry` (creation time via
  `ATTR_CMN_CRTIME`) in parent-before-child order. FIFOs, sockets and
  devices are reported as `File` (fx-model has no kind for them).
- Gate check: `cargo run --release -p fx-fs-macos --example scan_bench -- /
  --check-duplicates` (also the ignored test in `tests/scanner_policy.rs`).
- Out of scope: external volumes (Phase 2 of `docs/PLAN.md`) and network
  drives (Phase 6).

## Full Disk Access probe: the `fda` module

- **`FdaProbe`** (macOS only) implements `fx_fs::PermissionProbe`.
  `full_access()` opens, in order, `~/Library/Application
  Support/com.apple.TCC/TCC.db` and `/Library/Application
  Support/com.apple.TCC/TCC.db` with `O_RDONLY | O_NOFOLLOW | O_NONBLOCK |
  O_CLOEXEC` and closes them at once; contents are never read. `Granted` if
  any open succeeds, `Denied` if one fails with `EPERM`/`EACCES` and none
  succeeds, `Unknown` otherwise (missing files, `ENOTDIR`, `ELOOP` for a
  symlink, ...). `FdaProbe::with_paths` swaps in other files for tests.
  The probe never opens anything in a consent-prompt location, so it never
  shows a TCC dialog.
- **`open_settings()`** runs `/usr/bin/open` on `FDA_SETTINGS_URL`
  (`x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`)
  on a helper thread. `open_settings_command()` builds the command without
  running it.
- **Non-FDA mode**: `consent_prompt_paths(home)` lists the folders to skip
  when access is not `Granted`: `~/Desktop`, `~/Documents`, `~/Downloads`,
  `~/Library/Mobile Documents`, `~/Library/CloudStorage`,
  `~/Pictures/Photos Library.photoslibrary`, `~/Library/Mail`,
  `~/Library/Messages`, `~/Library/Safari`, `~/Library/Containers`,
  `~/Library/Group Containers`, the user and system TCC folders, and
  `/Volumes`. `consent_prompt_exclusions(home)` puts that list in
  `fx_fs::ExclusionConfig::excluded_folders` (nothing else set), for
  `fx_fs::Exclusions::new` and `ScanOptions::exclusions`. Both are pure and
  build on every host.

## Benchmark: `scan_bench`

```sh
cargo run --release -p fx-fs-macos --example scan_bench -- \
    --root / --threads 1,2,4,6,8 --iterations 5 --warmup 1 --cache warm
```

| Flag           | Default      | Meaning                                         |
|----------------|--------------|-------------------------------------------------|
| `--root`       | `/`          | directory to scan (a symlink is rejected)       |
| `--threads`    | `1,2,4,6,8`  | comma-separated worker counts, each 1..=256     |
| `--iterations` | `5`          | timed scans per thread count                    |
| `--warmup`     | `1`          | untimed scans per thread count first            |
| `--cache`      | `unknown`    | `warm`, `cold` or `unknown`: a label only       |
| `--json`       | off          | print one JSON object instead of text           |

For each thread count it runs `scan()` with `collect = false` through the
`fx-bench` harness. It reports the entries, directories, errors, skipped
mount points and skipped dataless directories from the last run, then the
elapsed min, p50 and p95, entries/s at p50, and the speedup over 1 thread.
It also prints the machine (OS, architecture, logical CPUs, CPU brand) and
the cache label. Then it prints one PASS/FAIL line per gate:

- at least 180,000 entries/s on 1 thread (SKIP if 1 is not in
  `--threads`);
- whole-root scan in at most 10 s at the best thread count (the one with
  the lowest p50). This gate is checked only when `--root` is `/`.

Gate results don't change the exit code. The exit code is 0 after a
completed run, 1 if the scan fails, and 2 for bad arguments. On hosts other
than macOS the example prints `scan_bench requires macOS` and exits with 2.

Progress lines go to stderr, so `--json` output on stdout can be piped as
is.

### Warm cache

Run it as shown above. `--warmup 1` makes one untimed scan per thread
count first, so the timed runs see a warm cache. Label the run
`--cache warm`.

### Cold cache

The `--cache` flag only labels the report; it does not flush anything.
For a cold run:

1. Reboot (the most reliable way), or run `sudo purge` to flush the disk
   cache. `purge` may not evict all cached file-system metadata, so say in
   the recorded numbers which method you used.
2. Run once with a single thread count and no warmup, since every scan
   after the first is warm again:

   ```sh
   cargo run --release -p fx-fs-macos --example scan_bench -- \
       --root / --threads 8 --iterations 1 --warmup 0 --cache cold
   ```

   Repeat steps 1 and 2 for each thread count you want to measure. Build
   the example first (`cargo build --release -p fx-fs-macos --example
   scan_bench`) so the build itself doesn't warm the cache unevenly.

### Full Disk Access

Without Full Disk Access, macOS privacy protection (TCC) refuses to list
folders such as `~/Library/Mail`, `~/Library/Messages`, other users' home
folders and parts of `/Library` and `/private/var`. Those directories are
counted as errors and their contents are missing, so a scan of `/` covers
fewer entries and finishes faster than a scan with access. FDA is granted
to the app that launches the benchmark (the terminal), under
System Settings > Privacy & Security > Full Disk Access. When you record
numbers, always note whether FDA was on, the machine, and whether the
cache was warm or cold. The benchmark prints a note whenever any
directory could not be read.

To benchmark a reproducible tree instead of `/`, write one with `fx-bench`
into an empty directory and scan that:

```sh
mkdir /tmp/fx-tree
cargo run --release -p fx-bench -- materialize --count 200000 --seed 7 --out /tmp/fx-tree
cargo run --release -p fx-fs-macos --example scan_bench -- --root /tmp/fx-tree --cache warm
```

## FSEvents: the `fsevents` module

The resume cursor is the shared `fx_model::ResumeCursor::FsEvents`, and
`FsEventsSource` implements the shared `fx_fs::ChangeSource`; the other
types below are local to this crate, and `fx-model` does not depend on
them. The binding was written only from Apple's FSEvents documentation
and the SDK header `FSEvents.h`.

### API

| Item | OS | What it is |
|------|----|------------|
| `flags::EventFlags` | all | the raw `FSEventStreamEventFlags` word, with one constant per `kFSEventStreamEventFlag*` bit, plus `contains` / `intersects` / `bits` |
| `flags::classify(path, flags, scope_root) -> EventAction` | all | pure per-event decision: `HistoryDone`, `RescanVolume`, `RescanSubtree(path)`, `CheckPath(path)` or `Ignore`; `scope_root` is `VolumeStream::scope_root()` |
| `flags::admit(path, flags, filter) -> bool` | all | the stream's `path_filter` decision: what a filtered stream delivers |
| `cursor::FsEventsCursor { event_id, db_uuid }` | all | a named-field view of `fx_model::ResumeCursor::FsEvents` (`From` / `TryFrom`); `to_bytes` / `from_bytes` with a fixed 24-byte layout |
| `cursor::decide_start(saved, current_uuid, current_event_id)` | all | pure gap detection over a saved `ResumeCursor`; returns an `fx_model::StartOutcome` and the `sinceWhen` ID |
| `cursor::CursorTracker` | all | pure bookkeeping behind `VolumeStream::cursor()`: `since_now` / `replay`, `on_raw_event` (any event, by its flags), `on_event`, `on_history_done`, `on_ids_wrapped`, `cursor_id`, `history_done`, `dropped_before_cursor` |
| `volume::current_event_id()` | macOS | `FSEventsGetCurrentEventId` |
| `volume::volume_uuid(dev)` | macOS | `FSEventsCopyUUIDForDevice`, as 16 bytes, or `None` |
| `volume::device_of(path)` | macOS | `st_dev` of a path |
| `volume::last_event_id_before(dev, unix_secs)` | macOS | `FSEventsGetLastEventIdForDeviceBeforeTime` |
| `stream::StreamConfig` | macOS | `watch_path` (selects the volume), `path_filter` (optional absolute prefix), `latency` (default 10 ms), `no_defer` |
| `stream::VolumeStream::start(config, from)` | macOS | starts a stream and returns it with the `StartOutcome` |
| `VolumeStream::{recv_timeout, try_iter}` | macOS | receive `RawEvent { path, flags, id, received }` |
| `VolumeStream::{cursor, history_done, flush_sync, stop}` | macOS | resume point, replay state, flush, and teardown (`Drop` also stops) |
| `VolumeStream::mount_point()` | macOS | the watched volume's mount path in the stream's normalised form (`/`, `/Volumes/X`, `/System/Volumes/Data`) |
| `VolumeStream::scope_root()` | macOS | the root of what the stream delivers: its `path_filter`, or `mount_point()` without one; pass it to `classify` as `scope_root` |
| `stream::normalise_firmlinks(path)` | macOS | maps `/System/Volumes/Data/<firmlink>/...` back to `/<firmlink>/...` |

`VolumeStream::start` creates one stream for the whole volume holding
`watch_path`, using `FSEventStreamCreateRelativeToDevice` with
`kFSEventStreamCreateFlagFileEvents` (file-level events). The stream runs on
its own serial dispatch queue (`FSEventStreamSetDispatchQueue`), so no run
loop is needed on the caller's side. The C callback never unwinds. It turns
each event into a `RawEvent` and sends it over an `std::sync::mpsc`
channel; `received` is the `Instant` at which the callback ran. `stop`
stops and invalidates the stream, waits on the queue until any running
callback has finished, then releases the stream and the queue.

### Cursor semantics

- A cursor is the last delivered event ID **plus** the UUID of the
  volume's FSEvents database (`FSEventsCopyUUIDForDevice`). An event ID
  means nothing without the database it came from.
- Encoding: bytes 0..8 are the event ID in little-endian order; bytes 8..24
  are the UUID as is. Any other length is rejected.
- `VolumeStream::cursor()` is always a safe resume point. On a resumed
  stream it stays at the replay start (the saved cursor's event ID) until
  the HistoryDone marker arrives, because replay comes in path order, not
  ID order: a high replayed ID does not mean the lower ones were delivered.
  The marker moves it to the highest delivered ID or the marker's ID,
  whichever is larger, and each delivered live event advances it from
  there. A stream that starts from now starts at the current event ID. The
  cursor and `history_done()` change together under one lock, so a cursor
  read before `history_done()` returns false was taken before HistoryDone.
  It is never the `kFSEventStreamEventIdSinceNow` sentinel, and it stays
  readable after `stop` (a stop before HistoryDone leaves it at the replay
  start). Events removed by `path_filter` do not move it. The bookkeeping
  is the pure `CursorTracker`, unit-tested in `tests/fsevents_cursor.rs`.
- On resume (`StartOutcome::Resumed`), FSEvents replays the history after
  the saved ID, then sends the HistoryDone marker (reported with the mount
  point as its path), then live events. The stream drops any item event
  whose ID is at or below the saved one, so a replay never repeats the
  past. Special events (a rescan request, a mount or unmount, RootChanged,
  whose ID `FSEvents.h` gives as zero) are always delivered.
- `EventIdsWrapped`: every earlier ID is meaningless. The event rescans the
  volume; the tracker stops dropping events at or below the replay start
  (every later ID is below it), ends the replay, and moves the cursor to
  the wrap event's ID.
- Resuming from an older ID than necessary is safe: it only repeats
  events, and every action is idempotent (`CheckPath` re-stats).

### Gap rules (`decide_start`)

| Saved cursor | Volume now | Outcome | Stream starts |
|--------------|------------|---------|---------------|
| none | any | `NoCursorSupport` | now |
| not an FSEvents cursor (`Usn`, `None`) | any | `GapDetected` | now |
| any | no FSEvents UUID (for example a read-only volume) | `GapDetected` | now |
| UUID differs (the volume's FSEvents store was recreated, or the cursor is another volume's) | any | `GapDetected` | now |
| same UUID, saved ID > current ID (the FSEvents store was reset, or the cursor was restored from a backup) | any | `GapDetected` | now |
| same UUID, saved ID <= current ID | any | `Resumed` | from the saved ID |

After a gap the caller must rescan the volume. The new stream starts from
now, and its cursor carries the volume's real UUID.

### Flag-to-action table (`classify`)

`classify(path, flags, scope_root)` takes the root of what the stream
delivers as `scope_root`; pass `VolumeStream::scope_root()` (the path
filter, or the volume's mount point without one), which is in the same
normalised form as the event paths. Priority is from top to bottom; the
first matching row wins. Paths are compared component-wise.

| Flags on the event | Action |
|--------------------|--------|
| `HistoryDone` | `HistoryDone`: replay finished; live events follow |
| `EventIdsWrapped`, `RootChanged`, `UserDropped` or `KernelDropped`, on any path | `RescanVolume` |
| `Unmount` of the scope root (a trailing slash does not matter), or of `""`, `"/"` or `"."` | `RescanVolume` |
| on a folder above the scope root: `MustScanSubDirs`, `Mount`, `Unmount`, `ItemCreated`, `ItemRemoved`, `ItemRenamed`, or no item bit | `RescanSubtree(scope_root)` |
| on a folder above the scope root: anything else (metadata) | `Ignore` |
| `Mount` on the scope root | `RescanSubtree(scope_root)` |
| `MustScanSubDirs` | `RescanSubtree(path)` |
| no item bit and no special bit (`kFSEventStreamEventFlagNone`, possibly with `OwnEvent` or unknown bits) | `RescanSubtree(path)` |
| any other `Mount` or `Unmount` (below the scope root) | `CheckPath(path)` |
| any `Item*` flag (`Created`, `Removed`, `Renamed`, `Modified`, `InodeMetaMod`, `FinderInfoMod`, `ChangeOwner`, `XattrMod`, `Cloned`, type and hard-link flags), with or without `OwnEvent` | `CheckPath(path)` |

Why the rescans (`FSEvents.h`): with `UserDropped` or `KernelDropped` "the
client must do a full scan of any directories (and their subdirectories,
recursively) being monitored by this stream"; `MustScanSubDirs` comes from
hierarchical coalescing, so it can name a folder above the scope root (the
header's example coalesces events in a user's `Music` and `Pictures`
folders into one on the user's home folder); `kFSEventStreamEventFlagNone`
means "some change in the directory", which only a listing shows. A folder
above the scope root that is created, removed or renamed has moved the
indexed root, which is what a `WatchRoot` stream would report as
`RootChanged`.

Item flags are only hints. The kernel coalesces events, so one event can
carry `Created | Renamed | Removed` for a path that no longer exists. The
safe reaction is always to stat the path again. The two sides of a rename
are separate, unpaired events, and each is checked on its own.

During replay only, the stream sets `RawEvent::rescan_subtree` on every
directory rename (`ItemRenamed | ItemIsDir`). The history store does not
replay events recorded below a directory that was renamed later; only the
directory's own rename (old and new name) comes back. So the subtree below
both names must be rescanned. `RawEvent::flags` is always the exact kernel
word. The
offline-replay test and `fsevents_bench replay` both print how many
scripted paths were reported directly and how many were covered only by
such an added rescan, so the strength of the replay gate is visible.

### Observations: firmlinks and device-relative paths

These were observed on macOS on Apple silicon (M4 Pro):

- `std::env::temp_dir()` (`/var/folders/...`, which is really
  `/private/var/folders/...`) is on the Data volume. `statfs` reports its
  mount point as `/System/Volumes/Data`.
- A device-relative stream reports paths relative to the volume root, for
  example `private/var/folders/...`. The stream joins them to the mount
  point (`/System/Volumes/Data/private/var/...`) and then maps them back to
  the firmlinked form (`/private/var/...`). The mapping uses the table in
  `/usr/share/firmlinks`, with a built-in copy of the standard entries as a
  fallback. Data-volume paths that are not below a firmlink keep their
  `/System/Volumes/Data/...` form.
- File-level events work with device-relative streams. If
  `FSEventStreamCreateRelativeToDevice` ever returns null, the stream falls
  back to `FSEventStreamCreate` on the mount point. That stream reports
  absolute paths, which get the same normalisation.
- Device-relative event IDs are compared with the system-wide
  `FSEventsGetCurrentEventId`. In every test the saved IDs were below it,
  and resumption worked.
- Replayed history is coalesced per path: `replay --ops 10000` (5,269
  distinct paths) delivered 5,270 events, one per path plus one for the
  bench directory itself. They come roughly in path order, not ID order.
- Live latency is bounded below by the stream's coalescing `latency`
  (10 ms). With `no_defer` on, only the first event after a quiet period is
  sent at once. The bench's back-to-back operations keep the stream busy,
  so they measure the deferred case: about 1.5 ms over the configured
  latency.

### Rescans and gaps

`FsEventsSource` turns every event into an `fx_model::Change` (see
`src/fsevents/change.rs`), and the engine applies it:

- `MustScanSubDirs`, a flag word with no item bit, and every directory
  rename (including the replay-time renames above): rescan the subtree.
- `EventIdsWrapped`, `RootChanged`, `UserDropped`, `KernelDropped` and
  `Unmount` of the scope root: rescan the volume. `Mount` of another volume
  below the scope root is only a path check: external volumes are out of
  scope (see the scanner policy above).
- An event on a folder above the scope root that rescans, mounts over,
  creates, removes or renames it: rescan the scope root. A metadata change
  there changes nothing indexed.
- A gap at start (see the gap rules): the engine rescans the volume and
  streams from now.
- The cursor saved with a checkpoint stays at the replay start until the
  HistoryDone marker, because replay arrives in path order, not ID order.
- A `path_filter` passes the events above it that rescan it, and the
  volume-level flags on any path (`flags::admit`), so a rescan reported on
  a folder above the indexed root, or the root moving away and back, is
  seen. The app filters its stream to the indexed root, `/` by default.
- Every route is tested end to end in `fx-engine`'s `tests/macos_e2e.rs`:
  the real stream is muted while files change, the test checks that the
  index missed the changes, then injects the one raw event FSEvents would
  send, routed as `FsEventsSource` routes it; the index must then equal a
  fresh scan. FSEvents cannot be made to send these flags on demand.

## Benchmark: `fsevents_bench`

```sh
cargo run --release -p fx-fs-macos --example fsevents_bench -- latency --ops 1000 --cache warm
cargo run --release -p fx-fs-macos --example fsevents_bench -- replay --ops 10000
```

| Mode / flag | Default   | Meaning |
|-------------|-----------|---------|
| `latency`   |           | live latency of N scripted operations |
| `replay`    |           | offline replay of N scripted changes from a saved cursor |
| `--ops N`   | `1000`    | number of operations, 1..=1,000,000 |
| `--cache`   | `unknown` | `warm`, `cold` or `unknown`; a label only, `latency` mode only |

**`latency`** creates a fresh directory under `std::env::temp_dir()` and
starts a `VolumeStream` filtered to it (10 ms latency, `no_defer` on). It
then performs N operations in rotation: create `f<k>.txt`, rename it to
`r<k>.txt`, delete `r<k>.txt`. Each sample runs from just before the
syscall to the `received` time of the matching event: `ItemCreated` on the
new file, `ItemRenamed` on the new name, `ItemRemoved` on the deleted
path. The bench waits for each event before the next operation, and an
operation with no event within 5 s counts as missed. It prints p50, p95,
p99 and max over all operations and per operation kind (nearest rank, from
`fx_bench::Stats`), the machine and the cache label, then
`GATE live p95 <= 50 ms: PASS/FAIL`. Any missed operation fails the gate.

**`replay`** starts a stream on a fresh directory, flushes it, saves its
cursor as bytes and stops it. It then applies N offline changes, all
counted as operations: bucket directories of 250 files, where each file is
created, modified, renamed, and (four times out of five) removed again. It
waits 500 ms, resumes from the decoded cursor and collects everything up to
the HistoryDone marker. It prints the time from `start` to the marker, the
number of events, distinct paths and subtree rescans, and
`GATE replay exact: PASS/FAIL`. The gate passes when the start outcome is
`Resumed`, the marker arrives, every scripted path is reported (or covered
by a rescan of an ancestor, which is counted separately), nothing outside
the scripted paths, their ancestors and the bench directory is reported,
and no event has an ID at or below the saved cursor.

Exit codes: 0 after a completed run (gate results don't change it), 1 if
the stream or a file operation fails, and 2 for bad arguments. On hosts
other than macOS the example prints a message and exits with 0.

The bench creates, renames and removes entries only inside its own temp
directory, and removes that directory at the end. For a cold run, label
it `--cache cold` after a reboot. FSEvents latency depends little on the
disk cache, but the label keeps the records comparable with the other
benchmarks.
