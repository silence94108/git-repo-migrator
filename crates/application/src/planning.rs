use git_repo_migrator_domain::{ConflictPolicy, MigrationPlan, ModuleSelection, RepositoryMapping};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub source_url: String,
    pub name: String,
    pub namespace: String,
    pub target_url: Option<String>,
    pub target_name: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    Unknown,
    Missing,
    Empty,
    NonEmpty,
    Inaccessible,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSet {
    pub matching_ids: BTreeSet<String>,
    pub excluded_ids: BTreeSet<String>,
}
impl SelectionSet {
    pub fn select_all<I: IntoIterator<Item = String>>(ids: I) -> Self {
        Self {
            matching_ids: ids.into_iter().collect(),
            excluded_ids: BTreeSet::new(),
        }
    }
    pub fn exclude(&mut self, id: impl Into<String>) {
        self.excluded_ids.insert(id.into());
    }
    pub fn selected(&self) -> impl Iterator<Item = &String> {
        self.matching_ids
            .iter()
            .filter(|id| !self.excluded_ids.contains(*id))
    }
    pub fn len(&self) -> usize {
        self.selected().count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPreview {
    pub mappings: Vec<RepositoryMapping>,
    pub actions: Vec<String>,
    pub blocking: Vec<String>,
    pub warnings: Vec<String>,
    pub capability_snapshot_hash: String,
    pub requires_confirmation: bool,
}
impl PlanPreview {
    pub fn can_freeze(&self, dangerous_confirmation: bool) -> bool {
        self.blocking.is_empty() && (!self.requires_confirmation || dangerous_confirmation)
    }
    pub fn freeze(
        &self,
        modules: ModuleSelection,
        policy: ConflictPolicy,
        confirmed: bool,
    ) -> Result<MigrationPlan, String> {
        if !self.can_freeze(confirmed) {
            return Err("预检存在阻断项或缺少二次确认".into());
        }
        MigrationPlan::freeze(self.mappings.clone(), modules, policy).map_err(|e| e.to_string())
    }
}
pub fn build_preview(
    selection: &SelectionSet,
    candidates: &[Candidate],
    targets: &HashMap<String, TargetState>,
    policy: ConflictPolicy,
    capability_snapshot_hash: impl Into<String>,
) -> PlanPreview {
    let by_id: HashMap<_, _> = candidates.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut mappings = Vec::new();
    let mut actions = Vec::new();
    let mut blocking = Vec::new();
    let mut warnings = Vec::new();
    let mut target_seen = BTreeSet::new();
    let mut requires_confirmation = false;
    for id in selection.selected() {
        let Some(c) = by_id.get(id.as_str()) else {
            blocking.push(format!("候选仓库不存在: {id}"));
            continue;
        };
        let target_url = c.target_url.clone().unwrap_or_default();
        if target_url.is_empty() {
            blocking.push(format!("目标未知: {}", c.source_url));
            continue;
        }
        if !target_seen.insert(target_url.clone()) {
            blocking.push(format!("目标 URL 重复: {target_url}"));
        }
        match targets.get(id).copied().unwrap_or(TargetState::Unknown) {
            TargetState::Unknown | TargetState::Inaccessible => {
                blocking.push(format!("无法确认目标状态: {target_url}"))
            }
            TargetState::Missing => actions.push(format!("创建目标: {target_url}")),
            TargetState::Empty => actions.push(format!("复用空目标: {target_url}")),
            TargetState::NonEmpty if policy.skip_non_empty && !policy.allow_overwrite => {
                actions.push(format!("跳过非空目标: {target_url}"));
                warnings.push(format!("目标非空，默认跳过: {target_url}"));
            }
            TargetState::NonEmpty if policy.allow_overwrite => {
                actions.push(format!("覆盖非空目标: {target_url}"));
                requires_confirmation = true;
            }
            TargetState::NonEmpty => blocking.push(format!("非空目标未配置处理策略: {target_url}")),
        }
        mappings.push(RepositoryMapping {
            source_url: c.source_url.clone(),
            target_url,
            source_name: c.name.clone(),
            target_name: c.target_name.clone().unwrap_or_else(|| c.name.clone()),
        });
    }
    if mappings.len() > 100 {
        warnings.push(format!("批次包含 {} 个仓库，将分批执行", mappings.len()));
    }
    let cap = capability_snapshot_hash.into();
    let hash = format!("{:x}", Sha256::digest(cap.as_bytes()));
    PlanPreview {
        mappings,
        actions,
        blocking,
        warnings,
        capability_snapshot_hash: hash,
        requires_confirmation,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_all_then_excludes_across_pages() {
        let mut s = SelectionSet::select_all((0..100).map(|i| i.to_string()));
        s.exclude("42");
        assert_eq!(s.len(), 99);
    }
    #[test]
    fn unknown_target_blocks_preview() {
        let c = Candidate {
            id: "1".into(),
            source_url: "https://s/r".into(),
            name: "r".into(),
            namespace: "n".into(),
            target_url: None,
            target_name: None,
        };
        let p = build_preview(
            &SelectionSet::select_all(vec!["1".into()]),
            &[c],
            &HashMap::new(),
            ConflictPolicy::default(),
            "x",
        );
        assert!(!p.blocking.is_empty());
    }
}
