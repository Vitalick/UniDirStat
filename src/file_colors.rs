use std::collections::HashMap;

use eframe::egui::Color32;
use unidirstat::analysis::ExtensionStats;

pub(crate) const FILE_PALETTE: [Color32; 18] = [
    // Blend the existing hues 60:40 with neutral 128 to soften saturation and glare.
    Color32::from_rgb(112, 112, 204),
    Color32::from_rgb(204, 112, 112),
    Color32::from_rgb(112, 204, 112),
    Color32::from_rgb(189, 189, 51),
    Color32::from_rgb(51, 189, 189),
    Color32::from_rgb(189, 51, 189),
    Color32::from_rgb(204, 167, 57),
    Color32::from_rgb(78, 146, 204),
    Color32::from_rgb(204, 78, 146),
    Color32::from_rgb(146, 204, 78),
    Color32::from_rgb(167, 57, 204),
    Color32::from_rgb(78, 204, 146),
    Color32::from_rgb(204, 57, 167),
    Color32::from_rgb(57, 167, 204),
    Color32::from_rgb(204, 146, 78),
    Color32::from_rgb(57, 204, 167),
    Color32::from_rgb(146, 78, 204),
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
        assert_eq!(colors.color("raw"), FILE_PALETTE[0]);
    }

    #[test]
    fn palette_remains_colorful_without_full_intensity_channels() {
        assert!(FILE_PALETTE.iter().all(|color| {
            color.a() == 255
                && [color.r(), color.g(), color.b()]
                    .into_iter()
                    .all(|channel| (50..=205).contains(&channel))
        }));
        assert!(FILE_PALETTE[..17].iter().all(|color| {
            color.r().max(color.g()).max(color.b()) - color.r().min(color.g()).min(color.b()) >= 90
        }));
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
