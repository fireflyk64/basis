# transport — port diffs

C#: `BasisNetworkCore/Transport/` + `LiteNetLib/` · Rust: `basis_network_core/src/transport/`

## File map

### The abstraction (`BasisNetworkCore/Transport/`)

| C# | Rust | lines (C#→Rust) | status |
|---|---|---|---|
| `ConnectionTarget.cs` | `connection_target.rs` | 54→72 | extended |
| `BasisNetworkShell.cs` | `basis_network_shell.rs` | 275→433 | extended |
| `BasisNetworkStackRegistry.cs` | `basis_network_stack_registry.rs` | 299→334 | deviates |
| `LNLConnectionTargetParser.cs` | `lnl_connection_target_parser.rs` | 108→105 | faithful |
| `IrohConnectionTargetParser.cs` | `iroh_connection_target_parser.rs` | 76→71 | deviates |
| `LNLNetworkImpl.cs` | `lnl_network_impl/mod.rs` + `net_manager.rs` surface | 304→39 + surface | deviates |
| `IrohNetworkImpl.cs` | `iroh_network_impl.rs` | 497→1984 | see note |
| — | `mixed_network_impl.rs` | —→216 | Rust only |
| — | `transport/mod.rs` | —→26 | Rust only |

Note on iroh: the audit brief describes `IrohNetworkImpl.cs` as an FFI wrapper over an absent
native library and `iroh_network_impl.rs` as an unrelated Rust-only reimplementation. That is not
the relationship in this repo. `basis_iroh_ffi` is a workspace member (`basis_server/Cargo.toml:19`)
whose `src/lib.rs:33` imports `basis_network_core::transport::IrohNetManager` — the type defined at
`iroh_network_impl.rs:606`. The C# file is P/Invoke glue onto a C ABI shim over the very Rust file
being audited, so "does the wire behaviour match" is answered by construction, not by comparison.
Sections below treat it as Rust-only, per the brief, and describe the C#-side gaps as such.

### The LiteNetLib protocol (`LiteNetLib/` → `lnl_network_impl/`)

| C# | Rust | lines (C#→Rust) | status |
|---|---|---|---|
| `NetConstants.cs` | `net_constants.rs` | 84→47 | faithful |
| `NetPacket.cs` | `net_packet.rs` | 165→305 | faithful |
| `InternalPackets.cs` | `internal_packets.rs` | 148→167 | faithful |
| `CompactMerge.cs` | `compact_merge.rs` | 174→255 | faithful |
| `ReliableChannel.cs` + `BaseChannel.cs` | `reliable_channel.rs` | 410→426 | faithful |
| `SequencedChannel.cs` | `sequenced_channel.rs` | 115→175 | deviates |
| `NetPeer.cs` | `net_peer.rs` | 2053→1424 | deviates + extended |
| `ConnectionRequest.cs` | `connection_request.rs` | 134→111 | deviates |
| `NetManager.cs` + `NetManager.Socket.cs` + `NetManager.HashSet.cs` | `net_manager.rs` | 4366→1265 | deviates + extended |
| `NetUtils.cs` | `net_utils.rs` | 237→84 | partial (only what the transport needs) |
| `NetStatistics.cs` | atomics on `LnlPeer` / `ManagerInner` | 178→~20 | condensed |
| `INetEventListener.cs` | `basis_network_shell.rs` (`EventBasedNetListener`) | 272→~90 | condensed |
| `NetDebug.cs` | `basis_network_shell.rs` (`NetDebug`) | 67→~20 | condensed |
| `Utils/NetDataReader.cs`, `Utils/NetDataWriter.cs` | `crate::io` (outside this module) | 1705→— | ported elsewhere |
| `Utils/FastBitConverter.cs` | inlined as `to_le_bytes`/`from_le_bytes` | 302→— | dissolved |
| `NatPunchModule.cs` | — | 305→0 | not ported |
| `Layers/*.cs` + `Utils/CRC32C.cs` | — | 267→0 | not ported |
| `Utils/NtpPacket.cs` + `Utils/NtpRequest.cs` | — | 465→0 | not ported |
| `NativeSocket.cs` | — | 549→0 | not ported |
| `NetManager.PacketPool.cs` + `PooledPacket.cs` | — | 394→0 | not ported |
| `Utils/NetSerializer.cs` + `Utils/NetPacketProcessor.cs` | — | 1058→0 | not ported |
| `PausedSocketFix.cs` | — | 67→0 | not ported |
| `Trimming.cs`, `Utils/Preserve.cs`, `Utils/INetSerializable.cs` | — | 32→0 | not applicable |

## Deviations

### Wire format

I found **no wire-format incompatibility with the C# LiteNetLib**. Every byte-level construct was
compared field by field:

- **Packet header bits.** `NetPacket.cs:73-94` and `net_packet.rs:124-156` agree: property in bits
  0-4 (`0x1F`), connection number in bits 5-6 (`0x60`, shifted 5), fragmented flag in bit 7
  (`0x80`). Sequence at offset 1 (LE u16), channel at 3, fragment id/part/total at 4/6/8
  (`NetPacket.cs:83-118` vs `net_packet.rs:158-198`). Header sizes match property for property
  (`NetPacket.cs:39-66` vs `net_packet.rs:63-74`), and `Verify` has the same two-part rule
  (`NetPacket.cs:154-162` vs `net_packet.rs:205-212`). `PacketProperty` ordinals 0-18 are identical
  including the appended `CompactMerged = 18` (`NetPacket.cs:6-27` vs `net_packet.rs:12-32`).
- **Handshake.** `ConnectRequest` layout `[prop][protocol:4][time:8][peerId:4][addrLen:1][addr][payload]`
  with `HeaderSize = 18` and the 16/28-byte address check (`InternalPackets.cs:30-70` vs
  `internal_packets.rs:26-51`); `ConnectAccept` `[prop][time:8][connNum:1][reused:1][peerId:4]`,
  `Size = 15`, with all four rejection cases (`InternalPackets.cs:104-136` vs
  `internal_packets.rs:67-93`); `PeerNotFound`/network-changed reply at
  `InternalPackets.cs:138-146` vs `internal_packets.rs:97-104`. `ProtocolId = 14` on both
  (`NetConstants.cs:59`, `net_constants.rs:24`). The `.NET SocketAddress` byte layout the connect
  request carries — LE address family, BE port, then the address, 16 or 28 bytes — is reproduced
  by hand at `net_utils.rs:30-52` and is what the P2P simultaneous-connect tiebreak compares
  (`NetPeer.cs:1428-1437` vs `net_peer.rs:544-556`).
- **Reliable ack window and bitfield.** Identical: the ack packet is
  `NetPacket(Ack, (windowSize-1)/8 + 2)` = 4-byte header + 17 payload bytes
  (`ReliableChannel.cs:101` vs `reliable_channel.rs:91`); the bit for sequence *s* is
  `ChanneledHeaderSize + (s % 128)/8`, bit `(s % 128) % 8` on both
  (`ReliableChannel.cs:302-304` vs `reliable_channel.rs:256-258`); the window-move loop that clears
  stale bits and rewrites `_outgoingAcks.Sequence` matches statement for statement
  (`ReliableChannel.cs:281-296` vs `reliable_channel.rs:241-252`); the drop rules
  (`relate < 0`, `relate >= windowSize*2`, `relateSeq > windowSize`, `seq >= MaxSequence`) match
  (`ReliableChannel.cs:245-273` vs `reliable_channel.rs:221-238`); `ProcessAck`'s window walk,
  false-ack skip and window advance match (`ReliableChannel.cs:106-172` vs
  `reliable_channel.rs:128-169`). `RelativeSequenceNumber` is the same expression on the same
  15-bit ring (`NetUtils.cs:182-185` vs `net_utils.rs:10-14`).
- **Sequenced channel.** Sequence pre-increment (first packet is 1), the `relative > 0` accept rule,
  the fragmented-packet reject, and the bare `Ack` carrying `_remoteSequence` are all the same
  (`SequencedChannel.cs:48-49,66-70,76-113` vs `sequenced_channel.rs:66-67,77-84,90-113`).
- **Fragmentation.** Same split arithmetic — `packetDataSize = mtu - headerSize - FragmentHeaderSize`,
  `totalPackets = ceil(len/packetDataSize)`, payload written at `FragmentedHeaderTotalSize` (10)
  (`NetPeer.cs:862-891` vs `net_peer.rs:735-761`). Same fragment id source (a per-peer counter
  incremented before use: `NetPeer.cs:874` vs `net_peer.rs:746`). Reassembly applies the same
  guards (`FragmentsTotal == 0 || > MaxFragmentsCount`, `FragmentPart >= FragmentsTotal`, metadata
  mismatch, duplicate part) and concatenates parts in index order stripping 10 bytes each
  (`NetPeer.cs:1137-1252` vs `net_peer.rs:1127-1214`).
- **Ping/pong and RemoteTimeDelta.** `Ping` is 3 bytes (`[prop][seq:2]`), `Pong` is 11
  (`[prop][seq:2][ticks:8]`), the ticks written at offset 3 are .NET `DateTime.UtcNow.Ticks`
  (reproduced at `net_utils.rs:18-22` with the 621355968000000000 epoch offset), and the delta is
  `remoteTicks + (elapsedMs * 10000)/2 - localTicks` on both (`NetPeer.cs:1580-1590` vs
  `net_peer.rs:1078-1088`). The pong-suppression test is the same `RelativeSequenceNumber(seq,
  lastPongSeq) > 0` (`NetPeer.cs:1573` vs `net_peer.rs:1070`).
- **MTU discovery.** Same ladder `[1024, 1164, 1392, 1404, 1424, 1432]` (`NetConstants.cs:65-74` vs
  `net_constants.rs:31-38`); same probe shape (a datagram of exactly `newMtu` bytes with the value
  written LE at offset 1 and again at `size-4`), same validity test
  (`received == size && received == endCheck && received <= MaxPacketSize`), same 1000 ms interval
  and 4-attempt cap, same MtuCheck→MtuOk echo by property flip
  (`NetPeer.cs:1322-1401` vs `net_peer.rs:462-527`). `ExtraPacketSizeForLayer` is 0 whenever no
  packet layer is installed (`NetManager.cs:747`) and the server installs none
  (`LNLNetworkImpl.cs:207` passes `null`), so the Rust omitting that subtraction
  (`net_peer.rs:486,516`) produces identical numbers.
- **Merged framing.** `[Merged][len:2][packet]...` parsed with the same `size == 0` / short-read /
  `Verify()` break conditions (`NetPeer.cs:1481-1508` vs `net_peer.rs:1010-1035`). The
  single-entry optimisation that sends the inner packet raw from offset `HeaderSize+2` matches
  (`NetPeer.cs:1717-1723` vs `net_peer.rs:885-889`).
- **CompactMerged framing.** Byte-for-byte: `0x80` long-length flag, `0x40` raw-packet flag,
  `0x3F` channel mask, 2-byte short / 3-byte long entry overhead, the canonicality rules
  (long form rejected for lengths ≤ 255, non-zero channel bits rejected on a raw tag, raw entries
  restricted to `Ack`/`Channeled` so nesting is impossible), and the `payload + 2 > MaxPacketSize`
  bound (`CompactMerge.cs:16-152` vs `compact_merge.rs:21-123`). The single-entry send path — which
  rewrites the two bytes immediately before the payload into an `Unreliable` header so the payload
  never moves, for both the 2-byte and 3-byte entry forms — is reproduced exactly
  (`NetPeer.cs:1697-1712` vs `net_peer.rs:875-882`).
- **Connection-number stamping.** The C# mutates `packet.ConnectionNumber` in place at the top of
  `SendUserData` (`NetPeer.cs:1756`); the Rust computes `header0 = (raw[0] & 0x9F) | (num << 5)`
  and stamps it into the merge buffer or a send copy (`net_peer.rs:911-913, 960-979`). The emitted
  bytes are the same, including for the compact raw-entry path where the inner packet's first byte
  is overwritten after the copy (`net_peer.rs:966-969`).

### Behaviour

1. **`SequencedChannel` resend clock — the Rust fixes a live C# bug, changing what a client sees.**
   `SequencedChannel.cs:29-31` measures the resend delay in `Stopwatch` units, `:36` stamps
   `_lastPacketSendTime` with `Stopwatch.GetTimestamp()`, but `:55` — the line that actually runs
   on the normal send path — stamps it with `DateTime.UtcNow.Ticks` (~6.4e17, versus a
   monotonic-clock value orders of magnitude smaller). `currentTime - _lastPacketSendTime` is
   therefore massively negative and the branch at `:34` returns early forever: the C# server never
   retransmits the tail packet of a `ReliableSequenced` channel. The Rust threads a single clock
   (`utc_now_ticks()`) through both branches (`sequenced_channel.rs:56-62`, fed from
   `net_peer.rs:1360`), so it does resend. Not a wire break — the retransmit is an ordinary
   Channeled packet the client's own `SequencedChannel.ProcessPacket` drops as old
   (`SequencedChannel.cs:88`) and re-acks — but a deployed client will now see duplicates it never
   saw before on `ReliableSequenced` channels.

2. **Ping sequence wraps at 32768, not 65536.** `NetPeer.cs:1928` does a bare `_pingPacket.Sequence++`
   on a `ushort`; `net_peer.rs:1338` does `wrapping_add(1) % MAX_SEQUENCE`. Both peers compare with
   `RelativeSequenceNumber`, which is mod-32768, so values in 0..32767 are unambiguous either way.
   The C# form has a latent hole at the 65535→0 rollover (C#'s `%` on a negative operand yields
   -32767, so `> 0` fails and pongs stop for ~16384 pings ≈ 4.5 hours at the 1 s interval); the
   Rust form never reaches it. The Rust receive side keeps the C# comparison verbatim
   (`net_peer.rs:1070`), so it inherits the same hole for pings *from* a C# client — deliberate
   symmetry, not a divergence.

3. **NAT punch is gone, and that is visible to a client.** `NatPunchModule.cs` is not ported and
   `NatMessage` datagrams are dropped unconditionally (`net_manager.rs:360`). The C# server used it:
   `BasisServerP2PBroker.cs:109-110` initialises `manager.NatPunchModule` so two LiteNetLib clients
   can be introduced for a direct link. The Rust replaces this with a capability flag —
   `direct_link_capable()` defaults true (`basis_network_shell.rs:257-259`) but the LNL peer
   overrides it to false (`net_manager.rs:985-987`) — and the broker declines any pair naming such a
   peer (`basis_network_server/src/p2p/basis_server_p2p_broker.rs:210`). A legacy client that asks
   for a P2P offload is refused and its traffic stays relayed. That is a functional regression for
   LiteNetLib clients, deliberately taken (documented at `lnl_network_impl/mod.rs:19-21`), and it
   costs server bandwidth rather than correctness. Only iroh peers get an introducer
   (`basis_network_server/src/p2p/iroh_peer_introducer.rs`).

4. **Latency and delivery events dropped.** `NetManager.ConnectionLatencyUpdated`
   (`NetPeer.cs:1592`, `NetManager.cs:784`) has no Rust counterpart; the C# server's handler was
   already empty (`LNLNetworkImpl.cs:55-58`), so nothing is lost. `IDeliveryEventListener` /
   `MessageDelivered` / `NetPacket.UserData` / `RecycleAndDeliver` / `_deliveredFragments`
   (`NetPeer.cs:1996-2033`) and `SendWithDeliveryEvent` are also absent — the shell interface
   (`BasisNetworkShell.cs:92-115`) never exposed them, so again nothing above the transport used
   them.

5. **`Broadcast` dropped.** `net_manager.rs:351` returns unconditionally where
   `NetManager.cs:1425-1429` would raise an event. `LNLNetworkImpl.cs:215` sets
   `BroadcastReceiveEnabled = false`, so the deployed behaviour is identical; the Rust simply has
   no config path to turn it on.

6. **The connection-request handler no longer runs on the receive thread.** C# raises it inline
   (`NetManager.cs:1302` → `CreateEvent` → `ProcessEvent`, with `UnsyncedEvents = true` at
   `LNLNetworkImpl.cs:245`), so a slow auth check stalls that socket's receive loop. The Rust
   hands it to `spawn_blocking` (`net_manager.rs:508-514`). No race follows — the request is
   inserted into `self.requests` before the spawn, so a resend from the same address takes the
   `update_request` path (`net_manager.rs:478-481`) — but the accept can now complete concurrently
   with further datagrams from that address, where the C# serialised them.

7. **Peer address change is applied inline instead of deferred.** C# marks the peer
   (`NetManager.cs:1478`) and does the actual re-key on the poll thread under the peers write lock
   (`NetManager.cs:931-946`), raising `OnPeerAddressChanged`. The Rust re-keys the address map right
   there on the receive task (`net_manager.rs:398-406`) and logs instead of raising. No
   `IPeerAddressChangedListener` is registered anywhere in the C# server, so the dropped event is
   unobservable; the timing change is real but benign.

8. **Oversized reject data.** `NetManager.cs:1211-1218`: when the reject payload pushes the
   Disconnect packet past `PossibleMtu[0]` the C# logs an error and sends the packet *anyway*, with
   the payload region still holding whatever the pooled buffer last contained. The Rust truncates to
   the 9-byte header (`net_manager.rs:529-531`). Different bytes on the wire in that pathological
   case, and the Rust is the one not leaking pool memory to a stranger.

9. **`set_connection_number` masks to two bits.** `NetPacket.cs:80` writes `value << 5` unmasked, so
   `value == 4` would set the fragmented bit; `net_packet.rs:144` masks with `0x3` first. Connection
   numbers only ever cycle 0..3 (`(n+1) % MaxConnectionNumber`, `NetManager.cs:1285` /
   `net_manager.rs:472`), so this never fires.

10. **Receive buffer is 2048, not 1432.** `net_manager.rs:123,703` versus `MaxPacketSize` in
    `NetManager.Socket.cs:633`. The Rust accepts (and will attempt to parse) a datagram larger than
    any C# peer can produce. More permissive, not incompatible.

11. **Manual mode is not implemented.** `LNLNetworkImpl.cs:269-278` delegates `StartManual`,
    `PollEvents` and `ManualUpdate` to LiteNetLib; `impl NetManager for LnlNetManager`
    (`net_manager.rs:1190-1259`) overrides none of them and inherits the trait's
    `Err(Unsupported)` defaults (`basis_network_shell.rs:284-292`). Nothing outside
    `BasisNetworkCore/Transport/` calls them in the C# server, so this is dead API — but it is dead
    API the C# had.

12. **Registry default stack changed.** `BasisNetworkStackRegistry.cs:37` has
    `DefaultId = LiteNetLibId`; `basis_network_stack_registry.rs:86` has `DEFAULT_ID = IROH_ID`,
    plus a new `SERVER_DEFAULT_ID = MIXED_ID` (`:89`). Every fallback path — `create` (`:188`),
    `get_parser` (`:235`), `probe_async` (`:256`), `create_introducer` (`:294`) — changes meaning:
    an unknown or empty stack id that resolved to LiteNetLib now resolves to iroh. Three stacks are
    registered in a different order than the C#'s two (`:106-124` vs
    `BasisNetworkStackRegistry.cs:73-79`), so `stacks()` returns a different list.
    `ActiveStackChanged` handlers also lost their try/catch (`:218-222` vs
    `BasisNetworkStackRegistry.cs:192-193`), and `CancellationToken` is dropped from the probe
    signature (`:40` vs `:29`).

13. **`IrohConnectionTargetParser` is a rewrite, not a port.** The endpoint id is no longer required
    (`iroh_connection_target_parser.rs:32-36` accepts a bare `host:port`, where
    `IrohConnectionTargetParser.cs:72-73` returned false); the port default moved from 0
    (`.cs:43`) to 4296 by delegating to the LNL parser (`.rs:41`); `Format` emits different strings
    in three ways (`.cs:25-37` vs `.rs:49-70` — default port, `@` omission, IPv6 bracketing); and
    the set of keys written differs (`.cs:19-22` always writes four, `.rs:37-46` writes
    conditionally except for `PASSWORD`). `TryParseConnectionString` (`.cs:39`) has no Rust
    equivalent.

14. **`LNLConnectionTargetParser` is faithful** — default port 4296, first-`#` password split,
    bracketed-IPv6 handling including the malformed-bracket fallback, `LastIndexOf(':')` with the
    `port > 0` and "candidate still contains a colon ⇒ bare IPv6" rules all match
    (`LNLConnectionTargetParser.cs:9,46-98` vs `lnl_connection_target_parser.rs:19,35-73`). One
    output difference: `Format` on a target with no address returns `""` in Rust
    (`.rs:93-94`) and `":4296"` in C# (`.cs:22-34`).

15. **`Connect` resolution differs.** C# goes through `NetUtils.ResolveAddress`, which prefers IPv6
    (`NetUtils.cs:37-42`); the Rust prefers IPv4 and takes IPv6 only when a v6 socket is bound
    (`net_manager.rs:1180-1187`). `Connect` with `port == 0` re-parses `target` as a full connection
    string defaulting to 4296 (`net_manager.rs:1168-1172`) where the C# built an endpoint on port 0.
    Server-side irrelevant (the server never dials), client-side a real difference.

16. **Per-subscriber reader cursors.** C# hands one `NetPacketReader` to every subscriber, so
    handler *n+1* sees handler *n*'s advanced position; the Rust clones per handler
    (`basis_network_shell.rs:412,430`), giving each an independent cursor from the same start.
    Observable only with more than one subscriber on an event.

17. **`NetPeer.RemoteUtcTime` dropped.** `BasisNetworkShell.cs:108` had it as a default interface
    member; only `remote_time_delta()` survives (`basis_network_shell.rs:243`).

## Corners cut

1. **No send batching.** `NetManager.Socket.cs:1133-1178` opens a per-worker `SendBatcher` around
   each `Parallel.ForEach` partition (`NetManager.cs:1041-1045`) and coalesces a partition's
   datagrams into one `sendmmsg`. The Rust does one `try_send_to` per datagram
   (`net_manager.rs:236`) with no batching layer. Same bytes on the wire, one syscall each instead
   of one per partition. Pure throughput cost at high player counts; nothing a client can observe
   except through added latency under load.

2. **No packet pool.** `NetManager.PacketPool.cs` (362 lines of striped, cache-line-padded pooling,
   written specifically because `PoolGetPacket` profiled at 21.6% of server CPU) has no Rust
   counterpart — every packet is a fresh `Vec<u8>` (`net_packet.rs:88-90`), and `AutoRecycle`,
   `PacketPoolSize`, `PacketPoolSizePerPeer`, `PacketPoolSizeMax` and `ResolvePacketPoolMax`
   (`LNLNetworkImpl.cs:231,247,256-257`) are read from config and ignored. `recycle_queued_packets`
   (`net_peer.rs:846`) just clears the queues. Allocation cost, not behaviour; Rust's allocator is
   not .NET's `ConcurrentQueue`, so this may well be the right call — but it was not measured here
   and the C# comment says the C# version was.

3. **No native sockets.** `NativeSocket.cs` (549 lines) is not ported; `UseNativeSockets`
   (`LNLNetworkImpl.cs:216`) is ignored. Tokio's `UdpSocket` replaces it. No observable difference.

4. **No packet layers.** `Layers/Crc32cLayer.cs`, `Layers/XorEncryptLayer.cs`,
   `Layers/PacketLayerBase.cs` and `Utils/CRC32C.cs` are not ported. **No client-observable
   change**: `LNLNetworkImpl.cs:207` constructs the manager with a `null` layer, so
   `_extraPacketLayer` was always null and `ExtraPacketSizeForLayer` always 0
   (`NetManager.cs:747`). Nothing was ever added to or checked on the wire.

5. **No NTP.** `Utils/NtpPacket.cs` and `Utils/NtpRequest.cs` are not ported, and the NTP branch of
   `HandleMessageReceived` (`NetManager.cs:1372-1398`) is absent. **No client-observable change**:
   `CreateNtpRequest` and `INtpEventListener` have zero call sites outside `LiteNetLib/` in the
   whole C# tree.

6. **No latency/loss simulation.** `SimulateLatency`, `SimulatePacketLoss`, `SimulationMinLatency`,
   `SimulationMaxLatency`, `SimulationPacketLossChance` are read from config
   (`LNLNetworkImpl.cs:248-252`, `lnl_transport_config.rs:18-22`) and ignored by
   `LnlSettings::from_config` (`net_manager.rs:87-116`). **No client-observable change in a release
   build**: `HandleSimulateLatency` and `HandleSimulatePacketLoss` are both
   `[Conditional("DEBUG")]` (`NetManager.cs:1330,1355`), so the C# release server never ran them
   either. The config fields are now silently inert on both sides, which is worth a log line the
   Rust does not emit.

7. **No NAT punch** — covered under Deviations #3; it is the one omission with a real client-facing
   consequence.

8. **No serializers.** `Utils/NetSerializer.cs` and `Utils/NetPacketProcessor.cs` (1058 lines) are
   not ported. Zero call sites outside `LiteNetLib/`. Free.

9. **No panic guard on the logic pass.** `UpdateLogic` wraps every pass in
   `catch (Exception e) { NetDebug.WriteError(...) }` (`NetManager.cs:1109-1116`), so one peer
   throwing costs one logged pass. `logic_loop` (`net_manager.rs:723-772`) has no `catch_unwind`
   anywhere, and a panic inside `pool.install(...)` (`:757`) or the serial fallback unwinds out of
   the loop and kills the logic thread — after which acks, resends, pings, timeouts and every
   merged datagram stop for every peer, while the receive tasks keep running and the process stays
   up. The reachable path is narrow (the merge-buffer slice indices are bounded by the MTU guards at
   `net_peer.rs:938-945`, except under a `MtuOverride` larger than `MaxPacketSize`, where the C#
   throws and the Rust panics), but the failure modes are not comparable: C# degrades, Rust stops.

10. **Ack re-arm hole, inherited and half-fixed.** In both, a channel can set "I owe an ack" between
    `send_next_packets` returning false and the queued flag being cleared, in which case the ack
    waits. The C# never rechecks (`BaseChannel.cs:36-42`); the Rust rechecks the outgoing queue
    (`net_peer.rs:1388-1393`) but not `must_send_acks`, so the residual window is the same size as
    the C#'s for acks specifically. Bounded — the next inbound packet on that channel re-arms it.

11. **`PeersUpdatedTotal` / `PeerUpdateBusyMicros` / slow-pass warnings** (`NetManager.cs:486-489,
    1084-1101`) are not ported; the Rust logic loop emits no pass-time telemetry. The C# used these
    to feed the CPU budget allocator.

## Improvements

1. **Two C# data races are closed by construction.** `ReliableChannel.ProcessPacket` guards only the
   ack bitfield with `lock (_outgoingAcks)` (`ReliableChannel.cs:279-315`); `_remoteSequence`,
   `_receivedPackets` and `_earlyReceived` (`:320-358`) are touched with no lock at all, and with
   `MultiSocketCount > 1` several receive threads can be inside the same channel at once
   (`NetManager.Socket.cs:172-177`). `SequencedChannel` is worse: the logic thread reads
   `_lastPacket` (`SequencedChannel.cs:33`) while a receive thread nulls it (`:83`). The Rust puts
   one `Mutex<Option<Channel>>` per channel around all of it (`net_peer.rs:645-655`) and — the part
   that matters — drops it before any listener event is raised (`net_peer.rs:1107-1116`), so a
   handler may send on the channel that just delivered to it.

2. **Reliable send queue is bounded and watched.** The C# bounds only the unreliable queues
   (`NetPeer.cs:490-556`); a peer that stops acking grows its reliable outgoing queue without limit
   behind the 128-packet window. The Rust adds a per-peer byte budget refreshed with the population
   (`net_peer.rs:229,676-701`, `net_manager.rs:199-210`), returns `SendError::QueueFull` rather than
   accepting the write, and disconnects a peer whose queue has not drained for the grace period
   (`net_peer.rs:1266-1272,1318-1327`).

3. **Fragment reassembly is bounded by bytes, not set count.** C# caps the number of in-flight sets
   (`NetPeer.cs:1167`) — but one set can hold 65535 fragments, so the real memory a sender can pin
   is unbounded. The Rust adds a byte cap (`net_peer.rs:1152-1160`, default 8 MB) on top of the same
   set cap.

4. **Two DoS caps the C# lacked.** `max_pending_requests` on the connection-request table
   (`net_manager.rs:483-497`) — the C# `_requestsDict` grows one entry per spoofable source address
   — and `max_reject_peers` (`net_manager.rs:544-566`), past which a rejection becomes one datagram
   with no retained state instead of a full peer object.

5. **Ping sequence rollover fixed** (Deviations #2) and **ReliableSequenced resend fixed**
   (Deviations #1).

6. **Bounds check added to MTU handling.** `NetPeer.cs:1354` indexes `PossibleMtu[_mtuIdx + 1]` with
   no length check, relying on `_finishMtu` being set first; `net_peer.rs:486` checks explicitly.

7. **Duplicate pongs no longer skew RTT.** `NetPeer.cs:1583-1586` calls `_pingTimer.Stop()` and reads
   `ElapsedMilliseconds`, which on a second pong for the same sequence re-feeds the same sample into
   the average. `net_peer.rs:1083` takes the timer, so only the first pong counts.

8. **`try_read_entry` never advances the offset on failure.** `CompactMerge.cs:109` increments
   `offset` before several of its rejection returns; `compact_merge.rs:76,121` works on a local and
   commits only on success. No caller depends on it, but the Rust version is the safe one to reuse.

9. **Bind failure is reported.** `LNLNetworkImpl.cs:262` discards LiteNetLib's `bool Start(...)`
   return, so a failed bind was silent; `net_manager.rs:1191-1193` returns a `BasisResult` with the
   address and OS error in it.

10. **Listener handlers are panic-isolated.** `basis_network_shell.rs:380-391` wraps every raise in
    `catch_unwind`; a C# handler that throws propagated into the receive or logic thread.

11. **`Drop for LnlNetManager` stops the transport** (`net_manager.rs:1261-1265`); the C# had no
    dispose path.

12. **`ConnectionRequest` is genuinely single-decision.** `connection_request.rs:79-110` CAS-guards
    accept/reject and reports a contradicting second verdict as a `Conflict`; the C# `TryActivate`
    (`ConnectionRequest.cs:40-43`) silently returns null/void.

13. **Oversized reject data is truncated rather than sent from an uninitialised pool buffer**
    (Deviations #8).

## Rust-only

**`iroh_network_impl.rs`** (1984 lines) is a full QUIC transport over `iroh 1.1`
(workspace `Cargo.toml:34`, `Cargo.lock` pins 1.1.0), not a port of anything. It defines its own
wire protocol: ALPNs `basis/1` and `basis-probe/1` (`:71,73`); reliable-ordered and
reliable-sequenced share one uni stream per channel (`:344`, `:1385-1404`), reliable-unordered gets
a fresh stream per message (`:345`, `:1406-1417`), unreliable and sequenced ride QUIC datagrams
(`:347-361`); stream frames are `[kind][channel][len:u32 LE][payload]` capped at 1 MiB (`:95,1428`),
datagrams are `[channel]` or `[channel|0x40][seq:u16 LE]` (`:88-89,274-286`) with a wrapping
old-sequence drop (`:1385-1387`); a bidirectional control stream carries
`CONNECT/ACCEPTED/REJECTED/PING/PONG/DISCONNECT` (`:76-81`). Peer identity is the ed25519 public key
in z-base-32, persisted across restarts (`:688-700,724`). It adds relay configuration (`:996-1010`),
QUIC transport tuning with keep-alive and flow-control windows (`:972-994`), the same reliable-byte
budget and priority-queue machinery as the LNL port (`:257-311`), a handshake cap and a 15 s
decision timeout (`:454-468`), and a `PendingPeer` that queues reliable sends issued before the dial
completes (`:1853-1984`). Two defects worth flagging: `connection_string()` finds the bound socket
and then formats `{id}@127.0.0.1:{port}`, discarding the real address (`:723-728`); and client-side
peers are all inserted under id 0 (`:907,1610,1643`), so a Rust client dialling two servers
overwrites the first — harmless for the server, wrong for a multi-dial client. It does *not* add
iroh discovery (both endpoints use `presets::Minimal`, `:754,1015`), so the config field
`publish_address` is dead, nor 0-RTT, nor any reconnection logic. Relative to `IrohNetworkImpl.cs`
the Rust is the implementation and the C# is the shim; the C#-side gaps are that it hardcodes
`RemoteTimeDelta = 0` (`IrohNetworkImpl.cs:193`, the ABI struct has no field for it),
`DisconnectForce` silently delegates to the graceful path (`:215`), `SendUnconnectedMessage` always
returns false (`:468-472`) even though the Rust implements the probe (`:743-809,1094-1110`), and
every peer property read takes a process-global lock (`:175-195`).

**`mixed_network_impl.rs`** (216 lines) has no C# counterpart — the C# registry knows only
`litenetlib` and `iroh` (`BasisNetworkStackRegistry.cs:34-37`). It runs both transports as one
logical server and is now the server default (`basis_network_stack_registry.rs:89`). It is a
fan-out/fan-in facade with no framing of its own: one shared `PeerIdAllocator` so a player id names
one player regardless of carrier (`:58-61`); LiteNetLib keeps the configured port and iroh takes
`SetPort + 1` unless configured, with `0 → 0` preserved (`:17-23,85-93`); targets route by shape —
an `@` before any `#`, or a 52-char z-base-32 / 64-char hex id, goes to iroh (`:97-100`), driving
`connect` (`:130-136`), `probe` (`:103-109`) and the composite parser (`:177-193`); start is
all-or-nothing, stopping LNL if iroh fails (`:113-122`); statistics and counters are summed
(`:144-166`).

**`transport/mod.rs`** (26 lines) is module aggregation with no C# analogue.

## Verdict

**No wire-format incompatibility found.** I read the whole of both sides of every byte-level file
and compared them field by field: `NetConstants.cs`/`net_constants.rs`, `NetPacket.cs`/`net_packet.rs`,
`InternalPackets.cs`/`internal_packets.rs`, `CompactMerge.cs`/`compact_merge.rs`,
`ReliableChannel.cs`+`BaseChannel.cs`/`reliable_channel.rs`, `SequencedChannel.cs`/`sequenced_channel.rs`,
and `NetUtils.RelativeSequenceNumber` plus the `SocketAddress` serialisation. In `NetPeer.cs` I read
closely everything that touches bytes or timing — `SendInternal` (both overloads),
`SendUserData`, `SendMerged`, `ProcessPacket`, `AddReliablePacket`, `ProcessMtuPacket`,
`UpdateMtuLogic`, `ProcessConnectRequest`, `ProcessConnectAccept`, `Update`, the constructors, the
unreliable enqueue paths — against the whole of `net_peer.rs`. In `NetManager` I read
`HandleMessageReceived`, `CreateReceiveEvent`, `OnConnectionSolved`, `ProcessConnectRequest`,
`UpdateLogic`, `ProcessEvent`, and the `SendRaw`/batching and receive-thread sections of
`NetManager.Socket.cs`, against the whole of `net_manager.rs`. The Rust's 30 in-module tests pass and
several assert the C# layout directly (`net_packet.rs:253-264`, `internal_packets.rs:112-125`,
`net_utils.rs:69-78`).

Sampled rather than read line by line: the remainder of `NetManager.cs` and `NetManager.Socket.cs`
(the `SendToAll` overload family, `NetManager.HashSet.cs`'s peer set, the socket option and error
plumbing), `NetStatistics.cs`, `INetEventListener.cs`, and the `Utils/` readers and writers, which
live outside this module in `crate::io`. The not-ported files I checked only for call sites, not
content — that check is what supports the "no observable change" claims for the packet layers, NTP,
the simulation hooks and the serializers, and it is a call-site search, not a proof.

The abstraction and parser pairs were compared in full by two focused passes; their findings are
recorded above with citations, and I spot-verified the ones with behavioural consequences
(the registry default, the iroh parser rewrite, the dropped manual mode, the ignored config knobs).
I did not diff `iroh_network_impl.rs` against anything, per the brief.

Counts: **17 deviations, 11 corners cut, 13 improvements.** The wire-critical core — headers,
handshake, ack window, sequencing, fragmentation, MTU, ping, and both merged framings — is a
faithful port. The deviations that a deployed client can actually perceive are three: the
`ReliableSequenced` tail packet is now retransmitted where the C# had a clock bug that stopped it,
NAT-punch P2P offload is refused for LiteNetLib clients so their traffic stays relayed, and an
oversized connection rejection is truncated instead of padded with pool garbage. The most serious
non-wire gap is the missing panic guard on the logic thread: the C# logged and carried on, the Rust
would stop servicing every peer.
