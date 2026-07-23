use crate::command::HELP_TEXT;
#[cfg(test)]
use crate::persistence::CodexPreferencesV2;
use crate::persistence::{AccountScope, PreferencesV2};
use crate::provider::{
    ConversationRef, ModelKey, OpenRouterConversationId, OpenRouterTurnId, ProviderId, TurnRef,
};
use crate::text::sanitize_terminal_text;
use std::collections::{BTreeMap, BTreeSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod account;
mod actions;
mod domain;
mod popup;
mod reducer;
mod state;
mod thinking;
mod thread_events;
mod thread_picker;
mod transcript;
mod turn;

pub use actions::{Action, DomainEvent, Effect, TurnOutcome};
pub use domain::{
    AuthState, ConnectionState, Intent, ModelChoice, OpenRouterConversationState,
    OpenRouterCredentialValidation, OpenRouterState, ThreadState, TranscriptEntry,
    TranscriptEntryStatus, TranscriptRole, TranscriptTruncation, TurnState,
};
pub(crate) use popup::{catalog_search_matches, model_search_matches};
pub use popup::{AuthPopupMode, PopupState};
pub use state::AppState;
pub use thinking::{ThinkingEntry, ThinkingKind, ThinkingState};
pub use thread_picker::{
    ThreadChoice, ThreadDeleteConfirmation, ThreadDeletionFailure, ThreadPickerPhase,
    ThreadPickerState,
};
pub use turn::remaining_context_percent;

#[cfg(test)]
mod tests;
