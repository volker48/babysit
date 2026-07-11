use std::cell::RefCell;

use babysit::cli::{GatewayTokenAction, gateway_token_action};
use babysit::credentials::{ACCOUNT, SERVICE, SecretToken, TokenStore};
use babysit::forge::CliError;

#[derive(Default)]
struct MemoryStore(RefCell<Option<SecretToken>>);

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<SecretToken>, CliError> {
        Ok(self.0.borrow().clone())
    }

    fn save(&self, token: &SecretToken) -> Result<(), CliError> {
        self.0.replace(Some(token.clone()));
        Ok(())
    }

    fn delete(&self) -> Result<(), CliError> {
        self.0.replace(None);
        Ok(())
    }
}

#[test]
fn token_store_reports_absence_replaces_values_and_deletes_them() {
    let store = MemoryStore::default();
    assert!(store.load().unwrap().is_none());
    store
        .save(&SecretToken::new("first-token".to_string()).unwrap())
        .unwrap();
    store
        .save(&SecretToken::new("replacement-token".to_string()).unwrap())
        .unwrap();
    assert!(store.load().unwrap().is_some());
    store.delete().unwrap();
    assert!(store.load().unwrap().is_none());
}

#[test]
fn token_actions_cover_enroll_status_delete_rotate_and_keep_input_failures_safe() {
    let store = MemoryStore::default();
    let enrolled = SecretToken::new("enrolled-token".to_string()).unwrap();
    assert_eq!(
        gateway_token_action(GatewayTokenAction::Enroll, &store, || Ok(enrolled)).unwrap(),
        "gateway token saved"
    );
    let status =
        gateway_token_action(GatewayTokenAction::Status, &store, || unreachable!()).unwrap();
    assert_eq!(status, "gateway token: configured");
    assert!(!status.contains("enrolled-token"));

    let error = gateway_token_action(GatewayTokenAction::Rotate, &store, || {
        Err(CliError::new("input failed", false))
    })
    .unwrap_err();
    assert_eq!(error.message, "input failed");
    assert_eq!(
        gateway_token_action(GatewayTokenAction::Status, &store, || unreachable!()).unwrap(),
        "gateway token: configured"
    );
    assert_eq!(
        gateway_token_action(GatewayTokenAction::Delete, &store, || unreachable!()).unwrap(),
        "gateway token deleted"
    );
    assert_eq!(
        gateway_token_action(GatewayTokenAction::Status, &store, || unreachable!()).unwrap(),
        "gateway token: not configured"
    );
    assert_eq!((SERVICE, ACCOUNT), ("babysit", "gateway-bearer-token"));
}

#[test]
fn token_values_are_accepted_only_when_nonempty_and_single_line() {
    assert!(SecretToken::new("gateway-token".to_string()).is_ok());
    assert!(SecretToken::new("".to_string()).is_err());
    assert!(SecretToken::new("one\ntwo".to_string()).is_err());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platform_never_falls_back_to_environment_or_files() {
    let error = match babysit::credentials::production_store().load() {
        Ok(_) => panic!("unsupported platform credential load unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.message.contains("macOS Keychain"));
    assert!(!error.message.contains("gateway-token"));
}
