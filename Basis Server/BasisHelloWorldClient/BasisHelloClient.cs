using System.Text;
using Basis.Contrib.Auth.DecentralizedIds.Newtypes;
using Basis.Contrib.Crypto;
using Basis.Network.Core;
using BasisNetworkClient;
using static Basis.Network.Core.Compression.BasisAvatarBitPacking;
using static Basis.Network.Core.Serializable.SerializableBasis;
using static SerializableBasis;

namespace Basis.HelloWorld
{
    /// <summary>How a hello message reached its recipient.</summary>
    public enum HelloTransport
    {
        /// <summary>Relayed by the server, which stamped it with the sender's player id.</summary>
        ServerRelay,

        /// <summary>Carried over a direct peer-to-peer link, never touching the server.</summary>
        DirectLink,
    }

    /// <summary>
    /// The smallest client that can hold a conversation on a Basis server: connect, prove who you
    /// are, learn your own player id, then send numbers and strings to another player by id.
    ///
    /// <para>Everything an avatar client also does — poses, voice, resources — is left out, so what
    /// remains is the part every peer-to-peer feature is built on. Messages ride
    /// <see cref="BasisNetworkCommons.SceneChannel"/>, which the server relays verbatim to the
    /// recipients named in the message and stamps with the sender's player id. The server never
    /// looks inside the payload, so the format below is agreed between clients alone.</para>
    ///
    /// <para>Wire format of the payload, one byte of tag and then the body:
    /// <c>[kind:1][int32 little-endian]</c> for a number, <c>[kind:1][utf8...]</c> for text.</para>
    ///
    /// <para>Thread model: the transport runs in manual mode, so one pump thread — shared by every
    /// client in the process, see <see cref="Pump"/> — owns both the receive and the flush.
    /// <see cref="NumberReceived"/> and <see cref="TextReceived"/> are raised on that thread; the
    /// Send methods may be called from any thread.</para>
    /// </summary>
    public class BasisHelloClient : IDisposable
    {
        /// <summary>
        /// Identifies this app's traffic on the shared scene channel. The field is a network id in
        /// a real deployment (assigned by the server's id database); a hello-world has no ids to
        /// register, so it picks a constant and both ends agree on it.
        /// </summary>
        public const ushort HelloMessageIndex = 0xE0C0;

        private const byte KindNumber = 0;
        private const byte KindText = 1;

        private readonly PrivKey _privateKey;
        private readonly byte[] _avatarBytes;
        private readonly ManualResetEventSlim _joined = new(false);

        private NetworkClient? _client;
        private NetPeer? _peer;

        /// <summary>
        /// Gates the pump. Set only once the receive handler is attached, so a tick that lands
        /// between StartClient and that subscription cannot dispatch the auth challenge into
        /// nothing — which fails the handshake in a way that looks like a server-side reject.
        /// </summary>
        private volatile bool _pumping;

        /// <summary>Name this client shows up under in the server's player list.</summary>
        public string DisplayName { get; }

        /// <summary>This client's did:key identity, freshly generated per instance.</summary>
        public string Did { get; }

        /// <summary>
        /// The id the server knows this client by, and the one other clients address it with.
        /// Only meaningful once <see cref="IsJoined"/> — the transport learns it from the
        /// connection accept, which is also what every other player sees as the sender id.
        /// </summary>
        public ushort PlayerId => (ushort)(_peer?.RemoteId ?? 0);

        /// <summary>True once the server has accepted the identity challenge and sent our metadata.</summary>
        public bool IsJoined => _joined.IsSet;

        /// <summary>Raised with (senderPlayerId, value, path) for every number another player sends here.</summary>
        public event Action<ushort, int, HelloTransport>? NumberReceived;

        /// <summary>Raised with (senderPlayerId, text, path) for every string another player sends here.</summary>
        public event Action<ushort, string, HelloTransport>? TextReceived;

        public BasisHelloClient(string displayName)
        {
            DisplayName = displayName;

            BasisDIDAuthIdentityClient.ClientKeyCreation(out (PubKey, PrivKey) keys, out Did did);
            _privateKey = keys.Item2;
            Did = did.V;

            // The server stores this blob and replays it to other players without ever decoding it
            // — only a Unity client knows the avatar format. It has to be non-empty, because an
            // empty one fails ReadyMessage.WasDeserializedCorrectly and the join is refused.
            _avatarBytes = Encoding.UTF8.GetBytes("basis-hello-world-no-avatar");
        }

        /// <summary>
        /// Connects, authenticates, and waits until the server has admitted this client.
        /// Returns false if that has not happened within <paramref name="timeout"/>.
        /// One use per instance: reconnecting means a new client, and a new identity with it.
        /// </summary>
        public bool Connect(string ip, int port, string password, TimeSpan timeout)
        {
            if (_client != null) throw new InvalidOperationException($"{DisplayName} has already connected; construct a new client.");

            ReadyMessage ready = new ReadyMessage
            {
                playerMetaDataMessage = new ClientMetaDataMessage
                {
                    playerDisplayName = DisplayName,
                    playerUUID = Did,
                    playerPlatform = "Headless",
                },
                clientAvatarChangeMessage = new ClientAvatarChangeMessage
                {
                    byteArray = _avatarBytes,
                    loadMode = 0,
                    LocalAvatarIndex = 0,
                    ArmScale = 1f,
                    LegScale = 1f,
                    TorsoScale = 1f,
                },
                localAvatarSyncMessage = new LocalAvatarSyncMessage
                {
                    // A pose of all zeros. The server only cares that the payload is exactly the
                    // length its quality level declares — it forwards the bytes, it never reads them.
                    array = new byte[ConvertToSize(BitQuality.High)],
                    DataQualityLevel = (byte)BitQuality.High,
                    AdditionalAvatarDataSize = 0,
                    AdditionalAvatarDatas = null,
                    LinkedAvatarIndex = 0,
                },
            };

            ServerHost = ip;
            ServerPort = port;

            _client = new NetworkClient();
            _peer = _client.StartClient(ip, port, ready, Encoding.UTF8.GetBytes(password), CreateConfiguration(), manualMode: true);
            if (_peer == null)
            {
                return false;
            }

            _client.listener.NetworkReceiveEvent += OnReceive;
            _pumping = true;
            Pump.Add(this);

            return _joined.Wait(timeout);
        }

        /// <summary>Sends one number to one player. Reliable and ordered, so a volley cannot overtake itself.</summary>
        public void SendNumber(ushort targetPlayerId, int value) => Send(targetPlayerId, EncodeNumber(value));

        /// <summary>Sends one string to one player.</summary>
        public void SendText(ushort targetPlayerId, string text) => Send(targetPlayerId, EncodeText(text));

        protected static byte[] EncodeNumber(int value) => new byte[]
        {
            KindNumber,
            (byte)value,
            (byte)(value >> 8),
            (byte)(value >> 16),
            (byte)(value >> 24),
        };

        protected static byte[] EncodeText(string text)
        {
            byte[] utf8 = Encoding.UTF8.GetBytes(text);
            byte[] payload = new byte[utf8.Length + 1];
            payload[0] = KindText;
            Buffer.BlockCopy(utf8, 0, payload, 1, utf8.Length);
            return payload;
        }

        private void Send(ushort targetPlayerId, byte[] payload)
        {
            NetPeer? peer = _peer;
            if (peer == null || !IsJoined)
            {
                throw new InvalidOperationException($"{DisplayName} has not joined a server yet.");
            }

            SendVia(peer, targetPlayerId, payload, BasisNetworkCommons.SceneChannel);
        }

        /// <summary>
        /// Puts one payload on a server relay channel addressed to one player. The channel is a
        /// parameter because the server runs the same relay for the plain scene channel and for
        /// the direct-origin fallback channel, and a subclass with a P2P link needs the latter.
        /// </summary>
        protected static void SendVia(NetPeer peer, ushort targetPlayerId, byte[] payload, byte channel)
        {
            SceneDataMessage message = new SceneDataMessage
            {
                messageIndex = HelloMessageIndex,
                // A non-empty recipient list is what makes this a direct message: the server relays
                // to exactly these player ids. Leaving it null would broadcast to the whole room.
                recipients = new[] { targetPlayerId },
                payload = payload,
            };

            NetDataWriter writer = new NetDataWriter();
            message.Serialize(writer);
            peer.Send(writer, channel, DeliveryMethod.ReliableOrdered);
        }

        /// <summary>The connection to the server, for a subclass that needs to signal on it.</summary>
        protected NetPeer? ServerPeer => _peer;

        /// <summary>Where the server is. A NAT-punch introduce request has to be addressed to it by name.</summary>
        protected string ServerHost { get; private set; } = string.Empty;

        /// <summary>Port the server listens on.</summary>
        protected int ServerPort { get; private set; }

        /// <summary>
        /// Tells the server we are leaving, then closes the socket. Idempotent, and safe to call
        /// from a message handler — the pump takes the client off its list before the transport
        /// goes away, and never stops a transport it is in the middle of polling.
        /// </summary>
        public virtual void Disconnect()
        {
            if (_client == null) return;

            _pumping = false;
            Pump.Remove(this);
            _client.Disconnect();
            _client = null;
            _peer = null;
        }

        public void Dispose() => Disconnect();

        /// <summary>One pump iteration: receive and dispatch, then ack, ping and flush.</summary>
        private void Tick(float elapsedMs)
        {
            if (!_pumping) return;

            try
            {
                _client?.Poll();
                _client?.Update(elapsedMs);
                OnTick(elapsedMs);
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} pump tick failed: {ex}");
            }
        }

        private void OnReceive(NetPeer peer, NetPacketReader reader, byte channel, DeliveryMethod method)
        {
            try
            {
                switch (channel)
                {
                    case BasisNetworkCommons.AuthIdentityChannel:
                        RespondToChallenge(peer, reader);
                        break;

                    case BasisNetworkCommons.metaDataChannel:
                        // The server only sends this once it has admitted us, which makes it the
                        // signal that the connection is usable — and by now the transport has the
                        // connection accept, so PlayerId is populated.
                        _joined.Set();
                        break;

                    case BasisNetworkCommons.SceneChannel:
                        HandleRelayedScene(reader);
                        break;

                    default:
                        HandleOtherChannel(peer, reader, channel);
                        break;
                }
            }
            catch (Exception ex)
            {
                BNL.LogError($"{DisplayName} failed to handle a message on channel {channel}: {ex.Message}");
            }
            finally
            {
                reader.Recycle();
            }
        }

        private void RespondToChallenge(NetPeer peer, NetPacketReader reader)
        {
            BytesMessage challenge = new BytesMessage();
            if (!challenge.Deserialize(reader, out byte[] nonce))
            {
                BNL.LogError($"{DisplayName} received a malformed auth challenge.");
                return;
            }

            if (!Ed25519.Sign(_privateKey, new Payload(nonce), out Signature? signature) || signature == null)
            {
                BNL.LogError($"{DisplayName} could not sign the auth challenge.");
                return;
            }

            NetDataWriter writer = new NetDataWriter();
            new BytesMessage().Serialize(writer, signature.V);
            // The fragment names which key in a multi-key DID answered; this client has exactly one.
            new BytesMessage().Serialize(writer, Encoding.UTF8.GetBytes("N/A"));
            peer.Send(writer, BasisNetworkCommons.AuthIdentityChannel, DeliveryMethod.ReliableOrdered);
        }

        /// <summary>A message the base client does not know; a subclass may claim it.</summary>
        protected virtual void HandleOtherChannel(NetPeer peer, NetPacketReader reader, byte channel)
        {
        }

        /// <summary>Extra work on the shared pump thread, for a subclass with its own transport.</summary>
        protected virtual void OnTick(float elapsedMs)
        {
        }

        /// <summary>Reads one server-relayed scene message, which carries the sender's id in the frame.</summary>
        protected void HandleRelayedScene(NetPacketReader reader)
        {
            ServerSceneDataMessage message = new ServerSceneDataMessage();
            message.Deserialize(reader);

            if (message.sceneDataMessage.messageIndex != HelloMessageIndex) return;

            RaisePayload(
                message.playerIdMessage.playerID,
                message.sceneDataMessage.payload,
                message.sceneDataMessage.payloadLength,
                HelloTransport.ServerRelay);
        }

        /// <summary>
        /// Turns one decoded payload into an event. Separate from the frame parsing above because a
        /// direct link identifies its peer by the socket rather than by a sender id in the bytes.
        /// </summary>
        protected void RaisePayload(ushort sender, byte[]? payload, int length, HelloTransport transport)
        {
            if (payload == null || length < 1) return;

            switch (payload[0])
            {
                case KindNumber when length >= 5:
                    int value = payload[1] | (payload[2] << 8) | (payload[3] << 16) | (payload[4] << 24);
                    NumberReceived?.Invoke(sender, value, transport);
                    break;

                case KindText:
                    TextReceived?.Invoke(sender, Encoding.UTF8.GetString(payload, 1, length - 1), transport);
                    break;
            }
        }

        private static Configuration CreateConfiguration()
        {
            return new Configuration
            {
                UseAuthIdentity = true,
                // Nothing here reads the per-channel counters, and leaving them on costs an
                // interlocked increment per packet on the pump thread.
                EnableStatistics = false,
            };
        }

        /// <summary>
        /// One thread drives every client in the process.
        ///
        /// <para>Manual mode means somebody has to call Poll (receive and dispatch) and Update (ack,
        /// ping, flush the reliable queues). A thread per client is the obvious answer and the wrong
        /// one — Basis's own load-test client hands each core a slice of the population for exactly
        /// this reason. Sixteen clients on a two-core box would spend more time context-switching
        /// than working, and a driver that misses its tick is indistinguishable from a server that
        /// cannot keep up.</para>
        ///
        /// <para>The thread starts with the first client and exits with the last, so a process that
        /// is done talking has nothing left running.</para>
        /// </summary>
        private static class Pump
        {
            /// <summary>Poll interval. Well under LiteNetLib's own update tick, so acks are never the delay.</summary>
            private const int TickMs = 5;

            private static readonly List<BasisHelloClient> Clients = new();
            private static Thread? _thread;

            public static void Add(BasisHelloClient client)
            {
                lock (Clients)
                {
                    Clients.Add(client);
                    if (_thread != null) return;

                    _thread = new Thread(Loop) { Name = "BasisHelloClientPump", IsBackground = true };
                    _thread.Start();
                }
            }

            public static void Remove(BasisHelloClient client)
            {
                lock (Clients)
                {
                    Clients.Remove(client);
                }
            }

            private static void Loop()
            {
                List<BasisHelloClient> ticking = new();
                DateTime last = DateTime.UtcNow;

                while (true)
                {
                    DateTime now = DateTime.UtcNow;
                    float elapsedMs = (float)(now - last).TotalMilliseconds;
                    last = now;

                    // Held across the whole sweep so a Disconnect on another thread cannot stop a
                    // transport this one is in the middle of polling. The lock is reentrant, so a
                    // message handler is still free to disconnect from inside Tick — which is why
                    // the sweep runs over a copy rather than over the list being mutated.
                    lock (Clients)
                    {
                        if (Clients.Count == 0)
                        {
                            _thread = null;
                            return;
                        }

                        ticking.Clear();
                        ticking.AddRange(Clients);

                        for (int i = 0; i < ticking.Count; i++)
                        {
                            ticking[i].Tick(elapsedMs);
                        }
                    }

                    Thread.Sleep(TickMs);
                }
            }
        }
    }
}
