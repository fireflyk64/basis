//! Port of `BasisNetworkCore/Configuration`: the server config, the per-transport sidecars, the
//! doc-comment XML layer they are written through, and the population/tuning helpers.
//!
//! The C# reached every field by reflection (XmlSerializer, environment overrides, the tuning
//! profile). Rust has no reflection, so [`basis_xml_config!`] generates the same field table —
//! name, kind, default, get-by-name, set-by-name — for each config type, and everything that
//! walked fields reflectively walks that table instead.

pub mod basis_config_xml_docs;
pub mod basis_population_scale;
pub mod basis_server_configuration;
pub mod basis_transport_config_store;
pub mod basis_tuning_profile;
pub mod iroh_transport_config;
pub mod lnl_transport_config;

pub use basis_config_xml_docs::{BasisConfigXmlDocs, ConfigXmlError, FieldDoc};
pub use basis_population_scale::BasisPopulationScale;
pub use basis_server_configuration::Configuration;
pub use basis_transport_config_store::{BasisTransportConfigObject, BasisTransportConfigStore};
pub use basis_tuning_profile::{BasisTunedSetting, BasisTuningProfile, TuningError};
pub use iroh_transport_config::IrohTransportConfig;
pub use lnl_transport_config::LNLTransportConfig;

use crate::identity::BasisUserRestrictionMode;

/// The scalar kinds a config field can have — the set the C# reflection code handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Int,
    UShort,
    Byte,
    Long,
    Float,
    Double,
    Bool,
    Str,
    RestrictionMode,
}

/// A field could not be set from its text form: the name is unknown to this build, or the
/// value does not parse as the field's type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigFieldError {
    #[error("'{field}' is not a setting in this build")]
    UnknownField { field: String },
    #[error("'{field}' cannot be set to '{value}': {reason}")]
    BadValue { field: String, value: String, reason: String },
}

impl ConfigFieldError {
    pub fn field(&self) -> &str {
        match self {
            ConfigFieldError::UnknownField { field } | ConfigFieldError::BadValue { field, .. } => field,
        }
    }
}

/// A config type that can be walked field-by-field by XML element name. Implemented by
/// [`basis_xml_config!`].
pub trait BasisXmlConfig: Default + Clone + Send + Sync + 'static {
    /// The XML root element (the C# type name).
    const XML_ROOT: &'static str;
    /// The type's `CurrentConfigVersion`; 0 when it has no version stamp.
    const CURRENT_CONFIG_VERSION: i32;

    /// Every serialized field, in declaration (and therefore XML) order.
    fn field_names() -> &'static [&'static str];
    fn field_kind(name: &str) -> Option<FieldKind>;
    /// The field's value in invariant-culture text, `None` for an unknown name.
    fn get_field(&self, name: &str) -> Option<String>;
    /// Sets a field from its text form. `Err` names the field and the reason (unknown field,
    /// unparseable value).
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), ConfigFieldError>;

    /// Schema version found in a just-deserialised config; 0 when it predates versioning.
    fn config_version(&self) -> i32 {
        self.get_field("ConfigVersion").and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn set_config_version(&mut self, version: i32) {
        let _ = self.set_field("ConfigVersion", &version.to_string());
    }

    /// The C# `IBasisTransportConfigMigration.MigrateFrom` hook: retire values a newer build knows
    /// are harmful, before the missing-settings upgrade re-saves the file. No-op by default.
    fn migrate_from(&mut self, _loaded_version: i32) {}
}

/// Formatting and parsing of one field value, the way `XmlSerializer` / `TryParse` did it.
pub trait ConfigFieldValue: Sized {
    fn format_field(&self) -> String;
    fn parse_field(text: &str) -> Result<Self, String>;
}

macro_rules! numeric_field {
    ($($t:ty),*) => {$(
        impl ConfigFieldValue for $t {
            fn format_field(&self) -> String { self.to_string() }
            fn parse_field(text: &str) -> Result<Self, String> {
                text.trim().parse::<$t>().map_err(|_| format!("'{text}' is not a valid {}", stringify!($t)))
            }
        }
    )*};
}
numeric_field!(i32, u16, u8, i64, f32, f64);

impl ConfigFieldValue for bool {
    fn format_field(&self) -> String {
        if *self { "true".into() } else { "false".into() }
    }
    fn parse_field(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(format!("'{text}' is not true or false")),
        }
    }
}

impl ConfigFieldValue for String {
    fn format_field(&self) -> String {
        self.clone()
    }
    fn parse_field(text: &str) -> Result<Self, String> {
        Ok(text.to_string())
    }
}

impl ConfigFieldValue for BasisUserRestrictionMode {
    fn format_field(&self) -> String {
        self.as_str().to_string()
    }
    fn parse_field(text: &str) -> Result<Self, String> {
        BasisUserRestrictionMode::parse(text).ok_or_else(|| format!("'{text}' is not a BasisUserRestrictionMode"))
    }
}

/// Declares a config struct with public fields and defaults, and derives the field table the
/// XML layer, the environment overrides and the tuning profile all use.
///
/// ```ignore
/// basis_xml_config! {
///     pub struct MyConfig ("MyConfig", 0) {
///         pub some_setting: i32 = 3 => "SomeSetting" [Int],
///     }
/// }
/// ```
#[macro_export]
macro_rules! basis_xml_config {
    (
        $(#[$m:meta])*
        pub struct $name:ident ($root:expr, $ver:expr) {
            $( $(#[$fm:meta])* pub $field:ident : $ty:ty = $default:expr => $xml:literal [$kind:ident], )*
        }
    ) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name {
            $( $(#[$fm])* pub $field: $ty, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        impl $crate::configuration::BasisXmlConfig for $name {
            const XML_ROOT: &'static str = $root;
            const CURRENT_CONFIG_VERSION: i32 = $ver;

            fn field_names() -> &'static [&'static str] {
                &[ $( $xml, )* ]
            }

            fn field_kind(name: &str) -> Option<$crate::configuration::FieldKind> {
                match name {
                    $( $xml => Some($crate::configuration::FieldKind::$kind), )*
                    _ => None,
                }
            }

            fn get_field(&self, name: &str) -> Option<String> {
                use $crate::configuration::ConfigFieldValue as _;
                match name {
                    $( $xml => Some(self.$field.format_field()), )*
                    _ => None,
                }
            }

            fn set_field(&mut self, name: &str, value: &str) -> Result<(), $crate::configuration::ConfigFieldError> {
                use $crate::configuration::ConfigFieldValue as _;
                match name {
                    $( $xml => {
                        self.$field = <$ty>::parse_field(value).map_err(|reason| {
                            $crate::configuration::ConfigFieldError::BadValue {
                                field: name.to_string(),
                                value: value.to_string(),
                                reason,
                            }
                        })?;
                        Ok(())
                    } )*
                    _ => Err($crate::configuration::ConfigFieldError::UnknownField { field: name.to_string() }),
                }
            }

            fn migrate_from(&mut self, loaded_version: i32) {
                $crate::configuration::__migrate_hook(self, loaded_version);
            }
        }
    };
}

/// Types opt into a migration by implementing [`ConfigMigration`]; the macro routes through this
/// so the trait method can stay generated.
pub trait ConfigMigration {
    fn migrate_from(&mut self, loaded_version: i32);
}

#[doc(hidden)]
pub fn __migrate_hook<T: 'static>(config: &mut T, loaded_version: i32) {
    use std::any::Any;
    let any: &mut dyn Any = config;
    if let Some(m) = any.downcast_mut::<LNLTransportConfig>() {
        m.migrate_from(loaded_version);
    } else if let Some(m) = any.downcast_mut::<IrohTransportConfig>() {
        ConfigMigration::migrate_from(m, loaded_version);
    }
}
