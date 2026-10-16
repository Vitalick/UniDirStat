use std::collections::HashSet;
use std::path::{Component, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Node {
    pub path: PathBuf,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub kind: EntryKind,
    pub logical_size: u64,
    pub allocated_size: Option<u64>,
    pub files: u64,
    pub directories: u64,
    pub modified: Option<u64>,
    pub readonly: bool,
    pub permissions: Option<u32>,
    pub owner: Option<u32>,
    pub file_id: Option<String>,
    pub error: Option<String>,
}

impl Node {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or(self.path.as_os_str())
            .to_string_lossy()
            .into_owned()
    }

    pub fn extension(&self) -> String {
        self.path
            .extension()
            .filter(|extension| !extension.is_empty())
            .map_or_else(
                || "(no extension)".to_owned(),
                |value| value.to_string_lossy().to_lowercase(),
            )
    }

    pub fn size(&self, allocated: bool) -> u64 {
        if allocated {
            self.allocated_size.unwrap_or(self.logical_size)
        } else {
            self.logical_size
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Snapshot {
    pub version: u32,
    pub roots: Vec<usize>,
    pub nodes: Vec<Node>,
    pub errors: Vec<String>,
    pub started: u64,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

impl Snapshot {
    pub fn total_size(&self, allocated: bool) -> u64 {
        self.roots
            .iter()
            .filter_map(|&root| self.nodes.get(root))
            .fold(0, |total, node| total.saturating_add(node.size(allocated)))
    }

    pub fn file_count(&self) -> u64 {
        self.roots
            .iter()
            .filter_map(|&root| self.nodes.get(root))
            .fold(0, |total, node| total.saturating_add(node.files))
    }

    pub fn directory_count(&self) -> u64 {
        self.roots
            .iter()
            .filter_map(|&root| self.nodes.get(root))
            .fold(0, |total, node| {
                total
                    .saturating_add(node.directories)
                    .saturating_add(u64::from(node.kind == EntryKind::Directory))
            })
    }

    pub fn validate(&self) -> Result<(), String> {
        const LATEST_TIMESTAMP: u64 = 253_402_300_799;
        if self.version != 1 {
            return Err(format!("Unsupported snapshot version: {}", self.version));
        }
        if self.started > LATEST_TIMESTAMP
            || self.elapsed_ms > 3_155_760_000_000
            || self.errors.iter().any(|error| error.len() > 1_048_576)
        {
            return Err("Snapshot metadata exceeds supported limits".to_owned());
        }
        if self.nodes.is_empty() {
            return if self.roots.is_empty() && self.cancelled {
                Ok(())
            } else {
                Err("Snapshot has no scanned roots".to_owned())
            };
        }
        let mut root_set = HashSet::new();
        let mut root_paths = HashSet::new();
        for &root in &self.roots {
            let node = self.nodes.get(root).ok_or("Invalid root index")?;
            if !root_set.insert(root) || node.parent.is_some() {
                return Err("Duplicate root or root with a parent".to_owned());
            }
            if node.kind == EntryKind::Directory {
                root_paths.insert(node.path.as_path());
            }
        }
        let mut paths = HashSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            if node.path.as_os_str().is_empty()
                || !node.path.is_absolute()
                || node.path.as_os_str().len() > 32_768
                || node.path.as_os_str().as_encoded_bytes().contains(&0)
                || node
                    .path
                    .components()
                    .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
                || !paths.insert(node.path.as_path())
                || node
                    .modified
                    .is_some_and(|modified| modified > LATEST_TIMESTAMP)
                || node.permissions.is_some_and(|mode| mode > 0xffff)
                || node
                    .file_id
                    .as_ref()
                    .is_some_and(|identity| identity.len() > 256)
                || node
                    .error
                    .as_ref()
                    .is_some_and(|error| error.len() > 1_048_576)
            {
                return Err(format!("Invalid metadata at node {index}"));
            }
            match node.parent {
                Some(parent) => {
                    let parent_node = self.nodes.get(parent).ok_or("Invalid parent index")?;
                    if parent == index
                        || parent_node.kind != EntryKind::Directory
                        || node.path.parent() != Some(parent_node.path.as_path())
                    {
                        return Err(format!("Invalid parent or path at node {index}"));
                    }
                }
                None if !root_set.contains(&index) => {
                    return Err(format!("Unlisted root at node {index}"));
                }
                None => {
                    if node
                        .path
                        .ancestors()
                        .skip(1)
                        .any(|ancestor| root_paths.contains(ancestor))
                    {
                        return Err("Snapshot roots overlap".to_owned());
                    }
                }
            }
            if node.kind != EntryKind::Directory && !node.children.is_empty() {
                return Err(format!("Non-directory has children at node {index}"));
            }
            let mut children = HashSet::new();
            for &child in &node.children {
                let child_node = self.nodes.get(child).ok_or("Invalid child index")?;
                if !children.insert(child) || child_node.parent != Some(index) {
                    return Err(format!(
                        "Duplicate child or incorrect parent at node {index}"
                    ));
                }
            }
        }
        let mut visited = vec![false; self.nodes.len()];
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut pending = self.roots.clone();
        while let Some(index) = pending.pop() {
            if std::mem::replace(&mut visited[index], true) {
                return Err("Snapshot contains a cycle or shared child".to_owned());
            }
            order.push(index);
            pending.extend(&self.nodes[index].children);
        }
        if order.len() != self.nodes.len() {
            return Err("Snapshot contains unreachable nodes".to_owned());
        }
        for index in order.into_iter().rev() {
            let node = &self.nodes[index];
            let (logical, allocated, files, directories) = match node.kind {
                EntryKind::Directory => {
                    let mut logical = 0_u64;
                    let mut allocated = Some(0_u64);
                    let mut files = 0_u64;
                    let mut directories = 0_u64;
                    for &child in &node.children {
                        let child_node = &self.nodes[child];
                        logical = logical.saturating_add(child_node.logical_size);
                        allocated = allocated
                            .zip(child_node.allocated_size)
                            .map(|(total, size)| total.saturating_add(size));
                        files = files.saturating_add(child_node.files);
                        directories = directories
                            .saturating_add(child_node.directories)
                            .saturating_add(u64::from(child_node.kind == EntryKind::Directory));
                    }
                    if node.children.is_empty() && node.allocated_size.is_none() {
                        allocated = None;
                    }
                    (logical, allocated, files, directories)
                }
                EntryKind::File => (node.logical_size, node.allocated_size, 1, 0),
                EntryKind::Symlink | EntryKind::Other => (0, node.allocated_size.map(|_| 0), 0, 0),
            };
            if (
                node.logical_size,
                node.allocated_size,
                node.files,
                node.directories,
            ) != (logical, allocated, files, directories)
            {
                return Err(format!("Incorrect aggregate at node {index}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Snapshot {
        let root = std::env::temp_dir().join("unidirstat-validation");
        Snapshot {
            version: 1,
            roots: vec![0],
            nodes: vec![
                Node {
                    path: root.clone(),
                    parent: None,
                    children: vec![1],
                    kind: EntryKind::Directory,
                    logical_size: 4,
                    allocated_size: Some(512),
                    files: 1,
                    directories: 0,
                    modified: Some(1),
                    readonly: false,
                    permissions: None,
                    owner: None,
                    file_id: None,
                    error: None,
                },
                Node {
                    path: root.join("photo.JPG"),
                    parent: Some(0),
                    children: vec![],
                    kind: EntryKind::File,
                    logical_size: 4,
                    allocated_size: Some(512),
                    files: 1,
                    directories: 0,
                    modified: Some(1),
                    readonly: false,
                    permissions: None,
                    owner: None,
                    file_id: None,
                    error: None,
                },
            ],
            errors: vec![],
            started: 1,
            elapsed_ms: 1,
            cancelled: false,
        }
    }

    #[test]
    fn validation_accepts_a_consistent_tree() {
        assert!(fixture().validate().is_ok());
    }

    #[test]
    fn validation_rejects_invalid_version_and_unreasonable_metadata() {
        let original = fixture();
        let mut changed = original.clone();
        changed.version = 2;
        assert!(changed.validate().is_err());
        changed = original.clone();
        changed.nodes[1].modified = Some(u64::MAX);
        assert!(changed.validate().is_err());
        changed = original;
        changed.nodes[1].permissions = Some(u32::MAX);
        assert!(changed.validate().is_err());
    }

    #[test]
    fn validation_rejects_bad_indices_cycles_and_unreachable_nodes() {
        let original = fixture();
        let mut changed = original.clone();
        changed.nodes[0].children.push(42);
        assert!(changed.validate().is_err());
        changed = original.clone();
        changed.nodes[1].kind = EntryKind::Directory;
        changed.nodes[1].children = vec![0];
        changed.nodes[0].parent = Some(1);
        assert!(changed.validate().is_err());
        changed = original;
        changed.nodes[0].children.clear();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn validation_rejects_duplicate_children_roots_and_paths() {
        let original = fixture();
        let mut changed = original.clone();
        changed.nodes[0].children.push(1);
        assert!(changed.validate().is_err());
        changed = original.clone();
        changed.roots.push(0);
        assert!(changed.validate().is_err());
        changed = original;
        changed.nodes[1].path = changed.nodes[0].path.clone();
        assert!(changed.validate().is_err());
    }

    #[test]
    fn validation_rejects_wrong_parent_paths_and_aggregates() {
        let original = fixture();
        let mut changed = original.clone();
        changed.nodes[1].path = std::env::temp_dir().join("outside.JPG");
        assert!(changed.validate().is_err());
        changed = original.clone();
        changed.nodes[0].logical_size = 3;
        assert!(changed.validate().is_err());
        changed = original.clone();
        changed.nodes[0].allocated_size = Some(4);
        assert!(changed.validate().is_err());
        changed = original;
        changed.nodes[1].files = 0;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn summary_uses_descendant_counts_and_lowercase_extensions() {
        let snapshot = fixture();
        assert_eq!(
            (
                snapshot.total_size(false),
                snapshot.total_size(true),
                snapshot.file_count(),
                snapshot.directory_count()
            ),
            (4, 512, 1, 1)
        );
        assert_eq!(snapshot.nodes[1].extension(), "jpg");
    }

    #[test]
    fn validation_handles_deep_trees_without_recursion() {
        let mut snapshot = fixture();
        let mut template = snapshot.nodes[0].clone();
        template.logical_size = 0;
        template.allocated_size = Some(0);
        template.files = 0;
        snapshot.nodes.clear();
        let mut path = template.path.clone();
        for index in 0_usize..1500 {
            let mut node = template.clone();
            node.path = path.clone();
            node.parent = index.checked_sub(1);
            node.children = if index < 1499 {
                vec![index + 1]
            } else {
                vec![]
            };
            node.directories = (1499 - index) as u64;
            snapshot.nodes.push(node);
            path.push("d");
        }
        assert!(snapshot.validate().is_ok());
    }

    #[test]
    fn validation_rejects_separate_roots_inside_another_root() {
        let mut snapshot = fixture();
        snapshot.nodes[0].children.clear();
        snapshot.nodes[0].logical_size = 0;
        snapshot.nodes[0].allocated_size = Some(0);
        snapshot.nodes[0].files = 0;
        snapshot.nodes[1].parent = None;
        snapshot.roots.push(1);
        assert!(snapshot.validate().is_err());
    }
}
