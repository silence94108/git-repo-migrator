use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Auth,
    Permission,
    Conflict,
    RateLimited,
    Network,
    Validation,
    Disk,
    Unsupported,
    Git,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationError {
    pub code: String,
    pub category: ErrorCategory,
    pub retryable: bool,
    pub stage: String,
    pub safe_message: String,
    pub action: String,
}
