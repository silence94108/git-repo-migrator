pub mod capability;
pub mod error;
pub mod plan;
pub mod ref_policy;
pub mod task_state;

pub use capability::{CapabilityMatrix, Fidelity};
pub use error::{ErrorCategory, MigrationError};
pub use plan::{ConflictPolicy, MigrationPlan, ModuleSelection, RepositoryMapping};
pub use ref_policy::{RefClassification, RefPolicy, RefPolicyDecision};
pub use task_state::{RepoTaskState, TaskTransition};
