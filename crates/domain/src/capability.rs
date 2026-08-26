use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    NativeRebuild,
    ReadOnlyArchive,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityMatrix {
    pub supported: bool,
    pub permitted: bool,
    pub required_scopes: Vec<String>,
    pub version: Option<String>,
    pub reason: Option<String>,
    pub degradation: Option<String>,
    pub fidelity: Fidelity,
}

impl CapabilityMatrix {
    pub fn native(scopes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            supported: true,
            permitted: true,
            required_scopes: scopes.into_iter().map(Into::into).collect(),
            version: None,
            reason: None,
            degradation: None,
            fidelity: Fidelity::NativeRebuild,
        }
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            supported: false,
            permitted: false,
            required_scopes: Vec::new(),
            version: None,
            reason: Some(reason.into()),
            degradation: None,
            fidelity: Fidelity::Unsupported,
        }
    }
}
