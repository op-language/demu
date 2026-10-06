# The config reference

This chapter is the quick field reference for demu machine
definitions. The complete grammar, validation rules, and as-built
notes live in `docs/dsl-specification.md`. The per-machine
explanations live in the machine chapters. The step-by-step
constructions live in `docs/machine-definitions-tutorial.md`.

## 1. The bundled configs

demu ships one canonical config per machine triple in `configs/`,
embedded in the binary. The `--config` selector accepts a triple
name or a file path:

```
demu --config sm83-nintendo-gameboy          # a bundled triple
demu --config /home/you/my-machine.toml      # a file
```

The bundled triples:

| Config file | Triple | Machine group |
|---|---|---|
| `smoke.toml` | `smoke` | the framework test machine |
| `sm83-nintendo-gameboy` | Game Boy DMG | SM83 |
| `sm83-nintendo-gameboy-color` | Game Boy Color | SM83 |
| `rp2A03-nintendo-nes-ntsc` | NES NTSC (the 2A03 view) | 6502 |
| `rp2A07-nintendo-nes-pal` | NES PAL (the 2A07 view) | 6502 |
| `mos6502-nintendo-nes-ntsc` | NES NTSC (the plain 6502 view) | 6502, extends the RP2A03 config |
| `mos6502-nintendo-nes-pal` | NES PAL (the plain 6502 view) | 6502, extends the RP2A07 config |
| `mos6502-atari-2600` | Atari 2600 | 6502 |
| `mos6502-atari-5200` | Atari 5200 | 6502 |
| `mos6502-atari-7800` | Atari 7800 | 6502 |
| `mos6502-atari-800-ntsc` | Atari 800 NTSC | 6502 |
| `mos6502-atari-800-pal` | Atari 800 PAL | 6502 |
| `mos6502-commodore-64` | Commodore 64 NTSC | 6502 |
| `mos6502-commodore-64-pal` | Commodore 64 PAL | 6502, extends the NTSC config |
| `mos6502-apple-ii` | Apple II base | 6502 |
| `mos6502-apple-iic` | Apple IIc | 6502, extends the base |
| `mos6502-apple-iie` | Apple IIe | 6502, extends the base |
| `mos6502-apple-iie-enhanced` | Apple IIe enhanced | 6502, extends the base |
| `wdc65c816-nintendo-snes` | SNES | 65C816 |
| `wdc65c816-apple-iigs` | Apple IIgs | 65C816 |
| `vl65nc02-atari-lynx` | Atari Lynx | 65SC02 |
| `mos6502-nec-pcengine` | PC Engine | 6502 (carries the HuC6280 kind) |
| `z80-sega-sg1000` | SG-1000 | Z80 |
| `z80-sega-mastersystem` | Master System | Z80 |
| `z80-sega-gamegear` | Game Gear | Z80 |
| `z80-sinclair-zx80` | ZX80 | Z80 |
| `z80-sinclair-zx81` | ZX81 | Z80 |
| `z80-sinclair-spectrum` | ZX Spectrum 48K | Z80 |
| `z80-sinclair-spectrum128` | ZX Spectrum 128K | Z80, extends the 48K |
| `z80-ti-85` | TI-85 | Z80 |
| `m68000-sega-genesis` | Mega Drive (the 68000 view) | 68000 |
| `z80-sega-genesis` | Mega Drive (the sound view) | 68000 machine, Z80 primary |
| `m68000-neogeo-aes` | Neo Geo AES (the 68000 view) | 68000 |
| `z80-neogeo-aes` | Neo Geo AES (the sound view) | 68000 machine, Z80 primary |

Validate any file or bundled triple:

```
demu config --check configs/z80-ti-85.toml
```

## 2. The section map

| Section | Purpose | Chapter of the specification |
|---|---:|---|
| `[system]` | `name`, `triple`, `description` | 2 |
| `[extends]` | the base config, `base` | 3 |
| `[clock.<name>]` | `frequency`, `origin` | 4 |
| `[[device]]` | `name`, `kind`, `domain`, `params`, `dividers` | 5 |
| `[[bus]]` | `name`, `address_bits`, `owner`, `parent` | 6 |
| `[[window]]` | `name`, `bus`, `device`, `base`, `size`, `kind`, mirrors, overlays, `conflict` | 7 |
| `[[connection]]` | `from`, `to`, `semantics`, `direction` | 8 |
| `[[signal]]` | `name`, `lines`, `open_collector` | 9 |
| `[boot.<device>]` | `rom`, `pattern`, `hex`, `registers` | 10 |
| `[quirks]` | quirk flags | 11 |

The specification carries the full field tables with types,
defaults, and constraints. The kind table of the specification
(section 13.3) lists the params and dividers per kind.

## 3. The per-machine clock table

| Machines | Domain names | Frequencies |
|---|---|---|
| Game Boy, Game Boy Color | `main` | 4194304 |
| NES NTSC | `main` | `236250000/11` |
| NES PAL | `main` | `53203425/2` |
| Atari 2600 | `main` | `315000000/88` |
| Atari 5200, 7800, 800 NTSC, C64 NTSC, Apple II | `main` | `315000000/44` |
| Atari 800 PAL | `main` | `7093790/2` |
| C64 PAL | `main` | `17734475/18` |
| SNES | `main`, `sound` | `236250000/11`, 1024000 |
| Apple IIgs | `main` | 28636360 |
| Atari Lynx | `main` | 16000000 |
| PC Engine | `main` | 21477270 |
| Sega 8-bit machines | `main` | 10738635 |
| ZX80, ZX81 | `main` | 3250000 |
| Spectrum 48K | `main` | 3500000 |
| Spectrum 128K | `main` | 3546895 |
| TI-85 | `main` | 6000000 |
| Genesis | `m68k`, `z80` (the sound view: `zmain`, `sound`) | 53693175 and `53693175/15` |
| Neo Geo AES | `m68k`, `z80` (the 68000 view; the sound view: `z80`, `zmain`) | 24000000 and `24000000/6` |

The primary domain is the first clock name in name-sorted order.
The sound-view configs put the sound domain first for that purpose.

## 4. The per-machine quirk table

| Config | Quirk keys (values the bundled configs set) |
|---|---|
| `sm83-nintendo-gameboy` | `sm83.oam_bug = true`, `sm83.wave_ram_undoc = true`, `sm83.halt_undoc = true` |
| `sm83-nintendo-gameboy-color` | `sm83.oam_bug = false`, the other two true |
| `rp2A03-nintendo-nes-ntsc` | `rp2A03.undocumented = true` |
| `rp2A07-nintendo-nes-pal` | `rp2A07.undocumented = true` |
| `mos6502-nintendo-nes-ntsc` | `mos6502.undocumented = true` |
| `mos6502-nintendo-nes-pal` | `mos6502.undocumented = true` |
| Atari consoles and home machines | `mos6502.undocumented = true` |
| `mos6502-nec-pcengine` | none (the HuC6280 die carries the quirks internally) |
| `wdc65c816-nintendo-snes` | `apu.port_race_or` (declared, value false) |
| `vl65nc02-atari-lynx` | `vl65nc02.bbr_smb = false` (explicit) |
| Sega 8-bit machines | the Game Gear sets `sega.gg_start = true`. The three Sega configs set no `z80` quirk. |
| Sinclair machines | `zx.zx81_slow = true` on the ZX81 |
| The dual-CPU machines | none |

Defaults and the hardware motivation of every flag live in the
machine chapters.

## 5. Window field summary

Every window needs `name`, `bus`, `device`, `base`, `size`. The
optional fields:

| Field | Form | Use |
|---|---|---|
| `kind` | `"rom"`, `"ram"`, `"chr"` | the memory-kind annotation |
| `mirror_stride`, `mirror_count` | a number string and an integer | the repeat chains |
| `overlay`, `overlay_read_priority`, `overlay_write_priority` | a group name and integers | the overlapping windows |
| `conflict` | `"first"`, `"and"`, `"last-wins"` | the multi-answer policy |

## 6. Boot state forms

```toml
[boot.cart]
rom = "demo.gb"                  # the file form

[boot.cpu0]
registers = { pc = "0x0000" }    # the CPU preset

[boot.wram]
pattern = "zero"                 # or "random(seed)"
```

The positional ROM argument of the CLI overrides the cart device's
boot state. The untagged parser matches `rom`, then `pattern`, then
`hex`, then `registers`.

## 7. Connection semantics values

| Value | Meaning |
|---|---|
| `irq` | an interrupt request edge |
| `dma-request` | a DMA handoff, usually the ready-line halt |
| `display-sync` | a frame or scanline boundary signal |
| `async-port` | a free-running port pair; the only value that may join two clock domains |

## 8. The validation loop

```
demu config --check <file>
```

Four layers: schema, device graph, semantic, lint. Exit 0 with the
`ok:` summary on success and warnings printed. Exit 2 with every
error listed otherwise. The tutorial carries the iterate loop.