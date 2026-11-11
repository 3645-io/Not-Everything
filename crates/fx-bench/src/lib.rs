//! Benchmarks: a reproducible synthetic name and file-tree generator, and
//! a harness ([`run`]) that reports nearest-rank p50 and p95 timings.
//!
//! # What the generator models
//!
//! [`generate_names`] produces file names calibrated to the statistics the
//! plan measured on a macOS startup volume: a mean length of about 17.3
//! bytes; a mix of lowercase words, digits, separators (`-`, `_`, `.`,
//! space), CamelCase, counters, hashes and dates; extensions from a fixed
//! weighted table plus extensionless names; and about 1 in 80,000 non-ASCII
//! names (accented Latin in NFC and NFD forms, CJK). Names are stored in one
//! NUL-separated buffer ([`Names`]).
//!
//! [`generate_tree`] arranges such names into a [`Tree`] with about 19%
//! directories (370k folders among 1.93M entries were measured) and bounded
//! depth, where parents precede children and sibling names are unique even
//! on a case- and normalization-insensitive file system.
//!
//! # Stability
//!
//! Randomness comes from a hand-written xoshiro256** generator seeded with
//! SplitMix64 ([`Rng`]), using integer arithmetic only. A seed therefore
//! gives identical output on every OS, architecture and toolchain.
//!
//! Changing the generator's output for an existing seed is a breaking
//! change: tests pin golden checksums, and recorded benchmark numbers refer
//! to specific seeds. Add new behaviour behind new parameters instead.
//!
//! [`materialize()`] writes a tree to disk as directories and empty files
//! under an existing empty directory, for scanner benchmarks. It only
//! creates entries; it never deletes, renames or moves anything.
//!
//! The [`gate`] module parses the `GATE ...` verdict lines that the Phase 0
//! benchmarks print and renders them as one results table; the binary's
//! `gate-summary` subcommand uses it. It only reads text.
//!
//! The [`gate1`] module holds the Phase 1 gate: its criteria table, the
//! `key=value` / JSON-lines samples the Phase 1 benchmarks print, verdicts
//! (pass, fail or not measured) and the markdown `summary.md`; the binary's
//! `phase1-summary` subcommand uses it.
//!
//! The [`perf`] module holds the performance gate's comparison: a hand-parsed
//! TOML baseline of metrics with targets and allowed regressions, the
//! `key=value` metrics files the benchmarks write, and one verdict per
//! metric; the binary's `perf-compare` subcommand uses it. Two tools feed
//! it: [`sample`] reads a running process's CPU, energy, wakeups, disk
//! writes and memory from the kernel (`sample`, macOS only), and [`churn`]
//! creates, renames and deletes files in a new folder at a steady seeded
//! rate (`churn`) so the app can be measured while files change. [`saves`]
//! projects the index saves a background phase is too short to see (`saves`),
//! which the disk-writes metric adds to the writes it measured.
//!
//! The crate uses only the standard library (on macOS, `sample` also calls
//! libSystem through its own `extern "C"` declarations).

pub mod churn;
pub mod gate;
pub mod gate1;
mod harness;
mod materialize;
mod names;
pub mod perf;
pub mod release;
mod rng;
pub mod sample;
pub mod saves;
mod tree;

pub use gate::{GateLine, GateRow, Verdict, parse_gate_lines};
pub use harness::{BenchConfig, BenchReport, CacheState, MachineInfo, Stats, run};
pub use materialize::{MaterializeStats, materialize};
pub use names::{NameGen, Names, generate_names};
pub use rng::Rng;
pub use tree::{Tree, TreeShape, generate_tree};
