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
    /// Both export formats go through the same redaction, so JSON can never be
    /// the format that leaks a token the CSV would have masked.
    fn redacted(&self) -> Self {
        Self {
            rows: self
                .rows
                .iter()
                .map(|r| ReportRow {
                    task_id: redact(&r.task_id),
                    source_url: redact(&r.source_url),
                    target_url: redact(&r.target_url),
                    status: r.status,
                    error_code: r.error_code.as_deref().map(redact),
                    excluded_refs: r.excluded_refs.iter().map(|s| redact(s)).collect(),
                })
                .collect(),
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.redacted())
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

/// Truncates a value at the first credential-bearing marker. Applied to every
/// exported field regardless of format.
fn redact(value: &str) -> String {
    let mut s = value.to_owned();
    for marker in [
        "token=",
        "access_token=",
        "private_token=",
        "password=",
        "Authorization:",
        "Cookie:",
    ] {
        if let Some(i) = s.to_ascii_lowercase().find(&marker.to_ascii_lowercase()) {
            s.truncate(i);
            s.push_str("[已脱敏]");
        }
    }
    s
}

/// CSV additionally loses the separators that would otherwise split a cell.
fn safe(value: &str) -> String {
    redact(value).replace(['\r', '\n', ','], " ")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_export_format_redacts_a_token() {
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
        for format in [ExportFormat::Json, ExportFormat::Csv] {
            let exported = r.export(format).expect("export");
            assert!(!exported.contains("abc"), "{format:?} leaked a token");
            assert!(exported.contains("[已脱敏]"));
        }
    }

    #[test]
    fn csv_cells_never_carry_a_separator() {
        let r = Report {
            rows: vec![ReportRow {
                task_id: "1".into(),
                source_url: "https://x/a,b\nc".into(),
                target_url: "https://y/r".into(),
                status: AggregateStatus::Skipped,
                error_code: None,
                excluded_refs: vec![],
            }],
        };
        assert_eq!(r.to_csv().lines().count(), 2);
    }
}
