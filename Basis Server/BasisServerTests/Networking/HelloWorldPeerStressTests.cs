using System.Net;
using Basis.HelloWorld;
using Xunit;
using Xunit.Abstractions;

namespace BasisServerTests;

// ─────────────────────────────────────────────────────────────────────────────
// Sustained traffic over BOTH paths a Basis client has to a peer at once: the
// server relay, and a direct peer-to-peer link the server only introduces.
//
// The direct path is the reason this suite exists separately. Nothing in
// HelloWorldPeerMessageTests touches it, and it is the path with the most moving
// parts: signalling on the P2P channel, an X25519 key exchange relayed by the
// server, NAT introduce requests from a second socket, the server matching two
// endpoints and telling each side the other's ip address, a simultaneous open,
// and finally the offload handshake after which the server stops relaying
// between the pair.
//
// Sized to stay small on purpose - see ClientCount/Rounds below. A stress test
// that has to be skipped on a modest box tests nothing.
// ─────────────────────────────────────────────────────────────────────────────

[Collection("BasisServer shared network statics")]
public sealed class HelloWorldPeerStressTests : IClassFixture<HelloWorldServerFixture>
{
    /// <summary>
    /// Default population. Every one of these runs two UDP sockets — the server connection and
    /// its direct-link socket — so the footprint per client is roughly double a plain hello
    /// client's, and the pairing below means the direct links scale with it too. Eight is enough
    /// for four independent pairs plus non-partners to exercise the fallback against.
    /// </summary>
    private const int DefaultClientCount = 8;

    /// <summary>Messages per client per path. The run sends 2 x ClientCount x Rounds in total.</summary>
    private const int DefaultRounds = 25;

    /// <summary>
    /// Pacing between rounds. Sends are reliable-ordered, so an unpaced loop just moves the whole
    /// run into LiteNetLib's outgoing queues and measures those instead of the network.
    /// </summary>
    private static readonly TimeSpan RoundPause = TimeSpan.FromMilliseconds(4);

    private static readonly TimeSpan JoinTimeout = TimeSpan.FromSeconds(20);
    private static readonly TimeSpan LinkTimeout = TimeSpan.FromSeconds(25);
    private static readonly TimeSpan DeliveryTimeout = TimeSpan.FromSeconds(45);

    private readonly HelloWorldServerFixture _server;
    private readonly ITestOutputHelper _output;

    public HelloWorldPeerStressTests(HelloWorldServerFixture server, ITestOutputHelper output)
    {
        _server = server;
        _output = output;
    }

    /// <summary>
    /// Counters rather than a log of messages. At the default size a list would be harmless, but
    /// the scale is meant to be turned up, and a collector that grows with the traffic makes the
    /// test the thing that runs out of memory first. A running sum still catches a lost, doubled
    /// or corrupted message, because the expected total is known in advance.
    /// </summary>
    private sealed class Tally
    {
        public long DirectCount;
        public long DirectSum;
        public long RelayCount;
        public long RelaySum;
        public long FallbackCount;
        public long MisroutedFallbacks;   // a fallback that somehow took a direct link
        public long WrongSender;
    }

    [Fact]
    public void PeerClients_SustainTrafficOverDirectLinksAndTheServerAtOnce()
    {
        int clientCount = ReadScale("BASIS_HELLO_STRESS_CLIENTS", DefaultClientCount, min: 4);
        int rounds = ReadScale("BASIS_HELLO_STRESS_ROUNDS", DefaultRounds, min: 1);
        Assert.True(clientCount % 2 == 0, "the pairing below needs an even population");

        long allocatedBefore = GC.GetTotalAllocatedBytes();
        var clients = new HelloPeerClient[clientCount];

        try
        {
            for (int i = 0; i < clientCount; i++)
            {
                clients[i] = new HelloPeerClient($"Peer{i:X2}");
                Assert.True(
                    clients[i].Connect(IPAddress.Loopback.ToString(), _server.Port, HelloWorldServerFixture.Password, JoinTimeout),
                    $"peer client {i} did not join the server on port {_server.Port}");
            }

            ushort[] ids = clients.Select(c => c.PlayerId).ToArray();
            Assert.Equal(clientCount, ids.Distinct().Count());

            // Partner = the other half of an adjacent pair; across = a peer we deliberately never
            // link to, so its traffic has to go through the server; fallback = a third peer, also
            // unlinked, used to prove a "direct" send still lands when there is no link.
            int Partner(int i) => i % 2 == 0 ? i + 1 : i - 1;
            int Across(int i) => (i + clientCount / 2) % clientCount;
            int FallbackTarget(int i) => (i + 3) % clientCount;

            var tally = new Tally[clientCount];
            for (int i = 0; i < clientCount; i++)
            {
                tally[i] = new Tally();
                int index = i;

                clients[i].NumberReceived += (sender, value, transport) =>
                {
                    Tally t = tally[index];
                    if (transport == HelloTransport.DirectLink)
                    {
                        if (sender != ids[Partner(index)]) Interlocked.Increment(ref t.WrongSender);
                        Interlocked.Increment(ref t.DirectCount);
                        Interlocked.Add(ref t.DirectSum, value);
                    }
                    else
                    {
                        if (sender != ids[Across(index)]) Interlocked.Increment(ref t.WrongSender);
                        Interlocked.Increment(ref t.RelayCount);
                        Interlocked.Add(ref t.RelaySum, value);
                    }
                };

                clients[i].TextReceived += (sender, text, transport) =>
                {
                    Tally t = tally[index];
                    Interlocked.Increment(ref t.FallbackCount);
                    if (transport != HelloTransport.ServerRelay) Interlocked.Increment(ref t.MisroutedFallbacks);
                };
            }

            // Only one side of each pair dials; the other accepts. Both ends still connect to the
            // discovered endpoint underneath — that is LiteNetLib's simultaneous open — but the
            // request/accept exchange has an initiator, and having both sides initiate would just
            // race two sessions for the same pair.
            for (int i = 0; i < clientCount; i += 2)
            {
                Assert.True(
                    clients[i].OpenDirectLink(ids[i + 1], LinkTimeout),
                    $"no direct link between peer {i} and peer {i + 1} within {LinkTimeout.TotalSeconds}s");
            }

            for (int i = 0; i < clientCount; i++)
            {
                Assert.True(clients[i].HasDirectLink(ids[Partner(i)]), $"peer {i} has no confirmed link to its partner");
                Assert.False(clients[i].HasDirectLink(ids[Across(i)]), $"peer {i} unexpectedly linked to a non-partner");
            }

            _output.WriteLine($"{clientCount} peers joined; {clientCount / 2} direct links up.");

            // One unlinked "direct" send per client, before the bulk traffic, so the fallback is
            // measured while the direct links are live rather than in a quiet moment.
            for (int i = 0; i < clientCount; i++)
            {
                clients[i].SendTextDirect(ids[FallbackTarget(i)], $"fallback-from-{i:X2}");
            }

            for (int round = 1; round <= rounds; round++)
            {
                for (int i = 0; i < clientCount; i++)
                {
                    clients[i].SendNumberDirect(ids[Partner(i)], round);   // over the direct link
                    clients[i].SendNumber(ids[Across(i)], round);          // through the server
                }
                Thread.Sleep(RoundPause);
            }

            long expectedSum = (long)rounds * (rounds + 1) / 2;

            WaitUntil(
                () => tally.All(t =>
                    Interlocked.Read(ref t.DirectCount) >= rounds &&
                    Interlocked.Read(ref t.RelayCount) >= rounds &&
                    Interlocked.Read(ref t.FallbackCount) >= 1),
                DeliveryTimeout,
                () => "delivery stalled: " + string.Join(", ", tally.Select((t, i) =>
                    $"p{i} direct={Interlocked.Read(ref t.DirectCount)}/{rounds} relay={Interlocked.Read(ref t.RelayCount)}/{rounds} fallback={Interlocked.Read(ref t.FallbackCount)}/1")));

            for (int i = 0; i < clientCount; i++)
            {
                Tally t = tally[i];
                Assert.Equal(0, Interlocked.Read(ref t.WrongSender));
                Assert.Equal(rounds, Interlocked.Read(ref t.DirectCount));
                Assert.Equal(rounds, Interlocked.Read(ref t.RelayCount));
                Assert.Equal(expectedSum, Interlocked.Read(ref t.DirectSum));
                Assert.Equal(expectedSum, Interlocked.Read(ref t.RelaySum));

                // A "direct" send with no link must still arrive, and must arrive relayed. Getting
                // it over a direct link would mean the client linked to someone it never asked to.
                Assert.Equal(1, Interlocked.Read(ref t.FallbackCount));
                Assert.Equal(0, Interlocked.Read(ref t.MisroutedFallbacks));
            }

            long messages = (long)clientCount * (2 * rounds + 1);
            long allocated = GC.GetTotalAllocatedBytes() - allocatedBefore;
            _output.WriteLine(
                $"{messages} messages delivered ({clientCount * rounds} direct, {clientCount * rounds} relayed, {clientCount} fallback); " +
                $"{allocated / 1024 / 1024} MB allocated, heap now {GC.GetTotalMemory(false) / 1024 / 1024} MB.");
        }
        finally
        {
            foreach (HelloPeerClient? client in clients)
            {
                try { client?.Dispose(); }
                catch (Exception ex) { _output.WriteLine($"cleanup of a peer client failed: {ex}"); }
            }
        }
    }

    /// <summary>
    /// Reads a scale override, so the same test can be turned up on a machine that has the room
    /// without committing a size that fails everywhere else.
    /// </summary>
    private static int ReadScale(string variable, int fallback, int min)
    {
        string? raw = Environment.GetEnvironmentVariable(variable);
        if (string.IsNullOrWhiteSpace(raw) || !int.TryParse(raw, out int value)) return fallback;
        return Math.Max(min, value);
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
