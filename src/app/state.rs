use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppState {
    pub active_provider: ProviderId,
    pub connection: ConnectionState,
    pub auth: AuthState,
    pub openrouter: OpenRouterState,
    pub claude: ClaudeState,
    pub thread: ThreadState,
    pub turn: TurnState,
    pub models: Vec<ModelChoice>,
    pub selected_model: Option<ModelKey>,
    pub selected_reasoning: Option<String>,
    pub context_remaining_percent: Option<u8>,
    pub context_suppressed_turn: Option<(String, String)>,
    pub transcript: Vec<TranscriptEntry>,
    pub transcript_dropped_prefix_bytes: BTreeMap<(String, String), TranscriptTruncation>,
    pub popup: Option<PopupState>,
    pub thinking: ThinkingState,
    pub preferences: PreferencesV4,
    pub notice: Option<String>,
    /// The account identity captured when an eager `/new` operation began.
    /// The outer option distinguishes no request from a request made while the
    /// server reported no stable account identity.
    pub pending_new_thread_scope: Option<Option<AccountScope>>,
    pub pending_new_claude_session: bool,
    pub pending_thread_deletions: Option<BTreeSet<String>>,
    pub shutting_down: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            active_provider: ProviderId::Codex,
            connection: ConnectionState::Disconnected,
            auth: AuthState::Unknown,
            openrouter: OpenRouterState::default(),
            claude: ClaudeState::default(),
            thread: ThreadState::None,
            turn: TurnState::Idle,
            models: Vec::new(),
            selected_model: None,
            selected_reasoning: None,
            context_remaining_percent: None,
            context_suppressed_turn: None,
            transcript: Vec::new(),
            transcript_dropped_prefix_bytes: BTreeMap::new(),
            popup: None,
            thinking: ThinkingState::default(),
            preferences: PreferencesV4::default(),
            notice: None,
            pending_new_thread_scope: None,
            pending_new_claude_session: false,
            pending_thread_deletions: None,
            shutting_down: false,
        }
    }
}

impl AppState {
    pub fn conversation_popup(&self) -> Option<&ThreadPickerState> {
        match self.popup.as_ref() {
            Some(PopupState::Conversation(picker)) => Some(picker),
            _ => None,
        }
    }

    pub fn conversation_popup_mut(&mut self) -> Option<&mut ThreadPickerState> {
        match self.popup.as_mut() {
            Some(PopupState::Conversation(picker)) => Some(picker),
            _ => None,
        }
    }

    pub(in crate::app) fn close_conversation_popup(&mut self) {
        if matches!(self.popup, Some(PopupState::Conversation(_))) {
            self.popup = None;
        }
    }

    pub fn active_conversation_ref(&self) -> Option<ConversationRef> {
        match self.active_provider {
            ProviderId::Codex => match &self.thread {
                ThreadState::Ready { id } => Some(ConversationRef::Codex {
                    thread_id: id.clone(),
                }),
                ThreadState::None
                | ThreadState::Resuming { .. }
                | ThreadState::ResumeFailed { .. }
                | ThreadState::AccountMismatch { .. } => None,
            },
            ProviderId::OpenRouter => match &self.openrouter.conversation {
                OpenRouterConversationState::Ready { id } => Some(ConversationRef::OpenRouter {
                    conversation_id: id.clone(),
                }),
                OpenRouterConversationState::None
                | OpenRouterConversationState::ResumeFailed { .. } => None,
            },
            ProviderId::Claude => match &self.claude.conversation {
                ClaudeConversationState::Ready { id } => Some(ConversationRef::Claude {
                    session_id: id.clone(),
                }),
                ClaudeConversationState::None
                | ClaudeConversationState::ResumeFailed { .. }
                | ClaudeConversationState::CreationUncertain { .. } => None,
            },
        }
    }

    pub fn active_turn_ref(&self) -> Option<TurnRef> {
        match &self.turn {
            TurnState::Streaming { thread_id, turn_id } => Some(TurnRef::Codex {
                thread_id: thread_id.clone(),
                turn_id: turn_id.clone(),
            }),
            TurnState::OpenRouterStreaming {
                conversation_id,
                turn_id,
            } => Some(TurnRef::OpenRouter {
                conversation_id: conversation_id.clone(),
                turn_id: turn_id.clone(),
            }),
            TurnState::ClaudeStreaming {
                session_id,
                turn_id,
            } => Some(TurnRef::Claude {
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
            }),
            _ => None,
        }
    }

    /// Returns whether the active turn is waiting for its first visible assistant payload.
    ///
    /// This is deliberately derived from turn and transcript state. The activity indicator is
    /// presentation-only and must never be inserted into conversation history or preferences.
    pub fn is_waiting_for_assistant_text(&self) -> bool {
        let provider_ready = match self.active_provider {
            ProviderId::Codex => matches!(self.connection, ConnectionState::Ready { .. }),
            ProviderId::OpenRouter => true,
            ProviderId::Claude => matches!(self.claude.availability, ClaudeAvailability::Ready),
        };
        if self.shutting_down || !provider_ready || !self.active_provider_is_authenticated() {
            return false;
        }

        match &self.turn {
            TurnState::Starting => match self.active_provider {
                ProviderId::Codex => {
                    matches!(self.thread, ThreadState::None | ThreadState::Ready { .. })
                }
                ProviderId::OpenRouter => matches!(
                    self.openrouter.conversation,
                    OpenRouterConversationState::None | OpenRouterConversationState::Ready { .. }
                ),
                ProviderId::Claude => matches!(
                    self.claude.conversation,
                    ClaudeConversationState::None | ClaudeConversationState::Ready { .. }
                ),
            },
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
            TurnState::OpenRouterStreaming {
                conversation_id,
                turn_id,
            } => {
                let active_matches = matches!(
                    &self.openrouter.conversation,
                    OpenRouterConversationState::Ready { id } if id == conversation_id
                );
                active_matches
                    && !self.transcript.iter().rev().any(|entry| {
                        entry.turn_id.as_deref() == Some(turn_id.as_str())
                            && entry.role == TranscriptRole::Assistant
                            && !entry.text.is_empty()
                    })
            }
            TurnState::ClaudeStreaming {
                session_id,
                turn_id,
            } => {
                let active_matches = matches!(
                    &self.claude.conversation,
                    ClaudeConversationState::Ready { id } if id == session_id
                );
                active_matches
                    && !self.transcript.iter().rev().any(|entry| {
                        entry.provider == ProviderId::Claude
                            && entry.turn_id.as_deref() == Some(turn_id.as_str())
                            && entry.role == TranscriptRole::Assistant
                            && !entry.text.is_empty()
                    })
            }
            TurnState::Idle
            | TurnState::Completed { .. }
            | TurnState::Interrupted { .. }
            | TurnState::Failed { .. } => false,
        }
    }

    pub fn active_provider_is_authenticated(&self) -> bool {
        match self.active_provider {
            ProviderId::Codex => matches!(self.auth, AuthState::SignedIn { .. }),
            ProviderId::OpenRouter => {
                self.openrouter.auth == crate::openrouter::OpenRouterAuthStatus::Valid
            }
            ProviderId::Claude => self.claude.auth == ClaudeAuthStatus::Subscription,
        }
    }

    pub fn pending_claude_auth_request(&self) -> Option<&ClaudeAuthRequest> {
        match &self.claude.auth_operation {
            ClaudeAuthOperation::AwaitingTerminal { request } => Some(request),
            ClaudeAuthOperation::Idle | ClaudeAuthOperation::Checking { .. } => None,
        }
    }
}
