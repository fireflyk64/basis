"""Compare two folded profiles by the shares that a change was supposed to move."""
import sys, re
def load(p):
    out=[]
    for raw in open(p):
        raw=raw.rstrip("\n")
        if not raw: continue
        try: s,c=raw.rsplit(" ",1); c=int(c)
        except ValueError: continue
        f=s.split(";"); out.append((f[0],f[1:],c))
    return out
PATS=[("sender_task path",        re.compile(r"sender_task")),
      ("send_datagram(_wait)",    re.compile(r"send_datagram")),
      ("Notify / waker / wake",   re.compile(r"Notify|notify|waker|Waker|wake_by")),
      ("tokio poll/schedule",     re.compile(r"task::harness|poll_future|scheduler|run_task")),
      ("noq inclusive",           re.compile(r"\bnoq")),
      ("poll_transmit",           re.compile(r"poll_transmit")),
      ("datagram_task (recv)",    re.compile(r"datagram_task|read_datagram")),
      ("alloc/free (innermost 3)",re.compile(r"__rust_alloc|__rust_dealloc|\bmalloc\b|_int_free|RawVec|raw_vec|alloc::alloc|exchange_malloc")),
     ]
a,b=load(sys.argv[1]),load(sys.argv[2])
la,lb=sys.argv[3],sys.argv[4]
ta,tb=sum(c for _,_,c in a),sum(c for _,_,c in b)
print(f"{'':<28}{la:>16}{lb:>16}{'change':>10}")
print(f"{'samples':<28}{ta:>16}{tb:>16}")
for name,rx in PATS:
    inner = name.startswith("alloc")
    ca=sum(cn for _,f,cn in a if any(rx.search(x) for x in (f[-3:] if inner else f)))
    cb=sum(cn for _,f,cn in b if any(rx.search(x) for x in (f[-3:] if inner else f)))
    pa,pb=100*ca/ta,100*cb/tb
    rel=(pb-pa)/pa*100 if pa else 0
    print(f"{name:<28}{pa:>15.2f}%{pb:>15.2f}%{rel:>9.0f}%")
