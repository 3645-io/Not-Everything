# Plan: a reconnected drive back in search quickly

> **Later, not scheduled** (owner, 14 Nov 2026). Run 2.19 in `docs/PLAN.md`.
> The limits below are proposals. Before the run is planned in detail, each
> one must be checked against what the hardware and macOS actually allow, and
> against the app's existing requirements on CPU, memory and disk writes (SSD
> wear), which meeting it must not break.

## What happens today (0.5.0-RC-1)

Measured on 14 Nov 2026 on the M4 Pro (macOS 27.2.1, AC power) with an exFAT
USB SSD (10 Gbps link, 878,443 entries) that the app had indexed before. The
drive was ejected, a search was left open in the window, and the drive was
unplugged and plugged in again. Times come from the system log (kernel, disk
arbitration, Finder), AppKit's mount notifications, and the window's result
list read through the Accessibility API every 100 ms.

| Step | Time |
| --- | --- |
| Plugged in → the kernel lists the USB device | 0.57 s |
| → the volume is mounted and Finder shows it | 2.48 s |
| → the drive's files are back in the app's results | 25.97 s |
| Plugged in → results, in all | 29.0 s |

- For those 26 s the status line read "Up to date" and the window didn't
  change at all. No new snapshot was published: the object count, which had
  moved every second or two before, stood still.
- `scan_bench` walked the same drive in 15.9 s on 6 threads and 23.3 s on
  1 thread, with its listing already cached: about 55,000 entries a second,
  against about 180,000 (1 thread) to 295,000 (4–6 threads) on the internal
  APFS disk. Right after a reconnect nothing is cached, so the app's walk
  is slower than these.

Why, in the code at 0.5.0-RC-1:

- External volumes have no resume cursor, so every mount becomes a
  `Change::RescanSubtree` of the mount point
  (`crates/fx-fs-macos/src/fsevents/volumes_source.rs`). FAT and exFAT keep
  no FSEvents history, so for them a walk can't be avoided. APFS and HFS+
  drives are walked too.
- The writer handles that rescan inside one batch (`Writer::rescan_subtree`).
  The walk streams every entry into the index as a new entry. The drive's old
  entries, kept offline since the unmount and re-keyed onto an offline volume
  ID, are dropped at the end of the batch (`drop_replaced_offline`), and one
  snapshot is published at the end. Nothing is published in between, so
  changes on other volumes probably wait too (step 1 checks this).
- An offline volume is matched to a newly mounted one by its name alone.

## Proposed limits (to verify)

| ID | Measures | Proposed limit | 0.5.0-RC-1 |
| --- | --- | --- | --- |
| F7 | A drive indexed before: its files back in the results after macOS mounts it | ≤2 s, as search usable after a relaunch (F6u) | 26 s |
| F8 | That drive's index equals a fresh scan of it | ≤1.2× a bare walk of the same drive; tracked, since it depends on the drive | 26 s, against a bare walk of 16–23 s with the listing cached |
| F1 during a reconnect | A new file on another volume shown while a drive is re-read | ≤1 s, F1's own limit | probably about 26 s (step 1) |
| – | The status line says a drive is being indexed while it is | Pass or fail | "Up to date" |

## What the limits must be checked against

Each limit is checked for what is actually possible, and meeting it must not
break an existing requirement. The targets are in `docs/gates/performance.md`.

- **CPU and energy.**
  - A walk uses up to 8 threads. The app's budgets for normal running are B3
    (busiest 10 s ≤100% of a core) and, with the window hidden, B1 (≤1% of a
    core), B4 (≤50 mW) and B6 (≤2 wakeups a second).
  - Decide whether a reconnect may go over B3 for the length of its walk, or
    must be throttled (fewer threads, a lower QoS class), which makes F8
    longer.
  - Publishing every quarter second re-runs an open search each time, which
    costs B2 and B5 while the window shows a search.
- **Memory.**
  - Today the drive's offline entries and its re-walked entries are both in
    the index until the batch ends. That is about twice the drive's entries
    at the peak: about 77 MB more for 878k entries at about 88 B each.
  - The limits are M1 (≤200 MB, window hidden), M2 (≤250 MB, showing a
    search), M3 (≤500 MB peak during the first scan) and M4 (≤100 B per
    entry).
  - Bringing the entries back online and reconciling them in place should
    lower that peak; measure it.
  - Every offline drive costs memory and saved-index size for as long as it
    is kept. Whether to cap that is an open question.
- **Disk writes (SSD wear).**
  - B7 allows ≤512 MB a day. It is still tracked, and the pr tier of
    14 Nov 2026 projects about 674 MB, almost all of it from compaction saves.
  - A reconnect re-keys or rewrites all of the drive's entries. If that makes
    a compaction due, every reconnect costs a full index save: 160 to 350 MB
    on the Mac measured, depending on what the index holds.
  - A drive plugged in and out several times a day must not push B7 past its
    target. Either re-keying in place doesn't count towards a compaction, or
    the saves after a reconnect follow the daily-save policy (run 2.15).
  - The walk only reads the drive's metadata, and macOS mounts exFAT with
    `noatime`, so it adds no wear to the drive itself.
- **What macOS allows.**
  - exFAT keeps no history, so F8 is bounded by the drive's walk speed: the
    FSKit exFAT module and the USB link.
  - Whether exFAT file IDs stay the same across remounts decides whether the
    reconcile can match entries by ID or must match them by path (step 1).

## Steps

Each step is its own branch and PR, with its plan first and failing tests
before code.

1. **Spike, no shipped code.** Use exFAT and APFS disk images (`fx-testkit`'s
   `DiskImage`) to answer:
   - Do file IDs stay the same across a detach and an attach?
   - Does a change on the boot volume wait while an image is being re-read?
   - What do the walk, the memory peak and the disk writes of a reconnect
     cost today? These are the baseline for F7, F8 and the budgets above.
2. **A known drive back at once.**
   - When a volume goes offline, keep its volume UUID next to its name. The
     store's format changes for this (`fx-store`, a restricted module:
     `.agents/rules/clean-room.md`).
   - When a volume with the same name and UUID mounts, re-key its offline
     entries back onto the mounted volume and publish at once. Then reconcile
     in the background: added, removed and renamed entries.
   - A reformatted drive, or another drive with the same name, has another
     UUID and gets a fresh index.
   - Files:
     - `fx-fs-macos`: `volumes.rs` reads the UUID; `volumes_source.rs`
       reports it with the mount.
     - `fx-model`: `Change`.
     - `fx-engine`: `writer.rs`, `config.rs`.
     - `fx-store`, and `fx-index` if re-keying needs it.
   - Tests:
     - the drive's files are searchable ≤2 s after the mount;
     - a file deleted while the drive was elsewhere disappears after the
       reconcile;
     - another drive with the same name brings nothing back;
     - after the reconcile, the index equals a fresh scan.
3. **Re-read a drive in pieces.** The walk feeds the index in chunks, and the
   writer publishes between them (about every 250 ms), so changes on other
   volumes keep F1. Test: a file created on the boot volume during a re-read
   shows within 1 s.
4. **Status line.** "Indexing <drive name>… N files" while a drive is re-read,
   designed on the mockups canvas first, then built in `fx-ui-macos`'s status
   view.
5. **Performance gate.** A `scripts/perf-check.sh` step detaches and attaches
   an exFAT disk image. It measures F7, F8, F1 during the reconnect, and the
   CPU, memory and disk writes a reconnect costs. The new entries go in
   `docs/gates/performance.md` and `docs/gates/perf-baseline.toml`, with
   targets the owner approves.

**Later:** APFS and HFS+ drives keep FSEvents history. A resume cursor per
volume, keyed by its UUID, would replay their changes instead of walking the
whole drive: the plan's "FSEvents replay keyed by volume UUID".

## Open questions

- **Stale results for a short time.** A drive's last-known files show at
  once, so a file deleted on another computer can show until the reconcile
  removes it: about 26 s on the drive above. The app already behaves like
  this after a relaunch. Is that acceptable?
- **Matching drives** by name and volume UUID: is that right?
- **Status line:** its wording and design on the mockups canvas.
- **`._` files.** macOS writes these AppleDouble files beside files on exFAT
  drives, and Finder hides them; the app lists them. Hide them by default?
  This is a separate product decision.
- **Keeping offline drives.** How long to keep an offline drive's entries,
  given their memory and saved-index size.
