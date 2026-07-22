/// Removes terminal control characters before text is retained, measured, or rendered.
/// Newlines remain as layout separators; tabs become spaces.
pub fn sanitize_terminal_text(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let complete = append_sanitized_terminal_text(&mut sanitized, value, usize::MAX);
    debug_assert!(complete);
    sanitized
}

/// Appends a sanitized prefix without allowing the destination to exceed `max_bytes`.
///
/// The limit is measured in UTF-8 bytes and input characters are appended atomically, so a
/// truncated value always remains valid UTF-8. The return value is `false` when visible input was
/// omitted because it would exceed the limit.
pub fn append_sanitized_terminal_text(
    destination: &mut String,
    value: &str,
    max_bytes: usize,
) -> bool {
    if destination.len() > max_bytes {
        return false;
    }
    for character in value.chars() {
        match character {
            '\n' => {
                if !append_if_fits(destination, "\n", max_bytes) {
                    return false;
                }
            }
            '\t' => {
                if !append_if_fits(destination, "    ", max_bytes) {
                    return false;
                }
            }
            character if is_terminal_unsafe(character) => {}
            character => {
                let mut encoded = [0_u8; 4];
                if !append_if_fits(destination, character.encode_utf8(&mut encoded), max_bytes) {
                    return false;
                }
            }
        }
    }
    true
}

fn append_if_fits(destination: &mut String, value: &str, max_bytes: usize) -> bool {
    if destination.len().saturating_add(value.len()) > max_bytes {
        return false;
    }
    destination.push_str(value);
    true
}

pub(crate) fn is_terminal_unsafe(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{206f}'
        )
}

#[cfg(test)]
mod tests {
    use super::{append_sanitized_terminal_text, sanitize_terminal_text};

    #[test]
    fn removes_terminal_controls_and_preserves_layout_text() {
        assert_eq!(
            sanitize_terminal_text("a\r\u{1b}[31m\tb\nline\u{009b}"),
            "a[31m    b\nline"
        );
    }

    #[test]
    fn removes_bidi_spoofing_controls_without_damaging_unicode_text() {
        assert_eq!(
            sanitize_terminal_text(
                "safe\u{061c}\u{200e}\u{200f}\u{202a}ltr\u{202c}\u{202e}rtl\u{202c}\u{2066}isolated\u{2069}\u{206a}\u{206f} e\u{301} \u{1f469}\u{200d}\u{1f4bb}"
            ),
            "safeltrrtlisolated e\u{301} \u{1f469}\u{200d}\u{1f4bb}"
        );
    }

    #[test]
    fn bounded_append_never_splits_a_utf8_character_or_tab_expansion() {
        let mut value = "ab".to_owned();
        assert!(!append_sanitized_terminal_text(&mut value, "界x", 4));
        assert_eq!(value, "ab");

        assert!(append_sanitized_terminal_text(&mut value, "\u{202e}\n", 4));
        assert_eq!(value, "ab\n");
        assert!(!append_sanitized_terminal_text(&mut value, "\t", 6));
        assert_eq!(value, "ab\n");
    }
}
