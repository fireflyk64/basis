//! Port of `Networking/BasisNetworkChat.cs`: server-side chat handling. Deserializes incoming
//! chat, applies word filtering, and broadcasts to all other authenticated peers.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use basis_error::{BasisResult, ResultExt};
use basis_network_core::SerializableBasis::{ChatMessage, PlayerIdMessage, ServerChatMessage};
use basis_network_core::configuration::Configuration;
use basis_network_core::sanitization::basis_chat_sanitizer::BasisChatSanitizer;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};
use parking_lot::RwLock;

use crate::NetworkServer;
use crate::networking::BasisWordFilter;
use crate::security::{BasisGlobalLockManager, PermNodes, PermissionIntegration};

static BLOCKED_WORDS: LazyLock<RwLock<Arc<Vec<String>>>> = LazyLock::new(|| RwLock::new(Arc::new(Vec::new())));

pub struct BasisNetworkChat;

impl BasisNetworkChat {
    pub const DEFAULT_WORD_FILTER: &'static str = concat!(
        "# Chat word filter - one word or phrase per line\n",
        "# Lines starting with # are comments\n",
        "# Words are case-insensitive\n",
        "fuck\nfucking\nfucker\nfucked\nmotherfucker\nshit\nshitting\nbullshit\nbitch\nbitches\nass\nasshole\nbastard\n",
        "damn\ndamned\ncunt\ndick\ndickhead\ncock\ncocksucker\npussy\nwhore\nslut\npiss\npissed\ncrap\nwanker\ntwat\n",
        "prick\ndouche\ndouchebag\n",
        "# Slurs\n",
        "nigger\nnigga\nfaggot\nfag\nretard\nretarded\ntranny\nkike\nspic\nchink\ngook\nwetback\nbeaner\ncoon\ndyke\n",
        "# Threats / harassment\n",
        "kill yourself\nkys\nneck yourself\ngo die\nrape\nraping\nrapist\n"
    );

    pub fn word_filter_file_path() -> PathBuf {
        NetworkServer::config_directory().join("chat_word_filter.txt")
    }

    /// Loads the word filter list from disk. Each line in the file is a blocked word/phrase.
    /// Creates a default file if none exists. No-op when file support is off.
    pub fn load_word_filter(configuration: &Configuration) -> BasisResult<()> {
        if !configuration.has_file_support {
            return Ok(());
        }
        let path = Self::word_filter_file_path();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("creating '{}'", dir.display()))?;
            }
            std::fs::write(&path, Self::DEFAULT_WORD_FILTER).with_context(|| format!("writing '{}'", path.display()))?;
            BNL::log(format!("Created default chat word filter file: {}", path.display()));
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading '{}'", path.display()))?;
        Self::load_word_filter_from_text(&text);
        Ok(())
    }

    /// Installs the blocked words from `text` (one per line, `#` comments, case-insensitive).
    pub fn load_word_filter_from_text(text: &str) {
        let mut words: Vec<String> = Vec::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if !words.iter().any(|w| w.eq_ignore_ascii_case(trimmed)) {
                words.push(trimmed.to_string());
            }
        }
        BNL::log(format!("Loaded {} words into chat filter (using homoglyph + trigram detection)", words.len()));
        *BLOCKED_WORDS.write() = Arc::new(words);
    }

    pub fn blocked_words() -> Arc<Vec<String>> {
        BLOCKED_WORDS.read().clone()
    }

    /// Applies the word filter to a message, replacing blocked words with asterisks. Uses
    /// homoglyph detection (Unicode lookalike characters) and trigram-based false positive
    /// prevention to catch evasion attempts while avoiding incorrect matches (e.g. won't match
    /// "ass" in "assignment").
    pub fn filter_message(message: &str) -> String {
        let words = Self::blocked_words();
        if words.is_empty() || message.is_empty() {
            return message.to_string();
        }
        BasisWordFilter::filter(message, &words)
    }

    /// True when the global text-chat lock is on and this peer lacks basis.chat.lockbypass.
    /// Shared by the chat and typing-state paths so a locked peer can't leak "is typing" activity
    /// while their messages are being dropped.
    pub fn is_chat_blocked_for(peer: &NetPeerRef) -> bool {
        BasisGlobalLockManager::text_chat_locked() && !PermissionIntegration::has_valid_requirement(peer, PermNodes::CHAT_LOCK_BYPASS)
    }

    /// UUID-keyed form of [`is_chat_blocked_for`](Self::is_chat_blocked_for).
    pub fn is_chat_blocked_for_uuid(uuid: &str) -> bool {
        BasisGlobalLockManager::text_chat_locked() && !PermissionIntegration::has_valid_requirement_uuid(uuid, PermNodes::CHAT_LOCK_BYPASS)
    }

    /// Handles an incoming chat message from a client peer: deserializes, filters,
    /// re-serializes, and broadcasts to all other peers.
    pub fn handle_chat_message(mut reader: NetPacketReader, sender: &NetPeerRef) {
        if Self::is_chat_blocked_for(sender) {
            // Dropped silently: clients already grey out their composer from the broadcast lock
            // state, so anything arriving here is an old or modified client. Chat can arrive far
            // faster than content shares, so neither a per-message reply nor a log line is safe —
            // both would hand a blocked peer a cheap amplification vector.
            return;
        }

        let mut chat_message = ChatMessage::default();
        if chat_message.deserialize(&mut reader).is_err() {
            // Same reasoning: a malformed chat packet is not worth a log line per packet.
            return;
        }

        // Decode, filter, re-encode
        let payload_size = usize::from(chat_message.payload_size);
        if payload_size > 0 && chat_message.payload.len() >= payload_size {
            let text = String::from_utf8_lossy(&chat_message.payload[..payload_size]).into_owned();
            let text = Self::filter_message(&text);
            let text = BasisChatSanitizer::sanitize(&text);
            let filtered = text.into_bytes();
            chat_message.payload_size = u16::try_from(filtered.len()).unwrap_or(u16::MAX);
            chat_message.payload = filtered;
        }

        // Wrap with sender ID
        let mut server_chat_message =
            ServerChatMessage { player_id_message: PlayerIdMessage::new(sender.id() as u16), chat_message };

        // Serialize and broadcast to all except sender
        let mut writer = NetworkServer::rent_writer();
        if server_chat_message.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients_excluding(
                &writer,
                BasisNetworkCommons::CHAT_CHANNEL,
                sender,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }
}
