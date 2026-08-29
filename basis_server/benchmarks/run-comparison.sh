#!/usr/bin/env bash
# Measures the C# server and the Rust server with the SAME harness and the SAME crowd.
#
# The crowd is the C# load client (BasisNetworkClientConsole): real LiteNetLib clients, the
# ones every deployed Basis install runs today. The harness is the C# BasisServerBenchmark,
# unmodified in how it measures; the Rust server is dropped into a server directory under the
# name the harness expects (BasisNetworkConsole) and serves the same /health document.
#
#   run-comparison.sh <workdir> <players> [players...]
#
# <workdir> holds servers/{csharp,rust} and clients/{csharp,rust}, each already booted once so
# their configs exist (see stage-workdir.sh). Results land in <workdir>/results/<run>/.
#
# Environment:
#   BENCH_SERVERS   which servers to measure, default "csharp rust"
#   BENCH_MIX       legacy share of the crowd, e.g. 0.5 seats half the crowd over iroh from
#                   clients/rust (Rust server only; the C# server has no iroh listener)
#   BENCH_TWO_CORE  "on" (default) pins the server to core 0 and the crowd to core 1
#   BENCH_EXTRA     extra /set lines, e.g. $'/set windows 6\n/set window-sec 20'
set -euo pipefail

WORK=${1:?usage: run-comparison.sh <workdir> <players> [players...]}
shift
POPULATIONS=("$@")
if [ ${#POPULATIONS[@]} -eq 0 ]; then
  echo "give at least one population" >&2
  exit 2
fi

: "${DOTNET_ROOT:=$HOME/.dotnet}"
export DOTNET_ROOT DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
DOTNET="$DOTNET_ROOT/dotnet"
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
BENCH_DLL="$REPO/Basis Server/BasisServerBenchmark/bin/Release/net10.0/BasisServerBenchmark.dll"
[ -f "$BENCH_DLL" ] || { echo "build the C# solution first: $BENCH_DLL is missing" >&2; exit 2; }

SERVERS=${BENCH_SERVERS:-"csharp rust"}
MIX=${BENCH_MIX:-1}
TWO_CORE=${BENCH_TWO_CORE:-on}
RUN=$(date -u +%Y%m%dT%H%M%SZ)
OUT="$WORK/results/$RUN"
mkdir -p "$OUT"

for server in $SERVERS; do
  for players in "${POPULATIONS[@]}"; do
    label="$server-legacy"
    args=(--server "$WORK/servers/$server" --client "$WORK/clients/csharp" --out "$OUT/$server")
    if [ "$TWO_CORE" = on ]; then args+=(--two-core); fi
    if [ "$MIX" != 1 ]; then
      label="$server-mix$MIX"
      args+=(--modern-client "$WORK/clients/rust" --mix "$MIX")
    fi
    log="$OUT/$label-$players.log"
    echo "=== $label at $players players -> $log"
    {
      if [ -n "${BENCH_EXTRA:-}" ]; then printf '%s\n' "$BENCH_EXTRA"; fi
      printf '/show\n/measure %s\n/quit\n' "$players"
    } | "$DOTNET" "$BENCH_DLL" "${args[@]}" 2>&1 | tee "$log" | grep -E "RESULT|window [0-9]+/|Run failed|connected;|starting" || true
  done
done

echo
echo "Summary:"
python3 "$HERE/summarize.py" "$OUT"
