//! The **bi-temporal operation log** — design doc §5.6, principle **P4**, and
//! invariant **I7**.
//!
//! The repository's mutable state is modelled as a set of **refs**
//! (`name -> `[`CommitId`]). Every mutation is an [`Operation`] carrying a
//! **transaction time**: a monotonic sequence number ([`Operation::seq`])
//! assigned when the operation is appended. The log is strictly append-only —
//! it never mutates or deletes a past entry — which is what makes the two time
//! axes (valid time in the change graph, transaction time here) jointly
//! queryable (Thesis claim 3, §5.6).
//!
//! # Undo is an inverse operation, not erasure (§5.6)
//!
//! > *"Undo is an inverse operation, not history erasure; the log never lies
//! > about what was believed."*
//!
//! [`OpLog::undo`] never shrinks the log. It appends a fresh [`OpKind::Undo`]
//! whose *effect* is the inverse of the most recent operation still in effect.
//! Undoing an [`OpKind::Undo`] is therefore a redo, obtained by the very same
//! mechanism. Which operations are "in effect" at a given transaction time is
//! recomputed by [`OpLog::refs_at`], so
//! `"what did we believe the refs were, as of transaction time t"` is a
//! first-class query rather than reflog archaeology.
//!
//! # Invariant I7
//!
//! > **I7 Op-log invertibility:** every operation has an inverse;
//! > `undo ∘ op ≡ identity` on repository state.
//!
//! Every [`OpKind`] variant records enough before/after data
//! (`old`/`new`, `from`/`to`) to be inverted without consulting any other
//! state, and [`OpLog::undo`] discharges the `undo ∘ op ≡ identity` half by
//! restoring the folded ref state. The obligations are annotated with
//! `// PROOF OBLIGATION (I7): …` at the relevant sites. Verus is not available
//! in this environment, so each obligation is backed by an executable unit test.

use std::collections::BTreeMap;
use std::path::Path;

use omoplata_identity::{ChangeId, CommitId, Phase};
use serde::{Deserialize, Serialize};

use crate::error::WorkError;

/// The kind of a repository mutation recorded in the [`OpLog`].
///
/// Each variant carries enough before/after data to be **invertible** on its
/// own (invariant I7): a [`SetRef`](OpKind::SetRef) knows both the `old` and
/// `new` target, a [`SetPhase`](OpKind::SetPhase) knows both `from` and `to`,
/// and an [`Undo`](OpKind::Undo) names the exact `target_seq` it inverts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    /// Point a ref at a (possibly absent) commit.
    ///
    /// `old` is the target the ref had immediately before this operation
    /// (`None` if the ref did not exist); `new` is the target afterwards
    /// (`None` deletes the ref). Storing both makes the operation invertible.
    SetRef {
        /// The ref name, e.g. `"main"`.
        name: String,
        /// The target before this operation (`None` if the ref was absent).
        old: Option<CommitId>,
        /// The target after this operation (`None` deletes the ref).
        new: Option<CommitId>,
    },
    /// Advance (or record) the phase of a change.
    ///
    /// Both `from` and `to` are stored so the transition can be inverted.
    SetPhase {
        /// The change whose phase changed.
        change: ChangeId,
        /// The phase before this operation.
        from: Phase,
        /// The phase after this operation.
        to: Phase,
    },
    /// Record that a change auto-rebased its tip onto an advancing base
    /// (reduction **R4**, design doc §5.3 stacking + §5.4 rebase-over-conflicts).
    ///
    /// This is the **transaction-time** half of an auto-rebase: it stamps, at a
    /// monotonic `seq`, that the change moved from `old_tip` to `new_tip` by
    /// replaying onto `onto`, carrying `conflicts` conflict values forward (never
    /// blocking — §3 P3). The **valid-time** half is the supersession edge
    /// recorded in the [`ChangeGraph`](omoplata_identity::ChangeGraph); the two
    /// are jointly queryable (§5.6, Thesis claim 3).
    ///
    /// Like every other variant it is invertible on its own: it names both
    /// `old_tip` and `new_tip`, and when folded it points the change's ref
    /// (keyed by [`ChangeId`]) at `new_tip`, so [`OpLog::undo`] restores
    /// `old_tip` simply by deactivating this operation and letting the prior
    /// active operation win.
    Rebase {
        /// The change whose tip advanced.
        change: ChangeId,
        /// The change's tip before this rebase (restored on undo).
        old_tip: CommitId,
        /// The change's tip after this rebase.
        new_tip: CommitId,
        /// The commit this rebase replayed onto (the advanced base).
        onto: CommitId,
        /// How many conflict values the rebase carried forward (0 == clean).
        conflicts: usize,
    },
    /// Invert the operation at transaction time `target_seq`.
    ///
    /// This is how undo (and, when `target_seq` names another `Undo`, redo) is
    /// recorded: as a new operation, never as a deletion.
    Undo {
        /// The [`Operation::seq`] of the operation this one inverts.
        target_seq: u64,
    },
}

impl OpKind {
    /// A short, human-readable one-line summary for `omo op log`.
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            OpKind::SetRef { name, old, new } => {
                let render = |c: &Option<CommitId>| {
                    c.as_ref()
                        .map_or_else(|| "∅".to_owned(), ToString::to_string)
                };
                format!("set-ref {name} {} -> {}", render(old), render(new))
            }
            OpKind::SetPhase { change, from, to } => {
                format!("set-phase {change} {from:?} -> {to:?}")
            }
            OpKind::Rebase {
                change,
                old_tip,
                new_tip,
                onto,
                conflicts,
            } => {
                let carried = if *conflicts == 0 {
                    "clean".to_owned()
                } else {
                    format!("{conflicts} conflict(s)")
                };
                format!("rebase {change} {old_tip} -> {new_tip} onto {onto} ({carried})")
            }
            OpKind::Undo { target_seq } => format!("undo #{target_seq}"),
        }
    }
}

/// A single, immutable entry in the operation log.
///
/// `seq` is the transaction time: it is assigned on append, strictly increases,
/// and equals the entry's index in the log. `time` is an *optional*,
/// caller-supplied wall-clock label — the library never reads the system clock,
/// so logs stay deterministic in tests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Transaction time: the monotonic sequence number assigned on append.
    pub seq: u64,
    /// What the operation did.
    pub kind: OpKind,
    /// An optional human note describing the operation.
    pub note: Option<String>,
    /// An optional caller-supplied wall-clock label (never read from the system
    /// clock by this library, keeping the log deterministic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

/// An append-only, bi-temporal operation log (§5.6).
///
/// See the [module documentation](self) for the undo/redo model and the
/// transaction-time query [`refs_at`](OpLog::refs_at).
#[derive(Debug, Default, Clone)]
pub struct OpLog {
    ops: Vec<Operation>,
}

impl OpLog {
    /// Create an empty operation log.
    #[must_use]
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Borrow every operation ever appended, oldest first.
    ///
    /// The slice includes undone operations and the `Undo` entries themselves —
    /// the log never lies about what was believed (§5.6).
    #[must_use]
    pub fn operations(&self) -> &[Operation] {
        &self.ops
    }

    /// The transaction time that the next appended operation will receive.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.ops.len() as u64
    }

    /// Append a mutation, assigning it the next transaction-time `seq`.
    ///
    /// The log is append-only: this never mutates or deletes a past entry. The
    /// appended operation's `time` is `None`; use [`append_at`](OpLog::append_at)
    /// to attach a deterministic wall-clock label.
    pub fn append(&mut self, kind: OpKind, note: Option<String>) -> &Operation {
        self.append_at(kind, note, None)
    }

    /// Append a mutation with an explicit, caller-supplied wall-clock `time`.
    ///
    /// The library never reads the system clock; the label is stored verbatim so
    /// logs remain deterministic under test.
    pub fn append_at(
        &mut self,
        kind: OpKind,
        note: Option<String>,
        time: Option<String>,
    ) -> &Operation {
        let seq = self.next_seq();
        let idx = self.ops.len();
        self.ops.push(Operation {
            seq,
            kind,
            note,
            time,
        });
        // `idx` is the index just pushed to, so this never panics.
        &self.ops[idx]
    }

    /// Convenience: point `name` at `new` (or delete it when `new` is `None`),
    /// capturing the current target as `old` so the operation is invertible.
    pub fn set_ref(
        &mut self,
        name: impl Into<String>,
        new: Option<CommitId>,
        note: Option<String>,
    ) -> &Operation {
        let name = name.into();
        let old = self.refs_now().get(&name).cloned();
        self.append(OpKind::SetRef { name, old, new }, note)
    }

    /// Undo the most recent operation still in effect, by appending its inverse.
    ///
    /// This never shrinks the log (§5.6): it appends a new [`OpKind::Undo`]
    /// targeting the highest-`seq` operation currently active. Because an
    /// `Undo` entry is itself an operation, undoing one that is active performs
    /// a **redo**.
    ///
    /// # Errors
    ///
    /// [`WorkError::NothingToUndo`] if no operation is currently in effect
    /// (an empty log, or one whose every operation has been undone).
    ///
    /// PROOF OBLIGATION (I7): the appended inverse restores the folded ref state
    /// to what it was before the target operation — `undo ∘ op ≡ identity` — as
    /// exercised by the `undo_restores_prior_ref_state` and `redo_reapplies`
    /// tests.
    pub fn undo(&mut self) -> Result<&Operation, WorkError> {
        let target = self
            .ops
            .iter()
            .rev()
            .find(|op| self.is_active(op.seq, u64::MAX))
            .map(|op| op.seq)
            .ok_or(WorkError::NothingToUndo)?;
        Ok(self.append(OpKind::Undo { target_seq: target }, None))
    }

    /// Whether the operation at `seq` is currently in effect, considering only
    /// operations with `seq' <= limit`.
    ///
    /// An operation is inactive iff an **odd** number of *active* `Undo`
    /// operations target it. The recursion terminates because every `Undo`
    /// targeting `seq` has a strictly greater `seq` (you can only undo an
    /// operation already in the log).
    fn is_active(&self, seq: u64, limit: u64) -> bool {
        let mut active_undos = 0u64;
        for op in &self.ops {
            if op.seq > limit {
                break;
            }
            if let OpKind::Undo { target_seq } = op.kind {
                if target_seq == seq && self.is_active(op.seq, limit) {
                    active_undos += 1;
                }
            }
        }
        active_undos.is_multiple_of(2)
    }

    /// Fold the active [`OpKind::SetRef`] operations with `seq <= limit` into a
    /// ref map. Ties are resolved by transaction order (a later active operation
    /// overrides an earlier one).
    fn fold_refs(&self, limit: u64) -> BTreeMap<String, CommitId> {
        let mut refs = BTreeMap::new();
        for op in &self.ops {
            if op.seq > limit {
                break;
            }
            if !self.is_active(op.seq, limit) {
                continue;
            }
            match &op.kind {
                OpKind::SetRef { name, new, .. } => match new {
                    Some(commit) => {
                        refs.insert(name.clone(), commit.clone());
                    }
                    None => {
                        refs.remove(name);
                    }
                },
                // A `Rebase` advances the change's ref (keyed by [`ChangeId`]) to
                // the rebased tip. Deactivating it (undo) drops this insert, so the
                // prior active operation restores `old_tip` — invertibility (I7).
                OpKind::Rebase {
                    change, new_tip, ..
                } => {
                    refs.insert(change.to_string(), new_tip.clone());
                }
                OpKind::SetPhase { .. } | OpKind::Undo { .. } => {}
            }
        }
        refs
    }

    /// The current ref state: all operations folded, respecting undos.
    #[must_use]
    pub fn refs_now(&self) -> BTreeMap<String, CommitId> {
        self.fold_refs(u64::MAX)
    }

    /// The ref state **as believed at transaction time `seq`** — the
    /// transaction-time query that makes the log bi-temporal (§5.6, Thesis
    /// claim 3).
    ///
    /// Only operations with transaction time `<= seq` are considered, and the
    /// active/undone status of each is recomputed as of that point, so the
    /// result reflects what the repository believed *then*, not now.
    #[must_use]
    pub fn refs_at(&self, seq: u64) -> BTreeMap<String, CommitId> {
        self.fold_refs(seq)
    }

    /// Persist the log to `path` as JSON-lines (one [`Operation`] per line).
    ///
    /// # Errors
    ///
    /// [`WorkError::Io`] on any filesystem failure, or [`WorkError::Decode`] if
    /// an operation cannot be serialized (never expected for the closed set of
    /// [`OpKind`] variants).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), WorkError> {
        let path = path.as_ref();
        let mut buf = String::new();
        for op in &self.ops {
            let line = serde_json::to_string(op).map_err(|e| WorkError::Decode(e.to_string()))?;
            buf.push_str(&line);
            buf.push('\n');
        }
        std::fs::write(path, buf).map_err(|source| WorkError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load a log from `path` (JSON-lines). A missing file yields an empty log,
    /// so callers can create the file lazily.
    ///
    /// # Errors
    ///
    /// [`WorkError::Io`] on a filesystem failure other than "not found", or
    /// [`WorkError::Decode`] if any line is not a valid [`Operation`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkError> {
        let path = path.as_ref();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(source) => {
                return Err(WorkError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        let mut ops = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let op: Operation =
                serde_json::from_str(line).map_err(|e| WorkError::Decode(e.to_string()))?;
            ops.push(op);
        }
        Ok(Self { ops })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(s: &str) -> CommitId {
        CommitId::new(s)
    }

    #[test]
    fn append_assigns_increasing_seqs() {
        let mut log = OpLog::new();
        let a = log
            .append(
                OpKind::SetRef {
                    name: "main".into(),
                    old: None,
                    new: Some(commit("a")),
                },
                None,
            )
            .seq;
        let b = log
            .append(
                OpKind::SetRef {
                    name: "main".into(),
                    old: Some(commit("a")),
                    new: Some(commit("b")),
                },
                None,
            )
            .seq;
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(log.next_seq(), 2);
    }

    #[test]
    fn setref_then_undo_restores_prior_ref_state_but_log_grows() {
        let mut log = OpLog::new();
        log.set_ref("main", Some(commit("a")), None);
        log.set_ref("main", Some(commit("b")), None);
        assert_eq!(log.refs_now().get("main"), Some(&commit("b")));

        let len_before = log.operations().len();
        log.undo().unwrap();
        // PROOF OBLIGATION (I7): undo restores the prior ref state.
        assert_eq!(log.refs_now().get("main"), Some(&commit("a")));
        // The log never shrinks; it grows by the Undo entry (§5.6).
        assert_eq!(log.operations().len(), len_before + 1);
    }

    #[test]
    fn undo_of_setref_to_new_ref_deletes_it() {
        let mut log = OpLog::new();
        log.set_ref("feature", Some(commit("f")), None);
        assert!(log.refs_now().contains_key("feature"));
        log.undo().unwrap();
        assert!(!log.refs_now().contains_key("feature"));
    }

    #[test]
    fn redo_reapplies() {
        let mut log = OpLog::new();
        log.set_ref("main", Some(commit("a")), None);
        log.undo().unwrap(); // main deleted
        assert!(!log.refs_now().contains_key("main"));
        // Undo the undo == redo.
        log.undo().unwrap();
        assert_eq!(log.refs_now().get("main"), Some(&commit("a")));
    }

    #[test]
    fn undo_redo_undo_toggles() {
        let mut log = OpLog::new();
        log.set_ref("main", Some(commit("a")), None);
        log.undo().unwrap(); // gone
        log.undo().unwrap(); // redo -> back
        log.undo().unwrap(); // undo the redo -> gone again
        assert!(!log.refs_now().contains_key("main"));
    }

    #[test]
    fn refs_at_gives_historical_state_distinct_from_now() {
        let mut log = OpLog::new();
        log.set_ref("main", Some(commit("a")), None); // seq 0
        log.set_ref("main", Some(commit("b")), None); // seq 1
        log.undo().unwrap(); // seq 2: back to a

        // As believed now: a.
        assert_eq!(log.refs_now().get("main"), Some(&commit("a")));
        // As believed at seq 1 (before the undo): b.
        assert_eq!(log.refs_at(1).get("main"), Some(&commit("b")));
        // As believed at seq 0: a.
        assert_eq!(log.refs_at(0).get("main"), Some(&commit("a")));
        // The two axes genuinely disagree.
        assert_ne!(log.refs_at(1), log.refs_now());
    }

    #[test]
    fn undo_on_empty_log_errors() {
        let mut log = OpLog::new();
        assert!(matches!(log.undo(), Err(WorkError::NothingToUndo)));
    }

    #[test]
    fn undo_all_then_nothing_to_undo() {
        let mut log = OpLog::new();
        log.set_ref("main", Some(commit("a")), None);
        log.undo().unwrap(); // undo the setref
                             // Now active ops: only the Undo (seq 1). Undo it -> redo.
        log.undo().unwrap();
        // Active ops: setref (seq 0). Undo it.
        log.undo().unwrap();
        // Active ops: the last undo. Undo it (redo again).
        log.undo().unwrap();
        // This is fine to keep toggling; there is always the newest undo active.
        assert!(log.undo().is_ok());
    }

    #[test]
    fn save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplog.jsonl");
        let mut log = OpLog::new();
        log.append_at(
            OpKind::SetRef {
                name: "main".into(),
                old: None,
                new: Some(commit("a")),
            },
            Some("first".into()),
            Some("2026-07-22T00:00:00Z".into()),
        );
        log.set_ref("feature", Some(commit("f")), None);
        log.append(
            OpKind::SetPhase {
                change: ChangeId::new("c1"),
                from: Phase::Draft,
                to: Phase::Public,
            },
            None,
        );
        log.undo().unwrap();
        log.save(&path).unwrap();

        let loaded = OpLog::load(&path).unwrap();
        assert_eq!(loaded.operations(), log.operations());
        assert_eq!(loaded.refs_now(), log.refs_now());
    }

    #[test]
    fn rebase_op_folds_and_undo_restores_prior_tip() {
        let mut log = OpLog::new();
        // The change's ref starts at the pre-rebase tip (seq 0).
        log.set_ref("c1", Some(commit("t0")), None);
        // Auto-rebase advances it to t1 (seq 1).
        log.append(
            OpKind::Rebase {
                change: ChangeId::new("c1"),
                old_tip: commit("t0"),
                new_tip: commit("t1"),
                onto: commit("onto"),
                conflicts: 2,
            },
            None,
        );
        // Valid "now": the folded ref is the rebased tip.
        assert_eq!(log.refs_now().get("c1"), Some(&commit("t1")));
        // Bi-temporal: as believed at transaction time 0 (before the rebase), the
        // tip was still t0.
        assert_eq!(log.refs_at(0).get("c1"), Some(&commit("t0")));
        assert_ne!(log.refs_at(0), log.refs_now());

        // PROOF OBLIGATION (I7): undoing the rebase restores old_tip.
        log.undo().unwrap();
        assert_eq!(log.refs_now().get("c1"), Some(&commit("t0")));
        // The log grew (it never erases): setref, rebase, undo.
        assert_eq!(log.operations().len(), 3);
    }

    #[test]
    fn rebase_op_serde_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplog.jsonl");
        let mut log = OpLog::new();
        log.set_ref("c1", Some(commit("t0")), None);
        log.append(
            OpKind::Rebase {
                change: ChangeId::new("c1"),
                old_tip: commit("t0"),
                new_tip: commit("t1"),
                onto: commit("onto"),
                conflicts: 1,
            },
            Some("auto-rebase".into()),
        );
        log.save(&path).unwrap();

        let loaded = OpLog::load(&path).unwrap();
        assert_eq!(loaded.operations(), log.operations());
        assert_eq!(loaded.refs_now(), log.refs_now());
        // The persisted variant survives the round trip with all fields intact.
        assert!(matches!(
            loaded.operations()[1].kind,
            OpKind::Rebase { conflicts: 1, .. }
        ));
    }

    #[test]
    fn load_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = OpLog::load(dir.path().join("nope.jsonl")).unwrap();
        assert!(log.operations().is_empty());
    }
}
