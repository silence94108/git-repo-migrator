//! A deliberately small, argv-only boundary around the system Git executable.
//! No shell is ever involved and credentials are never accepted as command input.

mod process;
pub mod refs;
pub mod verification;

pub use process::{GitError, GitExecutable, GitOutput, GitRunner, RunOptions};
pub use refs::{build_allowlisted_refspecs, discover_refs, push_allowlisted_refs, RefEntry};
pub use verification::{verify_refs, RefVerification};
