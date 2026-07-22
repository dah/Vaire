use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppState {
    pub connection: ConnectionState,
    pub auth: AuthState,
    pub thread: ThreadState,
    pub turn: TurnState,
    pub models: Vec<ModelChoice>,
    pub selected_model: Option<String>,
    pub selected_reasoning: Option<String>,
    pub context_remaining_percent: Option<u8>,
    pub context_suppressed_turn: Option<(String, String)>,
    pub transcript: Vec<TranscriptEntry>,
    pub transcript_dropped_prefix_bytes: BTreeMap<(String, String), TranscriptTruncation>,
    pub thread_picker: Option<ThreadPickerState>,
    pub thinking: ThinkingState,
    pub preferences: PreferencesV1,
    pub notice: Option<String>,
    /// The account identity captured when an eager `/new` operation began.
    /// The outer option distinguishes no request from a request made while the
    /// server reported no stable account identity.
    pub pending_new_thread_scope: Option<Option<AccountScope>>,
    pub pending_thread_deletions: Option<BTreeSet<String>>,
    pub shutting_down: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            connection: ConnectionState::Disconnected,
            auth: AuthState::Unknown,
            thread: ThreadState::None,
            turn: TurnState::Idle,
            models: Vec::new(),
            selected_model: None,
            selected_reasoning: None,
            context_remaining_percent: None,
            context_suppressed_turn: None,
            transcript: Vec::new(),
            transcript_dropped_prefix_bytes: BTreeMap::new(),
            thread_picker: None,
            thinking: ThinkingState::default(),
            preferences: PreferencesV1::default(),
            notice: None,
            pending_new_thread_scope: None,
            pending_thread_deletions: None,
            shutting_down: false,
        }
    }
}

impl AppState {
    /// Returns whether the active turn is waiting for its first visible assistant payload.
    ///
    /// This is deliberately derived from turn and transcript state. The activity indicator is
    /// presentation-only and must never be inserted into conversation history or preferences.
    pub fn is_waiting_for_assistant_text(&self) -> bool {
        if self.shutting_down
            || !matches!(self.connection, ConnectionState::Ready { .. })
            || !matches!(self.auth, AuthState::SignedIn { .. })
        {
            return false;
        }

        match &self.turn {
            TurnState::Starting => {
                matches!(self.thread, ThreadState::None | ThreadState::Ready { .. })
            }
            TurnState::Streaming { thread_id, turn_id } => {
                let active_thread_matches =
                    matches!(&self.thread, ThreadState::Ready { id } if id == thread_id);
                active_thread_matches
                    && !self
                        .transcript
                        .iter()
                        .rev()
                        .take_while(|entry| entry.role == TranscriptRole::Assistant)
                        .any(|entry| {
                            entry.turn_id.as_deref() == Some(turn_id) && !entry.text.is_empty()
                        })
            }
            TurnState::Idle
            | TurnState::Completed { .. }
            | TurnState::Interrupted { .. }
            | TurnState::Failed { .. } => false,
        }
    }
}
