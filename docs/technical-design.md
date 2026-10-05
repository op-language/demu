# demu Technical Design

This document is the technical specification of `demu`, a cycle-accurate
debugging emulator for the 32 target triples that `opc` and `std` support.
The words MUST, MUST NOT, SHALL, SHALL NOT, and MAY are RFC 2119 words.

The prose in this document follows ASD-STE100 (strict): short sentences, one
idea per sentence, active voice, and a controlled vocabulary.

Each new machine family reads this document and records as-built
notes when the design changes.

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
`extends` semantics are specified in `docs/dsl-specification.md`. This
section is the summary.

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

Each acceptance run records pass counts and screenshots in
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

The standard verification set for every change:

```
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

Documentation work adds `cargo doc --no-deps`.
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
## 20. As-built notes: Atari 2600, 5200, 7800

The three machine families share one color clock: 315000000/88 Hz. The
2600 domain is the color clock itself; the 5200 and the 7800 domains
carry the double-rate master of 315000000/44 Hz, with the color clock
divided by two and the CPU by four.

- The CPU of all three is the plain 6502 kind with a divider override:
  three ticks on the 2600 (the 6507), four on the 5200 and the 7800
  (SALLY). The 6507 connects no interrupt lines. The `undocumented`
  quirk flag is on: NES-era games and Atari games use the opcodes.
- TIA walks one color clock per visit in the 2600 form. Position
  counters follow the TIA hardware notes: the register strobes re-base
  at the beam with the decode delay, copies follow the NUSIZ layout,
  and the HMOVE strobe shifts each object by `8 - (HMX ^ 0x08)`.
  The TIA sound engine follows the published divider model with the
  poly taps and the 13:18 Div-31 duty, ticked at the 31.4 kHz audio
  clock. The 7800 keeps the same device with the video parameter off.
- The collision bits follow the published register map, but the pairs
  that the matrix reports are simplified: a missile reports its hit
  against either player of its register, and the priority table picks
  the first matching layer. Cycle-precision collision positions and
  the response-register quirks are a follow-up verification pass.
- RIOT 6532 carries 128 bytes of RAM, the interval timer with the
  zero-hold and fast-wrap phases, and the port registers. The read of
  INTIM or INTFLG that clears the flag reverts the wrap mode; the
  exact hardware edge alignment of that revert is approximate.
- The 5200 display pipeline is one device: the ANTIC display-list
  processor and the GTIA register file compose together, because the
  pixel output is internal to the die pair. The GTIA file answers
  $C000 and the ANTIC file answers $D400. The display list walks at
  the line start; the DMA bytes are stolen machine cycles of one halt
  pulse, not the intra-line interleave of the real circuit.
- The 5200 map: 16 KiB RAM at $0000, the flat 32 KiB cart window at
  $4000 end-aligned so $BFFE carries the start vector, GTIA at $C000,
  ANTIC at $D400, POKEY at $E800, and the 2 KiB BIOS at $F800.
- The 7800 map: TIA at $0000, MARIA at $0020, the RAM shadow of the
  zero page and the stack page, the RIOT at $0280, main RAM at
  $1800-$27FF, the cart at $4000-$BFFF and $D000-$FFFF, and the BIOS
  at $C000. MARIA walks one pixel clock per master tick, 456 cycles
  per line. The display list list entries, the direct mode entries,
  and the 160A/B encodings run per the GCC software guide; the
  320-pixel encodings, the character modes, holey DMA, and the
  Kangaroo and border control bits decode but are not exhaustively
  verified. Cart banking: flat images map down from $FFFF; banked
  images page 16-KiB chunks into $8000-$BFFF from the written value;
  the audio-cartridge variants are out of the set.
- POKEY carries the channel dividers with the joined sixteen-bit
  pairs, the seventeen-bit and nine-bit global polys, the four-bit
  and five-bit per-channel polys, the keyboard-code register with the
  key interrupt, and the IRQ mask with the write-to-acknowledge rule.
  The serial port and the pot scan timing are stubs.
- Per-console test-ROM patterns and BIOS checksum runs remain pending
  a follow-up evidence pass with real ROM assets; the acceptance
  programs of these machines are synthetic images with recorded outputs.

## 21. As-built notes: Atari 800, Commodore 64, Apple II

The home-computer 6502 machines reuse the Atari display and audio
devices of the earlier families and add the computer-specific chips.

- The Atari 800 shares the ANTIC and GTIA pair device and the POKEY.
  The PIA 6520 joins as `atari.pia`: the register map puts the port
  pair at offsets 0 and 1 and the control pair at 2 and 3, the control
  bit 2 selects the data register or the direction register, data
  reads clear the interrupt flag, and the flag arms the line that the
  config wires in OR with the POKEY interrupt. The two sources drive
  one shared CPU input, so each notifies only on a level change.
  Joystick ports live in PORTA active low, with
  `p0-up`-through-`p1-right` and `-release` input grammar.
- The 800 NTSC domain carries 315000000/44 Hz with the CPU on divider
  four and the color clock on two. The PAL domain carries the color
  clock 7093790/2 Hz with the CPU on two, one color clock per ANTIC
  visit, and the ANTIC `pal` parameter switches the frame length to
  312 lines. The 48 KiB RAM
  answers $0000-$BFFF, the I/O page $D000-$D7FF mirrors the four
  chip banks, and the 8 KiB OS ROM answers $E000-$FFFF. The XL bank
  switches, the serial disk of the SIO, and the cartridge slot stay
  out of the set.
- The Commodore 64 NTSC domain carries the pixel clock
  315000000/44 Hz, so one dot is one tick; the CPU divider is seven
  (1.022727 MHz), the VIC raster line is 455 ticks (65 CPU cycles of
  the 6567R8 form), and the frame is 263 lines. The PAL form aliases
  the NTSC base, carries the CPU rate 17734475/18 Hz with one tick
  per CPU cycle, 63 ticks per line, and 312 lines per frame, and sets
  the `pal` parameters of the VIC, the SID, and the CIAs. The VIC composes one line per visit through a fetch walk on
  the primary bus, holds the 1024 color-RAM nibbles behind
  $D800-$DBFF, and raises the raster interrupt through `vic.irq`.
  Badline CPU stuns and the per-cycle sprite fetches stay out of the
  set, in line with the per-line composition model.
- The SID carries three voices at the CPU rate with the 24-bit phase
  accumulators, the triangle, saw, pulse, and 23-bit LFSR noise
  forms, the ring and sync paths, and the attack-decay-sustain-release
  shape of the rate table. The resonant filter block stays out of the
  set; the two chip models share the register behavior of this build.
- The two CIA 6526 devices carry the paired down-count timers with
  the force-load rule, the time-of-day clock with the alarm compare,
  the wired shift and flag registers as stubs, and one interrupt
  each: CIA1 presses the CPU line, CIA2 presses the NMI line. The TOD
  divider counts CPU-cycle visits: 17045 ticks per pulse of the
  sixty-pulse NTSC form and the device's own 19705 default for the
  fifty-pulse PAL form. The keyboard matrix reads through CIA1 port A
  driving rows and port B reading
  columns, with injected keys as row-column codes of one byte.
- The C64 static memory map keeps 64 KiB RAM in segments around the
  hardware page, the color RAM, the two CIA banks, and the top 4 KiB.
  The `$01` PLA banking stays store-only: the build carries no BASIC,
  KERNAL, or character ROM assets, so the banking cannot switch
  anything and the config documents the flat picture.
- The four Apple II machines share one memory plan: the console
  block of `a2.io` answers the `$C0xx` page and the language-card
  block $D000-$EFFF with its internal 12 KiB and 4 KiB banks that
  stay invisible until a card switch; the card's upper half behind
  $E000-$EFFF answers at the device level, and the monitor ROM
  window keeps that page from the card at the machine level in this
  build. The video device of `a2.video` owns
  the display pages $0400-$0BFF and $2000-$5FFF plus the form
  switches $C050-$C057 over an overlay window, and the monitor ROM
  answers $F000-$FFFF. The three second members alias the base
  machine through `extends` and restate only metadata.
- The Apple video walks 455 dots per line and 262 lines per frame at
  one dot per visit, with the CPU at seven dots. The text, low-res,
  and high-res forms compose per line from the owned page bytes, the
  mixed modes keep the text rows at the bottom, and the colors come
  from the integer palette of the TIA family.
- The Disk II joins as `a2.disk2` with the slot-6 window
  $C0E0-$C0EF over the console page. The four-phase stepper moves the
  arm a half track per adjacent pulse, the motor and drive select
  switches latch, the data latch answers $C0EC, and the boot image
  loads from the device's own boot state. The media reads one raw
  byte stream per track with no GCR sector framing, and writes latch
  only. The boot-disk setting of the plan is this device boot state,
  set through `[boot.disk2]`.
- The startup-screen acceptance runs one synthetic bring-up image per
  family: the 800 stub parks in the ANTIC WSYNC loop, the C64 counts
  raster interrupts through the full IRQ path with a snapshot round
  trip, and the Apple stub writes the text page and steps the Disk II
  track. Real OS, KERNAL, and BASIC assets and checksum runs remain
  pending the follow-up evidence pass, with the serial line checks of
  POKEY and the CIA; the paddles, buttons, and speaker toggles count
  as observable stubs.

## 22. As-built notes: SNES and Apple IIgs

The 65C816 machine families are the first two-domain systems, and
they exercised the cross-domain machinery for the first time in an
assembled machine.

- The scheduler gained its secondary-domain bound: `step_once`
  compares every domain's pending event against the target's exact
  real time through the rational cross-multiplication, so a
  secondary domain free-runs only up to the target instant and the
  run loop terminates. Single-domain machines keep the same
  behavior, because the compare reduces to the raw tick target for
  the primary domain. The SnesPad taught the queue lesson: a
  device whose boot schedules a visit but whose tick does not
  advance the schedule loops the scheduler forever, so a device
  with no periodic work declares no next visit.
- The 65C816 core runs all 256 opcodes in both emulation and
  native forms, with the M/X register widths, the direct page, the
  data and program banks, the block move, WAI and STP, and the
  interrupt vector tables of both modes. One instruction runs per
  visit; every bus access carries its cycle stamp. The tests
  include one deterministic opcode sweep over the whole table.
- The SNES sound die (`apu.core`) holds the full SPC700 core
  (256 opcodes, the timers with their precounter, the DIV/MUL
  unit, the bit operations, and the DSP bridge) and the S-DSP
  engine (eight voices, BRR decode, envelope, echo and noise) in
  one device on the sound domain. The audio ports work as latched
  registers: the CPU window answers $2140-$217F and the SPC sees
  $F4-$F7, each side sampling at its own instruction boundaries.
  The config declares the crossing as the `async-port` connection
  between the two devices; the bytes travel through the port
  window's ownership and the die's latches, not through the
  connection's line path, which carries no edges in this build.
  The published port race (a read that overlaps a write returns
  the OR of the old and new bytes) ships behind the
  `apu.port_race_or` quirk with a deterministic one-visit window;
  the window closes at the next visit boundary rather than the
  exact sub-cycle strobe. The IPL boot handshake is not modeled;
  the boot state preloads the sound RAM and the reset vector.
  SLEEP and STOP halt only the core, while the timers and the DSP
  keep the sample clock exact, and the halt state rides the
  snapshot.
- The S-PPU renders one line per visit over the composed modes 0
  through 7 (with EXTBG), the sprite evaluation with the time-over
  latch, the color math, the mosaic, and the counter/latch
  registers of both dies in one device. The per-dot timing,
  interlace, window masks, and direct color stand as recorded
  approximations. The 5A22 support die (`snes.sys`) carries the
  DMA and HDMA engines with their halt pulses to the CPU on the
  declared line, the multiplication and division units, the H and
  V timers with a fixed-dot H approximation, and the auto-joypad
  read.
- The SNES memory windows follow the published map. The bus mirror
  form repeats on the stride boundary (repeat k at `base + k*stride`
  carrying the first `size` bytes), which the SNES bank layout and
  the sparse WRAM mirror need; the decoder fix also repaired the
  TIA's page mirrors. A window that mirrors a bank form declares the
  count of repeats after the primary range: the LoROM ROM and SRAM
  regions are four windows, each with one mirror stride of one bank.
- The Apple IIgs joins as `iigs.mega2` (the console glue: the
  keyboard, the IIe switch set, the IIgs new-video, shadow, clock,
  CYA, and interrupt registers, and the state register), `iigs.iwm`
  (the disk port at the IWM state form: the phase pairs, the drive
  enable and select, and the Q7/Q6 data, status, handshake, and
  mode register path), and `iigs.doc` (the Ensoniq DOC with the
  sound GLU face at $C03C-$C03F). The Mega II answers the console
  page; the DOC and IWM windows overlay it. The language-card and
  auxiliary banking forms, the shadow copy engine, the ADB mouse
  and keyboard registers, and the one-second and quarter-second
  interrupt timers are not modeled; their readbacks return the
  stored or open-bus forms. The Doc's oscillator scan uses the
  published frequency-multiply form of one phase step per visit
  and its register map follows the ES5503 group layout with the
  two bytes at $E0/$E1.
- The IIgs runs one domain at 28636360 Hz with the CPU on ten, the
  video on four, and the Doc on fourteen per the sound GLU's
  divider form. The boot view keeps the monitor ROM mapped at bank
  $00 $F000-$FFFF; the extended ROM banks and the language-card
  views stay out of the build. The IIgs video reuses the Apple
  device with the classic 7.159090 MHz dot clock.
- The acceptance evidence runs the synthetic forms: the SNES cart
  test walks the reset vector, the PPU setup, the audio-port
  write and readback, and one mode-one DMA burst that fills VRAM
  through the $2118/$2119 pair, and the IIgs monitor lands its
  banner in the text page and parks through the switch walk. The
  async determinism test runs two machines of the same inputs to
  one target and compares full save states byte for byte across
  both domains, and the save and restore tests repeat the form
  mid-run. The real test-ROM suites, the SameBoy-class
  cross-checks, and the BIOS checksum runs remain pending the
  follow-up evidence pass with real ROM assets, per the precedent the
  SM83 and NES families set.

## 23. As-built notes: Lynx and PC Engine

The 65SC02-derivative and HuC6280 machines complete the 6502-family
group. Each machine runs one clock domain, so the cross-domain
machinery of the SNES group sees no new form here; the machines
instead exercise the die-internal coupling: the map unit, the on-die
timer, and the register-protocol surfaces.

- One core crate serves two kinds. `mos65sc02.cpu` runs the published
  CMOS 65C02 instruction form, and `vl65nc02.cpu` is the same core in
  its Lynx form: the die reads the MAPCTL register at
  `$FFF9`/`$FFF8` through its own bus at every instruction boundary
  and drives the `suzy_ram`, `mikey_ram`, `rom_ram`, and `vec_ram`
  overlay lines from the set bits. A high-speed bit (bit 7) lengthens
  every cycle by one domain tick. The plain kind keeps the page-bit
  instructions SMB, RMB, BBS, and BBR, which the Lynx core replaces
  with two-cycle NOP forms; the quirk flag `bbr_smb` records the
  chosen flavor per kind. WAI and STP park the core until the IRQ
  edge or the reset line, and a parked core declares no next event
  until the line rises; the save and restore tests repeat the park
  form mid-run.
- The Lynx Mikey die (`lynx.mikey`) carries the timer block of four
  bytes per timer with the published expiry form (a timer that starts
  at N expires after N plus one source periods), the reload and hold
  rules, the cascade chains 0-2-4 and 1-3, the interrupt set and
  reset registers, the joypad byte with the published button bits,
  the switch byte with the slot lines fixed high, the cart strobe and
  data lines of SYSCTL1 and IODAT, and the AUDIN page select line.
  The LCD line engine walks 317-dot line periods (a recorded 0.15
  percent approximation of the published line rate) over 105 line
  periods per frame, with the display rows at periods 2 through 103.
  The serial link, the magtape bits, the parallel-port pin
  semantics, and the direction gating of IODIR store their bytes and
  stay out of the model; their records name each form.
- The Lynx Suzy die (`lynx.suzy`) owns the sprite engine with the
  packed and literal line forms of the control block chain, the quadrant walk
  with flip and direction bits, the scaling, stretch, and tilt
  accumulators, the background and collision buffer compose, the
  collision depository write-back through the machine bus, the math
  unit (signed multiply and unsigned divide with recorded, not
  simulated, latency), the four pulse-width audio channels with
  their polynomial shift registers, integrate mode, link clocks, and
  the mute byte, and the twelve-bit palette. The walk runs
  synchronously inside one display-line visit, the build and
  collision buffers stay internal mirrors, the divide latency is
  recorded, and the everon bit keeps the Handy polarity; each
  approximation carries its record in the module documentation.
- The Lynx cartridge (`lynx.cart`) answers the stream form: a ripple
  address counter that increments on every read, the block shift
  register fed by the IODAT data bit and the SYSCTL1 strobe, the
  RCART0 and RCART1 ports that read the banks at `$FCB2`/`$FCB3`,
  the `raw` and `LNX` header forms, the bank-1 shadow RAM of the
  single-bank cards, and the AUDIN 1 MiB carrier form. The BLL
  header form stays out and is recorded.
- The Lynx memory devices (`lynx.bootrom`, `lynx.mapctl`) carry the
  512-byte boot ROM over the vector RAM and the map control
  register. The bundled config declares no boot ROM bytes: the real
  512-byte ROM loads through the boot state of the positional flow
  (`[boot.bootrom]` with a `hex` or a `path` form), and the bundled
  machine boots with an empty ROM surface until a machine build
  supplies one. The boot flow test supplies a synthetic boot ROM
  whose copy loop maps the ROM space to the RAM underneath and parks
  the core in the copied page, the published boot shape.
- The HuC6280 core (`huc6280.cpu`) carries the MPR mapping unit with
  the published reset state, the zero-page, stack, and T-cell rule
  that routes through MPR 1, the on-die timer at the I/O offsets
  `$0C00`/`$0C01`/`$0C07` with the recorded `1024/(select+1)`
  interval form, and the interrupt controller in the published
  Hudson form: the mask register at `$1402` (a write sets the disable
  bits and a read returns the mask), the status read at `$1403`, and
  the timer acknowledge at the write to `$1403`; the pre-2007 MAME
  forms stay as recorded alias writes at `$1400`/`$1401`. The service
  priority follows the published form: NMI, then the timer, then
  IRQ1, then IRQ2. The die intercepts the accesses of the hardware
  bank `$FF`; every other offset routes through the bus. The reset
  vector always comes from bank 0, which is why the machine resets
  out of the card. The one-cycle VDC and VCE window penalty (the
  MAME form) stays out and the record names it.
- The PC Engine video pair (`pce.vdc`, `pce.vce`) answers the twenty
  internal registers of the VDC with the status flags, the SATB
  transfer at the vertical blank, the raster compose of one line per
  visit (510 dots at the dot divider, 242 line periods, a 256 by 239
  frame), and the VCE color table of 512 nine-bit entries. The RCR
  scanline convention follows the published form: the die's raster
  counter reads 64 at the last blank line before the display
  window, so the register carries the target display line plus 64,
  and the compare fires inside the display window only. The VDC busy
  status flag reads 0, because the VRAM ports are immediate in this
  build. The VDC reads the color table bytes
  through the bus at the `palette_base` parameter, the declared
  machine coupling; the direct window face at the published
  `$1FF800` address answers those bytes read-only with their byte
  index, and the CPU's real path to the palette keeps the register
  protocol. The VDC's `bus` parameter names the walk bus and
  defaults to the first configured bus.
- The PC Engine PSG (`pce.psg`) runs six channels in one device: the
  32-entry five-bit waveform tables, the DDA direct form with the
  published circular buffer, the noise form of channels 5 and 6 on an
  eighteen-bit shift register, the main amplitude pair, and the LFO
  register pair held as a recorded form. The `sample_rate` surface
  reports the published internal rate and `take_samples` drains the
  mono buffer; the same registers produce the same samples.
- The PC Engine pad (`pce.joypad`) answers the I/O page form: the
  pad select write whose bit 0 selects the multiplexer row and whose
  CLR bit stays stored state, the published row readback with the
  CD bits and the country line (set for the Japan form and clear for
  the USA form, per the MagicKit hardware text and the Mednafen
  readback), the idle form of the six-button rows (a recorded
  approximation), and the turbo autofire on a visit cadence from the
  rate and enable bytes.
- The validation of the triple kind rule gained one exception: the
  `mos6502-nec-pcengine` triple names the 6502 family, but the
  machine carries the real chip kind `huc6280.cpu`; validation
  accepts that one pairing and every other triple keeps the strict
  family rule.
- Both bundled configs assemble and boot to a parked core: the Lynx
  test walks the boot ROM copy and map flow on the full published
  wiring, and the PC Engine test drives the reset vector, the VDC
  and VCE register surfaces, the direct color-table face, and the
  pad byte. The real test-ROM suites and the boot-ROM check screen
  with the actual 512-byte boot ROM remain pending the follow-up
  evidence pass with real ROM assets, per the precedent every
  machine family before this one set.

## 24. As-built notes: Z80 machines

The Sega and Sinclair systems and the TI-85 of the Z80 family
implement this design. The notes record the deviations and the
details the design leaves open, in the order of the machine parts.

### The core (`z80.cpu`)

- `z80.cpu` runs the full documented instruction set with the
  published per-access timing: one opcode fetch costs 4 T-states, one
  memory access 3, one port access 4 with its automatic wait state,
  and the index forms pay their internal address add as the published
  totals require. The `cpu` divider entry carries the domain ticks
  per T-state (3 on the Sega crystal domain, 1 where the domain is
  the CPU clock).
- Interrupt machinery: modes 0, 1, and 2; an NMI service costs 11
  T-states with the stack decrement inside the acknowledge cycle;
  mode 1 accepts in 13 and mode 2 in 19 with the vector low byte read
  from the port plane at 0x10000. An EI defers maskable acceptance by
  one boundary. Mode 0 runs the accepted data byte with the program
  counter at the interrupted address, so a supplied RST pushes and
  jumps once.
- The undocumented forms sit behind the `undocumented` quirk flag:
  the index-register halves (IXH, IXL, IYH, IYL), the SLL shift, the
  `OUT (C),0` store, the `IN (C)` flag-only form, and the index
  register IN/OUT forms. With the flag clear the forms run
  effect-free: the `IN (C)` and `OUT (C),0` pair falls to the
  unknown-form 8 T-state fallback with no bus access, and the index
  halves read zero, while the SLL shift and the index IN/OUT forms
  keep their access totals.
  The flag bits 3 and 5 copy the result, operand, or address bits
  where the NMOS core copies them; those copies are not gated by the
  quirk.
- A repeated block instruction refetches its opcode pair once per
  iteration, so the refresh counter gains two per iteration and the
  iteration totals match the published 21 T-states with 16 for the
  last one. Block operations run to their end without sampling
  interrupts, since the core samples only at instruction boundaries.
- The bit tests of a memory byte take their flag copy bits from the
  effective-address high byte, the registered behavior of the NMOS
  core.

### Sinclair memory contention

- The delay follows the published ULA pattern 6, 5, 4, 3, 2, 1, 0, 0
  indexed by the frame T-state modulo 8. The access path carries four
  range slots per side; a config with more entries contends only the
  first four (a recorded cap for the bundled machines, which declare
  two or fewer). The 48K window runs over
  frame T-states 14336 to 57344 with the phase base 14344; the 128K
  window runs 14361 to 58140 with the base 14361. The parameters
  `contended`, `io_contend`, `contend_from`, `contend_to`, and
  `contend_base` live on the CPU device entry.
- I/O contention keys on the high port byte $40-$7F range and on port
  $FE itself. The frame anchor is the tick of the `int` line rising
  edge, so contention stays phase-locked to the frame start.
- The banked-page contention of the 128K rides the `contend_page`
  line from `zx.bankram` (high when the paged bank is 1, 3, 5, or 7);
  the CPU keeps the $4000-$7FFF static range.

### Port plane

- A Z80 machine declares two buses: the memory bus and the port bus.
  The CPU presents every port access at the plane address
  `$10000 | port`, so port windows live in `$10000-$1FFFF` and never
  collide with the memory spans of a device that answers both spaces.
  The `IN A,(n)` and `OUT (n),A` forms place the accumulator on the
  port high byte, per the published Z80.
- The Sega port decode is low-byte only, so the Sega port windows
  carry a mirror stride of 0x100 over the full plane. The TI-85
  decodes ports by their exact 16-bit value; the ZX ULA answers every
  even port through one plane-wide window.

### The Sega systems

- One VDP struct carries the three forms by kind: `tms9918.vdp` (the
  TMS9918A modes over a 16 KiB VRAM), `sms.vdp` (the SMS1 class), and
  `gg.vdp` (the SMS2-class Game Gear). The TMS modes render Graphics
  I, Graphics II, Text 40, and Multicolor; invalid mode-bit
  combinations fall through to Graphics I. The mode 4 render runs on
  the SMS and Game Gear forms (the R0 bit 2 with the R1 bit 1) with
  the 32x28 name table, the per-line horizontal scroll, the scroll
  locks, the 64 sprites with the 8-per-line limit, the D0 row
  terminator, collision, and the 224/240-line height modes behind the
  `extended_height` parameter (SMS only).
- The mode-4 data-port read publishes the V counter with the
  published 218/213 fold; the H counter read is not implemented. The
  Vblank trigger emits once per frame boundary; the frame and line
  interrupt flags ride the R1 bit 5 and R0 bit 4 enables with the R10
  counter, and the IRQ line re-drives per visit on a level change.
- The SMS CRAM holds one byte per entry (red in bits 1-0, blue in
  bits 3-2, green in bits 5-4) with the upper half mirrored; the Game
  Gear CRAM holds two bytes per entry (blue and green in the first,
  red in the second) and publishes the 160x144 window of raster rows
  24 to 167 and columns 48 to 207. The published Game Gear centered
  border is not composed; the window is the whole surface (a recorded
  approximation).
- The mode-4 pattern generator honors the R4 base; the SMS1
  name-table mask bit applies the simplified full-base form.
- SN76489: three tone channels with 10-bit periods (a period of zero
  or one holds a constant level, the published sample-playback form),
  a 4-bit attenuation each, and a noise channel with the 16-bit
  linear feedback shift register, white taps {0, 3}, the periodic
  single tap, control writes resetting the register to $8000, and the
  shift-rate table 16, 32, 64, or the tone C period. The mixed output
  uses a linear 16-step attenuation; the chip's per-step 2.5 dB
  ladder is not modeled.
- The I/O controller (`sega.io`) answers the $DC-$DD pad ports with
  the active-low directions and fire lines under the TH swap rule. On
  the sms form a `pause` press latches one staged NMI pulse (_high
  one visit, low the next); on the game gear form the START key reads
  through port $00 bit 7 and the `sega.gg_start` quirk registers the
  staged behavior.
- Cartridges (`sega.cart`) load the flat SG-1000 form, the flat SMS
  form, the Sega 16 KiB-page mapper ($3FFE/$3FFF/$402C/$402D), and
  the Codemasters mapper (the $0000 write banks the $8000 window at
  value + 1), all wrapping by page count.

### The Sinclair systems

- The ULA (`zx.ula`) carries the three machine forms. The $FE port
  write stores border bits 1-3, the beeper drive (bit 4 on the
  Spectrum, bit 0 on the ZX80/ZX81), and the microphone bit 0; the
  read returns the keyboard half-row selected by the port high byte
  with pressed keys as cleared bits and the EAR bit 6 low. The frame
  rhythm: the Spectrum raises `int` for one scanline at line 0 and
  publishes the 320x240 frame (256x192 with the 32/24 border); the
  ZX80 raises one `nmi` per frame; the ZX81 raises `int` on the
  display lines and `nmi` on the blank lines under the
  `zx.zx81_slow` quirk.
- The ZX80/ZX81 display renders each display line from the display
  file and the character ROM through `peek`, one row per scheduled
  line, rather than from the recorded bus traffic of the running
  core; this is a recorded approximation. The ZX81 slow mode repaints
  each DFILE row at four display-line visits, idempotent against the
  16 display lines per row that its 96-line window provides. The
  ZX81 block graphics mapping is recorded in the ULA module docs.
- The plain memory kinds `z80.rom` and `z80.ram` wrap StaticRam; each
  window base must be a multiple of its device size, since a device
  maps addresses by modulo. The 48K config splits the RAM into three
  16 KiB devices at $4000, $8000, and $C000. `z80.bankrom` holds the
  two-page ROM switched by the `select` line; `zx.bankram` holds the
  128 KiB of RAM with the fixed banks 5 and 2, the paged door over
  $C000-$FFFF, and the $7FFD paging port (write-only, reads $FF,
  lockable).
- The Spectrum 128K variant (`z80-sinclair-spectrum128`) extends the
  48K config: the PAL crystal 3546895, the 228-T-state 311-line
  frame, the banked map, and the new line set; the bundled selector
  name resolves it as a machine variant of the `z80-sinclair-spectrum`
  triple.

### The TI-85

- `ti85.mem` holds the 128 KiB ROM in eight 16 KiB pages and the
  32 KiB RAM in two banks. The page map: $0000-$3FFF follows the ROM
  page latch on port $08, $4000-$7FFF holds ROM page 0, $8000-$BFFF
  follows the RAM bank latch on port $06 (reset 1), and $C000-$FFFF
  follows the port $07 latch (reset 0). The device answers the latch
  values at the memory addresses $0006-$0008 as well, since a device
  sees no bus identity in an access, so a real ROM image whose first
  bytes sit there is unservable; a bundled machine avoids the span
  through its park image. This alias is a recorded limitation for the
  real-ROM evidence pass.
- `ti85.lcd` holds the 128x64 one-bit display buffer at $FC00-$FFFF
  (bit 7 the leftmost pixel of each byte) with the driver page inside
  at $FEC0-$FEFF (the record of the published driver layout pending
  the real-ROM evidence pass). The driver decodes three addresses:
  $FEC0 latches a command (01 fills the buffer with the next data
  byte, 02 accepted as no operation, 08 and 0C turn the display off
  and on), $FEC2 latches the keypad column in bits 2-0, and the
  keypad read reports pressed keys as cleared bits with bit 7 idle
  high. The frame publishes from the buffer on every touched write.
- `ti85.link` answers port $05: the write carries the drive levels in
  bits 0-1 and the output enables in bits 2-3; the read reports the
  enabled lines as the local drive and the floating lines as the
  remote levels, with the remote levels injected with the event form
  `link <white> <red>`.
- The machine boots the DI; HALT park at 0x0000 and carries no
  interrupt sources in this build.

### Pending evidence
- The interrupt acknowledge of modes 0 and 2 reads the vector byte
  at the port plane address $10000; on machines whose port decode
  answers that address (the Game Gear START port, the Spectrum ULA
  keyboard row) the acknowledge byte comes from that device. Bundled
  machines use mode 1, so the collision is observational until the
  interrupt-model evidence pass.


Per the precedent every machine family before this one set, the
following remain pending a follow-up evidence pass with real ROM
assets: the ZEXALL and ZEXDOC instruction suites, the per-VDP and
per-system test-ROM screens, the Sinclair real-BIOS contention
check, the ZX80/ZX81 real-BIOS boot screens, and the TI-85 real-ROM
boot.


## 25. As-built notes: 68000 dual-CPU machines

### The borrowed-decoder route

The core's `BusAccess` carries a `borrow(address)` decision beside
`decodes(address)`. A window device that composes a cross-bus access
itself answers `Borrow::Bridge` with the borrowed bus name and the
translated address; the visit context re-decodes the access on that
bus and the answering device there serves it. The route is the
as-built form of the DMA-parented bus of section 5.2: no
configuration surface exists for it, because the composing device
behaves like a bus owner, not like a second bus. A loop of bridges
stops at the depth cap of four and the access answers nothing. The
machine-level debug reader and writer follow the same route, so
`peek` and `debug_write` cross a bridge the same way the run does.

### Mega Drive clocking

The two domains: `m68k` at 53693175 Hz (the NTSC master crystal) and
`z80` at `53693175/15` Hz (the published sound division). The 68000
declares the `cpu` divider 7; the VDP counts master ticks (`dot` 1).
A scanline takes exactly 3420 master clocks; the published line and
frame forms live with the VDP device notes. The Z80 core declares
`cpu` divider 1 on its own domain, since the domain tick is the Z80
clock. A PAL machine swaps the master crystal to 53203424 Hz and sets
the VDP's `region` parameter.

### The bus arbiter and the bank mapper

- `genesis.arbiter` answers the control trio on the 68000 bus at
  \$A11000 (the memory mode, bit 0: 0 the boot ROM form, 1 the RAM
  form), \$A11100 (the bus request: the write with bit 0 clear
  registers the request, the grant lands on the arbiter's next tick
  visit, and the read's grant bit is active low; the write with bit 0
  set releases), and \$A11200 (a write with bit 0 clear pulses the
  sound core's reset and re-runs its power-on form). The arbiter
  drives `stall` high while the request stands and low on release,
  and pulses `zreset`; both route to the sound core over declared
  connections with `async-port` semantics, the DSL's cross-domain
  form.
- `genesis.mapper` owns the 8 KiB of sound RAM and the 2 KiB boot ROM
  image (the image loads through the mapper's own boot state; the
  published board form places the image there). The sound bus's
  \$0000-\$1FFF window answers the RAM with the boot ROM image over
  the first 2 KiB while the memory mode reads 0, and the RAM itself
  in mode 1. The \$6000-\$7FFF window carries the nine-bit
  shift-register bank. The \$8000-\$FFFF window borrows the 68000
  bus's decoder in 32-KiB pages through the bank register. The
  68000-side window at \$A00000-\$A03FFF answers the sound RAM view
  (with the \$2000 mirror) directly, so the mapper owns both views.
- The `genesis.io` die answers \$A10000-\$A100FF: the version cell
  and the three port cells with the TH-direction split; the shell
  injects events with the `port <n> press|lift <bits>` form.

### Genesis window map

The 68000 bus: the cart at \$000000-\$3FFFFF, the VDP at
\$C00000-\$C0000F, the PSG at \$C00010-\$C0001F, the sound RAM
view at \$A00000-\$A03FFF, the YM2612 at \$A04000-\$A05FFF, the IO
die at \$A10000-\$A100FF, the arbiter trio at \$A11000-\$A113FF,
and the work RAM at \$E00000-\$FFFFFF (the 64-KiB block mirrored
on the \$10000 stride). The sound bus: the mapper's RAM/ROM form at
\$0000-\$1FFF, the YM2612 ports at
\$4000-\$5FFF, the bank page at \$6000-\$7FFF, the PSG's port
window over \$7F10-\$7F1F, and the banked window at
\$8000-\$FFFF.

### Interrupt wiring

The 68000 decodes its interrupt level from three open-collector IPL
lines that follow the machine's high-assert convention: the level's
binary digits are the asserted lines. The VDP drives the level-6 line
to IPL bits one and two, the level-4 interval line to bit two, and
the level-2 feed and the YM2612's timer interrupt to bit one; the Z80
receives its own vblank feed on its `int` line. The sources that
share a bit pull the same open-collector line exactly like the
wired-OR interrupt pins of the real machine.

### Boot forms

The 68000 view boots through the boot-state register preset naming
the supervisor stack pointer and the program counter (the reset
exception's vector fetch stays outside this build; the cart's vector
longwords at \$000000/\$000004 are informational in bundled
machines). The Z80 view boots into the mapper's boot ROM form and
runs its stub; the sound view's configuration names the sound domain
first, so the scheduler's primary domain is the sound crystal and the
run commands pace on the Z80.

### Pending evidence

Per the precedent every machine family before this one set, the
following remain pending a follow-up evidence pass with real ROM
assets: the 68000 instruction-suite runs, the YM2612 and YM2610
register-image captures, the VDP golden screenshots from real titles,
the BIOS checksum runs, and the bus-arbiter timing cross-check
against a hardware capture.


### The 68000 core

The core runs one instruction per visit with per-access stamps; the
published Appendix B totals ride the access flow. The effective
address walks the extension words with the published signed-index and
PC-relative forms; the predecrement store folds through the
per-size-lane rules. The exception frame pushes the 32-bit program
counter first, then the 16-bit status word (the Musashi-pinned
three-word short form), so the frame reads $SP+0 = status, $SP+2 =
the return address; RTE and RTR pull through the same form. Illegal,
privilege, and the unimplemented-line entries stack the faulting
instruction's own address; trace, TRAP, TRAPV, CHK, the divide
entries, and the interrupt stack the advanced address. The address
error uses vector 3. The interrupt entry pays 44 clocks with the
internal acknowledge and installs the serviced level's mask. The
divide and multiply walk the published bounds (140/158 and 70+).
The reset exception's vector fetch stays outside this build: the
machine's boot preset names the supervisor stack pointer and the
program counter. The three IPL lines are open-collector; the decode
of the level reads the asserted lines as its binary digits, so the
machines wire or share sources per bit exactly like the real
interrupt pins. A cross-domain edge carries the sender's tick stamp;
the core's wake lands at the next CPU boundary after it, which
overshoots on a slower receiving domain by construction of the stamp
(recorded for the arbiter handoff as a first-build latency form, not
a determinism form: the handoff stays exact and byte-stable).

### The Genesis VDP

The VDP counts 3420 master clocks per scanline in both horizontal
modes (H32: 342 dots at /10; H40: 390 dots at /8 plus 30 at /10), the
published line form. The HV counter readback carries the pinned
Nemesis/Kabuto spans with the two-dot H40 form ($00-$B5, $B6 and $E4
one dot each, $E5-$FF), the V-increment boundaries at slots 266/330,
and the PAL line-progress jump tables. The control port runs the
two-word command form with the pending flag; byte writes enter both
lanes; the CRAM and VSRAM reads fold, the VRAM byte read answers the
odd-lane swap form; register writes cancel the pending form and clear
the code register. The read-back of registers rides the 11-prefix
command and aborts on the second word. The status word carries the
published bits 0-9. The DMA forms: the 68k transfer at 16/167
(H32) and 18/205 (H40) bytes per line over the word rates for the
CRAM and VSRAM, the fill at 15/166 and 17/204 with the high-byte
form, the copy at 8/83 and 9/102; the zero length reads $10000. The
fill and copy seize the 68000 through the hold line in place of the
hardware port restriction. The render walks the plane ladder with
the window union clipping, the column VSRAM pairs, the column-major
tile walk, the raw-X-zero mask, the overflow at the 20/320 and 16/256
line caps, and the shadow/highlight operators with the BlastEm
$3E/$3F form (the MacDonald v1.5f swap is recorded as the
divergence). The CRAM's 9-bit channels expand with the published
(v<<5)|(v<<2)|(v>>1) form; the shadow halves and the highlight adds
128 to the staged channel.

### The Neo Geo dies

The LSPC runs 384 pixels per line at /4 of the master (6 MHz), the
published line form, over 264 (NTSC) or 312 (PAL) lines; the palette
window and its mirrors span $400000-$7FFFFF. The register page
decodes on bits 1-3; the VRAM address carries its own zone rule (the
auto-increment keeps the zone bit and wraps inside the zone), the
signed modulo walks backwards, and a data write refills the read
buffer. The sprite walk follows the SCB1-SCB4 layout with the stick
chain, the 96-entry per-line list with the zero tail, the chained
strip's X advance, and the L0-table vertical shrink (the tables are
generated from the published subsample semantics: the real $000-lo.lo
data stays a ROM dump and is recorded as the LSPC evidence gap). The
C ROM decode implements the published 128-byte tile interleave; the
FIX layer walks column-major from $7000 with the palette in the word's
top four bits, first 16 palettes, and the published 32-byte tile
form. The palette chip answers the selected bank's word forms; byte
lanes stay unmapped (the published word-only rule); the last word is
the backdrop index. The vblank interrupt asserts at line 240 (NTSC)
and clears through $3C000C; the raster timer counts pixel clocks with
the published reload forms.

The 68k IO die follows the MAME AES maps: the controllers read
active-low with the Up/Down/Left/Right/A/B/C/D bit order at the even
lanes of $300000-$31FFFF and $340000-$35FFFF, the watchdog arms at
boot and re-arms per odd-lane kick with the published 3,244,030-tick
deadline, $320000 carries the sound latch (byte = the command form,
word = the reply crossing), $320001 reads the AES status-A zero form,
$380000 reads $7F idle, and the odd outputs $3A0001-$3A001F store
their latch bytes: $3A001F selects palette bank 1 (high), $3A000F
selects bank 0, and $3A0003/$3A0013 drive the BIOS-vector mux line.
The MVS-only backup RAM, RTC, and memory card stay out of the AES
map by design.

The sound die reads its command latch at io port $00 (the write
clears it and drops a pending NMI pulse), the YM2610 owns $04-$07,
the reply latch stores at $0C and crosses with a one-tick delay, and
the NMI enable/disable writes share the $08/$18 ports with the M1
bank reads through the per-direction overlay: MAME's audio map puts
the bank select on the read side, with the port address's high byte
carrying the bank, and the NMI form on the write side. The M1 ROM's
static bank reads $0000-$7FFF and its four banked windows read at
2/4/8/16-KiB granularity through the latched banks, all zero at
reset.

### The YM2610

The chip follows the landed OPNB form: four FM channels of four
operators at the published port split (the A1 bank bit selects the
SSG/ADPCM-B/FM-common/ch1-2 map on port A and the ADPCM-A/ch3-4 map
on port B), the die-verified operator addressing (OP1, OP3, OP2, OP4
at $x1/$x5/$x9/$xD for channels 1 and 2, and the mirrored pair for
channels 3 and 4), the key-on channel codes 001/010/101/110, the
high-register-first frequency latch, and the special-mode pair map
with OP4 keeping the channel's own pair. The status 0 byte carries
the busy and both timer flags; the status 1 byte carries the ADPCM-B
end bit and the six ADPCM-A channel-end flags with the mask form of
$1C. The SSG block follows the YM2149 equations at 32 chip clocks
per tick with the published mono mix form. The ADPCM-A engine runs
six channels at 18.5 kHz (one nibble per third FM sample) with the
wrapping 12-bit accumulator, and the ADPCM-B engine runs the variable
rate with the 16-bit delta fraction. A data write raises busy for 83
chip clocks. The FM sample lands every 144 chip clocks; on the 24 MHz
master the machine's divider is 432.

### Known first-build approximations

- The debug-write and debug-read paths stamp accesses with the
  primary domain's tick; a device on a secondary domain converts the
  arm from the received stamp. A machine-level test that writes a
  secondary-domain device after a long run arms past the run's end:
  issue debug writes before the run. A future pass converts the debug
  stamps cross-domain at the decode point.
- The reset exception's vector fetch stays out of the 68000 build;
  the bundled boots preset the core directly.
- The arbiter's cross-domain wake overshoots (see the 68000 core
  notes); the handoff form stays deterministic and byte-stable.
- The LSPC's vertical-shrink tables are generated; the timer's lap
  fires at line-granular visits carrying the exact published
  one-pixel decrement.
- The VDP's DMA budget uses the published per-line byte rates; the
  FIFO's full/empty flags ride the queued forms.
- The YM2610's mono downmix averages the two pan sums (a single-pan
  channel renders at half level), recorded as the house audio form.

### Recorded divergences of the first build

- The 68k IO die decodes its three port cells by the address's low
  bits, so even control addresses inside a port cell answer the data
  form; the pad data registers read correctly at the addresses real
  titles use.
- The V-ROM chip has no sprite-decode window: the LSPC's C-ROM
  interleave reads the C ROM directly, and the V ROM feeds only the
  YM2610's ADPCM engines. The published Neo Geo cart separates C and
  V banks per game; a per-cart banking pass is pending evidence.
- The data-source exemption of the fabric check is a broad heuristic:
  any device's string parameter naming another device clears the
  orphan rule; a later pass can restrict it to the kinds' known
  data-source keys.
- The M1 bank ports map four bytes at the io spaces 08-0B without the
  MAME mirror form, so drivers that bank through the port address's
  high byte stay pending the real-ROM evidence pass.

## 26. As-built notes: the media pipeline

### The input script

The parser accepts the canonical `#<domain>:<ticks> <device> <event>`
form and the nanosecond form `#<ns>`; a nanosecond stamp converts
against the machine's primary domain (the first clock name in the
resolved name-sorted clock map, the domain the scheduler treats as
primary) with the section 2.5 boundary rule. A `#` token is an event stamp only when it carries the stamp
shape, a `domain:ticks` pair or a digit run; any other `#` token begins
a comment, as does a `#` after the timestamp position. `label <name>`
marks a cyclic section; `repeat <name> <count>` repeats the section
`count` times total, shifting each copy by the section's own span
(last event tick minus first tick), so the first event of copy `k`
lands exactly on the last event tick of copy `k-1` and the copies fire
in script order at the shared tick. A section must use one stamp form;
mixed forms, zero spans, unknown labels, duplicates, and empty
sections are validation errors.

### The replay scheduler

Pending events stay in script order. The scheduler fires an event when
the event instant compares due against the machine's primary tick by
the exact cross-multiplication. For a pending event on any domain, the
run loop stops the primary at tick `p = ceil(t * d_e * n_p / (n_e *
d_p))`, computed exactly in u128, so an event on a secondary domain
fires no earlier than its instant. The `run-until` idle-window
behavior changed to support exact sync points: when a finite run
target sits past every domain's queued events, the primary clock
advances to the target (a free run still ends without advancing). A
parked machine therefore reports the run boundary's tick, and the
parked state itself is visible through the device's queue (a parked
core declares no next event).

### The Matroska forms

`demu-mkv` implements its own EBML primitives and two file forms. The
input form carries one metadata track (TrackType `0x21`, CodecID
`D_DEMU/INPUT/1`) whose private codec data is the 8-byte magic
`DEMUINP1`, one block per event in file order, one cluster per event
at the event's nanosecond time; the block payload is
`u16le|u64le|u16le|u16le` length-prefixed domain, ticks, device, and
event text, so the replay reads the exact tick values and never a
nanosecond round trip. The system configuration TOML rides as the
`demu.config` tag text and as a `config.toml` attachment; `demu.triple`
and `demu.machine` tags carry the machine summary. The render form
carries one cluster per emulated display frame at TimestampScale `1`
(nanosecond timebase): the FFV1 keyframe block on track 2, the
native-rate PCM block on track 3, and the resampled 48 kHz block on
track 1, the first and default-selected audio track. The FFV1 track's
CodecID is `V_FFV1` and its codec private is demu's configuration
record; the PCM tracks use `A_PCM/INT/LIT` with a WAVEFORMATEX private.

### The resampler

`demu-resample` holds a rational phase accumulator: output sample `i`
takes the source sample at index `floor(i * source_rate / 48000)`,
compared incrementally in u128, so the output over any split of the
push calls is bit-identical and any two runs produce the same stream.

### The shell commands

`script <file>` loads and validates a script against the loaded
machine and installs it as the run schedule; `run`, `step`,
`run-frame`, `run-until`, and the free run then apply the scheduled
events at their exact instants. `mkv-input <script> -o <file>` writes
the input form. `mkv-render <input.mkv> <frames> -o <output.mkv>`
rebuilds the machine from the file's embedded configuration through
the binary's factory, replays the events over the requested display
frames, and records the render form. `render-session <frames> -o
<file>` records the current session onward. The recorder drains every
audio device after each frame in device declaration order,
concatenates the chunks for the native track, and feeds the same
chunks through the 48 kHz accumulator; a machine without an audio
device records video only, and a machine without a video device is an
error.

### Known first-build approximations

- The FFV1 implementation is demu's own range-coder build with one
  slice per frame, per-slice CRC, and no run mode; common-player
  playback spot checks remain pending the follow-up evidence pass.
- The render segment opens with the EBML unknown-size marker and
  stays open until end of file, because the frame clusters stream as
  they record; the demuxer falls back to "the rest of the buffer" for
  the all-ones size.
- Cluster timestamps come from the primary domain's tick at the
  vblank; a secondary-domain display would stamp with its own domain.
- The audio drain happens once per completed display frame; a machine
  whose audio and frame clocks differ in ratio accumulates that exact
  drift in the native-track sample counts, which the timestamps
  reflect.
- The SM83 PPU's vblank edge fired on every dot of line 144 in the
  first build; it now fires once on the line's first dot, so the
  `frames` counter and the Vblank event carry one entry per emulated
  display frame, which the one-frame-per-video-block contract
  requires.
