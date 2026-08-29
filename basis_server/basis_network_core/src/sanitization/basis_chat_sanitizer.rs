use crate::serializable::ChatMessage;

/// Applies Basis chat transport limits without producing invalid UTF-16/UTF-8.
pub struct BasisChatSanitizer;

impl BasisChatSanitizer {
    /// Counted in UTF-16 code units, like the C# `string.Length` it clamps.
    pub const MAX_MESSAGE_CHARACTERS: usize = 256;
    pub const MAX_MESSAGE_BYTES: usize = ChatMessage::MAX_PAYLOAD_BYTES;

    pub fn sanitize(message: &str) -> String {
        if message.is_empty() {
            return String::new();
        }
        let mut sanitized = Self::clamp_utf16_length(message, Self::MAX_MESSAGE_CHARACTERS);
        while !sanitized.is_empty() && sanitized.len() > Self::MAX_MESSAGE_BYTES {
            sanitized = Self::trim_last_scalar(&sanitized);
        }
        sanitized
    }

    /// Keeps the first `max_length` UTF-16 code units, never splitting a surrogate pair — a
    /// char over the BMP counts as two, exactly as `string.Length` counted it.
    fn clamp_utf16_length(text: &str, max_length: usize) -> String {
        let mut units = 0usize;
        let mut end = text.len();
        for (i, ch) in text.char_indices() {
            let w = ch.len_utf16();
            if units + w > max_length {
                end = i;
                break;
            }
            units += w;
        }
        text[..end].to_string()
    }

    fn trim_last_scalar(text: &str) -> String {
        let mut s = text.to_string();
        s.pop();
        s
    }
}
