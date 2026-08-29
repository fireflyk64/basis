//! Port of `LiteNetLib/SequencedChannel.cs`: newest-wins delivery, optionally with the last
//! packet made reliable (`ReliableSequenced`).

use std::collections::VecDeque;

use crate::transport::basis_network_shell::DeliveryMethod;

use super::net_constants::NetConstants;
use super::net_packet::{NetPacket, PacketProperty};
use super::net_utils::{TICKS_PER_MILLISECOND, relative_sequence_number};
use super::reliable_channel::ChannelOutcome;

pub struct SequencedChannel {
    outgoing_queue: VecDeque<NetPacket>,
    local_sequence: i32,
    remote_sequence: u16,
    reliable: bool,
    last_packet: Option<NetPacket>,
    ack_packet: Option<NetPacket>,
    must_send_ack: bool,
    id: u8,
    last_packet_send_time: i64,
}

impl SequencedChannel {
    pub fn new(reliable: bool, id: u8, connection_number: u8) -> Self {
        let ack_packet = reliable.then(|| {
            let mut ack = NetPacket::with_property(PacketProperty::Ack, 0);
            ack.set_channel_id(id);
            ack.set_connection_number(connection_number);
            ack
        });
        Self {
            outgoing_queue: VecDeque::new(),
            local_sequence: 0,
            remote_sequence: 0,
            reliable,
            last_packet: None,
            ack_packet,
            must_send_ack: false,
            id,
            last_packet_send_time: 0,
        }
    }

    pub fn packets_in_queue(&self) -> usize {
        self.outgoing_queue.len()
    }

    pub fn add_to_queue(&mut self, packet: NetPacket) {
        self.outgoing_queue.push_back(packet);
    }

    pub fn send_next_packets(&mut self, current_time: i64, resend_delay_ms: f64, send: &mut dyn FnMut(&NetPacket), on_dequeue: &mut dyn FnMut(usize)) -> bool {
        if self.reliable && self.outgoing_queue.is_empty() {
            let packet_hold_time = (current_time - self.last_packet_send_time) as f64;
            if packet_hold_time >= resend_delay_ms * TICKS_PER_MILLISECOND as f64
                && let Some(packet) = &self.last_packet
            {
                self.last_packet_send_time = current_time;
                send(packet);
            }
        } else {
            while let Some(mut packet) = self.outgoing_queue.pop_front() {
                on_dequeue(packet.size());
                self.local_sequence = (self.local_sequence + 1) % i32::from(NetConstants::MAX_SEQUENCE);
                packet.set_sequence(self.local_sequence as u16);
                packet.set_channel_id(self.id);
                send(&packet);
                if self.reliable && self.outgoing_queue.is_empty() {
                    self.last_packet_send_time = current_time;
                    self.last_packet = Some(packet);
                }
            }
        }

        if self.reliable
            && self.must_send_ack
            && let Some(ack) = self.ack_packet.as_mut()
        {
            self.must_send_ack = false;
            ack.set_sequence(self.remote_sequence);
            send(ack);
        }

        self.last_packet.is_some()
    }

    /// A deliverable packet is pushed to `deliver` with the method its receive event carries.
    pub fn process_packet(&mut self, packet: NetPacket, deliver: &mut Vec<(DeliveryMethod, NetPacket)>) -> ChannelOutcome {
        if packet.is_fragmented() {
            return ChannelOutcome::default();
        }
        if packet.property() == Some(PacketProperty::Ack) {
            if self.reliable && self.last_packet.as_ref().is_some_and(|last| last.sequence() == packet.sequence()) {
                self.last_packet = None;
            }
            return ChannelOutcome::default();
        }
        let relative = relative_sequence_number(i32::from(packet.sequence()), i32::from(self.remote_sequence));
        let mut outcome = ChannelOutcome::default();
        if packet.sequence() < NetConstants::MAX_SEQUENCE && relative > 0 {
            outcome.packet_loss = u32::try_from(relative - 1).unwrap_or(0);
            self.remote_sequence = packet.sequence();
            deliver.push((if self.reliable { DeliveryMethod::ReliableSequenced } else { DeliveryMethod::Sequenced }, packet));
            outcome.processed = true;
        }
        if self.reliable {
            self.must_send_ack = true;
            outcome.request_send = true;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(seq: u16) -> NetPacket {
        let mut p = NetPacket::with_property(PacketProperty::Channeled, 0);
        p.set_sequence(seq);
        p
    }

    #[test]
    fn older_packets_are_dropped_and_losses_counted() {
        let mut rx = SequencedChannel::new(false, 1, 0);
        let mut delivered = Vec::new();
        assert!(rx.process_packet(packet(1), &mut delivered).processed);
        let late = rx.process_packet(packet(1), &mut delivered);
        assert!(!late.processed && !late.request_send);
        let skipped = rx.process_packet(packet(4), &mut delivered);
        assert!(skipped.processed);
        assert_eq!(skipped.packet_loss, 2);
        assert!(!rx.process_packet(packet(3), &mut delivered).processed);
        assert_eq!(delivered.len(), 2);
        assert!(delivered.iter().all(|(m, _)| *m == DeliveryMethod::Sequenced));
    }

    #[test]
    fn sequences_start_at_one_and_wrap() {
        let mut tx = SequencedChannel::new(false, 1, 0);
        tx.add_to_queue(packet(0));
        let mut out = Vec::new();
        assert!(!tx.send_next_packets(1, 27.0, &mut |p| out.push(p.clone()), &mut |_| {}));
        assert_eq!(out[0].sequence(), 1);
        assert_eq!(out[0].channel_id(), 1);
    }

    #[test]
    fn reliable_sequenced_resends_the_last_packet_until_acked() {
        let mut tx = SequencedChannel::new(true, 3, 0);
        tx.add_to_queue(packet(0));
        let mut out = Vec::new();
        assert!(tx.send_next_packets(0, 27.0, &mut |p| out.push(p.clone()), &mut |_| {}));
        assert_eq!(out.len(), 1);
        // Resent after the delay, and it keeps the sequence it was given.
        assert!(tx.send_next_packets(40 * TICKS_PER_MILLISECOND, 27.0, &mut |p| out.push(p.clone()), &mut |_| {}));
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].sequence(), 1);
        // The receiver acks it: the ack carries the sequence, and the sender stops.
        let mut rx = SequencedChannel::new(true, 3, 0);
        let mut delivered = Vec::new();
        assert!(rx.process_packet(out[0].clone(), &mut delivered).request_send);
        assert_eq!(delivered[0].0, DeliveryMethod::ReliableSequenced);
        let mut acks = Vec::new();
        rx.send_next_packets(0, 27.0, &mut |p| acks.push(p.clone()), &mut |_| {});
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0].property(), Some(PacketProperty::Ack));
        assert_eq!(acks[0].sequence(), 1);
        tx.process_packet(acks[0].clone(), &mut delivered);
        assert!(!tx.send_next_packets(100 * TICKS_PER_MILLISECOND, 27.0, &mut |_| panic!("nothing left to resend"), &mut |_| {}));
    }
}
