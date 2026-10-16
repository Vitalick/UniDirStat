//! Geometry shared by the desktop maps and headless consumers.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashSet},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use crate::model::{EntryKind, Snapshot};

/// An axis-aligned rectangle in display coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A node's rectangle in a treemap or flame view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub node: usize,
    pub rect: Rect,
}

/// Splits the longest rectangle edge at a balanced weight boundary.
///
/// Weights are stably sorted from largest to smallest. Zero weights and invalid
/// bounds are ignored. Coordinates use `f32`, so subpixel tiles can collapse.
pub fn treemap(weights: &[(usize, u64)], bounds: Rect) -> Vec<Tile> {
    if !valid_bounds(bounds) {
        return Vec::new();
    }
    let mut weights: Vec<_> = weights
        .iter()
        .copied()
        .filter(|entry| entry.1 > 0)
        .collect();
    weights.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let mut tiles = Vec::with_capacity(weights.len());
    let mut pending = vec![(weights.as_slice(), bounds)];
    while let Some((entries, bounds)) = pending.pop() {
        if entries.len() <= 1 {
            if let Some(&(node, _)) = entries.first() {
                tiles.push(Tile { node, rect: bounds });
            }
            continue;
        }
        let total: f64 = entries.iter().map(|entry| entry.1 as f64).sum();
        let mut split = 1;
        let mut left_total = entries[0].1 as f64;
        while split + 1 < entries.len() {
            let next_total = left_total + entries[split].1 as f64;
            if (next_total - total / 2.0).abs() > (left_total - total / 2.0).abs() {
                break;
            }
            left_total = next_total;
            split += 1;
        }
        let fraction = (left_total / total) as f32;
        let (first, second) = if bounds.width >= bounds.height {
            let width = bounds.width * fraction;
            (
                Rect { width, ..bounds },
                Rect {
                    x: bounds.x + width,
                    width: bounds.width - width,
                    ..bounds
                },
            )
        } else {
            let height = bounds.height * fraction;
            (
                Rect { height, ..bounds },
                Rect {
                    y: bounds.y + height,
                    height: bounds.height - height,
                    ..bounds
                },
            )
        };
        pending.push((&entries[split..], second));
        pending.push((&entries[..split], first));
    }
    tiles
}

fn valid_bounds(bounds: Rect) -> bool {
    bounds.x.is_finite()
        && bounds.y.is_finite()
        && bounds.width.is_finite()
        && bounds.height.is_finite()
        && bounds.width > 0.0
        && bounds.height > 0.0
        && (bounds.x + bounds.width).is_finite()
        && (bounds.y + bounds.height).is_finite()
}

/// Lays out roots and descendants in proportional horizontal depth bands.
///
/// Displays at most 64 levels; select a descendant as a new root to explore
/// deeper trees. Empty input, zero-size nodes and invalid bounds yield no tiles.
pub fn flame(snapshot: &Snapshot, roots: &[usize], allocated: bool, bounds: Rect) -> Vec<Tile> {
    if !valid_bounds(bounds) {
        return Vec::new();
    }
    let sectors = sunburst(snapshot, roots, allocated);
    let bands = sectors
        .iter()
        .map(|sector| sector.depth.min(63))
        .max()
        .unwrap_or(0)
        + 1;
    let height = bounds.height / bands as f32;
    sectors
        .into_iter()
        .filter(|sector| sector.depth < 64)
        .map(|sector| {
            let left = sector.start / std::f32::consts::TAU;
            let right = sector.end / std::f32::consts::TAU;
            Tile {
                node: sector.node,
                rect: Rect {
                    x: bounds.x + bounds.width * left,
                    y: bounds.y + height * sector.depth as f32,
                    width: bounds.width * (right - left),
                    height,
                },
            }
        })
        .collect()
}

/// A node's angular interval in radians, with roots at depth zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sector {
    pub node: usize,
    pub start: f32,
    pub end: f32,
    pub depth: usize,
}

/// Partitions one full turn among the roots, then each parent among its children.
///
/// Traversal is iterative and has no depth cap. Invalid indices, repeated nodes
/// and zero-size nodes are skipped. Children retain their snapshot order.
pub fn sunburst(snapshot: &Snapshot, roots: &[usize], allocated: bool) -> Vec<Sector> {
    let mut sectors = Vec::new();
    let mut visited = vec![false; snapshot.nodes.len()];
    let mut pending = vec![(roots, 0.0, std::f64::consts::TAU, 0)];
    while let Some((indices, start, end, depth)) = pending.pop() {
        let mut entries = Vec::new();
        for &index in indices {
            let Some(node) = snapshot.nodes.get(index) else {
                continue;
            };
            let size = node.size(allocated);
            if visited[index] || size == 0 {
                continue;
            }
            visited[index] = true;
            entries.push((index, size));
        }
        let total: f64 = entries.iter().map(|entry| entry.1 as f64).sum();
        let mut consumed = 0.0;
        let mut cursor = start;
        for (node, size) in entries {
            consumed += size as f64;
            let next = (start + (end - start) * (consumed / total)).min(end);
            sectors.push(Sector {
                node,
                start: cursor as f32,
                end: next as f32,
                depth,
            });
            let children = &snapshot.nodes[node].children;
            if !children.is_empty() {
                pending.push((children.as_slice(), cursor, next, depth + 1));
            }
            cursor = next;
        }
    }
    sectors
}

/// Cancellation and counts of completed and discovered layout work items.
#[derive(Default)]
pub struct LayoutControl {
    cancelled: AtomicBool,
    processed: AtomicU64,
    total: AtomicU64,
}

impl LayoutControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn progress(&self) -> (u64, u64) {
        let processed = self.processed.load(Ordering::Acquire);
        let total = self.total.load(Ordering::Acquire);
        (processed, total)
    }

    fn add_work(&self, count: usize) {
        self.total.fetch_add(count as u64, Ordering::Release);
    }

    fn advance(&self) -> Option<()> {
        if self.is_cancelled() {
            return None;
        }
        self.processed.fetch_add(1, Ordering::Release);
        Some(())
    }
}

/// Visible files, complete directory regions and aggregated tiny contents.
#[derive(Default)]
pub struct TreemapLayout {
    pub tiles: Vec<Tile>,
    pub frames: Vec<Tile>,
    pub aggregates: Vec<Tile>,
}

/// Builds file rectangles within contiguous, proportional directory regions.
/// Returns `None` when cancelled. Tiny contents retain their share of the area.
pub fn treemap_for_snapshot(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
) -> Option<Vec<Tile>> {
    Some(treemap_hierarchy(snapshot, roots, allocated, bounds, control)?.tiles)
}

/// Builds a cancellable hierarchy with explicit regions for omitted contents.
/// Aggregate nodes identify the containing directory where one is available.
pub fn treemap_hierarchy(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
) -> Option<TreemapLayout> {
    if control.is_cancelled() {
        return None;
    }
    let mut result = TreemapLayout::default();
    if !valid_bounds(bounds) {
        return Some(result);
    }
    let mut visited = HashSet::new();
    let mut groups = vec![(roots, bounds, None)];
    while let Some((indices, bounds, owner)) = groups.pop() {
        let mut weights = Vec::with_capacity(indices.len());
        control.add_work(indices.len());
        for &index in indices {
            control.advance()?;
            let Some(node) = snapshot.nodes.get(index) else {
                continue;
            };
            if node.size(allocated) > 0
                && matches!(node.kind, EntryKind::File | EntryKind::Directory)
                && visited.insert(index)
            {
                weights.push((index, node.size(allocated)));
            }
        }
        if weights.is_empty() {
            if let Some(node) = owner {
                result.aggregates.push(Tile { node, rect: bounds });
            }
            continue;
        }
        let aggregate_node = owner.unwrap_or(weights[0].0);
        let partitions = partition_visible(&weights, bounds, control)?;
        control.add_work(partitions.len());
        for (index, rect) in partitions {
            control.advance()?;
            let Some(node) = index else {
                result.aggregates.push(Tile {
                    node: aggregate_node,
                    rect,
                });
                continue;
            };
            let tile = Tile { node, rect };
            if snapshot.nodes[node].kind == EntryKind::Directory {
                result.frames.push(tile);
                groups.push((snapshot.nodes[node].children.as_slice(), rect, Some(node)));
            } else {
                result.tiles.push(tile);
            }
        }
    }
    Some(result)
}

fn partition_visible(
    weights: &[(usize, u64)],
    bounds: Rect,
    control: &LayoutControl,
) -> Option<Vec<(Option<usize>, Rect)>> {
    let mut total = 0_u128;
    control.add_work(weights.len());
    for &(_, size) in weights {
        control.advance()?;
        total += u128::from(size);
    }
    if total == 0 {
        return Some(Vec::new());
    }
    let mut heap = BinaryHeap::new();
    control.add_work(weights.len());
    for (order, &(node, size)) in weights.iter().enumerate() {
        control.advance()?;
        if size == 0 {
            continue;
        }
        heap.push((u128::from(size), Reverse(order), Some(node)));
    }
    let mut sorted = Vec::with_capacity(heap.len());
    control.add_work(heap.len());
    while let Some((size, _, node)) = heap.pop() {
        control.advance()?;
        sorted.push((node, size));
    }
    let mut prefixes = Vec::with_capacity(sorted.len() + 1);
    prefixes.push(0_u128);
    let mut consumed = 0_u128;
    control.add_work(sorted.len());
    for &(_, size) in &sorted {
        control.advance()?;
        consumed += size;
        prefixes.push(consumed);
    }
    let mut tiles = Vec::new();
    let mut pending = vec![(0, sorted.len(), bounds)];
    control.add_work(1);
    while let Some((start, end, bounds)) = pending.pop() {
        control.advance()?;
        if bounds.width < 1.0 || bounds.height < 1.0 {
            tiles.push((None, bounds));
            continue;
        }
        if end - start == 1 {
            tiles.push((sorted[start].0, bounds));
            continue;
        }
        let total = prefixes[end] - prefixes[start];
        let half = total as f64 / 2.0;
        let mut split = start
            + 1
            + prefixes[start + 1..end]
                .partition_point(|&prefix| ((prefix - prefixes[start]) as f64) < half);
        split = split.min(end - 1);
        if split > start + 1 {
            let previous = (prefixes[split - 1] - prefixes[start]) as f64;
            let current = (prefixes[split] - prefixes[start]) as f64;
            if (previous - half).abs() < (current - half).abs() {
                split -= 1;
            }
        }
        let fraction = (prefixes[split] - prefixes[start]) as f64 / total as f64;
        let (first, second) = if bounds.width >= bounds.height {
            let width = bounds.width * fraction as f32;
            (
                Rect { width, ..bounds },
                Rect {
                    x: bounds.x + width,
                    width: bounds.width - width,
                    ..bounds
                },
            )
        } else {
            let height = bounds.height * fraction as f32;
            (
                Rect { height, ..bounds },
                Rect {
                    y: bounds.y + height,
                    height: bounds.height - height,
                    ..bounds
                },
            )
        };
        control.add_work(2);
        pending.push((split, end, second));
        pending.push((start, split, first));
    }
    Some(tiles)
}

fn hierarchy_visible(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    depth_limit: usize,
    minimum_span: f64,
    control: &LayoutControl,
) -> Option<(Vec<Sector>, usize)> {
    if control.is_cancelled() {
        return None;
    }
    let mut sectors = Vec::new();
    let mut max_depth = 0;
    let mut visited = vec![false; snapshot.nodes.len()];
    let mut pending = vec![(roots, 0.0, std::f64::consts::TAU, 0)];
    control.add_work(roots.len());
    while let Some((indices, start, end, depth)) = pending.pop() {
        let mut entries = Vec::new();
        let mut total = 0.0;
        for &index in indices {
            control.advance()?;
            let Some(node) = snapshot.nodes.get(index) else {
                continue;
            };
            let size = node.size(allocated);
            if visited[index] || size == 0 {
                continue;
            }
            visited[index] = true;
            total += size as f64;
            entries.push((index, size));
        }
        let mut consumed = 0.0;
        let mut cursor = start;
        control.add_work(entries.len());
        for (node, size) in entries {
            control.advance()?;
            consumed += size as f64;
            let next = (start + (end - start) * (consumed / total)).min(end);
            if next - cursor >= minimum_span {
                sectors.push(Sector {
                    node,
                    start: cursor as f32,
                    end: next as f32,
                    depth,
                });
                max_depth = max_depth.max(depth);
                let children = &snapshot.nodes[node].children;
                if depth + 1 < depth_limit && !children.is_empty() {
                    control.add_work(children.len());
                    pending.push((children.as_slice(), cursor, next, depth + 1));
                }
            }
            cursor = next;
        }
    }
    Some((sectors, max_depth))
}

/// Builds cancellable flame geometry, pruning subpixel branches and deep levels.
pub fn flame_visible(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
) -> Option<Vec<Tile>> {
    if control.is_cancelled() {
        return None;
    }
    if !valid_bounds(bounds) {
        return Some(Vec::new());
    }
    let bands = (bounds.height / 24.0).floor().max(1.0) as usize;
    let (sectors, _) = hierarchy_visible(
        snapshot,
        roots,
        allocated,
        bands,
        std::f64::consts::TAU / f64::from(bounds.width),
        control,
    )?;
    let height = bounds.height / bands as f32;
    let mut tiles = Vec::with_capacity(sectors.len());
    control.add_work(sectors.len());
    for sector in sectors {
        control.advance()?;
        let width = bounds.width * ((sector.end - sector.start) / std::f32::consts::TAU);
        if width >= 1.0 && height >= 1.0 {
            tiles.push(Tile {
                node: sector.node,
                rect: Rect {
                    x: bounds.x + bounds.width * (sector.start / std::f32::consts::TAU),
                    y: bounds.y + height * sector.depth as f32,
                    width,
                    height,
                },
            });
        }
    }
    Some(tiles)
}

/// Builds at most twelve visible sunburst rings and skips subpixel branches.
pub fn sunburst_visible(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
) -> Option<Vec<Sector>> {
    if control.is_cancelled() {
        return None;
    }
    if !valid_bounds(bounds) {
        return Some(Vec::new());
    }
    let radius = bounds.width.min(bounds.height) / 2.0 - 5.0;
    if radius <= 2.0 {
        return Some(Vec::new());
    }
    let depth_limit = (radius as usize / 3).saturating_sub(1).clamp(1, 12);
    let (sectors, max_depth) = hierarchy_visible(
        snapshot,
        roots,
        allocated,
        depth_limit,
        1.0 / f64::from(radius),
        control,
    )?;
    let band = radius / (max_depth + 2) as f32;
    let mut visible = Vec::with_capacity(sectors.len());
    control.add_work(sectors.len());
    for sector in sectors {
        control.advance()?;
        let outer = band * (sector.depth + 2) as f32 - 2.0;
        if band > 2.0 && (sector.end - sector.start) * outer >= 1.0 {
            visible.push(sector);
        }
    }
    Some(visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryKind, Node};
    use std::{f32::consts::TAU, path::PathBuf};

    fn bounds() -> Rect {
        Rect {
            x: 10.0,
            y: 20.0,
            width: 120.0,
            height: 80.0,
        }
    }

    fn fixture() -> Snapshot {
        let nodes = [
            ("root", None, vec![1, 4], 100, 100),
            ("root/folder", Some(0), vec![2, 3], 60, 20),
            ("root/folder/first", Some(1), vec![], 45, 15),
            ("root/folder/second", Some(1), vec![], 15, 5),
            ("root/third", Some(0), vec![], 40, 80),
        ]
        .into_iter()
        .map(
            |(path, parent, children, logical_size, allocated_size)| Node {
                path: PathBuf::from(path),
                parent,
                kind: if children.is_empty() {
                    EntryKind::File
                } else {
                    EntryKind::Directory
                },
                children,
                logical_size,
                allocated_size: Some(allocated_size),
                files: 0,
                directories: 0,
                modified: None,
                readonly: false,
                permissions: None,
                owner: None,
                file_id: None,
                error: None,
            },
        )
        .collect();
        Snapshot {
            version: 1,
            roots: vec![0],
            nodes,
            errors: vec![],
            started: 0,
            elapsed_ms: 0,
            cancelled: false,
        }
    }

    fn sector(sectors: &[Sector], node: usize) -> Sector {
        *sectors.iter().find(|sector| sector.node == node).unwrap()
    }

    #[test]
    fn treemap_assigns_area_in_proportion_to_each_weight() {
        let tiles = treemap(&[(3, 6), (2, 3), (1, 1)], bounds());
        let areas: Vec<_> = tiles
            .iter()
            .map(|tile| (tile.node, tile.rect.width * tile.rect.height))
            .collect();

        assert_eq!(areas.len(), 3);
        for (node, expected) in [(3, 5760.0), (2, 2880.0), (1, 960.0)] {
            let actual = areas.iter().find(|entry| entry.0 == node).unwrap().1;
            assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
        }
    }

    #[test]
    fn treemap_tiles_do_not_overlap_or_escape_the_bounds() {
        let bounds = bounds();
        let tiles = treemap(&[(0, 50), (1, 25), (2, 12), (3, 8), (4, 5)], bounds);

        assert_eq!(tiles.len(), 5);
        for (index, tile) in tiles.iter().enumerate() {
            let rect = tile.rect;
            assert!(rect.x >= bounds.x && rect.y >= bounds.y);
            assert!(rect.x + rect.width <= bounds.x + bounds.width + 0.001);
            assert!(rect.y + rect.height <= bounds.y + bounds.height + 0.001);
            for other in &tiles[index + 1..] {
                let overlap_width = (rect.x + rect.width).min(other.rect.x + other.rect.width)
                    - rect.x.max(other.rect.x);
                let overlap_height = (rect.y + rect.height).min(other.rect.y + other.rect.height)
                    - rect.y.max(other.rect.y);
                assert!(overlap_width <= 0.001 || overlap_height <= 0.001);
            }
        }
    }

    #[test]
    fn treemap_balances_equal_weights_into_squares() {
        let tiles = treemap(
            &[(0, 1), (1, 1), (2, 1), (3, 1)],
            Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        );

        assert_eq!(tiles.len(), 4);
        assert!(
            tiles
                .iter()
                .all(|tile| tile.rect.width == 50.0 && tile.rect.height == 50.0)
        );
    }

    #[test]
    fn treemap_ignores_zero_weights() {
        assert_eq!(
            treemap(&[(0, 0), (1, 7), (2, 0)], bounds()),
            vec![Tile {
                node: 1,
                rect: bounds(),
            }]
        );
    }

    #[test]
    fn treemap_returns_no_tiles_for_empty_input_or_invalid_bounds() {
        assert!(treemap(&[], bounds()).is_empty());
        assert!(treemap(&[(0, 0)], bounds()).is_empty());
        for invalid in [
            Rect {
                width: 0.0,
                ..bounds()
            },
            Rect {
                height: -1.0,
                ..bounds()
            },
            Rect {
                x: f32::NAN,
                ..bounds()
            },
            Rect {
                width: f32::INFINITY,
                ..bounds()
            },
            Rect {
                x: f32::MAX,
                width: f32::MAX,
                ..bounds()
            },
        ] {
            assert!(treemap(&[(0, 1)], invalid).is_empty());
        }
    }

    #[test]
    fn treemap_keeps_extreme_weights_finite_and_ties_stable() {
        let tiles = treemap(&[(8, u64::MAX), (4, u64::MAX), (9, 1)], bounds());

        assert_eq!(
            tiles.iter().map(|tile| tile.node).collect::<Vec<_>>(),
            [8, 4, 9]
        );
        assert!(tiles.iter().all(|tile| {
            [tile.rect.x, tile.rect.y, tile.rect.width, tile.rect.height]
                .iter()
                .all(|value| value.is_finite())
        }));
    }

    #[test]
    fn sunburst_places_children_inside_their_parent_angles() {
        let snapshot = fixture();
        let sectors = sunburst(&snapshot, &[0], false);

        assert_eq!(sectors.len(), 5);
        assert_eq!(
            sector(&sectors, 0),
            Sector {
                node: 0,
                start: 0.0,
                end: TAU,
                depth: 0
            }
        );
        assert!((sector(&sectors, 1).end - TAU * 0.6).abs() < 0.00001);
        assert!((sector(&sectors, 2).end - TAU * 0.45).abs() < 0.00001);
        for child in &sectors[1..] {
            let parent = sector(&sectors, snapshot.nodes[child.node].parent.unwrap());
            assert_eq!(child.depth, parent.depth + 1);
            assert!(child.start >= parent.start && child.end <= parent.end);
        }
        assert_eq!(sector(&sectors, 3).end, sector(&sectors, 1).end);
        assert_eq!(sector(&sectors, 4).end, TAU);
    }

    #[test]
    fn sunburst_uses_allocated_size_and_selected_roots() {
        let sectors = sunburst(&fixture(), &[1, 4], true);

        assert_eq!(sectors.len(), 4);
        assert!((sector(&sectors, 1).end - TAU * 0.2).abs() < 0.00001);
        assert!((sector(&sectors, 2).end - TAU * 0.15).abs() < 0.00001);
        assert_eq!(
            (sector(&sectors, 1).depth, sector(&sectors, 4).depth),
            (0, 0)
        );
    }

    #[test]
    fn flame_places_proportional_children_in_the_next_band() {
        let snapshot = fixture();
        let tiles = flame(&snapshot, &[0], false, bounds());

        assert_eq!(tiles.len(), 5);
        for tile in &tiles {
            let expected_width = match tile.node {
                0 => 120.0,
                1 => 72.0,
                2 => 54.0,
                3 => 18.0,
                4 => 48.0,
                _ => unreachable!(),
            };
            assert!((tile.rect.width - expected_width).abs() < 0.0001);
            if let Some(parent) = snapshot.nodes[tile.node].parent {
                let parent_rect = tiles.iter().find(|tile| tile.node == parent).unwrap().rect;
                assert!(tile.rect.x >= parent_rect.x);
                assert!(
                    tile.rect.x + tile.rect.width <= parent_rect.x + parent_rect.width + 0.0001
                );
                assert!((tile.rect.y - parent_rect.y - parent_rect.height).abs() < 0.0001);
            }
        }
    }

    #[test]
    fn hierarchy_handles_empty_zero_and_invalid_indices_without_panicking() {
        let mut snapshot = fixture();
        snapshot.nodes[2].logical_size = 0;
        snapshot.nodes[1].children.push(99);
        snapshot.nodes[1].children.push(0);

        assert!(sunburst(&snapshot, &[], false).is_empty());
        assert!(sunburst(&snapshot, &[99], false).is_empty());
        assert_eq!(sunburst(&snapshot, &[0], false).len(), 4);
        assert!(
            flame(
                &snapshot,
                &[0],
                false,
                Rect {
                    height: 0.0,
                    ..bounds()
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn hierarchy_is_iterative_and_flame_caps_visible_depth() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = (0_usize..10_000)
            .map(|index| Node {
                parent: index.checked_sub(1),
                children: if index < 9_999 {
                    vec![index + 1]
                } else {
                    vec![]
                },
                logical_size: 1,
                ..template.clone()
            })
            .collect();

        assert_eq!(sunburst(&snapshot, &[0], false).len(), 10_000);
        let tiles = flame(&snapshot, &[0], false, bounds());
        assert_eq!(tiles.len(), 64);
        assert!(
            tiles
                .iter()
                .all(|tile| tile.rect.y + tile.rect.height <= 100.001)
        );
    }

    #[test]
    fn visible_maps_stop_when_cancelled() {
        let snapshot = fixture();
        let control = LayoutControl::default();
        control.cancel();

        assert!(treemap_for_snapshot(&snapshot, &[0], false, bounds(), &control).is_none());
        assert!(flame_visible(&snapshot, &[0], false, bounds(), &control).is_none());
        assert!(sunburst_visible(&snapshot, &[0], false, bounds(), &control).is_none());
    }

    #[test]
    fn visible_treemap_preserves_area_when_subpixel_files_are_pruned() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = std::iter::once(Node {
            children: (1..=1_001).collect(),
            kind: EntryKind::Directory,
            logical_size: 11_000,
            ..template.clone()
        })
        .chain((1..=1_001).map(|index| Node {
            logical_size: if index == 1 { 10_000 } else { 1 },
            ..template.clone()
        }))
        .collect();
        let control = LayoutControl::default();
        let tiles = treemap_for_snapshot(
            &snapshot,
            &[0],
            false,
            Rect {
                width: 10.0,
                height: 10.0,
                ..bounds()
            },
            &control,
        )
        .unwrap();

        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].node, 1);
        assert!((tiles[0].rect.width * tiles[0].rect.height - 100.0 * 10.0 / 11.0).abs() < 0.001);
        let (processed, total) = control.progress();
        assert!(processed > 1_000 && processed == total);
    }

    #[test]
    fn visible_treemap_keeps_each_directory_in_one_contiguous_region() {
        let tiles =
            treemap_for_snapshot(&fixture(), &[0], false, bounds(), &LayoutControl::default())
                .unwrap();
        let first = tiles.iter().find(|tile| tile.node == 2).unwrap().rect;
        let second = tiles.iter().find(|tile| tile.node == 3).unwrap().rect;
        let width = (first.x + first.width).max(second.x + second.width) - first.x.min(second.x);
        let height = (first.y + first.height).max(second.y + second.height) - first.y.min(second.y);

        assert!((width * height - 5_760.0).abs() < 0.01);
    }

    #[test]
    fn hierarchy_frames_enclose_descendants_and_keep_proportional_area() {
        let geometry =
            treemap_hierarchy(&fixture(), &[0], false, bounds(), &LayoutControl::default())
                .unwrap();
        let folder = geometry
            .frames
            .iter()
            .find(|tile| tile.node == 1)
            .unwrap()
            .rect;

        assert!((folder.width * folder.height - 5_760.0).abs() < 0.01);
        for node in [2, 3] {
            let child = geometry
                .tiles
                .iter()
                .find(|tile| tile.node == node)
                .unwrap()
                .rect;
            assert!(child.x >= folder.x && child.y >= folder.y);
            assert!(child.x + child.width <= folder.x + folder.width + 0.001);
            assert!(child.y + child.height <= folder.y + folder.height + 0.001);
        }
    }

    #[test]
    fn tiny_contents_have_an_explicit_aggregate_without_losing_area() {
        let mut snapshot = fixture();
        snapshot.nodes[0].logical_size = 1_001;
        snapshot.nodes[1].logical_size = 1_001;
        snapshot.nodes[2].logical_size = 1_000;
        snapshot.nodes[3].logical_size = 1;
        snapshot.nodes[4].logical_size = 0;
        let geometry = treemap_hierarchy(
            &snapshot,
            &[0],
            false,
            Rect {
                width: 10.0,
                height: 10.0,
                ..bounds()
            },
            &LayoutControl::default(),
        )
        .unwrap();

        assert_eq!(geometry.aggregates.len(), 1);
        assert_eq!(geometry.aggregates[0].node, 1);
        let aggregate = geometry.aggregates[0].rect;
        assert!((aggregate.width * aggregate.height - 100.0 / 1_001.0).abs() < 0.001);
        let area: f32 = geometry
            .tiles
            .iter()
            .chain(&geometry.aggregates)
            .map(|tile| tile.rect.width * tile.rect.height)
            .sum();
        assert!((area - 100.0).abs() < 0.001);
    }

    #[test]
    fn hierarchical_treemap_sizes_folders_and_files_by_allocated_bytes() {
        let geometry =
            treemap_hierarchy(&fixture(), &[0], true, bounds(), &LayoutControl::default()).unwrap();

        for (node, area) in [(1, 1_920.0), (2, 1_440.0), (3, 480.0), (4, 7_680.0)] {
            let rect = geometry
                .frames
                .iter()
                .chain(&geometry.tiles)
                .find(|tile| tile.node == node)
                .unwrap()
                .rect;
            assert!((rect.width * rect.height - area).abs() < 0.01);
        }
    }

    #[test]
    fn visible_hierarchies_do_not_build_subpixel_siblings() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = std::iter::once(Node {
            children: (1..=10_000).collect(),
            kind: EntryKind::Directory,
            logical_size: 10_000,
            ..template.clone()
        })
        .chain((1..=10_000).map(|_| Node {
            logical_size: 1,
            ..template.clone()
        }))
        .collect();
        let bounds = Rect {
            width: 20.0,
            height: 20.0,
            ..bounds()
        };

        let tiles =
            flame_visible(&snapshot, &[0], false, bounds, &LayoutControl::default()).unwrap();
        let sectors =
            sunburst_visible(&snapshot, &[0], false, bounds, &LayoutControl::default()).unwrap();

        assert_eq!(tiles.iter().map(|tile| tile.node).collect::<Vec<_>>(), [0]);
        assert_eq!(
            sectors.iter().map(|sector| sector.node).collect::<Vec<_>>(),
            [0]
        );
    }

    #[test]
    fn visible_flame_stops_at_the_depth_limit() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = (0_usize..10_000)
            .map(|index| Node {
                children: if index < 9_999 {
                    vec![index + 1]
                } else {
                    vec![]
                },
                logical_size: 1,
                ..template.clone()
            })
            .collect();
        let control = LayoutControl::default();

        let tiles = flame_visible(&snapshot, &[0], false, bounds(), &control).unwrap();

        assert_eq!(tiles.len(), 3);
        assert!(tiles.iter().all(|tile| tile.rect.height >= 24.0));
        assert!(control.progress().0 < 512);
    }

    #[test]
    fn flame_bands_stay_fixed_when_more_width_reveals_a_deeper_branch() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = [
            (vec![1, 2], 100),
            (vec![], 90),
            (vec![3], 10),
            (vec![4], 10),
            (vec![], 10),
        ]
        .into_iter()
        .map(|(children, size)| Node {
            kind: if children.is_empty() {
                EntryKind::File
            } else {
                EntryKind::Directory
            },
            children,
            logical_size: size,
            ..template.clone()
        })
        .collect();
        let coarse = flame_visible(
            &snapshot,
            &[0],
            false,
            Rect {
                width: 8.0,
                height: 96.0,
                ..bounds()
            },
            &LayoutControl::default(),
        )
        .unwrap();
        let fine = flame_visible(
            &snapshot,
            &[0],
            false,
            Rect {
                width: 80.0,
                height: 96.0,
                ..bounds()
            },
            &LayoutControl::default(),
        )
        .unwrap();

        assert!(!coarse.iter().any(|tile| tile.node == 4));
        assert!(fine.iter().any(|tile| tile.node == 4));
        for node in [0, 1] {
            let before = coarse.iter().find(|tile| tile.node == node).unwrap().rect;
            let after = fine.iter().find(|tile| tile.node == node).unwrap().rect;
            assert_eq!((before.y, before.height), (after.y, after.height));
        }
    }

    #[test]
    fn treemap_zoom_detail_does_not_move_an_existing_large_file() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = std::iter::once(Node {
            children: (1..=20_001).collect(),
            kind: EntryKind::Directory,
            logical_size: 30_000,
            ..template.clone()
        })
        .chain((1..=20_001).map(|index| Node {
            logical_size: if index == 1 { 10_000 } else { 1 },
            ..template.clone()
        }))
        .collect();
        let coarse = treemap_for_snapshot(
            &snapshot,
            &[0],
            false,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
            },
            &LayoutControl::default(),
        )
        .unwrap();
        let fine = treemap_for_snapshot(
            &snapshot,
            &[0],
            false,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 200.0,
            },
            &LayoutControl::default(),
        )
        .unwrap();
        let before = coarse.iter().find(|tile| tile.node == 1).unwrap().rect;
        let after = fine.iter().find(|tile| tile.node == 1).unwrap().rect;

        for (before, after) in [
            (before.x, after.x),
            (before.y, after.y),
            (before.width, after.width),
            (before.height, after.height),
        ] {
            assert!((before * 2.0 - after).abs() < 0.001);
        }
    }
}
