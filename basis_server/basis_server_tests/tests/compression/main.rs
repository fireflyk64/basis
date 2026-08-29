//! `BasisServerTests/Compression`: one test binary, one module per C# file.
//!
//! Not ported here: `CompactMerge*Tests` (LiteNetLib's merged-datagram framing, which the iroh
//! transport does not have — QUIC coalesces datagram frames itself) and the characterization
//! experiments (`PositionQuantizationExperiment`, `SimdCodecBenchmark`, `BundleCompressionExperiment`,
//! `BundleDictionaryTrainer`), which print measurements rather than assert behaviour.

mod basis_bit_codec_tests;
mod basis_payload_diff_tests;
mod bundle_grouping_tests;
mod core_primitive_compression_tests;
mod quant_rescale_table_tests;
mod residual_codec_tests;
mod restricted_dof_codec_tests;
