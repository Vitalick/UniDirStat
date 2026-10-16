use std::{fs, path::Path, process::Command, time::UNIX_EPOCH};

use crate::{
    model::{EntryKind, Node},
    scanner::path_identity,
};

pub fn validate_live_node(node: &Node) -> Result<fs::Metadata, String> {
    if node.parent.is_none() {
        return Err(
            "Scan roots cannot be moved or trashed. Scan their parent to manage them.".into(),
        );
    }
    if !matches!(node.kind, EntryKind::File | EntryKind::Directory) {
        return Err("Cleanup is only available for regular files and folders.".into());
    }
    if node.file_id.is_none() {
        return Err("File identity is unavailable. Use your file manager for cleanup.".into());
    }
    let metadata = fs::symlink_metadata(&node.path).map_err(|error| error.to_string())?;
    let canonical = fs::canonicalize(&node.path).map_err(|error| error.to_string())?;
    if canonical != node.path
        || metadata.is_symlink()
        || metadata.is_dir() != (node.kind == EntryKind::Directory)
        || (node.kind == EntryKind::File && !metadata.is_file())
    {
        return Err("The path changed since scanning. Refresh before cleanup.".into());
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|time| time.as_secs());
    if (node.kind == EntryKind::File && metadata.len() != node.logical_size)
        || modified != node.modified
        || (node.file_id.is_some() && path_identity(&node.path, &metadata) != node.file_id)
    {
        return Err("The item changed since scanning. Refresh before cleanup.".into());
    }
    Ok(metadata)
}

pub fn trash_node(node: &Node) -> Result<(), String> {
    validate_live_node(node)?;
    trash::delete(&node.path).map_err(|error| error.to_string())
}

pub fn validate_empty_folder(node: &Node) -> Result<(), String> {
    validate_live_node(node)?;
    if node.kind != EntryKind::Directory {
        return Err("The item must be an empty folder".into());
    }
    if fs::read_dir(&node.path)
        .map_err(|error| error.to_string())?
        .next()
        .transpose()
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Err("The folder is not empty. Refresh before cleanup.".into());
    }
    Ok(())
}

pub fn trash_empty_folder(node: &Node) -> Result<(), String> {
    validate_empty_folder(node)?;
    trash_node(node)
}

pub fn permission_details(path: &Path) -> Result<String, String> {
    let path = fs::canonicalize(path).map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("/bin/ls");
        command.args(["-lde", "--"]);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = Command::new("icacls.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut command = Command::new("getfacl");
        command.args(["-p", "--"]);
        command
    };
    let output = command
        .arg(&path)
        .output()
        .map_err(|error| format!("Cannot inspect ACLs with the system permission tool: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn move_node(node: &Node, destination: &Path) -> Result<(), String> {
    validate_live_node(node)?;
    let destination = fs::canonicalize(destination).map_err(|error| error.to_string())?;
    if !destination.is_dir() {
        return Err("The destination must be a folder.".into());
    }
    let name = node.path.file_name().ok_or("The file has no name")?;
    let target = destination.join(name);
    validate_live_node(node)?;
    renamore::rename_exclusive(&node.path, &target).map_err(|error| format!("Move failed: {error}. Atomic moves require the same filesystem and no existing destination. Use your file manager for cross-volume moves."))
}

pub fn open_path(path: &Path, reveal: bool) -> Result<(), String> {
    let target = if reveal {
        path.parent().unwrap_or(path)
    } else {
        path
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        if reveal {
            command.arg("-R");
        }
        command.arg("--").arg(if reveal { path } else { target });
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        if reveal && path.is_file() {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(target);
        }
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(target);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{ScanControl, ScanOptions, scan};

    #[test]
    fn empty_folder_tool_rejects_nonempty_folders_and_files() {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("empty")).unwrap();
        fs::create_dir(fixture.path().join("occupied")).unwrap();
        fs::write(fixture.path().join("occupied/data"), b"keep").unwrap();
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .unwrap();
        let named = |name: &str| {
            snapshot
                .nodes
                .iter()
                .find(|node| node.name() == name)
                .unwrap()
        };
        assert!(validate_empty_folder(named("empty")).is_ok());
        assert!(validate_empty_folder(named("occupied")).is_err());
        assert!(validate_empty_folder(named("data")).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn permission_inspection_reads_acl_without_changing_contents() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("-quoted file");
        fs::write(&path, b"keep").unwrap();
        let details = permission_details(&path).unwrap();
        assert!(details.contains("-quoted file"));
        assert_eq!(fs::read(path).unwrap(), b"keep");
    }

    #[test]
    fn changed_file_is_refused_before_cleanup() {
        let fixture = tempfile::tempdir().expect("fixture");
        let path = fixture.path().join("data");
        fs::write(&path, "before").expect("file");
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .expect("scan");
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.kind == EntryKind::File)
            .expect("node");
        fs::write(path, "changed contents").expect("change");
        assert!(validate_live_node(node).is_err());
    }

    #[test]
    fn move_does_not_overwrite_destination() {
        let fixture = tempfile::tempdir().expect("fixture");
        let destination = tempfile::tempdir().expect("destination");
        fs::write(fixture.path().join("data"), "original").expect("file");
        fs::write(destination.path().join("data"), "keep me").expect("existing");
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .expect("scan");
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.kind == EntryKind::File)
            .expect("node");
        assert!(move_node(node, destination.path()).is_err());
        assert_eq!(
            fs::read(destination.path().join("data")).expect("kept"),
            b"keep me"
        );
        assert!(node.path.exists());
    }

    #[test]
    fn move_preserves_contents_and_removes_the_original_path() {
        let fixture = tempfile::tempdir().expect("fixture");
        let destination = tempfile::tempdir().expect("destination");
        fs::write(fixture.path().join("data"), b"original contents").expect("file");
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .expect("scan");
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.kind == EntryKind::File)
            .expect("node");
        move_node(node, destination.path()).expect("atomic move");
        assert!(!node.path.exists());
        assert_eq!(
            fs::read(destination.path().join("data")).expect("moved"),
            b"original contents"
        );
    }

    #[test]
    fn move_preserves_an_entire_folder() {
        let fixture = tempfile::tempdir().expect("fixture");
        let destination = tempfile::tempdir().expect("destination");
        fs::create_dir(fixture.path().join("folder")).expect("folder");
        fs::write(fixture.path().join("folder/data"), b"nested contents").expect("file");
        let snapshot = scan(
            &[fixture.path().into()],
            &ScanOptions::default(),
            &ScanControl::default(),
        )
        .expect("scan");
        let node = snapshot
            .nodes
            .iter()
            .find(|node| node.parent.is_some() && node.kind == EntryKind::Directory)
            .expect("node");
        move_node(node, destination.path()).expect("atomic folder move");
        assert!(!node.path.exists());
        assert_eq!(
            fs::read(destination.path().join("folder/data")).expect("moved"),
            b"nested contents"
        );
    }
}
