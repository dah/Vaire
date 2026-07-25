mod domain;
mod file;
mod validation;

pub use domain::{
    AccountScope, ClaudePreferencesV4, CodexPreferencesV2, LoadNotice, LoadOutcome,
    OpenRouterPreferencesV2, PersistenceError, PreferencesPort, PreferencesV4, PREFERENCES_VERSION,
};
pub use file::FilePreferences;

#[cfg(test)]
use domain::MAX_PREFERENCES_BYTES;

#[cfg(test)]
mod tests;
