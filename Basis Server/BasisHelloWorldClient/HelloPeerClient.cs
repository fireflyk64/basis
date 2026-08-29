using System.Collections.Concurrent;
using Basis.Network.Core;
using static SerializableBasis;

namespace Basis.HelloWorld
{
    /// <summary>
    /// A <see cref="BasisHelloClient"/> that can also talk to another player <em>directly</em>,
    /// with the server acting only as an introducer.
    ///
    /// <para>The server never sees the traffic on a direct link, so this is the path a stress test
    /// has to exercise separately. The sequence, all of it real:</para>
    /// <list type="number">
    /// <item>Initiator sends <c>P2PSub_Request</c> on the P2P channel, carrying a session token and
    /// an X25519 ephemeral public key. The server forwards it to the target and answers the
    /// initiator with <c>ServerArmed</c>.</item>
    /// <item>Target answers <c>P2PSub_Accept</c> with its own ephemeral key. Both sides now have the
    /// other's key and derive a per-pair send/receive key.</item>
    /// <item>Both sides send an <c>IntroduceRequest</c> carrying their own iroh endpoint address.
    /// LiteNetLib punched from a second socket here; an iroh endpoint hole-punches itself, so the
    /// server just hands each side the other's address (<c>Introduce</c>) and tells the initiator
    /// to dial.</item>
    /// <item>The initiator dials that address on the endpoint it already has; the target accepts
    /// the connection whose payload names a session it is punching for.</item>
    /// <item>Both report <c>P2PSub_LinkUp</c>, and once the server has heard from both it marks the
    /// pair offloaded and answers <c>P2PSub_Offloaded</c>. That is the point at which the server
    /// stops relaying between them.</item>
    /// </list>
    ///
    /// <para>Sends made through <see cref="SendNumberDirect"/> take the direct link when one is up
    /// and fall back to the server's direct-origin relay channel when one is not, which is the
    /// behaviour the protocol is designed around. <see cref="HelloTransport"/> on the receive
    /// events reports which one it actually was.</para>
    ///
    /// <para>The direct link is a QUIC connection, encrypted end to end by iroh; the ephemeral key
    /// exchange is kept for protocol parity with the shipping client.</para>
    /// </summary>
    public sealed class HelloPeerClient : BasisHelloClient
    {
        private sealed class DirectSession
        {
            public string Token = string.Empty;
            public ushort OtherPlayerId;

            public byte[] LocalPrivate = Array.Empty<byte>();
            public byte[] LocalPublic = Array.Empty<byte>();
            public byte[]? SendKey;
            public byte[]? RecvKey;

            public NetPeer? Peer;
            public int Dialed;
            public volatile bool Punching;
            public volatile bool Offloaded;

            /// <summary>Set once the server confirms the pair is offloaded — a fully direct link.</summary>
            public readonly ManualResetEventSlim Confirmed = new(false);
        }

        private readonly ConcurrentDictionary<string, DirectSession> _byToken = new();
        private readonly ConcurrentDictionary<ushort, DirectSession> _byPlayer = new();

        /// <summary>Number of peers this client currently has a server-confirmed direct link to.</summary>
        public int DirectLinkCount
        {
            get
            {
                int count = 0;
                foreach (DirectSession session in _byPlayer.Values)
                {
                    if (session.Offloaded && session.Peer != null) count++;
                }
                return count;
            }
        }

        /// <summary>The endpoint other peers dial to reach this client directly; empty before it is up.</summary>
        public string DirectEndpoint => (Transport as IrohNetManager)?.ConnectionString ?? string.Empty;

        public HelloPeerClient(string displayName) : base(displayName)
        {
        }

        /// <summary>True once the server has confirmed a direct link to that player is carrying traffic.</summary>
        public bool HasDirectLink(ushort otherPlayerId)
        {
            return _byPlayer.TryGetValue(otherPlayerId, out DirectSession? session)
                   && session.Offloaded
                   && session.Peer != null;
        }

        /// <summary>
        /// Opens a direct link to another player and waits for the server to confirm it, returning
        /// false if that has not happened within <paramref name="timeout"/>. A false here is not a
        /// failure to communicate — the sends below fall back to the server relay.
        /// </summary>
        public bool OpenDirectLink(ushort otherPlayerId, TimeSpan timeout)
        {
            NetPeer? server = ServerPeer;
            if (server == null || !IsJoined)
            {
                throw new InvalidOperationException($"{DisplayName} has not joined a server yet.");
            }
            if (otherPlayerId == PlayerId)
            {
                throw new ArgumentException("A client cannot open a direct link to itself.", nameof(otherPlayerId));
            }

            if (_byPlayer.TryGetValue(otherPlayerId, out DirectSession? existing))
            {
                return existing.Confirmed.Wait(timeout) && existing.Offloaded;
            }

            DirectSession session = NewSession(Guid.NewGuid().ToString("N"), otherPlayerId);
            if (!Register(session))
            {
                // Both sides asked at once and the other's session won the slot.
                return _byPlayer.TryGetValue(otherPlayerId, out DirectSession? winner) && winner.Confirmed.Wait(timeout) && winner.Offloaded;
            }

            SendSignal(server, BasisNetworkCommons.P2PSub_Request, otherPlayerId, session.Token, session.LocalPublic);
            // A session the server declined (no such player, a link that was lost) also wakes the
            // waiter, and that is a false, not a timeout.
            return session.Confirmed.Wait(timeout) && session.Offloaded;
        }

        /// <summary>Sends one number over the direct link if there is one, otherwise via the server.</summary>
        public void SendNumberDirect(ushort targetPlayerId, int value) => SendDirect(targetPlayerId, EncodeNumber(value));

        /// <summary>Sends one string over the direct link if there is one, otherwise via the server.</summary>
        public void SendTextDirect(ushort targetPlayerId, string text) => SendDirect(targetPlayerId, EncodeText(text));

        private void SendDirect(ushort targetPlayerId, byte[] payload)
        {
            if (_byPlayer.TryGetValue(targetPlayerId, out DirectSession? session))
            {
                NetPeer? peer = session.Peer;
                if (peer != null && (peer is not IrohNetPeer iroh || iroh.IsConnected))
                {
                    // No recipient list and no sender id: a direct link is point to point, so the
                    // frame is just the message index and the body.
                    NetDataWriter writer = new NetDataWriter();
                    writer.Put(HelloMessageIndex);
                    writer.Put(payload);
                    peer.Send(writer, BasisNetworkCommons.DirectSceneChannel, DeliveryMethod.ReliableOrdered);
                    return;
                }
            }

            NetPeer? server = ServerPeer;
            if (server == null || !IsJoined)
            {
                throw new InvalidOperationException($"{DisplayName} has not joined a server yet.");
            }

            // The fallback the protocol is built around: same message, same intent, relayed by the
            // server on the channel that says "this would have gone direct if it could".
            SendVia(server, targetPlayerId, payload, BasisNetworkCommons.DirectSceneServerChannel);
        }

        protected override void OnTransportReady(NetManager transport, EventBasedNetListener listener)
        {
            listener.ConnectionRequestEvent += OnDirectConnectionRequest;
            listener.PeerConnectedEvent += OnDirectPeerConnected;
        }

        protected override void HandleOtherChannel(NetPeer peer, NetPacketReader reader, byte channel)
        {
            switch (channel)
            {
                case BasisNetworkCommons.P2PChannel:
                    HandleSignal(peer, reader);
                    break;

                case BasisNetworkCommons.DirectSceneServerChannel:
                    // A direct-origin message the server had to relay after all. Same frame as the
                    // plain scene channel, and it is still a relayed message, so it reports as one.
                    HandleRelayedScene(reader);
                    break;
            }
        }

        protected override void HandlePeerMessage(NetPeer peer, NetPacketReader reader, byte channel)
        {
            if (channel != BasisNetworkCommons.DirectSceneChannel) return;

            // The connection identifies the sender: a direct link has exactly one peer on it, and
            // trusting an id in the payload instead would be a spoofing hole.
            DirectSession? session = SessionFor(peer);
            if (session == null) return;

            if (reader.AvailableBytes < 2) return;
            ushort messageIndex = reader.GetUShort();
            if (messageIndex != HelloMessageIndex) return;

            int length = reader.AvailableBytes;
            if (length < 1) return;
            byte[] payload = new byte[length];
            reader.GetBytes(payload, length);

            RaisePayload(session.OtherPlayerId, payload, length, HelloTransport.DirectLink);
        }

        public override void Disconnect()
        {
            foreach (DirectSession session in _byToken.Values)
            {
                try { session.Peer?.Disconnect(); }
                catch (Exception ex) { BNL.LogWarning($"{DisplayName} failed to close a direct link: {ex.Message}"); }
                session.Confirmed.Set();
            }
            _byToken.Clear();
            _byPlayer.Clear();

            base.Disconnect();
        }

        // ── P2P signalling, all of it through the server on the P2P channel ──

        private void HandleSignal(NetPeer server, NetPacketReader reader)
        {
            byte sub = reader.GetByte();

            if (sub == BasisNetworkCommons.P2PSub_Introduce)
            {
                BasisP2PIntroduce introduce = default;
                introduce.Deserialize(reader);
                OnIntroduce(introduce);
                return;
            }

            BasisP2PSignalMessage msg = default;
            msg.Deserialize(reader);

            switch (sub)
            {
                case BasisNetworkCommons.P2PSub_Request:
                    OnInboundRequest(server, msg);
                    break;

                case BasisNetworkCommons.P2PSub_Accept:
                    OnInboundAccept(server, msg);
                    break;

                case BasisNetworkCommons.P2PSub_Offloaded:
                    if (_byToken.TryGetValue(msg.sessionToken ?? string.Empty, out DirectSession? confirmed))
                    {
                        confirmed.Offloaded = true;
                        confirmed.Punching = false;
                        confirmed.Confirmed.Set();
                    }
                    break;

                case BasisNetworkCommons.P2PSub_Decline:
                case BasisNetworkCommons.P2PSub_Cancel:
                case BasisNetworkCommons.P2PSub_LinkLost:
                    Drop(msg.sessionToken);
                    break;

                // ServerArmed only confirms the session is registered; the initiator still waits
                // for the target's Accept before there is a key to punch with.
                case BasisNetworkCommons.P2PSub_ServerArmed:
                    break;
            }
        }

        private void OnInboundRequest(NetPeer server, BasisP2PSignalMessage msg)
        {
            // The server rewrites otherPlayerId to the sender's id on the way out, so this is who
            // is asking. A hello client accepts everyone; a real one asks its user first.
            ushort initiator = msg.otherPlayerId;
            if (string.IsNullOrEmpty(msg.sessionToken)) return;

            DirectSession session = NewSession(msg.sessionToken, initiator);
            if (!Register(session)) return;

            if (!DeriveKeys(session, msg.ephemeralPublicKey)) return;

            SendSignal(server, BasisNetworkCommons.P2PSub_Accept, initiator, session.Token, session.LocalPublic);
            BeginPunching(server, session);
        }

        private void OnInboundAccept(NetPeer server, BasisP2PSignalMessage msg)
        {
            if (!_byToken.TryGetValue(msg.sessionToken ?? string.Empty, out DirectSession? session)) return;
            if (!DeriveKeys(session, msg.ephemeralPublicKey)) return;

            BeginPunching(server, session);
        }

        /// <summary>Hands the server this endpoint's address so it can introduce the pair.</summary>
        private void BeginPunching(NetPeer server, DirectSession session)
        {
            session.Punching = true;

            if (Transport is not IrohNetManager iroh)
            {
                BNL.LogError($"{DisplayName} can only open direct links on the iroh stack.");
                return;
            }

            BasisP2PIntroduceRequest request = new BasisP2PIntroduceRequest
            {
                sessionToken = session.Token,
                endpointAddr = iroh.EndpointAddrJson,
            };

            NetDataWriter writer = new NetDataWriter();
            writer.Put(BasisNetworkCommons.P2PSub_IntroduceRequest);
            request.Serialize(writer);
            try
            {
                server.Send(writer, BasisNetworkCommons.P2PChannel, DeliveryMethod.ReliableOrdered);
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} could not ask for an introduction to player {session.OtherPlayerId}: {ex.Message}");
            }
        }

        /// <summary>The server's introduction: the other side's address, and whether this side dials.</summary>
        private void OnIntroduce(BasisP2PIntroduce msg)
        {
            if (!_byToken.TryGetValue(msg.sessionToken ?? string.Empty, out DirectSession? session)) return;
            if (!msg.dial) return;
            if (Interlocked.Exchange(ref session.Dialed, 1) != 0) return;

            try
            {
                string target = IrohNetManager.ConnectionStringFromEndpointAddr(msg.endpointAddr);
                NetDataWriter connectData = new NetDataWriter();
                connectData.Put(session.Token);
                session.Peer = Transport?.Connect(target, 0, connectData);
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} could not dial player {session.OtherPlayerId}: {ex.Message}");
                Interlocked.Exchange(ref session.Dialed, 0);
            }
        }

        private void SendSignal(NetPeer server, byte sub, ushort otherPlayerId, string token, byte[]? ephemeralPublicKey)
        {
            BasisP2PSignalMessage msg = new BasisP2PSignalMessage
            {
                otherPlayerId = otherPlayerId,
                sessionToken = token,
                ephemeralPublicKey = ephemeralPublicKey,
            };

            NetDataWriter writer = new NetDataWriter();
            writer.Put(sub);
            msg.Serialize(writer);
            server.Send(writer, BasisNetworkCommons.P2PChannel, DeliveryMethod.ReliableOrdered);
        }

        // ── The direct link, on the same endpoint as the server connection ──

        private void OnDirectConnectionRequest(ConnectionRequest request)
        {
            string token;
            try
            {
                token = request.Data.GetString(BasisP2PSignalMessage.MaxTokenLength);
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} got malformed direct-connect data: {ex.Message}");
                request.Reject(new NetDataWriter());
                return;
            }

            // Only a peer naming a token we are actually punching for gets in. Without this the
            // endpoint would accept anything that found it.
            if (!_byToken.TryGetValue(token, out DirectSession? session) || !session.Punching)
            {
                request.Reject(new NetDataWriter());
                return;
            }

            try
            {
                session.Peer = request.Accept();
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} could not accept a direct link: {ex.Message}");
                return;
            }
            ReportLinkUp(session);
        }

        private void OnDirectPeerConnected(NetPeer peer)
        {
            DirectSession? session = SessionFor(peer);
            if (session != null) ReportLinkUp(session);
        }

        private void ReportLinkUp(DirectSession session)
        {
            NetPeer? server = ServerPeer;
            if (server == null) return;
            try
            {
                SendSignal(server, BasisNetworkCommons.P2PSub_LinkUp, session.OtherPlayerId, session.Token, null);
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} could not report its direct link: {ex.Message}");
            }
        }

        private DirectSession? SessionFor(NetPeer peer)
        {
            foreach (DirectSession candidate in _byToken.Values)
            {
                if (candidate.Peer != null && candidate.Peer.Equals(peer)) return candidate;
            }
            return null;
        }

        // ── Per-pair keys ──

        private static DirectSession NewSession(string token, ushort otherPlayerId)
        {
            BasisCryptoHandshake.GenerateKeyPair(out byte[] privateKey, out byte[] publicKey);
            return new DirectSession
            {
                Token = token,
                OtherPlayerId = otherPlayerId,
                LocalPrivate = privateKey,
                LocalPublic = publicKey,
            };
        }

        private bool Register(DirectSession session)
        {
            if (!_byPlayer.TryAdd(session.OtherPlayerId, session)) return false;
            _byToken[session.Token] = session;
            return true;
        }

        private bool DeriveKeys(DirectSession session, byte[]? remotePublicKey)
        {
            if (remotePublicKey == null || remotePublicKey.Length != BasisP2PSignalMessage.PublicKeySize)
            {
                BNL.LogError($"{DisplayName} got no usable ephemeral key for player {session.OtherPlayerId}.");
                return false;
            }

            // DerivePeerKeys settles which half of the pair is "send" from the two public keys, so
            // both ends run the identical call and still end up mirrored.
            if (!BasisCryptoHandshake.DerivePeerKeys(session.LocalPrivate, session.LocalPublic, remotePublicKey,
                    out byte[] sendKey, out byte[] recvKey))
            {
                BNL.LogError($"{DisplayName} could not derive direct-link keys for player {session.OtherPlayerId}.");
                return false;
            }

            session.SendKey = sendKey;
            session.RecvKey = recvKey;
            return true;
        }

        private void Drop(string? token)
        {
            if (string.IsNullOrEmpty(token)) return;
            if (!_byToken.TryRemove(token, out DirectSession? session)) return;

            _byPlayer.TryRemove(new KeyValuePair<ushort, DirectSession>(session.OtherPlayerId, session));
            session.Punching = false;
            session.Offloaded = false;
            try { session.Peer?.Disconnect(); }
            catch (Exception ex) { BNL.LogWarning($"{DisplayName} failed to close a direct link: {ex.Message}"); }
            session.Peer = null;
            session.Confirmed.Set();
        }
    }
}
