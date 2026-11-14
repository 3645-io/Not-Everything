//! The index saves a background phase of the performance gate is too short
//! to see, projected to a day, for B7 ("Disk writes a day",
//! `docs/gates/performance.md`). `fx-bench saves` uses it.
//!
//! The engine's writer compacts the index when the rows added since the
//! last compaction, or the entries removed since then, pass
//! [`compaction_limit`]: [`COMPACT_FRACTION`] of the live entries, at least
//! [`MIN_COMPACT_ROWS`]. Each compaction then saves the whole index to the
//! store file, about 150 MB for a whole-disk index of 2M entries. A gate
//! phase (10 or 30 minutes at 20 file operations a second, about half of
//! them creates) adds 6,000 to 18,000 rows where such an index compacts
//! every 200,000, so the sampled disk writes never include a save.
//! [`project`] adds them back from the phase's file activity:
//!
//! - A file created adds one new row and a file deleted one removed row. A
//!   file renamed (or changed) after it was created adds none, because its
//!   row is updated in place. Two small effects pull the model either way:
//!   a file created and deleted within one of the writer's batches is gone
//!   before the writer looks and gets no row, though the model counts it;
//!   and a file of the base adds a row the first time it changes after a
//!   compaction, which the model leaves out (with `fx-bench churn`, at most
//!   its 5,000 files per compaction, 2.5% of a whole-disk index's
//!   200,000).
//! - New and removed rows count separately and both start again at each
//!   compaction, so the faster of the two sets the pace: a day of `c`
//!   creates and `d` deletes a second makes `max(c, d) × 86,400 ÷ (limit +
//!   1)` compactions.
//! - Each compaction writes the store once, at its size: the store writes
//!   a temp file and renames it over the old one.
//!
//! fx-bench doesn't depend on fx-engine, so the rule's two numbers are
//! repeated here; fx-engine's `tests/compaction_saves.rs` pins the fraction
//! to `fx_index::DEFAULT_COMPACT_FRACTION` and checks the whole model, the
//! floor included, against the real writer, driven by `fx-bench churn`'s
//! operation stream.

/// The fraction of the live entries that the new rows (or the removed
/// rows) may reach before the writer compacts: fx-index's
/// `DEFAULT_COMPACT_FRACTION`.
pub const COMPACT_FRACTION: f64 = 0.10;

/// The fewest rows the writer compacts for, whatever the index's size
/// (fx-engine's writer).
pub const MIN_COMPACT_ROWS: u64 = 1024;

/// Seconds in a day.
const DAY_S: f64 = 86_400.0;

/// The rows past which the writer compacts an index of `entries` live
/// entries: it compacts at the first batch that leaves more new rows, or
/// more removed rows, than this.
pub fn compaction_limit(entries: u64) -> u64 {
    ((entries as f64 * COMPACT_FRACTION) as u64).max(MIN_COMPACT_ROWS)
}

/// Whether the writer compacts an index of `entries` live entries after
/// `new_rows` new rows and `removed_rows` removed rows since the last
/// compaction.
pub fn compaction_due(entries: u64, new_rows: u64, removed_rows: u64) -> bool {
    let limit = compaction_limit(entries);
    new_rows > limit || removed_rows > limit
}

/// The file activity of a phase, as `fx-bench churn` reports it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Activity {
    /// Files created.
    pub creates: u64,
    /// Files deleted.
    pub deletes: u64,
    /// The phase's length, in seconds.
    pub seconds: f64,
}

/// A day of index saves at a phase's pace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    /// Live entries in the index.
    pub entries: u64,
    /// The rows of one kind that bring a compaction: one more than
    /// [`compaction_limit`].
    pub rows_per_compaction: u64,
    /// Compactions, and so saves, a day.
    pub compactions_per_day: f64,
    /// Bytes one save writes: the store file's size.
    pub save_bytes: u64,
    /// `compactions_per_day × save_bytes`.
    pub saves_per_day_bytes: f64,
}

/// The saves a day of `activity`'s pace makes on an index of `entries` live
/// entries whose store file is `save_bytes` long. `None` when the phase's
/// length is not a positive number of seconds.
pub fn project(entries: u64, save_bytes: u64, activity: &Activity) -> Option<Projection> {
    let seconds = activity.seconds;
    if !(seconds.is_finite() && seconds > 0.0) {
        return None;
    }
    let rows_per_compaction = compaction_limit(entries) + 1;
    let rows_per_day = activity.creates.max(activity.deletes) as f64 * DAY_S / seconds;
    let compactions_per_day = rows_per_day / rows_per_compaction as f64;
    Some(Projection {
        entries,
        rows_per_compaction,
        compactions_per_day,
        save_bytes,
        saves_per_day_bytes: compactions_per_day * save_bytes as f64,
    })
}

/// The projection's `key=value` lines, each key prefixed with `label` and
/// `_`: `entries`, `rows_per_compaction`, `compactions_per_day`,
/// `save_bytes` and `compaction_saves_per_day_bytes`; with the phase's
/// measured writes a day, also `disk_written_with_saves_per_day_bytes`,
/// their sum, which B7 reads.
pub fn metric_lines(label: &str, p: &Projection, measured_per_day: Option<f64>) -> Vec<String> {
    let mut lines = vec![
        format!("{label}_entries={}", p.entries),
        format!("{label}_rows_per_compaction={}", p.rows_per_compaction),
        format!("{label}_compactions_per_day={:.3}", p.compactions_per_day),
        format!("{label}_save_bytes={}", p.save_bytes),
        format!(
            "{label}_compaction_saves_per_day_bytes={:.0}",
            p.saves_per_day_bytes
        ),
    ];
    if let Some(measured) = measured_per_day {
        lines.push(format!(
            "{label}_disk_written_with_saves_per_day_bytes={:.0}",
            measured + p.saves_per_day_bytes
        ));
    }
    lines
}
