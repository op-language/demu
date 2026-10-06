# The Lynx and PC Engine machines: vl65nc02 and HuC6280

This chapter explains the two machines of the 65SC02-derivative
group: the Atari Lynx and the PC Engine. Each machine runs one clock
domain, so the two-domain machinery sees no new form here. The
machines exercise the die-internal coupling: the map unit, the
on-die timer, and the register-protocol surfaces. The chapter gives
the config anatomy, the subsystem explanations, the quirks, the
input grammar, and the recorded approximations.

The bundled configs: `vl65nc02-atari-lynx` and `mos6502-nec-pcengine`.

## 1. Clock domains

| Machine | Domain | Frequency | Dividers |
|---|---|---|---|
| Lynx | `main` | 16000000 | the CPU at 4, the Mikey LCD walk at 8 |
| PC Engine | `main` | 21477270 | the HuC6280 at 3, the VDC dot at 4 |

## 2. Devices

The Lynx devices:

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `vl65nc02.cpu` | the VTI 65C02 die of the Mikey chip, with the map unit |
| `suzy` | `lynx.suzy` | the sprite engine, the math unit, the PWM audio, the palette |
| `mikey` | `lynx.mikey` | the timers, the LCD line engine, the pad, the cart lines, the map gates |
| `ram0` | `lynx.ram` | the 64 KiB SRAM |
| `bootrom` | `lynx.bootrom` | the boot ROM window over the vector RAM |
| `mapctl` | `lynx.mapctl` | the MAPCTL register |
| `cart` | `lynx.cart` | the stream cartridge with the LNX and raw forms |

The PC Engine devices:

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `huc6280.cpu` | the HuC6280 die: MPR unit, on-die timer, interrupt controller |
| `vdc` | `pce.vdc` | the VDC with the raster compose and the SATB DMA |
| `vce` | `pce.vce` | the palette unit |
| `psg` | `pce.psg` | the six-channel PSG |
| `joypad` | `pce.joypad` | the turbo-tap |
| `ram0` | `pce.ram` | the RAM |
| `cart` | `pce.cart` | the HuCard loader |

The `mos6502-nec-pcengine` triple names the 6502 family, but the
machine carries the real chip kind `huc6280.cpu`. Validation
accepts that one pairing and every other triple keeps the strict
family rule.

## 3. The map-unit overlay model

The Mikey chip wires the address space of the VTI die: the MAPCTL
register at $FFF9 carries the four space bits. A set bit moves the
space to the RAM underneath:

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

The core reads the register back at every instruction boundary and
drives the `suzy_ram`, `mikey_ram`, `rom_ram`, and `vec_ram`
overlay lines from the set bits. The overlay groups resolve by the
line state the core drives. A high-speed bit (bit 7) lengthens
every core cycle by one domain tick.

## 4. Connections

The Lynx wires the map lines between the core, the boot ROM, the
map control register, and the Suzy and Mikey dies, so the overlay
groups resolve together. The pad and switch bytes track the Mikey
space view. The PC Engine wires the VDC interrupts to the die and
the pad to the I/O page. Read each config's connection list before
you change the wiring.

## 5. Boot states

- The Lynx bundled config declares no boot ROM bytes: a machine
  build supplies the 512-byte boot ROM with a `[boot.bootrom]` hex
  state (the `rom` file form does not reach the empty boot-rom
  device in this build). The bundled machine boots with an empty
  ROM surface until a machine build supplies one.
- The Lynx cart boot state selects the `raw` or the `LNX` form.
- The PC Engine boots out of the card: the reset vector always
  comes from bank 0, which is why the machine resets out of the
  card. The HuCard boot state loads the cart file.

## 6. Quirks

| Quirk | Default | Behavior |
|---|---|---|
| `vl65nc02.bbr_smb` | off for the VTI die | the page-bit instructions SMB, RMB, BBS, BBR run as two-cycle NOPs on the Lynx. The quirk turns the real forms on |
| `mos65sc02.bbr_smb` | on in the core | the plain CMOS core keeps the page-bit instructions. The quirk records the chosen flavor |

WAI and STP park the core until the IRQ edge or the reset line, and
a parked core declares no next event until the line rises.

## 7. The input grammar

| Device | Events |
|---|---|
| `lynx.mikey` | `press <name>` and `pause`. Names are the published button bits and joystick names |
| `pce.joypad` | `press <name>` and `release <name>`. Names include `vi` and the direction and action names |

## 8. Subsystem explanations

### The Suzy die

The Suzy die owns the sprite engine with the packed and literal
line forms of the control block chain, the quadrant walk with flip
and direction bits, the scaling, stretch, and tilt accumulators,
and the background and collision buffer compose. The collision
depository write-backs through the machine bus. The math unit runs
with recorded (not simulated) latency. The four pulse-width audio
channels carry their polynomial shift registers, integrate mode,
link clocks, and the mute byte. The palette is twelve bits deep.
The walk runs synchronously inside one display-line visit. The
build and collision buffers stay internal mirrors. The everon bit
keeps the Handy polarity.

### The Mikey die

The Mikey die carries the timer block of four bytes per timer with
the published expiry form: a timer that starts at N expires after
N plus one source periods. It also carries the reload and hold
rules, the cascade chains 0-2-4 and 1-3, the interrupt set and
reset registers, the joypad byte, the switch byte with the slot
lines fixed high, the cart strobe and data lines, and the AUDIN
page select line. The LCD
line engine walks 317-dot line periods over 105 line periods per
frame, with the display rows at periods 2 through 103.

### The stream cartridge

The Lynx cartridge answers the stream form: a ripple address
counter that increments on every read, the block shift register
fed by the IODAT data bit and the SYSCTL1 strobe, and the RCART0
and RCART1 ports that read the banks at $FCB2/$FCB3. The forms are
the `raw` and `LNX` headers, the bank-1 shadow RAM of the
single-bank cards, and the AUDIN 1 MiB carrier form. The BLL
header form stays out.

### The HuC6280 die

The die carries the MPR mapping unit with the published reset
state, the zero-page, stack, and T-cell rule that routes through
MPR 1, and the on-die timer at the I/O offsets
$0C00/$0C01/$0C07 with the recorded `1024/(select+1)` interval
form. The interrupt controller follows the published Hudson form:
the mask register at $1402, the status read at $1403, and the
timer acknowledge at the write to $1403. The service priority: NMI, then the timer, then
IRQ1, then IRQ2. The die intercepts the accesses of the hardware
bank $FF. Every other offset routes through the bus.

### The VDC and VCE

The VDC answers the twenty internal registers with the status
flags, the SATB transfer at the vertical blank, and the raster
compose of one line per visit: 510 dots at the dot divider, 242
line periods, a 256 by 239 frame. The RCR scanline convention
follows the published form: the compare adds the published 64-line
offset to the frame line, so the register carries the target frame
line plus 64. The VDC busy status flag
reads 0, because the VRAM ports are immediate in this build. The
VDC reads the color table bytes through the bus at the
`palette_base` parameter, the declared machine coupling. The direct
window face at $1FF800 answers those bytes read-only with their
byte index.

### The PSG and the turbo-tap

The PSG runs six channels in one device: the 32-entry five-bit
waveform tables, the DDA direct form, the noise form of channels 5
and 6, the main amplitude pair, and the LFO register pair held as a
recorded form. The pad answers the I/O page form: the pad select
write, the published row readback with the CD bits and the country
line (set for the Japan form and clear for the USA form), and the
turbo autofire on a visit cadence.

## 9. Recorded approximations of this build

- The Lynx LCD line period is a recorded 0.15 percent
  approximation of the published line rate.
- The divide latency of the Suzy math unit is recorded, not
  simulated.
- The Lynx serial link, the magtape bits, the parallel-port pin
  semantics, and the IODIR direction gating store their bytes and
  stay out of the model.
- The BLL header form of the stream cartridge stays out.
- The one-cycle VDC and VCE window penalty (the MAME form) stays
  out and the record names it.
- The VDC busy flag reads 0 (immediate VRAM ports).
- The idle form of the six-button rows of the turbo-tap is a
  recorded approximation.
- Real test-ROM suites and the boot-ROM check screen with the
  actual 512-byte boot ROM remain pending the follow-up evidence
  pass.

## 10. Debug notes for this family

- A parked core declares no next event. `run-frame` and free runs
  end at the run boundary tick over a parked core. Check the park
  with `regs`.
- The MAPCTL drive happens per instruction boundary: a debug write
  to $FFF9 takes effect at the next instruction edge, not at the
  write.
- The RCART ports read the stream bytes in order. Count your reads
  to track the stream position.
- The VCE palette window: the VDC reads the palette bytes through
  the bus. Check the `palette_base` param when the colors look
  wrong.