use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterFailureCategory {
    MissingCredential,
    CredentialStore,
    InvalidRequest,
    Unauthorized,
    RateLimited,
    Network,
    Timeout,
    Cancelled,
    InvalidResponse,
    ResourceLimit,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenRouterStreamStage {
    ContentType,
    SseFrameLimit,
    SseUtf8,
    ChunkJson,
    ProviderErrorShape,
    CompletionShape,
    ChoiceCardinality,
    ChoiceIndex,
    ResponseId,
    Model,
    PostTerminal,
    AfterDone,
    PrematureEof,
    AssistantLimit,
    UsageDropped,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct OpenRouterFailure {
    category: OpenRouterFailureCategory,
    status: Option<u16>,
    stage: Option<OpenRouterStreamStage>,
}

impl OpenRouterFailure {
    pub fn new(category: OpenRouterFailureCategory) -> Self {
        Self {
            category,
            status: None,
            stage: None,
        }
    }

    pub(crate) fn with_status(category: OpenRouterFailureCategory, status: u16) -> Self {
        Self {
            category,
            status: Some(status),
            stage: None,
        }
    }

    pub(crate) fn at_stage(mut self, stage: OpenRouterStreamStage) -> Self {
        self.stage = Some(stage);
        self
    }

    pub fn category(self) -> OpenRouterFailureCategory {
        self.category
    }

    pub fn status(self) -> Option<u16> {
        self.status
    }

    pub fn stage(self) -> Option<OpenRouterStreamStage> {
        self.stage
    }
}

impl fmt::Debug for OpenRouterFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenRouterFailure")
            .field("category", &self.category)
            .field("status", &self.status)
            .field("stage", &self.stage)
            .finish()
    }
}

impl fmt::Display for OpenRouterFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "OpenRouter operation failed ({:?})",
            self.category
        )?;
        if let Some(stage) = self.stage {
            write!(formatter, " at stream stage {stage:?}")?;
        }
        Ok(())
    }
}

impl std::error::Error for OpenRouterFailure {}
