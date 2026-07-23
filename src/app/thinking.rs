use super::*;

pub(in crate::app) const MAX_THINKING_BYTES: usize = 32 * 1024;
pub(in crate::app) const MAX_THINKING_ENTRIES: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThinkingKind {
    Summary,
    EmittedText,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThinkingEntry {
    pub provider: ProviderId,
    pub turn_id: String,
    pub item_id: String,
    pub kind: ThinkingKind,
    pub index: i64,
    pub text: String,
    pub completed: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ThinkingState {
    pub visible: bool,
    pub entries: Vec<ThinkingEntry>,
}

impl ThinkingState {
    pub(in crate::app) fn clear_content(&mut self) {
        self.entries.clear();
    }

    pub(in crate::app) fn ensure_entry(
        &mut self,
        turn_id: &str,
        item_id: &str,
        kind: ThinkingKind,
        index: i64,
    ) -> Option<&mut ThinkingEntry> {
        if index < 0 {
            return None;
        }
        if let Some(position) = self.entries.iter().position(|entry| {
            entry.turn_id == turn_id
                && entry.item_id == item_id
                && entry.kind == kind
                && entry.index == index
        }) {
            return self.entries.get_mut(position);
        }
        if self.entries.len() >= MAX_THINKING_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push(ThinkingEntry {
            provider: crate::provider::ProviderId::Codex,
            turn_id: turn_id.to_owned(),
            item_id: item_id.to_owned(),
            kind,
            index,
            text: String::new(),
            completed: false,
        });
        self.entries.last_mut()
    }

    pub(in crate::app) fn add_part(&mut self, turn_id: &str, item_id: &str, index: i64) {
        self.ensure_entry(turn_id, item_id, ThinkingKind::Summary, index);
    }

    pub(in crate::app) fn append_delta(
        &mut self,
        turn_id: &str,
        item_id: &str,
        kind: ThinkingKind,
        index: i64,
        delta: &str,
    ) {
        let delta = sanitize_terminal_text(delta);
        if delta.is_empty() {
            return;
        }
        if let Some(entry) = self.ensure_entry(turn_id, item_id, kind, index) {
            entry.text.push_str(&delta);
        }
        self.enforce_bound();
    }

    pub(in crate::app) fn reconcile_item(
        &mut self,
        turn_id: &str,
        item_id: &str,
        summary: &[String],
        content: &[String],
    ) {
        for (index, final_text) in summary.iter().enumerate() {
            self.reconcile_entry(
                turn_id,
                item_id,
                ThinkingKind::Summary,
                index as i64,
                final_text,
            );
        }
        for (index, final_text) in content.iter().enumerate() {
            self.reconcile_entry(
                turn_id,
                item_id,
                ThinkingKind::EmittedText,
                index as i64,
                final_text,
            );
        }
        for entry in &mut self.entries {
            if entry.turn_id == turn_id && entry.item_id == item_id {
                entry.completed = true;
            }
        }
        self.enforce_bound();
    }

    fn reconcile_entry(
        &mut self,
        turn_id: &str,
        item_id: &str,
        kind: ThinkingKind,
        index: i64,
        final_text: &str,
    ) {
        let final_text = sanitize_terminal_text(final_text);
        let Some(entry) = self.ensure_entry(turn_id, item_id, kind, index) else {
            return;
        };
        if final_text.is_empty() {
            return;
        }
        // The completed item is authoritative. A matching stream receives only its missing
        // suffix; a contradictory stream is replaced so it can never be duplicated.
        if final_text.starts_with(&entry.text) {
            entry.text.push_str(&final_text[entry.text.len()..]);
        } else {
            entry.text = final_text;
        }
    }

    fn enforce_bound(&mut self) {
        let total = self
            .entries
            .iter()
            .map(|entry| entry.text.len())
            .fold(0usize, usize::saturating_add);
        let mut excess = total.saturating_sub(MAX_THINKING_BYTES);
        for entry in &mut self.entries {
            if excess == 0 {
                break;
            }
            let available = entry.text.len();
            let remove = available.min(excess);
            let removed = trim_utf8_bytes_from_front(&mut entry.text, remove);
            excess = excess.saturating_sub(removed);
        }
    }
}

fn trim_utf8_bytes_from_front(value: &mut String, minimum_bytes: usize) -> usize {
    if minimum_bytes == 0 {
        return 0;
    }
    if minimum_bytes >= value.len() {
        let removed = value.len();
        *value = String::new();
        return removed;
    }
    let mut byte_index = minimum_bytes;
    while !value.is_char_boundary(byte_index) {
        byte_index += 1;
    }
    *value = value[byte_index..].to_owned();
    byte_index
}
