# The machine-definition tutorial

This tutorial teaches you to construct a demu machine definition from
scratch. Read this document alone and you can write a new machine
definition. The reference material is `docs/dsl-specification.md`. The
complete field tables and validation rules live there.

Every lesson works from a real bundled config in `configs/`. Read the
config, run the checker, and boot the machine. The lessons use the
machines that show the unique features of the DSL:

- Lesson 1 builds the smallest machine from an empty file.
- Lesson 2 reads the Game Boy: one domain, twelve devices, the overlay.
- Lesson 3 reads the NES: DMA, mappers, and the extends alias.
- Lesson 4 reads the SNES: the asynchronous dual-domain crossing.
- Lesson 5 reads the ZX Spectrum: memory contention and the ULA.
- Lesson 6 reads the Genesis: the dual-CPU bus arbitration.
- Lesson 7 reads the Lynx: the custom CPU derivative and the boot ROM.

The commands assume you run from the demu repository root.

## Lesson 1: The smallest machine

A machine definition declares what demu constructs. The smallest
useful machine has one clock domain, one CPU shim, one RAM chip, and
one bus with one window:

```toml
[system]
name = "tiny"
triple = "tiny"
description = "A minimal synthetic machine."

[clock.main]
frequency = "1000000"

[[device]]
name = "cpu0"
kind = "smoke.cpu"
domain = "main"
[device.dividers]
cpu = 1

[[device]]
name = "ram0"
kind = "smoke.ram"
domain = "main"
[device.params]
size = 8192

[[bus]]
name = "main_bus"
address_bits = 16
owner = "cpu0"

[[window]]
name = "w_ram"
bus = "main_bus"
device = "ram0"
base = "0x0000"
size = "0x2000"

[boot.ram0]
pattern = "zero"

[boot.cpu0]
registers = { ip = "0x0000" }
```

The document shows every top-level section once. Walk it section by
section:

1. `[system]` names the machine and its triple. The triple is the
   machine identity for the CLI selector.
2. `[clock.main]` declares the crystal. The name `main` is the
   domain key that device entries reference.
3. The two `[[device]]` entries pick compiled kinds by name and
   attach them to the domain. The `dividers` table names the
   integer dividers the device publishes: the CPU shim runs one
   tick per instruction step.
4. `[[bus]]` declares the decoded address space and its owner, the
   device the run commands address by default.
5. `[[window]]` maps one range of the bus to one device. A device
   needs at least one window on a bus, or a params reference in
   another device, or validation rejects the machine.
6. The `[boot.*]` sections declare the boot states: the RAM pattern
   is the `zero` fill, and the CPU preset puts the instruction
   pointer at zero.

Run the checker:

```
$ demu config --check tiny.toml
ok: tiny (tiny) devices=2 clocks=1
```

The check passes, because validation only checks the kind registry
and the graph. The machine assembles in the framework tests. Note
the limitation: the demo binary registers no `smoke.*`
constructors, so a live session with this file stops at assembly
with `no device constructor for kind 'smoke.cpu' in machine
'tiny'`. The lesson still gives you the complete shape: clock,
device, bus, window, boot. A real machine replaces the smoke kinds
with real kinds and follows the same shape.

One rule of the CPU-family check: when the triple carries a dash,
the part before the first dash must name the family of every `.cpu`
kind in the machine, ASCII case-insensitive. A dash-less triple
like `tiny` skips the rule.

## Lesson 2: The Game Boy, a minimal single-domain machine

Read `configs/sm83-nintendo-gameboy.toml`.

The machine has one crystal and no banking complexity at the
machine level. The anatomy comes in five parts.

**Part 1, the domain.** The DMG runs everything on one 4194304 Hz
crystal:

```toml
[clock.main]
frequency = "4194304"
origin = "DMG crystal; the CPU T-cycle and the PPU dot clock ride one domain"
```

The `origin` line is provenance text only. It exists so a reader
knows why the frequency is what it is.

**Part 2, the devices.** Twelve devices, one crystal: the CPU, the
boot ROM, the cart, the two RAM chips, the PPU, the APU, the DMA
unit, the timer, the pad, the serial port, and the IO mux. Each
device declares which divider entries it publishes:

```toml
[[device]]
name = "cpu0"
kind = "sm83.cpu"
domain = "main"
[device.dividers]
cpu = 1
```

The `cpu = 1` divider says the CPU T-cycle is one crystal tick. The
PPU publishes no explicit divider entry on this machine because its
dot is also one crystal tick.

**Part 3, the bus and the windows.** One bus, `cpu`, 16 bits, owned
by the CPU. Every device that answers addresses owns a window on
that bus. The map follows the published Game Boy layout, one window
per range or device.

**Part 4, the boot overlay.** The boot ROM covers $0000-$00FF over
the cart window at reset:

```toml
[[window]]
name = "w_bootrom"
bus = "cpu"
device = "bootrom"
base = "0x0000"
size = "0x0100"
overlay = "boot"
overlay_read_priority = 1
overlay_write_priority = 1

[[window]]
name = "w_cart_rom0"
bus = "cpu"
device = "cart"
base = "0x0000"
size = "0x4000"
overlay = "boot"
```

Both windows declare the group `boot`. The decoder resolves the
group's windows by priority: the priority 1 boot ROM wins over the
priority 0 cart. The $FF50 write that unmapped the boot ROM moves
the group to the cart form inside the device code. The lesson: an
overlay group plus priorities is the DSL form for "one surface with
two views".

**Part 5, the boot states and quirks.** The boot ROM embeds as a
`hex` dump, the CPU preset puts PC at $0000, and three quirks turn on:

```toml
[boot.cpu0]
registers = { pc = "0x0000" }

[quirks]
"sm83.oam_bug" = true
"sm83.wave_ram_undoc" = true
"sm83.halt_undoc" = true
```

Boot and drive it:

```
$ demu --config sm83-nintendo-gameboy demo.gb
demu> run-frame 2
ran to tick 135888
```

The REPL form of the same commands:

```
$ demu --config sm83-nintendo-gameboy demo.gb
demu> info
demu> run-frame 1
demu> screenshot frame.png
```

## Lesson 3: The NES, DMA, cart mappers, and the extends alias

Read `configs/rp2A03-nintendo-nes-ntsc.toml`.

**The crystal.** The NTSC master is not an integer Hz: the exact
frequency is 236250000/11:

```toml
[clock.main]
frequency = "236250000/11"
```

The frequency is a string with a rational form. demu keeps it exact:
the CPU divider 12 and the PPU dot divider 4 ride the same crystal,
and the three-dot relationship is exact. This is why the DSL carries
rational frequencies: an integer-Hz or float model would lose the
relationship.

**The DMA.** The `dma` device is one device entry with the CPU
divider:

```toml
[[device]]
name = "dma"
kind = "rp2A03.dma"
domain = "main"
[device.dividers]
cpu = 12
```

The DMA engine owns the ready line. The connection section wires
the DMA to the CPU. When the DMA or the DMC reader fetches, the
CPU's ready line drops and the run loop reschedules the core at the
release edge. The lesson: a DMA "stall" is a connection plus a
line, not a config knob.

**The cartridge window map and mirrors.** The PPU's register block
needs a mirror:

```toml
[[window]]
name = "w_ppu_regs"
bus = "cpu"
device = "ppu"
base = "0x2000"
size = "0x0008"
mirror_stride = "0x0008"
mirror_count = 1023
```

The mirror repeats the register block 1023 times, one stride apart,
so one window covers the full $2000-$3FFF register aliasing. The
$4000-$401F windows answer the IO block. The
mirror form repeats the window's first `size` bytes at `base +
k*stride`. The lesson: the mirror form is how the DSL compresses
the aliasing maps of real hardware.

**The alias through extends.** The plain 6502 view of the NES is
one short file:

```toml
[extends]
base = "rp2A03-nintendo-nes-ntsc"

[[device]]
name = "cpu0"
kind = "mos6502.cpu"
domain = "main"
[device.dividers]
cpu = 12

[quirks]
"mos6502.undocumented" = true
```

Only the CPU entry overrides. Every other device, window, and
connection comes from the base. Validation re-runs on the merged
result. The lesson: alias configs keep one mechanical truth per
family.

**Run the four machines:**

```
$ demu config --check configs/rp2A03-nintendo-nes-ntsc.toml
$ demu config --check configs/mos6502-nintendo-nes-pal.toml
$ demu --config rp2A03-nintendo-nes-ntsc demo.nes
```

## Lesson 4: The SNES, asynchronous dual-domain crossing

Read `configs/wdc65c816-nintendo-snes.toml`.

**Two crystals.** The SNES has two unrelated crystals. The DSL
models each with one domain, and the two never share a timeline:

```toml
[clock.main]
frequency = "236250000/11"
origin = "NTSC master crystal; the 65C816 sits at /6 and the S-PPU dot at /4"

[clock.sound]
frequency = "1024000"
origin = "the S-SMP/S-DSP crystal, asynchronous to the main domain"
```

**The devices per domain.** The CPU, PPU, support die, WRAM, pad,
and cart ride `main`. One device, the APU die, rides `sound`:

```toml
[[device]]
name = "apu"
kind = "apu.core"
domain = "sound"
[device.dividers]
cpu = 1
```

**The crossing.** The audio ports cross through one declared
connection with `async-port` semantics, the only semantics value
that may join two domains:

```toml
[[connection]]
from = "cpu0.ports"
to = "apu.ports"
semantics = "async-port"
```

The connection documents the crossing duty. The port bytes travel
through the port window's ownership and the die's latches: the CPU
window answers $2140-$217F on the main bus, and the SPC sees
$F4-$F7, each side sampling at its own instruction boundaries. The
lesson: declare the crossing in the config, and wire the actual
byte path through windows.

**The bank mirrors.** The SNES memory map repeats windows on the
bank stride:

```toml
[[window]]
name = "w_wram_mirror_low"
bus = "cpu"
device = "wram"
base = "0x000000"
size = "0x002000"
mirror_stride = "0x010000"
mirror_count = 63
```

Sixty-three repeats one bank apart: the WRAM mirror sits at every
bank of the first half of the space. The cart windows repeat the
same form. The lesson: LoROM maps are mirror chains over one bank
stride.

**The determinism proof.** Two machines of the same inputs must
reach byte-identical full save states, across both domains. This is
the acceptance check of every two-domain machine. Give the cart a
boot state, boot two machines, run the same frames with the same
commands, and save both states:

```
$ demu --config wdc65c816-nintendo-snes cart.sfc
demu> info
demu> run-frame 30
demu> save-state snes-a.state
```

A second machine with the same commands saves a second file. The
two files compare byte-identical.

## Lesson 5: The ZX Spectrum, memory contention and the ULA

Read `configs/z80-sinclair-spectrum.toml`.

**One domain, one contention.** The Spectrum runs one 3.5 MHz
domain, but every access in a window range waits on the ULA, with
the published pattern `6, 5, 4, 3, 2, 1, 0, 0` over the frame
T-state modulo 8. The CPU entry declares the contention as params:

```toml
[[device]]
name = "cpu0"
kind = "z80.cpu"
domain = "main"
[device.dividers]
cpu = 1
[device.params]
bus = "cpu"
io_bus = "io"
contended = "0x4000-0x8000,0xE000-0x10000"
io_contend = "0x40-0x80"
contend_from = 14336
contend_to = 57344
contend_base = 14344
```

- `contended` lists the memory ranges: the screen bank $4000-$8000
  and the bank $E000-$10000 of this config.
- `io_contend` lists the port range: the high port byte $40-$7F of
  this config.
- `contend_from` and `contend_to` bound the window in frame
  T-states, and `contend_base` phases the delay table.

The frame anchor is the tick of the `int` line rising edge, so the
contention stays phase-locked to the frame start. The lesson:
contention declares on the CPU that pays the delay, keyed to the
other device's line edge.

**The port plane.** A Z80 machine declares two buses. The CPU
presents every port access at the plane address `$10000 | port`,
so port windows live in $10000-$1FFFF and never collide with the
memory windows of a device that answers both spaces:

```toml
[[bus]]
name = "io"
address_bits = 17
```

The ULA's port window is one plane-wide span. The even-port gate is
the device-side decode hook: the ULA device decides which plane
addresses it answers. The Sega decode is low-byte only, and its
port windows carry the `0x100` mirror stride over the full plane.
The lesson: a device can gate its own decoding, and a low-byte-only
decoder becomes a mirror layout on the windows.

**The keyboard matrix.** The ULA read returns the keyboard
half-row that the port high byte selects, with pressed keys as
cleared bits. Input injection names the half-row keys.

**Run both Spectrum configs:**

```
$ demu config --check configs/z80-sinclair-spectrum.toml
$ demu config --check configs/z80-sinclair-spectrum128.toml
```

The 128K config extends the 48K: it replaces the crystal with the
PAL form, extends the frame, and adds the banked controller and the
new line set. Note that the 128K child keeps the parent's triple
`z80-sinclair-spectrum` and declares no separate triple: the
selector name resolves it as a machine variant.

## Lesson 6: The Genesis, dual-CPU bus arbitration

Read `configs/m68000-sega-genesis.toml`.

**Two domains.** The machine declares the master crystal domain and
the sound domain:

```toml
[clock.m68k]
frequency = "53693175"

[clock.z80]
frequency = "53693175/15"
```

The 68000 declares the `cpu` divider 7 and the VDP counts master
ticks (`dot = 1`). The Z80 declares `cpu` divider 1. The sound
domain's crystal is the published /15 division.

**The two-bus surface.** The machine declares a 68000-side bus and
a sound-side bus, each with its own windows. The arbiter
owns the control trio on the 68000 bus at $A11000-$A11200. The
mapper owns the 8 KiB sound RAM with the boot ROM overlay on the
sound bus. The banked window at $8000-$FFFF borrows the 68000
bus decoder:

```toml
[[device]]
name = "arbiter"
kind = "genesis.arbiter"
domain = "m68k"

[[device]]
name = "mapper"
kind = "genesis.mapper"
domain = "m68k"
[device.params]
main_bus = "cpu"
```

The `main_bus` param names the bus the banked window borrows. As
built, the borrowed-decoder route is a device decision: the device
answers the access with a `borrow` result that names the other bus,
and the visit context re-decodes there. No config surface exists
for the borrow route beyond the param. The lesson: a param that
names another device or bus is the DSL form for a device-owned
cross-bus surface.

**The interrupt wiring.** The 68000 decodes its interrupt level
from three open-collector IPL lines. The sources that share a bit
pull the same line, exactly like the wired-OR pins of the machine.
Each source connects its own line endpoint to the shared CPU line
device over declared connections. Read the connection table of the
config: the VDP, the arbiter feed, and the YM each route their
edges.

**The sound view.** A second config, `configs/z80-sega-genesis.toml`,
defines the same machine viewed from the Z80. Its clocks put the
Z80's crystal under the name that sorts first, so the scheduler's
primary domain is the sound crystal, and run commands pace on the
Z80. The 68000 device is
still declared. The view is for sound debugging. The lesson: two
configs name two views of one machine, and the primary domain rule,
the first clock name in name-sorted order, picks the pacing.

## Lesson 7: The Lynx, the custom CPU derivative and the boot ROM

Read `configs/vl65nc02-atari-lynx.toml`.

**The plain kernel of a die.** The Lynx runs one 16 MHz master
domain. The CPU is `vl65nc02.cpu`, the same core crate as the plain
CMOS 65SC02 kind, in its Lynx form: the die reads the MAPCTL
register at $FFF9 at every instruction boundary and drives the
overlay lines:

```toml
[[device]]
name = "cpu0"
kind = "vl65nc02.cpu"
domain = "main"
[device.dividers]
cpu = 4
```

**The overlay-gated memory.** The memory windows answer the RAM
four times: the RAM covers $0000-$FBFF, and the four overlay-gated
spaces at $FC00, $FD00, $FE00, and $FFFA carry the RAM underneath:

```toml
[[window]]
name = "w_ram_suzy"
bus = "cpu"
device = "ram0"
base = "0xFC00"
size = "0x0100"

[[window]]
name = "w_suzy"
bus = "cpu"
device = "suzy"
base = "0xFC00"
size = "0x0100"
overlay = "suzy_regs"
overlay_read_priority = 1
overlay_write_priority = 1
```

The map-control device drives the `suzy_ram`, `mikey_ram`,
`rom_ram`, and `vec_ram` lines from the MAPCTL set bits. A die that
the map gates off stops answering its own window in the decode. The
overlay priority then answers from the RAM underneath. The lesson:
a device line controls an overlay group, and the drop-out of the
die moves the space to the RAM underneath.

**The stream cartridge.** The Lynx cart is a stream device, not a
memory window: the boot ROM and the program read it through the
RCART0/RCART1 ports at $FCB2/$FCB3, and those two single-byte
windows answer from the cart device with a higher overlay
priority:

```toml
[[device]]
name = "cart"
kind = "lynx.cart"
domain = "main"
[device.params]
form = "raw"
```

The `form` param selects the `raw` byte-stream form or the `LNX`
header form. The lesson: not every device is memory-mapped. A cart
device communicates through its ports and its lines.

**The boot ROM and the real machine.** The bundled config declares
no boot ROM bytes: a machine build for the Lynx supplies the
512-byte boot ROM with a hex boot state on the bootrom device:

```toml
[boot.bootrom]
hex = "<the 512-byte boot ROM digits>"
```

The `rom` file form does not reach the empty boot-rom device in
this build (see the as-built note in the DSL specification). The
bundled boot-flow test supplies a synthetic boot ROM whose copy
loop maps the ROM space to the RAM underneath and parks the core,
the published boot shape. The lesson: a bundled config may declare
the surface and leave the asset to the machine build.

## The validation and iterate loop

When you write a new config, work in this loop:

1. Write the config file.
2. Run `demu config --check <file>`. Exit 0 means the four
   validation layers pass.
3. Read the message: every error names the section and the field.
4. Boot it: `demu --config <file>` starts the REPL. `info` prints
   the machine summary. A kind with a missing assembly param fails
   here with the kind named. Fix the param.
5. Add a boot state so the machine parks in a known state, then
   `step` and `read` the boot state surface.
6. Iterate.

The common failures:

| Failure message (the real text) | Cause |
|---|---|
| `config not found: <path>` | the selector matched neither a bundled triple nor a real file |
| `kind <k> requires a domain` | the device entry misses `domain` |
| `device '<name>' is not on any bus` | the device owns no window, no bus, and no params reference |
| `unknown clock domain reference '<name>'` | the device domain, a connection stamp, or a script references an unknown clock |
| `unknown quirk reference '<name>'` | the quirk key names a family with no device in the config, or an unregistered quirk name |
| the machine assembles, then the first `run` never ends | the boot state parks the CPU nowhere: add a park instruction to the boot hex or give the machine a bounded boot |

## The complete checklist

A new machine definition is complete when:

1. `demu config --check` passes with no errors and no warnings.
2. `info` names the machine, and the devices and clocks all appear.
3. The machine boots to a parked or looping known state.
4. Every device answers somewhere: a window, a bus, or a params
   reference.
5. The boot states declare the ROMs, RAM patterns, and CPU presets
   the real machine starts with.
6. The connections declare every interrupt, DMA, display, and
   cross-domain route, with `async-port` for every two-domain
   crossing.
7. The quirks record the hardware behaviors the machine shows, with
   the machine chapter documenting each flag.
8. The machine chapter of the documentation explains the
   subsystems and the recorded approximations.