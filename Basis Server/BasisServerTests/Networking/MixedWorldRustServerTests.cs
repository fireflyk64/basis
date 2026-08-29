using System.Collections.Concurrent;
using System.Diagnostics;
using System.Net;
using System.Net.Http;
using System.Net.Sockets;
using System.Text.RegularExpressions;
using Basis.HelloWorld;
using Basis.Network.Core;
using Xunit;

namespace BasisServerTests;

// ─────────────────────────────────────────────────────────────────────────────
// The mixed world, seen from C#: the real C# clients against the Rust server.
//
// The Rust server (basis_network_console) is spawned from its release build with a config that
// puts it on the mixed stack — LiteNetLib on one UDP port, iroh on the next — and the C# hello
// clients join it over LiteNetLib exactly as every shipped client would, and over iroh through
// the basis_iroh_ffi native library when it is beside the test assembly. Without a Rust build
// the tests report why and pass vacuously, so the suite stays green on a box without one.
// ─────────────────────────────────────────────────────────────────────────────

/// <summary>Boots the Rust server once for the class from a private copy of its binary.</summary>
public sealed class RustServerFixture : IDisposable
{
    public const string Password = "mixed-world-csharp-test";

    // The boot log line is coloured, so the connection string is taken up to the escape byte;
    // the health document is the authoritative source and is read first.
    private static readonly Regex IrohLine = new(@"iroh clients: ([^\s\x1b]+)", RegexOptions.Compiled);
    private static readonly Regex IrohField = new("\"iroh\":\"([^\"]+)\"", RegexOptions.Compiled);

    private readonly Process? _process;
    private readonly string? _directory;
    private readonly ConcurrentQueue<string> _output = new();

    /// <summary>Why the server is not running, or null when it is.</summary>
    public string? Unavailable { get; }

    /// <summary>UDP port the legacy (LiteNetLib) clients connect to.</summary>
    public int LegacyPort { get; }

    /// <summary>The iroh connection string the server printed at boot, or "" if it never did.</summary>
    public string IrohConnectionString { get; private set; } = "";

    public RustServerFixture()
    {
        string? exe = FindRustServer();
        if (exe == null)
        {
            Unavailable = "no Rust server build (cargo build --release -p basis_network_console; or set BASIS_RUST_SERVER)";
            return;
        }

        try
        {
            _directory = Path.Combine(Path.GetTempPath(), "basis-rust-server-" + Guid.NewGuid().ToString("N"));
            string configDir = Path.Combine(_directory, "config");
            Directory.CreateDirectory(configDir);
            string copied = Path.Combine(_directory, Path.GetFileName(exe));
            File.Copy(exe, copied);
            if (!OperatingSystem.IsWindows())
            {
                File.SetUnixFileMode(copied, File.GetUnixFileMode(copied) | UnixFileMode.UserExecute);
            }

            // Two adjacent free ports: LiteNetLib takes SetPort, iroh takes SetPort + 1.
            LegacyPort = FindAdjacentFreeUdpPorts();
            int healthPort = FindFreeTcpPort();
            // A config on disk is also what skips the first-boot wizard.
            var config = new Configuration
            {
                SetPort = (ushort)LegacyPort,
                Password = Password,
                UseAuth = true,
                UseAuthIdentity = true,
                HasFileSupport = true,
                EnableStatistics = false,
                EnableConsole = false,
                ApiEnabled = false,
                HealthCheckPort = (ushort)healthPort,
                PeerLimit = 64,
                NetworkStackId = "mixed",
            };
            config.SaveToXml(Path.Combine(configDir, "config.xml"));

            var info = new ProcessStartInfo(copied)
            {
                WorkingDirectory = _directory,
                UseShellExecute = false,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            _process = Process.Start(info) ?? throw new InvalidOperationException($"could not start {copied}");
            _process.OutputDataReceived += (_, e) =>
            {
                if (e.Data == null) return;
                _output.Enqueue(e.Data);
                Match m = IrohLine.Match(e.Data);
                if (m.Success) IrohConnectionString = m.Groups[1].Value;
            };
            _process.ErrorDataReceived += (_, e) => { if (e.Data != null) _output.Enqueue("stderr: " + e.Data); };
            _process.BeginOutputReadLine();
            _process.BeginErrorReadLine();

            if (!WaitForHealth($"http://localhost:{healthPort}/health", TimeSpan.FromSeconds(90)))
            {
                Unavailable = "the Rust server never reported healthy:\n" + string.Join("\n", _output);
                Dispose();
            }
        }
        catch (Exception ex)
        {
            Unavailable = "the Rust server could not be started: " + ex.Message;
            Dispose();
        }
    }

    public string Output => string.Join("\n", _output);

    private static string? FindRustServer()
    {
        string? fromEnv = Environment.GetEnvironmentVariable("BASIS_RUST_SERVER");
        if (!string.IsNullOrEmpty(fromEnv) && File.Exists(fromEnv)) return fromEnv;

        // <repo>/Basis Server/BasisServerTests/bin/<Configuration>/net10.0/ → <repo>/basis_server/target/…
        string repo = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "..", "..", "..", "..", ".."));
        foreach (string profile in new[] { "release", "debug" })
        {
            string candidate = Path.Combine(repo, "basis_server", "target", profile, OperatingSystem.IsWindows() ? "basis_network_console.exe" : "basis_network_console");
            if (File.Exists(candidate)) return candidate;
        }
        return null;
    }

    private static int FindAdjacentFreeUdpPorts()
    {
        for (int attempt = 0; attempt < 50; attempt++)
        {
            using var first = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
            first.Bind(new IPEndPoint(IPAddress.Loopback, 0));
            int port = ((IPEndPoint)first.LocalEndPoint!).Port;
            if (port >= ushort.MaxValue) continue;
            try
            {
                using var second = new Socket(AddressFamily.InterNetwork, SocketType.Dgram, ProtocolType.Udp);
                second.Bind(new IPEndPoint(IPAddress.Loopback, port + 1));
                return port;
            }
            catch (SocketException)
            {
                // the neighbour is taken; try another pair
            }
        }
        throw new InvalidOperationException("could not find two adjacent free UDP ports");
    }

    private static int FindFreeTcpPort()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        int port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();
        return port;
    }

    private bool WaitForHealth(string url, TimeSpan timeout)
    {
        using var http = new HttpClient { Timeout = TimeSpan.FromSeconds(2) };
        DateTime deadline = DateTime.UtcNow + timeout;
        while (DateTime.UtcNow < deadline)
        {
            if (_process == null || _process.HasExited) return false;
            try
            {
                string body = http.GetStringAsync(url).GetAwaiter().GetResult();
                Match field = IrohField.Match(body);
                if (field.Success) IrohConnectionString = field.Groups[1].Value;
                if (body.Contains("\"ready\":true")) return true;
            }
            catch
            {
                // not up yet
            }
            Thread.Sleep(500);
        }
        return false;
    }

    public void Dispose()
    {
        try
        {
            if (_process != null && !_process.HasExited)
            {
                _process.Kill(entireProcessTree: true);
                _process.WaitForExit(10000);
            }
        }
        catch { /* already gone */ }
        try { _process?.Dispose(); } catch { }
        try { if (_directory != null) Directory.Delete(_directory, recursive: true); } catch { }
    }
}

[Collection("BasisServer shared network statics")]
public sealed class MixedWorldRustServerTests : IClassFixture<RustServerFixture>
{
    private static readonly TimeSpan JoinTimeout = TimeSpan.FromSeconds(30);
    private static readonly TimeSpan DeliveryTimeout = TimeSpan.FromSeconds(30);

    private readonly RustServerFixture _server;

    public MixedWorldRustServerTests(RustServerFixture server)
    {
        _server = server;
    }

    private bool Skip(string what)
    {
        if (_server.Unavailable == null) return false;
        Console.WriteLine($"SKIPPED ({what}): {_server.Unavailable}");
        return true;
    }

    private BasisHelloClient Join(string name, string stack)
    {
        var client = new BasisHelloClient(name, stack);
        bool joined = stack == BasisNetworkStackRegistry.IrohId
            ? client.Connect(_server.IrohConnectionString, 0, RustServerFixture.Password, JoinTimeout)
            : client.Connect(IPAddress.Loopback.ToString(), _server.LegacyPort, RustServerFixture.Password, JoinTimeout);
        Assert.True(joined, $"{name} ({stack}) did not join the Rust server within {JoinTimeout.TotalSeconds}s:\n{_server.Output}");
        return client;
    }

    private static bool IrohAvailable()
    {
        string dir = AppContext.BaseDirectory;
        return File.Exists(Path.Combine(dir, "libbasis_iroh_ffi.so"))
            || File.Exists(Path.Combine(dir, "libbasis_iroh_ffi.dylib"))
            || File.Exists(Path.Combine(dir, "basis_iroh_ffi.dll"));
    }

    /// <summary>Eight legacy C# clients on the Rust server: the full mesh of directed messages.</summary>
    [Fact]
    public void LegacyClients_ExchangeDirectedMessagesThroughTheRustServer()
    {
        if (Skip("C# LiteNetLib → Rust")) return;
        const int count = 8;
        var clients = new BasisHelloClient[count];
        try
        {
            for (int i = 0; i < count; i++) clients[i] = Join($"Legacy{i:X2}", BasisNetworkStackRegistry.LiteNetLibId);
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
                () => $"only {inbox.Sum(b => b.Count)} of {count * (count - 1)} arrived");
            for (int to = 0; to < count; to++)
            {
                (ushort Sender, string Text)[] received = inbox[to].ToArray();
                Assert.Equal(count - 1, received.Length);
                for (int from = 0; from < count; from++)
                    if (from != to) Assert.Contains((clients[from].PlayerId, $"hello{from:X2}_{to:X2}"), received);
            }
        }
        finally
        {
            foreach (BasisHelloClient? c in clients) c?.Dispose();
        }
    }

    /// <summary>
    /// The mixed world proper: C# clients on LiteNetLib and C# clients on iroh, alternating
    /// round a ring on one Rust server, every hop crossing from one transport to the other.
    /// </summary>
    [Fact]
    public void LegacyAndIrohClients_PassTheNumberAroundOneRing()
    {
        if (Skip("C# mixed → Rust")) return;
        if (!IrohAvailable())
        {
            Console.WriteLine("SKIPPED (C# mixed → Rust): basis_iroh_ffi is not beside the test assembly");
            return;
        }
        if (string.IsNullOrEmpty(_server.IrohConnectionString))
        {
            Assert.Fail("the Rust server never printed its iroh connection string:\n" + _server.Output);
        }

        const int count = 4;
        var clients = new BasisHelloClient[count];
        try
        {
            for (int i = 0; i < count; i++)
            {
                string stack = i % 2 == 0 ? BasisNetworkStackRegistry.LiteNetLibId : BasisNetworkStackRegistry.IrohId;
                clients[i] = Join($"Mixed{i}", stack);
            }
            Assert.Equal(count, clients.Select(c => c.PlayerId).Distinct().Count());

            const int finalValue = 8;
            var hops = new ConcurrentQueue<(int Receiver, ushort Sender, int Value)>();
            var finished = new ManualResetEventSlim(false);
            for (int i = 0; i < count; i++)
            {
                int index = i;
                BasisHelloClient self = clients[i];
                BasisHelloClient next = clients[(i + 1) % count];
                self.NumberReceived += (sender, value, _) =>
                {
                    hops.Enqueue((index, sender, value));
                    if (value >= finalValue) finished.Set();
                    else self.SendNumber(next.PlayerId, value + 1);
                };
            }
            clients[0].SendNumber(clients[1].PlayerId, 1);
            WaitUntil(() => finished.IsSet, DeliveryTimeout, () => $"the volley stopped after {hops.Count} hops");

            (int Receiver, ushort Sender, int Value)[] ordered = hops.OrderBy(h => h.Value).ToArray();
            Assert.Equal(finalValue, ordered.Length);
            for (int hop = 0; hop < ordered.Length; hop++)
            {
                Assert.Equal(hop + 1, ordered[hop].Value);
                Assert.Equal((hop + 1) % count, ordered[hop].Receiver);
                Assert.Equal(clients[hop % count].PlayerId, ordered[hop].Sender);
            }
        }
        finally
        {
            foreach (BasisHelloClient? c in clients) c?.Dispose();
        }
    }

    /// <summary>A legacy client asking for a direct link is declined at once; the send still lands via the relay.</summary>
    [Fact]
    public void LegacyClient_IsNeverOffloadedToADirectLink()
    {
        if (Skip("C# LiteNetLib P2P → Rust")) return;
        using var legacy = new HelloPeerClient("LegacyPeer", BasisNetworkStackRegistry.LiteNetLibId);
        using var other = new HelloPeerClient("OtherLegacyPeer", BasisNetworkStackRegistry.LiteNetLibId);
        Assert.True(legacy.Connect(IPAddress.Loopback.ToString(), _server.LegacyPort, RustServerFixture.Password, JoinTimeout));
        Assert.True(other.Connect(IPAddress.Loopback.ToString(), _server.LegacyPort, RustServerFixture.Password, JoinTimeout));

        var sw = Stopwatch.StartNew();
        Assert.False(legacy.OpenDirectLink(other.PlayerId, TimeSpan.FromSeconds(60)), "a legacy client was offered a direct link");
        Assert.True(sw.Elapsed < TimeSpan.FromSeconds(20), $"the decline took {sw.Elapsed}; it should be immediate");

        var received = new ConcurrentQueue<(ushort Sender, int Value, HelloTransport Path)>();
        other.NumberReceived += (sender, value, path) => received.Enqueue((sender, value, path));
        legacy.SendNumberDirect(other.PlayerId, 41);
        WaitUntil(() => received.Count == 1, DeliveryTimeout, () => "the relayed fallback never arrived");
        Assert.True(received.TryDequeue(out var got));
        Assert.Equal((legacy.PlayerId, 41, HelloTransport.ServerRelay), got);
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
