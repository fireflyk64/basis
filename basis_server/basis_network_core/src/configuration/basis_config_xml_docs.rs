use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::Path;
use std::sync::LazyLock;

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

use super::ConfigFieldError;
use super::BasisXmlConfig;

/// One documented field: its XML element name, the comment written above it, and an optional
/// section banner written above the comment.
#[derive(Clone, Debug)]
pub struct FieldDoc {
    pub field: &'static str,
    pub comment: &'static str,
    pub section: Option<&'static str>,
}

impl FieldDoc {
    pub const fn new(field: &'static str, comment: &'static str, section: Option<&'static str>) -> Self {
        Self { field, comment, section }
    }
}

struct TypeDoc {
    header: &'static str,
    fields: Vec<FieldDoc>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigXmlError {
    #[error("{0}")]
    Malformed(String),
    #[error("root element is missing")]
    NoRoot,
    #[error("<{0} xmlns=''> was not expected")]
    WrongRoot(String),
    #[error("{0}")]
    BadValue(#[from] ConfigFieldError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("xml write failed: {0}")]
    Xml(String),
}

static DOCS: LazyLock<HashMap<&'static str, TypeDoc>> = LazyLock::new(|| {
    let mut docs = HashMap::new();
    BasisConfigXmlDocs::register_server_config(&mut docs);
    BasisConfigXmlDocs::register_lnl_config(&mut docs);
    BasisConfigXmlDocs::register_iroh_config(&mut docs);
    docs
});

/// Serializes a config object and injects human-readable XML comments before each element, so
/// the generated/saved config files document themselves. Comments are written on every save, so
/// they persist across restarts and saves. Reads ignore comments. A config type with no
/// registered docs is written exactly as `XmlSerializer` would.
pub struct BasisConfigXmlDocs;

impl BasisConfigXmlDocs {
    const XSI: &'static str = "http://www.w3.org/2001/XMLSchema-instance";
    const XSD: &'static str = "http://www.w3.org/2001/XMLSchema";

    /// Serialize `value` with doc comments injected, as the text `XmlSerializer` + `XDocument.Save` produced.
    pub fn serialize<T: BasisXmlConfig>(value: &T) -> Result<String, ConfigXmlError> {
        let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
        let mut root = BytesStart::new(T::XML_ROOT);
        root.push_attribute(("xmlns:xsi", Self::XSI));
        root.push_attribute(("xmlns:xsd", Self::XSD));
        writer.write_event(Event::Start(root)).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;

        let doc = DOCS.get(T::XML_ROOT);
        let by_field: HashMap<&str, &FieldDoc> = doc
            .map(|d| d.fields.iter().map(|f| (f.field, f)).collect())
            .unwrap_or_default();
        if let Some(d) = doc
            && !d.header.is_empty()
        {
            writer.write_event(Event::Comment(BytesText::from_escaped(d.header))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
        }
        for name in T::field_names() {
            if let Some(info) = by_field.get(name) {
                if let Some(section) = info.section
                    && !section.is_empty()
                {
                    writer.write_event(Event::Comment(BytesText::from_escaped(section))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
                }
                if !info.comment.is_empty() {
                    writer.write_event(Event::Comment(BytesText::from_escaped(info.comment))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
                }
            }
            let text = value.get_field(name).unwrap_or_default();
            writer.write_event(Event::Start(BytesStart::new(*name))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
            writer.write_event(Event::Text(BytesText::new(&text))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
            writer.write_event(Event::End(BytesEnd::new(*name))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
        }
        writer.write_event(Event::End(BytesEnd::new(T::XML_ROOT))).map_err(|e| ConfigXmlError::Xml(e.to_string()))?;
        String::from_utf8(writer.into_inner().into_inner()).map_err(|e| ConfigXmlError::Xml(e.to_string()))
    }

    /// Deserializes `xml` the way `XmlSerializer.Deserialize` did: the root must be the type's
    /// element, unknown children are ignored, missing children keep their defaults, and
    /// malformed input is an error.
    pub fn deserialize<T: BasisXmlConfig>(xml: &str) -> Result<T, ConfigXmlError> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut value = T::default();
        let mut buf = Vec::new();
        // Root
        let root_name = loop {
            match reader.read_event_into(&mut buf).map_err(|e| ConfigXmlError::Malformed(e.to_string()))? {
                Event::Start(e) => break e.name().as_ref().to_owned(),
                Event::Eof => return Err(ConfigXmlError::NoRoot),
                Event::Empty(e) => {
                    let name = e.name().as_ref().to_owned();
                    if name != T::XML_ROOT {
                        return Err(ConfigXmlError::WrongRoot(name));
                    }
                    return Ok(value);
                }
                _ => continue,
            }
        };
        if root_name != T::XML_ROOT {
            return Err(ConfigXmlError::WrongRoot(root_name));
        }
        loop {
            match reader.read_event_into(&mut buf).map_err(|e| ConfigXmlError::Malformed(e.to_string()))? {
                Event::Start(e) => {
                    let name = e.name().as_ref().to_owned();
                    let end = e.to_end().into_owned();
                    let text = reader
                        .read_text(end.name())
                        .map_err(|e| ConfigXmlError::Malformed(e.to_string()))?;
                    if T::field_kind(&name).is_some() {
                        let unescaped = quick_xml::escape::unescape(&text)
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| text.to_string());
                        value.set_field(&name, unescaped.trim())?;
                    }
                }
                Event::Empty(e) => {
                    let name = e.name().as_ref().to_owned();
                    if T::field_kind(&name).is_some() {
                        value.set_field(&name, "").map_err(ConfigXmlError::BadValue)?;
                    }
                }
                Event::End(_) | Event::Eof => break,
                _ => {}
            }
        }
        Ok(value)
    }

    /// True when the config on disk predates the current schema and should be re-saved: either
    /// its stamped `ConfigVersion` is below the type's `CurrentConfigVersion`, or the file is
    /// missing any element the current type would write.
    pub fn needs_upgrade<T: BasisXmlConfig>(file_path: &Path, loaded: &T) -> bool {
        if Self::read_version(loaded) < T::CURRENT_CONFIG_VERSION {
            return true;
        }
        Self::is_missing_any_field::<T>(file_path)
    }

    /// True if the XML at `file_path` lacks any element the current shape of `T` would serialize.
    pub fn is_missing_any_field<T: BasisXmlConfig>(file_path: &Path) -> bool {
        let Ok(xml) = std::fs::read_to_string(file_path) else {
            return false;
        };
        let Some(present) = Self::root_child_names(&xml) else {
            return false;
        };
        T::field_names().iter().any(|f| !present.contains(*f))
    }

    fn root_child_names(xml: &str) -> Option<HashSet<String>> {
        let mut reader = Reader::from_str(xml);
        let mut buf = Vec::new();
        let mut depth = 0usize;
        let mut names = HashSet::new();
        loop {
            match reader.read_event_into(&mut buf).ok()? {
                Event::Start(e) => {
                    depth += 1;
                    if depth == 2 {
                        names.insert(e.name().as_ref().to_owned());
                    }
                }
                Event::Empty(e) => {
                    if depth == 1 {
                        names.insert(e.name().as_ref().to_owned());
                    }
                }
                Event::End(_) => depth = depth.saturating_sub(1),
                Event::Eof => break,
                _ => {}
            }
        }
        if depth != 0 { None } else { Some(names) }
    }

    /// Stamp the instance `ConfigVersion` field (if present) up to the type's `CurrentConfigVersion`.
    pub fn stamp_version<T: BasisXmlConfig>(config: &mut T) {
        if T::field_kind("ConfigVersion").is_some() {
            config.set_config_version(T::CURRENT_CONFIG_VERSION);
        }
    }

    /// Schema version found in a just-deserialised config; 0 when it predates versioning.
    pub fn read_version<T: BasisXmlConfig>(config: &T) -> i32 {
        config.config_version()
    }

    /// Doc entries registered for a root element, for tests and tooling.
    pub fn field_docs(root: &str) -> Vec<FieldDoc> {
        DOCS.get(root).map(|d| d.fields.clone()).unwrap_or_default()
    }

    pub fn header(root: &str) -> Option<&'static str> {
        DOCS.get(root).map(|d| d.header)
    }

    fn register_server_config(docs: &mut HashMap<&'static str, TypeDoc>) {
        let mut t = TypeDoc { header: " Basis dedicated-server configuration. Any environment variable whose name matches a field below overrides it at launch (e.g. PeerLimit=256). These comments are emitted from code, so they survive restarts AND in-game admin-panel saves. ", fields: Vec::new() };
        t.fields.push(FieldDoc::new("ConfigVersion", " Config schema version, managed automatically. When the server gains new settings this file is rewritten to add them (with their defaults) and this number is bumped — don't edit by hand. ", None));
        t.fields.push(FieldDoc::new("PeerLimit", " Maximum number of simultaneously connected peers (players). int. Default 65535. ", Some(" ===== Networking / listener ===== ")));
        t.fields.push(FieldDoc::new("SetPort", " UDP port the server binds and listens on; clients connect to this. ushort, range 1-65535. ", None));
        t.fields.push(FieldDoc::new("ServerName", " Display name shown as the row title in client server-list UIs (server-info query). string. ", None));
        t.fields.push(FieldDoc::new("ServerMotd", " Short message-of-the-day returned alongside the server name. string; empty = none. ", None));
        t.fields.push(FieldDoc::new("EnableStatistics", " Collect transport statistics (per-peer/packet counters) and run the stats worker; surfaced via the health endpoint. true|false. ", None));
        t.fields.push(FieldDoc::new("HasFileSupport", " Master switch for writing data to disk: server logs, on-disc moderation lists, auth-identity persistence, chat file support. Set false for an in-memory/ephemeral server. true|false. ", None));
        t.fields.push(FieldDoc::new("HealthCheckHost", " Host/interface the HTTP health endpoint binds. string (hostname or IP). ", Some(" ===== Health-check HTTP endpoint ===== ")));
        t.fields.push(FieldDoc::new("HealthCheckPort", " Port for the HTTP health endpoint. ushort, range 1-65535. ", None));
        t.fields.push(FieldDoc::new("HealthPath", " URL path served by the health endpoint, e.g. /health. string. ", None));
        t.fields.push(FieldDoc::new("HealthIncludeBSRProfiling", " Add a 'bsr' object to the health endpoint response carrying Server Reduction System performance metrics: live tick load (mean tick ms, overrun ratio, tick period, load-shed tier, slicing) plus the last closed 5-second profiling window (per-phase ms/tick, message/send counts, bundle compression). Turning this on starts profiling collection on its own, so EnableBSRProfiling is not required. It also stops the '[BSR Profile]' and '[BSR] Load:' lines being written to the log — the endpoint is serving the same numbers, so they are not duplicated to disc. The endpoint is unauthenticated, so leave this off unless the health port is restricted to trusted callers. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("IdleMemoryReclaimEnabled", " Collect and hand memory back to the OS after the crowd leaves. true|false; default true. An emptied server allocates almost nothing, and collection is demand-driven, so without this nothing triggers a collection and everything the session used stays resident - a 1000-player instance that empties keeps its whole peak footprint until somebody joins again. When the population has fallen to a quarter of its peak and stayed there, an empty server gets a blocking compacting gen2 (nobody is connected to pause) and a still-occupied one gets a background gen2, which frees the same garbage without stopping the players who stayed. Turn it off only if you would rather the memory stayed mapped for the next crowd. ", Some(" ===== Memory ===== ")));
        t.fields.push(FieldDoc::new("IdleMemoryReclaimSettleSeconds", " How long the population must stay down before the collection runs, in seconds. int; default 30. Departing players are torn down over several seconds - the reduction system retires a bounded number of them per tick - so collecting the instant the last one leaves would run while their state is still live and reclaim little. Raise it on a server whose crowd churns in and out. ", None));
        t.fields.push(FieldDoc::new("IdleMemoryReclaimMinimumPeak", " Smallest peak population worth collecting after, in players. int; default 8. A server that never held more than a handful of people has nothing to hand back, and the collection is not free. ", None));
        t.fields.push(FieldDoc::new("BSRSMillisecondDefaultInterval", " Base send interval in milliseconds at zero distance. 50 ms ~= 20 Hz. Lower = more frequent updates and more bandwidth. int (ms). ", Some(" ===== Server Reduction System (avatar sync rate) =====  intervalMs = BSRSMillisecondDefaultInterval * (BSRBaseMultiplier + distance^2 * BSRSIncreaseRate). Nearby players update fast, distant players progressively slower. ")));
        t.fields.push(FieldDoc::new("BSRBaseMultiplier", " Flat multiplier applied to the base interval before distance scaling. number. Default 1. ", None));
        t.fields.push(FieldDoc::new("BSRSIncreaseRate", " How quickly the interval grows with squared distance; higher = distant players update much less often. float. Default 0.005. ", None));
        t.fields.push(FieldDoc::new("BSRSlowestSendRate", " Slowest send-rate floor handed to clients for very distant peers. float; a value of 0 is treated as unset and replaced with 2.55. ", None));
        t.fields.push(FieldDoc::new("DistanceUpdateIntervalTicks", " How many ticks one full refresh of the distance cache is spread over. The cache holds the send interval and quality tier for every pair, and the send loop reads it rather than recomputing distance; this decides how stale those decisions may get. Lower = the tier a pair is served at tracks the distance between them more closely, at the cost of more sweep work per tick. The sweep is O(players^2) per refresh, so the cost of lowering it grows quadratically with population. int (ticks); 125 at a 20 ms tick is a refresh every ~2.5 s. ", None));
        t.fields.push(FieldDoc::new("EnableComputeOffload", " Whether the distance sweep may run on a GPU when one is present. Falls back to the CPU sweep whenever there is no device, no BasisNetworkCompute.dll beside the server, the device refuses to initialise, or the backend disagrees with the CPU on a startup spot-check - so leaving this on costs nothing on a host without a GPU. What it buys is mostly freshness rather than CPU: the sweep is a small share of a broadcast server's time, but it is what sets how stale the per-pair quality tier may get, and a cheaper sweep affords a lower DistanceUpdateIntervalTicks. bool. ", None));
        t.fields.push(FieldDoc::new("ComputeDevice", " Which compute device the distance sweep may use, when EnableComputeOffload is on and this host has more than one. Empty picks the best device present, preferring a CUDA one and then the largest memory. An integer picks by position in the list the server logs at startup. Anything else is matched against the device name, so \"4090\" or \"Radeon\" is enough. A selector that matches no device is an error and the sweep stays on the CPU rather than silently running somewhere else - naming a card and getting a different one would look exactly like it having worked. string. ", None));
        t.fields.push(FieldDoc::new("ComputeDistanceUpdateIntervalTicks", " Refresh period for the distance cache while the sweep is running on a compute device, in ticks. Used INSTEAD of DistanceUpdateIntervalTicks, and only for as long as a device is actually carrying the sweep - if none is found, or one is found and then refused for disagreeing with the CPU, the period reverts to DistanceUpdateIntervalTicks on the spot, so a host without a GPU is never asked to keep up with a schedule fitted to one. This is where the offload is actually spent: moving the sweep to a device saves very little CPU, but it makes the sweep cheap enough to run several times more often, and that is what decides how stale the quality tier a pair is served at may get. int (ticks). ", None));
        t.fields.push(FieldDoc::new("HighQualityDistance", " Distance thresholds (world units/metres) bucketing peers into sync-quality tiers; squared internally. Keep High < Medium < Low. float. ", None));
        t.fields.push(FieldDoc::new("MediumQualityDistance", " Medium-quality distance threshold (see HighQualityDistance). float. ", None));
        t.fields.push(FieldDoc::new("LowQualityDistance", " Low-quality distance threshold (see HighQualityDistance). float. ", None));
        t.fields.push(FieldDoc::new("OverrideAutoDiscoveryOfIpv", " When true, bind exactly the addresses below instead of auto-discovering the IP version. true|false. ", Some(" ===== Address binding ===== ")));
        t.fields.push(FieldDoc::new("IPv4Address", " IPv4 address to bind. 0.0.0.0 = all IPv4 interfaces. string. ", None));
        t.fields.push(FieldDoc::new("IPv6Address", " IPv6 address to bind. ::1 = loopback, :: = all IPv6 interfaces. string. ", None));
        t.fields.push(FieldDoc::new("Password", " Password clients must present to join. Change this for any non-local server! string. ", Some(" ===== Authentication ===== ")));
        t.fields.push(FieldDoc::new("UseAuth", " Require the join password (above) to be correct. true|false. ", None));
        t.fields.push(FieldDoc::new("UseAuthIdentity", " Require cryptographic player-identity (DID) verification in addition to the password. true|false. The headless load-test client console supports this, so it can stay enabled. ", None));
        t.fields.push(FieldDoc::new("NetworkStackId", " Transport stack id. Empty = 'mixed' for a server: iroh (config/transports/iroh.xml) and the LiteNetLib protocol the existing C# clients speak (config/transports/litenetlib.xml) listening side by side, one player-id space, the server relaying between them. 'iroh' or 'litenetlib' runs one stack only; unknown ids fall back to the default. string. ", None));
        t.fields.push(FieldDoc::new("BasisUserRestrictionMode", " Player join restriction mode. Allowed values: Normal | BanList | AllowList | RejoinOnly. RejoinOnly locks the server to the players connected when it was enabled (admins may still join) and resets to Normal on restart. ", None));
        t.fields.push(FieldDoc::new("HowManyDuplicateAuthCanExist", " How many connections sharing the same auth identity may exist at once. int. ", None));
        t.fields.push(FieldDoc::new("AuthValidationTimeOutMiliseconds", " Time a client has to complete auth validation before being dropped. int (ms). ", None));
        t.fields.push(FieldDoc::new("EnableConsole", " Enable the interactive server console (CLI input). Set false for headless/daemon deployments. true|false. ", Some(" ===== Console / persistence ===== ")));
        t.fields.push(FieldDoc::new("EnableAvatarBundleCompression", " Bundle per-receiver avatar messages and send them compressed (falls back to uncompressed per-message when not worthwhile); clients must support the matching decoder. true|false. ", Some(" ===== Avatar bundle compression ===== ")));
        t.fields.push(FieldDoc::new("AvatarBundleMinMessages", " Minimum queued avatar messages to one receiver before a bundle is attempted. int. ", None));
        t.fields.push(FieldDoc::new("AvatarBundleMinBytes", " Minimum uncompressed bundle size (bytes) before compression is attempted. int. ", None));
        t.fields.push(FieldDoc::new("EnableAvatarBundleZstd", " Compress keyframe/full avatar bundles with Zstd against an embedded trained dictionary instead of LZ4 — measured 16.7-18.1% fewer bundle bytes at 250 clients for roughly 2x the compression CPU. Delta-only bundles stay on LZ4 regardless, because Zstd is a 2.8-4.5% loss on those. Inert unless the build embeds a dictionary (check bundles.zstd.dictGeneration on the health endpoint; 0 = none). Clients must support the matching decoder. true|false; default true. ", None));
        t.fields.push(FieldDoc::new("AvatarBundleZstdDeltaBundles", " Also send delta-only bundles through Zstd. Measured a 2.8-4.5% loss against LZ4; exists to re-measure the traffic-class split without a rebuild. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("AvatarBundleZstdLevel", " Zstd compression level for avatar bundles. Negative levels trade ratio for speed. int; default -2, the measured sweet spot (~17.3% saving at ~2.3x LZ4 CPU). -3 costs meaningfully less CPU for ~15-16%. ", None));
        t.fields.push(FieldDoc::new("AvatarBundleZstdMaxShedTier", " Highest BSR load-shed tier (0 healthy .. 2 maximum shedding) at which Zstd is still used; above it every bundle falls back to LZ4. Zstd buys bandwidth with CPU, which is the right trade while the tick has headroom and the wrong one once the server is already shedding avatar quality to keep up. int; default 1. Set 2 to keep Zstd on at every tier, -1 to disable it. ", None));
        t.fields.push(FieldDoc::new("EnableAvatarDeltaCompression", " Send periodic full avatar keyframes with per-field deltas between them on DeltaAvatarChannel instead of all-keyframes; clients must support the delta decoder. true|false. ", Some(" ===== Avatar delta compression ===== ")));
        t.fields.push(FieldDoc::new("AvatarDeltaKeyframeIntervalMs", " Base milliseconds between forced full avatar keyframes per sender. Lower = faster recovery from a lost keyframe, higher = less keyframe bandwidth. int; default 500. ", None));
        t.fields.push(FieldDoc::new("AvatarDeltaKeyframeMaxIntervalMs", " Ceiling for the adaptive keyframe stretch: while a sender's deltas stay tiny (idle avatar) the keyframe interval doubles up to this value; motion snaps it back to the base. Receivers that miss a keyframe request one on demand. 0 or <= base disables. int (ms); default 2000. ", None));
        t.fields.push(FieldDoc::new("StripAdditionalDataAtLowQuality", " Drop AdditionalAvatarData (face blendshapes, custom behaviour params) from the Low and VeryLow avatar tiers — unreadable at those distances; the reliable low-frequency behaviour channel still reaches everyone. High/Medium keep it. true|false; default true. ", None));
        t.fields.push(FieldDoc::new("EnableUplinkAvatarDelta", " Accept client-to-server avatar deltas and advertise support: clients upload a full keyframe every ~500 ms plus small deltas in between instead of full frames every packet (60-90% less avatar ingress). false = clients upload full keyframes only. true|false; default true. ", None));
        t.fields.push(FieldDoc::new("ImageShareEgressMegabitsPerSecond", " Server egress one sharing player may spend on image replication, in megabits per second. A shared image is relayed once per recipient who is not on a direct P2P link, so this budget divided by the fan-out is the rate the sharer actually uploads at - at the old client-side assumption of 4 Mb/s a twenty-player instance moved a picture at about 25 KB/s. Sized per sharer, so the worst case is this times the number of people sharing at once; divide it down on a small pipe and raise it on a large one. int (Mb/s); 0 leaves the client on its own conservative default; default 200. This figure is BOTH advertised to clients so they pace themselves and enforced server-side so a modified one cannot ignore it - see ImageShareEgressEnforcementPercent. Editable live from the admin panel. ", None));
        t.fields.push(FieldDoc::new("ImageShareDownloadMegabitsPerSecond", " Rate the server replays cached images to ONE arriving player, in megabits per second. This is the download side of image sharing and the server's own send: when somebody joins, the cache hands them every image the room already holds so the original sharers do not have to send them again. Unpaced, that replay goes out in a single burst - an instance near the cache ceiling pushes hundreds of megabytes into one peer's queue the moment it connects, which is a bad first ten seconds for that player and a memory spike for everyone. Sized per arriving player, so a join burst after a restart costs this much each. int (Mb/s); 0 = unpaced (the old behaviour); default 200. Editable live from the admin panel. ", None));
        t.fields.push(FieldDoc::new("ImageShareEgressEnforcementPercent", " How far over ImageShareEgressMegabitsPerSecond a client may go before the server starts dropping its image traffic, as a percentage. The advertised budget and the enforced one must not be the same number: a well-behaved client paces itself against the advertised figure but measures on its own clock, rounds chunks its own way and bursts across tick boundaries, so enforcing at exactly the advertised rate would break honest transfers on jitter alone - far worse than the abuse it is meant to stop. int; minimum 100, default 150 (drop only once a sender sustains half again what it was told it could have). Editable live from the admin panel. ", None));
        t.fields.push(FieldDoc::new("ImagePickupRangeMeters", " Maximum distance in metres between an image pickup and a player for that player to be sent the image. Advertised to clients and applied by the sharing client, so this is a bandwidth budget rather than an access control - nothing on the server or the receiver rejects an out-of-range image. Players entering range later receive a catch-up transfer, from the owner while the server is not holding the image and from the server image cache once it is. The cache never learns where anybody is: it offers each held image's spawn header - tens of bytes - and the receiving client measures the distance itself and asks for the ones it wants. 0 is unlimited - every player receives every image, which is how it behaved before this setting. float (metres); default 64. ", None));
        t.fields.push(FieldDoc::new("EnableBSRProfiling", " Emit Server Reduction System profiling output. true|false. ", None));
        t.fields.push(FieldDoc::new("LogConnectionHandshake", " Log the per-connection authentication handshake ('Processing connection from peer N' and 'Sending out Writer with size : N'). Off by default: that is two lines per joiner, and a mass rejoin after a restart writes them for the whole population at once - four thousand lines at 2000 players, none of which a reader can act on. 'Peer connected: N' and every rejection are logged regardless, so the default still shows who got in and who did not. Turn this on only to trace a handshake failing between those two points. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("BSRMaxSliceCount", " Furthest the Server Reduction System may slice its roster under load. int; 0 = scale with player count, which is recommended. At slice N each tick serves only 1/N of the receivers, so everyone's update rate drops uniformly - it is the last-resort lever, used only after stretching the tick period and shedding distant players. This cap decides how far the server may degrade before it stops degrading and simply starts missing its tick instead. It used to be a fixed 32, chosen when 2000 was a large instance; at 8000 players a cap of 32 still leaves 250 receivers per tick, so a struggling server reaches the ceiling with nowhere left to go. Automatic keeps the per-tick fan-out roughly flat as population grows. Set a positive value only to pin it. ", None));
        t.fields.push(FieldDoc::new("BSRMaxDegreeOfParallelism", " Worker cap for the Server Reduction System's parallel phases (send loop, message processing, distance sweep). int; 0 = automatic and recommended. Automatic scales the pool with the player count and caps it at the share of the machine the core allocator has granted this phase - a share whose ceiling is measured at runtime rather than assumed, so it already tracks the hardware. Setting a number here overrides all of that, including the measurement, and is clamped to the core count. The tick runs hundreds of times a second, so every worker costs dispatch and GC-poll traffic per tick; once the per-tick slice is large enough to keep them busy, extra workers cost more than they return. Set it only to hold the server down on a box shared with other services. Watch the 'send N/M workers' figures in the [CPU] log line to see what automatic is choosing. ", None));
        t.fields.push(FieldDoc::new("BSRSendPhaseBudgetPercent", " Share of the Server Reduction System's tick period that the send pass is sized against, as a percentage. int; 0 = the fitted default of 60, clamped to 20..85. The width of the send pool is worked out from a throughput rate this host measures for itself - sender/receiver pairs one worker gets through per busy millisecond - so the only thing left to choose is how many of the period's milliseconds that pass may spend, and rate times milliseconds is a worker count. The remainder of the period is not spare: the queue drain, message processing, the distance slice and the transport kick all run in the same tick, and how much they cost is a property of the machine - which is why this is a setting and not a constant. Too high and the send pass comfortably fits its budget while the tick overruns regardless; the load controller then starts shedding players and nothing in the logs points here. Too low and the pool is sized wider than the box while the tick sits half idle. The process cannot fit this itself, because the send pass has no view of what the phases beside it cost, so BasisServerBenchmark measures the split under load and writes the value. By hand: from the [CPU/POP] line take the budget duty times this percentage to get the send pass's share of the tick, subtract it from tick-ms over interval-ms to get what everything else costs, and leave that plus about ten points of headroom out of this number. ", None));
        t.fields.push(FieldDoc::new("DisallowHeadless", " Reject headless clients from connecting. true|false. ", None));
        t.fields.push(FieldDoc::new("AvatarsLocked", " Block avatar loading for non-bypass users. true|false. ", Some(" ===== Content lockouts (seed BasisGlobalLockManager at boot; each can also be toggled live from the admin panel). Users need the matching basis.resource.lockbypass.{avatar,prop,world} permission to load while locked. ===== ")));
        t.fields.push(FieldDoc::new("PropsLocked", " Block prop loading for non-bypass users. true|false. ", None));
        t.fields.push(FieldDoc::new("WorldsLocked", " Block world loading for non-bypass users. true|false. ", None));
        t.fields.push(FieldDoc::new("ServersLocked", " Block sharing of saved-server entries through the content-share system. true|false. ", None));
        t.fields.push(FieldDoc::new("ThirdPersonDisabled", " Tell every client to hard-disable the desktop third-person camera. true|false. ", None));
        t.fields.push(FieldDoc::new("AdditionalAvatarDataLock", " Strip AdditionalAvatarDatas (blendshapes, custom-behaviour params) from inbound avatar sync before relaying; muscle/position/rotation still sync. true|false. ", None));
        t.fields.push(FieldDoc::new("CameraMetadataDisallowMask", " Bitmask of camera photo-metadata categories disallowed for all clients (set bit = disallowed). 0 = everything allowed. byte, range 0-255; the category-to-bit mapping is defined client-side. ", None));
        t.fields.push(FieldDoc::new("CrashReportingEnabled", " Allow clients to send a one-shot report of each error/exception they hit to the server, stored under CrashReports/<uuid>.jsonl with their UUID and display name. true|false; default true. Set false to globally disable reporting (clients are told to stop sending). ", Some(" ===== Diagnostics ===== ")));
        t.fields.push(FieldDoc::new("MaxMicrophoneRangeMeters", " Maximum microphone (voice transmit) range, in metres, a client may set. Clients clamp their Microphone Range slider and effective range to this ceiling; can also be changed live from the admin panel. float; default 25. ", Some(" ===== Audio / voice range ===== ")));
        t.fields.push(FieldDoc::new("MaxHearingRangeMeters", " Maximum hearing (audio receive) range, in metres, a client may set. Clients clamp their Hearing Range slider and effective range to this ceiling. float; default 25. ", None));
        t.fields.push(FieldDoc::new("VoiceFrameDurationMs", " Opus voice frame duration pushed to every client at join. 20 = low-latency default; 40 halves the voice packet rate (25/s vs 50/s) and its per-packet overhead for +20 ms voice latency — worth trying on bandwidth-constrained servers. Admins can still change it live. 20|40; default 20. ", None));
        t.fields.push(FieldDoc::new("MinAvatarEyeHeightMeters", " Minimum avatar eye height, in metres, a non-admin player may scale to. Clients clamp their avatar scale to this floor. float; default 0.1 (effectively no minimum). Admins (basis.moderation.globallock) bypass it. ", Some(" ===== Avatar scale + movement restrictions (seed at boot; toggle live from the admin panel). Admins with basis.moderation.globallock bypass these. ===== ")));
        t.fields.push(FieldDoc::new("MaxAvatarEyeHeightMeters", " Maximum avatar eye height, in metres, a non-admin player may scale to. Clients clamp their avatar scale to this ceiling. float; default 100 (effectively no maximum). ", None));
        t.fields.push(FieldDoc::new("PlayspaceMoverLocked", " Stop non-admin players from using the playspace mover (grabbing/dragging/rotating/scaling their play space). true|false; default false. ", None));
        t.fields.push(FieldDoc::new("DirectConnectLocked", " Refuse to broker direct (peer-to-peer) connections for non-admin players; clients also hide the direct-connect control. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("CilboxLocked", " Tell every client to block sandboxed Cilbox code on avatars from running (props/worlds keep their own). true|false; default false. ", None));
        t.fields.push(FieldDoc::new("ImagesLocked", " Stop non-bypass clients from sharing new image pickups and from accepting inbound ones. Enforced client-side (image pickups ride the generic scene relay). true|false; default false. ", None));
        t.fields.push(FieldDoc::new("EndEffectorIKDisabled", " Tell every client to stop two-bone-IK anchoring remote avatars' tracked hands/feet, falling back to pure-FK playback. true|false; default false (feature on). ", None));
        t.fields.push(FieldDoc::new("TextChatLocked", " Drop text chat messages and typing state from peers lacking basis.chat.lockbypass. Enforced server-side, so a modified client cannot talk past it. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("VoiceChatLocked", " Drop voice (normal and shout) from peers lacking basis.voice.lockbypass. Enforced server-side, so a modified client cannot talk past it. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("MediaPlayerLocked", " Stop non-bypass clients from loading new media player URLs and from accepting inbound ones. Enforced client-side (media state rides the generic scene relay). Already-playing media keeps playing. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("CameraCaptureLocked", " Stop non-bypass clients from taking photos with the handheld camera. Enforced client-side (capture is entirely local). Separate from CameraMetadataDisallowMask, which only strips metadata. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("SafeDisplayNamesForced", " Render other players' display names with rich-text markup stripped and TMP rich text off. Enforced client-side. Stops name markup being used to draw over the screen. true|false; default false. ", None));
        t.fields.push(FieldDoc::new("PropGrabbingLocked", " Stop non-bypass clients from picking up or grabbing props. Enforced client-side (grabbing is local interaction logic). Separate from PropsLocked, which blocks prop loading instead. true|false; default false. ", None));
        docs.insert("Configuration", t);
    }
    fn register_lnl_config(docs: &mut HashMap<&'static str, TypeDoc>) {
        let mut t = TypeDoc { header: " LiteNetLib transport tuning (sidecar for the 'litenetlib' network stack). Maps onto LiteNetLib's NetManager. Fields marked [NOT APPLIED] are serialized but not currently wired into the server's NetManager. Comments are emitted from code, so they survive restarts and saves. ", fields: Vec::new() };
        t.fields.push(FieldDoc::new("ConfigVersion", " Config schema version, managed automatically; new settings are added to this file on load — don't edit by hand. ", None));
        t.fields.push(FieldDoc::new("UseNativeSockets", " Use OS-native socket calls instead of the managed path (lower overhead). true|false. ", None));
        t.fields.push(FieldDoc::new("NatPunchEnabled", " Enable the NAT punch-through module for peer introduction. true|false. ", None));
        t.fields.push(FieldDoc::new("NatPortPredictionRange", " Hard-NAT traversal: how many sequential ports above a peer's server-observed external port the OTHER peer also punches (port-prediction spray). Helps sequential symmetric/CGNAT mappings where the peer-to-peer port differs from the one the server sees. 0 disables the spray; sane range 0-128. int. ", None));
        t.fields.push(FieldDoc::new("PingInterval", " Interval between keep-alive pings to each peer. int (ms). ", None));
        t.fields.push(FieldDoc::new("DisconnectTimeout", " Time with no response from a peer before it is disconnected. int (ms). ", None));
        t.fields.push(FieldDoc::new("SimulatePacketLoss", " Artificially drop outgoing packets. true|false. ", Some(" ===== Debug network simulation (testing only) ===== ")));
        t.fields.push(FieldDoc::new("SimulateLatency", " Artificially delay packets. true|false. ", None));
        t.fields.push(FieldDoc::new("SimulationPacketLossChance", " Packet-loss percentage when SimulatePacketLoss is true. int, range 0-100. ", None));
        t.fields.push(FieldDoc::new("SimulationMinLatency", " Minimum added latency when SimulateLatency is true. int (ms). Keep Min <= Max. ", None));
        t.fields.push(FieldDoc::new("SimulationMaxLatency", " Maximum added latency when SimulateLatency is true. int (ms). ", None));
        t.fields.push(FieldDoc::new("ReconnectDelay", " [NOT APPLIED] Delay between reconnect attempts (client-side connect option). int (ms). ", None));
        t.fields.push(FieldDoc::new("MaxConnectAttempts", " [NOT APPLIED] Connection attempts before giving up (client-side connect option). int. ", None));
        t.fields.push(FieldDoc::new("ReuseAddresss", " [NOT APPLIED] Set the SO_REUSEADDR socket option (note the field name spelling). true|false. ", None));
        t.fields.push(FieldDoc::new("DontRoute", " [NOT APPLIED] Set the SO_DONTROUTE socket option (bypass routing tables). true|false. ", None));
        t.fields.push(FieldDoc::new("IPv6Enabled", " Enable IPv6 (dual-stack) socket support. true|false. ", None));
        t.fields.push(FieldDoc::new("MtuOverride", " Force a fixed MTU instead of negotiating. int (bytes); 0 = auto/disabled. ", None));
        t.fields.push(FieldDoc::new("MtuDiscovery", " Enable path-MTU discovery. true|false. ", None));
        t.fields.push(FieldDoc::new("DisconnectOnUnreachable", " [NOT APPLIED] Disconnect a peer when an ICMP 'unreachable' is received. true|false. ", None));
        t.fields.push(FieldDoc::new("AllowPeerAddressChange", " Allow a peer's remote endpoint (IP/port) to change mid-session, e.g. mobile network roaming. true|false. ", None));
        t.fields.push(FieldDoc::new("MergeHoldMs", " How long, in milliseconds, a partly-filled packet-merge buffer may wait for more data before being sent. float; 0 = send on every logic pass (legacy). The logic loop runs hundreds of times a second, so flushing every pass emits many half-empty datagrams and the server pays full per-packet cost for each; holding a partial buffer briefly lets consecutive passes coalesce. A buffer that fills the MTU is always sent immediately, so this caps added latency rather than adding it — only small sends ever wait, and never longer than this value. 2-5 is a reasonable range; raise it to cut packet rate further, lower it if voice latency matters more than CPU. ", None));
        t.fields.push(FieldDoc::new("CompactMerged", " Frame merged unreliable traffic with the compact per-entry format. true|false; true is recommended. A merged message used to carry four bytes of framing (a two-byte nested length plus its own property and channel bytes); the compact form drops the property byte, since the datagram already says everything inside it is unreliable, and uses a one-byte length for payloads up to 255 - two bytes of framing, or three above 255. Different traffic still shares one MTU-sized datagram, so avatar updates and voice keep riding together. Measured at 500 players: 0.93% less total egress (~4.97 Mbit/s, about 2.24 GB/hour), 0.37% fewer UDP packets, no CPU change, no drops. This is a send-side setting and is safe to change on one end only, because both framings are always decoded; every client able to connect understands it, which the transport protocol id and the server version check together guarantee. Turn it off only to A/B the saving or to rule the framing out while diagnosing something else. ", None));
        t.fields.push(FieldDoc::new("MaxUnreliableQueuePerPeer", " Maximum unreliable packets queued per peer before the oldest are dropped. int; 0 = size automatically from player count and available memory, which is recommended. This is the backstop that keeps an overloaded server alive: with no bound at all, a server that cannot drain its send queue grows the backlog instead of shedding, and at 2000 players that backlog reached ~40 GB before every peer timed out. Oldest are dropped first because a newer position update supersedes them. WARNING: this used to be a fixed 256, which is too small to be only a backstop - at 2000 players it discarded roughly half of every avatar update produced, and because discarding is cheaper than sending, the reduction system read the resulting fast ticks as spare capacity and produced even more. Raising it to 4096 on identical load measured zero drops, 22% more delivered bytes and 21% less CPU. Automatic sizes it per box; set a positive value only to pin it for a reproducible measurement. Applies to bulk state traffic only - voice has its own queue and its own bound, see MaxPriorityUnreliableQueuePerPeer. ", None));
        t.fields.push(FieldDoc::new("MaxPriorityUnreliableQueuePerPeer", " Maximum voice packets queued per peer before the oldest are dropped. int; 0 = size automatically from player count and available memory, which is recommended. Voice is queued separately from bulk avatar traffic and drained first, so a backlog of position updates can neither delay it nor shed it. That separation is the fix for a real bug: the bulk queue drops oldest-first because a newer avatar update supersedes the one behind it, which is not true of audio, so voice sharing that queue was being destroyed at the bulk stream's drop rate and whatever survived arrived behind the backlog, too late to play. This queue is allowed to be DEEPER than the bulk one, which sounds backwards and is the whole point: bulk depth buys avatar frames that the next frame replaces anyway, while voice depth buys audio that has no replacement. Measured at 1000 clients on a deliberately starved server, moving budget from bulk to voice improved both at once - voice delivered 85.7% to 93.6%, peak RSS 7.8 GB to 4.6 GB. A flat 256 here delivered only 32.8%, because a receiver in a crowd takes a voice packet from every audible talker every frame period and 256 covers single-digit milliseconds of that. Watch droppedVoice on /health, which should stay flat. ", None));
        t.fields.push(FieldDoc::new("MaxReliableQueueBytesPerPeer", " Bytes of reliable messages that may be queued for one peer before sends to it are refused. A peer that stays over it for ReliableQueueGraceMs is disconnected: a client that stops reading - stalled, or hostile - must cost a disconnect, not the server's memory, which is what an unbounded reliable queue costs. 0 = a share of the box's memory divided by the population (256 KiB to 8 MiB per peer). int. ", Some(" ===== Bounds (the difference between a slow client and a denial of service) ===== ")));
        t.fields.push(FieldDoc::new("ReliableQueueGraceMs", " How long a peer may stay over its reliable byte budget before it is disconnected with the reason 'send queue over budget'. int, milliseconds. ", None));
        t.fields.push(FieldDoc::new("MaxFragmentBytesPerPeer", " Bytes of incomplete fragment sets one peer may make the server hold; further sets are dropped until a set completes or goes stale. 0 = 8 MiB. int. ", None));
        t.fields.push(FieldDoc::new("MaxPendingRequests", " Connection requests awaiting a verdict at once; further ones are dropped (the client resends). 0 = 4096. int. ", None));
        t.fields.push(FieldDoc::new("MaxRejectPeers", " Rejected connections the server keeps state for so the reject reason is delivered reliably; past this a rejection is one datagram and no state, which is what a flood of bad passwords from spoofed addresses gets. 0 = 256. int. ", None));
        t.fields.push(FieldDoc::new("PeerUpdatePeersPerWorker", " Peers each worker in the transport's per-peer update pass is expected to service. int; 0 = 128. Lower means more workers for the same player count. This is the setting that decides how much of a large machine the server can actually use: the default was fitted to a 32-thread host, and it sizes the pool by population rather than by the machine, so at 4000 peers it picks 31 workers however many cores exist - a 128-core host then sits near a quarter utilisation. Halve it to double the workers. Tune it against the pass time in the [CPU] log line: above 25 ms with cores to spare means this is too high. Machines with many slower cores want a lower value than few fast ones, because each worker gets through fewer peers per pass. ", None));
        t.fields.push(FieldDoc::new("PeerUpdateParallelism", " Worker cap for the transport's per-peer update pass. int; 0 = automatic and recommended. Automatic sizes the pool from the peer count and holds it inside the share the core allocator has granted this pass, which moves with load - the pass widens while it is behind and gives the cores back when it recovers. Setting a number here pins the pool and opts out of that, and is clamped to the core count. The pass runs hundreds of times a second and does little work per peer, so letting it spread across every core costs more in thread wake-up and GC-poll traffic than it saves. Watch the 'peer-update N/M workers' figures and the pass time in the [CPU] log line before overriding. ", None));
        t.fields.push(FieldDoc::new("MaxSendSockets", " Ceiling on sockets the server may add at runtime when the network path is what limits it. int; 0 = auto (half the CPU cores, floored at 4, never above the core count). Linux only - needs SO_REUSEPORT. Each socket is both an extra send path and an extra receive thread. Sends: the send loop gets SLOWER with more threads on one socket - measured at 1000 players, 8 to 16 to 32 send workers took the update phase from 6.1 to 12.9 to 15.4 ms per tick while throughput fell from 497 to 393 MB/s - so another socket, not another core, is what adds capacity. Receives: one receive thread is one core's worth of syscall throughput, and past that the kernel discards inbound datagrams; this never appears as high CPU because the thread is pinned either way, so it is detected from the kernel's RcvbufErrors counter. On machines with many weak cores the useful range runs to 64 sockets, which is where the auto derivation lands on a 128-core host. Growth needs sustained pressure, except on receive drops which act immediately - and a socket added for drops is then checked: if the drop rate does not fall, growth stops, because the cause is not something more receive threads can fix (raise sysctl net.core.rmem_max, or the link is full). Grow-only. ", None));
        t.fields.push(FieldDoc::new("MultiSocketCount", " Number of UDP sockets to bind on the listen port using SO_REUSEPORT (Linux only). 1 = single socket / single receive thread (default). N>1 spawns N-1 extra sockets + receive threads so the Linux kernel RSS-hashes inbound 4-tuples across them; per-peer packet order is preserved because each peer's 4-tuple is stable. On Windows/macOS this falls back to 1 with a warning at Start(). Sensible range: 2-4 around 1k players, 4-8 around 2k. int. ", None));
        t.fields.push(FieldDoc::new("PacketPoolSizePerPeer", " Recycled packets kept per connected peer, so the packet pool grows with the crowd instead of hitting a fixed wall. The pool absorbs the gap between a burst of sends draining it and those packets being returned, and that gap scales with peer count: measured at ~2,850 players a fixed 65,536 pool swung between full (recycled packets discarded) and empty (every send allocating), with ~36% of pool requests allocating. The effective ceiling is peers x this value, floored at PacketPoolSize and capped by PacketPoolSizeMax. 0 disables scaling. int. ", None));
        t.fields.push(FieldDoc::new("PacketPoolSizeMax", " Hard ceiling on the scaled packet pool, so peer count cannot turn into unbounded memory. Each pooled packet retains its buffer (up to one MTU), so this is roughly the worst-case pool footprint in packets. int; 0 = size automatically from available memory, which is recommended. This used to be a fixed 262144, which is the wall from roughly 5400 peers upward: at 8000 peers the per-peer rule asks for 384000 and every recycle past the cap was discarded for the garbage collector to re-allocate. Set a positive value only to pin it. ", None));
        docs.insert("LNLTransportConfig", t);
    }
    fn register_iroh_config(docs: &mut HashMap<&'static str, TypeDoc>) {
        let mut t = TypeDoc {
            header: " iroh transport tuning (sidecar for the 'iroh' network stack). iroh carries the Basis protocol over QUIC: reliable channels ride streams, unreliable ones ride datagrams, and peers reach each other through hole punching with relay servers as the fallback. Comments are emitted from code, so they survive restarts and saves. ",
            fields: Vec::new(),
        };
        t.fields.push(FieldDoc::new("ConfigVersion", " Config schema version, managed automatically; new settings are added to this file on load — don't edit by hand. ", None));
        t.fields.push(FieldDoc::new("RelayMode", " Which relay servers the endpoint uses when a direct path cannot be found. 'default' = the n0 public relays, 'disabled' = direct connections only (LAN, tests), 'custom' = the URLs in RelayUrls. string. ", Some(" ===== Connectivity ===== ")));
        t.fields.push(FieldDoc::new("RelayUrls", " Comma-separated relay URLs used when RelayMode is 'custom'. string. ", None));
        t.fields.push(FieldDoc::new("MaxReliableQueueBytesPerPeer", " Bytes of reliable messages that may be queued for one peer before sends to it are refused. A peer that stays over it for ReliableQueueGraceMs is disconnected: a client that stops reading must cost a disconnect, not the server's memory. 0 = a share of the box's memory divided by the population (256 KiB to 8 MiB per peer). int. ", Some(" ===== Bounds (the difference between a slow client and a denial of service) ===== ")));
        t.fields.push(FieldDoc::new("ReliableQueueGraceMs", " How long a peer may stay over its reliable byte budget before it is disconnected. int, milliseconds. ", None));
        t.fields.push(FieldDoc::new("SendWindowBytes", " Bytes QUIC may hold per connection for data the far side has not acknowledged yet. 0 = 8 MiB. int. ", None));
        t.fields.push(FieldDoc::new("ReceiveWindowBytes", " Bytes QUIC may hold per connection for data this side has not read yet. 0 = 32 MiB. int. ", None));
        t.fields.push(FieldDoc::new("MaxPendingHandshakes", " Connections that may sit between the QUIC handshake and a connect verdict at once; further ones are closed at once. 0 = 1024. int. ", None));
        t.fields.push(FieldDoc::new("Port", " UDP port the iroh endpoint binds. 0 = the server's SetPort when iroh is the only stack; on the mixed stack LiteNetLib keeps SetPort (the port every deployed client knows) and iroh takes SetPort + 1. ushort. ", None));
        t.fields.push(FieldDoc::new("SecretKeyFile", " File (under the config folder) holding the server's iroh secret key, so its endpoint id survives restarts. Created on first boot when missing. string. ", None));
        t.fields.push(FieldDoc::new("PublishAddress", " Publish the server's endpoint address through iroh's DNS/pkarr discovery so clients can dial it by endpoint id alone. Needs outbound internet; off for LAN servers and tests. true|false. ", None));
        t.fields.push(FieldDoc::new("IdleTimeoutMs", " Time with no traffic from a peer before its connection is considered lost, in milliseconds. The QUIC idle timeout; keep-alives are sent at a third of it. int (ms). ", Some(" ===== Timeouts ===== ")));
        t.fields.push(FieldDoc::new("KeepAliveIntervalMs", " Interval between QUIC keep-alive probes on an otherwise idle connection. 0 = a third of IdleTimeoutMs. int (ms). ", None));
        t.fields.push(FieldDoc::new("MaxDatagramQueuePerPeer", " Unreliable datagrams buffered per peer before the oldest are dropped. int; 0 = size automatically from population and memory, which is recommended. This is the same backstop the LiteNetLib transport's MaxUnreliableQueuePerPeer was: it keeps an overloaded server alive by shedding stale position updates instead of growing a backlog. ", Some(" ===== Queues ===== ")));
        t.fields.push(FieldDoc::new("MaxPriorityDatagramQueuePerPeer", " Voice datagrams buffered per peer ahead of bulk traffic before the oldest are dropped. int; 0 = automatic. Voice is drained first and bounded separately because a newer avatar update supersedes the one behind it and a voice packet does not. ", None));
        t.fields.push(FieldDoc::new("TokioWorkerThreads", " Threads in the async runtime that drives every socket, stream and timer. int; 0 = automatic (one per core, leaving the reduction system's share). The runtime maps many tasks onto these threads - the m:n scheduling a 10 GigE host needs to keep every core busy - so this is the knob to raise before anything else when the [CPU] line shows the transport behind. ", Some(" ===== Runtime ===== ")));
        docs.insert("IrohTransportConfig", t);
    }
}
