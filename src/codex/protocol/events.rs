use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
    pub delta: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryPartAddedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub content_index: i64,
    pub delta: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ThreadItem,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    pub thread_id: String,
    pub turn: TurnSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub error: TurnError,
    pub will_retry: bool,
}

/// The required token total from one app-server usage breakdown.
///
/// The installed schema contains additional breakdown fields. Vairë only
/// needs each breakdown's required `totalTokens` value and deliberately ignores
/// the other counters at the protocol boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBreakdown {
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsage {
    /// Current context occupancy used for the remaining-context meter.
    pub last: TokenUsageBreakdown,
    /// Cumulative usage retained for schema validation, not context occupancy.
    pub total: TokenUsageBreakdown,
    #[serde(default)]
    pub model_context_window: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsageUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub token_usage: ThreadTokenUsage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolEvent {
    AccountLoginCompleted(AccountLoginCompletedNotification),
    AccountUpdated,
    ThreadStarted(ThreadSnapshot),
    TurnStarted(TurnNotification),
    AgentMessageDelta(AgentMessageDeltaNotification),
    ReasoningSummaryTextDelta(ReasoningSummaryTextDeltaNotification),
    ReasoningSummaryPartAdded(ReasoningSummaryPartAddedNotification),
    ReasoningTextDelta(ReasoningTextDeltaNotification),
    ItemCompleted(ItemCompletedNotification),
    TurnCompleted(TurnNotification),
    ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification),
    Error(ErrorNotification),
}
