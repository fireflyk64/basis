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

            Console.WriteLine($"Connecting {clientCount} clients to {ip}:{port} for a {hops}-hop volley.");

            var clients = new BasisHelloClient[clientCount];
            var finished = new ManualResetEventSlim(false);

            try
            {
                for (int i = 0; i < clientCount; i++)
                {
                    clients[i] = new BasisHelloClient($"Hello{i}");
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

                    self.NumberReceived += (senderId, value) =>
                    {
                        Console.WriteLine($"  {self.DisplayName} (player {self.PlayerId}) got {value} from player {senderId}");
                        if (value >= hops)
                        {
                            finished.Set();
                            return;
                        }
                        self.SendNumber(next.PlayerId, value + 1);
                    };
                }

                Console.WriteLine($"{clients[0].DisplayName} starts the volley with 1.");
                clients[0].SendNumber(clients[1].PlayerId, 1);

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
