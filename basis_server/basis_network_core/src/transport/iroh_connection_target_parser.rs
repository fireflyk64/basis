use super::connection_target::{ConnectionTarget, ConnectionTargetKeys, IConnectionTargetParser};
use super::lnl_connection_target_parser::LNLConnectionTargetParser;

/// iroh connection strings: `<endpoint-id>[@host:port][#password]`, where the endpoint id is the
/// server's z-base-32 public key and the optional `@host:port` is a direct address to try first.
/// A plain `host:port` (no endpoint id) is accepted too and resolved by probing the address for
/// its endpoint id, so the strings people already have keep working.
#[derive(Clone, Copy, Debug, Default)]
pub struct IrohConnectionTargetParser;

impl IrohConnectionTargetParser {
    pub const DEFAULT_PORT: u16 = LNLConnectionTargetParser::DEFAULT_PORT;

    /// True when `s` looks like an iroh endpoint id (52 z-base-32 characters or 64 hex).
    pub fn looks_like_endpoint_id(s: &str) -> bool {
        let s = s.trim();
        (s.len() == 52 && s.bytes().all(|b| b"ybndrfg8ejkmcpqxot1uwisza345h769".contains(&b.to_ascii_lowercase())))
            || (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}

impl IConnectionTargetParser for IrohConnectionTargetParser {
    fn parse(&self, target: &mut ConnectionTarget) {
        let raw = target.raw.clone();
        if raw.is_empty() {
            return;
        }
        let (left, password) = match raw.find('#') {
            Some(i) => (&raw[..i], &raw[i + 1..]),
            None => (raw.as_str(), ""),
        };
        let (id_part, addr_part) = match left.find('@') {
            Some(i) => (&left[..i], &left[i + 1..]),
            None if Self::looks_like_endpoint_id(left) => (left, ""),
            None => ("", left),
        };
        if !id_part.is_empty() {
            target.set(ConnectionTargetKeys::ENDPOINT_ID, id_part.trim());
        }
        if !addr_part.is_empty()
            && let Some(p) = LNLConnectionTargetParser::try_parse_connection_string(addr_part)
        {
            target.set(ConnectionTargetKeys::ADDRESS, &p.address);
            target.set(ConnectionTargetKeys::PORT, &p.port.to_string());
        }
        target.set(ConnectionTargetKeys::PASSWORD, password);
    }

    fn format(&self, target: &ConnectionTarget) -> String {
        let id = target.get(ConnectionTargetKeys::ENDPOINT_ID).unwrap_or_default();
        let address = target.get(ConnectionTargetKeys::ADDRESS).unwrap_or_default();
        let port = target.get(ConnectionTargetKeys::PORT).unwrap_or_else(|| Self::DEFAULT_PORT.to_string());
        let password = target.get(ConnectionTargetKeys::PASSWORD).unwrap_or_default();
        let mut s = String::new();
        if !id.is_empty() {
            s.push_str(&id);
        }
        if !address.is_empty() {
            if !s.is_empty() {
                s.push('@');
            }
            let is_ipv6 = matches!(address.parse::<std::net::IpAddr>(), Ok(std::net::IpAddr::V6(_)));
            s.push_str(&if is_ipv6 { format!("[{address}]:{port}") } else { format!("{address}:{port}") });
        }
        if !password.is_empty() {
            s.push('#');
            s.push_str(&password);
        }
        s
    }
}
