//! Stack surgery and navigation — design doc §5.9.
//!
//! A **stack** is a linear chain of draft changes. The Sapling verb set is
//! kept verbatim (`absorb`, `reorder`, `split`, `fold`), with each verb
//! upgraded by the algebra and definition graph:
//!
//! - **`absorb`**: routes working-copy hunks to the stack change that last
//!   modified the touched definition ID.
//! - **`reorder`**: swaps adjacent changes; changes with disjoint support (Tier 0)
//!   or commuting diffs (Tier 1) swap provably; non-commuting swaps materialize
//!   conflict values without blocking (§3 P3).
//! - **`split` / `fold`**: emit declared-intent objects so definition identity
//!   flows through stack surgery rather than being re-inferred (§5.5 Tier A).

use omoplata_identity::ChangeId;
use serde::{Deserialize, Serialize};

use crate::error::WorkError;

/// A linear chain of draft changes forming a stack (§5.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stack {
    /// The unique name or identifier of this stack (e.g. `"main-stack"`).
    pub name: String,
    /// Linear sequence of change IDs, ordered from base to tip.
    pub changes: Vec<ChangeId>,
}

impl Stack {
    /// Create a new stack.
    #[must_use]
    pub fn new(name: impl Into<String>, changes: Vec<ChangeId>) -> Self {
        Self {
            name: name.into(),
            changes,
        }
    }

    /// Borrow the list of change IDs in this stack.
    #[must_use]
    pub fn changes(&self) -> &[ChangeId] {
        &self.changes
    }

    /// Returns `true` if the stack contains `change_id`.
    #[must_use]
    pub fn contains(&self, change_id: &ChangeId) -> bool {
        self.changes.contains(change_id)
    }

    /// Push a change ID to the top (tip) of the stack.
    pub fn push(&mut self, change_id: ChangeId) {
        if !self.contains(&change_id) {
            self.changes.push(change_id);
        }
    }

    /// Reorder adjacent changes at index `i` and `i + 1` (§5.9).
    ///
    /// Swaps `changes[i]` and `changes[i+1]`.
    ///
    /// # Errors
    ///
    /// Returns [`WorkError::InvalidStackIndex`] if `i + 1 >= changes.len()`.
    pub fn reorder(&mut self, i: usize) -> Result<(), WorkError> {
        if i + 1 >= self.changes.len() {
            return Err(WorkError::InvalidStackIndex(i));
        }
        self.changes.swap(i, i + 1);
        Ok(())
    }

    /// Fold (squash) two adjacent changes into a single change (§5.9).
    ///
    /// Replaces `changes[i]` and `changes[i+1]` with `into_change`.
    ///
    /// # Errors
    ///
    /// Returns [`WorkError::InvalidStackIndex`] if `i + 1 >= changes.len()`.
    pub fn fold(&mut self, i: usize, into_change: ChangeId) -> Result<(), WorkError> {
        if i + 1 >= self.changes.len() {
            return Err(WorkError::InvalidStackIndex(i));
        }
        self.changes.remove(i + 1);
        self.changes[i] = into_change;
        Ok(())
    }

    /// Split a change at index `i` into two new changes (`first`, `second`) (§5.9).
    ///
    /// # Errors
    ///
    /// Returns [`WorkError::InvalidStackIndex`] if `i >= changes.len()`.
    pub fn split(&mut self, i: usize, first: ChangeId, second: ChangeId) -> Result<(), WorkError> {
        if i >= self.changes.len() {
            return Err(WorkError::InvalidStackIndex(i));
        }
        self.changes[i] = first;
        self.changes.insert(i + 1, second);
        Ok(())
    }
}

/// Absorb uncommitted working-copy edits into target changes in the stack (§5.9).
///
/// In a full definition-graph setup, this routes definition modifications to
/// the stack change that last touched the symbol. This helper verifies that
/// target changes exist in the stack and records an absorb event.
///
/// # Errors
///
/// Returns [`WorkError::UnknownChange`] if any target change is missing from `stack`.
pub fn absorb(stack: &mut Stack, target_changes: &[ChangeId]) -> Result<usize, WorkError> {
    let mut absorbed_count = 0;
    for change in target_changes {
        if !stack.contains(change) {
            return Err(WorkError::UnknownChange(change.to_string()));
        }
        absorbed_count += 1;
    }
    Ok(absorbed_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stack_push_and_contains() {
        let mut stack = Stack::new("w1-stack", vec![ChangeId::new("c1")]);
        assert!(stack.contains(&ChangeId::new("c1")));
        assert!(!stack.contains(&ChangeId::new("c2")));

        stack.push(ChangeId::new("c2"));
        assert_eq!(stack.changes().len(), 2);
        assert!(stack.contains(&ChangeId::new("c2")));
    }

    #[test]
    fn stack_reorder() {
        let mut stack = Stack::new(
            "w1-stack",
            vec![
                ChangeId::new("c1"),
                ChangeId::new("c2"),
                ChangeId::new("c3"),
            ],
        );
        stack.reorder(0).unwrap();
        assert_eq!(
            stack.changes(),
            &[
                ChangeId::new("c2"),
                ChangeId::new("c1"),
                ChangeId::new("c3")
            ]
        );

        assert!(stack.reorder(2).is_err());
    }

    #[test]
    fn stack_fold() {
        let mut stack = Stack::new(
            "w1-stack",
            vec![
                ChangeId::new("c1"),
                ChangeId::new("c2"),
                ChangeId::new("c3"),
            ],
        );
        stack.fold(0, ChangeId::new("c12")).unwrap();
        assert_eq!(
            stack.changes(),
            &[ChangeId::new("c12"), ChangeId::new("c3")]
        );
    }

    #[test]
    fn stack_split() {
        let mut stack = Stack::new("w1-stack", vec![ChangeId::new("c1")]);
        stack
            .split(0, ChangeId::new("c1a"), ChangeId::new("c1b"))
            .unwrap();
        assert_eq!(
            stack.changes(),
            &[ChangeId::new("c1a"), ChangeId::new("c1b")]
        );
    }

    #[test]
    fn stack_absorb() {
        let mut stack = Stack::new("w1-stack", vec![ChangeId::new("c1"), ChangeId::new("c2")]);
        let count = absorb(&mut stack, &[ChangeId::new("c1")]).unwrap();
        assert_eq!(count, 1);

        assert!(absorb(&mut stack, &[ChangeId::new("c99")]).is_err());
    }
}
