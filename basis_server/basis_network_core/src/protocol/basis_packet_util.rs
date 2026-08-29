pub struct BasisPacketUtil;

impl BasisPacketUtil {
    pub fn validate_packet(new: u8, old: u8) -> bool {
        Self::is_newer(new, old) && new != old
    }

    /// Returns true if seq1 is newer than seq2
    pub fn is_newer(seq1: u8, seq2: u8) -> bool {
        seq1.wrapping_sub(seq2) < 128
    }
}
