//! Geometry shared by the desktop maps and headless consumers.

use crate::model::Snapshot;

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
}
