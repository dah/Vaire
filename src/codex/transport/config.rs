use super::*;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const EVENT_QUEUE_CAPACITY: usize = 64;
pub const MAX_PENDING_REQUESTS: usize = 128;
pub(in crate::codex::transport) const RETIRED_REQUEST_CAPACITY: usize = 1_024;
// Installed 0.144.6 schema notifications containing tool output the current UI does not render.
// Item/turn terminal events remain queued so the conversation lifecycle still completes.
pub(in crate::codex::transport) const UNRENDERED_TOOL_PROGRESS_NOTIFICATIONS: [&str; 7] = [
    "command/exec/outputDelta",
    "process/outputDelta",
    "turn/diff/updated",
    "item/commandExecution/outputDelta",
    "item/commandExecution/terminalInteraction",
    "item/fileChange/outputDelta",
    "item/fileChange/patchUpdated",
];
// Item lifecycle snapshots for these installed-schema variants are likewise not
// rendered. Drop only known tool variants: agent and reasoning completions carry
// user-visible state and must continue through the bounded queue.
pub(in crate::codex::transport) const UNRENDERED_TOOL_ITEM_TYPES: [&str; 10] = [
    "commandExecution",
    "fileChange",
    "mcpToolCall",
    "dynamicToolCall",
    "collabAgentToolCall",
    "subAgentActivity",
    "webSearch",
    "imageView",
    "sleep",
    "imageGeneration",
];
pub(in crate::codex::transport) const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
pub(in crate::codex::transport) const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
pub(in crate::codex::transport) static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct RequestTimeouts {
    pub initialize: Duration,
    pub auth: Duration,
    pub catalog: Duration,
    pub thread: Duration,
    pub turn: Duration,
    pub fallback: Duration,
}

impl Default for RequestTimeouts {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(10),
            auth: Duration::from_secs(30),
            catalog: Duration::from_secs(10),
            thread: Duration::from_secs(15),
            turn: Duration::from_secs(10),
            fallback: Duration::from_secs(10),
        }
    }
}

impl RequestTimeouts {
    pub fn for_method(&self, method: &str) -> Duration {
        match method {
            "initialize" => self.initialize,
            "account/read" | "account/login/start" | "account/login/cancel" | "account/logout" => {
                self.auth
            }
            "model/list" => self.catalog,
            "thread/start" | "thread/resume" | "thread/read" | "thread/list" | "thread/delete" => {
                self.thread
            }
            "turn/start" | "turn/interrupt" => self.turn,
            _ => self.fallback,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

impl ProcessSpec {
    pub fn codex(
        executable: impl Into<PathBuf>,
        paths: &IsolationPaths,
        policy: &FullAccessPolicy,
    ) -> Self {
        let mut env = vec![
            (
                OsString::from("CODEX_HOME"),
                paths.codex_home.as_os_str().to_owned(),
            ),
            (OsString::from("NO_COLOR"), OsString::from("1")),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            env.push((OsString::from("PATH"), path));
        }
        Self {
            executable: executable.into(),
            args: policy.app_server_args(),
            cwd: paths.conversation.clone(),
            env,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum TransportError {
    #[error("could not start app-server: {0}")]
    Spawn(String),
    #[error("app-server I/O failed: {0}")]
    Io(String),
    #[error("app-server protocol error: {0}")]
    Protocol(String),
    #[error("app-server returned error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("app-server request timed out")]
    Timeout,
    #[error("app-server connection closed")]
    Closed,
    #[error("app-server connection is unusable after safety violation: {0}")]
    SafetyViolation(String),
}
