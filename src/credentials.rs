use crate::error::CliError;
use crate::secret_input::read_secret_line;

pub const SERVICE: &str = "babysit";
pub const ACCOUNT: &str = "gateway-bearer-token";

/// A bearer token that deliberately omits `Debug` to avoid accidental disclosure.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretToken(String);

impl SecretToken {
    /// Parses a nonempty, one-line bearer token.
    pub fn new(value: String) -> Result<Self, CliError> {
        if value.is_empty() || value.contains(['\r', '\n']) {
            return Err(CliError::new(
                "gateway token must be a nonempty single line",
                false,
            ));
        }
        Ok(Self(value))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

/// Stores the gateway token without exposing a filesystem or environment fallback.
pub trait TokenStore {
    fn load(&self) -> Result<Option<SecretToken>, CliError>;
    fn save(&self, token: &SecretToken) -> Result<(), CliError>;
    fn delete(&self) -> Result<(), CliError>;
}

/// Reads a bearer token from piped stdin or a no-echo terminal prompt.
pub fn read_token() -> Result<SecretToken, CliError> {
    SecretToken::new(read_secret_line("Gateway token: ", "gateway token")?)
}

pub fn production_store() -> Box<dyn TokenStore> {
    Box::new(PlatformTokenStore)
}

#[cfg(not(target_os = "macos"))]
struct PlatformTokenStore;

#[cfg(not(target_os = "macos"))]
impl TokenStore for PlatformTokenStore {
    fn load(&self) -> Result<Option<SecretToken>, CliError> {
        Err(keychain_unavailable())
    }

    fn save(&self, _token: &SecretToken) -> Result<(), CliError> {
        Err(keychain_unavailable())
    }

    fn delete(&self) -> Result<(), CliError> {
        Err(keychain_unavailable())
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_unavailable() -> CliError {
    CliError::new("gateway tokens require the macOS Keychain", false)
}

#[cfg(target_os = "macos")]
struct PlatformTokenStore;

#[cfg(target_os = "macos")]
impl TokenStore for PlatformTokenStore {
    fn load(&self) -> Result<Option<SecretToken>, CliError> {
        let entry = entry()?;
        match entry.get_password() {
            Ok(token) => SecretToken::new(token).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(keychain_error(error)),
        }
    }

    fn save(&self, token: &SecretToken) -> Result<(), CliError> {
        entry()?
            .set_password(token.expose())
            .map_err(keychain_error)
    }

    fn delete(&self) -> Result<(), CliError> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(keychain_error(error)),
        }
    }
}

#[cfg(target_os = "macos")]
fn entry() -> Result<keyring::Entry, CliError> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(keychain_error)
}

#[cfg(target_os = "macos")]
fn keychain_error(error: keyring::Error) -> CliError {
    CliError::new(format!("macOS Keychain operation failed: {error}"), false)
}
