//! Port of the pieces of `LiteNetLib/NetUtils.cs` the transport itself needs, plus the .NET
//! clock the wire carries in ping/pong and connect packets.

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use super::net_constants::NetConstants;

/// Distance from `expected` to `number` on the 15-bit sequence ring, in `-16384..16384`.
pub fn relative_sequence_number(number: i32, expected: i32) -> i32 {
    let max = i32::from(NetConstants::MAX_SEQUENCE);
    let half = i32::from(NetConstants::HALF_MAX_SEQUENCE);
    (number - expected + max + half) % max - half
}

/// .NET `DateTime.UtcNow.Ticks`: 100 ns intervals since 0001-01-01. Connect times, ping/pong
/// clocks and reliable resend stamps are all in this unit because the C# peer reads them.
pub fn utc_now_ticks() -> i64 {
    const EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    EPOCH_TICKS.saturating_add(i64::try_from(now.as_nanos() / 100).unwrap_or(i64::MAX))
}

pub const TICKS_PER_MILLISECOND: i64 = 10_000;

/// The bytes .NET's `SocketAddress` serializes an endpoint to: 16 for IPv4, 28 for IPv6. A
/// connect request carries the target's, and the C# peer compares them byte for byte to break
/// a simultaneous-connect tie, so the layout has to be .NET's exactly: a little-endian address
/// family, the port big-endian, then the address.
pub fn socket_address_bytes(addr: SocketAddr) -> Vec<u8> {
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 23;
    match addr {
        SocketAddr::V4(v4) => {
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(&AF_INET.to_le_bytes());
            bytes.extend_from_slice(&v4.port().to_be_bytes());
            bytes.extend_from_slice(&v4.ip().octets());
            bytes.resize(16, 0);
            bytes
        }
        SocketAddr::V6(v6) => {
            let mut bytes = Vec::with_capacity(28);
            bytes.extend_from_slice(&AF_INET6.to_le_bytes());
            bytes.extend_from_slice(&v6.port().to_be_bytes());
            bytes.extend_from_slice(&v6.flowinfo().to_le_bytes());
            bytes.extend_from_slice(&v6.ip().octets());
            bytes.extend_from_slice(&v6.scope_id().to_le_bytes());
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_sequence_wraps_the_ring() {
        assert_eq!(relative_sequence_number(5, 3), 2);
        assert_eq!(relative_sequence_number(3, 5), -2);
        assert_eq!(relative_sequence_number(0, 32767), 1);
        assert_eq!(relative_sequence_number(32767, 0), -1);
        assert_eq!(relative_sequence_number(16384, 0), -16384);
        assert_eq!(relative_sequence_number(16383, 0), 16383);
    }

    #[test]
    fn socket_address_bytes_match_dotnet_layout() {
        let v4 = socket_address_bytes("192.168.1.2:4296".parse().unwrap());
        assert_eq!(v4.len(), 16);
        assert_eq!(&v4[..4], &[2, 0, 0x10, 0xC8]);
        assert_eq!(&v4[4..8], &[192, 168, 1, 2]);
        let v6 = socket_address_bytes("[::1]:1".parse().unwrap());
        assert_eq!(v6.len(), 28);
        assert_eq!(&v6[..4], &[23, 0, 0, 1]);
        assert_eq!(v6[23], 1);
    }

    #[test]
    fn utc_ticks_are_after_the_unix_epoch() {
        assert!(utc_now_ticks() > 621_355_968_000_000_000);
    }
}
