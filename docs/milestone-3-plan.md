# Milestone 3: remote roots

*Status: in progress (phase 2, 2026-10-22) · Follows milestone 2 (local engine, text UI and watch mode, PRs #7–#29) · Source: `bcpierce00/unison` at `8772d26`*

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
   - ssh's password and passphrase prompts answered from the text interface, on Unix and Windows;
   - a lost connection reported as OCaml reports it.

Out of scope, and how each behaves until it is done:

| Cut | Behaviour in M3 | When |
|---|---|---|
| Synchronising xattrs and ACLs | The `xattrs` and `acl` preferences are registered so that OCaml clients can send them (see phase 1). Setting either to true is refused, as OCaml refuses it when the other side lacks the feature. `propsExtData` is not registered | M5 |
| External copy programs (`copyprog`, `copyprogrest`, `copythreshold`, `finishExternalTransfer`) | The preferences are accepted; a profile that turns external copying on gets a warning that it is ignored, and transfers use the built-in protocol. External rsync is deprecated upstream | Not planned |
| `rsh` (`rshcmd`, `rshargs`) | Marked removed, as in OCaml since 2.52 | Never |
| The self-test commands (`test.ml`: `makeRootEmpty`, `getfs`, `putfs`, ...) | Not registered | Never |
| `dumpArchive`, `inspectFilesystem` (debugging) | Not registered | M5, if wanted |
| GUI | The session's password questions exist; only the text interface asks them | M4 |

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
- **Password prompts:** the same shim can print `user@host's password:` and read the answer through the pseudo-terminal, so the prompt path is tested deterministically.
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
- **Windows:** `recorded/windows-socket.txt` holds 100 Windows-flavour scenarios on a socket, recorded with `8772d26` at both ends. Gemel replays them from phase 4, when it can serve on a socket. Homebrew's 2.54.0 on macOS replays them, 100 of 100.
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

### Phase 4: sockets, prompts and the session (`remote.ml`'s sockets, `terminal.ml`, `pty.c`, `uicommon`'s `openConnection*`, ~0.9k)

- **Sockets:**
  - TCP, with OCaml's address resolution and its order of attempts (`buildSocket`), and Unix-domain sockets;
  - the server side: `waitOnPort`, cleaning up its socket file, and `-listen`.
  - The manual's warning that the TCP method is insecure goes into gemel's documentation. OCaml prints none at run time.
  - Against 2.52 and 2.53 socket servers, this is where the compatibility mode of phase 3 is met.
- **Prompts:**
  - ssh runs under a pseudo-terminal, and `terminal.ml`'s matching of OpenSSH's prompts is ported: passwords, passphrases, host keys and failures;
  - the session asks the frontend (milestone 2's reply-channel questions), and the text interface prompts without echo.
- **Session:** `openConnectionStart`, `Prompt`, `Reply`, `End` and `Cancel` in the session's `connect`, the "Contacting server" status, and connection errors with OCaml's messages.
- **Pseudo-terminals:** Unix `openpty` through `gemel-fs`, the only crate allowed `unsafe` code, or `portable-pty`. Decided in this phase, which also covers Windows' ConPTY.
- **Tests:**
  - the shim with prompts;
  - sockets on localhost, against each version;
  - one real sshd cell in Linux CI.

### Phase 5: the rest of transfers, and concurrency (`copy.ml`, `transport.ml`, ~0.5k)

- Resuming a partial transfer: `fingerprintSubfile`, `validFilePrefix` and the append threshold.
- Copying a moved file instead (`tryCopyMovedFile`, with `xferhint`).
- `checkForChangesToSource` across hosts.
- Diff and merge with a remote root, copying the other side's file to a local temporary file.
- **Concurrency:**
  - transport runs items concurrently with a remote root (`maxthreads`);
  - before that, `exclusive` is revisited. Milestone 2's phase 6c note says its locking per piece is only safe while items run one at a time, so it holds the lock for a whole command, or OCaml's yield points are matched.
- **Tests:**
  - an interrupted transfer resumed;
  - a moved file;
  - remote diff and merge;
  - concurrent items, with the log order checked against OCaml's.

### Phase 6: Windows and the rest

- **Windows:**
  - ssh under ConPTY with the Windows OpenSSH client;
  - sockets.
  - OCaml's Windows handling where it differs: `kill_noerr` with `SIGKILL`, and path forms on the server.
- **Matrix:** the recorded outcomes for Windows cover remote scenarios.
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
| Prompts differ across ssh versions and platforms | Port `terminal.ml`'s patterns as they are, test them with the shim, and keep the real-sshd cell for drift |
| ConPTY on Windows behaves differently from Unix pseudo-terminals | Phase 6 alone, after the Unix path is settled; OCaml's Windows code is the reference |
| Concurrent transport exposes atomicity that milestone 2 relied on items running one at a time for | Revisit `exclusive` in phase 5 before turning `maxthreads` on |
| A server scanning (on blocking tasks) while it serves other requests | The scan already runs off the engine thread; requests that touch the same root wait on the host's turn, as local scans do |
| The socket mode has no authentication | As in OCaml: documented; gemel prints no warning, since OCaml doesn't |
