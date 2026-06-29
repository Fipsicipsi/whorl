# Security & soundness policy

Whorl makes a **soundness claim**: if a program is reported `[SAFE]`, it has no
lock-ordering (or, in embedded mode, single-core interrupt-preemption) deadlock.
For a tool like this, the most important "security" property *is* soundness - a
false `[SAFE]` (a missed deadlock) is the dangerous failure.

## Please read before relying on a verdict

Whorl is a **research prototype**, and its soundness is **argued and
adversarially tested, not yet mechanically proven**. Its development found and
fixed 7 real soundness bugs across 8 adversarial reviews - direct evidence that
informal arguments here can be wrong. **Do not treat a `[SAFE]` verdict as a
safety guarantee in a system where a deadlock is dangerous** until the soundness
of the encoding has been independently established for your use.

A `[SAFE]` verdict means **"no lock-ordering deadlock"** - *not* "cannot
deadlock". Out of scope (a green verdict says nothing about these):
condition-variable lost wakeups, channel/actor cycles, external resources
(e.g. a DB row lock), multicore preemption, and nested interrupts. `couple`,
`extern ... acquires`, and `mask` are **trusted assertions** by the author.

## Reporting

The most valuable report is a **soundness hole**: a program with a genuine
lock-ordering or ISR-preemption deadlock that Whorl reports `[SAFE]`. A minimal
reproducer plus the expected/actual verdict is ideal.

Please open a GitHub issue (or, for anything you consider sensitive, contact a
maintainer privately) with the reproducer. Soundness reports are triaged ahead
of everything else.
