use crate::command::HELP_TEXT;
use crate::persistence::{AccountScope, PreferencesV1};
use crate::text::sanitize_terminal_text;
use std::collections::{BTreeMap, BTreeSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod account;
mod actions;
mod domain;
mod reducer;
mod state;
mod thinking;
mod thread_events;
mod thread_picker;
mod transcript;
mod turn;

pub use actions::{Action, DomainEvent, Effect, TurnOutcome};
pub use domain::{
    AuthState, ConnectionState, Intent, ModelChoice, ThreadState, TranscriptEntry, TranscriptRole,
    TranscriptTruncation, TurnState,
};
pub use state::AppState;
pub use thinking::{ThinkingEntry, ThinkingKind, ThinkingState};
pub use thread_picker::{
    ThreadChoice, ThreadDeleteConfirmation, ThreadDeletionFailure, ThreadPickerPhase,
    ThreadPickerState,
};
pub use turn::remaining_context_percent;

#[cfg(test)]
mod tests;
