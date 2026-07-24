mod domain;
mod file;
mod validation;

pub use domain::{
    AccountScope, CodexPreferencesV2, LoadNotice, LoadOutcome, OpenRouterPreferencesV2,
    PersistenceError, PreferencesPort, PreferencesV2, PREFERENCES_VERSION,
};
pub use file::FilePreferences;

#[cfg(test)]
use domain::MAX_PREFERENCES_BYTES;

#[cfg(test)]
mod tests;
