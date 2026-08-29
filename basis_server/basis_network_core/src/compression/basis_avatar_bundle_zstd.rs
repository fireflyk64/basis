use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};

use parking_lot::{Mutex, RwLock};
use zstd::zstd_safe::{CCtx, CParameter, DCtx, DParameter, FrameFormat};

use super::basis_avatar_bundle_dictionary::BasisAvatarBundleDictionary;

/// Zstd half of the hybrid avatar-bundle codec, plus the flags byte that tells the two halves
/// apart on the wire.
///
/// Keyframe/full bundles measured 16.7-18.1% smaller with Zstd -2 against a 16 KiB trained
/// dictionary; delta-only bundles lose 2.8-4.5% against LZ4, so the server routes each bundle by
/// traffic class. Dictionary-less Zstd measured WORSE than LZ4 on this data, so with no dictionary
/// embedded this codec reports `available() == false` and the server stays on LZ4.
///
/// Frames are written magicless with no content size, no checksum and no dictionary id.
pub struct BasisAvatarBundleZstd;

struct Pooled<T> {
    value: T,
    epoch: i32,
}

static DICTIONARY: RwLock<Option<Vec<u8>>> = RwLock::new(None);
static DICTIONARY_GENERATION: AtomicU8 = AtomicU8::new(BasisAvatarBundleDictionary::GENERATION);
static DICTIONARY_OVERRIDDEN: AtomicU8 = AtomicU8::new(0);
static COMPRESSORS: Mutex<Vec<Pooled<CCtx<'static>>>> = Mutex::new(Vec::new());
static DECOMPRESSORS: Mutex<Vec<Pooled<DCtx<'static>>>> = Mutex::new(Vec::new());
static LEVEL: AtomicI32 = AtomicI32::new(BasisAvatarBundleZstd::DEFAULT_LEVEL);
static EPOCH: AtomicI32 = AtomicI32::new(0);

impl BasisAvatarBundleZstd {
    /// LZ4 block, K4os `L00_FAST`. The v50 codec; still used for delta-only bundles.
    pub const CODEC_LZ4: u8 = 0;
    /// Zstd magicless raw block against the embedded dictionary of the generation in the flags byte.
    pub const CODEC_ZSTD_DICT: u8 = 1;

    const CODEC_BITS: u32 = 3;
    const CODEC_MASK: u8 = (1 << Self::CODEC_BITS) - 1;
    const MAX_DICT_GENERATION: u8 = 0x1F;

    /// Packs a codec id and dictionary generation into the bundle's flags byte.
    pub fn pack_flags(codec: u8, dict_generation: u8) -> u8 {
        (codec & Self::CODEC_MASK) | ((dict_generation & Self::MAX_DICT_GENERATION) << Self::CODEC_BITS)
    }

    /// Codec id carried by a bundle flags byte.
    pub fn codec_of(flags: u8) -> u8 {
        flags & Self::CODEC_MASK
    }

    /// Dictionary generation carried by a bundle flags byte; 0 when the codec uses no dictionary.
    pub fn dict_generation_of(flags: u8) -> u8 {
        (flags >> Self::CODEC_BITS) & Self::MAX_DICT_GENERATION
    }

    /// 128 KiB ceiling on the window descriptor the frame header declares.
    const WINDOW_LOG: u32 = 17;

    /// Compression level the benchmark settled on. -3 costs less CPU but gives up ~2pp of ratio.
    pub const DEFAULT_LEVEL: i32 = -2;

    /// Lowest level zstd accepts.
    pub fn min_level() -> i32 {
        zstd::zstd_safe::min_c_level()
    }

    /// Highest level zstd accepts.
    pub fn max_level() -> i32 {
        zstd::zstd_safe::max_c_level()
    }

    fn dictionary() -> Vec<u8> {
        if DICTIONARY_OVERRIDDEN.load(Ordering::Acquire) != 0 {
            return DICTIONARY.read().clone().unwrap_or_default();
        }
        BasisAvatarBundleDictionary::bytes().to_vec()
    }

    /// Dictionary generation both ends compress and decompress against; 0 when none is embedded.
    pub fn dictionary_generation() -> u8 {
        DICTIONARY_GENERATION.load(Ordering::Acquire)
    }

    /// True when a dictionary is embedded and this codec is worth using.
    pub fn available() -> bool {
        Self::dictionary_generation() != 0 && !Self::dictionary().is_empty()
    }

    /// Test seam: swaps in a dictionary without rebuilding the generated file. Process-global;
    /// pair every call with [`Self::restore_embedded_dictionary_for_tests`].
    pub fn override_dictionary_for_tests(dictionary: &[u8], generation: u8) {
        *DICTIONARY.write() = Some(dictionary.to_vec());
        DICTIONARY_OVERRIDDEN.store(1, Ordering::Release);
        DICTIONARY_GENERATION.store(generation, Ordering::Release);
        // Pooled contexts hold the previous dictionary digested inside them.
        EPOCH.fetch_add(1, Ordering::SeqCst);
    }

    /// Puts the generated dictionary back after [`Self::override_dictionary_for_tests`].
    pub fn restore_embedded_dictionary_for_tests() {
        *DICTIONARY.write() = None;
        DICTIONARY_OVERRIDDEN.store(0, Ordering::Release);
        DICTIONARY_GENERATION.store(BasisAvatarBundleDictionary::GENERATION, Ordering::Release);
        EPOCH.fetch_add(1, Ordering::SeqCst);
    }

    /// A compressor carrying this codec's frame parameters and no dictionary.
    pub fn create_compressor(level: i32) -> Result<CCtx<'static>, String> {
        let mut c = CCtx::create();
        let set = |c: &mut CCtx<'static>, p: CParameter| c.set_parameter(p).map_err(|e| zstd::zstd_safe::get_error_name(e).to_string());
        set(&mut c, CParameter::CompressionLevel(level))?;
        set(&mut c, CParameter::ContentSizeFlag(false))?;
        set(&mut c, CParameter::ChecksumFlag(false))?;
        set(&mut c, CParameter::DictIdFlag(false))?;
        set(&mut c, CParameter::WindowLog(Self::WINDOW_LOG))?;
        set(&mut c, CParameter::Format(FrameFormat::Magicless))?;
        Ok(c)
    }

    /// Decompressor matching [`Self::create_compressor`]'s framing, no dictionary.
    pub fn create_decompressor() -> Result<DCtx<'static>, String> {
        let mut d = DCtx::create();
        d.set_parameter(DParameter::Format(FrameFormat::Magicless))
            .map_err(|e| zstd::zstd_safe::get_error_name(e).to_string())?;
        Ok(d)
    }

    /// Sets the compression level used by subsequent compressions. Pooled contexts built at the
    /// previous level are discarded as they are rented. No-op when unchanged.
    pub fn set_level(level: i32) {
        if LEVEL.load(Ordering::Acquire) == level {
            return;
        }
        LEVEL.store(level, Ordering::Release);
        EPOCH.fetch_add(1, Ordering::SeqCst);
    }

    /// Level currently in effect.
    pub fn level() -> i32 {
        LEVEL.load(Ordering::Acquire)
    }

    fn rent_compressor() -> Option<Pooled<CCtx<'static>>> {
        let epoch = EPOCH.load(Ordering::SeqCst);
        while let Some(pooled) = COMPRESSORS.lock().pop() {
            if pooled.epoch == epoch {
                return Some(pooled);
            }
        }
        let mut c = Self::create_compressor(LEVEL.load(Ordering::Acquire)).ok()?;
        // Digested once per context and retained across compressions.
        c.load_dictionary(&Self::dictionary()).ok()?;
        Some(Pooled { value: c, epoch })
    }

    fn rent_decompressor() -> Option<Pooled<DCtx<'static>>> {
        let epoch = EPOCH.load(Ordering::SeqCst);
        while let Some(pooled) = DECOMPRESSORS.lock().pop() {
            if pooled.epoch == epoch {
                return Some(pooled);
            }
        }
        let mut d = Self::create_decompressor().ok()?;
        d.load_dictionary(&Self::dictionary()).ok()?;
        Some(Pooled { value: d, epoch })
    }

    /// Worst-case compressed size for `raw_len` bytes.
    pub fn maximum_output_size(raw_len: usize) -> usize {
        zstd::zstd_safe::compress_bound(raw_len)
    }

    /// Compresses `raw` into `dst`. Returns the written length, or `None` when the result would
    /// not fit (the "overshoot, retry with a smaller chunk" signal the LZ4 path gives the packer).
    pub fn try_compress(raw: &[u8], dst: &mut [u8]) -> Option<usize> {
        if !Self::available() {
            return None;
        }
        let mut pooled = Self::rent_compressor()?;
        match pooled.value.compress2(dst, raw) {
            Ok(written) => {
                COMPRESSORS.lock().push(pooled);
                Some(written)
            }
            // A failing context is dropped rather than pooled; the caller falls back to an
            // uncompressed send.
            Err(_) => None,
        }
    }

    /// Decompresses one bundle payload. Returns `None` on any malformed input — this runs on
    /// network-supplied bytes, so a corrupt or hostile payload must drop the datagram.
    pub fn try_decompress(src: &[u8], dst: &mut [u8]) -> Option<usize> {
        if !Self::available() {
            return None;
        }
        let mut pooled = Self::rent_decompressor()?;
        match pooled.value.decompress(dst, src) {
            Ok(written) => {
                DECOMPRESSORS.lock().push(pooled);
                Some(written)
            }
            Err(_) => None,
        }
    }
}
