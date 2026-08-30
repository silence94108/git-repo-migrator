//! Answers Git's askpass prompts from the credential store.
//!
//! Git (and git-lfs) run the program named by `GIT_ASKPASS` whenever an HTTP
//! remote needs a username or password, passing the prompt as the only
//! argument. This module decides which half is being asked for and produces it:
//! the username comes from the environment verbatim, the password is read from
//! the credential store *at that moment* and written to stdout — a pipe that
//! leads to Git and to nothing else.
//!
//! A failure here is deliberately loud: an empty answer would look like the
//! operator cancelling the prompt, and Git would turn that into a bare 401 with
//! no hint of what actually went wrong.

use crate::CredentialStore;
use git_repo_migrator_platform_core::git_credentials::{ENV_CREDENTIAL_REF, ENV_USERNAME};
use git_repo_migrator_platform_core::{CredentialRef, PlatformError};

/// Answers one askpass prompt.
///
/// `username` and `credential_ref` are what the caller read from the process
/// environment (`ENV_USERNAME` / `ENV_CREDENTIAL_REF`); they are parameters so
/// the decision logic is testable without touching the real environment.
pub fn answer(
    prompt: &str,
    username: Option<&str>,
    credential_ref: Option<&str>,
    store: &CredentialStore,
) -> Result<String, PlatformError> {
    // Git's prompts are hard-coded English ("Username for '…': "), but the
    // matching stays prefix-free so the git-lfs variant of the prompt — and
    // anything else that is clearly not asking for a username — falls through
    // to the password, which is the far more common question.
    if prompt.to_ascii_lowercase().contains("username") {
        return username
            .map(str::to_owned)
            .ok_or_else(|| PlatformError::validation("askpass 缺少用户名（环境变量未设置）"));
    }
    let reference = credential_ref
        .ok_or_else(|| PlatformError::validation("askpass 缺少凭据引用（环境变量未设置）"))?;
    let reference = CredentialRef::new(reference)
        .map_err(|_| PlatformError::validation("askpass 凭据引用无效"))?;
    let guard = store.get(&reference)?;
    String::from_utf8(guard.expose().to_vec())
        .map_err(|_| PlatformError::validation("凭据不是 UTF-8 文本，无法作为 git 口令使用"))
}

/// Reads the environment contract and answers the prompt in `prompt_arg`.
///
/// This is what the `--askpass` entry mode calls; it is separated from
/// [`answer`] only so tests can inject the environment values.
pub fn answer_from_environment(
    prompt: &str,
    env: &dyn Fn(&str) -> Option<String>,
    store: &CredentialStore,
) -> Result<String, PlatformError> {
    answer(
        prompt,
        env(ENV_USERNAME).as_deref(),
        env(ENV_CREDENTIAL_REF).as_deref(),
        store,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn store_with_token() -> Arc<CredentialStore> {
        Arc::new(CredentialStore::in_memory())
    }

    #[test]
    fn a_username_prompt_is_answered_from_the_environment() {
        let store = store_with_token();
        let answer = answer(
            "Username for 'http://git.example.com': ",
            Some("oauth2"),
            None,
            &store,
        )
        .expect("username");
        assert_eq!(answer, "oauth2");
    }

    #[test]
    fn a_password_prompt_is_answered_with_the_stored_token() {
        let store = store_with_token();
        let reference = store.put("gitea", b"secret-token").expect("stored");
        let answer = answer(
            "Password for 'http://git@example.com': ",
            None,
            Some(reference.as_str()),
            &store,
        )
        .expect("password");
        assert_eq!(answer, "secret-token");
    }

    #[test]
    fn an_unrecognised_prompt_is_treated_as_a_password_question() {
        let store = store_with_token();
        let reference = store.put("source", b"secret-token").expect("stored");
        let answer = answer(
            "Credentials for 'http://example.com'",
            None,
            Some(reference.as_str()),
            &store,
        )
        .expect("password");
        assert_eq!(answer, "secret-token");
    }

    #[test]
    fn a_missing_username_or_reference_is_a_loud_failure() {
        let store = store_with_token();
        let error = answer("Username for 'http://x': ", None, None, &store)
            .expect_err("no username in the environment");
        assert!(error.safe_message.contains("用户名"));

        let error = answer("Password for 'http://x': ", None, None, &store)
            .expect_err("no reference in the environment");
        assert!(error.safe_message.contains("凭据引用"));
    }

    #[test]
    fn an_unknown_reference_is_reported_as_missing_not_empty() {
        let store = store_with_token();
        let error = answer(
            "Password for 'http://x': ",
            None,
            Some("credential/windows/deadbeef"),
            &store,
        )
        .expect_err("reference resolves to nothing");
        assert_eq!(error.code, "credential.not_found");
    }

    #[test]
    fn an_empty_reference_is_rejected_before_the_store_lookup() {
        let store = store_with_token();
        let error = answer("Password for 'http://x': ", None, Some("  "), &store)
            .expect_err("a blank reference is not a reference");
        assert!(error.safe_message.contains("无效"));
    }

    #[test]
    fn the_environment_plumbing_passes_both_values_through() {
        let store = store_with_token();
        let reference = store.put("target", b"secret-token").expect("stored");
        let env = |key: &str| {
            if key == ENV_USERNAME {
                Some("git".to_owned())
            } else if key == ENV_CREDENTIAL_REF {
                Some(reference.as_str().to_owned())
            } else {
                None
            }
        };
        assert_eq!(
            answer_from_environment("Username for 'http://x': ", &env, &store).expect("username"),
            "git"
        );
        assert_eq!(
            answer_from_environment("Password for 'http://x': ", &env, &store).expect("password"),
            "secret-token"
        );
    }
}
