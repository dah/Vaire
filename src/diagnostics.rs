use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
    file: Mutex<File>,
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
        Ok(Self {
            file: Mutex::new(file),
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
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(
                file,
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
            let _ = file.flush();
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

    use super::{redact_remote_message, DiagnosticEvent, DiagnosticSink, FileDiagnosticSink};

    #[test]
    fn diagnostics_do_not_preserve_untrusted_payloads() {
        let redacted = redact_remote_message("token=secret prompt=private");
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("private"));
    }

    #[test]
    fn diagnostic_file_contains_only_allowlisted_metadata() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("diagnostics").join("agentharness.log");
        let sink = FileDiagnosticSink::create(&path).unwrap();
        sink.record(DiagnosticEvent {
            category: "notice",
            generation: 2,
            method: Some("method\nTOKEN=secret\u{1b}".to_owned()),
            request_id: Some(3),
            byte_count: Some(4),
        });
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("method=methodTOKENsecret"));
        assert!(!contents.contains('\n') || contents.ends_with('\n'));
        assert!(!contents.contains('\u{1b}'));
    }
}
