use crate::protocol::BasisNetworkCommons;

/// Reader for the compressed-avatar-bundle body carried on `COMPRESSED_AVATAR_BUNDLE_CHANNEL`.
///
/// The server writes the body grouped by channel:
///   group*  where group := [origChannel:1][n:1][msgLen:2-LE] x n [bodies]
/// with the DeltaAvatarChannel group's bodies column-transposed. [`Self::try_flatten`] rewrites
/// a grouped body into the flat `[origChannel:1][msgLen:2-LE][body]*` stream.
pub struct BasisAvatarBundleCodec;

impl BasisAvatarBundleCodec {
    /// Buffer size [`Self::try_flatten`] can require for a grouped body of `grouped_length` bytes.
    pub fn max_flat_size(grouped_length: usize) -> usize {
        grouped_length * 2 + 8
    }

    /// Classifies a grouped bundle body by traffic class, walking only the group headers.
    /// Returns `Some(delta_only)`, or `None` on a malformed body (the caller should drop it).
    pub fn try_classify(grouped: &[u8]) -> Option<bool> {
        let mut delta_only = true;
        let mut read = 0usize;

        while read + 2 <= grouped.len() {
            let channel = grouped[read];
            let n = usize::from(grouped[read + 1]);
            read += 2;
            if n == 0 || read + n * 2 > grouped.len() {
                return None;
            }
            if channel != BasisNetworkCommons::DELTA_AVATAR_CHANNEL {
                delta_only = false;
            }
            let mut body_total = 0usize;
            for i in 0..n {
                let len = usize::from(u16::from_le_bytes([grouped[read + i * 2], grouped[read + i * 2 + 1]]));
                if len == 0 {
                    return None;
                }
                body_total += len;
            }
            read += n * 2;
            if read + body_total > grouped.len() {
                return None;
            }
            read += body_total;
        }

        if read != grouped.len() {
            return None;
        }
        Some(delta_only)
    }

    /// Rewrites a grouped bundle body into the flat `[channel:1][len:2-LE][body]*` form.
    /// Returns the flat length, or `None` on any inconsistency.
    pub fn try_flatten(grouped: &[u8], flat: &mut [u8]) -> Option<usize> {
        let mut read = 0usize;
        let mut write = 0usize;

        while read + 2 <= grouped.len() {
            let channel = grouped[read];
            let n = usize::from(grouped[read + 1]);
            read += 2;
            if n == 0 || read + n * 2 > grouped.len() {
                return None;
            }

            let lengths_at = read;
            read += n * 2;
            let len_of = |i: usize| usize::from(u16::from_le_bytes([grouped[lengths_at + i * 2], grouped[lengths_at + i * 2 + 1]]));

            let mut body_total = 0usize;
            let mut max_len = 0usize;
            for i in 0..n {
                let len = len_of(i);
                if len == 0 {
                    return None;
                }
                body_total += len;
                max_len = max_len.max(len);
            }
            if read + body_total > grouped.len() {
                return None;
            }
            if write + n * 3 + body_total > flat.len() {
                return None;
            }

            // Reserve the flat frames first so the un-transpose can scatter straight into
            // their body regions. Frame i is [channel][len:2] followed by len body bytes.
            let frame_at = write;
            for i in 0..n {
                let len = len_of(i);
                flat[write] = channel;
                flat[write + 1..write + 3].copy_from_slice(&(len as u16).to_le_bytes());
                write += 3 + len;
            }

            if channel != BasisNetworkCommons::DELTA_AVATAR_CHANNEL {
                // Bodies are stored back to back in entry order.
                let mut src = read;
                let mut at = frame_at;
                for i in 0..n {
                    let len = len_of(i);
                    flat[at + 3..at + 3 + len].copy_from_slice(&grouped[src..src + len]);
                    src += len;
                    at += 3 + len;
                }
            } else {
                // Column-major. Walk the columns in exactly the order the writer emitted them.
                let mut src = read;
                for j in 0..max_len {
                    let mut at = frame_at;
                    for i in 0..n {
                        let len = len_of(i);
                        if j < len {
                            flat[at + 3 + j] = grouped[src];
                            src += 1;
                        }
                        at += 3 + len;
                    }
                }
                if src != read + body_total {
                    return None;
                }
            }

            read += body_total;
        }

        if read != grouped.len() {
            return None;
        }
        Some(write)
    }
}
