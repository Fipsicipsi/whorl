"""Embedded critical-section cases through the dylint pipeline.

These need no_std lock primitives (critical_section, spin), so they run only
through the typed dylint front-end, not the stable-MIR text PoC (which handles
std locks only). Each case is wrapped in a crate with the two deps, checked with
the lint, and its events fed to `whorl --events`.

Needs the pinned nightly + cargo-dylint and the stable `whorl` binary built.
Work dirs live under work/ (gitignored); deps come from crates.io on first run.
"""
import glob
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
LINT = os.path.join(REPO, "experiments", "whorl_lint")
WHORL = os.path.join(REPO, "target", "debug", "whorl.exe")
WORK = os.path.join(HERE, "work")

CARGO_TOML = """[package]
name = "case"
version = "0.1.0"
edition = "2021"

[dependencies]
critical-section = "1"
spin = "0.9"

[workspace]
"""


def verdict(case_path, name):
    d = os.path.join(WORK, name)
    os.makedirs(os.path.join(d, "src"), exist_ok=True)
    with open(os.path.join(d, "Cargo.toml"), "w") as f:
        f.write(CARGO_TOML)
    shutil.copyfile(case_path, os.path.join(d, "src", "lib.rs"))
    events = os.path.join(d, "events.json")
    env = dict(os.environ)
    env["WHORL_EVENTS_OUT"] = events
    env["CARGO_TARGET_DIR"] = os.path.join(WORK, "shared-target")
    r = subprocess.run(
        ["cargo", "dylint", "--all", "--path", LINT],
        cwd=d, env=env, capture_output=True, text=True,
    )
    if not os.path.exists(events):
        return "ERR", (r.stderr or r.stdout).strip().splitlines()[-1:] or [""]
    v = subprocess.run([WHORL, "--events", events], capture_output=True, text=True)
    if "[DEADLOCK]" in v.stdout:
        return "DEADLOCK", ""
    if "[INCOMPLETE]" in v.stdout:
        return "INCOMPLETE", ""
    if "[SAFE]" in v.stdout:
        return "SAFE", ""
    return "ERR", "no verdict"


def main():
    if not os.path.exists(WHORL):
        sys.exit(f"build the stable binary first: cargo build (missing {WHORL})")
    os.makedirs(WORK, exist_ok=True)
    fn = 0
    for path in sorted(glob.glob(os.path.join(HERE, "cases", "*.rs"))):
        src = open(path).read()
        m = re.search(r"//\s*EXPECT:\s*(SAFE|DEADLOCK)", src)
        expect = m.group(1) if m else "?"
        got, _ = verdict(path, os.path.splitext(os.path.basename(path))[0])
        if got == "INCOMPLETE":
            status = "incomplete (fail-closed)"
        elif expect == "DEADLOCK" and got == "SAFE":
            status = "FALSE-NEG (UNSOUND)"
            fn += 1
        elif got == expect:
            status = "ok"
        elif expect == "SAFE" and got == "DEADLOCK":
            status = "false-pos"
        else:
            status = f"ERR ({got})"
        print(f"  {os.path.basename(path):28} expect={expect:9} whorl={got:9} {status}")
    print("-" * 66)
    print(f"embedded (critical_section) | false negatives: {fn}")
    sys.exit(1 if fn else 0)


if __name__ == "__main__":
    main()
