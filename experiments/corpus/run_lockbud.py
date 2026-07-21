"""Head-to-head: Lockbud vs the Whorl pipeline on the labeled corpus.

Lockbud (github.com/BurtonQin/lockbud) is the established static Rust deadlock
detector: MIR-based, points-to lock identity, guard-lifetime tracking, lock-order
cycles. It is an UNSOUND bug-finder by design (its own README documents false
positives and negatives); Whorl's claim is the opposite trade: sound
over-approximation. This runner puts both on the same hand-labeled cases.

The thesis this table exists to test: Whorl misses nothing (no false
negatives), and its false positives are bounded and explainable.

Needs: `cargo lockbud` installed (built from the lockbud repo with its OWN
pinned nightly; see its README) and the whorl pipeline prerequisites from
run_dylint.py. Lockbud runs are per-crate under lockbud-work/ (gitignored).
"""
import glob
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.join(HERE, "lockbud-work")
LOCKBUD_TOOLCHAIN = "nightly-2026-02-07"

CARGO_TOML = """[package]
name = "case"
version = "0.1.0"
edition = "2021"

[workspace]
"""

# Diagnostic markers lockbud prints for the -k deadlock detectors.
DEADLOCK_MARKERS = ("DoubleLock", "ConflictLock", "CondvarDeadlock")


def lockbud_verdict(case_path, name):
    d = os.path.join(WORK, name)
    os.makedirs(os.path.join(d, "src"), exist_ok=True)
    with open(os.path.join(d, "Cargo.toml"), "w") as f:
        f.write(CARGO_TOML)
    with open(os.path.join(d, "src", "lib.rs"), "w") as f:
        f.write(open(case_path).read())
    env = dict(os.environ)
    env.pop("CARGO_TARGET_DIR", None)  # per-case target, cleaned per run
    subprocess.run(
        ["cargo", f"+{LOCKBUD_TOOLCHAIN}", "clean"],
        cwd=d, env=env, capture_output=True, text=True,
    )
    r = subprocess.run(
        ["cargo", f"+{LOCKBUD_TOOLCHAIN}", "lockbud", "-k", "deadlock"],
        cwd=d, env=env, capture_output=True, text=True,
    )
    out = r.stdout + r.stderr
    if r.returncode != 0 and not any(m in out for m in DEADLOCK_MARKERS):
        return "ERR", out[-400:]
    if any(m in out for m in DEADLOCK_MARKERS):
        return "DEADLOCK", ""
    return "SAFE", ""


def main():
    os.makedirs(WORK, exist_ok=True)
    sys.path.insert(0, os.path.join(HERE, "..", "mir-poc"))
    sys.path.insert(0, HERE)
    from run_dylint import dylint_verdict  # the real pipeline, not the probe

    cases = sorted(glob.glob(os.path.join(HERE, "cases", "*.rs")))
    rows, lb_fn, lb_fp, whorl_fn = [], 0, 0, 0
    for path in cases:
        name = os.path.splitext(os.path.basename(path))[0]
        src = open(path).read()
        m = re.search(r"//\s*EXPECT:\s*(SAFE|DEADLOCK)", src)
        expect = m.group(1) if m else "?"
        # whorl column: the REAL pipeline (dylint front-end -> solver), so this
        # is a tool-vs-tool comparison rather than probe-vs-tool.
        whorl = dylint_verdict(path, name)
        lb, errnote = lockbud_verdict(path, name)
        if expect == "DEADLOCK" and whorl == "SAFE":
            whorl_fn += 1
        if expect == "DEADLOCK" and lb == "SAFE":
            lb_fn += 1
        if expect == "SAFE" and lb == "DEADLOCK":
            lb_fp += 1
        note = errnote if lb == "ERR" else ""
        rows.append((os.path.basename(path), expect, whorl, lb))
        print(f"  {os.path.basename(path):26} expect={expect:9} whorl={whorl:9} lockbud={lb:9}{note}")
    print("-" * 72)
    print(
        f"whorl false negatives: {whorl_fn} | "
        f"lockbud false negatives: {lb_fn} | lockbud false positives: {lb_fp}"
    )
    if whorl_fn == 0 and lb_fn > 0:
        print("Thesis holds on this corpus: whorl misses nothing; lockbud misses "
              f"{lb_fn} labeled deadlock(s).")
    elif whorl_fn == 0:
        print("Both miss nothing on this corpus; the difference is the claim "
              "(sound prover vs bug-finder) and the false-positive trade.")
    else:
        print(f"WHORL IS UNSOUND ON THIS CORPUS ({whorl_fn} false negatives) -- fix first.")
    sys.exit(1 if whorl_fn else 0)


if __name__ == "__main__":
    main()
