---
name: haul-scorecard
description: Run real links through a Haul test copy and record what each host came to — the live half of the host suite — with Haul's own code for each site on, and off (`HAUL_GENERIC_ONLY=1`) to measure what a host Haul has never seen would get. Use when asked how many hosts Haul handles, before and after changing how Haul treats hosts in general, and to find the next exception for the model suite in `crates/haul-core/tests/suite.rs`.
---

# The host scorecard

The host suite has two halves. `crates/haul-core/tests/suite.rs` is the model half: one row per kind of site, against local stand-ins, run on every CI run. This skill is the live half: real hosts, real links, on the user's machine, when asked.

## Before running it

- **Ask the user first, every time.** Real links use the user's free slots (ddownload's one free file every two hours, captchas, daily allowances), and the run takes a while.
- **Links stay outside the repository.** Put them in the scratchpad, one per line, optionally `label<TAB>link`. The repository is public: commit no link, file name or result that names a real file. Results name hosts only.
- **Never use the user's own Haul.** Run a test copy as the `haul-windows-test` skill says: its own data folder and port 8791, debug mode on.

## Running it

```sh
SCRIPT=.agents/skills/haul-scorecard/scripts/scorecard.py
WINCURL=/mnt/c/Windows/System32/curl.exe
# 1. With Haul's own code for each site: what Haul does today.
python3 -I $SCRIPT --links "$SCRATCH/links.txt" --out "$SCRATCH" --curl $WINCURL --mode normal
# 2. Stop the test copy, start it again with HAUL_GENERIC_ONLY=1 set in its
#    environment beside HAUL_DATA_DIR, and run again: what a never-seen host gets.
python3 -I $SCRIPT --links "$SCRATCH/links.txt" --out "$SCRATCH" --curl $WINCURL --mode generic-only
```

For each link the script adds it, waits up to `--wait` seconds (240 by default) for it to come to something, and then removes what it added, partial files included, so a run downloads only the first bytes of each file. Outcomes:

| Outcome | Meaning |
| --- | --- |
| `file` | Bytes of the file arrived |
| `listed N` | A wrapper or page listed N files |
| `needs you` | Under Needs you: a captcha, a gate or a page only a person passes |
| `failed` | Failed; the reason says why |
| `waits` | The host asked for a wait of more than five minutes |
| `still going` | Nothing after `--wait` seconds |

It writes `scorecard-<mode>.md`, a table of site, outcome, seconds and reason, and `scorecard-<mode>.json`.

## After running it

- Compare the two modes: the generic-only score is the one that says how far the general method reaches.
- **Every surprise becomes a model row.** Describe it generically (a host whose page counts down and then shows the link, say), add a stand-in for that behaviour in `tests/support/mod.rs`, and add a row to `tests/suite.rs`. Fix it by changing a general rule, never with a branch for one site, and keep every older row passing.
- Report hosts and outcomes to the user, not links.
