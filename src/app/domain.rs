use super::*;
use crate::openrouter::{OpenRouterAuthStatus, OpenRouterModel};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    SendMessage(String),
    NewThread,
    ShowLogin,
    Login,
    LoginDevice,
    ShowLogout,
    Logout,
    ShowModels,
    SelectModel(String),
    SelectProviderModel(ModelKey),
    RefreshOpenRouter,
    LogoutOpenRouter,
    RefreshClaude,
    LogoutClaude,
    PopupMoveUp,
    PopupMoveDown,
    PopupPageUp,
    PopupPageDown,
    PopupMoveFirst,
    PopupMoveLast,
    PopupSelect,
    PopupClose,
    PopupSearchAppend(char),
    PopupSearchBackspace,
    PopupCatalogToggle,
    PopupOpenCatalog,
    PopupRefresh,
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

impl ModelChoice {
    /// Codex-only compatibility tag until the unified model catalog lands.
    pub fn key(&self) -> ModelKey {
        ModelKey::codex(self.id.clone()).expect("validated Codex model IDs are nonempty")
    }
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
    OpenRouterStreaming {
        conversation_id: OpenRouterConversationId,
        turn_id: OpenRouterTurnId,
    },
    ClaudeStreaming {
        session_id: ClaudeSessionId,
        turn_id: ClaudeTurnId,
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
        matches!(
            self,
            Self::Starting
                | Self::Streaming { .. }
                | Self::OpenRouterStreaming { .. }
                | Self::ClaudeStreaming { .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenRouterConversationState {
    None,
    Ready {
        id: OpenRouterConversationId,
    },
    ResumeFailed {
        id: OpenRouterConversationId,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterCredentialValidation {
    Idle,
    Refreshing {
        operation_id: u64,
    },
    Validating {
        operation_id: u64,
        candidate_saved: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRouterState {
    pub auth: OpenRouterAuthStatus,
    pub catalog: Vec<OpenRouterModel>,
    pub conversation: OpenRouterConversationState,
    pub credential_validation: OpenRouterCredentialValidation,
}

impl Default for OpenRouterState {
    fn default() -> Self {
        Self {
            auth: OpenRouterAuthStatus::Missing,
            catalog: Vec::new(),
            conversation: OpenRouterConversationState::None,
            credential_validation: OpenRouterCredentialValidation::Idle,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaudeAvailability {
    Ready,
    Unavailable(String),
}

impl Default for ClaudeAvailability {
    fn default() -> Self {
        Self::Unavailable("Claude Code runtime has not been inspected".to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaudeConversationState {
    None,
    Ready {
        id: ClaudeSessionId,
    },
    ResumeFailed {
        id: ClaudeSessionId,
        message: String,
    },
    CreationUncertain {
        id: ClaudeSessionId,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaudeAuthRequest {
    pub operation_id: u64,
    pub action: crate::claude::ClaudeAuthAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaudeAuthOperation {
    Idle,
    Checking { operation_id: u64 },
    AwaitingTerminal { request: ClaudeAuthRequest },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeState {
    pub availability: ClaudeAvailability,
    pub auth: ClaudeAuthStatus,
    pub conversation: ClaudeConversationState,
    pub resolved_model: Option<ClaudeModelMetadata>,
    pub auth_operation: ClaudeAuthOperation,
}

impl Default for ClaudeState {
    fn default() -> Self {
        Self {
            availability: ClaudeAvailability::default(),
            auth: ClaudeAuthStatus::SignedOut,
            conversation: ClaudeConversationState::None,
            resolved_model: None,
            auth_operation: ClaudeAuthOperation::Idle,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptEntryStatus {
    Normal,
    FailedIncomplete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptEntry {
    pub provider: ProviderId,
    pub role: TranscriptRole,
    pub status: TranscriptEntryStatus,
    pub text: String,
    pub item_id: Option<String>,
    pub turn_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptTruncation {
    pub dropped_bytes: usize,
    pub dropped_hash: u64,
}
