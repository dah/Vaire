use crate::command::HELP_TEXT;
use crate::persistence::{AccountScope, PreferencesV1};
use crate::text::sanitize_terminal_text;

const MAX_THINKING_CHARS: usize = 32 * 1024;
const MAX_THINKING_ENTRIES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    SendMessage(String),
    NewThread,
    Login,
    LoginDevice,
    Logout,
    ShowModels,
    SelectModel(String),
    ShowReasoning,
    SelectReasoning(String),
    ToggleThinking,
    Resume,
    ThreadPickerMoveUp,
    ThreadPickerMoveDown,
    ThreadPickerSelect,
    ThreadPickerClose,
    ThreadPickerRequestDelete,
    ThreadPickerRequestClearInactive,
    ThreadPickerConfirmDelete,
    ThreadPickerCancelDelete,
    Help,
    Quit,
    Interrupt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Ready { generation: u64 },
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthState {
    Unknown,
    SignedOut,
    SigningIn { login_id: String },
    SignedIn { scope: Option<AccountScope> },
    Unsupported(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadState {
    None,
    Resuming { id: String },
    Ready { id: String },
    ResumeFailed { id: String, message: String },
    AccountMismatch { id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnState {
    Idle,
    Starting,
    Streaming {
        thread_id: String,
        turn_id: String,
    },
    Completed {
        turn_id: String,
    },
    Interrupted {
        turn_id: String,
    },
    Failed {
        turn_id: Option<String>,
        message: String,
    },
}

impl TurnState {
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Streaming { .. })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub text: String,
    pub item_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThinkingKind {
    Summary,
    EmittedText,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThinkingEntry {
    pub turn_id: String,
    pub item_id: String,
    pub kind: ThinkingKind,
    pub index: i64,
    pub text: String,
    pub completed: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ThinkingState {
    pub visible: bool,
    pub entries: Vec<ThinkingEntry>,
}

impl ThinkingState {
    fn clear_content(&mut self) {
        self.entries.clear();
    }

    fn ensure_entry(
        &mut self,
        turn_id: &str,
        item_id: &str,
        kind: ThinkingKind,
        index: i64,
    ) -> Option<&mut ThinkingEntry> {
        if index < 0 {
            return None;
        }
        if let Some(position) = self.entries.iter().position(|entry| {
            entry.turn_id == turn_id
                && entry.item_id == item_id
                && entry.kind == kind
                && entry.index == index
        }) {
            return self.entries.get_mut(position);
        }
        if self.entries.len() >= MAX_THINKING_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push(ThinkingEntry {
            turn_id: turn_id.to_owned(),
            item_id: item_id.to_owned(),
            kind,
            index,
            text: String::new(),
            completed: false,
        });
        self.entries.last_mut()
    }

    fn add_part(&mut self, turn_id: &str, item_id: &str, index: i64) {
        self.ensure_entry(turn_id, item_id, ThinkingKind::Summary, index);
    }

    fn append_delta(
        &mut self,
        turn_id: &str,
        item_id: &str,
        kind: ThinkingKind,
        index: i64,
        delta: &str,
    ) {
        let delta = sanitize_terminal_text(delta);
        if delta.is_empty() {
            return;
        }
        if let Some(entry) = self.ensure_entry(turn_id, item_id, kind, index) {
            entry.text.push_str(&delta);
        }
        self.enforce_bound();
    }

    fn reconcile_item(
        &mut self,
        turn_id: &str,
        item_id: &str,
        summary: &[String],
        content: &[String],
    ) {
        for (index, final_text) in summary.iter().enumerate() {
            self.reconcile_entry(
                turn_id,
                item_id,
                ThinkingKind::Summary,
                index as i64,
                final_text,
            );
        }
        for (index, final_text) in content.iter().enumerate() {
            self.reconcile_entry(
                turn_id,
                item_id,
                ThinkingKind::EmittedText,
                index as i64,
                final_text,
            );
        }
        for entry in &mut self.entries {
            if entry.turn_id == turn_id && entry.item_id == item_id {
                entry.completed = true;
            }
        }
        self.enforce_bound();
    }

    fn reconcile_entry(
        &mut self,
        turn_id: &str,
        item_id: &str,
        kind: ThinkingKind,
        index: i64,
        final_text: &str,
    ) {
        let final_text = sanitize_terminal_text(final_text);
        let Some(entry) = self.ensure_entry(turn_id, item_id, kind, index) else {
            return;
        };
        if final_text.is_empty() {
            return;
        }
        // The completed item is authoritative. A matching stream receives only its missing
        // suffix; a contradictory stream is replaced so it can never be duplicated.
        if final_text.starts_with(&entry.text) {
            entry.text.push_str(&final_text[entry.text.len()..]);
        } else {
            entry.text = final_text;
        }
    }

    fn enforce_bound(&mut self) {
        let total = self
            .entries
            .iter()
            .map(|entry| entry.text.chars().count())
            .sum::<usize>();
        let mut excess = total.saturating_sub(MAX_THINKING_CHARS);
        for entry in &mut self.entries {
            if excess == 0 {
                break;
            }
            let available = entry.text.chars().count();
            let remove = available.min(excess);
            trim_chars_from_front(&mut entry.text, remove);
            excess -= remove;
        }
    }
}

fn trim_chars_from_front(value: &mut String, count: usize) {
    if count == 0 {
        return;
    }
    let Some((byte_index, _)) = value.char_indices().nth(count) else {
        value.clear();
        return;
    };
    value.drain(..byte_index);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadChoice {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadPickerPhase {
    Loading,
    Ready,
    Resuming { id: String },
    Deleting { requested: usize },
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadDeleteConfirmation {
    Selected { target: ThreadChoice },
    AllInactive { targets: Vec<ThreadChoice> },
}

impl ThreadDeleteConfirmation {
    pub fn targets(&self) -> Vec<ThreadChoice> {
        match self {
            Self::Selected { target } => vec![target.clone()],
            Self::AllInactive { targets } => targets.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadPickerState {
    pub phase: ThreadPickerPhase,
    pub threads: Vec<ThreadChoice>,
    pub selected: usize,
    pub confirmation: Option<ThreadDeleteConfirmation>,
    pub message: Option<String>,
}

impl ThreadPickerState {
    fn loading() -> Self {
        Self {
            phase: ThreadPickerPhase::Loading,
            threads: Vec::new(),
            selected: 0,
            confirmation: None,
            message: None,
        }
    }

    pub fn selected_thread(&self) -> Option<&ThreadChoice> {
        self.threads.get(self.selected)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadDeletionFailure {
    pub id: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    StartLogin,
    StartDeviceLogin,
    CancelLogin { login_id: String },
    Logout,
    StartNewThread,
    ListThreads,
    ResumeThread { id: String },
    SwitchThread { id: String },
    DeleteThreads { ids: Vec<String> },
    SendMessage { text: String },
    InterruptTurn { thread_id: String, turn_id: String },
    Persist(PreferencesV1),
    Shutdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DomainEvent {
    PreferencesLoaded(PreferencesV1),
    Connecting,
    Connected {
        generation: u64,
    },
    ConnectionFailed(String),
    AccountLoaded(Option<AccountScope>),
    UnsupportedAccount(String),
    LoginStarted {
        login_id: String,
    },
    LoginFailed(String),
    LoggedOut,
    CatalogLoaded(Vec<ModelChoice>),
    ResumeStarted {
        id: String,
    },
    ResumeSucceeded {
        id: String,
        history: Vec<TranscriptEntry>,
    },
    ResumeFailed {
        id: String,
        message: String,
    },
    NewThreadSucceeded {
        id: String,
    },
    NewThreadFailed(String),
    ThreadListLoaded(Vec<ThreadChoice>),
    ThreadListFailed(String),
    ThreadSwitchSucceeded {
        id: String,
        history: Vec<TranscriptEntry>,
    },
    ThreadSwitchFailed {
        id: String,
        message: String,
    },
    ThreadDeletionFinished {
        requested: usize,
        deleted: Vec<String>,
        failures: Vec<ThreadDeletionFailure>,
    },
    ThreadStarted {
        id: String,
    },
    TurnStarted {
        thread_id: String,
        turn_id: String,
    },
    AgentDelta {
        thread_id: String,
        turn_id: String,
        item_id: String,
        delta: String,
    },
    AgentCompleted {
        thread_id: String,
        turn_id: String,
        item_id: String,
        text: String,
    },
    ThinkingSummaryPartAdded {
        thread_id: String,
        turn_id: String,
        item_id: String,
        summary_index: i64,
    },
    ThinkingDelta {
        thread_id: String,
        turn_id: String,
        item_id: String,
        kind: ThinkingKind,
        index: i64,
        delta: String,
    },
    ThinkingCompleted {
        thread_id: String,
        turn_id: String,
        item_id: String,
        summary: Vec<String>,
        content: Vec<String>,
    },
    TurnFinished {
        thread_id: String,
        turn_id: String,
        outcome: TurnOutcome,
    },
    TurnOperationFailed(String),
    ProcessExited(String),
    SafetyViolation(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Intent(Intent),
    Event(DomainEvent),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppState {
    pub connection: ConnectionState,
    pub auth: AuthState,
    pub thread: ThreadState,
    pub turn: TurnState,
    pub models: Vec<ModelChoice>,
    pub selected_model: Option<String>,
    pub selected_reasoning: Option<String>,
    pub transcript: Vec<TranscriptEntry>,
    pub thread_picker: Option<ThreadPickerState>,
    pub thinking: ThinkingState,
    pub preferences: PreferencesV1,
    pub notice: Option<String>,
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
            transcript: Vec::new(),
            thread_picker: None,
            thinking: ThinkingState::default(),
            preferences: PreferencesV1::default(),
            notice: None,
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

    pub fn reduce(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Intent(intent) => self.reduce_intent(intent),
            Action::Event(event) => self.reduce_event(event),
        }
    }

    fn reduce_intent(&mut self, intent: Intent) -> Vec<Effect> {
        self.notice = None;
        match intent {
            Intent::Help => self.notice = Some(HELP_TEXT.to_owned()),
            Intent::Quit => {
                self.shutting_down = true;
                let mut effects = Vec::new();
                if let TurnState::Streaming { thread_id, turn_id } = &self.turn {
                    effects.push(Effect::InterruptTurn {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                    });
                }
                effects.push(Effect::Shutdown);
                return effects;
            }
            Intent::ShowModels => {
                self.notice = Some(if self.models.is_empty() {
                    "model catalog is not available".to_owned()
                } else {
                    self.models
                        .iter()
                        .map(|model| model.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                });
            }
            Intent::ShowReasoning => {
                self.notice = Some(self.current_model().map_or_else(
                    || "select a model first".to_owned(),
                    |model| model.supported_reasoning_efforts.join(", "),
                ));
            }
            Intent::ToggleThinking => {
                self.thinking.visible = !self.thinking.visible;
            }
            Intent::SelectModel(id) => {
                let Some(model) = self.models.iter().find(|model| model.id == id).cloned() else {
                    self.notice = Some(format!("unknown model {id}; use /model"));
                    return Vec::new();
                };
                let old_reasoning = self.selected_reasoning.clone();
                self.selected_model = Some(model.id.clone());
                self.selected_reasoning = old_reasoning
                    .clone()
                    .filter(|effort| model.supported_reasoning_efforts.contains(effort))
                    .or_else(|| Some(model.default_reasoning_effort.clone()));
                if old_reasoning.is_some() && old_reasoning != self.selected_reasoning {
                    self.notice =
                        Some("reasoning was reset to the selected model's default".to_owned());
                }
                self.sync_selection_preferences();
                return vec![Effect::Persist(self.preferences.clone())];
            }
            Intent::SelectReasoning(effort) => {
                let Some(model) = self.current_model() else {
                    self.notice = Some("select a model first".to_owned());
                    return Vec::new();
                };
                if !model.supported_reasoning_efforts.contains(&effort) {
                    self.notice = Some(format!("unsupported reasoning {effort}; use /reasoning"));
                    return Vec::new();
                }
                self.selected_reasoning = Some(effort);
                self.sync_selection_preferences();
                return vec![Effect::Persist(self.preferences.clone())];
            }
            Intent::Login => return self.begin_login(Effect::StartLogin),
            Intent::LoginDevice => return self.begin_login(Effect::StartDeviceLogin),
            Intent::Logout => {
                match &self.auth {
                    AuthState::SigningIn { login_id } => {
                        return vec![Effect::CancelLogin {
                            login_id: login_id.clone(),
                        }];
                    }
                    AuthState::SignedIn { .. } => return vec![Effect::Logout],
                    _ => {}
                }
                self.notice = Some("not signed in".to_owned());
            }
            Intent::NewThread => {
                if let Some(reason) = self.thread_action_block_reason(false) {
                    self.notice = Some(reason);
                } else {
                    self.notice = Some("Starting a new thread…".to_owned());
                    return vec![Effect::StartNewThread];
                }
            }
            Intent::Resume => {
                if let Some(reason) = self.thread_action_block_reason(true) {
                    self.notice = Some(reason);
                } else {
                    self.thread_picker = Some(ThreadPickerState::loading());
                    return vec![Effect::ListThreads];
                }
            }
            Intent::ThreadPickerMoveUp => self.move_thread_picker(-1),
            Intent::ThreadPickerMoveDown => self.move_thread_picker(1),
            Intent::ThreadPickerSelect => return self.select_thread_picker(),
            Intent::ThreadPickerClose => self.close_thread_picker(),
            Intent::ThreadPickerRequestDelete => self.request_selected_thread_delete(),
            Intent::ThreadPickerRequestClearInactive => self.request_clear_inactive_threads(),
            Intent::ThreadPickerConfirmDelete => return self.confirm_thread_delete(),
            Intent::ThreadPickerCancelDelete => {
                if let Some(picker) = &mut self.thread_picker {
                    if picker.confirmation.take().is_some() {
                        picker.message = Some("Deletion cancelled".to_owned());
                    }
                }
            }
            Intent::SendMessage(text) => {
                if let Some(reason) = self.send_block_reason() {
                    self.notice = Some(reason);
                } else {
                    self.turn = TurnState::Starting;
                    self.thinking.clear_content();
                    self.transcript.push(TranscriptEntry {
                        role: TranscriptRole::User,
                        text: text.clone(),
                        item_id: None,
                        turn_id: None,
                    });
                    return vec![Effect::SendMessage { text }];
                }
            }
            Intent::Interrupt => {
                if let TurnState::Streaming { thread_id, turn_id } = &self.turn {
                    return vec![Effect::InterruptTurn {
                        thread_id: thread_id.clone(),
                        turn_id: turn_id.clone(),
                    }];
                }
                self.notice = Some("there is no active turn to interrupt".to_owned());
            }
        }
        Vec::new()
    }

    fn reduce_event(&mut self, event: DomainEvent) -> Vec<Effect> {
        match event {
            DomainEvent::PreferencesLoaded(mut preferences) => {
                if let (Some(id), Some(scope)) = (
                    preferences.thread_id.clone(),
                    preferences.account_scope.clone(),
                ) {
                    preferences.thread_account_scopes.entry(id).or_insert(scope);
                }
                self.selected_model = preferences.model_id.clone();
                self.selected_reasoning = preferences.reasoning_effort.clone();
                self.preferences = preferences;
            }
            DomainEvent::Connecting => self.connection = ConnectionState::Connecting,
            DomainEvent::Connected { generation } => {
                self.connection = ConnectionState::Ready { generation }
            }
            DomainEvent::ConnectionFailed(message) | DomainEvent::ProcessExited(message) => {
                self.connection = ConnectionState::Failed(message.clone());
                if let Some(picker) = &mut self.thread_picker {
                    picker.phase = ThreadPickerPhase::Failed;
                    picker.confirmation = None;
                    picker.message = Some(message.clone());
                }
                if self.turn.is_active() {
                    self.turn = TurnState::Failed {
                        turn_id: None,
                        message,
                    };
                }
            }
            DomainEvent::AccountLoaded(scope) => {
                self.thinking.clear_content();
                let completed_login = matches!(self.auth, AuthState::SigningIn { .. });
                let picker_was_open = self.thread_picker.is_some();
                self.auth = AuthState::SignedIn {
                    scope: scope.clone(),
                };
                if completed_login {
                    self.notice = Some("Signed in to ChatGPT".to_owned());
                }
                if let Some(id) = self.preferences.thread_id.clone() {
                    if scope.is_some() && scope == self.preferences.account_scope {
                        if !picker_was_open {
                            self.thread = ThreadState::Resuming { id: id.clone() };
                            return vec![Effect::ResumeThread { id }];
                        }
                        return Vec::new();
                    }
                    self.thread_picker = None;
                    self.thread = ThreadState::AccountMismatch { id };
                    self.notice =
                        Some("saved thread belongs to a different or unscoped account".to_owned());
                }
            }
            DomainEvent::UnsupportedAccount(message) => self.auth = AuthState::Unsupported(message),
            DomainEvent::LoginStarted { login_id } => self.auth = AuthState::SigningIn { login_id },
            DomainEvent::LoginFailed(message) => {
                self.auth = AuthState::SignedOut;
                self.notice = Some(message);
            }
            DomainEvent::LoggedOut => {
                self.auth = AuthState::SignedOut;
                self.thread = ThreadState::None;
                self.turn = TurnState::Idle;
                self.thread_picker = None;
                self.thinking.clear_content();
            }
            DomainEvent::CatalogLoaded(models) => {
                self.models = models;
                self.validate_selection();
            }
            DomainEvent::ResumeStarted { id } => self.thread = ThreadState::Resuming { id },
            DomainEvent::ResumeSucceeded { id, history } => {
                self.thread = ThreadState::Ready { id: id.clone() };
                self.transcript = history;
                self.thinking.clear_content();
                self.preferences.thread_id = Some(id.clone());
                self.register_thread_scope(&id);
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ResumeFailed { id, message } => {
                self.thread = ThreadState::ResumeFailed {
                    id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::NewThreadSucceeded { id } => {
                self.thread = ThreadState::Ready { id: id.clone() };
                self.turn = TurnState::Idle;
                self.transcript.clear();
                self.thread_picker = None;
                self.thinking.clear_content();
                self.preferences.thread_id = Some(id.clone());
                if let AuthState::SignedIn { scope } = &self.auth {
                    self.preferences.account_scope = scope.clone();
                }
                self.register_thread_scope(&id);
                self.notice = Some("Started a new thread".to_owned());
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::NewThreadFailed(message) => {
                self.notice = Some(format!(
                    "Could not start a new thread; the current thread was preserved: {message}"
                ));
            }
            DomainEvent::ThreadListLoaded(threads) => {
                let active_id = self.active_saved_thread_id().map(str::to_owned);
                let found_local_threads = !threads.is_empty();
                let threads = match &self.auth {
                    AuthState::SignedIn { scope: Some(scope) } => threads
                        .into_iter()
                        .filter(|thread| {
                            self.preferences.thread_account_scopes.get(&thread.id) == Some(scope)
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                if let Some(picker) = &mut self.thread_picker {
                    if matches!(picker.phase, ThreadPickerPhase::Loading) {
                        picker.threads = threads;
                        picker.selected = active_id
                            .as_deref()
                            .and_then(|active| {
                                picker.threads.iter().position(|thread| thread.id == active)
                            })
                            .unwrap_or(0);
                        picker.phase = ThreadPickerPhase::Ready;
                        picker.message = picker.threads.is_empty().then(|| {
                            if found_local_threads {
                                "No saved threads are registered to this ChatGPT account".to_owned()
                            } else {
                                "No saved AgentHarness threads were found".to_owned()
                            }
                        });
                    }
                }
            }
            DomainEvent::ThreadListFailed(message) => {
                if let Some(picker) = &mut self.thread_picker {
                    if matches!(picker.phase, ThreadPickerPhase::Loading) {
                        picker.phase = ThreadPickerPhase::Failed;
                        picker.message = Some(format!("Could not load threads: {message}"));
                    }
                }
            }
            DomainEvent::ThreadSwitchSucceeded { id, history } => {
                let matches_request = self.thread_picker.as_ref().is_some_and(|picker| {
                    matches!(&picker.phase, ThreadPickerPhase::Resuming { id: expected } if expected == &id)
                });
                if matches_request {
                    self.thread = ThreadState::Ready { id: id.clone() };
                    self.turn = TurnState::Idle;
                    self.transcript = history;
                    self.thread_picker = None;
                    self.thinking.clear_content();
                    self.preferences.thread_id = Some(id.clone());
                    if let AuthState::SignedIn { scope } = &self.auth {
                        self.preferences.account_scope = scope.clone();
                    }
                    self.register_thread_scope(&id);
                    self.notice = Some("Resumed the selected thread".to_owned());
                    return vec![Effect::Persist(self.preferences.clone())];
                }
            }
            DomainEvent::ThreadSwitchFailed { id, message } => {
                if let Some(picker) = &mut self.thread_picker {
                    if matches!(&picker.phase, ThreadPickerPhase::Resuming { id: expected } if expected == &id)
                    {
                        picker.phase = ThreadPickerPhase::Ready;
                        picker.message = Some(format!(
                            "Could not resume the selected thread; the active thread was preserved: {message}"
                        ));
                    }
                }
            }
            DomainEvent::ThreadDeletionFinished {
                requested,
                deleted,
                failures,
            } => {
                let active_id = self.active_saved_thread_id().map(str::to_owned);
                if let Some(picker) = &mut self.thread_picker {
                    let expected = match picker.phase {
                        ThreadPickerPhase::Deleting { requested } => requested,
                        _ => return Vec::new(),
                    };
                    if expected != requested {
                        picker.phase = ThreadPickerPhase::Failed;
                        picker.message = Some(
                            "Thread deletion result did not match the requested scope".to_owned(),
                        );
                        return Vec::new();
                    }
                    let protected_reported = deleted
                        .iter()
                        .any(|id| active_id.as_deref() == Some(id.as_str()));
                    let safe_deleted = deleted
                        .iter()
                        .filter(|id| active_id.as_deref() != Some(id.as_str()))
                        .cloned()
                        .collect::<std::collections::HashSet<_>>();
                    picker
                        .threads
                        .retain(|thread| !safe_deleted.contains(thread.id.as_str()));
                    picker.selected = picker.selected.min(picker.threads.len().saturating_sub(1));
                    picker.phase = ThreadPickerPhase::Ready;
                    picker.confirmation = None;

                    let deleted_count = safe_deleted.len();
                    let mut message = if failures.is_empty() && !protected_reported {
                        format!("Deleted {deleted_count} inactive thread(s)")
                    } else {
                        format!(
                            "Deleted {deleted_count} of {requested} inactive thread(s); {} failed",
                            failures.len() + usize::from(protected_reported)
                        )
                    };
                    if !failures.is_empty() {
                        let details = failures
                            .iter()
                            .map(|failure| format!("{}: {}", failure.id, failure.message))
                            .collect::<Vec<_>>()
                            .join("; ");
                        message.push_str(&format!(" — {details}"));
                    }
                    if protected_reported {
                        message
                            .push_str(" — ignored an invalid result for the active saved thread");
                    }
                    picker.message = Some(message);
                    let mut removed_scope = false;
                    for id in &safe_deleted {
                        removed_scope |=
                            self.preferences.thread_account_scopes.remove(id).is_some();
                    }
                    if removed_scope {
                        return vec![Effect::Persist(self.preferences.clone())];
                    }
                }
            }
            DomainEvent::ThreadStarted { id } => {
                self.thread = ThreadState::Ready { id: id.clone() };
                self.preferences.thread_id = Some(id.clone());
                if let AuthState::SignedIn { scope } = &self.auth {
                    self.preferences.account_scope = scope.clone();
                }
                self.register_thread_scope(&id);
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::TurnStarted { thread_id, turn_id } => {
                let thread_matches = matches!(
                    &self.thread,
                    ThreadState::Ready { id } if id == &thread_id
                );
                let turn_matches = match &self.turn {
                    TurnState::Starting => true,
                    TurnState::Streaming {
                        thread_id: active_thread,
                        turn_id: active_turn,
                    } => active_thread == &thread_id && active_turn == &turn_id,
                    _ => false,
                };
                if thread_matches && turn_matches {
                    self.turn = TurnState::Streaming { thread_id, turn_id };
                }
            }
            DomainEvent::AgentDelta {
                thread_id,
                turn_id,
                item_id,
                delta,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.append_delta(&turn_id, &item_id, &delta);
                }
            }
            DomainEvent::AgentCompleted {
                thread_id,
                turn_id,
                item_id,
                text,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    if let Err(message) = self.reconcile_final(&turn_id, &item_id, &text) {
                        self.turn = TurnState::Failed {
                            turn_id: Some(turn_id),
                            message: message.clone(),
                        };
                        self.notice = Some(message);
                    }
                }
            }
            DomainEvent::ThinkingSummaryPartAdded {
                thread_id,
                turn_id,
                item_id,
                summary_index,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.thinking.add_part(&turn_id, &item_id, summary_index);
                }
            }
            DomainEvent::ThinkingDelta {
                thread_id,
                turn_id,
                item_id,
                kind,
                index,
                delta,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.thinking
                        .append_delta(&turn_id, &item_id, kind, index, &delta);
                }
            }
            DomainEvent::ThinkingCompleted {
                thread_id,
                turn_id,
                item_id,
                summary,
                content,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.thinking
                        .reconcile_item(&turn_id, &item_id, &summary, &content);
                }
            }
            DomainEvent::TurnFinished {
                thread_id,
                turn_id,
                outcome,
            } => {
                if self.matches_active(&thread_id, &turn_id) {
                    self.turn = match outcome {
                        TurnOutcome::Completed => TurnState::Completed { turn_id },
                        TurnOutcome::Interrupted => TurnState::Interrupted { turn_id },
                        TurnOutcome::Failed(message) => TurnState::Failed {
                            turn_id: Some(turn_id),
                            message,
                        },
                    };
                }
            }
            DomainEvent::TurnOperationFailed(message) => {
                let turn_id = match &self.turn {
                    TurnState::Streaming { turn_id, .. } => Some(turn_id.clone()),
                    _ => None,
                };
                self.turn = TurnState::Failed {
                    turn_id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::SafetyViolation(method) => {
                self.connection =
                    ConnectionState::Failed("runtime request boundary was triggered".to_owned());
                self.turn = TurnState::Failed {
                    turn_id: None,
                    message: "unexpected server request was denied".to_owned(),
                };
                self.notice = Some(format!("unexpected server request denied: {method}"));
            }
        }
        Vec::new()
    }

    fn current_model(&self) -> Option<&ModelChoice> {
        self.selected_model
            .as_ref()
            .and_then(|id| self.models.iter().find(|model| &model.id == id))
    }

    fn thread_action_block_reason(&self, require_account_identity: bool) -> Option<String> {
        if self.thread_picker.is_some() {
            return Some(
                "close the thread picker before starting another thread action".to_owned(),
            );
        }
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        let AuthState::SignedIn { scope } = &self.auth else {
            return Some("sign in with /login before managing threads".to_owned());
        };
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if require_account_identity && scope.is_none() {
            return Some(
                "ChatGPT account identity is unavailable; thread history cannot be opened safely"
                    .to_owned(),
            );
        }
        if self.models.is_empty() || self.selected_model.is_none() {
            return Some("model catalog is not ready".to_owned());
        }
        None
    }

    fn move_thread_picker(&mut self, delta: isize) {
        let Some(picker) = &mut self.thread_picker else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready)
            || picker.confirmation.is_some()
            || picker.threads.is_empty()
        {
            return;
        }
        picker.message = None;
        let last = picker.threads.len().saturating_sub(1);
        picker.selected = if delta < 0 {
            picker.selected.saturating_sub(1)
        } else {
            picker.selected.saturating_add(1).min(last)
        };
    }

    fn select_thread_picker(&mut self) -> Vec<Effect> {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(picker) = &mut self.thread_picker else {
            return Vec::new();
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return Vec::new();
        }
        let Some(selected) = picker.selected_thread().cloned() else {
            picker.message = Some("No thread is available to resume".to_owned());
            return Vec::new();
        };
        if matches!(&self.thread, ThreadState::Ready { id } if id == &selected.id)
            && active_id.as_deref() == Some(selected.id.as_str())
        {
            self.thread_picker = None;
            self.notice = Some("That thread is already active".to_owned());
            return Vec::new();
        }
        picker.phase = ThreadPickerPhase::Resuming {
            id: selected.id.clone(),
        };
        picker.message = Some(format!("Opening {}…", selected.title));
        vec![Effect::SwitchThread { id: selected.id }]
    }

    fn close_thread_picker(&mut self) {
        let busy = self.thread_picker.as_ref().is_some_and(|picker| {
            matches!(
                picker.phase,
                ThreadPickerPhase::Resuming { .. } | ThreadPickerPhase::Deleting { .. }
            )
        });
        if busy {
            if let Some(picker) = &mut self.thread_picker {
                picker.message = Some("Wait for the current thread operation to finish".to_owned());
            }
        } else {
            self.thread_picker = None;
        }
    }

    fn request_selected_thread_delete(&mut self) {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(picker) = &mut self.thread_picker else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return;
        }
        let Some(target) = picker.selected_thread().cloned() else {
            picker.message = Some("No thread is selected".to_owned());
            return;
        };
        if active_id.as_deref() == Some(target.id.as_str()) {
            picker.message = Some(
                "The active thread cannot be deleted. Switch threads or use /new first.".to_owned(),
            );
            return;
        }
        picker.message = None;
        picker.confirmation = Some(ThreadDeleteConfirmation::Selected { target });
    }

    fn request_clear_inactive_threads(&mut self) {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(picker) = &mut self.thread_picker else {
            return;
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) || picker.confirmation.is_some() {
            return;
        }
        let targets = picker
            .threads
            .iter()
            .filter(|thread| active_id.as_deref() != Some(thread.id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if targets.is_empty() {
            picker.message = Some("There are no inactive threads to delete".to_owned());
            return;
        }
        picker.message = None;
        picker.confirmation = Some(ThreadDeleteConfirmation::AllInactive { targets });
    }

    fn confirm_thread_delete(&mut self) -> Vec<Effect> {
        let active_id = self.active_saved_thread_id().map(str::to_owned);
        let Some(picker) = &mut self.thread_picker else {
            return Vec::new();
        };
        if !matches!(picker.phase, ThreadPickerPhase::Ready) {
            return Vec::new();
        }
        let Some(confirmation) = picker.confirmation.take() else {
            return Vec::new();
        };
        let targets = confirmation.targets();
        if targets
            .iter()
            .any(|target| active_id.as_deref() == Some(target.id.as_str()))
        {
            picker.message = Some(
                "Deletion cancelled because its scope included the active saved thread".to_owned(),
            );
            return Vec::new();
        }
        let ids = targets
            .into_iter()
            .map(|target| target.id)
            .collect::<Vec<_>>();
        picker.phase = ThreadPickerPhase::Deleting {
            requested: ids.len(),
        };
        picker.message = Some(format!("Deleting {} inactive thread(s)…", ids.len()));
        vec![Effect::DeleteThreads { ids }]
    }

    fn active_saved_thread_id(&self) -> Option<&str> {
        self.preferences
            .thread_id
            .as_deref()
            .or(match &self.thread {
                ThreadState::Ready { id } => Some(id.as_str()),
                _ => None,
            })
    }

    fn register_thread_scope(&mut self, thread_id: &str) {
        if let AuthState::SignedIn { scope: Some(scope) } = &self.auth {
            self.preferences
                .thread_account_scopes
                .insert(thread_id.to_owned(), scope.clone());
        }
    }

    fn validate_selection(&mut self) {
        let had_saved_selection =
            self.preferences.model_id.is_some() || self.preferences.reasoning_effort.is_some();
        let selected = self
            .selected_model
            .as_ref()
            .and_then(|id| self.models.iter().find(|model| &model.id == id))
            .cloned()
            .or_else(|| self.models.iter().find(|model| model.is_default).cloned())
            .or_else(|| self.models.first().cloned());
        let Some(model) = selected else {
            self.selected_model = None;
            self.selected_reasoning = None;
            return;
        };
        self.selected_model = Some(model.id.clone());
        if !self
            .selected_reasoning
            .as_ref()
            .is_some_and(|effort| model.supported_reasoning_efforts.contains(effort))
        {
            self.selected_reasoning = Some(model.default_reasoning_effort.clone());
            if had_saved_selection {
                self.notice = Some(
                    "saved model or reasoning was unavailable; using the server default".to_owned(),
                );
            }
        }
        self.sync_selection_preferences();
    }

    fn sync_selection_preferences(&mut self) {
        self.preferences.model_id = self.selected_model.clone();
        self.preferences.reasoning_effort = self.selected_reasoning.clone();
    }

    fn begin_login(&mut self, effect: Effect) -> Vec<Effect> {
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            self.notice = Some("app-server is not connected".to_owned());
        } else if matches!(self.auth, AuthState::SignedOut) {
            return vec![effect];
        } else if matches!(self.auth, AuthState::SigningIn { .. }) {
            self.notice =
                Some("sign-in is already in progress; use /logout to cancel it".to_owned());
        } else {
            self.notice = Some("logout before starting another login".to_owned());
        }
        Vec::new()
    }

    fn send_block_reason(&self) -> Option<String> {
        if !matches!(self.connection, ConnectionState::Ready { .. }) {
            return Some("app-server is not connected".to_owned());
        }
        if !matches!(self.auth, AuthState::SignedIn { .. }) {
            return Some("sign in with /login before sending".to_owned());
        }
        if self.models.is_empty() || self.selected_model.is_none() {
            return Some("model catalog is not ready".to_owned());
        }
        if self.turn.is_active() {
            return Some("wait for or interrupt the active turn".to_owned());
        }
        if self.thread_picker.is_some() {
            return Some("close the thread picker before sending".to_owned());
        }
        if matches!(self.thread, ThreadState::Resuming { .. }) {
            return Some("wait for thread resume to finish".to_owned());
        }
        if matches!(
            self.thread,
            ThreadState::ResumeFailed { .. } | ThreadState::AccountMismatch { .. }
        ) {
            return Some(
                "resolve the saved thread with /resume or the matching account".to_owned(),
            );
        }
        None
    }

    fn matches_active(&self, thread_id: &str, turn_id: &str) -> bool {
        matches!(&self.turn, TurnState::Streaming { thread_id: expected_thread, turn_id: expected_turn } if expected_thread == thread_id && expected_turn == turn_id)
    }

    fn append_delta(&mut self, turn_id: &str, item_id: &str, delta: &str) {
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            entry.text.push_str(delta);
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: delta.to_owned(),
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
        }
    }

    fn reconcile_final(
        &mut self,
        turn_id: &str,
        item_id: &str,
        final_text: &str,
    ) -> Result<(), String> {
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            if final_text.starts_with(&entry.text) {
                entry.text.push_str(&final_text[entry.text.len()..]);
                Ok(())
            } else {
                Err("assistant final snapshot contradicted streamed text".to_owned())
            }
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: final_text.to_owned(),
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, default: bool, efforts: &[&str], default_effort: &str) -> ModelChoice {
        ModelChoice {
            id: id.to_owned(),
            display_name: id.to_owned(),
            is_default: default,
            default_reasoning_effort: default_effort.to_owned(),
            supported_reasoning_efforts: efforts.iter().map(|value| (*value).to_owned()).collect(),
        }
    }

    fn thread(id: &str, title: &str, updated_at: i64) -> ThreadChoice {
        ThreadChoice {
            id: id.to_owned(),
            title: title.to_owned(),
            updated_at,
        }
    }

    fn seed_thinking(state: &mut AppState, text: &str) {
        state.thinking.visible = true;
        state.thinking.entries.push(ThinkingEntry {
            turn_id: "turn-old".to_owned(),
            item_id: "thinking-old".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            text: text.to_owned(),
            completed: true,
        });
    }

    fn thread_ready_state() -> AppState {
        let scope = AccountScope::from_chatgpt_email("user@example.com");
        AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedIn {
                scope: scope.clone(),
            },
            thread: ThreadState::Ready {
                id: "thr-active".to_owned(),
            },
            turn: TurnState::Completed {
                turn_id: "turn-old".to_owned(),
            },
            models: vec![model("m1", true, &["high"], "high")],
            selected_model: Some("m1".to_owned()),
            selected_reasoning: Some("high".to_owned()),
            transcript: vec![TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: "old conversation".to_owned(),
                item_id: None,
                turn_id: None,
            }],
            preferences: PreferencesV1 {
                account_scope: scope,
                thread_id: Some("thr-active".to_owned()),
                model_id: Some("m1".to_owned()),
                reasoning_effort: Some("high".to_owned()),
                thread_account_scopes: [
                    "thr-active",
                    "thr-old",
                    "thr-old-a",
                    "thr-old-b",
                    "thr-old-c",
                ]
                .into_iter()
                .map(|id| {
                    (
                        id.to_owned(),
                        AccountScope::from_chatgpt_email("user@example.com").unwrap(),
                    )
                })
                .collect(),
                ..PreferencesV1::default()
            },
            ..AppState::default()
        }
    }

    fn waiting_turn_state() -> AppState {
        AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedIn { scope: None },
            thread: ThreadState::Ready {
                id: "thr".to_owned(),
            },
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        }
    }

    #[test]
    fn signed_out_send_is_local_and_same_account_auto_resumes() {
        let mut state = AppState::default();
        state.reduce(Action::Event(DomainEvent::Connected { generation: 1 }));
        let effects = state.reduce(Action::Intent(Intent::SendMessage("hello".to_owned())));
        assert!(effects.is_empty());
        assert!(state.notice.as_deref().unwrap().contains("sign in"));
        let scope = AccountScope::from_chatgpt_email("a@example.com");
        state.reduce(Action::Event(DomainEvent::PreferencesLoaded(
            PreferencesV1 {
                account_scope: scope.clone(),
                thread_id: Some("thr-old".to_owned()),
                ..PreferencesV1::default()
            },
        )));
        assert_eq!(
            state.reduce(Action::Event(DomainEvent::AccountLoaded(scope))),
            vec![Effect::ResumeThread {
                id: "thr-old".to_owned()
            }]
        );
    }

    #[test]
    fn signing_in_logout_cancels_the_active_login() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SigningIn {
                login_id: "login-active".to_owned(),
            },
            ..AppState::default()
        };

        assert_eq!(
            state.reduce(Action::Intent(Intent::Logout)),
            vec![Effect::CancelLogin {
                login_id: "login-active".to_owned(),
            }]
        );
        assert!(matches!(state.auth, AuthState::SigningIn { .. }));
    }

    #[test]
    fn completed_login_replaces_the_pending_browser_notice() {
        let mut state = AppState {
            auth: AuthState::SigningIn {
                login_id: "login-active".to_owned(),
            },
            notice: Some(
                "Complete sign-in in the browser; if it fails, use /logout then /login device"
                    .to_owned(),
            ),
            ..AppState::default()
        };

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("user@example.com"),
        )));

        assert_eq!(
            state.auth,
            AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("user@example.com"),
            }
        );
        assert_eq!(state.notice.as_deref(), Some("Signed in to ChatGPT"));
    }

    #[test]
    fn account_switch_and_logout_replace_and_remove_the_runtime_identity() {
        let mut state = AppState::default();

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("first@example.com"),
        )));
        assert_eq!(
            state.auth,
            AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("first@example.com"),
            }
        );

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("second@example.com"),
        )));
        assert_eq!(
            state.auth,
            AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("second@example.com"),
            }
        );

        state.reduce(Action::Event(DomainEvent::LoggedOut));
        assert_eq!(state.auth, AuthState::SignedOut);
    }

    #[test]
    fn account_change_and_resume_failure_preserve_saved_id() {
        let mut state = AppState::default();
        state.preferences.thread_id = Some("thr-old".to_owned());
        state.preferences.account_scope = AccountScope::from_chatgpt_email("old@example.com");
        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("new@example.com"),
        )));
        assert!(matches!(state.thread, ThreadState::AccountMismatch { .. }));
        state.reduce(Action::Event(DomainEvent::ResumeFailed {
            id: "thr-old".to_owned(),
            message: "stale".to_owned(),
        }));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-old"));
    }

    #[test]
    fn thread_picker_requires_account_identity_and_can_safely_replace_a_mismatched_active_thread() {
        let saved_scope = AccountScope::from_chatgpt_email("saved@example.com");
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("other@example.com"),
            },
            thread: ThreadState::AccountMismatch {
                id: "thr-saved".to_owned(),
            },
            preferences: PreferencesV1 {
                account_scope: saved_scope.clone(),
                thread_id: Some("thr-saved".to_owned()),
                ..PreferencesV1::default()
            },
            models: vec![model("m1", true, &["high"], "high")],
            selected_model: Some("m1".to_owned()),
            ..AppState::default()
        };

        assert_eq!(
            state.reduce(Action::Intent(Intent::Resume)),
            vec![Effect::ListThreads]
        );
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-saved"));
        assert!(matches!(
            state.thread_picker.as_ref().map(|picker| &picker.phase),
            Some(ThreadPickerPhase::Loading)
        ));
        state.reduce(Action::Intent(Intent::ThreadPickerClose));

        state.auth = AuthState::SignedIn { scope: None };
        assert!(state.reduce(Action::Intent(Intent::Resume)).is_empty());
        assert!(state.notice.as_deref().unwrap().contains("identity"));

        state.auth = AuthState::SignedIn { scope: saved_scope };
        state.thread = ThreadState::ResumeFailed {
            id: "thr-saved".to_owned(),
            message: "temporary failure".to_owned(),
        };
        assert_eq!(
            state.reduce(Action::Intent(Intent::Resume)),
            vec![Effect::ListThreads]
        );
        assert!(matches!(
            state.thread_picker.as_ref().map(|picker| &picker.phase),
            Some(ThreadPickerPhase::Loading)
        ));
    }

    #[test]
    fn catalog_falls_back_and_model_change_revalidates_reasoning() {
        let mut state = AppState {
            selected_model: Some("missing".to_owned()),
            selected_reasoning: Some("max".to_owned()),
            ..AppState::default()
        };
        state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![
            model("m1", true, &["low", "high"], "high"),
            model("m2", false, &["low"], "low"),
        ])));
        assert_eq!(state.selected_model.as_deref(), Some("m1"));
        assert_eq!(state.selected_reasoning.as_deref(), Some("high"));
        state.reduce(Action::Intent(Intent::SelectModel("m2".to_owned())));
        assert_eq!(state.selected_reasoning.as_deref(), Some("low"));
    }

    #[test]
    fn new_thread_is_eager_and_only_replaces_state_after_success() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "old reasoning");
        assert_eq!(
            state.reduce(Action::Intent(Intent::NewThread)),
            vec![Effect::StartNewThread]
        );
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(state.transcript[0].text, "old conversation");
        assert_eq!(state.thinking.entries[0].text, "old reasoning");

        state.reduce(Action::Event(DomainEvent::NewThreadFailed(
            "server rejected it".to_owned(),
        )));
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-active"));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(state.transcript.len(), 1);
        assert_eq!(state.thinking.entries[0].text, "old reasoning");

        let effects = state.reduce(Action::Event(DomainEvent::NewThreadSucceeded {
            id: "thr-new".to_owned(),
        }));
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-new"));
        assert!(state.transcript.is_empty());
        assert!(state.thinking.entries.is_empty());
        assert!(state.thinking.visible);
        assert!(matches!(state.turn, TurnState::Idle));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-new"));
        assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    }

    #[test]
    fn picker_navigation_and_failed_switch_preserve_the_active_thread() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "active thread reasoning");
        assert_eq!(
            state.reduce(Action::Intent(Intent::Resume)),
            vec![Effect::ListThreads]
        );
        state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
            thread("thr-active", "Current", 30),
            thread("thr-old", "Older", 20),
        ])));
        state.reduce(Action::Intent(Intent::ThreadPickerMoveDown));
        assert_eq!(state.thread_picker.as_ref().unwrap().selected, 1);
        assert_eq!(
            state.reduce(Action::Intent(Intent::ThreadPickerSelect)),
            vec![Effect::SwitchThread {
                id: "thr-old".to_owned()
            }]
        );
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-active"));

        state.reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
            id: "thr-old".to_owned(),
            message: "malformed history".to_owned(),
        }));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(state.transcript[0].text, "old conversation");
        assert_eq!(state.thinking.entries[0].text, "active thread reasoning");
        assert!(matches!(
            state.thread_picker.as_ref().map(|picker| &picker.phase),
            Some(ThreadPickerPhase::Ready)
        ));

        assert_eq!(
            state.reduce(Action::Intent(Intent::ThreadPickerSelect)),
            vec![Effect::SwitchThread {
                id: "thr-old".to_owned()
            }]
        );
        let history = vec![TranscriptEntry {
            role: TranscriptRole::User,
            text: "restored".to_owned(),
            item_id: None,
            turn_id: None,
        }];
        let effects = state.reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
            id: "thr-old".to_owned(),
            history: history.clone(),
        }));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-old"));
        assert_eq!(state.transcript, history);
        assert!(state.thinking.entries.is_empty());
        assert!(state.thinking.visible);
        assert!(state.thread_picker.is_none());
        assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    }

    #[test]
    fn automatic_resume_preserves_thinking_on_failure_and_clears_it_on_success() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "current thread reasoning");

        state.reduce(Action::Event(DomainEvent::ResumeStarted {
            id: "thr-old".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::ResumeFailed {
            id: "thr-old".to_owned(),
            message: "temporary failure".to_owned(),
        }));
        assert_eq!(state.thinking.entries[0].text, "current thread reasoning");

        state.reduce(Action::Event(DomainEvent::ResumeSucceeded {
            id: "thr-old".to_owned(),
            history: Vec::new(),
        }));
        assert!(state.thinking.entries.is_empty());
        assert!(state.thinking.visible);
    }

    #[test]
    fn picker_only_exposes_threads_registered_to_the_current_account() {
        let mut state = thread_ready_state();
        state.preferences.thread_account_scopes.insert(
            "thr-foreign".to_owned(),
            AccountScope::from_chatgpt_email("other@example.com").unwrap(),
        );
        state.thread_picker = Some(ThreadPickerState::loading());
        state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
            thread("thr-active", "Current", 30),
            thread("thr-old", "Same account", 20),
            thread("thr-foreign", "Other account", 10),
            thread("thr-unknown", "Unknown account", 5),
        ])));
        assert_eq!(
            state
                .thread_picker
                .as_ref()
                .unwrap()
                .threads
                .iter()
                .map(|thread| thread.id.as_str())
                .collect::<Vec<_>>(),
            vec!["thr-active", "thr-old"]
        );

        let scope = AccountScope::from_chatgpt_email("legacy@example.com").unwrap();
        let mut legacy = AppState::default();
        legacy.reduce(Action::Event(DomainEvent::PreferencesLoaded(
            PreferencesV1 {
                account_scope: Some(scope.clone()),
                thread_id: Some("thr-legacy".to_owned()),
                ..PreferencesV1::default()
            },
        )));
        assert_eq!(
            legacy.preferences.thread_account_scopes.get("thr-legacy"),
            Some(&scope)
        );
    }

    #[test]
    fn deletion_confirmation_protects_active_and_supports_cancellation() {
        let mut state = thread_ready_state();
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![
                thread("thr-active", "Current", 30),
                thread("thr-old", "Older", 20),
            ],
            selected: 0,
            confirmation: None,
            message: None,
        });
        assert!(state
            .reduce(Action::Intent(Intent::ThreadPickerRequestDelete))
            .is_empty());
        assert!(state.thread_picker.as_ref().unwrap().confirmation.is_none());
        assert!(state
            .thread_picker
            .as_ref()
            .unwrap()
            .message
            .as_deref()
            .unwrap()
            .contains("active"));

        state.reduce(Action::Intent(Intent::ThreadPickerMoveDown));
        state.reduce(Action::Intent(Intent::ThreadPickerRequestDelete));
        assert!(matches!(
            state
                .thread_picker
                .as_ref()
                .and_then(|picker| picker.confirmation.as_ref()),
            Some(ThreadDeleteConfirmation::Selected { target }) if target.id == "thr-old"
        ));
        assert!(state
            .reduce(Action::Intent(Intent::ThreadPickerCancelDelete))
            .is_empty());
        assert!(state.thread_picker.as_ref().unwrap().confirmation.is_none());

        state.reduce(Action::Intent(Intent::ThreadPickerRequestDelete));
        assert_eq!(
            state.reduce(Action::Intent(Intent::ThreadPickerConfirmDelete)),
            vec![Effect::DeleteThreads {
                ids: vec!["thr-old".to_owned()]
            }]
        );
        state.reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
            requested: 1,
            deleted: vec!["thr-old".to_owned()],
            failures: vec![],
        }));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(state.thread_picker.as_ref().unwrap().threads.len(), 1);
    }

    #[test]
    fn clear_inactive_reports_partial_failures_and_never_removes_active_saved_id() {
        let mut state = thread_ready_state();
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![
                thread("thr-active", "Current", 30),
                thread("thr-old-a", "Old A", 20),
                thread("thr-old-b", "Old B", 10),
            ],
            selected: 0,
            confirmation: None,
            message: None,
        });
        state.reduce(Action::Intent(Intent::ThreadPickerRequestClearInactive));
        let targets = state
            .thread_picker
            .as_ref()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .targets();
        assert_eq!(
            targets
                .iter()
                .map(|target| target.id.as_str())
                .collect::<Vec<_>>(),
            vec!["thr-old-a", "thr-old-b"]
        );
        assert_eq!(
            state.reduce(Action::Intent(Intent::ThreadPickerConfirmDelete)),
            vec![Effect::DeleteThreads {
                ids: vec!["thr-old-a".to_owned(), "thr-old-b".to_owned()]
            }]
        );
        state.reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
            requested: 2,
            deleted: vec!["thr-active".to_owned(), "thr-old-a".to_owned()],
            failures: vec![ThreadDeletionFailure {
                id: "thr-old-b".to_owned(),
                message: "permission denied".to_owned(),
            }],
        }));
        let picker = state.thread_picker.as_ref().unwrap();
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(
            picker
                .threads
                .iter()
                .map(|thread| thread.id.as_str())
                .collect::<Vec<_>>(),
            vec!["thr-active", "thr-old-b"]
        );
        assert!(picker.message.as_deref().unwrap().contains("failed"));
        assert!(picker
            .message
            .as_deref()
            .unwrap()
            .contains("active saved thread"));
    }

    #[test]
    fn first_catalog_default_does_not_claim_a_saved_selection_failed() {
        let mut state = AppState::default();
        state.reduce(Action::Event(DomainEvent::CatalogLoaded(vec![model(
            "m1",
            true,
            &["low"],
            "low",
        )])));
        assert_eq!(state.selected_model.as_deref(), Some("m1"));
        assert_eq!(state.notice, None);
    }

    #[test]
    fn assistant_activity_starts_with_the_turn_and_stops_on_first_nonempty_text() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedIn { scope: None },
            thread: ThreadState::Ready {
                id: "thr".to_owned(),
            },
            models: vec![model("m1", true, &["high"], "high")],
            selected_model: Some("m1".to_owned()),
            selected_reasoning: Some("high".to_owned()),
            ..AppState::default()
        };
        let preferences = state.preferences.clone();

        assert_eq!(
            state.reduce(Action::Intent(Intent::SendMessage("hello".to_owned()))),
            vec![Effect::SendMessage {
                text: "hello".to_owned(),
            }]
        );
        assert!(state.is_waiting_for_assistant_text());
        assert_eq!(state.transcript.len(), 1);
        assert_eq!(state.transcript[0].role, TranscriptRole::User);

        state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
        }));
        assert!(state.is_waiting_for_assistant_text());

        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "other".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "stale".to_owned(),
            delta: "ignore me".to_owned(),
        }));
        assert!(state.is_waiting_for_assistant_text());

        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: String::new(),
        }));
        assert!(state.is_waiting_for_assistant_text());

        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: "reply".to_owned(),
        }));
        assert!(!state.is_waiting_for_assistant_text());
        assert_eq!(state.transcript.last().unwrap().text, "reply");
        assert_eq!(state.preferences, preferences);
        assert!(state
            .transcript
            .iter()
            .all(|entry| !entry.text.contains('~')));
    }

    #[test]
    fn assistant_activity_stops_on_all_turn_terminal_states() {
        for outcome in [
            TurnOutcome::Completed,
            TurnOutcome::Interrupted,
            TurnOutcome::Failed("model failed".to_owned()),
        ] {
            let mut state = waiting_turn_state();
            assert!(state.is_waiting_for_assistant_text());
            state.reduce(Action::Event(DomainEvent::TurnFinished {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
                outcome,
            }));
            assert!(!state.is_waiting_for_assistant_text());
        }

        let mut operation_failed = waiting_turn_state();
        operation_failed.reduce(Action::Event(DomainEvent::TurnOperationFailed(
            "request failed".to_owned(),
        )));
        assert!(!operation_failed.is_waiting_for_assistant_text());

        for event in [
            DomainEvent::ConnectionFailed("disconnected".to_owned()),
            DomainEvent::ProcessExited("exited".to_owned()),
            DomainEvent::SafetyViolation("tool/request".to_owned()),
        ] {
            let mut state = waiting_turn_state();
            state.reduce(Action::Event(event));
            assert!(!state.is_waiting_for_assistant_text());
        }
    }

    #[test]
    fn assistant_activity_stops_on_account_thread_and_shutdown_transitions() {
        let mut logged_out = waiting_turn_state();
        logged_out.reduce(Action::Event(DomainEvent::LoggedOut));
        assert!(!logged_out.is_waiting_for_assistant_text());

        let mut unsupported_account = waiting_turn_state();
        unsupported_account.reduce(Action::Event(DomainEvent::UnsupportedAccount(
            "api key".to_owned(),
        )));
        assert!(!unsupported_account.is_waiting_for_assistant_text());

        let mut resuming = waiting_turn_state();
        resuming.reduce(Action::Event(DomainEvent::ResumeStarted {
            id: "other-thread".to_owned(),
        }));
        assert!(!resuming.is_waiting_for_assistant_text());

        let mut changed_thread = waiting_turn_state();
        changed_thread.reduce(Action::Event(DomainEvent::ThreadStarted {
            id: "other-thread".to_owned(),
        }));
        assert!(!changed_thread.is_waiting_for_assistant_text());

        let mut shutting_down = waiting_turn_state();
        shutting_down.reduce(Action::Intent(Intent::Quit));
        assert!(!shutting_down.is_waiting_for_assistant_text());

        let mut first_thread = waiting_turn_state();
        first_thread.thread = ThreadState::None;
        first_thread.turn = TurnState::Starting;
        assert!(first_thread.is_waiting_for_assistant_text());
        first_thread.reduce(Action::Event(DomainEvent::ThreadStarted {
            id: "thr-new".to_owned(),
        }));
        assert!(first_thread.is_waiting_for_assistant_text());
    }

    #[test]
    fn scoped_streaming_reconciles_utf8_suffix_without_duplication() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "other".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: "ignored".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: "hé".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::AgentCompleted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            text: "héllo".to_owned(),
        }));
        assert_eq!(state.transcript[0].text, "héllo");
        state.reduce(Action::Event(DomainEvent::TurnFinished {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            outcome: TurnOutcome::Interrupted,
        }));
        assert!(matches!(state.turn, TurnState::Interrupted { .. }));
    }

    #[test]
    fn unrelated_turn_started_cannot_replace_the_active_turn() {
        let mut state = AppState {
            thread: ThreadState::Ready {
                id: "thr-active".to_owned(),
            },
            turn: TurnState::Streaming {
                thread_id: "thr-active".to_owned(),
                turn_id: "turn-active".to_owned(),
            },
            ..AppState::default()
        };
        state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id: "thr-other".to_owned(),
            turn_id: "turn-other".to_owned(),
        }));
        assert_eq!(
            state.turn,
            TurnState::Streaming {
                thread_id: "thr-active".to_owned(),
                turn_id: "turn-active".to_owned(),
            }
        );
    }

    #[test]
    fn contradictory_final_snapshot_fails_the_turn() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        state.append_delta("turn", "item", "alpha");
        state.reduce(Action::Event(DomainEvent::AgentCompleted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            text: "beta".to_owned(),
        }));
        assert!(matches!(state.turn, TurnState::Failed { .. }));
    }

    #[test]
    fn thinking_toggle_stream_scope_and_completion_are_deterministic() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        assert!(!state.thinking.visible);
        assert!(state
            .reduce(Action::Intent(Intent::ToggleThinking))
            .is_empty());
        assert!(state.thinking.visible);

        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "stale".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            delta: "ignore".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr".to_owned(),
            turn_id: "old-turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            delta: "ignore".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::Summary,
            index: -1,
            delta: "ignore".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingSummaryPartAdded {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            summary_index: 0,
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            delta: "check\u{1b}[31m\ting".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::EmittedText,
            index: 0,
            delta: "detail".to_owned(),
        }));
        assert_eq!(state.thinking.entries.len(), 2);
        assert_eq!(state.thinking.entries[0].text, "check[31m    ing");
        assert!(!state.thinking.entries[0].text.contains('\u{1b}'));

        state.reduce(Action::Event(DomainEvent::ThinkingCompleted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            summary: vec!["checking facts".to_owned()],
            content: vec!["detail complete".to_owned()],
        }));
        assert_eq!(state.thinking.entries[0].text, "checking facts");
        assert_eq!(state.thinking.entries[1].text, "detail complete");
        assert!(state.thinking.entries.iter().all(|entry| entry.completed));

        state.reduce(Action::Event(DomainEvent::TurnFinished {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            outcome: TurnOutcome::Completed,
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            delta: " stale suffix".to_owned(),
        }));
        assert_eq!(state.thinking.entries[0].text, "checking facts");
        state.reduce(Action::Intent(Intent::ToggleThinking));
        assert!(!state.thinking.visible);
    }

    #[test]
    fn thinking_retention_is_bounded_and_new_turn_clears_only_content() {
        let mut state = AppState {
            connection: ConnectionState::Ready { generation: 1 },
            auth: AuthState::SignedIn { scope: None },
            thread: ThreadState::Ready {
                id: "thr".to_owned(),
            },
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            models: vec![model("m", true, &["high"], "high")],
            selected_model: Some("m".to_owned()),
            selected_reasoning: Some("high".to_owned()),
            ..AppState::default()
        };
        state.thinking.visible = true;
        let oversized = format!(
            "discard\u{0007}{}tail",
            "界".repeat(MAX_THINKING_CHARS + 20)
        );
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "why".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            delta: oversized,
        }));
        let retained = &state.thinking.entries[0].text;
        assert_eq!(retained.chars().count(), MAX_THINKING_CHARS);
        assert!(retained.ends_with("tail"));
        assert!(!retained.contains('\u{0007}'));

        state.turn = TurnState::Completed {
            turn_id: "turn".to_owned(),
        };
        assert_eq!(
            state.reduce(Action::Intent(Intent::SendMessage("next".to_owned()))),
            vec![Effect::SendMessage {
                text: "next".to_owned()
            }]
        );
        assert!(state.thinking.entries.is_empty());
        assert!(state.thinking.visible);
    }

    #[test]
    fn quitting_interrupts_before_the_ordered_shutdown_path() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        let effects = state.reduce(Action::Intent(Intent::Quit));
        assert!(matches!(
            effects.as_slice(),
            [Effect::InterruptTurn { .. }, Effect::Shutdown]
        ));
        assert!(state.shutting_down);
    }
}
