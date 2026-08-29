//! Counts heap allocations per packet operation, before and after pooling, with a counting
//! global allocator. This is the direct evidence for the change: the pooled paths' steady-state
//! allocation count and bytes per packet, printed as a table.
//!
//! Not a timing benchmark (the counting wrapper skews times) — run
//! `cargo bench -p basis_network_core --bench packet_pooling` for durations.
//!
//! Run: `cargo bench -p basis_network_core --bench allocations_per_packet`

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};

use basis_network_core::io::NetPacketReader;
use basis_network_core::pooling::PacketBufferPool;
use basis_network_core::transport::lnl_network_impl::{CompactMerge, NetConstants, NetPacket, PacketProperty};
use bytes::Bytes;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

// SAFETY: delegates every operation unchanged to `System`; the counters are side effects only.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTER: CountingAllocator = CountingAllocator;

const ITERATIONS: u64 = 100_000;

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8).collect()
}

/// Runs `op` `ITERATIONS` times after a warmup (which fills the pool to steady state) and
/// reports allocations and allocated bytes per operation.
fn measure(name: &str, mut op: impl FnMut()) {
    for _ in 0..1_000 {
        op();
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    for _ in 0..ITERATIONS {
        op();
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes;
    let per_op = allocations as f64 / ITERATIONS as f64;
    let bytes_per_op = bytes as f64 / ITERATIONS as f64;
    println!("{name:<44} {per_op:>10.3} {bytes_per_op:>14.1}");
    println!("RESULT {name} allocs_per_op={per_op:.3} bytes_per_op={bytes_per_op:.1}");
}

fn compact_merged_datagram(entries: usize, entry_len: usize) -> Vec<u8> {
    let body = payload(entry_len);
    let mut datagram = vec![0u8; NetConstants::HEADER_SIZE + entries * CompactMerge::entry_size(entry_len)];
    datagram[0] = PacketProperty::CompactMerged as u8;
    let mut offset = NetConstants::HEADER_SIZE;
    for channel in 0..entries {
        offset += CompactMerge::write_unreliable_entry(&mut datagram, offset, (channel % 4) as u8, &body);
    }
    datagram.truncate(offset);
    datagram
}

fn main() {
    println!("Heap allocations per packet operation ({ITERATIONS} iterations, steady state)");
    println!("{:<44} {:>10} {:>14}", "scenario", "allocs/op", "bytes/op");
    println!("{}", "-".repeat(70));

    let mut datagram = payload(1200);
    datagram[0] = PacketProperty::Unreliable as u8;

    // ── The receive path: datagram bytes → NetPacket → reader → dropped by the handler ──
    measure("receive_1200B/unpooled(from_bytes+to_vec)", || {
        let packet = NetPacket::from_bytes(black_box(&datagram).to_vec());
        let size = packet.size();
        black_box(NetPacketReader::with_offset(packet.into_bytes(), NetConstants::UNRELIABLE_HEADER_SIZE, size));
    });
    measure("receive_1200B/pooled(from_slice)", || {
        let packet = NetPacket::from_slice(black_box(&datagram));
        let size = packet.size();
        black_box(NetPacketReader::with_offset(packet.into_shared(), NetConstants::UNRELIABLE_HEADER_SIZE, size));
    });

    // ── The send path: user bytes → wire-shaped packet → sent (dropped) ──
    let body = payload(120);
    measure("send_120B/unpooled(zeroed Vec + copy)", || {
        let header = NetConstants::UNRELIABLE_HEADER_SIZE;
        let mut packet = vec![0u8; header + body.len()];
        packet[0] = PacketProperty::Unreliable as u8;
        packet[1] = 3;
        packet[header..].copy_from_slice(black_box(&body));
        black_box(packet);
    });
    measure("send_120B/pooled(with_payload)", || {
        let mut packet = NetPacket::with_payload(PacketProperty::Unreliable, 0, black_box(&body));
        if let Some(b) = packet.raw_mut().get_mut(1) {
            *b = 3;
        }
        black_box(packet);
    });

    // ── One CompactMerged datagram, 8 unreliable entries of 120 B, decoded to readers ──
    let merged = compact_merged_datagram(8, 120);
    measure("merged_8_entries/unpooled(packet per entry)", || {
        let packet = NetPacket::from_bytes(black_box(&merged).to_vec());
        let raw = packet.into_bytes();
        let mut pos = NetConstants::HEADER_SIZE;
        while pos < raw.len() {
            let Some(entry) = CompactMerge::try_read_entry(&raw, raw.len(), &mut pos) else { break };
            let start = pos;
            pos += entry.payload_length;
            let mut synthetic = vec![0u8; NetConstants::UNRELIABLE_HEADER_SIZE + entry.payload_length];
            synthetic[0] = PacketProperty::Unreliable as u8;
            synthetic[1] = entry.channel;
            synthetic[NetConstants::UNRELIABLE_HEADER_SIZE..].copy_from_slice(&raw[start..start + entry.payload_length]);
            let size = synthetic.len();
            black_box(NetPacketReader::with_offset(synthetic, NetConstants::UNRELIABLE_HEADER_SIZE, size));
        }
    });
    measure("merged_8_entries/pooled(shared views)", || {
        let raw: Bytes = NetPacket::from_slice(black_box(&merged)).into_shared();
        let mut pos = NetConstants::HEADER_SIZE;
        while pos < raw.len() {
            let Some(entry) = CompactMerge::try_read_entry(&raw, raw.len(), &mut pos) else { break };
            let start = pos;
            pos += entry.payload_length;
            black_box(NetPacketReader::with_offset(raw.clone(), start, start + entry.payload_length));
        }
    });

    // ── The iroh unreliable frame: header prefix + payload, shipped as Bytes ──
    measure("iroh_frame_1200B/unpooled(BytesMut+freeze)", || {
        let mut frame = bytes::BytesMut::with_capacity(black_box(&datagram).len() + 3);
        frame.extend_from_slice(&[7u8, 1, 2]);
        frame.extend_from_slice(black_box(&datagram));
        black_box(frame.freeze());
    });
    measure("iroh_frame_1200B/pooled(rent_frame)", || {
        let mut frame = PacketBufferPool::rent_frame(3, black_box(&datagram));
        if let Some(header) = frame.get_mut(0..3) {
            header.copy_from_slice(&[7u8, 1, 2]);
        }
        black_box(Bytes::from(frame));
    });

    let stats = PacketBufferPool::stats();
    println!("{}", "-".repeat(70));
    println!(
        "pool after run: reused_local={} reused={} allocated={} oversize={} recycled={} dropped_full={} resting={}",
        stats.reused_local,
        stats.reused,
        stats.allocated,
        stats.oversize,
        stats.recycled,
        stats.dropped_full,
        PacketBufferPool::pooled_buffers()
    );
}
