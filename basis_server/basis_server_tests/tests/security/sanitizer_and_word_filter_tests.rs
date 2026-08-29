//! Chat/display-name sanitization and word-filter behaviour. Chat path: word filter then chat
//! sanitizer. Connect path: display-name sanitizer, empty result rejects the peer. All non-ASCII
//! test data is written as escapes so the source stays encoding-proof.

use basis_network_core::SerializableBasis::ChatMessage;
use basis_network_core::sanitization::{BasisChatSanitizer, BasisDisplayNameSanitizer};
use basis_network_server::networking::basis_network_chat::BasisNetworkChat;
use basis_network_server::networking::basis_word_filter::BasisWordFilter;
use serial_test::serial;

const THUMBS_UP: &str = "\u{1F44D}"; // 4 UTF-8 bytes, 2 UTF-16 units
const CJK: char = '\u{597D}'; // 3 UTF-8 bytes

fn list(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn banned(text: &str, words: &[&str]) -> Option<String> {
    BasisWordFilter::contains_banned_word(text, &list(words))
}

fn filter(text: &str, words: &[&str]) -> String {
    BasisWordFilter::filter(text, &list(words))
}

// ---------------- BasisChatSanitizer (transport limits only) ----------------

#[test]
fn chat_sanitizer_clean_text_passes_unchanged() {
    for message in ["hello world", "The quick brown fox jumps over the lazy dog.", "punctuation !?~ 123 :)", "x"] {
        assert_eq!(BasisChatSanitizer::sanitize(message), message);
    }
}

#[test]
fn chat_sanitizer_empty_returns_empty() {
    assert_eq!(BasisChatSanitizer::sanitize(""), "");
}

// The chat sanitizer enforces length only; control/zero-width/RTL characters are intentionally
// left alone (the word filter and clients handle content concerns).
#[test]
fn chat_sanitizer_control_and_invisible_characters_pass_through() {
    for message in ["a\nb", "tab\tseparated", "zero\u{200B}width", "rtl\u{202E}override", "bell\u{0007}char"] {
        assert_eq!(BasisChatSanitizer::sanitize(message), message);
    }
}

#[test]
fn chat_sanitizer_legitimate_unicode_preserved() {
    for message in ["\u{4F60}\u{597D}\u{4E16}\u{754C}", "\u{3053}\u{3093}\u{306B}\u{3061}\u{306F}", "\u{1F44D}\u{1F389}", "caf\u{00E9} mixed \u{597D}"] {
        assert_eq!(BasisChatSanitizer::sanitize(message), message);
    }
}

#[test]
fn chat_sanitizer_at_character_cap_unchanged() {
    let message = "a".repeat(BasisChatSanitizer::MAX_MESSAGE_CHARACTERS);
    assert_eq!(BasisChatSanitizer::sanitize(&message), message);
}

#[test]
fn chat_sanitizer_over_character_cap_truncated_to_cap() {
    for length in [257, 300, 10000] {
        assert_eq!(BasisChatSanitizer::sanitize(&"a".repeat(length)), "a".repeat(BasisChatSanitizer::MAX_MESSAGE_CHARACTERS));
    }
}

#[test]
fn chat_sanitizer_truncation_does_not_split_surrogate_pair() {
    // 255 ASCII + one astral emoji = 257 UTF-16 units; cutting at 256 would land between the
    // surrogates, so the whole pair must be dropped instead.
    let result = BasisChatSanitizer::sanitize(&format!("{}{THUMBS_UP}", "a".repeat(255)));
    assert_eq!(result, "a".repeat(255));
}

#[test]
fn chat_sanitizer_emoji_message_clamps_to_whole_emoji_at_exact_byte_cap() {
    let input = THUMBS_UP.repeat(130);
    let result = BasisChatSanitizer::sanitize(&input);
    // 256 UTF-16 units = 128 emoji = exactly 512 UTF-8 bytes, which is allowed.
    assert_eq!(result, THUMBS_UP.repeat(128));
    assert_eq!(result.len(), BasisChatSanitizer::MAX_MESSAGE_BYTES);
}

#[test]
fn chat_sanitizer_cjk_over_byte_cap_trims_whole_characters() {
    // 256 chars * 3 bytes = 768 bytes; trims one scalar at a time down to 170 chars (510 bytes).
    let result = BasisChatSanitizer::sanitize(&CJK.to_string().repeat(256));
    assert_eq!(result, CJK.to_string().repeat(170));
    assert!(result.len() <= BasisChatSanitizer::MAX_MESSAGE_BYTES);
}

#[test]
fn chat_sanitizer_byte_trim_removes_whole_emoji_scalars() {
    // 250 CJK (750 bytes) + 3 emoji (12 bytes) = 256 units / 762 bytes: the three emoji must come
    // off as whole pairs, then CJK singles until under the byte cap.
    let input = format!("{}{THUMBS_UP}{THUMBS_UP}{THUMBS_UP}", CJK.to_string().repeat(250));
    assert_eq!(BasisChatSanitizer::sanitize(&input), CJK.to_string().repeat(170));
}

#[test]
fn chat_sanitizer_idempotent() {
    let inputs = ["hello world".to_string(), "a".repeat(300), CJK.to_string().repeat(256), THUMBS_UP.repeat(130), format!("{}{THUMBS_UP}{THUMBS_UP}{THUMBS_UP}", CJK.to_string().repeat(250))];
    for input in inputs {
        let once = BasisChatSanitizer::sanitize(&input);
        assert_eq!(BasisChatSanitizer::sanitize(&once), once);
    }
}

#[test]
fn chat_sanitizer_constants_match_chat_wire_contract() {
    assert_eq!(BasisChatSanitizer::MAX_MESSAGE_CHARACTERS, 256);
    assert_eq!(BasisChatSanitizer::MAX_MESSAGE_BYTES, 512);
    assert_eq!(ChatMessage::MAX_PAYLOAD_BYTES, BasisChatSanitizer::MAX_MESSAGE_BYTES);
}

// ---------------- BasisDisplayNameSanitizer ----------------

#[test]
fn display_name_clean_names_unchanged() {
    for name in ["PlayerOne", "Bob_42", "\u{73A9}\u{5BB6}\u{4E00}", "Alice\u{1F3AE}", "a"] {
        assert_eq!(BasisDisplayNameSanitizer::sanitize(name), name);
        assert!(BasisDisplayNameSanitizer::is_valid(name));
    }
}

#[test]
fn display_name_empty_returns_empty_and_invalid() {
    assert_eq!(BasisDisplayNameSanitizer::sanitize(""), "");
    assert!(!BasisDisplayNameSanitizer::is_valid(""));
}

#[test]
fn display_name_control_characters_removed() {
    for control in ['\u{0000}', '\u{0007}', '\u{001B}', '\u{007F}', '\u{009D}'] {
        assert_eq!(BasisDisplayNameSanitizer::sanitize(&format!("Pla{control}yer")), "Player", "{control:?}");
    }
}

#[test]
fn display_name_tabs_and_newlines_removed_as_controls_not_folded_to_space() {
    // Control check runs before the whitespace fold, so \t and \n vanish entirely.
    assert_eq!(BasisDisplayNameSanitizer::sanitize("a\tb"), "ab");
    assert_eq!(BasisDisplayNameSanitizer::sanitize("a\nb"), "ab");
    assert_eq!(BasisDisplayNameSanitizer::sanitize("a\r\nb"), "ab");
}

#[test]
fn display_name_format_characters_removed() {
    for format in ['\u{200B}', '\u{200C}', '\u{200D}', '\u{200E}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{FEFF}', '\u{00AD}'] {
        assert_eq!(BasisDisplayNameSanitizer::sanitize(&format!("Pla{format}yer")), "Player", "{format:?}");
    }
}

#[test]
fn display_name_known_invisible_glyphs_removed() {
    for glyph in ['\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}', '\u{2800}', '\u{180E}'] {
        assert_eq!(BasisDisplayNameSanitizer::sanitize(&format!("Pla{glyph}yer")), "Player", "{glyph:?}");
    }
}

// The connection handler rejects the connection when the sanitized name is empty.
#[test]
fn display_name_blank_after_sanitize_is_empty_and_invalid() {
    for name in ["   ", "\t\r\n", "\u{200B}\u{200B}", "\u{3164}\u{3164}", "\u{2800}\u{2800}\u{2800}", "\u{00A0}\u{00A0}", "\u{200B} \u{FEFF}"] {
        assert_eq!(BasisDisplayNameSanitizer::sanitize(name), "", "{name:?}");
        assert!(!BasisDisplayNameSanitizer::is_valid(name), "{name:?}");
    }
}

#[test]
fn display_name_unicode_whitespace_folded_to_plain_space() {
    for name in ["A\u{00A0}B", "A\u{3000}B", "A\u{2028}B"] {
        assert_eq!(BasisDisplayNameSanitizer::sanitize(name), "A B", "{name:?}");
    }
}

#[test]
fn display_name_outer_whitespace_trimmed() {
    for (name, expected) in [("  Alice  ", "Alice"), ("\u{00A0}Bob\u{3000}", "Bob"), ("\u{3000}\u{3000}Cara", "Cara")] {
        assert_eq!(BasisDisplayNameSanitizer::sanitize(name), expected);
    }
}

#[test]
fn display_name_interior_whitespace_runs_folded_but_not_collapsed() {
    assert_eq!(BasisDisplayNameSanitizer::sanitize("A \u{00A0} B"), "A   B");
}

#[test]
fn display_name_rtl_override_stripped_keeping_visible_text() {
    assert_eq!(BasisDisplayNameSanitizer::sanitize("abc\u{202E}def"), "abcdef");
}

#[test]
fn display_name_zwj_emoji_sequence_loses_joiner() {
    // Format stripping applies inside emoji ZWJ sequences too; the parts remain.
    assert_eq!(BasisDisplayNameSanitizer::sanitize("\u{1F468}\u{200D}\u{1F469}"), "\u{1F468}\u{1F469}");
}

#[test]
fn display_name_idempotent() {
    for input in ["  Alice  ", "Pla\u{200B}yer ", "A \u{00A0} B", "abc\u{202E}def", "\u{73A9}\u{5BB6}\u{4E00}", "\u{3164}\u{3164}"] {
        let once = BasisDisplayNameSanitizer::sanitize(input);
        assert_eq!(BasisDisplayNameSanitizer::sanitize(&once), once);
    }
}

// ---------------- BasisWordFilter ----------------
// Blacklist words below ("damn", "crap", "ass", "go die") are entries the server's default
// chat_word_filter.txt actually ships.

#[test]
fn word_filter_exact_word_detected() {
    assert_eq!(banned("damn", &["damn"]).as_deref(), Some("damn"));
}

#[test]
fn word_filter_exact_word_masked_with_asterisks() {
    assert_eq!(filter("damn", &["damn"]), "****");
    assert_eq!(filter("ass", &["ass"]), "***");
}

#[test]
fn word_filter_text_case_ignored() {
    for text in ["DAMN", "DaMn", "dAmN"] {
        assert!(banned(text, &["damn"]).is_some());
        assert_eq!(filter(text, &["damn"]), "****");
    }
}

#[test]
fn word_filter_word_inside_sentence_masked_in_place() {
    assert_eq!(filter("you damn fool", &["damn"]), "you **** fool");
}

#[test]
fn word_filter_word_at_start_and_end_of_message_masked() {
    assert_eq!(filter("damn that hurt", &["damn"]), "**** that hurt");
    assert_eq!(filter("that was damn", &["damn"]), "that was ****");
}

#[test]
fn word_filter_punctuation_and_space_adjacent_masked() {
    for (text, word, expected) in [("damn!", "damn", "****!"), ("(damn)", "damn", "(****)"), ("my ass.", "ass", "my ***."), ("my ass hurts", "ass", "my *** hurts")] {
        assert!(banned(text, &[word]).is_some(), "{text}");
        assert_eq!(filter(text, &[word]), expected);
    }
}

#[test]
fn word_filter_clean_sentences_pass() {
    for (text, word) in [("hello there friend", "damn"), ("The quick brown fox jumps over the lazy dog.", "damn"), ("a simple sentence", "ass"), ("hello there friend", "crap")] {
        assert!(banned(text, &[word]).is_none(), "{text}");
        assert_eq!(filter(text, &[word]), text);
    }
}

// Substring semantics as implemented: occurrences embedded in longer legitimate words are ignored
// via trigram context or the match-boundary check.
#[test]
fn word_filter_embedded_in_longer_word_not_flagged() {
    for (text, word) in [("assignment", "ass"), ("class", "ass"), ("bass", "ass"), ("assassinate", "ass"), ("scrape", "crap"), ("crappy", "crap"), ("damnation", "damn")] {
        assert!(banned(text, &[word]).is_none(), "{text}");
        assert_eq!(filter(text, &[word]), text);
    }
}

#[test]
fn word_filter_spaced_out_letters_detected() {
    assert!(banned("d a m n", &["damn"]).is_some());
    assert_eq!(filter("d a m n", &["damn"]), "* * * *");
}

#[test]
fn word_filter_punctuated_insertion_detected() {
    assert_eq!(filter("d.a.m.n", &["damn"]), "*.*.*.*");
}

#[test]
fn word_filter_zero_width_space_insertion_detected() {
    // U+200B is its own text element, so it is skipped like any inserted character; only the
    // matched letters are starred and the ZWSP survives in the output.
    assert!(banned("da\u{200B}mn", &["damn"]).is_some());
    assert_eq!(filter("da\u{200B}mn", &["damn"]), "**\u{200B}**");
}

#[test]
fn word_filter_homoglyph_and_leet_substitution_detected() {
    for (text, word) in [("d@mn", "damn"), ("d4mn", "damn"), ("d\u{03B1}mn", "damn"), ("\u{FF44}\u{FF41}\u{FF4D}\u{FF4E}", "damn"), ("cr4p", "crap")] {
        assert_eq!(banned(text, &[word]).as_deref(), Some(word), "{text}");
        assert_eq!(filter(text, &[word]), "****", "{text}");
    }
}

#[test]
fn word_filter_latin_diacritics_not_folded() {
    // U+00E2 is not in the homoglyph table for 'a'; the filter does no diacritic normalization.
    assert!(banned("d\u{00E2}mn", &["damn"]).is_none());
    assert_eq!(filter("d\u{00E2}mn", &["damn"]), "d\u{00E2}mn");
}

#[test]
fn word_filter_multi_word_phrase_detected_and_masked() {
    assert_eq!(banned("please go die now", &["go die"]).as_deref(), Some("go die"));
    // The phrase's interior space is part of the match and is starred too.
    assert_eq!(filter("please go die now", &["go die"]), "please ****** now");
}

#[test]
fn word_filter_phrase_words_far_apart_not_flagged() {
    assert!(banned("go outside and die", &["go die"]).is_none());
    assert_eq!(filter("go outside and die", &["go die"]), "go outside and die");
}

#[test]
fn word_filter_matched_word_reports_first_blacklist_entry_that_matches() {
    assert_eq!(banned("crap damn", &["damn", "crap"]).as_deref(), Some("damn"));
}

#[test]
fn word_filter_multiple_banned_words_all_masked() {
    assert_eq!(filter("damn crap", &["damn", "crap"]), "**** ****");
}

#[test]
fn word_filter_repeated_occurrences_all_masked() {
    assert_eq!(filter("damn damn damn", &["damn"]), "**** **** ****");
}

#[test]
fn word_filter_empty_inputs_safe_defaults() {
    assert!(banned("", &["damn"]).is_none());
    assert!(banned("damn", &[]).is_none());
    assert_eq!(filter("", &["damn"]), "");
    assert_eq!(filter("damn", &[]), "damn");
}

#[test]
fn word_filter_blank_blacklist_entries_ignored() {
    assert!(banned("damn", &[""]).is_none());
    assert_eq!(filter("damn", &[""]), "damn");
    assert_eq!(banned("damn", &["", "damn"]).as_deref(), Some("damn"));
}

#[test]
fn word_filter_long_clean_message_completes_unchanged() {
    // Sentence contains no 'a'/'d' (or their ASCII homoglyphs), so none of the words can ever
    // complete; pins that a large message is handled and untouched.
    let text = "we welcome everyone to the event tonight. ".repeat(120);
    let words = ["damn", "crap", "ass"];
    assert!(banned(&text, &words).is_none());
    assert_eq!(filter(&text, &words), text);
}

#[test]
fn word_filter_long_message_with_many_hits_all_masked() {
    let text = "damn ".repeat(100);
    let result = filter(&text, &["damn"]);
    assert_eq!(result, "**** ".repeat(100));
    assert_eq!(result.len(), text.len());
}

#[test]
fn word_filter_idempotent() {
    let words = ["damn", "crap", "ass"];
    for input in ["you damn fool", "d a m n", "damn crap", "assignment", "DAMN"] {
        let once = filter(input, &words);
        assert_eq!(filter(&once, &words), once);
    }
}

#[test]
#[serial(word_filter)]
fn server_chat_entry_point_filter_message_passthrough_when_no_list_loaded() {
    // With no list loaded the server entry point must forward messages untouched.
    BasisNetworkChat::load_word_filter_from_text("");
    assert_eq!(BasisNetworkChat::filter_message("damn message"), "damn message");
    assert_eq!(BasisNetworkChat::filter_message(""), "");
}

#[test]
#[serial(word_filter)]
fn server_chat_entry_point_filter_message_applies_a_loaded_list() {
    BasisNetworkChat::load_word_filter_from_text("damn\n\ncrap\n");
    assert_eq!(BasisNetworkChat::filter_message("damn message"), "**** message");
    BasisNetworkChat::load_word_filter_from_text("");
    assert_eq!(BasisNetworkChat::filter_message("damn message"), "damn message");
}
