use crate::verification::AggregateStatus;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Csv,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportRow {
    pub task_id: String,
    pub source_url: String,
    pub target_url: String,
    pub status: AggregateStatus,
    pub error_code: Option<String>,
    pub excluded_refs: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Report {
    pub rows: Vec<ReportRow>,
}
impl Report {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
    pub fn to_csv(&self) -> String {
        let mut out = "task_id,source_url,target_url,status,error_code,excluded_refs\n".to_owned();
        for r in &self.rows {
            out.push_str(&format!(
                "{},{},{},{:?},{},\"{}\"\n",
                safe(&r.task_id),
                safe(&r.source_url),
                safe(&r.target_url),
                r.status,
                r.error_code.as_deref().map(safe).unwrap_or_default(),
                r.excluded_refs
                    .iter()
                    .map(|s| safe(s))
                    .collect::<Vec<_>>()
                    .join(";")
            ));
        }
        out
    }
    pub fn export(&self, format: ExportFormat) -> Result<String, serde_json::Error> {
        match format {
            ExportFormat::Json => self.to_json(),
            ExportFormat::Csv => Ok(self.to_csv()),
        }
    }
}
fn safe(value: &str) -> String {
    let mut s = value.replace(['\r', '\n', ','], " ");
    for secret in ["token=", "access_token=", "Authorization:", "Cookie:"] {
        if let Some(i) = s.to_ascii_lowercase().find(&secret.to_ascii_lowercase()) {
            s.truncate(i);
            s.push_str("[已脱敏]");
        }
    }
    s
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_redacts_token() {
        let r = Report {
            rows: vec![ReportRow {
                task_id: "1".into(),
                source_url: "https://x/r?token=abc".into(),
                target_url: "https://y/r".into(),
                status: AggregateStatus::Succeeded,
                error_code: None,
                excluded_refs: vec![],
            }],
        };
        assert!(!r.to_csv().contains("abc"));
    }
}
