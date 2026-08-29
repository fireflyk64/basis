//! Negative tests for the configuration layer: malformed XML, bad values, unknown fields,
//! unwritable paths and bad tuning profiles all come back as typed errors that name the
//! problem, and never touch the values that were already valid.

use std::path::Path;

use basis_network_core::configuration::{
    BasisConfigXmlDocs, BasisTransportConfigStore, BasisTuningProfile, BasisXmlConfig, ConfigFieldError, ConfigXmlError,
    Configuration, IrohTransportConfig, TuningError,
};
use serial_test::serial;

#[test]
fn malformed_xml_is_reported_as_malformed() {
    let err = BasisConfigXmlDocs::deserialize::<Configuration>("<Configuration><PeerLimit>1").unwrap_err();
    assert!(matches!(err, ConfigXmlError::Malformed(_)), "{err}");
    let err = BasisConfigXmlDocs::deserialize::<Configuration>("<Configuration><a></b></Configuration>").unwrap_err();
    assert!(matches!(err, ConfigXmlError::Malformed(_)), "{err}");
}

#[test]
fn an_empty_document_has_no_root() {
    let err = BasisConfigXmlDocs::deserialize::<Configuration>("").unwrap_err();
    assert!(matches!(err, ConfigXmlError::NoRoot), "{err}");
    let err = BasisConfigXmlDocs::deserialize::<Configuration>("<!-- only a comment -->").unwrap_err();
    assert!(matches!(err, ConfigXmlError::NoRoot), "{err}");
}

#[test]
fn the_wrong_root_element_is_refused() {
    let err = BasisConfigXmlDocs::deserialize::<Configuration>("<Other><PeerLimit>3</PeerLimit></Other>").unwrap_err();
    assert!(matches!(&err, ConfigXmlError::WrongRoot(root) if root == "Other"), "{err}");
    let err = BasisConfigXmlDocs::deserialize::<IrohTransportConfig>("<Configuration/>").unwrap_err();
    assert!(matches!(err, ConfigXmlError::WrongRoot(_)), "{err}");
}

#[test]
fn a_bad_value_names_the_field_and_the_value() {
    let err = BasisConfigXmlDocs::deserialize::<Configuration>("<Configuration><PeerLimit>abc</PeerLimit></Configuration>")
        .unwrap_err();
    match err {
        ConfigXmlError::BadValue(ConfigFieldError::BadValue { field, value, reason }) => {
            assert_eq!(field, "PeerLimit");
            assert_eq!(value, "abc");
            assert!(reason.contains("i32"), "{reason}");
        }
        other => panic!("expected BadValue, got {other}"),
    }
    let err = BasisConfigXmlDocs::deserialize::<Configuration>("<Configuration><SetPort>70000</SetPort></Configuration>")
        .unwrap_err();
    assert!(matches!(err, ConfigXmlError::BadValue(ConfigFieldError::BadValue { .. })), "{err}");
    let err = BasisConfigXmlDocs::deserialize::<Configuration>(
        "<Configuration><EnableConsole>maybe</EnableConsole></Configuration>",
    )
    .unwrap_err();
    assert!(err.to_string().contains("EnableConsole"), "{err}");
}

#[test]
fn unknown_elements_are_ignored_and_missing_ones_keep_defaults() {
    let config: Configuration = BasisConfigXmlDocs::deserialize(
        "<Configuration><NotASetting>1</NotASetting><PeerLimit>7</PeerLimit><Nested><Deep>x</Deep></Nested></Configuration>",
    )
    .unwrap();
    assert_eq!(config.peer_limit, 7);
    assert_eq!(config.set_port, Configuration::default().set_port);
}

#[test]
fn set_field_refuses_unknown_names_and_bad_values_without_changing_state() {
    let mut config = Configuration::default();
    let before = config.clone();
    assert_eq!(config.set_field("NoSuchField", "1"), Err(ConfigFieldError::UnknownField { field: "NoSuchField".into() }));
    let err = config.set_field("PeerLimit", "not a number").unwrap_err();
    assert_eq!(err.field(), "PeerLimit");
    assert!(matches!(err, ConfigFieldError::BadValue { .. }));
    assert_eq!(config, before);
    assert!(config.set_field("PeerLimit", "12").is_ok());
    assert_eq!(config.peer_limit, 12);
    assert_eq!(config.get_field("NoSuchField"), None);
}

#[test]
fn serialize_then_deserialize_round_trips_every_field() {
    let mut config = Configuration {
        peer_limit: 99,
        server_name: "a <name> & \"quotes\"".to_string(),
        ..Configuration::default()
    };
    let xml = BasisConfigXmlDocs::serialize(&config).unwrap();
    let back: Configuration = BasisConfigXmlDocs::deserialize(&xml).unwrap();
    assert_eq!(back, config);
    for name in Configuration::field_names() {
        assert!(xml.contains(&format!("<{name}>")) || xml.contains(&format!("<{name}/>")), "{name} missing from {xml}");
    }
}

#[test]
#[serial]
fn loading_from_a_missing_file_creates_it_and_an_unwritable_path_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.xml");
    let created = Configuration::load_from_xml(&path).unwrap();
    assert!(path.exists());
    assert_eq!(created.peer_limit, Configuration::default().peer_limit);
    let loaded = Configuration::load_from_xml(&path).unwrap();
    assert_eq!(loaded, created);

    // A regular file where the directory should be: neither create nor save can succeed.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let bad_path = blocker.join("config.xml");
    let err = Configuration::load_from_xml(&bad_path).unwrap_err();
    assert!(matches!(err, ConfigXmlError::Io(_)), "{err}");
    let mut config = Configuration::default();
    let err = config.save_to_xml(&bad_path).unwrap_err();
    assert!(matches!(err, ConfigXmlError::Io(_)), "{err}");
}

#[test]
#[serial]
fn a_corrupt_config_file_is_reported_not_silently_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.xml");
    std::fs::write(&path, "<Configuration><PeerLimit>").unwrap();
    let err = Configuration::load_from_xml(&path).unwrap_err();
    assert!(matches!(err, ConfigXmlError::Malformed(_)), "{err}");
    // The operator's file is left for them to fix.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "<Configuration><PeerLimit>");
}

#[test]
fn tuning_profiles_reject_bad_xml_and_bad_settings() {
    let err = BasisTuningProfile::from_xml("<BasisTuningProfile><Settings>").unwrap_err();
    assert!(matches!(err, ConfigXmlError::Malformed(_)), "{err}");
    let err = BasisTuningProfile::from_xml("<BasisTuningProfile><ProfileVersion>x</ProfileVersion></BasisTuningProfile>")
        .unwrap_err();
    assert!(matches!(err, ConfigXmlError::BadValue(ConfigFieldError::BadValue { .. })), "{err}");

    let profile = BasisTuningProfile::from_xml(
        r#"<BasisTuningProfile><ProfileVersion>1</ProfileVersion><Settings><Setting Name="PeerLimit" Value="5" Stack="" Evidence="measured"><Rationale>why</Rationale></Setting></Settings></BasisTuningProfile>"#,
    )
    .unwrap();
    assert_eq!(profile.settings.len(), 1);
    assert_eq!(profile.settings[0].name, "PeerLimit");
    assert_eq!(profile.settings[0].rationale, "why");

    let mut config = Configuration::default();
    assert_eq!(BasisTuningProfile::try_set(&mut config, "NoSuchSetting", "1"), Err(TuningError::UnknownSetting));
    assert!(matches!(BasisTuningProfile::try_set(&mut config, "PeerLimit", "lots"), Err(TuningError::WrongType(_))));
    assert_eq!(
        BasisTuningProfile::try_set(&mut config, "BasisUserRestrictionMode", "BanList"),
        Err(TuningError::NotFromProfile)
    );
    let previous = BasisTuningProfile::try_set(&mut config, "PeerLimit", "5").unwrap();
    assert_eq!(previous, Configuration::default().peer_limit.to_string());
    assert_eq!(config.peer_limit, 5);
}

#[test]
#[serial]
fn a_bad_environment_override_is_ignored_and_a_good_one_applied() {
    let mut config = Configuration::default();
    // SAFETY: tests in this binary that touch the environment are serialised with #[serial].
    unsafe {
        std::env::set_var("PeerLimit", "not a number");
        std::env::set_var("ServerMotd", "from the environment");
    }
    config.process_environmental_overrides();
    unsafe {
        std::env::remove_var("PeerLimit");
        std::env::remove_var("ServerMotd");
    }
    assert_eq!(config.peer_limit, Configuration::default().peer_limit);
    assert_eq!(config.server_motd, "from the environment");
}

#[test]
#[serial]
fn the_transport_store_answers_none_for_an_unknown_stack() {
    assert!(BasisTransportConfigStore::with_object_mut("no-such-stack", |_| ()).is_none());
    assert!(BasisTransportConfigStore::get_object("no-such-stack").is_none());
    // A known type is always available with defaults, even before any file was loaded.
    let iroh: IrohTransportConfig = BasisTransportConfigStore::get("iroh");
    // Loading (or creating) stamps the current version, as the C# LoadOrCreate did.
    assert_eq!(iroh, IrohTransportConfig { config_version: IrohTransportConfig::CURRENT_CONFIG_VERSION, ..IrohTransportConfig::default() });
    let _ = Path::new("unused");
}
