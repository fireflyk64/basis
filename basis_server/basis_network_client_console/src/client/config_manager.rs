//! Port of `ConfigManager.cs`: the `ClientSimConfig.xml` settings, created with commented
//! defaults on first run and read tolerantly afterwards (a missing or bad element keeps its
//! fallback and says so).

use std::path::Path;
use std::sync::Arc;

use arc_swap::ArcSwap;
use basis_network_core::BNL;
use quick_xml::Reader;
use quick_xml::events::Event;

#[derive(Clone, Debug, PartialEq)]
pub struct ClientSimConfig {
    pub password: String,
    pub ip: String,
    pub port: i32,
    pub client_count: i32,
    pub client_connect_interval_ms: i32,

    pub avatar_password: String,
    pub avatar_url: String,
    pub avatar_load_mode: i32,

    pub use_random_avatar_from_key_store: bool,
    pub avatar_key_store_path: String,

    /// Report a mix of real client platforms instead of "Headless" for every simulated client.
    /// Off by default: a load client honestly IS headless.
    pub simulate_realistic_platforms: bool,
    /// On by default: real players are calibrated, so identity body-fit scales are the
    /// unrealistic case.
    pub simulate_body_fit: bool,
    /// Radius (metres) of the disc simulated clients spawn across. 0 keeps the legacy sub-metre
    /// cluster.
    pub spawn_radius_meters: f32,

    pub simulate_voice: bool,
    pub voice_range_meters: f32,
    pub voice_participant_percent: i32,
    pub voice_talk_burst_min_ms: i32,
    pub voice_talk_burst_max_ms: i32,
    pub voice_silence_min_ms: i32,
    pub voice_silence_max_ms: i32,
    pub voice_chorus_enabled: bool,
    pub voice_chorus_percent: i32,
    pub voice_chorus_duration_min_ms: i32,
    pub voice_chorus_duration_max_ms: i32,
    pub voice_chorus_interval_min_ms: i32,
    pub voice_chorus_interval_max_ms: i32,
    pub voice_recipient_refresh_ms: i32,
    pub voice_audible_timeout_ms: i32,
    pub voice_frame_ms: i32,
    pub voice_bitrate: i32,
    pub voice_bytes_per_frame: i32,
    pub voice_use_system_microphone: bool,
    pub voice_microphone_device: String,
}

impl Default for ClientSimConfig {
    fn default() -> Self {
        Self {
            password: "default_password".into(),
            ip: "localhost".into(),
            port: 4296,
            client_count: 250,
            client_connect_interval_ms: 1,
            avatar_password: "default_avatar_password".into(),
            avatar_url: "http://localhost/avatar".into(),
            avatar_load_mode: 1,
            use_random_avatar_from_key_store: true,
            avatar_key_store_path: String::new(),
            simulate_realistic_platforms: false,
            simulate_body_fit: true,
            spawn_radius_meters: 40.0,
            simulate_voice: true,
            voice_range_meters: 20.0,
            voice_participant_percent: 60,
            voice_talk_burst_min_ms: 500,
            voice_talk_burst_max_ms: 4000,
            voice_silence_min_ms: 4000,
            voice_silence_max_ms: 40000,
            voice_chorus_enabled: true,
            voice_chorus_percent: 85,
            voice_chorus_duration_min_ms: 8000,
            voice_chorus_duration_max_ms: 25000,
            voice_chorus_interval_min_ms: 45000,
            voice_chorus_interval_max_ms: 180000,
            voice_recipient_refresh_ms: 5000,
            voice_audible_timeout_ms: 6000,
            voice_frame_ms: 20,
            voice_bitrate: 32000,
            voice_bytes_per_frame: 60,
            voice_use_system_microphone: false,
            voice_microphone_device: "CABLE Output".into(),
        }
    }
}

static CURRENT: std::sync::LazyLock<ArcSwap<ClientSimConfig>> = std::sync::LazyLock::new(|| ArcSwap::from_pointee(ClientSimConfig::default()));

/// The root element name and the `(name, text)` of each of its direct children.
pub type FlatXml = (Option<String>, Vec<(String, String)>);

pub struct ConfigManager;

impl ConfigManager {
    /// The settings in force. Cheap: one atomic load, so hot loops can call it per tick.
    pub fn current() -> Arc<ClientSimConfig> {
        CURRENT.load_full()
    }

    pub fn set(config: ClientSimConfig) {
        CURRENT.store(Arc::new(config));
    }

    // ---------------- MAIN ENTRY ----------------

    pub fn load_or_create_config_xml(file_path: &str) {
        let path = std::path::absolute(file_path).unwrap_or_else(|_| Path::new(file_path).to_path_buf());
        BNL::log(format!("Config path: {}", path.display()));

        if !path.exists() {
            BNL::log("Config file not found. Creating default.");
            match Self::write_default(&path) {
                Ok(()) => BNL::log("Default config created successfully."),
                Err(e) => BNL::log_error(format!("Failed to create config file.{e}")),
            }
            return;
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                BNL::log_error(format!("Failed to load config XML (corrupt or in use).{e}"));
                return;
            }
        };
        let (root, elements) = match Self::parse_flat_xml(&text) {
            Ok(parsed) => parsed,
            Err(e) => {
                BNL::log_error(format!("Failed to load config XML (corrupt or in use).{e}"));
                return;
            }
        };
        let Some(root) = root else {
            BNL::log("Config XML has no root element.");
            return;
        };
        BNL::log(format!("Root element: {root} | Namespace: ''"));

        let mut config = (*Self::current()).clone();
        Self::apply_elements(&mut config, &elements);
        Self::set(config);
    }

    fn write_default(path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // atomic write
        let temp = path.with_extension("xml.tmp");
        std::fs::write(&temp, Self::default_xml(&ClientSimConfig::default())).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, path).map_err(|e| e.to_string())
    }

    /// The commented default document, element for element what the C# wrote.
    pub fn default_xml(c: &ClientSimConfig) -> String {
        let mut s = String::new();
        s.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        s.push_str("<!-- BasisNetworkClientConsole load-tester configuration. Spawns ClientCount fake clients that connect to a server for stress testing. -->\n");
        s.push_str("<Configuration>\n");
        let mut el = |comment: &str, name: &str, value: String| {
            s.push_str(&format!("  <!-- {comment} -->\n  <{name}>{}</{name}>\n", Self::escape(&value)));
        };
        let b = |v: bool| if v { "true".to_string() } else { "false".to_string() };
        el("Server connection password; must match the server's <Password>. string.", "Password", c.password.clone());
        el("Server host to connect to: hostname or IP (e.g. localhost / 127.0.0.1). string.", "Ip", c.ip.clone());
        el("Server UDP port; must match the server's <SetPort>. int, range 1-65535.", "Port", c.port.to_string());
        el("Number of simulated clients to spawn for load testing. int (>= 1); higher counts need more CPU, memory and sockets.", "ClientCount", c.client_count.to_string());
        el("Delay in ms between starting each simulated client's connection, controlling how fast the crowd ramps up. 0 or less starts them as fast as the loop runs. int.", "ClientConnectIntervalMs", c.client_connect_interval_ms.to_string());
        el("Avatar unlock password/key sent with the avatar; used to decrypt the (encrypted .BEE) bundle at <AvatarUrl>. string.", "AvatarPassword", c.avatar_password.clone());
        el("Avatar source each fake client advertises. For AvatarLoadMode 0 this is the (encrypted .BEE) bundle download URL. string.", "AvatarUrl", c.avatar_url.clone());
        el("How receiving clients load the avatar: 0 = AssetBundle (download from AvatarUrl), 1 = Addressables, 2 = In-scene. Allowed: 0, 1 or 2.", "AvatarLoadMode", c.avatar_load_mode.to_string());
        el("When true, each fake client advertises a random avatar from the Basis client's saved avatars (ItemKeyStore.json, Mode = Avatar) so load tests cover varied avatar types. When false, every client uses the single AvatarUrl/AvatarPassword/AvatarLoadMode above. bool: true or false.", "UseRandomAvatarFromKeyStore", b(c.use_random_avatar_from_key_store));
        el("Path to the avatar keystore (ItemKeyStore.json) read when UseRandomAvatarFromKeyStore is true. Leave empty to auto-detect the local Basis client's persistentDataPath. string.", "AvatarKeyStorePath", c.avatar_key_store_path.clone());
        el("Report a spread of real platforms (WindowsPlayer/Android/etc) instead of Headless. Off by default (a load client really is headless); turn on to measure what a real mixed crowd costs in per-player metadata. bool.", "SimulateRealisticPlatforms", b(c.simulate_realistic_platforms));
        el("Send per-client body-fit scales instead of identity, so the avatar record and join fill carry realistic proportions. bool.", "SimulateBodyFit", b(c.simulate_body_fit));
        el("Radius in metres that simulated clients spawn across. The server reduces avatar quality and send rate by pair distance, so a spread-out crowd is what resting network usage actually looks like; 0 clusters everyone at spawn (worst case). float.", "SpawnRadiusMeters", c.spawn_radius_meters.to_string());
        el("Simulate voice traffic. Each client advertises the peers within VoiceRangeMeters (Basis culls voice client-side) and transmits Opus-sized frames to them. Off = a silent crowd, which understates what a real instance costs. bool.", "SimulateVoice", b(c.simulate_voice));
        el("Audible radius in metres used to build each client's voice recipient list. float.", "VoiceRangeMeters", c.voice_range_meters.to_string());
        el("Percentage of clients that ever transmit; the rest listen or are muted. int, 0-100.", "VoiceParticipantPercent", c.voice_participant_percent.to_string());
        el("Talk-burst length range in ms. Participants alternate bursts and silence instead of holding the mic open, and a client with nobody inside VoiceRangeMeters transmits nothing at all. int.", "VoiceTalkBurstMinMs", c.voice_talk_burst_min_ms.to_string());
        s.push_str(&format!("  <VoiceTalkBurstMaxMs>{}</VoiceTalkBurstMaxMs>\n", c.voice_talk_burst_max_ms));
        let mut el = |comment: &str, name: &str, value: String| {
            s.push_str(&format!("  <!-- {comment} -->\n  <{name}>{}</{name}>\n", Self::escape(&value)));
        };
        el("Silence length range in ms between bursts. With the defaults roughly 6% of the crowd is audible at any moment. int.", "VoiceSilenceMinMs", c.voice_silence_min_ms.to_string());
        s.push_str(&format!("  <VoiceSilenceMaxMs>{}</VoiceSilenceMaxMs>\n", c.voice_silence_max_ms));
        let mut el = |comment: &str, name: &str, value: String| {
            s.push_str(&format!("  <!-- {comment} -->\n  <{name}>{}</{name}>\n", Self::escape(&value)));
        };
        el("Crowd chorus events: everyone singing happy birthday or cheering at once. Independent per-person bursts never produce that spike, and the spike is the peak the server has to survive. A chorus overrides the personal burst clock while it runs. bool.", "VoiceChorusEnabled", b(c.voice_chorus_enabled));
        el("Percentage of voice participants that join a chorus. int, 0-100.", "VoiceChorusPercent", c.voice_chorus_percent.to_string());
        el("How long a chorus lasts, in ms. Happy birthday is about 20 s. int.", "VoiceChorusDurationMinMs", c.voice_chorus_duration_min_ms.to_string());
        s.push_str(&format!("  <VoiceChorusDurationMaxMs>{}</VoiceChorusDurationMaxMs>\n", c.voice_chorus_duration_max_ms));
        let mut el = |comment: &str, name: &str, value: String| {
            s.push_str(&format!("  <!-- {comment} -->\n  <{name}>{}</{name}>\n", Self::escape(&value)));
        };
        el("Gap between chorus events, in ms. int.", "VoiceChorusIntervalMinMs", c.voice_chorus_interval_min_ms.to_string());
        s.push_str(&format!("  <VoiceChorusIntervalMaxMs>{}</VoiceChorusIntervalMaxMs>\n", c.voice_chorus_interval_max_ms));
        let mut el = |comment: &str, name: &str, value: String| {
            s.push_str(&format!("  <!-- {comment} -->\n  <{name}>{}</{name}>\n", Self::escape(&value)));
        };
        el("Milliseconds between voice frames per talking client. 20 ms matches a standard Opus frame (50 packets/sec). int.", "VoiceFrameMs", c.voice_frame_ms.to_string());
        el("How often a client re-derives who can hear it and republishes the recipient list, in ms. Real players join and move, so this is the reaction time. int.", "VoiceRecipientRefreshMs", c.voice_recipient_refresh_ms.to_string());
        el("Drop a player from the audible set after this long with no nearby avatar traffic from them, in ms. int.", "VoiceAudibleTimeoutMs", c.voice_audible_timeout_ms.to_string());
        el("Opus bitrate in bits/sec, matching the real client's encoder. Frame sizes come from the encoder itself. int.", "VoiceBitrate", c.voice_bitrate.to_string());
        el("Fallback payload bytes per frame, used only when native Opus is unavailable on this platform. int.", "VoiceBytesPerFrame", c.voice_bytes_per_frame.to_string());
        el("Transmit a real system recording device instead of the synthetic sweep, so a listener can judge actual voice quality under load. One capture is shared by every voice participant; the burst clock and VoiceRangeMeters culling are unchanged, so you hear real audio from whichever clients are near you. bool.", "VoiceUseSystemMicrophone", b(c.voice_use_system_microphone));
        el("Recording device to capture, matched case-insensitively as a substring (waveIn truncates names to 31 chars, so a prefix like \"CABLE Output\" is safest). Every device is listed at startup. string.", "VoiceMicrophoneDevice", c.voice_microphone_device.clone());
        s.push_str("</Configuration>\n");
        s
    }

    fn escape(value: &str) -> String {
        value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    pub fn parse_flat_xml(text: &str) -> Result<FlatXml, String> {
        let mut reader = Reader::from_str(text);
        let mut depth = 0usize;
        let mut root = None;
        let mut current: Option<String> = None;
        let mut buf = String::new();
        let mut out = Vec::new();
        loop {
            match reader.read_event().map_err(|e| e.to_string())? {
                Event::Start(e) => {
                    depth += 1;
                    let name = e.name().as_ref().to_string();
                    if depth == 1 {
                        root = Some(name);
                    } else if depth == 2 {
                        current = Some(name);
                        buf.clear();
                    }
                }
                Event::Empty(e) => {
                    if depth == 1 {
                        out.push((e.name().as_ref().to_string(), String::new()));
                    }
                }
                Event::Text(t) => {
                    if depth == 2 && current.is_some() {
                        buf.push_str(&t.into_inner());
                    }
                }
                Event::CData(c) => {
                    if depth == 2 && current.is_some() {
                        buf.push_str(&c.into_inner());
                    }
                }
                Event::End(_) => {
                    if depth == 2
                        && let Some(name) = current.take()
                    {
                        out.push((name, buf.trim().to_string()));
                    }
                    depth = depth.saturating_sub(1);
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok((root, out))
    }

    fn find<'a>(elements: &'a [(String, String)], name: &str) -> Option<&'a str> {
        elements.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn read_string(elements: &[(String, String)], name: &str, fallback: &str) -> String {
        match Self::find(elements, name) {
            None => {
                BNL::log(format!("Missing <{name}>, using fallback."));
                fallback.to_string()
            }
            Some(value) => {
                let value = value.trim().to_string();
                BNL::log(format!("Loaded {name}: [{value}]"));
                value
            }
        }
    }

    fn read_int(elements: &[(String, String)], name: &str, fallback: i32) -> i32 {
        match Self::find(elements, name) {
            None => {
                BNL::log(format!("Missing <{name}>, using fallback {fallback}."));
                fallback
            }
            Some(raw) => match raw.trim().parse::<i32>() {
                Ok(value) => {
                    BNL::log(format!("Loaded {name}: {value}"));
                    value
                }
                Err(_) => {
                    BNL::log(format!("Invalid <{name}> value '{raw}', using fallback {fallback}."));
                    fallback
                }
            },
        }
    }

    fn read_bool(elements: &[(String, String)], name: &str, fallback: bool) -> bool {
        match Self::find(elements, name) {
            None => {
                BNL::log(format!("Missing <{name}>, using fallback {fallback}."));
                fallback
            }
            Some(raw) => match raw.trim().to_lowercase().as_str() {
                "true" => {
                    BNL::log(format!("Loaded {name}: true"));
                    true
                }
                "false" => {
                    BNL::log(format!("Loaded {name}: false"));
                    false
                }
                _ => {
                    BNL::log(format!("Invalid <{name}> value '{raw}', using fallback {fallback}."));
                    fallback
                }
            },
        }
    }

    fn read_float(elements: &[(String, String)], name: &str, fallback: f32) -> f32 {
        match Self::find(elements, name) {
            None => {
                BNL::log(format!("Missing <{name}>, using fallback {fallback}."));
                fallback
            }
            Some(raw) => match raw.trim().parse::<f32>() {
                Ok(value) if value.is_finite() => {
                    BNL::log(format!("Loaded {name}: {value}"));
                    value
                }
                _ => {
                    BNL::log(format!("Invalid <{name}> value '{raw}', using fallback {fallback}."));
                    fallback
                }
            },
        }
    }

    pub fn apply_elements(c: &mut ClientSimConfig, e: &[(String, String)]) {
        c.password = Self::read_string(e, "Password", &c.password);
        c.ip = Self::read_string(e, "Ip", &c.ip);
        c.port = Self::read_int(e, "Port", c.port);
        c.client_count = Self::read_int(e, "ClientCount", c.client_count);
        c.client_connect_interval_ms = Self::read_int(e, "ClientConnectIntervalMs", c.client_connect_interval_ms);

        c.avatar_password = Self::read_string(e, "AvatarPassword", &c.avatar_password);
        c.avatar_url = Self::read_string(e, "AvatarUrl", &c.avatar_url);
        c.avatar_load_mode = Self::read_int(e, "AvatarLoadMode", c.avatar_load_mode);
        c.use_random_avatar_from_key_store = Self::read_bool(e, "UseRandomAvatarFromKeyStore", c.use_random_avatar_from_key_store);
        c.avatar_key_store_path = Self::read_string(e, "AvatarKeyStorePath", &c.avatar_key_store_path);
        c.simulate_realistic_platforms = Self::read_bool(e, "SimulateRealisticPlatforms", c.simulate_realistic_platforms);
        c.simulate_body_fit = Self::read_bool(e, "SimulateBodyFit", c.simulate_body_fit);
        c.spawn_radius_meters = Self::read_float(e, "SpawnRadiusMeters", c.spawn_radius_meters);
        c.simulate_voice = Self::read_bool(e, "SimulateVoice", c.simulate_voice);
        c.voice_range_meters = Self::read_float(e, "VoiceRangeMeters", c.voice_range_meters);
        c.voice_participant_percent = Self::read_int(e, "VoiceParticipantPercent", c.voice_participant_percent);
        c.voice_talk_burst_min_ms = Self::read_int(e, "VoiceTalkBurstMinMs", c.voice_talk_burst_min_ms);
        c.voice_talk_burst_max_ms = Self::read_int(e, "VoiceTalkBurstMaxMs", c.voice_talk_burst_max_ms);
        c.voice_silence_min_ms = Self::read_int(e, "VoiceSilenceMinMs", c.voice_silence_min_ms);
        c.voice_silence_max_ms = Self::read_int(e, "VoiceSilenceMaxMs", c.voice_silence_max_ms);
        c.voice_chorus_enabled = Self::read_bool(e, "VoiceChorusEnabled", c.voice_chorus_enabled);
        c.voice_chorus_percent = Self::read_int(e, "VoiceChorusPercent", c.voice_chorus_percent);
        c.voice_chorus_duration_min_ms = Self::read_int(e, "VoiceChorusDurationMinMs", c.voice_chorus_duration_min_ms);
        c.voice_chorus_duration_max_ms = Self::read_int(e, "VoiceChorusDurationMaxMs", c.voice_chorus_duration_max_ms);
        c.voice_chorus_interval_min_ms = Self::read_int(e, "VoiceChorusIntervalMinMs", c.voice_chorus_interval_min_ms);
        c.voice_chorus_interval_max_ms = Self::read_int(e, "VoiceChorusIntervalMaxMs", c.voice_chorus_interval_max_ms);
        c.voice_frame_ms = Self::read_int(e, "VoiceFrameMs", c.voice_frame_ms);
        c.voice_bitrate = Self::read_int(e, "VoiceBitrate", c.voice_bitrate);
        c.voice_recipient_refresh_ms = Self::read_int(e, "VoiceRecipientRefreshMs", c.voice_recipient_refresh_ms);
        c.voice_audible_timeout_ms = Self::read_int(e, "VoiceAudibleTimeoutMs", c.voice_audible_timeout_ms);
        c.voice_bytes_per_frame = Self::read_int(e, "VoiceBytesPerFrame", c.voice_bytes_per_frame);
        c.voice_use_system_microphone = Self::read_bool(e, "VoiceUseSystemMicrophone", c.voice_use_system_microphone);
        c.voice_microphone_device = Self::read_string(e, "VoiceMicrophoneDevice", &c.voice_microphone_device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_document_round_trips() {
        let defaults = ClientSimConfig::default();
        let xml = ConfigManager::default_xml(&defaults);
        let (root, elements) = ConfigManager::parse_flat_xml(&xml).unwrap();
        assert_eq!(root.as_deref(), Some("Configuration"));
        let mut parsed = ClientSimConfig { client_count: 1, ..ClientSimConfig::default() };
        ConfigManager::apply_elements(&mut parsed, &elements);
        assert_eq!(parsed, defaults);
    }

    #[test]
    fn bad_values_keep_fallbacks() {
        let xml = "<Configuration><Port>notaport</Port><SimulateVoice>maybe</SimulateVoice><SpawnRadiusMeters>12.5</SpawnRadiusMeters><Ip> 10.0.0.9 </Ip></Configuration>";
        let (_, elements) = ConfigManager::parse_flat_xml(xml).unwrap();
        let mut config = ClientSimConfig::default();
        ConfigManager::apply_elements(&mut config, &elements);
        assert_eq!(config.port, 4296);
        assert!(config.simulate_voice);
        assert_eq!(config.spawn_radius_meters, 12.5);
        assert_eq!(config.ip, "10.0.0.9");
    }

    #[test]
    fn malformed_xml_is_an_error() {
        assert!(ConfigManager::parse_flat_xml("<Configuration><Port>1</Configuration>").is_err());
    }
}
