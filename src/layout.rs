//! Geometry shared by the desktop maps and headless consumers.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap, HashSet},
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

/// Packs descending weights into rows with a minimum proportion of 0.4.
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
        .map(|(node, size)| (node, u128::from(size)))
        .collect();
    weights.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let mut tiles = Vec::with_capacity(weights.len());
    let control = LayoutControl::default();
    if let Some(rows) = partition_rows(&weights, bounds, &control) {
        for row in rows {
            if let Some(children) = partition_row(
                &weights[row.start..row.end],
                row.rect,
                bounds.width >= bounds.height,
                &control,
            ) {
                tiles.extend(children);
            }
        }
    }
    tiles
}

struct Row {
    start: usize,
    end: usize,
    rect: Rect,
}

fn partition_rows(
    weights: &[(usize, u128)],
    bounds: Rect,
    control: &LayoutControl,
) -> Option<Vec<Row>> {
    let total: u128 = weights.iter().map(|entry| entry.1).sum();
    if total == 0 {
        return Some(Vec::new());
    }
    let horizontal = bounds.width >= bounds.height;
    let (extent, columns, origin) = if horizontal {
        (bounds.height, bounds.width, bounds.y)
    } else {
        (bounds.width, bounds.height, bounds.x)
    };
    let normalized_width = f64::from(columns) / f64::from(extent);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut top = f64::from(origin);
    while start < weights.len() {
        let mut end = start;
        let mut row_weight = 0_u128;
        while end < weights.len() {
            control.add_work(1);
            control.advance()?;
            let next_weight = row_weight + weights[end].1;
            let fraction = next_weight as f64 / total as f64;
            let proportion =
                weights[end].1 as f64 / total as f64 * normalized_width / fraction / fraction;
            if end > start && proportion < 0.4 {
                break;
            }
            row_weight = next_weight;
            end += 1;
        }
        let next_top = if end == weights.len() {
            f64::from(origin) + f64::from(extent)
        } else {
            top + row_weight as f64 / total as f64 * f64::from(extent)
        };
        let row_origin = top as f32;
        let row_extent = (next_top as f32 - row_origin).max(0.0);
        rows.push(Row {
            start,
            end,
            rect: if horizontal {
                Rect {
                    y: row_origin,
                    height: row_extent,
                    ..bounds
                }
            } else {
                Rect {
                    x: row_origin,
                    width: row_extent,
                    ..bounds
                }
            },
        });
        top = next_top;
        start = end;
    }
    Some(rows)
}

fn partition_row(
    weights: &[(usize, u128)],
    bounds: Rect,
    horizontal: bool,
    control: &LayoutControl,
) -> Option<Vec<Tile>> {
    let total: u128 = weights.iter().map(|entry| entry.1).sum();
    let (origin, extent) = if horizontal {
        (bounds.x, bounds.width)
    } else {
        (bounds.y, bounds.height)
    };
    let mut consumed = 0_u128;
    let mut left = origin;
    let mut tiles = Vec::with_capacity(weights.len());
    control.add_work(weights.len());
    for &(node, weight) in weights {
        control.advance()?;
        consumed += weight;
        let right = if consumed == total {
            origin + extent
        } else {
            (f64::from(origin) + consumed as f64 / total as f64 * f64::from(extent)) as f32
        };
        tiles.push(Tile {
            node,
            rect: if horizontal {
                Rect {
                    x: left,
                    width: (right - left).max(0.0),
                    ..bounds
                }
            } else {
                Rect {
                    y: left,
                    height: (right - left).max(0.0),
                    ..bounds
                }
            },
        });
        left = right;
    }
    Some(tiles)
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
    pub aggregates: Vec<Aggregate>,
}

/// Same-color tiny files belonging to one containing directory.
#[derive(Debug, PartialEq)]
pub struct Aggregate {
    pub owner: Option<usize>,
    pub rect: Rect,
    pub color: usize,
    pub bytes: u128,
    pub count: usize,
    pub extensions: Vec<String>,
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

/// Builds a cancellable hierarchy, grouping tiny files by their extension hash.
pub fn treemap_hierarchy(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
) -> Option<TreemapLayout> {
    treemap_hierarchy_with_colors(snapshot, roots, allocated, bounds, control, |extension| {
        extension.bytes().fold(0_usize, |hash, byte| {
            hash.wrapping_mul(31).wrapping_add(usize::from(byte))
        })
    })
}

/// Builds a hierarchy with tiny file groups keyed by the caller's color palette.
pub fn treemap_hierarchy_with_colors(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
    color_key: impl Fn(&str) -> usize,
) -> Option<TreemapLayout> {
    treemap_hierarchy_with_depth(
        snapshot,
        roots,
        allocated,
        bounds,
        control,
        usize::MAX,
        color_key,
    )
}

/// Stops at the requested number of levels, retaining deeper folders as colored regions.
pub fn treemap_hierarchy_with_depth(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
    depth_limit: usize,
    color_key: impl Fn(&str) -> usize,
) -> Option<TreemapLayout> {
    if control.is_cancelled() {
        return None;
    }
    let mut result = TreemapLayout::default();
    if !valid_bounds(bounds) {
        return Some(result);
    }
    let mut visited = HashSet::new();
    let mut groups = vec![(roots, bounds, None, 0_usize)];
    while let Some((indices, bounds, owner, depth)) = groups.pop() {
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
                weights.push((index, u128::from(node.size(allocated))));
            }
        }
        if weights.is_empty() {
            continue;
        }
        let weights = sort_weights(weights, control)?;
        let rows = partition_rows(&weights, bounds, control)?;
        let horizontal = bounds.width >= bounds.height;
        let mut tiny_rows: Option<(usize, usize, Rect)> = None;
        for row in rows {
            let tiles = partition_row(&weights[row.start..row.end], row.rect, horizontal, control)?;
            if tiles.iter().all(|tile| {
                snapshot.nodes[tile.node].kind == EntryKind::File && !readable_tile(tile.rect)
            }) {
                if let Some((_, end, rect)) = &mut tiny_rows {
                    *end = row.end;
                    *rect = enclosing_rect(*rect, row.rect);
                } else {
                    tiny_rows = Some((row.start, row.end, row.rect));
                }
                continue;
            }
            if let Some((start, end, rect)) = tiny_rows.take() {
                result.aggregates.extend(aggregate_files(
                    snapshot,
                    &weights[start..end],
                    owner,
                    allocated,
                    rect,
                    control,
                    &color_key,
                )?);
            }
            let mut tiny_start = None;
            for (offset, &tile) in tiles.iter().enumerate() {
                control.add_work(1);
                control.advance()?;
                if snapshot.nodes[tile.node].kind == EntryKind::File && !readable_tile(tile.rect) {
                    tiny_start.get_or_insert(offset);
                    continue;
                }
                if let Some(start) = tiny_start.take() {
                    let rect = enclosing_rect(tiles[start].rect, tiles[offset - 1].rect);
                    result.aggregates.extend(aggregate_files(
                        snapshot,
                        &weights[row.start + start..row.start + offset],
                        owner,
                        allocated,
                        rect,
                        control,
                        &color_key,
                    )?);
                }
                if snapshot.nodes[tile.node].kind == EntryKind::Directory {
                    result.frames.push(tile);
                    if depth.saturating_add(1) >= depth_limit.max(1) {
                        result.aggregates.push(aggregate_directory(
                            snapshot, tile, allocated, control, &color_key,
                        )?);
                    } else {
                        groups.push((
                            snapshot.nodes[tile.node].children.as_slice(),
                            tile.rect,
                            Some(tile.node),
                            depth + 1,
                        ));
                    }
                } else {
                    result.tiles.push(tile);
                }
            }
            if let Some(start) = tiny_start {
                let rect = enclosing_rect(tiles[start].rect, tiles[tiles.len() - 1].rect);
                result.aggregates.extend(aggregate_files(
                    snapshot,
                    &weights[row.start + start..row.end],
                    owner,
                    allocated,
                    rect,
                    control,
                    &color_key,
                )?);
            }
        }
        if let Some((start, end, rect)) = tiny_rows {
            result.aggregates.extend(aggregate_files(
                snapshot,
                &weights[start..end],
                owner,
                allocated,
                rect,
                control,
                &color_key,
            )?);
        }
    }
    Some(result)
}

fn aggregate_directory(
    snapshot: &Snapshot,
    tile: Tile,
    allocated: bool,
    control: &LayoutControl,
    color_key: &impl Fn(&str) -> usize,
) -> Option<Aggregate> {
    let mut extensions = BTreeMap::<String, u128>::new();
    let mut count = 0;
    let mut pending = vec![tile.node];
    let mut visited = HashSet::new();
    while let Some(index) = pending.pop() {
        control.add_work(1);
        control.advance()?;
        let Some(node) = snapshot.nodes.get(index) else {
            continue;
        };
        if !visited.insert(index) {
            continue;
        }
        if node.kind == EntryKind::File {
            *extensions.entry(node.extension()).or_default() += u128::from(node.size(allocated));
            count += 1;
        } else if node.kind == EntryKind::Directory {
            pending.extend(&node.children);
        }
    }
    let extension = extensions
        .iter()
        .max_by_key(|(_, bytes)| *bytes)
        .map_or("(no extension)", |(extension, _)| extension.as_str());
    Some(Aggregate {
        owner: Some(tile.node),
        rect: tile.rect,
        color: color_key(extension),
        bytes: u128::from(snapshot.nodes[tile.node].size(allocated)),
        count,
        extensions: extensions.into_keys().collect(),
    })
}

fn sort_weights(
    weights: Vec<(usize, u128)>,
    control: &LayoutControl,
) -> Option<Vec<(usize, u128)>> {
    let mut heap = BinaryHeap::new();
    control.add_work(weights.len());
    for (order, (node, size)) in weights.into_iter().enumerate() {
        control.advance()?;
        heap.push((size, Reverse(order), node));
    }
    let mut sorted = Vec::with_capacity(heap.len());
    control.add_work(heap.len());
    while let Some((size, _, node)) = heap.pop() {
        control.advance()?;
        sorted.push((node, size));
    }
    Some(sorted)
}

fn readable_tile(rect: Rect) -> bool {
    rect.width >= 1.0 && rect.height >= 1.0
}

fn enclosing_rect(first: Rect, second: Rect) -> Rect {
    let x = first.x.min(second.x);
    let y = first.y.min(second.y);
    Rect {
        x,
        y,
        width: (first.x + first.width).max(second.x + second.width) - x,
        height: (first.y + first.height).max(second.y + second.height) - y,
    }
}

fn aggregate_files(
    snapshot: &Snapshot,
    weights: &[(usize, u128)],
    owner: Option<usize>,
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
    color_key: &impl Fn(&str) -> usize,
) -> Option<Vec<Aggregate>> {
    let mut colors = BTreeMap::<usize, (u128, usize, BTreeSet<String>)>::new();
    control.add_work(weights.len());
    for &(node, _) in weights {
        control.advance()?;
        let extension = snapshot.nodes[node].extension();
        let group = colors.entry(color_key(&extension)).or_default();
        group.0 += u128::from(snapshot.nodes[node].size(allocated));
        group.1 += 1;
        group.2.insert(extension);
    }
    let mut aggregates: Vec<_> = colors
        .into_iter()
        .map(|(color, (bytes, count, extensions))| Aggregate {
            owner,
            rect: bounds,
            color,
            bytes,
            count,
            extensions: extensions.into_iter().collect(),
        })
        .collect();
    let weights = sort_weights(
        aggregates
            .iter()
            .enumerate()
            .map(|(index, group)| (index, group.bytes))
            .collect(),
        control,
    )?;
    for row in partition_rows(&weights, bounds, control)? {
        for tile in partition_row(
            &weights[row.start..row.end],
            row.rect,
            bounds.width >= bounds.height,
            control,
        )? {
            aggregates[tile.node].rect = tile.rect;
        }
    }
    Some(aggregates)
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
    flame_visible_with_depth(snapshot, roots, allocated, bounds, control, usize::MAX)
}

/// Builds flame geometry with an additional level limit relative to its roots.
pub fn flame_visible_with_depth(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
    depth_limit: usize,
) -> Option<Vec<Tile>> {
    if control.is_cancelled() {
        return None;
    }
    if !valid_bounds(bounds) {
        return Some(Vec::new());
    }
    let bands = ((bounds.height / 24.0).floor().max(1.0) as usize).min(depth_limit.max(1));
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
    sunburst_visible_with_depth(snapshot, roots, allocated, bounds, control, usize::MAX)
}

/// Builds visible sunburst rings with an additional level limit relative to its roots.
pub fn sunburst_visible_with_depth(
    snapshot: &Snapshot,
    roots: &[usize],
    allocated: bool,
    bounds: Rect,
    control: &LayoutControl,
    depth_limit: usize,
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
    let depth_limit = (radius as usize / 3)
        .saturating_sub(1)
        .clamp(1, 12)
        .min(depth_limit.max(1));
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
                path: PathBuf::from(path).into(),
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
                scan_failed: false,
                scan_warning: false,
                depth_limited: false,
                scan_duration_ns: 0,
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
    fn treemap_packs_equal_weights_into_classic_rows() {
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
        assert_eq!(tiles[0].rect.height, 75.0);
        assert!((tiles[0].rect.width - 100.0 / 3.0).abs() < 0.001);
        assert_eq!(
            tiles[3].rect,
            Rect {
                x: 0.0,
                y: 75.0,
                width: 100.0,
                height: 25.0
            }
        );
    }

    #[test]
    fn treemap_rows_follow_the_parent_orientation_and_proportion_rule() {
        let weights = [(0, 6), (1, 3), (2, 1)];
        for (width, height, expected) in [
            (
                100.0,
                100.0,
                [
                    (0.0, 0.0, 100.0, 60.0),
                    (0.0, 60.0, 75.0, 40.0),
                    (75.0, 60.0, 25.0, 40.0),
                ],
            ),
            (
                100.0,
                200.0,
                [
                    (0.0, 0.0, 90.0, 200.0 / 3.0 * 2.0),
                    (0.0, 200.0 / 3.0 * 2.0, 90.0, 200.0 / 3.0),
                    (90.0, 0.0, 10.0, 200.0),
                ],
            ),
        ] {
            let tiles = treemap(
                &weights,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height,
                },
            );
            for (tile, (x, y, width, height)) in tiles.iter().zip(expected) {
                for (actual, expected) in [
                    (tile.rect.x, x),
                    (tile.rect.y, y),
                    (tile.rect.width, width),
                    (tile.rect.height, height),
                ] {
                    assert!((actual - expected).abs() < 0.001, "{actual} != {expected}");
                }
            }
        }
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
    fn dense_pixel_sized_files_stay_individual_instead_of_one_color_block() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        let count = 35_701;
        snapshot.nodes = std::iter::once(Node {
            kind: EntryKind::Directory,
            children: (1..=count).collect(),
            logical_size: count as u64,
            ..template.clone()
        })
        .chain((1..=count).map(|_| Node {
            parent: Some(0),
            logical_size: 1,
            ..template.clone()
        }))
        .collect();
        let geometry = treemap_hierarchy_with_colors(
            &snapshot,
            &[0],
            false,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 500.0,
            },
            &LayoutControl::default(),
            |_| 0,
        )
        .unwrap();
        assert_eq!(geometry.tiles.len(), count);
        assert!(geometry.aggregates.is_empty());
        let area: f32 = geometry
            .tiles
            .iter()
            .map(|tile| tile.rect.width * tile.rect.height)
            .sum();
        assert!((area - 500_000.0).abs() < 300.0, "covered {area}");
    }

    #[test]
    fn tiny_files_keep_separate_colors_without_one_block_per_row() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = std::iter::once(Node {
            path: PathBuf::from("root").into(),
            parent: None,
            children: (1..=20_000).collect(),
            kind: EntryKind::Directory,
            logical_size: 20_000,
            ..template.clone()
        })
        .chain((1..=20_000).map(|index| {
            Node {
                path: PathBuf::from(format!(
                    "root/{index}.{}",
                    if index % 2 == 0 { "txt" } else { "bin" }
                ))
                .into(),
                parent: Some(0),
                logical_size: 1,
                ..template.clone()
            }
        }))
        .collect();
        let geometry = treemap_hierarchy(
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

        assert!(geometry.tiles.is_empty());
        assert_eq!(geometry.aggregates.len(), 2);
        for aggregate in &geometry.aggregates {
            assert_eq!(aggregate.owner, Some(0));
            assert_eq!(aggregate.count, 10_000);
            assert_eq!(aggregate.bytes, 10_000);
            assert_eq!(aggregate.extensions.len(), 1);
            assert!((aggregate.rect.width * aggregate.rect.height - 10_000.0).abs() < 0.01);
        }
        let shared_color = treemap_hierarchy_with_colors(
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
            |_| 7,
        )
        .unwrap();
        assert_eq!(shared_color.aggregates.len(), 1);
        let aggregate = &shared_color.aggregates[0];
        assert_eq!(
            (aggregate.color, aggregate.count, aggregate.bytes),
            (7, 20_000, 20_000)
        );
        assert_eq!(aggregate.extensions, ["bin", "txt"]);
        assert_eq!(
            aggregate.rect,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0
            }
        );
    }

    #[test]
    fn tiny_groups_keep_their_containing_folders_and_reveal_files_when_zoomed() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = vec![
            Node {
                kind: EntryKind::Directory,
                children: vec![1, 2],
                logical_size: 512,
                ..template.clone()
            },
            Node {
                kind: EntryKind::Directory,
                parent: Some(0),
                children: (3..259).collect(),
                logical_size: 256,
                ..template.clone()
            },
            Node {
                kind: EntryKind::Directory,
                parent: Some(0),
                children: (259..515).collect(),
                logical_size: 256,
                ..template.clone()
            },
        ];
        snapshot.nodes.extend((3..515).map(|index| Node {
            path: PathBuf::from(format!("root/{index}.txt")).into(),
            parent: Some(if index < 259 { 1 } else { 2 }),
            logical_size: 1,
            ..template.clone()
        }));
        let coarse = treemap_hierarchy_with_colors(
            &snapshot,
            &[0],
            false,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 32.0,
                height: 16.0,
            },
            &LayoutControl::default(),
            |_| 0,
        )
        .unwrap();
        assert!(coarse.tiles.is_empty());
        assert_eq!(coarse.aggregates.len(), 2);
        for owner in [1, 2] {
            let aggregate = coarse
                .aggregates
                .iter()
                .find(|group| group.owner == Some(owner))
                .unwrap();
            let frame = coarse
                .frames
                .iter()
                .find(|tile| tile.node == owner)
                .unwrap();
            assert_eq!(aggregate.rect, frame.rect);
            assert_eq!((aggregate.count, aggregate.bytes), (256, 256));
        }
        let fine = treemap_hierarchy_with_colors(
            &snapshot,
            &[0],
            false,
            Rect {
                x: 0.0,
                y: 0.0,
                width: 512.0,
                height: 256.0,
            },
            &LayoutControl::default(),
            |_| 0,
        )
        .unwrap();
        assert_eq!(fine.tiles.len(), 512);
        assert!(fine.aggregates.is_empty());
    }

    #[test]
    fn tiny_root_files_have_no_invented_owner_and_grouping_can_be_cancelled() {
        let mut snapshot = fixture();
        snapshot.nodes[2].logical_size = 1;
        snapshot.nodes[3].logical_size = 1;
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 0.5,
        };
        let geometry = treemap_hierarchy_with_colors(
            &snapshot,
            &[2, 3],
            false,
            bounds,
            &LayoutControl::default(),
            |_| 0,
        )
        .unwrap();
        assert_eq!(geometry.aggregates.len(), 1);
        assert_eq!(geometry.aggregates[0].owner, None);
        assert_eq!(
            (geometry.aggregates[0].count, geometry.aggregates[0].bytes),
            (2, 2)
        );
        let control = LayoutControl::default();
        assert!(
            treemap_hierarchy_with_colors(&snapshot, &[2, 3], false, bounds, &control, |_| {
                control.cancel();
                0
            })
            .is_none()
        );
    }

    #[test]
    fn treemap_descends_through_tiny_directories_without_gray_remainders_or_recursion() {
        let mut snapshot = fixture();
        let template = snapshot.nodes[2].clone();
        snapshot.nodes = (0_usize..10_000)
            .map(|index| Node {
                parent: index.checked_sub(1),
                kind: if index < 9_999 {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                children: if index < 9_999 {
                    vec![index + 1]
                } else {
                    vec![]
                },
                logical_size: 1,
                ..template.clone()
            })
            .collect();
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 0.25,
        };
        let geometry =
            treemap_hierarchy(&snapshot, &[0], false, bounds, &LayoutControl::default()).unwrap();
        assert_eq!(geometry.frames.len(), 9_999);
        assert_eq!(geometry.aggregates.len(), 1);
        assert_eq!(geometry.aggregates[0].owner, Some(9_998));
        assert_eq!(geometry.aggregates[0].rect, bounds);
        assert_eq!(geometry.aggregates[0].bytes, 1);
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
        assert_eq!(geometry.aggregates[0].owner, Some(1));
        assert_eq!(geometry.aggregates[0].count, 1);
        assert_eq!(geometry.aggregates[0].bytes, 1);
        let aggregate = geometry.aggregates[0].rect;
        assert!((aggregate.width * aggregate.height - 100.0 / 1_001.0).abs() < 0.001);
        let area: f32 = geometry
            .tiles
            .iter()
            .map(|tile| tile.rect.width * tile.rect.height)
            .sum::<f32>()
            + geometry
                .aggregates
                .iter()
                .map(|tile| tile.rect.width * tile.rect.height)
                .sum::<f32>();
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
