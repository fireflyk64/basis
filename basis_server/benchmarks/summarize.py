#!/usr/bin/env python3
"""Turns the RESULT lines of a benchmark run into a comparison table.

    summarize.py <results-dir> [--markdown]

Each `<label>-<players>.log` written by run-comparison.sh carries one RESULT line per
/measure; the label names the server and the crowd (csharp-legacy, rust-legacy, rust-mix0.5).
"""
import glob
import os
import re
import sys

FIELDS = [
    ("pairHz", "delivered Hz/pair", "higher"),
    ("delivery", "delivery ratio", "higher"),
    ("serverCores", "server cores", "lower"),
    ("clientCores", "crowd cores", "info"),
    ("mbps", "egress MB/s", "info"),
    ("datagramsPerSec", "datagrams/s", "info"),
    ("dropsPerSec", "avatar drops/s", "lower"),
    ("voiceDropsPerSec", "voice drops/s", "lower"),
    ("slice", "slice count", "lower"),
    ("tickMs", "tick ms", "lower"),
    ("overrun", "overrun ratio", "lower"),
    ("committedMb", "committed MB", "lower"),
    ("voiceHeard", "voice heard", "higher"),
]


def parse(path):
    rows = []
    for line in open(path, encoding="utf-8", errors="replace"):
        line = line.strip()
        if not line.startswith("RESULT "):
            continue
        row = {}
        for token in line[len("RESULT "):].split():
            key, _, value = token.partition("=")
            row[key] = value
        rows.append(row)
    return rows


def number(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    directory = sys.argv[1]
    markdown = "--markdown" in sys.argv
    results = {}
    for path in sorted(glob.glob(os.path.join(directory, "*.log"))):
        name = os.path.basename(path)[:-4]
        match = re.match(r"(.+)-(\d+)$", name)
        if not match:
            continue
        label, players = match.group(1), int(match.group(2))
        for row in parse(path):
            results[(label, players)] = row
    if not results:
        print(f"no RESULT lines under {directory}")
        return 1

    populations = sorted({p for _, p in results})
    labels = sorted({l for l, _ in results})
    for players in populations:
        present = [l for l in labels if (l, players) in results]
        if not present:
            continue
        print()
        print(f"{'=' if not markdown else '###'} {players} players")
        head = ["metric"] + present
        if "csharp-legacy" in present:
            head += [f"{l} / csharp-legacy" for l in present if l != "csharp-legacy"]
        if markdown:
            print("| " + " | ".join(head) + " |")
            print("|" + "|".join("---" for _ in head) + "|")
        else:
            print("  ".join(f"{h:>22}" for h in head))
        for key, title, better in FIELDS:
            cells = [title]
            values = {}
            for label in present:
                v = number(results[(label, players)].get(key))
                values[label] = v
                cells.append("n/a" if v is None else f"{v:.4g}")
            if "csharp-legacy" in present:
                base = values.get("csharp-legacy")
                for label in present:
                    if label == "csharp-legacy":
                        continue
                    v = values.get(label)
                    if base is None or v is None or base == 0:
                        cells.append("-")
                    else:
                        ratio = v / base
                        cells.append(f"{ratio:.3f}x")
            if markdown:
                print("| " + " | ".join(cells) + " |")
            else:
                print("  ".join(f"{c:>22}" for c in cells))
        for label in present:
            row = results[(label, players)]
            print(f"{'' if not markdown else ''}  {label}: {row.get('players')} connected, {row.get('windows')} windows")
    return 0


if __name__ == "__main__":
    sys.exit(main())
