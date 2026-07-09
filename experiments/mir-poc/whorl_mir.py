"""Whorl stable-MIR feasibility PoC.

Extracts, from `rustc --emit=mir` text, the two make-or-break inputs the Whorl
solver needs from real Rust: per lock acquisition, the lock CLASS (from the
lock() receiver place) and the HELD-SET (from RAII guard liveness, via a CFG
may-be-held dataflow over Drop terminators). Straightforward, sound-leaning, and
deliberately a feasibility probe: the production path is the typed dylint pass in
../whorl_lint. Intra-procedural only; field names are positional (field0), not
source names (that needs typed MIR).
"""
import re, collections

RECV   = re.compile(r'(_\d+) = &\((.*?): std::sync::(Mutex|RwLock)<(.*?)>\);')
LOCK   = re.compile(r'(_\d+) = std::sync::(?:Mutex|RwLock)::<.*?>::(?:lock|read|write)\((?:move|copy) (_\d+)\)')
UNWRAP = re.compile(r'(_\d+) = .*?unwrap\((?:move|copy) (_\d+)\)')
DROP   = re.compile(r'drop\((_\d+)\)')
# std::mem::drop consumes the guard by move; no drop(_g) terminator remains in
# this body afterwards. Treating it as a kill is sound because mem::drop
# definitively destroys its argument. (A general move-out must NOT kill: the
# guard may live on elsewhere, and dropping it from the held-set would be a
# false negative.)
MEMDROP = re.compile(r'(?:std|core)::mem::drop::<.*?>\((?:move|copy) (_\d+)\)')
# `_a = move _b;` -- guards are not Copy, so a plain move statement transfers
# ownership: the guard now lives in _a and _b is dead. Tracking this keeps the
# held-set following the value (sound), so mem::drop of the moved-to temp
# releases the right guard.
MOVE   = re.compile(r'^\s*(_\d+) = move (_\d+);')
# `_5 = helper(copy _1) -> ...` -- a call to another function in the same file.
# Only names that are local functions count (checked against split_fns keys);
# the held-set at the call site feeds the interprocedural entry-may fixpoint.
CALL   = re.compile(r'_\d+ = (\w+)\((?:move|copy) ')
FN     = re.compile(r'^fn (\w+)\(')
BB     = re.compile(r'^\s*bb(\d+): \{')
SUCC   = re.compile(r'bb(\d+)')

def norm_class(place, lk, elem):
    p = re.sub(r'_\d+', '_', place)
    m = re.search(r'\.(\d+)', p)
    return f"{lk}<{elem}>::field{m.group(1)}" if m else f"{lk}<{elem}>::{p}"

def split_fns(text):
    out, cur, name = {}, [], None
    for ln in text.splitlines():
        fm = FN.match(ln)
        if fm: name, cur = fm.group(1), []
        if name is not None:
            cur.append(ln)
            if ln == '}': out[name] = cur; name = None
    return out

def parse_fn(lines, local_fns=frozenset()):
    recv, blocks, cur = {}, {}, None
    for ln in lines:
        bm = BB.match(ln)
        if bm: cur = int(bm.group(1)); blocks[cur] = {'eff': [], 'succ': []}; continue
        if cur is None: continue
        m = RECV.search(ln)
        if m: recv[m.group(1)] = norm_class(m.group(2), m.group(3), m.group(4)); continue
        m = LOCK.search(ln)
        if m: blocks[cur]['eff'].append(('lock', m.group(1), recv.get(m.group(2), '?')))
        else:
            m = UNWRAP.search(ln)
            if m: blocks[cur]['eff'].append(('unwrap', m.group(1), m.group(2)))
            else:
                m = MOVE.match(ln)
                if m: blocks[cur]['eff'].append(('move', m.group(1), m.group(2)))
                else:
                    m = MEMDROP.search(ln) or DROP.search(ln)
                    if m: blocks[cur]['eff'].append(('drop', m.group(1)))
                    else:
                        m = CALL.search(ln)
                        if m and m.group(1) in local_fns:
                            blocks[cur]['eff'].append(('call', m.group(1)))
        if '->' in ln:
            for s in SUCC.findall(ln.split('->', 1)[1]): blocks[cur]['succ'].append(int(s))
    return blocks

def analyze_fn(name, lines, local_fns=frozenset()):
    blocks = parse_fn(lines, local_fns)
    if not blocks: return [], []
    preds = collections.defaultdict(list)
    for b, info in blocks.items():
        for s in info['succ']:
            if s in blocks: preds[s].append(b)
    entry = min(blocks)
    gclass, pending = {}, {}
    def transfer(bb, state):
        st = set(state)
        for e in blocks[bb]['eff']:
            if e[0] == 'lock': pending[e[1]] = e[2]
            elif e[0] == 'unwrap':
                if e[2] in pending: gclass[e[1]] = pending[e[2]]; st.add(e[1])
            elif e[0] == 'move':
                if e[2] in st and e[2] in gclass:
                    gclass[e[1]] = gclass[e[2]]; st.discard(e[2]); st.add(e[1])
            elif e[0] == 'drop': st.discard(e[1])
        return frozenset(st)  # 'call' effects do not change guard liveness
    IN = {b: frozenset() for b in blocks}; OUT = dict(IN)
    changed = True
    while changed:
        changed = False
        for b in blocks:
            ins = frozenset() if (b == entry or not preds[b]) else frozenset().union(*[OUT[p] for p in preds[b]])
            o = transfer(b, ins)
            if ins != IN[b] or o != OUT[b]: IN[b] = ins; OUT[b] = o; changed = True
    events, calls = [], []
    for b in sorted(blocks):
        st = set(IN[b])
        for e in blocks[b]['eff']:
            if e[0] == 'lock':
                held = sorted({gclass[g] for g in st if g in gclass})
                events.append((name, held, e[2]))
            elif e[0] == 'unwrap' and e[2] in pending and e[1] in gclass: st.add(e[1])
            elif e[0] == 'move':
                if e[2] in st and e[2] in gclass: st.discard(e[2]); st.add(e[1])
            elif e[0] == 'drop': st.discard(e[1])
            elif e[0] == 'call':
                held = sorted({gclass[g] for g in st if g in gclass})
                calls.append((name, held, e[1]))
    return events, calls

def analyze_text(text):
    fns = split_fns(text)
    local = frozenset(fns)
    evs, calls = [], []
    for name, lines in fns.items():
        e, c = analyze_fn(name, lines, local)
        evs += e
        calls += c
    # Interprocedural entry-may fixpoint (same as the .whorl frontend): a guard
    # held at a call site is held throughout the callee, transitively.
    entry = collections.defaultdict(set)
    changed = True
    while changed:
        changed = False
        for caller, held, callee in calls:
            incoming = set(held) | entry[caller]
            if not incoming <= entry[callee]:
                entry[callee] |= incoming
                changed = True
    return [(fn, sorted(set(held) | entry[fn]), acq) for fn, held, acq in evs]

def verdict(events):
    """Return (label, detail). Same Havender model as whorl::solver."""
    edges = {}; selfloop = None
    for fn, held, acq in events:
        for h in held:
            edges[(h, acq)] = fn
            if h == acq and selfloop is None: selfloop = (h, fn)
    if selfloop:
        c, fn = selfloop
        return ("DEADLOCK", f"same-class held then re-acquired: {c} < {c} (via {fn})")
    g = collections.defaultdict(set)
    for (a, b) in edges:
        if a != b: g[a].add(b)
    color = collections.defaultdict(int); cyc = [None]
    def dfs(u, stk):
        color[u] = 1; stk.append(u)
        for v in g[u]:
            if color[v] == 1: cyc[0] = stk[stk.index(v):] + [v]; return True
            if color[v] == 0 and dfs(v, stk): return True
        color[u] = 2; stk.pop(); return False
    if any(color[n] == 0 and dfs(n, []) for n in list(g)):
        return ("DEADLOCK", "cross-class cycle: " + " < ".join(cyc[0]))
    return ("SAFE", "acyclic lock graph; a valid global order exists")
