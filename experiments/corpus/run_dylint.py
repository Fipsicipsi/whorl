"""End-to-end differential runner: the dylint MIR front-end on every corpus case.

For each labeled case this generates a one-file crate, runs the real pipeline
(cargo dylint -> events JSON -> `whorl --events`), and compares the verdict to
the hand label AND to the stable-MIR text PoC. Two independent implementations
of the same analysis on the same corpus is a real differential test: any
disagreement between them is a bug in one of the two.

Needs: the pinned nightly + cargo-dylint (see ../whorl_lint/README.md), and the
stable `whorl` binary built (`cargo build` at the repo root). Work dirs are
created under e2e-work/ (gitignored); a shared CARGO_TARGET_DIR keeps the lint
library build warm across cases.
"""
import glob
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
LINT = os.path.join(REPO, "experiments", "whorl_lint")
WHORL = os.path.join(REPO, "target", "debug", "whorl.exe")
WORK = os.path.join(HERE, "e2e-work")
sys.path.insert(0, os.path.join(HERE, "..", "mir-poc"))
import whorl_mir

CARGO_TOML = """[package]
name = "case"
version = "0.1.0"
edition = "2021"

[workspace]
"""


def dylint_verdict(case_path, name):
    d = os.path.join(WORK, name)
    os.makedirs(os.path.join(d, "src"), exist_ok=True)
    with open(os.path.join(d, "Cargo.toml"), "w") as f:
        f.write(CARGO_TOML)
    with open(os.path.join(d, "src", "lib.rs"), "w") as f:
        f.write(open(case_path).read())
    events = os.path.join(d, "events.json")
    env = dict(os.environ)
    env["WHORL_EVENTS_OUT"] = events
    env["CARGO_TARGET_DIR"] = os.path.join(WORK, "shared-target")
    r = subprocess.run(
        ["cargo", "dylint", "--all", "--path", LINT],
        cwd=d,
        env=env,
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        return "ERR(lint)"
    if not os.path.exists(events):
        return "ERR(no-json)"
    v = subprocess.run([WHORL, "--events", events], capture_output=True, text=True)
    if "[DEADLOCK]" in v.stdout:
        return "DEADLOCK"
    if "[SAFE]" in v.stdout:
        return "SAFE"
    return "ERR(verdict)"


def poc_verdict(case_path, name):
    mir = os.path.join(WORK, name + ".mir")
    subprocess.run(
        ["rustc", "--emit=mir", "--crate-type=lib", case_path, "-o", mir],
        capture_output=True,
    )
    if not os.path.exists(mir):
        return "ERR"
    label, _ = whorl_mir.verdict(whorl_mir.analyze_text(open(mir).read()))
    return label


def main():
    if not os.path.exists(WHORL):
        sys.exit(f"build the stable binary first: cargo build (missing {WHORL})")
    os.makedirs(WORK, exist_ok=True)
    cases = sorted(glob.glob(os.path.join(HERE, "cases", "*.rs")))
    rows, fn_dylint, disagree = [], 0, 0
    for path in cases:
        name = os.path.splitext(os.path.basename(path))[0]
        src = open(path).read()
        m = re.search(r"//\s*EXPECT:\s*(SAFE|DEADLOCK)", src)
        expect = m.group(1) if m else "?"
        poc = poc_verdict(path, name)
        dy = dylint_verdict(path, name)
        if expect == "DEADLOCK" and dy == "SAFE":
            status = "FALSE-NEG (UNSOUND)"
            fn_dylint += 1
        elif expect == "SAFE" and dy == "DEADLOCK":
            status = "false-pos"
        elif dy == expect:
            status = "ok"
        else:
            status = "ERR"
        if poc != dy and "ERR" not in (poc, dy):
            status += " DIVERGES-FROM-POC"
            disagree += 1
        rows.append((os.path.basename(path), expect, poc, dy, status))
        print(f"  {os.path.basename(path):26} expect={expect:9} poc={poc:9} dylint={dy:9} {status}")
    print("-" * 78)
    ok = sum(1 for r in rows if r[4].startswith("ok"))
    print(
        f"{ok}/{len(rows)} dylint matches ground truth | "
        f"dylint false negatives (soundness): {fn_dylint} | "
        f"poc/dylint divergences: {disagree}"
    )
    sys.exit(1 if fn_dylint else 0)


if __name__ == "__main__":
    main()
