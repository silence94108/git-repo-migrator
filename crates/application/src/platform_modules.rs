use crate::archive::{ArchiveDocument, ArchiveItem};
use git_repo_migrator_platform_core::{Fidelity, PlatformModule};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformItem {
    pub source_id: String,
    pub source_url: String,
    pub title: String,
    pub body: String,
    pub source_author: String,
    pub mapped_target_author: Option<String>,
    pub source_state: String,
    pub mapped_target_state: Option<String>,
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemFailure {
    pub source_id: String,
    pub code: String,
    pub retryable: bool,
    pub safe_message: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityMapping {
    pub source_author: String,
    pub target_author: Option<String>,
    pub attribution: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachmentMapping {
    pub source_url: String,
    pub target_url: Option<String>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleExecution {
    pub module: PlatformModule,
    pub fidelity: Fidelity,
    pub discovered: u64,
    pub migrated: u64,
    pub archived: u64,
    pub failed: u64,
    pub identity_mapping: Vec<IdentityMapping>,
    pub state_mapping: BTreeMap<String, Option<String>>,
    pub attachment_mapping: Vec<AttachmentMapping>,
    pub source_links: Vec<String>,
    pub item_mappings: BTreeMap<String, String>,
    pub item_failures: Vec<ItemFailure>,
    pub archive: Option<ArchiveDocument>,
}

pub fn execute_module<F>(
    batch_id: &str,
    task_id: &str,
    repository: &str,
    module: PlatformModule,
    fidelity: Fidelity,
    items: &[PlatformItem],
    mut migrate: F,
) -> ModuleExecution
where
    F: FnMut(&PlatformItem) -> Result<String, ItemFailure>,
{
    let mut result = ModuleExecution {
        module,
        fidelity,
        discovered: items.len() as u64,
        migrated: 0,
        archived: 0,
        failed: 0,
        identity_mapping: vec![],
        state_mapping: BTreeMap::new(),
        attachment_mapping: vec![],
        source_links: items.iter().map(|item| item.source_url.clone()).collect(),
        item_mappings: BTreeMap::new(),
        item_failures: vec![],
        archive: None,
    };
    for item in items {
        result.identity_mapping.push(IdentityMapping {
            source_author: item.source_author.clone(),
            target_author: item.mapped_target_author.clone(),
            attribution: item
                .mapped_target_author
                .as_ref()
                .map(|_| "mapped_account")
                .unwrap_or("source_attribution_only")
                .into(),
        });
        result
            .state_mapping
            .insert(item.source_state.clone(), item.mapped_target_state.clone());
        result
            .attachment_mapping
            .extend(item.attachments.iter().map(|source_url| AttachmentMapping {
                source_url: source_url.clone(),
                target_url: None,
                failure_reason: Some("附件尚未迁移，保留源链接".into()),
            }));
    }
    match fidelity {
        Fidelity::NativeRebuild => {
            for item in items {
                match migrate(item) {
                    Ok(target_id) => {
                        result.migrated += 1;
                        result
                            .item_mappings
                            .insert(item.source_id.clone(), target_id);
                    }
                    Err(failure) => {
                        result.failed += 1;
                        result.item_failures.push(failure);
                    }
                }
            }
        }
        Fidelity::ReadOnlyArchive => {
            result.archived = items.len() as u64;
            result.archive = Some(ArchiveDocument::new(
                batch_id,
                task_id,
                repository,
                module,
                items.iter().map(to_archive).collect(),
            ));
        }
        Fidelity::Unsupported => {}
    }
    result
}

pub fn retry_failed_items<F>(
    execution: &mut ModuleExecution,
    items: &[PlatformItem],
    mut migrate: F,
) where
    F: FnMut(&PlatformItem) -> Result<String, ItemFailure>,
{
    if execution.fidelity != Fidelity::NativeRebuild {
        return;
    }
    let retryable: BTreeSet<_> = execution
        .item_failures
        .iter()
        .filter(|failure| failure.retryable)
        .map(|failure| failure.source_id.clone())
        .collect();
    if retryable.is_empty() {
        return;
    }
    execution
        .item_failures
        .retain(|failure| !retryable.contains(&failure.source_id));
    execution.failed = execution.item_failures.len() as u64;
    for item in items
        .iter()
        .filter(|item| retryable.contains(&item.source_id))
    {
        match migrate(item) {
            Ok(target_id) => {
                execution.migrated += 1;
                execution
                    .item_mappings
                    .insert(item.source_id.clone(), target_id);
            }
            Err(failure) => {
                execution.failed += 1;
                execution.item_failures.push(failure);
            }
        }
    }
}

fn to_archive(item: &PlatformItem) -> ArchiveItem {
    ArchiveItem {
        source_id: item.source_id.clone(),
        source_url: item.source_url.clone(),
        title: item.title.clone(),
        body: item.body.clone(),
        source_author: item.source_author.clone(),
        state: item.source_state.clone(),
        attachments: item
            .attachments
            .iter()
            .map(|url| crate::archive::ArchiveAttachment {
                name: url.rsplit('/').next().unwrap_or("attachment").into(),
                source_url: url.clone(),
                local_reference: None,
                failure_reason: None,
            })
            .collect(),
        metadata: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: &str) -> PlatformItem {
        PlatformItem {
            source_id: id.into(),
            source_url: format!("https://source/{id}"),
            title: "t".into(),
            body: "b".into(),
            source_author: "alice".into(),
            mapped_target_author: None,
            source_state: "open".into(),
            mapped_target_state: Some("open".into()),
            attachments: vec![],
        }
    }
    #[test]
    fn missing_identity_is_attribution_not_fake_account() {
        let result = execute_module(
            "b",
            "t",
            "r",
            PlatformModule::Issues,
            Fidelity::NativeRebuild,
            &[item("1")],
            |_| Ok("target-1".into()),
        );
        assert_eq!(result.identity_mapping[0].target_author, None);
        assert_eq!(
            result.identity_mapping[0].attribution,
            "source_attribution_only"
        );
    }
    #[test]
    fn retry_only_processes_retryable_failures() {
        let items = [item("1"), item("2")];
        let mut result = execute_module(
            "b",
            "t",
            "r",
            PlatformModule::Issues,
            Fidelity::NativeRebuild,
            &items,
            |i| {
                Err(ItemFailure {
                    source_id: i.source_id.clone(),
                    code: "network".into(),
                    retryable: i.source_id == "1",
                    safe_message: "failed".into(),
                    action: "retry".into(),
                })
            },
        );
        retry_failed_items(&mut result, &items, |_| Ok("target".into()));
        assert!(result.item_mappings.contains_key("1"));
        assert!(!result.item_mappings.contains_key("2"));
        assert_eq!(result.failed, 1);
    }
}
