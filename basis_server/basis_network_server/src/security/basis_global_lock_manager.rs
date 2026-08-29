//! Port of `Security/BasisGlobalLockManager.cs`: server-wide toggles that admins can flip.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::configuration::Configuration;
use basis_network_core::{NetDataWriter, NetPeerRef, NetResult};

use super::{broadcast_admin_state, send_admin_state_to_peer};
use crate::NetworkServer;

pub struct BasisGlobalLockManager;

static AVATARS_LOCKED: AtomicBool = AtomicBool::new(false);
static PROPS_LOCKED: AtomicBool = AtomicBool::new(false);
static WORLDS_LOCKED: AtomicBool = AtomicBool::new(false);
static SERVERS_LOCKED: AtomicBool = AtomicBool::new(false);
static THIRD_PERSON_DISABLED: AtomicBool = AtomicBool::new(false);
static ADDITIONAL_AVATAR_DATA_LOCK: AtomicBool = AtomicBool::new(false);
static CAMERA_METADATA_DISALLOW_MASK: AtomicU8 = AtomicU8::new(0);
static PLAYSPACE_MOVER_LOCKED: AtomicBool = AtomicBool::new(false);
static DIRECT_CONNECT_LOCKED: AtomicBool = AtomicBool::new(false);
static CILBOX_LOCKED: AtomicBool = AtomicBool::new(false);
static IMAGES_LOCKED: AtomicBool = AtomicBool::new(false);
static TEXT_CHAT_LOCKED: AtomicBool = AtomicBool::new(false);
static VOICE_CHAT_LOCKED: AtomicBool = AtomicBool::new(false);
static MEDIA_PLAYER_LOCKED: AtomicBool = AtomicBool::new(false);
static CAMERA_CAPTURE_LOCKED: AtomicBool = AtomicBool::new(false);
static PROP_GRABBING_LOCKED: AtomicBool = AtomicBool::new(false);
static SAFE_DISPLAY_NAMES_FORCED: AtomicBool = AtomicBool::new(false);
/// false = feature on (default), true = admin-disabled. Inverted vs the locks above.
static END_EFFECTOR_IK_DISABLED: AtomicBool = AtomicBool::new(false);

fn toggle(field: &AtomicBool) -> bool {
    !field.fetch_xor(true, Ordering::AcqRel)
}

impl BasisGlobalLockManager {
    pub fn avatars_locked() -> bool {
        AVATARS_LOCKED.load(Ordering::Acquire)
    }
    pub fn props_locked() -> bool {
        PROPS_LOCKED.load(Ordering::Acquire)
    }
    pub fn worlds_locked() -> bool {
        WORLDS_LOCKED.load(Ordering::Acquire)
    }
    pub fn servers_locked() -> bool {
        SERVERS_LOCKED.load(Ordering::Acquire)
    }
    pub fn third_person_disabled() -> bool {
        THIRD_PERSON_DISABLED.load(Ordering::Acquire)
    }
    pub fn additional_avatar_data_lock() -> bool {
        ADDITIONAL_AVATAR_DATA_LOCK.load(Ordering::Acquire)
    }
    pub fn camera_metadata_disallow_mask() -> u8 {
        CAMERA_METADATA_DISALLOW_MASK.load(Ordering::Acquire)
    }
    pub fn playspace_mover_locked() -> bool {
        PLAYSPACE_MOVER_LOCKED.load(Ordering::Acquire)
    }
    pub fn direct_connect_locked() -> bool {
        DIRECT_CONNECT_LOCKED.load(Ordering::Acquire)
    }
    pub fn cilbox_locked() -> bool {
        CILBOX_LOCKED.load(Ordering::Acquire)
    }
    pub fn images_locked() -> bool {
        IMAGES_LOCKED.load(Ordering::Acquire)
    }
    pub fn text_chat_locked() -> bool {
        TEXT_CHAT_LOCKED.load(Ordering::Acquire)
    }
    pub fn voice_chat_locked() -> bool {
        VOICE_CHAT_LOCKED.load(Ordering::Acquire)
    }
    pub fn media_player_locked() -> bool {
        MEDIA_PLAYER_LOCKED.load(Ordering::Acquire)
    }
    pub fn camera_capture_locked() -> bool {
        CAMERA_CAPTURE_LOCKED.load(Ordering::Acquire)
    }
    pub fn prop_grabbing_locked() -> bool {
        PROP_GRABBING_LOCKED.load(Ordering::Acquire)
    }
    pub fn safe_display_names_forced() -> bool {
        SAFE_DISPLAY_NAMES_FORCED.load(Ordering::Acquire)
    }
    pub fn end_effector_ik_disabled() -> bool {
        END_EFFECTOR_IK_DISABLED.load(Ordering::Acquire)
    }

    /// Seed the initial lock state from the server configuration.
    pub fn initialize_from_config(config: &Configuration) {
        AVATARS_LOCKED.store(config.avatars_locked, Ordering::Release);
        PROPS_LOCKED.store(config.props_locked, Ordering::Release);
        WORLDS_LOCKED.store(config.worlds_locked, Ordering::Release);
        SERVERS_LOCKED.store(config.servers_locked, Ordering::Release);
        THIRD_PERSON_DISABLED.store(config.third_person_disabled, Ordering::Release);
        ADDITIONAL_AVATAR_DATA_LOCK.store(config.additional_avatar_data_lock, Ordering::Release);
        CAMERA_METADATA_DISALLOW_MASK.store(config.camera_metadata_disallow_mask, Ordering::Release);
        PLAYSPACE_MOVER_LOCKED.store(config.playspace_mover_locked, Ordering::Release);
        DIRECT_CONNECT_LOCKED.store(config.direct_connect_locked, Ordering::Release);
        CILBOX_LOCKED.store(config.cilbox_locked, Ordering::Release);
        IMAGES_LOCKED.store(config.images_locked, Ordering::Release);
        TEXT_CHAT_LOCKED.store(config.text_chat_locked, Ordering::Release);
        VOICE_CHAT_LOCKED.store(config.voice_chat_locked, Ordering::Release);
        MEDIA_PLAYER_LOCKED.store(config.media_player_locked, Ordering::Release);
        CAMERA_CAPTURE_LOCKED.store(config.camera_capture_locked, Ordering::Release);
        PROP_GRABBING_LOCKED.store(config.prop_grabbing_locked, Ordering::Release);
        SAFE_DISPLAY_NAMES_FORCED.store(config.safe_display_names_forced, Ordering::Release);
        END_EFFECTOR_IK_DISABLED.store(config.end_effector_ik_disabled, Ordering::Release);
    }

    /// Copies the live lock state back onto the configuration so a caller can persist it. The
    /// mirror image of [`initialize_from_config`](Self::initialize_from_config).
    pub fn write_to_config(config: &mut Configuration) {
        config.avatars_locked = Self::avatars_locked();
        config.props_locked = Self::props_locked();
        config.worlds_locked = Self::worlds_locked();
        config.servers_locked = Self::servers_locked();
        config.third_person_disabled = Self::third_person_disabled();
        config.additional_avatar_data_lock = Self::additional_avatar_data_lock();
        config.camera_metadata_disallow_mask = Self::camera_metadata_disallow_mask();
        config.playspace_mover_locked = Self::playspace_mover_locked();
        config.direct_connect_locked = Self::direct_connect_locked();
        config.cilbox_locked = Self::cilbox_locked();
        config.images_locked = Self::images_locked();
        config.text_chat_locked = Self::text_chat_locked();
        config.voice_chat_locked = Self::voice_chat_locked();
        config.media_player_locked = Self::media_player_locked();
        config.camera_capture_locked = Self::camera_capture_locked();
        config.prop_grabbing_locked = Self::prop_grabbing_locked();
        config.safe_display_names_forced = Self::safe_display_names_forced();
        config.end_effector_ik_disabled = Self::end_effector_ik_disabled();
    }

    /// Toggle avatar loading. Returns the new state (true = locked).
    pub fn toggle_avatars() -> bool {
        toggle(&AVATARS_LOCKED)
    }
    pub fn toggle_props() -> bool {
        toggle(&PROPS_LOCKED)
    }
    pub fn toggle_worlds() -> bool {
        toggle(&WORLDS_LOCKED)
    }
    pub fn toggle_servers() -> bool {
        toggle(&SERVERS_LOCKED)
    }
    pub fn toggle_third_person() -> bool {
        toggle(&THIRD_PERSON_DISABLED)
    }
    pub fn toggle_additional_avatar_data_lock() -> bool {
        toggle(&ADDITIONAL_AVATAR_DATA_LOCK)
    }
    pub fn toggle_playspace_mover() -> bool {
        toggle(&PLAYSPACE_MOVER_LOCKED)
    }
    pub fn toggle_direct_connect() -> bool {
        toggle(&DIRECT_CONNECT_LOCKED)
    }
    pub fn toggle_cilbox() -> bool {
        toggle(&CILBOX_LOCKED)
    }
    pub fn toggle_images() -> bool {
        toggle(&IMAGES_LOCKED)
    }
    pub fn toggle_text_chat() -> bool {
        toggle(&TEXT_CHAT_LOCKED)
    }
    pub fn toggle_voice_chat() -> bool {
        toggle(&VOICE_CHAT_LOCKED)
    }
    pub fn toggle_media_player() -> bool {
        toggle(&MEDIA_PLAYER_LOCKED)
    }
    pub fn toggle_camera_capture() -> bool {
        toggle(&CAMERA_CAPTURE_LOCKED)
    }
    pub fn toggle_prop_grabbing() -> bool {
        toggle(&PROP_GRABBING_LOCKED)
    }
    pub fn toggle_safe_display_names() -> bool {
        toggle(&SAFE_DISPLAY_NAMES_FORCED)
    }
    pub fn toggle_end_effector_ik() -> bool {
        toggle(&END_EFFECTOR_IK_DISABLED)
    }

    /// Set the per-category camera photo-metadata disallow mask (set bit = disallowed).
    pub fn set_camera_metadata_disallow_mask(mask: u8) {
        CAMERA_METADATA_DISALLOW_MASK.store(mask, Ordering::Release);
    }

    /// The lock-state payload. Fields are appended, never reordered, so an older client that
    /// stops reading earlier still parses.
    fn write_lock_state(writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_bool(Self::avatars_locked());
        writer.put_bool(Self::props_locked());
        writer.put_bool(Self::worlds_locked());
        writer.put_bool(Self::servers_locked());
        writer.put_bool(Self::third_person_disabled());
        writer.put_bool(Self::additional_avatar_data_lock());
        writer.put_byte(Self::camera_metadata_disallow_mask());
        writer.put_byte(NetworkServer::configuration_or_default().basis_user_restriction_mode as u8);
        writer.put_bool(Self::playspace_mover_locked());
        writer.put_bool(Self::direct_connect_locked());
        writer.put_bool(Self::cilbox_locked());
        writer.put_bool(Self::images_locked());
        writer.put_bool(Self::end_effector_ik_disabled());
        writer.put_bool(Self::text_chat_locked());
        writer.put_bool(Self::voice_chat_locked());
        writer.put_bool(Self::media_player_locked());
        writer.put_bool(Self::camera_capture_locked());
        writer.put_bool(Self::prop_grabbing_locked());
        writer.put_bool(Self::safe_display_names_forced());
        Ok(())
    }

    /// Sends the current global lock state to a specific peer (a new player).
    pub fn send_lock_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetLockState, Self::write_lock_state);
    }

    /// Broadcasts the current lock state to all connected clients.
    pub fn broadcast_lock_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetLockState, Self::write_lock_state);
    }
}
