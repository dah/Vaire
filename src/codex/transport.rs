use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tokio_util::codec::{FramedRead, LinesCodec};

use super::protocol::{classify_message, InboundEvent, InboundMessage, RequestId, RpcErrorObject};
use super::safety::{denial_response, FullAccessPolicy, IsolationPaths};
use crate::diagnostics::{
    redact_remote_message, DiagnosticEvent, DiagnosticSink, NoopDiagnosticSink,
};

mod actor;
mod channel;
mod client;
mod config;
mod framing;
mod io;

pub(in crate::codex::transport) use actor::*;
pub(in crate::codex::transport) use channel::*;
pub use client::{AppServerTransport, TransportEvent};
pub use config::{
    ProcessSpec, RequestTimeouts, TransportError, EVENT_QUEUE_CAPACITY, MAX_FRAME_BYTES,
    MAX_PENDING_REQUESTS,
};
pub(in crate::codex::transport) use config::{
    NEXT_GENERATION, RETIRED_REQUEST_CAPACITY, SHUTDOWN_GRACE, UNRENDERED_TOOL_ITEM_TYPES,
    UNRENDERED_TOOL_PROGRESS_NOTIFICATIONS, WRITE_TIMEOUT,
};
pub(in crate::codex::transport) use framing::*;
pub(in crate::codex::transport) use io::*;

#[cfg(test)]
mod tests;
