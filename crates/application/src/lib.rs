pub mod archive;
pub mod ipc_contract;
pub mod orchestrator;
pub mod planning;
pub mod platform_modules;
pub mod recovery;
pub mod report;
pub mod verification;

pub use archive::{ArchiveAttachment, ArchiveDocument, ArchiveItem};
pub use ipc_contract::{typescript_contract, IpcError, MigrationEvent};
pub use orchestrator::{BatchControl, Orchestrator, QueueTask, RetryDecision};
pub use planning::{build_preview, Candidate, PlanPreview, SelectionSet, TargetState};
pub use platform_modules::{execute_module, retry_failed_items, ModuleExecution, PlatformItem};
pub use report::{ExportFormat, Report, ReportRow};
pub use verification::{AggregateStatus, VerificationEvidence, VerificationResult};
