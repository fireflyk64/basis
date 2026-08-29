use std::net::IpAddr;

use super::connection_target::{ConnectionTarget, ConnectionTargetKeys, IConnectionTargetParser};

/// `host:port#password` / `[ipv6]:port#password` connection strings.
#[derive(Clone, Copy, Debug, Default)]
pub struct LNLConnectionTargetParser;

/// The parsed pieces of a connection string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedConnectionString {
    pub address: String,
    pub port: u16,
    pub port_provided: bool,
    pub password: String,
}

impl LNLConnectionTargetParser {
    pub const DEFAULT_PORT: u16 = 4296;

    /// Returns `Some` when an address was found; the C# `bool` plus its `out` values.
    pub fn try_parse_connection_string(raw: &str) -> Option<ParsedConnectionString> {
        let parsed = Self::parse_connection_string(raw);
        if parsed.address.is_empty() { None } else { Some(parsed) }
    }

    /// Always returns the partial result, exactly as the C# left its `out` parameters.
    pub fn parse_connection_string(raw: &str) -> ParsedConnectionString {
        let mut out = ParsedConnectionString { port: Self::DEFAULT_PORT, ..Default::default() };
        if raw.is_empty() {
            return out;
        }

        let mut left = raw;
        if let Some(hash_idx) = raw.find('#') {
            out.password = raw[hash_idx + 1..].to_string();
            left = &raw[..hash_idx];
        }

        if left.starts_with('[') {
            // Bracketed IPv6 literal: [addr]:port or [addr]
            if let Some(close_bracket) = left.find(']').filter(|i| *i > 0) {
                out.address = left[1..close_bracket].trim().to_string();
                let after_bracket = &left[close_bracket + 1..];
                if after_bracket.len() > 1
                    && after_bracket.starts_with(':')
                    && let Ok(parsed_port) = after_bracket[1..].parse::<u16>()
                    && parsed_port > 0
                {
                    out.port = parsed_port;
                    out.port_provided = true;
                }
            } else {
                out.address = left.trim().to_string(); // malformed bracket — treat whole string as address
            }
        } else if let Some(colon_idx) = left.rfind(':') {
            let port_part = &left[colon_idx + 1..];
            let parsed_port = port_part.parse::<u16>().ok().filter(|p| *p > 0);
            match parsed_port {
                Some(parsed_port) if colon_idx > 0 && colon_idx < left.len() - 1 => {
                    let candidate_address = left[..colon_idx].trim();
                    // A candidate address that still contains a colon is a bare IPv6 literal
                    // being misread as host:port. Leave it unsplit.
                    if !candidate_address.contains(':') {
                        out.address = candidate_address.to_string();
                        out.port = parsed_port;
                        out.port_provided = true;
                    } else {
                        out.address = left.trim().to_string();
                    }
                }
                _ => out.address = left.trim().to_string(),
            }
        } else {
            out.address = left.trim().to_string();
        }
        out
    }
}

impl IConnectionTargetParser for LNLConnectionTargetParser {
    fn parse(&self, target: &mut ConnectionTarget) {
        if let Some(p) = Self::try_parse_connection_string(&target.raw) {
            target.set(ConnectionTargetKeys::ADDRESS, &p.address);
            target.set(ConnectionTargetKeys::PORT, &p.port.to_string());
            target.set(ConnectionTargetKeys::PASSWORD, &p.password);
        }
    }

    fn format(&self, target: &ConnectionTarget) -> String {
        let address = target.get(ConnectionTargetKeys::ADDRESS).unwrap_or_default();
        let port_string = target.get(ConnectionTargetKeys::PORT).unwrap_or_else(|| Self::DEFAULT_PORT.to_string());
        let password = target.get(ConnectionTargetKeys::PASSWORD).unwrap_or_default();
        let is_ipv6 = matches!(address.parse::<IpAddr>(), Ok(IpAddr::V6(_)));
        let mut s = if is_ipv6 { format!("[{address}]:{port_string}") } else { format!("{address}:{port_string}") };
        if !password.is_empty() {
            s.push('#');
            s.push_str(&password);
        }
        s
    }
}
