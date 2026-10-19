use std::collections::HashMap;

use eframe::egui::Color32;
use unidirstat::analysis::ExtensionStats;

pub(crate) const FILE_PALETTE: [Color32; 18] = [
    Color32::from_rgb(101, 101, 255),
    Color32::from_rgb(255, 101, 101),
    Color32::from_rgb(101, 255, 101),
    Color32::from_rgb(229, 229, 0),
    Color32::from_rgb(0, 229, 229),
    Color32::from_rgb(229, 0, 229),
    Color32::from_rgb(255, 193, 10),
    Color32::from_rgb(44, 158, 255),
    Color32::from_rgb(255, 44, 158),
    Color32::from_rgb(158, 255, 44),
    Color32::from_rgb(193, 10, 255),
    Color32::from_rgb(44, 255, 158),
    Color32::from_rgb(255, 10, 193),
    Color32::from_rgb(10, 193, 255),
    Color32::from_rgb(255, 158, 44),
    Color32::from_rgb(10, 255, 193),
    Color32::from_rgb(158, 44, 255),
    Color32::from_rgb(153, 153, 153),
];

#[derive(Default)]
pub(crate) struct FileColors {
    indices: HashMap<String, usize>,
}

impl FileColors {
    pub(crate) fn from_extensions(extensions: &[ExtensionStats]) -> Self {
        let mut ranked: Vec<_> = extensions.iter().collect();
        ranked.sort_by(|first, second| {
            second
                .bytes
                .cmp(&first.bytes)
                .then_with(|| first.extension.cmp(&second.extension))
        });
        Self {
            indices: ranked
                .into_iter()
                .enumerate()
                .map(|(rank, stats)| (stats.extension.clone(), rank % (FILE_PALETTE.len() - 1)))
                .collect(),
        }
    }

    pub(crate) fn index(&self, extension: &str) -> usize {
        self.indices.get(extension).copied().unwrap_or_else(|| {
            extension.bytes().fold(0_usize, |hash, byte| {
                hash.wrapping_mul(31).wrapping_add(usize::from(byte))
            }) % (FILE_PALETTE.len() - 1)
        })
    }

    pub(crate) fn color(&self, extension: &str) -> Color32 {
        FILE_PALETTE[self.index(extension)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn larger_extension_totals_receive_the_first_colors_not_name_hashes() {
        let colors = FileColors::from_extensions(&[
            ExtensionStats {
                extension: "txt".into(),
                bytes: 10,
                files: 100,
            },
            ExtensionStats {
                extension: "raw".into(),
                bytes: 900,
                files: 1,
            },
            ExtensionStats {
                extension: "zip".into(),
                bytes: 40,
                files: 2,
            },
        ]);
        assert_eq!(colors.index("raw"), 0);
        assert_eq!(colors.index("zip"), 1);
        assert_eq!(colors.index("txt"), 2);
        assert_eq!(colors.color("raw"), Color32::from_rgb(101, 101, 255));
    }

    #[test]
    fn extension_ranks_after_seventeen_reuse_the_colored_palette() {
        let extensions: Vec<_> = (0..40)
            .map(|rank| ExtensionStats {
                extension: format!("type{rank:02}"),
                bytes: 100 - rank,
                files: 1,
            })
            .collect();
        let colors = FileColors::from_extensions(&extensions);
        assert_eq!(colors.index("type16"), 16);
        assert_eq!(colors.index("type17"), 0);
        assert_eq!(colors.index("type18"), 1);
        assert_eq!(colors.index("type39"), 5);
        assert!(extensions.iter().all(|stats| {
            let color = colors.color(&stats.extension);
            color.r() != color.g() || color.g() != color.b()
        }));
    }

    #[test]
    fn missing_extension_statistics_keep_distinct_stable_colors() {
        let colors = FileColors::default();
        assert_eq!(colors.index("a"), 12);
        assert_eq!(colors.index("b"), 13);
        assert_eq!(colors.color("a"), FileColors::default().color("a"));
    }

    #[test]
    fn equal_totals_have_deterministic_extension_order_and_no_extension_is_ranked() {
        let colors = FileColors::from_extensions(&[
            ExtensionStats {
                extension: "zip".into(),
                bytes: 20,
                files: 1,
            },
            ExtensionStats {
                extension: "(no extension)".into(),
                bytes: 100,
                files: 1,
            },
            ExtensionStats {
                extension: "txt".into(),
                bytes: 20,
                files: 1,
            },
        ]);
        assert_eq!(colors.index("(no extension)"), 0);
        assert_eq!(colors.index("txt"), 1);
        assert_eq!(colors.index("zip"), 2);
    }
}
