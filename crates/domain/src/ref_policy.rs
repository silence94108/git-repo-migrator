use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefClassification {
    Branch,
    Tag,
    Head,
    PlatformPrivate,
    RemoteTracking,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPolicyDecision {
    Allow,
    Archive,
    Ignore,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefPolicy {
    pub include_archived_refs: bool,
}

impl RefPolicy {
    pub fn classify(name: &str) -> RefClassification {
        if name == "HEAD" {
            return RefClassification::Head;
        }
        if name.starts_with("refs/heads/") {
            return RefClassification::Branch;
        }
        if name.starts_with("refs/tags/") {
            return RefClassification::Tag;
        }
        if name.starts_with("refs/pull/")
            || name.starts_with("refs/merge-requests/")
            || name.starts_with("refs/changes/")
            || name.starts_with("refs/keep-around/")
        {
            return RefClassification::PlatformPrivate;
        }
        if name.starts_with("refs/remotes/") {
            return RefClassification::RemoteTracking;
        }
        RefClassification::Unknown
    }

    pub fn decide(&self, name: &str) -> RefPolicyDecision {
        match Self::classify(name) {
            RefClassification::Branch | RefClassification::Tag | RefClassification::Head => {
                RefPolicyDecision::Allow
            }
            RefClassification::PlatformPrivate | RefClassification::RemoteTracking => {
                if self.include_archived_refs {
                    RefPolicyDecision::Archive
                } else {
                    RefPolicyDecision::Ignore
                }
            }
            RefClassification::Unknown => RefPolicyDecision::Ignore,
        }
    }

    pub fn allowed_refspecs(&self) -> Vec<String> {
        vec![
            "+refs/heads/*:refs/heads/*".into(),
            "+refs/tags/*:refs/tags/*".into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_refs_are_never_push_allowlisted() {
        let policy = RefPolicy::default();
        for name in [
            "refs/pull/1/head",
            "refs/merge-requests/2/head",
            "refs/changes/34/1",
            "refs/remotes/origin/main",
            "refs/notes/review",
        ] {
            assert_ne!(policy.decide(name), RefPolicyDecision::Allow);
        }
        assert!(policy
            .allowed_refspecs()
            .iter()
            .all(|r| !r.contains("mirror")));
    }

    #[test]
    fn branches_and_tags_are_allowlisted() {
        let policy = RefPolicy::default();
        assert_eq!(policy.decide("refs/heads/main"), RefPolicyDecision::Allow);
        assert_eq!(policy.decide("refs/tags/v1.0.0"), RefPolicyDecision::Allow);
    }
}
