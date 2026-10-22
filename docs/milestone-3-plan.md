# Milestone 3: remote roots

*Status: proposed (2026-10-22) · Follows milestone 2 (local engine, text UI and watch mode, PRs #7–#29) · Source: `bcpierce00/unison` at `8772d26`*

Milestone 3 is step 3 of the port order in [the analysis](unison-port-analysis.md#4-architecture-recommendation): remote roots. It ends with gemel synchronising with a root on another host over ssh or a socket. It must work in both directions with OCaml Unison 2.52 to 2.54: a gemel client with an OCaml server, an OCaml client with a gemel server, and gemel at both ends. That is the Tier A promise (§3 of the analysis).

## Definition of done

For generated replica trees and change scripts, with one root reached over ssh or a socket:

1. **Every pairing agrees with OCaml's,** on Linux live, on macOS against Homebrew's 2.54.0, and on Windows through recorded outcomes (see Verification). Each of these proposes the same actions and leaves the same trees as OCaml at both ends, for each server version in the matrix:
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

  It compares each pairing with OCaml against itself, the same version at both ends.
- **Windows:** extends the recorded outcomes for Windows with remote scenarios over a socket, where a gemel client talks to a gemel server.

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
  - every milestone 2 suite with one root on the other host.

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
| 2.52.1 needs an OCaml 4 image, which may age out of Debian | The bookworm image is pinned by digest; if it stops building, 2.52 is dropped from the matrix and the plan says so |
| Prompts differ across ssh versions and platforms | Port `terminal.ml`'s patterns as they are, test them with the shim, and keep the real-sshd cell for drift |
| ConPTY on Windows behaves differently from Unix pseudo-terminals | Phase 6 alone, after the Unix path is settled; OCaml's Windows code is the reference |
| Concurrent transport exposes atomicity that milestone 2 relied on items running one at a time for | Revisit `exclusive` in phase 5 before turning `maxthreads` on |
| A server scanning (on blocking tasks) while it serves other requests | The scan already runs off the engine thread; requests that touch the same root wait on the host's turn, as local scans do |
| The socket mode has no authentication | As in OCaml: documented; gemel prints no warning, since OCaml doesn't |
