use git_repo_migrator_domain::Fidelity;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateStatus {
    Succeeded,
    Partial,
    Failed,
    RetryableFailed,
    Skipped,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VerificationEvidence {
    pub refs_checked: u32,
    pub refs_missing: u32,
    pub lfs_checked: u32,
    pub lfs_missing: u32,
    pub metadata_checked: bool,
    pub excluded_refs: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    pub status: AggregateStatus,
    pub git_ok: bool,
    pub lfs_ok: bool,
    pub metadata_ok: bool,
    pub fidelity: Vec<Fidelity>,
    pub evidence: VerificationEvidence,
}
impl VerificationResult {
    pub fn aggregate(
        git_ok: bool,
        lfs_ok: bool,
        metadata_ok: bool,
        evidence: VerificationEvidence,
        fidelity: Vec<Fidelity>,
    ) -> Self {
        let status = if !git_ok || !lfs_ok {
            AggregateStatus::Failed
        } else if !metadata_ok
            || fidelity
                .iter()
                .any(|f| matches!(f, Fidelity::ReadOnlyArchive | Fidelity::Unsupported))
        {
            AggregateStatus::Partial
        } else {
            AggregateStatus::Succeeded
        };
        Self {
            status,
            git_ok,
            lfs_ok,
            metadata_ok,
            fidelity,
            evidence,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn git_success_platform_failure_is_partial() {
        let r = VerificationResult::aggregate(
            true,
            true,
            false,
            VerificationEvidence::default(),
            vec![Fidelity::NativeRebuild],
        );
        assert_eq!(r.status, AggregateStatus::Partial);
    }
    #[test]
    fn missing_lfs_fails() {
        let r = VerificationResult::aggregate(
            true,
            false,
            true,
            VerificationEvidence::default(),
            vec![],
        );
        assert_eq!(r.status, AggregateStatus::Failed);
    }
}
