/// Pure decision logic for BasisP2PManager's direct-link (P2P) health watchdog — the part that
/// decides when a Connected direct link has gone dead / one-way and must fall back to the server
/// relay so voice and avatar don't freeze. Extracted from the client (which is Unity-only and
/// can't be unit-tested headless) so the thresholds and their ordering can be pinned by tests.
///
/// This is the mechanism that makes a direct-connected pair recover after one of them rejoins:
/// the rejoiner's old socket goes silent, no inbound P2P arrives, and the link is demoted back to
/// the server relay. If this logic (or the timer that drives it) is wrong, the pair stays routed
/// over a dead direct link and can't hear each other — the reported symptom.
pub struct BasisP2PLinkHealth;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ConnectedVerdict {
    /// Link looks healthy (or still inside the post-connect settle window); leave it Connected.
    Healthy,
    /// No inbound P2P traffic for too long — the path is dead/one-way; demote to the server relay.
    DemoteStale,
    /// Reached Connected but the server never confirmed the offload — the peer never fully came up; demote.
    DemoteUnconfirmed,
    /// Healthy for a long continuous dwell; clear the connect->die->reconnect flap counter.
    ClearFlapCounter,
}

impl BasisP2PLinkHealth {
    /// Decide what to do with a Connected direct link this tick. Staleness is checked before the
    /// never-confirmed case so a demotion reports the more specific reason.
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_connected(
        connected_age_ms: i64,
        since_inbound_ms: i64,
        offload_confirmed: bool,
        has_flap_counter: bool,
        grace_ms: i64,
        stale_ms: i64,
        confirm_ms: i64,
        healthy_dwell_ms: i64,
    ) -> ConnectedVerdict {
        // Settle window right after connecting — never demote yet (a fresh link hasn't had time
        // to prove liveness or to receive the server's offload confirmation).
        if connected_age_ms < grace_ms {
            return ConnectedVerdict::Healthy;
        }
        if since_inbound_ms > stale_ms {
            return ConnectedVerdict::DemoteStale;
        }
        if !offload_confirmed && connected_age_ms > confirm_ms {
            return ConnectedVerdict::DemoteUnconfirmed;
        }
        if connected_age_ms > healthy_dwell_ms && has_flap_counter {
            return ConnectedVerdict::ClearFlapCounter;
        }
        ConnectedVerdict::Healthy
    }

    /// A punch / re-punch that has been in flight past `punch_timeout_ms` is stuck and should be
    /// retried (re-armed) instead of hanging in Punching/Reconnecting forever.
    pub fn punch_stalled(punch_age_ms: i64, punch_timeout_ms: i64) -> bool {
        punch_age_ms > punch_timeout_ms
    }
}
