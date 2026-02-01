use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    Frame,
};
use serde::{Deserialize, Serialize};

use crate::widgets::ChatPane;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PaneNode {
    Single(usize), // Index into App.panes
    Split {
        direction: SplitDirection,
        children: Vec<Box<PaneNode>>,
        #[serde(default)]
        ratios: Vec<u16>, // Per-child percentage ratios (empty = equal)
    },
}

impl PaneNode {
    pub fn new_single(pane_idx: usize) -> Self {
        PaneNode::Single(pane_idx)
    }

    pub fn split(&mut self, direction: SplitDirection, new_pane_idx: usize) {
        // If already a split in the same direction, append to it for equal sizing
        if let PaneNode::Split { direction: d, children, ratios } = self {
            if *d == direction {
                children.push(Box::new(PaneNode::Single(new_pane_idx)));
                ratios.clear(); // Reset to equal sizing
                return;
            }
        }
        let old_node = std::mem::replace(self, PaneNode::Single(0));
        *self = PaneNode::Split {
            direction,
            children: vec![Box::new(old_node), Box::new(PaneNode::Single(new_pane_idx))],
            ratios: vec![],
        };
    }
    
    /// Split a specific pane in the tree
    pub fn split_pane(&mut self, target_pane_idx: usize, direction: SplitDirection, new_pane_idx: usize) -> bool {
        match self {
            PaneNode::Single(idx) if *idx == target_pane_idx => {
                self.split(direction, new_pane_idx);
                true
            }
            PaneNode::Single(_) => false,
            PaneNode::Split { direction: d, children, ratios } => {
                // If a direct child is the target and our direction matches, add sibling here
                if *d == direction {
                    if let Some(_pos) = children.iter().position(|c| matches!(**c, PaneNode::Single(idx) if idx == target_pane_idx)) {
                        children.push(Box::new(PaneNode::Single(new_pane_idx)));
                        ratios.clear();
                        return true;
                    }
                }
                // Otherwise recurse
                for child in children.iter_mut() {
                    if child.split_pane(target_pane_idx, direction, new_pane_idx) {
                        return true;
                    }
                }
                false
            }
        }
    }
    
    pub fn split_pane_with_ratio(&mut self, target_pane_idx: usize, direction: SplitDirection, new_pane_idx: usize, new_pane_percent: u16) -> bool {
        match self {
            PaneNode::Single(idx) if *idx == target_pane_idx => {
                let old_node = std::mem::replace(self, PaneNode::Single(0));
                let old_percent = 100u16.saturating_sub(new_pane_percent);
                *self = PaneNode::Split {
                    direction,
                    children: vec![Box::new(old_node), Box::new(PaneNode::Single(new_pane_idx))],
                    ratios: vec![old_percent, new_pane_percent],
                };
                true
            }
            PaneNode::Single(_) => false,
            PaneNode::Split { direction: d, children, ratios } => {
                // If a direct child is the target and our direction matches, add sibling here
                if *d == direction {
                    if let Some(pos) = children.iter().position(|c| matches!(**c, PaneNode::Single(idx) if idx == target_pane_idx)) {
                        // Insert new pane after the target
                        children.insert(pos + 1, Box::new(PaneNode::Single(new_pane_idx)));
                        // Recalculate ratios: keep existing ratios proportional, add new one
                        let total_ratio: u16 = ratios.iter().sum();
                        let old_percent = total_ratio.saturating_sub(new_pane_percent);
                        // Scale existing ratios to fit the remaining space
                        if total_ratio > 0 {
                            for ratio in ratios.iter_mut() {
                                *ratio = (*ratio * old_percent) / total_ratio;
                            }
                        } else {
                            // If no ratios set, distribute equally
                            let existing_count = children.len() - 1;
                            ratios.clear();
                            for _ in 0..existing_count {
                                ratios.push(old_percent / existing_count.max(1) as u16);
                            }
                        }
                        ratios.insert(pos + 1, new_pane_percent);
                        return true;
                    }
                }
                // Otherwise recurse - try to find and split the target pane
                for child in children.iter_mut() {
                    if child.split_pane_with_ratio(target_pane_idx, direction, new_pane_idx, new_pane_percent) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Split with a specific ratio for the new pane (in percent).
    pub fn split_with_ratio(
        &mut self,
        direction: SplitDirection,
        new_pane_idx: usize,
        new_pane_percent: u16,
    ) {
        let old_node = std::mem::replace(self, PaneNode::Single(0));
        let old_percent = 100u16.saturating_sub(new_pane_percent);
        *self = PaneNode::Split {
            direction,
            children: vec![Box::new(old_node), Box::new(PaneNode::Single(new_pane_idx))],
            ratios: vec![old_percent, new_pane_percent],
        };
    }

    pub fn get_pane_indices(&self) -> Vec<usize> {
        match self {
            PaneNode::Single(idx) => vec![*idx],
            PaneNode::Split { children, .. } => {
                children.iter().flat_map(|c| c.get_pane_indices()).collect()
            }
        }
    }

    #[cfg(test)]
    pub fn count_panes(&self) -> usize {
        match self {
            PaneNode::Single(_) => 1,
            PaneNode::Split { children, .. } => {
                children.iter().map(|child| child.count_panes()).sum()
            }
        }
    }

    #[cfg(test)]
    pub fn find_and_remove_pane(&mut self, pane_idx: usize) -> bool {
        match self {
            PaneNode::Single(idx) => *idx == pane_idx,
            PaneNode::Split { children, .. } => {
                // Check if any child IS the pane we want to remove
                if let Some(pos) = children
                    .iter()
                    .position(|child| matches!(**child, PaneNode::Single(idx) if idx == pane_idx))
                {
                    // Remove this direct child
                    children.remove(pos);

                    // If only one child remains, collapse the split
                    if children.len() == 1 {
                        let child = children.remove(0);
                        *self = *child;
                    }
                    return true;
                }

                // Otherwise, recurse into children to find and remove
                for child in children.iter_mut() {
                    if child.find_and_remove_pane(pane_idx) {
                        return true;
                    }
                }

                false
            }
        }
    }

    pub fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        panes: &[ChatPane],
        focused_idx: usize,
        render_fn: &impl Fn(&mut Frame, Rect, &ChatPane, bool),
        pane_areas: &mut std::collections::HashMap<usize, Rect>,
    ) {
        match self {
            PaneNode::Single(pane_idx) => {
                if let Some(pane) = panes.get(*pane_idx) {
                    let is_focused = *pane_idx == focused_idx;
                    pane_areas.insert(*pane_idx, area);
                    render_fn(f, area, pane, is_focused);
                }
            }
            PaneNode::Split {
                direction,
                children,
                ratios,
            } => {
                if children.is_empty() {
                    return;
                }

                let constraints: Vec<Constraint> = if ratios.len() == children.len() {
                    ratios.iter().map(|&r| Constraint::Percentage(r)).collect()
                } else {
                    let n = children.len() as u32;
                    (0..n).map(|_| Constraint::Ratio(1, n)).collect()
                };

                let layout_direction = match direction {
                    SplitDirection::Horizontal => Direction::Vertical,
                    SplitDirection::Vertical => Direction::Horizontal,
                };

                let chunks = Layout::default()
                    .direction(layout_direction)
                    .constraints(constraints)
                    .split(area);

                for (i, child) in children.iter().enumerate() {
                    if let Some(&chunk) = chunks.get(i) {
                        child.render(f, chunk, panes, focused_idx, render_fn, pane_areas);
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub fn get_next_pane_idx(&self, current: usize) -> Option<usize> {
        let indices = self.get_pane_indices();
        if let Some(pos) = indices.iter().position(|&idx| idx == current) {
            let next_pos = (pos + 1) % indices.len();
            Some(indices[next_pos])
        } else {
            indices.first().copied()
        }
    }

    /// Toggle the split direction of the focused split node
    pub fn toggle_direction(&mut self) {
        match self {
            PaneNode::Split { direction, .. } => {
                *direction = match direction {
                    SplitDirection::Horizontal => SplitDirection::Vertical,
                    SplitDirection::Vertical => SplitDirection::Horizontal,
                };
            }
            PaneNode::Single(_) => {
                // Can't toggle direction of a single pane
            }
        }
    }

    /// Close a pane by removing it from the tree
    pub fn close_pane(&mut self, pane_idx: usize) {
        if let PaneNode::Split {
            children, ratios, ..
        } = self
        {
            // Find and remove the pane
            if let Some(pos) = children
                .iter()
                .position(|child| matches!(**child, PaneNode::Single(idx) if idx == pane_idx))
            {
                children.remove(pos);
                if pos < ratios.len() {
                    ratios.remove(pos);
                }
            } else {
                // Recurse into children
                for child in children.iter_mut() {
                    child.close_pane(pane_idx);
                }
            }

            // If only one child left, collapse to single
            if children.len() == 1 {
                if let Some(child) = children.first() {
                    *self = (**child).clone();
                }
            }
        }
    }
    
    /// Reindex all pane indices after a pane is removed
    /// All indices > removed_idx need to be decremented by 1
    pub fn reindex_after_removal(&mut self, removed_idx: usize) {
        match self {
            PaneNode::Single(idx) => {
                if *idx > removed_idx {
                    *idx -= 1;
                }
            }
            PaneNode::Split { children, .. } => {
                for child in children.iter_mut() {
                    child.reindex_after_removal(removed_idx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_single_pane() {
        let mut node = PaneNode::new_single(0);
        node.split(SplitDirection::Vertical, 1);

        let indices = node.get_pane_indices();
        assert_eq!(indices, vec![0, 1]);
        assert_eq!(node.count_panes(), 2);
    }

    #[test]
    fn test_remove_pane() {
        let mut node = PaneNode::new_single(0);
        node.split(SplitDirection::Vertical, 1);

        let removed = node.find_and_remove_pane(1);
        assert!(removed);

        // Should collapse back to single
        match node {
            PaneNode::Single(idx) => assert_eq!(idx, 0),
            _ => panic!("Expected Single node after collapse"),
        }
    }

    #[test]
    fn test_cycle_focus() {
        let mut node = PaneNode::new_single(0);
        node.split(SplitDirection::Vertical, 1);
        node.split(SplitDirection::Horizontal, 2);

        let next = node.get_next_pane_idx(0);
        assert_eq!(next, Some(1));

        let next = node.get_next_pane_idx(2);
        assert_eq!(next, Some(0)); // Wraps around
    }
}
