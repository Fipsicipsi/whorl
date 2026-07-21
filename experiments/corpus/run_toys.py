"""Run the Whorl pipeline on Lockbud's OWN toy test cases.

These are external, independently-authored deadlock programs shipped in the
lockbud repo (toys/). Running Whorl on someone else's ground-truth set is a
stronger test than hand-written cases: it removes author bias. Only the
lock-ordering-relevant toys are included; lockbud's condvar / atomicity /
use-after-free / invalid-free toys are different bug classes and out of scope
for a lock-ordering analyzer.

Ground truth is taken from lockbud's own naming (the `*-no-deadlock` toys are
safe; `conflict`/`inter`/`intra`/`static-ref`/`lock-closure` are deadlocks) and
confirmed by reading each source.

Needs: LOCKBUD_SRC pointing at a lockbud checkout (default ../../../lockbud-src),
the pinned nightly + cargo-dylint, and the stable `whorl` binary built. Work
dirs live under toys-work/ (gitignored); deps (parking_lot/spin/lazy_static)
are fetched by cargo on first run.
"""
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
LINT = os.path.join(REPO, "experiments", "whorl_lint")
WHORL = os.path.join(REPO, "target", "debug", "whorl.exe")
LOCKBUD_SRC = os.environ.get(
    "LOCKBUD_SRC", os.path.abspath(os.path.join(REPO, "..", "lockbud-src"))
)
WORK = os.path.join(HERE, "toys-work")

# lock-ordering-relevant toys and their ground-truth verdict.
TOYS = {
    "conflict": "DEADLOCK",             # 3-class cycle mu < rw1 < rw2 < mu
    "conflict-inter": "DEADLOCK",       # interprocedural mu1 < rw1 < mu1
    "inter": "DEADLOCK",                # double-lock across calls (std/parking_lot/spin)
    "intra": "DEADLOCK",                # double-lock within a match (temporary guard)
    "static-ref": "DEADLOCK",           # lazy_static Mutex locked twice
    "lock-closure": "DEADLOCK",         # a<b in one closure, b<a in another
    "call-no-deadlock": "SAFE",         # guards moved into a call, then dropped
    "recursive-no-deadlock": "SAFE",    # parking_lot read + read_recursive
    "wait-lock-no-deadlock": "SAFE",    # guard passed through a fn and dropped
}


def verdict(name):
    src = os.path.join(LOCKBUD_SRC, "toys", name)
    dst = os.path.join(WORK, name)
    # Overwrite in place: rmtree races with Windows file locks (AV / lingering
    # build handles) on the shared target dir.
    shutil.copytree(src, dst, dirs_exist_ok=True)
    # Isolate from any parent workspace.
    with open(os.path.join(dst, "Cargo.toml"), "a") as f:
        f.write("\n[workspace]\n")
    events = os.path.join(dst, "events.json")
    env = dict(os.environ)
    env["WHORL_EVENTS_OUT"] = events
    env["CARGO_TARGET_DIR"] = os.path.join(WORK, "shared-target")
    r = subprocess.run(
        ["cargo", "dylint", "--all", "--path", LINT],
        cwd=dst, env=env, capture_output=True, text=True,
    )
    if not os.path.exists(events):
        tail = (r.stderr or r.stdout).strip().splitlines()[-1:] or [""]
        return "ERR", tail[0][:90]
    v = subprocess.run([WHORL, "--events", events], capture_output=True, text=True)
    if "[DEADLOCK]" in v.stdout:
        return "DEADLOCK", ""
    if "[INCOMPLETE]" in v.stdout:
        return "INCOMPLETE", ""
    if "[SAFE]" in v.stdout:
        return "SAFE", ""
    return "ERR", "no verdict"


def main():
    if not os.path.isdir(LOCKBUD_SRC):
        sys.exit(f"lockbud checkout not found at {LOCKBUD_SRC}; set LOCKBUD_SRC")
    if not os.path.exists(WHORL):
        sys.exit(f"build the stable binary first: cargo build (missing {WHORL})")
    os.makedirs(WORK, exist_ok=True)
    fn, other = 0, 0
    for name, expect in TOYS.items():
        got, note = verdict(name)
        if got == "INCOMPLETE":
            status = "incomplete (fail-closed)"
        elif got == "ERR":
            status = f"ERR ({note})"
        elif got == expect:
            status = "ok"
        elif expect == "DEADLOCK":
            status = "FALSE-NEG (UNSOUND)"
            fn += 1
        else:
            status = "false-pos"
            other += 1
        print(f"  {name:24} expect={expect:9} whorl={got:9} {status}")
    print("-" * 66)
    print(f"lockbud-toys | whorl false negatives: {fn} | false positives: {other}")
    sys.exit(1 if fn else 0)


if __name__ == "__main__":
    main()
