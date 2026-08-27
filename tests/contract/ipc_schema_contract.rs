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
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!("{:x}", Sha256::digest(generated.as_bytes())),
        "4ea0a313dbb3514fc30ad666087aef36bbbce5982a941959e6b2327215e753f9"
    );
}
