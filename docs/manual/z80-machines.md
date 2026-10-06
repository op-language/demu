# The Z80 machines: Sega 8-bit, Sinclair, TI-85

This chapter explains the eight Z80 configs of demu: SG-1000, Master
System, Game Gear, ZX80, ZX81, Spectrum 48K and 128K, and TI-85. The
chapter gives the config anatomy, the subsystem explanations, the
quirks, the input grammar, and the recorded approximations.

The bundled configs:

| Config | Triple | Extends | Machine |
|---|---|---|---|
| `z80-sega-sg1000` | `z80-sega-sg1000` | — | SG-1000 |
| `z80-sega-mastersystem` | `z80-sega-mastersystem` | — | Master System |
| `z80-sega-gamegear` | `z80-sega-gamegear` | — | Game Gear |
| `z80-sinclair-zx80` | `z80-sinclair-zx80` | — | ZX80 |
| `z80-sinclair-zx81` | `z80-sinclair-zx81` | — | ZX81 |
| `z80-sinclair-spectrum` | `z80-sinclair-spectrum` | — | Spectrum 48K |
| `z80-sinclair-spectrum128` | `z80-sinclair-spectrum` | extends `z80-sinclair-spectrum` | Spectrum 128K |
| `z80-ti-85` | `z80-ti-85` | — | TI-85 |

## 1. Clock domains and the port plane

Every Z80 config declares two buses: the memory bus and the io bus.
The CPU presents every port access at the plane address
`$10000 | port`, so port windows live in $10000-$1FFFF and never
collide with the memory spans of a device that answers both spaces.

| Machine | Domain frequency | CPU divider (ticks per T-state) |
|---|---|---|
| Sega 8-bit machines | 10738635 (the three-times NTSC crystal, one domain) | 3 |
| ZX80, ZX81 | 3250000 | 1 |
| Spectrum 48K | 3500000 | 1 |
| Spectrum 128K | 3546895 | 1 |
| TI-85 | 6000000 | 1 |

## 2. Devices

| Machine | Devices (kinds) |
|---|---|
| SG-1000 | `z80.cpu`, `tms9918.vdp`, `sn76489.psg`, `sega.io` (`form = "sg1000"`), `z80.ram` (1 KiB), `sega.cart` (`form = "sg1000"`) |
| Master System | `z80.cpu`, `sms.vdp`, `sn76489.psg`, `sega.io` (`form = "sms"`), `z80.ram` (8 KiB), `sega.cart` (`form = "sms"`) |
| Game Gear | `z80.cpu`, `gg.vdp`, `sn76489.psg`, `sega.io` (`form = "gg"`), `z80.ram` (8 KiB), `sega.cart` (`form = "sega"`) |
| ZX80 | `z80.cpu`, `zx.ula` (`form = "zx80"`), `z80.rom`, `z80.ram` |
| ZX81 | `z80.cpu`, `zx.ula` (`form = "zx81"`), `z80.rom`, `z80.ram` |
| Spectrum 48K | `z80.cpu`, `zx.ula` (`form = "spectrum"`), `z80.rom`, three `z80.ram` devices of 16 KiB each |
| Spectrum 128K | the 48K devices plus `z80.bankrom`, `zx.bankram`, and the new windows and lines |
| TI-85 | `z80.cpu`, `ti85.mem`, `ti85.lcd`, `ti85.link` |

The flat memory kinds `z80.ram` and `z80.rom` take a required `size`
param. A device maps addresses by modulo, so a window base must be a
multiple of the device size. The 48K config splits the RAM into
three 16 KiB devices at $4000, $8000, and $C000 because of this
rule.

## 3. The core

`z80.cpu` runs the full documented instruction set with the
published per-access timing: one opcode fetch costs 4 T-states. One memory access costs 3. One port access
costs 4 with its automatic wait state. The index forms pay their
internal address add.

The interrupt machinery runs modes 0, 1, and 2: an NMI service
costs 11 T-states. Mode 1 accepts in 13. Mode 2 accepts in 19, and
the vector low byte comes from the port plane at 0x10000. An EI defers
maskable acceptance by one boundary. Mode 0 runs the accepted data
byte at the interrupted address.

A repeated block instruction refetches its opcode pair once per
iteration. Block operations run to their end without sampling
interrupts. The core samples only at instruction boundaries.

## 4. Quirks

| Quirk | Default | Behavior |
|---|---|---|
| `z80.undocumented` | not set by any bundled config: the Sega cores run the documented instruction set | the index-register halves (IXH, IXL, IYH, IYL), the SLL shift, the `OUT (C),0` store, the `IN (C)` flag-only form, and the index IN/OUT forms |
| `zx.zx81_slow` | true on the ZX81 config | the ZX81 slow-mode repaint flow |
| `sega.gg_start` | true on the Game Gear config | the START-key staged behavior on port $00 |

With `z80.undocumented` clear, the undocumented forms run
effect-free: the `IN (C)` and `OUT (C),0` pair falls to the
8-T-state unknown-form fallback with no bus access, and the index
halves read zero. The SLL shift and the index IN/OUT forms keep
their access totals.

## 5. The input grammar

| Device | Events |
|---|---|
| `sega.io` | `press <key>` and `release <key>`, or the bare `key` and `key-release` forms; the player prefix `p1-` or `p2-` picks the port; keys: `up`, `down`, `left`, `right`, `1`, `2`, `start`, `pause` (SMS only), and `th-0`, `th-1` |
| `zx.ula` | `press <key>`, `release <key>`, `key-release`, or the bare `key`; names come from the Spectrum matrix |
| `ti85.lcd` | `press <key>` and `release <key>` against the TI matrix |
| `ti85.link` | `link <white> <red>`, with `0` or `1` for each line |

## 6. The Sega systems

### The VDP family

One VDP type carries the three forms by kind: `tms9918.vdp` (the
TMS9918A modes over a 16 KiB VRAM), `sms.vdp` (the SMS1 class), and
`gg.vdp` (the SMS2-class Game Gear). The TMS modes render Graphics
I, Graphics II, Text 40, and Multicolor. Invalid mode-bit
combinations fall through to Graphics I. The mode 4 render runs on
the SMS and Game Gear forms with the 32x28 name table, the
per-line horizontal scroll, the scroll locks, the 64 sprites with
the 8-per-line limit, the D0 row terminator, collision, and the
224/240-line height modes behind the `extended_height` parameter
(SMS only).

The mode-4 data-port read publishes the V counter with the
published 218/213 fold. The H counter read is not implemented. The
vblank trigger emits once per frame boundary. The frame and line
interrupt flags ride the R1 bit 5 and R0 bit 4 enables with the R10
counter.

The SMS CRAM holds one byte per entry (red in bits 1-0, blue in
bits 3-2, green in bits 5-4) with the upper half mirrored. The Game
Gear CRAM holds two bytes per entry and publishes the 160x144
window of raster rows 24 to 167 and columns 48 to 207. The Game
Gear centered border is not composed. The window is the whole
surface.

### The SN76489

Three tone channels with 10-bit periods (a period of zero or one
holds a constant level), a 4-bit attenuation each, and a noise
channel with the 16-bit LFSR, white taps {0, 3}, the periodic
single tap, control writes resetting the register to $8000, and the
shift-rate table 16, 32, 64, or the tone C period. The mixed
output uses a linear 16-step attenuation. The chip's per-step LAD
ladder is not modeled.

### The IO die and the cartridge

The `sega.io` die answers the $DC-$DD pad ports with the
active-low directions and fire lines under the TH swap rule. On the
sms form a `pause` press latches one staged NMI pulse. On the
game gear form the START key reads through port $00 bit 7.

The `sega.cart` loads the flat SG-1000 form, the flat SMS form, the
Sega 16 KiB-page mapper (the latches at $3FFE/$3FFF/$402C/$402D),
and the Codemasters mapper (the $0000 write banks the $8000 window
at value + 1), all wrapping by page count.

## 7. The Sinclair systems

### The ULA

The `zx.ula` carries the three machine forms. The $FE port write
stores border bits 1-3, the beeper drive (bit 4 on the Spectrum,
bit 0 on the ZX80/ZX81), and the microphone bit 0. The read returns
the keyboard half-row selected by the port high byte with pressed
keys as cleared bits and the EAR bit 6 low.

The frame rhythm: the Spectrum raises `int` for one scanline at
line 0 and publishes the 320x240 frame (256x192 with the border).
The ZX80 raises one `nmi` per frame. The ZX81 raises `int` on the
display lines and `nmi` on the blank lines under the
`zx.zx81_slow` quirk.

### Memory contention

The delay follows the published ULA pattern 6, 5, 4, 3, 2, 1, 0, 0
indexed by the frame T-state modulo 8. The parameters
`contended`, `io_contend`, `contend_from`, `contend_to`, and
`contend_base` live on the CPU device entry:

- The 48K window runs over frame T-states 14336 to 57344 with the
  phase base 14344.
- The 128K window runs 14361 to 58140 with the base 14361.
- I/O contention keys on the high port byte $40-$7F range and on
  port $FE itself.
- The frame anchor is the tick of the `int` line rising edge, so
  contention stays phase-locked to the frame start.
- The banked-page contention of the 128K rides the `contend_page`
  line from `zx.bankram` (high when the paged bank is 1, 3, 5, or
  7).

### The 128K memory controller

The 128K variant keeps the base's 16 KiB ROM device: the bundled
config does not use the `z80.bankrom` two-page form, and the
`rom_bank` connection to the plain ROM device is inert in this
build. `zx.bankram` holds the 128 KiB of RAM with the fixed banks 5
and 2, the paged door over $C000-$FFFF, and the $7FFD paging port
(write-only, reads $FF, lockable). The 128K variant extends the 48K
config: the PAL crystal 3546895, the 228-T-state 311-line frame,
the banked map, and the new line set.

## 8. The TI-85

`ti85.mem` holds the 128 KiB ROM in eight 16 KiB pages and the 32
KiB RAM in two banks. The page map: $0000-$3FFF follows the ROM
page latch on port $08, $4000-$7FFF holds ROM page 0, $8000-$BFFF
follows the RAM bank latch on port $06 (reset 1), and $C000-$FFFF
follows the port $07 latch (reset 0).

`ti85.lcd` holds the 128x64 one-bit display buffer at $FC00-$FFFF
(bit 7 is the leftmost pixel of each byte) with the driver page
inside at $FEC0-$FEFF. The driver decodes three addresses: $FEC0
latches a command (01 fills the buffer with the next data byte, 02
is accepted as no operation, 08 and 0C turn the display off and
on), $FEC2 latches the keypad column in bits 2-0, and the keypad
read reports pressed keys as cleared bits with bit 7 idle high.
The frame publishes from the buffer on every touched write.

`ti85.link` answers port $05: the write carries the drive levels
in bits 0-1 and the output enables in bits 2-3. The read reports
the enabled lines as the local drive and the floating lines as the
remote levels, with the remote levels injected with the `link`
event form.

The machine boots to the DI and the HALT park at 0x0000. The build
carries no
interrupt sources in this build.

## 9. Recorded approximations of this build

- The contended access path carries four range slots per side. A
  config with more entries contends only the first four.
- The ZX80/ZX81 display renders each display line from the display
  file and the character ROM through peek, one row per scheduled
  line, rather than from the recorded bus traffic of the running
  core.
- The ZX81 slow mode repaints each DFILE row at four display-line
  visits.
- The TI-85 latch values alias the memory addresses $0006-$0008,
  because a device sees no bus identity in an access. A real ROM
  image whose first bytes sit there is unservable. A bundled
  machine avoids the span through its park image.
- The TI-85 LCD driver page inside $FEC0-$FEFF is the record of the
  published driver layout, pending the real-ROM evidence pass.
- The interrupt acknowledge of modes 0 and 2 reads the vector byte
  at the port plane address $10000. On machines whose port decode
  answers that address, the acknowledge byte comes from that
  device. Bundled machines use mode 1.
- ZEXALL, ZEXDOC, the per-system test-ROM screens, the Sinclair
  real-BIOS contention check, the ZX80/ZX81 real-BIOS boot
  screens, and the TI-85 real-ROM boot remain pending the
  follow-up evidence pass.

## 10. Debug notes for this family

- `run-until` on the port plane: the ports live at $10000 | port,
  so `run-until read 0x101DC` stops on a $DC read.
- The ZX Spectrum contention pattern depends on the frame anchor:
  compare `regs` and a `triggers`-free `run-frame` run for the
  steady state.
- The Game Gear START key reads through port $00: `run-until read
  0x10000` stops at the port.
- The TI-85 keypad reads through the LCD driver page. Check
  `read 0xFEC0` for the command latch state.