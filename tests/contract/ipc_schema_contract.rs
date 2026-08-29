use git_repo_migrator_application::ipc_contract::ConnectionTestInput;

#[test]
fn rust_input_rejects_unknown_secret_fields() {
    let json = r#"{"endpoint":"https://github.com","platform_hint":"github","credential_ref":null,"access_token":"secret"}"#;
    assert!(serde_json::from_str::<ConnectionTestInput>(json).is_err());
}

#[test]
fn generated_typescript_contains_contract_matrix_enums() {
    let generated = git_repo_migrator_application::typescript_contract();
    for value in [
        "native_rebuild",
        "read_only_archive",
        "unsupported",
        "rate_limited",
        "generic_git",
    ] {
        assert!(
            generated.contains(value),
            "generated IPC is missing {value}"
        );
    }
    assert!(!generated.contains("access_token"));
    assert!(!generated.contains("response_body"));
    // The authorize payload is the closest thing to a credential command; its
    // shape is pinned so a secret field cannot be added without this failing.
    assert!(generated.contains("export interface ConnectionAuthorizeInput { name: string; }"));
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!("{:x}", Sha256::digest(generated.as_bytes())),
        "57237cbed4626d60f0b0a50b6b2dab9f7b335bafba1d2b7c8e4af9466c725007"
    );
}
