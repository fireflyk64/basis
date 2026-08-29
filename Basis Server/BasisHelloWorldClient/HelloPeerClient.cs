using System.Collections.Concurrent;
using System.Net;
using Basis.Network.Core;
using static SerializableBasis;
using LiteDelivery = LiteNetLib.DeliveryMethod;
using LiteListener = LiteNetLib.EventBasedNetListener;
using LiteManager = LiteNetLib.NetManager;
using LiteNatAddressType = LiteNetLib.NatAddressType;
using LiteNatListener = LiteNetLib.EventBasedNatPunchListener;
using LitePeer = LiteNetLib.NetPeer;
using LiteReader = LiteNetLib.NetPacketReader;
using LiteRequest = LiteNetLib.ConnectionRequest;
using LiteWriter = LiteNetLib.Utils.NetDataWriter;

namespace Basis.HelloWorld
{
    /// <summary>
    /// A <see cref="BasisHelloClient"/> that can also talk to another player <em>directly</em>,
    /// with the server acting only as an introducer.
    ///
    /// <para>The server never sees the traffic on a direct link, so this is the path a stress test
    /// has to exercise separately — a run that only sends relayed messages proves nothing about it.
    /// The sequence, all of it real:</para>
    /// <list type="number">
    /// <item>Initiator sends <c>P2PSub_Request</c> on the P2P channel, carrying a session token and
    /// an X25519 ephemeral public key. The server forwards it to the target and answers the
    /// initiator with <c>ServerArmed</c>.</item>
    /// <item>Target answers <c>P2PSub_Accept</c> with its own ephemeral key. Both sides now have the
    /// other's key and derive a per-pair send/receive key.</item>
    /// <item>Both sides send NAT introduce requests to the server from a SECOND socket. The server
    /// collects one endpoint pair per session and fires <c>NatIntroduce</c>, which tells each side
    /// where the other actually is — the ip-address introduction this whole path is built on.</item>
    /// <item>Both connect to the discovered endpoint; LiteNetLib's simultaneous-open tiebreak
    /// collapses the two attempts into one link.</item>
    /// <item>Both report <c>P2PSub_LinkUp</c>, and once the server has heard from both it marks the
    /// pair offloaded and answers <c>P2PSub_Offloaded</c>. That is the point at which the server
    /// stops relaying between them.</item>
    /// </list>
    ///
    /// <para>Sends made through <see cref="SendNumberDirect"/> take the direct link when one is up
    /// and fall back to the server's direct-origin relay channel when one is not, which is the
    /// behaviour the protocol is designed around — the caller does not have to know which.
    /// <see cref="HelloTransport"/> on the receive events reports which one it actually was.</para>
    ///
    /// <para>The direct link is encrypted, as a real one is: a client that opened it in the clear
    /// would be exercising a path the shipping client refuses to take.</para>
    /// </summary>
    public sealed class HelloPeerClient : BasisHelloClient
    {
        /// <summary>
        /// Introduce requests per punch attempt, and the gap between them. The request and the
        /// accept travel over the server connection while the introduce request leaves from the
        /// P2P socket, so the two can arrive out of order and the server drops an introduce for a
        /// session it has not armed yet. Repeating is what closes that race.
        /// </summary>
        private const int PunchRequestSends = 5;
        private const double PunchRequestIntervalMs = 250;

        /// <summary>
        /// A direct link here carries a trickle of small messages, so the default pool — sized for a
        /// server fanning out to thousands of peers — is pure footprint. With one of these managers
        /// per client, that difference is the whole memory cost of the P2P path in a stress run.
        /// </summary>
        private const int P2PPacketPoolSize = 64;

        private sealed class DirectSession
        {
            public string Token = string.Empty;
            public ushort OtherPlayerId;

            public byte[] LocalPrivate = Array.Empty<byte>();
            public byte[] LocalPublic = Array.Empty<byte>();
            public byte[]? SendKey;
            public byte[]? RecvKey;
            public IPEndPoint? CryptoEndpoint;

            public LitePeer? Peer;
            public IPAddress? ExpectedRemote;
            public int ConnectIssued;

            public int IntroducesLeft;
            public double NextIntroduceAtMs;
            public volatile bool Punching;
            public volatile bool Offloaded;

            /// <summary>Set once the server confirms the pair is offloaded — a fully direct link.</summary>
            public readonly ManualResetEventSlim Confirmed = new(false);
        }

        private readonly ConcurrentDictionary<string, DirectSession> _byToken = new();
        private readonly ConcurrentDictionary<ushort, DirectSession> _byPlayer = new();
        private readonly object _managerLock = new();

        private LiteManager? _p2p;
        private BasisCryptoLayer? _crypto;
        private double _nowMs;

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

        /// <summary>UDP port this client's direct-link socket is bound to; 0 before the first link.</summary>
        public int DirectPort => _p2p?.LocalPort ?? 0;

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
        /// failure to communicate — the sends below fall back to the server relay — so a caller that
        /// only cares about delivery can ignore it.
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
                return existing.Confirmed.Wait(timeout);
            }

            EnsureP2PManager();

            DirectSession session = NewSession(Guid.NewGuid().ToString("N"), otherPlayerId);
            if (!Register(session))
            {
                // Both sides asked at once and the other's session won the slot. Whichever survives
                // ends up as the one link between the pair, which is all the caller wanted.
                return _byPlayer.TryGetValue(otherPlayerId, out DirectSession? winner) && winner.Confirmed.Wait(timeout);
            }

            SendSignal(server, BasisNetworkCommons.P2PSub_Request, otherPlayerId, session.Token, session.LocalPublic);
            return session.Confirmed.Wait(timeout);
        }

        /// <summary>Sends one number over the direct link if there is one, otherwise via the server.</summary>
        public void SendNumberDirect(ushort targetPlayerId, int value) => SendDirect(targetPlayerId, EncodeNumber(value));

        /// <summary>Sends one string over the direct link if there is one, otherwise via the server.</summary>
        public void SendTextDirect(ushort targetPlayerId, string text) => SendDirect(targetPlayerId, EncodeText(text));

        private void SendDirect(ushort targetPlayerId, byte[] payload)
        {
            if (_byPlayer.TryGetValue(targetPlayerId, out DirectSession? session))
            {
                LitePeer? peer = session.Peer;
                if (peer != null && peer.ConnectionState == LiteNetLib.ConnectionState.Connected)
                {
                    // No recipient list and no sender id: a direct link is point to point, so the
                    // frame is just the message index and the body.
                    LiteWriter writer = new LiteWriter();
                    writer.Put(HelloMessageIndex);
                    writer.Put(payload);
                    peer.Send(writer, BasisNetworkCommons.DirectSceneChannel, LiteDelivery.ReliableOrdered);
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

        protected override void OnTick(float elapsedMs)
        {
            _nowMs += elapsedMs;

            LiteManager? manager = Volatile.Read(ref _p2p);
            if (manager == null) return;

            // Same thread as the server connection. A second thread per client would double the
            // pump cost of a population for a socket that is idle most of the time.
            manager.PollEvents();
            manager.ManualUpdate(elapsedMs);

            foreach (DirectSession session in _byToken.Values)
            {
                if (!session.Punching || session.IntroducesLeft <= 0) continue;
                if (_nowMs < session.NextIntroduceAtMs) continue;

                session.IntroducesLeft--;
                session.NextIntroduceAtMs = _nowMs + PunchRequestIntervalMs;
                try
                {
                    manager.NatPunchModule.SendNatIntroduceRequest(ServerHost, ServerPort, session.Token);
                }
                catch (Exception ex)
                {
                    BNL.LogError($"{DisplayName} could not ask for an introduction to player {session.OtherPlayerId}: {ex.Message}");
                }
            }
        }

        public override void Disconnect()
        {
            LiteManager? manager;
            lock (_managerLock)
            {
                manager = _p2p;
                _p2p = null;
            }

            // Before the base call, so the pump cannot tick a manager that is being stopped.
            base.Disconnect();

            try { manager?.Stop(); }
            catch (Exception ex) { BNL.LogWarning($"{DisplayName} failed to stop its direct-link socket: {ex.Message}"); }

            foreach (DirectSession session in _byToken.Values)
            {
                session.Confirmed.Dispose();
            }
            _byToken.Clear();
            _byPlayer.Clear();
            _crypto = null;
        }

        // ── P2P signalling, all of it through the server on the P2P channel ──

        private void HandleSignal(NetPeer server, NetPacketReader reader)
        {
            byte sub = reader.GetByte();
            BasisP2PSignalMessage msg = default;
            msg.Deserialize(reader);

            switch (sub)
            {
                case BasisNetworkCommons.P2PSub_Request:
                    OnInboundRequest(server, msg);
                    break;

                case BasisNetworkCommons.P2PSub_Accept:
                    OnInboundAccept(msg);
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

            EnsureP2PManager();

            DirectSession session = NewSession(msg.sessionToken, initiator);
            if (!Register(session)) return;

            if (!DeriveKeys(session, msg.ephemeralPublicKey)) return;

            SendSignal(server, BasisNetworkCommons.P2PSub_Accept, initiator, session.Token, session.LocalPublic);
            BeginPunching(session);
        }

        private void OnInboundAccept(BasisP2PSignalMessage msg)
        {
            if (!_byToken.TryGetValue(msg.sessionToken ?? string.Empty, out DirectSession? session)) return;
            if (!DeriveKeys(session, msg.ephemeralPublicKey)) return;

            BeginPunching(session);
        }

        private void BeginPunching(DirectSession session)
        {
            session.IntroducesLeft = PunchRequestSends;
            session.NextIntroduceAtMs = 0;   // first one on the next tick
            session.Punching = true;
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

        // ── The direct socket ──

        private void EnsureP2PManager()
        {
            lock (_managerLock)
            {
                if (_p2p != null) return;

                LiteListener listener = new LiteListener();
                listener.ConnectionRequestEvent += OnDirectConnectionRequest;
                listener.PeerConnectedEvent += OnDirectPeerConnected;
                listener.NetworkReceiveEvent += OnDirectReceive;

                LiteNatListener natListener = new LiteNatListener();
                natListener.NatIntroductionSuccess += OnNatIntroductionSuccess;

                _crypto = new BasisCryptoLayer();
                LiteManager manager = new LiteManager(listener, _crypto)
                {
                    NatPunchEnabled = true,
                    AutoRecycle = false,
                    ChannelsCount = BasisNetworkCommons.TotalChannels,
                    UpdateTime = BasisNetworkCommons.NetworkIntervalPoll,
                    PacketPoolSize = P2PPacketPoolSize,
                };
                manager.NatPunchModule.Init(natListener);
                // Fires the introduction callback inline from PollEvents, which on this client is
                // the pump thread — so the whole punch runs on one thread with no extra polling.
                manager.NatPunchModule.UnsyncedEvents = true;

                // Port 0: the OS picks. What that port turns out to be is exactly what the server
                // has to tell the other side, which is the point of the introduction.
                if (!manager.StartInManualMode(IPAddress.Any, IPAddress.IPv6Any, 0))
                {
                    BNL.LogError($"{DisplayName} could not start its direct-link socket.");
                    return;
                }

                _p2p = manager;
            }
        }

        private void OnNatIntroductionSuccess(IPEndPoint targetEndPoint, LiteNatAddressType type, string token)
        {
            if (!_byToken.TryGetValue(token, out DirectSession? session)) return;
            if (session.Peer != null) return;

            session.ExpectedRemote ??= targetEndPoint.Address;

            // Both sides dial the discovered endpoint rather than one waiting: behind a symmetric
            // NAT the other side's real port is only knowable here. LiteNetLib collapses the two
            // attempts into one link.
            if (Interlocked.Exchange(ref session.ConnectIssued, 1) != 0) return;

            if (!InstallKeys(session, targetEndPoint))
            {
                BNL.LogError($"{DisplayName} will not open an unencrypted direct link to player {session.OtherPlayerId}.");
                Interlocked.Exchange(ref session.ConnectIssued, 0);
                return;
            }

            try
            {
                LiteWriter connectData = new LiteWriter();
                connectData.Put(token);
                Volatile.Read(ref _p2p)?.Connect(targetEndPoint, connectData);
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} could not dial player {session.OtherPlayerId}: {ex.Message}");
                Interlocked.Exchange(ref session.ConnectIssued, 0);
            }
        }

        private void OnDirectConnectionRequest(LiteRequest request)
        {
            string token;
            try
            {
                token = request.Data.GetString(BasisP2PSignalMessage.MaxTokenLength);
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} got malformed direct-connect data: {ex.Message}");
                request.Reject();
                return;
            }

            // Only a peer naming a token we are actually punching for gets in. Without this the
            // socket would accept anything that found the port.
            if (!_byToken.TryGetValue(token, out DirectSession? session) || !session.Punching)
            {
                request.Reject();
                return;
            }

            if (!InstallKeys(session, request.RemoteEndPoint))
            {
                request.Reject();
                return;
            }

            session.Peer = request.Accept();
        }

        private void OnDirectPeerConnected(LitePeer peer)
        {
            DirectSession? matched = null;
            foreach (DirectSession session in _byToken.Values)
            {
                if (session.Peer != null && session.Peer.Equals(peer))
                {
                    matched = session;
                    break;
                }
                if (session.Peer == null && session.Punching &&
                    session.ExpectedRemote != null && session.ExpectedRemote.Equals(peer.Address))
                {
                    session.Peer = peer;
                    matched = session;
                    break;
                }
            }

            if (matched == null) return;

            NetPeer? server = ServerPeer;
            if (server != null)
            {
                SendSignal(server, BasisNetworkCommons.P2PSub_LinkUp, matched.OtherPlayerId, matched.Token, null);
            }
        }

        private void OnDirectReceive(LitePeer peer, LiteReader reader, byte channel, LiteDelivery delivery)
        {
            try
            {
                if (channel != BasisNetworkCommons.DirectSceneChannel) return;

                // The socket identifies the sender: a direct link has exactly one peer on it, and
                // trusting an id in the payload instead would be a spoofing hole.
                DirectSession? session = null;
                foreach (DirectSession candidate in _byToken.Values)
                {
                    if (candidate.Peer != null && candidate.Peer.Equals(peer))
                    {
                        session = candidate;
                        break;
                    }
                }
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
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} failed to read a direct message: {ex.Message}");
            }
            finally
            {
                reader.Recycle();
            }
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

        private bool InstallKeys(DirectSession session, IPEndPoint endpoint)
        {
            BasisCryptoLayer? crypto = _crypto;
            if (crypto == null || session.SendKey == null || session.RecvKey == null) return false;

            if (session.CryptoEndpoint != null) crypto.RemoveEndpoint(session.CryptoEndpoint);
            crypto.SetEndpointKeys(endpoint, session.SendKey, session.RecvKey);
            session.CryptoEndpoint = endpoint;
            return true;
        }

        private void Drop(string? token)
        {
            if (string.IsNullOrEmpty(token)) return;
            if (!_byToken.TryRemove(token, out DirectSession? session)) return;

            _byPlayer.TryRemove(new KeyValuePair<ushort, DirectSession>(session.OtherPlayerId, session));
            session.Punching = false;
            session.Offloaded = false;
            if (session.CryptoEndpoint != null) _crypto?.RemoveEndpoint(session.CryptoEndpoint);
            session.Confirmed.Set();
        }
    }
}
