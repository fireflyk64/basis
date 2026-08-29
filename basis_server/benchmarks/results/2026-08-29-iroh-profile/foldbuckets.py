#!/usr/bin/env python3
"""Bucket a pprof .folded file by component.

Views:
 1. self-time  — each sample attributed to the first (leaf-most) frame matching a bucket
 2. inclusive  — % of samples with the component anywhere in the stack
 3. threads    — sample mass per thread name
 4. leaves     — top leaf symbols (self time), trimmed
"""
import sys, re
from collections import Counter

# Order matters for self-time: first match walking from the leaf wins.
BUCKETS = [
    ("crypto(ring/aead)",  re.compile(r"\bring\b|aead|poly1305|chacha|aes_|::aes|header_protection|GFp_")),
    ("quinn-udp/socket",   re.compile(r"quinn_udp|sendmsg|recvmsg|sendmmsg|recvmmsg|socket")),
    ("quinn-proto",        re.compile(r"quinn_proto")),
    ("quinn-rt",           re.compile(r"\bquinn::")),
    ("iroh-magicsock",     re.compile(r"magicsock|iroh::net|netcheck|portmapper|relay")),
    ("iroh-other",         re.compile(r"\biroh|n0_watcher|n0_error|iroh_base")),
    ("rustls",             re.compile(r"rustls")),
    ("tokio/mio/timers",   re.compile(r"tokio|mio::|\bpark\b|futex|Notify|waker|context::|scheduler")),
    ("alloc/free",         re.compile(r"__rust_alloc|__rust_dealloc|malloc|_int_free|\bfree\b|RawVec|alloc::raw_vec|alloc::alloc")),
    ("memcpy/memset",      re.compile(r"memcpy|memmove|memset|copy_nonoverlapping")),
    ("parking_lot/locks",  re.compile(r"parking_lot|Mutex|RwLock|lock_api")),
    ("basis-transport",    re.compile(r"basis_network_core::transport")),
    ("basis-pool",         re.compile(r"pooling|PacketBufferPool|PooledBytes")),
    ("basis-server/game",  re.compile(r"basis_network_server|basis_network_compute|reduction|server_reduction")),
    ("basis-core-other",   re.compile(r"basis_network_core")),
    ("std/other-rust",     re.compile(r"std::|core::|alloc::")),
]

def main(path):
    lines = []
    with open(path) as f:
        for raw in f:
            raw = raw.rstrip("\n")
            if not raw: continue
            try:
                stack, cnt = raw.rsplit(" ", 1); cnt = int(cnt)
            except ValueError:
                continue
            frames = stack.split(";")
            thread, frames = frames[0], frames[1:]
            lines.append((thread, frames, cnt))
    total = sum(c for _,_,c in lines)
    if not total:
        print(f"{path}: no samples"); return

    print(f"== {path}: {total} samples ==")
    threads = Counter()
    for t,_,c in lines: threads[t] += c
    print("\n-- per thread --")
    for t,c in threads.most_common():
        print(f"  {c:6d}  {100*c/total:5.1f}%  {t}")

    self_b = Counter(); incl_b = Counter()
    leaves = Counter()
    for t, frames, c in lines:
        leaf = frames[-1] if frames else "??"
        leaves[leaf] += c
        seen = set()
        for name, rx in BUCKETS:
            if any(rx.search(fr) for fr in frames):
                if name not in seen:
                    incl_b[name] += c; seen.add(name)
        hit = None
        for fr in reversed(frames):          # leaf-most first
            for name, rx in BUCKETS:
                if rx.search(fr):
                    hit = name; break
            if hit: break
        self_b[hit or "unmatched"] += c

    print("\n-- self time (leaf-most bucket) --")
    for name, c in self_b.most_common():
        print(f"  {c:6d}  {100*c/total:5.1f}%  {name}")
    print("\n-- inclusive (bucket anywhere in stack) --")
    for name, c in incl_b.most_common():
        print(f"  {c:6d}  {100*c/total:5.1f}%  {name}")
    print("\n-- top leaf symbols --")
    for sym, c in leaves.most_common(28):
        sym = sym if len(sym) <= 130 else sym[:127] + "..."
        print(f"  {c:6d}  {100*c/total:5.1f}%  {sym}")

if __name__ == "__main__":
    for p in sys.argv[1:]:
        main(p); print()
