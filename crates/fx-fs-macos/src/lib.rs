//! File-system source for macOS.
//!
//! * [`attrbuf`] is pure Rust and compiles on every host: it decodes macOS
//!   `getattrlistbulk` result buffers and holds the walker's descend policy
//!   (no symlinks, no mount points or automount triggers below the root, no
//!   dataless cloud-only folders).
//! * `scan` (macOS only) is the production parallel, read-only
//!   `getattrlistbulk` directory walker built on it. It applies the
//!   `fx-fs` exclusion engine before descending, enters no mount point
//!   below the root and no `/System/Volumes/Data` alias, and switches off
//!   dataless-file materialization so a scan never triggers a cloud
//!   download.
//! * `enumerator` (macOS only) holds `MacScanner`, which implements
//!   `fx_fs::VolumeEnumerator` on top of `scan` and emits `fx_model::Entry`
//!   values in parent-before-child order.
//! * [`fsevents`] is the FSEvents change source: a hand-written binding
//!   (macOS only) with a per-volume stream that replays from a saved
//!   `fx_model::ResumeCursor`, the `FsEventsSource` `fx_fs::ChangeSource`
//!   on top of it (macOS only), plus the cross-platform event-flag
//!   classifier, the pure event-to-`Change` mapping and the cursor logic.
//! * `path_stat` (macOS only) holds `MacPathStat`, the `lstat`-based
//!   `fx_fs::PathStat` that keys entries like `MacScanner` and never
//!   materializes cloud files.
//! * [`volumes`] is the mount table: `getfsstat` (macOS only) and the pure
//!   mount policy. `/` is indexed, and with [`volumes::ExternalVolumes`]
//!   the external APFS, HFS+, FAT and exFAT volumes in `/Volumes` too (run
//!   2.4); every other mount point is listed but not entered, and network
//!   mounts are never entered. `fsevents::volumes_source` watches the
//!   external volumes and reports their mounts and unmounts.
//! * [`firmlinks`] parses `/usr/share/firmlinks` and folds
//!   `/System/Volumes/Data/...` paths onto their canonical `/` form, so the
//!   Data volume is never indexed twice.
//! * [`cloud`] is the cloud-file policy: switching off dataless
//!   materialization (macOS only), spotting `SF_DATALESS` folders, and
//!   treating `EDEADLK` as "skip, don't retry".
//! * [`fda`] is the Full Disk Access side: `FdaProbe` (macOS only), the
//!   `fx_fs::PermissionProbe` that opens the TCC databases read-only to
//!   tell Granted, Denied and Unknown apart without any consent prompt, the
//!   System Settings deep link, and the pure consent-prompt exclusion set
//!   that the non-FDA mode hands to the scanner.

pub mod attrbuf;
pub mod cloud;
pub mod fda;
pub mod firmlinks;
pub mod fsevents;
pub mod volumes;

#[cfg(target_os = "macos")]
pub mod enumerator;
#[cfg(target_os = "macos")]
pub mod path_stat;
#[cfg(target_os = "macos")]
pub mod scan;
