namespace Basis.Benchmark.Harness;

/// <summary>Everything one load run needs to know.</summary>
public sealed class RunOptions
{
    public required string ServerDirectory { get; init; }
    public required string LoadClientDirectory { get; init; }
    public required int Players { get; init; }

    /// <summary>
    /// Time between full population and the first timed window.
    ///
    /// <para>Sixty seconds, not thirty, and the difference is not caution. The reduction system's
    /// slicing controller does not settle at high population — it was measured oscillating across
    /// slice 4, 5 and 6 at a fixed 2000-player load with CPU tracking it inversely across a 2.2x
    /// range. A short warmup lands wherever that oscillation happens to be and reports it as the
    /// steady state, which is how one run reported 7.8 cores for a workload that averages
    /// 10.9.</para>
    /// </summary>
    public TimeSpan Warmup { get; init; } = TimeSpan.FromSeconds(60);

    public TimeSpan WindowLength { get; init; } = TimeSpan.FromSeconds(30);

    /// <summary>
    /// How many windows to close. Five is the floor for a comparison to reach a verdict, because
    /// the oscillation above has a period of several windows and fewer can sit entirely inside one
    /// phase of it.
    /// </summary>
    public int Windows { get; init; } = 6;

    /// <summary>How long to wait for every client to finish connecting before giving up.</summary>
    public TimeSpan ConnectTimeout { get; init; } = TimeSpan.FromMinutes(5);

    /// <summary>Settings written into the config files before the server starts.</summary>
    public IReadOnlyDictionary<string, string> Settings { get; init; } = new Dictionary<string, string>();

    public string HealthHost { get; init; } = "localhost";
    public ushort HealthPort { get; init; } = 10666;
    public string HealthPath { get; init; } = "/health";

    /// <summary>Human-readable name for this arm, used in progress output and the report.</summary>
    public string Label { get; init; } = "baseline";

    /// <summary>
    /// Delay the load client leaves between starting each client, or null to leave its own setting
    /// alone.
    ///
    /// <para>0 is the thundering herd — clients start as fast as the loop runs, which is what a
    /// server restart actually produces. The default of 1 ms is a deliberately gentle ramp, and it
    /// is right for the capacity ladder (which wants a steady state to measure, not a join storm)
    /// and exactly wrong for measuring admission.</para>
    /// </summary>
    public int? ClientConnectIntervalMs { get; init; }

    /// <summary>The server's game port, which a remote crowd needs in order to find it.</summary>
    public ushort ServerPort { get; init; } = 4296;

    /// <summary>
    /// Supplies the crowd. Local by default; a remote agent when one was configured.
    ///
    /// Owned by the caller rather than this options object, because one driver holds a control
    /// connection across every arm of a sweep - reconnecting per arm would cost the agent's
    /// clients a teardown each time.
    /// </summary>
    public ILoadClientDriver? Driver { get; init; }

    public string HealthUrl => $"http://{HealthHost}:{HealthPort}{HealthPath}";

    /// <summary>
    /// Two-core mode: the server is pinned to CPU 0 and every load client to CPU 1.
    ///
    /// <para>On a box with only two cores the server and the crowd otherwise time-slice one
    /// another, and a server's CPU figure is then mostly a measure of how often the scheduler
    /// happened to prefer it. Pinning gives each side exactly one core so two servers measured
    /// back to back see the same machine. The absolute numbers are small and say nothing about
    /// a real host; the <em>ratio</em> between two servers under identical pinning is what this
    /// mode is for.</para>
    /// </summary>
    public bool TwoCore { get; init; }

    /// <summary>
    /// Directory holding the Rust load client (<c>basis_network_client_console</c>), which speaks
    /// the iroh stack. Null means the whole crowd is the LiteNetLib load client.
    /// </summary>
    public string? ModernClientDirectory { get; init; }

    /// <summary>
    /// Share of the crowd that joins as legacy LiteNetLib clients, 0..1. The rest join over iroh
    /// through <see cref="ModernClientDirectory"/>. 1 (the default) is the all-legacy crowd every
    /// existing deployment has today.
    /// </summary>
    public double LegacyFraction { get; init; } = 1.0;

    /// <summary>How many of <see cref="Players"/> join as legacy clients.</summary>
    public int LegacyPlayers => ModernClientDirectory == null ? Players : (int)Math.Round(Players * Math.Clamp(LegacyFraction, 0, 1));

    /// <summary>How many join over iroh.</summary>
    public int ModernPlayers => Players - LegacyPlayers;

    /// <summary>Total wall time this run will take if nothing goes wrong.</summary>
    public TimeSpan EstimatedDuration =>
        TimeSpan.FromSeconds(20) +                       // server boot
        TimeSpan.FromSeconds(Players * 0.03) +           // connect ramp, ~30 ms per client
        Warmup +
        WindowLength * Windows +
        TimeSpan.FromSeconds(10);                        // teardown
}
