use std::sync::atomic::{AtomicU16, Ordering};

pub struct BasisNetworkVersion;

// 52: restricted-DOF bone encoding — 2-DOF limb/extremity joints and 1-DOF toes ship
// quantized angles instead of smallest-three quaternions (wire-format change).
// 53: hybrid avatar-bundle codec and developer CompactMerged framing. Byte 0 of the
// channel-52 bundle header was a message count that every decoder documented as a hint
// and none read; it now carries the codec id and dictionary generation.
// 54: CompactMerged mixed framing adds raw Ack/Channeled entries (wire-format change).
static SERVER_VERSION: AtomicU16 = AtomicU16::new(54);

impl BasisNetworkVersion {
    /// The protocol version this build speaks. A mutable static in C#; tests that pin a
    /// mismatch use [`BasisNetworkVersion::set_server_version`].
    pub fn server_version() -> u16 {
        SERVER_VERSION.load(Ordering::Relaxed)
    }

    pub fn set_server_version(version: u16) {
        SERVER_VERSION.store(version, Ordering::Relaxed);
    }
}
