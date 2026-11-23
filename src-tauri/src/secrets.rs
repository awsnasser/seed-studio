use crate::error::{AppError, Res};
use std::collections::HashMap;
use std::sync::Mutex;

/// Where API keys live. Production: OS keychain. Tests / dev bridge: memory only.
pub trait SecretStore: Send + Sync {
    fn set(&self, account: &str, secret: &str) -> Res<()>;
    fn get(&self, account: &str) -> Res<Option<String>>;
    fn delete(&self, account: &str) -> Res<()>;
}

pub const SERVICE: &str = "com.seedstudio.desktop";

pub fn account(profile: &str, region: &str) -> String {
    format!("modelark:{profile}:{region}")
}

pub struct KeychainStore;

fn kerr(e: keyring::Error) -> AppError {
    // Never include the secret; keyring errors carry only status text.
    AppError::new("keychain", format!("Could not access the macOS Keychain: {e}"))
}

impl SecretStore for KeychainStore {
    fn set(&self, account: &str, secret: &str) -> Res<()> {
        keyring::Entry::new(SERVICE, account).map_err(kerr)?.set_password(secret).map_err(kerr)
    }
    fn get(&self, account: &str) -> Res<Option<String>> {
        match keyring::Entry::new(SERVICE, account).map_err(kerr)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(kerr(e)),
        }
    }
    fn delete(&self, account: &str) -> Res<()> {
        match keyring::Entry::new(SERVICE, account).map_err(kerr)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(kerr(e)),
        }
    }
}

#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn set(&self, account: &str, secret: &str) -> Res<()> {
        self.0.lock().unwrap().insert(account.into(), secret.into());
        Ok(())
    }
    fn get(&self, account: &str) -> Res<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn delete(&self, account: &str) -> Res<()> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}
