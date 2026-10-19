use std::borrow::Borrow;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A native path whose directory prefix can be shared by sibling entries.
#[derive(Clone)]
pub struct NodePath {
    prefix: Arc<Path>,
    name: Option<Box<OsStr>>,
}

pub(super) struct SharedPrefix(Arc<Path>);

impl Borrow<OsStr> for SharedPrefix {
    fn borrow(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

impl PartialEq for SharedPrefix {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_os_str() == other.0.as_os_str()
    }
}

impl Eq for SharedPrefix {}

impl Hash for SharedPrefix {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_os_str().hash(state);
    }
}

impl NodePath {
    pub(crate) fn from_parent(prefix: Arc<Path>, name: Box<OsStr>) -> Self {
        let mut components = Path::new(&name).components();
        if matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none()
            && Path::new(&name).file_name() == Some(name.as_ref())
        {
            Self {
                prefix,
                name: Some(name),
            }
        } else {
            prefix.join(name.as_ref()).into()
        }
    }

    pub(crate) fn root(path: PathBuf) -> Self {
        Self {
            prefix: Arc::from(path),
            name: None,
        }
    }

    pub fn to_path_buf(&self) -> PathBuf {
        self.name.as_ref().map_or_else(
            || self.prefix.to_path_buf(),
            |name| self.prefix.join(name.as_ref()),
        )
    }

    pub fn file_name(&self) -> Option<&OsStr> {
        self.name.as_deref().or_else(|| self.prefix.file_name())
    }

    pub fn extension(&self) -> Option<&OsStr> {
        self.file_name()
            .and_then(|name| Path::new(name).extension())
    }

    pub fn parent(&self) -> Option<&Path> {
        self.name
            .as_ref()
            .map_or_else(|| self.prefix.parent(), |_| Some(self.prefix.as_ref()))
    }

    fn components(&self) -> impl Iterator<Item = Component<'_>> {
        self.prefix.components().chain(
            self.name
                .as_deref()
                .into_iter()
                .flat_map(|name| Path::new(name).components()),
        )
    }

    pub(super) fn matches_path(&self, path: &Path) -> bool {
        self.components().eq(path.components())
    }

    pub(super) fn intern_prefix(&mut self, prefixes: &mut HashSet<SharedPrefix>) {
        // Raw native spelling must survive serialization even when Path considers it equivalent.
        if let Some(prefix) = prefixes.get(self.prefix.as_os_str()) {
            self.prefix = Arc::clone(&prefix.0);
        } else {
            prefixes.insert(SharedPrefix(Arc::clone(&self.prefix)));
        }
    }
}

impl From<PathBuf> for NodePath {
    fn from(path: PathBuf) -> Self {
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            // Preserve unusual spelling, including trailing separators and verbatim paths.
            if parent.join(name).as_os_str() == path.as_os_str() {
                return Self::from_parent(Arc::from(parent), name.into());
            }
        }
        Self::root(path)
    }
}

impl From<&Path> for NodePath {
    fn from(path: &Path) -> Self {
        path.to_path_buf().into()
    }
}

impl From<&str> for NodePath {
    fn from(path: &str) -> Self {
        Path::new(path).into()
    }
}

impl From<String> for NodePath {
    fn from(path: String) -> Self {
        PathBuf::from(path).into()
    }
}

impl PartialEq<PathBuf> for NodePath {
    fn eq(&self, other: &PathBuf) -> bool {
        self.matches_path(other)
    }
}

impl PartialEq<NodePath> for PathBuf {
    fn eq(&self, other: &NodePath) -> bool {
        other == self
    }
}

impl PartialEq for NodePath {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NodePath {}

impl PartialOrd for NodePath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NodePath {
    fn cmp(&self, other: &Self) -> Ordering {
        if Arc::ptr_eq(&self.prefix, &other.prefix)
            && let (Some(left), Some(right)) = (&self.name, &other.name)
        {
            return Path::new(left).cmp(Path::new(right));
        }
        self.components().cmp(other.components())
    }
}

impl Hash for NodePath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for component in self.components() {
            component.hash(state);
        }
    }
}

impl fmt::Display for NodePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_path_buf().display().fmt(formatter)
    }
}

impl fmt::Debug for NodePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_path_buf().fmt(formatter)
    }
}

impl Serialize for NodePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_path_buf().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NodePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        PathBuf::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    #[test]
    fn siblings_and_clones_share_the_native_parent() {
        let parent: Arc<Path> = Arc::from(std::env::temp_dir().join("shared-parent"));
        let left = NodePath::from_parent(Arc::clone(&parent), OsStr::new("first.dat").into());
        let right = NodePath::from_parent(Arc::clone(&parent), OsStr::new("second.dat").into());
        let cloned = left.clone();
        assert!(Arc::ptr_eq(&left.prefix, &right.prefix));
        assert!(Arc::ptr_eq(&left.prefix, &cloned.prefix));
        assert_eq!(left.to_path_buf(), parent.join("first.dat"));
        assert_eq!(left.extension(), Some(OsStr::new("dat")));
    }

    #[test]
    fn native_spelling_order_and_hash_match_path_semantics() {
        let mut paths: Vec<PathBuf> = [
            "",
            ".",
            "..",
            "a",
            "a/",
            "a/.",
            "a/b",
            "a//b",
            "a/b/c",
            "a/bb",
            "a/c",
            "/",
            "/a/b",
            "данные/файл.dat",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        #[cfg(windows)]
        paths.extend(
            [
                r"C:\",
                r"C:\data\file",
                r"c:\data\file",
                r"C:/data//file",
                r"\\?\C:\data\file",
                r"\\?\C:\data\.\file",
                r"\\?\C:\data\..\file",
                r"\\?\C:\data\\file",
                r"\\?\UNC\server\share\file",
                r"\\server\share\file",
                r"C:relative",
                r"C:.",
            ]
            .into_iter()
            .map(PathBuf::from),
        );
        paths.push(std::env::temp_dir().join("deep".repeat(300)).join("leaf"));
        for left in &paths {
            let stored_left = NodePath::from(left.as_path());
            assert_eq!(stored_left.to_path_buf().as_os_str(), left.as_os_str());
            assert_eq!(
                serde_json::to_value(&stored_left).unwrap(),
                serde_json::to_value(left).unwrap()
            );
            assert_eq!(stored_left.file_name(), left.file_name());
            assert_eq!(stored_left.parent(), left.parent());
            for right in &paths {
                let stored_right = NodePath::from(right.as_path());
                assert_eq!(stored_left.cmp(&stored_right), left.cmp(right));
                if left == right {
                    let hash = |path: &NodePath| {
                        let mut hasher = DefaultHasher::new();
                        path.hash(&mut hasher);
                        hasher.finish()
                    };
                    assert_eq!(hash(&stored_left), hash(&stored_right));
                }
            }
        }
    }

    #[test]
    fn serde_keeps_the_existing_full_path_representation() {
        let path = std::env::temp_dir().join("данные").join("file.txt");
        let stored = NodePath::from(path.clone());
        let json = serde_json::to_string(&stored).unwrap();
        assert_eq!(json, serde_json::to_string(&path).unwrap());
        assert_eq!(serde_json::from_str::<NodePath>(&json).unwrap(), stored);
    }

    #[test]
    fn prefix_interning_preserves_distinct_native_spelling() {
        let mut first = NodePath::from("parent//directory/one");
        let mut second = NodePath::from("parent/directory/two");
        let original = [first.to_path_buf(), second.to_path_buf()];
        let mut prefixes = HashSet::new();
        first.intern_prefix(&mut prefixes);
        second.intern_prefix(&mut prefixes);
        assert_eq!(first.to_path_buf().as_os_str(), original[0].as_os_str());
        assert_eq!(second.to_path_buf().as_os_str(), original[1].as_os_str());
        assert!(!Arc::ptr_eq(&first.prefix, &second.prefix));
    }

    #[test]
    fn native_non_unicode_names_survive_storage() {
        #[cfg(windows)]
        let name = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[0x66, 0xd800, 0x6f])
        };
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(vec![b'f', 0xff, b'o'])
        };
        let path = std::env::temp_dir().join(&name);
        let stored = NodePath::from(path.clone());
        assert_eq!(stored.to_path_buf(), path);
        assert_eq!(stored.file_name(), Some(name.as_os_str()));
        assert!(serde_json::to_string(&stored).is_err());
    }
}
