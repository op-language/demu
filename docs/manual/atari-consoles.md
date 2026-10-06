# The 6502 Atari console machines: 2600, 5200, 7800

This chapter explains the three Atari console machines of demu: the
2600 with the TIA, the 5200 with the ANTIC/GTIA die pair and POKEY,
and the 7800 with MARIA. The chapter gives the config anatomy, the
subsystem explanations, the quirks, the input grammar, and the
recorded approximations.

The bundled configs: `mos6502-atari-2600`, `mos6502-atari-5200`, and
`mos6502-atari-7800`. The triples use the same names.

## 1. Clock domains

The three consoles share one color-clock crystal family:

| Machine | Domain frequency | CPU divider | Notes |
|---|---|---|---|
| 2600 | `315000000/88` | 3 | the domain is the color clock itself; the TIA dot divider is 1 |
| 5200 | `315000000/44` | 4 | the double-rate master; the color clock is at divider 2 |
| 7800 | `315000000/44` | 4 | the same form; MARIA walks one pixel clock per master tick |

## 2. Devices

| Machine | Devices (kinds) |
|---|---|
| 2600 | `mos6502.cpu` (the 6507 form: divider 3, no interrupt lines), `atari.tia`, `atari.riot`, `atari.cart2600` |
| 5200 | `mos6502.cpu` (divider 4, the SALLY form), `atari.antic` (the ANTIC/GTIA pair device), `atari.pokey`, `atari.ram`, `atari.rom`, `atari.cart5200` |
| 7800 | `mos6502.cpu` (divider 4), `atari.maria`, `atari.tia` (video off), `atari.riot`, `atari.ram`, `atari.rom`, `atari.cart7800` |

The CPU is the plain `mos6502.cpu` kind on all three machines. The
divider override: three ticks per cycle on the 2600 (the 6507), and
four on the 5200 and the 7800 (SALLY). The 6507 connects no interrupt
lines. The `undocumented` quirk is on: NES-era and Atari-era software
uses the undocumented opcodes.

## 3. Buses and windows

The 2600 cart window covers the image forms 2K, 4K, F8, F6, and F4,
wrapping by page count. The 5200 map: 16 KiB RAM at $0000, the flat
32 KiB cart window at $4000 end-aligned, GTIA at $C000, ANTIC at
$D400, POKEY at $E800, and the 2 KiB BIOS at $F800. The 7800 map:
TIA at $0000, MARIA at $0020, the RAM shadow of the zero page and
the stack page, the RIOT at $0280, main RAM at $1800-$27FF, the cart
at $4000-$BFFF and $D000-$FFFF, and the BIOS at $C000.

Cart banking forms on the 7800: flat images map down from $FFFF.
banked images page 16 KiB chunks into $8000-$BFFF from the written
value. The audio-cartridge variants are out of the set.

## 4. Boot states

- The 5200 and the 7800 load the console BIOS in `[boot.bios]`.
- The carts load through the device boot state or the positional ROM
  argument.

## 5. Quirks

| Quirk | Default | Behavior |
|---|---|---|
| `mos6502.undocumented` | true | the NMOS undocumented opcodes |

The TIA-family hardware quirks are built into the device behavior
itself and are not config flags in this build: the TIA register
strobes re-base at the beam with the decode delay, the HMOVE strobe
shifts each object by `8 - (HMX ^ 0x08)`, and the RIOT timer carries
the zero-hold and fast-wrap flow.

## 6. The input grammar

| Device | Events |
|---|---|
| `atari.tia` | `p0-fire`, `p0-fire-release`, `paddle0 <value>` |
| `atari.riot` | `p0-up`, `p0-down`, `p0-left`, `p0-right`, `p1-up` through `p1-right`, `select`, `reset`; each with the `-release` suffix |
| `atari.pia` (5200) | `p0-up` through `p1-right`, with the `-release` suffix |
| `atari.antic` (5200) | `trigger0` through `trigger3` press and `-release` |
| `atari.pokey` | `key <n>` and `key <n>-release`, `pot<n> <value>` for n 0-7 |

## 7. Subsystem explanations

### The TIA

The 2600 TIA walks one color clock per visit. The position counters
follow the TIA hardware notes: register strobes re-base at the beam
with the decode delay, copies follow the NUSIZ layout, and the HMOVE
strobe shifts each object by `8 - (HMX ^ 0x08)`. The TIA sound
engine follows the published divider model with the poly taps and
the 13:18 Div-31 duty, ticked at the 31.4 kHz audio clock. The 7800
keeps the same device with the video parameter off.

The collision bits follow the published register map, with two
simplifications: a missile reports its hit against either player of
its register, and the priority table picks the first matching layer.

### The RIOT

The RIOT 6532 carries 128 bytes of RAM, the interval timer with the
zero-hold and fast-wrap phases, and the port registers. The read of
INTIM or INTFLG that clears the flag reverts the wrap mode. The
exact hardware edge alignment of that revert is approximate.

### The 5200 display pipeline

The ANTIC display-list processor and the GTIA register file compose
together as one device, because the pixel output is internal to the
die pair. The GTIA register file answers $C000 and the ANTIC file
answers $D400. The display list walks at the line start. The DMA
bytes are stolen machine cycles of one halt pulse, not the
intra-line interleave of the real circuit. A display list with LMS
and JVB walks and paints picture rows.

### The 7800 MARIA

MARIA walks one pixel clock per master tick, 456 cycles per line.
The display-list entries, the direct mode entries, and the 160A/B
encodings run per the GCC software guide. The 320-pixel encodings,
the character modes, holey DMA, and the Kangaroo and border control
bits decode but are not exhaustively verified.

### POKEY

POKEY carries the channel dividers with the joined sixteen-bit
pairs, the seventeen-bit and nine-bit global polys, the four-bit and
five-bit per-channel polys, the keyboard-code register with the key
interrupt, and the IRQ mask with the write-to-acknowledge rule. The
serial port and the pot scan timing are stubs.

## 8. Recorded approximations of this build

- Cycle-precision collision positions and the response-register
  quirks are a follow-up verification pass.
- Per-console test-ROM patterns and BIOS checksum runs remain
  pending a follow-up evidence pass with real ROM assets. The
  acceptance programs of these machines are synthetic images with
  recorded outputs.
- The 2600 kernel drives the TIA through WSYNC and composes a
  deterministic playfield frame. The 5200 BIOS hands off to the cart
  and the ANTIC walks a real display list. The 7800 hands off to the
  cart and the MARIA display-list DMA paints its pixels.

## 9. Debug notes for this family

- WSYNC halts the CPU to the next line boundary. Use `run-until` on
  a read or write of a TIA strobe to stop at handoff points.
- The kernel composes per line. Check `dump-framebuffer` between
  frames.
- The BIOS window at $F800 (5200) answers only after the boot
  completes. Compare `read 0xF800` before and after the BIOS hands
  off.