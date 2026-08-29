//! Micro-benchmarks for the packet buffer pool: every arm measures one of the transport's real
//! per-packet allocation shapes, pooled against the pre-pool behavior reconstructed inline
//! (`unpooled_*`). Run with `cargo bench -p basis_network_core --bench packet_pooling`.
//!
//! The interesting comparisons on this repository's 2-core box:
//! * `datagram_receive`: `NetPacket::from_slice` (pooled) vs `from_bytes(to_vec)` (the old
//!   receive path), both carried to the reader hand-off and dropped.
//! * `send_unreliable`: `NetPacket::with_payload` vs the old zeroed-Vec-then-copy build.
//! * `compact_merged_decode`: one 8-entry CompactMerged datagram decoded to readers — the old
//!   synthetic per-entry packet vs the pooled zero-copy view.
//! * `contended_2_threads`: both threads of this box churning buffers at once, pool vs malloc.

use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Barrier, OnceLock};
use std::time::{Duration, Instant};

use basis_network_core::io::NetPacketReader;
use basis_network_core::pooling::PacketBufferPool;
use basis_network_core::transport::lnl_network_impl::{CompactMerge, NetConstants, NetPacket, PacketProperty};
use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};

/// A deterministic payload of `len` bytes.
fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8).collect()
}

/// The old receive path: a fresh `Vec` per datagram, handed to the reader as `Bytes`.
fn unpooled_receive(datagram: &[u8]) -> NetPacketReader {
    let packet = NetPacket::from_bytes(datagram.to_vec());
    let size = packet.size();
    NetPacketReader::with_offset(packet.into_bytes(), NetConstants::UNRELIABLE_HEADER_SIZE, size)
}

/// The pooled receive path as `on_message_received` + `create_receive_event` now run it.
fn pooled_receive(datagram: &[u8]) -> NetPacketReader {
    let packet = NetPacket::from_slice(datagram);
    let size = packet.size();
    NetPacketReader::with_offset(packet.into_shared(), NetConstants::UNRELIABLE_HEADER_SIZE, size)
}

/// The old send build: zeroed Vec of the full size, then the payload copied over the zeros.
fn unpooled_send(data: &[u8], channel: u8) -> Vec<u8> {
    let header = NetConstants::UNRELIABLE_HEADER_SIZE;
    let mut packet = vec![0u8; header + data.len()];
    packet[0] = PacketProperty::Unreliable as u8;
    packet[1] = channel;
    packet[header..].copy_from_slice(data);
    packet
}

/// The pooled send build as `send_internal` now runs it.
fn pooled_send(data: &[u8], channel: u8) -> NetPacket {
    let mut packet = NetPacket::with_payload(PacketProperty::Unreliable, 0, data);
    if let Some(b) = packet.raw_mut().get_mut(1) {
        *b = channel;
    }
    packet
}

/// One CompactMerged datagram carrying `entries` unreliable entries of `entry_len` bytes.
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

fn bench_rent_shapes(c: &mut Criterion) {
    let mut group = c.benchmark_group("rent_vs_alloc");
    for len in [200usize, 1200, 1432] {
        let body = payload(len);
        group.bench_function(format!("pooled_copy_{len}"), |b| b.iter(|| PacketBufferPool::rent_copy(black_box(&body))));
        group.bench_function(format!("alloc_copy_{len}"), |b| b.iter(|| black_box(&body).to_vec()));
        group.bench_function(format!("pooled_zeroed_{len}"), |b| b.iter(|| PacketBufferPool::rent_zeroed(black_box(len))));
        group.bench_function(format!("alloc_zeroed_{len}"), |b| b.iter(|| vec![0u8; black_box(len)]));
    }
    let body = payload(1200);
    group.bench_function("pooled_frame_10_1200", |b| b.iter(|| PacketBufferPool::rent_frame(10, black_box(&body))));
    group.bench_function("alloc_frame_10_1200", |b| {
        b.iter(|| {
            let mut v = vec![0u8; 10 + body.len()];
            v[10..].copy_from_slice(black_box(&body));
            v
        })
    });
    group.finish();
}

fn bench_receive_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("datagram_receive");
    for len in [200usize, 1200] {
        let mut datagram = payload(len);
        datagram[0] = PacketProperty::Unreliable as u8;
        group.bench_function(format!("pooled_{len}"), |b| b.iter(|| pooled_receive(black_box(&datagram))));
        group.bench_function(format!("unpooled_{len}"), |b| b.iter(|| unpooled_receive(black_box(&datagram))));
    }
    group.finish();
}

fn bench_send_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("send_unreliable");
    for len in [120usize, 1200] {
        let body = payload(len);
        group.bench_function(format!("pooled_{len}"), |b| b.iter(|| pooled_send(black_box(&body), 3)));
        group.bench_function(format!("unpooled_{len}"), |b| b.iter(|| unpooled_send(black_box(&body), 3)));
    }
    group.finish();
}

fn bench_compact_merged_decode(c: &mut Criterion) {
    const ENTRIES: usize = 8;
    const ENTRY_LEN: usize = 120;
    let datagram = compact_merged_datagram(ENTRIES, ENTRY_LEN);
    let mut group = c.benchmark_group("compact_merged_decode");
    group.throughput(criterion::Throughput::Elements(ENTRIES as u64));

    // The pooled path: the datagram is one pooled buffer; each entry is a zero-copy view.
    group.bench_function("pooled_views", |b| {
        b.iter(|| {
            let raw: Bytes = NetPacket::from_slice(black_box(&datagram)).into_shared();
            let mut pos = NetConstants::HEADER_SIZE;
            let mut delivered = 0usize;
            while pos < raw.len() {
                let Some(entry) = CompactMerge::try_read_entry(&raw, raw.len(), &mut pos) else { break };
                let start = pos;
                pos += entry.payload_length;
                let reader = NetPacketReader::with_offset(raw.clone(), start, start + entry.payload_length);
                delivered += reader.user_data_size();
            }
            delivered
        })
    });

    // The old path: a fresh Vec for the datagram, then a synthetic packet per entry.
    group.bench_function("unpooled_packets", |b| {
        b.iter(|| {
            let packet = NetPacket::from_bytes(black_box(&datagram).to_vec());
            let raw = packet.into_bytes();
            let mut pos = NetConstants::HEADER_SIZE;
            let mut delivered = 0usize;
            while pos < raw.len() {
                let Some(entry) = CompactMerge::try_read_entry(&raw, raw.len(), &mut pos) else { break };
                let start = pos;
                pos += entry.payload_length;
                let mut synthetic = vec![0u8; NetConstants::UNRELIABLE_HEADER_SIZE + entry.payload_length];
                synthetic[0] = PacketProperty::Unreliable as u8;
                synthetic[1] = entry.channel;
                synthetic[NetConstants::UNRELIABLE_HEADER_SIZE..].copy_from_slice(&raw[start..start + entry.payload_length]);
                let size = synthetic.len();
                let reader = NetPacketReader::with_offset(synthetic, NetConstants::UNRELIABLE_HEADER_SIZE, size);
                delivered += reader.user_data_size();
            }
            delivered
        })
    });
    group.finish();
}

/// Two persistent workers — the shape of a real server's long-lived receive and logic threads,
/// whose thread-local caches stay warm — churning both of this box's cores at once. Each job
/// hands both workers `iters / 2` operations; the wall time covers both finishing.
struct ChurnWorkers {
    jobs: Vec<SyncSender<(u64, bool)>>,
    done: std::sync::Mutex<Receiver<()>>,
    start: std::sync::Arc<Barrier>,
}

static CHURN_OPS: AtomicU64 = AtomicU64::new(0);

fn churn_workers() -> &'static ChurnWorkers {
    static WORKERS: OnceLock<ChurnWorkers> = OnceLock::new();
    WORKERS.get_or_init(|| {
        let start = std::sync::Arc::new(Barrier::new(3));
        let (done_tx, done) = sync_channel(4);
        let mut jobs = Vec::new();
        for _ in 0..2 {
            let (job_tx, job_rx) = sync_channel::<(u64, bool)>(1);
            jobs.push(job_tx);
            let done_tx = done_tx.clone();
            let start = start.clone();
            std::thread::spawn(move || {
                let body = payload(1200);
                while let Ok((ops, pooled)) = job_rx.recv() {
                    start.wait();
                    if pooled {
                        for _ in 0..ops {
                            drop(black_box(PacketBufferPool::rent_copy(black_box(&body))));
                        }
                    } else {
                        for _ in 0..ops {
                            drop(black_box(black_box(&body[..]).to_vec()));
                        }
                    }
                    CHURN_OPS.fetch_add(ops, Ordering::Relaxed);
                    if done_tx.send(()).is_err() {
                        return;
                    }
                }
            });
        }
        ChurnWorkers { jobs, done: std::sync::Mutex::new(done), start }
    })
}

fn run_two_threads(iters: u64, pooled: bool) -> Duration {
    let workers = churn_workers();
    for job in &workers.jobs {
        if job.send((iters / 2, pooled)).is_err() {
            return Duration::ZERO;
        }
    }
    let Ok(done) = workers.done.lock() else {
        return Duration::ZERO;
    };
    workers.start.wait();
    let started = Instant::now();
    for _ in 0..workers.jobs.len() {
        if done.recv().is_err() {
            return Duration::ZERO;
        }
    }
    started.elapsed()
}

fn bench_contended(c: &mut Criterion) {
    let mut group = c.benchmark_group("contended_2_threads");
    group.bench_function("pooled_copy_1200", |b| b.iter_custom(|iters| run_two_threads(iters, true)));
    group.bench_function("alloc_copy_1200", |b| b.iter_custom(|iters| run_two_threads(iters, false)));
    group.finish();
}

criterion_group!(
    benches,
    bench_rent_shapes,
    bench_receive_path,
    bench_send_path,
    bench_compact_merged_decode,
    bench_contended
);
criterion_main!(benches);
