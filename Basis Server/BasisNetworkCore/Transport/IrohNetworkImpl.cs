using System;
using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace Basis.Network.Core
{
    /// <summary>
    /// P/Invoke surface of <c>basis_iroh_ffi</c>: the Rust server's own iroh transport, exposed as
    /// a C ABI so a C# client speaks exactly the same wire protocol as the server. Handles are
    /// integers, events are pulled with <see cref="ManagerPoll"/>, and every call reports failure
    /// through a negative return code with the message in <see cref="LastError"/>.
    /// </summary>
    internal static class IrohNative
    {
        public const string Library = "basis_iroh_ffi";
        public const uint AbiVersion = 1;

        public const int Ok = 0;
        public const int ErrNoHandle = -1;
        public const int ErrBadArgument = -2;
        public const int ErrBufferTooSmall = -3;
        public const int ErrTransport = -4;
        public const int ErrPanic = -5;

        public const uint EventNone = 0;
        public const uint EventPeerConnected = 1;
        public const uint EventPeerDisconnected = 2;
        public const uint EventReceive = 3;
        public const uint EventConnectionRequest = 4;
        public const uint EventNetworkError = 5;
        public const uint EventReceiveUnconnected = 6;

        [StructLayout(LayoutKind.Sequential)]
        public struct Event
        {
            public uint Kind;
            public uint DataLen;
            public ulong Peer;
            public ulong Request;
            public int Reason;
            public int SocketError;
            public byte Channel;
            public byte Delivery;
            public byte RemoteIpLen;
            public byte Reserved;
            public ushort RemotePort;
            public ushort Reserved2;
            [MarshalAs(UnmanagedType.ByValArray, SizeConst = 16)]
            public byte[] RemoteIp;

            public IPEndPoint RemoteEndPoint()
            {
                if (RemoteIpLen == 4) return new IPEndPoint(new IPAddress(new ReadOnlySpan<byte>(RemoteIp, 0, 4)), RemotePort);
                if (RemoteIpLen == 16) return new IPEndPoint(new IPAddress(RemoteIp), RemotePort);
                return new IPEndPoint(IPAddress.Any, RemotePort);
            }
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct Statistics
        {
            public ulong PacketsSent;
            public ulong PacketsReceived;
            public ulong BytesSent;
            public ulong BytesReceived;
            public ulong PacketLoss;
            public long UnreliableDropped;
            public long PriorityUnreliableDropped;
            public int ConnectedPeers;
            public int Reserved;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct PeerInfo
        {
            public int Id;
            public int RemoteId;
            public int RoundTripTime;
            public int Mtu;
            public float TimeSinceLastPacket;
            public byte Connected;
            public byte IpLen;
            public ushort Reserved;
            [MarshalAs(UnmanagedType.ByValArray, SizeConst = 16)]
            public byte[] Ip;

            public IPAddress Address()
            {
                if (IpLen == 4) return new IPAddress(new ReadOnlySpan<byte>(Ip, 0, 4));
                if (IpLen == 16) return new IPAddress(Ip);
                return IPAddress.None;
            }
        }

        [DllImport(Library, EntryPoint = "basis_iroh_abi_version")] public static extern uint AbiVersionOf();
        [DllImport(Library, EntryPoint = "basis_iroh_last_error")] public static extern int LastErrorInto(byte[] buf, UIntPtr cap);
        [DllImport(Library, EntryPoint = "basis_iroh_set_transport_setting", CharSet = CharSet.Ansi)] public static extern int SetTransportSetting(string name, string value);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_create")] public static extern ulong ManagerCreate(int enableStatistics);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_start", CharSet = CharSet.Ansi)] public static extern int ManagerStart(ulong handle, string ipv4, string ipv6, ushort port);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_stop")] public static extern int ManagerStop(ulong handle);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_destroy")] public static extern int ManagerDestroy(ulong handle);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_connect", CharSet = CharSet.Ansi)] public static extern int ManagerConnect(ulong handle, string target, ushort port, byte[] payload, UIntPtr len, out ulong peer);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_poll")] public static extern int ManagerPoll(ulong handle, out Event ev, byte[] data, UIntPtr cap);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_pending_events")] public static extern int ManagerPendingEvents(ulong handle);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_connected_peers")] public static extern int ManagerConnectedPeers(ulong handle);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_statistics")] public static extern int ManagerStatistics(ulong handle, out Statistics stats);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_connection_string")] public static extern int ManagerConnectionString(ulong handle, byte[] buf, UIntPtr cap);
        [DllImport(Library, EntryPoint = "basis_iroh_manager_endpoint_addr_json")] public static extern int ManagerEndpointAddrJson(ulong handle, byte[] buf, UIntPtr cap);
        [DllImport(Library, EntryPoint = "basis_iroh_endpoint_addr_to_connection_string")] public static extern int EndpointAddrToConnectionString(byte[] json, UIntPtr jsonLen, byte[] buf, UIntPtr cap);
        [DllImport(Library, EntryPoint = "basis_iroh_peer_send")] public static extern int PeerSend(ulong handle, ulong peer, byte channel, byte delivery, byte[] data, UIntPtr len);
        [DllImport(Library, EntryPoint = "basis_iroh_peer_disconnect")] public static extern int PeerDisconnect(ulong handle, ulong peer, byte[] data, UIntPtr len);
        [DllImport(Library, EntryPoint = "basis_iroh_peer_queue_count")] public static extern int PeerQueueCount(ulong handle, ulong peer, byte channel, byte delivery);
        [DllImport(Library, EntryPoint = "basis_iroh_peer_info")] public static extern int PeerInfoOf(ulong handle, ulong peer, out PeerInfo info);
        [DllImport(Library, EntryPoint = "basis_iroh_peer_release")] public static extern int PeerRelease(ulong handle, ulong peer);
        [DllImport(Library, EntryPoint = "basis_iroh_request_accept")] public static extern int RequestAccept(ulong handle, ulong request, out ulong peer);
        [DllImport(Library, EntryPoint = "basis_iroh_request_reject")] public static extern int RequestReject(ulong handle, ulong request, byte[] data, UIntPtr len);

        public static string LastError()
        {
            byte[] buf = new byte[1024];
            int n = LastErrorInto(buf, (UIntPtr)buf.Length);
            return n <= 0 ? string.Empty : Encoding.UTF8.GetString(buf, 0, Math.Min(n, buf.Length));
        }

        /// <summary>Reads a string-valued query, growing the buffer when the library asks for more room.</summary>
        public static string ReadString(Func<byte[], UIntPtr, int> query)
        {
            byte[] buf = new byte[512];
            while (true)
            {
                int n = query(buf, (UIntPtr)buf.Length);
                if (n >= 0) return Encoding.UTF8.GetString(buf, 0, n);
                if (n != ErrBufferTooSmall || buf.Length >= 1 << 20) throw new IrohTransportException(LastError());
                buf = new byte[buf.Length * 4];
            }
        }

        public static void EnsureAbi()
        {
            uint version = AbiVersionOf();
            if (version != AbiVersion)
            {
                throw new IrohTransportException($"basis_iroh_ffi speaks ABI {version}, this build expects {AbiVersion}; rebuild the native library and the client together.");
            }
        }
    }

    public sealed class IrohTransportException : Exception
    {
        public IrohTransportException(string message) : base(message) { }
    }

    /// <summary>
    /// A connection over the Rust iroh transport. Peer identity is the transport's own, so two
    /// wrappers of the same connection compare equal exactly as the LiteNetLib wrappers did.
    /// </summary>
    public sealed class IrohNetPeer : NetPeer
    {
        private readonly IrohNetManager _manager;
        internal readonly ulong Handle;
        private object _tag;
        private IrohNative.PeerInfo _last;

        internal IrohNetPeer(IrohNetManager manager, ulong handle)
        {
            _manager = manager;
            Handle = handle;
            Refresh();
        }

        private IrohNative.PeerInfo Refresh()
        {
            if (_manager.Handle != 0 && IrohNative.PeerInfoOf(_manager.Handle, Handle, out IrohNative.PeerInfo info) == IrohNative.Ok)
            {
                _last = info;
            }
            else
            {
                _last.Connected = 0;
            }
            return _last;
        }

        public int Id => Refresh().Id;
        public IPAddress Address => Refresh().Address();
        public int RemoteId => Refresh().RemoteId;
        public int RoundTripTime => Refresh().RoundTripTime;
        public float TimeSinceLastPacket => Refresh().TimeSinceLastPacket;
        public long RemoteTimeDelta => 0;
        public int Mtu => Refresh().Mtu;
        public bool IsConnected => Refresh().Connected != 0;

        public object Tag
        {
            get => _tag;
            set => _tag = value;
        }

        public void Disconnect() => Disconnect(null);

        public void Disconnect(byte[] b)
        {
            if (_manager.Handle == 0) return;
            int code = IrohNative.PeerDisconnect(_manager.Handle, Handle, b ?? Array.Empty<byte>(), (UIntPtr)(b?.Length ?? 0));
            if (code != IrohNative.Ok && code != IrohNative.ErrNoHandle)
            {
                BNL.LogWarning($"iroh disconnect failed: {IrohNative.LastError()}");
            }
        }

        public void DisconnectForce() => Disconnect(null);

        public int GetPacketsCountInQueue(byte channel, DeliveryMethod deliveryMethod)
        {
            if (_manager.Handle == 0) return 0;
            int count = IrohNative.PeerQueueCount(_manager.Handle, Handle, channel, (byte)deliveryMethod);
            return count < 0 ? 0 : count;
        }

        public void Send(byte[] data, byte channelNumber, DeliveryMethod deliveryMethod)
        {
            SendRange(data, data?.Length ?? 0, channelNumber, deliveryMethod);
        }

        public void Send(NetDataWriter data, byte channelNumber, DeliveryMethod deliveryMethod)
        {
            SendRange(data.Data, data.Length, channelNumber, deliveryMethod);
        }

        private void SendRange(byte[] data, int length, byte channelNumber, DeliveryMethod deliveryMethod)
        {
            if (_manager.Handle == 0) return;
            int code = IrohNative.PeerSend(_manager.Handle, Handle, channelNumber, (byte)deliveryMethod, data ?? Array.Empty<byte>(), (UIntPtr)length);
            if (code == IrohNative.Ok || code == IrohNative.ErrNoHandle) return;
            throw new IrohTransportException(IrohNative.LastError());
        }

        public void SendUnreliableRawMerge(byte[] data, int offset, int length, byte channelNumber, int patchOffset = -1, byte patchValue = 0)
        {
            byte[] copy = new byte[length];
            Buffer.BlockCopy(data, offset, copy, 0, length);
            if (patchOffset >= 0 && patchOffset < length) copy[patchOffset] = patchValue;
            Send(copy, channelNumber, DeliveryMethod.Unreliable);
        }

        public override bool Equals(object obj) => obj is IrohNetPeer other && other.Handle == Handle && ReferenceEquals(other._manager, _manager);

        public override int GetHashCode() => Handle.GetHashCode();
    }

    public sealed class IrohConnectionRequest : ConnectionRequest
    {
        private readonly IrohNetManager _manager;
        private readonly ulong _request;
        private readonly NetDataReader _data;
        private readonly IPEndPoint _remote;

        internal IrohConnectionRequest(IrohNetManager manager, ulong request, byte[] data, IPEndPoint remote)
        {
            _manager = manager;
            _request = request;
            _data = new NetDataReader(data);
            _remote = remote;
        }

        public NetDataReader Data => _data;
        public IPEndPoint RemoteEndPoint => _remote;

        public NetPeer Accept()
        {
            if (_manager.Handle == 0) throw new IrohTransportException("the transport has been stopped");
            int code = IrohNative.RequestAccept(_manager.Handle, _request, out ulong peer);
            if (code != IrohNative.Ok) throw new IrohTransportException(IrohNative.LastError());
            return _manager.PeerFor(peer);
        }

        public void Reject(NetDataWriter w)
        {
            if (_manager.Handle == 0) return;
            byte[] payload = w?.Data ?? Array.Empty<byte>();
            int length = w?.Length ?? 0;
            int code = IrohNative.RequestReject(_manager.Handle, _request, payload, (UIntPtr)length);
            if (code != IrohNative.Ok && code != IrohNative.ErrNoHandle)
            {
                BNL.LogWarning($"iroh reject failed: {IrohNative.LastError()}");
            }
        }
    }

    /// <summary>
    /// The iroh network stack for C# clients. In normal mode a background thread drains the
    /// native event queue and raises the listener's events from it; in manual mode
    /// <see cref="PollEvents"/> does the same on the caller's thread.
    /// </summary>
    public sealed class IrohNetManager : NetManager
    {
        private readonly EventBasedNetListener _listener;
        private readonly ConcurrentDictionary<ulong, IrohNetPeer> _peers = new ConcurrentDictionary<ulong, IrohNetPeer>();
        private readonly object _pollLock = new object();
        private byte[] _pollBuffer = new byte[64 * 1024];
        private Thread _pumpThread;
        private volatile bool _pumping;

        internal ulong Handle { get; private set; }

        public IrohNetManager(EventBasedNetListener listener, Configuration configuration)
        {
            IrohNative.EnsureAbi();
            _listener = listener ?? throw new ArgumentNullException(nameof(listener));
            Handle = IrohNative.ManagerCreate(configuration != null && configuration.EnableStatistics ? 1 : 0);
            if (Handle == 0) throw new IrohTransportException(IrohNative.LastError());
        }

        /// <summary>Sets one iroh transport setting by its XML element name, for managers created afterwards.</summary>
        public static void SetTransportSetting(string name, string value)
        {
            if (IrohNative.SetTransportSetting(name, value) != IrohNative.Ok) throw new IrohTransportException(IrohNative.LastError());
        }

        /// <summary><c>&lt;endpoint-id&gt;@host:port</c> — what another client passes to connect to this endpoint.</summary>
        public string ConnectionString => Handle == 0 ? string.Empty : IrohNative.ReadString((buf, cap) => IrohNative.ManagerConnectionString(Handle, buf, cap));

        /// <summary>This endpoint's full iroh address as JSON bytes, the payload of a P2P introduce request.</summary>
        public byte[] EndpointAddrJson => Handle == 0 ? Array.Empty<byte>() : Encoding.UTF8.GetBytes(IrohNative.ReadString((buf, cap) => IrohNative.ManagerEndpointAddrJson(Handle, buf, cap)));

        /// <summary>Turns another endpoint's address JSON into the connection string <see cref="Connect"/> takes.</summary>
        public static string ConnectionStringFromEndpointAddr(byte[] json)
        {
            if (json == null || json.Length == 0) throw new ArgumentException("endpoint address is empty", nameof(json));
            return IrohNative.ReadString((buf, cap) => IrohNative.EndpointAddrToConnectionString(json, (UIntPtr)json.Length, buf, cap));
        }

        public void Start(IPAddress IPv4Address, IPAddress IPv6Address, int SetPort)
        {
            StartManual(IPv4Address, IPv6Address, SetPort);
            _pumping = true;
            _pumpThread = new Thread(PumpLoop) { Name = "IrohNetManagerPump", IsBackground = true };
            _pumpThread.Start();
        }

        public void StartManual(IPAddress IPv4Address, IPAddress IPv6Address, int SetPort)
        {
            if (Handle == 0) throw new IrohTransportException("the transport has been stopped");
            string v4 = IPv4Address == null || IPv4Address.Equals(IPAddress.Any) ? string.Empty : IPv4Address.ToString();
            string v6 = IPv6Address == null || IPv6Address.Equals(IPAddress.IPv6Any) ? string.Empty : IPv6Address.ToString();
            int code = IrohNative.ManagerStart(Handle, v4, v6, (ushort)SetPort);
            if (code != IrohNative.Ok) throw new IrohTransportException(IrohNative.LastError());
        }

        private void PumpLoop()
        {
            while (_pumping)
            {
                if (!PollEvents()) Thread.Sleep(1);
            }
        }

        /// <summary>Drains every queued transport event into the listener. Returns whether any were raised.</summary>
        public bool PollEvents()
        {
            bool any = false;
            lock (_pollLock)
            {
                while (Handle != 0)
                {
                    int code = IrohNative.ManagerPoll(Handle, out IrohNative.Event ev, _pollBuffer, (UIntPtr)_pollBuffer.Length);
                    if (code == 0) break;
                    if (code == IrohNative.ErrBufferTooSmall)
                    {
                        _pollBuffer = new byte[Math.Max(ev.DataLen, _pollBuffer.Length * 2)];
                        continue;
                    }
                    if (code < 0)
                    {
                        BNL.LogError($"iroh poll failed: {IrohNative.LastError()}");
                        break;
                    }
                    any = true;
                    byte[] data = new byte[ev.DataLen];
                    Buffer.BlockCopy(_pollBuffer, 0, data, 0, (int)ev.DataLen);
                    Dispatch(ev, data);
                }
            }
            return any;
        }

        private void Dispatch(IrohNative.Event ev, byte[] data)
        {
            try
            {
                switch (ev.Kind)
                {
                    case IrohNative.EventPeerConnected:
                        _listener.RaisePeerConnected(PeerFor(ev.Peer));
                        break;
                    case IrohNative.EventPeerDisconnected:
                        IrohNetPeer gone = PeerFor(ev.Peer);
                        _listener.RaisePeerDisconnected(gone, new DisconnectInfo
                        {
                            Reason = (DisconnectReason)ev.Reason,
                            SocketErrorCode = (SocketError)ev.SocketError,
                            AdditionalData = NetPacketReader.Create(data, 0, data.Length, null),
                        });
                        _peers.TryRemove(ev.Peer, out _);
                        break;
                    case IrohNative.EventReceive:
                        _listener.RaiseNetworkReceive(PeerFor(ev.Peer), NetPacketReader.Create(data, 0, data.Length, null), ev.Channel, (DeliveryMethod)ev.Delivery);
                        break;
                    case IrohNative.EventConnectionRequest:
                        _listener.RaiseConnectionRequest(new IrohConnectionRequest(this, ev.Request, data, ev.RemoteEndPoint()));
                        break;
                    case IrohNative.EventNetworkError:
                        _listener.RaiseNetworkError(ev.RemoteEndPoint(), (SocketError)ev.SocketError);
                        break;
                    case IrohNative.EventReceiveUnconnected:
                        _listener.RaiseNetworkReceiveUnconnected(ev.RemoteEndPoint(), NetPacketReader.Create(data, 0, data.Length, null));
                        break;
                }
            }
            catch (Exception ex)
            {
                BNL.LogError($"iroh event handler threw: {ex}");
            }
        }

        internal IrohNetPeer PeerFor(ulong handle) => _peers.GetOrAdd(handle, h => new IrohNetPeer(this, h));

        public void ManualUpdate(float elapsedMilliseconds)
        {
            // The transport runs its own clock; there is no ack/ping/flush step to drive.
        }

        public void Stop()
        {
            _pumping = false;
            Thread pump = _pumpThread;
            _pumpThread = null;
            if (pump != null && pump != Thread.CurrentThread) pump.Join(2000);

            ulong handle = Handle;
            Handle = 0;
            if (handle != 0) IrohNative.ManagerDestroy(handle);
            _peers.Clear();
        }

        public NetPeer Connect(string sIP, int port, NetDataWriter Writer)
        {
            if (Handle == 0) throw new IrohTransportException("the transport has been stopped");
            byte[] payload = Writer?.Data ?? Array.Empty<byte>();
            int length = Writer?.Length ?? 0;
            int code = IrohNative.ManagerConnect(Handle, sIP ?? string.Empty, (ushort)Math.Clamp(port, 0, ushort.MaxValue), payload, (UIntPtr)length, out ulong peer);
            if (code != IrohNative.Ok) throw new IrohTransportException(IrohNative.LastError());
            return PeerFor(peer);
        }

        public bool SendUnconnectedMessage(NetDataWriter writer, IPEndPoint remoteEndPoint)
        {
            // iroh has no unconnected datagrams; the server-info probe for this stack is the REST health check.
            return false;
        }

        public NetStatistics Statistics
        {
            get
            {
                NetStatistics stats = new NetStatistics();
                if (Handle != 0 && IrohNative.ManagerStatistics(Handle, out IrohNative.Statistics s) == IrohNative.Ok)
                {
                    stats.PacketsSent = (long)s.PacketsSent;
                    stats.PacketsReceived = (long)s.PacketsReceived;
                    stats.BytesSent = (long)s.BytesSent;
                    stats.BytesReceived = (long)s.BytesReceived;
                    stats.PacketLoss = (long)s.PacketLoss;
                }
                return stats;
            }
        }

        public int ConnectedPeersCount => Handle == 0 ? 0 : Math.Max(0, IrohNative.ManagerConnectedPeers(Handle));

        public long UnreliableDropped => Handle != 0 && IrohNative.ManagerStatistics(Handle, out IrohNative.Statistics s) == IrohNative.Ok ? s.UnreliableDropped : 0;

        public long PriorityUnreliableDropped => Handle != 0 && IrohNative.ManagerStatistics(Handle, out IrohNative.Statistics s) == IrohNative.Ok ? s.PriorityUnreliableDropped : 0;
    }
}
