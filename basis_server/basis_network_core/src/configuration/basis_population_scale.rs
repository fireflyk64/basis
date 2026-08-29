use std::sync::atomic::{AtomicI64, Ordering};

/// Resolves the settings whose correct value is a function of how many players are connected
/// and how much memory the box has. Every value here is a CEILING, not a target, expressed as a
/// share of what the machine actually has, never as a fixed count.
pub struct BasisPopulationScale;

static AVAILABLE_MEMORY_BYTES: AtomicI64 = AtomicI64::new(0);

impl BasisPopulationScale {
    /// Share of the machine's memory the unreliable send queues may occupy at their bound.
    pub const UNRELIABLE_QUEUE_MEMORY_SHARE: f64 = 0.10;
    /// Share of memory the VOICE send queues may occupy at their bound.
    pub const PRIORITY_QUEUE_MEMORY_SHARE: f64 = 0.10;
    /// Share of memory the packet pool may retain — a ceiling on the SAME packets the queues hold.
    pub const PACKET_POOL_MEMORY_SHARE: f64 = Self::UNRELIABLE_QUEUE_MEMORY_SHARE + Self::PRIORITY_QUEUE_MEMORY_SHARE + 0.02;
    /// Floor for the per-peer unreliable bound.
    pub const MIN_UNRELIABLE_QUEUE_PER_PEER: i32 = 512;
    /// Ceiling for the per-peer unreliable bound.
    pub const MAX_UNRELIABLE_QUEUE_PER_PEER: i32 = 8192;
    /// Assumed bytes per queued packet, for turning a memory share into a packet count.
    const APPROX_PACKET_BYTES: i64 = 1432;
    /// Floor for the per-peer VOICE bound.
    pub const MIN_PRIORITY_QUEUE_PER_PEER: i32 = 1024;
    /// Ceiling for the per-peer voice bound.
    pub const MAX_PRIORITY_QUEUE_PER_PEER: i32 = 8192;
    /// Share of memory the RELIABLE send queues may occupy at their bound — the bytes queued
    /// for peers that are not reading. This is the bound that turns a stalled or hostile
    /// client from a memory leak into a disconnect.
    pub const RELIABLE_QUEUE_MEMORY_SHARE: f64 = 0.10;
    /// Floor for the per-peer reliable byte budget: enough for a join snapshot plus a burst of
    /// avatar changes and chat to a slow client.
    pub const MIN_RELIABLE_QUEUE_BYTES_PER_PEER: i32 = 256 * 1024;
    /// Ceiling for the per-peer reliable byte budget.
    pub const MAX_RELIABLE_QUEUE_BYTES_PER_PEER: i32 = 8 * 1024 * 1024;

    /// Memory the process believes it can use — the container limit when there is one, physical
    /// RAM otherwise. Read once and cached.
    pub fn available_memory_bytes() -> i64 {
        let cached = AVAILABLE_MEMORY_BYTES.load(Ordering::Acquire);
        if cached > 0 {
            return cached;
        }
        let mut total = Self::cgroup_memory_limit().unwrap_or(0);
        if total <= 0 {
            let mut sys = sysinfo::System::new();
            sys.refresh_memory();
            total = sys.total_memory() as i64;
        }
        // A runtime that will not answer must not be read as "this box has no memory".
        if total <= 0 {
            total = 4 * 1024 * 1024 * 1024;
        }
        AVAILABLE_MEMORY_BYTES.store(total, Ordering::Release);
        total
    }

    fn cgroup_memory_limit() -> Option<i64> {
        for path in ["/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes"] {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(v) = text.trim().parse::<i64>()
                && v > 0
                && v < (1i64 << 50)
            {
                return Some(v);
            }
        }
        None
    }

    /// Test seam: pin the memory figure so the resolvers can be exercised at any box size.
    pub fn override_available_memory_for_tests(bytes: i64) {
        AVAILABLE_MEMORY_BYTES.store(bytes, Ordering::Release);
    }

    fn clamp(value: i64, min: i32, max: i32) -> i32 {
        if value < i64::from(min) { min } else if value > i64::from(max) { max } else { value as i32 }
    }

    /// Per-peer unreliable queue depth for `peers` connected peers. `configured` > 0 wins.
    pub fn unreliable_queue_per_peer(configured: i32, peers: i32) -> i32 {
        if configured > 0 {
            return configured;
        }
        let peers = peers.max(1) as i64;
        let budget_packets = (Self::available_memory_bytes() as f64 * Self::UNRELIABLE_QUEUE_MEMORY_SHARE) as i64 / Self::APPROX_PACKET_BYTES;
        Self::clamp(budget_packets / peers, Self::MIN_UNRELIABLE_QUEUE_PER_PEER, Self::MAX_UNRELIABLE_QUEUE_PER_PEER)
    }

    /// Per-peer voice queue depth for `peers` connected peers. Sized as a fan-in.
    pub fn priority_queue_per_peer(configured: i32, peers: i32) -> i32 {
        if configured > 0 {
            return configured;
        }
        let peers = peers.max(1) as i64;
        let budget_packets = (Self::available_memory_bytes() as f64 * Self::PRIORITY_QUEUE_MEMORY_SHARE) as i64 / Self::APPROX_PACKET_BYTES;
        Self::clamp(budget_packets / peers, Self::MIN_PRIORITY_QUEUE_PER_PEER, Self::MAX_PRIORITY_QUEUE_PER_PEER)
    }

    /// Per-peer budget, in bytes, for reliable messages queued but not yet delivered — sends
    /// past it are refused and a peer that stays past it is disconnected. `configured` > 0 wins.
    pub fn reliable_queue_bytes_per_peer(configured: i32, peers: i32) -> i32 {
        if configured > 0 {
            return configured;
        }
        let peers = peers.max(1) as i64;
        let budget_bytes = (Self::available_memory_bytes() as f64 * Self::RELIABLE_QUEUE_MEMORY_SHARE) as i64;
        Self::clamp(budget_bytes / peers, Self::MIN_RELIABLE_QUEUE_BYTES_PER_PEER, Self::MAX_RELIABLE_QUEUE_BYTES_PER_PEER)
    }

    /// Ceiling on the scaled packet pool: sized to take back everything the queues can let go of.
    pub fn packet_pool_max(configured: i32, peers: i32, per_peer: i32) -> i32 {
        if configured > 0 {
            return configured;
        }
        let peers = peers.max(1) as i64;
        let per_peer = per_peer.max(1) as i64;
        // Twice the per-peer demand: the pool has to hold what is in flight plus what is coming back.
        let mut want = peers * per_peer * 2;
        // Plus everything the send queues are allowed to hold.
        want += peers
            * (i64::from(Self::unreliable_queue_per_peer(0, peers as i32)) + i64::from(Self::priority_queue_per_peer(0, peers as i32)));
        let memory_cap = (Self::available_memory_bytes() as f64 * Self::PACKET_POOL_MEMORY_SHARE) as i64 / Self::APPROX_PACKET_BYTES;
        if want > memory_cap {
            want = memory_cap;
        }
        Self::clamp(want, 65536, i32::MAX)
    }

    /// Upper bound on reduction-system slicing: ~64 receivers per slice.
    pub fn slice_cap(configured: i32, players: i32) -> i32 {
        if configured > 0 {
            return configured;
        }
        let players = players.max(1) as i64;
        Self::clamp(players / 64, 32, 256)
    }

    /// One line for the boot log, so the resolved ceilings are visible without a debugger.
    pub fn describe(peers: i32, pool_per_peer: i32) -> String {
        format!(
            "[POP] {peers} peers, {} MB available: bulk queue {}/peer, voice queue {}/peer, packet pool max {}, slice cap {}",
            Self::available_memory_bytes() / (1024 * 1024),
            Self::unreliable_queue_per_peer(0, peers),
            Self::priority_queue_per_peer(0, peers),
            Self::packet_pool_max(0, peers, pool_per_peer),
            Self::slice_cap(0, peers)
        )
    }
}

#[cfg(test)]
mod reliable_budget_tests {
    use super::BasisPopulationScale as P;
    use serial_test::serial;

    #[test]
    #[serial(population_scale)]
    fn reliable_budget_is_a_memory_share_divided_by_population_within_floor_and_ceiling() {
        // 8 GiB box: 10% = ~819 MiB for reliable queues across all peers.
        P::override_available_memory_for_tests(8 * 1024 * 1024 * 1024);
        // A configured value wins outright.
        assert_eq!(P::reliable_queue_bytes_per_peer(1_000_000, 500), 1_000_000);
        // At a handful of peers the per-peer share is huge, so the ceiling clamps it.
        assert_eq!(P::reliable_queue_bytes_per_peer(0, 1), P::MAX_RELIABLE_QUEUE_BYTES_PER_PEER);
        // At a large population the share shrinks toward the floor and never below it.
        assert_eq!(P::reliable_queue_bytes_per_peer(0, 100_000), P::MIN_RELIABLE_QUEUE_BYTES_PER_PEER);
        // In between it tracks the division: 819 MiB / 2000 ~= 429 KiB, inside the band.
        let mid = P::reliable_queue_bytes_per_peer(0, 2000);
        assert!(mid > P::MIN_RELIABLE_QUEUE_BYTES_PER_PEER && mid < P::MAX_RELIABLE_QUEUE_BYTES_PER_PEER, "mid was {mid}");
        // Monotonic: more players, no more per peer.
        assert!(P::reliable_queue_bytes_per_peer(0, 4000) <= mid);
        P::override_available_memory_for_tests(0);
    }
}
