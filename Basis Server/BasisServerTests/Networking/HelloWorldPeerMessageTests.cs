using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using Basis.HelloWorld;
using BasisServerHandle;
using Xunit;

namespace BasisServerTests;

// ─────────────────────────────────────────────────────────────────────────────
// End-to-end peer messaging over a real server and real UDP sockets.
//
// Unlike the rest of the networking suite — which drives BasisServerHandleEvents
// synchronously through interface fakes — this boots NetworkServer for real on a
// loopback port and joins sixteen BasisHelloClients through the full handshake:
// version check, password, DID challenge/response, and the metadata reply that
// admits the peer. What it proves is the thing no fake can: that sixteen clients
// can hold a full mesh of directed conversations through one server at once.
//
// Runs in the shared-network-statics collection because NetworkServer's state is
// process-wide.
// ─────────────────────────────────────────────────────────────────────────────

/// <summary>
/// Boots one real server for the whole class. Started once because a boot binds sockets, starts
/// the join-broadcast worker and rebuilds the auth identity — none of it worth paying per test.
/// </summary>
public sealed class HelloWorldServerFixture : IDisposable
{
    public const string Password = "hello-world-integration-test";

    public int Port { get; }

    public HelloWorldServerFixture()
    {
        Port = FindFreeUdpPort();

        NetworkServer.StartServer(new Configuration
        {
            SetPort = (ushort)Port,
            Password = Password,
            UseAuth = true,
            UseAuthIdentity = true,
            // Keeps the run from writing config.xml, permissions.xml and the allow/ban lists into
            // the test binary's folder. Everything the test needs lives in memory.
            HasFileSupport = false,
            EnableStatistics = false,
            EnableConsole = false,
            ApiEnabled = false,
            PeerLimit = 64,
        });
    }

    public void Dispose()
    {
        // StopWorker first: it unsubscribes the handlers, which reads NetworkServer.Listener —
        // and StopServer nulls it. Neither touches BasisServerReductionSystemEvents, whose tick
        // loop is started from a static constructor and cannot be restarted once shut down.
        BasisServerHandleEvents.StopWorker();
        NetworkServer.StopServer();
    }

    /// <summary>
    /// Asks the OS for a port nobody is using. Binding to port 0 and reading back what was
    /// assigned is the only way to get one that is genuinely free; a hard-coded port collides with
    /// whatever else is on the build machine, including a second copy of this suite.
    /// </summary>
    private static int FindFreeUdpPort()
    {
        using Socket probe = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        probe.Bind(new IPEndPoint(IPAddress.Loopback, 0));
        return ((IPEndPoint)probe.LocalEndPoint!).Port;
    }
}

[Collection("BasisServer shared network statics")]
public sealed class HelloWorldPeerMessageTests : IClassFixture<HelloWorldServerFixture>
{
    private const int ClientCount = 16;
    private static readonly TimeSpan JoinTimeout = TimeSpan.FromSeconds(20);
    private static readonly TimeSpan DeliveryTimeout = TimeSpan.FromSeconds(30);

    private readonly HelloWorldServerFixture _server;

    public HelloWorldPeerMessageTests(HelloWorldServerFixture server)
    {
        _server = server;
    }

    /// <summary>
    /// Sixteen clients join, then every one of them sends a distinct directed message to each of
    /// the other fifteen — 240 messages across a full mesh. Every client must receive exactly the
    /// fifteen addressed to it, from the right sender, and nothing addressed to anyone else.
    /// </summary>
    [Fact]
    public void SixteenClients_ExchangeDirectedMessagesAcrossTheFullMesh()
    {
        BasisHelloClient[] clients = JoinClients(ClientCount);
        try
        {
            // Received text per client, tagged with the sender's player id. A bag rather than a
            // list because the fifteen senders arrive concurrently on each client's pump thread.
            var inbox = new ConcurrentBag<(ushort Sender, string Text)>[ClientCount];
            for (int i = 0; i < ClientCount; i++)
            {
                inbox[i] = new ConcurrentBag<(ushort, string)>();
                int index = i;
                clients[i].TextReceived += (sender, text, _) => inbox[index].Add((sender, text));
            }

            for (int from = 0; from < ClientCount; from++)
            {
                for (int to = 0; to < ClientCount; to++)
                {
                    if (from == to) continue;
                    clients[from].SendText(clients[to].PlayerId, MessageFor(from, to));
                }
            }

            WaitUntil(
                () => inbox.All(bag => bag.Count >= ClientCount - 1),
                DeliveryTimeout,
                () => $"only {inbox.Sum(bag => bag.Count)} of {ClientCount * (ClientCount - 1)} messages arrived " +
                      $"(per client: {string.Join(", ", inbox.Select(bag => bag.Count))})");

            for (int to = 0; to < ClientCount; to++)
            {
                (ushort Sender, string Text)[] received = inbox[to].ToArray();

                // Exactly fifteen: an extra would mean the server relayed a message to someone it
                // was not addressed to, which is the failure that matters most here.
                Assert.Equal(ClientCount - 1, received.Length);

                for (int from = 0; from < ClientCount; from++)
                {
                    if (from == to) continue;
                    ushort senderId = clients[from].PlayerId;
                    Assert.Contains((senderId, MessageFor(from, to)), received);
                }
            }

            // The example from the request, spelled out: client 10 (0x0A) to client 15 (0x0F).
            Assert.Contains(
                (clients[10].PlayerId, "hello0A_0F"),
                inbox[15].ToArray());
        }
        finally
        {
            DisconnectAll(clients);
        }
    }

    /// <summary>
    /// The hello-world behaviour itself: a number passed around a ring of sixteen clients, each
    /// one adding 1 and handing it to its neighbour. Sixteen hops means the volley crosses every
    /// client-to-client edge of the ring and comes back to where it started.
    /// </summary>
    [Fact]
    public void SixteenClients_EchoNumbersAroundTheRing()
    {
        BasisHelloClient[] clients = JoinClients(ClientCount);
        try
        {
            const int FinalValue = ClientCount;
            var hops = new ConcurrentQueue<(int Receiver, ushort Sender, int Value)>();
            var finished = new ManualResetEventSlim(false);

            for (int i = 0; i < ClientCount; i++)
            {
                int index = i;
                BasisHelloClient self = clients[i];
                BasisHelloClient next = clients[(i + 1) % ClientCount];

                self.NumberReceived += (sender, value, _) =>
                {
                    hops.Enqueue((index, sender, value));
                    if (value >= FinalValue) finished.Set();
                    else self.SendNumber(next.PlayerId, value + 1);
                };
            }

            clients[0].SendNumber(clients[1].PlayerId, 1);

            WaitUntil(
                () => finished.IsSet,
                DeliveryTimeout,
                () => $"the volley stopped after {hops.Count} hops: " +
                      $"{string.Join(" -> ", hops.Select(h => $"c{h.Receiver}={h.Value}"))}");

            (int Receiver, ushort Sender, int Value)[] ordered = hops.OrderBy(h => h.Value).ToArray();
            Assert.Equal(FinalValue, ordered.Length);

            for (int hop = 0; hop < ordered.Length; hop++)
            {
                int expectedValue = hop + 1;
                int expectedReceiver = (hop + 1) % ClientCount;
                int expectedSender = hop % ClientCount;

                Assert.Equal(expectedValue, ordered[hop].Value);
                Assert.Equal(expectedReceiver, ordered[hop].Receiver);
                Assert.Equal(clients[expectedSender].PlayerId, ordered[hop].Sender);
            }
        }
        finally
        {
            DisconnectAll(clients);
        }
    }

    /// <summary>The message the request asked for: "hello0A_0F" is client 10 talking to client 15.</summary>
    private static string MessageFor(int from, int to) => $"hello{from:X2}_{to:X2}";

    private BasisHelloClient[] JoinClients(int count)
    {
        var clients = new BasisHelloClient[count];
        try
        {
            for (int i = 0; i < count; i++)
            {
                clients[i] = new BasisHelloClient($"Hello{i:X2}");
                Assert.True(
                    clients[i].Connect(IPAddress.Loopback.ToString(), _server.Port, HelloWorldServerFixture.Password, JoinTimeout),
                    $"client {i} did not join the server on port {_server.Port} within {JoinTimeout.TotalSeconds}s");
            }
        }
        catch
        {
            DisconnectAll(clients);
            throw;
        }

        // Distinct ids are what makes the addressing above meaningful — two clients sharing one id
        // would let a mesh test pass while every message went to the wrong peer.
        ushort[] ids = clients.Select(c => c.PlayerId).ToArray();
        Assert.Equal(count, ids.Distinct().Count());

        return clients;
    }

    private static void DisconnectAll(BasisHelloClient?[] clients)
    {
        foreach (BasisHelloClient? client in clients)
        {
            // Teardown must never mask the failure that brought us here: a client that could not
            // finish connecting may also not be able to finish disconnecting.
            try { client?.Dispose(); }
            catch (Exception ex) { Console.WriteLine($"cleanup of a hello client failed: {ex}"); }
        }
    }

    private static void WaitUntil(Func<bool> condition, TimeSpan timeout, Func<string> describeFailure)
    {
        DateTime deadline = DateTime.UtcNow + timeout;
        while (DateTime.UtcNow < deadline)
        {
            if (condition()) return;
            Thread.Sleep(25);
        }

        Assert.Fail($"Timed out after {timeout.TotalSeconds}s: {describeFailure()}");
    }
}
