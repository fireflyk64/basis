use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::LazyLock;

use crate::io::{NetDataReader, NetDataWriter};
use crate::BNL;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdminRequest {
    message_index: u8,
}

impl AdminRequest {
    pub fn get_admin_request_mode(&self) -> Option<AdminRequestMode> {
        AdminRequestMode::from_byte(self.message_index)
    }

    /// The raw mode byte, for a value this build does not know.
    pub fn message_index(&self) -> u8 {
        self.message_index
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) {
        let bytes_available = reader.available_bytes();
        if bytes_available > 0 {
            self.message_index = reader.get_byte().unwrap_or(0);
        } else {
            BNL::log_error(format!("Unable to read remaining bytes, available: {bytes_available}"));
        }
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter, admin_request_mode: AdminRequestMode) {
        self.message_index = admin_request_mode as u8;
        writer.put_byte(self.message_index);
    }
}

/// The admin-channel sub-type byte. APPEND ONLY — the discriminant is the wire id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AdminRequestMode {
    Ban = 0,
    Kick = 1,
    IpAndBan = 2,
    Message = 3,
    MessageAll = 4,
    UnBanIP = 5,
    UnBan = 6,
    TeleportAll = 7,
    TeleportPlayer = 8,
    GetPermissions = 9,
    SetUserGroup = 10,
    SetUserNode = 11,
    SetGroupNode = 12,
    CreateGroup = 13,
    DeleteGroup = 14,
    SetGroupParent = 15,
    EnableShoutMode = 16,
    DisableShoutMode = 17,
    GlobalToggleAvatars = 18,
    GlobalToggleProps = 19,
    GlobalToggleWorlds = 20,
    GlobalGetLockState = 21,
    GlobalGetHeadlessAudioState = 22,
    SetGlobalHeadlessAudio = 23,
    GlobalGetHeadlessDisallowState = 24,
    SetGlobalHeadlessDisallow = 25,
    SetGlobalOpusPacketLoss = 26,
    GlobalGetOpusPacketLossState = 27,
    SetUserOpusBitrate = 28,
    UserOpusBitrateOverride = 29,
    SetGlobalOpusFrameDuration = 30,
    GlobalGetOpusFrameDurationState = 31,
    SetServerName = 32,
    SetServerMotd = 33,
    SetAllowlistMode = 34,
    AddAllowlist = 35,
    RemoveAllowlist = 36,
    GlobalToggleServers = 37,
    GlobalToggleThirdPerson = 38,
    AddDefaultLibraryItem = 39,
    RemoveDefaultLibraryItem = 40,
    GlobalToggleAdditionalAvatarDataLock = 41,
    SetGlobalCameraPolicy = 42,
    GlobalGetCrashReportState = 43,
    SetGlobalCrashReporting = 44,
    GlobalGetAudioRangeLimits = 45,
    SetGlobalAudioRangeLimits = 46,
    RequestAllLogs = 47,
    LogBundleBegin = 48,
    LogBundleChunk = 49,
    LogBundleEnd = 50,
    ClearAllScenes = 51,
    DeleteAllLogs = 52,
    GlobalTogglePlayspaceMover = 53,
    GlobalToggleDirectConnect = 54,
    GlobalGetAvatarScaleLimits = 55,
    SetGlobalAvatarScaleLimits = 56,
    GlobalGetResourceLimits = 57,
    SetGlobalResourceLimits = 58,
    GlobalToggleCilbox = 59,
    GlobalToggleImages = 60,
    SetFullQualityBroadcast = 61,
    SetGlobalReductionSettings = 62,
    GlobalGetReductionSettings = 63,
    SetGlobalOpusBitrate = 64,
    GlobalGetOpusBitrateState = 65,
    GlobalToggleEndEffectorIK = 66,
    GlobalToggleTextChat = 67,
    GlobalToggleVoiceChat = 68,
    GlobalToggleMediaPlayer = 69,
    GlobalToggleCameraCapture = 70,
    GlobalTogglePropGrabbing = 71,
    GlobalToggleSafeDisplayNames = 72,
    ForceAvatar = 73,
    ForceAvatarApply = 74,
    ForceAvatarAll = 75,
    SetLocomotionOverride = 76,
    LocomotionOverrideApply = 77,
    SetLocomotionOverrideAll = 78,
    SetGlobalImageBandwidth = 79,
    GlobalGetImageBandwidth = 80,
    SetGlobalPeerLimit = 81,
    GlobalGetPeerLimit = 82,
}

impl AdminRequestMode {
    pub const ALL: [AdminRequestMode; 83] = [
        Self::Ban, Self::Kick, Self::IpAndBan, Self::Message, Self::MessageAll, Self::UnBanIP, Self::UnBan,
        Self::TeleportAll, Self::TeleportPlayer, Self::GetPermissions, Self::SetUserGroup, Self::SetUserNode,
        Self::SetGroupNode, Self::CreateGroup, Self::DeleteGroup, Self::SetGroupParent, Self::EnableShoutMode,
        Self::DisableShoutMode, Self::GlobalToggleAvatars, Self::GlobalToggleProps, Self::GlobalToggleWorlds,
        Self::GlobalGetLockState, Self::GlobalGetHeadlessAudioState, Self::SetGlobalHeadlessAudio,
        Self::GlobalGetHeadlessDisallowState, Self::SetGlobalHeadlessDisallow, Self::SetGlobalOpusPacketLoss,
        Self::GlobalGetOpusPacketLossState, Self::SetUserOpusBitrate, Self::UserOpusBitrateOverride,
        Self::SetGlobalOpusFrameDuration, Self::GlobalGetOpusFrameDurationState, Self::SetServerName,
        Self::SetServerMotd, Self::SetAllowlistMode, Self::AddAllowlist, Self::RemoveAllowlist,
        Self::GlobalToggleServers, Self::GlobalToggleThirdPerson, Self::AddDefaultLibraryItem,
        Self::RemoveDefaultLibraryItem, Self::GlobalToggleAdditionalAvatarDataLock, Self::SetGlobalCameraPolicy,
        Self::GlobalGetCrashReportState, Self::SetGlobalCrashReporting, Self::GlobalGetAudioRangeLimits,
        Self::SetGlobalAudioRangeLimits, Self::RequestAllLogs, Self::LogBundleBegin, Self::LogBundleChunk,
        Self::LogBundleEnd, Self::ClearAllScenes, Self::DeleteAllLogs, Self::GlobalTogglePlayspaceMover,
        Self::GlobalToggleDirectConnect, Self::GlobalGetAvatarScaleLimits, Self::SetGlobalAvatarScaleLimits,
        Self::GlobalGetResourceLimits, Self::SetGlobalResourceLimits, Self::GlobalToggleCilbox,
        Self::GlobalToggleImages, Self::SetFullQualityBroadcast, Self::SetGlobalReductionSettings,
        Self::GlobalGetReductionSettings, Self::SetGlobalOpusBitrate, Self::GlobalGetOpusBitrateState,
        Self::GlobalToggleEndEffectorIK, Self::GlobalToggleTextChat, Self::GlobalToggleVoiceChat,
        Self::GlobalToggleMediaPlayer, Self::GlobalToggleCameraCapture, Self::GlobalTogglePropGrabbing,
        Self::GlobalToggleSafeDisplayNames, Self::ForceAvatar, Self::ForceAvatarApply, Self::ForceAvatarAll,
        Self::SetLocomotionOverride, Self::LocomotionOverrideApply, Self::SetLocomotionOverrideAll,
        Self::SetGlobalImageBandwidth, Self::GlobalGetImageBandwidth, Self::SetGlobalPeerLimit,
        Self::GlobalGetPeerLimit,
    ];

    pub fn from_byte(b: u8) -> Option<Self> {
        Self::ALL.get(usize::from(b)).copied()
    }
}

/// Maps well-known permission nodes to bit indices for compact bitset serialization.
/// APPEND ONLY — never reorder or remove entries, or you break wire compatibility.
pub struct PermissionBitsetMap;

static INDEX_TO_NODE: [&str; 28] = [
    "*",                              // 0
    "basis.server.stats",             // 1
    "basis.resource.load.world",      // 2
    "basis.resource.unload.world",    // 3
    "basis.resource.load.prop",       // 4
    "basis.resource.unload.prop",     // 5
    "basis.resource.load.avatar",     // 6
    "basis.resource.unload.avatar",   // 7
    "basis.ownership.transfer",       // 8
    "basis.ownership.remove",         // 9
    "basis.ownership.get",            // 10
    "basis.contentshare.delete",      // 11
    "basis.contentshare.create",      // 12
    "basis.protection",               // 13
    "basis.configuration",            // 14
    "basis.moderation",               // 15
    "basis.moderation.ban",           // 16
    "basis.moderation.kick",          // 17
    "basis.moderation.ipban",         // 18
    "basis.moderation.unban",         // 19
    "basis.moderation.unbanip",       // 20
    "basis.moderation.message",       // 21
    "basis.moderation.messageall",    // 22
    "basis.moderation.teleport",      // 23
    "basis.moderation.shout",         // 24
    "basis.permissions.view",         // 25
    "basis.permissions.edit",         // 26
    "basis.moderation.headlessaudio", // 27
];

static NODE_TO_INDEX: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
    INDEX_TO_NODE.iter().enumerate().map(|(i, n)| (n.to_lowercase(), i)).collect()
});

impl PermissionBitsetMap {
    pub fn known_count() -> usize {
        INDEX_TO_NODE.len()
    }

    /// Minimum bytes needed to represent all known nodes as bits.
    pub fn byte_count() -> usize {
        (INDEX_TO_NODE.len() + 7) >> 3
    }

    fn index_of(node: &str) -> Option<usize> {
        NODE_TO_INDEX.get(&node.to_lowercase()).copied()
    }

    /// Splits allowed permission nodes into a compact bitset (known nodes) and extras (dynamic
    /// nodes that don't have a bit index). "*" sets all known bits; denied nodes are cleared.
    pub fn encode(allowed_nodes: &[String], denied_nodes: Option<&[String]>) -> (Vec<u8>, Vec<String>) {
        let mut bitset = vec![0u8; Self::byte_count()];
        let mut extras = Vec::new();
        let mut has_wildcard = false;

        for node in allowed_nodes {
            if node == "*" {
                has_wildcard = true;
            }
            match Self::index_of(node) {
                Some(idx) => bitset[idx >> 3] |= 1 << (idx & 7),
                None => extras.push(node.clone()),
            }
        }

        // Wildcard: set every known permission bit so the client sees all nodes explicitly
        if has_wildcard {
            for i in 0..Self::known_count() {
                bitset[i >> 3] |= 1 << (i & 7);
            }
        }

        // Clear any explicitly denied nodes
        if let Some(denied) = denied_nodes {
            for node in denied {
                if let Some(idx) = Self::index_of(node) {
                    bitset[idx >> 3] &= !(1 << (idx & 7));
                }
            }
        }

        (bitset, extras)
    }

    /// Reconstructs the full set of allowed permission strings from bitset + extras. The C# set
    /// was case-insensitive; entries are stored lower-cased so lookups can be too.
    pub fn decode(bitset: &[u8], extras: &[String]) -> HashSet<String> {
        let mut result = HashSet::new();
        let max_bit = Self::known_count().min(bitset.len() << 3);
        for i in 0..max_bit {
            if (bitset[i >> 3] & (1 << (i & 7))) != 0 {
                result.insert(INDEX_TO_NODE[i].to_lowercase());
            }
        }
        for e in extras {
            result.insert(e.to_lowercase());
        }
        result
    }
}

/// Deflate compression for extra permission strings sent over the wire.
/// Wire format: [byte flag][payload...]; flag 0 = raw UTF8, flag 1 = Deflate compressed.
/// Strings are NUL-joined before compression.
pub struct PermissionCompression;

impl PermissionCompression {
    const MAX_DECOMPRESSED_BYTES: usize = 1024 * 1024;

    pub fn compress_extras(strings: &[String]) -> Vec<u8> {
        if strings.is_empty() {
            return Vec::new();
        }
        let raw = strings.join("\0").into_bytes();
        let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&raw).expect("in-memory write");
        let deflated = e.finish().expect("in-memory finish");

        // Pick whichever is smaller (1 byte flag overhead)
        let mut result;
        if deflated.len() < raw.len() {
            result = Vec::with_capacity(1 + deflated.len());
            result.push(1);
            result.extend_from_slice(&deflated);
        } else {
            result = Vec::with_capacity(1 + raw.len());
            result.push(0);
            result.extend_from_slice(&raw);
        }
        result
    }

    pub fn decompress_extras(data: &[u8], expected_count: usize) -> Vec<String> {
        if data.is_empty() || expected_count == 0 {
            return Vec::new();
        }
        let flag = data[0];
        let payload = if flag == 1 {
            let mut d = flate2::read::DeflateDecoder::new(&data[1..]);
            let mut out = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                match d.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if out.len() + read > Self::MAX_DECOMPRESSED_BYTES {
                            return Vec::new();
                        }
                        out.extend_from_slice(&buffer[..read]);
                    }
                    Err(_) => return Vec::new(),
                }
            }
            out
        } else {
            data[1..].to_vec()
        };
        let joined = String::from_utf8_lossy(&payload);
        joined.split('\0').map(|s| s.to_string()).collect()
    }
}
