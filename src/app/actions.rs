use super::*;
use crate::openrouter::{
    OpenRouterAuthStatus, OpenRouterFailureCategory, OpenRouterModel, OpenRouterStreamStage,
    TokenUsage,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    StartLogin,
    StartDeviceLogin,
    CancelLogin {
        login_id: String,
    },
    Logout,
    StartNewThread,
    StartNewOpenRouterConversation,
    ListThreads,
    ResumeThread {
        id: String,
    },
    SwitchThread {
        id: String,
        model: ModelKey,
        reasoning: String,
    },
    SwitchOpenRouterConversation {
        id: OpenRouterConversationId,
        model: ModelKey,
    },
    DeleteThreads {
        ids: Vec<String>,
    },
    DeleteOpenRouterConversations {
        ids: Vec<OpenRouterConversationId>,
    },
    DeleteConversations {
        codex_ids: Vec<String>,
        openrouter_ids: Vec<OpenRouterConversationId>,
    },
    SendMessage {
        text: String,
    },
    RefreshOpenRouter,
    LogoutOpenRouter,
    SendOpenRouterMessage {
        text: String,
    },
    InterruptOpenRouterTurn,
    RefreshClaude,
    LoginClaude,
    LogoutClaude,
    StartNewClaudeSession,
    SwitchClaudeSession {
        id: ClaudeSessionId,
    },
    SendClaudeMessage {
        text: String,
    },
    InterruptClaudeTurn,
    DeleteClaudeSessions {
        ids: Vec<ClaudeSessionId>,
    },
    DeleteAllConversations {
        codex_ids: Vec<String>,
        openrouter_ids: Vec<OpenRouterConversationId>,
        claude_ids: Vec<ClaudeSessionId>,
    },
    InterruptTurn {
        thread_id: String,
        turn_id: String,
    },
    Persist(PreferencesV3),
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
    PreferencesLoaded(PreferencesV3),
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
    OpenRouterStartup {
        auth: OpenRouterAuthStatus,
        catalog: Vec<OpenRouterModel>,
    },
    OpenRouterAuthChanged(OpenRouterAuthStatus),
    OpenRouterCatalogLoaded(Vec<OpenRouterModel>),
    OpenRouterCatalogLoadedForAutomaticResume(Vec<OpenRouterModel>),
    OpenRouterOperationFailed(OpenRouterFailureCategory),
    OpenRouterCandidateRejected(OpenRouterFailureCategory),
    OpenRouterTurnStarted {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
    },
    OpenRouterConversationStarted {
        conversation_id: OpenRouterConversationId,
    },
    OpenRouterConversationRestored {
        conversation_id: OpenRouterConversationId,
        history: Vec<TranscriptEntry>,
        model: ModelKey,
        automatic: bool,
    },
    OpenRouterConversationSwitchFailed {
        conversation_id: OpenRouterConversationId,
        message: String,
    },
    OpenRouterResumeFailed {
        conversation_id: OpenRouterConversationId,
    },
    OpenRouterDelta {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
        delta: String,
    },
    OpenRouterUsage {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
        usage: TokenUsage,
    },
    ClaudeStartup {
        availability: ClaudeAvailability,
        auth: ClaudeAuthStatus,
    },
    ClaudeAuthRequested(ClaudeAuthRequest),
    ClaudeAuthChanged(ClaudeAuthStatus),
    ClaudeOperationFailed(ClaudeError),
    ClaudeSessionStarted {
        session_id: ClaudeSessionId,
    },
    ClaudeNewSessionFailed(String),
    ClaudeSessionCreationUncertain {
        session_id: ClaudeSessionId,
        message: String,
    },
    ClaudeSessionRestored {
        session: ClaudeSessionV1,
        automatic: bool,
    },
    ClaudeSessionSwitchFailed {
        session_id: ClaudeSessionId,
        message: String,
    },
    ClaudeResumeFailed {
        session_id: ClaudeSessionId,
        message: String,
    },
    ClaudeTurnStarted {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
    },
    ClaudeInitialized {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
        model: ClaudeModelMetadata,
    },
    ClaudeDelta {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
        delta: String,
    },
    ClaudeTurnFinished {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
        outcome: ClaudeTurnOutcome,
        assistant_text: Option<String>,
        incomplete_assistant_text: Option<String>,
        creation_uncertain: bool,
        failure: Option<ClaudeError>,
    },
    OpenRouterTurnFinished {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
        outcome: TurnOutcome,
        assistant_text: Option<String>,
        incomplete_assistant_text: Option<String>,
        failure_stage: Option<OpenRouterStreamStage>,
    },
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
        model: ModelKey,
        reasoning: String,
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
