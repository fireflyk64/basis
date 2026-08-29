use std::io::{Read, Write};

use crate::io::{NetDataError, NetDataReader, NetDataWriter, NetResult};
use crate::protocol::BasisNetworkCommons;
use crate::BNL;

use super::avatar::{ClientAvatarChangeMessage, LocalAvatarSyncMessage};
use super::identity::{ClientMetaDataMessage, PlayerIdMessage};
use crate::compression::BitQuality;

/// Consists of a ushort length, followed by a byte array (of the same length).
#[derive(Clone, Copy, Debug, Default)]
pub struct BytesMessage;

impl BytesMessage {
    /// Returns the data, or `None` (after logging) where the C# returned false.
    pub fn deserialize(&self, reader: &mut NetDataReader) -> Option<Vec<u8>> {
        let Some(msg_length) = reader.try_get_ushort() else {
            BNL::log_error("unable to read the size of the data");
            return None;
        };
        let msg_length = usize::from(msg_length);
        if reader.available_bytes() < msg_length {
            BNL::log_error(format!(
                "BytesMessage: declared length {msg_length} exceeds available bytes {}; possible protocol mismatch or truncated packet.",
                reader.available_bytes()
            ));
            return None;
        }
        reader.get_bytes_vec(msg_length).ok()
    }

    /// The length prefix is a ushort; a larger payload is refused rather than written under a
    /// wrapped count.
    pub fn serialize(&self, writer: &mut NetDataWriter, data: &[u8]) -> NetResult<()> {
        let length = u16::try_from(data.len()).map_err(|_| NetDataError::too_long("bytes message", data.len(), usize::from(u16::MAX)))?;
        if length == 0 {
            BNL::log_error("this data does not belong on the network! was size 0");
        }
        writer.put_ushort(length);
        writer.put_bytes(data);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorMessage {
    pub message: String,
}

impl ErrorMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.message = reader.get_string().map_err(|e| e.for_field("ErrorMessage.message"))?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_string(&self.message)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadyMessage {
    pub player_meta_data_message: ClientMetaDataMessage,
    pub client_avatar_change_message: ClientAvatarChangeMessage,
    pub local_avatar_sync_message: LocalAvatarSyncMessage,
}

impl ReadyMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_meta_data_message.deserialize(reader)?;
        self.client_avatar_change_message.deserialize(reader)?;
        self.local_avatar_sync_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_meta_data_message.serialize(writer)?;
        self.client_avatar_change_message.serialize(writer)?;
        let quality = BitQuality::from_byte(self.local_avatar_sync_message.data_quality_level);
        self.local_avatar_sync_message.serialize(writer, quality)?;
        Ok(())
    }

    pub fn was_deserialized_correctly(&self) -> bool {
        self.client_avatar_change_message.byte_array.is_some() && self.local_avatar_sync_message.array.is_some()
    }
}

/// A run of [`ServerReadyMessage`]s delivered as one packet, used for the join fill.
///
/// Wire: [count:ushort][compressed:1][payloadLength:int][payload] where payload is count
/// ServerReadyMessages back to back, optionally raw-Deflate'd. The flag is per batch because
/// compression is skipped when it does not pay (small batches inflate).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerReadyBatchMessage {
    pub count: u16,
    /// Uncompressed concatenation.
    pub payload: Vec<u8>,
    /// What the last serialize/deserialize actually did.
    pub was_compressed: bool,
}

impl ServerReadyBatchMessage {
    /// Uncompressed payload bytes per batch.
    pub const MAX_PAYLOAD_BYTES: usize = 32 * 1024;
    /// Below this a Deflate block header costs more than it saves.
    pub const MIN_COMPRESS_BYTES: usize = 256;

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        let body = &self.payload;
        let mut framed: &[u8] = body;
        let mut compressed = false;
        let deflated;
        if body.len() >= Self::MIN_COMPRESS_BYTES
            && let Ok(d) = Self::deflate(body)
        {
            // Only pay for compression when it actually wins; a high-entropy batch can grow.
            // A deflate failure (never seen for an in-memory sink) just sends the batch raw.
            deflated = d;
            if deflated.len() < body.len() {
                framed = &deflated;
                compressed = true;
            }
        }
        self.was_compressed = compressed;
        writer.put_ushort(self.count);
        writer.put_bool(compressed);
        let length = i32::try_from(framed.len())
            .map_err(|_| NetDataError::too_long("ready batch", framed.len(), i32::MAX as usize))?;
        writer.put_int(length);
        writer.put_bytes(framed);
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.count = reader.get_ushort()?;
        self.was_compressed = reader.get_bool()?;
        let length = reader.get_int()?;
        if length < 0 || length as usize > reader.available_bytes() {
            return Err(NetDataError::invalid("ReadyBatch", format!(
                "length {length} exceeds available data ({} bytes).",
                reader.available_bytes()
            )));
        }
        let framed = reader.get_bytes_vec(length as usize)?;
        self.payload = if self.was_compressed {
            Self::inflate(&framed).map_err(|e| NetDataError::invalid("ReadyBatch", format!("inflate failed: {e}")))?
        } else {
            framed
        };
        Ok(())
    }

    pub fn deflate(raw: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(raw)?;
        e.finish()
    }

    pub fn inflate(compressed: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut d = flate2::read::DeflateDecoder::new(compressed);
        let mut out = Vec::new();
        d.read_to_end(&mut out)?;
        Ok(out)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerReadyMessage {
    /// who this came from
    pub player_id_message: PlayerIdMessage,
    pub local_ready_message: ReadyMessage,
}

impl ServerReadyMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.local_ready_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_id_message.serialize(writer)?;
        self.local_ready_message.serialize(writer)?;
        Ok(())
    }
}

/// Snapshot of server/client stats. Fixed layout for easy wire format.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerStatisticMessage {
    pub data: Vec<u8>,
}

impl ServerStatisticMessage {
    pub fn serialize(&mut self, w: &mut NetDataWriter) {
        w.put_bytes(&self.data);
    }

    pub fn deserialize(&mut self, r: &mut NetDataReader) -> NetResult<()> {
        self.data = r.get_remaining_bytes();
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisP2PSignalMessage {
    pub other_player_id: u16,
    pub session_token: String,
    /// X25519 ephemeral public key of the sender, relayed by the server so the two peers can
    /// derive a per-pair key and always encrypt the direct (P2P) link.
    pub ephemeral_public_key: Option<Vec<u8>>,
}

impl BasisP2PSignalMessage {
    pub const MAX_TOKEN_LENGTH: usize = 64;
    pub const PUBLIC_KEY_SIZE: usize = 32;

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.other_player_id = reader.get_ushort()?;
        self.session_token = reader.get_string_max(Self::MAX_TOKEN_LENGTH)?;
        let has_key = reader.get_byte()?;
        if has_key == 1 && reader.available_bytes() >= Self::PUBLIC_KEY_SIZE {
            self.ephemeral_public_key = Some(reader.get_bytes_vec(Self::PUBLIC_KEY_SIZE)?);
        } else {
            self.ephemeral_public_key = None;
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.other_player_id);
        writer.put_string_max(&self.session_token, Self::MAX_TOKEN_LENGTH)?;
        match &self.ephemeral_public_key {
            Some(key) if key.len() == Self::PUBLIC_KEY_SIZE => {
                writer.put_byte(1);
                writer.put_bytes(key);
            }
            _ => writer.put_byte(0),
        }
        Ok(())
    }
}

/// Client → server on `P2P_CHANNEL` under `P2P_SUB_INTRODUCE_REQUEST`: "here is the iroh
/// endpoint address I accept direct links on for this session". The transport-neutral
/// counterpart of LiteNetLib's out-of-band `NatIntroduceRequest`.
///
/// Wire: [sessionToken:string][addr:bytesWithLength] — `addr` is the postcard-free, self-describing
/// form written by `transport::iroh_network_impl::encode_endpoint_addr`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisP2PIntroduceRequest {
    pub session_token: String,
    pub endpoint_addr: Vec<u8>,
}

impl BasisP2PIntroduceRequest {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.session_token = reader.get_string_max(BasisP2PSignalMessage::MAX_TOKEN_LENGTH)?;
        self.endpoint_addr = reader.get_bytes_with_length()?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_string_max(&self.session_token, BasisP2PSignalMessage::MAX_TOKEN_LENGTH)?;
        writer.put_bytes_with_length(&self.endpoint_addr)?;
        Ok(())
    }
}

/// Server → client on `P2P_CHANNEL` under `P2P_SUB_INTRODUCE`: the other side's endpoint address
/// for this session, plus which of the two the receiver is (so exactly one side dials).
///
/// Wire: [sessionToken:string][otherPlayerId:ushort][dial:1][addr:bytesWithLength]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisP2PIntroduce {
    pub session_token: String,
    pub other_player_id: u16,
    /// True for the side that should open the connection; the other side accepts.
    pub dial: bool,
    pub endpoint_addr: Vec<u8>,
}

impl BasisP2PIntroduce {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.session_token = reader.get_string_max(BasisP2PSignalMessage::MAX_TOKEN_LENGTH)?;
        self.other_player_id = reader.get_ushort()?;
        self.dial = reader.get_bool()?;
        self.endpoint_addr = reader.get_bytes_with_length()?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_string_max(&self.session_token, BasisP2PSignalMessage::MAX_TOKEN_LENGTH)?;
        writer.put_ushort(self.other_player_id);
        writer.put_bool(self.dial);
        writer.put_bytes_with_length(&self.endpoint_addr)?;
        Ok(())
    }
}

bitflags_like! {
    /// Flags on a message descriptor.
    pub struct BasisMessageFlags: u8 {
        const NONE = 0;
        /// Rides a shared plugin channel (61-63) with a leading [messageId:2] prefix.
        const MULTIPLEXED = 1 << 0;
        /// The client must bind a handler for this id or the server disconnects it.
        const REQUIRED = 1 << 1;
        /// The server is allowed to send this message to clients.
        const SERVER_TO_CLIENT = 1 << 2;
        /// Clients are allowed to send this message to the server.
        const CLIENT_TO_SERVER = 1 << 3;
    }
}

macro_rules! bitflags_like {
    ($(#[$m:meta])* pub struct $name:ident: $t:ty { $($(#[$fm:meta])* const $f:ident = $v:expr;)* }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
        pub struct $name(pub $t);
        impl $name {
            $($(#[$fm])* pub const $f: $name = $name($v);)*
            pub const fn bits(self) -> $t { self.0 }
            pub const fn contains(self, other: $name) -> bool { (self.0 & other.0) == other.0 }
        }
        impl std::ops::BitOr for $name { type Output = $name; fn bitor(self, o: $name) -> $name { $name(self.0 | o.0) } }
        impl std::ops::BitOrAssign for $name { fn bitor_assign(&mut self, o: $name) { self.0 |= o.0; } }
    };
}
use bitflags_like;

/// One row of the message registry the server supplies to a client on connect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisMessageDescriptor {
    /// Flat message id. For a core message this equals its dedicated channel (0-59).
    pub id: u16,
    /// Payload schema version.
    pub version: u8,
    /// Channel this message travels on (its own for core, one of 61-63 for a multiplexed plugin).
    pub channel: u8,
    /// BasisMessageFlags bitfield.
    pub flags: u8,
    /// Stable string identity, e.g. "basis.core.voice" or "com.acme.plugin.foo".
    pub name: String,
}

impl BasisMessageDescriptor {
    pub fn serialize(&self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.id);
        writer.put_byte(self.version);
        writer.put_byte(self.channel);
        writer.put_byte(self.flags);
        writer.put_string(&self.name)?;
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> bool {
        let Some(id) = reader.try_get_ushort() else { BNL::log_error("BasisMessageDescriptor: missing Id"); return false; };
        self.id = id;
        let Some(version) = reader.try_get_byte() else { BNL::log_error("BasisMessageDescriptor: missing Version"); return false; };
        self.version = version;
        let Some(channel) = reader.try_get_byte() else { BNL::log_error("BasisMessageDescriptor: missing Channel"); return false; };
        self.channel = channel;
        let Some(flags) = reader.try_get_byte() else { BNL::log_error("BasisMessageDescriptor: missing Flags"); return false; };
        self.flags = flags;
        let Some(name) = reader.try_get_string() else { BNL::log_error("BasisMessageDescriptor: missing Name"); return false; };
        self.name = name;
        true
    }
}

/// Server to client on RegistryControlChannel (sub-type RegistrySub_Supply): the full set of
/// message types this server understands this session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisMessageSupply {
    pub descriptors: Vec<BasisMessageDescriptor>,
}

impl BasisMessageSupply {
    pub fn serialize(&self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.descriptors.len() as u16);
        for d in &self.descriptors {
            d.serialize(writer)?;
        }
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> bool {
        let Some(count) = reader.try_get_ushort() else {
            BNL::log_error("BasisMessageSupply: missing count");
            self.descriptors = Vec::new();
            return false;
        };
        // Each descriptor costs at least a byte on the wire.
        if usize::from(count) > reader.available_bytes() {
            BNL::log_error(format!("BasisMessageSupply: count {count} exceeds available {}", reader.available_bytes()));
            self.descriptors = Vec::new();
            return false;
        }
        self.descriptors = vec![BasisMessageDescriptor::default(); usize::from(count)];
        for i in 0..usize::from(count) {
            if !self.descriptors[i].deserialize(reader) {
                return false;
            }
        }
        true
    }
}

/// Client to server on RegistryControlChannel (sub-type RegistrySub_Subscribe): the message ids
/// the client has a handler for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisMessageSubscribe {
    pub ids: Vec<u16>,
}

impl BasisMessageSubscribe {
    pub fn serialize(&self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.ids.len() as u16);
        for id in &self.ids {
            writer.put_ushort(*id);
        }
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> bool {
        let Some(count) = reader.try_get_ushort() else {
            BNL::log_error("BasisMessageSubscribe: missing count");
            self.ids = Vec::new();
            return false;
        };
        // Each id is 2 bytes on the wire.
        if usize::from(count) * 2 > reader.available_bytes() {
            BNL::log_error(format!("BasisMessageSubscribe: count {count} exceeds available {}", reader.available_bytes()));
            self.ids = Vec::new();
            return false;
        }
        self.ids = vec![0u16; usize::from(count)];
        for i in 0..usize::from(count) {
            match reader.try_get_ushort() {
                Some(id) => self.ids[i] = id,
                None => {
                    BNL::log_error("BasisMessageSubscribe: truncated id list");
                    return false;
                }
            }
        }
        true
    }
}

/// The canonical set of core message descriptors (channels 0-60). Core ids equal their
/// dedicated channel.
pub struct BasisMessageCatalog;

impl BasisMessageCatalog {
    /// Schema version of the core message set. Bump when a core payload layout changes.
    pub const CORE_VERSION: u8 = 1;

    pub fn build_core() -> &'static [BasisMessageDescriptor] {
        static CORE: std::sync::LazyLock<Vec<BasisMessageDescriptor>> = std::sync::LazyLock::new(|| {
            let add = |channel: u8, name: &str| BasisMessageDescriptor {
                id: u16::from(channel),
                version: BasisMessageCatalog::CORE_VERSION,
                channel,
                flags: BasisMessageFlags::NONE.bits(),
                name: name.to_string(),
            };
            type C = BasisNetworkCommons;
            vec![
                add(C::AUTH_IDENTITY_CHANNEL, "basis.core.auth.identity"),
                add(C::META_DATA_CHANNEL, "basis.core.metadata"),
                add(C::DISCONNECTION_CHANNEL, "basis.core.disconnection"),
                add(C::VOICE_CHANNEL, "basis.core.voice"),
                add(C::SHOUT_VOICE_CHANNEL, "basis.core.voice.shout"),
                add(C::AUDIO_RECIPIENTS_CHANNEL, "basis.core.voice.recipients"),
                add(C::PLAYER_AVATAR_VERY_LOW_CHANNEL, "basis.core.avatar.verylow"),
                add(C::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_CHANNEL, "basis.core.avatar.verylow.additional"),
                add(C::PLAYER_AVATAR_LOW_CHANNEL, "basis.core.avatar.low"),
                add(C::PLAYER_AVATAR_LOW_ADDITIONAL_CHANNEL, "basis.core.avatar.low.additional"),
                add(C::PLAYER_AVATAR_MEDIUM_CHANNEL, "basis.core.avatar.medium"),
                add(C::PLAYER_AVATAR_MEDIUM_ADDITIONAL_CHANNEL, "basis.core.avatar.medium.additional"),
                add(C::PLAYER_AVATAR_HIGH_CHANNEL, "basis.core.avatar.high"),
                add(C::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL, "basis.core.avatar.high.additional"),
                add(C::AVATAR_CHANGE_MESSAGE_CHANNEL, "basis.core.avatar.change"),
                add(C::AVATAR_CHANNEL, "basis.core.avatar.data"),
                add(C::CREATE_REMOTE_PLAYER_CHANNEL, "basis.core.player.create"),
                add(C::CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL, "basis.core.player.create.bulk"),
                add(C::CHAT_CHANNEL, "basis.core.chat"),
                add(C::GET_CURRENT_OWNER_REQUEST_CHANNEL, "basis.core.ownership.get"),
                add(C::CHANGE_CURRENT_OWNER_REQUEST_CHANNEL, "basis.core.ownership.change"),
                add(C::REMOVE_CURRENT_OWNER_REQUEST_CHANNEL, "basis.core.ownership.remove"),
                add(C::NET_ID_ASSIGN_CHANNEL, "basis.core.netid.assign"),
                add(C::NET_ID_ASSIGNS_CHANNEL, "basis.core.netid.assigns"),
                add(C::SCENE_CHANNEL, "basis.core.scene.data"),
                add(C::LOAD_RESOURCE_CHANNEL, "basis.core.resource.load"),
                add(C::UNLOAD_RESOURCE_CHANNEL, "basis.core.resource.unload"),
                add(C::PRELOAD_READY_CHANNEL, "basis.core.resource.preloadready"),
                add(C::SPAWN_PRELOADED_CHANNEL, "basis.core.resource.spawnpreloaded"),
                add(C::CONTENT_SHARE_CHANNEL, "basis.core.contentshare"),
                add(C::DELTA_AVATAR_CHANNEL, "basis.core.avatar.delta"),
                add(C::SERVER_BOUND_CHANNEL, "basis.core.serverbound"),
                add(C::ADMIN_CHANNEL, "basis.core.admin"),
                add(C::SERVER_STATISTICS_CHANNEL, "basis.core.statistics"),
                add(C::CAMERA_PIP_STATE_CHANNEL, "basis.core.camera.pip.state"),
                add(C::CAMERA_PIP_POSITION_CHANNEL, "basis.core.camera.pip.position"),
                add(C::EVENTS_CHANNEL, "basis.core.events"),
                add(C::AUDIO_RECIPIENTS_LARGE_CHANNEL, "basis.core.voice.recipients.large"),
                add(C::VOICE_LARGE_CHANNEL, "basis.core.voice.large"),
                add(C::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL, "basis.core.avatar.verylow.large"),
                add(C::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_LARGE_CHANNEL, "basis.core.avatar.verylow.additional.large"),
                add(C::PLAYER_AVATAR_LOW_LARGE_CHANNEL, "basis.core.avatar.low.large"),
                add(C::PLAYER_AVATAR_LOW_ADDITIONAL_LARGE_CHANNEL, "basis.core.avatar.low.additional.large"),
                add(C::PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL, "basis.core.avatar.medium.large"),
                add(C::PLAYER_AVATAR_MEDIUM_ADDITIONAL_LARGE_CHANNEL, "basis.core.avatar.medium.additional.large"),
                add(C::PLAYER_AVATAR_HIGH_LARGE_CHANNEL, "basis.core.avatar.high.large"),
                add(C::PLAYER_AVATAR_HIGH_ADDITIONAL_LARGE_CHANNEL, "basis.core.avatar.high.additional.large"),
                add(C::AUDIO_RECIPIENTS_INVERTED_CHANNEL, "basis.core.voice.recipients.inverted"),
                add(C::AUDIO_RECIPIENTS_INVERTED_LARGE_CHANNEL, "basis.core.voice.recipients.inverted.large"),
                add(C::AUDIO_RECIPIENTS_BITFIELD_CHANNEL, "basis.core.voice.recipients.bitfield"),
                add(C::COMPRESSED_AVATAR_BUNDLE_CHANNEL, "basis.core.avatar.bundle.compressed"),
                add(C::SERVER_LIBRARY_CHANNEL, "basis.core.library"),
                add(C::P2P_CHANNEL, "basis.core.p2p"),
                add(C::MODIFY_RESOURCE_CHANNEL, "basis.core.resource.modify"),
                add(C::DIRECT_SCENE_CHANNEL, "basis.core.scene.direct"),
                add(C::DIRECT_SCENE_SERVER_CHANNEL, "basis.core.scene.direct.server"),
                add(C::DIRECT_AVATAR_CHANNEL, "basis.core.avatar.direct"),
                add(C::DIRECT_AVATAR_SERVER_CHANNEL, "basis.core.avatar.direct.server"),
                add(C::REGISTRY_CONTROL_CHANNEL, "basis.core.registry.control"),
            ]
        });
        &CORE
    }
}
