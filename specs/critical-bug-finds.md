# Critical Bug Findings — corsair-void-rs

> **Status:** Open — to be fixed in future work.
> **Generated:** 2026-05-30
> **Scope reviewed:** all source under `src/` (~3,200 LOC, 18 files) at commit `f863b1a` (branch `main`).
> **How this was produced:** Multi-agent adversarial review. 9 finders (5 module-scoped: device/audio/ipc/tray/core; 4 cross-cutting: concurrency, memory-safety/FFI/arithmetic, HID-protocol correctness, error-handling/resource-leaks) → 29 raw findings → 3 verification rounds, each finding cross-examined by two independent adversarial verifiers (one read the real code to prove it could **not** trigger; one tried to deflate severity), with a completeness critic re-probing coverage gaps between rounds → **49 confirmed real findings → synthesized to 18 distinct bugs** below.

## How to use this document

Each entry gives the exact `file:line` of the defect, the platform/feature gating that must hold for it to fire, the observable symptom, the root cause, and a concrete fix direction. Line numbers are anchored to commit `f863b1a`; if the file has drifted, locate the defect by the quoted code/symbol names rather than the raw line. Verify each fix against real hardware where the bug touches the HID/audio protocol (ranks 4, 16, 18).

---

## Executive summary

The codebase is a functional, well-structured cross-platform tray daemon. There are **no crash or memory-safety bugs that fire under normal use**. The genuine defects cluster in two subsystems — the **Windows WASAPI mic-boost engine** (`src/audio/boost.rs`) and the **IPC layer** (`src/ipc/mod.rs` plus the tray/main glue).

Two dominant themes:

1. **Unsynchronized cross-thread state.** A single shared stop-flag reused across boost-thread generations; a single named-pipe HANDLE reused as both listener and per-client transport; blocking external calls (PulseAudio, systemd) executed directly on the single-threaded tray event loop.
2. **Missing teardown / lifecycle hygiene.** Boost threads that self-exit are never reaped; the IPC `Stop` handler exits before responding; and an entire advertised feature (the Linux sysfs backend) is never wired into dispatch.

Most issues degrade gracefully or are gated behind optional features (mic boost + VB-CABLE, Linux-only paths), but several silently break core features or can hang/wedge the daemon under realistic conditions.

---

## Quick reference

| # | Severity | Bug | Location | Platform / gating |
|---|----------|-----|----------|-------------------|
| 1 | HIGH | Shared stop `AtomicBool` reused across boost-thread generations | `src/audio/boost.rs:131` | Windows; `mic_boost_db>0` + VB-CABLE |
| 2 | HIGH | Linux sysfs kernel-driver backend never dispatched | `src/main.rs:125` | Linux; kernel 6.13+ with `hid-corsair-void` bound |
| 3 | HIGH | Tray UI thread can hang forever in synchronous PulseAudio calls | `src/audio/linux.rs:117` | Linux; wedged PulseAudio/PipeWire |
| 4 | HIGH | Linux mic boost hardcodes 2-channel `ChannelVolumes`, silently fails | `src/audio/linux.rs:56` | Linux; mic boost |
| 5 | MEDIUM | Self-exiting boost thread never reaped → boost silently disabled | `src/audio/boost.rs:109` | Windows; mic boost |
| 6 | MEDIUM | IPC `Stop` handler `process::exit(0)` before responding → CLI reports failure | `src/tray/mod.rs:465` | All |
| 7 | MEDIUM | `ConnectNamedPipe` treats `ERROR_PIPE_CONNECTED` as fatal → dropped command | `src/ipc/mod.rs:157` | Windows |
| 8 | MEDIUM | Unix IPC `accept()` has no read timeout or size bound | `src/ipc/mod.rs:317` | Linux |
| 9 | MEDIUM | `thread::spawn` panic while holding boost mutex poisons it permanently | `src/audio/boost.rs:133` | Windows; mic boost |
| 10 | LOW | Windows IPC reuses single pipe HANDLE → response misrouting on stall | `src/main.rs:241` | Windows |
| 11 | LOW | IPC `BOOST` & config `mic_boost_db` applied unvalidated | `src/ipc/mod.rs:39` | All |
| 12 | LOW | Unauthenticated IPC accepts `Stop`/`Boost` from any same-user process | `src/ipc/mod.rs:134` | All (`/tmp` fallback cross-user) |
| 13 | LOW | Windows IPC ignores `ReadFile`/`WriteFile` results; single-read framing | `src/ipc/mod.rs:164` | Windows |
| 14 | LOW | `GetMixFormat` `WAVEFORMATEX` pointers never freed | `src/audio/boost.rs:277` | Windows; mic boost |
| 15 | LOW | `generate_tone()` sample-count overflow; volume passed unclamped | `src/sound.rs:57` | All; hand-edited config |
| 16 | LOW | WASAPI buffers cast to `f32` without validating mix format (latent) | `src/audio/boost.rs:389` | Windows; not triggerable under shared mode |
| 17 | LOW | `systemctl --user enable --now` blocks the tray event loop | `src/autostart.rs:51` | Linux |
| 18 | LOW | `request_notifications()` sends malformed `0xCA` packet (harmless no-op) | `src/device/hid.rs:48` | All |

---

## HIGH severity

### 1. Shared stop `AtomicBool` reused across boost-thread generations causes lost-stop race, leaked passthrough thread, and duplicated/garbled mic audio on reconnect
- **Location:** `src/audio/boost.rs:131` (reset), with the root state at `src/audio/boost.rs:59` (Arc created in `new()`).
- **Platform / gating:** Windows; only when `mic_boost_db > 0` **and** VB-CABLE is installed.
- **Impact:** On every wireless reconnect (the routine sleep/wake/range cycle of the headset) the tray runs `stop_boost()` immediately followed by `set_boost_db()`. `stop()` signals the single shared `inner.stop = true` and takes the handle, but `set_boost_db()` reuses that **same** `Arc` and resets it to `false` (line 131) before the old worker observes `true`. The old thread then runs forever, untracked and unkillable (its handle was already moved into a detached 2s joiner that times out and drops it). Two passthrough threads then read the same gain atomic and both write to the same VB-CABLE render endpoint, producing duplicated/garbled boosted mic audio, and threads + WASAPI/COM handles accumulate across reconnect cycles.
- **Root cause:** `BoostEngineInner.stop` is a single `Arc<AtomicBool>` created once in `new()` (line 59) and shared by every thread generation. `stop()` is non-blocking and never replaces the Arc; `set_boost_db()` clones the same Arc and does `stop.store(false)` (line 131) before spawning the new thread. There is no per-generation flag and no `is_finished()`/join gate, so a still-running prior thread's only stop signal is clobbered.
- **Suggested fix:** Give each spawned passthrough thread its own `stop` (and gain) `Arc`. In `stop()`, after taking the handle, replace `inner.stop` with a fresh `Arc::new(AtomicBool::new(false))` so the abandoned thread retains its own permanently-true flag. In `set_boost_db()`, create the fresh Arcs at spawn time and never reset an Arc another thread may still observe. Prefer keeping the `JoinHandle` so the worker is genuinely owned/joinable rather than abandoned after 2s. **Fix together with #5 and #9** — they are the same boost-engine concurrency/lifecycle cluster.

### 2. Linux sysfs kernel-driver backend is never dispatched — headset shows permanently disconnected when `hid-corsair-void` is bound
- **Location:** `src/main.rs:125` (`hid_polling_loop`) and `src/main.rs:275` (`run_cli`); the unused backend is `src/device/sysfs.rs`.
- **Platform / gating:** Linux, kernel 6.13+ with the `hid-corsair-void` driver bound.
- **Impact:** When the kernel owns the HID interface, `hidapi open_path()` fails (busy/permission). `hid_polling_loop` swallows the `Err` (`Err(_) => {}`), never emits `DeviceEvent::Connected`, and the device shows permanently Disconnected (no battery, no mic-mute control, no boost feedback). The CLI status path prints the error and exits 1. The advertised, spec-required "hybrid backend: sysfs when kernel driver loaded, hidapi fallback otherwise" is completely inert.
- **Root cause:** `hid_polling_loop` (main.rs:125) and `run_cli` (main.rs:275) call `HidBackend::open()` unconditionally. `SysfsBackend` and `sysfs_available()` are defined in `device/sysfs.rs` but referenced nowhere else — there is no backend-selection/dispatch at the device layer (unlike the audio layer's trait + factory).
- **Suggested fix:** Add a `cfg`-gated device-backend dispatch: define a trait `DeviceBackend` (`request_status` / `request_notifications` / `read_status`) implemented by both `HidBackend` and `SysfsBackend`, plus a factory that on Linux uses `SysfsBackend::open()` when `sysfs_available()` and `HidBackend` otherwise. Add a no-op `request_notifications()` to `SysfsBackend` so the trait is satisfiable. Wire the polling loop and CLI through the factory.

### 3. Tray main/UI thread can hang forever in synchronous PulseAudio calls (`connect_pulse` / `wait_for_op` have no timeout)
- **Location:** `src/audio/linux.rs:117` (`connect_pulse`, lines 117–133) and `src/audio/linux.rs:142` (`wait_for_op`, lines 142–151).
- **Platform / gating:** Linux; triggered when PulseAudio/PipeWire is wedged, restarting, or stalls in a non-terminal context state (common around login / suspend-resume / PipeWire restart).
- **Impact:** Every `find_device` / `set_boost_db` / `get_boost_db` call (invoked synchronously from the single tray event loop via menu clicks, IPC `Boost`, and `refresh_audio` on reconnect) opens a fresh PulseAudio connection and drains it with `mainloop.iterate(true)` in an unbounded loop. If the daemon stalls in `Connecting`/`Authorizing` or leaves an operation `Running`, `iterate(true)` blocks indefinitely. The entire tray freezes: icon/menu unresponsive, IPC commands unserviced, device events back up, with no recovery short of killing the process.
- **Root cause:** `connect_pulse()` and `wait_for_op()` spin on the **blocking** `mainloop.iterate(true)` with no wall-clock deadline and no escape for non-terminal states, and all PulseAudio work runs on the tray main thread instead of a worker.
- **Suggested fix:** Move all `AudioController` calls off the tray main thread onto a dedicated worker thread communicating via mpsc, so a stall can never freeze the UI. At minimum, add an `Instant`-based deadline (e.g. 2–3s) to both loops using non-blocking `iterate(false)` + bounded sleep, treat post-connect `Unconnected` as failure, call `op.cancel()` on timeout, and return `AudioError::ApiError`. Consider keeping a single persistent context instead of reconnecting per call.

### 4. Linux mic boost builds a hardcoded 2-channel `ChannelVolumes`, misusing `pa_cvolume_set`; boost silently fails on the mono Corsair source while reporting success
- **Location:** `src/audio/linux.rs:56` (`set_boost_db`); related `wait_for_op` at `src/audio/linux.rs:142`–151 and the `find_device` source match.
- **Platform / gating:** Linux; mic boost on the mono Corsair Void capture source.
- **Impact:** `set_boost_db` builds the volume as `ChannelVolumes::default().set(2, volume)`. Because the first arg of `pa_cvolume_set` is the channel **count**, not an index, this hardcodes a 2-channel cvolume regardless of the real source layout. For the mono Corsair Void capture source, PulseAudio rejects the incompatible cvolume (`PA_ERR_INVALID`), so the boost is never applied. `wait_for_op` never inspects the final op state, so `set_boost_db` returns `Ok(())` — the headline Linux feature silently does nothing while reporting success. (The PA server, not corsair-void, rejects; on release PA builds it rejects without crashing.)
- **Root cause:** Hardcoded channel count of 2 that does not match the device's actual channel layout; `find_device` never captures `source.sample_spec.channels`; and `wait_for_op` ignores `op.get_state() == Cancelled`/error so rejected operations masquerade as success.
- **Suggested fix:** In `find_device`, capture the matched source's real channel count (`source.sample_spec.channels` / `channel_map.len()`) and store it. In `set_boost_db`, build the cvolume with that count: `cv.set(self.channels, volume)`. As a safe fallback when unknown, use a 1-channel volume (the server always accepts `channels == 1` and remaps it). Separately, make `wait_for_op` return an error when the operation ends in `Cancelled` so a rejected change is not reported as `Ok`.

---

## MEDIUM severity

### 5. Boost passthrough thread that self-exits on a WASAPI error is never reaped, leaving a stale `thread_handle` that silently disables all future boost changes
- **Location:** `src/audio/boost.rs:109` (the `thread_handle.is_some()` fast path).
- **Platform / gating:** Windows; mic boost.
- **Impact:** `passthrough_thread_inner` returns `Err` and the thread exits on any in-loop WASAPI failure (e.g. `AUDCLNT_E_DEVICE_INVALIDATED` from VB-CABLE removal, sleep/resume, or a format change), but `inner.thread_handle` stays `Some(<dead handle>)`. `set_boost_db` then short-circuits on `thread_handle.is_some()` (line 109), only updating an atomic no thread reads, and returns `Ok` — so the tray menu and IPC report success while boost is silently off. It only self-heals on a headset disconnect/reconnect transition (which calls `stop()`); a glitch that leaves the HID link "connected" leaves boost dead. No crash; bounded single leaked handle.
- **Root cause:** The respawn decision keys solely off `thread_handle.is_some()` with no `is_finished()` liveness check; `thread_handle` is cleared only by `stop()`, never by the worker on self-exit.
- **Suggested fix:** Before the `is_some()` fast path, reap a finished worker: `if inner.thread_handle.as_ref().map_or(false, |h| h.is_finished()) { let _ = inner.thread_handle.take().map(|h| h.join()); }`, then base the spawn decision on the refreshed handle. Equivalently, have the worker clear a shared "running" `AtomicBool` on exit and key the decision off that. **Combine with the per-generation stop-flag fix (#1).**

### 6. IPC `Stop` handler calls `process::exit(0)` before sending the response, so `corsair-void stop` always reports failure (exit 1) and skips graceful teardown
- **Location:** `src/tray/mod.rs:465` (`handle_ipc_command`, `IpcMessage::Stop` arm).
- **Platform / gating:** All platforms.
- **Impact:** The handler calls `std::process::exit(0)` before `run_tray` reaches `cmd.responder.send(response)` / `cmd.done.send(())`. No `IpcResponse::Ok` is ever written; the client's blocking read returns EOF, `IpcResponse::parse("")` yields `Error("Unknown response: ")`, and the CLI prints "Error:" and exits 1 on **every** successful stop. The daemon does stop correctly, but the documented primary stop path always signals failure (breaking scripts/automation), and teardown skips all destructors (`Drop`, `IpcServer` socket cleanup, ordered boost stop) — though the OS reclaims everything on exit and the Linux socket is overwritten on next bind, so no accumulating leak.
- **Root cause:** Process termination is performed inside the command handler instead of being signaled back to the event loop, so the response/done handshake is bypassed and shutdown is uncoordinated and inconsistent with the `Quit` path.
- **Suggested fix:** Return `IpcResponse::Ok` for `Stop` from `handle_ipc_command` (do not exit there). In `run_tray`, after `cmd.responder.send(response)` + `cmd.done.send(())`, detect the `Stop` message, then perform ordered teardown (stop boost, signal/join worker threads or at least drop the `IpcServer` to remove the socket) and exit. This guarantees the client receives OK before the pipe closes and unifies `Stop` with the `Quit` teardown path.

### 7. Windows `ConnectNamedPipe` treats `ERROR_PIPE_CONNECTED` as fatal, intermittently dropping an already-connected client's command
- **Location:** `src/ipc/mod.rs:157` (`accept`).
- **Platform / gating:** Windows; timing-dependent race.
- **Impact:** When a client connects (`CreateFileW`) in the window between the server's `CreateNamedPipeW`/`DisconnectNamedPipe` and its `ConnectNamedPipe` call, Win32 returns `FALSE` with `GetLastError() == ERROR_PIPE_CONNECTED` (535) — the normal "client already here" success signal. The code maps any `Err` and propagates it: `ipc_server_loop` logs "IPC accept error", disconnects the client, and never reads its STATUS/BOOST/STOP command. The client's command is silently lost and its CLI invocation fails. Self-recovering (next command works).
- **Root cause:** `accept()` does `ConnectNamedPipe(self.handle, None).map_err(...)?` and treats every error as fatal, without special-casing `ERROR_PIPE_CONNECTED` (and `ERROR_NO_DATA`) which Win32 documents as a successfully-connected client.
- **Suggested fix:** After `ConnectNamedPipe` fails, inspect the OS error: if `Error::last_os_error().raw_os_error() == Some(535)` (`ERROR_PIPE_CONNECTED`), proceed to `ReadFile` as if connected instead of returning `Err`. Also handle `ERROR_NO_DATA` (232) as a connected-but-closed client.

### 8. Unix IPC `accept()` has no read timeout or size bound: a local client can hang the IPC thread or exhaust memory
- **Location:** `src/ipc/mod.rs:317` (`accept`, Unix path).
- **Platform / gating:** Linux; any local process.
- **Impact:** `accept()` does `reader.read_line(&mut line)?` on the `UnixStream` with no read timeout and no length cap. Any local process can connect to the socket and either (a) send bytes without a newline and never close, hanging `read_line` forever — which wedges the single serial accept loop so all CLI commands (status/boost/stop) stop being serviced — or (b) stream a newline-free flood to grow the `String` until OOM. Local-only, usually same-user (`XDG_RUNTIME_DIR` is 0700), self-recovering on restart; HID polling and the tray menu remain functional, so it degrades a secondary control path rather than crashing the core daemon. The `/tmp` fallback (when `XDG_RUNTIME_DIR` is unset) widens the attacker set.
- **Root cause:** Unbounded `BufRead::read_line` on a blocking socket with no `set_read_timeout`, inside a single-threaded serial accept loop. (The Windows path is already bounded by a fixed 1024-byte `ReadFile`.)
- **Suggested fix:** Cap the read: wrap the reader with `.take(MAX_IPC_LINE)` before `read_line` and reject input that reaches the cap without a newline (`InvalidData`). Set `stream.set_read_timeout(Some(2s))` immediately after `accept()` and treat `WouldBlock`/`TimedOut` as a dropped client (disconnect and continue). Optionally handle each connection on a short-lived worker so one stalled client cannot block accept of the next. Apply the same cap to the client read.

### 9. `thread::spawn` panic while holding the BoostEngine mutex poisons it permanently, crashing the main thread on every later boost/stop
- **Location:** `src/audio/boost.rs:133` (spawn while holding the guard taken at line 102); lock sites at lines 68/91/98/102/147/176.
- **Platform / gating:** Windows; mic boost. Low trigger probability, unrecoverable consequence.
- **Impact:** `set_boost_db` holds the `inner` mutex guard across `thread::spawn` (the guard from line 102 lives to end of function). If spawn panics (OS thread-creation failure: EAGAIN/RLIMIT/handle exhaustion), the unwind poisons the `std::sync::Mutex`. Thereafter every accessor (`.lock().unwrap()` at lines 68/91/98/102/147/176) panics on `PoisonError`. Since `set_boost_db`/`get_boost_db`/`stop` run on the main tray thread (and `stop` runs from `Drop`), the next menu click / IPC `Boost` / disconnect crashes the tray permanently with no recovery.
- **Root cause:** The mutex guard is held across a fallible `thread::spawn`, and every lock site uses `.lock().unwrap()` with no poison recovery. The inner state has no invariant a panic could corrupt (only Option/u8/Arc fields).
- **Suggested fix:** Do not hold the lock across `thread::spawn`: pull the needed Arcs/IDs out of the guard, drop the guard, spawn, then re-lock briefly to store the handle. Add a poison-tolerant helper used at all lock sites: `fn lock(&self) -> MutexGuard<...> { self.inner.lock().unwrap_or_else(|e| e.into_inner()) }`, replacing every `.lock().unwrap()`. **Part of the boost-engine cluster (#1, #5).**

---

## LOW severity (defense-in-depth, observability, edge cases)

### 10. Windows IPC reuses the single pipe HANDLE; a >2s main-thread stall + timeout disconnect can misroute one client's response to another
- **Location:** `src/main.rs:241` (`ipc_server_loop` timeout/disconnect path); the shared handle lives in `IpcServer`/`IpcResponder` in `src/ipc/mod.rs`.
- **Platform / gating:** Windows; requires a pathological >2s main-thread stall **and** two concurrent one-shot CLI clients.
- **Impact:** The Windows `IpcServer` owns one pipe HANDLE and `accept()` copies it into `IpcResponder`. `ipc_server_loop` waits only `IPC_RESPONSE_TIMEOUT_MS` (2000ms) on `done_rx`, then unconditionally calls `disconnect_client()` and loops into `accept()` on the **same** handle. If the main thread stalls >2s while a command is in flight and a second client connects, the late `cmd.responder.send` `WriteFile` lands on the new client — cross-client response misdelivery, plus a data race on the shared handle that violates the `IpcResponder` `unsafe Send` "one thread at a time" invariant. In practice the IPC handlers are all near-instant, so this is hard to hit; errors are swallowed, no crash/leak, self-recovering.
- **Root cause:** A single listening pipe handle is reused as both the listener and the per-connection responder, and the timeout path tears down/rebinds it without coordinating with the main thread that may still write the response.
- **Suggested fix:** Give each accepted connection its own pipe instance: `CreateNamedPipeW` supports `PIPE_UNLIMITED_INSTANCES` — keep the per-client instance handle in `IpcResponder` and `CloseHandle` it after responding (this also enables concurrent clients). Alternatively, have the main thread own the full write+disconnect lifecycle and block on `done_rx.recv()` (no timeout-then-disconnect) so the handle is never rebound while a response is pending.

### 11. IPC `BOOST` and config `mic_boost_db` are applied unvalidated, bypassing the CLI's 0/5/10 contract
- **Location:** `src/ipc/mod.rs:39` (`IpcMessage::parse` BOOST arm); also `Config::load` for `general.mic_boost_db`.
- **Platform / gating:** All platforms; same-user/local trust boundary.
- **Impact:** `IpcMessage::parse` accepts any `u8` for BOOST (`parse::<u8>().ok().map(Self::Boost)`), and `Config::load` deserializes `general.mic_boost_db` as a raw unvalidated `u8`; both bypass the CLI's `parse_boost_db {0,5,10}` gate. `handle_ipc_command` applies the value via `set_boost_db` and persists it. A value like 200 yields an enormous gain factor, but Windows clamps every sample to `[-1.0, 1.0]` (full-scale clipping, no overflow) and Linux saturates the u32 PA volume (and PA clamps internally) — so the effect is distorted/over-loud-but-recoverable mic audio plus a config value that no longer matches any menu checkbox. No crash, fully reversible.
- **Root cause:** Validation lives only in the CLI front-end (clap `value_parser`), not at the IPC parse boundary, in `Config::load`, or centrally in `set_boost_db`.
- **Suggested fix:** Enforce the `{0,5,10}` invariant centrally inside `set_boost_db` (clamp/snap or reject) so all entry points are covered. Additionally validate at the IPC boundary (`rest.trim().parse::<u8>().ok().filter(|v| matches!(v, 0|5|10))`) and clamp/normalize `mic_boost_db` in `Config::load`, falling back to a default on out-of-range.

### 12. Unauthenticated IPC accepts process-terminating `Stop` and state-mutating `Boost` from any same-user local process
- **Location:** `src/ipc/mod.rs:134` (Windows pipe creation with NULL security attributes); the Unix bind has no explicit 0600/peer check.
- **Platform / gating:** All; the genuinely weaker case is the Unix `/tmp` fallback (`XDG_RUNTIME_DIR` unset), which is cross-user.
- **Impact:** A local process can send STOP (clean `process::exit(0)` — a graceful, recoverable DoS) or BOOST (reversible mic-gain change). The default named-pipe DACL / 0700 XDG runtime dir scopes write access to the same user, who already has full authority over the daemon, so no privilege boundary is normally crossed. Defense-in-depth gap.
- **Root cause:** No DACL restriction on the Windows pipe, no explicit 0600 / `SO_PEERCRED` on the Unix socket, a world-accessible `/tmp` fallback, and no peer-identity check before honoring mutating/terminating commands.
- **Suggested fix:** Pass `SECURITY_ATTRIBUTES` with an owner+SYSTEM-only DACL to `CreateNamedPipeW`; on Unix create the socket with 0600 (set umask/chmod) and avoid the `/tmp` fallback (refuse to start without a private runtime dir). Optionally verify the peer (`GetNamedPipeClientProcessId` + token SID / `SO_PEERCRED`) before honoring `Stop`/`Boost`.

### 13. Windows IPC ignores `ReadFile`/`WriteFile` results and assumes one `ReadFile` returns a whole newline-framed message
- **Location:** `src/ipc/mod.rs:164` (server `accept`) and the client `send` path.
- **Platform / gating:** Windows; in practice essentially never triggers (messages <10 bytes into a 1024-byte buffer over a local synchronous one-message-per-connection pipe).
- **Impact:** The server and client discard the `ReadFile`/`WriteFile` BOOL with `let _ =` and consult only `bytes_read`, on a byte-mode pipe (`PIPE_TYPE_BYTE`) for a newline-delimited protocol. A genuine read error is indistinguishable from a zero-length message (reported as "Invalid IPC message" with the real OS error lost), and there is no read-until-`\n` loop. Net effect is a diagnosability/observability gap, not a correctness or stability problem.
- **Root cause:** Ignored I/O return values plus a framing assumption (single `ReadFile` == whole message) that is not guaranteed by byte-mode pipes.
- **Suggested fix:** Check the `ReadFile`/`WriteFile` BOOL and return `std::io::Error::last_os_error()` on failure. Either switch the pipe to message mode (`PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE`) or accumulate bytes in a loop until `\n` (mirroring the Unix `read_line` path), with a size cap.

### 14. `GetMixFormat` `WAVEFORMATEX` pointers (capture & render) are never freed with `CoTaskMemFree`
- **Location:** `src/audio/boost.rs:277` (capture format ptr) and `src/audio/boost.rs:311` (render format ptr).
- **Platform / gating:** Windows; mic boost. Negligible practical impact.
- **Impact:** `IAudioClient::GetMixFormat` returns a caller-owned `CoTaskMemAlloc`'d `WAVEFORMATEX`; `capture_format_ptr` and `render_format_ptr` are used but never freed. A fresh passthrough thread spawns on each boost start and on each wireless reconnect, so two small (~18–88 byte) structs leak per cycle — unbounded over an infinite process lifetime but only tens of KB even after thousands of cycles. The author already frees the device-ID PWSTR correctly elsewhere, so this is a clear oversight.
- **Root cause:** Missing `CoTaskMemFree` for the two caller-owned format pointers in `passthrough_thread_inner`.
- **Suggested fix:** Call `windows::Win32::System::Com::CoTaskMemFree(Some(ptr as *const _))` for both pointers after `Initialize` copies the format. Use a small RAII guard so the free also runs on the intervening `?` early-return paths (the pointer must remain valid through the `Initialize` call).

### 15. Integer overflow in `generate_tone()` sample-count from unvalidated config `duration_ms`; volume passed unclamped to `set_volume`
- **Location:** `src/sound.rs:57` (`generate_tone`).
- **Platform / gating:** All; requires the local user to hand-edit their own config to absurd values.
- **Impact:** `generate_tone` computes `(sample_rate * duration_ms / 1000)` in `u32` with `sample_rate = 44100`; this overflows once `duration_ms >= 97392`. In debug builds it panics ("attempt to multiply with overflow"); in release it wraps to a wrong sample count. Separately, `config.sound.volume` (unvalidated `f32`) is passed straight to `Sink::set_volume`, so NaN/negative/huge values produce a NaN/garbage warning tone. Both run inside a detached sound thread, so the worst case is one corrupted/silent low-battery beep or a dead sound thread — the daemon and other threads are unaffected.
- **Root cause:** `u32` arithmetic before the divide/cast, and no validation/clamping of `SoundConfig` fields (`duration_ms`, `volume`, `freq_*`) in `Config::load`.
- **Suggested fix:** Compute in `u64`: `let samples = ((sample_rate as u64 * duration_ms as u64) / 1000) as usize`. Clamp/validate `SoundConfig` on load: `duration_ms.min(MAX_DURATION_MS)`, `volume = if v.is_finite() { v.clamp(0.0, 1.0) } else { default }`, and bound `freq_*` to a sane audio range.

### 16. WASAPI passthrough casts capture/render buffers to `f32` without validating the mix format is 32-bit IEEE float (latent)
- **Location:** `src/audio/boost.rs:389` (capture cast) and `src/audio/boost.rs:418` (render cast).
- **Platform / gating:** Windows; mic boost. **Not triggerable under the shared-mode contract** — documented here as a latent hardening gap.
- **Impact:** `passthrough_thread_inner` reads only `nSamplesPerSec`/`nChannels` from `GetMixFormat` and unconditionally reinterprets capture (line 389) and render (line 418) buffers as `f32` sized `frames*channels`. If the negotiated format were 16-bit PCM, this would be an out-of-bounds read (capture) and out-of-bounds write (render). However, both clients use `AUDCLNT_SHAREMODE_SHARED` and pass the exact `GetMixFormat` pointer to `Initialize`, and the Windows shared-mode audio engine mixes in 32-bit IEEE float, so `GetBuffer` always delivers float regardless of the endpoint's device format. So this is a latent defensive-hardening gap, not a triggerable defect under the shared-mode contract.
- **Root cause:** The `f32` element type is hardcoded while the actual sample format from `GetMixFormat` (`wFormatTag`/`wBitsPerSample`/`SubFormat`) is never inspected; the cast relies on an unverified (but in shared mode guaranteed) float assumption.
- **Suggested fix:** After `GetMixFormat`, validate `wFormatTag == WAVE_FORMAT_IEEE_FLOAT` (or `WAVE_FORMAT_EXTENSIBLE` with `SubFormat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`) and `wBitsPerSample == 32`; otherwise return `AudioError` (disable boost gracefully) or convert. Derive slice byte-size from the actual `nBlockAlign` rather than assuming 4 bytes/sample.

### 17. `systemctl` autostart toggle blocks the tray event loop synchronously (Linux)
- **Location:** `src/autostart.rs:51` (`set_auto_start`).
- **Platform / gating:** Linux; only when the user toggles "Start on Login".
- **Impact:** `set_auto_start()` runs `systemctl --user enable --now corsair-void` via `.status()` (a blocking spawn-and-wait that talks to the systemd user manager over dbus and waits for the unit to start) directly inside the single-threaded tray menu event loop. For the call duration (hundreds of ms to several seconds) the whole tray freezes: no menu/device/IPC processing, no icon updates. Self-recovers, no crash or data loss (mpsc channels are unbounded).
- **Root cause:** A blocking child-process + dbus call performed on the OS event-loop thread, violating the project's invariant that the main loop stay responsive.
- **Suggested fix:** Run only the `systemctl` invocation off-thread (`std::thread::spawn`) and report failure asynchronously (e.g. revert the checkbox via a channel); the unit-file `fs::write` is fast and can stay inline.

### 18. `request_notifications()` sends a malformed `0xCA` packet (wrong `byte[1]`, missing `alert_id`)
- **Location:** `src/device/hid.rs:48` (`send_command`).
- **Platform / gating:** All. **Harmless no-op today** — documented to prevent a naive "fix" from making things worse.
- **Impact:** `send_command` writes one fixed layout (`buf[1] = STATUS_REPORT_ID 0x64`) for both `0xC9` and `0xCA`. That is correct for the `0xC9` status request but wrong for `0xCA`, which the reference driver builds as `[0xCA, 0x02, alert_id]`. **Critically, `0xCA` is NOT a "subscribe to push notifications" command** — it triggers a one-off headset alert/beep, and status/mic/battery reports are pushed *unsolicited* by the dongle and primed by the (correctly-formed) `0xC9`. So the core status path is NOT broken; the malformed `0xCA` is a harmless no-op. ⚠️ **Note: "fixing" it to a correct `0xCA` while keeping the periodic 5s re-send would make the headset beep every 5 seconds — worse than current behavior.**
- **Root cause:** One shared packet layout for two semantically different commands, plus a misunderstanding of `0xCA` as a notification-subscription rather than an alert command (the periodic `request_notifications` re-send is dead/no-op code).
- **Suggested fix:** Give `request_notifications` its own builder matching the reference driver (`[0xCA, 0x02, alert_id]`) **only** for explicit user-triggered alerts, and remove the periodic 5s re-send loop. Keep `request_status` as `[0xC9, 0x64]`. Rely on unsolicited push reports + `0xC9` for status. Verify against hardware (the reference driver issues these as SET_REPORT control transfers — use `send_feature_report` if interrupt-OUT writes are ignored).

---

## Recommended fix ordering

1. **Boost-engine concurrency/lifecycle cluster — #1, #5, #9 together.** They interlock (shared stop flag, unreaped self-exit, mutex-poison-across-spawn) and a partial fix can mask the others. Highest leverage for Windows stability.
2. **#2 — wire up the Linux sysfs backend.** A spec-promised, supported configuration is currently completely broken (permanent disconnect).
3. **Quick correctness wins — #6 (`stop` exit code) and #7 (pipe-connected race).** Small, self-contained, user-visible.
4. **#3 + #4 — Linux PulseAudio robustness** (UI hang + silent boost failure); larger because the clean fix moves audio onto a worker thread.
5. **#8, #11, #12, #13 — IPC hardening** (timeout/size cap, validation, auth, framing). Can be done as one IPC-layer pass.
6. **Remaining lows — #10, #14, #15, #16, #17, #18** as cleanup; note the ⚠️ caveat on #18.
