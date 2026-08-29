//! Port of `MovementSender.cs`: the per-client pose stream (keyframes plus v42 uplink deltas) and
//! the nested `VoiceSender` that models who talks, to whom, and when.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU8, AtomicU64, Ordering};
use std::time::Instant;

use basis_network_core::BNL;
use basis_network_core::SerializableBasis::{AdditionalAvatarData, LocalAvatarSyncMessage};
use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarDeltaCompression, BasisBoneRotationCompression, BitQuality};
use basis_network_core::mathematics::Vector3;
use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPeerRef};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use rand::{Rng, RngExt};

use crate::audio::microphone_capture::MicrophoneCapture;
use crate::avatar::fake_pose_generator::FakePoseGenerator;
use crate::client::client_manager::{ClientManager, ClientSlot};
use crate::client::config_manager::ConfigManager;
use crate::util::randomizer::Randomizer;

// Precomputed byte offsets into the packet for High quality
const ROTATION_REGION_OFFSET: usize = BasisAvatarBitPacking::WRITE_POSITION; // 9
fn scale_offset() -> usize {
    BasisAvatarBitPacking::WRITE_POSITION + BasisBoneRotationCompression::rotation_bytes(BitQuality::High)
}
// After flip: this is the HIPS WORLD rotation slot. 7-byte smallest-three quaternion.
fn hips_rotation_offset() -> usize {
    scale_offset() + BasisAvatarBitPacking::WRITE_SCALE
}
// 5 bytes — 3 signed 13-bit axes at ±1m. Default zero bytes already decode to zero delta.
fn hips_local_delta_offset() -> usize {
    hips_rotation_offset() + BasisAvatarBitPacking::WRITE_ROTATION
}
// 7-byte smallest-three quaternion for hips local-rotation delta. Default zero bytes do NOT
// decode to identity, so the test client writes an explicit identity once at init.
fn hips_local_rotation_offset() -> usize {
    hips_local_delta_offset() + BasisAvatarBitPacking::WRITE_HIPS_DELTA
}

pub struct PlayerData {
    pub writer: NetDataWriter,
    pub message: LocalAvatarSyncMessage,
    pub sequence_byte: u8,
    pub phase_offset: f32,
    // v42 uplink delta state — mirrors the real client: a full keyframe every
    // UPLINK_KEYFRAME_INTERVAL_MS on the High channel (which the server snapshots as the
    // baseline), dirty-mask deltas against it on DeltaAvatarChannel in between.
    pub baseline: Vec<u8>,
    pub baseline_seq: u8,
    pub has_baseline: bool,
    pub last_keyframe: Option<Instant>,
    pub delta_scratch: Vec<u8>,
    pub force_keyframe: bool,
    // Per-sender strictly-increasing face counter embedded in the synthetic AdditionalAvatarData
    // payload; the observer verifies monotonicity per sender.
    pub face_counter: u32,
}

struct MovementState {
    positions: Vec<Mutex<Vector3>>,
    players: Vec<Mutex<PlayerData>>,
}

static STATE: RwLock<Option<Arc<MovementState>>> = RwLock::new(None);
// Animation timer — shared across all players, per-player phase offsets provide variety
static ANIM_TIMER: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
/// Send v42 uplink deltas like a real client (false = legacy all-keyframe uploads).
static USE_UPLINK_DELTAS: AtomicBool = AtomicBool::new(true);
/// Attach a synthetic AdditionalAvatarData (face-tracking shaped) to every send. Off by default.
static EMIT_FACE_DATA: AtomicBool = AtomicBool::new(false);
/// BASIS_FACE_SPACING: pin client i at (i * spacing, 1, 0) and stop the random walk (f32 bits).
static PIN_SPACING_METERS: AtomicU32 = AtomicU32::new(0);

const UPLINK_KEYFRAME_INTERVAL_MS: u128 = 500;
// Precompute compressed scale once; reused for all messages.
const COMPRESSED_SCALE: u16 = 0x4000;

pub struct MovementSender;

impl MovementSender {
    pub fn use_uplink_deltas() -> bool {
        USE_UPLINK_DELTAS.load(Ordering::Relaxed)
    }

    pub fn set_use_uplink_deltas(value: bool) {
        USE_UPLINK_DELTAS.store(value, Ordering::Relaxed);
    }

    pub fn emit_face_data() -> bool {
        EMIT_FACE_DATA.load(Ordering::Relaxed)
    }

    pub fn set_emit_face_data(value: bool) {
        EMIT_FACE_DATA.store(value, Ordering::Relaxed);
    }

    pub fn pin_spacing_meters() -> f32 {
        f32::from_bits(PIN_SPACING_METERS.load(Ordering::Relaxed))
    }

    pub fn set_pin_spacing_meters(value: f32) {
        PIN_SPACING_METERS.store(value.to_bits(), Ordering::Relaxed);
    }

    fn state() -> Option<Arc<MovementState>> {
        STATE.read().clone()
    }

    /// Server NACK (DeltaControlUplinkKeyframeRequest) → next send is a keyframe.
    pub fn request_keyframe(index: usize) {
        if let Some(state) = Self::state()
            && let Some(player) = state.players.get(index)
        {
            player.lock().force_keyframe = true;
        }
    }

    pub fn initialize(client_count: usize) {
        let pin = Self::pin_spacing_meters();
        let radius = ConfigManager::current().spawn_radius_meters;
        let positions: Vec<Mutex<Vector3>> = (0..client_count)
            .map(|i| Mutex::new(if pin > 0.0 { Vector3::new(i as f32 * pin, 1.0, 0.0) } else { Randomizer::get_spawn_position(radius) }))
            .collect();
        let players: Vec<Mutex<PlayerData>> = positions.iter().map(|p| Mutex::new(Self::generate_at(*p.lock()))).collect();
        *STATE.write() = Some(Arc::new(MovementState { positions, players }));
    }

    /// The current position of one simulated client, when it exists.
    pub fn position(index: usize) -> Option<Vector3> {
        Self::state().and_then(|s| s.positions.get(index).map(|p| *p.lock()))
    }

    /// Builds a starting payload. Pass the player's index so the pose carries the position that
    /// player was actually spawned at — the server reads the join pose to decide what quality
    /// every other player should be sent at.
    pub fn generate(player_index: Option<usize>) -> PlayerData {
        let spawn = player_index.and_then(Self::position).unwrap_or_else(|| Randomizer::get_spawn_position(ConfigManager::current().spawn_radius_meters));
        Self::generate_at(spawn)
    }

    fn generate_at(spawn: Vector3) -> PlayerData {
        let mut message = LocalAvatarSyncMessage {
            data_quality_level: BitQuality::High as u8,
            additional_avatar_datas: None,
            additional_avatar_data_size: 0,
            linked_avatar_index: 0,
            array: Some(vec![0u8; ClientManager::size().max(BasisAvatarBitPacking::convert_to_size(BitQuality::High))]),
        };
        // Per-player random phase offset so idle animations aren't synchronized
        let phase = (rand::rng().random::<f64>() * std::f64::consts::PI * 2.0) as f32;
        // Build the full initial payload (position, bone rotations, scale, hips rotation)
        Self::write_initial_payload(&mut message, phase, spawn);
        PlayerData {
            writer: NetDataWriter::new(),
            message,
            sequence_byte: 0,
            phase_offset: phase,
            baseline: Vec::new(),
            baseline_seq: 0,
            has_baseline: false,
            last_keyframe: None,
            delta_scratch: Vec::new(),
            force_keyframe: false,
            face_counter: 0,
        }
    }

    fn write_initial_payload(message: &mut LocalAvatarSyncMessage, phase: f32, spawn: Vector3) {
        // Make sure buffer is correct size for High
        let size = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
        let array = message.array.get_or_insert_with(|| vec![0u8; size]);
        if array.len() != size {
            *array = vec![0u8; size];
        }
        let time = ANIM_TIMER.elapsed().as_secs_f64();

        // 1) Position (after the recent flip this is the HIPS WORLD position)
        Self::write_position(spawn, array, 0);
        // 2) Bone rotations: natural standing pose with idle animation
        FakePoseGenerator::write_bone_rotations(array, ROTATION_REGION_OFFSET, BitQuality::High, time, phase);
        // 3) Scale
        Self::write_scale_ushort(COMPRESSED_SCALE, array, scale_offset());
        // 4) Hips world rotation: slight body orientation
        FakePoseGenerator::write_compressed_hips_rotation(array, hips_rotation_offset(), time, phase);
        // 5) Hips local-position delta — left as zero bytes; the receiver's signed-short decode
        //    treats that as a zero delta.
        // 6) Hips local-rotation delta — must be an explicit identity, since smallest-three on
        //    all-zero bytes does NOT decode to identity. Set once here.
        Self::write_identity_quaternion(array, hips_local_rotation_offset());
    }

    /// Writes the identity quaternion (0,0,0,1) into a 7-byte smallest-three slot: index byte 3
    /// (drop w), three small components at the midpoint 32768 = 0x8000.
    fn write_identity_quaternion(dst: &mut [u8], offset: usize) {
        if dst.len() < offset + 7 {
            return;
        }
        dst[offset] = 3;
        dst[offset + 1] = 0x00;
        dst[offset + 2] = 0x80;
        dst[offset + 3] = 0x00;
        dst[offset + 4] = 0x80;
        dst[offset + 5] = 0x00;
        dst[offset + 6] = 0x80;
    }

    fn write_scale_ushort(value: u16, buffer: &mut [u8], byte_offset: usize) {
        if buffer.len() >= byte_offset + 2 {
            buffer[byte_offset] = value as u8;
            buffer[byte_offset + 1] = (value >> 8) as u8;
        }
    }

    pub fn write_position(position: Vector3, buffer: &mut [u8], offset: usize) -> usize {
        BasisAvatarBitPacking::encode_position(position.x, position.y, position.z, buffer, offset);
        offset + BasisAvatarBitPacking::WRITE_POSITION
    }

    pub fn process_single(peer: &NetPeerRef, index: usize) {
        let Some(state) = Self::state() else { return };
        let (Some(position_slot), Some(player_slot)) = (state.positions.get(index), state.players.get(index)) else {
            return;
        };
        let mut pd = player_slot.lock();
        let time = ANIM_TIMER.elapsed().as_secs_f64();
        let phase = pd.phase_offset;

        // Update position (held fixed when pinned to a distance tier)
        let position = {
            let mut p = position_slot.lock();
            if Self::pin_spacing_meters() <= 0.0 {
                *p = *p + Randomizer::get_random_offset();
            }
            *p
        };

        let PlayerData { message: msg, .. } = &mut *pd;
        let size = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
        let array = msg.array.get_or_insert_with(|| vec![0u8; size]);
        if array.len() != size {
            *array = vec![0u8; size];
        }
        // 1) Position (first 9 bytes)
        Self::write_position(position, array, 0);
        // 2) Animated bone rotations (natural pose + idle animation, every slot fresh per send)
        FakePoseGenerator::write_bone_rotations(array, ROTATION_REGION_OFFSET, BitQuality::High, time, phase);
        // 3) Scale unchanged
        // 4) Animated hips rotation
        FakePoseGenerator::write_compressed_hips_rotation(array, hips_rotation_offset(), time, phase);

        let seq = pd.sequence_byte;
        pd.sequence_byte = pd.sequence_byte.wrapping_add(1);

        // Face-data test mode: ride one AdditionalAvatarData on this frame, exactly like the real
        // client ships HVR high-frequency face variables (messageIndex 1, payload
        // [16][timing][counter…]). The per-sender counter lets the observer verify ordering.
        let has_additional = if Self::emit_face_data() {
            pd.face_counter = pd.face_counter.wrapping_add(1);
            let counter = (pd.face_counter & 0xFFFF) as u16;
            let array = vec![16u8, 1, (counter & 0xFF) as u8, ((counter >> 8) & 0xFF) as u8, 200, 150, 100];
            pd.message.additional_avatar_datas = Some(vec![AdditionalAvatarData { payload_size: array.len() as u8, message_index: 1, array: Some(array) }]);
            pd.message.linked_avatar_index = 0;
            true
        } else {
            pd.message.additional_avatar_datas = None;
            pd.message.additional_avatar_data_size = 0;
            false
        };

        let now = Instant::now();
        let payload_len = pd.message.array.as_ref().map(|a| a.len()).unwrap_or(0);
        let mut keyframe = !Self::use_uplink_deltas()
            || pd.force_keyframe
            || !pd.has_baseline
            || pd.baseline.len() != payload_len
            || pd.last_keyframe.is_none_or(|last| now.duration_since(last).as_millis() >= UPLINK_KEYFRAME_INTERVAL_MS);

        let mut delta_len = 0usize;
        if !keyframe {
            let cap = BasisAvatarDeltaCompression::max_delta_size(BitQuality::High);
            if pd.delta_scratch.len() < cap {
                pd.delta_scratch = vec![0u8; cap];
            }
            let PlayerData { baseline, delta_scratch, message, .. } = &mut *pd;
            let current = message.array.as_deref().unwrap_or(&[]);
            match BasisAvatarDeltaCompression::build_delta(baseline, current, BitQuality::High, delta_scratch, 0) {
                Some(len) if len < payload_len => delta_len = len,
                _ => keyframe = true,
            }
        }

        pd.writer.reset();
        if keyframe {
            // Full keyframe on the High channel — the server snapshots it as this sender's uplink
            // delta baseline. Odd channel when additional data rides along.
            let PlayerData { writer, message, .. } = &mut *pd;
            writer.put_byte(seq);
            if message.serialize_for_channel(writer, BitQuality::High).is_err() {
                return;
            }
            let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(BitQuality::High as i32, has_additional);
            let _ = peer.send_writer(writer, channel, DeliveryMethod::Unreliable);

            if Self::use_uplink_deltas() {
                let PlayerData { baseline, message, .. } = &mut *pd;
                let current = message.array.as_deref().unwrap_or(&[]);
                baseline.clear();
                baseline.extend_from_slice(current);
                pd.baseline_seq = seq;
                pd.has_baseline = true;
                pd.last_keyframe = Some(now);
                pd.force_keyframe = false;
            }
        } else {
            // v42 uplink delta: [hdr][seq][baseSeq][body][additional?] on DeltaAvatarChannel.
            let baseline_seq = pd.baseline_seq;
            let PlayerData { writer, message, delta_scratch, .. } = &mut *pd;
            writer.put_byte(BasisNetworkCommons::build_delta_header(BitQuality::High as i32, has_additional, false));
            writer.put_byte(seq);
            writer.put_byte(baseline_seq);
            writer.put_bytes(&delta_scratch[..delta_len]);
            if has_additional && message.serialize_additional_only(writer).is_err() {
                return;
            }
            let _ = peer.send_writer(writer, BasisNetworkCommons::DELTA_AVATAR_CHANNEL, DeliveryMethod::Unreliable);
        }
    }
}

/// Voice traffic. Basis culls voice on the CLIENT: each player tells the server which peers are
/// close enough to hear it, and the server routes only to that list. So the simulation builds a
/// recipient list from the spawn positions inside the audible radius, then transmits
/// Opus-sized frames on the voice channel. Only a slice of the crowd talks at once.
pub struct VoiceSender;

struct VoiceState {
    recipients: Vec<Mutex<Option<Arc<Vec<u16>>>>>,
    participates: Vec<bool>,
    talking: Vec<AtomicBool>,
    joins_chorus: Vec<bool>,
    next_switch_ms: Vec<Mutex<f64>>,
    seq: Vec<AtomicU8>,
    silent_units: Vec<AtomicI32>,
    mic_cursor: Vec<AtomicI64>,
    // Who each simulated client can currently hear, and when they were last seen. Keyed by the
    // server-assigned player id, so real players land in here exactly like simulated ones.
    audible: Vec<DashMap<u16, i64>>,
    built: AtomicI32,
    opus_frames: Vec<Vec<u8>>,
    opus_average_frame_bytes: usize,
    // Shared clock: chorus events are global, and the driver threads each have their own
    // stopwatch origin, so their elapsed values cannot be compared against one another.
    voice_clock: Instant,
    chorus_until_ms: AtomicU64,
    next_chorus_ms: AtomicU64,
    chorus_lock: Mutex<()>,
}

static VOICE: RwLock<Option<Arc<VoiceState>>> = RwLock::new(None);

thread_local! {
    // Scratch reused per driver thread so a rebuild allocates nothing on the hot path.
    static T_NEAR: std::cell::RefCell<HashSet<u16>> = std::cell::RefCell::new(HashSet::new());
    static T_SCRATCH: std::cell::RefCell<Vec<u16>> = const { std::cell::RefCell::new(Vec::new()) };
}

impl VoiceSender {
    fn state() -> Option<Arc<VoiceState>> {
        VOICE.read().clone()
    }

    /// Independent per-person bursts produce a smooth, low concurrency that never spikes — but
    /// crowds are correlated. Baseline conversation is punctuated by chorus events where most of
    /// the crowd talks at once.
    fn chorus_active(state: &VoiceState, now_ms: f64) -> bool {
        let config = ConfigManager::current();
        if !config.voice_chorus_enabled {
            return false;
        }
        if now_ms < f64::from_bits(state.chorus_until_ms.load(Ordering::Acquire)) {
            return true;
        }
        let next = f64::from_bits(state.next_chorus_ms.load(Ordering::Acquire));
        if now_ms < next {
            return false;
        }
        let _guard = state.chorus_lock.lock();
        if now_ms < f64::from_bits(state.chorus_until_ms.load(Ordering::Acquire)) {
            return true;
        }
        let next = f64::from_bits(state.next_chorus_ms.load(Ordering::Acquire));
        let mut rng = rand::rng();
        if next < 0.0 {
            // First scheduling pass: don't open the run mid-song.
            let min = config.voice_chorus_interval_min_ms;
            let max = (min + 1).max(config.voice_chorus_interval_max_ms);
            state.next_chorus_ms.store((now_ms + rng.random_range(min..max) as f64).to_bits(), Ordering::Release);
            return false;
        }
        if now_ms < next {
            return false;
        }
        let min = config.voice_chorus_duration_min_ms;
        let max = (min + 1).max(config.voice_chorus_duration_max_ms);
        let until = now_ms + rng.random_range(min..max) as f64;
        state.chorus_until_ms.store(until.to_bits(), Ordering::Release);
        let gap_min = config.voice_chorus_interval_min_ms;
        let gap_max = (gap_min + 1).max(config.voice_chorus_interval_max_ms);
        state.next_chorus_ms.store((until + rng.random_range(gap_min..gap_max) as f64).to_bits(), Ordering::Release);
        true
    }

    pub fn in_chorus() -> bool {
        Self::state().is_some_and(|s| f64::from_bits(s.chorus_until_ms.load(Ordering::Acquire)) > s.voice_clock.elapsed().as_secs_f64() * 1000.0)
    }

    pub fn opus_average_frame_bytes() -> usize {
        Self::state().map(|s| s.opus_average_frame_bytes).unwrap_or(0)
    }

    /// Real Opus rather than random bytes would be right: random payloads are the wrong size
    /// distribution and undecodable. This build carries no native Opus binding, so the fallback
    /// the C# used when libopus was missing is what runs: fixed-size synthetic frames, announced
    /// as such because the traffic shape is then only approximately right.
    fn build_opus_frames() -> (Vec<Vec<u8>>, usize) {
        let config = ConfigManager::current();
        BNL::log_error("Opus encoder unavailable (no native Opus binding in this build); falling back to fixed-size synthetic frames.");
        let mut fallback = vec![0u8; config.voice_bytes_per_frame.max(1) as usize];
        rand::rng().fill_bytes(&mut fallback);
        let len = fallback.len();
        (vec![fallback], len)
    }

    pub fn initialize(client_count: usize) {
        let config = ConfigManager::current();
        let (opus_frames, opus_average_frame_bytes) = Self::build_opus_frames();
        let percent = config.voice_participant_percent.clamp(0, 100);
        let chorus_percent = config.voice_chorus_percent.clamp(0, 100);
        let mut rng = rand::rng();
        let mut participates = Vec::with_capacity(client_count);
        let mut joins_chorus = Vec::with_capacity(client_count);
        let mut next_switch_ms = Vec::with_capacity(client_count);
        for _ in 0..client_count {
            participates.push(rng.random_range(0..100) < percent);
            joins_chorus.push(rng.random_range(0..100) < chorus_percent);
            // Start everyone silent and stagger the first burst, so a run does not open with the
            // entire crowd unmuting on the same tick.
            next_switch_ms.push(Mutex::new(rng.random_range(0..config.voice_silence_max_ms.max(1)) as f64));
        }
        let state = Arc::new(VoiceState {
            recipients: (0..client_count).map(|_| Mutex::new(None)).collect(),
            participates,
            talking: (0..client_count).map(|_| AtomicBool::new(false)).collect(),
            joins_chorus,
            next_switch_ms,
            seq: (0..client_count).map(|_| AtomicU8::new(0)).collect(),
            silent_units: (0..client_count).map(|_| AtomicI32::new(0)).collect(),
            mic_cursor: (0..client_count).map(|_| AtomicI64::new(0)).collect(),
            audible: (0..client_count).map(|_| DashMap::new()).collect(),
            built: AtomicI32::new(0),
            opus_frames,
            opus_average_frame_bytes,
            voice_clock: Instant::now(),
            chorus_until_ms: AtomicU64::new(0f64.to_bits()),
            next_chorus_ms: AtomicU64::new((-1.0f64).to_bits()),
            chorus_lock: Mutex::new(()),
        });

        if config.voice_use_system_microphone {
            let started = MicrophoneCapture::start(&config.voice_microphone_device, config.voice_frame_ms, config.voice_bitrate);
            if started {
                let newest = MicrophoneCapture::newest_frame_index();
                let mut participants = 0;
                for i in 0..client_count {
                    state.mic_cursor[i].store(newest, Ordering::Relaxed);
                    if state.participates[i] {
                        participants += 1;
                    }
                }
                BNL::log(format!("[Mic] One capture feeding all {participants} voice participant(s); burst clock and the {} m recipient range are unchanged.", config.voice_range_meters));
            }
        }
        *VOICE.write() = Some(state);
    }

    /// Every voice participant transmits the single shared capture, so a listener hears real audio
    /// from whichever bots are inside VoiceRangeMeters of them.
    pub fn is_mic_client(index: usize) -> bool {
        MicrophoneCapture::active() && Self::state().is_some_and(|s| s.participates.get(index).copied().unwrap_or(false))
    }

    pub fn sync_mic_cursor(index: usize) {
        if let Some(state) = Self::state()
            && let Some(cursor) = state.mic_cursor.get(index)
        {
            cursor.store(MicrophoneCapture::newest_frame_index(), Ordering::Relaxed);
        }
    }

    /// Speech is bursty: a person says something for a few seconds, then listens. Each participant
    /// alternates burst/silence with randomised durations, so who is talking keeps changing.
    pub fn is_talking(index: usize, now_ms: f64) -> bool {
        let Some(state) = Self::state() else { return false };
        if !state.participates.get(index).copied().unwrap_or(false) {
            return false;
        }
        // Alone in the world: nobody is inside the audible radius, so a real client transmits
        // nothing at all. Hold the burst clock too.
        let has_audience = state.recipients[index].lock().as_ref().is_some_and(|r| !r.is_empty());
        if !has_audience {
            state.talking[index].store(false, Ordering::Relaxed);
            return false;
        }
        // A chorus overrides the personal burst clock — that is the point of it.
        if state.joins_chorus[index] && Self::chorus_active(&state, state.voice_clock.elapsed().as_secs_f64() * 1000.0) {
            return true;
        }
        let mut next_switch = state.next_switch_ms[index].lock();
        if now_ms >= *next_switch {
            let talking = !state.talking[index].load(Ordering::Relaxed);
            state.talking[index].store(talking, Ordering::Relaxed);
            let config = ConfigManager::current();
            let min = if talking { config.voice_talk_burst_min_ms } else { config.voice_silence_min_ms };
            let mut max = if talking { config.voice_talk_burst_max_ms } else { config.voice_silence_max_ms };
            if max <= min {
                max = min + 1;
            }
            *next_switch = now_ms + rand::rng().random_range(min..max) as f64;
        }
        state.talking[index].load(Ordering::Relaxed)
    }

    /// Called when avatar traffic arrives about `player_id` at a quality tier the server only
    /// sends to nearby peers — reusing the distance work the SERVER already did.
    pub fn note_audible(client_index: usize, player_id: u16) {
        if let Some(state) = Self::state()
            && let Some(map) = state.audible.get(client_index)
        {
            map.insert(player_id, state.voice_clock.elapsed().as_millis() as i64);
        }
    }

    /// Whether this client already has a recipient list it can transmit against.
    pub fn has_recipients(index: usize) -> bool {
        Self::state().is_some_and(|s| s.recipients.get(index).is_some_and(|r| r.lock().is_some()))
    }

    /// Rebuilds one client's recipient list unconditionally. Callers decide *when* — the driver
    /// sweeps the population on a fixed window. Returns true once the client has a list.
    pub fn rebuild_recipients(peer: &NetPeerRef, slots: &[ClientSlot], index: usize) -> bool {
        let Some(state) = Self::state() else { return false };
        if index >= state.recipients.len() {
            return false;
        }
        let first = state.recipients[index].lock().is_none();
        let config = ConfigManager::current();
        let range_sq = config.voice_range_meters * config.voice_range_meters;

        let changed = T_NEAR.with(|near_cell| {
            T_SCRATCH.with(|scratch_cell| {
                let mut near = near_cell.borrow_mut();
                near.clear();
                // Seed from the simulated crowd's fixed spawn positions.
                if let Some(self_position) = MovementSender::position(index) {
                    for (j, slot) in slots.iter().enumerate() {
                        if j == index {
                            continue;
                        }
                        let Some(other) = slot.peer() else { continue };
                        let Some(p) = MovementSender::position(j) else { continue };
                        let d = p - self_position;
                        if d.squared_magnitude() <= range_sq {
                            near.insert(other.remote_id() as u16);
                        }
                    }
                }
                // Add anyone we can currently hear who is not part of the simulated crowd — a real
                // player, or one that moved into range. Stale entries drop out.
                if let Some(map) = state.audible.get(index) {
                    let now = state.voice_clock.elapsed().as_millis() as i64;
                    let stale = config.voice_audible_timeout_ms as i64;
                    map.retain(|_, last| now - *last <= stale);
                    for kv in map.iter() {
                        near.insert(*kv.key());
                    }
                }
                near.remove(&(peer.remote_id() as u16));

                let mut scratch = scratch_cell.borrow_mut();
                scratch.clear();
                scratch.extend(near.iter().copied());
                scratch.sort_unstable();

                let mut slot = state.recipients[index].lock();
                if let Some(previous) = slot.as_ref()
                    && previous.as_slice() == scratch.as_slice()
                {
                    return false;
                }
                *slot = Some(Arc::new(scratch.clone()));
                true
            })
        });
        if !changed {
            return true;
        }
        if first {
            state.built.fetch_add(1, Ordering::Relaxed);
        }
        Self::send_recipients(peer, index);
        true
    }

    pub fn send_recipients(peer: &NetPeerRef, index: usize) {
        let Some(list) = Self::state().and_then(|s| s.recipients.get(index).and_then(|r| r.lock().clone())) else {
            return;
        };
        // The count is byte-width on the small channel, so anything past 255 recipients has to go
        // out on the large one or the server reads a truncated list.
        let large = list.len() > u8::MAX as usize;
        let mut writer = NetDataWriter::new();
        if large {
            writer.put_ushort(list.len() as u16);
        } else {
            writer.put_byte(list.len() as u8);
        }
        for id in list.iter() {
            writer.put_ushort(*id);
        }
        let channel = if large { BasisNetworkCommons::AUDIO_RECIPIENTS_LARGE_CHANNEL } else { BasisNetworkCommons::AUDIO_RECIPIENTS_CHANNEL };
        let _ = peer.send_writer(&writer, channel, DeliveryMethod::ReliableOrdered);
    }

    pub fn note_silence(index: usize) {
        if let Some(state) = Self::state()
            && let Some(units) = state.silent_units.get(index)
        {
            let _ = units.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| if v < u8::MAX as i32 { Some(v + 1) } else { None });
        }
    }

    pub fn send_frame(peer: &NetPeerRef, index: usize) {
        let Some(state) = Self::state() else { return };
        if state.opus_frames.is_empty() || !state.recipients.get(index).is_some_and(|r| r.lock().as_ref().is_some_and(|l| !l.is_empty())) {
            return;
        }
        // Walk the encoded second so consecutive frames differ, and stagger the starting point per
        // client so the crowd isn't phase-locked.
        let seq = state.seq[index].load(Ordering::Relaxed) as usize;
        let frame = state.opus_frames[(seq + index) % state.opus_frames.len()].clone();
        Self::send_encoded(&state, peer, index, &frame);
    }

    pub fn send_mic_frames(peer: &NetPeerRef, index: usize, max_frames: i32) -> i32 {
        let Some(state) = Self::state() else { return 0 };
        if !state.recipients.get(index).is_some_and(|r| r.lock().as_ref().is_some_and(|l| !l.is_empty())) {
            return 0;
        }
        let mut sent = 0;
        let mut cursor = state.mic_cursor[index].load(Ordering::Relaxed);
        for _ in 0..max_frames {
            let Some((frame, is_speech)) = MicrophoneCapture::try_read(&mut cursor) else { break };
            if is_speech {
                Self::send_encoded(&state, peer, index, &frame);
            } else {
                Self::note_silence(index);
            }
            sent += 1;
        }
        state.mic_cursor[index].store(cursor, Ordering::Relaxed);
        sent
    }

    fn send_encoded(state: &VoiceState, peer: &NetPeerRef, index: usize, frame: &[u8]) {
        let seq = state.seq[index].fetch_add(1, Ordering::Relaxed);
        let silence = state.silent_units[index].swap(0, Ordering::Relaxed) as u8;
        let mut writer = NetDataWriter::new();
        writer.put_byte(seq);
        writer.put_byte(silence);
        writer.put_bytes(frame);
        let _ = peer.send_writer(&writer, BasisNetworkCommons::VOICE_CHANNEL, DeliveryMethod::Sequenced);
    }

    pub fn built_count() -> i32 {
        Self::state().map(|s| s.built.load(Ordering::Relaxed)).unwrap_or(0)
    }
}
