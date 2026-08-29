//! Port of `LiteNetLib/ReliableChannel.cs`: a 128-packet sliding window with a bitfield ack,
//! ordered or unordered. One instance per (channel, delivery method) pair, created on first use.
//!
//! The channel never touches the socket or the listener itself: `send_next_packets` hands each
//! packet to a `send` callback (the peer's merge buffer) and `process_packet` hands deliverable
//! packets back to the caller, so no lock is held across a listener event.

use std::collections::VecDeque;

use crate::transport::basis_network_shell::DeliveryMethod;

use super::net_constants::NetConstants;
use super::net_packet::{NetPacket, PacketProperty};
use super::net_utils::{TICKS_PER_MILLISECOND, relative_sequence_number};

/// What processing one inbound packet produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChannelOutcome {
    /// The packet was accepted (a duplicate or an ack is not).
    pub processed: bool,
    /// The channel has something to send (an ack, a resend); the peer queues it for the next update.
    pub request_send: bool,
    /// Packets the far side reported missing, for the statistics.
    pub packet_loss: u32,
}

struct PendingPacket {
    packet: Option<NetPacket>,
    time_stamp: i64,
    is_sent: bool,
}

impl PendingPacket {
    const EMPTY: Self = Self { packet: None, time_stamp: 0, is_sent: false };

    fn init(&mut self, packet: NetPacket) {
        self.packet = Some(packet);
        self.is_sent = false;
    }

    /// Sends (or resends, once the resend delay has passed) and reports whether a packet is
    /// still pending.
    fn try_send(&mut self, current_time: i64, resend_delay_ms: f64, send: &mut dyn FnMut(&NetPacket)) -> bool {
        let Some(packet) = &self.packet else {
            return false;
        };
        if self.is_sent {
            let resend_delay = resend_delay_ms * TICKS_PER_MILLISECOND as f64;
            let packet_hold_time = (current_time - self.time_stamp) as f64;
            if packet_hold_time < resend_delay {
                return true;
            }
        }
        self.time_stamp = current_time;
        self.is_sent = true;
        send(packet);
        true
    }

    fn is_empty(&self) -> bool {
        self.packet.is_none()
    }

    fn clear(&mut self) -> bool {
        self.packet.take().is_some()
    }
}

pub struct ReliableChannel {
    outgoing_queue: VecDeque<NetPacket>,
    outgoing_acks: NetPacket,
    pending_packets: Vec<PendingPacket>,
    received_packets: Vec<Option<NetPacket>>,
    early_received: Vec<bool>,
    local_sequence: i32,
    remote_sequence: i32,
    local_window_start: i32,
    remote_window_start: i32,
    must_send_acks: bool,
    delivery_method: DeliveryMethod,
    ordered: bool,
    window_size: i32,
    id: u8,
}

const BITS_IN_BYTE: i32 = 8;

impl ReliableChannel {
    pub fn new(ordered: bool, id: u8, connection_number: u8) -> Self {
        let window_size = NetConstants::DEFAULT_WINDOW_SIZE;
        let mut outgoing_acks = NetPacket::with_property(PacketProperty::Ack, (window_size - 1) / 8 + 2);
        outgoing_acks.set_channel_id(id);
        outgoing_acks.set_connection_number(connection_number);
        Self {
            outgoing_queue: VecDeque::with_capacity(window_size),
            outgoing_acks,
            pending_packets: (0..window_size).map(|_| PendingPacket::EMPTY).collect(),
            received_packets: if ordered { (0..window_size).map(|_| None).collect() } else { Vec::new() },
            early_received: if ordered { Vec::new() } else { vec![false; window_size] },
            local_sequence: 0,
            remote_sequence: 0,
            local_window_start: 0,
            remote_window_start: 0,
            must_send_acks: false,
            delivery_method: if ordered { DeliveryMethod::ReliableOrdered } else { DeliveryMethod::ReliableUnordered },
            ordered,
            window_size: window_size as i32,
            id,
        }
    }

    pub fn packets_in_queue(&self) -> usize {
        self.outgoing_queue.len()
    }

    pub fn add_to_queue(&mut self, packet: NetPacket) {
        self.outgoing_queue.push_back(packet);
    }

    fn max_sequence() -> i32 {
        i32::from(NetConstants::MAX_SEQUENCE)
    }

    fn slot(sequence: i32, window_size: i32) -> usize {
        usize::try_from(sequence.rem_euclid(window_size)).unwrap_or(0)
    }

    fn process_ack(&mut self, packet: &NetPacket) -> u32 {
        if packet.size() != self.outgoing_acks.size() {
            return 0;
        }
        let ack_window_start = i32::from(packet.sequence());
        let window_rel = relative_sequence_number(self.local_window_start, ack_window_start);
        if ack_window_start >= Self::max_sequence() || window_rel < 0 {
            return 0;
        }
        // check relevance
        if window_rel >= self.window_size {
            return 0;
        }
        let acks_data = packet.raw();
        let mut packet_loss = 0;
        let mut pending_seq = self.local_window_start;
        while pending_seq != self.local_sequence {
            let rel = relative_sequence_number(pending_seq, ack_window_start);
            if rel >= self.window_size {
                break;
            }
            let pending_idx = Self::slot(pending_seq, self.window_size);
            let current_byte = NetConstants::CHANNELED_HEADER_SIZE + pending_idx / BITS_IN_BYTE as usize;
            let current_bit = pending_idx % BITS_IN_BYTE as usize;
            if acks_data.get(current_byte).copied().unwrap_or(0) & (1 << current_bit) == 0 {
                if !self.pending_packets[pending_idx].is_empty() {
                    packet_loss += 1;
                }
                // Skip false ack
                pending_seq = (pending_seq + 1) % Self::max_sequence();
                continue;
            }
            if pending_seq == self.local_window_start {
                // Move window
                self.local_window_start = (self.local_window_start + 1) % Self::max_sequence();
            }
            // clear packet
            self.pending_packets[pending_idx].clear();
            pending_seq = (pending_seq + 1) % Self::max_sequence();
        }
        packet_loss
    }

    /// Sends what the window allows and resends what is overdue. Returns whether anything is
    /// still pending, which is the signal to keep the channel in the peer's send queue.
    pub fn send_next_packets(&mut self, current_time: i64, resend_delay_ms: f64, send: &mut dyn FnMut(&NetPacket)) -> bool {
        if self.must_send_acks {
            self.must_send_acks = false;
            send(&self.outgoing_acks);
        }

        let mut has_pending_packets = false;

        // Step 1: how many packets the window can accept.
        let capacity = self.window_size - relative_sequence_number(self.local_sequence, self.local_window_start);

        // Step 2/3: assign sequences and init pending slots.
        let mut dequeued = 0;
        while dequeued < capacity {
            let Some(mut packet) = self.outgoing_queue.pop_front() else {
                break;
            };
            packet.set_sequence(self.local_sequence as u16);
            packet.set_channel_id(self.id);
            let slot = Self::slot(self.local_sequence, self.window_size);
            self.pending_packets[slot].init(packet);
            self.local_sequence = (self.local_sequence + 1) % Self::max_sequence();
            dequeued += 1;
        }

        // send
        let mut pending_seq = self.local_window_start;
        while pending_seq != self.local_sequence {
            let slot = Self::slot(pending_seq, self.window_size);
            if self.pending_packets[slot].try_send(current_time, resend_delay_ms, send) {
                has_pending_packets = true;
            }
            pending_seq = (pending_seq + 1) % Self::max_sequence();
        }

        has_pending_packets || self.must_send_acks || !self.outgoing_queue.is_empty()
    }

    /// Processes an inbound Ack or Channeled packet. Deliverable packets are pushed to `deliver`
    /// in delivery order, each with the method the receive event should carry.
    pub fn process_packet(&mut self, packet: NetPacket, deliver: &mut Vec<(DeliveryMethod, NetPacket)>) -> ChannelOutcome {
        if packet.property() == Some(PacketProperty::Ack) {
            let packet_loss = self.process_ack(&packet);
            return ChannelOutcome { processed: false, request_send: false, packet_loss };
        }
        let seq = i32::from(packet.sequence());
        if seq >= Self::max_sequence() {
            return ChannelOutcome::default();
        }
        let relate = relative_sequence_number(seq, self.remote_window_start);
        let relate_seq = relative_sequence_number(seq, self.remote_sequence);
        if relate_seq > self.window_size {
            return ChannelOutcome::default();
        }
        // Drop bad packets
        if relate < 0 {
            // Too old packet doesn't ack
            return ChannelOutcome::default();
        }
        if relate >= self.window_size * 2 {
            // Some very new packet
            return ChannelOutcome::default();
        }

        // If very new - move window
        if relate >= self.window_size {
            let new_window_start = (self.remote_window_start + relate - self.window_size + 1) % Self::max_sequence();
            self.outgoing_acks.set_sequence(new_window_start as u16);
            // Clean old data
            while self.remote_window_start != new_window_start {
                let ack_idx = Self::slot(self.remote_window_start, self.window_size);
                let ack_byte = NetConstants::CHANNELED_HEADER_SIZE + ack_idx / BITS_IN_BYTE as usize;
                let ack_bit = ack_idx % BITS_IN_BYTE as usize;
                self.outgoing_acks.raw_mut()[ack_byte] &= !(1u8 << ack_bit);
                self.remote_window_start = (self.remote_window_start + 1) % Self::max_sequence();
            }
        }

        // Final stage - process valid packet; trigger acks send
        self.must_send_acks = true;
        let ack_idx = Self::slot(seq, self.window_size);
        let ack_byte = NetConstants::CHANNELED_HEADER_SIZE + ack_idx / BITS_IN_BYTE as usize;
        let ack_bit = ack_idx % BITS_IN_BYTE as usize;
        if self.outgoing_acks.raw()[ack_byte] & (1u8 << ack_bit) != 0 {
            // duplicate: the ack still has to go out again
            return ChannelOutcome { processed: false, request_send: true, packet_loss: 0 };
        }
        // save ack
        self.outgoing_acks.raw_mut()[ack_byte] |= 1u8 << ack_bit;

        // detailed check
        if seq == self.remote_sequence {
            deliver.push((self.delivery_method, packet));
            self.remote_sequence = (self.remote_sequence + 1) % Self::max_sequence();
            if self.ordered {
                loop {
                    let slot = Self::slot(self.remote_sequence, self.window_size);
                    let Some(held) = self.received_packets[slot].take() else {
                        break;
                    };
                    deliver.push((self.delivery_method, held));
                    self.remote_sequence = (self.remote_sequence + 1) % Self::max_sequence();
                }
            } else {
                loop {
                    let slot = Self::slot(self.remote_sequence, self.window_size);
                    if !self.early_received[slot] {
                        break;
                    }
                    self.early_received[slot] = false;
                    self.remote_sequence = (self.remote_sequence + 1) % Self::max_sequence();
                }
            }
            return ChannelOutcome { processed: true, request_send: true, packet_loss: 0 };
        }

        // holden packet
        if self.ordered {
            self.received_packets[ack_idx] = Some(packet);
        } else {
            self.early_received[ack_idx] = true;
            deliver.push((self.delivery_method, packet));
        }
        ChannelOutcome { processed: true, request_send: true, packet_loss: 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channeled(seq: u16, body: u8) -> NetPacket {
        let mut p = NetPacket::with_property(PacketProperty::Channeled, 1);
        p.set_sequence(seq);
        p.raw_mut()[4] = body;
        p
    }

    fn ack_for(channel: &ReliableChannel) -> NetPacket {
        // The ack the receiving side would send back: its outgoing_acks mirrors what it got.
        channel.outgoing_acks.clone()
    }

    #[test]
    fn ordered_channel_holds_a_gap_until_the_missing_packet_arrives() {
        let mut rx = ReliableChannel::new(true, 2, 0);
        let mut delivered = Vec::new();
        assert!(rx.process_packet(channeled(1, b'b'), &mut delivered).processed);
        assert!(delivered.is_empty(), "sequence 1 must wait for 0");
        assert!(rx.process_packet(channeled(2, b'c'), &mut delivered).processed);
        assert!(rx.process_packet(channeled(0, b'a'), &mut delivered).processed);
        let bodies: Vec<u8> = delivered.iter().map(|(_, p)| p.raw()[4]).collect();
        assert_eq!(bodies, b"abc");
        assert!(delivered.iter().all(|(m, _)| *m == DeliveryMethod::ReliableOrdered));
        // A duplicate is refused but still asks for an ack to be resent.
        let outcome = rx.process_packet(channeled(1, b'b'), &mut delivered);
        assert!(!outcome.processed && outcome.request_send);
        assert_eq!(delivered.len(), 3);
    }

    #[test]
    fn unordered_channel_delivers_immediately_and_dedups() {
        let mut rx = ReliableChannel::new(false, 0, 0);
        let mut delivered = Vec::new();
        rx.process_packet(channeled(3, b'd'), &mut delivered);
        rx.process_packet(channeled(0, b'a'), &mut delivered);
        rx.process_packet(channeled(3, b'd'), &mut delivered);
        let bodies: Vec<u8> = delivered.iter().map(|(_, p)| p.raw()[4]).collect();
        assert_eq!(bodies, b"da");
        assert!(delivered.iter().all(|(m, _)| *m == DeliveryMethod::ReliableUnordered));
    }

    #[test]
    fn packets_outside_the_window_are_dropped() {
        let mut rx = ReliableChannel::new(true, 2, 0);
        let mut delivered = Vec::new();
        assert!(!rx.process_packet(channeled(300, 0), &mut delivered).processed, "far too new");
        assert!(!rx.process_packet(channeled(32767, 0), &mut delivered).processed, "too old (wrapped)");
        assert!(!rx.process_packet(channeled(32768, 0), &mut delivered).processed, "beyond the ring");
        assert!(delivered.is_empty());
    }

    #[test]
    fn acks_release_the_sender_window_and_count_losses() {
        let mut tx = ReliableChannel::new(true, 2, 0);
        for i in 0..3u8 {
            tx.add_to_queue(NetPacket::with_property(PacketProperty::Channeled, 1 + usize::from(i)));
        }
        let mut sent = Vec::new();
        assert!(tx.send_next_packets(1_000_000, 27.0, &mut |p| sent.push(p.clone())));
        assert_eq!(sent.len(), 3);
        assert_eq!(sent.iter().map(|p| p.sequence()).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(sent.iter().all(|p| p.channel_id() == 2));

        // Nothing is resent before the delay passes...
        let mut again = Vec::new();
        assert!(tx.send_next_packets(1_000_000 + 10 * TICKS_PER_MILLISECOND, 27.0, &mut |p| again.push(p.clone())));
        assert!(again.is_empty());
        // ...and everything unacked is resent once it does.
        assert!(tx.send_next_packets(1_000_000 + 30 * TICKS_PER_MILLISECOND, 27.0, &mut |p| again.push(p.clone())));
        assert_eq!(again.len(), 3);

        // The receiver acks 0 and 2 but not 1: the window moves past 0, 1 counts as a loss.
        let mut rx = ReliableChannel::new(true, 2, 0);
        let mut delivered = Vec::new();
        rx.process_packet(sent[0].clone(), &mut delivered);
        rx.process_packet(sent[2].clone(), &mut delivered);
        let outcome = tx.process_packet(ack_for(&rx), &mut delivered);
        assert!(!outcome.processed);
        assert_eq!(outcome.packet_loss, 1);
        assert_eq!(tx.local_window_start, 1);
        // Only the lost packet is pending now.
        let mut resent = Vec::new();
        assert!(tx.send_next_packets(1_000_000 + 60 * TICKS_PER_MILLISECOND, 27.0, &mut |p| resent.push(p.clone())));
        assert_eq!(resent.iter().map(|p| p.sequence()).collect::<Vec<_>>(), vec![1]);
        // Once it is acked the channel is idle.
        rx.process_packet(sent[1].clone(), &mut delivered);
        tx.process_packet(ack_for(&rx), &mut delivered);
        assert!(!tx.send_next_packets(2_000_000, 27.0, &mut |_| {}));
    }

    #[test]
    fn the_sender_window_caps_packets_in_flight() {
        let mut tx = ReliableChannel::new(true, 2, 0);
        for _ in 0..200 {
            tx.add_to_queue(NetPacket::with_property(PacketProperty::Channeled, 1));
        }
        let mut sent = 0;
        assert!(tx.send_next_packets(1, 27.0, &mut |_| sent += 1));
        assert_eq!(sent, NetConstants::DEFAULT_WINDOW_SIZE);
        assert_eq!(tx.packets_in_queue(), 72);
    }

    #[test]
    fn a_pending_ack_is_sent_first_and_once() {
        let mut rx = ReliableChannel::new(true, 2, 1);
        let mut delivered = Vec::new();
        assert!(rx.process_packet(channeled(0, 0), &mut delivered).request_send);
        let mut out = Vec::new();
        rx.send_next_packets(1, 27.0, &mut |p| out.push(p.clone()));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].property(), Some(PacketProperty::Ack));
        assert_eq!(out[0].connection_number(), 1);
        assert_eq!(out[0].channel_id(), 2);
        assert_eq!(out[0].size(), 4 + 17);
        assert_eq!(out[0].raw()[4] & 1, 1, "bit for sequence 0 is set");
        let mut again = Vec::new();
        assert!(!rx.send_next_packets(2, 27.0, &mut |p| again.push(p.clone())));
        assert!(again.is_empty());
    }
}
