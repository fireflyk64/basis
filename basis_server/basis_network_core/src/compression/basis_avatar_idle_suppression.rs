/// Decides whether a freshly-encoded local avatar frame must be transmitted or can be suppressed
/// as redundant. Two byte-identical payloads reconstruct to the exact same pose on every
/// receiver, so re-sending one is pure wire redundancy; a heartbeat still forces a periodic
/// resend so a late joiner converges to the correct rest pose.
pub struct BasisAvatarIdleSuppression;

impl BasisAvatarIdleSuppression {
    /// Resend an unchanged pose at least this often (seconds).
    pub const DEFAULT_HEARTBEAT_SECONDS: f64 = 5.0;

    /// True when the frame must be sent. Returns false (suppress) only when the new payload is
    /// byte-identical to the last one actually sent, there is no additional (blendshape) data
    /// riding along this frame, the linked-avatar index is unchanged, and the heartbeat window
    /// has not elapsed.
    #[allow(clippy::too_many_arguments)]
    pub fn should_send(
        current: &[u8],
        last_sent: &[u8],
        has_last_sent: bool,
        has_additional_data: bool,
        linked_avatar_changed: bool,
        now_seconds: f64,
        last_sent_seconds: f64,
        heartbeat_seconds: f64,
    ) -> bool {
        if !has_last_sent {
            return true;
        }
        if has_additional_data {
            return true;
        }
        if linked_avatar_changed {
            return true;
        }
        if now_seconds - last_sent_seconds >= heartbeat_seconds {
            return true;
        }
        if current.len() != last_sent.len() {
            return true;
        }
        current != last_sent
    }
}
