# fx-store

On-disk persistence of the index: the checkpoint file format, checksums,
the atomic checkpoint writer and the zero-copy mmap loader.

The checkpoint format is **version 2**; version 1 was an early
prototype's, which is no longer read. It is designed from `docs/PLAN.md` only
("Saving and loading", "Data layout", and the `ResumeCursor` in "Proposed
architecture"). `src/layout.rs` documents every byte and holds the
constants.

## What a checkpoint holds

- A whole `fx_index` snapshot as `fx_index::IndexParts`: name bytes and
  offsets, parents, volumes, file IDs, packed metadata, modified and
  created times, extension IDs, the extension table and the live bitmap.
  Every per-entry column has one element per entry ID, removed ones
  included, so entry IDs are the same after a reload.
- One `fx_model::ResumeCursor` per volume (FSEvents, USN or none).
- A clean-shutdown flag.
- Zero or more saved orders: `u32` permutations of the live entry IDs,
  each tagged with a `u8` key code that the engine maps to
  `fx_sort::SortKey` (fx-store does not depend on fx-sort).

### Saved order contract

A saved order holds exactly `live_count` elements, every element is a live
entry ID below `id_bound`, and no element repeats: a permutation of the
live IDs, as `fx_sort::Order` holds. `write_checkpoint` rejects a
violation with `StoreError::InvalidOrder { key, problem }`, where
`OrderProblem` is `Length`, `OutOfRange`, `DeadId` or `DuplicateId`.
`MappedIndex::open` checks only the length (O(1)) and reports a mismatch
as `LoadError::Malformed`; `verify_checksums` also checks every element's
range, liveness and uniqueness (also `Malformed`). So after a header-only
open, `MappedIndex::order()` guarantees only the length.

## Format (version 2)

Everything is little-endian; the writer and the loader both refuse
big-endian hosts. A file is a header followed by the columns.

The header has three parts, all covered by one header checksum:

1. A fixed 72-byte part: magic `FXSTORE1`, `u32` version at offset 8 (the
   same place as in the version 1 prototype, so its files are reported as
   an unsupported version), an endianness marker, the header length, flags
   (bit 0 = clean shutdown), `id_bound`, live count, a creation timestamp
   (microseconds since the epoch), the volume and column record counts,
   the total file length and the header checksum.
2. The volume table: 32 bytes per volume, holding the volume ID, a cursor
   tag and the cursor fields (FsEvents `event_id` and `db_uuid`, Usn
   `journal_id` and `next_usn`, or None `snapshot_epoch`).
3. The column table: 32 bytes per column, holding the `u16` kind, the `u8`
   order key code, the `u8` element size, the byte offset, the byte length
   and the column checksum. The 12 `IndexParts` columns come first, in kind
   order, then the saved orders in save order.

Every column starts on a 64-byte boundary with zero padding in between.
Because the mapping is page-aligned, the columns can be used in place as
slices through `bytemuck`. The name bytes are always followed by one zero
pad byte, so `MappedIndex::name_bytes_nul` can hand the mapped names to
`fx_query::substring::NameColumn` without a copy.

### Checksum policy

The checksum (`layout::checksum`) is a std-only, word-at-a-time 64-bit
hash derived from the public-domain FNV-1a algorithm (Fowler/Noll/Vo,
<http://www.isthe.com/chongo/tech/comp/fnv/>), with its 64-bit offset
basis and prime. It reads little-endian `u64` words round-robin into four
independent lanes (lane `i` starts from the offset basis XOR `i`; each
step is `h = rotl((h ^ word) * prime, 31)`; the rotation feeds high bits
back down so that two bit-63 flips cannot cancel), folds the zero-padded
tail and the total length into lane 0, combines the four lanes with the
same step, and finishes with the public-domain SplitMix64 finaliser
(Steele, Lea, Flood, "Fast Splittable Pseudorandom Number Generators",
2014). It is not compatible with FNV-1a over bytes and is for corruption
detection only: torn writes, bit rot and truncation. It is not
cryptographic and not tamper-proof.

- `MappedIndex::open` checks, in order, the magic, the version, the
  endianness marker, the header length, the header checksum, the file
  length, the volume table and every column record: known kind, matching
  element size, 64-byte alignment, inside the file, no overlap, every
  required column present once, lengths consistent with `id_bound` (saved
  orders: with the live count). This costs O(1) in the data size, so
  loading stays fast.
- `MappedIndex::verify_checksums` checks every column's checksum, the
  live count and the saved order contract. It reads the whole file, so it isn't on the
  load-to-first-result path. Callers can run it in the background or after
  an unclean shutdown.
- `MappedIndex::to_index` copies the columns into `Index::from_parts`,
  which validates their contents.

Rejections are typed (`LoadError`): `NotFound`, `BadMagic`,
`UnsupportedVersion` (version 1 files land here), `WrongEndianness`,
`Truncated`, `HeaderChecksum`, `ColumnChecksum` (naming the column),
`Malformed` (with a reason) and `Io`. `open_or_rebuild` (below) then
rebuilds the index from a scan and writes a fresh checkpoint.

### Atomic checkpoint

`write_checkpoint(path, &SaveInput { parts, volumes, orders,
clean_shutdown })` validates the input, writes the file to a uniquely named
hidden temp file in the target's directory (`.<name>.tmp.<pid>.<n>`, see
`temp_name_prefix`), calls `File::sync_all` (which issues `F_FULLFSYNC` on
macOS), renames the temp file over the target, and then syncs the
directory. A reader sees either the old file or the new one, never a torn
file, and an existing mapping of the old file stays valid. On error the
temp file is removed and the old file is left untouched.

### Private files and folders

A checkpoint holds the names of every indexed file, including names in
folders that other programs may not list. On Unix the temp file is
created with mode 0600, so the checkpoint renamed from it is 0600 too,
whatever mode an older file at that path had. `create_private_dir(dir)`
creates a missing store folder (and any missing parents) with mode 0700.
It never changes a folder that already exists: the store can be pointed
at any folder, such as the home folder or a shared `/tmp`, and a folder
that belongs to someone else can't be changed anyway.
`open_or_rebuild` and the engine's saves call it before writing.

`make_dir_private(dir)` creates `dir` the same way and also sets an
existing `dir` to 0700. It is for a caller's own folders: the app calls
it at launch on its data folder and its stores folder, so a folder an
older version left readable is tightened. The store never calls it.
`PRIVATE_DIR_MODE` (0700) and `PRIVATE_FILE_MODE` (0600) are the two
modes.

## Startup: open or rebuild

`open_or_rebuild(path, verify, rebuild)` (module `checkpoint`) is the
startup policy from the plan's startup flow:

1. It removes this checkpoint's own temp files of interrupted writes
   (`cleanup_stale_temps(path)`, best effort).
2. It opens the file with `MappedIndex::open`. With `VerifyMode::Full` it
   also runs `verify_checksums`; `VerifyMode::HeaderOnly` doesn't read
   the columns.
3. A valid file is returned as `Opened::Loaded(MappedIndex)`, whether or
   not it was written by a clean shutdown. `Opened::clean_shutdown()`
   reports the flag, and `false` tells the engine to replay changes from
   the saved resume cursors.
4. A missing file, an old format version (version 1 included) or any
   corruption the loader detects (bad magic, truncation, header or column
   checksum, malformed tables) calls `rebuild` exactly once. The returned
   `RebuiltIndex { parts, volumes, orders, clean_shutdown }` is written
   atomically with `write_checkpoint` (after `create_private_dir` creates
   a missing parent directory private), reopened with the same verification,
   and returned as `Opened::Rebuilt { index, reason }`. `reason` is the
   `RejectReason` (the loader's `LoadError`), so the caller can log why.

Errors are not silently rebuilt. An I/O error other than "not found" is
returned as `StoreError::Load` without calling `rebuild`. A failed rebuild
is returned as `StoreError::Rebuild` carrying the caller's error, and the
old file stays in place.

`cleanup_stale_temps(target)` is scoped to one checkpoint. In the
target's directory it removes only regular files named like the writer's
temp files for that target: `temp_name_prefix(target)` (`.<name>.tmp.`)
followed by `<pid>.<counter>` in decimal. Temp files of other checkpoints
in the same directory, other files, directories and symlinks are kept.
A temp file whose process no longer runs (`kill(pid, 0)` fails with
`ESRCH`, on Unix) is removed at once: its writer was killed, crashed or
quit during a checkpoint, so the next start cleans up after it. Temp files
that may belong to a running writer are skipped: those that carry the
current process's ID, and, while a process with their ID runs, those
modified less than `STALE_TEMP_MIN_AGE` (10 minutes) ago, or with an
unreadable or future modification time. A checkpoint write takes seconds,
so a younger temp is assumed to be in progress; a process ID reused by an
unrelated process only delays the removal to a later start. It returns how
many files it removed.

## Benchmark: load to first result

The gate is that a 5M-entry checkpoint loads to its first search result in
≤ 50 ms (p95) with a warm cache. Release mode is required; debug builds
are many times slower and their numbers mean nothing:

```text
cargo run --release -p fx-store --example load_bench -- --entries 5000000 --cache warm
cargo run -p fx-store --example load_bench -- --quick    # debug smoke run only
```

The example generates the entries with `fx_bench::generate_tree`, puts
them in a synthetic `fx_index::Index` and writes the checkpoint with one
volume cursor and one saved order, reporting file size and bytes per
entry. It then times these stages with the `fx-bench` harness and reports
p50/p95/p99/max for each:

- `write`: `write_checkpoint` (temp file, `sync_all`/`F_FULLFSYNC`,
  rename, directory sync), over `min(--iters, 5)` repeated writes;
- `open (header)`: `MappedIndex::open`, the `VerifyMode::HeaderOnly`
  cost;
- `open+full verify`: plus `verify_checksums`, the `VerifyMode::Full`
  cost;
- `open+to_index`: `open` plus `MappedIndex::to_index`;
- `open+column`: `open` plus `fx_query::substring::NameColumn::new` over
  the mapped names and offsets;
- `load to first result` (mmap to first result): `open+column` plus an
  `fx-query` substring search for `--query`, taking the first (smallest)
  matching ID. The search has no early-exit mode, so this times a full
  scan, which is an upper bound.

Full verify and `to_index` read the whole file and aren't on the gate
path. The output ends with the machine, the cache label (with a note when
the cache is warm) and the line
`GATE load-to-first-result p95 <= 50 ms: PASS|FAIL (<value> ms)`.

Options: `--entries` or `--count` (default 5000000), `--seed` (1),
`--iters` (20), `--query` (`conf`), `--quick` (50000 entries and 3 samples
unless given explicitly; for debug builds), `--dir` (default: a fresh temp
directory), `--cache warm|cold|unknown` (label only; default `warm` for a
freshly written image and `unknown` with `--reuse`), `--keep` (keep
`index.fxs` afterwards) and `--reuse` (with `--dir`: time an existing
`index.fxs` instead of writing a new one, skipping the write stage; a
reused file is never deleted). Warm runs do two untimed warm-up samples
per read stage; cold and unknown runs do none.

The 2M- and 5M-entry release numbers are not recorded yet; the Phase 0
and Phase 1 gate scripts collect them (see `docs/gates/`).

`--cache` is only a label: the benchmark can't drop caches by itself.
Cold-cache numbers need `sudo purge` or a reboot before each run. Writing
the checkpoint puts it in the page cache, so write it first, purge, and
then time the existing file. Use `--iters 1`, because after the first
sample the file is back in the page cache:

```text
cargo run --release -p fx-store --example load_bench -- --dir /tmp/fx-load --keep
sudo purge
cargo run --release -p fx-store --example load_bench -- --dir /tmp/fx-load --reuse --cache cold --iters 1
```

Record the machine and whether the cache was cold or warm with every
number.
