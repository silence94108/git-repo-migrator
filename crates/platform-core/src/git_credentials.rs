//! The environment contract that carries Git authentication across the
//! process boundary without ever carrying a secret.
//!
//! Git itself cannot read Windows Credential Manager through our private
//! namespace, so the executor points Git's `GIT_ASKPASS` at the application
//! binary and describes *which* credential to use through two environment
//! variables. The askpass program resolves the reference and hands the token to
//! Git over a pipe; it never appears in argv, in the environment, or in a log.
//!
//! The names live here — not in the application or credential crates — because
//! both sides of the contract depend on this crate, and a copy in each would
//! drift apart silently.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::PlatformKind;

/// Git's own variable: the program asked for an HTTP username or password.
pub const ENV_ASKPASS_PROGRAM: &str = "GIT_ASKPASS";
/// The credential-store *reference* (a public id, never the token) to answer
/// the password prompt with.
pub const ENV_CREDENTIAL_REF: &str = "GRM_CREDENTIAL_REF";
/// The basic-auth username to answer the username prompt with.
pub const ENV_USERNAME: &str = "GRM_GIT_USERNAME";

/// Git-level authentication for one remote, described without any secret.
///
/// The token stays in the credential store until the askpass program reads it;
/// this type only says where to find it and which username to present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCredential {
    /// Credential-store reference, e.g. `credential/windows/<id>`.
    pub credential_ref: String,
    /// The basic-auth username. Platforms that ignore it (Gitea, GitHub) accept
    /// any non-empty value; GitLab requires `oauth2`.
    pub username: String,
}

/// The username Git should present for a platform's HTTP basic auth.
///
/// The token rides in the password field, where every supported platform
/// verifies it independently of the username — except GitLab, which insists on
/// the literal `oauth2` user.
pub fn basic_auth_username(platform: PlatformKind, account_name: Option<&str>) -> String {
    match platform {
        PlatformKind::Gitlab => "oauth2".to_owned(),
        _ => account_name
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "git".to_owned()),
    }
}

/// Builds the environment that points Git's askpass hook at one credential.
///
/// Exactly three variables, none of them secret. Every caller — the executor,
/// the target probe — goes through this function so the contract has a single
/// implementation on the writing side, next to the askpass reader that consumes
/// it.
pub fn askpass_env(program: &Path, credential: &RemoteCredential) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert(
        ENV_ASKPASS_PROGRAM.to_owned(),
        program.display().to_string(),
    );
    env.insert(
        ENV_CREDENTIAL_REF.to_owned(),
        credential.credential_ref.clone(),
    );
    env.insert(ENV_USERNAME.to_owned(), credential.username.clone());
    env
}
