pub(super) use std::ffi::OsString;
pub(super) use std::fs;
pub(super) use std::path::{Path, PathBuf};
pub(super) use std::sync::Arc;
pub(super) use std::time::Duration;

pub(super) use agentharness::codex::protocol::InboundEvent;
pub(super) use agentharness::codex::transport::{
    AppServerTransport, ProcessSpec, RequestTimeouts, TransportError, EVENT_QUEUE_CAPACITY,
    MAX_FRAME_BYTES, MAX_PENDING_REQUESTS,
};
pub(super) use agentharness::diagnostics::MemoryDiagnosticSink;
pub(super) use futures_util::{stream::FuturesUnordered, StreamExt};
pub(super) use serde_json::json;
pub(super) use tempfile::tempdir;

pub(super) use crate::shared_support::script;

pub(super) fn spec(root: &Path, executable: PathBuf) -> ProcessSpec {
    ProcessSpec {
        executable,
        args: Vec::new(),
        cwd: root.to_owned(),
        env: Vec::new(),
    }
}

pub(super) fn unrendered_tool_flood(count: usize) -> String {
    format!(
        r#"
for method in \
  command/exec/outputDelta \
  process/outputDelta \
  turn/diff/updated \
  item/commandExecution/outputDelta \
  item/commandExecution/terminalInteraction \
  item/fileChange/outputDelta \
  item/fileChange/patchUpdated
do
  i=0
  while [ "$i" -lt {count} ]; do
    printf '{{"method":"%s","params":{{"payload":"x"}}}}\n' "$method"
    i=$((i + 1))
  done
done
for lifecycle in item/started item/completed
do
  i=0
  while [ "$i" -lt {count} ]; do
    printf '{{"method":"%s","params":{{"threadId":"thr-tools","turnId":"turn-tools","item":{{"id":"command-%s","type":"commandExecution"}}}}}}\n' "$lifecycle" "$i"
    printf '{{"method":"%s","params":{{"threadId":"thr-tools","turnId":"turn-tools","item":{{"id":"file-%s","kind":"fileChange"}}}}}}\n' "$lifecycle" "$i"
    i=$((i + 1))
  done
done
"#
    )
}
