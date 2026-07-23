use super::*;

pub(in crate::codex::session) const MAX_PAGINATION_PAGES: usize = 256;
pub(in crate::codex::session) const MAX_MODEL_PAGE_ITEMS: usize = 1_024;
pub(in crate::codex::session) const MAX_THREAD_PAGE_ITEMS: usize = 50;
pub(in crate::codex::session) const MAX_PAGINATION_ITEMS: usize = 16_384;
pub(in crate::codex::session) const MAX_PAGINATION_RETAINED_BYTES: usize = 16 * 1024 * 1024;
pub(in crate::codex::session) const MAX_CURSOR_BYTES: usize = 16 * 1024;

#[derive(Default)]
pub(in crate::codex::session) struct PaginationBudget {
    items: usize,
    retained_bytes: usize,
}

impl PaginationBudget {
    pub(in crate::codex::session) fn retain(
        &mut self,
        method: &'static str,
        bytes: usize,
    ) -> Result<(), SessionError> {
        self.items = self.items.checked_add(1).ok_or_else(|| {
            SessionError::Protocol(format!("{method} exceeded the retained item limit"))
        })?;
        self.retained_bytes = self.retained_bytes.checked_add(bytes).ok_or_else(|| {
            SessionError::Protocol(format!("{method} exceeded the retained byte limit"))
        })?;
        if self.items > MAX_PAGINATION_ITEMS {
            return Err(SessionError::Protocol(format!(
                "{method} exceeded the retained item limit"
            )));
        }
        if self.retained_bytes > MAX_PAGINATION_RETAINED_BYTES {
            return Err(SessionError::Protocol(format!(
                "{method} exceeded the retained byte limit"
            )));
        }
        Ok(())
    }

    pub(in crate::codex::session) fn retain_additional_bytes(
        &mut self,
        method: &'static str,
        bytes: usize,
    ) -> Result<(), SessionError> {
        self.retained_bytes = self.retained_bytes.checked_add(bytes).ok_or_else(|| {
            SessionError::Protocol(format!("{method} exceeded the retained byte limit"))
        })?;
        if self.retained_bytes > MAX_PAGINATION_RETAINED_BYTES {
            return Err(SessionError::Protocol(format!(
                "{method} exceeded the retained byte limit"
            )));
        }
        Ok(())
    }
}

pub(in crate::codex::session) fn decode<T: DeserializeOwned>(
    method: &'static str,
    value: Value,
) -> Result<T, SessionError> {
    serde_json::from_value(value).map_err(|_| SessionError::Decode { method })
}

pub(in crate::codex::session) fn next_cursor(
    method: &'static str,
    pages: usize,
    seen: &mut HashSet<String>,
    next: Option<String>,
) -> Result<Option<String>, SessionError> {
    let Some(next) = next else {
        return Ok(None);
    };
    if next.is_empty() {
        return Err(SessionError::Protocol(format!(
            "{method} returned an empty pagination cursor"
        )));
    }
    if next.len() > MAX_CURSOR_BYTES || next.chars().any(crate::text::is_terminal_unsafe) {
        return Err(SessionError::Protocol(format!(
            "{method} returned an invalid pagination cursor"
        )));
    }
    if seen.contains(&next) {
        return Err(SessionError::Protocol(format!(
            "{method} returned a cursor cycle"
        )));
    }
    if pages >= MAX_PAGINATION_PAGES {
        return Err(SessionError::Protocol(format!(
            "{method} exceeded the pagination limit"
        )));
    }
    seen.insert(next.clone());
    Ok(Some(next))
}

pub(in crate::codex::session) fn validate_page_len(
    method: &'static str,
    items: usize,
    maximum: usize,
) -> Result<(), SessionError> {
    if items > maximum {
        Err(SessionError::Protocol(format!(
            "{method} exceeded the page item limit"
        )))
    } else {
        Ok(())
    }
}

pub(in crate::codex::session) fn model_retained_bytes(model: &ModelInfo) -> usize {
    let option_bytes = model
        .supported_reasoning_efforts
        .iter()
        .fold(0usize, |total, option| {
            total
                .saturating_add(option.reasoning_effort.len())
                .saturating_add(option.description.len())
        });
    model
        .id
        .len()
        .saturating_mul(2)
        .saturating_add(model.display_name.len())
        .saturating_add(model.default_reasoning_effort.len())
        .saturating_add(option_bytes)
}

#[cfg(test)]
pub(in crate::codex::session) fn thread_retained_bytes(thread: &ThreadListEntry) -> usize {
    thread_origin_retained_bytes(thread).saturating_add(thread_additional_retained_bytes(thread))
}

pub(in crate::codex::session) fn thread_origin_retained_bytes(thread: &ThreadListEntry) -> usize {
    thread
        .id
        .len()
        .saturating_add(thread.cwd.to_string_lossy().len())
}

pub(in crate::codex::session) fn thread_additional_retained_bytes(
    thread: &ThreadListEntry,
) -> usize {
    thread
        .id
        .len()
        .saturating_add(thread.name.as_deref().map_or(0, str::len))
        .saturating_add(thread.preview.len())
        .saturating_add(thread.cwd.to_string_lossy().len())
}
