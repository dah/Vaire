use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;

use thiserror::Error;

use crate::storage::CommitStatus;
use zeroize::Zeroizing;

mod file;

pub use file::FileCredentialStore;

pub const MAX_CREDENTIAL_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CredentialAccount {
    OpenRouterApiKey,
    AnthropicConsoleApiKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialFailureCategory {
    Read,
    Write,
    Delete,
    Permissions,
    Corrupt,
}

#[derive(Clone, Copy, Error, Eq, PartialEq)]
#[error("credential storage failed ({category:?})")]
pub struct CredentialStoreError {
    category: CredentialFailureCategory,
}

impl CredentialStoreError {
    pub fn new(category: CredentialFailureCategory) -> Self {
        Self { category }
    }

    pub fn category(self) -> CredentialFailureCategory {
        self.category
    }
}

impl fmt::Debug for CredentialStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialStoreError")
            .field("category", &self.category)
            .finish()
    }
}

/// A short-lived secret buffer.
///
/// It intentionally cannot be cloned or serialized, and both debug output and error paths are
/// content-free. Dropping it zeroizes the owned bytes.
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    pub fn from_input(value: impl Into<String>) -> Result<Self, CredentialStoreError> {
        let value = Zeroizing::new(value.into());
        let normalized = value.trim();
        Self::from_normalized_string(normalized.to_owned())
    }

    pub(crate) fn from_stored_bytes(bytes: Vec<u8>) -> Result<Self, CredentialStoreError> {
        match String::from_utf8(bytes) {
            Ok(value) => Self::from_normalized_string(value),
            Err(error) => {
                let mut bytes = Zeroizing::new(error.into_bytes());
                bytes.clear();
                Err(CredentialStoreError::new(
                    CredentialFailureCategory::Corrupt,
                ))
            }
        }
    }

    fn from_normalized_string(value: String) -> Result<Self, CredentialStoreError> {
        let mut value = Zeroizing::new(value);
        if value.is_empty()
            || value.len() > MAX_CREDENTIAL_BYTES
            || value.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
        {
            value.clear();
            return Err(CredentialStoreError::new(
                CredentialFailureCategory::Corrupt,
            ));
        }
        Ok(Self(value))
    }

    /// Exposes the secret only to credential and request-construction boundaries.
    pub fn expose_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Returns ownership to the masked editor without creating an unprotected copy.
    pub fn into_input(self) -> Zeroizing<String> {
        self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

pub trait CredentialStore: Send + Sync {
    fn load(&self, account: CredentialAccount)
        -> Result<Option<SecretValue>, CredentialStoreError>;

    fn replace(
        &self,
        account: CredentialAccount,
        value: SecretValue,
    ) -> Result<(), CredentialStoreError>;

    fn replace_with_commit(
        &self,
        account: CredentialAccount,
        value: SecretValue,
    ) -> Result<CommitStatus, CredentialStoreError> {
        self.replace(account, value)?;
        Ok(CommitStatus::Verified)
    }

    fn delete(&self, account: CredentialAccount) -> Result<(), CredentialStoreError>;

    fn delete_with_commit(
        &self,
        account: CredentialAccount,
    ) -> Result<CommitStatus, CredentialStoreError> {
        self.delete(account)?;
        Ok(CommitStatus::Verified)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FakeCredentialOperation {
    Load(CredentialAccount),
    Replace(CredentialAccount),
    Delete(CredentialAccount),
}

#[derive(Default)]
struct FakeCredentialState {
    values: BTreeMap<CredentialAccount, Zeroizing<Vec<u8>>>,
    operations: Vec<FakeCredentialOperation>,
    next_failure: Option<CredentialFailureCategory>,
}

/// A deterministic, content-redacting credential store for unit and integration tests.
#[derive(Default)]
pub struct FakeCredentialStore {
    state: Mutex<FakeCredentialState>,
}

impl FakeCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_openrouter_key(value: SecretValue) -> Self {
        let store = Self::new();
        store
            .replace(CredentialAccount::OpenRouterApiKey, value)
            .expect("in-memory credential replacement cannot fail");
        store
            .state
            .lock()
            .expect("fake store lock")
            .operations
            .clear();
        store
    }

    pub fn fail_next(&self, category: CredentialFailureCategory) {
        self.state.lock().expect("fake store lock").next_failure = Some(category);
    }

    pub fn operations(&self) -> Vec<FakeCredentialOperation> {
        self.state
            .lock()
            .expect("fake store lock")
            .operations
            .clone()
    }

    pub fn is_configured(&self, account: CredentialAccount) -> bool {
        self.state
            .lock()
            .expect("fake store lock")
            .values
            .contains_key(&account)
    }

    fn state(
        &self,
        operation: FakeCredentialOperation,
    ) -> Result<std::sync::MutexGuard<'_, FakeCredentialState>, CredentialStoreError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CredentialStoreError::new(CredentialFailureCategory::Corrupt))?;
        state.operations.push(operation);
        if let Some(category) = state.next_failure.take() {
            return Err(CredentialStoreError::new(category));
        }
        Ok(state)
    }
}

impl fmt::Debug for FakeCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FakeCredentialStore([REDACTED])")
    }
}

impl CredentialStore for FakeCredentialStore {
    fn load(
        &self,
        account: CredentialAccount,
    ) -> Result<Option<SecretValue>, CredentialStoreError> {
        let state = self.state(FakeCredentialOperation::Load(account))?;
        state
            .values
            .get(&account)
            .map(|value| value.to_vec())
            .map(SecretValue::from_stored_bytes)
            .transpose()
    }

    fn replace(
        &self,
        account: CredentialAccount,
        value: SecretValue,
    ) -> Result<(), CredentialStoreError> {
        let mut state = self.state(FakeCredentialOperation::Replace(account))?;
        state
            .values
            .insert(account, Zeroizing::new(value.expose_bytes().to_vec()));
        Ok(())
    }

    fn delete(&self, account: CredentialAccount) -> Result<(), CredentialStoreError> {
        let mut state = self.state(FakeCredentialOperation::Delete(account))?;
        state.values.remove(&account);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
