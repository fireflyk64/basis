"""Arm comparison that normalises for how much traffic the crowd actually delivered.

serverCores alone moves with the crowd's own speed (its CPU drifted 0.57 -> 0.70 across one
session on this box). Server CPU per delivered packet does not, so it is the statistic to
compare arms on; both are printed, plus every paired difference where the same rep ran both.
"""
import sys, statistics as st
from collections import defaultdict

rows=[]
for path in sys.argv[1:]:
    for line in open(path):
        f=line.rstrip("\n").split("\t")
        if len(f)<9 or f[0]=='rep' or not f[3]: continue
        try: rows.append((f[0],f[1],f[2],float(f[3]),float(f[4]),float(f[5]),float(f[6]),float(f[7])))
        except ValueError: continue

by=defaultdict(list)
for rep,arm,rung,cores,pkts,tick,mb,voice in rows:
    by[(rung,arm)].append((rep,cores,pkts,cores/pkts*1e6 if pkts else float('nan'),tick,mb,voice))

for rung in sorted({k[0] for k in by}):
    print(f"\n=== {rung} ===")
    print(f"{'arm':<20}{'cores med':>10}{'n':>3}{'µs/pkt med':>12}{'pkts/s':>9}{'tickMs':>8}{'MB':>7}{'voice':>8}")
    base=None
    for arm in sorted({k[1] for k in by if k[0]==rung}):
        v=by[(rung,arm)]
        med=lambda i: st.median([x[i] for x in v])
        if arm=='system': base=(med(1),med(3))
        line=f"{arm:<20}{med(1):>10.3f}{len(v):>3}{med(3):>12.2f}{med(2):>9.0f}{med(4):>8.2f}{med(5):>7.1f}{med(6):>8.4f}"
        if base and arm!='system':
            line+=f"   cores {100*(med(1)-base[0])/base[0]:+.1f}%  µs/pkt {100*(med(3)-base[1])/base[1]:+.1f}%"
        print(line)
        print(f"{'  runs:':<20}"+", ".join(f"{x[1]:.3f}/{x[3]:.1f}µs" for x in v))
    # paired differences: same rep label, system vs each other arm
    sysruns={x[0]:x for x in by.get((rung,'system'),[])}
    for arm in sorted({k[1] for k in by if k[0]==rung and k[1]!='system'}):
        pairs=[(sysruns[x[0]],x) for x in by[(rung,arm)] if x[0] in sysruns]
        if not pairs: continue
        dc=[100*(b[1]-a[1])/a[1] for a,b in pairs]
        du=[100*(b[3]-a[3])/a[3] for a,b in pairs]
        print(f"  paired vs system ({len(pairs)}): cores {['%+.1f%%'%x for x in dc]} mean {st.mean(dc):+.1f}% | "
              f"µs/pkt {['%+.1f%%'%x for x in du]} mean {st.mean(du):+.1f}%")
