use git_repo_migrator_credential_store::CredentialStore;

#[test]
fn secret_does_not_cross_persistence_or_diagnostic_boundaries() {
    let store = CredentialStore::in_memory();
    let reference = store
        .put("github.example/alice", b"fixture-secret")
        .unwrap();
    let guard = store.get(&reference).unwrap();

    let sqlite_payload = format!("{{\"credential_ref\":\"{}\"}}", reference.as_str());
    let log_payload = format!("credential={reference:?} secret={guard:?}");
    let argv = ["git", "fetch", "https://example.test/team/repo.git"];
    let environment = ["GIT_TERMINAL_PROMPT=0"];
    let crash_payload = format!("reference={reference:?}");

    for payload in [
        sqlite_payload,
        log_payload,
        argv.join(" "),
        environment.join(" "),
        crash_payload,
    ] {
        assert!(!payload.contains("fixture-secret"));
    }
}

#[test]
fn deleting_an_active_connection_is_rejected() {
    let store = CredentialStore::in_memory();
    let reference = store.put("gitlab.example/alice", b"secret").unwrap();
    assert!(store.delete(&reference, true).is_err());
    assert!(store.get(&reference).is_ok());
}
