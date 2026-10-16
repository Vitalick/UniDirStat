use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Volume {
    pub path: PathBuf,
    pub name: String,
    pub total: u64,
    pub available: u64,
    pub system_data_group: bool,
}

pub fn mounted_volumes() -> Vec<Volume> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let volumes = disks
        .iter()
        .map(|disk| Volume {
            path: disk.mount_point().to_path_buf(),
            name: disk.name().to_string_lossy().into_owned(),
            total: disk.total_space(),
            available: disk.available_space(),
            system_data_group: false,
        })
        .collect();
    group_boot_volumes(volumes, shared_boot_namespace(&disks))
}

#[cfg(target_os = "macos")]
fn shared_boot_namespace(disks: &sysinfo::Disks) -> bool {
    use std::os::unix::fs::MetadataExt;

    let both_apfs = ["/", "/System/Volumes/Data"].iter().all(|path| {
        disks.iter().any(|disk| {
            disk.mount_point() == Path::new(path)
                && disk
                    .file_system()
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case("apfs"))
        })
    });
    both_apfs
        && ["Users", "Applications", "private"].iter().any(|name| {
            let system = std::fs::symlink_metadata(Path::new("/").join(name));
            let data = std::fs::symlink_metadata(Path::new("/System/Volumes/Data").join(name));
            match (system, data) {
                (Ok(system), Ok(data)) => {
                    system.is_dir()
                        && data.is_dir()
                        && system.dev() == data.dev()
                        && system.ino() == data.ino()
                }
                _ => false,
            }
        })
}

#[cfg(not(target_os = "macos"))]
fn shared_boot_namespace(_: &sysinfo::Disks) -> bool {
    false
}

fn group_boot_volumes(mut volumes: Vec<Volume>, same_group: bool) -> Vec<Volume> {
    if same_group
        && volumes.iter().any(|volume| volume.path == Path::new("/"))
        && volumes
            .iter()
            .any(|volume| volume.path == Path::new("/System/Volumes/Data"))
    {
        volumes.retain(|volume| volume.path != Path::new("/System/Volumes/Data"));
        for volume in &mut volumes {
            if volume.path == Path::new("/") {
                volume.system_data_group = true;
            }
        }
    }
    volumes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(path: &str) -> Volume {
        Volume {
            path: path.into(),
            name: "Same name".into(),
            total: 1024,
            available: 512,
            system_data_group: false,
        }
    }

    #[test]
    fn system_and_data_share_one_card_without_summing_container_capacity() {
        let volumes = group_boot_volumes(vec![volume("/"), volume("/System/Volumes/Data")], true);
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].path, Path::new("/"));
        assert!(volumes[0].system_data_group);
        assert_eq!((volumes[0].total, volumes[0].available), (1024, 512));
    }

    #[test]
    fn equal_names_and_capacities_do_not_merge_separate_disks_or_volumes() {
        let volumes = group_boot_volumes(
            vec![
                volume("/"),
                volume("/System/Volumes/Data"),
                volume("/Volumes/External"),
                volume("/Volumes/Another"),
            ],
            false,
        );
        assert_eq!(volumes.len(), 4);
    }

    #[test]
    fn data_volume_is_not_hidden_when_the_system_volume_is_absent() {
        let volumes = group_boot_volumes(vec![volume("/System/Volumes/Data")], true);
        assert_eq!(volumes.len(), 1);
    }
}
