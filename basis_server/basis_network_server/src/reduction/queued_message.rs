//! Port of `Reduction/QueuedMessage.cs` + `QueuedMessagePool.cs`: an inbound avatar frame
//! waiting for the tick, and the thread-local pool that recycles its buffer.

use std::cell::RefCell;

use basis_network_core::NetPeerRef;
use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;

#[derive(Default)]
pub struct QueuedMessage {
    pub from_peer: Option<NetPeerRef>,
    pub sequence: u8,
    pub avatar_message: LocalAvatarSyncMessage,
}

thread_local! {
    static POOL: RefCell<Vec<QueuedMessage>> = const { RefCell::new(Vec::new()) };
}

pub struct QueuedMessagePool;

impl QueuedMessagePool {
    const THREAD_LOCAL_CAPACITY: usize = 64;

    pub fn rent() -> QueuedMessage {
        POOL.with(|pool| pool.borrow_mut().pop()).unwrap_or_default()
    }

    /// Returns a message; its payload buffer is kept so the next `rent` can reuse it instead of
    /// allocating a fresh one per deserialization.
    pub fn return_message(mut msg: QueuedMessage) {
        msg.from_peer = None;
        msg.sequence = 0;
        let saved = msg.avatar_message.array.take();
        msg.avatar_message = LocalAvatarSyncMessage { array: saved, ..Default::default() };
        POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.len() < Self::THREAD_LOCAL_CAPACITY {
                pool.push(msg);
            }
            // else: drop it — keeps the pool bounded
        });
    }
}
