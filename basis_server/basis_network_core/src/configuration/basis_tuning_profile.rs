use std::io::Cursor;
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};

use crate::BNL;

use super::basis_server_configuration::Configuration;
use super::basis_transport_config_store::BasisTransportConfigStore;
use super::{BasisXmlConfig, FieldKind};

/// One setting the benchmark fitted, and why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisTunedSetting {
    /// Field name on `Configuration` or on the transport config.
    pub name: String,
    /// Value to write, in invariant culture.
    pub value: String,
    /// Empty for a server setting; otherwise the network stack whose sidecar declares it.
    pub stack: String,
    /// How the value was arrived at, carried through so the boot log can say.
    pub evidence: String,
    /// The reasoning, kept in the file because a bare number in a config explains nothing.
    pub rationale: String,
}

/// Settings fitted to this machine by the benchmark, applied once on the next boot and then
/// folded into config.xml so config.xml stays the single source of truth. Refuses to apply on
/// different hardware: the fingerprint is OS family, architecture and core count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasisTuningProfile {
    pub profile_version: i32,
    /// When the benchmark produced this, ISO-8601 UTC.
    pub generated_utc: String,
    pub generated_by: String,
    /// The machine it was measured on. Compared against the booting host.
    pub machine: String,
    pub machine_detail: String,
    pub design_players: i32,
    /// Apply even when the fingerprint does not match the booting machine.
    pub apply_to_any_machine: bool,
    /// Empty until applied; stamped afterwards so a restart does not re-apply it.
    pub applied_utc: String,
    pub settings: Vec<BasisTunedSetting>,
}

impl Default for BasisTuningProfile {
    fn default() -> Self {
        Self {
            profile_version: Self::CURRENT_VERSION,
            generated_utc: String::new(),
            generated_by: String::new(),
            machine: String::new(),
            machine_detail: String::new(),
            design_players: 0,
            apply_to_any_machine: false,
            applied_utc: String::new(),
            settings: Vec::new(),
        }
    }
}

impl BasisTuningProfile {
    /// Bump when the shape changes. A newer file is refused rather than half-read.
    pub const CURRENT_VERSION: i32 = 1;
    /// Looked for in the config folder, beside config.xml.
    pub const FILE_NAME: &'static str = "tuning-profile.xml";

    /// OS family, architecture and core count — the properties the settings depend on.
    pub fn fingerprint() -> String {
        let os = match std::env::consts::OS {
            "linux" => "linux",
            "windows" => "windows",
            "macos" => "macos",
            _ => "other",
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "x86" => "x86",
            "aarch64" => "arm64",
            "arm" => "arm",
            other => other,
        };
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        format!("{os}-{arch}-{cores}c")
    }

    pub fn resolve_path(config_dir: &Path) -> PathBuf {
        config_dir.join(Self::FILE_NAME)
    }

    pub fn try_load(path: &Path) -> Option<BasisTuningProfile> {
        if !path.exists() {
            return None;
        }
        match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|xml| Self::from_xml(&xml)) {
            Ok(p) => Some(p),
            Err(e) => {
                BNL::log_warning(format!("[Tuning] Could not read '{}': {e}", path.display()));
                None
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let temp = PathBuf::from(format!("{}.tmp", path.display()));
        std::fs::write(&temp, self.to_xml())?;
        std::fs::rename(&temp, path)
    }

    pub fn to_xml(&self) -> String {
        let mut w = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
        w.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None))).unwrap();
        let mut root = BytesStart::new("BasisTuningProfile");
        root.push_attribute(("xmlns:xsi", "http://www.w3.org/2001/XMLSchema-instance"));
        root.push_attribute(("xmlns:xsd", "http://www.w3.org/2001/XMLSchema"));
        w.write_event(Event::Start(root)).unwrap();
        let mut elem = |name: &str, text: &str| {
            w.write_event(Event::Start(BytesStart::new(name))).unwrap();
            w.write_event(Event::Text(BytesText::new(text))).unwrap();
            w.write_event(Event::End(BytesEnd::new(name))).unwrap();
        };
        elem("ProfileVersion", &self.profile_version.to_string());
        elem("GeneratedUtc", &self.generated_utc);
        elem("GeneratedBy", &self.generated_by);
        elem("Machine", &self.machine);
        elem("MachineDetail", &self.machine_detail);
        elem("DesignPlayers", &self.design_players.to_string());
        elem("ApplyToAnyMachine", if self.apply_to_any_machine { "true" } else { "false" });
        elem("AppliedUtc", &self.applied_utc);
        w.write_event(Event::Start(BytesStart::new("Settings"))).unwrap();
        for s in &self.settings {
            let mut e = BytesStart::new("Setting");
            e.push_attribute(("Name", s.name.as_str()));
            e.push_attribute(("Value", s.value.as_str()));
            e.push_attribute(("Stack", s.stack.as_str()));
            e.push_attribute(("Evidence", s.evidence.as_str()));
            w.write_event(Event::Start(e)).unwrap();
            w.write_event(Event::Start(BytesStart::new("Rationale"))).unwrap();
            w.write_event(Event::Text(BytesText::new(&s.rationale))).unwrap();
            w.write_event(Event::End(BytesEnd::new("Rationale"))).unwrap();
            w.write_event(Event::End(BytesEnd::new("Setting"))).unwrap();
        }
        w.write_event(Event::End(BytesEnd::new("Settings"))).unwrap();
        w.write_event(Event::End(BytesEnd::new("BasisTuningProfile"))).unwrap();
        String::from_utf8(w.into_inner().into_inner()).unwrap()
    }

    pub fn from_xml(xml: &str) -> Result<BasisTuningProfile, String> {
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut profile = BasisTuningProfile::default();
        let mut in_settings = false;
        let mut current: Option<BasisTunedSetting> = None;
        let attr = |e: &BytesStart, name: &str| -> String {
            e.attributes()
                .flatten()
                .find(|a| a.key.as_ref() == name.as_bytes())
                .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
                .unwrap_or_default()
        };
        loop {
            match reader.read_event_into(&mut buf).map_err(|e| e.to_string())? {
                Event::Start(e) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    match name.as_str() {
                        "BasisTuningProfile" => {}
                        "Settings" => in_settings = true,
                        "Setting" if in_settings => {
                            current = Some(BasisTunedSetting {
                                name: attr(&e, "Name"),
                                value: attr(&e, "Value"),
                                stack: attr(&e, "Stack"),
                                evidence: attr(&e, "Evidence"),
                                rationale: String::new(),
                            });
                        }
                        _ => {
                            let end = e.to_end().into_owned();
                            let text = reader.read_text(end.name()).map_err(|e| e.to_string())?;
                            let text = quick_xml::escape::unescape(&text).map(|c| c.into_owned()).unwrap_or_else(|_| text.to_string());
                            let text = text.trim().to_string();
                            if let Some(s) = current.as_mut() {
                                if name == "Rationale" {
                                    s.rationale = text;
                                }
                                continue;
                            }
                            match name.as_str() {
                                "ProfileVersion" => profile.profile_version = text.parse().map_err(|_| "ProfileVersion is not an integer".to_string())?,
                                "GeneratedUtc" => profile.generated_utc = text,
                                "GeneratedBy" => profile.generated_by = text,
                                "Machine" => profile.machine = text,
                                "MachineDetail" => profile.machine_detail = text,
                                "DesignPlayers" => profile.design_players = text.parse().unwrap_or(0),
                                "ApplyToAnyMachine" => profile.apply_to_any_machine = text.eq_ignore_ascii_case("true"),
                                "AppliedUtc" => profile.applied_utc = text,
                                _ => {}
                            }
                        }
                    }
                }
                Event::Empty(e) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    if name == "Setting" && in_settings {
                        profile.settings.push(BasisTunedSetting {
                            name: attr(&e, "Name"),
                            value: attr(&e, "Value"),
                            stack: attr(&e, "Stack"),
                            evidence: attr(&e, "Evidence"),
                            rationale: String::new(),
                        });
                    }
                }
                Event::End(e) => {
                    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                    match name.as_str() {
                        "Setting" => {
                            if let Some(s) = current.take() {
                                profile.settings.push(s);
                            }
                        }
                        "Settings" => in_settings = false,
                        _ => {}
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        Ok(profile)
    }

    /// The boot hook: finds a profile beside the config, applies it, and folds it into the config
    /// files. Silent and harmless when there is no profile. Returns true when settings were
    /// applied and the config files were rewritten.
    pub fn apply_if_present(config_dir: &Path, config: &mut Configuration) -> bool {
        let path = Self::resolve_path(config_dir);
        let Some(mut profile) = Self::try_load(&path) else {
            return false;
        };

        if profile.profile_version > Self::CURRENT_VERSION {
            BNL::log_warning(format!(
                "[Tuning] '{}' is version {}; this build understands {}. Ignoring it rather than applying part of a file it does not fully understand.",
                Self::FILE_NAME, profile.profile_version, Self::CURRENT_VERSION
            ));
            return false;
        }

        if !profile.applied_utc.is_empty() {
            BNL::log(format!(
                "[Tuning] '{}' was already applied on {}; its settings are in config.xml. Delete the file, or clear its AppliedUtc, to apply it again.",
                Self::FILE_NAME, profile.applied_utc
            ));
            return false;
        }

        let here = Self::fingerprint();
        if !profile.apply_to_any_machine && !profile.machine.eq_ignore_ascii_case(&here) {
            BNL::log_warning(format!(
                "[Tuning] '{}' was measured on '{}' and this host is '{here}'. Every setting in it is derived from the core count and kernel of the machine it was fitted on, so it has NOT been applied. Re-run the benchmark here, or set <ApplyToAnyMachine>true</ApplyToAnyMachine> in the file if the two hosts really are equivalent.",
                Self::FILE_NAME, profile.machine
            ));
            return false;
        }

        if profile.settings.is_empty() {
            BNL::log(format!("[Tuning] '{}' contains no settings - the benchmark found nothing worth changing.", Self::FILE_NAME));
            profile.stamp();
            let _ = profile.save(&path);
            return false;
        }

        BNL::log(format!(
            "[Tuning] Applying '{}' (measured {} on {}{})",
            Self::FILE_NAME,
            profile.generated_utc,
            profile.machine,
            if profile.design_players > 0 { format!(", fitted at {} players", profile.design_players) } else { String::new() }
        ));

        let mut applied = 0;
        for setting in &profile.settings {
            if setting.name.is_empty() {
                continue;
            }
            let result = if setting.stack.is_empty() {
                Self::try_set(config, &setting.name, &setting.value)
            } else {
                match BasisTransportConfigStore::with_object_mut(&setting.stack, |target| Self::try_set_object(target, &setting.name, &setting.value)) {
                    Some(r) => r,
                    None => {
                        BNL::log_warning(format!("[Tuning]   {}: no '{}' transport is registered; skipped.", setting.name, setting.stack));
                        continue;
                    }
                }
            };
            match result {
                Ok(previous) => {
                    let location = if setting.stack.is_empty() { "config.xml".to_string() } else { format!("{}.xml", setting.stack) };
                    BNL::log(format!(
                        "[Tuning]   {}: {previous} -> {}  [{location}{}]",
                        setting.name,
                        setting.value,
                        if setting.evidence.is_empty() { String::new() } else { format!(", {}", setting.evidence) }
                    ));
                    applied += 1;
                }
                Err(failure) => BNL::log_warning(format!("[Tuning]   {}: {failure}", setting.name)),
            }
        }

        if applied == 0 {
            BNL::log_warning("[Tuning] Nothing could be applied; the config files are unchanged.");
            return false;
        }

        // Written into the config files, so from here on config.xml is authoritative again.
        if let Err(e) = config.save_to_xml(&config_dir.join("config.xml")) {
            BNL::log_error(format!(
                "[Tuning] Applied {applied} setting(s) in memory but could not persist them: {e}. They are live for this run and the profile has NOT been stamped, so the next boot retries."
            ));
            return true;
        }

        profile.stamp();
        if let Err(e) = profile.save(&path) {
            BNL::log_warning(format!("[Tuning] Could not stamp '{}': {e}", path.display()));
        }

        BNL::log(format!("[Tuning] {applied} setting(s) written into the config. config.xml is authoritative from here."));
        true
    }

    fn stamp(&mut self) {
        self.applied_utc = Self::now_iso8601();
    }

    /// ISO-8601 UTC with the "O" round-trip shape the C# wrote (`2026-08-29T12:34:56.1234567Z`).
    pub fn now_iso8601() -> String {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let secs = now.as_secs() as i64;
        let ticks = (now.subsec_nanos() / 100) as u64;
        let days = secs.div_euclid(86400);
        let rem = secs.rem_euclid(86400);
        let (y, m, d) = civil_from_days(days);
        format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ticks:07}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
    }

    /// Sets one public field by name, converting from the profile's string form with the same
    /// conversion set the environment overrides accept. Returns the previous value's text.
    pub fn try_set<T: BasisXmlConfig>(target: &mut T, field_name: &str, value: &str) -> Result<String, String> {
        let Some(kind) = T::field_kind(field_name) else {
            return Err("no such setting in this build; skipped.".to_string());
        };
        let previous = target.get_field(field_name).unwrap_or_default();
        Self::check_kind(kind, value)?;
        target.set_field(field_name, value)?;
        Ok(previous)
    }

    fn try_set_object(target: &mut dyn super::BasisTransportConfigObject, field_name: &str, value: &str) -> Result<String, String> {
        let Some(kind) = target.field_kind(field_name) else {
            return Err("no such setting in this build; skipped.".to_string());
        };
        let previous = target.get_field(field_name).unwrap_or_default();
        Self::check_kind(kind, value)?;
        target.set_field(field_name, value)?;
        Ok(previous)
    }

    fn check_kind(kind: FieldKind, value: &str) -> Result<(), String> {
        let ok = match kind {
            FieldKind::Int => value.trim().parse::<i32>().is_ok(),
            FieldKind::UShort => value.trim().parse::<u16>().is_ok(),
            FieldKind::Byte => value.trim().parse::<u8>().is_ok(),
            FieldKind::Long => value.trim().parse::<i64>().is_ok(),
            FieldKind::Float | FieldKind::Double => value.trim().parse::<f64>().is_ok(),
            FieldKind::Bool => matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "false"),
            FieldKind::Str => true,
            FieldKind::RestrictionMode => return Err("type BasisUserRestrictionMode cannot be set from a profile.".to_string()),
        };
        if ok {
            Ok(())
        } else {
            Err(match kind {
                FieldKind::Int => format!("'{value}' is not an integer."),
                FieldKind::UShort => format!("'{value}' is not a ushort."),
                FieldKind::Byte => format!("'{value}' is not a byte."),
                FieldKind::Long => format!("'{value}' is not a long."),
                FieldKind::Float | FieldKind::Double => format!("'{value}' is not a number."),
                FieldKind::Bool => format!("'{value}' is not true or false."),
                _ => unreachable!(),
            })
        }
    }
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
