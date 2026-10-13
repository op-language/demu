---
name: demu-rom-dev
description: Write, test, and debug an Op ROM through the demu daemon MCP tools in OpenCode. Use this skill to boot a built cart in a daemon session, run and step the emulated machine, read and write memory, set breaks and watches, take screenshots, inject input, and save or restore state. This mirrors the demu-debug skill, but speaks the daemon MCP transport instead of the Unix socket.
---

# demu-rom-dev

Write, test, and debug an Op-machine ROM with `demu`, the deterministic
debugging emulator. The daemon hosts named emulator sessions as an MCP server
on the loopback interface. OpenCode reaches each session through the MCP
tools of the configured `demu` server. This skill teaches the loop:

1. Build the ROM with `cart`.
2. Boot it in a daemon session with `demu_session_create`.
3. Debug the machine with the tools below.
4. Rebuild the ROM after each source change.
5. Repeat from step 2.

The complete tool reference is section 7 of the daemon reference at
`~/.config/demu/docs/manual/daemon-reference.md`, which the install
script `packaging/install.sh` ships; in a demu repository checkout, the
same file is `docs/manual/daemon-reference.md`. The command syntax
reference is `~/.config/demu/docs/manual/repl-reference.md`
(`docs/manual/repl-reference.md` in a checkout).

## Prerequisites

1. The daemon runs as a systemd user daemon. Check with
   `systemctl --user status demu`.
2. The OpenCode configuration has the `mcp.servers.demu` entry.
   `opencode mcp list` shows `✓ demu  connected`.
3. A session output folder. The `demu_session_create` result names it
   `output_dir`.

## Session lifecycle

One session is one machine with one session output directory. Sessions live
in the daemon process and survive a client disconnect. A daemon restart
loses every session and every machine state.

The MCP tools carry the `demu_` prefix, for example `demu_session_create`.
Use the exact names. Under OpenCode's Code Mode, the same tools appear as
`tools.demu.<tool>`, where `<tool>` is the tool name without the prefix.

### Create a session

`demu_session_create` assembles and boots the machine. The result carries
`name`, `output_dir`, and `warnings`. The `warnings` value is the warning
list that `config --check` prints.

| Argument | Required | Meaning |
|---|---|---|
| `name` | yes | 1 to 64 characters of ASCII letters, digits, `-`, and `_` |
| `config_toml` | one of two | TOML system-configuration text |
| `config_triple` | one of two | a bundled triple, for example `sm83-nintendo-gameboy` |
| `rom_base64` | no | the ROM image, base64-encoded |
| `symbols_text` | no | the `.opl` symbol table as JSON text, version envelope 1 |
| `output_dir` | no | an output-directory override |

Supply exactly one of `config_toml` and `config_triple`. A supplied ROM
overrides the cartridge boot file and reboots the machine through it. A ROM
with no cartridge device in the config is an error.

Encode the ROM image with `base64`:

```sh
base64 -w0 <rom-file>
```

Pass the text as `rom_base64`. The decoded image can reach 64 MiB.

For symbols, read the `.opl` symbol table that `cart` writes. Pass its text
as `symbols_text`. Symbol names then stand for addresses in the memory and
trigger tools.

The default session output directory is the session name under the daemon
data root. An `output_dir` override replaces it. The directory collects the
dump files, the screenshots, the save states, and the command log
`session.jsonl`.

### Inventory and destroy

`demu_session_list` takes no arguments. It returns one entry per live
session. `demu_session_info` takes `name` and returns the machine summary:
the triple, the devices, the clocks, and the tick `now`. The summary also
carries the display-frame count.

`demu_session_destroy` takes `name` and ends the session. The name is free
again after destroy. A new session under a reused name starts with a fresh
machine. The old session output directory and its files stay on disk.

## The debug loop

Work in small steps.

1. Call `demu_session_info` first. It confirms the session and prints the
   tick.
2. Advance a bounded number of units with `demu_run_frame` or `demu_step`.
3. Call `demu_regs` after every stop.
4. Call `demu_read_memory` around the suspect addresses.
5. Set `demu_break` and `demu_watch` where the program misbehaves.
6. Call `demu_screenshot` after each suspect change.
7. Record the machine tick `now` from every response.

WARNING: `demu_run` with no `units` blocks the session until a trigger
fires. There is no interrupt. A `demu_run_frame` call with a `frames` value
of zero waits for the first display frame, and it can park the session too.
OpenCode applies its own execution timeout to every MCP tool call. Make
bounded calls the default habit.

## The named debug tools

Each named tool takes the session name `name` and returns the JSON data of
the matching shell command. The argument values are typed JSON integers,
strings, and booleans. They do not obey the one-token rule of the
passthrough section.

- `demu_run` takes the optional integer `units`. With `units`, it runs
  exactly that many machine units. Without `units`, it runs free until a
  registered trigger fires.
- `demu_run_frame` runs display frames. `demu_step` runs primary-domain
  tick units. Both take their count as an optional integer argument.
- `demu_run_until` runs until an event: `vblank`, `irq`, `fetch`, `read`,
  or `write`. The optional `addr` argument is a number token, for example
  `0x0150`. Symbol names do not resolve here. An
  unparseable address matches every event of the kind.
- `demu_read_memory` takes `addr` and an optional `count`. The `addr` value
  is a symbol name or a number, for example `0xFF40`. The default `count`
  is 16, and the cap is 65536. The response carries the bytes as hex text.
- `demu_write_memory` takes `addr` and `bytes_hex`. The `bytes_hex` value
  is pairs of hex digits. Use the tool for a targeted patch.
- `demu_regs` returns the register map. `demu_set_reg` takes `reg` and
  `value`, for example `A` and `0x7F`.
- `demu_break` takes `addr` and sets a breakpoint. `demu_watch` takes
  `addr` and sets a write watchpoint. A registered trigger makes the next
  free `demu_run` stop with `trigger_hit`. List the triggers with the
  `triggers` command over `demu_command`.
- `demu_screenshot` writes `screenshot-<tick>.png` into the session output
  directory and returns the PNG image together with
  `{path, width, height, size}`. It needs a video device and one complete
  frame.

## The passthrough tool

`demu_command` runs any demu shell command on one session. The argument
`name` selects the session. The argument `cmd` is the shell command word.
The optional `args` value is an array of strings.

Every argument must be one whitespace-free token. The daemon re-splits the
joined command line on whitespace. A token with a space in it breaks the
command or binds to the wrong argument.

The result is the JSON-lines response object of the command. It carries
`"ok": true` with a `data` field on success, and an `error` object on
failure. An unknown session name, a confinement violation, or a
whitespace-bearing argument arrives as a tool error. An unknown shell
command word arrives as the `bad_args` error.

Examples:

```
demu_command(name="gb-boot", cmd="run-until", args=["vblank"])
demu_command(name="gb-boot", cmd="run-until", args=["fetch", "0x0150"])
demu_command(name="gb-boot", cmd="inject", args=["joypad", "press", "a"])
demu_command(name="gb-boot", cmd="script", args=["demo.scr"])
demu_command(name="gb-boot", cmd="save-state", args=["state.bin"])
demu_command(name="gb-boot", cmd="load-state", args=["state.bin"])
demu_command(name="gb-boot", cmd="dump", args=["0x0000", "32", "head.bin"])
demu_command(name="gb-boot", cmd="dump-vram", args=["cart"])
demu_command(name="gb-boot", cmd="fill", args=["0xC000", "4", "0xFF"])
demu_command(name="gb-boot", cmd="symbols", args=["cart.linked.opl"])
```

The layer-dump family is `dump-vram`, `dump-nametable`, `dump-oam`,
`dump-palette`, and `dump-framebuffer`. demu appends the layer suffix, such
as `.vram`, to the path. The event grammar of `inject` is per device family.

### Paths and input files

A path argument of a file command resolves under the session output
directory. demu rejects a `..` segment and an absolute path outside the
directory.

There is no file upload. Write input files into the session output
directory with the normal file tools. Then reference them by the bare name
in the command arguments. For example, write `demo.scr` into the directory
first. Then call `script` with `demo.scr`. A `save-state` file also lands
in the directory, and `load-state` reads it back.

## The build workflow

Build the ROM with `cart` outside demu. `cart` writes the ROM image and the
`.opl` symbol table in its build tree. Boot the image through the daemon
(see `Session lifecycle`).

The daemon is deterministic. Use `save-state` and `load-state` for run
comparisons. Save a state at a chosen tick. Load it into a session with the
same machine configuration. Two machines restored from the same save
continue byte-identically.

## Troubleshooting

- Server not connected. `opencode mcp list` does not show
  `✓ demu  connected`. Check the `/mcps` panel of the interface. The entry
  for the daemon needs `"oauth": false`.
- Daemon down. Check `systemctl --user status demu`. Read the journal with
  `journalctl --user -u demu`.
- Stale client refusal. The daemon runs, but `opencode mcp list` keeps
  reporting `ConnectionRefused`. The opencode background service holds the
  failed MCP client state of the project directory and does not re-attempt
  it. Reconnect the client with
  `opencode api post '/api/experimental/mcp/demu/connect?location[directory]=<project-directory>'`,
  or restart the service with `opencode service restart`.
- Session cap reached. `demu_session_create` fails after 8 live sessions.
  Destroy finished sessions. Reuse their names.
- A call blocks. A free `demu_run` waits for a trigger. A `demu_run_until`
  with an unreachable target waits forever. There is no interrupt.
- Tool not found. Check the exact tool name with the `demu_` prefix. In
  Code Mode the tools appear without the prefix in `tools.demu.<tool>`.
- Unknown session. The daemon restarted, and sessions are transient. Create
  the session again with the same config and ROM.
- Daemon stopped mid-session. The machine state is gone. The files in the
  session output directory remain.