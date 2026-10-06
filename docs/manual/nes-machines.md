# The 6502 NES machines: NTSC and PAL

This chapter explains the four NES configs of demu: the RP2A03 NTSC
machine, the RP2A07 PAL machine, and the two plain-6502 alias
configs that extend them. The chapter gives the config anatomy, the
subsystem explanations, the quirks, the input grammar, and the
recorded approximations.

The bundled configs:

| Config | Triple | Base |
|---|---|---|
| `rp2A03-nintendo-nes-ntsc` | `rp2A03-nintendo-nes-ntsc` | — |
| `rp2A07-nintendo-nes-pal` | `rp2A07-nintendo-nes-pal` | — |
| `mos6502-nintendo-nes-ntsc` | `mos6502-nintendo-nes-ntsc` | extends the RP2A03 NTSC config |
| `mos6502-nintendo-nes-pal` | `mos6502-nintendo-nes-pal` | extends the RP2A07 PAL config |

## 1. Clock domains

One clock domain, `main`. The NTSC master crystal is the rational
frequency `236250000/11` Hz. The CPU sits at divider 12 and the PPU
dot clock at divider 4 of the same crystal, so the three-dot
relationship of the CPU cycle and the PPU dot is exact without float
math.

| Machine | Domain frequency | CPU divider | PPU dot divider |
|---|---|---|---|
| RP2A03 NTSC | `236250000/11` | 12 | 4 |
| RP2A07 PAL | `53203425/2` | 16 | 5 |

The frame runs 89341 dots on NTSC: 341 dots per scanline over 262
scanlines, minus the skipped last dot of the pre-render scanline on
odd frames.

## 2. Devices

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `rp2A03.cpu` / `rp2A07.cpu` / `mos6502.cpu` | the 6502 core with per-access timing |
| `ppu` | `rp2A03.ppu` / `rp2A07.ppu` | the 2C02/2C07 PPU |
| `apu` | `rp2A03.apu` / `rp2A07.apu` | the 2A03/2A07 audio and IO register file |
| `dma` | `rp2A03.dma` / `rp2A07.dma` | the OAM DMA engine |
| `joypad` | `rp2A03.joypad` / `rp2A07.joypad` | the two controller ports |
| `cart` | `rp2A03.cart` / `rp2A07.cart` | the iNES loader and mappers |
| `wram` | `rp2A03.wram` / `rp2A07.wram` | the 2 KiB work RAM |

The alias configs keep every chip device of the parent config and
override only the CPU kind to `mos6502.cpu` with the same divider,
because `std` names the plain 6502 triple.

## 3. The extends alias pattern

The plain-6502 configs are one-file aliases:

```toml
[system]
name = "Nintendo NES (NTSC, plain 6502)"
triple = "mos6502-nintendo-nes-ntsc"
description = "The NTSC NES machine viewed as a plain MOS 6502."

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

The base resolves as a bundled config. The device override replaces
the named device of the base. Validation re-runs on the merged
config.

## 4. Connections

| From | To | Semantics | Role |
|---|---|---|---|
| `ppu.nmi` | `cpu0.nmi` | `irq` | the vblank NMI edge every frame |
| `apu.irq` | `cpu0.irq` | `irq` | the frame IRQ |
| `cart.irq` | `cpu0.irq` | `irq` | the mapper IRQ (the MMC5 scanline IRQ) |
| `ppu.scanline` | `cart.scanline` | `display-sync` | the scanline feed of the MMC5 counter |
| `ppu.vblank` | `cart.vblank` | `display-sync` | the vblank feed of the mapper |
| `dma.oam_halt` | `cpu0.halt` | `dma-request` | the OAM DMA ready-line window |
| `apu.halt` | `cpu0.halt` | `dma-request` | the DMC reader ready-line window |

The OAM DMA and the DMC reader drop the CPU ready line for the
transfer window. The run loop reschedules the core at the release
edge. The PPU emits a runtime vblank event every frame, so
`run-frame` and `run-until vblank` work.

## 5. Boot states

- The bundled configs declare no boot states. The positional ROM
  argument of the CLI supplies the cartridge image: demu overrides
  the cart device's boot state with a ROM file state and reboots.
- A user config may add `[[device]]` boot states, for example
  `[boot.cart] rom = "demo.nes"`.

The iNES loader parses the header, derives the mapper, and rejects
images it cannot serve.

## 6. Quirks

| Quirk | Default | Behavior |
|---|---|---|
| `rp2A03.undocumented` | true | the NMOS undocumented opcodes of the 2A03, enabled because NES software uses them |
| `rp2A07.undocumented` | true | the PAL form of the same rule |
| `mos6502.undocumented` | true | the same rule on the plain 6502 view |

## 7. The input grammar

The `joypad` device takes `[p1|p2] press <button>` and `[p1|p2]
release <button>`. A bare event without the player word lands on
port 1. The buttons: `left`, `right`, `up`, `down`, `a`, `b`,
`select`, `start`.

```
inject joypad p1 press start
#main:89342 joypad p1 press a
```

## 8. Subsystem explanations

### The PPU

The PPU walks one dot per dot-divider tick with the exact scanline
and frame timing. It carries sprite 0 hit, the sprite overflow flag,
and the vblank edge. The composition composes one scanline per visit
from the register state at the scanline start. Mid-scanline raster
effects inside one line are not part of that line. Cycle-precision
sprite evaluation is a follow-up.

A register write that must reach another device without machine
context queues for the next dot visit: the pattern-bus write of
$2007 drains before the next dot.

The font renderer of `std` erases its glyph planes through PPU
writes to character space. A cartridge whose header carries
character banks keeps those writes read-only, so a demo renders text
with the blank character set. Other emulators behave the same way
for this image.

### The APU

The channels run with the frame IRQ and the DMC reader. The DMC
reader drops the CPU ready line for its fetches, exactly like the
OAM DMA window.

### The cartridge mappers

The loader supports the iNES forms with these mappers:

| Mapper | Form |
|---|---|
| NROM | the flat 16 KiB or 32 KiB image |
| UNROM | one switchable 16 KiB bank at $8000 and the last bank fixed at $C000 |
| MMC1 | the five-write shift register |
| MMC5 | the program page registers and the scanline IRQ |

An eight-kilobyte program RAM answers $6000-$7FFF on every mapper.
The MMC5 extended RAM bank and the split mode are out of the set.

## 9. Recorded approximations of this build

- Sprite-zero hit timing and the sprite-overflow bug follow the
  simplified per-scanline evaluation.
- The PAL machine uses the same structure with the PAL sequencer
  steps, the PAL DMC and noise tables, and the PAL crystal.
- The nestest and blargg acceptance runs and the cross-checks
  against other emulators are pending the follow-up evidence
  pass. The bundled configs boot the demo cart. The evidence
  directory holds the boot and determinism runs.

## 10. Debug notes for this family

- `run-until vblank` stops at a frame boundary on both machines.
- The PPU register writes compose per line. Check `dump-nametable`
  and `dump-oam` between frames for exact mid-frame state.
- The NTSC and PAL configs report both on `info` with their triple.