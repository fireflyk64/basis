using System.Globalization;

namespace Basis.Network.Core
{
    /// <summary>
    /// Connection strings for the iroh stack: <c>&lt;endpoint-id&gt;[@host:port][#password]</c>.
    /// The endpoint id is the server's public key (z-base-32); host:port is an optional direct
    /// address hint that skips discovery when the server is reachable there.
    /// </summary>
    public sealed class IrohConnectionTargetParser : IConnectionTargetParser
    {
        public const string EndpointIdKey = "endpointId";

        public void Parse(ConnectionTarget target)
        {
            if (target == null) return;
            if (!TryParseConnectionString(target.Raw, out string endpointId, out string host, out ushort port, out string password)) return;

            target.Set(EndpointIdKey, endpointId);
            target.Set(ConnectionTarget.Keys.Address, host);
            target.Set(ConnectionTarget.Keys.Port, port.ToString(CultureInfo.InvariantCulture));
            target.Set(ConnectionTarget.Keys.Password, password);
        }

        public string Format(ConnectionTarget target)
        {
            if (target == null) return string.Empty;
            string endpointId = target.Get(EndpointIdKey, string.Empty);
            string host = target.Get(ConnectionTarget.Keys.Address, string.Empty);
            string port = target.Get(ConnectionTarget.Keys.Port, "0");
            string password = target.Get(ConnectionTarget.Keys.Password, string.Empty);

            string s = endpointId;
            if (!string.IsNullOrEmpty(host) && port != "0") s += $"@{host}:{port}";
            if (!string.IsNullOrEmpty(password)) s += "#" + password;
            return s;
        }

        public static bool TryParseConnectionString(string raw, out string endpointId, out string host, out ushort port, out string password)
        {
            endpointId = string.Empty;
            host = string.Empty;
            port = 0;
            password = string.Empty;
            if (string.IsNullOrWhiteSpace(raw)) return false;

            string left = raw.Trim();
            int hash = left.IndexOf('#');
            if (hash >= 0)
            {
                password = left.Substring(hash + 1);
                left = left.Substring(0, hash);
            }

            int at = left.IndexOf('@');
            if (at >= 0)
            {
                string address = left.Substring(at + 1);
                left = left.Substring(0, at);
                int colon = address.LastIndexOf(':');
                if (colon > 0 && ushort.TryParse(address.Substring(colon + 1), NumberStyles.Integer, CultureInfo.InvariantCulture, out ushort parsed))
                {
                    host = address.Substring(0, colon).Trim('[', ']');
                    port = parsed;
                }
                else
                {
                    host = address;
                }
            }

            endpointId = left.Trim();
            return endpointId.Length > 0;
        }
    }
}
