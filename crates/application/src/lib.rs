/// The archive document types live in `platform-core` so platform adapters can
/// produce them directly; they are re-exported here because the executor and the
/// fidelity contracts treat them as application-level vocabulary.
pub mod archive {
    pub use git_repo_migrator_platform_core::archive::{
        ArchiveAttachment, ArchiveDocument, ArchiveItem, ARCHIVE_SCHEMA_VERSION,
    };
}
pub mod executor;
pub mod ipc_contract;
pub mod orchestrator;
pub mod planning;
pub mod platform_modules;
pub mod recovery;
pub mod report;
pub mod verification;

pub use archive::{ArchiveAttachment, ArchiveDocument, ArchiveItem};
pub use executor::{
    ExecutionAction, ExecutionStage, ModuleGateway, ModuleReport, StageExecutor, StageRecorder,
    TargetGateway, TaskAssignment, TaskExecution,
};
pub use ipc_contract::{typescript_contract, IpcError, MigrationEvent};
pub use orchestrator::{BatchControl, Orchestrator, QueueTask, RetryDecision};
pub use planning::{build_preview, Candidate, PlanPreview, SelectionSet, TargetState};
pub use platform_modules::{execute_module, retry_failed_items, ModuleExecution, PlatformItem};
pub use report::{ExportFormat, Report, ReportRow};
pub use verification::{AggregateStatus, VerificationEvidence, VerificationResult};
