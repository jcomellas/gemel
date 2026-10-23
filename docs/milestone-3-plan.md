# Milestone 3: remote roots

*Status: in progress (phase 5c, 2026-10-23) · Follows milestone 2 (local engine, text UI and watch mode, PRs #7–#29) · Source: `bcpierce00/unison` at `8772d26`*

Milestone 3 is step 3 of the port order in [the analysis](unison-port-analysis.md#4-architecture-recommendation): remote roots. It ends with gemel synchronising with a root on another host over ssh or a socket. It must work in both directions with OCaml Unison 2.52 to 2.54: a gemel client with an OCaml server, an OCaml client with a gemel server, and gemel at both ends. That is the Tier A promise (§3 of the analysis).

## Definition of done

For generated replica trees and change scripts, with one root reached over ssh or a socket:

1. **Every pairing agrees with OCaml's,** on Linux live, on macOS against Homebrew's 2.54.0, and on Windows through recorded outcomes (see Verification). Each of these proposes the same actions and leaves the same trees as the pairing with `8772d26` in place of each gemel end, for each version in the matrix:
   - a gemel client with an OCaml server;
   - an OCaml client with a gemel server;
   - gemel at both ends.
2. **The matrix is OCaml 2.52.1, 2.53.8 and 2.54 (`8772d26`), in both roles, on Linux.** The RPC version handshake works with each:
   - every client from 2.52 on starts an ssh server with `-server __new-rpc-mode` and goes straight to the handshake, so a gemel server needs nothing else, as 2.54's own servers don't;
   - a 2.52 or 2.53 socket server starts in its compatibility mode for 2.51 clients: it sends an old header, then the magic packet, and a gemel client answers it and upgrades, as a 2.54 client does (`checkServerUpgrade`).
3. **Archives interoperate on both hosts.** After any pairing syncs, the other pairings load both hosts' archives, find them consistent and report nothing to do.
4. **Transfers are OCaml's.** Files go as plain contents or as rsync deltas against the destination's old version, in the stream format OCaml's `transfer.ml` reads and writes. An interrupted transfer resumes from the partial file, and a file identical to one already on the destination is copied there instead of transferred.
5. **Connections are usable:**
   - ssh, with OCaml's `sshcmd`, `sshargs`, `servercmd`, `addversionno`, user and port handling;
   - TCP and Unix-domain sockets;
   - ssh's password and passphrase prompts answered at the terminal, where ssh asks them itself, as with OCaml's text interface, on Unix and Windows;
   - a lost connection reported as OCaml reports it.

Out of scope, and how each behaves until it is done:

| Cut | Behaviour in M3 | When |
|---|---|---|
| Synchronising xattrs and ACLs | The `xattrs` and `acl` preferences are registered so that OCaml clients can send them (see phase 1). Setting either to true is refused, as OCaml refuses it when the other side lacks the feature. `propsExtData` is not registered | M5 |
| External copy programs (`copyprog`, `copyprogrest`, `copythreshold`, `finishExternalTransfer`) | The preferences are accepted; a profile that turns external copying on gets a warning that it is ignored, and transfers use the built-in protocol. External rsync is deprecated upstream | Not planned |
| `rsh` (`rshcmd`, `rshargs`) | Marked removed, as in OCaml since 2.52 | Never |
| The self-test commands (`test.ml`: `makeRootEmpty`, `getfs`, `putfs`, ...) | Not registered | Never |
| `dumpArchive`, `inspectFilesystem` (debugging) | Not registered | M5, if wanted |
| GUI, with ssh's prompts in it (a pseudo-terminal or ConPTY, `terminal.ml`, and the session's `openConnection*`) | ssh asks at the terminal, as with OCaml's text interface; OCaml's GUIs alone run ssh under a pseudo-terminal (phase 4's notes) | M4, with the GUI (decided 2026-10-23) |

## Design decisions

These are made now because reversing them later would mean rewriting the connection code.

### 1. Connections run on the engine thread

- **Threading:** a connection is OCaml's `connection`, one per server, owned by the engine thread like the rest of the engine state (`Rc`/`RefCell`, no `Send`).
- **Tasks:**
  - A reader task receives packets and wakes the receivers (`Remote.receive`).
  - A writer drains the output queues, with OCaml's priorities (urgent, normal, idle) and its flow control (write tokens, `halfduplex`).
- **tokio** gains `process`, `net`, `io-util` and `time`. The single-threaded runtime of milestone 2 stays.
- **Blocking work:** reading and writing files for a transfer, and rsync's checksums, run on blocking tasks as copies do now. Only the packets cross back to the engine thread.

### 2. Requests go both ways on one connection

OCaml's `receive` serves requests from the other side on the same connection that carries the replies to its own:
- the server forwards warnings and messages to the client (`showWarningOnClient`, `forwardMsgToClient`);
- `transferFile` runs on the destination host, which calls `compress` on the source host;
- `compress` streams the file back with `processTransferInstruction`.

So each side issues and serves requests, with one message-id space and one receivers map. Phase 1 builds that, not client-to-server calls only, so that phase 4 doesn't need a redesign.

### 3. Loopback becomes an in-memory connection

Milestone 2's loopback dispatch encodes every root call's sections as a server would. Phase 1 replaces it with a real connection over an in-memory duplex pipe: packets, message ids, headers, flow control and the server's command loop, everything but the handshake and the transport. Every milestone 2 suite then runs through the protocol a remote root uses.
- **A separate step.** The swap reruns every suite at once, so it lands without new behaviour, and failures are easy to bisect.
- **Two hosts.** The loopback host names itself as the client, which is right for the local roots it stands in for. A real server names itself after its own host. That name feeds `thisRootsGlobalName` and the archive names. So the in-memory connection can also stand for a server on another host, with a host name of its own:
  - copies between the client's root and the server's then go through `transferFile`, as with a real remote root, which lets phase 2 build transfers before there is a handshake or a transport;
  - server-side archive naming gets tested.

### 4. The server is the `gemel` program

- **Server mode:** `gemel -server` serves on stdin and stdout, and `gemel -socket <port|path>` listens (`-listen` chooses the address). This is OCaml's `beAServer` and `waitOnPort`.
- **New RPC mode:** `-server __new-rpc-mode` skips the compatibility header, as OCaml's servers do when started by a 2.54 client.
- **What the server knows:** it registers the same preferences and commands as the client (milestone 2's `gemel_ui_api::register_prefs` and the engine's commands). So `Prefs.load` accepts what an OCaml client dumps.
- **Headless:** `bins/gemel` keeps zero GUI dependencies.

### 5. Testing doesn't need an ssh server

- **An `sshcmd` shim:** a script given as `-sshcmd` runs the server command locally and ignores the host. With `servercmd` it chooses which program serves: gemel, or one of the OCaml builds.
- **Password prompts:** the same shim can print `user@host's password:` on `/dev/tty` and read the answer there, with the client in a pseudo-terminal the test holds, so the prompt path is tested deterministically.
- **Sockets** are tested on localhost and in a temporary directory.
- **A real sshd** runs in one Linux CI cell at most.

### 6. The default server command is gemel's

- **Default:** `servercmd` defaults to `Uutil.myName`, gemel's own name, as everywhere else. A gemel client starts `gemel -server` on the remote host, and connecting to an OCaml server needs `servercmd = unison`.
- **`addversionno`:** appends gemel's major version (`gemel-0.1`).
- An OCaml client reaches a gemel server through its own `servercmd`, or through a `unison` link to gemel.

## Phases

There is one PR per phase, cut from `main` and merged before the next phase starts, with no stacking. A phase that grows past a reviewable size is split, as phases 4, 6, 7 and 8 of milestone 2 were. Line counts are the OCaml being ported.

### Phase 0: the remote oracle

- **Build the matrix:**
  - 2.53.8 builds as `8772d26` does, in the oracle image (Debian trixie, OCaml 5.3);
  - 2.52.1 links against `bigarray.cmxa`, which OCaml 5 removed, so it builds in a Debian bookworm image (OCaml 4.13.1).

  Both builds were tried on 2026-10-22. `scripts/build-ocaml-unison.sh` takes a tag and writes `target/oracle/unison-<version>-linux-<arch>`.
- **CI's oracle job:**
  - builds the three versions, or caches them by tag;
  - keeps testing against apt's OCaml 4.14 build of `8772d26`, as now.
- **Harness:** the oracle harness (`tools/oracle`) runs scenarios with root `b` remote: through the `sshcmd` shim, or on a socket. For each of client and server it chooses the implementation:
  - gemel or OCaml;
  - which OCaml version.

  It compares a pairing with gemel at either end with the same pairing with `8772d26` in gemel's place: gemel × 2.52.1 with `8772d26` × 2.52.1. Releases differ from each other in what they print and in what they write (see the notes), so comparing with an OCaml version at both ends would report differences that aren't gemel's.
- **Windows:** extends the recorded outcomes for Windows with remote scenarios over a socket, where a gemel client talks to a gemel server.

**Notes (phase 0, 2026-10-22):**
- **Builds:**
  - `scripts/build-ocaml-unison.sh /path/to/unison <tag>` builds a release's text UI in Docker as `target/oracle/unison-<version>-linux-<arch>`: 2.52 with `make text` on bookworm, the others with `make tui` on trixie. Without a tag it builds the checked-out tree as before.
  - CI builds both tags with apt's OCaml 4.14, which builds 2.52.1 too, so it needs no Docker. The binaries are cached by tag and compiler version.
- **Harness** (`tools/oracle`, `--remote ssh|socket`, `--reference-server`, `--candidate-server`):
  - **ssh:** `-sshcmd` is a shim that records its arguments, drops what precedes the server command (`[-l user] [-p port] host -e none`), and runs that command as the server. The argument order is the same in 2.52.1, 2.53.8 and `8772d26`.
    - OCaml kills ssh as it exits (SIGTERM, then SIGKILL 10 ms later), and the server then ends on the end of its input. So the shim runs the server in a subshell the kill doesn't reach, which records the server's exit code, and the harness reads the server's standard error once the server has exited. That matters from phase 3, when a gemel server might write after the client has gone.
    - The arguments recorded aren't checked yet; phase 3 compares those gemel passes with OCaml's.
  - **Socket:** a fresh server for each sync, on a port the system chooses, listening on `127.0.0.1`. The harness waits for "server started", which goes to standard error, and kills the server afterwards: the client's preferences replace the server's, `killserver` included, so it never stops by itself.
  - **The server's own state:**
    - a Unison directory of its own;
    - its home is the work directory, where the root `b` is;
    - host names `client` and `server` (`UNISONLOCALHOSTNAME`);
    - its standard error is compared on its own, since ssh would otherwise interleave it with the client's;
    - its Unison directory is compared too.
  - **The recheck** runs the reference pairing on the candidate's final state, so OCaml must accept both hosts' archives.
- **What the releases do** (Linux, five scenarios of each pairing against `8772d26` at both ends):
  - 2.53.8 can't be told apart from `8772d26` at either end, over ssh or a socket.
  - A 2.52.1 server leaves no fingerprint cache in its Unison directory in one scenario of five.
  - A 2.52.1 client prints its summary differently ("Propagating updates" on standard error, a line about donations) in every scenario.
  - Every pairing agrees with itself, and the socket servers of 2.52.1 and 2.53.8 talk to `8772d26` clients in their compatibility mode.
- **CI's oracle job:**
  - `8772d26` at both ends, 20 scenarios over each transport;
  - each release at each end with `8772d26` at the other, 10 scenarios each, each pairing against itself, until gemel can take an end;
  - the recorded socket outcomes for Windows replayed with `8772d26`.
- **Windows:** `recorded/windows-socket.txt` holds 100 Windows-flavour scenarios on a socket, recorded with `8772d26` at both ends. Gemel replays them from phase 4a, when it can serve on a socket. Homebrew's 2.54.0 on macOS replays them, 100 of 100.
- **Sensitivity:** a candidate server run with another host name differs in every scenario, over each transport.

### Phase 1: packets and dispatch (`remote.ml`, ~1.2k)

- **Packets:**
  - packets (`receivePacket`, `intSize` lengths), message ids, the receivers map and `wait_for_reply`;
  - the header kinds: request, normal result, transient and fatal error, stream abort.
- **Output queues and flow control:** `makeOutputQueue`, `performOutput`, `enableFlowControl`, `negociateFlowControl` and `halfduplex`.
- **Registration and dispatch:**
  - `Dispatch::Remote`: a root's commands go to the connection of its host. `ClientConn` keeps one connection per server, shared by the roots there;
  - `registerServerCmd`, `registerRootCmdWithConnection` and `commandAvailable`;
  - serving requests from the other side (design decision 2).
- **Connection lifetime:**
  - `lostConnection`, the close handlers (`at_conn_close`), and the resources released with a connection (`resourceWithConnCleanup`, `lwtRegionWithConnCleanup`);
  - the connection check (`checkConnection`).
- **Loopback swap:** loopback becomes an in-memory connection (design decision 3), with no new behaviour. Every milestone 2 suite runs through it, and the swap is its own commit. The in-memory connection can also stand for a server on another host.
- **`xattrs` and `acl`:**
  - **The problem:** an OCaml client sends both whenever the xattr and ACL features are negotiated, and gemel registers those features for their archive format. A server that doesn't know a preference refuses the client: "Preference xattrs not found: inconsistent Unison versions??".
  - **The fix:** gemel registers both, with OCaml's `send` checks. Setting either to true is refused with OCaml's own validators' message ("... but the server does not support this"), until M5.
  - **Where:** in `props.ml`'s place in OCaml's link order, since the registration order is the order of the preference dump. `prefs_dump.txt` is checked against it, or regenerated.
  - **What else changes:**
    - `-help` lists them, so `batch.rs` stops filtering `-acl` and `-xattrs` out;
    - a gemel client sends them to OCaml servers when the features are negotiated, which those accept.
- **Tests:** dispatch, flow control and lost connections over the in-memory connection, and every milestone 2 suite through it.

**Notes (phase 1, 2026-10-22):**
- **Commits:** `xattrs`/`acl` first; then the connection layer, with loopback keeping its round trip through the codecs; then the swap on its own, which changes nothing but loopback's path.
- **`xattrs` and `acl`:**
  - **Four, not two:** `xattrignore` and `xattrignorenot` have the same send condition as `xattrs`. Milestone 2 registered them in `update.rs` without it, so gemel sent them to every server, and a 2.52 server, which has neither the features nor these preferences, would have refused them. All four are now sent only with their features, in `props.ml`'s place in the order: gemel-fs registers up to `xattrs`, gemel-engine the two predicates, then gemel-fs `acl`.
  - **Refused:** each feature's validator refuses its preference set to true, enabled or not. A gemel server loading `xattrs = true` from an OCaml client doesn't refuse it yet; phase 3 decides whether its `installPrefs` should.
  - **`prefs_dump.txt`** covers six other preferences and is unchanged.
- **Connections** (`gemel-rpc`'s `connection` module):
  - **Each side is an `Endpoint`:** its commands, its host and its message ids (2 up to 10⁹, then 2 again). OCaml's are a process's globals; the client and a loopback server share a process here. Replies are kept per connection.
  - **A writer task** drains the queues: urgent writes at once and flushed, normal then idle ones only with the token, then a flush after a yield, handing the token over first when flow control is on. A token that crosses the negotiation (one that arrives after this side turned flow control off) is ignored, where OCaml would assert.
  - **The receiver** spawns a task for each request from the other side. It doesn't yield first, as OCaml does: tokio wakes tasks that yielded in the same tick last first, which started two roots' scans in reverse and swapped the transfer hints' order.
  - **Errors:** a fatal error from the other side becomes `Server: ...`, as in OCaml; loopback strips it, since it stands for local roots. A request for a command the server lacks gets a fatal error back, where OCaml's server stops and its client sees a lost connection.
  - **A lost connection** (the end of the input, or any error reading or writing) fails every waiting request with "Lost connection with the server", runs the connection's close handlers, and ends `serve` quietly.
  - **Streams:** a chunk for a stream the server doesn't know is aborted with an urgent `StreamAbort`, once, as OCaml does. `registerStreamCmd` comes with transfers (phase 2).
  - **Checked against OCaml:** a session of Homebrew's 2.54.0 over a `tee`d ssh shim shows the server's token right after the handshake, the client's requests 2 and 3 then its token, no tokens once negotiated, and replies out of order. The tests' scripted peers expect that order.
- **Dispatch** (`registry`): `ServerCmd` is `registerServerCmd`; `Rpc::call_root_with_connection` is `registerRootCmdWithConnection`'s client side; `commandAvailable` and `negociateFlowControl` are registered with every registry. `ClientConns` is `ClientConn`, and `Rpc::connection_checked` is `ofClrootConncheck`. A remote root without a connection gets "No connection with the server".
- **Close handlers:** `Host::at_conn_close` and `Connection::at_close`. `resourceWithConnCleanup` and `lwtRegionWithConnCleanup` come with their first users: file descriptors and regions with transfers (phase 2), and locks with a socket server's successive clients (phase 4).
- **The swap:**
  - The tests' `block_on` keeps one runtime and `LocalSet` per thread, as the engine thread has one: a loopback connection's tasks, started in one call, must live into the next.
  - A loopback host goes away once its connection's tasks have seen the connection closed, not when the client drops it. The archive test that checks a dropped host's locks lets them run first.
- **Two hosts:** `Rpc::connect_in_memory` connects to a host standing for another machine, named with `Host::set_host_name`. A test checks that such a server names its root's archives after itself, in its own Unison directory.

### Phase 2: transferring contents (`copy.ml`'s transfers, `transfer.ml`, ~1.2k)

Built over the two-host in-memory connection (design decision 3), so that the next phases can sync end to end.
- **Transfers:**
  - `transferFile` (a root command with the connection) and `compress`;
  - `registerStreamCmd`, the `stream` preference (milestone 2 left `maxThreads` reading it as always on), and `processTransferInstruction`.
- **Contents:**
  - `Transfer.send` and `receive` for plain contents;
  - the rsync algorithm: `Rsync.rsyncPreprocess`, `rsyncCompress`, `rsyncDecompress`, and the block sizes and probabilities behind them;
  - resource forks.
- **Golden vectors:** rsync's token stream and checksums must be bit-exact for OCaml to decode them. So vectors are generated from OCaml's `transfer.ml` with milestone 1's fixture scripts, beside the syncs.
- **Tests:**
  - the golden vectors;
  - transfers of each size class (below the rsync threshold, deltas, appends) in both directions between the two in-memory hosts;
  - every milestone 2 suite with one root on the other host (phase 2c).

**Notes (phase 2a, 2026-10-22):**
- **Split in two:** 2a is `transfer.ml` and `checksum.ml` on their own, synchronous and tested against OCaml's vectors; 2b is streams, `compress` and `transferFile`, the engine's copies between hosts, and the suites with a root on the other host.
- **Vectors** (`tools/ocaml-fixtures/gen_transfer.ml`, built against the real `transfer.ml` and `checksum.ml` with Unison's Lwt and stand-ins for `Trace`, `Uutil` and `Safelist`): checksums and rolls, `sizes` around the block-size roundings (2^(2k+1) ± 1) up to 1 TiB, and twelve rsync cases and seven plain ones. For each, the block information marshalled, and every chunk sent, as its length, MD5 and token counts. A 64-bit LCG, written the same in OCaml and Rust, makes the inputs, and each case records their MD5s.
- **What the vectors pin down:**
  - chunks break where OCaml's queue does: 65 500 bytes, a string's flush decided on its whole length, the end token in a chunk of its own when the queue is full;
  - blocks merge up to 255;
  - a bucket's blocks are tried highest first, so a zero-filled old file matches its last zero block every time, and those tokens don't merge.

  Breaking either of the last two fails the test.
- **Differences from OCaml:** a chunk that isn't a valid stream, or a block past the old file's end, is a transient error, where OCaml asserts or writes whatever its buffer held. Block information whose counts don't fit its checksums is refused with OCaml's message.
- **For 2b:**
  - `processTransferInstruction`'s section is the tag, the encoded file id, then the raw chunk, with no umarshal frame, so it needs a raw variant of `encode_section`;
  - OCaml waits for each idle send to be buffered before the next chunk and before the `Ping`. Phase 1's sends don't wait, and normal messages go before idle ones, so the `Ping` and `compress`'s reply could overtake chunks. Idle sends must be awaitable;
  - stream chunks are decoded in order within the receive loop, as OCaml awaits `processStream`;
  - the file id comes from the endpoint's message ids;
  - the client serves `compress` too, when the destination root is remote;
  - `streamReg`, `rsyncReg` and `transferFileReg` (only with `stream` off), and the transfers' file descriptors, are where `lwtRegionWithConnCleanup` and `resourceWithConnCleanup` come in;
  - the decompressor copies blocks by reading and writing; OCaml's `copyFileRange` (`copy_file_range`) comes with the destination's file handling;
  - phase 5's pieces (`validFilePrefix`, `tryCopyMovedFile`, `checkForChangesToSource` across hosts) keep their call sites and return nothing.

**Notes (phase 2b, 2026-10-22):**
- **Split again:** 2b is streams, `compress`, `transferFile` and the engine's copies between hosts, tested between two in-memory hosts; 2c runs every milestone 2 suite with root `b` on the other host. 2c needs a third setup mode in the suites' support, and a host per root wherever they look at a host's state.
- **Streams** (`StreamCmd`, `Connection::stream`):
  - chunks go as idle messages, each waited for until it is written (`send_and_wait`), so the ping and the reply after it can't overtake them;
  - the receiver runs them in order, in its loop; one that fails aborts the stream (an urgent `StreamAbort`, once), the sender stops at its next chunk, and the ping brings back the error;
  - one stream at a time leaves each side (`streamReg`, a semaphore on the endpoint);
  - with `stream` off, each chunk is a request. `maxThreads` already read `stream`.
- **Transfers** (`copy.rs`):
  - the source reads on a blocking task, outside the host's lock, and the chunks come back over a channel of four; the destination decodes each chunk under the lock;
  - a transfer's state lives on the destination's host, by the id the destination gives it, and goes when `compress` returns, as do its files. Failures propagate, and dropping releases the files and the stream's turn, so neither `resourceWithConnCleanup` nor `lwtRegionWithConnCleanup` is needed yet;
  - `fileIsTransferred` is ported (a whole temporary file only gets its props);
  - connections count the bytes they emit and receive (`emittedBytes`, `receivedBytes`).
- **Checked against OCaml:** two syncs by Homebrew's 2.54.0 over a `tee`d ssh shim, the second with rsync both ways (`crates/gemel-engine/tests/captured/transfers.txt`). Every `transferFile`, `compress` (with and without block information), stream chunk and ping section decodes with gemel's codecs and encodes back to the same bytes.
- **Moved:**
  - `rsyncReg` and `transferFileReg` (memory and concurrency limits on transfers) go to phase 5, with items running concurrently;
  - copying blocks with `copy_file_range` in the decompressor goes to phase 5;
  - `copyprog` with an OCaml server: a gemel server ignores external copying with a warning, but an OCaml server decides from the client's preferences and can answer `EXTERNAL`, which the gemel client turns into an error for that file. Phase 3 sends `copythreshold = -1` to servers after the warning.

**Notes (phase 2c, 2026-10-22):**
- **Three ways** (`Via` in the engine suites' support): direct, loopback, and remote, where root `b` is `ssh://server//...` behind an in-memory connection to a host named "server", which keeps its archives in the same Unison directory. `archives`, `commit`, `copy`, `files`, `recon`, `transport`, `update` and `watch` run each test all three ways.
- **The remote root is installed as a real one will be:** `Roots::install` canonizes it over the connection to its server (`canonizeRoot`'s second half: `canonizeOnServer`, then the connection is registered under the canonical root). Only opening the connection waits for phase 3.
- **OCaml is run the same way:** with a remote root, the suites' OCaml runs reach root `b` as `ssh://server//...` through an `-sshcmd` shim that starts an OCaml server named "server", so OCaml sees the archives gemel's two hosts wrote. Reconciliation, propagation, moves, failures and the archives agree with OCaml's in remote mode.
- **What the suites found:**
  - **Concurrency:** with a remote root, items ran up to `maxthreads` at once, which milestone 2 found unsafe with `exclusive` locking pieces, not commands: an abort from another thread no longer stopped items that hadn't started. They run one at a time until phase 5. OCaml runs them concurrently, so its log interleaves `[END]` lines with the next items' `[BGN]`; until phase 5, the remote comparison takes the `[END]` lines without their order.
  - **Error wording:** a transfer's destination opens its file as the first chunk is decoded, so OCaml's error reads "Error in processing a transfer instruction"; gemel used the source's wording.
  - **A stray log:** phase 2b's two-host tests logged in the crate's directory, and committed `unison.log`; they log in their own now.
- **Left out of remote mode:**
  - merging (`files`' and `transport`'s merge tests), which is phase 5;
  - one stasher test, where both hosts back up the same file into one shared backup directory, which real hosts don't share.
- **Not yet:** the user interface's suites (`gemel-ui-api`'s `startup` and `session`, and `bins/gemel`'s) start from a profile, so their remote roots need a real connection: phase 3, with the oracle's remote mode.

### Phase 3: the handshake, the server and spawned connections (`remote.ml`, ~1.2k; `main.ml`'s server mode)

- **Handshake:**
  - the connection header and the version handshake: `VERSIONS`, `VERSION`, `OK`/`NOK`, `selectServerVersion`;
  - on the client, a 2.52 or 2.53 socket server's compatibility mode: its old header and the magic packet, answered with the upgrade to RPC version 1 (`checkServerUpgrade`, `checkForMagicString`);
  - the negotiation timeout (120 seconds).
- **Server:**
  - `commandLoop` with the new handshake only (as 2.54's), `checkClientVersion`, `beAServer` (the server changes to `HOME`) and `-killserver`;
  - feature negotiation and the preference dump over the connection (`negotiate.ml`, already used in loopback);
  - `showWarningOnClient` and `forwardMsgToClient`.
- **Spawned connections** (`buildShellConnection`), without a pseudo-terminal yet:
  - `sshcmd`, `sshargs` (with OCaml's backslash escapes), `servercmd`, `addversionno` (design decision 6), `-l user` and `-p port`;
  - the server's standard error forwarded.

  This is what the `sshcmd` shim needs (design decision 5).
- **`canonizeOnServer`** for remote roots.
- **Tests:**
  - gemel clients against OCaml servers and OCaml clients against gemel servers, for the three versions, through the shim, with whole syncs from the oracle harness;
  - the compatibility mode, through a shim that starts a 2.52 or 2.53 server without `__new-rpc-mode`.

It is split in two, since gemel's server can be tested against OCaml clients before gemel has a client to go with it:
- **3a, the server:**
  - `gemel -server`: the handshake's server side and `commandLoop`, `beAServer` and `catch_all`;
  - warnings and messages forwarded to the client;
  - tests: the server's side of the handshake against OCaml's, byte for byte, and the oracle harness with OCaml clients of each version and a gemel server.

  `-killserver` only matters to socket servers, so it goes to phase 4 with them.
- **3b, the client:**
  - the handshake's client side, with the compatibility mode and the timeout;
  - spawned connections;
  - `canonizeRoot` opening them, and `isRootConnected`;
  - `copythreshold = -1` sent to servers (phase 2b's notes);
  - tests: gemel clients against OCaml servers and against gemel's; the arguments the shim records compared with OCaml's.
- **3c, the user interface's suites with a remote root** (phase 2c's notes): `gemel-ui-api`'s `startup` and `session`, and `bins/gemel`'s, about 56 tests. They were part of 3b, which grew large enough without them.

**Notes (phase 3a, 2026-10-22):**
- **The server:** `gemel -server` (with `__new-rpc-mode`, an anonymous argument, ignored) creates the Unison directory, changes to `HOME`, and serves on standard input and output with a host of its own: every preference and command registered, the preferences at their defaults until the client sends its own (`installPrefs`), its trace marked as a server's.
  - **Its output is the connection,** so its trace writes to standard error. It writes nothing to its Unison directory that OCaml's doesn't: its log entries go to the client, as OCaml's do.
  - **Failures** are shown as `catch_all` shows them (`Unison server failed: ...`) and exit with 1. OCaml raises `Sys_error` for `chdir`, so that message is `path: reason`, not `reason [chdir(path)]`.
- **The handshake's server side** (`gemel_rpc::handshake`) reads from the buffer the connection then reads its packets from (OCaml's `inputBuffer`), so nothing sent right after a line is lost, and the rest of what's buffered is what OCaml's messages quote.
  - A refused version gets `NOK` and the reason, and the server goes on serving, as OCaml's does, until the client hangs up.
  - OCaml's server yields as it turns flow control on, so its write token goes out before it reads anything; gemel's waits for its writer to do the same.
  - A client that sends nothing gets nothing from OCaml's server, whose header is still buffered when it sees the end of its input. Gemel's flushes its header first. No client hangs up before it has read the header, so that's left as it is.
- **Warnings and messages** go to the client (`showWarningOnClient`, `forwardMsgToClient`), each queued as a request as it is made, so that it goes out before the reply to the request that caused it. OCaml waits for each one's reply; gemel doesn't, which only changes timing.
  - A trace crosses threads, so its forwarder holds a key to its connection in a table on the engine thread, not the connection itself (OCaml's forwarder is a global).
- **`tryCopyMovedFile` and `registerFileTransfer`** came forward from phase 5. OCaml clients see them:
  - with a gemel server, a file moved on the server's side was sent again rather than copied there, and the log lacked OCaml's `Shortcut: copied ...`;
  - with items running concurrently, a transfer of contents another transfer is bringing waits for it, then copies its temporary file.
  - OCaml's `try` around the copy is synchronous, so it catches errors reading and copying the candidate, which then fall back to a transfer, and not errors setting the copy's props or checking it, which come after a bind and fail the transfer. Gemel's does the same.
- **Concurrency:** an OCaml client runs items concurrently by default, which a server can't stop (the gemel client runs them one at a time until phase 5). The oracle's `--concurrent` lets them, and compares what the syncs print without its order. OCaml's order is deterministic (it agrees with itself), and gemel's server answers in another, since its file work runs on blocking threads. The order is left for phase 5, with the gemel client's concurrency.
- **A reply larger than the output buffer could stay unflushed:** the writer took an empty buffer to mean nothing was left to flush, but a large message goes past the buffer into the output. Standard output holds what follows its last newline, so a 23 KB reply to `find` waited there until something else was written, sometimes for minutes. An in-memory pipe holds nothing, so no test saw it. The writer now remembers what it wrote since it last flushed; `connection.rs`'s tests check it on an output that holds everything until it is flushed.
- **Against OCaml:**
  - the server's output for each kind of first line, and its failures, byte for byte (`bins/gemel/tests/server.rs`, against literals recorded from 2.54, and against OCaml's server when there is one);
  - whole syncs from the oracle harness, an OCaml client with a gemel server against the same client with `8772d26` as its server: on Linux, 50 of 50 scenarios with an `8772d26` client, and 30 of 30 with each of 2.53.8 and 2.52.1; on macOS, 40 of 40 with Homebrew's 2.54.0, and 60 of 60 with `--concurrent`. CI runs 50, then 20 for each release, each way;
  - a larger sync by hand, concurrently, with an OCaml client: 700 files, 3 to 5.5 MB files edited in the middle (rsync), moves, and contents in several places. Gemel's server and OCaml's leave the same trees, times and permissions, copy the same 20 files rather than send them, and OCaml's recheck of each result agrees.
- **Left for phase 4:** `-socket`, and with it `-killserver`, which only socket servers read.

**Notes (phase 3b, 2026-10-22):**
- **The handshake's client side** (`gemel_rpc::handshake`): the header read a byte at a time against the current one and both old ones; a server in its compatibility mode upgraded (its magic packet read, its write token taken, the packet sent back, and the handshake started over); `VERSIONS` parsed as OCaml parses it, the newest common version sent, and `OK` or `NOK` read. The end of the input while the client waits for the server is a lost connection, as in OCaml.
- **Spawned connections** (`gemel_rpc::shell`, `buildShellConnection` without a terminal):
  - ssh's command line as OCaml builds it: `sshcmd` (not split), `-l user`, `-p port`, the host, `-e none`, `sshargs` and the server's command, each split into words, with backslash escapes;
  - the server's command is `servercmd`, or `gemel`, with `-<major version>` if `addversionno` (`gemel-0.1`), then `-server __new-rpc-mode` (design decision 6);
  - ssh starts with `SIGINT` blocked (`gemel_fs::signal::block_interrupt_in_child`, the one piece that needs `unsafe`), its standard error is the client's, and it is waited for once the connection closes;
  - the handshake has OCaml's 120 seconds. The engine thread's runtime now has tokio's I/O and time drivers, for the process and the timeout.
  - **Left for phase 4:** a pseudo-terminal for ssh's prompts, and killing ssh as the client exits while ssh talks to the terminal (OCaml's `at_exit` handler). Without a terminal, ssh ends when its input does.
- **`canonizeRoot`** opens a connection when there is none for the root, and registers it under the canonical root; a socket root says sockets aren't supported yet. `connectRoots` counts a remote root as connected while its connection is up (`isRootConnected`), so a repeat or watch cycle reconnects only once it is lost.
- **"Waiting for changes from server":** `findUpdates` runs the roots with OCaml's waiting action (`allRootsMapWithWaitingAction`), which shows that status once the client's own scan has ended and the server's hasn't.
- **External copying:** a remote host gets the client's preferences with `copythreshold = -1`, so an OCaml server never hands a file to `copyprog`; a profile that turns it on gets the warning that it is ignored, once.
- **The oracle:** ssh's arguments are compared (the server's path aside), and `--old-server` drops `__new-rpc-mode` in the shim, so that 2.52 and 2.53 start in their compatibility mode.
- **Against OCaml:**
  - every way a shell or a server can be wrong, with OCaml's client in the same place (`bins/gemel/tests/client.rs`): no shell, a shell that prints something else or dies, an old server with and without the upgrade, `NOK`, versions it doesn't speak, a refused version;
  - whole syncs, a gemel client against `8772d26`'s client with the same server: on Linux, 30 of 30 scenarios with each of `8772d26`, 2.53.8 and 2.52.1 as the server, 10 of 10 with 2.53.8 and 2.52.1 in their compatibility mode, and 30 of 30 with gemel at both ends; on macOS, 20 of 20 against Homebrew's 2.54.0 and with gemel at both ends.
  - With `--concurrent`, a gemel client agrees on trees and archives but not always on its output (18 of 20), since it runs items one at a time until phase 5; CI doesn't run that pairing.

**Notes (phase 3c, 2026-10-23):**
- **A remote way for the user interface's suites:** `ui_support::Pair::remote` puts root `b` at `ssh://server//...`, reached through a shim in the pair's directory that runs the server here, named "server", with the pair's Unison directory and home set in the shim (an in-process client's server would otherwise inherit the test's environment, and its real `~/.unison`).
  - **`gemel-ui-api`'s suites** run the client in-process, so their server is OCaml's, and the remote way runs only with OCaml Unison, on Unix: the profile syncs of `startup` and `session` (three ways now), and `session`'s progress, status, props, conflict and action tests. `startup` checks that a remote root is connected once, and again once its connection is closed (`isRootConnected`).
  - **`bins/gemel`'s suites** run gemel's twin with the gemel program as its server, and OCaml's with OCaml's (`common::twins_each`): four batch tests, four interactive ones, both repeat tests, the raw-mode keys and the main watch test, where the server runs its own watcher. Without OCaml, the gemel runs are still checked, so the test jobs run gemel at both ends.
  - **Left with both roots here:** merging and differences with a remote root (phase 5), and the tests about profiles and options, which don't reach the roots.
- **Comparisons:** with a remote root, the log's and stderr's `[END]` lines are compared without their order, since gemel's client runs items one at a time until phase 5 (as in phase 2c). For the same reason, a test with two new files of the same contents gives them different contents: OCaml's client would copy the second from the first's temporary file.
- **What the suites found:**
  - **Warnings before the scan:** the session asked about warnings at the end of the update step, where OCaml's text interface asks as each is given. With a remote root, the client then scanned, and said it waited for the server, before the warning that there are no archives. The session now asks once the archives are loaded, before the scan (`update::find_loaded_updates`), which also spares a first sync of a large replica a whole scan before the question.
  - **Closing a root's connection** forgets it at once, as OCaml's `clientConnClose` does, so `connectRoots` connects again; the close handlers still run when the connection ends.
  - **Test runtimes** have tokio's drivers now, as the engine thread's do, and drop their tasks before the runtime, whose drivers wake them as they shut down. The ssh connection's close handler drops the process rather than starting a task to wait for it (tokio reaps it), since it can run as the engine thread ends.
- **A flaky test, once:** on macOS, `copy.rs`'s `resource_forks_are_copied` read a resource fork's length as other than 17 right after writing it, during a whole-workspace run; alone, and in three runs of its suite, it passed. It isn't changed here.

### Phase 4: sockets, and ssh at the terminal (`remote.ml`'s sockets, ~0.4k)

- **Sockets:**
  - TCP, with OCaml's address resolution and its order of attempts (`buildSocket`), and Unix-domain sockets;
  - the server side: `waitOnPort`, cleaning up its socket file, `-listen` and `-killserver`.
  - The manual's warning that the TCP method is insecure goes into gemel's documentation. OCaml prints none at run time.
  - Against 2.52 and 2.53 socket servers, this is where the compatibility mode of phase 3 is met.
- **ssh at the terminal:** OCaml's text interface connects without a pseudo-terminal (`connectRoots` without `termInteract`): ssh asks for passwords, passphrases and host keys at the terminal itself, with Ctrl-C blocked, and the client ends it as it exits should it still run (`end_ssh`). Gemel's client did the first since phase 3b; the second is here.
- **Prompts through the user interface:** a pseudo-terminal for ssh, `terminal.ml`'s handling of what it prints (`handlePasswordRequests`, its escape sequences), and the session's `openConnectionStart`, `Prompt`, `Reply`, `End` and `Cancel`. Only OCaml's GUIs use them (`uigtk3`'s `termInteract`, the Mac bridge), so they move to milestone 4 with gemel's GUI.
- **Tests:**
  - sockets on localhost and on a path, against each version, both ways, with the oracle;
  - the servers' and clients' failures against OCaml's;
  - a shim standing in for ssh that asks for a password at the terminal (design decision 5);
  - one real sshd cell in Linux CI, moved to phase 6 with the other odds and ends.

It was split in two:
- **4a, sockets,** ending ssh at exit, and ssh's prompts at the terminal tested (PR #40).
- **4b, prompts through the user interface,** moved to milestone 4 with the GUI, their only user (decided 2026-10-23, once 4a was merged). Phase 4 ends with 4a.

**Notes (phase 4a, 2026-10-23):**
- **The client** (`gemel_rpc::socket`, `buildConnectSocket`) tries the host's addresses in the resolver's order (`getaddrinfo`, in `gemel_fs::net`, the one new piece of `unsafe`); one whose family or protocol the system lacks (`EAFNOSUPPORT`, `EPROTONOSUPPORT`, `EINVAL`) is skipped, and any other failure ends the attempt, as in OCaml. A path in braces is a Unix-domain socket.
  - **A connection is checked as Unison's own Lwt checks it** (`check_socket`): once it can be written to, it is made if it has a peer, and otherwise a read says why not. That gives OCaml's messages on each system: on macOS a refused connection has no peer with `EINVAL`, so the address is skipped and the message names no address (`Failed to connect to the server on host 127.0.0.1:5000`), where Linux says `ENOTCONN`, then that it was refused.
  - A path too long for a socket's address fails with `ENAMETOOLONG`, as OCaml's does, where the standard library has an error of its own.
- **The server** (`gemel -socket <port|path>`, `-listen`, also `-host`): `waitOnPort` without changing to the home directory (only `-server` does). It binds every address of each host (IPv6 ones IPv6-only, addresses reused, one pending client), reports the first failure if any, says `server started`, and serves one client at a time.
  - **Each client gets a new host,** so nothing of one client's (preferences, caches, locks) reaches the next; the close handlers run between clients unless the client set `killserver`, which stops the server after it.
  - Accepting goes on after the errors OCaml's `accept_retry` lists, with its message; keepalive is on for TCP clients.
  - **Ctrl-C** stops it with 0, and the socket file it made goes however it stops. OCaml's does that on Linux; Homebrew's 2.54 on macOS doesn't stop on Ctrl-C at all, so the test checks OCaml's on Linux only.
- **ssh at the terminal:** the client ends ssh as it exits, should it still run (`end_ssh`: `SIGTERM`, then `SIGKILL` 10 ms later). Ctrl-C while ssh waits for a password ended gemel at once but left ssh running, since ssh has Ctrl-C blocked. OCaml's text interface (2.54) didn't stop on that Ctrl-C within 20 seconds.
- **ssh's prompts at the terminal** are tested (`bins/gemel/tests/terminal.rs`): with the client in a session whose controlling terminal is a pseudo-terminal, a shim standing in for ssh asks for a password on `/dev/tty` without echo, and runs the server once it is right. Gemel connects before it sets the terminal up for its keys, as OCaml does, and reads keys only when it asks for them; both end the same.
- **The pseudo-terminal isn't here,** nor `terminal.ml`'s prompts or the session's `openConnection*` (see above): OCaml's text interface never uses them.
- **Against OCaml:**
  - the servers' failures, byte for byte (a port in use, a path there already or too long, a host not found, a port that isn't one), and the clients' (refused, no socket, a stale socket, a host not found), with 2.54 on macOS and `8772d26` on Linux (`bins/gemel/tests/socket.rs`); a server of each serving gemel and OCaml clients in turn, until `-killserver`;
  - whole syncs: on Linux, 30 of 30 scenarios with an `8772d26` client and a gemel server, alone and concurrently, 10 of 10 with each of 2.52.1 and 2.53.8 as the client, 30 of 30 with a gemel client and an `8772d26` server, 10 of 10 with 2.52.1 and 2.53.8 servers (in their compatibility mode, since nothing on a socket asks for `__new-rpc-mode`), and 30 of 30 with gemel at both ends; on macOS, 20 of 20 for each pairing with Homebrew's 2.54.0;
  - the 100 recorded Windows-flavour socket scenarios, 100 of 100 on macOS; the macOS and Windows test jobs now replay them with gemel.
  - A client killed during a sync leaves no lock on a socket server, of either kind, for the next client.

### Phase 5: the rest of transfers, and concurrency (`copy.ml`, `transport.ml`, ~0.5k)

- Resuming a partial transfer: `fingerprintSubfile`, `validFilePrefix` and the append threshold.
- Copying a moved file instead (`tryCopyMovedFile`, with `xferhint`, and `registerFileTransfer`): done in phase 3a, since OCaml clients see it.
- `checkForChangesToSource` across hosts: done in phase 3a, with transfers.
- Diff and merge with a remote root, copying the other side's file to a local temporary file.
- **Concurrency:**
  - transport runs items concurrently with a remote root (`maxthreads`);
  - before that, `exclusive` is revisited. Milestone 2's phase 6c note says its locking per piece is only safe while items run one at a time, so it holds the lock for a whole command, or OCaml's yield points are matched.
- **Tests:**
  - an interrupted transfer resumed;
  - remote diff and merge;
  - concurrent items, with the log order checked against OCaml's, for a gemel server too (phase 3a's notes).

It is split in three, since concurrency changes how the engine yields and wants a review of its own:
- **5a:** resuming transfers, and diff and merge with a remote root (PR #41).
- **5b:** the server's side of concurrency, which OCaml clients already use: a host's work done at OCaml's yield points instead of `exclusive` locking pieces, OCaml's output queue, and `rsyncReg`.
- **5c:** the client's side: items run concurrently with a remote root (`maxthreads`), after the abort bug of phase 2c's notes is understood, with `copyReg` and `transferFileReg`, `doTransportFailCleanup`, the rsync decompressor copying blocks with `copy_file_range` (phase 2b's notes), and the tests' and the oracle's comparisons without order dropped where gemel's client now runs items as OCaml's does.

**Notes (phase 5a, 2026-10-23):**
- **Resuming** (`validFilePrefix`): the destination keeps the temporary file an interrupted transfer left if it is a file of at least 1 MiB, shorter than the source, and the source starts with it. It asks the source for the digests of that much of its file (`fingerprintSubfile`, 1023 MiB at a time, the last chunk first, as OCaml lists them), and on any failure there transfers the whole file. Then it receives the rest (`` `DATA_APPEND ``), which the source and the destination already handled.
  - **Gemel didn't answer `fingerprintSubfile` before.** An OCaml destination resuming a transfer from a gemel source got an error back, which it takes to mean nothing can be kept, so it transferred the whole file.
  - **The tests can't see a resumed transfer in the trees,** which end the same either way. `bins/gemel/tests/transfer.rs` leaves the temporary file with a hard link to it outside the replicas: a resumed transfer completes the linked file, and one that starts again removes it. The file's name holds the destination root's archive hash, so the test leaves one under each root's.
  - **Onto an old version too,** where the rest comes as rsync deltas against the old file from where the transfer resumes (`DATA_APPEND` moves where both ends read it). Nothing had run that before, since no transfer resumed.
  - Against Homebrew's 2.54.0 on macOS and `8772d26` on Linux: gemel and OCaml in each role, to the server and to the client, onto a new file and onto an old version, with a prefix of the source, one that isn't, and one too short, all as OCaml at both ends does.
  - Not tested past 1023 MiB, where the digests come in chunks and their order counts.
- **Diff with a remote root** copies the remote version next to the local one as a temporary file (`-diff` added to its name), runs the command, and removes it. OCaml's two cases differ, and gemel follows each: with the remote root first, the temporary file isn't removed before the copy, and the arguments come the other way round, so the older version still comes first for `diff -u OLDER NEWER`.
- **Merge with a remote root** swaps the roots if the first is remote, so the merge happens next to the local version, as OCaml does. Gemel's merge already copied the second root's version over whatever connection it had.
- **The tests** (`files.rs`, `transport.rs`, `interactive.rs`) now run merges and diffs with a remote root, given first or second (through the command line), against OCaml. Gemel's test helpers now show temporary files' archive hashes as `H`, since the twin pairs' roots differ.
- `checkForChangesToSource` was already called on the source's root, from phase 3a.

**Notes (phase 5b, 2026-10-23):**
- **The measure** is an OCaml client running items concurrently against a gemel server, compared with the same client against an OCaml server *with* the order of what it prints (the oracle's new `--ordered`). OCaml agrees with itself in that order, so a gemel server's must come out the same. Before this phase, 15 of 20 scenarios did on macOS; each difference below was found by tracing both runs' messages (`--client-args '-debug remote+' --dump`).
- **A host's work runs at once** (`exclusive::now`), on the engine thread, where it ran on blocking threads under a host lock. OCaml's root commands run without yielding until their next request to the other side, so a command is atomic (milestone 2's phase 6c note) and replies go in the order the requests came. On threads, a symlink's reply overtook a copy's. This also covers the rescans that check a target hasn't changed (`checkNoUpdates`), which ran on a thread of their own.
  - **A copy between local roots** still runs on a thread, so that an abort reaches it (the session sets the abort on the engine thread). Items between local roots run one at a time, so nothing else of the host's runs meanwhile.
  - **The update scan** still runs on a thread, as before; it doesn't overlap transport.
- **`compress` reads in step with its sends:** OCaml reads a chunk without yielding and yields only to send it. The reading runs on a thread, but the engine thread waits for each chunk and the reader for each send, which keeps OCaml's yield points without rewriting the rsync encoder as a coroutine.
- **OCaml's output queue** (`performOutput`, `performOutputRec`, `popOutputQueues`, `maybeFlush`): a message goes into the output buffer at once, whatever its kind, if no write is under way and this side holds the token; the 16 KiB buffer is written out when full, at once if the output takes it (the bundled Lwt tries writes before waiting), and other messages wait meanwhile, to go urgent, then normal, then idle. Gemel had queued every message for a writer task, which picked by kind later, so a reply overtook a stream chunk sent before it. A write that has to wait is left to a task (`runtime::run_now`); one that finds the output in use (the token came while an urgent write waits, say) waits for it, as OCaml's goes on with the same buffer. A write that fails fails the connection from a task of its own, so that the close handlers don't run inside whatever was sending.
  - **A client killed during a large transfer** (`SIGKILL`, 40 files of 8 MB) leaves a gemel server that ends quietly over ssh and goes on serving on a socket, with no lock left; the next sync resumes, and `-killserver` still stops it.
- **A server's standard input and output are non-blocking** on Unix, as OCaml's: tokio's own do their reads and writes on other threads, so a write often couldn't go at once, and the order depended on timing (one scenario failed 4 runs in 5, and never with debugging on). Pipes become tokio's `pipe` ends and sockets its `UnixStream`, both safe; anything else keeps tokio's standard streams.
  - **A socket** is what sshd gives a server unless OpenSSH was built with `USE_PIPES`, the same one as standard input and output. `server.rs` checks that the server answers on one as on pipes, against OCaml; by hand, a sync through a relay that gives the server a socket pair (31 files, 8.9 MiB, then changes both ways) ended as over pipes.
- **`rsyncReg`** (`region.rs`, OCaml's `Lwt_util` regions): rsync transfers to a host share 40 KiB of block information; one that would overflow it waits. A test sends six 3 MiB files changed in the middle from an OCaml client, of which three fill the region; the client streams one file at a time anyway, so the region can't change the order it prints.
- **What's left:** with their order, 119 of 120 scenarios agree on macOS over ssh (Homebrew's 2.54.0) and 59 of 60 on sockets; on Linux with `8772d26`, 119 of 120 over ssh and on sockets, and 19 of 20 with each of 2.52.1 and 2.53.8 as the client; with `xferbycopying` off, 80 of 80 on each system. The one that differs, scenario 3 each time, is `registerFileTransfer`: transfers of the same contents wait for the first, then copy it. When one finishes, OCaml's `Lwt.wakeup` runs the next at once, inside the first, which copies and replies before the first replies, and so on down the chain: with three files, the third's reply comes first. Gemel wakes the next to run after, so the replies come in the order the transfers started. Matching that needs a woken task to run inside the one waking it, which is emulating Lwt's scheduler on tokio; it was decided (2026-10-23) to leave it, and keep comparing a gemel server's output without its order when items run concurrently. The trees and archives agree. CI compares it with its order with `xferbycopying` off (30 scenarios over ssh and on sockets, which agreed 40 of 40 three times in a row on macOS).
- **Regions wake their waiters the same way,** after the task leaving goes on, where OCaml runs the waiter at once; nothing observed depends on it.
- **Unordered, as CI compares them,** every pairing agrees, over ssh and on sockets: on Linux, 50 of 50 with an `8772d26` client and a gemel server, concurrently and not, 20 of 20 concurrently with each of 2.52.1 and 2.53.8 as the client, 50 of 50 with a gemel client and an `8772d26` server, and 50 of 50 with gemel at both ends; on macOS the same pairings with Homebrew's 2.54.0.

**Notes (phase 5c, 2026-10-23):**
- **Items run concurrently with a remote root,** up to `maxthreads` (1000 with streaming), as OCaml's do; between local roots still one at a time, where OCaml's don't yield. Each starts as it is handed out and runs until it waits before the next is (`runTask`), so the `[BGN]` lines and the first requests go out in OCaml's order.
- **A merge holds the others back:** OCaml's `Files.merge` runs its own scheduler (`Lwt_unix.run`) for its copies, so no other item starts until it is done, though those running go on. Here the dispenser waits while a merge runs (`merges_propagate_like_ocaml` found it, in order, with a remote root).
- **The abort "bug" of phase 2c's notes** was the tests' one-at-a-time expectation: with items concurrent, those already under way when everything is aborted fail at their next check ("Aborted by user request", as OCaml's `Abort.checkAll` raises), and the next run does them. The engine's abort and fatal-error tests keep `maxthreads = 1` for what they check, and `items_under_way_fail_when_everything_is_aborted` covers the rest.
- **`doTransportFailCleanup`:** after the first error, nothing more starts, everything is aborted, and the connection to the other root is closed (for a local root, the client's close handlers run), so that items still running fail. A new interactive test makes the server meet a fatal error mid-run (the commit log appears once the sync is under way, as it replaces a directory), with ten other items going on, and ends as OCaml does, over ssh too. It found that the commit log's errors were worded as `Unix` errors (`File exists [open(…)]`) where OCaml's channels give `<path>: File exists`.
- **A directory's children are copied concurrently** with a remote root, as OCaml starts a thread for each (`copyRec`), in `copyReg`, which transport resizes to as many as items may run; their results are taken last name first, as OCaml's fold does, which is also the order of the paths that failed. Between local roots they go one at a time, as OCaml's run there. (A directory of three files ended after one of two subdirectories' files where OCaml's ends first.)
- **`transferFileReg`:** without streaming, a client's transfers share 440 KiB of output buffers.
- **Rsync blocks are copied with `copy_file_range`** first on Linux (`copyFn`), the output flushed first, and read and written for what it doesn't copy; elsewhere they are read and written.
- **The tests compare remote logs in order** now (`ends_unordered` is gone), against OCaml. One test still gives two new files different contents: with the same, the second is copied from the first's temporary file, and OCaml runs it at once as the first ends, so it ends first (phase 5b's notes, on the client's side this time).
- **Against OCaml, a concurrent gemel client,** over ssh and on sockets, with Homebrew's 2.54.0 on macOS and `8772d26` on Linux: in order with `xferbycopying` off, 50 of 50 on each system, with an OCaml server and with gemel at both ends; without order, 50 of 50 with the defaults, and on Linux 20 of 20 with each of 2.52.1 and 2.53.8 as the server. In order with the defaults, 38 of 40 on macOS and 49 of 50 each way on Linux, the differences copying contents another transfer brings. CI now runs those pairings.

### Phase 6: Windows and the rest

- **Windows:**
  - ssh with the Windows OpenSSH client, asking at the console, as with OCaml's text interface, which starts it as a plain process (`buildShellConnection` without `termInteract`); ConPTY (`terminal.ml`'s `win_openpty`) is the GUI's, in milestone 4;
  - sockets.
  - OCaml's Windows handling where it differs: `kill_noerr` with `SIGKILL`, and path forms on the server.
- **Matrix:** the recorded outcomes for Windows cover remote scenarios.
- **A real sshd** in one Linux CI cell (from phase 4).
- **Odds and ends** found on the way, listed in the earlier phases' notes.

## Verification

- **Protocol:**
  - golden vectors for the handshake strings, packets and headers, generated from OCaml;
  - the in-memory connection under every engine suite.
- **Behaviour:**
  - the oracle harness on every PR from phase 3 on, across the pairings and versions of the definition of done, comparing proposed actions, final trees, both hosts' archives, and a recheck by OCaml;
  - in phase 2, gemel against gemel over the two-host in-memory connection.
- **Transfers:** rsync golden vectors, and byte counts per transfer compared with OCaml's (`bytesTransferred`).
- **Platforms:**
  - CI on Linux, macOS and Windows;
  - the OCaml matrix runs on Linux, with Homebrew's 2.54.0 on macOS;
  - Windows replays recorded outcomes and runs gemel against gemel live.

## Risks and open questions

| Risk or question | Plan |
|---|---|
| A deadlock between flow control and ssh's buffering | Port OCaml's write tokens exactly, and keep `halfduplex`; test with large transfers in both directions at once |
| rsync's token stream or checksums drift by a bit, which OCaml rejects or silently mis-decodes | Golden vectors from OCaml before any end-to-end test; compare the files transferred, not only the outcome |
| 2.52.1 needs OCaml 4, which may age out of Debian, and of the CI runner's apt | Locally the bookworm image builds it. CI caches the binaries by tag and compiler version, so a runner whose apt moves to OCaml 5 fails to build them; then CI installs OCaml 4 through opam, or 2.52 is dropped from the matrix and the plan says so |
| Prompts differ across ssh versions and platforms | ssh asks them itself at the terminal, so gemel matches none in this milestone; milestone 4 ports `terminal.ml`'s patterns as they are, and the real-sshd cell watches for drift |
| ConPTY on Windows behaves differently from Unix pseudo-terminals | Milestone 4, with the GUI, after the Unix path is settled; OCaml's Windows code is the reference |
| Concurrent transport exposes atomicity that milestone 2 relied on items running one at a time for | A host's work runs at OCaml's yield points since phase 5b; phase 5c turns `maxthreads` on |
| A server scanning (on blocking tasks) while it serves other requests | The scan already runs off the engine thread; requests that touch the same root wait on the host's turn, as local scans do |
| The socket mode has no authentication | As in OCaml: documented; gemel prints no warning, since OCaml doesn't |
