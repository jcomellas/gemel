# gemel-oracle

Differential test harness: it runs a candidate implementation and OCaml
Unison (the reference) on identical generated replicas and compares the
results. It is phase 0 of [milestone 2](../../docs/milestone-2-plan.md), and
its agreement on all three operating systems is the milestone's definition
of done. With remote roots, it is phase 0 of
[milestone 3](../../docs/milestone-3-plan.md) too.

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
(`src/run.rs`). With `--concurrent`, `-maxthreads 1` is left out, so that
items run concurrently as clients run them by default, and what each
sync prints is compared without its order; `--ordered` keeps the order.
OCaml's order is deterministic, and a gemel server keeps it in all but
one case, copying contents another transfer brings (milestone 3's phase
5b notes): CI compares a gemel server's output with its order with
`-xferbycopying=false`, and without its order otherwise.

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
| `--flavour unix\|windows` | Generate scenarios for this system (default: the host's) |
| `--record FILE` | Save the reference's outcomes, if every scenario agrees |
| `--replay FILE` | Compare the candidate with recorded outcomes, without a reference |
| `--work DIR` | Where to work (default: a fresh temporary directory) |
| `--keep` | Keep the work directories of scenarios that agree |
| `--remote ssh\|socket` | Reach replica `b` through a server (see below) |
| `--reference-server PATH`, `--candidate-server PATH` | Each side's server (default: its client) |
| `--concurrent` | Let items run concurrently, and compare the output without its order |
| `--ordered` | With `--concurrent`, compare the output with its order |
| `--old-server` | Start ssh servers without `__new-rpc-mode` (their compatibility mode) |
| `--client-args ARGS` | More arguments for both clients, split at spaces (`-debug remote+`) |
| `--dump` | Print what each sync printed, in full, where it differs |

A scenario that differs prints its operations and the first difference in
each compared item, and its `<seed>-reference` and `<seed>-candidate`
directories are kept for inspection. To see why the order differs, trace
both runs' messages from the client's side:

```sh
gemel-oracle --remote ssh --concurrent --ordered --reference "$(which unison)" \
  --candidate-server target/debug/gemel --seed 3 --count 1 \
  --client-args '-debug remote+' --dump
```

With the reference as its own candidate, every scenario must agree. A
candidate that runs the same unison with `-perms 0` fails every scenario,
which shows the comparison is sensitive. CI's `oracle` job runs this
self-test, then gemel as the candidate.

## Remote roots

With `--remote`, each sync reaches replica `b` through a server on the
same machine, as milestone 3 needs (`docs/milestone-3-plan.md`):

- **`ssh`:** the root is `ssh://server/b`, and `-sshcmd` is a shim that
  runs the server command here instead of on a host. It records its
  arguments in `remote/argv`, which are compared too (with the server's
  path as `<server>`), and drops what precedes the server command
  (`[-l user] [-p port] host -e none`). `-servercmd` names the server.
  With `--old-server`, it also drops `__new-rpc-mode`, so that a 2.52 or
  2.53 server starts in its compatibility mode, and the client has to
  upgrade it. The shim is a copy of the harness (`ssh-shim/ssh` beside
  the work directory), so it runs on Windows too. The server's path must
  have no spaces, since clients split `servercmd` on them; on Windows it
  is given with `/`, since they take a backslash as an escape.
- **`socket`:** the root is `socket://127.0.0.1:<port>/b`, served by a
  fresh `-socket` server for each sync. The harness waits for "server
  started" and kills the server afterwards: the client's preferences
  replace the server's, `killserver` among them, so the server never
  stops by itself.

The server has its own Unison directory (`unison-server`), its home is the
work directory (so `b` is relative to it), and the host names are fixed
(`UNISONLOCALHOSTNAME` is `client` and `server`). The comparison adds the
server's standard error, kept apart from the client's, and the files in
its Unison directory. The recheck runs the reference's client and server,
so OCaml must accept both hosts' archives.

A pairing with gemel at one end is compared with the same pairing with
`8772d26` in gemel's place, the other end unchanged: for a gemel server,
`--reference <client> --reference-server <8772d26> --candidate <client>
--candidate-server <gemel>`. Releases differ from
one another: a 2.52.1 client prints its summary differently from 2.54,
and a 2.52.1 server sometimes writes no fingerprint cache. 2.53.8 can't
be told apart from 2.54 at either end.

```sh
# Releases for the matrix, in Docker: target/oracle/unison-<version>-linux-<arch>
scripts/build-ocaml-unison.sh /path/to/unison v2.52.1
scripts/build-ocaml-unison.sh /path/to/unison v2.53.8

# 2.54 client, 2.52.1 server, on a socket, against itself
scripts/oracle.sh /path/to/unison --remote socket --count 10 \
    --reference-server /work/target/oracle/unison-2.52.1-linux-<arch>
```

A candidate server that runs unison with `UNISONLOCALHOSTNAME=elsewhere`
fails every scenario, over either transport.

## Recorded outcomes, for Windows

There is no OCaml Unison build on Windows, so CI's Windows job replays
outcomes recorded from the pinned reference, as does its macOS job, which
has no OCaml either: [`recorded/windows.txt`](recorded/windows.txt), 100
Windows-flavour scenarios (no permission changes or symbolic links). The
file is kept byte for byte (`-text` in `.gitattributes`). The oracle job
replays the file with the reference as the candidate, which shows it is
still what the reference does, and with gemel.

```sh
# Record again, in Docker, after a change to scenarios or normalisation
scripts/oracle.sh /path/to/unison --flavour windows --count 100 \
    --record /work/tools/oracle/recorded/windows.txt

# Replay, here with gemel
cargo run -p gemel-oracle -- --replay tools/oracle/recorded/windows.txt \
    --candidate target/debug/gemel
```

[`recorded/windows-socket.txt`](recorded/windows-socket.txt) holds 100
Windows-flavour scenarios with replica `b` on a socket, recorded with
`8772d26` at both ends. Its first lines say so (`remote socket`), and
each step has the server's standard error, and each scenario the server's
Unison directory. CI replays it with OCaml on Linux, and with gemel on
macOS and Windows.

```sh
scripts/oracle.sh /path/to/unison --flavour windows --remote socket --count 100 \
    --record /work/tools/oracle/recorded/windows-socket.txt
```

A replay compares the same things, with what depends on the system
removed from both sides:
- line endings;
- the padding of local times;
- separators in the roots;
- permissions, which OCaml prints as `read-write` or `read-only` on
  Windows;
- the archives' hashes, which cover the roots' paths and the host's name.

With no reference to recheck the final state, the candidate rechecks its
own. That is weaker than a live run: it shows that gemel accepts its own
archives, not that OCaml does. A candidate that runs unison with
`-ignore "Name a"` fails 59 of the 100 recorded scenarios.
