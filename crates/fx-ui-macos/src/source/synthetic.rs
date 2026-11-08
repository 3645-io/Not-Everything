//! [`SyntheticSource`]: millions of synthetic rows from `fx-bench`, for
//! measuring the window without an index (`measure` builds only).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use fx_engine::Generation;
use fx_query::Cancelled;
use fx_sort::{Direction, SortKey};

use super::{ResultPage, ResultsSource, Row, RowPositions, SourceStatus};
use crate::model::RowModel;

/// The generation of every synthetic page: the rows never change.
const SYNTHETIC_GENERATION: Generation = Generation(1);

/// Synthetic rows over an `fx-bench` tree, for running the window on
/// millions of rows without an index.
///
/// Row `i` is [`RowModel`] row `i`; its full path is `/` plus the tree path.
/// Queries are case-blind substring matches on the name. Sort orders are
/// built on first use per key and kept; the last query's matches are cached
/// so paging through one result set does not rescan every name.
pub struct SyntheticSource {
    model: RowModel,
    /// Name order, then the rank of each row in it.
    name_order: OnceLock<(Vec<u32>, Vec<u32>)>,
    path_order: OnceLock<Vec<u32>>,
    size_order: OnceLock<Vec<u32>>,
    modified_order: OnceLock<Vec<u32>>,
    last_query: Mutex<Option<(String, Arc<Vec<bool>>)>>,
    subscribers: Mutex<Vec<Sender<Generation>>>,
    /// Simulated cost of every search (see [`SyntheticSource::with_page_cost`]).
    page_cost: Duration,
}

impl SyntheticSource {
    /// `rows` synthetic rows generated from `seed` (see [`RowModel::new`]).
    pub fn new(rows: usize, seed: u64) -> SyntheticSource {
        SyntheticSource::from_model(RowModel::new(rows, seed))
    }

    /// A source over an existing model.
    pub fn from_model(model: RowModel) -> SyntheticSource {
        SyntheticSource {
            model,
            name_order: OnceLock::new(),
            path_order: OnceLock::new(),
            size_order: OnceLock::new(),
            modified_order: OnceLock::new(),
            last_query: Mutex::new(None),
            subscribers: Mutex::new(Vec::new()),
            page_cost: Duration::ZERO,
        }
    }

    /// Makes every [`search`](ResultsSource::search) take at least `cost`
    /// (sleeping for whatever the real work did not use), without changing
    /// its results. This approximates the paging cost of a real engine
    /// search when measuring scrolling with `--autoscroll --page-cost-ms`;
    /// frame times measured this way are still synthetic.
    pub fn with_page_cost(mut self, cost: Duration) -> SyntheticSource {
        self.page_cost = cost;
        self
    }

    /// The simulated cost of every search; zero by default.
    pub fn page_cost(&self) -> Duration {
        self.page_cost
    }

    /// The underlying row model.
    pub fn model(&self) -> &RowModel {
        &self.model
    }

    fn name_cmp(&self, a: u32, b: u32) -> Ordering {
        let names = &self.model.tree().names;
        let (x, y) = (names.get(a as usize), names.get(b as usize));
        let folded = x
            .bytes()
            .map(|c| c.to_ascii_lowercase())
            .cmp(y.bytes().map(|c| c.to_ascii_lowercase()));
        folded.then_with(|| x.cmp(y)).then(a.cmp(&b))
    }

    fn name_order(&self) -> &(Vec<u32>, Vec<u32>) {
        self.name_order.get_or_init(|| {
            let n = self.model.row_count();
            let mut order: Vec<u32> = (0..n as u32).collect();
            order.sort_unstable_by(|&a, &b| self.name_cmp(a, b));
            let mut rank = vec![0u32; n];
            for (r, &id) in order.iter().enumerate() {
                rank[id as usize] = r as u32;
            }
            (order, rank)
        })
    }

    /// Preorder walk with children in name order: full-path order.
    fn path_order(&self) -> &Vec<u32> {
        self.path_order.get_or_init(|| {
            let tree = self.model.tree();
            let n = tree.len();
            let mut children: Vec<Vec<u32>> = vec![Vec::new(); n];
            let mut roots = Vec::new();
            for &id in &self.name_order().0 {
                match tree.parent[id as usize] {
                    u32::MAX => roots.push(id),
                    p => children[p as usize].push(id),
                }
            }
            let mut order = Vec::with_capacity(n);
            let mut stack: Vec<u32> = roots.into_iter().rev().collect();
            while let Some(id) = stack.pop() {
                order.push(id);
                stack.extend(children[id as usize].iter().rev());
            }
            order
        })
    }

    /// Rows sorted by `key`, ties in name order.
    fn keyed_order(&self, key: impl Fn(usize) -> i128) -> Vec<u32> {
        let rank = &self.name_order().1;
        let mut order: Vec<u32> = (0..self.model.row_count() as u32).collect();
        order.sort_unstable_by_key(|&id| (key(id as usize), rank[id as usize]));
        order
    }

    fn order(&self, sort: SortKey) -> &[u32] {
        match sort {
            SortKey::Name => &self.name_order().0,
            SortKey::Path => self.path_order(),
            SortKey::Size => self.size_order.get_or_init(|| {
                self.keyed_order(|i| i128::from(self.model.size_bytes(i).unwrap_or(0)))
            }),
            SortKey::Modified | SortKey::Created => self.modified_order.get_or_init(|| {
                self.keyed_order(|i| i128::from(self.model.modified_unix(i).unwrap_or(0)))
            }),
        }
    }

    /// Which rows match `query`, cached for the last query.
    fn matches(&self, query: &str) -> Arc<Vec<bool>> {
        let mut last = self
            .last_query
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((q, m)) = last.as_ref() {
            if q == query {
                return Arc::clone(m);
            }
        }
        let mut hit = vec![false; self.model.row_count()];
        for i in self.model.filter(query) {
            hit[i] = true;
        }
        let hit = Arc::new(hit);
        *last = Some((query.to_string(), Arc::clone(&hit)));
        hit
    }
}

impl ResultsSource for SyntheticSource {
    fn search(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> Result<ResultPage, Cancelled> {
        let started = Instant::now();
        let page = self.search_now(query, sort, dir, offset, limit);
        if let Some(rest) = self.page_cost.checked_sub(started.elapsed()) {
            if !rest.is_zero() {
                thread::sleep(rest);
            }
        }
        Ok(page)
    }

    fn cancel(&self) {}

    fn row(&self, _generation: Generation, id: u32) -> Option<Row> {
        let i = id as usize;
        let name = self.model.name(i)?.to_string();
        let is_folder = self.model.is_dir(i)?;
        Some(Row {
            full_path: format!("/{}", self.model.tree().path(i)),
            name,
            size: if is_folder {
                None
            } else {
                self.model.size_bytes(i)
            },
            modified: self.model.modified_unix(i),
            is_folder,
        })
    }

    /// Each path is found with a binary search of the name order, then one
    /// pass over the sort order (filtered by the query) gives the rows.
    fn positions(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        paths: &[String],
    ) -> Result<RowPositions, Cancelled> {
        let hit = (!query.is_empty()).then(|| self.matches(query));
        let matched = |id: u32| hit.as_ref().is_none_or(|h| h[id as usize]);
        let targets: Vec<Option<u32>> = paths
            .iter()
            .map(|p| self.find_path(p).filter(|&id| matched(id)))
            .collect();
        let wanted: HashSet<u32> = targets.iter().flatten().copied().collect();
        let mut found: HashMap<u32, usize> = HashMap::with_capacity(wanted.len());
        if !wanted.is_empty() {
            let order = self.order(sort);
            let ids: &mut dyn Iterator<Item = &u32> = match dir {
                Direction::Ascending => &mut order.iter(),
                Direction::Descending => &mut order.iter().rev(),
            };
            for (row, &id) in ids.filter(|&&id| matched(id)).enumerate() {
                if wanted.contains(&id) {
                    found.insert(id, row);
                    if found.len() == wanted.len() {
                        break;
                    }
                }
            }
        }
        Ok(RowPositions {
            generation: SYNTHETIC_GENERATION,
            rows: targets
                .iter()
                .map(|t| t.and_then(|id| found.get(&id).copied()))
                .collect(),
        })
    }

    fn generation(&self) -> Generation {
        SYNTHETIC_GENERATION
    }

    fn status(&self) -> SourceStatus {
        SourceStatus {
            indexing: false,
            entries: self.model.row_count(),
        }
    }

    fn subscribe(&self) -> Receiver<Generation> {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(tx);
        rx
    }
}

impl SyntheticSource {
    /// The row whose full path is `path` (`/` plus the tree path): the rows
    /// named like its last component, from the name order, checked one by
    /// one.
    fn find_path(&self, path: &str) -> Option<u32> {
        let rel = path.strip_prefix('/')?;
        let name = rel.rsplit('/').next()?;
        let names = &self.model.tree().names;
        // The first two levels of `name_cmp`: folded, then raw bytes.
        let cmp = |id: u32| {
            let other = names.get(id as usize);
            other
                .bytes()
                .map(|c| c.to_ascii_lowercase())
                .cmp(name.bytes().map(|c| c.to_ascii_lowercase()))
                .then_with(|| other.cmp(name))
        };
        let order = &self.name_order().0;
        let start = order.partition_point(|&id| cmp(id) == Ordering::Less);
        order[start..]
            .iter()
            .take_while(|&&id| cmp(id) == Ordering::Equal)
            .find(|&&id| self.model.tree().path(id as usize) == rel)
            .copied()
    }

    /// The search itself, without the simulated page cost.
    fn search_now(
        &self,
        query: &str,
        sort: SortKey,
        dir: Direction,
        offset: usize,
        limit: usize,
    ) -> ResultPage {
        let order = self.order(sort);
        let window = |iter: &mut dyn Iterator<Item = u32>| -> Vec<u32> {
            iter.skip(offset).take(limit).collect()
        };
        let (total, ids) = if query.is_empty() {
            let ids = match dir {
                Direction::Ascending => window(&mut order.iter().copied()),
                Direction::Descending => window(&mut order.iter().rev().copied()),
            };
            (order.len(), ids)
        } else {
            let hit = self.matches(query);
            let total = hit.iter().filter(|&&h| h).count();
            let mut matching = order.iter().copied().filter(|&id| hit[id as usize]);
            let ids = match dir {
                Direction::Ascending => window(&mut matching),
                Direction::Descending => {
                    let mut rev = order.iter().rev().copied().filter(|&id| hit[id as usize]);
                    window(&mut rev)
                }
            };
            (total, ids)
        };
        ResultPage {
            generation: SYNTHETIC_GENERATION,
            total,
            ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ids(source: &SyntheticSource, sort: SortKey, dir: Direction) -> Vec<u32> {
        source.search("", sort, dir, 0, usize::MAX).unwrap().ids
    }

    #[test]
    fn synthetic_orders_are_permutations_and_sorted() {
        let source = SyntheticSource::new(500, 3);
        let n = source.model().row_count();
        for sort in [
            SortKey::Name,
            SortKey::Path,
            SortKey::Size,
            SortKey::Modified,
        ] {
            let mut ids = all_ids(&source, sort, Direction::Ascending);
            let mut desc = all_ids(&source, sort, Direction::Descending);
            desc.reverse();
            assert_eq!(ids, desc, "{sort:?} descending is the exact reverse");
            ids.sort_unstable();
            assert_eq!(ids, (0..n as u32).collect::<Vec<_>>(), "{sort:?}");
        }

        let sizes: Vec<u64> = all_ids(&source, SortKey::Size, Direction::Ascending)
            .iter()
            .map(|&id| source.model().size_bytes(id as usize).unwrap())
            .collect();
        assert!(sizes.windows(2).all(|w| w[0] <= w[1]));

        let path_order = all_ids(&source, SortKey::Path, Direction::Ascending);
        assert_eq!(path_order[0], 0, "the root comes first");
        let pos: Vec<usize> = {
            let mut pos = vec![0; n];
            for (k, &id) in path_order.iter().enumerate() {
                pos[id as usize] = k;
            }
            pos
        };
        let tree = source.model().tree();
        for i in 1..n {
            assert!(pos[tree.parent[i] as usize] < pos[i], "parent before child");
        }
    }

    #[test]
    fn synthetic_query_pages_agree_with_the_total() {
        let source = SyntheticSource::new(2000, 5);
        let all = source
            .search("a", SortKey::Name, Direction::Ascending, 0, usize::MAX)
            .unwrap();
        assert_eq!(all.ids.len(), all.total);
        let page = source
            .search("a", SortKey::Name, Direction::Ascending, 10, 5)
            .unwrap();
        assert_eq!(page.total, all.total);
        assert_eq!(page.ids, all.ids[10..15].to_vec());
    }
}
