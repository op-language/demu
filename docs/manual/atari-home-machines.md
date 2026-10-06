# The 6502 home machines: Atari 800, Commodore 64, Apple II family

This chapter explains the home-computer 6502 machines of demu: the
Atari 800 (NTSC and PAL), the Commodore 64 (NTSC and PAL), and the
Apple II family. The chapter gives the config anatomy, the subsystem
explanations, the quirks, the input grammar, and the recorded
approximations.

The bundled configs:

| Config | Triple | Extends |
|---|---|---|
| `mos6502-atari-800-ntsc` | `mos6502-atari-800-ntsc` | — |
| `mos6502-atari-800-pal` | `mos6502-atari-800-pal` | — |
| `mos6502-commodore-64` | `mos6502-commodore-64` | — |
| `mos6502-commodore-64-pal` | `mos6502-commodore-64-pal` | extends `mos6502-commodore-64` |
| `mos6502-apple-ii` | `mos6502-apple-ii` | — |
| `mos6502-apple-iic` | `mos6502-apple-iic` | extends `mos6502-apple-ii` |
| `mos6502-apple-iie` | `mos6502-apple-iie` | extends `mos6502-apple-ii` |
| `mos6502-apple-iie-enhanced` | `mos6502-apple-iie-enhanced` | extends `mos6502-apple-ii` |

The three Apple alias configs restate only metadata: one shared
memory plan and one console block serve all four members, and the
build records the generation differences in the technical design.

## 1. Clock domains

| Machine | Domain frequency | CPU divider | Notes |
|---|---|---|---|
| Atari 800 NTSC | `315000000/44` | 4 | the ANTIC color clock at divider 2 |
| Atari 800 PAL | `7093790/2` | 2 | one color clock per ANTIC visit; the `pal` param switches the frame to 312 lines |
| C64 NTSC | `315000000/44` | 7 | one dot per tick; the raster line is 455 ticks |
| C64 PAL | `17734475/18` | 1 | 63 ticks per line; 312 lines per frame |
| Apple II | `315000000/44` | 7 | 455 dots per line, 262 lines per frame, one dot per visit |

The Apple alias configs inherit the base machine's clock.

## 2. Devices

| Machine | Devices (kinds) |
|---|---|
| Atari 800 | `mos6502.cpu`, `atari.antic` (the ANTIC/GTIA pair device), `atari.pokey`, `atari.pia`, `atari.ram`, `atari.rom` |
| Commodore 64 | `mos6502.cpu`, `c64.vic`, `c64.sid`, `c64.cia` (two instances), `atari.ram`, `atari.rom` |
| Apple II family | `mos6502.cpu`, `a2.io`, `a2.video`, `a2.disk2`, `atari.ram`, `atari.rom` |

The flat memory kinds serve every family: `atari.ram` and
`atari.rom` are the flat StaticRam devices with a required `size`
param, and the `*.rom` kinds register read-only.

## 3. Memory maps

- The Atari 800: 48 KiB RAM at $0000-$BFFF, the I/O page
  $D000-$D7FF mirroring the four chip banks, and the 8 KiB OS ROM
  at $E000-$FFFF. The XL bank switches, the SIO serial disk, and
  the cartridge slot stay out of the set.
- The C64: 64 KiB RAM in segments around the hardware page, the
  color RAM, the two CIA banks, and the top 4 KiB. The `$01` PLA
  banking is store-only in this build: the build carries no BASIC,
  KERNAL, or character ROM assets, so the config documents the flat
  picture.
- The Apple II machines: the console block `a2.io` answers the
  $C0xx page and the language-card block $D000-$EFFF with its
  internal banks. The video device owns the display pages
  $0400-$0BFF and $2000-$5FFF plus the form switches
  $C050-$C057 over an overlay window. The monitor ROM answers
  $F000-$FFFF. The Disk II joins as `a2.disk2` with the slot-6
  window $C0E0-$C0EF.

## 4. Connections

- The Atari 800: the PIA control register line and the POKEY
  interrupt drive one shared CPU input. Each notifies only on a
  level change.
- The C64: the VIC raises the raster interrupt through `vic.irq`.
  CIA1 presses the CPU interrupt line. CIA2 presses the NMI line.
- The Apple II: the interrupt sources route over the declared lines
  of the machine chapter's config.

## 5. Boot states

- The Atari 800 loads the OS ROM through `[boot.os]`.
- The C64 machines carry RAM patterns. The config documents the
  flat picture without BASIC or KERNAL assets.
- The Apple II base machine loads the monitor ROM image and the
  Disk II boot image through the device boot states
  (`[boot.monitor]`, `[boot.disk2]` with a `rom` path or a `hex`
  dump). The Disk II drive reads one raw byte stream per track.

## 6. Quirks

| Quirk | Default | Behavior |
|---|---|---|
| `mos6502.undocumented` | true | the NMOS undocumented opcodes, including the CMOS machines' software that relies on them |

The C64 PAL config sets the `pal` params of the VIC, the SID, and
the CIAs through its own device entries.

## 7. The input grammar

| Device | Events |
|---|---|
| `atari.pia` (800) | `p0-up` through `p1-right`, with the `-release` suffix |
| `atari.pokey` | `key <n>` and `key <n>-release`, `pot<n> <value>` for n 0-7 |
| `c64.cia` | `key <code>` and `key <code>-release`; the code is row*8+column under 64 |
| `c64.sid` | `potx <value>`, `poty <value>` |
| `a2.io` | `key <byte or ASCII char>`, `paddle<n> <0-255>`, `button<n>` and `button<n>-release` (buttons 0-2; the IIgs `iigs.mega2` takes buttons 0-3) |

## 8. Subsystem explanations

### The ANTIC/GTIA pair and POKEY

The 800 shares the ANTIC and GTIA pair device and the POKEY of the
console machines. The 800 NTSC frame runs the same ANTIC
forms. The
PAL config sets the 312-line frame and the PAL color clock.

### The VIC-II

The VIC composes one line per visit through a fetch walk on the
primary bus, holds the 1024 color-RAM nibbles behind $D800-$DBFF,
and raises the raster interrupt. The raster line period is 455
ticks on NTSC (65 CPU cycles of the 6567R8 form) and 63 ticks per
line on PAL. Badline CPU stuns and per-cycle sprite fetches stay
out of the set, in line with the per-line composition model.

### The SID

Three voices at the CPU rate with the 24-bit phase accumulators,
the triangle, saw, pulse, and 23-bit LFSR noise forms, the ring and
sync paths, and the attack-decay-sustain-release shape of the rate
table. The resonant filter block stays out of the set. The two chip
models share the register behavior of this build.

### The CIAs

The two CIA 6526 devices carry the paired down-count timers with
the force-load rule, the time-of-day clock with the alarm compare,
and the wired shift and flag registers as stubs. The TOD divider
counts CPU-cycle visits: 17045 ticks per pulse of the sixty-pulse
NTSC form, and the device's own 19705 default for the fifty-pulse
PAL form. The keyboard matrix reads through CIA1 port A driving
rows and port B reading columns, with injected keys as row-column
codes of one byte.

### The Apple II video

The video device walks 455 dots per line and 262 lines per frame at
one dot per visit. The CPU sits at seven dots. The text, low-res,
and high-res forms compose per line from the owned page bytes, the
mixed modes keep the text rows at the bottom, and the colors come
from the integer palette of the TIA family.

### The Disk II

The four-phase stepper moves the arm a half track per adjacent
pulse, the motor and drive select switches latch, the data latch
answers $C0EC, and the boot image loads from the device's own boot
state. The media reads one raw byte stream per track with no GCR
sector framing, and writes latch only.

## 9. Recorded approximations of this build

- The 800 boot stub parks in the ANTIC WSYNC loop.
- The C64 counts raster interrupts through the full IRQ path with a
  snapshot round trip.
- The Apple stub writes the text page and steps the Disk II track.
- Real OS, KERNAL, and BASIC assets and checksum runs remain
  pending the follow-up evidence pass. The serial line checks of
  POKEY and the CIA are part of that pass. The paddles, buttons,
  and speaker toggles count as observable stubs.
- The Disk II media carries no GCR sector framing in this build.

## 10. Debug notes for this family

- The C64 raster interrupt walks the full CPU path: break on the
  IRQ service address to chase raster timing.
- The Apple II switch window at $C050-$C057 is an overlay. Reads
  toggle the video form. A `write` there behaves like a read.
- The C64 `$01` banking is store-only. A program that maps BASIC or
  KERNAL sees the flat picture of this build.