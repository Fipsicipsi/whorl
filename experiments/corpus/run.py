"""Differential / ground-truth harness for the Whorl MIR PoC.

For each labeled case under cases/, emit MIR with stable rustc, run the PoC, and
compare the verdict to the hand-labeled EXPECT. The decisive metric is
SOUNDNESS: a case labeled DEADLOCK that the tool calls SAFE is a FALSE NEGATIVE
and a soundness violation (the thing Whorl must never do). False positives
(SAFE labeled, DEADLOCK reported) are acceptable but tracked.

The `lockbud` column is left as TBD here: Lockbud needs its own nightly + driver
(see README). Filling that column is how the soundness claim gets *earned* head
to head, per Direction 2 of the research synthesis.
"""
import os, re, sys, glob, subprocess
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "mir-poc"))
import whorl_mir

cases = sorted(glob.glob(os.path.join(HERE, "cases", "*.rs")))
rows, false_neg, false_pos = [], 0, 0
for path in cases:
    src = open(path).read()
    m = re.search(r'//\s*EXPECT:\s*(SAFE|DEADLOCK)', src)
    expect = m.group(1) if m else "?"
    mir = path[:-3] + ".mir"
    subprocess.run(["rustc", "--emit=mir", "--crate-type=lib", path, "-o", mir],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if os.path.exists(mir):
        label, _ = whorl_mir.verdict(whorl_mir.analyze_text(open(mir).read()))
    else:
        label = "ERR"
    if expect == "DEADLOCK" and label == "SAFE": status = "FALSE-NEG (UNSOUND)"; false_neg += 1
    elif expect == "SAFE" and label == "DEADLOCK": status = "false-pos"; false_pos += 1
    elif expect == label: status = "ok"
    else: status = "MISMATCH"
    rows.append((os.path.basename(path), expect, label, "TBD", status))

w = max(len(r[0]) for r in rows)
print(f"{'case'.ljust(w)}  {'expect':9} {'whorl':9} {'lockbud':8} status")
print("-" * (w + 38))
for name, expect, label, lb, status in rows:
    print(f"{name.ljust(w)}  {expect:9} {label:9} {lb:8} {status}")
print("-" * (w + 38))
ok = sum(1 for r in rows if r[4] == "ok")
print(f"{ok}/{len(rows)} match ground truth | false negatives (soundness): {false_neg} | false positives: {false_pos}")
print("SOUND on this corpus (no false negatives)." if false_neg == 0
      else f"UNSOUND: {false_neg} false negative(s) -- investigate before trusting any SAFE verdict.")
sys.exit(1 if false_neg else 0)
