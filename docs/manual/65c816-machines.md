# The 65C816 machines: SNES and Apple IIgs

This chapter explains the two 65C816 machines of demu: the SNES with
its two crystals and the Apple IIgs. These are the first two-domain
machines, and they exercised the cross-domain machinery for the
first time in an assembled machine. The chapter gives the config
anatomy, the subsystem explanations, the quirks, the input grammar,
and the recorded approximations.

The bundled configs: `wdc65c816-nintendo-snes` and
`wdc65c816-apple-iigs`.

## 1. Clock domains

The SNES runs two unrelated crystals:

| Domain | Frequency | Devices | Dividers |
|---|---|---|---|
| `main` | `236250000/11` | the 65C816, the S-PPU, the 5A22 support die, the WRAM, the pad, the cart | `cpu = 6`, `dot = 4` |
| `sound` | `1024000` | the APU die (the SPC700 core and the S-DSP engine in one device) | `cpu = 1` |

The two domains never share a timeline. The scheduler orders
cross-domain events by the exact rational comparison of the domain
times.

The IIgs runs one domain:

| Domain | Frequency | Devices | Dividers |
|---|---|---|---|
| `main` | 28636360 | the 65C816, the Mega II glue, the Apple video, the DOC, the IWM | `cpu = 10`, `dot = 4` (video), `cpu = 14` (per the DOC visit form) |

## 2. Devices

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `wdc65c816.cpu` | the 65C816 core, full 256-opcode map in both modes |
| `ppu` | `snes.ppu` | the S-PPU pair as one device |
| `sys` | `snes.sys` | the 5A22 support die: DMA, HDMA, multiply/divide, H/V timers, auto-joypad |
| `wram` | `snes.wram` | the WRAM with the bank window and the port |
| `pad` | `snes.pad` | the controller port |
| `cart` | `snes.cart` | the LoROM/HiROM loader with the SRAM params |
| `apu` | `apu.core` | the SPC700 core and the S-DSP engine on the sound domain |

The IIgs devices:

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `wdc65c816.cpu` | the 65C816 core |
| `mega2` | `iigs.mega2` | the console glue: keyboard, IIe switches, IIgs new-video, shadow, clock, CYA, and interrupt registers |
| `video` | `a2.video` | the Apple video at the classic dot clock |
| `doc` | `iigs.doc` | the Ensoniq DOC with the sound GLU face |
| `iwm` | `iigs.iwm` | the disk port at the IWM state form |
| `sram`, `rom` | `atari.ram`, `atari.rom` | the flat memory kinds |

## 3. Memory windows and the mirror form

The SNES windows follow the published map. The bus mirror form
repeats on the stride boundary: repeat `k` at `base + k*stride`
carries the first `size` bytes. The LoROM ROM and SRAM regions are
four windows, each with one mirror stride of one bank:

```toml
[[window]]
name = "w_cart_rom_low"
bus = "cpu"
device = "cart"
base = "0x008000"
size = "0x008000"
mirror_stride = "0x010000"
mirror_count = 63
```

The WRAM mirrors repeat the same form at the low and the high half
of the space.

## 4. Connections

| From | To | Semantics | Role |
|---|---|---|---|
| `ppu.scanline` | `sys.scanline` | `display-sync` | the scanline feed of the 5A22 timers |
| `ppu.nmi` | `sys.vblank` | `display-sync` | the vblank feed into the support die |
| `sys.nmi` | `cpu0.nmi` | `irq` | the CPU NMI line |
| `sys.irq` | `cpu0.irq` | `irq` | the CPU IRQ line |
| `sys.halt` | `cpu0.halt` | `dma-request` | the DMA/HDMA halt pulses |
| `cpu0.ports` | `apu.ports` | `async-port` | the audio-port crossing, the only two-domain route |

## 5. Boot states

- `cpu0` — the register preset.
- `cart` — the cart file through the device boot state or the
  positional ROM argument.
- `apu` — the boot state preloads the sound RAM and the reset
  vector. The config declares the `apu.port_race_or` quirk key with
  the value false.

## 6. Quirks

| Quirk | Default | Behavior |
|---|---|---|
| `apu.port_race_or` | false on the SNES config | the audio-port read that overlaps a write returns the OR of the old and new bytes, with a deterministic one-visit window |

## 7. The input grammar

The `pad` device takes `pad1 <hex u16>` and `pad2 <hex u16>`, with
the `-release` suffix that clears the bits:

```
inject pad pad1 8000
#main:2000 pad pad1 0800
```

The IIgs `mega2` device takes `key <byte or ASCII char>`,
`paddle<n> <0-255>`, and `button<n>` with buttons 0-3.

## 8. Subsystem explanations

### The 65C816 core

The core runs all 256 opcodes in both emulation and native forms,
with the M/X register widths, the direct page, the data and program
banks, the block move, WAI and STP, and the interrupt vector tables
of both modes. One instruction runs per visit. Every bus access
carries its cycle stamp.

### The S-PPU

The S-PPU renders one line per visit over the composed modes 0
through 7 with EXTBG, the sprite evaluation with the time-over
latch, the color math, the mosaic, and the counter and latch
registers of both dies in one device.

### The 5A22 support die

The die carries the DMA and HDMA engines with their halt pulses to
the CPU on the declared line, the multiplication and division
units, the H and V timers with a fixed-dot H approximation, and the
auto-joypad read. One display-list DMA burst fills VRAM through the
$2118/$2119 pair.

### The audio die

The `apu.core` device holds the full SPC700 core in one device on
the sound domain: 256 opcodes, the timers with their precounter,
the DIV/MUL unit, the bit operations, and the DSP bridge. The same
device holds the S-DSP engine: eight voices, BRR decode, envelope,
echo, and noise. The
audio ports work as latched registers: the CPU window answers
$2140-$217F and the SPC sees $F4-$F7, each side sampling at its own
instruction boundaries. The IPL boot handshake is not modeled. The
boot state preloads the sound RAM and the reset vector. SLEEP and
STOP halt only the core. The timers and the DSP keep the sample
clock exact, and the halt state rides the snapshot.

### The Apple IIgs dies

The Mega II answers the console page. The DOC and IWM windows
overlay it. The IWM carries the phase pairs, the drive enable and
select, and the Q7/Q6 data, status, handshake, and mode register
path. The DOC runs one phase step per visit over the published
frequency-multiply form, and its register map follows the ES5503
group layout. The language-card and auxiliary banking forms, the
shadow copy engine, the ADB mouse and keyboard registers, and the
one-second and quarter-second interrupt timers are not modeled.

## 9. Recorded approximations of this build

- The scheduler carries its secondary-domain bound. `step_once`
  compares every domain's pending event against the target's exact
  real time. A secondary domain free-runs only up to the target
  instant. A device with no periodic work declares no next event.
- The port race window closes at the next visit boundary rather
  than the exact sub-cycle strobe.
- The per-dot S-PPU timing, interlace, window masks, and direct
  color stand as recorded approximations.
- The boot view of the IIgs keeps the monitor ROM mapped at bank
  $00 $F000-$FFFF. The extended ROM banks and the language-card
  views stay out of the build.
- Real test-ROM suites, the SameBoy-class cross-checks, and the
  BIOS checksum runs remain pending the follow-up evidence pass,
  per the precedent of the SM83 and NES families.

## 10. Debug notes for this family

- The async proof: two machines, same inputs, byte-identical save
  states across both domains. Use this to bisect async regressions.
- `run-frame` paces on the primary domain. The SNES audio stays on
  the sound domain and only advances up to the run target.
- The audio-port window answers without the die running: writes
  latch into the port registers, and the die samples on its own
  instruction boundaries.
- A queued port write reaches the die only through its own visits.
  Do not expect a port write from the debug path to wake a parked
  die.