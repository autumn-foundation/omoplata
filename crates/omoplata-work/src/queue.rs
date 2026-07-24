//! The Merge Queue and submission landing — design doc §5.10.
//!
//! **Landing is the merge queue (v1).** `land` enqueues an approved [`Submission`].
//! The queue gates on approvals-per-policy plus dynamic validation (§3 P9);
//! landing is the `Draft -> Public` phase transition (§3 P5, §5.3).
//!
//! Batching by Tier 0: pairwise-disjoint changes batch, test as one, and land
//! in parallel; overlapping changes serialize with commutation checks.
//!
//! # Named queues (ADR-0009)
//!
//! A repository can carry any number of **named landing queues**, each with its
//! own [`QueuePolicy`]: a P9 validator command, an approval requirement, and a
//! carried-conflict rule. This is the release-line story: what a git shop
//! models as a `release/*` branch plus branch-filtered CI is here a *policy
//! object* attached to the landing gate — validation runs **before** the
//! `Draft -> Public` transition, in-band, not after a merge commit exists.
//! A change is landed *into a queue*; landing the same change into a second
//! queue is the backport story, with change identity preserved (one change
//! object, two landings, no cherry-pick fork).
//!
//! The queue named `trunk` always exists implicitly ([`QueuePolicy::trunk`]):
//! permissive about carried conflict values (the fleet keeps landing; §5.4)
//! and validator-free unless registered explicitly with a stricter policy.
//! Landing into `trunk` writes the legacy `public/<change>` refs; every other
//! queue lands at `public/<queue>/<change>`.

use std::path::{Path, PathBuf};

use omoplata_identity::{ChangeGraph, ChangeId, Phase, Submission, SubmissionId};
use omoplata_store::{atomic_write, Repository};
use serde::{Deserialize, Serialize};

use crate::error::WorkError;
use crate::oplog::{OpKind, OpLog};

/// The policy of one named landing queue (ADR-0009).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuePolicy {
    /// The queue's unique name (e.g. `trunk`, `release-1.2`).
    pub name: String,
    /// Optional human description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// P9 dynamic-validation command, run against the materialized content of
    /// a submission before it may land. `{}` is substituted with the
    /// materialized directory; without a placeholder the directory is appended.
    /// `None` = no validation gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validate: Option<String>,
    /// Whether landing requires the submission to be approved. Waivable for
    /// experimental queues; multi-approval thresholds await the bi-temporal
    /// approval model (§5.6) and are future work in ADR-0009.
    #[serde(default = "default_true")]
    pub require_approval: bool,
    /// Whether a submission whose content still **carries conflict values**
    /// (§5.4 marker blocks) may land in this queue. `trunk` defaults to
    /// permissive — the fleet keeps landing while a conflict awaits its
    /// resolution — while registered queues default to strict, the right
    /// posture for a release line.
    #[serde(default)]
    pub allow_carried: bool,
}

fn default_true() -> bool {
    true
}

impl QueuePolicy {
    /// The implicit `trunk` queue: approval required, no validator,
    /// carried conflict values allowed (§5.4 — landing throughput must not
    /// wait on resolution latency).
    #[must_use]
    pub fn trunk() -> Self {
        Self {
            name: "trunk".to_owned(),
            description: Some("the implicit fleet trunk".to_owned()),
            validate: None,
            require_approval: true,
            allow_carried: true,
        }
    }
}

/// The set of registered landing queues, persisted in the shared `.omoplata`.
///
/// Mirrors [`WorkspaceRegistry`](crate::WorkspaceRegistry): pretty JSON at
/// [`QueueRegistry::path_in`], crash-atomic writes, and mutation only under the
/// repository lock via [`mutate_locked`](QueueRegistry::mutate_locked).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRegistry {
    queues: Vec<QueuePolicy>,
}

impl QueueRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The canonical registry path: `.omoplata/queues.json`.
    #[must_use]
    pub fn path_in(repo: &Repository) -> PathBuf {
        repo.control_dir().join("queues.json")
    }

    /// Every registered queue, in registration order.
    #[must_use]
    pub fn queues(&self) -> &[QueuePolicy] {
        &self.queues
    }

    /// Borrow a registered queue by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&QueuePolicy> {
        self.queues.iter().find(|q| q.name == name)
    }

    /// Resolve a queue name to its policy: a registered policy wins; the name
    /// `trunk` falls back to the implicit [`QueuePolicy::trunk`] (so `trunk`
    /// can be re-registered with a stricter policy).
    ///
    /// # Errors
    ///
    /// [`WorkError::UnknownQueue`] for an unregistered non-`trunk` name.
    pub fn resolve(&self, name: &str) -> Result<QueuePolicy, WorkError> {
        if let Some(q) = self.get(name) {
            return Ok(q.clone());
        }
        if name == "trunk" {
            return Ok(QueuePolicy::trunk());
        }
        Err(WorkError::UnknownQueue(name.to_owned()))
    }

    /// Register a queue.
    ///
    /// # Errors
    ///
    /// [`WorkError::QueueExists`] if a queue with the same name is registered.
    pub fn add(&mut self, policy: QueuePolicy) -> Result<&QueuePolicy, WorkError> {
        if self.get(&policy.name).is_some() {
            return Err(WorkError::QueueExists(policy.name));
        }
        self.queues.push(policy);
        let idx = self.queues.len() - 1;
        Ok(&self.queues[idx])
    }

    /// Remove a queue by name, returning it.
    ///
    /// # Errors
    ///
    /// [`WorkError::UnknownQueue`] if no queue with `name` is registered.
    pub fn remove(&mut self, name: &str) -> Result<QueuePolicy, WorkError> {
        let idx = self
            .queues
            .iter()
            .position(|q| q.name == name)
            .ok_or_else(|| WorkError::UnknownQueue(name.to_owned()))?;
        Ok(self.queues.remove(idx))
    }

    /// Persist the registry to `path` as pretty JSON, crash-atomically (same
    /// discipline as the workspace registry and the op log, ADR-0008).
    ///
    /// # Errors
    ///
    /// [`WorkError::Decode`] if serialization fails, [`WorkError::Store`] on
    /// filesystem failure.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), WorkError> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| WorkError::Decode(e.to_string()))?;
        atomic_write(path.as_ref(), &json)?;
        Ok(())
    }

    /// Load a registry from `path`. A missing file yields an empty registry.
    ///
    /// # Errors
    ///
    /// [`WorkError::Io`] on a filesystem failure other than "not found", or
    /// [`WorkError::Decode`] for invalid registry JSON.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkError> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(source) => {
                return Err(WorkError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        serde_json::from_slice(&bytes).map_err(|e| WorkError::Decode(e.to_string()))
    }

    /// Locked, crash-atomic read-modify-write on the repository's queue
    /// registry, mirroring [`OpLog::mutate_locked`].
    ///
    /// # Errors
    ///
    /// [`WorkError::Store`] if the lock cannot be acquired, any error `f`
    /// returns, or [`WorkError::Io`]/[`WorkError::Decode`] from load/save.
    pub fn mutate_locked<F, T>(repo: &Repository, f: F) -> Result<T, WorkError>
    where
        F: FnOnce(&mut QueueRegistry) -> Result<T, WorkError>,
    {
        let _guard = repo.lock()?;
        let path = Self::path_in(repo);
        let mut registry = QueueRegistry::load(&path)?;
        let out = f(&mut registry)?;
        registry.save(&path)?;
        Ok(out)
    }
}

/// The observed facts a queue's gates are evaluated against. The caller (the
/// CLI, later the landing daemon) materializes the submission's content and
/// runs the validator; the queue applies policy to what was observed — the
/// same split as driver-proposes / kernel-admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueGates {
    /// How many conflict values (§5.4 marker blocks) the submission's
    /// materialized content carries.
    pub carried_values: usize,
    /// The P9 validator verdict: `Some(true)` passed, `Some(false)` failed,
    /// `None` not run. When the policy configures a validator, only
    /// `Some(true)` lands.
    pub validated: Option<bool>,
}

/// The result of landing a submission through the merge queue (§5.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandResult {
    /// The landed submission ID.
    pub submission_id: SubmissionId,
    /// The landed change IDs whose phases advanced to [`Public`](Phase::Public).
    pub landed_changes: Vec<omoplata_identity::ChangeId>,
    /// Summary message.
    pub message: String,
}

/// Land an approved submission, advancing its changes to [`Phase::Public`] (§5.10).
///
/// # Errors
///
/// * [`WorkError::SubmissionNotApproved`] if `submission` is not approved.
/// * [`WorkError::UnknownChange`] if any change in `submission` is missing from `change_graph`.
pub fn land_submission(
    submission: &Submission,
    change_graph: &mut ChangeGraph,
    op_log: &mut OpLog,
) -> Result<LandResult, WorkError> {
    if !submission.is_approved() {
        return Err(WorkError::SubmissionNotApproved(submission.id.to_string()));
    }

    let mut landed_changes = Vec::new();

    for change_id in &submission.changes {
        let change = change_graph
            .change(change_id)
            .ok_or_else(|| WorkError::UnknownChange(change_id.to_string()))?;

        if change.phase == Phase::Draft {
            change_graph
                .set_phase(change_id, Phase::Public)
                .map_err(WorkError::Identity)?;
        }
        landed_changes.push(change_id.clone());

        // Append a ref operation representing the public landing of this change tip.
        let ref_name = format!("public/{}", change_id.as_str());
        let old_ref = op_log.refs_now().get(&ref_name).cloned();
        let tip = change_graph.tip(change_id).cloned();

        op_log.append(
            OpKind::SetRef {
                name: ref_name,
                old: old_ref,
                new: tip,
            },
            Some(format!("land change {}", change_id)),
        );
    }

    Ok(LandResult {
        submission_id: submission.id.clone(),
        landed_changes,
        message: format!("Submission {} landed successfully", submission.id),
    })
}

/// Land a submission **into a named queue**, applying the queue's policy
/// (ADR-0009): approval requirement, carried-conflict rule, and the P9
/// validation verdict. Refused landings mutate nothing.
///
/// Landing into `trunk` writes the legacy `public/<change>` refs; any other
/// queue lands at `public/<queue>/<change>` — which is what lets the *same*
/// change land in several queues (trunk + a release line) without forking its
/// identity.
///
/// # Errors
///
/// * [`WorkError::SubmissionNotApproved`] — policy requires approval and the
///   submission has none.
/// * [`WorkError::QueueCarriedRefused`] — content carries conflict values and
///   the policy is strict.
/// * [`WorkError::QueueValidationFailed`] — a validator is configured and the
///   observed verdict is not a pass.
/// * [`WorkError::UnknownChange`] — a change is missing from `change_graph`.
pub fn land_submission_in_queue(
    submission: &Submission,
    policy: &QueuePolicy,
    gates: &QueueGates,
    change_graph: &mut ChangeGraph,
    op_log: &mut OpLog,
) -> Result<LandResult, WorkError> {
    if policy.require_approval && !submission.is_approved() {
        return Err(WorkError::SubmissionNotApproved(submission.id.to_string()));
    }
    if gates.carried_values > 0 && !policy.allow_carried {
        return Err(WorkError::QueueCarriedRefused {
            queue: policy.name.clone(),
            count: gates.carried_values,
        });
    }
    if policy.validate.is_some() && gates.validated != Some(true) {
        return Err(WorkError::QueueValidationFailed {
            queue: policy.name.clone(),
            reason: match gates.validated {
                Some(false) => "validator exited non-zero".to_owned(),
                _ => "validator was not run".to_owned(),
            },
        });
    }

    let mut landed_changes = Vec::new();
    for change_id in &submission.changes {
        let change = change_graph
            .change(change_id)
            .ok_or_else(|| WorkError::UnknownChange(change_id.to_string()))?;

        if change.phase == Phase::Draft {
            change_graph
                .set_phase(change_id, Phase::Public)
                .map_err(WorkError::Identity)?;
        }
        landed_changes.push(change_id.clone());

        let ref_name = queue_ref(&policy.name, change_id);
        let old_ref = op_log.refs_now().get(&ref_name).cloned();
        let tip = change_graph.tip(change_id).cloned();
        let note = if policy.name == "trunk" {
            format!("land change {change_id}")
        } else {
            format!("land change {change_id} (queue {})", policy.name)
        };
        op_log.append(
            OpKind::SetRef {
                name: ref_name,
                old: old_ref,
                new: tip,
            },
            Some(note),
        );
    }

    let carried_note = if gates.carried_values > 0 {
        format!(", carrying {} conflict value(s)", gates.carried_values)
    } else {
        String::new()
    };
    Ok(LandResult {
        submission_id: submission.id.clone(),
        landed_changes,
        message: format!(
            "Submission {} landed in queue {}{carried_note}",
            submission.id, policy.name
        ),
    })
}

/// The public ref a queue landing writes for a change: `public/<change>` for
/// `trunk` (legacy shape), `public/<queue>/<change>` for every other queue.
#[must_use]
pub fn queue_ref(queue: &str, change: &ChangeId) -> String {
    if queue == "trunk" {
        format!("public/{}", change.as_str())
    } else {
        format!("public/{queue}/{}", change.as_str())
    }
}

/// The observed facts for a **batch** landing (§5.10 Tier-0 batching):
/// per-submission content manifests plus the batch-wide gate observations.
#[derive(Debug, Clone, Default)]
pub struct BatchGates {
    /// Per submission: its materialized content as `path -> content hash`.
    /// Pairwise-disjointness is judged from these — the Tier-0 support check
    /// at file granularity.
    pub manifests: Vec<(SubmissionId, std::collections::BTreeMap<String, String>)>,
    /// Conflict values carried across the whole batch's content.
    pub carried_values: usize,
    /// The P9 validator verdict for the batch validated **as one**.
    pub validated: Option<bool>,
}

/// Land several submissions through a queue **as one batch** (§5.10):
/// pairwise-disjoint submissions validate as one and land together; an
/// overlapping pair refuses the whole batch with the colliding paths named.
///
/// Disjointness is the file-granularity Tier-0 check: two submissions overlap
/// iff some path appears in both manifests **with different content**
/// (identical bytes at the same path trivially commute). Disjointness is what
/// licenses order-independence (I3′), which is why the landing order within
/// the batch carries no meaning.
///
/// All gates are applied before any landing; a refused batch mutates nothing.
///
/// # Errors
///
/// The per-submission and policy errors of [`land_submission_in_queue`], plus
/// [`WorkError::BatchOverlap`] for the first overlapping pair found.
pub fn land_batch_in_queue(
    submissions: &[&Submission],
    policy: &QueuePolicy,
    gates: &BatchGates,
    change_graph: &mut ChangeGraph,
    op_log: &mut OpLog,
) -> Result<Vec<LandResult>, WorkError> {
    // Approval gate for every submission first (cheapest, clearest error).
    if policy.require_approval {
        for sub in submissions {
            if !sub.is_approved() {
                return Err(WorkError::SubmissionNotApproved(sub.id.to_string()));
            }
        }
    }

    // Tier-0 pairwise disjointness over the manifests.
    for (i, (id_a, man_a)) in gates.manifests.iter().enumerate() {
        for (id_b, man_b) in gates.manifests.iter().skip(i + 1) {
            let colliding: Vec<String> = man_a
                .iter()
                .filter(|(path, hash)| man_b.get(*path).is_some_and(|h| h != *hash))
                .map(|(path, _)| path.clone())
                .collect();
            if !colliding.is_empty() {
                return Err(WorkError::BatchOverlap {
                    a: id_a.to_string(),
                    b: id_b.to_string(),
                    paths: colliding,
                });
            }
        }
    }

    let single_gates = QueueGates {
        carried_values: gates.carried_values,
        validated: gates.validated,
    };
    // Check the remaining gates against the first submission WITHOUT landing,
    // so a carried/validation refusal cannot leave a half-landed batch.
    if gates.carried_values > 0 && !policy.allow_carried {
        return Err(WorkError::QueueCarriedRefused {
            queue: policy.name.clone(),
            count: gates.carried_values,
        });
    }
    if policy.validate.is_some() && gates.validated != Some(true) {
        return Err(WorkError::QueueValidationFailed {
            queue: policy.name.clone(),
            reason: match gates.validated {
                Some(false) => "validator exited non-zero".to_owned(),
                _ => "validator was not run".to_owned(),
            },
        });
    }

    let mut results = Vec::with_capacity(submissions.len());
    for sub in submissions {
        results.push(land_submission_in_queue(
            sub,
            policy,
            &single_gates,
            change_graph,
            op_log,
        )?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use omoplata_identity::{Change, ChangeId, CommitId, SubmissionId};

    use super::*;

    #[test]
    fn land_unapproved_submission_fails() {
        let sub = Submission::new(
            SubmissionId::new("sub-1"),
            "Feature",
            vec![ChangeId::new("c1")],
            "agent-1",
        );
        let mut cg = ChangeGraph::new();
        cg.add_change(Change::new(
            ChangeId::new("c1"),
            vec![CommitId::new("commit-1")],
            Phase::Draft,
        ));
        let mut log = OpLog::new();

        let err = land_submission(&sub, &mut cg, &mut log).unwrap_err();
        assert!(matches!(err, WorkError::SubmissionNotApproved(_)));
    }

    fn approved_sub(id: &str, change: &str) -> Submission {
        let mut sub = Submission::new(
            SubmissionId::new(id),
            "Feature",
            vec![ChangeId::new(change)],
            "agent-1",
        );
        sub.approve("reviewer-1");
        sub
    }

    fn graph_with(change: &str) -> ChangeGraph {
        let mut cg = ChangeGraph::new();
        cg.add_change(Change::new(
            ChangeId::new(change),
            vec![CommitId::new("commit-1")],
            Phase::Draft,
        ));
        cg
    }

    #[test]
    fn registry_resolves_implicit_trunk_and_rejects_unknown() {
        let reg = QueueRegistry::new();
        let trunk = reg.resolve("trunk").unwrap();
        assert!(trunk.allow_carried && trunk.require_approval);
        assert!(matches!(
            reg.resolve("release-1.2").unwrap_err(),
            WorkError::UnknownQueue(_)
        ));
    }

    #[test]
    fn registry_rejects_duplicate_names() {
        let mut reg = QueueRegistry::new();
        reg.add(QueuePolicy::trunk()).unwrap();
        assert!(matches!(
            reg.add(QueuePolicy::trunk()).unwrap_err(),
            WorkError::QueueExists(_)
        ));
    }

    #[test]
    fn strict_queue_refuses_carried_values_permissive_lands_them() {
        let strict = QueuePolicy {
            name: "release-1.2".to_owned(),
            description: None,
            validate: None,
            require_approval: true,
            allow_carried: false,
        };
        let gates = QueueGates {
            carried_values: 2,
            validated: None,
        };
        let sub = approved_sub("sub-1", "c1");

        let mut cg = graph_with("c1");
        let mut log = OpLog::new();
        assert!(matches!(
            land_submission_in_queue(&sub, &strict, &gates, &mut cg, &mut log).unwrap_err(),
            WorkError::QueueCarriedRefused { count: 2, .. }
        ));
        // Refusal mutates nothing.
        assert_eq!(cg.change(&ChangeId::new("c1")).unwrap().phase, Phase::Draft);

        let mut cg = graph_with("c1");
        let res = land_submission_in_queue(&sub, &QueuePolicy::trunk(), &gates, &mut cg, &mut log)
            .unwrap();
        assert!(res.message.contains("carrying 2 conflict value(s)"));
        assert_eq!(
            cg.change(&ChangeId::new("c1")).unwrap().phase,
            Phase::Public
        );
    }

    #[test]
    fn validation_gate_demands_a_pass() {
        let queue = QueuePolicy {
            name: "release-1.2".to_owned(),
            description: None,
            validate: Some("cargo test".to_owned()),
            require_approval: true,
            allow_carried: false,
        };
        let sub = approved_sub("sub-1", "c1");
        let mut log = OpLog::new();

        for (verdict, ok) in [(None, false), (Some(false), false), (Some(true), true)] {
            let mut cg = graph_with("c1");
            let gates = QueueGates {
                carried_values: 0,
                validated: verdict,
            };
            let res = land_submission_in_queue(&sub, &queue, &gates, &mut cg, &mut log);
            assert_eq!(res.is_ok(), ok, "verdict {verdict:?}");
        }
    }

    #[test]
    fn queue_landing_writes_per_queue_refs_trunk_keeps_legacy_shape() {
        let c1 = ChangeId::new("c1");
        assert_eq!(queue_ref("trunk", &c1), "public/c1");
        assert_eq!(queue_ref("release-1.2", &c1), "public/release-1.2/c1");

        let queue = QueuePolicy {
            name: "release-1.2".to_owned(),
            description: None,
            validate: None,
            require_approval: true,
            allow_carried: false,
        };
        let sub = approved_sub("sub-1", "c1");
        let mut cg = graph_with("c1");
        let mut log = OpLog::new();
        land_submission_in_queue(&sub, &queue, &QueueGates::default(), &mut cg, &mut log).unwrap();
        assert!(log.refs_now().contains_key("public/release-1.2/c1"));
    }

    #[test]
    fn approval_waiver_lands_pending_submission() {
        let queue = QueuePolicy {
            name: "sandbox".to_owned(),
            description: None,
            validate: None,
            require_approval: false,
            allow_carried: true,
        };
        let sub = Submission::new(
            SubmissionId::new("sub-1"),
            "Experiment",
            vec![ChangeId::new("c1")],
            "agent-1",
        );
        let mut cg = graph_with("c1");
        let mut log = OpLog::new();
        assert!(
            land_submission_in_queue(&sub, &queue, &QueueGates::default(), &mut cg, &mut log)
                .is_ok()
        );
    }

    fn manifest(
        id: &str,
        entries: &[(&str, &str)],
    ) -> (SubmissionId, std::collections::BTreeMap<String, String>) {
        (
            SubmissionId::new(id),
            entries
                .iter()
                .map(|(p, h)| ((*p).to_owned(), (*h).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn disjoint_batch_lands_together_overlap_refuses_whole_batch() {
        let sub_a = approved_sub("sub-a", "ca");
        let sub_b = approved_sub("sub-b", "cb");
        let mut cg = graph_with("ca");
        cg.add_change(Change::new(
            ChangeId::new("cb"),
            vec![CommitId::new("commit-b")],
            Phase::Draft,
        ));
        let mut log = OpLog::new();

        // Disjoint paths (and one identical shared path): batches.
        let gates = BatchGates {
            manifests: vec![
                manifest("sub-a", &[("src/a.rs", "h1"), ("Cargo.toml", "same")]),
                manifest("sub-b", &[("src/b.rs", "h2"), ("Cargo.toml", "same")]),
            ],
            carried_values: 0,
            validated: None,
        };
        let results = land_batch_in_queue(
            &[&sub_a, &sub_b],
            &QueuePolicy::trunk(),
            &gates,
            &mut cg,
            &mut log,
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            cg.change(&ChangeId::new("ca")).unwrap().phase,
            Phase::Public
        );
        assert_eq!(
            cg.change(&ChangeId::new("cb")).unwrap().phase,
            Phase::Public
        );

        // Same path, different content: the whole batch refuses, nothing lands.
        let mut cg = graph_with("ca");
        cg.add_change(Change::draft(ChangeId::new("cb")));
        let overlapping = BatchGates {
            manifests: vec![
                manifest("sub-a", &[("src/lib.rs", "h1")]),
                manifest("sub-b", &[("src/lib.rs", "h2")]),
            ],
            carried_values: 0,
            validated: None,
        };
        let err = land_batch_in_queue(
            &[&sub_a, &sub_b],
            &QueuePolicy::trunk(),
            &overlapping,
            &mut cg,
            &mut log,
        )
        .unwrap_err();
        assert!(
            matches!(err, WorkError::BatchOverlap { ref paths, .. } if paths == &vec!["src/lib.rs".to_owned()])
        );
        assert_eq!(cg.change(&ChangeId::new("ca")).unwrap().phase, Phase::Draft);
    }

    #[test]
    fn batch_gates_apply_before_any_landing() {
        // A failing validator refuses the batch with every change still Draft.
        let sub_a = approved_sub("sub-a", "ca");
        let mut cg = graph_with("ca");
        let mut log = OpLog::new();
        let queue = QueuePolicy {
            name: "release-1.2".to_owned(),
            description: None,
            validate: Some("suite".to_owned()),
            require_approval: true,
            allow_carried: false,
        };
        let gates = BatchGates {
            manifests: vec![manifest("sub-a", &[("src/a.rs", "h1")])],
            carried_values: 0,
            validated: Some(false),
        };
        let err = land_batch_in_queue(&[&sub_a], &queue, &gates, &mut cg, &mut log).unwrap_err();
        assert!(matches!(err, WorkError::QueueValidationFailed { .. }));
        assert_eq!(cg.change(&ChangeId::new("ca")).unwrap().phase, Phase::Draft);
    }

    #[test]
    fn land_approved_submission_advances_phase_to_public() {
        let mut sub = Submission::new(
            SubmissionId::new("sub-1"),
            "Feature",
            vec![ChangeId::new("c1")],
            "agent-1",
        );
        sub.approve("reviewer-1");

        let mut cg = ChangeGraph::new();
        cg.add_change(Change::new(
            ChangeId::new("c1"),
            vec![CommitId::new("commit-1")],
            Phase::Draft,
        ));
        let mut log = OpLog::new();

        let res = land_submission(&sub, &mut cg, &mut log).unwrap();
        assert_eq!(res.submission_id, SubmissionId::new("sub-1"));
        assert_eq!(
            cg.change(&ChangeId::new("c1")).unwrap().phase,
            Phase::Public
        );
    }
}
