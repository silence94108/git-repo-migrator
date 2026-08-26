use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryMapping {
    pub source_url: String,
    pub target_url: String,
    pub source_name: String,
    pub target_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleSelection {
    pub lfs: bool,
    pub metadata: bool,
    pub issues: bool,
    pub pull_requests: bool,
    pub wiki: bool,
    pub releases: bool,
}

impl Default for ModuleSelection {
    fn default() -> Self {
        Self {
            lfs: true,
            metadata: true,
            issues: false,
            pull_requests: false,
            wiki: false,
            releases: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictPolicy {
    pub reuse_empty: bool,
    pub skip_non_empty: bool,
    pub auto_rename: bool,
    pub allow_overwrite: bool,
}

impl Default for ConflictPolicy {
    fn default() -> Self {
        Self {
            reuse_empty: true,
            skip_non_empty: true,
            auto_rename: true,
            allow_overwrite: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub mappings: Vec<RepositoryMapping>,
    pub modules: ModuleSelection,
    pub conflict_policy: ConflictPolicy,
    pub plan_hash: String,
}

impl MigrationPlan {
    pub fn freeze(
        mut mappings: Vec<RepositoryMapping>,
        modules: ModuleSelection,
        conflict_policy: ConflictPolicy,
    ) -> Result<Self, String> {
        mappings.sort_by(|a, b| a.source_url.cmp(&b.source_url));
        let mut seen = std::collections::BTreeSet::new();
        for mapping in &mappings {
            let source = Url::parse(&mapping.source_url).map_err(|_| "源 URL 无效")?;
            let target = Url::parse(&mapping.target_url).map_err(|_| "目标 URL 无效")?;
            if source.scheme() != "https" && source.scheme() != "ssh" && source.scheme() != "http" {
                return Err("源 URL 必须使用 HTTP(S) 或 SSH".into());
            }
            if !seen.insert(mapping.target_url.clone()) {
                return Err("目标 URL 在计划中重复".into());
            }
            if target.scheme() != "https" && target.scheme() != "ssh" && target.scheme() != "http" {
                return Err("目标 URL 必须使用 HTTP(S) 或 SSH".into());
            }
        }
        let canonical = serde_json::to_vec(&(&mappings, &modules, &conflict_policy))
            .map_err(|_| "计划序列化失败")?;
        let hash = format!("{:x}", Sha256::digest(canonical));
        Ok(Self {
            mappings,
            modules,
            conflict_policy,
            plan_hash: hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_hash_is_stable_after_sorting() {
        let a = RepositoryMapping {
            source_url: "https://a/src".into(),
            target_url: "https://b/a".into(),
            source_name: "a".into(),
            target_name: "a".into(),
        };
        let b = RepositoryMapping {
            source_url: "https://a/src2".into(),
            target_url: "https://b/b".into(),
            source_name: "b".into(),
            target_name: "b".into(),
        };
        let first = MigrationPlan::freeze(
            vec![b.clone(), a.clone()],
            ModuleSelection::default(),
            ConflictPolicy::default(),
        )
        .unwrap();
        let second = MigrationPlan::freeze(
            vec![a, b],
            ModuleSelection::default(),
            ConflictPolicy::default(),
        )
        .unwrap();
        assert_eq!(first.plan_hash, second.plan_hash);
    }

    #[test]
    fn overwrite_is_disabled_by_default() {
        assert!(!ConflictPolicy::default().allow_overwrite);
    }
}
