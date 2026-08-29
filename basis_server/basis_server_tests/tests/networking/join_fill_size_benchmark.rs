//! Sizes the join fill — what a client receives about everyone already present — at a
//! 2000-player scale. It exists because the intuitive answer here is wrong: batching and
//! compressing the fill looks like a ~20x win with empty pose buffers, but a real pose is
//! quantized bone rotations, which is close to incompressible, and the pose is roughly 70% of
//! every record. The three pose modes below bracket that.

use std::collections::HashMap;

use basis_network_core::NetDataWriter;
use basis_network_core::SerializableBasis::{ClientAvatarChangeMessage, ClientMetaDataMessage, LocalAvatarSyncMessage, PlayerIdMessage, ReadyMessage, ServerReadyBatchMessage, ServerReadyMessage};
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_server_tests::support::delta_test_support::TestRng;

const PLAYERS: usize = 2000;
const DISTINCT_AVATARS: usize = 54;

/// zeros — floor: unrealistic, every byte identical.
/// idle — realistic: a crowd sharing a handful of resting poses with small per-player jitter.
/// random — ceiling: every player mid-motion, quantized rotations with no shared structure.
fn pose(kind: &str, rng: &mut TestRng, player: usize, idle_poses: &mut HashMap<usize, Vec<u8>>) -> Vec<u8> {
    let n = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
    match kind {
        "zeros" => vec![0u8; n],
        "random" => rng.bytes(n),
        _ => {
            let slot = player % 20;
            let base = idle_poses.entry(slot).or_insert_with(|| TestRng::new(1000 + slot as u64).bytes(n)).clone();
            let mut copy = base;
            for _ in 0..8 {
                let at = rng.next(n);
                copy[at] = rng.next(256) as u8;
            }
            copy
        }
    }
}

/// Mirrors BasisAvatarNetworkLoad.EncodeToBytes: two strings, then raw Deflate on that one
/// record. Per-record compression is why the blob barely shrinks — the redundancy is across
/// players.
fn avatar_blob(avatar_index: usize) -> Vec<u8> {
    let url = format!("https://BasisFramework.b-cdn.net/Avatars/BEE/BEE/avatar{avatar_index}/{avatar_index:08x}20251003.BEE");
    let pw: String = std::iter::repeat_n((b'a' + (avatar_index % 6) as u8) as char, 64).collect();
    let mut raw = Vec::new();
    raw.extend_from_slice(&(url.len() as u16).to_le_bytes());
    raw.extend_from_slice(url.as_bytes());
    raw.extend_from_slice(&(pw.len() as u16).to_le_bytes());
    raw.extend_from_slice(pw.as_bytes());
    ServerReadyBatchMessage::deflate(&raw).expect("deflate")
}

fn join_fill_batching_beats_per_packet(pose_kind: &str) {
    let mut rng = TestRng::new(7);
    let mut idle_poses = HashMap::new();
    let mut all = NetDataWriter::new();
    let mut per_packet_bytes = 0usize;

    for i in 0..PLAYERS {
        // Skewed avatar popularity, like a real public instance: a few avatars dominate.
        let avatar = ((rng.next_f64().powi(3) * DISTINCT_AVATARS as f64).floor() as usize).min(DISTINCT_AVATARS - 1);
        let mut srm = ServerReadyMessage {
            player_id_message: PlayerIdMessage { player_id: i as u16 },
            local_ready_message: ReadyMessage {
                player_meta_data_message: ClientMetaDataMessage {
                    player_uuid: (76561198000000000i64 + i as i64).to_string(),
                    player_display_name: format!("Player{i}"),
                    player_platform: if i % 3 == 0 { "Android".into() } else { "WindowsPlayer".into() },
                },
                client_avatar_change_message: ClientAvatarChangeMessage { load_mode: 1, byte_array: Some(avatar_blob(avatar)), local_avatar_index: i as u8, ..Default::default() },
                local_avatar_sync_message: LocalAvatarSyncMessage { data_quality_level: BitQuality::High as u8, array: Some(pose(pose_kind, &mut rng, i, &mut idle_poses)), ..Default::default() },
            },
        };
        let mut one = NetDataWriter::new();
        srm.serialize(&mut one).expect("serialize");
        per_packet_bytes += one.length();
        srm.serialize(&mut all).expect("serialize");
    }

    let payload = all.copy_data();
    let mut batched_bytes = 0usize;
    let mut offset = 0usize;
    let mut packets = 0usize;
    while offset < payload.len() {
        let chunk = ServerReadyBatchMessage::MAX_PAYLOAD_BYTES.min(payload.len() - offset);
        let mut batch = ServerReadyBatchMessage { count: 1, payload: payload[offset..offset + chunk].to_vec(), ..Default::default() };
        let mut bw = NetDataWriter::new();
        batch.serialize(&mut bw).expect("serialize batch");
        batched_bytes += bw.length();
        offset += chunk;
        packets += 1;
    }

    println!("pose={pose_kind:<7} per-packet {:>8.1} KB in {PLAYERS} packets", per_packet_bytes as f64 / 1024.0);
    println!("pose={pose_kind:<7} batched    {:>8.1} KB in {packets} packets   ({:.2}x bytes, {:.0}x fewer packets)", batched_bytes as f64 / 1024.0, per_packet_bytes as f64 / batched_bytes as f64, PLAYERS as f64 / packets as f64);

    // Bytes: guaranteed only in the loose sense, since an all-random crowd barely compresses.
    assert!(batched_bytes < per_packet_bytes, "batching must never be larger: {batched_bytes} vs {per_packet_bytes}");
    // Packet count is the robust win and holds regardless of how compressible the poses are — it
    // is what stops a joiner being buried under ~2000 reliable sends.
    assert!(packets * 10 < PLAYERS, "expected at least a 10x packet reduction, got {PLAYERS} -> {packets}");
}

#[test]
fn join_fill_batching_beats_per_packet_zeros() {
    join_fill_batching_beats_per_packet("zeros");
}

#[test]
fn join_fill_batching_beats_per_packet_idle() {
    join_fill_batching_beats_per_packet("idle");
}

#[test]
fn join_fill_batching_beats_per_packet_random() {
    join_fill_batching_beats_per_packet("random");
}
