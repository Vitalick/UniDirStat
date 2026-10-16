use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct Selection {
    selected: BTreeSet<usize>,
    primary: Option<usize>,
    anchor: Option<usize>,
}

impl Selection {
    pub(crate) fn contains(&self, index: usize) -> bool {
        self.selected.contains(&index)
    }

    pub(crate) fn len(&self) -> usize {
        self.selected.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub(crate) fn primary(&self) -> Option<usize> {
        self.primary
    }

    pub(crate) fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.selected.iter().copied()
    }

    pub(crate) fn clear(&mut self) {
        self.selected.clear();
        self.primary = None;
        self.anchor = None;
    }

    pub(crate) fn click(
        &mut self,
        index: usize,
        visible: &[(usize, usize)],
        toggle: bool,
        range: bool,
    ) {
        if range {
            let anchor_position = self
                .anchor
                .and_then(|anchor| visible.iter().position(|&(node, _)| node == anchor));
            let clicked_position = visible.iter().position(|&(node, _)| node == index);
            if let (Some(anchor), Some(clicked)) = (anchor_position, clicked_position) {
                if !toggle {
                    self.selected.clear();
                }
                self.selected.extend(
                    visible[anchor.min(clicked)..=anchor.max(clicked)]
                        .iter()
                        .map(|&(node, _)| node),
                );
                self.primary = Some(index);
                return;
            }
        }
        if !toggle || range {
            self.selected.clear();
        }
        if !toggle || range || !self.selected.remove(&index) {
            self.selected.insert(index);
            self.primary = Some(index);
        } else {
            self.primary = self.selected.last().copied();
        }
        self.anchor = Some(index);
    }

    pub(crate) fn right_click(&mut self, index: usize) {
        if !self.contains(index) {
            self.selected.clear();
            self.selected.insert(index);
            self.anchor = Some(index);
        }
        self.primary = Some(index);
    }

    pub(crate) fn select_all(&mut self, visible: &[(usize, usize)]) {
        self.selected = visible.iter().map(|&(index, _)| index).collect();
        self.primary = self
            .primary
            .filter(|index| self.selected.contains(index))
            .or_else(|| visible.first().map(|&(index, _)| index));
        self.anchor = self.primary;
    }
}

#[cfg(test)]
mod tests {
    use super::Selection;

    const ROWS: &[(usize, usize)] = &[(9, 0), (4, 1), (7, 1), (2, 0), (6, 1)];

    #[test]
    fn ordinary_click_replaces_selection_and_primary() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.click(4, ROWS, true, false);
        selection.click(7, ROWS, false, false);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [7]);
        assert_eq!(selection.primary(), Some(7));
        assert!(selection.contains(7));
        assert_eq!(selection.len(), 1);
        assert!(!selection.is_empty());
    }

    #[test]
    fn toggle_adds_and_removes_rows() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.click(4, ROWS, true, false);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [4, 9]);
        assert_eq!(selection.primary(), Some(4));
        selection.click(4, ROWS, true, false);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [9]);
        assert_eq!(selection.primary(), Some(9));
        selection.click(9, ROWS, true, false);
        assert!(selection.is_empty());
        assert_eq!(selection.primary(), None);
    }

    #[test]
    fn shift_selects_visible_range_with_stable_anchor() {
        let mut selection = Selection::default();
        selection.click(4, ROWS, false, false);
        selection.click(2, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2, 4, 7]);
        assert_eq!(selection.primary(), Some(2));
        selection.click(7, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [4, 7]);
    }

    #[test]
    fn shift_selects_reversed_visible_range() {
        let mut selection = Selection::default();
        selection.click(2, ROWS, false, false);
        selection.click(4, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2, 4, 7]);
        assert_eq!(selection.primary(), Some(4));
    }

    #[test]
    fn toggle_shift_extends_existing_selection() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.click(7, ROWS, true, false);
        selection.click(6, ROWS, true, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2, 6, 7, 9]);
    }

    #[test]
    fn shift_without_anchor_selects_only_clicked_row() {
        let mut selection = Selection::default();
        selection.click(7, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [7]);
        selection.click(2, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2, 7]);
    }

    #[test]
    fn shift_with_hidden_anchor_selects_only_clicked_row() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.click(4, ROWS, true, false);
        selection.click(6, &ROWS[2..], true, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [6]);
        selection.click(2, &ROWS[2..], false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2, 6]);
    }

    #[test]
    fn right_click_on_selected_row_preserves_selection_and_anchor() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.click(4, ROWS, true, false);
        selection.right_click(9);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [4, 9]);
        assert_eq!(selection.primary(), Some(9));
        selection.click(7, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [4, 7]);
    }

    #[test]
    fn right_click_outside_selection_selects_only_clicked_row() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.click(4, ROWS, true, false);
        selection.right_click(7);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [7]);
        assert_eq!(selection.primary(), Some(7));
        selection.click(2, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2, 7]);
    }

    #[test]
    fn clear_removes_selection_primary_and_anchor() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.click(4, ROWS, true, false);
        selection.clear();
        assert!(selection.is_empty());
        assert_eq!(selection.primary(), None);
        selection.click(2, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn select_all_replaces_selection_with_visible_rows() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.select_all(&ROWS[1..4]);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2, 4, 7]);
        assert_eq!(selection.primary(), Some(4));
        selection.click(7, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [4, 7]);
    }

    #[test]
    fn select_all_keeps_existing_visible_primary() {
        let mut selection = Selection::default();
        selection.click(7, ROWS, false, false);
        selection.select_all(ROWS);
        assert_eq!(selection.primary(), Some(7));
        assert_eq!(selection.len(), 5);
    }

    #[test]
    fn select_all_empty_clears_selection() {
        let mut selection = Selection::default();
        selection.click(9, ROWS, false, false);
        selection.select_all(&[]);
        assert!(selection.is_empty());
        assert_eq!(selection.primary(), None);
        selection.click(2, ROWS, false, true);
        assert_eq!(selection.indices().collect::<Vec<_>>(), [2]);
    }
}
