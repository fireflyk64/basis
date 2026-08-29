"""Pool forward-order and reverse-order runs so within-rep drift cancels.

Each rep runs the three arms back to back, and this box's packet rate drifts downward inside a
rep, so whichever arm goes last looks best. Running the sequence in both directions and pooling
removes that: every arm occupies first, middle and last position an equal number of times.
"""
import sys, statistics as st
from collections import defaultdict
def load(path, order):
    out=[]
    for line in open(path):
        f=line.rstrip("\n").split("\t")
        if len(f)<10 or f[0]=='rep' or not f[3]: continue
        out.append((order,f[0],f[1],float(f[3]),float(f[4]),float(f[5]),float(f[6]),float(f[7]),float(f[9])))
    return out
rows=load(sys.argv[1],"fwd")+load(sys.argv[2],"rev")
by=defaultdict(list)
for order,rep,arm,cores,pkts,tick,mb,voice,crowd in rows:
    by[arm].append((order,rep,cores,pkts,cores/pkts*1e6,tick,mb,voice,crowd))
ORDER=["baseline","serveronly","bothends"]
print(f"{'arm':<12}{'n':>2}{'pkts/s med':>12}{'cores med':>11}{'µs/pkt':>9}{'crowd':>8}{'tick':>7}{'voice':>8}")
base=None
for arm in ORDER:
    v=by.get(arm)
    if not v: continue
    med=lambda i: st.median([x[i] for x in v])
    if arm=="baseline": base=(med(3),med(2),med(8))
    line=f"{arm:<12}{len(v):>2}{med(3):>12.0f}{med(2):>11.3f}{med(4):>9.2f}{med(8):>8.3f}{med(5):>7.2f}{med(7):>8.4f}"
    if base and arm!="baseline":
        line+=f"  pkts {100*(med(3)-base[0])/base[0]:+.1f}%  cores {100*(med(2)-base[1])/base[1]:+.1f}%  crowd {100*(med(8)-base[2])/base[2]:+.1f}%"
    print(line)
    print(f"{'  runs:':<12}"+", ".join(f"{x[0]}{x[1]}:{x[3]:.0f}pkt/{x[2]:.3f}c" for x in v))
# paired within (order, rep)
b={(x[0],x[1]):x for x in by.get("baseline",[])}
for arm in ORDER[1:]:
    pairs=[(b[(x[0],x[1])],x) for x in by.get(arm,[]) if (x[0],x[1]) in b]
    if not pairs: continue
    dp=[100*(y[3]-x[3])/x[3] for x,y in pairs]; dc=[100*(y[2]-x[2])/x[2] for x,y in pairs]
    print(f"  {arm} paired ({len(pairs)}): packets mean {st.mean(dp):+.1f}% {['%+.1f'%v for v in dp]} | cores mean {st.mean(dc):+.1f}% {['%+.1f'%v for v in dc]}")
