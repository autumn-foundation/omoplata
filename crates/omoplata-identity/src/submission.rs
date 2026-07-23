//! Submissions and approval certificates — design doc §5.10.
//!
//! A **submission** is the review unit in Omoplata: a typed object referencing
//! an evaluated revset of [`ChangeId`](crate::ChangeId)s (not commit hashes).
//! As changes are rebased or amended, supersession edges update their tip
//! commits without altering the submission's identity.
//!
//! **Approval carry-forward with commutation certificates:** when a reviewed
//! change is rebased, Tier-0 (disjoint support) or Tier-1 (checked commutation)
//! proves whether the rebase intersected the change's support. If support is
//! disjoint or commutation holds, the approval persists with an
//! [`ApprovalCertificate`] attached (§5.10).

use serde::{Deserialize, Serialize};

use crate::{ChangeId, CommitId};

/// A unique identifier for a review submission (§5.10).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubmissionId(String);

impl SubmissionId {
    /// Construct a submission id from any string-like value.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the underlying identifier string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for SubmissionId {
    type Err = std::convert::Infallible;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        Ok(Self(id.to_owned()))
    }
}

impl std::fmt::Display for SubmissionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A certificate verifying that a change's approval persists across a rebase
/// (§5.10).
///
/// Attests that `rebased_commit` commutes with or has disjoint support from
/// `original_commit`, preserving review validity without requiring re-review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalCertificate {
    /// The change id whose approval is carried forward.
    pub change_id: ChangeId,
    /// The original commit id that received human or arbiter approval.
    pub original_commit: CommitId,
    /// The rebased commit id.
    pub rebased_commit: CommitId,
    /// The reviewer who granted the original approval.
    pub approved_by: String,
    /// Verification status/witness tag (e.g. `"Tier-0: disjoint support"` or
    /// `"Tier-1: checked commutation"`).
    pub proof_witness: String,
}

/// Review approval status of a submission (§5.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Approval {
    /// Pending review.
    Pending,
    /// Approved with optional carry-forward certificates across rebases.
    Approved {
        /// Reviewer identifier.
        reviewer: String,
        /// Carry-forward certificates for rebased changes in the submission.
        certificates: Vec<ApprovalCertificate>,
    },
    /// Rejected during review.
    Rejected {
        /// Reason for rejection.
        reason: String,
    },
}

/// A review submission referencing a revset of change IDs (§5.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    /// Unique submission identifier.
    pub id: SubmissionId,
    /// Title / summary of the submission.
    pub title: String,
    /// Ordered list of draft change IDs contained in this submission.
    pub changes: Vec<ChangeId>,
    /// Author agent or engineer identifier.
    pub author: String,
    /// Current approval status.
    pub approval: Approval,
}

impl Submission {
    /// Create a new pending submission.
    #[must_use]
    pub fn new(
        id: SubmissionId,
        title: impl Into<String>,
        changes: Vec<ChangeId>,
        author: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            changes,
            author: author.into(),
            approval: Approval::Pending,
        }
    }

    /// Approve this submission.
    pub fn approve(&mut self, reviewer: impl Into<String>) {
        self.approval = Approval::Approved {
            reviewer: reviewer.into(),
            certificates: Vec::new(),
        };
    }

    /// Attach an approval carry-forward certificate across a rebase (§5.10).
    pub fn add_certificate(&mut self, cert: ApprovalCertificate) {
        if let Approval::Approved {
            ref mut certificates,
            ..
        } = self.approval
        {
            certificates.push(cert);
        }
    }

    /// Returns `true` if this submission is approved.
    #[must_use]
    pub fn is_approved(&self) -> bool {
        matches!(self.approval, Approval::Approved { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_id_round_trip() {
        let id = SubmissionId::new("sub-123");
        assert_eq!(id.as_str(), "sub-123");
        assert_eq!(id.to_string(), "sub-123");
        assert_eq!("sub-123".parse::<SubmissionId>().unwrap(), id);
    }

    #[test]
    fn submission_approval_flow() {
        let mut sub = Submission::new(
            SubmissionId::new("sub-1"),
            "Auth refactor",
            vec![ChangeId::new("c1"), ChangeId::new("c2")],
            "agent-alpha",
        );
        assert!(!sub.is_approved());
        assert_eq!(sub.approval, Approval::Pending);

        sub.approve("reviewer-beta");
        assert!(sub.is_approved());

        let cert = ApprovalCertificate {
            change_id: ChangeId::new("c1"),
            original_commit: CommitId::new("sha256:aaa"),
            rebased_commit: CommitId::new("sha256:bbb"),
            approved_by: "reviewer-beta".to_string(),
            proof_witness: "Tier-0: disjoint support".to_string(),
        };
        sub.add_certificate(cert.clone());

        if let Approval::Approved { certificates, .. } = &sub.approval {
            assert_eq!(certificates.len(), 1);
            assert_eq!(certificates[0], cert);
        } else {
            panic!("expected Approved status");
        }
    }
}
