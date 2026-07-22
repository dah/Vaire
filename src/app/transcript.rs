use super::*;

pub(in crate::app) const MAX_TRANSCRIPT_BYTES: usize = 1024 * 1024;
pub(in crate::app) const MAX_TRANSCRIPT_ENTRIES: usize = 2048;
pub(in crate::app) const MAX_TRANSCRIPT_NEWLINES: usize = 16 * 1024;
pub(in crate::app) const MAX_TRANSCRIPT_DISPLAY_COLUMNS: usize = 512 * 1024;
// The interactive composer uses a tighter 128 KiB responsiveness bound. This reducer-level cap
// also covers programmatic intents; after sanitization, JSON escaping can at most double retained
// bytes, leaving ample envelope headroom under the transport's 1 MiB frame limit.
pub(in crate::app) const MAX_MESSAGE_BYTES: usize = 256 * 1024;
// This bounded non-cryptographic fingerprint detects accidental stream/snapshot contradictions;
// it is not used as an authenticity or security primitive.
const TRANSCRIPT_HASH_OFFSET: u64 = 0xcbf29ce484222325;
const TRANSCRIPT_HASH_PRIME: u64 = 0x100000001b3;

fn extend_transcript_hash(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(TRANSCRIPT_HASH_PRIME);
    }
    hash
}

fn trim_bytes_from_front(
    value: &mut String,
    minimum_bytes: usize,
    prefix_hash: u64,
) -> (usize, u64) {
    if minimum_bytes == 0 {
        return (0, prefix_hash);
    }
    if minimum_bytes >= value.len() {
        let removed = value.len();
        let hash = extend_transcript_hash(prefix_hash, value.as_bytes());
        *value = String::new();
        return (removed, hash);
    }
    let mut byte_index = minimum_bytes;
    while !value.is_char_boundary(byte_index) {
        byte_index += 1;
    }
    let hash = extend_transcript_hash(prefix_hash, &value.as_bytes()[..byte_index]);
    *value = value[byte_index..].to_owned();
    (byte_index, hash)
}

fn prefix_bytes_for_newlines(entries: &[TranscriptEntry], mut newlines: usize) -> usize {
    if newlines == 0 {
        return 0;
    }
    let mut bytes = 0usize;
    for entry in entries {
        for (index, byte) in entry.text.bytes().enumerate() {
            if byte == b'\n' {
                newlines -= 1;
                if newlines == 0 {
                    return bytes.saturating_add(index + 1);
                }
            }
        }
        bytes = bytes.saturating_add(entry.text.len());
    }
    bytes
}

fn prefix_bytes_for_display_width(entries: &[TranscriptEntry], mut width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    let mut bytes = 0usize;
    for entry in entries {
        for (index, grapheme) in entry.text.grapheme_indices(true) {
            width = width.saturating_sub(UnicodeWidthStr::width(grapheme));
            if width == 0 {
                return bytes.saturating_add(index + grapheme.len());
            }
        }
        bytes = bytes.saturating_add(entry.text.len());
    }
    bytes
}

fn transcript_item_key(entry: &TranscriptEntry) -> Option<(String, String)> {
    if entry.role != TranscriptRole::Assistant {
        return None;
    }
    Some((entry.turn_id.clone()?, entry.item_id.clone()?))
}

impl AppState {
    pub(in crate::app) fn replace_transcript(&mut self, history: Vec<TranscriptEntry>) {
        self.transcript = history;
        for entry in &mut self.transcript {
            entry.text = sanitize_terminal_text(&entry.text);
        }
        self.transcript_dropped_prefix_bytes.clear();
        self.enforce_transcript_bound();
    }

    pub(in crate::app) fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.transcript.shrink_to_fit();
        self.transcript_dropped_prefix_bytes.clear();
    }

    pub(in crate::app) fn enforce_transcript_bound(&mut self) {
        if self.transcript.len() > MAX_TRANSCRIPT_ENTRIES {
            let keep_from = self.transcript.len() - MAX_TRANSCRIPT_ENTRIES;
            for entry in &self.transcript[..keep_from] {
                if let Some(key) = transcript_item_key(entry) {
                    self.transcript_dropped_prefix_bytes.remove(&key);
                }
            }
            self.transcript = self.transcript.split_off(keep_from);
        }

        let (total_bytes, total_newlines, total_display_columns) = self.transcript.iter().fold(
            (0usize, 0usize, 0usize),
            |(bytes, newlines, columns), entry| {
                (
                    bytes.saturating_add(entry.text.len()),
                    newlines
                        .saturating_add(entry.text.bytes().filter(|byte| *byte == b'\n').count()),
                    columns.saturating_add(UnicodeWidthStr::width(entry.text.as_str())),
                )
            },
        );
        let byte_excess = total_bytes.saturating_sub(MAX_TRANSCRIPT_BYTES);
        let newline_prefix = prefix_bytes_for_newlines(
            &self.transcript,
            total_newlines.saturating_sub(MAX_TRANSCRIPT_NEWLINES),
        );
        let display_prefix = prefix_bytes_for_display_width(
            &self.transcript,
            total_display_columns.saturating_sub(MAX_TRANSCRIPT_DISPLAY_COLUMNS),
        );
        let mut excess = byte_excess.max(newline_prefix).max(display_prefix);
        if excess == 0 {
            return;
        }

        let mut drop_entries = 0;
        while let Some(entry) = self.transcript.get(drop_entries) {
            let entry_bytes = entry.text.len();
            if entry_bytes > excess {
                break;
            }
            if let Some(key) = transcript_item_key(entry) {
                self.transcript_dropped_prefix_bytes.remove(&key);
            }
            excess -= entry_bytes;
            drop_entries += 1;
        }
        if drop_entries > 0 {
            self.transcript = self.transcript.split_off(drop_entries);
        }
        if excess == 0 {
            return;
        }

        if let Some(entry) = self.transcript.first_mut() {
            let key = transcript_item_key(entry);
            if let Some(key) = key {
                let truncation = self.transcript_dropped_prefix_bytes.entry(key).or_insert(
                    TranscriptTruncation {
                        dropped_bytes: 0,
                        dropped_hash: TRANSCRIPT_HASH_OFFSET,
                    },
                );
                let (removed, hash) =
                    trim_bytes_from_front(&mut entry.text, excess, truncation.dropped_hash);
                truncation.dropped_bytes = truncation.dropped_bytes.saturating_add(removed);
                truncation.dropped_hash = hash;
            } else {
                let _ = trim_bytes_from_front(&mut entry.text, excess, TRANSCRIPT_HASH_OFFSET);
            }
        }
    }

    pub(in crate::app) fn append_delta(&mut self, turn_id: &str, item_id: &str, delta: &str) {
        let delta = sanitize_terminal_text(delta);
        if delta.is_empty() {
            return;
        }
        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            entry.text.push_str(&delta);
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: delta,
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
        }
        self.enforce_transcript_bound();
    }

    pub(in crate::app) fn reconcile_final(
        &mut self,
        turn_id: &str,
        item_id: &str,
        final_text: &str,
    ) -> Result<(), String> {
        let final_text = sanitize_terminal_text(final_text);
        let key = (turn_id.to_owned(), item_id.to_owned());
        if let Some(truncation) = self.transcript_dropped_prefix_bytes.remove(&key) {
            if let Some(entry) = self.transcript.iter_mut().find(|entry| {
                entry.item_id.as_deref() == Some(item_id)
                    && entry.turn_id.as_deref() == Some(turn_id)
            }) {
                let consistent = final_text
                    .get(..truncation.dropped_bytes)
                    .zip(final_text.get(truncation.dropped_bytes..))
                    .is_some_and(|(prefix, suffix)| {
                        extend_transcript_hash(TRANSCRIPT_HASH_OFFSET, prefix.as_bytes())
                            == truncation.dropped_hash
                            && suffix.starts_with(&entry.text)
                    });
                if !consistent {
                    return Err("assistant final snapshot contradicted streamed text".to_owned());
                }
                entry.text = final_text.clone();
            } else {
                self.transcript.push(TranscriptEntry {
                    role: TranscriptRole::Assistant,
                    text: final_text.clone(),
                    item_id: Some(item_id.to_owned()),
                    turn_id: Some(turn_id.to_owned()),
                });
            }
            self.enforce_transcript_bound();
            return Ok(());
        }

        if let Some(entry) = self.transcript.iter_mut().find(|entry| {
            entry.item_id.as_deref() == Some(item_id) && entry.turn_id.as_deref() == Some(turn_id)
        }) {
            if final_text.starts_with(&entry.text) {
                entry.text.push_str(&final_text[entry.text.len()..]);
                self.enforce_transcript_bound();
                Ok(())
            } else {
                Err("assistant final snapshot contradicted streamed text".to_owned())
            }
        } else {
            self.transcript.push(TranscriptEntry {
                role: TranscriptRole::Assistant,
                text: final_text,
                item_id: Some(item_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
            });
            self.enforce_transcript_bound();
            Ok(())
        }
    }
}
