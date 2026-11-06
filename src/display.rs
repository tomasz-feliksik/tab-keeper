//! Making text written by websites safe to print on a person's terminal.

const REPLACEMENT: char = '\u{FFFD}';

/// A title can hold escape sequences that rewrite the terminal, and direction marks
/// that make text read differently from what it says. Neither survives this.
pub fn clean(text: &str, max_chars: usize) -> String {
    let mut cleaned = String::new();
    for (count, character) in text.chars().enumerate() {
        if count == max_chars {
            cleaned.push('\u{2026}');
            break;
        }
        cleaned.push(if is_unsafe(character) {
            REPLACEMENT
        } else {
            character
        });
    }
    cleaned
}

fn is_unsafe(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

/// The first `max_chars` characters, and whether anything was cut.
pub fn shortened(text: &str, max_chars: usize) -> (String, bool) {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => (text[..end].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_escape_sequences_cannot_reach_the_terminal() {
        let cleaned = clean("safe\u{1b}]0;owned\u{7}\u{1b}[2Jtext\r\n", 100);

        assert!(!cleaned.chars().any(char::is_control), "{cleaned:?}");
        assert!(cleaned.starts_with("safe"));
    }

    #[test]
    fn direction_marks_and_invisible_characters_are_replaced() {
        for character in ['\u{202E}', '\u{2066}', '\u{200B}', '\u{2028}', '\u{FEFF}'] {
            let cleaned = clean(&format!("a{character}b"), 100);

            assert_eq!(cleaned, "a\u{FFFD}b", "{:x}", character as u32);
        }
    }

    #[test]
    fn ordinary_text_is_left_alone_including_other_scripts() {
        assert_eq!(
            clean("Zażółć gęślą jaźń 日本語", 100),
            "Zażółć gęślą jaźń 日本語"
        );
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary_and_says_so() {
        assert_eq!(clean("ąęść", 2), "ąę\u{2026}");
        assert_eq!(clean("ąę", 2), "ąę");
    }

    #[test]
    fn shortening_reports_whether_it_cut_anything() {
        assert_eq!(shortened("ąęść", 2), ("ąę".to_owned(), true));
        assert_eq!(shortened("ąę", 2), ("ąę".to_owned(), false));
    }
}
