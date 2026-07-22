use super::*;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TurnError {
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserInput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

impl UserInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_owned(),
            text: Some(text.into()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ThreadItemContent {
    UserInput(UserInput),
    Text(String),
    Other(Value),
}

impl From<UserInput> for ThreadItemContent {
    fn from(value: UserInput) -> Self {
        Self::UserInput(value)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThreadItem {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub content: Vec<ThreadItemContent>,
    #[serde(default)]
    pub summary: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TurnSnapshot {
    pub id: String,
    pub items: Vec<ThreadItem>,
    pub status: TurnStatus,
    #[serde(default)]
    pub error: Option<TurnError>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThreadSnapshot {
    pub id: String,
    pub turns: Vec<TurnSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListEntry {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub preview: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub cwd: PathBuf,
    pub ephemeral: bool,
    pub source: SessionSource,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub enum SessionSource {
    Named(SessionSourceName),
    Custom {
        custom: String,
    },
    SubAgent {
        #[serde(rename = "subAgent")]
        sub_agent: Value,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SessionSourceName {
    Cli,
    Vscode,
    Exec,
    AppServer,
    Unknown,
    #[serde(other)]
    Other,
}

impl SessionSource {
    pub(in crate::codex) fn is_supported_resume_source(&self) -> bool {
        matches!(
            self,
            Self::Named(SessionSourceName::AppServer | SessionSourceName::Vscode)
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadSourceKind {
    AppServer,
    Vscode,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    pub source_kinds: Vec<ThreadSourceKind>,
    pub archived: bool,
    pub cursor: Option<String>,
    pub cwd: PathBuf,
    pub limit: u32,
    pub sort_direction: String,
    pub sort_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    pub data: Vec<ThreadListEntry>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDeleteParams {
    pub thread_id: String,
}

pub type ThreadDeleteResponse = EmptyObjectResponse;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmptyObjectResponse;

impl<'de> Deserialize<'de> for EmptyObjectResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EmptyObjectVisitor;

        impl<'de> Visitor<'de> for EmptyObjectVisitor {
            type Value = EmptyObjectResponse;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(EmptyObjectResponse)
            }
        }

        deserializer.deserialize_map(EmptyObjectVisitor)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    pub thread_source: ThreadSourceKind,
    pub approval_policy: String,
    pub config: Value,
    pub cwd: PathBuf,
    pub sandbox: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
    pub approval_policy: String,
    pub config: Value,
    pub cwd: PathBuf,
    pub sandbox: String,
    pub model: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadParams {
    pub thread_id: String,
    pub include_turns: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ThreadResponse {
    pub thread: ThreadSnapshot,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    pub model: String,
    pub effort: String,
    pub summary: ReasoningSummary,
    pub approval_policy: String,
    pub cwd: PathBuf,
    pub sandbox_policy: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReasoningSummary {
    Auto,
    Detailed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TurnStartResponse {
    pub turn: TurnSnapshot,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

pub type TurnInterruptResponse = EmptyObjectResponse;
