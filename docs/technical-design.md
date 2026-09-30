# demu Technical Design

This document is the technical specification of `demu`, a cycle-accurate
debugging emulator for the 32 target triples that `opc` and `std` support.
The words MUST, MUST NOT, SHALL, SHALL NOT, and MAY are RFC 2119 words.

The prose in this document follows ASD-STE100 (strict): short sentences, one
idea per sentence, active voice, and a controlled vocabulary.

Each later phase reads this document and records as-built notes when the
design changes.

## 1. Scope and goals

demu emulates one machine at a time. The user selects a machine through the
system-configuration DSL. demu constructs the machine at startup from the
configuration.

demu MUST produce identical results for identical inputs: the same ROM, the
same configuration, the same injected input, and the same command sequence
MUST produce the same final state on every run.

demu MUST NOT use floating-point arithmetic anywhere in the simulation core.
All timing arithmetic is exact integer or rational arithmetic.

demu is headless. demu MUST NOT open windows or produce live audio.

## 2. Clock model

### 2.1 Clock domains

A clock domain models one hardware crystal. Each system declares one or more
domains. A domain declares its crystal frequency as an exact rational
`num/den` in Hz. An integer-Hz crystal is the trivial case where `den = 1`.
Example: the NES NTSC master crystal is `236250000/11` Hz.

Each domain owns one unsigned integer tick counter. The counter counts
crystal periods and never wraps for realistic session lengths.

### 2.2 Derived clocks

Every device in a domain runs at `crystal / N` where `N` is a positive
integer. This matches real divider chains. The configuration validation
SHALL reject any device ratio that is not an exact integer divider of its
domain crystal.

A device MAY publish several named dividers. For example, an SM83 machine
publishes the CPU divider and the PPU dot divider from the same domain.

### 2.3 Unrelated crystals

Systems with two unrelated crystals (SNES, Apple IIgs) declare two or more
domains. Two domains never share a timeline. Cross-domain signals cross only
through declared connections. The connection semantics are one of:

- `irq` — an interrupt request edge.
- `dma-request` — a DMA handoff.
- `display-sync` — a frame or scanline boundary signal.
- `async-port` — a free-running port pair. One side writes at its own
  cycle; the other side samples on its own tick edges. The SNES audio ports
  are the model case.

### 2.4 Cross-domain ordering

The scheduler keeps one event queue per domain. When the scheduler must
order events from two domains, it compares domain times as reduced
fractions: for times `a/b` and `c/d`, the comparison is `a*d` versus `c*b`
in exact integer arithmetic.

A domain time is the pair (tick count, domain frequency). The scheduler
SHALL NOT convert domain times to a common grid before comparison.

Tie-breaking is deterministic: the device registration order in the
configuration decides which device runs first.

### 2.5 Boundary conversion

Ticks are the canonical internal time. Human-facing artifacts (input
scripts, Matroska timestamps) carry nanoseconds. The conversion is:

```
ticks = round_half_to_even(ns * num / (den * 1_000_000_000))
ns    = round_half_to_even(ticks * den * 1_000_000_000 / num)
```

The `Frequency` type in `demu-core` implements the conversion in one
function. All conversions MUST call this function. No other rounding rule
is permitted.

## 3. Time types

The following newtypes live in `demu-core`:

- `Frequency` — a reduced rational `num/den` with `num > 0`, `den > 0`. The
  constructor reduces by GCD and rejects zero. Comparison is exact.
- `Tick` — a `u64` count of domain ticks.
- `Address` — a `u32` bus address.
- `Divider` — a positive integer divisor.

All newtypes validate at construction, expose no public inner field, and
implement conversions through the canonical constructor.

## 4. Device traits

The device model lives in `demu-core`. Four traits define device behavior:

### 4.1 `Device`

Every simulated component implements `Device`:

- `reset` — restore power-on or configured boot state.
- `boot` — apply the configured boot state (ROM load, RAM pattern, register
  presets).
- `snapshot` — write the full device state as bytes.
- `restore` — read a snapshot written by the same device version.
- `notify_line` — receive a signal-line edge from a connected device.

The scheduler calls `notify_line` when a connection delivers an event. A
device that registers a next-event time SHALL be visited by the scheduler
at that time.

### 4.2 `BusAccess`

Memory-mapped components implement `BusAccess`:

- `read(width, address, tick) -> u32`
- `write(width, address, value, tick)`
- `peek(address) -> u8` — a side-effect-free read for debug commands.

The `tick` parameter carries the domain tick at the moment of the access.
Per-access timing means each CPU steps per bus access and stamps every
access with its exact tick.

### 4.3 `MappedDevice`

A device that occupies address space implements `MappedDevice`:

- `windows() -> &[Window]` — the address windows the device owns.
- `wait_states(address) -> u32` — extra cycles the bus must spend.

### 4.4 `SignalLine`

A signal line carries one bit between devices. A line has a level, edge
events, and, where the hardware needs it, open-collector semantics (several
drivers pull the line low; no driver pulls it high).

## 5. Scheduler

The scheduler is event-driven. Each device declares its next event time as
a domain tick. The scheduler algorithm:

1. Find the domain with the smallest next-event time (exact rational
   comparison, section 2.4).
2. Advance that domain's tick counter to the event time.
3. Dispatch the event to the device.
4. Repeat.

The scheduler MUST NOT allocate in the per-event path. Event queues are
pre-allocated binary heaps keyed on `Tick` with a stable tie-break index.

Sync points: the shell requests a stop at a target (cycle count, trigger
hit, frame boundary). The scheduler computes the stop time and runs until
the stop time or a trigger fires.

## 5.2 Bus and address decoder

A machine declares buses and windows:

- A window maps an address range `[base, base + size)` to one device.
- A mirror repeats a window at a stride.
- An overlay puts two windows at the same range. The configuration declares
  which window wins for reads and writes.
- A bus conflict policy decides what happens when two devices drive one
  bus. The policy is per-window: `first`, `and`, or `last-wins`.
- A DMA-parented bus is a bus owned by a device (the DMA engine) that
  borrows the address decoder of another bus for the duration of a
  transfer.

The address decoder resolves an address to a device value without trait
objects. The machine assembly stores one typed enum value per device-family
crate (see section 8).

## 6. Triggers and expressions

### 6.1 Trigger kinds

The trigger registry accepts these kinds:

- breakpoint (instruction address)
- watchpoint (memory read or write at an address or range)
- I/O port read or write
- signal line level change
- cycle count reached
- clock edge on a named domain and divider
- IRQ asserted
- instruction fetch complete
- DMA event (start, complete)
- vblank
- scanline reached

Each trigger carries an optional condition expression. When the condition
is absent, the trigger fires on every event.

### 6.2 Expression grammar

The condition language is a small expression language embedded in string
fields of the configuration and of REPL commands.

```
expr    := or
or      := and ( "||" and )*
and     := cmp ( "&&" cmp )*
cmp     := sum ( ("==" | "!=" | "<" | "<=" | ">" | ">=") cmp )?
sum     := term ( ("+" | "-") term )*
term    := factor ( ("*" | "/" | "%") factor )*
factor  := "!" factor | "(" expr ")" | number | ident
number  := decimal | "0x" hex
ident   := register name | symbol name | device-qualified name
```

Operator precedence, from low to high: `||`, `&&`, comparison, additive,
multiplicative, unary `!`, primary.

Evaluation semantics: all arithmetic is unsigned 64-bit arithmetic. A
register wider than 64 bit does not exist in the supported machines.
Division by zero evaluates to 0. A comparison evaluates to 1 or 0. An
unknown identifier is a configuration error, evaluated once at trigger
creation.

## 7. Interactive interface

### 7.1 Transports

demu has three modes:

- Text REPL on stdin/stdout for humans.
- JSON-lines mode (`--json`) on stdin/stdout for machines.
- Socket mode (`--serve <path>`): a Unix-domain socket that speaks the
  JSON-lines protocol. POSIX only. One connection at a time.

All three modes run the same command set. One protocol, two transports.

### 7.2 REPL commands

| Family | Commands |
|---|---|
| Session | `load`, `config`, `info [device]`, `reset`, `power` |
| Run | `step [N]`, `run [N]`, `run-frame [N]`, `run-until <trigger>`, `pause`, `status` |
| Registers | `regs`, `set-reg <name> <value>` |
| Memory | `read <addr> [n]`, `write <addr> <bytes>`, `dump <addr> <n> <file>`, `fill <addr> <n> <byte>` |
| Triggers | `break <addr\|symbol> [cond]`, `watch <addr> [cond]`, `on-io <port> [cond]`, `on-line <device.line> <level>`, `on-cycle <n>`, `on-irq`, `on-fetch`, `triggers`, `clear <id>` |
| Video | `screenshot [--ppm|--png]`, `dump-vram`, `dump-nametable`, `dump-oam`, `dump-palette`, `dump-framebuffer` |
| State | `save-state <file>`, `load-state <file>` |
| Symbols | `symbols <file.opl>`, `sym <name>` |
| Input | `inject <device> <event>`, `script <file>` |
| Media | `mkv-input <script> -o <file>`, `mkv-render <input.mkv> -o <output.mkv>`, `render-session` |

Deterministic run commands only. `run [N]` runs N machine units (cycles of
the primary domain or display frames, as the machine declares). `run-until`
accepts a trigger expression.

### 7.3 JSON-lines protocol

Each request is one JSON object on one line. Each response is one JSON
object on one line.

Request:

```json
{"id": 1, "cmd": "read", "args": {"addr": "0x8000", "n": 16}}
```

Response:

```json
{"id": 1, "ok": true, "data": {"bytes": "00 11 22"}}
```

Error response:

```json
{"id": 1, "ok": false, "error": {"code": "bad_address", "message": "..." }}
```

Framing rules:

- One object per line, terminated by `\n`.
- The `id` value is copied from request to response. A client MAY use
  integers or strings.
- Responses arrive in request order.
- demu MUST NOT write log lines to stdout in JSON mode. Logs go to stderr
  or nowhere.
- Errors carry a stable machine-readable `code` from a closed set, plus a
  human-readable `message`.

The full per-command schema is specified with the command implementations
and documented in the manual.

### 7.4 Symbols

demu reads `.opl` and `.linked.opl` files. These files are JSON dumps of the
compiler's object file behind an envelope `{version, ...}`. demu defines
its own subset schema:

```json
{
  "version": 1,
  "target": "...",
  "sections": [
    {"name": "...", "org": 0, "bank": 0,
     "symbols": [{"name": "...", "offset": 0, "size": 0, "kind": "label"}]}
  ]
}
```

The reader MUST check the envelope `version` field against the supported
version (currently 1) and MUST ignore unknown fields. Symbol lookup serves
breakpoints, watchpoints, and display by name.

## 8. Machine assembly and the kind registry

Device behavior lives in compiled device-family crates. Each crate
registers its device kinds in a static table keyed by a stable `kind`
string, for example `"sm83.cpu"`, `"sm83.ppu"`, `"rp2A03.cpu"`.

Machine assembly is the reverse of a trait-object graph. Each device-family
crate exports one enum that holds all of its device kinds:

```rust
enum VideoDevice {
    Ppu(Ppu),
    Vdp(Vdp),
    // ...
}
```

The enum implements the `Device`, `BusAccess`, and `MappedDevice` traits.
The assembled machine stores one enum value per family crate. This gives
static dispatch in hot paths and no heap allocation for device graphs.

The configuration selects kinds by name. Unknown kinds are a validation
error. The configuration cannot define new device behavior; it selects,
parameterizes, wires, and boots compiled behavior.

## 9. System configuration DSL

The DSL is TOML. The full grammar, field reference, validation rules, and
`extends` semantics are specified in `docs/dsl-specification.md` (Phase 1
and Phase 13 deliverables). This section is the summary.

Sections:

- `[system]` — name, triple, description.
- `[extends]` — optional base configuration (a bundled triple name or a
  file path). Overrides replace named devices, windows, connections, boot
  states, and quirk flags.
- `[clock.<name>]` — a clock domain: `frequency = "num/den"`, optional
  `origin` note.
- `[[device]]` — an instance: `name`, `kind`, `params`, `domain`,
  `dividers`.
- `[[bus]]`, `[[window]]` — buses, address windows, mirrors, overlays,
  bus-conflict policy, window owners.
- `[[connection]]` — endpoints (`device.line`), direction, semantics.
- `[[signal]]` — line groups with open-collector semantics.
- `[boot.<device>]` — boot states: ROM file path, RAM pattern
  (`random(seed)`, `zero`, hex dump), CPU register presets.
- `[quirks]` — named quirk flags with typed values.

Embedded expression strings use the grammar of section 6.2.

Validation has four layers: schema, device graph, semantic, and lint.
`demu config --check <file>` runs all layers standalone and exits nonzero
on errors, zero on success or warnings only.

Bundled canonical configurations ship in `configs/<triple>.toml`, embedded
with `include_str!`. A `--config` argument accepts a triple name or a file
path.

## 10. Boot states

Each device declares its boot state:

- ROM devices load a file. The validation checks the file size against the
  device capacity.
- RAM devices take a pattern: `zero`, `random(seed)`, or a fixed hex dump.
  The random pattern uses a deterministic seeded generator, so the same
  configuration boots identically everywhere.
- CPU devices take register presets. The reset vector handling is
  per-machine and documented in the machine chapter.

## 11. Save states

`save-state` writes a versioned binary file:

```
magic "DEMU" | format_version u32 | machine triple hash u64
| device_count u32 | (device_name | chunk_len | chunk_bytes)*
| scheduler_state | clock_domain_states | checksum
```

The format version increments on incompatible changes. `load-state` MUST
reject a file whose format version or machine triple hash does not match
the running machine. Restore MUST reproduce the exact running state,
including the scheduler queues and all domain tick counters.

## 12. Input scripting and Matroska

### 12.1 Input scripts

An input script is a text file. One event per line:

```
#<domain>:<ticks> <device> <event>
```

The canonical timestamp is the domain name and a tick count. The
nanosecond form `#<ns> <device> <event>` is accepted and converted at load
with the boundary rule of section 2.5. Lines starting with `#` after the
timestamp position, blank lines, and `label <name>` lines are comments and
labels. Labels let scripts repeat sections cyclically.

The parser validates device names against the loaded configuration.

### 12.2 Matroska pipeline

`demu-mkv` implements its own EBML reader and writer and a Matroska muxer
and demuxer. The pipeline:

- Input scripts convert to Matroska input tracks. Each track carries a
  private codec payload (the event encoding).
- A Matroska file carries the system configuration TOML as a Tag plus an
  attachment. demu reads the configuration from the file and reconstructs
  the machine.
- `mkv-render` runs the simulation with the input track and records the
  output: FFV1 lossless video at one frame per emulated display frame with
  frame durations derived from the machine frame clock, a native-rate PCM
  track, and a resampled 48 kHz PCM track as default track 0.
- The 48 kHz resampler (`demu-resample`) uses a rational phase accumulator
  keyed on the source domain tick. Given the same input samples and source
  domain, the resampler output is bit-exact across runs.

## 13. CLI contract

```
demu [OPTIONS] [ROM]
  --config <triple|file>   system configuration
  --symbols <file.opl>     symbol table
  --dump <path>            memory dump at exit
  --sentinel <addr>:<val>  compare byte, set exit code
  --checks <file>          demu-native check file
  --json                   JSON-lines protocol on stdio
  --serve <path>           Unix-socket server mode
```

Exit codes:

- `0` — success, or sentinel pass.
- `1` — run failure, sentinel miss, or a failed check.
- `2` — usage error, configuration error, or I/O error.

The checks file (`--checks`) declares assertions that demu evaluates at a
stop point: memory-region equality, trigger-hit assertions, and screenshot
hashes. Its schema is TOML:

```toml
[[check]]
at = { domain = "cpu", ticks = 70224 }   # or trigger = "..."
kind = "memory"
region = "wrram"
expect_hex = "..."

[[check]]
at = { trigger = "vblank" }
kind = "trigger-hit"
trigger = "dmg"

[[check]]
at = { frame = 60 }
kind = "screenshot"
hash = "sha256:..."
```

## 14. cart integration

demu exposes the CLI contract of section 13. cart uses it:

- `cart run` with `[[run.profile]] emulator = "demu"` appends the ROM path.
  The demu entry in `emulator_model_args` supplies `["--config", <triple>]`.
- `cart test` forwards `--dump`, `--sentinel`, `--symbols`, and optionally
  `--checks` when the profile names demu. The demu exit code decides pass
  and fail.
- `cart debug` launches demu with `--serve <path>` and `--symbols`, prints
  the socket path, and stays alive while the client works.

demu does not join the default emulator matrix in cart.

## 15. Quirks

Each device module recreates known hardware quirks and bugs. Each quirk has
a named configuration flag with a typed value, for example
`quirks.oam_bug = true`. The manual documents all flags per machine.

The quirk registry is per device kind. The configuration sets flags by
name. Unknown quirk names are a validation error.

Doc obligation: each machine chapter lists its quirk flags, their default
values, and the hardware evidence that motivates each flag.

## 16. Acceptance-test matrix

| System group | Suites | Golden evidence |
|---|---|---|
| SM83 (GB, CGB) | blargg, mooneye | `dmg-demo`, `cgb-demo` screenshots |
| 6502 (NES) | nestest, blargg CPU/PPU | `nes-demo` |
| 6502 (Atari consoles) | per-console test ROMs | startup screens |
| 6502 (Atari 800, C64) | CPU and VIC timing | startup screens |
| 6502 (Apple II family) | startup screens | serial line checks |
| 65C816 (SNES, IIgs) | test ROMs | boot sequence, async determinism run |
| vl65nc02 (Lynx) | boot ROM check | boot screen |
| HuC6280 (PC Engine) | CPU and VDC timing | boot screen |
| Z80 (Sega 8-bit) | ZEXALL, VDP tests | boot screens |
| Z80 (Sinclair, TI-85) | ZEXALL, known screens | startup screens |
| m68000 (Genesis, Neo Geo) | CPU tests, bus arbiter | boot screens |

Each acceptance phase records pass counts and screenshots in
`docs/manual/evidence/`.

## 17. Performance and build notes

The scheduler hot path MUST NOT allocate: event queues are pre-allocated,
the address decoder uses array indexing, and hashing uses `rustc-hash`.

The release profile guidance is `opt-level = 3` and `lto = "thin"`. The
repository does not pin profiles; profile tuning happens with profiling
evidence.

Observability uses `tracing` with structured fields. demu MUST NOT write
logs to stdout in JSON mode.

## 18. Repository verification

The standard verification set for every phase:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

Documentation phases add `cargo doc --no-deps`.
## 19. As-built notes: NES

The NES machines of the 6502 family implement this design. The notes
record the deviations and the details the design leaves open.

- `extends` resolution is wired into the bundled-config loader. A
  `--config mos6502-nintendo-nes-pal` names a bundled alias document
  whose base resolves through the bundled list.
- The NTSC master domain carries `236250000/11` Hz with the CPU on the
  divider 12 and the PPU dot on 4. The PAL master carries
  `53203425/2` Hz (26.6017125 MHz) with the dividers 16 and 5.
- The DMA stall uses a `halt` line: the OAM DMA unit and the DMC
  reader drop the CPU's ready line for the transfer window and the
  run loop reschedules the core at the release edge.
- The PPU drives the `nmi` edge over the declared connection and
  emits a runtime `Vblank` event every frame, so `run-frame` and
  `run-until vblank` work on all machines.
- Register write paths that must reach another device without the
  machine context queue the access for the next dot visit. The queue
  is one entry for the pattern-bus write of $2007 and drains before
  the next dot.
- The PPU composes one scanline per visit from the register state at
  the scanline start. Mid-scanline raster effects are not part of the
  composition.
- Sprite-zero hit timing and the sprite-overflow bug follow the
  simplified per-scanline evaluation; the cycle-level evaluation of
  the secondary OAM is a follow-up.
- The font renderer of `std` erases its glyph planes through PPU
  writes to CHR space. A cartridge whose header carries character
  banks keeps those writes read-only, so the demo renders text with
  the blank character set. FCEUX behaves the same for this image.
- The MMC5 support carries the program pages, the character pages,
  and the scanline IRQ. The extended RAM bank and the split mode are
  out of the set.
- The PAL region uses the same structure with the PAL sequencer
  steps, the PAL DMC and noise tables, and the divider of the region's
  crystal.
