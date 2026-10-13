# The demu daemon and MCP reference

This document is the complete reference for `demu daemon`, the daemon
execution mode of demu. The daemon serves the Model Context Protocol
(MCP) over Streamable HTTP on the loopback interface. An MCP client
such as OpenCode connects to it, creates named emulator sessions, and
drives them with the demu command set.

The daemon is a fourth transport beside the three of the [REPL and
protocol reference](repl-reference.md):

- The text REPL on stdin/stdout, for humans.
- The JSON-lines protocol on stdin/stdout (`--json`), for machines.
- The JSON-lines protocol over a Unix-domain socket (`--serve <path>`),
  for machines. POSIX only.
- The MCP Streamable HTTP endpoint (`daemon`), for MCP clients.

The classic transports run untouched beside the daemon. The manual
index explains the other manual chapters. The technical design is the
arbiter for any behavior question.

## 1. Overview

Daemon mode turns demu from a one-machine process into a session host.
The daemon process keeps one registry of emulator sessions. Each
session is one machine with its output directory. The client creates a
session from a system configuration and an optional ROM image, then
issues demu commands against it, one call at a time.

The daemon changes nothing about how machines work:

- The command set. A session answers the same commands as the text
  REPL, `--json`, and `--serve`. The daemon wraps the same shell.
- Machine assembly, config validation, and determinism. `config
  --check` and every diagnostic behave as before.
- The classic transports. The text REPL, `--json`, and `--serve` carry
  no change, and their exit codes stay the same.

What the daemon adds is hosting. Sessions live in the daemon process,
they keep their machines between client calls, several MCP connections
can use the registry at the same time, and the daemon writes lifecycle
events (startup, shutdown, session create and destroy, failures) to
stderr.

## 2. Starting the daemon by hand

```
demu daemon [--http <addr:port>] [--session-limit <n>] [--data-root <dir>]
```

Flags:

- `--http <addr:port>` sets the bind address. The default is
  `127.0.0.1:6502`. An IPv6 address uses the bracket form, for example
  `--http [::1]:6502`.
- `--session-limit <n>` sets the maximum number of concurrent
  sessions. The default is 8. The number must be at least 1, and
  larger numbers raise the cap. `0` is rejected as an option error.
- `--data-root <dir>` sets the default parent of session output
  directories. The default is `$XDG_DATA_HOME/demu` when
  `XDG_DATA_HOME` is an absolute path, otherwise
  `$HOME/.local/share/demu` when `HOME` is an absolute path
  (section 9). A relative value falls through to the next candidate,
  per the XDG base-directory spec.

Lifecycle events print on stderr: startup, shutdown, session create
and destroy, and failures.

Exit codes:

| Code | Meaning |
|---:|---|
| 0 | graceful shutdown on SIGTERM or SIGINT |
| 1 | does not occur in daemon mode |
| 2 | option error, or bind failure, including address already in use |

Ctrl-C (SIGINT) or SIGTERM ends the daemon with exit 0. A failing bind
is a startup error: the daemon prints the reason, exits 2, and starts
nothing.

## 3. Installing as a systemd user daemon

The repository ships the user unit `packaging/systemd/demu.service`:

```
[Unit]
Description=demu cycle-accurate emulator daemon

[Service]
ExecStart=%h/.cargo/bin/demu daemon --http 127.0.0.1:6502
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

Install steps:

1. Build and install the binary with
   `cargo install --path crates/demu`. The binary lands in
   `~/.cargo/bin/demu`, which the unit references.
2. Copy `packaging/systemd/demu.service` to
   `~/.config/systemd/user/demu.service`, then run
   `systemctl --user daemon-reload`.
3. Start and enable the service with
   `systemctl --user enable --now demu`.
4. Keep the daemon after logout with
   `loginctl enable-linger $USER`. Without linger, systemd stops user
   services at the end of the login session.

Where systemd exists, `systemd-analyze verify
~/.config/systemd/user/demu.service` checks the installed unit (or run
it on the repository path). A host without systemd skips the check.

Restart semantics: sessions are transient. Every session disappears
when the daemon stops, and a restart brings none of them back.
`Restart=on-failure` with `RestartSec=2` re-launches the daemon after
a crash, and a normal `systemctl --user stop demu` ends it cleanly
with exit 0 — with no long-running command in flight; a command in
flight extends the stop to the service timeout (section 10).

Removal: `systemctl --user disable --now demu`, then delete the copied
unit file. Nothing persists outside the session output directories.

The daemon's stderr events land in the user journal. Read them with:

```
journalctl --user -u demu
```

## 4. The endpoint

The MCP endpoint is the path `/mcp` on the bind address. With the
default flags the endpoint URL is:

```
http://127.0.0.1:6502/mcp
```

The client URL must match the bind address. A daemon started with
`--http [::1]:6502` serves `http://[::1]:6502/mcp`, and a daemon
started with `--http 127.0.0.1:7700` serves
`http://127.0.0.1:7700/mcp`. Point the client at the URL the daemon
actually binds (section 11).

OpenCode connects as a remote MCP server. Add the entry by hand to the
OpenCode configuration, global at `~/.config/opencode/opencode.jsonc`
or project-local at `opencode.jsonc`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "demu": {
        "type": "remote",
        "url": "http://127.0.0.1:6502/mcp",
        "oauth": false
      }
    }
  }
}
```

`"oauth": false` matters: the daemon serves no OAuth metadata, so
OpenCode must not start its OAuth discovery flow. The CLI alternative
`opencode mcp add demu --url http://127.0.0.1:6502/mcp` writes the
entry too; then set `"oauth": false` in the written entry. Check the
connection with `opencode mcp list`.

The server name is the tool prefix. OpenCode surfaces the MCP tool
`session_create` as `demu_session_create`, and the same for the rest
(section 7).

## 5. The trust model

The daemon has no authentication. Loopback binding is the decision
that stands in for it:

- Loopback only. The daemon binds a loopback address: `127.0.0.1` by
  default, or the address the operator passes to `--http`. A non-local
  host cannot reach the endpoint. Exposing the daemon to the network
  requires a proxy you add yourself.
- Any local process can connect and drive sessions. There is no token
  and no user check. Everything a local process can do, the daemon
  lets it do: create sessions, run commands, and write files under
  session output directories.
- DNS rebinding is closed by default. A web page in a browser can
  attempt DNS rebinding against `127.0.0.1:6502`: it points a DNS name
  at the loopback address and hopes the browser sends the request. The
  MCP Streamable HTTP server rejects every request whose `Host`
  header is not one of the loopback names (`localhost`, `127.0.0.1`,
  `::1`), which is the default allowlist, so that attack class is
  stopped before any handler runs. The server library also carries an
  origin allowlist option for stricter hardening.
- Path confinement is the file-level limit. A client reads and writes
  files only under its session's output directory (section 7). The
  commands that touch no files stay unconstrained: the client can run
  arbitrary machine commands. Confinement is lexical: it follows
  symlinks, so a symlink a local process plants inside the session
  directory redirects confined writes outside it. The trust boundary
  is the loopback, not the file system.

## 6. Session lifecycle

Sessions live in the daemon process only. A session survives a client
disconnect: the machine keeps its state, and the next call continues
it. Several MCP connections can use the registry at once; the commands
of one session serialize, so two calls against one session run one
after the other. The session disappears when the daemon stops, and a
daemon restart loses every session and every machine state.

A session is created fully: creation assembles and boots the machine,
so a creation failure leaves no session behind and a creation success
answers a ready machine.

The session name is 1 to 64 characters of ASCII letters, digits,
hyphens, and underscores. The name doubles as a path component: the
default output directory is `<data-root>/<name>` (section 9), so the
rules keep the name a safe directory name.

The daemon admits up to 8 concurrent sessions by default;
`--session-limit <n>` raises or lowers that cap. Creating beyond the
cap is an error until a session is destroyed.

Destroying frees the name and the cap slot at once. A name is free
again after its session is destroyed, and a new session under the
reused name starts with a fresh machine. The previous session's output
directory stays on disk with its files (section 9).

## 7. The tool reference

The daemon serves 16 MCP tools: the four session tools, the `command`
passthrough, and the 11 named debug tools. OpenCode names every tool
`<server>_<tool>`, so with the server name `demu` the tools appear as
`demu_session_create`, `demu_session_list`, `demu_session_info`,
`demu_session_destroy`, and `demu_command`, and a debug tool such as
`run` appears as `demu_run`. The prefix rule applies to all of them.
The subsections below name each tool by its MCP name and give the
OpenCode surface name.

### `session_create` — OpenCode `demu_session_create`

Arguments:

| Argument | Required | Meaning |
|---|---|---|
| `name` | yes | the session name, 1 to 64 characters of ASCII letters, digits, `-`, and `_` |
| `config_toml` | XOR | TOML system-configuration text |
| `config_triple` | XOR | a bundled triple, for example `sm83-nintendo-gameboy` |
| `rom_base64` | no | the ROM image, base64-encoded |
| `symbols_text` | no | the `.opl` symbol table as JSON text, version envelope 1 |
| `output_dir` | no | an output-directory override |

Exactly one of `config_toml` or `config_triple` must be supplied.
A supplied ROM overrides the cartridge device's boot file and reboots
the machine through it; a ROM with no cartridge device in the config
is an error, as in the CLI.

The transport caps one MCP request body at 128 MiB. A 64 MiB ROM
encodes to roughly 85 MiB of base64, so a large ROM plus a large
symbol table can hit the request-body cap; that rejection arrives as
a transport-level error, not a tool error.

Success returns:

```json
{"name": "gb-boot", "output_dir": "~/.local/share/demu/gb-boot",
 "warnings": [{"code": "unused_clock", "message": "..."}]}
```

`output_dir` is the directory the session writes to (section 9).
`warnings` lists the config lint warnings, each with its `code` and
`message`; these are the same warnings `config --check` prints.

Errors surface as MCP tool errors with a human-readable message. The
causes: a duplicate name among the live sessions, the session cap
reached, a bad name, a TOML parse failure, a config validation
failure, no cartridge device for a supplied ROM, a bad symbol table,
an output-directory failure, a bad base64 image, and a decoded ROM
over the 64 MiB cap.

### `session_list` — OpenCode `demu_session_list`

No arguments. Returns a JSON array with one entry per live session:

```json
[{"name": "gb-boot", "output_dir": "~/.local/share/demu/gb-boot"}]
```

### `session_info` — OpenCode `demu_session_info`

Argument `name`. Returns the machine info JSON object, the same shape
as the `info` REPL command: the machine name, the triple, the
description, the device list, the clock list, the tick `now`, and the
`frames` count where a video device exists.

### `session_destroy` — OpenCode `demu_session_destroy`

Argument `name`. Returns:

```json
{"name": "gb-boot"}
```

Destroying an unknown session, or destroying one session twice, is a
clean tool error with the message `no session named ...`. The error is
not a crash and corrupts nothing; a client that destroys twice sees
the same error both times and loses nothing.

### `command` — OpenCode `demu_command`

Arguments:

| Argument | Required | Meaning |
|---|---|---|
| `name` | yes | the session name |
| `cmd` | yes | the demu-shell command word |
| `args` | no | the arguments, an array of strings; omit for a command with no arguments |

The tool runs the demu-shell command on the session and returns the
JSON-lines protocol response object as the tool result. On success:

```json
{"id": null, "ok": true, "data": {"now": 60, "start": 0}}
```

On command failure:

```json
{"id": null, "ok": false, "error": {"code": "bad_address", "message": "..."}}
```

`id` is always `null`: the correlation is the MCP request itself. The
`data` or `error` fields carry the same forms the JSON protocol
returns for the command; the quick table of the [REPL and protocol
reference](repl-reference.md) lists the `data` forms per command. The
error `code` set is the closed protocol set: `unknown_command`,
`bad_args`, `bad_address`, `bad_trigger`, `bad_script`,
`unknown_device`, `unknown_register`, `no_machine`, `io`, `failed`.

MCP tool errors cover everything outside the shell: an unknown session
name, a confinement violation, and a whitespace-bearing argument.

A short tour in the OpenCode surface names:

```
demu_session_create(name="gb-boot", config_triple="sm83-nintendo-gameboy",
                    rom_base64="<image>")
demu_command(name="gb-boot", cmd="info", args=[])
demu_command(name="gb-boot", cmd="run", args=["60"])
demu_command(name="gb-boot", cmd="read", args=["0xFF40"])
demu_command(name="gb-boot", cmd="screenshot", args=["--png", "boot.png"])
demu_command(name="gb-boot", cmd="dump", args=["0x0000", "32", "head.bin"])
demu_session_destroy(name="gb-boot")
```

### The command passthrough rules

The `command` tool wraps the same shell as the REPL, so the command
syntax of the [REPL and protocol reference](repl-reference.md) applies,
with two daemon rules.

One token per argument. Every argument string must be one
whitespace-free token. The shell parser re-splits the joined command
line on whitespace, so a token with a space in it breaks the command
or binds to the wrong argument. Write `mkv-render` input names and
output names without spaces.

Path confinement. The commands that touch files join their path
arguments under the session output directory. A `..` segment and an
absolute path outside the directory are rejected; an absolute path is
accepted when it resolves inside the directory. The confined commands
and their paths:

| Command | Confined path arguments |
|---|---|
| `dump` | the third token is the file path |
| `dump-vram`, `dump-nametable`, `dump-oam`, `dump-palette`, `dump-framebuffer` | the first token is the path; demu appends the layer suffix, such as `.vram`, after confinement |
| `screenshot` | the first non-flag token is the path; the flags are `--png` and `--ppm` |
| `save-state`, `load-state`, `script`, `symbols` | the first token is the path |
| `mkv-input`, `mkv-render` | the first token and the value after `-o` are paths |
| `render-session` | the value after `-o` is the path |

Every other command touches no files. A confinement violation is an
MCP tool error, not a shell `io` error.

### The named debug tools

The named debug tools are the ergonomic path over some `command`
passthrough invocations. Each tool takes the session name `name`,
returns the same JSON data object the JSON-lines protocol returns for
the matching shell command, and reports shell failures as tool errors
whose text is the failure's shell message. The argument values are
typed JSON
(integers, strings, booleans), so the one-token-per-argument rule of
the passthrough does not apply to them.

- `run` — OpenCode `demu_run`. Optional `units` (integer). Without
  `units` the run continues until a registered trigger fires and
  returns `{now, start, trigger_hit}`; with `units` it runs exactly
  that many machine units and returns `{now, start}`. WARNING: a free
  run with no trigger set blocks the session until a trigger hits
  (section 10).
- `run_frame` — OpenCode `demu_run_frame`. Optional `frames` (integer,
  default 1). Returns `{now, start, frames}` with the
  `[start, end]` display-frame counts. A `frames` value of zero waits
  for the first completed display frame, which may never come on a
  machine that produces none (section 10).
- `run_until` — OpenCode `demu_run_until`. Argument `event`, one of
  `vblank`, `irq`, `fetch`, `read`, `write`; optional `addr` (a number
  token; the shell's event addressing, not symbol resolution). Returns
  `{now, start, event}`, where `event` is the fired event or `null`
  when a registered trigger stopped the run instead. WARNING: an event
  that never fires blocks the session (section 10).
- `step` — OpenCode `demu_step`. Optional `units` (integer, default
  1). Returns the same result form as `run` with `units`.
- `read_memory` — OpenCode `demu_read_memory`. Argument `addr` (a
  string token: a symbol name or a number such as `0xFF40`);
  optional `count` (integer, default 16, cap 65536; a larger count is
  rejected before the command runs). Returns `{addr, bytes}` with the
  bytes as a hex text with one space between the bytes.
- `write_memory` — OpenCode `demu_write_memory`. Arguments `addr` (a
  string token) and `bytes_hex` (hex digit pairs; whitespace inside
  the text is ignored). An empty or odd-length text is rejected before
  the command runs. Returns `{addr, written}`.
- `regs` — OpenCode `demu_regs`. No arguments besides the session
  name. Returns the register map object.
- `set_reg` — OpenCode `demu_set_reg`. Arguments `reg` (register
  name) and `value` (an integer, or a number token such as `0xFF`).
  Returns the text form, for example `"A = 0x42"`.
- `break` — OpenCode `demu_break`. Argument `addr` (a string token;
  symbol resolution applies). Sets a breakpoint and returns the text,
  for example `"trigger 1 set"`. A registered trigger makes the next
  free `run` stop and report `trigger_hit`.
- `watch` — OpenCode `demu_watch`. Argument `addr` (a string token).
  Sets a write watchpoint and returns the same trigger text form.
  Trigger management stays with the shell: `command` with `triggers`
  and `clear <id>`.
- `screenshot` — OpenCode `demu_screenshot`. Only the session name.
  Captures the composed frame as PNG into the session directory: the
  deterministic name `screenshot-<tick>.png` carries the machine tick,
  and a `-1`, `-2`, ... counter suffix resolves a same-tick collision.
  The result carries an MCP image content block (base64 PNG,
  `image/png`) plus a text object `{path, width, height, size}`. A
  machine with no video device, and a machine whose video reports no
  frame, fail cleanly as a tool error.

## 8. Out-of-band file placement

The MCP tools carry no file upload. An input script, a save state, or
a Matroska input file reaches a session only through the session
output directory.

The client places the file there with its own file tools, then
commands reference it:

1. Write `demo.scr` into the session output directory with the normal
   file-writing tool.
2. Call `command` with `cmd: "script"` and `args: ["demo.scr"]`. The
   command reads `<output_dir>/demo.scr`.

The same applies to `load-state` (the state file), `mkv-input` (the
input script), and `mkv-render` (the input MKV). Since the confined
commands write into the directory too, a round trip stays inside it:
`save-state state.bin` writes `<output_dir>/state.bin`, and a later
`load-state state.bin` reads it back.

## 9. The session output directory

A session's output directory is where every confined command reads and
writes. The default is the session name joined under the data root:
`<data-root>/<name>`, with the data root of section 2
(`$XDG_DATA_HOME/demu`, else `$HOME/.local/share/demu`). A session may
name its own `output_dir` instead; the loopback trust covers that
choice (section 5).

Examples, with defaults and no `output_dir` override:

- The session `gb-boot` collects its files at
  `~/.local/share/demu/gb-boot`.
- With `--data-root /tmp/demo`, the directory is `/tmp/demo/gb-boot`.

What the directory collects: `dump` images, the layer dumps
(`dump-vram cart` writes `cart.vram` there), screenshots from
`screenshot --png` and `--ppm`, save states, MKV inputs and MKV
renders, the input scripts and states the client placed out of band
(section 8), and the session command log `session.jsonl` of the
named debug tools and the `command` calls (section 7).

The `session.jsonl` command log records one JSON object per logged
command and one per response. A `request` entry carries the tool
name, the key arguments, and a `wall_ms` wall-clock field that is
metadata only. The response entry carries the `ok` flag, the protocol
`error` code string for failures, and the machine `tick`. The entry
pair of one command is consecutive. The file is append-only, so a
reused session name keeps the earlier eras' entries, and a reader
sees an era boundary where the tick starts again. A log write failure
is recorded once on stderr and never fails a command. The daemon
treats `session.jsonl` as its own log name: a confined command that
writes a file of that name makes the daemon append later log entries
to it, so keep the name free of confined output.

The directory and its files stay on disk after the session is
destroyed and after the daemon stops. The files are the durable
record; the sessions are not.

## 10. Blocking behavior

One session is one queue: the commands of one session serialize
(section 6). A command that runs long blocks that queue:

- An unbounded `run` (no argument) keeps running until a registered
  trigger fires.
- A `run-until` whose event never fires runs until the machine makes
  it happen. The `run-until` caution of the [REPL and protocol
  reference](repl-reference.md) lists the no-event cases.
- A `run-frame` with a `frames` value of zero runs until the first
  completed display frame; on a machine that produces no display
  frames, it parks the session.

Both block the session's queue until the command returns. Other
sessions keep answering; the blocked session answers nothing meanwhile,
and later calls against it queue behind the running command. There is
no interrupt: `pause` is not implemented in this build, so a started
run cannot be stopped from the command set.

The MCP tools `run` (without `units`) and `run_until` (with an
unreachable target) carry the same block-the-queue behavior as their
shell forms (section 7).

Make bounded commands the default habit: `step <N>`, `run <N>`,
`run-frame <N>`, and `run-until` with a target the machine actually
reaches. Reach for an unbounded run only when a trigger exists and the
wait is the goal. OpenCode also enforces its own execution timeout on
every MCP tool call; the server's `timeout` object in the OpenCode
configuration sets it explicitly.

A command in flight holds the daemon shutdown too. SIGTERM and SIGINT
stop the acceptor but wait for open MCP connections, and the
connection of a running command stays open until the command returns,
so the shutdown waits for it. Under systemd, a stop with a command in
flight ends at the service stop timeout with SIGKILL, which takes the
daemon and every session with it. Wait for a long-running command, or
accept the loss, before `systemctl --user stop demu`.

## 11. Troubleshooting

**Nothing listens.** The client reports connection refused, and
`opencode mcp list` shows the server down or failing. The daemon is
not running. Start it by hand (section 2) or check the service with
`systemctl --user status demu`.

**Address already in use, exit 2.** Another process holds
`127.0.0.1:6502`, the daemon prints the bind failure, and exits 2
before serving. Find the holder with `ss -tlnp | grep 6502`, stop it,
or bind another address with a changed `--http` (and the matching
OpenCode URL; a service install means editing the copied unit file,
then `systemctl --user daemon-reload`).

**Port mismatch.** The OpenCode URL must match `--http` exactly,
including the brackets for IPv6. A URL of
`http://127.0.0.1:6503/mcp` against a daemon bound to 6502 is
connection refused, with nothing in the daemon journal, because the
request never arrives.

**Session cap reached.** `session_create` fails after 8 live
sessions. A `systemctl --user restart demu` frees nothing useful:
every session is gone after the restart and the machine states with
it. Destroy the sessions you finished with (`session_destroy`), reuse
their names, or raise the cap with `--session-limit`.

**Unknown session after a daemon restart.** Every session lived in
the daemon process, so a restart answers `no session named ...` for
every old name. Create the session again with the same config and ROM.
The machine state of the old session is unrecoverable; the files in
its output directory remain.

**Diagnosing with the journal.** Under systemd, read the daemon's
stderr events with `journalctl --user -u demu`, and follow them with
`journalctl --user -u demu -f`. Startup, shutdown, every session
create and destroy, and every failure print there. A hand-started
daemon prints the same events on its stderr.