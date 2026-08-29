//! Native credential entry.
//!
//! CM-004 forbids a secret in any command payload, which means the GUI can
//! never be the thing that reads a token. The renderer only ever asks for a
//! *name*; the secret is typed into the console companion binary
//! (`src/bin/credential_entry.rs`) and written straight to Windows Credential
//! Manager, so it never enters the webview, the IPC boundary or SQLite.
//!
//! The logic lives here rather than in the binary so it can be tested with an
//! injected reader instead of a terminal.

use crate::CredentialStore;
use git_repo_migrator_platform_core::{CredentialRef, PlatformError};

/// Longest credential name worth accepting. Windows Credential Manager targets
/// are limited well above this; the cap exists so a pasted token can never be
/// mistaken for a name.
const MAX_NAME_LEN: usize = 64;

/// Validates the name the operator chose for a credential.
///
/// The name reaches a child process as an argument, so it is restricted to
/// characters that cannot be read as a flag, a path traversal or a shell
/// metacharacter — even though no shell is ever involved.
pub fn validate_name(name: &str) -> Result<&str, PlatformError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(PlatformError::validation("凭据名称不能为空"));
    }
    if trimmed.len() > MAX_NAME_LEN {
        return Err(PlatformError::validation(format!(
            "凭据名称不能超过 {MAX_NAME_LEN} 个字符"
        )));
    }
    if trimmed.starts_with('-') {
        return Err(PlatformError::validation("凭据名称不能以 - 开头"));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(PlatformError::validation(
            "凭据名称只能包含字母、数字、连字符、下划线和点",
        ));
    }
    Ok(trimmed)
}

/// The reference a given name will always map to.
///
/// The connection page shows this so the operator can copy it after entry, and
/// the entry binary produces the same value; both must agree without either
/// side seeing the secret.
pub fn reference_for(name: &str) -> Result<CredentialRef, PlatformError> {
    let name = validate_name(name)?;
    CredentialRef::new(format!("credential/windows/{}", crate::stable_id(name)))
}

/// Rejects input that is obviously not a token, so a mistyped entry fails here
/// rather than as a confusing 401 halfway through a batch.
fn validate_secret(secret: &str) -> Result<(), PlatformError> {
    if secret.is_empty() {
        return Err(PlatformError::validation("令牌不能为空"));
    }
    if secret.chars().any(char::is_whitespace) {
        return Err(PlatformError::validation(
            "令牌不能包含空格或换行；请确认粘贴的内容完整且没有多余字符",
        ));
    }
    Ok(())
}

/// Outcome of an entry session. Deliberately carries no secret: this is what
/// the operator sees and what may be logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub name: String,
    pub reference: CredentialRef,
}

/// Stores a secret under `name`, reading it through `read`.
///
/// The secret is confirmed by typing it twice, because a wrong token stored
/// silently is indistinguishable from a permission problem later on.
pub fn store_interactively<R>(
    store: &CredentialStore,
    name: &str,
    mut read: R,
) -> Result<StoredCredential, PlatformError>
where
    R: FnMut(&str) -> Result<String, PlatformError>,
{
    let name = validate_name(name)?.to_owned();
    let secret = read("请粘贴或输入令牌（输入时不会回显）：")?;
    validate_secret(&secret)?;
    let confirmation = read("请再输入一次以确认：")?;
    if secret != confirmation {
        return Err(PlatformError::validation("两次输入不一致，凭据未保存"));
    }
    let reference = store.put(&name, secret.as_bytes())?;
    Ok(StoredCredential { name, reference })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(values: Vec<&str>) -> impl FnMut(&str) -> Result<String, PlatformError> {
        let mut queue = values
            .into_iter()
            .map(str::to_owned)
            .collect::<std::collections::VecDeque<_>>();
        move |_| {
            queue
                .pop_front()
                .ok_or_else(|| PlatformError::validation("没有更多输入"))
        }
    }

    #[test]
    fn a_matching_pair_is_stored_and_only_a_reference_comes_back() {
        let store = CredentialStore::in_memory();
        let stored = store_interactively(
            &store,
            "git-repo-migrator-source",
            reader(vec!["ghp-secret", "ghp-secret"]),
        )
        .expect("stored");

        assert_eq!(stored.name, "git-repo-migrator-source");
        let debug = format!("{stored:?}");
        assert!(
            !debug.contains("ghp-secret"),
            "the entry result must never carry the secret: {debug}"
        );
        assert_eq!(
            store.get(&stored.reference).expect("read back").expose(),
            b"ghp-secret"
        );
    }

    #[test]
    fn a_mistyped_confirmation_stores_nothing() {
        let store = CredentialStore::in_memory();
        let error = store_interactively(&store, "source", reader(vec!["ghp-secret", "ghp-secrat"]))
            .expect_err("a mismatch must not be stored");
        assert!(error.safe_message.contains("不一致"));
        assert!(!error.safe_message.contains("ghp-"));
    }

    #[test]
    fn an_empty_or_whitespace_secret_is_refused() {
        let store = CredentialStore::in_memory();
        for value in ["", " ", "gh p", "ghp-secret\n"] {
            assert!(
                store_interactively(&store, "source", reader(vec![value, value])).is_err(),
                "{value:?} must not be accepted as a token"
            );
        }
    }

    #[test]
    fn a_name_that_could_be_read_as_a_flag_or_a_path_is_refused() {
        for name in [
            "",
            "   ",
            "--store-credential",
            "-x",
            "../../etc/passwd",
            "a b",
            "name;calc.exe",
            "name\"quote",
            &"x".repeat(65),
        ] {
            assert!(
                validate_name(name).is_err(),
                "{name:?} must not be accepted as a credential name"
            );
        }
    }

    #[test]
    fn ordinary_names_are_accepted_and_trimmed() {
        assert_eq!(validate_name("  source  ").expect("valid"), "source");
        assert_eq!(
            validate_name("git-repo-migrator.target_1").expect("valid"),
            "git-repo-migrator.target_1"
        );
    }

    /// The reference is what the operator types into the connection page, so it
    /// has to be stable for a given name.
    #[test]
    fn the_same_name_always_maps_to_the_same_reference() {
        let store = CredentialStore::in_memory();
        let first = store_interactively(&store, "source", reader(vec!["a", "a"])).expect("first");
        let second = store_interactively(&store, "source", reader(vec!["b", "b"])).expect("second");
        assert_eq!(first.reference, second.reference);
        assert_eq!(
            store.get(&second.reference).expect("read back").expose(),
            b"b",
            "re-entering a credential must replace it"
        );
    }

    /// The GUI predicts the reference so it can prefill the connection form; the
    /// entry binary produces it independently. A drift here would leave the
    /// operator with a reference that resolves to nothing.
    #[test]
    fn the_predicted_reference_matches_what_entry_actually_stores() {
        let store = CredentialStore::in_memory();
        let stored = store_interactively(&store, "target", reader(vec!["a", "a"])).expect("stored");
        assert_eq!(
            reference_for("target").expect("predicted"),
            stored.reference
        );
        assert_ne!(
            reference_for("target").expect("predicted"),
            reference_for("source").expect("predicted")
        );
        assert!(reference_for("--flag").is_err());
    }
}
