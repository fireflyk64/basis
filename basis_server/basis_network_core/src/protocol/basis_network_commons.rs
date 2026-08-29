use crate::transport::basis_network_shell::DeliveryMethod;

/// Protocol constants: channel numbers, sub-type bytes, magic values and the small pure
/// helpers that map between them. Every constant keeps its C# name (in SCREAMING_SNAKE_CASE)
/// and value; this is the wire contract with the C# clients.
pub struct BasisNetworkCommons;

impl BasisNetworkCommons {
    /// this is the maximum Connections that can occur under the hood.
    pub const MAX_CONNECTIONS: i32 = u16::MAX as i32;

    pub const NETWORK_INTERVAL_POLL: i32 = 2;
    pub const PING_INTERVAL: i32 = 1500;
    pub const RECEIVE_POLLING_TIME: i32 = 50000;
    /// Transport packet pool FLOOR; see the C# comment on the retained-buffer sizing.
    pub const PACKET_POOL_SIZE: i32 = 8192;

    // ── Single-datagram budget ───────────────────────────────────────────
    /// Largest payload a caller may hand to a send that the transport cannot fragment
    /// (see [`BasisNetworkCommons::can_fragment`]). Over this, the send THROWS rather than
    /// truncating or dropping, so anything sizing a packet has to check first.
    ///
    /// Deliberately derived from the smallest MTU any peer can be at, not the negotiated one.
    pub const MINIMUM_PEER_MTU: i32 = 1024;
    /// Headroom held back from [`Self::MINIMUM_PEER_MTU`] for transport headers and framing.
    pub const UNFRAGMENTED_HEADROOM: i32 = 36;
    /// Payload ceiling for one unfragmentable datagram, framing included.
    pub const MAX_UNFRAGMENTED_PAYLOAD: i32 = Self::MINIMUM_PEER_MTU - Self::UNFRAGMENTED_HEADROOM;

    /// True if the transport will split an over-MTU payload across datagrams for this delivery
    /// method. Only the two reliable non-sequenced methods can: sequencing is a per-datagram
    /// property, so a fragmented sequenced packet has no coherent meaning.
    pub fn can_fragment(method: DeliveryMethod) -> bool {
        method == DeliveryMethod::ReliableOrdered || method == DeliveryMethod::ReliableUnordered
    }

    /// when adding a new message we need to increase this — will function up to 64
    pub const TOTAL_CHANNELS: u8 = 64;

    // ── Avatar send-interval byte ────────────────────────────────────────
    // The per-receiver interval byte in avatar keyframe/delta frames encodes the send
    // cadence relative to the server's base interval. 0..199 map 1:1 (base+b ms, the
    // pre-v42 range); 200..255 step 12 ms each (base+200 .. base+860 ms) so very distant
    // receivers can drop below the old 3.3 Hz floor while staying inside the receiver's
    // 1 s interpolation-window clamp.
    pub const AVATAR_INTERVAL_EXTENDED_START: u8 = 200;
    pub const AVATAR_INTERVAL_EXTENDED_STEP_MS: i32 = 12;

    pub fn encode_avatar_interval_byte(interval_ms: i32, base_interval_ms: i32) -> u8 {
        let rel = interval_ms - base_interval_ms;
        if rel <= 0 {
            return 0;
        }
        if rel < i32::from(Self::AVATAR_INTERVAL_EXTENDED_START) {
            return rel as u8;
        }
        let mut steps = (rel - i32::from(Self::AVATAR_INTERVAL_EXTENDED_START)
            + (Self::AVATAR_INTERVAL_EXTENDED_STEP_MS >> 1))
            / Self::AVATAR_INTERVAL_EXTENDED_STEP_MS;
        let max_steps = i32::from(u8::MAX) - i32::from(Self::AVATAR_INTERVAL_EXTENDED_START);
        if steps > max_steps {
            steps = max_steps;
        }
        (i32::from(Self::AVATAR_INTERVAL_EXTENDED_START) + steps) as u8
    }

    pub fn decode_avatar_interval_ms(encoded: u8, base_interval_ms: i32) -> i32 {
        if encoded < Self::AVATAR_INTERVAL_EXTENDED_START {
            return base_interval_ms + i32::from(encoded);
        }
        base_interval_ms
            + i32::from(Self::AVATAR_INTERVAL_EXTENDED_START)
            + i32::from(encoded - Self::AVATAR_INTERVAL_EXTENDED_START) * Self::AVATAR_INTERVAL_EXTENDED_STEP_MS
    }

    // ── Connection lifecycle ─────────────────────────────────────────────
    /// Auth Identity Message
    pub const AUTH_IDENTITY_CHANNEL: u8 = 0;
    /// Player metadata (UUID, display name, permissions)
    pub const META_DATA_CHANNEL: u8 = 1;
    /// Removes a player entity
    pub const DISCONNECTION_CHANNEL: u8 = 2;

    // ── Voice ────────────────────────────────────────────────────────────
    /// Spatialized voice data
    pub const VOICE_CHANNEL: u8 = 3;
    /// Shout mode voice. Non-spatialized audio broadcast to all clients.
    pub const SHOUT_VOICE_CHANNEL: u8 = 4;
    /// Voice recipient list (byte count, ≤255 recipients)
    pub const AUDIO_RECIPIENTS_CHANNEL: u8 = 5;
    /// Voice recipient list (ushort count, >255 recipients)
    pub const AUDIO_RECIPIENTS_LARGE_CHANNEL: u8 = 39;
    /// Spatialized voice data (ushort playerID, for IDs >255)
    pub const VOICE_LARGE_CHANNEL: u8 = 40;
    /// Voice excluded list (byte count, ≤255 excluded). Server sends to everyone EXCEPT listed IDs.
    pub const AUDIO_RECIPIENTS_INVERTED_CHANNEL: u8 = 49;
    /// Voice excluded list (ushort count, >255 excluded). Server sends to everyone EXCEPT listed IDs.
    pub const AUDIO_RECIPIENTS_INVERTED_LARGE_CHANNEL: u8 = 50;
    /// Voice recipients as a bitfield. Bit at position playerID = recipient.
    pub const AUDIO_RECIPIENTS_BITFIELD_CHANNEL: u8 = 51;

    // ── Per-quality avatar channels ──────────────────────────────────────
    // Layout: PlayerAvatarVeryLowChannel + quality * 2 + hasAdditional
    pub const PLAYER_AVATAR_VERY_LOW_CHANNEL: u8 = 6;
    pub const PLAYER_AVATAR_VERY_LOW_ADDITIONAL_CHANNEL: u8 = 7;
    pub const PLAYER_AVATAR_LOW_CHANNEL: u8 = 8;
    pub const PLAYER_AVATAR_LOW_ADDITIONAL_CHANNEL: u8 = 9;
    pub const PLAYER_AVATAR_MEDIUM_CHANNEL: u8 = 10;
    pub const PLAYER_AVATAR_MEDIUM_ADDITIONAL_CHANNEL: u8 = 11;
    pub const PLAYER_AVATAR_HIGH_CHANNEL: u8 = 12;
    pub const PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL: u8 = 13;

    // ── Avatar management ────────────────────────────────────────────────
    /// Swap to a different avatar
    pub const AVATAR_CHANGE_MESSAGE_CHANNEL: u8 = 14;
    /// Full avatar change: ClientAvatarChangeMessage / ServerAvatarChangeMessage.
    pub const AVATAR_CHANGE_KIND_FULL: u8 = 0;
    /// Body-fit-only update: ClientBodyFitMessage / ServerBodyFitMessage. No avatar reload.
    pub const AVATAR_CHANGE_KIND_BODY_FIT: u8 = 1;
    /// Generic avatar script data
    pub const AVATAR_CHANNEL: u8 = 15;

    // ── Player management ────────────────────────────────────────────────
    /// Create a remote player entity
    pub const CREATE_REMOTE_PLAYER_CHANNEL: u8 = 16;
    /// Create remote player entities for a newly joined peer
    pub const CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL: u8 = 17;
    /// Chat text messages displayed above player nameplates
    pub const CHAT_CHANNEL: u8 = 18;

    // ── Ownership ────────────────────────────────────────────────────────
    pub const GET_CURRENT_OWNER_REQUEST_CHANNEL: u8 = 19;
    pub const CHANGE_CURRENT_OWNER_REQUEST_CHANNEL: u8 = 20;
    pub const REMOVE_CURRENT_OWNER_REQUEST_CHANNEL: u8 = 21;

    // ── Net IDs ──────────────────────────────────────────────────────────
    /// Assign a net id (string to ushort)
    pub const NET_ID_ASSIGN_CHANNEL: u8 = 22;
    /// Assign an array of net ids (string to ushort)
    pub const NET_ID_ASSIGNS_CHANNEL: u8 = 23;

    // ── Scene & resources ────────────────────────────────────────────────
    pub const SCENE_CHANNEL: u8 = 24;
    pub const LOAD_RESOURCE_CHANNEL: u8 = 25;
    pub const UNLOAD_RESOURCE_CHANNEL: u8 = 26;
    /// Client tells server it has finished preloading a resource (ready or failed).
    pub const PRELOAD_READY_CHANNEL: u8 = 27;
    /// Server tells all clients to spawn a previously preloaded resource.
    pub const SPAWN_PRELOADED_CHANNEL: u8 = 28;
    /// Modify an already-spawned resource's flags. 55 because 29-54 were taken when it was added.
    pub const MODIFY_RESOURCE_CHANNEL: u8 = 55;

    // ── Content sharing (multiplexed: first payload byte = ContentShareSub_*) ──
    pub const CONTENT_SHARE_CHANNEL: u8 = 29;
    pub const CONTENT_SHARE_SUB_DROP: u8 = 0;
    pub const CONTENT_SHARE_SUB_CLEANUP: u8 = 1;

    // ── Avatar delta (server → client only) ──────────────────────────────
    /// Server→client avatar delta frames. Wire:
    ///   [header:1][playerId:1|2][interval:1][sequence:1][baseSeq:1][delta body]
    /// header bits: quality(2) | hasAdditional<<2 | largeId<<3. Unreliable, server-only.
    pub const DELTA_AVATAR_CHANNEL: u8 = 30;

    // ── Server-bound ─────────────────────────────────────────────────────
    /// Developer hook — data only delivered to the server
    pub const SERVER_BOUND_CHANNEL: u8 = 31;

    // ── Admin ────────────────────────────────────────────────────────────
    // Channels 32 & 33 are free (held the removed server-side database).
    pub const ADMIN_CHANNEL: u8 = 34;

    // ── Stats, camera & events ───────────────────────────────────────────
    pub const SERVER_STATISTICS_CHANNEL: u8 = 35;
    /// PIP camera created/destroyed state (reliable, per-player).
    pub const CAMERA_PIP_STATE_CHANNEL: u8 = 36;
    /// PIP camera position updates (sequenced, position only).
    pub const CAMERA_PIP_POSITION_CHANNEL: u8 = 37;
    /// Generic low-priority events channel. The first byte of the payload identifies the event type.
    pub const EVENTS_CHANNEL: u8 = 38;

    // ── Event type sub-bytes for EventsChannel ──
    pub const EVENT_TYPE_CAMERA_SHUTTER_SOUND: u8 = 0;
    pub const EVENT_TYPE_CAMERA_COUNTDOWN: u8 = 1;
    pub const EVENT_TYPE_PLAYER_TEMP_BLOCK: u8 = 2;
    pub const EVENT_TYPE_AVATAR_RATE_CHANGE: u8 = 3;
    pub const EVENT_TYPE_TALK_MODE_CHANGED: u8 = 4;
    pub const EVENT_TYPE_MUTE_STATE_CHANGED: u8 = 5;
    pub const EVENT_TYPE_PLAYER_CHAT_TYPING: u8 = 6;
    pub const EVENT_TYPE_ERROR_REPORT: u8 = 7;
    pub const EVENT_TYPE_VOICE_RECORD_REQUEST: u8 = 8;
    pub const EVENT_TYPE_VOICE_RECORD_CONSENT: u8 = 9;
    pub const EVENT_TYPE_JIGGLE_GRAB: u8 = 10;
    pub const JIGGLE_GRAB_OP_START: u8 = 0;
    pub const JIGGLE_GRAB_OP_STOP: u8 = 1;
    pub const JIGGLE_GRAB_OP_DENY: u8 = 2;

    // ── Per-quality avatar channels (ushort playerID, for IDs >255) ──
    pub const PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL: u8 = 41;
    pub const PLAYER_AVATAR_VERY_LOW_ADDITIONAL_LARGE_CHANNEL: u8 = 42;
    pub const PLAYER_AVATAR_LOW_LARGE_CHANNEL: u8 = 43;
    pub const PLAYER_AVATAR_LOW_ADDITIONAL_LARGE_CHANNEL: u8 = 44;
    pub const PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL: u8 = 45;
    pub const PLAYER_AVATAR_MEDIUM_ADDITIONAL_LARGE_CHANNEL: u8 = 46;
    pub const PLAYER_AVATAR_HIGH_LARGE_CHANNEL: u8 = 47;
    pub const PLAYER_AVATAR_HIGH_ADDITIONAL_LARGE_CHANNEL: u8 = 48;

    // ── Compressed avatar bundle (server → client only) ──────────────────
    /// Wire format (v53): [flags:1][rawLen:2-LE][compressed( group* )]. See BasisAvatarBundleCodec.
    pub const COMPRESSED_AVATAR_BUNDLE_CHANNEL: u8 = 52;

    // ── Server-provided default library ──────────────────────────────────
    pub const SERVER_LIBRARY_CHANNEL: u8 = 53;

    // ── Peer-to-peer direct connection ───────────────────────────────────
    // First byte of payload selects a P2PSub_* sub-type; remaining bytes are
    // the BasisP2PSignalMessage body. Reliable-ordered.
    pub const P2P_CHANNEL: u8 = 54;

    pub const P2P_SUB_REQUEST: u8 = 0;
    pub const P2P_SUB_ACCEPT: u8 = 1;
    pub const P2P_SUB_DECLINE: u8 = 2;
    pub const P2P_SUB_CANCEL: u8 = 3;
    pub const P2P_SUB_LINK_LOST: u8 = 4;
    pub const P2P_SUB_SERVER_ARMED: u8 = 5;
    pub const P2P_SUB_LINK_UP: u8 = 6;
    /// Server → both peers once both sides reported LinkUp and it began offloading the pair.
    pub const P2P_SUB_OFFLOADED: u8 = 7;
    /// Client → server: the iroh endpoint address this client accepts direct links on. The
    /// counterpart of LiteNetLib's NatIntroduceRequest — carried in-band on the P2P channel
    /// because iroh needs no separate NAT-punch socket. Body: BasisP2PIntroduceRequest.
    pub const P2P_SUB_INTRODUCE_REQUEST: u8 = 8;
    /// Server → client: the other side's iroh endpoint address; dial it. Body: BasisP2PIntroduce.
    pub const P2P_SUB_INTRODUCE: u8 = 9;

    // ── Direct-connect custom data (P2P-first, server fallback) ──────────
    /// P2P world/prop direct custom data. Frame: [messageIndex:2][payload].
    pub const DIRECT_SCENE_CHANNEL: u8 = 56;
    /// Server relay of a direct-origin scene message (recipients with no direct link).
    pub const DIRECT_SCENE_SERVER_CHANNEL: u8 = 57;
    /// P2P avatar direct custom data. Frame: [messageIndex:1][avatarLinkIndex:1][payload].
    pub const DIRECT_AVATAR_CHANNEL: u8 = 58;
    /// Server relay of a direct-origin avatar message (recipients with no direct link).
    pub const DIRECT_AVATAR_SERVER_CHANNEL: u8 = 59;

    // ── Dynamic message registry (subscribe & supply) ────────────────────
    /// Registry handshake. First payload byte is a RegistrySub_* sub-type.
    pub const REGISTRY_CONTROL_CHANNEL: u8 = 60;
    /// Reliable-ordered plugin payloads. Frame: [messageId:2][payload].
    pub const PLUGIN_RELIABLE_CHANNEL: u8 = 61;
    /// Sequenced plugin payloads. Frame: [messageId:2][payload].
    pub const PLUGIN_SEQUENCED_CHANNEL: u8 = 62;
    /// Unreliable plugin payloads. Frame: [messageId:2][payload].
    pub const PLUGIN_UNRELIABLE_CHANNEL: u8 = 63;

    pub const REGISTRY_SUB_SUPPLY: u8 = 0;
    pub const REGISTRY_SUB_SUBSCRIBE: u8 = 1;

    /// Maps a plugin DeliveryMethod to its multiplexed channel (61-63). Returns RegistryControlChannel for unmapped values.
    pub fn get_plugin_channel_for_delivery(delivery: DeliveryMethod) -> u8 {
        match delivery {
            DeliveryMethod::ReliableOrdered
            | DeliveryMethod::ReliableUnordered
            | DeliveryMethod::ReliableSequenced => Self::PLUGIN_RELIABLE_CHANNEL,
            DeliveryMethod::Sequenced => Self::PLUGIN_SEQUENCED_CHANNEL,
            DeliveryMethod::Unreliable => Self::PLUGIN_UNRELIABLE_CHANNEL,
        }
    }

    /// Canonical DeliveryMethod for a multiplexed plugin channel (reverse of get_plugin_channel_for_delivery).
    pub fn get_delivery_for_plugin_channel(channel: u8) -> DeliveryMethod {
        match channel {
            Self::PLUGIN_SEQUENCED_CHANNEL => DeliveryMethod::Sequenced,
            Self::PLUGIN_UNRELIABLE_CHANNEL => DeliveryMethod::Unreliable,
            _ => DeliveryMethod::ReliableOrdered,
        }
    }

    /// True if the channel is one of the multiplexed plugin channels (61-63) carrying a [messageId:2] prefix.
    pub fn is_plugin_channel(channel: u8) -> bool {
        (Self::PLUGIN_RELIABLE_CHANNEL..=Self::PLUGIN_UNRELIABLE_CHANNEL).contains(&channel)
    }

    // ── Server info unconnected query ────────────────────────────────────
    /// Magic header for the unconnected info query packet from the client.
    pub const SERVER_INFO_QUERY_MAGIC: u32 = 0xBA515101;
    /// Magic header for the unconnected info response packet from the server.
    pub const SERVER_INFO_RESPONSE_MAGIC: u32 = 0xBA515102;
    /// Wire-format version for the info query payload. Bump when the layout changes.
    pub const SERVER_INFO_PROTOCOL_VERSION: u16 = 1;
    pub const SERVER_INFO_NAME_MAX_LENGTH: usize = 64;
    pub const SERVER_INFO_MOTD_MAX_LENGTH: usize = 256;
    /// Minimum total request size (in bytes) the server will accept on an info query. Clients
    /// pad their query up to this size so the response is never larger than the request.
    pub const SERVER_INFO_MIN_REQUEST_BYTES: usize = 384;

    // ── Structured connection-reject payload ─────────────────────────────
    // Wire: [magic:uint][kind:byte][aux0:ushort][aux1:ushort][message:string]
    /// Marker for a structured reject payload. "BA51 5CE1" ≈ "Basis reject".
    pub const REJECT_MAGIC: u32 = 0xBA515CE1;
    pub const REJECT_KIND_VERSION_MISMATCH: u8 = 1;
    pub const REJECT_KIND_SERVER_FULL: u8 = 2;

    /// Channels whose unreliable traffic must not be queued behind, or shed alongside, bulk
    /// avatar state. Indexed by channel number. Only the voice DATA channels belong here.
    pub fn build_priority_unreliable_channel_map() -> Vec<bool> {
        let mut map = vec![false; usize::from(Self::TOTAL_CHANNELS)];
        map[usize::from(Self::VOICE_CHANNEL)] = true;
        map[usize::from(Self::SHOUT_VOICE_CHANNEL)] = true;
        map[usize::from(Self::VOICE_LARGE_CHANNEL)] = true;
        map
    }

    /// Maps quality index (0-3) + additional data presence → byte-ID channel.
    pub fn get_player_avatar_channel_for_quality(quality_index: i32, has_additional_data: bool) -> u8 {
        (i32::from(Self::PLAYER_AVATAR_VERY_LOW_CHANNEL) + quality_index * 2 + i32::from(has_additional_data)) as u8
    }

    /// Maps quality index (0-3) + additional data presence → ushort-ID channel (for playerIDs >255).
    pub fn get_player_avatar_large_channel_for_quality(quality_index: i32, has_additional_data: bool) -> u8 {
        (i32::from(Self::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL) + quality_index * 2 + i32::from(has_additional_data)) as u8
    }

    /// Returns true if this channel uses ushort playerID (large variant).
    pub fn is_large_player_id_channel(channel: u8) -> bool {
        channel == Self::VOICE_LARGE_CHANNEL
            || (Self::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL..=Self::PLAYER_AVATAR_HIGH_ADDITIONAL_LARGE_CHANNEL)
                .contains(&channel)
    }

    /// Reverse mapping: channel → quality index (0-3). Works for both byte-ID and ushort-ID avatar channels.
    pub fn get_quality_from_channel(channel: u8) -> u8 {
        if channel >= Self::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL {
            return (channel - Self::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL) / 2;
        }
        channel.wrapping_sub(Self::PLAYER_AVATAR_VERY_LOW_CHANNEL) / 2
    }

    /// Reverse mapping: channel → has additional data. Odd offset channels carry additional data.
    pub fn channel_has_additional_data(channel: u8) -> bool {
        if channel >= Self::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL {
            return ((channel - Self::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL) & 1) == 1;
        }
        (channel.wrapping_sub(Self::PLAYER_AVATAR_VERY_LOW_CHANNEL) & 1) == 1
    }

    // ── DeltaAvatarChannel header helpers ────────────────────────────────
    /// Packs quality(0-3) + additional + large-id into the DeltaAvatarChannel header byte.
    pub fn build_delta_header(quality_index: i32, has_additional_data: bool, large_id: bool) -> u8 {
        ((quality_index & 0x3) | if has_additional_data { 0x4 } else { 0 } | if large_id { 0x8 } else { 0 }) as u8
    }
    pub fn delta_header_quality(header: u8) -> u8 {
        header & 0x3
    }
    pub fn delta_header_has_additional_data(header: u8) -> bool {
        (header & 0x4) != 0
    }
    pub fn delta_header_large_id(header: u8) -> bool {
        (header & 0x8) != 0
    }

    // Control frames on DeltaAvatarChannel (v42): header bit 7 marks a non-delta control message.
    pub const DELTA_HEADER_CONTROL_BIT: u8 = 0x80;
    pub const DELTA_CONTROL_KEYFRAME_REQUEST: u8 = 0x80;
    pub const DELTA_CONTROL_UPLINK_KEYFRAME_REQUEST: u8 = 0xC0;
    /// True when a DeltaAvatarChannel first byte is a control frame, not a delta.
    pub fn is_delta_control_header(header: u8) -> bool {
        (header & Self::DELTA_HEADER_CONTROL_BIT) != 0
    }

    /// All 16 per-quality avatar channels (byte-ID + ushort-ID) for aggregate congestion checks.
    pub const PLAYER_AVATAR_QUALITY_CHANNELS: [u8; 16] = [
        Self::PLAYER_AVATAR_VERY_LOW_CHANNEL,
        Self::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_CHANNEL,
        Self::PLAYER_AVATAR_LOW_CHANNEL,
        Self::PLAYER_AVATAR_LOW_ADDITIONAL_CHANNEL,
        Self::PLAYER_AVATAR_MEDIUM_CHANNEL,
        Self::PLAYER_AVATAR_MEDIUM_ADDITIONAL_CHANNEL,
        Self::PLAYER_AVATAR_HIGH_CHANNEL,
        Self::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL,
        Self::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL,
        Self::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_LARGE_CHANNEL,
        Self::PLAYER_AVATAR_LOW_LARGE_CHANNEL,
        Self::PLAYER_AVATAR_LOW_ADDITIONAL_LARGE_CHANNEL,
        Self::PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL,
        Self::PLAYER_AVATAR_MEDIUM_ADDITIONAL_LARGE_CHANNEL,
        Self::PLAYER_AVATAR_HIGH_LARGE_CHANNEL,
        Self::PLAYER_AVATAR_HIGH_ADDITIONAL_LARGE_CHANNEL,
    ];
}
