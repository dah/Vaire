use super::*;

pub(in crate::backend) const MAX_TRACKED_COMPLETED_ITEMS_PER_TURN: usize = 1_024;
pub(in crate::backend) const MAX_TRACKED_COMPLETED_ITEM_ID_BYTES: usize = 64 * 1_024;

#[derive(Debug, Default)]
pub(in crate::backend) struct CompletedItemTracker {
    pub(in crate::backend) scope: Option<(String, String)>,
    pub(in crate::backend) ids: HashSet<String>,
    pub(in crate::backend) id_bytes: usize,
    pub(in crate::backend) saturated: bool,
}

impl CompletedItemTracker {
    pub(in crate::backend) fn begin_turn(&mut self, thread_id: &str, turn_id: &str) {
        self.scope = Some((thread_id.to_owned(), turn_id.to_owned()));
        self.ids.clear();
        self.id_bytes = 0;
        self.saturated = false;
    }

    pub(in crate::backend) fn observe_turn(&mut self, thread_id: &str, turn_id: &str) {
        if !self.is_scope(thread_id, turn_id) {
            self.begin_turn(thread_id, turn_id);
        }
    }

    pub(in crate::backend) fn should_ignore(
        &self,
        thread_id: &str,
        turn_id: &str,
        item_id: &str,
    ) -> bool {
        self.is_scope(thread_id, turn_id) && (self.saturated || self.ids.contains(item_id))
    }

    pub(in crate::backend) fn record(&mut self, thread_id: &str, turn_id: &str, item_id: &str) {
        if !self.is_scope(thread_id, turn_id) || self.saturated || self.ids.contains(item_id) {
            return;
        }
        if self.ids.len() >= MAX_TRACKED_COMPLETED_ITEMS_PER_TURN
            || self.id_bytes.saturating_add(item_id.len()) > MAX_TRACKED_COMPLETED_ITEM_ID_BYTES
        {
            // Once exact tracking cannot continue within its hard bounds, stop accepting all
            // subsequent item mutations for this turn. Dropping output is safer than allowing a
            // late delta to rewrite an item whose completion could not be retained.
            self.saturated = true;
            return;
        }
        self.ids.insert(item_id.to_owned());
        self.id_bytes = self.id_bytes.saturating_add(item_id.len());
    }

    pub(in crate::backend) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(in crate::backend) fn is_scope(&self, thread_id: &str, turn_id: &str) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|(active_thread, active_turn)| {
                active_thread == thread_id && active_turn == turn_id
            })
    }
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    Browser(#[from] BrowserError),
    #[error("the connected app-server reported unsupported platform {0}")]
    UnsupportedPlatform(String),
    #[error("Codex provider is unavailable")]
    CodexUnavailable,
}

#[derive(Debug)]
pub enum BackendRuntimeEvent {
    Codex(Option<Result<SessionEvent, SessionError>>),
    OpenRouter(Option<OpenRouterServiceEvent>),
    Claude(Option<ClaudeServiceEvent>),
}

pub struct ClaudeBackendRuntime {
    pub(in crate::backend) service: ClaudeService,
    pub(in crate::backend) credentials: std::sync::Arc<dyn CredentialStore>,
    pub(in crate::backend) policy: ClaudeCliPolicy,
    pub(in crate::backend) next_operation_id: u64,
}

impl ClaudeBackendRuntime {
    pub fn new(
        service: ClaudeService,
        credentials: std::sync::Arc<dyn CredentialStore>,
        policy: ClaudeCliPolicy,
    ) -> Self {
        Self {
            service,
            credentials,
            policy,
            next_operation_id: 1,
        }
    }

    pub(in crate::backend) fn operation_id(&mut self) -> u64 {
        let id = self.next_operation_id;
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::backend) struct PendingOpenRouterAutoResume {
    pub(in crate::backend) operation_id: u64,
    pub(in crate::backend) conversation_id: crate::provider::OpenRouterConversationId,
    pub(in crate::backend) model_id: Option<String>,
}

pub struct BackendCoordinator<P, B> {
    pub(in crate::backend) state: AppState,
    pub(in crate::backend) session: Option<SessionService>,
    pub(in crate::backend) openrouter: Option<OpenRouterService>,
    pub(in crate::backend) claude: Option<ClaudeBackendRuntime>,
    pub(in crate::backend) preferences: P,
    pub(in crate::backend) browser: B,
    pub(in crate::backend) may_persist: bool,
    pub(in crate::backend) pending_openrouter_auto_resume: Option<PendingOpenRouterAutoResume>,
    pub(in crate::backend) completed_items: CompletedItemTracker,
}
