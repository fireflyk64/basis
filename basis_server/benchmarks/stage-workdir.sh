#!/usr/bin/env bash
# Builds the benchmark work directory: private copies of both servers and both load clients,
# each booted once so it writes its default config, which is what the harness patches.
#
#   stage-workdir.sh <workdir>
set -euo pipefail
WORK=${1:?usage: stage-workdir.sh <workdir>}
: "${DOTNET_ROOT:=$HOME/.dotnet}"
export DOTNET_ROOT DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
CS="$REPO/Basis Server"
RS="$REPO/basis_server/target/release"

rm -rf "$WORK/servers" "$WORK/clients"
mkdir -p "$WORK/servers/rust" "$WORK/clients/rust" "$WORK/results"
cp -r "$CS/BasisServerConsole/bin/Release/net10.0" "$WORK/servers/csharp"
rm -rf "$WORK/servers/csharp/config" "$WORK/servers/csharp/logs"
cp -r "$CS/BasisNetworkClientConsole/BasisNetworkClientConsole/bin/Release/net10.0" "$WORK/clients/csharp"
rm -f "$WORK/clients/csharp/ClientSimConfig.xml"
# The harness launches whatever is called BasisNetworkConsole in the server directory; a copy
# rather than a link, because the Rust console keeps its config beside its real path.
cp "$RS/basis_network_console" "$WORK/servers/rust/BasisNetworkConsole"
cp "$RS/basis_network_client_console" "$WORK/clients/rust/"

for d in rust csharp; do
  (cd "$WORK/servers/$d" && timeout 30 ./BasisNetworkConsole < /dev/null > "$WORK/first-boot-server-$d.log" 2>&1 || true)
  [ -f "$WORK/servers/$d/config/config.xml" ] || { echo "the $d server wrote no config; see $WORK/first-boot-server-$d.log" >&2; exit 1; }
done
(cd "$WORK/clients/rust" && timeout 6 ./basis_network_client_console < /dev/null > "$WORK/first-boot-client-rust.log" 2>&1 || true)
(cd "$WORK/clients/csharp" && timeout 8 ./BasisNetworkClientConsole < /dev/null > "$WORK/first-boot-client-csharp.log" 2>&1 || true)
for d in rust csharp; do
  [ -f "$WORK/clients/$d/ClientSimConfig.xml" ] || { echo "the $d load client wrote no ClientSimConfig.xml; see $WORK/first-boot-client-$d.log" >&2; exit 1; }
done
echo "staged in $WORK"
