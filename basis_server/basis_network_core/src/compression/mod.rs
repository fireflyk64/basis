//! Port of `BasisNetworkCore/Compression`: the avatar pose codecs and the bundle compression
//! that sits above them. Everything here is pure arithmetic shared byte-for-byte with the C#
//! client, so the wire format is fixed by these files.

pub mod basis_avatar_bit_packing;
pub mod basis_avatar_bundle_codec;
pub mod basis_avatar_bundle_dictionary;
pub mod basis_avatar_bundle_zstd;
pub mod basis_avatar_channel_map;
pub mod basis_avatar_deadband;
pub mod basis_avatar_delta_compression;
pub mod basis_avatar_idle_suppression;
pub mod basis_bit_codec;
pub mod basis_bone_rotation_compression;
pub mod basis_generic_bone_rotation;
pub mod basis_network_compression_extensions;
pub mod basis_network_primitive_compression;
pub mod basis_object_pool;
pub mod basis_payload_diff;
pub mod basis_residual_codec;

pub use basis_avatar_bit_packing::{BasisAvatarBitPacking, BitQuality};
pub use basis_avatar_bundle_codec::BasisAvatarBundleCodec;
pub use basis_avatar_bundle_dictionary::BasisAvatarBundleDictionary;
pub use basis_avatar_bundle_zstd::BasisAvatarBundleZstd;
pub use basis_avatar_channel_map::{BasisAvatarChannel, BasisAvatarChannelLayout, BasisAvatarChannelMap, BasisChannelKind};
pub use basis_avatar_deadband::BasisAvatarDeadband;
pub use basis_avatar_delta_compression::BasisAvatarDeltaCompression;
pub use basis_avatar_idle_suppression::BasisAvatarIdleSuppression;
pub use basis_bit_codec::BasisBitCodec;
pub use basis_bone_rotation_compression::BasisBoneRotationCompression;
pub use basis_generic_bone_rotation::{BasisGenericBoneRotation, Quat};
pub use basis_network_compression_extensions::BasisNetworkCompressionExtensions;
pub use basis_network_primitive_compression::{BasisNetworkPrimitiveCompression, BasisRangedUshortFloatData};
pub use basis_object_pool::BasisObjectPool;
pub use basis_payload_diff::BasisPayloadDiff;
pub use basis_residual_codec::{BasisResidualCodec, BitReader, BitWriter};
