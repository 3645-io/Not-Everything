# fx-index

The in-memory file-name index: column store, name buffer, extension table,
delta, deletion bitset, compaction and snapshots. Std only, no `unsafe`.

Clean-room: written from `docs/PLAN.md` ("Search engine and index design in
Rust": data-layout table, "Updates and concurrency") only.

## Layout

Base columns, indexed by `EntryId` (`u32`):

| Column | Type | Bytes/entry |
| --- | --- | --- |
| Name | `NameBuffer` (NUL-separated UTF-8 + `u32` offset) | name + 1 + 4 |
| Parent | `u32`, `EntryId::NONE` for roots | 4 |
| Size, kind, flags | `u64`, see below | 8 |
| Modified, created | `i64` seconds each | 16 |
| Extension | `ExtId` (`u16`) | 2 |
| File key | volume `u32` + file ID `u64` | 12 |
| Key table | open addressing, `u32` slots, load <= 3/4 | ~5.3 |

The writer also keeps a live-children count per entry (4 B) so removing a
leaf needs no scan. Packed `u64`: bits 0-55 size (saturating at 2^56 - 1),
56-59 `EntryFlags`, 60-61 kind (0 file, 1 dir, 2 symlink), 62-63 reserved.

Measured estimate (`Index::heap_bytes_per_entry`): 73 B/entry for a
100k-entry `fx-bench` tree after `compact()`, default columns, no folded
search copy.

## Updates

- `upsert(Entry)`: a new key appends to the delta; an existing key keeps its
  ID and, if anything changed, its row goes to the delta as an override. A
  rename or move touches only that entry; descendants' paths follow their
  parent pointers.
- An entry whose parent key is not live is **rejected**
  (`IndexError::UnknownParent`), not queued. Moving an entry under itself or
  a descendant is rejected with `IndexError::Cycle`.
- `remove(FileKey)` sets deletion bits for the entry and every descendant.
- `compact()` rebuilds a dense base in ID order and returns the remap
  (old ID -> new ID, `EntryId::NONE` if dropped). It runs automatically when
  the delta or the removed entries pass `compact_fraction()` (default 0.10)
  of the live entries; those remaps are collected for `take_auto_remap()`.
- `snapshot()` is O(1): it shares the base `Arc` and the current delta,
  bitset and extension table `Arc`s, which the index copies on its next
  write. Snapshots are `Send + Sync` and never change.
