/// Removes terminal control characters before text is retained, measured, or rendered.
/// Newlines remain as layout separators; tabs become spaces.
pub fn sanitize_terminal_text(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' => sanitized.push('\n'),
            '\t' => sanitized.push_str("    "),
            character if character.is_control() => {}
            character => sanitized.push(character),
        }
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::sanitize_terminal_text;

    #[test]
    fn removes_terminal_controls_and_preserves_layout_text() {
        assert_eq!(
            sanitize_terminal_text("a\r\u{1b}[31m\tb\nline\u{009b}"),
            "a[31m    b\nline"
        );
    }
}
