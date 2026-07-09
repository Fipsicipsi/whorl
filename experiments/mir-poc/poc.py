import sys, whorl_mir
for f in sys.argv[1:]:
    print(f"### {f}")
    evs = whorl_mir.analyze_text(open(f).read())
    for (name, held, acq) in evs:
        print(f"  fn {name}: acquire {acq}  held={{{', '.join(held)}}}")
    label, detail = whorl_mir.verdict(evs)
    print(f"  [{label}] {detail}\n")
