/// Normalizes player display names so a name that renders blank cannot slip through. Strips
/// control, format and known invisible glyphs, folds Unicode whitespace to spaces and trims.
/// Returns an empty string when nothing renderable remains.
pub struct BasisDisplayNameSanitizer;

impl BasisDisplayNameSanitizer {
    const INVISIBLE_GLYPHS: [char; 6] = ['\u{115F}', '\u{1160}', '\u{3164}', '\u{FFA0}', '\u{2800}', '\u{180E}'];

    pub fn sanitize(display_name: &str) -> String {
        if display_name.is_empty() {
            return String::new();
        }
        let mut builder = String::with_capacity(display_name.len());
        for character in display_name.chars() {
            if character.is_control() {
                continue;
            }
            if Self::is_format_category(character) {
                continue;
            }
            if Self::is_invisible_glyph(character) {
                continue;
            }
            builder.push(if character.is_whitespace() { ' ' } else { character });
        }
        builder.trim().to_string()
    }

    pub fn is_valid(display_name: &str) -> bool {
        !Self::sanitize(display_name).is_empty()
    }

    fn is_invisible_glyph(character: char) -> bool {
        Self::INVISIBLE_GLYPHS.contains(&character)
    }

    /// Unicode general category Cf (Format), the set `CharUnicodeInfo.GetUnicodeCategory`
    /// reported as `UnicodeCategory.Format`. Enumerated rather than pulled from a Unicode
    /// tables crate: it is a short, stable list.
    fn is_format_category(c: char) -> bool {
        matches!(u32::from(c),
            0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2
            | 0x180E | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
            | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
    }
}
