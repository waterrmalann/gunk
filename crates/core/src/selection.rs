use std::collections::BTreeSet;

/// Tracks which commit indices are selected and the anchor for shift-range selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionState {
    /// Currently selected indices (sorted for deterministic iteration).
    pub selected: BTreeSet<usize>,
    /// Anchor index for shift-range selection (the last non-shift click).
    pub anchor: Option<usize>,
    /// Total number of items in the list (for bounds checking).
    pub count: usize,
}

/// Messages that drive selection state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionMsg {
    /// Plain click — select exactly this index, deselecting everything else.
    Click(usize),
    /// Ctrl+click — toggle this index without affecting others.
    CtrlClick(usize),
    /// Shift+click — select the range from anchor to this index (inclusive).
    ShiftClick(usize),
    /// Clear all selection.
    Clear,
    /// Select all items.
    SelectAll,
    /// Select a specific set of indices (e.g., from search results).
    SelectSet(BTreeSet<usize>),
}

impl SelectionState {
    /// Create a new empty selection for a list of `count` items.
    pub fn new(count: usize) -> Self {
        Self {
            selected: BTreeSet::new(),
            anchor: None,
            count,
        }
    }

    /// Reducer: apply a message to produce the next state.
    pub fn reduce(&self, msg: SelectionMsg) -> Self {
        match msg {
            SelectionMsg::Click(idx) => {
                if idx >= self.count {
                    return self.clone();
                }
                let mut selected = BTreeSet::new();
                selected.insert(idx);
                Self {
                    selected,
                    anchor: Some(idx),
                    count: self.count,
                }
            }
            SelectionMsg::CtrlClick(idx) => {
                if idx >= self.count {
                    return self.clone();
                }
                let mut selected = self.selected.clone();
                if selected.contains(&idx) {
                    selected.remove(&idx);
                } else {
                    selected.insert(idx);
                }
                Self {
                    selected,
                    anchor: Some(idx),
                    count: self.count,
                }
            }
            SelectionMsg::ShiftClick(idx) => {
                if idx >= self.count {
                    return self.clone();
                }
                let anchor = self.anchor.unwrap_or(0);
                let (start, end) = if anchor <= idx {
                    (anchor, idx)
                } else {
                    (idx, anchor)
                };
                let mut selected = self.selected.clone();
                for i in start..=end {
                    selected.insert(i);
                }
                Self {
                    selected,
                    // Anchor stays the same on shift-click.
                    anchor: self.anchor,
                    count: self.count,
                }
            }
            SelectionMsg::Clear => Self {
                selected: BTreeSet::new(),
                anchor: None,
                count: self.count,
            },
            SelectionMsg::SelectAll => {
                let selected: BTreeSet<usize> = (0..self.count).collect();
                Self {
                    selected,
                    anchor: None,
                    count: self.count,
                }
            }
            SelectionMsg::SelectSet(indices) => {
                let selected: BTreeSet<usize> =
                    indices.into_iter().filter(|&i| i < self.count).collect();
                Self {
                    selected,
                    anchor: None,
                    count: self.count,
                }
            }
        }
    }

    /// Returns true if the given index is selected.
    pub fn is_selected(&self, idx: usize) -> bool {
        self.selected.contains(&idx)
    }

    /// Returns the number of selected items.
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    /// Returns true if nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_selection_is_empty() {
        let state = SelectionState::new(10);
        assert!(state.is_empty());
        assert_eq!(state.len(), 0);
        assert_eq!(state.anchor, None);
    }

    // ── Click ──────────────────────────────────────────────────────

    #[test]
    fn click_selects_single_item() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::Click(2));

        assert_eq!(state.selected, BTreeSet::from([2]));
        assert_eq!(state.anchor, Some(2));
    }

    #[test]
    fn click_replaces_previous_selection() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::Click(1));
        let state = state.reduce(SelectionMsg::Click(3));

        assert_eq!(state.selected, BTreeSet::from([3]));
        assert_eq!(state.anchor, Some(3));
    }

    #[test]
    fn click_out_of_bounds_is_ignored() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::Click(10));

        assert!(state.is_empty());
    }

    // ── CtrlClick ──────────────────────────────────────────────────

    #[test]
    fn ctrl_click_toggles_on() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::CtrlClick(1));

        assert!(state.is_selected(1));
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn ctrl_click_toggles_off() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::CtrlClick(1));
        let state = state.reduce(SelectionMsg::CtrlClick(1));

        assert!(!state.is_selected(1));
        assert!(state.is_empty());
    }

    #[test]
    fn ctrl_click_accumulates_selections() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::CtrlClick(0));
        let state = state.reduce(SelectionMsg::CtrlClick(2));
        let state = state.reduce(SelectionMsg::CtrlClick(4));

        assert_eq!(state.selected, BTreeSet::from([0, 2, 4]));
        assert_eq!(state.len(), 3);
    }

    #[test]
    fn ctrl_click_updates_anchor() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::CtrlClick(1));
        assert_eq!(state.anchor, Some(1));

        let state = state.reduce(SelectionMsg::CtrlClick(3));
        assert_eq!(state.anchor, Some(3));
    }

    #[test]
    fn ctrl_click_out_of_bounds_is_ignored() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::CtrlClick(1));
        let state = state.reduce(SelectionMsg::CtrlClick(99));

        assert_eq!(state.selected, BTreeSet::from([1]));
    }

    // ── ShiftClick ─────────────────────────────────────────────────

    #[test]
    fn shift_click_selects_range_forward() {
        let state = SelectionState::new(10);
        let state = state.reduce(SelectionMsg::Click(2)); // set anchor
        let state = state.reduce(SelectionMsg::ShiftClick(5));

        assert_eq!(state.selected, BTreeSet::from([2, 3, 4, 5]));
    }

    #[test]
    fn shift_click_selects_range_backward() {
        let state = SelectionState::new(10);
        let state = state.reduce(SelectionMsg::Click(7)); // set anchor
        let state = state.reduce(SelectionMsg::ShiftClick(4));

        assert_eq!(state.selected, BTreeSet::from([4, 5, 6, 7]));
    }

    #[test]
    fn shift_click_with_no_anchor_uses_zero() {
        let state = SelectionState::new(10);
        let state = state.reduce(SelectionMsg::ShiftClick(3));

        assert_eq!(state.selected, BTreeSet::from([0, 1, 2, 3]));
    }

    #[test]
    fn shift_click_preserves_anchor() {
        let state = SelectionState::new(10);
        let state = state.reduce(SelectionMsg::Click(3));
        let state = state.reduce(SelectionMsg::ShiftClick(6));

        assert_eq!(state.anchor, Some(3)); // anchor unchanged
    }

    #[test]
    fn shift_click_unions_with_existing_selection() {
        let state = SelectionState::new(10);
        let state = state.reduce(SelectionMsg::CtrlClick(1));
        let state = state.reduce(SelectionMsg::CtrlClick(8)); // anchor = 8
        let state = state.reduce(SelectionMsg::ShiftClick(6));

        // Existing: {1, 8}, shift-range 6..=8 → {1, 6, 7, 8}
        assert_eq!(state.selected, BTreeSet::from([1, 6, 7, 8]));
    }

    #[test]
    fn shift_click_out_of_bounds_is_ignored() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::Click(2));
        let state = state.reduce(SelectionMsg::ShiftClick(99));

        assert_eq!(state.selected, BTreeSet::from([2])); // unchanged
    }

    // ── Clear ──────────────────────────────────────────────────────

    #[test]
    fn clear_removes_all_selection() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::Click(2));
        let state = state.reduce(SelectionMsg::CtrlClick(4));
        let state = state.reduce(SelectionMsg::Clear);

        assert!(state.is_empty());
        assert_eq!(state.anchor, None);
    }

    // ── SelectAll ──────────────────────────────────────────────────

    #[test]
    fn select_all_selects_every_index() {
        let state = SelectionState::new(4);
        let state = state.reduce(SelectionMsg::SelectAll);

        assert_eq!(state.selected, BTreeSet::from([0, 1, 2, 3]));
        assert_eq!(state.len(), 4);
    }

    #[test]
    fn select_all_on_empty_list() {
        let state = SelectionState::new(0);
        let state = state.reduce(SelectionMsg::SelectAll);

        assert!(state.is_empty());
    }

    // ── SelectSet ──────────────────────────────────────────────────

    #[test]
    fn select_set_replaces_selection() {
        let state = SelectionState::new(10);
        let state = state.reduce(SelectionMsg::Click(5));
        let state = state.reduce(SelectionMsg::SelectSet(BTreeSet::from([1, 3, 7])));

        assert_eq!(state.selected, BTreeSet::from([1, 3, 7]));
    }

    #[test]
    fn select_set_filters_out_of_bounds() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::SelectSet(BTreeSet::from([1, 3, 99])));

        assert_eq!(state.selected, BTreeSet::from([1, 3]));
    }

    // ── Composite workflows ────────────────────────────────────────

    #[test]
    fn click_after_ctrl_click_resets_to_single() {
        let state = SelectionState::new(5);
        let state = state.reduce(SelectionMsg::CtrlClick(0));
        let state = state.reduce(SelectionMsg::CtrlClick(2));
        let state = state.reduce(SelectionMsg::CtrlClick(4));
        let state = state.reduce(SelectionMsg::Click(3));

        assert_eq!(state.selected, BTreeSet::from([3]));
    }

    #[test]
    fn shift_after_ctrl_click_extends_from_last_ctrl_anchor() {
        let state = SelectionState::new(10);
        let state = state.reduce(SelectionMsg::CtrlClick(2));
        let state = state.reduce(SelectionMsg::CtrlClick(5)); // anchor = 5
        let state = state.reduce(SelectionMsg::ShiftClick(8));

        // {2, 5} + range 5..=8 → {2, 5, 6, 7, 8}
        assert_eq!(state.selected, BTreeSet::from([2, 5, 6, 7, 8]));
    }

    #[test]
    fn select_all_then_ctrl_click_deselects_one() {
        let state = SelectionState::new(4);
        let state = state.reduce(SelectionMsg::SelectAll);
        let state = state.reduce(SelectionMsg::CtrlClick(2));

        assert_eq!(state.selected, BTreeSet::from([0, 1, 3]));
    }
}
