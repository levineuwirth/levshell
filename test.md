# Methodology for analyzing/testing the QML shell

Work from the inside out — confirm each layer before trusting the next. The shell is the *last* mile, so most "QML bugs" are really framing or event-source bugs.

## 1. Smoke the daemon alone first

Before launching Quickshell, confirm the daemon is healthy:

```bash
RUST_LOG=levshell=trace,swayipc_async=info cargo run -p levshell-daemon
# In another terminal:
ls -l $XDG_RUNTIME_DIR/levshell.sock
swaymsg -t get_workspaces   # sanity-check what Sway reports
```

The tracing logs should show migrations running, IPC bound, and a `SwayWorkspaceModule` start. If `start()` returns `Unavailable`, you'll see the module parked with no task spawned — a sign Sway isn't reachable, not a QML problem.

## 2. Observe the wire before the renderer

The framing contract (4-byte BE u32 + JSON payload) is exactly what trips Quickshell socket APIs, so read the bytes directly:

```bash
# Raw hex view of every frame the daemon pushes:
socat -u UNIX-CONNECT:$XDG_RUNTIME_DIR/levshell.sock - | xxd | head -80
```

Drive events by switching Sway workspaces (`swaymsg workspace 2`, focus a different window) and watch frames appear. You should see a 4-byte length, then `{"type":"widget_update",...}`. If frames never arrive, the bug is daemon-side (module not publishing, or writer task stalled). If frames arrive but QML doesn't render, the bug is in the QML parser.

Only **one** peer can connect at a time in Phase 0 — the daemon accepts a single shell connection — so **stop the `socat` probe before starting Quickshell**, or start a second daemon instance on a temp socket.

## 3. Run Quickshell with verbose logging

The QML file uses `console.log` at connect/disconnect and `console.warn` on parse failures. Launch it so those land in the terminal:

```bash
quickshell -p shell/main.qml 2>&1 | tee /tmp/levshell-qml.log
```

Iterate with hot-reload: Quickshell reloads on file change, so edit `shell/main.qml`, save, and watch the log. No daemon restart needed.

## 4. Signal-name variance is the #1 QML gotcha

The `Socket` element's raw-read signal has changed across Quickshell releases (`onTextRead`, `onRead`, `dataReady`, …). If the daemon is clearly emitting frames (step 2) but QML shows an empty bar with no `parse` warnings, your Quickshell build is delivering bytes to a different signal than `onTextRead`. Check with:

```bash
quickshell --version
quickshell types Quickshell.Io.Socket   # or whatever the introspection flag is on your build
```

Then adjust the handler name in `shell/main.qml:133`. The framing math in `pumpFrames()` is version-independent — you only need to point the right signal at it and push bytes into `daemonSocket.buffer`.

## 5. Inject synthetic frames for QML-only debugging

When you want to exercise the QML render path without Sway or the daemon, bypass both:

```bash
# Kill the daemon first so the socket is free, then:
python3 -c '
import socket, struct, json, os
s = socket.socket(socket.AF_UNIX)
s.bind(os.environ["XDG_RUNTIME_DIR"] + "/levshell.sock")
s.listen(1); c, _ = s.accept()
payload = json.dumps({
    "type":"widget_update","widget_id":"workspace-indicator",
    "widget_type":"workspace_indicator","status":"normal",
    "state":{"active":"2","focused_window":"Firefox",
             "workspaces":[{"name":"1","num":1,"focused":False},
                           {"name":"2","num":2,"focused":True}]}
}).encode()
c.sendall(struct.pack(">I", len(payload)) + payload)
import time; time.sleep(600)
'
```

Launch Quickshell against this fake daemon and you can hand-craft malformed frames, oversized frames, partial frames across multiple `send` calls, etc., to validate the JS framing state machine.

## 6. Exercise the unhappy paths

Phase-0 shell must survive:

- Daemon restart while Quickshell stays up → `onConnectionStateChanged` should log disconnect, then reconnect. If the bar goes stale without re-rendering, the reconnect path needs a buffer reset (`expectedLen = -1`, `buffer = new Uint8Array(0)`).
- Partial frames (the daemon's TCP-like stream semantics mean a single JSON message may arrive across two reads).
- Quickshell launched *before* the daemon → socket does not yet exist. Confirm graceful retry.

## 7. Keep the daemon-side truth visible

The Rust end has 35 tests including a full end-to-end (`crates/levshell-daemon/tests/end_to_end.rs`) that spins up `run()` with a fake module. That test is your reference oracle — if QML disagrees with what the end-to-end test asserts, trust the Rust test and hunt the discrepancy in QML parsing, not in the daemon.

## Order of escalation when the bar is blank

1. `cargo test -p levshell-daemon`
2. `socat` probe
3. Quickshell console
4. Synthetic-frame injector

Each step rules out an entire layer.
