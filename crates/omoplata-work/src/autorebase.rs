//! The change-graph auto-rebase loop across the op log — reduction **R4**.
//!
//! This module ties together the four lower layers so a *change* auto-rebases
//! onto an advancing base, **without ever blocking**, and records the move on
//! both of the design doc's time axes at once:
//!
//! * the verified algebra ([`omoplata_algebra::rebase`] / [`rebase_stack`]) does
//!   the line-level replay and carries conflicts as **values** (§5.4, P3);
//! * the object store ([`omoplata_store::Repository`]) holds each commit's
//!   content as a blob — a commit id *is* the [`ObjectId`] of its text;
//! * the op log ([`crate::OpLog`]) records each rebase as an [`OpKind::Rebase`]
//!   entry at **transaction time** (§5.6);
//! * the change graph ([`ChangeGraph`]) records each rebase as a **supersession
//!   edge** (`new_tip` obsoletes `old_tip`) at **valid time** (§5.3).
//!
//! # What the design doc asks for
//!
//! **§5.3 — Change identity and supersession:**
//!
//! > A *change* is a stable-ID node in the change graph. Commits are its
//! > revisions; **supersession edges record that revision B obsoletes revision
//! > A.** … stacking are properties of changes, not commits.
//!
//! **§5.4 — Conflicts as values:**
//!
//! > **Rebase maps over conflicts; resolution is a commit that collapses the
//! > term.**
//!
//! **§5.6 — Bi-temporal operation log:**
//!
//! > Every repository mutation (commit, **rebase**, phase change, fetch, undo
//! > itself) is an operation with transaction time. **Valid-time assertions (the
//! > change graph) and transaction-time assertions (the op log) are jointly
//! > queryable.**
//!
//! **Thesis claim 3 — History is bi-temporal and queryable:**
//!
//! > The repository records both what was true (**valid time**: the commit
//! > graph, supersession of changes) and what was believed (**transaction
//! > time**: the operation log).
//!
//! # PROOF OBLIGATION (§5.6 + Thesis claim 3 — bi-temporal joint queryability)
//!
//! Each [`RebaseEngine::auto_rebase`] writes exactly one fact to each axis: an
//! [`OpKind::Rebase`] op (transaction time) *and* a supersession edge (valid
//! time). [`RebaseEngine::history`] then reconstructs both from the two stores
//! for the same change: its **current tip** comes from [`ChangeGraph::tip`]
//! (valid time — what is true now) while its **rebase sequence** and any
//! **as-of-then tip** come from the op log ([`OpLog::operations`] and
//! [`OpLog::refs_at`] — what was believed at a transaction time). Because the op
//! log is append-only and never rewrites a past entry, the transaction-time view
//! at seq *t* is stable forever, which is what makes "what did we think this
//! change's tip was, as of *t*" a first-class query rather than reflog
//! archaeology. Guarded by the `history_is_jointly_queryable` and
//! `refs_at_shows_pre_rebase_tip` tests.
//!
//! No `unwrap`/`expect`/`panic` appears in non-test code; every fallible step
//! returns [`WorkError`].
//!
//! [`ObjectId`]: omoplata_store::ObjectId
//! [`OpKind::Rebase`]: crate::OpKind::Rebase
//! [`rebase_stack`]: omoplata_algebra::rebase_stack

use omoplata_algebra::{rebase, Conflict, Doc};
use omoplata_identity::{Change, ChangeGraph, ChangeId, CommitId, Phase};
use omoplata_store::{Object, ObjectId, Repository};

use crate::oplog::{OpKind, OpLog};
use crate::WorkError;

/// The outcome of auto-rebasing a single change (see
/// [`RebaseEngine::auto_rebase`]).
///
/// `conflicts` are carried as **values** (design doc §5.4): a non-empty vector
/// never means the rebase failed — `new_tip` is always a real, stored commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseOutcome {
    /// The commit id of the rebased content (the change's new tip).
    pub new_tip: CommitId,
    /// The conflict values the rebase carried forward; empty iff `clean`.
    pub conflicts: Vec<Conflict>,
    /// Whether the rebase produced no conflicts (`conflicts.is_empty()`).
    pub clean: bool,
}

/// One entry of a change to rebase in a stack (see
/// [`RebaseEngine::auto_rebase_stack`]).
///
/// `base` is the change's three-way base and `tip` its current (pre-rebase)
/// tip; the stack threads each rebased result into the next change's `onto`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackItem {
    /// The change's stable identity.
    pub change: ChangeId,
    /// The change's three-way base commit.
    pub base: CommitId,
    /// The change's current tip commit (the content to replay).
    pub tip: CommitId,
}

impl StackItem {
    /// Convenience constructor.
    #[must_use]
    pub fn new(change: ChangeId, base: CommitId, tip: CommitId) -> Self {
        Self { change, base, tip }
    }
}

/// A single recorded rebase, read back from the op log (transaction time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseRecord {
    /// The transaction time (op-log `seq`) at which the rebase was recorded.
    pub seq: u64,
    /// The change's tip before the rebase.
    pub old_tip: CommitId,
    /// The change's tip after the rebase.
    pub new_tip: CommitId,
    /// The commit the rebase replayed onto.
    pub onto: CommitId,
    /// How many conflict values the rebase carried forward.
    pub conflicts: usize,
}

/// The joint bi-temporal view of one change (see [`RebaseEngine::history`]).
///
/// Combines the **valid-time** answer ([`current_tip`](Self::current_tip), from
/// the change graph's supersession chain) with the **transaction-time** answer
/// ([`rebases`](Self::rebases), the ordered op-log history). This is the §5.6
/// "jointly queryable" surface made concrete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeHistory {
    /// The change this history is for.
    pub change: ChangeId,
    /// Valid time: the change's current, non-superseded tip
    /// ([`ChangeGraph::tip`]). `None` if the change has no revisions yet.
    pub current_tip: Option<CommitId>,
    /// Transaction time: every recorded rebase of the change, oldest first.
    pub rebases: Vec<RebaseRecord>,
}

/// The auto-rebase engine (reduction **R4**).
///
/// Owns a repository handle, a bi-temporal [`OpLog`], and a [`ChangeGraph`], and
/// drives changes' tips forward onto an advancing base. Construct it over an
/// existing op log with [`RebaseEngine::with_log`] (the CLI path — the log is
/// persisted between invocations) or empty with [`RebaseEngine::new`].
#[derive(Debug)]
pub struct RebaseEngine {
    repo: Repository,
    log: OpLog,
    graph: ChangeGraph,
}

impl RebaseEngine {
    /// Create an engine over `repo` with an empty op log and change graph.
    #[must_use]
    pub fn new(repo: Repository) -> Self {
        Self {
            repo,
            log: OpLog::new(),
            graph: ChangeGraph::new(),
        }
    }

    /// Create an engine over `repo` continuing an existing op `log` (the change
    /// graph starts empty and is rebuilt from the rebases performed this
    /// session).
    #[must_use]
    pub fn with_log(repo: Repository, log: OpLog) -> Self {
        Self {
            repo,
            log,
            graph: ChangeGraph::new(),
        }
    }

    /// Borrow the underlying object store handle.
    #[must_use]
    pub fn repository(&self) -> &Repository {
        &self.repo
    }

    /// Borrow the op log (transaction-time axis).
    #[must_use]
    pub fn log(&self) -> &OpLog {
        &self.log
    }

    /// Mutably borrow the op log (e.g. to establish a change's ref before a
    /// rebase, or to undo a rebase).
    pub fn log_mut(&mut self) -> &mut OpLog {
        &mut self.log
    }

    /// Borrow the change graph (valid-time axis).
    #[must_use]
    pub fn graph(&self) -> &ChangeGraph {
        &self.graph
    }

    /// Consume the engine, returning its op log and change graph (e.g. to persist
    /// the log after a batch of rebases).
    #[must_use]
    pub fn into_parts(self) -> (OpLog, ChangeGraph) {
        (self.log, self.graph)
    }

    /// Load a commit's content (a [`Doc`]) from the object store.
    ///
    /// A commit id is the [`ObjectId`] of its text blob, so this parses the id,
    /// reads the blob, and decodes it as UTF-8.
    ///
    /// [`ObjectId`]: omoplata_store::ObjectId
    fn load_doc(&self, commit: &CommitId) -> Result<Doc, WorkError> {
        let id: ObjectId = commit
            .as_str()
            .parse()
            .map_err(|e| WorkError::Content(format!("malformed commit id {commit}: {e}")))?;
        match self.repo.read_object(&id)? {
            Object::Blob(blob) => {
                let text = std::str::from_utf8(blob.bytes()).map_err(|e| {
                    WorkError::Content(format!("commit {commit} is not UTF-8: {e}"))
                })?;
                Ok(Doc::from_str(text))
            }
            Object::Tree(_) => Err(WorkError::Content(format!(
                "commit {commit} is a tree, not text content"
            ))),
        }
    }

    /// Store a rebased [`Doc`] as a blob and return its commit id.
    fn store_doc(&self, doc: &Doc) -> Result<CommitId, WorkError> {
        let id = self.repo.write_blob(doc.to_string().into_bytes())?;
        Ok(CommitId::new(id.to_string()))
    }

    /// Ensure `commit` is a registered revision of `change`, creating the change
    /// (as [`Draft`](Phase::Draft)) on first sight. Idempotent per commit.
    fn ensure_revision(&mut self, change: &ChangeId, commit: &CommitId) -> Result<(), WorkError> {
        match self.graph.change(change) {
            None => {
                self.graph.add_change(Change::new(
                    change.clone(),
                    vec![commit.clone()],
                    Phase::Draft,
                ));
            }
            Some(c) if !c.revisions.contains(commit) => {
                self.graph.add_revision(change, commit.clone())?;
            }
            Some(_) => {}
        }
        Ok(())
    }

    /// Auto-rebase one change onto an advanced base.
    ///
    /// Loads `base_commit`, `old_tip_commit` (my content) and `onto_commit` from
    /// the store, replays my change with [`omoplata_algebra::rebase`], stores the
    /// rebased document as a new blob (`new_tip`), and records the move on **both
    /// time axes**: an [`OpKind::Rebase`] op (transaction time) and a
    /// supersession edge `old_tip -> new_tip` in the change graph (valid time).
    ///
    /// Conflicts are carried as **values** in [`RebaseOutcome::conflicts`] — a
    /// conflicted rebase still produces a real `new_tip` and still records both
    /// facts; it never blocks (design doc §3 P3, §5.4).
    ///
    /// A rebase whose result is byte-identical to `old_tip` (e.g. replaying onto
    /// the base itself) yields `new_tip == old_tip`: the op is still recorded, but
    /// no self-supersession edge is added (that would violate I6's acyclicity).
    ///
    /// # Errors
    ///
    /// [`WorkError::Store`] if any content cannot be read or written,
    /// [`WorkError::Content`] if a commit's blob is malformed or non-UTF-8, or
    /// [`WorkError::Identity`] if the change graph rejects the supersession
    /// (e.g. the change is public — P5/I6).
    pub fn auto_rebase(
        &mut self,
        change_id: &ChangeId,
        base_commit: &CommitId,
        old_tip_commit: &CommitId,
        onto_commit: &CommitId,
    ) -> Result<RebaseOutcome, WorkError> {
        let base = self.load_doc(base_commit)?;
        let mine = self.load_doc(old_tip_commit)?;
        let onto = self.load_doc(onto_commit)?;

        // The verified algebra does the replay; conflicts come back as values.
        let rebased = rebase(&base, &mine, &onto);
        let new_tip = self.store_doc(&rebased.result)?;

        // Transaction-time fact: one Rebase op stamped at the next seq (§5.6).
        self.log.append(
            OpKind::Rebase {
                change: change_id.clone(),
                old_tip: old_tip_commit.clone(),
                new_tip: new_tip.clone(),
                onto: onto_commit.clone(),
                conflicts: rebased.conflicts.len(),
            },
            None,
        );

        // Valid-time fact: new_tip obsoletes old_tip in the change graph (§5.3).
        // Register both as revisions first so the edge has no orphan endpoint (I6).
        self.ensure_revision(change_id, old_tip_commit)?;
        if new_tip != *old_tip_commit {
            self.ensure_revision(change_id, &new_tip)?;
            self.graph.supersede(old_tip_commit, &new_tip)?;
        }

        Ok(RebaseOutcome {
            new_tip,
            clean: rebased.conflicts.is_empty(),
            conflicts: rebased.conflicts,
        })
    }

    /// Auto-rebase a **stack** of changes onto an advanced base — the R4 loop.
    ///
    /// Each change in `stack` is rebased in order (per [`rebase_stack`]
    /// semantics): change *i* replays with its own `base` as the three-way base
    /// and the *accumulated* rebased tip as `onto`, and its `new_tip` becomes the
    /// `onto` the next change lands on. One [`OpKind::Rebase`] op and one
    /// supersession edge are recorded per change.
    ///
    /// A conflict mid-stack **never aborts** the loop: because [`auto_rebase`]
    /// carries conflicts as values rather than erroring, later independent changes
    /// still rebase (design doc §3 P3 / P4 — async resolution keeps the fleet
    /// unblocked). The returned vector has one [`RebaseOutcome`] per stack entry,
    /// in stack order.
    ///
    /// # Errors
    ///
    /// Propagates the same errors as [`auto_rebase`] — but note a *conflict* is
    /// not an error.
    ///
    /// [`auto_rebase`]: RebaseEngine::auto_rebase
    /// [`rebase_stack`]: omoplata_algebra::rebase_stack
    pub fn auto_rebase_stack(
        &mut self,
        stack: &[StackItem],
        onto: &CommitId,
    ) -> Result<Vec<RebaseOutcome>, WorkError> {
        let mut out = Vec::with_capacity(stack.len());
        let mut acc = onto.clone();
        for item in stack {
            let outcome = self.auto_rebase(&item.change, &item.base, &item.tip, &acc)?;
            acc = outcome.new_tip.clone();
            out.push(outcome);
        }
        Ok(out)
    }

    /// The joint bi-temporal history of `change` (§5.6, Thesis claim 3).
    ///
    /// Reads the **valid-time** current tip from the change graph and the
    /// **transaction-time** rebase sequence from the op log, returning both in a
    /// single [`ChangeHistory`]. See this module's PROOF OBLIGATION note.
    #[must_use]
    pub fn history(&self, change: &ChangeId) -> ChangeHistory {
        let current_tip = self.graph.tip(change).cloned();
        let rebases = self
            .log
            .operations()
            .iter()
            .filter_map(|op| match &op.kind {
                OpKind::Rebase {
                    change: c,
                    old_tip,
                    new_tip,
                    onto,
                    conflicts,
                } if c == change => Some(RebaseRecord {
                    seq: op.seq,
                    old_tip: old_tip.clone(),
                    new_tip: new_tip.clone(),
                    onto: onto.clone(),
                    conflicts: *conflicts,
                }),
                _ => None,
            })
            .collect();
        ChangeHistory {
            change: change.clone(),
            current_tip,
            rebases,
        }
    }

    /// The tip the change was *believed* to have as of transaction time `seq` —
    /// the as-of-then query (§5.6). Complements [`history`](Self::history)'s
    /// valid-time [`current_tip`](ChangeHistory::current_tip).
    #[must_use]
    pub fn tip_as_of(&self, change: &ChangeId, seq: u64) -> Option<CommitId> {
        self.log.refs_at(seq).get(change.as_str()).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A fresh repository plus the commit ids of three stored documents.
    struct Fixture {
        _dir: TempDir,
        engine: RebaseEngine,
    }

    fn store(repo: &Repository, text: &str) -> CommitId {
        let id = repo.write_blob(text.as_bytes().to_vec()).unwrap();
        CommitId::new(id.to_string())
    }

    fn new_engine() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        Fixture {
            _dir: dir,
            engine: RebaseEngine::new(repo),
        }
    }

    fn content(engine: &RebaseEngine, commit: &CommitId) -> String {
        engine.load_doc(commit).unwrap().to_string()
    }

    #[test]
    fn auto_rebase_independent_advance_is_clean() {
        let mut fx = new_engine();
        let repo = fx.engine.repository().clone();
        let base = store(&repo, "a\nb\nc\nd");
        let mine = store(&repo, "a\nB\nc\nd"); // I edit line 1
        let onto = store(&repo, "a\nb\nc\nD"); // base advanced line 3

        let change = ChangeId::new("c1");
        let outcome = fx.engine.auto_rebase(&change, &base, &mine, &onto).unwrap();

        assert!(outcome.clean);
        assert!(outcome.conflicts.is_empty());
        // The new tip carries BOTH edits.
        assert_eq!(content(&fx.engine, &outcome.new_tip), "a\nB\nc\nD");

        // Op log gained exactly one Rebase entry (transaction time).
        let rebases: Vec<_> = fx
            .engine
            .log()
            .operations()
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Rebase { .. }))
            .collect();
        assert_eq!(rebases.len(), 1);

        // Change graph tip advanced to new_tip; old tip is superseded (valid time).
        assert_eq!(fx.engine.graph().tip(&change), Some(&outcome.new_tip));
        assert_ne!(outcome.new_tip, mine);
    }

    #[test]
    fn auto_rebase_overlapping_advance_carries_conflicts() {
        let mut fx = new_engine();
        let repo = fx.engine.repository().clone();
        let base = store(&repo, "a\nb\nc");
        let mine = store(&repo, "a\nX\nc"); // I edit line 1
        let onto = store(&repo, "a\nY\nc"); // base edits the same line

        let change = ChangeId::new("c1");
        let outcome = fx.engine.auto_rebase(&change, &base, &mine, &onto).unwrap();

        // Conflict carried as a value; the rebase did NOT fail.
        assert!(!outcome.clean);
        assert_eq!(outcome.conflicts.len(), 1);
        // A real new tip was still created and stored (conflict markers rendered).
        let text = content(&fx.engine, &outcome.new_tip);
        assert!(text.contains("<<<<<<< mine"));
        assert!(text.contains(">>>>>>> onto"));
        // Both facts recorded despite the conflict.
        assert_eq!(fx.engine.graph().tip(&change), Some(&outcome.new_tip));
        assert!(fx
            .engine
            .log()
            .operations()
            .iter()
            .any(|o| matches!(&o.kind, OpKind::Rebase { conflicts: 1, .. })));
    }

    #[test]
    fn auto_rebase_stack_loops_over_changes() {
        // A stack of three changes, each editing a distinct line, onto a base that
        // advanced a fourth line — all independent.
        // Edits are kept on non-adjacent lines (anchors between them) so each lands
        // in its own diff3 region rather than merging into a false conflict.
        let mut fx = new_engine();
        let repo = fx.engine.repository().clone();

        let base = store(&repo, "a\nb\nc\nd\ne\nf\ng");
        let c1_tip = store(&repo, "A\nb\nc\nd\ne\nf\ng"); // edits line 0
        let c2_tip = store(&repo, "a\nb\nC\nd\ne\nf\ng"); // edits line 2
        let c3_tip = store(&repo, "a\nb\nc\nd\nE\nf\ng"); // edits line 4
        let onto = store(&repo, "a\nb\nc\nd\ne\nf\nG"); // base advanced line 6

        let stack = vec![
            StackItem::new(ChangeId::new("c1"), base.clone(), c1_tip),
            StackItem::new(ChangeId::new("c2"), base.clone(), c2_tip),
            StackItem::new(ChangeId::new("c3"), base.clone(), c3_tip),
        ];
        let outcomes = fx.engine.auto_rebase_stack(&stack, &onto).unwrap();

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|o| o.clean));
        // The last change's rebased tip carries every edit plus the base advance.
        assert_eq!(
            content(&fx.engine, &outcomes[2].new_tip),
            "A\nb\nC\nd\nE\nf\nG"
        );

        // N Rebase ops appended, N supersession edges (each tip advanced).
        let n_rebases = fx
            .engine
            .log()
            .operations()
            .iter()
            .filter(|o| matches!(o.kind, OpKind::Rebase { .. }))
            .count();
        assert_eq!(n_rebases, 3);
        assert_eq!(
            fx.engine.graph().tip(&ChangeId::new("c1")),
            Some(&outcomes[0].new_tip)
        );
        assert_eq!(
            fx.engine.graph().tip(&ChangeId::new("c3")),
            Some(&outcomes[2].new_tip)
        );
    }

    #[test]
    fn stack_mid_conflict_does_not_stop_later_changes() {
        // Middle change conflicts with the advanced base; the third, independent
        // change must still rebase and land its edit.
        // Edits sit on well-separated lines so only the intended overlap (line 3)
        // conflicts; the independent line-0 and line-6 edits stay clean.
        let mut fx = new_engine();
        let repo = fx.engine.repository().clone();

        let base = store(&repo, "a\nb\nc\nd\ne\nf\ng");
        let c1_tip = store(&repo, "A\nb\nc\nd\ne\nf\ng"); // edits line 0 (independent)
        let c2_tip = store(&repo, "a\nb\nc\nX\ne\nf\ng"); // edits line 3 (conflicts)
        let c3_tip = store(&repo, "a\nb\nc\nd\ne\nf\nG"); // edits line 6 (independent)
        let onto = store(&repo, "a\nb\nc\nY\ne\nf\ng"); // base edits line 3

        let stack = vec![
            StackItem::new(ChangeId::new("c1"), base.clone(), c1_tip),
            StackItem::new(ChangeId::new("c2"), base.clone(), c2_tip),
            StackItem::new(ChangeId::new("c3"), base.clone(), c3_tip),
        ];
        let outcomes = fx.engine.auto_rebase_stack(&stack, &onto).unwrap();

        assert_eq!(outcomes.len(), 3);
        assert!(outcomes[0].clean);
        assert!(!outcomes[1].clean); // the conflict
                                     // The third change still applied its line-6 edit despite the mid conflict.
        assert!(content(&fx.engine, &outcomes[2].new_tip).ends_with("\nG"));
        // All three rebases were recorded.
        assert_eq!(
            fx.engine
                .log()
                .operations()
                .iter()
                .filter(|o| matches!(o.kind, OpKind::Rebase { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn refs_at_shows_pre_rebase_tip() {
        // Bi-temporal: after a rebase, refs_now shows new_tip but refs_at the
        // pre-rebase seq shows the old tip.
        let mut fx = new_engine();
        let repo = fx.engine.repository().clone();
        let base = store(&repo, "a\nb\nc\nd");
        let mine = store(&repo, "a\nB\nc\nd");
        let onto = store(&repo, "a\nb\nc\nD");

        let change = ChangeId::new("c1");
        // Establish the change's ref at its pre-rebase tip (seq 0).
        fx.engine
            .log_mut()
            .set_ref(change.to_string(), Some(mine.clone()), None);
        let seq_before = fx.engine.log().next_seq(); // the rebase will get this seq
        let outcome = fx.engine.auto_rebase(&change, &base, &mine, &onto).unwrap();

        assert_eq!(
            fx.engine.log().refs_now().get(change.as_str()),
            Some(&outcome.new_tip)
        );
        // As believed just before the rebase op: the old tip.
        assert_eq!(fx.engine.tip_as_of(&change, seq_before - 1), Some(mine));
    }

    #[test]
    fn history_is_jointly_queryable() {
        let mut fx = new_engine();
        let repo = fx.engine.repository().clone();
        let base = store(&repo, "a\nb\nc\nd");
        let mine = store(&repo, "a\nB\nc\nd");
        let onto = store(&repo, "a\nb\nc\nD");

        let change = ChangeId::new("c1");
        let outcome = fx.engine.auto_rebase(&change, &base, &mine, &onto).unwrap();

        let hist = fx.engine.history(&change);
        // Valid time: the change graph's current tip.
        assert_eq!(hist.current_tip.as_ref(), Some(&outcome.new_tip));
        // Transaction time: the op log's rebase sequence.
        assert_eq!(hist.rebases.len(), 1);
        assert_eq!(hist.rebases[0].old_tip, mine);
        assert_eq!(hist.rebases[0].new_tip, outcome.new_tip);
        assert_eq!(hist.rebases[0].conflicts, 0);
    }

    #[test]
    fn auto_rebase_is_deterministic() {
        // Two independent engines rebasing the same inputs reach the same tip id
        // and the same rendered content (content-addressed determinism).
        let run = || {
            let mut fx = new_engine();
            let repo = fx.engine.repository().clone();
            let base = store(&repo, "a\nb\nc\nd\ne");
            let mine = store(&repo, "a\nB\nc\nd\ne");
            let onto = store(&repo, "a\nb\nC\nd\nE");
            let outcome = fx
                .engine
                .auto_rebase(&ChangeId::new("c1"), &base, &mine, &onto)
                .unwrap();
            let text = content(&fx.engine, &outcome.new_tip);
            (outcome.new_tip, text)
        };
        assert_eq!(run(), run());
    }
}
