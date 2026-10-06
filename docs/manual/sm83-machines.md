# The SM83 machines: Game Boy and Game Boy Color

This chapter explains the two SM83 machines of demu, the Nintendo
Game Boy (DMG) and the Game Boy Color (CGB). The chapter gives the
config anatomy, the subsystem explanations, the quirks, the input
event grammar, and the recorded approximations of this build.

The bundled configs are `configs/sm83-nintendo-gameboy.toml` and
`configs/sm83-nintendo-gameboy-color.toml`. The triples are
`sm83-nintendo-gameboy` and `sm83-nintendo-gameboy-color`.

## 1. Clock domains

One clock domain serves both machines:

| Machine | Domain | Frequency | Devices on it |
|---|---|---|---|
| DMG, CGB | `main` | 4194304 Hz | all twelve devices (the table below) |

The DMG crystal is one 4.19 MHz crystal. The CPU counts one T-cycle
per tick (`cpu = 1`), and the PPU draws one dot per tick. The Game Boy
has no secondary crystal.

The CGB carries a dual-speed mode: the core runs at the double rate
when the program writes the speed switch. demu models the switch as
the divider change of the CPU device, declared in the design as a
divider-chain change, not as a second domain.

## 2. Devices

The DMG config declares twelve devices:

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `sm83.cpu` | the SM83 core, per-bus-access timing |
| `bootrom` | `sm83.bootrom` | the 256-byte boot ROM, `cgb` selects the shape |
| `cart` | `sm83.cart` | the cartridge loader and mapper |
| `wram` | `sm83.wram` | the work RAM, with the CGB eight-bank form behind `cgb = true` |
| `hram` | `sm83.hram` | the high RAM page |
| `ppu` | `sm83.ppu` | the PPU with the modes, palette, and window forms |
| `apu` | `sm83.apu` | the four channels plus the frame counter |
| `dma` | `sm83.dma` | the OAM DMA engine |
| `hdma` | `sm83.hdma` | the CGB HDMA and GDMA engines (CGB config only) |
| `timer` | `sm83.timer` | the timer with the DIV register tap for the APU |
| `joypad` | `sm83.joypad` | the pad matrix |
| `serial` | `sm83.serial` | the serial link |
| `io` | `sm83.io` | the hardware-register bus mux at $FF00-$FF7F |

The CGB config adds `hdma` and sets `cgb = true` on the kind
parameters of `bootrom`, `wram`, `ppu`, `apu`, and `io`. The cart
device keeps the `sm83.cart` kind: the header of the CGB image
drives the loader.

## 3. Buses, windows, and the boot overlay

The machine declares one bus, `cpu`, with 16 address bits and the
owner `cpu0`. The window list follows the published Game Boy map. All
devices ride the one bus:

| Range | Device | Notes |
|---|---|---|
| $0000-$3FFF | `cart` (ROM0) | the boot overlay covers the first $0100 at reset |
| $0000-$00FF | `bootrom` | overlay `boot`, priority 1 over the cart |
| $4000-$7FFF | `cart` (ROMN) | the banked ROM window |
| $8000-$9FFF | `ppu` (VRAM) | |
| $A000-$BFFF | `cart` (SRAM) | |
| $C000-$DFFF | `wram` | |
| $E000-$FDFF | `wram` (echo) | the partial echo form |
| $FF00-$FF7F | `io` | the hardware-register mux |
| $FF00 | `joypad` | overlay `regs` |
| $FF01-$FF02 | `serial` | overlay `regs` |
| $FF04-$FF07 | `timer` | overlay `regs` |
| $FF10-$FF2F | `apu` | overlay `regs` |
| $FF40-$FF4B | `ppu` (registers) | overlay `regs` |
| $FF46 | `dma` | overlay `regs` |
| $FF50 | `bootrom` | the unmapping latch, overlay `regs` |
| $FF80-$FFFE | `hram` | |

The overlay model: several windows can cover one range. Each window
carries an `overlay` group name and a read and write priority. The
decoder resolves the access to the highest-priority window of the
group the program last opened. The boot overlay puts the boot ROM
over the cart ROM at reset. The $FF50 unmapping latch removes it.

## 4. Connections

| From | To | Semantics |
|---|---|---|
| `ppu.vblank` | `cpu0.vblank` | `irq` |
| `ppu.stat` | `cpu0.stat` | `irq` |
| `timer.int` | `cpu0.timer` | `irq` |
| `joypad.int` | `cpu0.joypad` | `irq` |
| `serial.int` | `cpu0.serial` | `irq` |
| `timer.div_apu` | `apu.div_apu` | `display-sync` |

The `display-sync` connection carries the DIV-register tap that the
APU frame sequencer and the wave channel clock against. A timer
divider edge on the main domain delivers one frame-sync event to the
APU per tap.

## 5. Boot states

- `bootrom` — `hex`: the boot ROM carries the byte-identical
  reconstruction of the 256-byte DMG boot ROM (the CGB config embeds
  the CGB half). A user config may replace it with its own `hex`
  dump. A `rom` path does not reach this device as built, because
  the boot-rom device loads through its hex form only (see the DSL
  specification as-built notes).
- `cpu0` — the register preset: PC starts at $0000, so the machine
  boots through the boot ROM.
- The cart loads through `[boot.cart] rom = "..."`, or through the
  positional ROM path of the CLI, which overrides the boot state.

The boot ROM verifies the cartridge logo and header. When no cart is
loaded, the verification loop never passes and the machine never
reaches $0100. Bound your runs with a real cart, `run-frame`, or an
event the machine produces.

## 6. Quirks

| Quirk | Default | Behavior |
|---|---|---|
| `sm83.oam_bug` | true on DMG | the OAM corruption bug: the OAM DMA and the PPU render race corrupt OAM entries on a $FE00 access window |
| `sm83.wave_ram_undoc` | true both | a wave RAM write during wave playback corrupts the first four bytes, the hardware rule |
| `sm83.halt_undoc` | true both | the HALT edge case: the interrupt enable and flag change during HALT re-runs one byte of the interrupt dispatch |

The CGB config sets `sm83.oam_bug = false`, because the CGB hardware
fixed the bug.

## 7. The input grammar

The `joypad` device takes `press <button>` and `release <button>` for
the buttons `a`, `b`, `select`, `start`, `right`, `left`, `up`,
`down`.

```
inject joypad press start
#main:402176 joypad press start
```

## 8. Subsystem explanations

### The PPU

The PPU walks one dot per domain tick. The line periods: 456 dots
per scanline, 154 lines per frame. The modes walk the hardware
sequence: OAM scan (80 dots), drawing, HBlank, VBlank. The vblank
edge fires once per display frame at the line 144 first dot, which
keeps `run-frame` and the media pipeline at one frame per vblank.

The composition models the published per-line behavior: the
background and window tile fetches, the sprite priority ladder, the
palettes, and the CGB color path. Read the recorded approximations
below before you debug raster-exact effects. The line composition
uses the register state at the line start.

### The APU

Four channels: two pulse, one wave, one noise, with the frame
sequencer clocked by the timer DIV tap. The chip runs its full
register set, because registers drive timing.

### The DMA engine

The OAM DMA engine copies the 160 bytes from the requested source
page at $FF46. The copy takes 160 machine write cycles through the
bus, and the CPU sees the copy as normal bus traffic for snapshot
and trigger purposes. The CGB `hdma` device carries the HDMA line
transfer and the GDMA immediate copy.

### The timer

The timer carries the DIV counter tapped by the APU, the four
selectable periods, and the interrupt edge rules.

### The serial link

The serial shift register and the transfer control register. The
link carries no peer in a single machine. The transfer clock runs
internal.

### The cartridge loader

`sm83.cart` loads the published cartridge forms: ROM-only, MBC1, MBC2,
MBC3 with RTC, and MBC5. The loader derives the mapper from the
header. The boot overlay sits over ROM0 at reset.

## 9. Recorded approximations of this build

The design as-built notes for SM83 machines name the build's
boundaries. The main ones for debugging:

- The SM83 core runs per-bus-access timing with the published
  instruction cycle totals.
- The PPU composes per line from the register state at the line
  start. Mid-scanline register effects inside one visible line are
  not part of the composition of that line.
- The CGB dual-speed switch rides the divider change of the CPU
  device.
- The blargg and mooneye suite runs, the recorded pass counts, and
  the SameBoy golden cross-checks are pending the follow-up evidence
  pass. The bundled configs boot the demo carts. The evidence
  directory holds the boot and determinism runs.

## 10. Debug notes for this family

- The LCD-off state freezes the PPU pipeline: dot zero, no next
  event. `run-frame` waits for a vblank, so with the LCD off a
  `run-frame` run does not stop. Stop with `run [N]` or `step`.
- VRAM and OAM lock during rendering. A `write` into a locked range
  reports success and stores nothing. A `read` is a force peek and
  always reads the buffer.
- The boot overlay sits over the cart until $FF50 writes. `read
  0x0000` before the handoff shows boot ROM bytes. After, cartridge
  bytes.
- Break on a fetch at the cart entry $0100 only after the boot ROM
  hands off. Before that, no fetch reaches $0100.