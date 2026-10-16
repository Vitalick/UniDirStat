use std::cmp::Ordering;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering as AtomicOrdering;
use std::time::{SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::model::{EntryKind, Node, Snapshot};
use crate::scanner::{ScanControl, ScanPhase, sort_work_total, thread_pool};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchTarget {
    #[default]
    Path,
    Name,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchSort {
    #[default]
    Size,
    Name,
    Modified,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchQuery {
    pub text: String,
    pub regex: bool,
    pub target: SearchTarget,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub allocated: bool,
    pub min_age_seconds: Option<u64>,
    pub max_age_seconds: Option<u64>,
    pub kinds: Vec<EntryKind>,
    pub extension: Option<String>,
    pub owner: Option<u32>,
    pub empty_directories: bool,
    pub scope: Option<usize>,
    pub sort: SearchSort,
    pub descending: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            regex: false,
            target: SearchTarget::Path,
            min_size: None,
            max_size: None,
            allocated: false,
            min_age_seconds: None,
            max_age_seconds: None,
            kinds: Vec::new(),
            extension: None,
            owner: None,
            empty_directories: false,
            scope: None,
            sort: SearchSort::Size,
            descending: true,
        }
    }
}

pub struct SearchIndex {
    snapshot: Arc<Snapshot>,
    paths: Vec<CachedPath>,
    order: Vec<usize>,
    subtrees: Vec<Range<usize>>,
    pool: Result<rayon::ThreadPool, String>,
}

struct CachedPath {
    value: String,
    name_start: usize,
    extension_start: Option<usize>,
}

impl CachedPath {
    fn new(node: &Node) -> Self {
        let value = node.path.to_string_lossy().to_lowercase();
        let name = node
            .path
            .file_name()
            .unwrap_or(node.path.as_os_str())
            .to_string_lossy()
            .to_lowercase();
        let extension_start = node
            .path
            .extension()
            .filter(|extension| !extension.is_empty())
            .map(|extension| value.len() - extension.to_string_lossy().to_lowercase().len());
        Self {
            name_start: value.len() - name.len(),
            value,
            extension_start,
        }
    }

    fn name(&self) -> &str {
        &self.value[self.name_start..]
    }

    fn extension(&self) -> &str {
        self.extension_start
            .map_or("", |start| &self.value[start..])
    }
}

impl SearchIndex {
    pub fn new(snapshot: Arc<Snapshot>) -> Self {
        Self::with_threads(snapshot, 0)
    }

    pub fn with_threads(snapshot: Arc<Snapshot>, threads: usize) -> Self {
        Self::with_control(Arc::clone(&snapshot), threads, &ScanControl::default()).unwrap_or_else(
            |error| Self {
                snapshot,
                paths: Vec::new(),
                order: Vec::new(),
                subtrees: Vec::new(),
                pool: Err(error),
            },
        )
    }

    pub fn with_control(
        snapshot: Arc<Snapshot>,
        threads: usize,
        control: &ScanControl,
    ) -> Result<Self, String> {
        checkpoint(control)?;
        let pool = thread_pool(threads)?;
        control.start_work((snapshot.nodes.len() as u64).saturating_mul(3));
        control.set_phase(ScanPhase::Indexing);
        let paths: Result<Vec<_>, String> = pool.install(|| {
            snapshot
                .nodes
                .par_iter()
                .map(|node| {
                    checkpoint(control)?;
                    let path = CachedPath::new(node);
                    control.advance_work(1);
                    Ok(path)
                })
                .collect()
        });
        let paths = paths?;
        let mut order = Vec::with_capacity(snapshot.nodes.len());
        let mut subtrees = vec![0..0; snapshot.nodes.len()];
        let mut seen = vec![false; snapshot.nodes.len()];
        let mut pending: Vec<_> = snapshot
            .roots
            .iter()
            .rev()
            .map(|&root| (root, false))
            .collect();
        let mut completed = 0;
        while let Some((index, exiting)) = pending.pop() {
            checkpoint(control)?;
            let Some(node) = snapshot.nodes.get(index) else {
                continue;
            };
            if exiting {
                subtrees[index].end = order.len();
                completed += 1;
            } else if !std::mem::replace(&mut seen[index], true) {
                subtrees[index].start = order.len();
                order.push(index);
                pending.push((index, true));
                pending.extend(node.children.iter().rev().map(|&child| (child, false)));
                completed += 1;
            }
            if completed == 256 {
                control.advance_work(completed);
                completed = 0;
            }
        }
        control.advance_work(completed);
        checkpoint(control)?;
        Ok(Self {
            snapshot,
            paths,
            order,
            subtrees,
            pool: Ok(pool),
        })
    }

    pub fn search(&self, query: &SearchQuery, control: &ScanControl) -> Result<Vec<usize>, String> {
        checkpoint(control)?;
        if query.empty_directories && self.snapshot.cancelled {
            return Err("Refresh a cancelled scan before searching for empty folders".into());
        }
        if query
            .min_size
            .zip(query.max_size)
            .is_some_and(|(min, max)| min > max)
            || query
                .min_age_seconds
                .zip(query.max_age_seconds)
                .is_some_and(|(min, max)| min > max)
        {
            return Err("Minimum filter value exceeds maximum".into());
        }
        let pool = self.pool.as_ref().map_err(|error| error.clone())?;
        let candidates = match query.scope {
            Some(scope) => {
                &self.order[self
                    .subtrees
                    .get(scope)
                    .ok_or("Invalid search scope")?
                    .clone()]
            }
            None => &self.order,
        };
        let text = query.text.to_lowercase();
        let regex = query
            .regex
            .then(|| {
                RegexBuilder::new(&query.text)
                    .case_insensitive(true)
                    .build()
            })
            .transpose()
            .map_err(|error| format!("Invalid search expression: {error}"))?;
        let extension = query.extension.as_ref().map(|extension| {
            extension
                .strip_prefix('.')
                .unwrap_or(extension)
                .to_lowercase()
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        control.start_work(candidates.len() as u64);
        control.set_phase(ScanPhase::Filtering);
        let mut found: Vec<_> = pool.install(|| {
            candidates
                .par_iter()
                .enumerate()
                .map(|(position, &index)| {
                    if !control.checkpoint() {
                        return None;
                    }
                    if position % 512 == 0 {
                        control.visited.fetch_add(
                            (candidates.len() - position).min(512) as u64,
                            AtomicOrdering::Relaxed,
                        );
                    }
                    let node = &self.snapshot.nodes[index];
                    let path = &self.paths[index];
                    let haystack = match query.target {
                        SearchTarget::Path => path.value.as_str(),
                        SearchTarget::Name => path.name(),
                    };
                    let text_matches = if let Some(regex) = &regex {
                        let original = match query.target {
                            SearchTarget::Path => node.path.to_str(),
                            SearchTarget::Name => {
                                node.path.file_name().and_then(|name| name.to_str())
                            }
                        }
                        .unwrap_or(haystack);
                        regex.is_match(original)
                    } else {
                        haystack.contains(&text)
                    };
                    let size = if query.allocated {
                        node.allocated_size
                    } else {
                        Some(node.logical_size)
                    };
                    let age = node.modified.map(|modified| now.saturating_sub(modified));
                    let matches = text_matches
                        && (query.kinds.is_empty() || query.kinds.contains(&node.kind))
                        && (!query.empty_directories
                            || (node.kind == EntryKind::Directory
                                && node.children.is_empty()
                                && node.error.is_none()))
                        && query.owner.is_none_or(|owner| node.owner == Some(owner))
                        && extension.as_ref().is_none_or(|extension| {
                            path.extension() == extension
                                || (extension == "(no extension)" && path.extension().is_empty())
                        })
                        && query
                            .min_size
                            .is_none_or(|minimum| size.is_some_and(|size| size >= minimum))
                        && query
                            .max_size
                            .is_none_or(|maximum| size.is_some_and(|size| size <= maximum))
                        && query
                            .min_age_seconds
                            .is_none_or(|minimum| age.is_some_and(|age| age >= minimum))
                        && query
                            .max_age_seconds
                            .is_none_or(|maximum| age.is_some_and(|age| age <= maximum));
                    control.advance_work(1);
                    Some(matches.then_some(index))
                })
                .while_some()
                .filter_map(|index| index)
                .collect()
        });
        checkpoint(control)?;
        self.sort_nodes(&mut found, query, control)?;
        Ok(found)
    }

    pub fn sort_nodes(
        &self,
        indices: &mut Vec<usize>,
        query: &SearchQuery,
        control: &ScanControl,
    ) -> Result<(), String> {
        checkpoint(control)?;
        for &index in indices.iter() {
            checkpoint(control)?;
            if index >= self.snapshot.nodes.len() {
                return Err("Invalid node index".into());
            }
        }
        let pool = self.pool.as_ref().map_err(|error| error.clone())?;
        let compare = |left: &usize, right: &usize| {
            let left_node = &self.snapshot.nodes[*left];
            let right_node = &self.snapshot.nodes[*right];
            let order = match query.sort {
                SearchSort::Size => ordered(
                    if query.allocated {
                        left_node.allocated_size
                    } else {
                        Some(left_node.logical_size)
                    },
                    if query.allocated {
                        right_node.allocated_size
                    } else {
                        Some(right_node.logical_size)
                    },
                    query.descending,
                ),
                SearchSort::Name => {
                    let order = self.paths[*left].name().cmp(self.paths[*right].name());
                    if query.descending {
                        order.reverse()
                    } else {
                        order
                    }
                }
                SearchSort::Modified => {
                    ordered(left_node.modified, right_node.modified, query.descending)
                }
            };
            order
                .then_with(|| self.paths[*left].value.cmp(&self.paths[*right].value))
                .then_with(|| left.cmp(right))
        };
        sort_cancellable(indices, &compare, pool, control)
    }
}

fn checkpoint(control: &ScanControl) -> Result<(), String> {
    if control.checkpoint() {
        Ok(())
    } else {
        Err("Search cancelled".into())
    }
}

fn ordered<Value: Ord>(left: Option<Value>, right: Option<Value>, descending: bool) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            if descending {
                right.cmp(&left)
            } else {
                left.cmp(&right)
            }
        }
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn sort_cancellable(
    indices: &mut Vec<usize>,
    compare: &(impl Fn(&usize, &usize) -> Ordering + Sync),
    pool: &rayon::ThreadPool,
    control: &ScanControl,
) -> Result<(), String> {
    const SORT_CHUNK: usize = 2048;
    control.start_work(sort_work_total(indices.len(), SORT_CHUNK));
    control.set_phase(ScanPhase::Sorting);
    pool.install(|| {
        indices.par_chunks_mut(SORT_CHUNK).try_for_each(|chunk| {
            checkpoint(control)?;
            chunk.sort_unstable_by(compare);
            control.advance_work(chunk.len() as u64);
            checkpoint(control)
        })
    })?;
    let mut width = SORT_CHUNK;
    if indices.len() > width {
        let mut buffer = vec![0; indices.len()];
        while width < indices.len() {
            checkpoint(control)?;
            let step = width.saturating_mul(2);
            pool.install(|| {
                buffer
                    .par_chunks_mut(step)
                    .enumerate()
                    .try_for_each(|(block, output)| {
                        let start = block * step;
                        let middle = (start + width).min(indices.len());
                        let end = start + output.len();
                        let mut left = start;
                        let mut right = middle;
                        for (position, item) in output.iter_mut().enumerate() {
                            if position % 256 == 0 {
                                checkpoint(control)?;
                            }
                            if left < middle
                                && (right == end
                                    || compare(&indices[left], &indices[right])
                                        != Ordering::Greater)
                            {
                                *item = indices[left];
                                left += 1;
                            } else {
                                *item = indices[right];
                                right += 1;
                            }
                            if (position + 1) % 256 == 0 {
                                control.advance_work(256);
                            }
                        }
                        control.advance_work((output.len() % 256) as u64);
                        Ok::<(), String>(())
                    })
            })?;
            std::mem::swap(indices, &mut buffer);
            width = step;
        }
    }
    checkpoint(control)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::Ordering;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::scanner::{ScanOptions, scan};

    #[test]
    fn indexing_and_search_report_complete_work_without_resetting_scan_totals() {
        let (_directory, snapshot) = fixture();
        let control = ScanControl::default();
        control.bytes.store(1234, Ordering::Relaxed);
        let index = SearchIndex::with_control(Arc::clone(&snapshot), 2, &control).unwrap();
        assert_eq!(
            control.work_total.load(Ordering::Relaxed),
            snapshot.nodes.len() as u64 * 3
        );
        assert_eq!(
            control.work_done.load(Ordering::Relaxed),
            control.work_total.load(Ordering::Relaxed)
        );
        let found = index.search(&SearchQuery::default(), &control).unwrap();
        assert_eq!(
            control.work_total.load(Ordering::Relaxed),
            found.len() as u64
        );
        assert_eq!(
            control.work_done.load(Ordering::Relaxed),
            found.len() as u64
        );
        assert_eq!(
            control.visited.load(Ordering::Relaxed),
            snapshot.nodes.len() as u64
        );
        assert_eq!(control.bytes.load(Ordering::Relaxed), 1234);
    }

    #[test]
    fn indexing_and_filtering_cancel_after_reporting_progress() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(Arc::clone(&snapshot));
        for phase in [ScanPhase::Indexing, ScanPhase::Filtering] {
            let control = ScanControl::default();
            let gate = crate::scanner::tests::register_gate(&control, phase);
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| match phase {
                    ScanPhase::Indexing => {
                        SearchIndex::with_control(Arc::clone(&snapshot), 2, &control).map(|_| ())
                    }
                    _ => index.search(&SearchQuery::default(), &control).map(|_| ()),
                });
                gate.arrived.wait();
                assert!(control.work_done.load(Ordering::Relaxed) > 0);
                control.cancel();
                gate.release.wait();
                assert!(worker.join().unwrap().is_err());
            });
        }
    }

    fn fixture() -> (tempfile::TempDir, Arc<Snapshot>) {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("Документ.RS"), [0; 10]).unwrap();
        fs::write(directory.path().join("image.PNG"), [0; 20]).unwrap();
        fs::write(directory.path().join("nested/notes.txt"), [0; 5]).unwrap();
        fs::write(directory.path().join("nested/other.rs"), [0; 30]).unwrap();
        let mut snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for node in &mut snapshot.nodes {
            if node.kind == EntryKind::File {
                node.owner = Some(if node.extension() == "rs" { 1000 } else { 2000 });
                node.modified = Some(
                    now - if node.extension() == "rs" {
                        86400
                    } else {
                        3 * 86400
                    },
                );
                node.allocated_size = Some(node.logical_size * 2);
            }
        }
        (directory, Arc::new(snapshot))
    }

    #[test]
    fn search_combines_unicode_text_size_age_kind_extension_and_owner() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let query = SearchQuery {
            text: "ДОКУМЕНТ".into(),
            min_size: Some(9),
            max_size: Some(12),
            min_age_seconds: Some(3600),
            max_age_seconds: Some(2 * 86400),
            kinds: vec![EntryKind::File],
            extension: Some(".RS".into()),
            owner: Some(1000),
            ..SearchQuery::default()
        };

        let found = index.search(&query, &ScanControl::default()).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(snapshot.nodes[found[0]].name(), "Документ.RS");
    }

    #[test]
    fn name_search_does_not_match_the_parent_path() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(snapshot);
        let mut query = SearchQuery {
            text: "nested".into(),
            kinds: vec![EntryKind::File],
            ..SearchQuery::default()
        };

        assert_eq!(
            index.search(&query, &ScanControl::default()).unwrap().len(),
            2
        );
        query.target = SearchTarget::Name;
        assert!(
            index
                .search(&query, &ScanControl::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn regex_search_matches_unicode_and_rejects_invalid_expressions() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(snapshot);
        let mut query = SearchQuery {
            text: r"ДОКУМЕНТ\.rs$".into(),
            regex: true,
            target: SearchTarget::Name,
            ..SearchQuery::default()
        };

        assert_eq!(
            index.search(&query, &ScanControl::default()).unwrap().len(),
            1
        );
        query.text = "[".into();
        assert!(index.search(&query, &ScanControl::default()).is_err());
    }

    #[test]
    fn subtree_scope_uses_arena_topology_and_includes_scope_itself() {
        let (_directory, snapshot) = fixture();
        let scope = snapshot
            .nodes
            .iter()
            .position(|node| node.name() == "nested")
            .unwrap();
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let mut query = SearchQuery {
            scope: Some(scope),
            ..SearchQuery::default()
        };

        let found = index.search(&query, &ScanControl::default()).unwrap();

        assert_eq!(found.len(), 3);
        assert!(found.contains(&scope));
        query.scope = Some(usize::MAX);
        assert!(index.search(&query, &ScanControl::default()).is_err());
    }

    #[test]
    fn allocated_size_filters_do_not_fall_back_to_logical_size() {
        let (_directory, snapshot) = fixture();
        let mut snapshot = (*snapshot).clone();
        snapshot
            .nodes
            .iter_mut()
            .find(|node| node.name() == "other.rs")
            .unwrap()
            .allocated_size = None;
        let index = SearchIndex::new(Arc::new(snapshot));
        let query = SearchQuery {
            allocated: true,
            min_size: Some(30),
            kinds: vec![EntryKind::File],
            ..SearchQuery::default()
        };

        let found = index.search(&query, &ScanControl::default()).unwrap();

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn sorting_is_deterministic_for_size_name_and_modified() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let mut query = SearchQuery {
            kinds: vec![EntryKind::File],
            ..SearchQuery::default()
        };
        let found = index.search(&query, &ScanControl::default()).unwrap();
        assert_eq!(snapshot.nodes[found[0]].name(), "other.rs");
        query.sort = SearchSort::Name;
        query.descending = false;
        let found = index.search(&query, &ScanControl::default()).unwrap();
        assert_eq!(snapshot.nodes[found[0]].name(), "image.PNG");
        query.sort = SearchSort::Modified;
        query.descending = true;
        let first = index.search(&query, &ScanControl::default()).unwrap();
        let repeated = index.search(&query, &ScanControl::default()).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(snapshot.nodes[first[0]].extension(), "rs");
    }

    #[test]
    fn sorting_supplied_siblings_keeps_unknown_allocations_last() {
        let (_directory, snapshot) = fixture();
        let mut snapshot = Arc::try_unwrap(snapshot).unwrap();
        let root = snapshot.roots[0];
        for &index in &snapshot.nodes[root].children.clone() {
            let node = &mut snapshot.nodes[index];
            node.allocated_size = match node.name().as_str() {
                "nested" => Some(15),
                "image.PNG" => Some(40),
                _ => None,
            };
        }
        let snapshot = Arc::new(snapshot);
        let index = SearchIndex::new(Arc::clone(&snapshot));
        for (descending, expected) in [
            (false, ["nested", "image.PNG", "Документ.RS"]),
            (true, ["image.PNG", "nested", "Документ.RS"]),
        ] {
            let mut children = snapshot.nodes[root].children.clone();
            let query = SearchQuery {
                allocated: true,
                descending,
                ..SearchQuery::default()
            };
            let control = ScanControl::default();

            index.sort_nodes(&mut children, &query, &control).unwrap();

            assert_eq!(
                children
                    .iter()
                    .map(|&index| snapshot.nodes[index].name())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(control.work_done.load(Ordering::Relaxed), 3);
            assert_eq!(control.work_total.load(Ordering::Relaxed), 3);
            assert_eq!(control.visited.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn supplied_sibling_sort_cancels_after_reporting_progress() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let control = ScanControl::default();
        let gate = crate::scanner::tests::register_gate(&control, ScanPhase::Sorting);
        let mut children = snapshot.nodes[snapshot.roots[0]].children.clone();
        std::thread::scope(|scope| {
            let worker =
                scope.spawn(|| index.sort_nodes(&mut children, &SearchQuery::default(), &control));
            gate.arrived.wait();
            control.cancel();
            gate.release.wait();
            assert!(worker.join().unwrap().is_err());
        });
    }

    #[test]
    fn supplied_sibling_sort_rejects_invalid_indices() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let mut nodes = vec![snapshot.nodes.len()];

        assert!(
            index
                .sort_nodes(&mut nodes, &SearchQuery::default(), &ScanControl::default())
                .is_err()
        );
    }

    #[test]
    fn snapshot_search_does_not_read_or_change_live_files() {
        let (directory, snapshot) = fixture();
        let index = SearchIndex::new(snapshot);
        fs::remove_file(directory.path().join("Документ.RS")).unwrap();
        let query = SearchQuery {
            text: "документ".into(),
            ..SearchQuery::default()
        };

        assert_eq!(
            index.search(&query, &ScanControl::default()).unwrap().len(),
            1
        );
    }

    #[test]
    fn search_honors_cancellation_and_rejects_inverted_ranges() {
        let (_directory, snapshot) = fixture();
        let index = SearchIndex::new(snapshot);
        let control = ScanControl::default();
        control.cancelled.store(true, Ordering::Relaxed);

        assert!(index.search(&SearchQuery::default(), &control).is_err());
        let query = SearchQuery {
            min_size: Some(10),
            max_size: Some(1),
            ..SearchQuery::default()
        };
        assert!(index.search(&query, &ScanControl::default()).is_err());
    }

    #[test]
    fn parallel_merge_sort_matches_the_reference_order_across_many_chunks() {
        let pool = thread_pool(4).unwrap();
        let mut indices: Vec<_> = (0..25000).rev().collect();
        let compare = |left: &usize, right: &usize| {
            (left % 37).cmp(&(right % 37)).then_with(|| right.cmp(left))
        };
        let mut expected = indices.clone();
        expected.sort_unstable_by(compare);

        sort_cancellable(&mut indices, &compare, &pool, &ScanControl::default()).unwrap();

        assert_eq!(indices, expected);
    }

    #[test]
    fn cancellation_interrupts_an_active_sort() {
        let pool = thread_pool(2).unwrap();
        let mut indices: Vec<_> = (0..25000).rev().collect();
        let control = ScanControl::default();
        let comparisons = std::sync::atomic::AtomicUsize::new(0);
        let compare = |left: &usize, right: &usize| {
            if comparisons.fetch_add(1, Ordering::Relaxed) == 1000 {
                control.cancel();
            }
            left.cmp(right)
        };

        assert!(sort_cancellable(&mut indices, &compare, &pool, &control).is_err());
        assert!(comparisons.load(Ordering::Relaxed) < 25000);
    }

    #[test]
    fn merge_sort_reports_each_pass_without_exceeding_total_work() {
        let pool = thread_pool(2).unwrap();
        let mut indices: Vec<_> = (0..25000).rev().collect();
        let control = ScanControl::default();
        let compare = |left: &usize, right: &usize| {
            assert!(
                control.work_done.load(Ordering::Relaxed)
                    <= control.work_total.load(Ordering::Relaxed)
            );
            left.cmp(right)
        };
        sort_cancellable(&mut indices, &compare, &pool, &control).unwrap();
        assert_eq!(control.work_total.load(Ordering::Relaxed), 125000);
        assert_eq!(control.work_done.load(Ordering::Relaxed), 125000);
    }

    #[test]
    fn saved_queries_default_old_profiles_and_roundtrip_filters() {
        let default: SearchQuery = serde_json::from_str("{}").unwrap();
        assert!(default.descending);
        assert!(!default.empty_directories);
        let query = SearchQuery {
            text: "Русский".into(),
            owner: Some(42),
            min_size: Some(1024),
            kinds: vec![EntryKind::File],
            ..SearchQuery::default()
        };
        let restored: SearchQuery =
            serde_json::from_value(serde_json::to_value(&query).unwrap()).unwrap();
        assert_eq!(restored.text, query.text);
        assert_eq!(restored.kinds, query.kinds);
        assert_eq!(restored.owner, query.owner);
        assert_eq!(restored.min_size, query.min_size);
    }

    #[test]
    fn index_building_can_be_cancelled_while_paused() {
        let (_directory, snapshot) = fixture();
        let control = ScanControl::default();
        control.pause();
        let ready = std::sync::Barrier::new(2);

        let result = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                ready.wait();
                SearchIndex::with_control(snapshot, 2, &control)
            });
            ready.wait();
            std::thread::sleep(std::time::Duration::from_millis(10));
            control.cancel();
            worker.join().unwrap()
        });

        assert!(result.is_err());
    }

    #[test]
    fn empty_directory_search_checks_children_and_errors_and_combines_filters() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("parent/child")).unwrap();
        fs::create_dir(directory.path().join("empty-target")).unwrap();
        fs::create_dir(directory.path().join("empty-ignored")).unwrap();
        fs::create_dir(directory.path().join("unreadable")).unwrap();
        fs::write(directory.path().join("zero-byte-file"), []).unwrap();
        let mut snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        for node in &mut snapshot.nodes {
            if node.name() == "unreadable" {
                node.error = Some("Permission denied".into());
            }
            if node.name() == "empty-target" {
                node.owner = Some(42);
            }
        }
        let snapshot = Arc::new(snapshot);
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let mut query = SearchQuery {
            empty_directories: true,
            sort: SearchSort::Name,
            descending: false,
            ..SearchQuery::default()
        };

        let found = index.search(&query, &ScanControl::default()).unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|&index| snapshot.nodes[index].name())
            .collect();

        assert_eq!(names, ["child", "empty-ignored", "empty-target"]);
        query.text = "TARGET".into();
        query.owner = Some(42);
        query.kinds = vec![EntryKind::Directory];
        query.min_size = Some(0);
        query.max_size = Some(0);
        let found = index.search(&query, &ScanControl::default()).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(snapshot.nodes[found[0]].name(), "empty-target");
    }

    #[test]
    fn empty_directory_search_refuses_cancelled_partial_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let mut snapshot = scan(
            &[directory.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        snapshot.cancelled = true;
        let index = SearchIndex::new(Arc::new(snapshot));
        let query = SearchQuery {
            empty_directories: true,
            ..SearchQuery::default()
        };

        assert_eq!(
            index.search(&query, &ScanControl::default()).unwrap_err(),
            "Refresh a cancelled scan before searching for empty folders"
        );
    }

    #[test]
    fn empty_directory_candidates_can_hide_excluded_contents_but_cleanup_refuses_them() {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("candidate")).unwrap();
        fs::write(fixture.path().join("candidate/hidden.tmp"), b"keep").unwrap();
        let snapshot = Arc::new(
            scan(
                &[fixture.path().into()],
                &ScanOptions {
                    exclude: vec![r"hidden\.tmp$".into()],
                    ..ScanOptions::default()
                },
                &ScanControl::default(),
            )
            .unwrap(),
        );
        let index = SearchIndex::new(Arc::clone(&snapshot));
        let found = index
            .search(
                &SearchQuery {
                    empty_directories: true,
                    ..SearchQuery::default()
                },
                &ScanControl::default(),
            )
            .unwrap();
        assert_eq!(found.len(), 1);
        let candidate = &snapshot.nodes[found[0]];
        assert_eq!(candidate.name(), "candidate");
        assert!(fs::read_dir(&candidate.path).unwrap().next().is_some());
        #[cfg(feature = "gui")]
        assert!(crate::actions::validate_empty_folder(candidate).is_err());
    }
}
