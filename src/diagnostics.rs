use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_DIAGNOSTIC_BYTES: u64 = 1024 * 1024;
const MAX_DIAGNOSTIC_RECORD_BYTES: u64 = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticEvent {
    pub category: &'static str,
    pub generation: u64,
    pub method: Option<String>,
    pub request_id: Option<u64>,
    pub byte_count: Option<usize>,
}

impl DiagnosticEvent {
    pub fn connection(category: &'static str, generation: u64) -> Self {
        Self {
            category,
            generation,
            method: None,
            request_id: None,
            byte_count: None,
        }
    }
}

pub trait DiagnosticSink: Send + Sync {
    fn record(&self, event: DiagnosticEvent);
}

#[derive(Default)]
pub struct NoopDiagnosticSink;

impl DiagnosticSink for NoopDiagnosticSink {
    fn record(&self, _event: DiagnosticEvent) {}
}

#[derive(Clone, Default)]
pub struct MemoryDiagnosticSink {
    events: Arc<Mutex<Vec<DiagnosticEvent>>>,
}

impl MemoryDiagnosticSink {
    pub fn events(&self) -> Vec<DiagnosticEvent> {
        self.events
            .lock()
            .expect("diagnostic lock poisoned")
            .clone()
    }
}

impl DiagnosticSink for MemoryDiagnosticSink {
    fn record(&self, event: DiagnosticEvent) {
        self.events
            .lock()
            .expect("diagnostic lock poisoned")
            .push(event);
    }
}

pub struct FileDiagnosticSink {
    file: Mutex<DiagnosticFile>,
}

struct DiagnosticFile {
    file: File,
    bytes_reserved: u64,
}

impl FileDiagnosticSink {
    pub fn create(path: &Path) -> io::Result<Self> {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "diagnostic path has no parent")
        })?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let mut bytes_reserved = file.metadata()?.len();
        if bytes_reserved.saturating_add(MAX_DIAGNOSTIC_RECORD_BYTES) > MAX_DIAGNOSTIC_BYTES {
            file.set_len(0)?;
            bytes_reserved = 0;
        }
        Ok(Self {
            file: Mutex::new(DiagnosticFile {
                file,
                bytes_reserved,
            }),
        })
    }
}

impl DiagnosticSink for FileDiagnosticSink {
    fn record(&self, event: DiagnosticEvent) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let method = event
            .method
            .as_deref()
            .map(sanitize_metadata)
            .unwrap_or_else(|| "-".to_owned());
        let line = format!(
            "time={timestamp} category={} generation={} method={} request_id={} bytes={}",
            sanitize_metadata(event.category),
            event.generation,
            method,
            event
                .request_id
                .map_or_else(|| "-".to_owned(), |id| id.to_string()),
            event
                .byte_count
                .map_or_else(|| "-".to_owned(), |count| count.to_string())
        );
        if let Ok(mut state) = self.file.lock() {
            let line_bytes = line.len() as u64 + 1;
            if state.bytes_reserved.saturating_add(line_bytes) > MAX_DIAGNOSTIC_BYTES {
                if state.file.set_len(0).is_err() {
                    return;
                }
                state.bytes_reserved = 0;
                const ROTATION_MARKER: &str = "diagnostics_rotated\n";
                if state.file.write_all(ROTATION_MARKER.as_bytes()).is_err() {
                    return;
                }
                state.bytes_reserved = ROTATION_MARKER.len() as u64;
            }
            if writeln!(state.file, "{line}").is_ok() {
                state.bytes_reserved += line_bytes;
                let _ = state.file.flush();
            }
        }
    }
}

fn sanitize_metadata(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '_' | '.' | ':' | '-')
        })
        .take(128)
        .collect()
}

pub fn redact_remote_message(_message: &str) -> String {
    "app-server returned an error; see the visible operation context".to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        redact_remote_message, DiagnosticEvent, DiagnosticSink, FileDiagnosticSink,
        MAX_DIAGNOSTIC_BYTES,
    };

    #[test]
    fn diagnostics_do_not_preserve_untrusted_payloads() {
        let redacted = redact_remote_message("token=secret prompt=private");
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("private"));
    }

    #[test]
    fn diagnostic_file_contains_only_allowlisted_metadata() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("diagnostics").join("vaire.log");
        let sink = FileDiagnosticSink::create(&path).unwrap();
        sink.record(DiagnosticEvent {
            category: "notice",
            generation: 2,
            method: Some("method\nTOKEN=secret\u{1b}".to_owned()),
            request_id: Some(3),
            byte_count: Some(4),
        });
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.contains("method=methodTOKENsecret"));
        assert!(contents.ends_with('\n'));
        assert!(!contents.contains('\u{1b}'));
    }

    #[test]
    fn diagnostic_file_stays_bounded_after_oversized_state_and_event_flood() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("diagnostics").join("vaire.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'x'; MAX_DIAGNOSTIC_BYTES as usize + 1]).unwrap();

        let sink = FileDiagnosticSink::create(&path).unwrap();
        for _ in 0..10_000 {
            sink.record(DiagnosticEvent {
                category: "stderr",
                generation: u64::MAX,
                method: Some("x".repeat(256)),
                request_id: Some(u64::MAX),
                byte_count: Some(usize::MAX),
            });
        }
        sink.record(DiagnosticEvent {
            category: "protocol_terminal",
            generation: 9,
            method: Some("turn/completed".to_owned()),
            request_id: Some(7),
            byte_count: None,
        });
        drop(sink);

        let contents = fs::read(&path).unwrap();
        assert!(contents.len() as u64 <= MAX_DIAGNOSTIC_BYTES);
        assert!(contents.ends_with(b"\n"));
        let text = String::from_utf8(contents).unwrap();
        assert!(text.contains("diagnostics_rotated"));
        assert!(text.contains("category=protocol_terminal"));
        assert!(text.contains("method=turn/completed"));
    }
}
