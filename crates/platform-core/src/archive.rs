use crate::PlatformModule;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const ARCHIVE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveAttachment {
    pub name: String,
    pub source_url: String,
    pub local_reference: Option<String>,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveItem {
    pub source_id: String,
    pub source_url: String,
    pub title: String,
    pub body: String,
    pub source_author: String,
    pub state: String,
    pub attachments: Vec<ArchiveAttachment>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl ArchiveItem {
    pub fn sanitize(mut self) -> Self {
        self.title = redact(&self.title);
        self.body = redact(&self.body);
        self.source_author = redact(&self.source_author);
        self.metadata.retain(|key, value| {
            let lower = key.to_ascii_lowercase();
            let safe = ![
                "token",
                "password",
                "secret",
                "authorization",
                "cookie",
                "private_key",
            ]
            .iter()
            .any(|needle| lower.contains(needle));
            if safe {
                *value = redact(value);
            }
            safe
        });
        for attachment in &mut self.attachments {
            attachment.name = redact(&attachment.name);
            attachment.failure_reason = attachment.failure_reason.as_deref().map(redact);
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDocument {
    pub schema_version: u32,
    pub read_only: bool,
    pub batch_id: String,
    pub task_id: String,
    pub repository: String,
    pub module: PlatformModule,
    pub items: Vec<ArchiveItem>,
}

impl ArchiveDocument {
    pub fn new(
        batch_id: impl Into<String>,
        task_id: impl Into<String>,
        repository: impl Into<String>,
        module: PlatformModule,
        items: Vec<ArchiveItem>,
    ) -> Self {
        Self {
            schema_version: ARCHIVE_SCHEMA_VERSION,
            read_only: true,
            batch_id: safe_segment(batch_id.into()),
            task_id: safe_segment(task_id.into()),
            repository: repository.into(),
            module,
            items: items.into_iter().map(ArchiveItem::sanitize).collect(),
        }
    }

    pub fn retention_path(&self) -> PathBuf {
        self.retention_dir()
            .join(format!("{}.json", module_name(self.module)))
    }

    /// Directory holding every archive document of this task. Used by the
    /// executor as the report's archive path.
    pub fn retention_dir(&self) -> PathBuf {
        PathBuf::from("archives")
            .join(&self.batch_id)
            .join(&self.task_id)
    }

    /// Rebinds a document produced by a platform adapter to the task that will
    /// persist it: adapters have no batch or task ids of their own.
    pub fn rebind(
        mut self,
        batch_id: impl Into<String>,
        task_id: impl Into<String>,
        repository: impl Into<String>,
    ) -> Self {
        self.batch_id = safe_segment(batch_id.into());
        self.task_id = safe_segment(task_id.into());
        self.repository = repository.into();
        self
    }
}

fn safe_segment(value: String) -> String {
    let sanitized: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "unknown".into()
    } else {
        sanitized
    }
}

fn module_name(module: PlatformModule) -> &'static str {
    match module {
        PlatformModule::Metadata => "metadata",
        PlatformModule::Issues => "issues",
        PlatformModule::PullRequests => "pull_requests",
        PlatformModule::MergeRequests => "merge_requests",
        PlatformModule::Wiki => "wiki",
        PlatformModule::Releases => "releases",
        PlatformModule::ReleaseAssets => "release_assets",
    }
}

pub fn redact(value: &str) -> String {
    let mut output = value.replace(['\r', '\n'], " ");
    for marker in [
        "authorization:",
        "token=",
        "access_token=",
        "password=",
        "cookie:",
        "private_key=",
    ] {
        if let Some(index) = output.to_ascii_lowercase().find(marker) {
            output.truncate(index);
            output.push_str("[REDACTED]");
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_is_read_only_sanitized_and_has_fixed_path() {
        let item = ArchiveItem {
            source_id: "1".into(),
            source_url: "https://source/items/1".into(),
            title: "hello".into(),
            body: "token=abc".into(),
            source_author: "alice".into(),
            state: "open".into(),
            attachments: vec![],
            metadata: BTreeMap::from([("Authorization".into(), "Bearer abc".into())]),
        };
        let doc = ArchiveDocument::new(
            "batch/1",
            "task:1",
            "team/repo",
            PlatformModule::PullRequests,
            vec![item],
        );
        assert!(doc.read_only);
        assert!(!serde_json::to_string(&doc).unwrap().contains("abc"));
        assert_eq!(
            doc.retention_path(),
            PathBuf::from("archives/batch_1/task_1/pull_requests.json")
        );
    }
}
