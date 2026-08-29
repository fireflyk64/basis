using Basis.Network.Core;

namespace Basis.HelloWorld
{
    /// <summary>
    /// Hello world for the Basis network: a ring of clients passing a number around, each one
    /// adding 1 and handing it to its neighbour, so every hop is a real UDP round trip through a
    /// real server.
    ///
    /// <code>
    ///   dotnet run --project BasisHelloWorldClient -- --ip 127.0.0.1 --port 4296 --password default_password
    /// </code>
    ///
    /// The password has to match the running server's <c>Password</c> in config.xml.
    ///
    /// <para><c>--ip</c> takes a host or, on the iroh stack, a connection string
    /// (<c>&lt;endpoint-id&gt;[@host:port]</c>) — the server prints its own at boot. The clients
    /// reach the Rust server through the basis_iroh_ffi native library beside the executable;
    /// <c>--stack litenetlib</c> talks to the legacy C# server instead.</para>
    ///
    /// <para>Add <c>--direct</c> and the ring runs over direct peer-to-peer links instead: the
    /// server introduces each neighbour pair by endpoint address and then carries none of the
    /// traffic. Each hop prints the path it actually took, so a link that failed to come up shows
    /// up as a relayed hop rather than as silence.</para>
    /// </summary>
    public static class Program
    {
        public static int Main(string[] args)
        {
            string ip = Arg(args, "--ip", "127.0.0.1");
            int port = int.Parse(Arg(args, "--port", "4296"));
            string password = Arg(args, "--password", "default_password");
            int clientCount = Math.Max(2, int.Parse(Arg(args, "--clients", "2")));
            int hops = Math.Max(1, int.Parse(Arg(args, "--hops", "10")));
            bool direct = Array.IndexOf(args, "--direct") >= 0;
            BasisHelloClient.NetworkStackId = Arg(args, "--stack", BasisNetworkStackRegistry.IrohId);

            Console.WriteLine($"Connecting {clientCount} clients to {ip}:{port} for a {hops}-hop volley{(direct ? " over direct links" : "")}.");

            var clients = new BasisHelloClient[clientCount];
            var finished = new ManualResetEventSlim(false);

            try
            {
                for (int i = 0; i < clientCount; i++)
                {
                    clients[i] = direct ? new HelloPeerClient($"Hello{i}") : new BasisHelloClient($"Hello{i}");
                    if (!clients[i].Connect(ip, port, password, TimeSpan.FromSeconds(15)))
                    {
                        Console.Error.WriteLine($"Client {i} could not join {ip}:{port}. Is the server running, and is --password right?");
                        return 1;
                    }
                    Console.WriteLine($"  Hello{i} joined as player {clients[i].PlayerId}");
                }

                // Every client does the same thing: take the number, add one, pass it on. The ring
                // is set up after all of them have joined, because a neighbour's player id is only
                // known once the neighbour is in.
                for (int i = 0; i < clientCount; i++)
                {
                    BasisHelloClient self = clients[i];
                    BasisHelloClient next = clients[(i + 1) % clientCount];

                    self.NumberReceived += (senderId, value, path) =>
                    {
                        Console.WriteLine($"  {self.DisplayName} (player {self.PlayerId}) got {value} from player {senderId} via {path}");
                        if (value >= hops)
                        {
                            finished.Set();
                            return;
                        }
                        Pass(self, next.PlayerId, value + 1);
                    };
                }

                // One link per ring edge, opened before the volley starts. A link that does not come
                // up is not fatal: the send below falls back to the server, and the printed path
                // says so on every hop.
                if (direct)
                {
                    for (int i = 0; i < clientCount; i++)
                    {
                        var self = (HelloPeerClient)clients[i];
                        ushort neighbour = clients[(i + 1) % clientCount].PlayerId;
                        bool up = self.OpenDirectLink(neighbour, TimeSpan.FromSeconds(20));
                        Console.WriteLine($"  {self.DisplayName} -> player {neighbour}: {(up ? $"direct link up (own endpoint {self.DirectEndpoint})" : "no direct link, will relay")}");
                    }
                }

                Console.WriteLine($"{clients[0].DisplayName} starts the volley with 1.");
                Pass(clients[0], clients[1].PlayerId, 1);

                if (!finished.Wait(TimeSpan.FromSeconds(30)))
                {
                    Console.Error.WriteLine("The volley did not reach the end within 30s.");
                    return 1;
                }

                Console.WriteLine($"Done: the number went round the ring and reached {hops}.");
                return 0;
            }
            finally
            {
                foreach (BasisHelloClient? client in clients)
                {
                    client?.Dispose();
                }
            }
        }

        /// <summary>Hands the number on, over a direct link when the client has one.</summary>
        private static void Pass(BasisHelloClient from, ushort targetPlayerId, int value)
        {
            if (from is HelloPeerClient peer) peer.SendNumberDirect(targetPlayerId, value);
            else from.SendNumber(targetPlayerId, value);
        }

        private static string Arg(string[] args, string name, string fallback)
        {
            for (int i = 0; i < args.Length - 1; i++)
            {
                if (args[i] == name) return args[i + 1];
            }
            return fallback;
        }
    }
}
