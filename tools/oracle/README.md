# gemel-oracle

Differential test harness: it runs a candidate implementation and OCaml
Unison (the reference) on identical generated replicas and compares the
results. It is phase 0 of [milestone 2](../../docs/milestone-2-plan.md), and
its agreement on all three operating systems is the milestone's definition
of done.

## What a scenario is

Each scenario is generated from a seed (`src/scenario.rs`):

1. an initial tree in replica `a`;
2. replica `b` either empty, a copy of `a`, or a copy with changes of its
   own, so the first sync has conflicts;
3. a first sync;
4. random changes on each side: new, modified, deleted and renamed files,
   new directories and, on Unix, permission changes and symbolic links;
5. a second sync.

File modification times are set explicitly, well in the past, so that
update detection never depends on the clock. Names include spaces and
non-ASCII characters but never differ only in case.

## What is compared

For each scenario the reference and then the candidate run in the same
work directory. Archive names hash the roots' absolute paths, so the
directory has to be the same. The harness compares:

- each sync's exit code and its stdout and stderr, after normalising what
  legitimately varies: the work directory's path, the version banner,
  clock times, dates, durations, the progress bar and the scanning spinner;
- both final replicas: each path's kind, permissions, contents or link
  target, and file modification times;
- the names of the files in the Unison directory (archives, fingerprint
  caches, `default.prf`);
- a **recheck**: the reference run once more on the final state. It must
  report no warnings, and nothing to do beyond the conflicts the second
  sync skipped. For the candidate, this checks that OCaml accepts its
  archives.

Every sync runs with `-batch -ui text -color false -log=false
-confirmbigdel=false -contactquietly -times -maxthreads 1`
(`src/run.rs`).

## Running it

```sh
# macOS, against Homebrew's unison (the candidate defaults to the reference)
cargo run -p gemel-oracle -- --reference "$(which unison)" --count 30

# Linux in Docker, against OCaml Unison built from a Unison checkout
scripts/oracle.sh /path/to/unison --count 30
```

| Option | Meaning |
|---|---|
| `--candidate PATH` | The binary under test |
| `--seed N --count K` | Run scenarios `N` to `N+K-1` |
| `--work DIR` | Where to work (default: a fresh temporary directory) |
| `--keep` | Keep the work directories of scenarios that agree |

A scenario that differs prints its operations and the first difference in
each compared item, and its `<seed>-reference` and `<seed>-candidate`
directories are kept for inspection.

With the reference as its own candidate, every scenario must agree; CI
checks this (the `oracle` job). A candidate that runs the same unison with
`-perms 0` fails every scenario, which shows the comparison is sensitive.
