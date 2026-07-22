use crate::command::HELP_TEXT;
use crate::persistence::{AccountScope, PreferencesV1};
use crate::text::sanitize_terminal_text;
use std::collections::{BTreeMap, BTreeSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const MAX_THINKING_BYTES: usize = 32 * 1024;
const MAX_THINKING_ENTRIES: usize = 128;
const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;
const MAX_TRANSCRIPT_ENTRIES: usize = 2048;
const MAX_TRANSCRIPT_NEWLINES: usize = 16 * 1024;
const MAX_TRANSCRIPT_DISPLAY_COLUMNS: usize = 512 * 1024;
// The interactive composer uses a tighter 128 KiB responsiveness bound. This reducer-level cap
// also covers programmatic intents; after sanitization, JSON escaping can at most double retained
// bytes, leaving ample envelope headroom under the transport's 1 MiB frame limit.
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
// This bounded non-cryptographic fingerprint detects accidental stream/snapshot contradictions;
// it is not used as an authenticity or security primitive.
const TRANSCRIPT_HASH_OFFSET: u64 = 0xcbf29ce484222325;
const TRANSCRIPT_HASH_PRIME: u64 = 0x100000001b3;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptTruncation {
    pub dropped_bytes: usize,
    pub dropped_hash: u64,
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
            .map(|entry| entry.text.len())
            .fold(0usize, usize::saturating_add);
        let mut excess = total.saturating_sub(MAX_THINKING_BYTES);
        for entry in &mut self.entries {
            if excess == 0 {
                break;
            }
            let available = entry.text.len();
            let remove = available.min(excess);
            let removed = trim_utf8_bytes_from_front(&mut entry.text, remove);
            excess = excess.saturating_sub(removed);
        }
    }
}

fn trim_utf8_bytes_from_front(value: &mut String, minimum_bytes: usize) -> usize {
    if minimum_bytes == 0 {
        return 0;
    }
    if minimum_bytes >= value.len() {
        let removed = value.len();
        *value = String::new();
        return removed;
    }
    let mut byte_index = minimum_bytes;
    while !value.is_char_boundary(byte_index) {
        byte_index += 1;
    }
    *value = value[byte_index..].to_owned();
    byte_index
}

fn extend_transcript_hash(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(TRANSCRIPT_HASH_PRIME);
    }
    hash
}

fn trim_bytes_from_front(
    value: &mut String,
    minimum_bytes: usize,
    prefix_hash: u64,
) -> (usize, u64) {
    if minimum_bytes == 0 {
        return (0, prefix_hash);
    }
    if minimum_bytes >= value.len() {
        let removed = value.len();
        let hash = extend_transcript_hash(prefix_hash, value.as_bytes());
        *value = String::new();
        return (removed, hash);
    }
    let mut byte_index = minimum_bytes;
    while !value.is_char_boundary(byte_index) {
        byte_index += 1;
    }
    let hash = extend_transcript_hash(prefix_hash, &value.as_bytes()[..byte_index]);
    *value = value[byte_index..].to_owned();
    (byte_index, hash)
}

fn prefix_bytes_for_newlines(entries: &[TranscriptEntry], mut newlines: usize) -> usize {
    if newlines == 0 {
        return 0;
    }
    let mut bytes = 0usize;
    for entry in entries {
        for (index, byte) in entry.text.bytes().enumerate() {
            if byte == b'\n' {
                newlines -= 1;
                if newlines == 0 {
                    return bytes.saturating_add(index + 1);
                }
            }
        }
        bytes = bytes.saturating_add(entry.text.len());
    }
    bytes
}

fn prefix_bytes_for_display_width(entries: &[TranscriptEntry], mut width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let mut bytes = 0usize;
    for entry in entries {
        for (index, grapheme) in entry.text.grapheme_indices(true) {
            width = width.saturating_sub(UnicodeWidthStr::width(grapheme));
            if width == 0 {
                return bytes.saturating_add(index + grapheme.len());
            }
        }
        bytes = bytes.saturating_add(entry.text.len());
    }
    bytes
}

fn transcript_item_key(entry: &TranscriptEntry) -> Option<(String, String)> {
    if entry.role != TranscriptRole::Assistant {
        return None;
    }
    Some((entry.turn_id.clone()?, entry.item_id.clone()?))
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
    TokenUsageUpdated {
        thread_id: String,
        turn_id: String,
        context_tokens: i64,
        model_context_window: Option<i64>,
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

    pub fn reduce(&mut self, action: Action) -> Vec<Effect> {
        if self.shutting_down {
            return Vec::new();
        }
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
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
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
                let model_changed = self.selected_model.as_deref() != Some(model.id.as_str());
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
                if model_changed {
                    self.invalidate_context_for_current_turn();
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
                    let AuthState::SignedIn { scope } = &self.auth else {
                        unreachable!("thread_action_block_reason requires signed-in auth")
                    };
                    self.pending_new_thread_scope = Some(scope.clone());
                    self.notice = Some("Starting a new thread…".to_owned());
                    return vec![Effect::StartNewThread];
                }
            }
            Intent::Resume => {
                if let Some(reason) = self.thread_action_block_reason(true) {
                    self.notice = Some(reason);
                } else {
                    self.pending_thread_deletions = None;
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
                let text = sanitize_terminal_text(&text);
                if text.trim().is_empty() {
                    self.notice = Some("enter a message or /help".to_owned());
                    return Vec::new();
                }
                if text.len() > MAX_MESSAGE_BYTES {
                    self.notice = Some(format!(
                        "message is too large; keep it under {} KiB",
                        MAX_MESSAGE_BYTES / 1024
                    ));
                    return Vec::new();
                }
                if let Some(reason) = self.send_block_reason() {
                    self.notice = Some(reason);
                } else {
                    self.turn = TurnState::Starting;
                    self.thinking.clear_content();
                    self.transcript_dropped_prefix_bytes.clear();
                    self.transcript.push(TranscriptEntry {
                        role: TranscriptRole::User,
                        text: text.clone(),
                        item_id: None,
                        turn_id: None,
                    });
                    self.enforce_transcript_bound();
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
                self.reset_context_window();
            }
            DomainEvent::Connecting => {
                self.connection = ConnectionState::Connecting;
                self.reset_context_window();
            }
            DomainEvent::Connected { generation } => {
                self.connection = ConnectionState::Ready { generation }
            }
            DomainEvent::ConnectionFailed(message) | DomainEvent::ProcessExited(message) => {
                self.connection = ConnectionState::Failed(message.clone());
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
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
                let completed_login = matches!(self.auth, AuthState::SigningIn { .. });
                let picker_was_open = self.thread_picker.is_some();
                let same_account = matches!(
                    &self.auth,
                    AuthState::SignedIn { scope: current } if current == &scope
                );
                let switched_accounts = matches!(
                    &self.auth,
                    AuthState::SignedIn { scope: current } if current != &scope
                );
                if !same_account {
                    self.reset_context_window();
                    self.thinking.clear_content();
                }
                if self.pending_new_thread_scope.as_ref() != Some(&scope) {
                    self.pending_new_thread_scope = None;
                }
                if switched_accounts {
                    // An account update can arrive while a turn is active. Detach the old turn
                    // before changing the displayed identity so every subsequently queued event
                    // from that turn becomes stale.
                    self.turn = TurnState::Idle;
                    self.thread_picker = None;
                    self.pending_thread_deletions = None;
                }
                self.auth = AuthState::SignedIn {
                    scope: scope.clone(),
                };
                if completed_login {
                    self.notice = Some("Signed in to ChatGPT".to_owned());
                }
                if let Some(id) = self.preferences.thread_id.clone() {
                    if scope.is_some() && scope == self.preferences.account_scope {
                        // Account refresh notifications are not lifecycle requests. Only attach
                        // the saved thread when startup or login left us without a thread; an
                        // already-ready or in-flight resume must remain untouched.
                        if !picker_was_open && matches!(self.thread, ThreadState::None) {
                            self.thread = ThreadState::Resuming { id: id.clone() };
                            return vec![Effect::ResumeThread { id }];
                        }
                        return Vec::new();
                    }
                    self.thread_picker = None;
                    self.thread = ThreadState::AccountMismatch { id };
                    self.notice =
                        Some("saved thread belongs to a different or unscoped account".to_owned());
                } else if switched_accounts {
                    self.thread = ThreadState::None;
                }
            }
            DomainEvent::UnsupportedAccount(message) => {
                self.auth = AuthState::Unsupported(message);
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.turn = TurnState::Idle;
                self.thread_picker = None;
                self.thinking.clear_content();
                self.reset_context_window();
                self.thread = self
                    .preferences
                    .thread_id
                    .clone()
                    .map_or(ThreadState::None, |id| ThreadState::AccountMismatch { id });
            }
            DomainEvent::LoginStarted { login_id } => self.auth = AuthState::SigningIn { login_id },
            DomainEvent::LoginFailed(message) => {
                self.auth = AuthState::SignedOut;
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.notice = Some(message);
                self.reset_context_window();
            }
            DomainEvent::LoggedOut => {
                self.auth = AuthState::SignedOut;
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                self.thread = ThreadState::None;
                self.turn = TurnState::Idle;
                self.thread_picker = None;
                self.thinking.clear_content();
                self.reset_context_window();
            }
            DomainEvent::CatalogLoaded(models) => {
                let previous_model = self.selected_model.clone();
                self.models = models;
                self.validate_selection();
                if previous_model != self.selected_model {
                    self.invalidate_context_for_current_turn();
                }
            }
            DomainEvent::ResumeStarted { id } => {
                // Account loading already moves the reducer into `Resuming` before it emits the
                // backend effect. Treat this echo as acknowledgement only, so a delayed event can
                // never detach a newer active thread.
                if !matches!(&self.thread, ThreadState::Resuming { id: expected } if expected == &id)
                {
                    return Vec::new();
                }
            }
            DomainEvent::ResumeSucceeded { id, history } => {
                if !matches!(&self.thread, ThreadState::Resuming { id: expected } if expected == &id)
                {
                    return Vec::new();
                }
                self.reset_context_window();
                self.thread = ThreadState::Ready { id: id.clone() };
                self.turn = TurnState::Idle;
                self.replace_transcript(history);
                self.thinking.clear_content();
                self.preferences.thread_id = Some(id.clone());
                self.register_thread_scope(&id);
                return vec![Effect::Persist(self.preferences.clone())];
            }
            DomainEvent::ResumeFailed { id, message } => {
                if !matches!(&self.thread, ThreadState::Resuming { id: expected } if expected == &id)
                {
                    return Vec::new();
                }
                self.thread = ThreadState::ResumeFailed {
                    id,
                    message: message.clone(),
                };
                self.notice = Some(message);
            }
            DomainEvent::NewThreadSucceeded { id } => {
                let Some(requested_scope) = self.pending_new_thread_scope.take() else {
                    return Vec::new();
                };
                if !matches!(&self.auth, AuthState::SignedIn { scope } if scope == &requested_scope)
                {
                    return Vec::new();
                }
                self.reset_context_window();
                self.thread = ThreadState::Ready { id: id.clone() };
                self.turn = TurnState::Idle;
                self.clear_transcript();
                self.thread_picker = None;
                self.pending_thread_deletions = None;
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
                if self.pending_new_thread_scope.take().is_none() {
                    return Vec::new();
                }
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
                    self.reset_context_window();
                    self.thread = ThreadState::Ready { id: id.clone() };
                    self.turn = TurnState::Idle;
                    self.replace_transcript(history);
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
                let Some(expected_ids) = self.pending_thread_deletions.take() else {
                    return Vec::new();
                };
                let phase_request_count =
                    match self.thread_picker.as_ref().map(|picker| &picker.phase) {
                        Some(ThreadPickerPhase::Deleting { requested }) => *requested,
                        _ => return Vec::new(),
                    };
                let mut reported_ids = BTreeSet::new();
                let no_duplicate_results = deleted
                    .iter()
                    .chain(failures.iter().map(|failure| &failure.id))
                    .all(|id| reported_ids.insert(id.clone()));
                let result_count_matches = deleted
                    .len()
                    .checked_add(failures.len())
                    .is_some_and(|count| count == requested);
                let result_matches_request = phase_request_count == requested
                    && requested == expected_ids.len()
                    && result_count_matches
                    && no_duplicate_results
                    && reported_ids == expected_ids;
                if !result_matches_request {
                    if let Some(picker) = &mut self.thread_picker {
                        picker.phase = ThreadPickerPhase::Failed;
                        picker.confirmation = None;
                        picker.message = Some(
                            "Thread deletion result did not match the requested scope".to_owned(),
                        );
                    }
                    return Vec::new();
                }

                let active_id = self.active_saved_thread_id().map(str::to_owned);
                if let Some(picker) = &mut self.thread_picker {
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
                if !matches!(self.thread, ThreadState::None)
                    || !matches!(self.turn, TurnState::Starting)
                {
                    return Vec::new();
                }
                self.reset_context_window();
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
                    let is_suppressed_turn = self.context_suppressed_turn.as_ref().is_some_and(
                        |(suppressed_thread, suppressed_turn)| {
                            suppressed_thread == &thread_id && suppressed_turn == &turn_id
                        },
                    );
                    if !is_suppressed_turn {
                        self.context_suppressed_turn = None;
                    }
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
            DomainEvent::TokenUsageUpdated {
                thread_id,
                turn_id,
                context_tokens,
                model_context_window,
            } => {
                let suppressed = self.context_suppressed_turn.as_ref().is_some_and(
                    |(suppressed_thread, suppressed_turn)| {
                        suppressed_thread == &thread_id && suppressed_turn == &turn_id
                    },
                );
                if !suppressed && self.matches_relevant_turn(&thread_id, &turn_id) {
                    self.context_remaining_percent =
                        remaining_context_percent(context_tokens, model_context_window);
                }
            }
            DomainEvent::TurnOperationFailed(message) => {
                if !self.turn.is_active() {
                    return Vec::new();
                }
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
                self.pending_new_thread_scope = None;
                self.pending_thread_deletions = None;
                if let Some(picker) = &mut self.thread_picker {
                    picker.phase = ThreadPickerPhase::Failed;
                    picker.confirmation = None;
                    picker.message = Some("Unexpected server request was denied".to_owned());
                }
                if self.turn.is_active() {
                    let turn_id = match &self.turn {
                        TurnState::Streaming { turn_id, .. } => Some(turn_id.clone()),
                        _ => None,
                    };
                    self.turn = TurnState::Failed {
                        turn_id,
                        message: "unexpected server request was denied".to_owned(),
                    };
                }
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
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
        }
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
        let expected_ids = ids.iter().cloned().collect::<BTreeSet<_>>();
        if expected_ids.len() != ids.len() {
            picker.phase = ThreadPickerPhase::Failed;
            picker.message =
                Some("Deletion cancelled because the thread list was invalid".to_owned());
            return Vec::new();
        }
        self.pending_thread_deletions = Some(expected_ids);
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
        if self.pending_new_thread_scope.is_some() {
            return Some("wait for the new thread request to finish".to_owned());
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

    fn matches_relevant_turn(&self, thread_id: &str, turn_id: &str) -> bool {
        let thread_matches = matches!(&self.thread, ThreadState::Ready { id } if id == thread_id);
        thread_matches
            && match &self.turn {
                TurnState::Streaming {
                    thread_id: active_thread,
                    turn_id: active_turn,
                } => active_thread == thread_id && active_turn == turn_id,
                TurnState::Completed {
                    turn_id: active_turn,
                }
                | TurnState::Interrupted {
                    turn_id: active_turn,
                } => active_turn == turn_id,
                TurnState::Failed {
                    turn_id: Some(active_turn),
                    ..
                } => active_turn == turn_id,
                TurnState::Idle | TurnState::Starting | TurnState::Failed { turn_id: None, .. } => {
                    false
                }
            }
    }

    fn reset_context_window(&mut self) {
        self.context_remaining_percent = None;
        self.context_suppressed_turn = None;
    }

    fn invalidate_context_for_current_turn(&mut self) {
        self.context_remaining_percent = None;
        self.context_suppressed_turn = self.current_turn_key();
    }

    fn current_turn_key(&self) -> Option<(String, String)> {
        let ThreadState::Ready { id: thread_id } = &self.thread else {
            return None;
        };
        let turn_id = match &self.turn {
            TurnState::Streaming { turn_id, .. }
            | TurnState::Completed { turn_id }
            | TurnState::Interrupted { turn_id }
            | TurnState::Failed {
                turn_id: Some(turn_id),
                ..
            } => turn_id,
            TurnState::Idle | TurnState::Starting | TurnState::Failed { turn_id: None, .. } => {
                return None;
            }
        };
        Some((thread_id.clone(), turn_id.clone()))
    }

    fn replace_transcript(&mut self, history: Vec<TranscriptEntry>) {
        self.transcript = history;
        for entry in &mut self.transcript {
            entry.text = sanitize_terminal_text(&entry.text);
        }
        self.transcript_dropped_prefix_bytes.clear();
        self.enforce_transcript_bound();
    }

    fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.transcript.shrink_to_fit();
        self.transcript_dropped_prefix_bytes.clear();
    }

    fn enforce_transcript_bound(&mut self) {
        if self.transcript.len() > MAX_TRANSCRIPT_ENTRIES {
            let keep_from = self.transcript.len() - MAX_TRANSCRIPT_ENTRIES;
            for entry in &self.transcript[..keep_from] {
                if let Some(key) = transcript_item_key(entry) {
                    self.transcript_dropped_prefix_bytes.remove(&key);
                }
            }
            self.transcript = self.transcript.split_off(keep_from);
        }

        let (total_bytes, total_newlines, total_display_columns) = self.transcript.iter().fold(
            (0usize, 0usize, 0usize),
            |(bytes, newlines, columns), entry| {
                (
                    bytes.saturating_add(entry.text.len()),
                    newlines
                        .saturating_add(entry.text.bytes().filter(|byte| *byte == b'\n').count()),
                    columns.saturating_add(UnicodeWidthStr::width(entry.text.as_str())),
                )
            },
        );
        let byte_excess = total_bytes.saturating_sub(MAX_TRANSCRIPT_BYTES);
        let newline_prefix = prefix_bytes_for_newlines(
            &self.transcript,
            total_newlines.saturating_sub(MAX_TRANSCRIPT_NEWLINES),
        );
        let display_prefix = prefix_bytes_for_display_width(
            &self.transcript,
            total_display_columns.saturating_sub(MAX_TRANSCRIPT_DISPLAY_COLUMNS),
        );
        let mut excess = byte_excess.max(newline_prefix).max(display_prefix);
        if excess == 0 {
            return;
        }

        let mut drop_entries = 0;
        while let Some(entry) = self.transcript.get(drop_entries) {
            let entry_bytes = entry.text.len();
            if entry_bytes > excess {
                break;
            }
            if let Some(key) = transcript_item_key(entry) {
                self.transcript_dropped_prefix_bytes.remove(&key);
            }
            excess -= entry_bytes;
            drop_entries += 1;
        }
        if drop_entries > 0 {
            self.transcript = self.transcript.split_off(drop_entries);
        }
        if excess == 0 {
            return;
        }

        if let Some(entry) = self.transcript.first_mut() {
            let key = transcript_item_key(entry);
            if let Some(key) = key {
                let truncation = self.transcript_dropped_prefix_bytes.entry(key).or_insert(
                    TranscriptTruncation {
                        dropped_bytes: 0,
                        dropped_hash: TRANSCRIPT_HASH_OFFSET,
                    },
                );
                let (removed, hash) =
                    trim_bytes_from_front(&mut entry.text, excess, truncation.dropped_hash);
                truncation.dropped_bytes = truncation.dropped_bytes.saturating_add(removed);
                truncation.dropped_hash = hash;
            } else {
                let _ = trim_bytes_from_front(&mut entry.text, excess, TRANSCRIPT_HASH_OFFSET);
            }
        }
    }

    fn append_delta(&mut self, turn_id: &str, item_id: &str, delta: &str) {
        let delta = sanitize_terminal_text(delta);
        if delta.is_empty() {
            return;
        }
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            entry.text.push_str(&delta);
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: delta,
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
        }
        self.enforce_transcript_bound();
    }

    fn reconcile_final(
        &mut self,
        turn_id: &str,
        item_id: &str,
        final_text: &str,
    ) -> Result<(), String> {
        let final_text = sanitize_terminal_text(final_text);
        let key = (turn_id.to_owned(), item_id.to_owned());
        if let Some(truncation) = self.transcript_dropped_prefix_bytes.remove(&key) {
            if let Some(entry) = self.transcript.iter_mut().find(|entry| {
                entry.item_id.as_deref() == Some(item_id)
                    && entry.turn_id.as_deref() == Some(turn_id)
            }) {
                let consistent = final_text
                    .get(..truncation.dropped_bytes)
                    .zip(final_text.get(truncation.dropped_bytes..))
                    .is_some_and(|(prefix, suffix)| {
                        extend_transcript_hash(TRANSCRIPT_HASH_OFFSET, prefix.as_bytes())
                            == truncation.dropped_hash
                            && suffix.starts_with(&entry.text)
                    });
                if !consistent {
                    return Err("assistant final snapshot contradicted streamed text".to_owned());
                }
                entry.text = final_text.clone();
            } else {
                self.transcript.push(TranscriptEntry {
                    role: TranscriptRole::Assistant,
                    text: final_text.clone(),
                    item_id: Some(item_id.to_owned()),
                    turn_id: Some(turn_id.to_owned()),
                });
            }
            self.enforce_transcript_bound();
            return Ok(());
        }

        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            if final_text.starts_with(&entry.text) {
                entry.text.push_str(&final_text[entry.text.len()..]);
                self.enforce_transcript_bound();
                Ok(())
            } else {
                Err("assistant final snapshot contradicted streamed text".to_owned())
            }
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: final_text,
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
            self.enforce_transcript_bound();
            Ok(())
        }
    }
}

/// Returns the remaining model-context percentage rounded to the nearest whole
/// percent, with exact half-percent values rounded up.
///
/// `context_tokens` is the current occupancy reported by
/// `tokenUsage.last.totalTokens`, not the cumulative `tokenUsage.total` value.
/// Occupancy is clamped to the context-window size before subtraction. `u128`
/// intermediates keep multiplication and rounding safe for every signed 64-bit
/// value accepted by the installed app-server schema.
pub fn remaining_context_percent(
    context_tokens: i64,
    model_context_window: Option<i64>,
) -> Option<u8> {
    let context_window = u128::try_from(model_context_window?).ok()?;
    let consumed = u128::try_from(context_tokens).ok()?;
    if context_window == 0 {
        return None;
    }
    let remaining = context_window.saturating_sub(consumed.min(context_window));
    let rounded = (remaining * 100 + context_window / 2) / context_window;
    u8::try_from(rounded).ok()
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

    fn deliver_stale_old_turn_events(state: &mut AppState) {
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
            item_id: "thinking-old".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            delta: "stale delta".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingCompleted {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
            item_id: "thinking-old".to_owned(),
            summary: vec!["stale final reasoning".to_owned()],
            content: vec!["stale emitted detail".to_owned()],
        }));
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
            item_id: "agent-old".to_owned(),
            delta: "stale assistant text".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::TurnFinished {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
            outcome: TurnOutcome::Failed("stale failure".to_owned()),
        }));
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
            context_tokens: 99,
            model_context_window: Some(100),
        }));
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

    fn active_context_state() -> AppState {
        AppState {
            auth: AuthState::SignedIn {
                scope: AccountScope::from_chatgpt_email("user@example.com"),
            },
            thread: ThreadState::Ready {
                id: "thr".to_owned(),
            },
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn-1".to_owned(),
            },
            ..AppState::default()
        }
    }

    #[test]
    fn remaining_context_arithmetic_is_honest_clamped_and_overflow_safe() {
        assert_eq!(remaining_context_percent(25, Some(100)), Some(75));
        assert_eq!(remaining_context_percent(1, Some(200)), Some(100));
        assert_eq!(remaining_context_percent(199, Some(200)), Some(1));
        assert_eq!(remaining_context_percent(100, Some(100)), Some(0));
        assert_eq!(remaining_context_percent(150, Some(100)), Some(0));
        assert_eq!(remaining_context_percent(0, Some(i64::MAX)), Some(100));
        assert_eq!(
            remaining_context_percent(i64::MAX - 1, Some(i64::MAX)),
            Some(0)
        );
        assert_eq!(remaining_context_percent(1, None), None);
        assert_eq!(remaining_context_percent(1, Some(0)), None);
        assert_eq!(remaining_context_percent(1, Some(-1)), None);
        assert_eq!(remaining_context_percent(-1, Some(100)), None);
    }

    #[test]
    fn token_usage_is_scoped_and_completed_values_survive_until_a_newer_update() {
        let mut state = active_context_state();
        for (thread_id, turn_id) in [("stale", "turn-1"), ("thr", "stale-turn")] {
            state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
                thread_id: thread_id.to_owned(),
                turn_id: turn_id.to_owned(),
                context_tokens: 90,
                model_context_window: Some(100),
            }));
        }
        assert_eq!(state.context_remaining_percent, None);

        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
            context_tokens: 25,
            model_context_window: Some(100),
        }));
        assert_eq!(state.context_remaining_percent, Some(75));

        state.reduce(Action::Event(DomainEvent::TurnFinished {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
            outcome: TurnOutcome::Completed,
        }));
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
            context_tokens: 26,
            model_context_window: Some(100),
        }));
        assert_eq!(state.context_remaining_percent, Some(74));

        state.turn = TurnState::Starting;
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
            context_tokens: 99,
            model_context_window: Some(100),
        }));
        assert_eq!(state.context_remaining_percent, Some(74));
        state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id: "thr".to_owned(),
            turn_id: "turn-2".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
            context_tokens: 99,
            model_context_window: Some(100),
        }));
        assert_eq!(state.context_remaining_percent, Some(74));
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-2".to_owned(),
            context_tokens: 30,
            model_context_window: Some(100),
        }));
        assert_eq!(state.context_remaining_percent, Some(70));
    }

    #[test]
    fn unusable_relevant_usage_becomes_unknown() {
        let mut state = active_context_state();
        for (context_tokens, model_context_window) in
            [(10, None), (10, Some(0)), (10, Some(-1)), (-1, Some(100))]
        {
            state.context_remaining_percent = Some(80);
            state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
                thread_id: "thr".to_owned(),
                turn_id: "turn-1".to_owned(),
                context_tokens,
                model_context_window,
            }));
            assert_eq!(state.context_remaining_percent, None);
        }
    }

    #[test]
    fn context_resets_only_when_model_or_account_identity_actually_changes() {
        let scope = AccountScope::from_chatgpt_email("user@example.com");
        let mut state = active_context_state();
        state.connection = ConnectionState::Ready { generation: 1 };
        state.models = vec![
            model("m1", true, &["high"], "high"),
            model("m2", false, &["high"], "high"),
        ];
        state.selected_model = Some("m1".to_owned());
        state.selected_reasoning = Some("high".to_owned());

        state.context_remaining_percent = Some(70);
        state.reduce(Action::Intent(Intent::SelectModel("m1".to_owned())));
        assert_eq!(state.context_remaining_percent, Some(70));
        state.reduce(Action::Intent(Intent::SelectModel("m2".to_owned())));
        assert_eq!(state.context_remaining_percent, None);
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-1".to_owned(),
            context_tokens: 10,
            model_context_window: Some(100),
        }));
        assert_eq!(state.context_remaining_percent, None);
        state.turn = TurnState::Starting;
        state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id: "thr".to_owned(),
            turn_id: "turn-2".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr".to_owned(),
            turn_id: "turn-2".to_owned(),
            context_tokens: 20,
            model_context_window: Some(100),
        }));
        assert_eq!(state.context_remaining_percent, Some(80));

        state.auth = AuthState::SignedIn {
            scope: scope.clone(),
        };
        state.preferences.thread_id = None;
        state.context_remaining_percent = Some(69);
        seed_thinking(&mut state, "keep me");
        state.reduce(Action::Event(DomainEvent::AccountLoaded(scope)));
        assert_eq!(state.context_remaining_percent, Some(69));
        assert_eq!(state.thinking.entries[0].text, "keep me");
        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("other@example.com"),
        )));
        assert_eq!(state.context_remaining_percent, None);
        assert!(state.thinking.entries.is_empty());

        state.context_remaining_percent = Some(68);
        state.reduce(Action::Event(DomainEvent::LoggedOut));
        assert_eq!(state.context_remaining_percent, None);
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
    fn invalid_or_oversized_messages_remain_local_and_preserve_conversation_state() {
        for message in [
            "\u{1b}\u{0007}".to_owned(),
            "x".repeat(MAX_MESSAGE_BYTES + 1),
        ] {
            let mut state = thread_ready_state();
            let transcript = state.transcript.clone();
            assert!(state
                .reduce(Action::Intent(Intent::SendMessage(message)))
                .is_empty());
            assert_eq!(state.transcript, transcript);
            assert!(matches!(state.turn, TurnState::Completed { .. }));
            assert!(state.notice.is_some());
        }
    }

    #[test]
    fn same_account_refresh_preserves_ready_and_in_flight_resume_state() {
        let mut state = thread_ready_state();
        let scope = AccountScope::from_chatgpt_email("user@example.com");
        state.turn = TurnState::Streaming {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
        };
        state.context_remaining_percent = Some(61);
        seed_thinking(&mut state, "live reasoning");
        let ready_snapshot = state.clone();

        assert!(state
            .reduce(Action::Event(DomainEvent::AccountLoaded(scope.clone())))
            .is_empty());
        assert_eq!(state, ready_snapshot);

        state.thread = ThreadState::Resuming {
            id: "thr-active".to_owned(),
        };
        state.turn = TurnState::Idle;
        let resuming_snapshot = state.clone();
        assert!(state
            .reduce(Action::Event(DomainEvent::AccountLoaded(scope)))
            .is_empty());
        assert_eq!(state, resuming_snapshot);
    }

    #[test]
    fn account_switch_settles_old_turn_closes_picker_and_rejects_late_events() {
        let mut state = thread_ready_state();
        state.turn = TurnState::Streaming {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
        };
        state.context_remaining_percent = Some(61);
        seed_thinking(&mut state, "old account reasoning");
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![thread("thr-old", "Old account thread", 1)],
            selected: 0,
            confirmation: None,
            message: None,
        });
        let transcript = state.transcript.clone();

        assert!(state
            .reduce(Action::Event(DomainEvent::AccountLoaded(
                AccountScope::from_chatgpt_email("other@example.com"),
            )))
            .is_empty());
        assert!(matches!(state.turn, TurnState::Idle));
        assert!(matches!(
            state.thread,
            ThreadState::AccountMismatch { ref id } if id == "thr-active"
        ));
        assert!(state.thread_picker.is_none());
        assert!(state.thinking.entries.is_empty());
        assert_eq!(state.context_remaining_percent, None);

        deliver_stale_old_turn_events(&mut state);
        assert!(matches!(state.turn, TurnState::Idle));
        assert_eq!(state.transcript, transcript);
        assert!(state.thinking.entries.is_empty());
        assert_eq!(state.context_remaining_percent, None);
    }

    #[test]
    fn account_switch_closes_picker_even_when_new_scope_matches_saved_thread() {
        let mut state = thread_ready_state();
        let saved_scope = AccountScope::from_chatgpt_email("saved@example.com");
        state.thread = ThreadState::AccountMismatch {
            id: "thr-saved".to_owned(),
        };
        state.turn = TurnState::Starting;
        state.preferences.account_scope = saved_scope.clone();
        state.preferences.thread_id = Some("thr-saved".to_owned());
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![thread("thr-old", "Previous account thread", 1)],
            selected: 0,
            confirmation: None,
            message: None,
        });

        assert!(state
            .reduce(Action::Event(DomainEvent::AccountLoaded(saved_scope)))
            .is_empty());
        assert!(state.thread_picker.is_none());
        assert!(matches!(state.turn, TurnState::Idle));
        assert!(matches!(
            state.thread,
            ThreadState::AccountMismatch { ref id } if id == "thr-saved"
        ));
    }

    #[test]
    fn unsupported_account_detaches_saved_thread_and_rejects_late_events() {
        let mut state = thread_ready_state();
        state.turn = TurnState::Streaming {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
        };
        state.context_remaining_percent = Some(61);
        seed_thinking(&mut state, "old account reasoning");
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![thread("thr-old", "Old account thread", 1)],
            selected: 0,
            confirmation: None,
            message: None,
        });
        let transcript = state.transcript.clone();

        state.reduce(Action::Event(DomainEvent::UnsupportedAccount(
            "unsupported account type apiKey; use ChatGPT login".to_owned(),
        )));
        assert!(matches!(state.auth, AuthState::Unsupported(_)));
        assert!(matches!(state.turn, TurnState::Idle));
        assert!(matches!(
            state.thread,
            ThreadState::AccountMismatch { ref id } if id == "thr-active"
        ));
        assert!(state.thread_picker.is_none());
        assert!(state.thinking.entries.is_empty());
        assert_eq!(state.context_remaining_percent, None);

        deliver_stale_old_turn_events(&mut state);
        assert!(matches!(state.turn, TurnState::Idle));
        assert_eq!(state.transcript, transcript);
        assert!(state.thinking.entries.is_empty());
        assert_eq!(state.context_remaining_percent, None);
    }

    #[test]
    fn successful_automatic_resume_settles_turn_state() {
        let mut state = thread_ready_state();
        state.thread = ThreadState::Resuming {
            id: "thr-active".to_owned(),
        };
        state.turn = TurnState::Streaming {
            thread_id: "thr-active".to_owned(),
            turn_id: "stale-turn".to_owned(),
        };

        state.reduce(Action::Event(DomainEvent::ResumeSucceeded {
            id: "thr-active".to_owned(),
            history: Vec::new(),
        }));

        assert!(matches!(state.turn, TurnState::Idle));
    }

    #[test]
    fn resume_results_are_correlated_and_cannot_cross_account_boundaries() {
        let mut state = thread_ready_state();
        let snapshot = state.clone();

        assert!(state
            .reduce(Action::Event(DomainEvent::ResumeStarted {
                id: "thr-stale".to_owned(),
            }))
            .is_empty());
        assert_eq!(state, snapshot);
        assert!(state
            .reduce(Action::Event(DomainEvent::ResumeSucceeded {
                id: "thr-stale".to_owned(),
                history: Vec::new(),
            }))
            .is_empty());
        assert_eq!(state, snapshot);
        assert!(state
            .reduce(Action::Event(DomainEvent::ResumeFailed {
                id: "thr-stale".to_owned(),
                message: "late failure".to_owned(),
            }))
            .is_empty());
        assert_eq!(state, snapshot);

        state.thread = ThreadState::Resuming {
            id: "thr-active".to_owned(),
        };
        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("other@example.com"),
        )));
        let account_switched = state.clone();

        assert!(state
            .reduce(Action::Event(DomainEvent::ResumeSucceeded {
                id: "thr-active".to_owned(),
                history: vec![TranscriptEntry {
                    role: TranscriptRole::Assistant,
                    text: "wrong account history".to_owned(),
                    item_id: None,
                    turn_id: None,
                }],
            }))
            .is_empty());
        assert_eq!(state, account_switched);
        assert!(state
            .reduce(Action::Event(DomainEvent::ResumeFailed {
                id: "thr-active".to_owned(),
                message: "late failure".to_owned(),
            }))
            .is_empty());
        assert_eq!(state, account_switched);
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
        state.context_remaining_percent = Some(68);
        assert_eq!(
            state.reduce(Action::Intent(Intent::NewThread)),
            vec![Effect::StartNewThread]
        );
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(state.transcript[0].text, "old conversation");
        assert_eq!(state.thinking.entries[0].text, "old reasoning");
        assert_eq!(state.context_remaining_percent, Some(68));

        state.reduce(Action::Event(DomainEvent::NewThreadFailed(
            "server rejected it".to_owned(),
        )));
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-active"));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(state.transcript.len(), 1);
        assert_eq!(state.thinking.entries[0].text, "old reasoning");
        assert_eq!(state.context_remaining_percent, Some(68));

        assert_eq!(
            state.reduce(Action::Intent(Intent::NewThread)),
            vec![Effect::StartNewThread]
        );
        let effects = state.reduce(Action::Event(DomainEvent::NewThreadSucceeded {
            id: "thr-new".to_owned(),
        }));
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-new"));
        assert!(state.transcript.is_empty());
        assert!(state.thinking.entries.is_empty());
        assert!(state.thinking.visible);
        assert_eq!(state.context_remaining_percent, None);
        assert!(matches!(state.turn, TurnState::Idle));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-new"));
        assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
    }

    #[test]
    fn new_thread_creation_is_single_flight_and_scoped_to_the_starting_account() {
        let mut state = thread_ready_state();
        assert_eq!(
            state.reduce(Action::Intent(Intent::NewThread)),
            vec![Effect::StartNewThread]
        );
        assert!(state.reduce(Action::Intent(Intent::NewThread)).is_empty());
        assert!(state
            .reduce(Action::Intent(Intent::SendMessage("race".to_owned())))
            .is_empty());
        assert_eq!(state.transcript.len(), 1);

        state.reduce(Action::Event(DomainEvent::AccountLoaded(
            AccountScope::from_chatgpt_email("other@example.com"),
        )));
        let switched = state.clone();
        assert!(state
            .reduce(Action::Event(DomainEvent::NewThreadSucceeded {
                id: "thr-created-for-old-account".to_owned(),
            }))
            .is_empty());
        assert_eq!(state, switched);
        assert!(state
            .reduce(Action::Event(DomainEvent::NewThreadFailed(
                "late failure from old account".to_owned(),
            )))
            .is_empty());
        assert_eq!(state, switched);

        assert_eq!(
            state.reduce(Action::Intent(Intent::NewThread)),
            vec![Effect::StartNewThread]
        );
        assert!(matches!(
            state
                .reduce(Action::Event(DomainEvent::NewThreadSucceeded {
                    id: "thr-new-account".to_owned(),
                }))
                .as_slice(),
            [Effect::Persist(_)]
        ));
        assert!(matches!(
            state.thread,
            ThreadState::Ready { ref id } if id == "thr-new-account"
        ));
    }

    #[test]
    fn implicit_thread_start_only_attaches_to_the_expected_first_message() {
        let mut state = thread_ready_state();
        let ready = state.clone();
        assert!(state
            .reduce(Action::Event(DomainEvent::ThreadStarted {
                id: "thr-stale".to_owned(),
            }))
            .is_empty());
        assert_eq!(state, ready);

        state.thread = ThreadState::None;
        state.turn = TurnState::Idle;
        let idle = state.clone();
        assert!(state
            .reduce(Action::Event(DomainEvent::ThreadStarted {
                id: "thr-unrequested".to_owned(),
            }))
            .is_empty());
        assert_eq!(state, idle);

        state.turn = TurnState::Starting;
        assert!(matches!(
            state
                .reduce(Action::Event(DomainEvent::ThreadStarted {
                    id: "thr-expected".to_owned(),
                }))
                .as_slice(),
            [Effect::Persist(_)]
        ));
        assert!(matches!(
            state.thread,
            ThreadState::Ready { ref id } if id == "thr-expected"
        ));
    }

    #[test]
    fn successful_new_thread_rejects_every_stale_old_turn_event() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "old reasoning");
        state.context_remaining_percent = Some(68);

        assert_eq!(
            state.reduce(Action::Intent(Intent::NewThread)),
            vec![Effect::StartNewThread]
        );
        state.reduce(Action::Event(DomainEvent::NewThreadSucceeded {
            id: "thr-new".to_owned(),
        }));
        let replaced = state.clone();

        deliver_stale_old_turn_events(&mut state);

        assert_eq!(state, replaced);
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-new"));
        assert!(matches!(state.turn, TurnState::Idle));
        assert!(state.transcript.is_empty());
        assert!(state.thinking.entries.is_empty());
        assert_eq!(state.context_remaining_percent, None);
    }

    #[test]
    fn picker_ignores_mismatched_results_then_atomically_switches_threads() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "active reasoning");
        state.context_remaining_percent = Some(67);
        state.reduce(Action::Intent(Intent::Resume));
        state.reduce(Action::Event(DomainEvent::ThreadListLoaded(vec![
            thread("thr-active", "Current", 30),
            thread("thr-old", "Target", 20),
            thread("thr-old-a", "Unrelated", 10),
        ])));
        state.reduce(Action::Intent(Intent::ThreadPickerMoveDown));
        assert_eq!(
            state.reduce(Action::Intent(Intent::ThreadPickerSelect)),
            vec![Effect::SwitchThread {
                id: "thr-old".to_owned()
            }]
        );
        assert!(matches!(
            state.thread_picker.as_ref().map(|picker| &picker.phase),
            Some(ThreadPickerPhase::Resuming { id }) if id == "thr-old"
        ));
        let awaiting_b = state.clone();

        assert!(state
            .reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
                id: "thr-old-a".to_owned(),
                history: vec![TranscriptEntry {
                    role: TranscriptRole::Assistant,
                    text: "wrong history".to_owned(),
                    item_id: None,
                    turn_id: None,
                }],
            }))
            .is_empty());
        assert_eq!(state, awaiting_b);

        assert!(state
            .reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
                id: "thr-old-a".to_owned(),
                message: "wrong failure".to_owned(),
            }))
            .is_empty());
        assert_eq!(state, awaiting_b);

        let history = vec![TranscriptEntry {
            role: TranscriptRole::Assistant,
            text: "target history".to_owned(),
            item_id: Some("agent-b".to_owned()),
            turn_id: Some("turn-b".to_owned()),
        }];
        let effects = state.reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
            id: "thr-old".to_owned(),
            history: history.clone(),
        }));
        assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-old"));
        assert_eq!(state.transcript, history);
        assert!(state.thinking.entries.is_empty());
        assert!(state.thinking.visible);
        assert_eq!(state.context_remaining_percent, None);
        assert!(state.thread_picker.is_none());
    }

    #[test]
    fn successful_picker_switch_rejects_every_stale_old_turn_event() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "active reasoning");
        state.context_remaining_percent = Some(67);
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Resuming {
                id: "thr-b".to_owned(),
            },
            threads: vec![
                thread("thr-active", "Current", 30),
                thread("thr-b", "Target", 20),
            ],
            selected: 1,
            confirmation: None,
            message: None,
        });
        state.reduce(Action::Event(DomainEvent::ThreadSwitchSucceeded {
            id: "thr-b".to_owned(),
            history: vec![TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: "target history".to_owned(),
                item_id: Some("agent-b".to_owned()),
                turn_id: Some("turn-b".to_owned()),
            }],
        }));
        let replaced = state.clone();

        deliver_stale_old_turn_events(&mut state);

        assert_eq!(state, replaced);
        assert!(matches!(&state.thread, ThreadState::Ready { id } if id == "thr-b"));
        assert!(matches!(state.turn, TurnState::Idle));
        assert_eq!(state.transcript[0].text, "target history");
        assert!(state.thinking.entries.is_empty());
        assert_eq!(state.context_remaining_percent, None);
    }

    #[test]
    fn picker_navigation_and_failed_switch_preserve_the_active_thread() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "active thread reasoning");
        state.context_remaining_percent = Some(67);
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
        assert_eq!(state.context_remaining_percent, Some(67));

        state.reduce(Action::Event(DomainEvent::ThreadSwitchFailed {
            id: "thr-old".to_owned(),
            message: "malformed history".to_owned(),
        }));
        assert_eq!(state.preferences.thread_id.as_deref(), Some("thr-active"));
        assert_eq!(state.transcript[0].text, "old conversation");
        assert_eq!(state.thinking.entries[0].text, "active thread reasoning");
        assert_eq!(state.context_remaining_percent, Some(67));
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
        assert_eq!(state.context_remaining_percent, None);
        assert!(state.thread_picker.is_none());
        assert!(matches!(effects.as_slice(), [Effect::Persist(_)]));

        state.reduce(Action::Intent(Intent::SendMessage("continue".to_owned())));
        state.reduce(Action::Event(DomainEvent::TurnStarted {
            thread_id: "thr-old".to_owned(),
            turn_id: "turn-new".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::ThinkingDelta {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
            item_id: "thinking-stale".to_owned(),
            kind: ThinkingKind::Summary,
            index: 0,
            delta: "stale reasoning".to_owned(),
        }));
        state.reduce(Action::Event(DomainEvent::TokenUsageUpdated {
            thread_id: "thr-active".to_owned(),
            turn_id: "turn-old".to_owned(),
            context_tokens: 99,
            model_context_window: Some(100),
        }));
        assert!(state.thinking.entries.is_empty());
        assert_eq!(state.context_remaining_percent, None);
    }

    #[test]
    fn automatic_resume_preserves_thinking_on_failure_and_clears_it_on_success() {
        let mut state = thread_ready_state();
        seed_thinking(&mut state, "current thread reasoning");
        state.context_remaining_percent = Some(66);

        state.thread = ThreadState::Resuming {
            id: "thr-old".to_owned(),
        };
        state.reduce(Action::Event(DomainEvent::ResumeFailed {
            id: "thr-old".to_owned(),
            message: "temporary failure".to_owned(),
        }));
        assert_eq!(state.thinking.entries[0].text, "current thread reasoning");
        assert_eq!(state.context_remaining_percent, Some(66));

        state.thread = ThreadState::Resuming {
            id: "thr-old".to_owned(),
        };
        state.reduce(Action::Event(DomainEvent::ResumeSucceeded {
            id: "thr-old".to_owned(),
            history: Vec::new(),
        }));
        assert!(state.thinking.entries.is_empty());
        assert!(state.thinking.visible);
        assert_eq!(state.context_remaining_percent, None);
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
            deleted: vec!["thr-old-a".to_owned()],
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
        assert!(!picker
            .message
            .as_deref()
            .unwrap()
            .contains("active saved thread"));
    }

    #[test]
    fn deletion_results_must_exactly_match_the_confirmed_target_set() {
        let mut state = thread_ready_state();
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Ready,
            threads: vec![
                thread("thr-active", "Current", 30),
                thread("thr-old-a", "Old A", 20),
                thread("thr-old-b", "Old B", 10),
            ],
            selected: 1,
            confirmation: None,
            message: None,
        });
        state.reduce(Action::Intent(Intent::ThreadPickerRequestDelete));
        assert_eq!(
            state.reduce(Action::Intent(Intent::ThreadPickerConfirmDelete)),
            vec![Effect::DeleteThreads {
                ids: vec!["thr-old-a".to_owned()]
            }]
        );
        let preferences = state.preferences.clone();

        state.reduce(Action::Event(DomainEvent::ThreadDeletionFinished {
            requested: 1,
            deleted: vec!["thr-old-b".to_owned()],
            failures: vec![],
        }));

        let picker = state.thread_picker.as_ref().unwrap();
        assert!(matches!(picker.phase, ThreadPickerPhase::Failed));
        assert_eq!(picker.threads.len(), 3);
        assert_eq!(state.preferences, preferences);
        assert!(picker.message.as_deref().unwrap().contains("did not match"));
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
            delta: "\u{1b}\u{0007}".to_owned(),
        }));
        assert!(state.is_waiting_for_assistant_text());
        assert_eq!(state.transcript.len(), 1);

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
        resuming.thread = ThreadState::Resuming {
            id: "other-thread".to_owned(),
        };
        assert!(!resuming.is_waiting_for_assistant_text());

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
    fn transcript_retention_is_bounded_without_breaking_stream_reconciliation() {
        let mut state = AppState {
            thread: ThreadState::Ready {
                id: "thr".to_owned(),
            },
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        let streamed = format!("prefix-{}", "界".repeat(MAX_TRANSCRIPT_BYTES / 3 + 100));
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            delta: streamed.clone(),
        }));
        assert!(
            state
                .transcript
                .iter()
                .map(|entry| entry.text.len())
                .sum::<usize>()
                <= MAX_TRANSCRIPT_BYTES
        );
        assert!(state
            .transcript_dropped_prefix_bytes
            .contains_key(&("turn".to_owned(), "item".to_owned())));
        assert_eq!(state.transcript_dropped_prefix_bytes.len(), 1);

        let mut contradicted = state.clone();
        let mut contradictory_final = streamed.clone();
        contradictory_final.replace_range(..1, "X");
        contradicted.reduce(Action::Event(DomainEvent::AgentCompleted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            text: contradictory_final,
        }));
        assert!(matches!(contradicted.turn, TurnState::Failed { .. }));

        let final_text = format!("{streamed}-tail");
        state.reduce(Action::Event(DomainEvent::AgentCompleted {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "item".to_owned(),
            text: final_text,
        }));
        assert!(matches!(state.turn, TurnState::Streaming { .. }));
        assert!(state.transcript.last().unwrap().text.ends_with("-tail"));
        assert!(
            state
                .transcript
                .iter()
                .map(|entry| entry.text.len())
                .sum::<usize>()
                <= MAX_TRANSCRIPT_BYTES
        );

        state.thread = ThreadState::Resuming {
            id: "thr-history".to_owned(),
        };
        let history = (0..=MAX_TRANSCRIPT_ENTRIES)
            .map(|index| TranscriptEntry {
                role: TranscriptRole::User,
                text: format!("history-{index}"),
                item_id: None,
                turn_id: None,
            })
            .collect();
        state.reduce(Action::Event(DomainEvent::ResumeSucceeded {
            id: "thr-history".to_owned(),
            history,
        }));
        assert_eq!(state.transcript.len(), MAX_TRANSCRIPT_ENTRIES);
        assert_eq!(state.transcript.first().unwrap().text, "history-1");
    }

    #[test]
    fn transcript_retention_bounds_newline_and_display_width_floods() {
        let mut state = AppState {
            thread: ThreadState::Ready {
                id: "thr".to_owned(),
            },
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };
        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "newlines".to_owned(),
            delta: format!("HEAD{}TAIL", "\n".repeat(MAX_TRANSCRIPT_NEWLINES + 70_000)),
        }));
        let retained = &state.transcript.last().unwrap().text;
        assert!(retained.bytes().filter(|byte| *byte == b'\n').count() <= MAX_TRANSCRIPT_NEWLINES);
        assert!(retained.ends_with("TAIL"));

        state.reduce(Action::Event(DomainEvent::AgentDelta {
            thread_id: "thr".to_owned(),
            turn_id: "turn".to_owned(),
            item_id: "wide".to_owned(),
            delta: format!(
                "{}WIDTH-TAIL",
                "x".repeat(MAX_TRANSCRIPT_DISPLAY_COLUMNS + 1_000)
            ),
        }));
        let columns = state.transcript.iter().fold(0usize, |total, entry| {
            total.saturating_add(UnicodeWidthStr::width(entry.text.as_str()))
        });
        assert!(columns <= MAX_TRANSCRIPT_DISPLAY_COLUMNS);
        assert!(state
            .transcript
            .last()
            .unwrap()
            .text
            .ends_with("WIDTH-TAIL"));
        assert!(state.transcript_dropped_prefix_bytes.len() <= 1);
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
            "界".repeat(MAX_THINKING_BYTES / 3 + 20)
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
        assert!(retained.len() <= MAX_THINKING_BYTES);
        assert!(retained.len() >= MAX_THINKING_BYTES.saturating_sub(2));
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
    fn thinking_entry_count_evicts_exactly_the_oldest_active_turn_entries() {
        let mut state = AppState {
            turn: TurnState::Streaming {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
            },
            ..AppState::default()
        };

        for index in 0..=(MAX_THINKING_ENTRIES as i64 + 1) {
            state.reduce(Action::Event(DomainEvent::ThinkingDelta {
                thread_id: "thr".to_owned(),
                turn_id: "turn".to_owned(),
                item_id: format!("why-{index}"),
                kind: ThinkingKind::Summary,
                index,
                delta: index.to_string(),
            }));
        }

        assert_eq!(state.thinking.entries.len(), MAX_THINKING_ENTRIES);
        assert_eq!(state.thinking.entries[0].item_id, "why-2");
        assert_eq!(state.thinking.entries[0].text, "2");
        assert_eq!(
            state.thinking.entries.last().unwrap().item_id,
            format!("why-{}", MAX_THINKING_ENTRIES + 1)
        );
        assert!(state
            .thinking
            .entries
            .iter()
            .all(|entry| entry.turn_id == "turn"));
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

    #[test]
    fn settled_turns_and_shutdown_ignore_late_mutating_results() {
        let mut state = thread_ready_state();
        let settled = state.clone();
        assert!(state
            .reduce(Action::Event(DomainEvent::TurnOperationFailed(
                "late interrupt failure".to_owned(),
            )))
            .is_empty());
        assert_eq!(state, settled);

        let effects = state.reduce(Action::Intent(Intent::Quit));
        assert_eq!(effects, vec![Effect::Shutdown]);
        let shutting_down = state.clone();
        for action in [
            Action::Intent(Intent::Quit),
            Action::Intent(Intent::NewThread),
            Action::Intent(Intent::SendMessage("too late".to_owned())),
            Action::Event(DomainEvent::NewThreadSucceeded {
                id: "thr-too-late".to_owned(),
            }),
            Action::Event(DomainEvent::ResumeSucceeded {
                id: "thr-too-late".to_owned(),
                history: Vec::new(),
            }),
        ] {
            assert!(state.reduce(action).is_empty());
            assert_eq!(state, shutting_down);
        }
    }

    #[test]
    fn safety_violation_settles_busy_picker_and_pending_thread_work() {
        let mut state = thread_ready_state();
        assert_eq!(
            state.reduce(Action::Intent(Intent::NewThread)),
            vec![Effect::StartNewThread]
        );
        state.thread_picker = Some(ThreadPickerState {
            phase: ThreadPickerPhase::Deleting { requested: 1 },
            threads: vec![thread("thr-old", "Old", 1)],
            selected: 0,
            confirmation: None,
            message: None,
        });

        state.reduce(Action::Event(DomainEvent::SafetyViolation(
            "unknown/request".to_owned(),
        )));

        let picker = state.thread_picker.as_ref().unwrap();
        assert!(matches!(picker.phase, ThreadPickerPhase::Failed));
        assert!(picker.message.as_deref().unwrap().contains("denied"));
        let snapshot = state.clone();
        state.reduce(Action::Event(DomainEvent::NewThreadSucceeded {
            id: "thr-too-late".to_owned(),
        }));
        assert_eq!(state, snapshot);
    }
}
