use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use crate::protocol::BasisCpuBudget;

const INDICES: usize = 256;

struct Stripe {
    in_count: [AtomicI64; INDICES],
    in_bytes: [AtomicI64; INDICES],
    out_count: [AtomicI64; INDICES],
    out_bytes: [AtomicI64; INDICES],
}

impl Stripe {
    fn new() -> Self {
        Self {
            in_count: std::array::from_fn(|_| AtomicI64::new(0)),
            in_bytes: std::array::from_fn(|_| AtomicI64::new(0)),
            out_count: std::array::from_fn(|_| AtomicI64::new(0)),
            out_bytes: std::array::from_fn(|_| AtomicI64::new(0)),
        }
    }
}

// More stripes -> less contention. Two per core, from the shared sizing helper so every
// contention table in the server is derived the same way.
static STRIPES: LazyLock<Vec<Stripe>> = LazyLock::new(|| {
    let count = BasisCpuBudget::concurrency_width(2, 16, 1024);
    (0..count).map(|_| Stripe::new()).collect()
});
static IS_RECORDING_DATA: AtomicBool = AtomicBool::new(false);
static NEXT_THREAD_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    // Thread-local stripe selection. 0 means "uninitialized".
    static STRIPE_PLUS_ONE: Cell<usize> = const { Cell::new(0) };
}

/// High-throughput, thread-safe network statistics with striped counters to minimize
/// contention. Tracks inbound/outbound counts and bytes per 0..255 message index, and supports
/// compact encoding/decoding to/from byte arrays (optionally deflated — the C# used Brotli,
/// which nothing decodes on the far side but the same code; deflate keeps the dependency
/// footprint to what the join batch already needs).
pub struct BasisNetworkStatistics;

impl BasisNetworkStatistics {
    pub fn is_recording_data() -> bool {
        IS_RECORDING_DATA.load(Ordering::Relaxed)
    }

    pub fn set_is_recording_data(value: bool) {
        IS_RECORDING_DATA.store(value, Ordering::Relaxed);
    }

    /// Record one inbound message for `index`, adding its encoded byte length.
    #[inline]
    pub fn record_inbound(index: u8, bytes_encoded: usize) {
        if !Self::is_recording_data() {
            return;
        }
        let s = &STRIPES[Self::ensure_stripe()];
        s.in_count[usize::from(index)].fetch_add(1, Ordering::Relaxed);
        s.in_bytes[usize::from(index)].fetch_add(bytes_encoded as i64, Ordering::Relaxed);
    }

    /// Record one outbound message for `index`, adding its encoded byte length.
    #[inline]
    pub fn record_outbound(index: u8, bytes_encoded: usize) {
        if !Self::is_recording_data() {
            return;
        }
        let s = &STRIPES[Self::ensure_stripe()];
        s.out_count[usize::from(index)].fetch_add(1, Ordering::Relaxed);
        s.out_bytes[usize::from(index)].fetch_add(bytes_encoded as i64, Ordering::Relaxed);
    }

    /// Record a batch of N outbound messages on the same `index`. Folds N×(add, add) into two.
    #[inline]
    pub fn record_outbound_batch(index: u8, count: i64, bytes_encoded: i64) {
        if !Self::is_recording_data() || count <= 0 {
            return;
        }
        let s = &STRIPES[Self::ensure_stripe()];
        s.out_count[usize::from(index)].fetch_add(count, Ordering::Relaxed);
        s.out_bytes[usize::from(index)].fetch_add(bytes_encoded, Ordering::Relaxed);
    }

    /// Non-destructive snapshot. Values may change during read, but each read is atomic.
    pub fn get_snapshot() -> Snapshot {
        Self::collect(false)
    }

    /// Atomic cut: collect and reset all counters without losing increments.
    pub fn snapshot_and_reset() -> Snapshot {
        Self::collect(true)
    }

    fn collect(reset: bool) -> Snapshot {
        let mut in_per_index = BTreeMap::new();
        let mut out_per_index = BTreeMap::new();
        let take = |a: &AtomicI64| if reset { a.swap(0, Ordering::Relaxed) } else { a.load(Ordering::Relaxed) };
        for i in 0..INDICES {
            let (mut in_count, mut in_bytes, mut out_count, mut out_bytes) = (0i64, 0i64, 0i64, 0i64);
            for s in STRIPES.iter() {
                in_count += take(&s.in_count[i]);
                in_bytes += take(&s.in_bytes[i]);
                out_count += take(&s.out_count[i]);
                out_bytes += take(&s.out_bytes[i]);
            }
            if (in_count | in_bytes) != 0 {
                in_per_index.insert(i as u8, IndexStats::new(in_count as u64, in_bytes as u64));
            }
            if (out_count | out_bytes) != 0 {
                out_per_index.insert(i as u8, IndexStats::new(out_count as u64, out_bytes as u64));
            }
        }
        Snapshot::new(in_per_index, out_per_index)
    }

    /// Zero everything.
    pub fn clear() {
        for s in STRIPES.iter() {
            for i in 0..INDICES {
                s.in_count[i].store(0, Ordering::Relaxed);
                s.in_bytes[i].store(0, Ordering::Relaxed);
                s.out_count[i].store(0, Ordering::Relaxed);
                s.out_bytes[i].store(0, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    fn ensure_stripe() -> usize {
        STRIPE_PLUS_ONE.with(|c| {
            let v = c.get();
            if v != 0 {
                return v - 1;
            }
            let stripe = Self::pick_stripe();
            c.set(stripe + 1);
            stripe
        })
    }

    #[inline]
    fn pick_stripe() -> usize {
        // Stable, cheap spread of threads across stripes.
        let id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed) as u32;
        let mut x = id;
        x ^= x >> 17;
        x = x.wrapping_mul(0xED5AD4BB);
        x ^= x >> 11;
        x = x.wrapping_mul(0xAC4C1B51);
        x ^= x >> 15;
        x = x.wrapping_mul(0x31848BAB);
        x ^= x >> 14;
        (x as usize) % STRIPES.len()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub count: u64,
    pub bytes: u64,
}

impl IndexStats {
    pub fn new(count: u64, bytes: u64) -> Self {
        Self { count, bytes }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Inbound (the C# "back-compat" `PerIndex`).
    pub per_index: BTreeMap<u8, IndexStats>,
    /// Outbound.
    pub out_per_index: BTreeMap<u8, IndexStats>,
}

impl Snapshot {
    pub fn new(in_per_index: BTreeMap<u8, IndexStats>, out_per_index: BTreeMap<u8, IndexStats>) -> Self {
        Self { per_index: in_per_index, out_per_index }
    }

    pub fn total_calls(&self) -> u64 {
        self.per_index.values().map(|s| s.count).sum()
    }

    pub fn out_total_calls(&self) -> u64 {
        self.out_per_index.values().map(|s| s.count).sum()
    }

    /// Take an atomic cut *and* reset the live counters, then encode & (optionally) compress.
    pub fn snapshot_reset_encode(compress: bool) -> std::io::Result<Vec<u8>> {
        let snap = BasisNetworkStatistics::snapshot_and_reset();
        let raw = Self::encode_snapshot(&snap);
        if compress { Self::deflate(&raw) } else { Ok(raw) }
    }

    /// Encode a non-destructive snapshot (no reset). Useful for debugging.
    pub fn encode_current(compress: bool) -> std::io::Result<Vec<u8>> {
        let snap = BasisNetworkStatistics::get_snapshot();
        let raw = Self::encode_snapshot(&snap);
        if compress { Self::deflate(&raw) } else { Ok(raw) }
    }

    /// Decode snapshot bytes (after optional decompression).
    pub fn decode(data: &[u8], compressed: bool) -> Result<Snapshot, StatisticsDecodeError> {
        let raw = if compressed { Self::inflate(data)? } else { data.to_vec() };
        Self::decode_snapshot(&raw)
    }

    fn encode_snapshot(s: &Snapshot) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        Self::write_map(&mut out, &s.per_index);
        Self::write_map(&mut out, &s.out_per_index);
        out
    }

    fn decode_snapshot(raw: &[u8]) -> Result<Snapshot, StatisticsDecodeError> {
        let mut r = SpanReader { span: raw, pos: 0 };
        let in_per = Self::read_map(&mut r)?;
        let out_per = Self::read_map(&mut r)?;
        Ok(Snapshot::new(in_per, out_per))
    }

    fn write_map(s: &mut Vec<u8>, map: &BTreeMap<u8, IndexStats>) {
        let n = map.values().filter(|v| (v.count | v.bytes) != 0).count();
        Self::write_uvar(s, n as u64);
        for (index, stats) in map {
            if (stats.count | stats.bytes) == 0 {
                continue;
            }
            s.push(*index);
            Self::write_uvar(s, stats.count);
            Self::write_uvar(s, stats.bytes);
        }
    }

    fn read_map(r: &mut SpanReader<'_>) -> Result<BTreeMap<u8, IndexStats>, StatisticsDecodeError> {
        let n = r.read_uvar32()?;
        let mut dict = BTreeMap::new();
        for _ in 0..n {
            let index = r.read_byte()?;
            let count = r.read_uvar()?;
            let bytes = r.read_uvar()?;
            dict.insert(index, IndexStats::new(count, bytes));
        }
        Ok(dict)
    }

    fn write_uvar(s: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            s.push(((value & 0x7F) | 0x80) as u8);
            value >>= 7;
        }
        s.push(value as u8);
    }

    fn deflate(raw: &[u8]) -> std::io::Result<Vec<u8>> {
        use std::io::Write;
        let mut e = flate2::write::DeflateEncoder::new(Vec::with_capacity(raw.len() / 2), flate2::Compression::default());
        e.write_all(raw)?;
        e.finish()
    }

    fn inflate(comp: &[u8]) -> Result<Vec<u8>, StatisticsDecodeError> {
        use std::io::Read;
        // A snapshot is a few KB; bound the inflate so a corrupt frame cannot balloon.
        let mut d = flate2::read::DeflateDecoder::new(comp).take(Self::MAX_INFLATED_BYTES as u64 + 1);
        let mut out = Vec::with_capacity(512);
        d.read_to_end(&mut out)?;
        if out.len() > Self::MAX_INFLATED_BYTES {
            return Err(StatisticsDecodeError::TooLarge { max: Self::MAX_INFLATED_BYTES });
        }
        Ok(out)
    }

    /// Largest decoded snapshot accepted: 64 indices × 2 maps × (1 + 10 + 10) bytes is ~2.7 KB.
    pub const MAX_INFLATED_BYTES: usize = 64 * 1024;
}

/// Why a statistics snapshot could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum StatisticsDecodeError {
    #[error("end of stream")]
    EndOfStream,
    #[error("varint too long")]
    VarintTooLong,
    #[error("uvar32 overflow")]
    Uvar32Overflow,
    #[error("snapshot larger than {max} bytes")]
    TooLarge { max: usize },
    #[error("inflate failed: {0}")]
    Inflate(#[from] std::io::Error),
}

struct SpanReader<'a> {
    span: &'a [u8],
    pos: usize,
}

impl SpanReader<'_> {
    fn read_byte(&mut self) -> Result<u8, StatisticsDecodeError> {
        let Some(&b) = self.span.get(self.pos) else {
            return Err(StatisticsDecodeError::EndOfStream);
        };
        self.pos += 1;
        Ok(b)
    }

    fn read_uvar(&mut self) -> Result<u64, StatisticsDecodeError> {
        let mut result = 0u64;
        let mut shift = 0;
        loop {
            let b = self.read_byte()?;
            result |= u64::from(b & 0x7F) << shift;
            if (b & 0x80) == 0 {
                return Ok(result);
            }
            shift += 7;
            if shift > 63 {
                return Err(StatisticsDecodeError::VarintTooLong);
            }
        }
    }

    fn read_uvar32(&mut self) -> Result<u32, StatisticsDecodeError> {
        let v = self.read_uvar()?;
        u32::try_from(v).map_err(|_| StatisticsDecodeError::Uvar32Overflow)
    }
}
