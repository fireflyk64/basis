use super::basis_avatar_bit_packing::BasisAvatarBitPacking;
use crate::mathematics::Vector3;

/// The hips world-position field at the head of every avatar payload, in the int24-millimetre
/// form [`BasisAvatarBitPacking::WRITE_POSITION`] describes.
pub struct BasisNetworkCompressionExtensions;

impl BasisNetworkCompressionExtensions {
    #[inline]
    pub fn write_position(position: Vector3, buffer: &mut [u8], offset: &mut usize) {
        BasisAvatarBitPacking::encode_position(position.x, position.y, position.z, buffer, *offset);
        *offset += BasisAvatarBitPacking::WRITE_POSITION;
    }

    #[inline]
    pub fn read_position(buffer: &[u8]) -> Vector3 {
        let (x, y, z) = BasisAvatarBitPacking::decode_position(buffer, 0);
        Vector3 { x, y, z }
    }
}
