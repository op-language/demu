# The demu REPL and protocol reference

This document is the complete command reference for `demu`, the
deterministic debugging emulator. demu runs one machine from a system
configuration. You control the machine with one command set. The same
command set works in three transports:

- The text REPL on stdin/stdout, for humans.
- The JSON-lines protocol on stdin/stdout (`--json`), for machines.
- The JSON-lines protocol over a Unix-domain socket (`--serve <path>`),
  for machines. POSIX only.

The manual index explains the other manual chapters. The technical
design is the arbiter for any behavior question.

## 1. Startup: the command line

```
demu - a cycle-accurate debugging emulator

Usage:
  demu [OPTIONS] [ROM]
  demu config --check <file|triple>

Options:
  --config <triple|file>   system configuration
  --symbols <file.opl>     symbol table
  --dump <path>            memory dump at exit
  --sentinel <addr>:<val>  compare byte at exit, set the exit code
  --json                   JSON-lines protocol on stdio
  --serve <path>           Unix-domain-socket server mode
  -h, --help               show this help
```

Rules:

- `--config <triple|file>` selects the machine. A triple name selects a
  bundled config from the `configs/` directory of the demu source. Any
  other text opens a config file from disk. The default selector is
  the built-in test machine `smoke`. Note the bundled `smoke` machine is
  a framework-test image. Its device kinds are registered by the test
  code, so a session with `--config smoke` reports an assembly error.
  Use a real triple.
- `[ROM]` is the positional cart path. When the config declares a
  cartridge device, the path overrides that device's boot file, and the
  machine reboots through it. A ROM path with no cartridge device in
  the config is a usage error.
- `--symbols <file.opl>` loads a symbol table at start. Symbol names
  then stand for addresses in `read`, `read-n`, `write`, `dump`,
  `fill`, `break`, and `watch`.
- `--dump <path>` writes the exit memory dump: the address-indexed
  image of the primary bus. The file carries 64 KiB. The byte at file
  offset `A` is the bus byte at address `A`. This matches the `cart
  test` dump contract.
- `--sentinel <addr>:<val>` compares the dump byte at `addr` with
  `val` after the session ends. A match exits 0. A miss prints
  `sentinel {addr:#x} = {value:#x} does not match {expected:#x}` on
  stderr and exits 1. Both numbers are decimal or `0x` hex.
- `--json` changes stdin/stdout to the JSON-lines protocol. The command
  set is identical to the text REPL.
- `--serve <path>` serves the JSON-lines protocol on a Unix-domain
  socket. demu removes a stale socket file at the path, binds it, and
  serves one connection at a time. When a client disconnects, demu
  accepts the next connection. The server stays alive until the process
  ends. The `--dump` and `--sentinel` flags do not apply in socket mode.
- `config --check <file|triple>` validates a config with all four
  validation layers and exits before any session starts. Success
  prints `ok: {name} ({triple}) devices={n} clocks={n}`. Warnings
  print as `warning: {code} - {message}`. A validation error prints
  every message and exits 2.

Exit codes:

| Code | Meaning |
|---:|---|
| 0 | success, or a sentinel match |
| 1 | a sentinel miss |
| 2 | usage error, config error, assembly error, I/O error, or socket bind failure |

Command errors inside a session do not change the exit code. The
session ends with exit 0 unless the sentinel misses.

## 2. Syntax and conventions

Text commands:

- One command per line. The command word is the text before the first
  space. The arguments are the rest, split on whitespace.
- Numbers are decimal, or hex with the `0x` or `0X` prefix. Negative
  numbers are not accepted.
- Addresses accept hexadecimal, decimal, or a symbol name when a
  symbol table is loaded. Symbols win over numbers: a symbol with a
  numeric-looking name resolves as a symbol. Use a `0x` prefix when a
  symbol name can also parse as a number.
- Byte arguments for `write` are pairs of hex digits. Whitespace
  between pairs is allowed: `DEADBEEF` and `DE AD BE EF` are the same
  bytes.
- The `-o <path>` word sequence marks the output file for the media
  commands. Exactly one `-o` is allowed.
- The REPL prints the prompt `demu> ` before each command. At end of
  input, or after `quit` or `exit`, demu prints `bye` and ends. A
  command error prints `error: {message}` on the output line and the
  session continues.

JSON mode:

- One request object per line: `{"id": <any>, "cmd": "<name>",
  "args": <any>}`. Both `id` and `args` are optional. The `cmd` field
  is required.
- One response object per line: `{"id": <copied>, "ok": true,
  "data": <value>}` on success, or `{"id": <copied>, "ok": false,
  "error": {"code": "<code>", "message": "<text>"}}` on failure.
- `args` carries the same tokens as the text command. A scalar number
  or a string maps to the bare text. An array maps to its items joined
  with one space. An `args` of `null` maps to no arguments. An object
  is one word for the dispatcher. The behavior then depends on the
  command: `info` ignores it, single-argument commands report their
  resolution error, and multi-argument commands report the schema
  error.
- A malformed request line gets the response
  `{"id": null, "ok": false, "error": {"code": "bad_args",
  "message": "malformed request: ..."}}`.
- Responses carry the request id exactly as sent. Strings and numbers
  are both valid ids.
- Blank lines are skipped. Responses arrive in request order.
- demu writes no log lines to the protocol stream in JSON mode.

The closed set of protocol error codes:

| Code | Meaning |
|---|---|
| `unknown_command` | reserved in the code set. Not produced as of this build. |
| `bad_args` | the arguments do not match the command form, or the command name is unknown |
| `bad_address` | the address, symbol, or byte span does not resolve |
| `bad_trigger` | the trigger text does not parse |
| `bad_script` | the input script does not parse or validate |
| `unknown_device` | the device name is unknown, or no CPU answers |
| `unknown_register` | the register name is unknown |
| `no_machine` | no machine is loaded |
| `io` | a file operation failed |
| `failed` | the command failed for a run-time reason |

Notes on the as-built behavior: an unknown command name produces
`bad_args` with the message `unknown command: {name}`, not
`unknown_command`. An unknown device name on `inject` produces
`unknown_device`. Related device-absence conditions elsewhere, for
example no video device on a video command, surface as `failed`.
`unknown_command` exists in the protocol type but no
command path emits it in this build.

## 3. Session commands

### `info`

Prints the machine summary. `status` is an alias.

```
demu> info
Nintendo Game Boy (sm83-nintendo-gameboy) at tick 0
```

JSON data object:

```json
{"name": "Nintendo Game Boy", "triple": "sm83-nintendo-gameboy",
 "description": "The 4.19 MHz monochrome Game Boy with the DMG boot ROM.",
 "devices": ["cpu0", "bootrom", "cart", "wram", "hram", "ppu", "apu",
             "dma", "timer", "joypad", "serial", "io"],
 "clocks": ["main"], "now": 0, "frames": 0}
```

`frames` is the display-frame count of the first video device, or
`null` when the machine declares no video device. `now` is the primary
domain tick.

## 4. Run commands

All runs apply scheduled input-script events at their exact instants.
All stop points are deterministic. The run commands return
`{"now": <tick at stop>, "start": <tick at start>}`.

### `step [N]`

Runs N tick units of the primary domain. The default is 1.

```
demu> step 20
ran to 20
```

### `run [N]`

Runs N tick units. Without an argument, the run continues freely until
a registered trigger fires. The free run adds `"trigger_hit": <bool>`
to the JSON data.

```
demu> run 100
ran to 120
```

### `run-frame [N]`

Runs N display frames. The run loop counts vblank events and stops
after N. The stop condition reads the vblank events of every video
device. The JSON `frames` array reads the frame counter of the first
video device. The
default is 1. The text output is `ran to tick {now}`. The JSON data
adds `"frames": [<frame count before>, <frame count after>]`.

### `run-until <kind> [addr]`

Runs until a runtime event fires. The kinds are `vblank`, `irq`,
`fetch <addr>`, `read <addr>`, and `write <addr>`. A missing address
matches every event of the kind. The JSON data carries
`"event": "<the Debug form of the trigger event>"`, or `"event": null`
when the run reached the idle target without an event.

```
demu> run-until fetch 0x0150
ran to 23524784
```

Caution: an event that never fires, with an unbounded kind, runs free
without a stop. A machine with no cart never completes the boot-ROM
logo check, so `run-until fetch 0x0100` never fires. Bound such runs:
load the cart, use `run-frame`, or match an event the machine
produces. The address must be a number: a symbol name is not
accepted here, and any unparseable address silently matches every
event of the kind. An address of 0 also matches every event. An
unknown kind word matches no event, so the run is unbounded.

### `pause`

Not implemented in this build. A run command that has started cannot
be interrupted from the command set. Stop runs with a bounded target
or a trigger.

## 5. Register commands

### `regs`

Prints the register set of the machine's first CPU device, space
separated. One-byte registers with the one-letter names (for example
the SM83 `A`, `B`, `C`, `D`, `E`, `H`, `L`, `F`, and the 6502 `A`)
print as `K=XX`. Every other register prints in the `0x`-prefixed
form that the width needs. The
JSON data is one object that maps
register names to numbers.

```
demu> regs
A=00 AF=0x0000 B=00 BC=0x0000 C=00 D=00 DE=0x0000 E=00 F=00 H=00 HL=0x0000 IE=0x0000 IF=0x00E1 IME=0x0000 L=00 PC=0x0000 SP=0xFFFE
```

### `set-reg <name> <value>`

Sets one CPU register.

```
demu> set-reg A 0x7F
A = 0x7f
```

Register names are per CPU family. Read the machine chapter for the
name list. An unknown name is the `unknown_register` error.

## 6. Memory commands

### `read <addr>`

```
demu> read 0x0100
0100: C3 00 00 00 CE ED 66 66 CC 0D 00 0B 03 73 00 83
```

Peeks 16 bytes through the bus decoder of the primary bus. The data
path is side-effect free. The text form prefixes the address. The
bytes are space-separated uppercase hex.

```
demu> read 0x0100
0100: C3 00 00 00 CE ED 66 66 CC 0D 00 0B 03 73 00 83
```

The read goes through the CPU (primary) bus only. The JSON data adds
`window` beside `addr` and `bytes`. `window` is the name of the bus
window that answered the first byte of the read, or `null` when no
window answers.

On the NES the bus does not expose the palette RAM. A read at `$3F00`
routes through the `w_ppu_regs` mirror window and answers the 2C02
debug peek for write-only registers. That answer is deterministic, and
it is not palette data. The truthful background-palette read is
`dump-palette`, which dumps the 2C02 palette RAM (32 bytes) to a file.

### `read-n <addr> <n>`

Peeks `n` bytes at the address. Same output form as `read`.

### `write <addr> <bytes>`

Writes bytes through the debug-write path. The path follows the same
address rules as real bus traffic, including overlaid windows and
borrowed-decoder bridges. Byte arguments are hex pairs.

```
demu> write 0xFF40 AA
wrote 1 bytes at FF40
```

A write obeys the device's own access rules. A locked device range
drops the store: the SM83 PPU locks the VRAM and the OAM during
rendering, so a write there reports success and stores nothing. A
read is a force peek and shows the stored state either way. When the
target range has no writable window at all, the command fails with
`only {n} of {m} bytes landed in a writable window`.

### `dump <addr> <n> <file>`

Peeks `n` bytes and writes them to a file.

```
demu> dump 0x0000 32 /tmp/dump.bin
dumped 32 bytes at 0000 to /tmp/dump.bin
```

The peeks go through the CPU (primary) bus. The JSON data carries the
same `window` field as `read`.

### `fill <addr> <n> <byte>`

Fills `n` bytes with one byte value through the debug-write path. The
byte argument is a number: decimal, or `0x` hex. The plain hex-pair
form of `write` does not apply here.

```
demu> fill 0xC000 4 0xFF
filled 4 bytes at C000
```

## 7. Trigger commands

Triggers stop free runs and carry counters. The registry assigns ids
from 1.

### `break <addr|symbol>`

Sets a breakpoint at an instruction address. The core reports a
breakpoint when an instruction fetch lands at the address.

```
demu> break 0x0150
trigger 1 set
```

Note the usage text of this command advertises an optional condition
(`break <addr|symbol> [cond]`), but this build rejects the second
argument. A condition form is a follow-up.

### `watch <addr>`

Sets a watchpoint on a memory write at the address.

```
demu> watch 0xFF46
trigger 2 set
```

### `triggers`

Lists the live triggers, one per line, with the id and the kind in the
Rust debug form:

```
demu> triggers
1: Breakpoint { address: Address(336) }
2: WriteWatch { address: Address(65350) }
```

### `clear <id>`

Removes one trigger.

```
demu> clear 1
cleared
```

An unknown id answers `no such trigger`.

The internal registry also models io-port read and write triggers,
read watches, line-change triggers, cycle-count and clock-edge
triggers, IRQ, fetch, DMA-phase, vblank, and scanline kinds. The
command surface of this build constructs only breakpoints and write
watchpoints. The other kinds are reachable through `run-until` for
vblank, IRQ, fetch, read, and write events. The design lists the
full kind set. The manual records the as-built command surface here.

## 8. Video commands

Every video command needs a video device and, for screenshots, one
complete frame.

### `screenshot [--ppm|--png] <path>`

Writes the composited display frame. The default format is binary PPM
(P6). The `--png` word selects PNG (RGB, 8-bit). The `--ppm` word is
accepted and selects PPM. One path is required.

```
demu> screenshot --png /tmp/frame.png
wrote /tmp/frame.png (160x144)
```

### Layer dumps

`dump-vram`, `dump-nametable`, `dump-oam`, `dump-palette`, `dump-pattern`,
and `dump-framebuffer` write one raw layer each. demu appends the layer
name to the path you give, so `dump-vram /tmp/gb` writes
`/tmp/gb.vram`. The layer byte layout is per device family. The machine chapter
names it.

```
demu> dump-vram /tmp/gb
dumped vram to /tmp/gb.vram
```

`dump-pattern` dumps the cartridge CHR store (the pattern-table bytes).
demu appends `.pattern`, so `dump-pattern /tmp/chr` writes
`/tmp/chr.pattern`. On the NES machines the cartridge CHR is the
pattern table. The PPU carries no pattern-table layer of its own, so
`dump-pattern` is the CHR dump of the family.

```
demu> dump-pattern /tmp/chr
dumped 8192 pattern bytes to /tmp/chr.pattern
```

## 9. State commands

### `save-state <file>`

Writes a full deterministic snapshot of the machine: every device
chunk, the scheduler, every clock domain, and a checksum.

```
demu> save-state /tmp/state.bin
saved to /tmp/state.bin
```

### `load-state <file>`

Restores a snapshot made by the same machine format version and the
same machine. A mismatch is the `failed` error.

```
demu> load-state /tmp/state.bin
loaded from /tmp/state.bin
```

## 10. Symbol commands

### `symbols <file.opl>`

Loads a `.opl` or `.linked.opl` symbol table. The table is a JSON
envelope with the version, the target, and sections of symbols. The
reader checks the envelope version and ignores unknown fields.

```
demu> symbols cart.linked.opl
loaded 124 symbols
```

### `sym <name>`

Looks up one symbol.

```
demu> sym boot_start
boot_start = 0x100 (label code)
```

## 11. Input commands

### `inject <device> <event text>`

Sends one input event to one device. The event grammar is per device
family. Machine chapters document the exact event words. Examples:
`inject joypad press a` for the Game Boy pad, `port 0 press 8` for the
Genesis IO die.

```
demu> inject joypad press a
injected press a
```

An unknown device or an unknown event word is an error.

### `script <file>`

Loads an input script, validates it against the loaded machine, and
installs it as the run schedule. Events then apply during every run
command at their exact instants.

```
demu> script demo.scr
loaded 42 input events
```

The script grammar is specified in `docs/manual/media-pipeline.md`.

## 12. Media commands

### `mkv-input <script> -o <file>`

Converts an input script into the input form of a Matroska file. The
file carries one event track, the system configuration as tags plus an
attachment, and the machine summary.

```
demu> mkv-input demo.scr -o demo-input.mkv
wrote 42 event blocks to demo-input.mkv
```

### `mkv-render <input.mkv> <frames> -o <output.mkv>`

Rebuilds the machine from the file's embedded configuration, replays
its events over `frames` display frames, and records the render form.
The render form carries FFV1 lossless video at one frame per emulated
display frame. It also carries the native-rate PCM track and the
resampled 48 kHz PCM track as the first and default track.

```
demu> mkv-render demo-input.mkv 120 -o demo.mkv
recorded 120 frames to demo.mkv
```

### `render-session [frames] -o <file>`

Records the current session onward for `frames` display frames into
the render form. A `frames` of 0 records nothing and prints
`recorded 0 frames` with no file written.

```
demu> render-session 60 -o session.mkv
recorded 60 frames to session.mkv
```

A machine with no video device is the `failed` error. A machine with
audio devices of different sample rates is also an error. The recorder
drains audio after each display frame in device declaration order.

## 13. Socket-mode session example

```
$ rm -f /tmp/demu.sock
$ demu --serve /tmp/demu.sock --config sm83-nintendo-gameboy &
$ python3 - <<'EOF'
import socket, json
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/demu.sock")
def send(mid, cmd, args=None):
    req = {"id": mid, "cmd": cmd}
    if args is not None:
        req["args"] = args
    s.sendall((json.dumps(req) + "\n").encode())
    return json.loads(s.makefile("rb").readline().decode())
print(send(1, "info"))
print(send(2, "step", 60))
print(send(3, "read", "0xFF40"))
s.close()
EOF
{"id": 1, "ok": true, "data": {"name": "Nintendo Game Boy", ...}}
{"id": 2, "ok": true, "data": {"now": 60, "start": 0}}
{"id": 3, "ok": true, "data": {"addr": "0xff40", "bytes": "91 82 ..."}}
```

The `quit` command over the socket answers `{"ok": true,
"data": "bye"}` and the server keeps the socket open for the next
request. End the session by closing the client, and stop the server
process when the debug work is done.

## 14. As-built notes for the interface

The technical design summary lists the planned command set. This
build differs in these ways:

- `read` reads a fixed 16 bytes at one address. `read-n <addr> <n>`
  replaces the optional length of the planned form.
- `break` and `watch` take exactly one token. The condition form is
  not reachable from the command surface.
- The `load`, `config`, `reset`, `power`, `pause`, `on-io`, `on-line`,
  `on-cycle`, `on-irq`, and `on-fetch` commands of the design summary
  are not implemented in this build. `run-until` covers the vblank,
  IRQ, fetch, read, and write stop events. Machine assembly happens
  at startup only.
- The `--checks <file>` flag of the design summary is not implemented
  in this build. The checks-file schema is a follow-up.
- `mkv-render` takes the frame count as a required second positional.
- `render-session` requires `-o <file>`.
- The `unknown_command` protocol code is not produced. Unknown
  commands surface as `bad_args`.
- Exit code 1 means sentinel miss only.
- `run` counts primary-domain tick units on every machine.
  Frame-paced runs use `run-frame`.

## 15. The JSON data forms quick table

The JSON renderer returns the typed data of some commands and the
plain text of the rest. The table completes the per-command JSON
`data` forms:

| Command | `data` on success |
|---|---|
| `info`, `status` | the machine summary object |
| `regs` | the register name to number object |
| `set-reg` | the plain string, for example `A = 0x7f` |
| `read`, `read-n` | `{"addr": "0x...", "bytes": "...", "window": "<name or null>"}` |
| `write`, `fill` | `{"addr": "0x...", "written": <n>}` |
| `run`, `step` | `{"now": <n>, "start": <n>}` |
| free `run` | `{"now", "start", "trigger_hit": <bool>}` |
| `run-until` | `{"now", "start", "event": <debug string or null>}` |
| `run-frame` | `{"now", "start", "frames": [<before>, <after>]}` |
| `break`, `watch` | the plain string `trigger <id> set` |
| `triggers` | the listing as a plain string |
| `clear` | `cleared` or `no such trigger` as a plain string |
| `dump` | `{"addr": "0x...", "written": <n>, "window": "<name or null>"}` |
| `screenshot` | `{"path", "width", "height", "format"}` |
| layer dumps | the plain string `dumped <layer> to <file>` |
| `dump-pattern` | `{"path": "<file>", "bytes": <n>}` and the text `dumped <n> pattern bytes to <file>` |
| `save-state`, `load-state` | the plain string `saved to <path>` or `loaded from <path>` |
| `symbols` | the plain string `loaded <n> symbols` |
| `sym` | the plain string `<name> = <addr> (<kind> <section>)` |
| `inject` | the plain string `injected <event>` |
| `script` | `{"events": <n>}` |
| `mkv-input` | `{"events": <n>, "path": "<path>"}` |
| `mkv-render`, `render-session` | `{"frames": <n>, "path": "<path>"}` |
| `quit`, `exit` | the plain string `bye` |