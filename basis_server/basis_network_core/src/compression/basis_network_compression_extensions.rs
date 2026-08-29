use super::basis_avatar_bit_packing::BasisAvatarBitPacking;
use crate::mathematics::Vector3;

/// The hips world-position field at the head of every avatar payload, in the int24-millimetre
/// form [`BasisAvatarBitPacking::WRITE_POSITION`] describes.
pub struct BasisNetworkCompressionExtensions;

impl BasisNetworkCompressionExtensions {
    /// Writes the position at `*offset` and advances it. False — nothing written, offset
    /// unchanged — when the buffer has no room.
    #[inline]
    pub fn write_position(position: Vector3, buffer: &mut [u8], offset: &mut usize) -> bool {
        if !BasisAvatarBitPacking::encode_position(position.x, position.y, position.z, buffer, *offset) {
            return false;
        }
        *offset += BasisAvatarBitPacking::WRITE_POSITION;
        true
    }

    /// `None` when the buffer is shorter than a position.
    #[inline]
    pub fn read_position(buffer: &[u8]) -> Option<Vector3> {
        let (x, y, z) = BasisAvatarBitPacking::decode_position(buffer, 0)?;
        Some(Vector3 { x, y, z })
    }
}
