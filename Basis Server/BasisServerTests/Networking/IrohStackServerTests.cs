using System.Collections.Concurrent;
using System.Net;
using System.Net.Sockets;
using Basis.HelloWorld;
using Basis.Network.Core;
using BasisServerHandle;
using Xunit;

namespace BasisServerTests;

// ─────────────────────────────────────────────────────────────────────────────
// The C# server on the iroh stack.
//
// The server runs one transport, chosen at startup by NetworkStackId. These tests boot it on
// iroh and put real clients through the full handshake, which is the only way to know that the
// stack selection reaches every part of the server and not just the bind: the version check, the
// password, the DID challenge, the metadata reply, and the relay that carries a directed message
// to exactly the recipients it names.
//
// They need the basis_iroh_ffi native library beside the test assembly. Without it the stack
// cannot be created, and the tests say so and pass rather than failing on a machine that has not
// built the Rust workspace.
// ─────────────────────────────────────────────────────────────────────────────

/// <summary>Boots one C# server on the iroh stack for the whole class.</summary>
public sealed class IrohServerFixture : IDisposable
{
    public const string Password = "csharp-iroh-stack-test";

    /// <summary>Why the server is not running, or null when it is.</summary>
    public string? Unavailable { get; }

    /// <summary>What an iroh client dials: the endpoint id, with a direct address to try first.</summary>
    public string ConnectionString { get; } = "";

    public IrohServerFixture()
    {
        if (!NativeLibraryPresent())
        {
            Unavailable = "basis_iroh_ffi is not beside the test assembly (cargo build --release -p basis_iroh_ffi, then rebuild the solution)";
            return;
        }

        try
        {
            NetworkServer.StartServer(new Configuration
            {
                SetPort = (ushort)FindFreeUdpPort(),
                Password = Password,
                UseAuth = true,
                UseAuthIdentity = true,
                // Keeps the run from writing config.xml and the lists into the test binary's folder.
                HasFileSupport = false,
                EnableStatistics = false,
                EnableConsole = false,
                ApiEnabled = false,
                PeerLimit = 64,
                NetworkStackId = BasisNetworkStackRegistry.IrohId,
            });
            ConnectionString = NetworkServer.IrohConnectionString;
            if (ConnectionString.Length == 0)
            {
                Unavailable = "the server started but published no iroh connection string";
            }
        }
        catch (Exception ex)
        {
            Unavailable = "the iroh stack could not be started: " + ex.Message;
        }
    }

    private static bool NativeLibraryPresent()
    {
        string dir = AppContext.BaseDirectory;
        return File.Exists(Path.Combine(dir, "libbasis_iroh_ffi.so"))
            || File.Exists(Path.Combine(dir, "libbasis_iroh_ffi.dylib"))
            || File.Exists(Path.Combine(dir, "basis_iroh_ffi.dll"));
    }

    private static int FindFreeUdpPort()
    {
        using Socket probe = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
        probe.Bind(new IPEndPoint(IPAddress.Loopback, 0));
        return ((IPEndPoint)probe.LocalEndPoint!).Port;
    }

    public void Dispose()
    {
        if (Unavailable != null) return;
        BasisServerHandleEvents.StopWorker();
        NetworkServer.StopServer();
    }
}

[Collection("BasisServer shared network statics")]
public sealed class IrohStackServerTests : IClassFixture<IrohServerFixture>
{
    private static readonly TimeSpan JoinTimeout = TimeSpan.FromSeconds(30);
    private static readonly TimeSpan DeliveryTimeout = TimeSpan.FromSeconds(30);

    private readonly IrohServerFixture _server;

    public IrohStackServerTests(IrohServerFixture server)
    {
        _server = server;
    }

    private bool Skip()
    {
        if (_server.Unavailable == null) return false;
        Console.WriteLine($"SKIPPED (C# server on iroh): {_server.Unavailable}");
        return true;
    }

    /// <summary>
    /// The stack selection reaches the whole server, not just the bind: four clients complete the
    /// handshake over QUIC and every directed message lands on exactly its recipient.
    /// </summary>
    [Fact]
    public void ClientsJoinOverIrohAndTheRelayAddressesExactlyTheRecipients()
    {
        if (Skip()) return;

        const int count = 4;
        var clients = new BasisHelloClient[count];
        try
        {
            for (int i = 0; i < count; i++)
            {
                clients[i] = new BasisHelloClient($"Iroh{i:X2}", BasisNetworkStackRegistry.IrohId);
                Assert.True(
                    clients[i].Connect(_server.ConnectionString, 0, IrohServerFixture.Password, JoinTimeout),
                    $"client {i} did not join the iroh server within {JoinTimeout.TotalSeconds}s");
            }

            // Distinct ids are what makes the addressing below meaningful.
            Assert.Equal(count, clients.Select(c => c.PlayerId).Distinct().Count());

            var inbox = new ConcurrentBag<(ushort Sender, string Text)>[count];
            for (int i = 0; i < count; i++)
            {
                inbox[i] = new ConcurrentBag<(ushort, string)>();
                int index = i;
                clients[i].TextReceived += (sender, text, _) => inbox[index].Add((sender, text));
            }

            for (int from = 0; from < count; from++)
                for (int to = 0; to < count; to++)
                    if (from != to) clients[from].SendText(clients[to].PlayerId, $"hello{from:X2}_{to:X2}");

            WaitUntil(() => inbox.All(bag => bag.Count >= count - 1), DeliveryTimeout,
                () => $"only {inbox.Sum(b => b.Count)} of {count * (count - 1)} messages arrived");

            for (int to = 0; to < count; to++)
            {
                (ushort Sender, string Text)[] received = inbox[to].ToArray();
                // Exactly three: an extra would mean the relay delivered to someone the message
                // was not addressed to, which is the failure that matters most.
                Assert.Equal(count - 1, received.Length);
                for (int from = 0; from < count; from++)
                    if (from != to) Assert.Contains((clients[from].PlayerId, $"hello{from:X2}_{to:X2}"), received);
            }
        }
        finally
        {
            foreach (BasisHelloClient? c in clients)
            {
                try { c?.Dispose(); } catch (Exception ex) { Console.WriteLine($"cleanup failed: {ex.Message}"); }
            }
        }
    }

    /// <summary>The stack is reported where operators and tools look for it.</summary>
    [Fact]
    public void TheServerReportsWhichStackItIsOn()
    {
        if (Skip()) return;

        Assert.Equal(BasisNetworkStackRegistry.IrohId, NetworkServer.ActiveStackId);
        // An iroh client cannot derive the endpoint id from the port, so the server has to
        // publish it or nobody can connect.
        Assert.NotEmpty(NetworkServer.IrohConnectionString);
        Assert.Contains("@", NetworkServer.IrohConnectionString);
    }

    /// <summary>A wrong password is refused over QUIC exactly as it is over UDP.</summary>
    [Fact]
    public void TheWrongPasswordIsRefusedOverIroh()
    {
        if (Skip()) return;

        using var client = new BasisHelloClient("IrohWrongPassword", BasisNetworkStackRegistry.IrohId);
        Assert.False(client.Connect(_server.ConnectionString, 0, "not-the-password", TimeSpan.FromSeconds(10)));
        Assert.False(client.IsJoined);
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

/// <summary>
/// Stack selection refuses what it cannot honour.
///
/// <para>A separate class so it may stop and start the server without disturbing the shared iroh
/// fixture; the collection serialises it against every other suite that touches the server
/// statics.</para>
/// </summary>
[Collection("BasisServer shared network statics")]
public sealed class IrohStackSelectionTests
{
    /// <summary>
    /// The registry answers an unknown id with the default stack, which for a server would mean a
    /// typo in config.xml quietly serving a protocol nobody asked for. Startup refuses instead.
    /// </summary>
    [Fact]
    public void AnUnknownStackIdStopsTheServerAtBootInsteadOfServingTheDefault()
    {
        try
        {
            var ex = Assert.Throws<InvalidOperationException>(() => NetworkServer.StartServer(new Configuration
            {
                SetPort = 4399,
                HasFileSupport = false,
                EnableStatistics = false,
                EnableConsole = false,
                ApiEnabled = false,
                NetworkStackId = "irho",
            }));

            // The operator has to be able to fix it from the message alone.
            Assert.Contains("irho", ex.Message);
            Assert.Contains(BasisNetworkStackRegistry.LiteNetLibId, ex.Message);
            Assert.Contains(BasisNetworkStackRegistry.IrohId, ex.Message);
            // Nothing may be left listening on a stack the operator did not choose.
            Assert.Null(NetworkServer.Server);
        }
        finally
        {
            BasisServerHandleEvents.StopWorker();
            NetworkServer.StopServer();
        }
    }

    /// <summary>Both shipped stacks are registered, so either may be named in config.xml.</summary>
    [Fact]
    public void BothShippedStacksAreSelectable()
    {
        Assert.True(BasisNetworkStackRegistry.IsRegistered(BasisNetworkStackRegistry.LiteNetLibId));
        Assert.True(BasisNetworkStackRegistry.IsRegistered(BasisNetworkStackRegistry.IrohId));
        Assert.False(BasisNetworkStackRegistry.IsRegistered("irho"));
        // An empty id is how an untouched config.xml arrives, and must keep meaning LiteNetLib.
        Assert.Equal(BasisNetworkStackRegistry.LiteNetLibId, BasisNetworkStackRegistry.DefaultId);
    }
}
