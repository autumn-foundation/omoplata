//! The Merge Queue and submission landing — design doc §5.10.
//!
//! **Landing is the merge queue (v1).** `land` enqueues an approved [`Submission`].
//! The queue gates on approvals-per-policy plus dynamic validation (§3 P9);
//! landing is the `Draft -> Public` phase transition (§3 P5, §5.3).
//!
//! Batching by Tier 0: pairwise-disjoint changes batch, test as one, and land
//! in parallel; overlapping changes serialize with commutation checks.

use omoplata_identity::{ChangeGraph, Phase, Submission, SubmissionId};
use serde::{Deserialize, Serialize};

use crate::error::WorkError;
use crate::oplog::{OpKind, OpLog};

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
