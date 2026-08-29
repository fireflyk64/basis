//! What the host's UDP stack will actually do for us, probed once at start-up.
//!
//! Two host facts decide a large part of this server's network cost, and neither is visible
//! from inside the transports that depend on them:
//!
//! * **Generic Segmentation Offload.** QUIC (through `quinn-udp`) puts several packets bound for
//!   the same peer into one `sendmsg` when the kernel offers `UDP_SEGMENT`, and falls back to one
//!   syscall per packet when it does not. Profiling this server under an iroh crowd put ~29 % of
//!   the network worker's time in syscalls, so which of those two the host does is worth knowing
//!   before reading any benchmark taken on it.
//! * **Socket buffer clamps.** iroh's sockets ask the kernel for 7 MiB of receive and send buffer
//!   (`netwatch`'s `SOCKET_BUFFER_SIZE`) and accept whatever they get, logging the refusal at
//!   `debug` level where no operator will see it. A host with the stock `net.core.rmem_max`
//!   grants 208 KiB of it, and the shortfall shows up only as dropped datagrams under load.
//!
//! The probe binds a throwaway UDP socket, asks it the same questions the real sockets ask, and
//! reports the answers. It never touches a live socket, allocates nothing that outlives the
//! call, and is cached: the answers describe the host, which does not change while the process
//! runs. Everything is best-effort — a probe that cannot run reports `Unknown`, never an error,
//! because this is diagnostics and must not be able to stop a server from booting.

use std::sync::OnceLock;

/// Bytes iroh's sockets ask the kernel for, from `netwatch`'s `SOCKET_BUFFER_SIZE` (7 MiB). The
/// probe asks for the same, so what it reports is what the real sockets got.
pub const IROH_REQUESTED_SOCKET_BUFFER: usize = 7 << 20;

/// Segments per `sendmsg` the Linux UDP stack accepts with `UDP_SEGMENT` set. The kernel's own
/// ceiling (`UDP_MAX_SEGMENTS`), and what `quinn-udp` will use when the option is available.
pub const MAX_GSO_SEGMENTS: usize = 64;

/// A host capability that may be supported, unsupported, or not knowable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Yes,
    No,
    Unknown,
}

impl Support {
    fn as_str(self) -> &'static str {
        match self {
            Support::Yes => "yes",
            Support::No => "no",
            Support::Unknown => "unknown",
        }
    }

    fn as_json(self) -> &'static str {
        match self {
            Support::Yes => "true",
            Support::No => "false",
            Support::Unknown => "null",
        }
    }
}

/// What this host's UDP stack offers. Every field is best-effort.
#[derive(Debug, Clone, Copy)]
pub struct HostUdpCapabilities {
    /// `UDP_SEGMENT` accepted: QUIC can carry several packets per syscall.
    pub gso: Support,
    /// `UDP_GRO` accepted: several received packets can arrive in one `recvmsg`.
    pub gro: Support,
    /// Receive buffer the kernel granted when asked for [`IROH_REQUESTED_SOCKET_BUFFER`].
    pub granted_receive_buffer: Option<usize>,
    /// Send buffer the kernel granted for the same request.
    pub granted_send_buffer: Option<usize>,
    /// `net.core.rmem_max`, the ceiling the grant is clamped to.
    pub rmem_max: Option<usize>,
    /// `net.core.wmem_max`.
    pub wmem_max: Option<usize>,
}

impl HostUdpCapabilities {
    /// The probe, run once per process. Later calls return the first result.
    pub fn get() -> &'static HostUdpCapabilities {
        static CACHED: OnceLock<HostUdpCapabilities> = OnceLock::new();
        CACHED.get_or_init(Self::probe)
    }

    fn unknown() -> Self {
        Self {
            gso: Support::Unknown,
            gro: Support::Unknown,
            granted_receive_buffer: None,
            granted_send_buffer: None,
            rmem_max: None,
            wmem_max: None,
        }
    }

    /// True when the kernel granted materially less buffer than iroh asked for. "Materially" is
    /// half, because Linux doubles what it reports for its own bookkeeping and a host that gave
    /// us most of the request is not the problem this warns about.
    pub fn socket_buffers_were_clamped(&self) -> bool {
        let short = |granted: Option<usize>| granted.is_some_and(|got| got < IROH_REQUESTED_SOCKET_BUFFER / 2);
        short(self.granted_receive_buffer) || short(self.granted_send_buffer)
    }

    /// One line for the boot log.
    pub fn report(&self) -> String {
        let kb = |v: Option<usize>| v.map(|b| format!("{} KB", b / 1024)).unwrap_or_else(|| "unknown".to_string());
        format!(
            "UDP offload: GSO {} (up to {} packets per sendmsg), GRO {}. Socket buffers: asked {} MB, granted {} receive / {} send (net.core.rmem_max {}, wmem_max {}).",
            self.gso.as_str(),
            MAX_GSO_SEGMENTS,
            self.gro.as_str(),
            IROH_REQUESTED_SOCKET_BUFFER >> 20,
            kb(self.granted_receive_buffer),
            kb(self.granted_send_buffer),
            kb(self.rmem_max),
            kb(self.wmem_max),
        )
    }

    /// The same facts as a JSON object, for the health document a remote benchmark reads.
    pub fn json(&self) -> String {
        let num = |v: Option<usize>| v.map(|b| b.to_string()).unwrap_or_else(|| "null".to_string());
        format!(
            "{{\"gso\":{},\"gsoMaxSegments\":{},\"gro\":{},\"requestedSocketBufferBytes\":{},\"grantedReceiveBufferBytes\":{},\"grantedSendBufferBytes\":{},\"rmemMax\":{},\"wmemMax\":{}}}",
            self.gso.as_json(),
            MAX_GSO_SEGMENTS,
            self.gro.as_json(),
            IROH_REQUESTED_SOCKET_BUFFER,
            num(self.granted_receive_buffer),
            num(self.granted_send_buffer),
            num(self.rmem_max),
            num(self.wmem_max),
        )
    }

    #[cfg(not(target_os = "linux"))]
    fn probe() -> Self {
        // GSO/GRO are Linux socket options; other platforms answer through different mechanisms
        // that quinn-udp handles itself, and reporting a guess would be worse than saying so.
        Self::unknown()
    }

    #[cfg(target_os = "linux")]
    fn probe() -> Self {
        let Ok(socket) = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP)) else {
            return Self::unknown();
        };
        let mut caps = Self::unknown();

        // GSO is answered by doing it, not by asking. Sandboxed kernels (gVisor and friends)
        // accept `UDP_SEGMENT` and then quietly deliver one large datagram instead of several
        // small ones, so a probe that only checks `setsockopt` reports offload that the host will
        // never perform — and every syscall figure measured on it would be read wrongly.
        caps.gso = probe_gso();
        // GRO has no such cheap end-to-end test (it depends on what the sender coalesces), so the
        // option check is what it gets, and it is reported as the weaker fact it is.
        caps.gro = set_udp_option(&socket, UDP_GRO, 1);

        // Ask for what iroh's sockets ask for, then read back what we actually hold. Linux reports
        // roughly double what it granted (it counts its own overhead), and that is left as the
        // kernel states it rather than halved here, so the number matches `ss -m` and every other
        // tool an operator will reach for.
        if socket.set_recv_buffer_size(IROH_REQUESTED_SOCKET_BUFFER).is_ok() {
            caps.granted_receive_buffer = socket.recv_buffer_size().ok();
        }
        if socket.set_send_buffer_size(IROH_REQUESTED_SOCKET_BUFFER).is_ok() {
            caps.granted_send_buffer = socket.send_buffer_size().ok();
        }
        caps.rmem_max = read_sysctl("/proc/sys/net/core/rmem_max");
        caps.wmem_max = read_sysctl("/proc/sys/net/core/wmem_max");
        caps
    }
}

/// Sends one 2·N byte datagram with `UDP_SEGMENT = N` over loopback and looks at what comes out
/// the other side: two datagrams of N means the host really segments, one of 2N means it took the
/// option and ignored it. Everything is bounded by a 250 ms receive timeout so a host that
/// answers neither way cannot delay a boot.
#[cfg(target_os = "linux")]
fn probe_gso() -> Support {
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

    const SEGMENT: usize = 1200;
    let Ok(receiver) = UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))) else {
        return Support::Unknown;
    };
    let Ok(target) = receiver.local_addr() else { return Support::Unknown };
    if receiver.set_read_timeout(Some(std::time::Duration::from_millis(250))).is_err() {
        return Support::Unknown;
    }
    // The receive buffer must be able to hold both segments, or a full buffer would look like a
    // host that did not segment.
    let Ok(sender) = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP)) else {
        return Support::Unknown;
    };
    if set_udp_option(&sender, libc::UDP_SEGMENT, SEGMENT as libc::c_int) != Support::Yes {
        return Support::No;
    }
    let payload = vec![0x47u8; SEGMENT * 2];
    if sender.send_to(&payload, &socket2::SockAddr::from(target)).is_err() {
        // The kernel took the option and then refused the segmented send: no offload here.
        return Support::No;
    }
    let mut buffer = vec![0u8; SEGMENT * 4];
    match receiver.recv(&mut buffer) {
        Ok(len) if len == SEGMENT => Support::Yes,       // segmented: first of two
        Ok(len) if len == SEGMENT * 2 => Support::No,    // delivered whole: the option was ignored
        Ok(_) => Support::Unknown,
        Err(_) => Support::Unknown,
    }
}

/// `UDP_GRO`, which `libc` does not name.
#[cfg(target_os = "linux")]
const UDP_GRO: libc::c_int = 104;

#[cfg(target_os = "linux")]
fn set_udp_option(socket: &socket2::Socket, option: libc::c_int, value: libc::c_int) -> Support {
    use std::os::fd::AsRawFd;
    // SAFETY: `fd` is owned by `socket` and outlives the call; `value` is a live `c_int` and the
    // length passed matches its size, which is what setsockopt requires for these options.
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_UDP,
            option,
            std::ptr::addr_of!(value).cast(),
            std::mem::size_of_val(&value) as libc::socklen_t,
        )
    };
    if result == 0 { Support::Yes } else { Support::No }
}

#[cfg(target_os = "linux")]
fn read_sysctl(path: &str) -> Option<usize> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_answers_and_is_cached() {
        let first = HostUdpCapabilities::get();
        let second = HostUdpCapabilities::get();
        assert!(std::ptr::eq(first, second), "the probe must run once and be reused");
    }

    #[test]
    fn the_report_names_every_field_it_knows() {
        let report = HostUdpCapabilities::get().report();
        assert!(report.contains("GSO"), "{report}");
        assert!(report.contains("GRO"), "{report}");
        assert!(report.contains("Socket buffers"), "{report}");
    }

    #[test]
    fn the_json_is_parseable_and_complete() {
        let json = HostUdpCapabilities::get().json();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        for field in ["gso", "gsoMaxSegments", "gro", "requestedSocketBufferBytes", "grantedReceiveBufferBytes", "grantedSendBufferBytes", "rmemMax", "wmemMax"] {
            assert!(parsed.get(field).is_some(), "missing {field} in {json}");
        }
    }

    #[test]
    fn a_clamp_is_only_reported_when_the_grant_is_genuinely_short() {
        let mut caps = HostUdpCapabilities::unknown();
        assert!(!caps.socket_buffers_were_clamped(), "unknown grants are not a clamp");
        caps.granted_receive_buffer = Some(IROH_REQUESTED_SOCKET_BUFFER);
        caps.granted_send_buffer = Some(IROH_REQUESTED_SOCKET_BUFFER);
        assert!(!caps.socket_buffers_were_clamped());
        caps.granted_receive_buffer = Some(208 * 1024);
        assert!(caps.socket_buffers_were_clamped(), "a stock rmem_max grant is a clamp");
    }
}
