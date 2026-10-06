# The 68000 dual-CPU machines: Genesis and Neo Geo AES

This chapter explains the two 68000 machine families of demu: the
Sega Mega Drive (Genesis) with its two domains and the Neo Geo AES.
Each machine carries a main 68000 and a Z80 sound core. The chapter
gives the config anatomy, the subsystem explanations, the quirks,
the input grammar, and the recorded approximations.

The bundled configs:

| Config | Triple | View |
|---|---|---|
| `m68000-sega-genesis` | `m68000-sega-genesis` | the 68000 view |
| `z80-sega-genesis` | `z80-sega-genesis` | the sound view |
| `m68000-neogeo-aes` | `m68000-neogeo-aes` | the 68000 view |
| `z80-neogeo-aes` | `z80-neogeo-aes` | the sound view |

Both views define the same machine. The view configs name the
Z80's crystal under the name that sorts first, so the primary
domain of that session is the sound crystal, and run commands pace
on the Z80.

## 1. Clock domains

| Machine | Domain | Frequency | Dividers |
|---|---|---|---|
| Genesis | `m68k` | 53693175 (the NTSC master crystal) | the 68000 `cpu = 7`, the VDP `dot = 1` |
| Genesis | `z80` | `53693175/15` | the Z80 `cpu = 1` |
| Neo Geo AES | `m68k` | 24000000 | the 68000, the LSPC at /4 |
| Neo Geo AES | `z80` | `24000000/6` | the Z80, the YM2610 at its published division |

A PAL Genesis machine swaps the master crystal to 53203424 Hz and
sets the VDP's `region` parameter.

## 2. Devices, Genesis

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `m68000.cpu` | the 68000 core |
| `soundcpu` | `z80.cpu` | the Z80 sound core on its own domain and bus |
| `vdp` | `genesis.vdp` | the Mega Drive VDP with the slot timing |
| `arbiter` | `genesis.arbiter` | the bus arbiter and the control trio |
| `io` | `genesis.io` | the controller ports |
| `mapper` | `genesis.mapper` | the sound bus mapper with the 8 KiB sound RAM and the 2 KiB boot ROM |
| `ym` | `ym2612.psg` | the YM2612 |
| `psg` | `sn76489.psg` | the PSG |
| `wram` | `z80.ram` | the Z80 work RAM |
| `cart` | `genesis.cart` | the cart ROM and SRAM |

## 3. Devices, Neo Geo AES

| Device | Kind | Purpose |
|---|---|---|
| `cpu0` | `m68000.cpu` | the 68000 |
| `soundcpu` | `z80.cpu` | the Z80 sound core |
| `lspc` | `neogeo.lspc` | the video die |
| `io` | `neogeo.io` | the watchdog, LED, and port die |
| `soundio` | `neogeo.soundio` | the sound-side latch die |
| `palette` | `neogeo.palette` | the palette chip |
| `ym` | `ym2610.chip` | the YM2610 (OPNB) |
| `bios` | `neogeo.rom` | the BIOS image chip |
| `wram`, `nzram` | `z80.ram` | the two work RAMs |
| the image chips | `neogeo.cart`, `neogeo.m1rom`, `neogeo.rom` | the P/S/C/V/M1 ROM chips |

The image chips carry names that the die params reference: `srom`,
`crom`, `adpcma`, `adpcmb`, and the `bios` name of the P ROM cart.
The referenced devices deliver their boot image bytes to the
referencing kind.

## 4. The bus arbiter and the bank mapper

`genesis.arbiter` answers the control trio on the 68000 bus at
$A11000 (the memory mode, bit 0: 0 the boot ROM form, 1 the RAM
form). $A11100 carries the bus request: the write with bit 0 clear
registers the request. The grant lands on the arbiter's next tick
visit, and the read's grant bit is active low. The write with bit 0
set releases. $A11200 pulses the sound core's reset on a write
with bit 0 clear.

The arbiter drives `stall` high while the request stands and low on
release, and pulses `zreset`. Both route to the sound core over
declared connections with `async-port` semantics.

`genesis.mapper` owns the 8 KiB of sound RAM and the 2 KiB boot ROM
image. The sound bus's $0000-$1FFF window answers the RAM with the
boot ROM image over the first 2 KiB while the memory mode reads 0,
and the RAM itself in mode 1. The $6000-$7FFF window carries the
nine-bit shift-register bank. The $8000-$FFFF window borrows the
68000 bus's decoder in 32-KiB pages through the bank register. The
68000-side window at $A00000-$A03FFF answers the sound RAM view
directly.

## 5. The borrowed-decoder route

The core's `BusAccess` carries a `borrow` decision beside the decode
decision. A window device that composes a cross-bus access itself
answers with the borrowed bus name and the translated address. The
visit context re-decodes the access on that bus and the answering
device there serves it. A loop of bridges stops at the depth cap of
four and the access answers nothing. The machine-level debug reader
and writer follow the same route, so `read` and `write` cross a
bridge the same way the run does.

No configuration surface exists for the route beyond the `main_bus`
param, because the composing device behaves like a bus owner.

## 6. The interrupt wiring

The 68000 decodes its interrupt level from three open-collector IPL
lines. The level's binary digits are the asserted lines:

- The VDP drives the level-6 line to IPL bits one and two.
- The level-4 interval line drives bit two.
- The level-2 feed and the YM2612's timer interrupt drive bit one.
- The Z80 receives its own vblank feed on its `int` line.

The sources that share a bit pull the same open-collector line
exactly like the wired-OR interrupt pins of the real machine.

## 7. The Genesis window map

The 68000 bus:

| Range | Device |
|---|---|
| $000000-$3FFFFF | the cart |
| $A00000-$A03FFF | the sound RAM view |
| $A04000-$A05FFF | the YM2612 |
| $A10000-$A100FF | the IO die |
| $A11000-$A113FF | the arbiter trio |
| $C00000-$C0000F | the VDP |
| $C00010-$C0001F | the PSG |
| $E00000-$FFFFFF | the work RAM, mirrored on the $10000 stride |

The sound bus: the mapper's RAM/ROM form at $0000-$1FFF, the
YM2612 ports at $4000-$5FFF, the bank page at $6000-$7FFF, the
PSG's port window over $7F10-$7F1F, and the banked window at
$8000-$FFFF.

## 8. The 68000 core

The core runs one instruction per visit with per-access stamps. The
published cycle totals ride the access flow. The exception frame
pushes the 32-bit program counter first, then the 16-bit status
word (the short form), so the frame reads $SP+0 = status, $SP+2 =
the return address. RTE and RTR pull through the same form.

Illegal, privilege, and the unimplemented-line entries stack the
faulting instruction's own address. Trace, TRAP, TRAPV, CHK, the
divide entries, and the interrupt stack the advanced address. The
address error uses vector 3. The interrupt entry pays 44 clocks
with the internal acknowledge and installs the serviced level's
mask. The reset exception's vector fetch stays outside this build:
the machine's boot preset names the supervisor stack pointer and
the program counter.

## 9. The Genesis VDP

The VDP counts 3420 master clocks per scanline in both horizontal
modes (H32: 342 dots at /10. H40: 390 dots at /8 plus 30 at /10).
The control port runs the two-word command form with the pending
flag. Byte writes enter both lanes. The CRAM and VSRAM reads fold.
the VRAM byte read answers the odd-lane swap form. Register writes
cancel the pending form and clear the code register. The read-back
of registers rides the 11-prefix command and aborts on the second
word. The status word carries the published bits 0-9.

The DMA forms: the 68k transfer at the published per-line byte
rates for the CRAM and VSRAM word rates, the fill with the
high-byte form, and the copy. The fill and copy seize the 68000
through the hold line in place of the hardware port restriction.

The render walks the plane ladder with the window union clipping,
the column VSRAM pairs, the column-major tile walk, and the
raw-X-zero mask. The overflow caps and the shadow and highlight
operators follow the published forms.

## 10. The Neo Geo dies

The LSPC runs 384 pixels per line at /4 of the master over 264
(NTSC) or 312 (PAL) lines. The palette window and its mirrors span
$400000-$7FFFFF. The register page decodes on bits 1-3. The VRAM
address carries its own zone rule, the signed modulo walks
backwards, and a data write refills the read buffer. The sprite
walk follows the SCB1-SCB4 layout with the stick chain, the
96-entry per-line list with the zero tail, the chained strip's X
advance, and the L0-table vertical shrink. The C ROM decode
implements the published 128-byte tile interleave. The FIX layer
walks column-major from $7000. The palette chip answers the
selected bank's word forms. Byte lanes stay unmapped. The vblank
interrupt asserts at line 240 (NTSC) and clears through $3C000C.
the raster timer counts pixel clocks.

### The 68k IO die

The controllers read active-low with the
Up/Down/Left/Right/A/B/C/D bit order at the even lanes of
$300000-$31FFFF and $340000-$35FFFF. The watchdog arms at boot and
re-arms per odd-lane kick. $320000 carries the sound latch.
$320001 reads the AES status-A zero form. $380000 reads $7F idle.
The odd outputs $3A0001-$3A001F store their latch bytes: $3A001F
selects palette bank 1 (high), $3A000F selects bank 0, and
$3A0003/$3A0013 drive the BIOS-vector mux line.

### The sound die

The sound die reads its command latch at io port $00. The YM2610
owns $04-$07. The reply latch stores at $0C and crosses with a
one-tick delay. The NMI enable and disable writes share the $08
and $18 ports with the M1 bank reads through the per-direction
overlay. The M1 ROM's static bank reads $0000-$7FFF and its four
banked windows read at 2/4/8/16-KiB granularity.

## 11. The YM2610

The chip follows the landed OPNB form: four FM channels of four
operators at the published port split, the operator addressing, the
key-on channel codes, the high-register-first frequency latch, and
the special-mode pair map. The status 0 byte carries the busy and
both timer flags. The status 1 byte carries the ADPCM-B end bit and
the six ADPCM-A channel-end flags with the mask form of $1C. The
SSG block follows the YM2149 equations at 32 chip clocks per tick.
The ADPCM-A engine runs six channels at 18.5 kHz, and the ADPCM-B
engine runs the variable rate. A data write raises busy for 83
chip clocks. The FM sample lands every 144 chip clocks.

The `ym2610.chip` params name the two devices that carry the ADPCM
ROM bytes:

```toml
[device.params]
adpcma = "adpcma"
adpcmb = "adpcmb"
```

## 12. The input grammar

| Device | Events |
|---|---|
| `genesis.io` | `port <n> press <bits>` and `port <n> lift <bits>`, decimal over the six data bits |
| `neogeo.io` | `p1 press <bits>`, `p1 lift <bits>`, and the `p2` forms; bits 0-7 are Up, Down, Left, Right, A, B, C, D |

## 13. Quirks

The 68000 machines declare no config quirk flags in this build. The
published behaviors are device-internal: the open-collector IPL
decode, the arbiter handshake, and the exception frame form.

## 14. Boot states and the sound view

The 68000 view boots through the boot-state register preset naming
the supervisor stack pointer and the program counter. The cart's
vector longwords at $000000/$000004 are informational in bundled
machines. The Z80 view boots into the mapper's boot ROM form and
runs its stub.

The sound view's configuration puts the Z80's crystal under the
name that sorts first, so the run commands pace on the Z80. The
machine's devices and windows are the same as the 68000 view.

## 15. Known first-build approximations

- The debug-write and debug-read paths stamp accesses with the
  primary domain's tick. A machine-level test that writes a
  secondary-domain device after a long run arms past the run's
  end: issue debug writes before the run.
- The reset exception's vector fetch stays out of the 68000 build.
  the bundled boots preset the core directly.
- The arbiter's cross-domain wake overshoots. The handoff stays
  exact and byte-stable.
- The LSPC's vertical-shrink tables are generated from the
  published subsample semantics. The real $000-lo.lo data stays a
  ROM dump and is recorded as the LSPC evidence gap.
- The VDP's DMA budget uses the published per-line byte rates. The
  FIFO flags ride the queued forms.
- The YM2610's mono downmix averages the two pan sums (a
  single-pan channel renders at half level).
- The 68k IO die decodes its three port cells by the address's low
  bits, so even control addresses inside a port cell answer the
  data form. The pad data registers read correctly at the
  addresses real titles use.
- The V-ROM chip has no sprite-decode window: the LSPC's C-ROM
  interleave reads the C ROM directly, and the V ROM feeds only
  the YM2610's ADPCM engines. A per-cart banking pass is pending
  evidence.
- The data-source exemption of the fabric check is a broad
  heuristic: any device's string parameter naming another device
  clears the orphan rule.
- The M1 bank ports map four bytes at the io spaces 08-0B without
  the published mirror form.
- The 68000 instruction-suite runs, the register-image captures,
  the VDP golden screenshots from real titles, the BIOS checksum
  runs, and the bus-arbiter timing cross-check remain pending the
  follow-up evidence pass.

## 16. Debug notes for this family

- The bus-arbiter determinism: save a state mid-hold. Two machines
  restored from the same save continue byte-identically.
- `read` and `write` cross the borrowed-decoder bridges the same
  way the run does. The sound-bus banked window reaches the 68000
  bus.
- The arbiter's grant lands on the arbiter's next tick visit:
  `step 1` after the request write and re-read $A11100 for the
  grant bit.
- The watchdog re-arms per odd-lane kick. A parked core eventually
  hits the deadline. Re-arm through the kick strobe in your debug
  runs.