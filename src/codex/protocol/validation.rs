use super::*;

pub(in crate::codex) fn validate_thread_snapshot(thread: &ThreadSnapshot) -> Result<(), ()> {
    validate_scope(&[&thread.id])?;
    for turn in &thread.turns {
        validate_turn_snapshot(turn)?;
    }
    Ok(())
}

pub(in crate::codex) fn validate_turn_snapshot(turn: &TurnSnapshot) -> Result<(), ()> {
    validate_scope(&[&turn.id])?;
    for item in &turn.items {
        validate_thread_item(item)?;
    }
    Ok(())
}

pub(in crate::codex::protocol) fn validate_thread_item(item: &ThreadItem) -> Result<(), ()> {
    validate_scope(&[&item.id])?;
    if item.kind == "agentMessage" && item.text.is_none() {
        return Err(());
    }
    Ok(())
}

pub(in crate::codex::protocol) fn validate_scope(values: &[&str]) -> Result<(), ()> {
    if values.iter().any(|value| !valid_identifier(value)) {
        Err(())
    } else {
        Ok(())
    }
}

pub(in crate::codex) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value == value.trim()
        && value.len() <= MAX_PROTOCOL_IDENTIFIER_BYTES
        && !value.chars().any(is_terminal_unsafe)
}
