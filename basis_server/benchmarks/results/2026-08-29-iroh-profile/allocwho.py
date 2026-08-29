#!/usr/bin/env python3
"""Who allocates, and how big is the quinn-fork (noq) really."""
import sys, re
from collections import Counter
ALLOC = re.compile(r"__rust_alloc|__rust_dealloc|\bmalloc\b|_int_free|cfree|\bfree\b|RawVec|raw_vec|alloc::alloc|exchange_malloc|box_free|Global>::(alloc|dealloc|grow)")
GLUE  = re.compile(r"^(core::|std::|alloc::(?!collections)|__rust|<core|<std|<alloc::(?!collections))|drop_in_place|drop_glue|Layout|NonNull|Unique|PhantomData|^\{\{?closure|^\{closure")
def main(path):
    data=[]
    with open(path) as f:
        for raw in f:
            raw=raw.rstrip("\n")
            if not raw: continue
            try: stack,cnt=raw.rsplit(" ",1); cnt=int(cnt)
            except ValueError: continue
            fr=stack.split(";"); data.append((fr[0],fr[1:],cnt))
    total=sum(c for _,_,c in data)
    print(f"== {path}: {total} samples ==")
    for name,rx in [("noq/noq_proto (quinn fork) inclusive", re.compile(r"\bnoq")),
                    ("iroh:: inclusive", re.compile(r"\biroh")),
                    ("tokio inclusive", re.compile(r"tokio")),
                    ("basis (our code) inclusive", re.compile(r"basis_network"))]:
        c=sum(cn for _,fr,cn in data if any(rx.search(x) for x in fr))
        print(f"  {c:5d} {100*c/total:5.1f}%  {name}")
    # alloc-leaf samples: attribute to nearest meaningful caller
    callers=Counter(); alloc_total=0
    for _,fr,cn in data:
        if not fr: continue
        # leaf-ward scan: is the leaf (or the two innermost frames) an alloc frame?
        if not any(ALLOC.search(x) for x in fr[-3:]): continue
        alloc_total+=cn
        who="??"
        for x in reversed(fr):
            if ALLOC.search(x) or GLUE.search(x): continue
            who=x; break
        callers[who if len(who)<=120 else who[:117]+"..."]+=cn
    print(f"\n-- samples with an alloc/free frame in the innermost 3: {alloc_total} ({100*alloc_total/total:.1f}%) — nearest real caller --")
    for who,cn in callers.most_common(25):
        print(f"  {cn:5d} {100*cn/total:5.1f}%  {who}")
if __name__=="__main__":
    for p in sys.argv[1:]: main(p); print()
