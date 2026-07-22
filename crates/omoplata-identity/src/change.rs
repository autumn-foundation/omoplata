//! The **change graph** — design doc §5.3, principle **P5** (two-tier identity).
//!
//! Commits are content-addressed and immutable. A *change* is the stable-ID
//! node that survives rebase and amend: its commits are *revisions*, linked by
//! **supersession** edges recording that a newer revision obsoletes an older one
//! (Mercurial obsolescence, done properly — §5.3). **Phases** formalize what is
//! safe to rewrite: a [`Draft`](Phase::Draft) change may be superseded, a
//! [`Public`](Phase::Public) change may not, and the phase only ever advances
//! `Draft -> Public` (monotone).
//!
//! The supersession relation must satisfy invariant **I6** — *"the change graph
//! is acyclic with no orphaned obsolescence"* (design doc §7). This module
//! discharges I6 at edge-insertion time: cycles are rejected (never panicked)
//! and both endpoints of every edge must be registered revisions (no orphans).

use std::collections::HashMap;
use std::collections::HashSet;

use crate::error::IdentityError;

/// A stable change identity that survives rebase and amend (§5.3, P5).
///
/// Unlike a [`CommitId`], a `ChangeId` is not content-addressed: it is minted
/// once and then carried across every revision of the logical change. Explicit
/// construction is provided so tests are deterministic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChangeId(String);

impl ChangeId {
    /// Construct a change id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for ChangeId {
    type Err = std::convert::Infallible;

    /// Construct a change id from a string slice (infallible); enables
    /// `"c1".parse::<ChangeId>()` and `ChangeId::from_str("c1")` for
    /// deterministic tests.
    fn from_str(id: &str) -> Result<Self, Self::Err> {
        Ok(Self(id.to_owned()))
    }
}

impl std::fmt::Display for ChangeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An opaque, content-addressed commit identifier (a revision of a change).
///
/// The identity layer treats a commit id as an opaque hash string; the object
/// store owns the actual hashing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(String);

impl CommitId {
    /// Construct a commit id from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying hash string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The rewrite phase of a change (design doc §5.3, P5).
///
/// Phases are ordered: `Draft < Public`. A change may advance `Draft -> Public`
/// but never regress — enforced by [`ChangeGraph::set_phase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// A rewritable change: its commits may be superseded.
    Draft,
    /// An immutable, published change: its commits may not be superseded (P5).
    Public,
}

/// A stable-ID node in the change graph whose commits are its revisions (§5.3).
#[derive(Debug, Clone)]
pub struct Change {
    /// The stable identity of this change.
    pub id: ChangeId,
    /// The commits that are revisions of this change, in insertion order.
    pub revisions: Vec<CommitId>,
    /// The current rewrite phase.
    pub phase: Phase,
}

impl Change {
    /// Create a new change with the given id, revisions, and phase.
    pub fn new(id: ChangeId, revisions: Vec<CommitId>, phase: Phase) -> Self {
        Self {
            id,
            revisions,
            phase,
        }
    }

    /// Create a new empty [`Draft`](Phase::Draft) change.
    pub fn draft(id: ChangeId) -> Self {
        Self {
            id,
            revisions: Vec::new(),
            phase: Phase::Draft,
        }
    }
}

/// The change graph: changes plus a supersession DAG over their commits.
///
/// The supersession relation is stored as a map `old -> new`, meaning "commit
/// `new` obsoletes commit `old`". Following those edges from a change's
/// revisions reaches its [`tip`](ChangeGraph::tip) — the surviving,
/// non-superseded head.
#[derive(Debug, Default)]
pub struct ChangeGraph {
    changes: HashMap<ChangeId, Change>,
    /// Maps every registered commit to the change that owns it.
    commit_owner: HashMap<CommitId, ChangeId>,
    /// Supersession edges: `old -> new` (new obsoletes old).
    superseded_by: HashMap<CommitId, CommitId>,
}

impl ChangeGraph {
    /// Create an empty change graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a change, registering each of its revisions as owned commits.
    ///
    /// Replaces any existing change with the same id.
    pub fn add_change(&mut self, change: Change) {
        for commit in &change.revisions {
            self.commit_owner.insert(commit.clone(), change.id.clone());
        }
        self.changes.insert(change.id.clone(), change);
    }

    /// Append a new revision (commit) to an existing change.
    pub fn add_revision(
        &mut self,
        change_id: &ChangeId,
        commit: CommitId,
    ) -> Result<(), IdentityError> {
        let change = self
            .changes
            .get_mut(change_id)
            .ok_or_else(|| IdentityError::UnknownChange(change_id.to_string()))?;
        self.commit_owner.insert(commit.clone(), change_id.clone());
        change.revisions.push(commit);
        Ok(())
    }

    /// Borrow a change by id.
    pub fn change(&self, change_id: &ChangeId) -> Option<&Change> {
        self.changes.get(change_id)
    }

    /// Record that `new` supersedes (obsoletes) `old`.
    ///
    /// # Errors
    ///
    /// * [`IdentityError::UnknownCommit`] if either endpoint is not a registered
    ///   revision — this is how the "no orphaned obsolescence" half of I6 is
    ///   enforced.
    /// * [`IdentityError::PublicImmutable`] if `old` belongs to a
    ///   [`Public`](Phase::Public) change (P5: public changes are immutable).
    /// * [`IdentityError::SupersessionCycle`] if the edge would create a cycle —
    ///   the "acyclic" half of I6.
    pub fn supersede(&mut self, old: &CommitId, new: &CommitId) -> Result<(), IdentityError> {
        // PROOF OBLIGATION (I6): "no orphaned obsolescence" — both endpoints of a
        // supersession edge must be registered revisions of some change.
        let owner = self
            .commit_owner
            .get(old)
            .ok_or_else(|| IdentityError::UnknownCommit(old.to_string()))?
            .clone();
        if !self.commit_owner.contains_key(new) {
            return Err(IdentityError::UnknownCommit(new.to_string()));
        }

        // P5: a commit belonging to a Public change may never be superseded.
        let phase = self
            .changes
            .get(&owner)
            .map(|c| c.phase)
            .unwrap_or(Phase::Draft);
        if phase == Phase::Public {
            return Err(IdentityError::PublicImmutable(old.to_string()));
        }

        // PROOF OBLIGATION (I6): "acyclic" — adding old -> new must not create a
        // cycle. A cycle would exist iff `new` can already reach `old` along
        // existing edges (this also rejects the self-edge old == new).
        if self.reaches(new, old) {
            return Err(IdentityError::SupersessionCycle(
                old.to_string(),
                new.to_string(),
            ));
        }

        self.superseded_by.insert(old.clone(), new.clone());
        Ok(())
    }

    /// Whether `from` can reach `target` by following supersession edges.
    ///
    /// Bounded by a visited set so a pre-existing (impossible under I6) cycle can
    /// never loop forever — the traversal terminates and never panics.
    fn reaches(&self, from: &CommitId, target: &CommitId) -> bool {
        if from == target {
            return true;
        }
        let mut cursor = from;
        let mut seen: HashSet<&CommitId> = HashSet::new();
        while let Some(next) = self.superseded_by.get(cursor) {
            if next == target {
                return true;
            }
            if !seen.insert(next) {
                break;
            }
            cursor = next;
        }
        false
    }

    /// The non-superseded head of a change, following supersession edges.
    ///
    /// Returns `None` if the change is unknown or has no revisions. The walk is
    /// guarded by a visited set, so it always terminates.
    ///
    /// PROOF OBLIGATION (I6): because the relation is acyclic, following
    /// `old -> new` from any revision reaches a unique terminal (non-superseded)
    /// commit — the tip.
    pub fn tip(&self, change_id: &ChangeId) -> Option<&CommitId> {
        let change = self.changes.get(change_id)?;
        let mut cursor = change.revisions.first()?;
        let mut seen: HashSet<&CommitId> = HashSet::new();
        seen.insert(cursor);
        while let Some(next) = self.superseded_by.get(cursor) {
            if !seen.insert(next) {
                break;
            }
            cursor = next;
        }
        Some(cursor)
    }

    /// Set the phase of a change, enforcing monotonicity.
    ///
    /// # Errors
    ///
    /// * [`IdentityError::UnknownChange`] if the change is not present.
    /// * [`IdentityError::PhaseRegression`] if the transition would move
    ///   `Public -> Draft`. Phases only ever advance (P5).
    pub fn set_phase(&mut self, change_id: &ChangeId, phase: Phase) -> Result<(), IdentityError> {
        let change = self
            .changes
            .get_mut(change_id)
            .ok_or_else(|| IdentityError::UnknownChange(change_id.to_string()))?;
        // Monotone: Draft -> Public is allowed; Public -> Draft is rejected.
        if change.phase == Phase::Public && phase == Phase::Draft {
            return Err(IdentityError::PhaseRegression(change_id.to_string()));
        }
        change.phase = phase;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(s: &str) -> CommitId {
        CommitId::new(s)
    }

    #[test]
    fn tip_follows_supersession_chain() {
        let mut g = ChangeGraph::new();
        let id = ChangeId::new("c1");
        g.add_change(Change::new(
            id.clone(),
            vec![commit("a"), commit("b"), commit("c")],
            Phase::Draft,
        ));
        // a superseded by b, b superseded by c => tip is c.
        g.supersede(&commit("a"), &commit("b")).unwrap();
        g.supersede(&commit("b"), &commit("c")).unwrap();
        assert_eq!(g.tip(&id), Some(&commit("c")));
    }

    #[test]
    fn tip_of_single_revision_is_itself() {
        let mut g = ChangeGraph::new();
        let id = ChangeId::new("c1");
        g.add_change(Change::new(id.clone(), vec![commit("a")], Phase::Draft));
        assert_eq!(g.tip(&id), Some(&commit("a")));
    }

    #[test]
    fn supersession_cycle_is_rejected() {
        let mut g = ChangeGraph::new();
        let id = ChangeId::new("c1");
        g.add_change(Change::new(
            id,
            vec![commit("a"), commit("b"), commit("c")],
            Phase::Draft,
        ));
        g.supersede(&commit("a"), &commit("b")).unwrap();
        g.supersede(&commit("b"), &commit("c")).unwrap();
        // c -> a would close a cycle a->b->c->a.
        let err = g.supersede(&commit("c"), &commit("a")).unwrap_err();
        assert_eq!(
            err,
            IdentityError::SupersessionCycle("c".into(), "a".into())
        );
    }

    #[test]
    fn self_supersession_is_a_cycle() {
        let mut g = ChangeGraph::new();
        let id = ChangeId::new("c1");
        g.add_change(Change::new(id, vec![commit("a")], Phase::Draft));
        assert!(g.supersede(&commit("a"), &commit("a")).is_err());
    }

    #[test]
    fn superseding_public_change_is_rejected() {
        let mut g = ChangeGraph::new();
        let id = ChangeId::new("c1");
        g.add_change(Change::new(
            id.clone(),
            vec![commit("a"), commit("b")],
            Phase::Draft,
        ));
        g.set_phase(&id, Phase::Public).unwrap();
        let err = g.supersede(&commit("a"), &commit("b")).unwrap_err();
        assert_eq!(err, IdentityError::PublicImmutable("a".into()));
    }

    #[test]
    fn phase_is_monotone() {
        let mut g = ChangeGraph::new();
        let id = ChangeId::new("c1");
        g.add_change(Change::draft(id.clone()));
        // Draft -> Public advances fine.
        g.set_phase(&id, Phase::Public).unwrap();
        // Public -> Draft regresses and is rejected.
        let err = g.set_phase(&id, Phase::Draft).unwrap_err();
        assert_eq!(err, IdentityError::PhaseRegression("c1".into()));
        // Public -> Public is idempotent and allowed.
        g.set_phase(&id, Phase::Public).unwrap();
    }

    #[test]
    fn supersede_unknown_commit_is_orphan_rejected() {
        let mut g = ChangeGraph::new();
        let id = ChangeId::new("c1");
        g.add_change(Change::new(id, vec![commit("a")], Phase::Draft));
        // `z` is not a registered revision => orphan, rejected (I6).
        assert!(g.supersede(&commit("a"), &commit("z")).is_err());
        assert!(g.supersede(&commit("z"), &commit("a")).is_err());
    }
}
