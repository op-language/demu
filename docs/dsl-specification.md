# The demu machine-definition DSL specification

This document is the complete specification of the demu system
configuration DSL. demu constructs every emulated machine at startup
from a TOML document of this language. The document gives the
grammar, every section and field, the validation rules, the
`extends` semantics, the kind registry contract, and the embedded
expression language.

The words MUST, MUST NOT, SHALL, SHALL NOT, and MAY are RFC 2119
words. The prose follows ASD-STE100 (strict). The technical design
(`docs/technical-design.md`) specifies the runtime model behind the
language and is the arbiter for behavior questions.

## 1. Document structure

A demu machine definition is one TOML document. The top-level keys:

| Section | Shape | Required | Purpose |
|---|---|---|---|
| `[system]` | table | yes | the machine identity |
| `[extends]` | table | no | the base configuration |
| `[clock.<name>]` | table per domain | at least one | a clock domain |
| `[[device]]` | array of tables | at least one | a device instance |
| `[[bus]]` | array of tables | at least one | a bus |
| `[[window]]` | array of tables | no | an address window |
| `[[connection]]` | array of tables | no | a signal connection |
| `[[signal]]` | array of tables | no | a line group declaration |
| `[boot.<device>]` | table per device | no | a device boot state |
| `[quirks]` | table | no | quirk flags |

Rules:

- Unknown top-level keys and tables are ignored. An unknown device
  kind, an unknown quirk family or name, and unknown clock domains in
  device sections are validation errors.
- The value types are TOML types. Number fields that accept hex take
  a string with a decimal or `0x` form; the parser accepts `"0x1FA"`
  and `"506"`.
- Every section but `[system]` is optional in the document shape.
  Validation enforces the presence of at least one clock, one
  device, and one bus (section 12).
- Configs select and parameterize compiled behavior. A config cannot
  invent device behavior. The kind registry holds every behavior.

The pipeline order: the parser reads the raw document, the `extends`
resolver merges the chain, and the validator runs four layers. A
`SystemConfig` value exists only when every layer accepts.

## 2. The `[system]` section

| Field | Type | Required | Constraint |
|---|---|---|---|
| `name` | string | yes | non-empty after trimming |
| `triple` | string | yes | non-empty after trimming |
| `description` | string | no | default `""` |

The `triple` is the machine identity. The semantic validation layer
compares the CPU family against the triple prefix (section 12). The
triple names a bundled config to the `--config` selector.

```toml
[system]
name = "Nintendo Game Boy"
triple = "sm83-nintendo-gameboy"
description = "The 4.19 MHz monochrome Game Boy with the DMG boot ROM."
```

## 3. The `[extends]` section

A config may extend a base configuration. The base is one bundled
triple name or one file path.

```toml
[extends]
base = "rp2A03-nintendo-nes-ntsc"
```

### 3.1 Base resolution

- An exact match against the bundled table resolves to the embedded
  config.
- Any other value is a file path read from disk. A missing file is
  the `config not found` error.
- Bases resolve recursively: a base may itself extend another config,
  up to the depth limit of 32 levels. A repeated key in the chain,
  or a chain beyond the depth limit, is the `ExtendsCycle` error with
  the chain in the message.

### 3.2 Merge rules

The merged result is the root document with every ancestor below it.
The child wins on every per-name merge. The rules:

| Section | Merge rule |
|---|---|
| `[system]` | the child replaces the whole table when present; omitted keeps the parent |
| `clock.<name>` | per-name replace or insert |
| `[[device]]` | per-name replace of the whole device entry, else append |
| `[[bus]]` | per-name replace, else append |
| `[[window]]` | per-name replace, else append |
| `[[signal]]` | per-name replace, else append |
| `[[connection]]` | the child list replaces the parent list as a whole; a partial connection list leaves the wiring ambiguous |
| `[boot.<device>]` | per-device key replace; other keys survive |
| `[quirks]` | per-key replace or insert |
| `[extends]` | always cleared in the merged result |

A device override replaces the full device entry: kind, domain,
params, and dividers all come from the child entry. A config that
changes only a device's kind must restate every field it needs.

### 3.3 Re-validation

The resolver does not validate by itself. Every caller runs all four
validation layers on the merged document. An alias config that
invalidates the merged graph fails with the merge in place.

## 4. Clock domains

Each `[clock.<name>]` declares one clock domain. The name is the
domain key. One entry per crystal:

```toml
[clock.main]
frequency = "236250000/11"
origin = "NTSC master crystal; the CPU cycle is 12 ticks"

[clock.sound]
frequency = "1024000"
```

| Field | Type | Required | Constraint |
|---|---|---|---|
| `frequency` | string | yes | `"<num>/<den>"` or a bare integer; reduced by GCD; `num > 0` and `den > 0` required |
| `origin` | string | no | free-text provenance, default `""` |

The frequency type is an exact rational. Zero parts are the
`FrequencyZero` error. The domain tick counter counts crystal
periods. The scheduler treats the first clock name in name-sorted
order as the primary domain: the domain that `step`, `run`, and the
media pipeline pace on. Declare the machine's pacing domain with a
name that sorts ahead, or accept the default.

## 5. Devices

Each `[[device]]` entry declares one device instance:

```toml
[[device]]
name = "cpu0"
kind = "sm83.cpu"
domain = "main"
[device.dividers]
cpu = 1

[[device]]
name = "ppu"
kind = "sm83.ppu"
domain = "main"
[device.params]
cgb = false
[device.dividers]
dot = 1
```

| Field | Type | Required | Constraint |
|---|---|---|---|
| `name` | string | yes | the device-name grammar: starts with an ASCII letter or `_`, then ASCII letters, digits, `_`, `-` |
| `kind` | string | yes | the kind-name grammar `<family>.<kind>`, both parts non-empty ASCII letters, digits, `_`, `-`; must be a registered kind |
| `domain` | string | yes for every registered kind | must name a declared clock domain |
| `params` | table | no | kind-specific; read by the kind at assembly time |
| `dividers` | table of integers | no | the named integer dividers the device publishes |

Notes:

- Device names are unique. A duplicate name is an error.
- Every registered kind requires a domain. An empty domain is an
  error at both the schema and the graph layer.
- `params` is free-form TOML. Validation does not read it. The kind
  constructor reads it at assembly time. A malformed or missing
  required param becomes an assembly error at machine construction,
  not a config validation error. The error text names the kind.
- `dividers` values are positive integers. The per-kind tables in
  section 11 name the dividers each kind reads and its default.
  Dividers of zero are not rejected globally. The kinds guard or
  tolerate them per device.
- A string param MAY name another device. Referenced devices deliver
  their boot image bytes to the referencing kind, for example the
  `srom`, `crom`, `adpcma`, `adpcmb`, and `bios` params of the Neo Geo
  kinds. A device that appears only in a `params` string satisfies
  the on-bus rule of the semantic layer.

## 6. Buses

Each `[[bus]]` declares one bus and its decoder:

```toml
[[bus]]
name = "cpu"
address_bits = 24
owner = "cpu0"

[[bus]]
name = "io"
address_bits = 16
```

| Field | Type | Required | Constraint |
|---|---|---|---|
| `name` | string | yes | |
| `address_bits` | integer | no | default 16; windows must fit `2^bits - 1` |
| `owner` | string | no | must name a declared device; feeds the on-bus rule of layer 3 |
| `parent` | string | no | must name another declared bus; not the same bus |

The `parent` field is validated and carried into the machine. As
built, the runtime borrowed-decoder route is a per-device decision
inside device kinds, not a config-level route. See the as-built
notes (section 14).

The first declared bus is the default bus for `read`, `read-n`,
`write`, `dump`, and `fill`, and the machine's primary decoded bus.
As built, the `owner` value does not join the runtime bus value. The
on-bus rule of the semantic layer consumes it.

## 7. Windows

Each `[[window]]` maps one address range of one bus to one device:

```toml
[[window]]
name = "w_ppu"
bus = "cpu"
device = "ppu"
base = "0x2100"
size = "0x0040"

[[window]]
name = "w_cart_rom_low"
bus = "cpu"
device = "cart"
base = "0x008000"
size = "0x008000"
mirror_stride = "0x010000"
mirror_count = 63
```

| Field | Type | Required | Constraint |
|---|---|---|---|
| `name` | string | yes | the merge key for extends |
| `bus` | string | yes | must name a declared bus |
| `device` | string | yes | must name a declared device |
| `base` | string | yes | a number: decimal or `0x` hex |
| `size` | string | yes | a number: decimal or `0x` hex |
| `kind` | string | no | `"rom"`, `"ram"`, or `"chr"`; mirrors the `op-target` memory kinds |
| `mirror_stride` | string | no | a number; pairs with `mirror_count` |
| `mirror_count` | integer | no | pairs with `mirror_stride` |
| `overlay` | string | no | the overlay group name |
| `overlay_read_priority` | integer | no | default 0 |
| `overlay_write_priority` | integer | no | default 0 |
| `conflict` | string | no | `"first"`, `"and"`, or `"last-wins"`; default `"first"` |

Rules:

- `base` and `size` are numbers in string form. The size must stay
  inside the bus: `base + size` within `2^address_bits`.
- A mirror repeats the window: repeat `k` for `k` in `1..count`
  covers `base + k*stride` carrying the first `size` bytes. The
  primary window is repeat 0. A stride of zero never mirrors. A
  mirror layout that overflows the address space is an error.
- `mirror_stride` requires `mirror_count` and the reverse.
- Two windows on one bus that overlap without an `overlay` group are
  the `WindowOverlap` error. Windows in one overlay group may
  overlap. The priorities decide read and write resolution, and the
  higher number wins. Equal priorities fall to declaration order,
  and the first declared window wins.
- The `conflict` policy declares what happens when two devices
  answer one window: `"first"` serves the first matching window,
  `"last-wins"` the last, and `"and"` requires all answerers. As
  built, the `conflict` field validates and stores. No runtime
  consumer reads it in this build.
- Window names are not checked for uniqueness across the document,
  but the name drives the extends merge, so unique names matter for
  alias configs.
- The `kind` field is validated and stored. As built, the runtime
  does not consume it. The address decode comes from the window
  list. See the as-built notes (section 14).

## 8. Connections

Each `[[connection]]` declares one signal route between two device
endpoints:

```toml
[[connection]]
from = "ppu.vblank"
to = "cpu0.vblank"
semantics = "irq"
```

| Field | Type | Required | Constraint |
|---|---|---|---|
| `from` | string | yes | a line path `device.line`; both parts follow the device-name grammar |
| `to` | string | yes | a line path `device.line` |
| `semantics` | string | yes | `"irq"`, `"dma-request"`, `"display-sync"`, or `"async-port"` |
| `direction` | string | no | `"forward"` (default) or `"bidir"` |

Rules:

- An endpoint cannot connect to itself.
- An endpoint's device must be declared.
- A connection between two different clock domains is legal only
  with `async-port` semantics. Cross-domain `irq`, `dma-request`, and
  `display-sync` connections are errors.
- `bidir` routes a second reversed connection along with the first.
- As built, the semantics value carries the cross-domain gate and
  documentation duty. The routed-edge deliverance to devices is the
  same for every semantics value. The scheduler delivers the line
  edge to the receiving device at the routing time. Read the
  machine chapter for the per-machine meaning of each connection.

## 9. Signal groups

Each `[[signal]]` entry declares a named group of line paths, for
documentation and validation of the line wiring:

```toml
[[signal]]
name = "ipl"
lines = ["vdp.ipl1", "vdp.ipl2", "sys.ipl1", "ym.timer"]
open_collector = true
```

| Field | Type | Required | Constraint |
|---|---|---|---|
| `name` | string | yes | duplicate group names are a lint warning |
| `lines` | array of strings | yes | at least one `device.line` path; line paths parse with the endpoint grammar |
| `open_collector` | boolean | no | default false |

As built, signal declarations validate and store, and the runtime
open-collector behavior lives in the device kinds that share a CPU
interrupt line (for example the 68000 IPL decode over line levels).
Consult the machine chapter for the per-machine wiring facts.

## 10. Boot states

Each `[boot.<device>]` entry declares the boot state of one device.
The key names a declared device. The state table is untagged. The
parser matches the variants in this order: `rom`, then `pattern`,
then `hex`, then `registers`.

| Variant | Fields | Meaning |
|---|---|---|
| ROM file | `rom = "<path>"` | the device loads the file contents as its image |
| RAM pattern | `pattern = "zero"` or `pattern = "random(<seed>)"` | zero-filled, or filled by the deterministic seeded generator |
| Hex dump | `hex = "<string>"` | the raw bytes of the hex digits, left to right; whitespace allowed |
| CPU preset | `registers = { <name> = "<num>" }` | register values at boot; decimal or `0x` hex |

```toml
[boot.bootrom]
hex = """
    31FEFFAF21FF9F32CB7C...
"""

[boot.cpu0]
registers = { pc = "0x0000" }

[boot.cart]
rom = "demo.gb"
```

Rules:

- The `random(<seed>)` pattern fills the device with the
  deterministic SplitMix64 sequence over little-endian `u64`
  chunks. The same configuration boots identically everywhere.
- The hex dump needs an even digit count and hex digits only.
- A CPU applies the register presets through its register setter.
  Unknown register names are skipped silently. The register name
  list is per CPU family (machine chapters).
- An unknown device key in the boot table is an error.
- The validation does not check the ROM file size against the
  device capacity in this build. A flat static-memory device
  truncates a large image at the capacity. A boot-rom device whose
  buffer starts empty loads nothing from a `rom` path. See the
  as-built notes (section 14).
- The positional ROM argument of the CLI overrides the cart
  device's boot state with a ROM file state.

## 11. Quirks

The `[quirks]` table sets named quirk flags:

```toml
[quirks]
"sm83.oam_bug" = true
"sm83.wave_ram_undoc" = true
"sm83.halt_undoc" = true
```

Rules:

- A quirk key has the grammar `<family>.<quirk>`.
- The family must be the family of some selected device kind. A
  quirk for a family with no device in the config is an error.
- The quirk name must appear in the kind registry's quirk list of
  that family. An unknown name is an error.
- A quirk value is a `bool`, an integer, or a string. Other types
  are errors.
- Consumers read a quirk as a flag where a boolean applies: a bool
  is itself, an integer is its non-zeroness, and a string is false.
- The quirk lists per kind live in section 11.1. The machine
  chapters state the defaults and the hardware motivation of each
  flag used by the bundled configs.

### 11.1 The declared quirk flags

| Family | Quirk | Kind(s) | Behavior |
|---|---|---|---|
| `sm83` | `oam_bug` | `sm83.cpu` | the OAM corruption race during the DMA window; true on the DMG config |
| `sm83` | `wave_ram_undoc` | `sm83.cpu` | the wave RAM corruption rule; the audio device applies the rule unconditionally in this build |
| `sm83` | `halt_undoc` | `sm83.cpu` | the HALT dispatch edge case |
| `rp2A03` | `undocumented` | `rp2A03.cpu` | the NMOS undocumented opcodes |
| `rp2A07` | `undocumented` | `rp2A07.cpu` | the PAL form |
| `mos6502` | `undocumented` | `mos6502.cpu` | the plain 6502 form |
| `z80` | `undocumented` | `z80.cpu` | the index halves, the SLL shift, the `IN/OUT (C),0` forms |
| `apu` | `port_race_or` | `apu.core` | the SNES audio-port read/write race returns the OR of the old and new bytes |
| `vl65nc02` | `bbr_smb` | `vl65nc02.cpu` | the page-bit instruction set of the plain CMOS core in the VTI die; the Lynx machine leaves it off |
| `mos65sc02` | `bbr_smb` | `mos65sc02.cpu` | the same page-bit set on the plain core; on by default in the core, the quirk forces them |
| `sega` | `gg_start` | `sega.io` | the Game Gear START-key staged behavior on port $00 |
| `zx` | `zx81_slow` | `zx.ula` | the ZX81 slow-mode repaint flow |
| `smoke` | `trace` | `smoke.cpu` | a declared flag of the test framework machine |

## 12. Validation

Validation runs four layers. Errors stop the pipeline and the config
never resolves. Lint warnings do not stop the pipeline.

`demu config --check <file|triple>` runs all layers standalone. It
prints warnings as `warning: {code} - {message}` and the summary
`ok: {name} ({triple}) devices={n} clocks={n}` on success, with exit
0. A failure prints all errors and warnings and exits 2.

### 12.1 Layer 1: schema

The layer checks document shape and value forms:

1. `[system]` exists with a non-empty `name` and `triple`.
2. At least one clock, one device, and one bus.
3. Every clock `frequency` parses as a non-zero rational.
4. Every device entry has a `name`, a `kind`, and a `domain`.
5. Every window `base` and `size` parses as a number. The mirror
   pair is complete. The mirror layout does not overflow the
   address space.

### 12.2 Layer 2: device graph

The layer checks cross-section references:

1. Device names follow the grammar and are unique.
2. Kind keys follow the `<family>.<kind>` grammar and exist in the
   kind registry.
3. Device domains name declared clocks. Registered kinds require a
   domain.
4. Bus `owner` values name devices. Bus `parent` values name other
   buses. No bus is its own parent.
5. Window `bus` and `device` values name a bus and a device.
6. Connection endpoints parse as line paths. No endpoint connects
   to itself. The endpoint devices exist. Cross-domain connections
   use `async-port`.
7. Signal line paths parse.
8. Boot keys name declared devices. Variant values parse (the
   pattern grammar, the hex digit forms, the register numbers).
9. Quirk values have the allowed types. Quirk keys match a selected
   family and a registered quirk name.
10. Window ranges are 32-bit numbers.

### 12.3 Layer 3: semantic

The layer checks machine coherence:

1. The CPU-family rule: a device kind ending in `.cpu` must carry
   the family of the triple prefix, the part before the first dash.
   The comparison is ASCII case-insensitive. A dash-less triple is
   unconstrained. Exceptions, exact: the
   `mos6502-nec-pcengine` triple additionally accepts the
   `huc6280` family, because the triple names the plain 6502 family
   but the machine carries the real chip. The four dual-CPU triples
   (`m68000-sega-genesis`, `z80-sega-genesis`,
   `m68000-neogeo-aes`, `z80-neogeo-aes`) additionally accept both
   the `m68000` and the `z80` families.
2. Every device must answer somewhere: own a window on a bus, own a
   bus, or appear in another device's `params` string (the data
   chips). Otherwise the `DeviceOffBus` error.
3. Every window fits the bus address width.
4. Overlapping windows without one `overlay` group are the
   `WindowOverlap` error.

### 12.4 Layer 4: lint

Warnings only:

| Code | Message rule |
|---|---|
| `unused_clock` | a clock domain with no devices |
| `orphan_device` | a device with no window, no bus ownership, and no params reference. Unreachable in a run that passed layer 3, because the same condition is the blocking `DeviceOffBus` error |
| `duplicate_label` | a signal group name declared twice |

### 12.5 Error variants

| Variant | Meaning |
|---|---|
| `TomlParse` | the document does not parse |
| `Schema` | a shape or range rule, with the field path |
| `InvalidValue` | a value does not convert (pattern, hex, register, window width, quirk value type) |
| `FrequencyZero` | a clock frequency with a zero part |
| `InvalidKindName` | a kind key that violates the grammar |
| `InvalidDeviceName` | a device name that violates the grammar |
| `InvalidLinePath` | an endpoint that does not split at the dot |
| `UnknownReference` | a name that resolves to nothing, with the reference kind |
| `DuplicateDevice` | two devices with one name |
| `DeviceOffBus` | a device with no decode surface |
| `InvalidConnection` | a self-connection, a bad cross-domain rule, or a self-parent bus |
| `ExtendsCycle` | an extend chain that repeats or passes the depth limit |
| `NotFound` | a base or config file that does not exist |
| `BootRomNotFound`, `BootRomSize` | declared variants, not produced in this build |
| `Semantic` | a semantic-layer rule message |
| `WindowOverlap` | two same-bus windows that overlap without one overlay group |

## 13. The kind registry contract

Device behavior lives in compiled kinds. Two registries serve the
machine pipeline. A config only selects through the validation-time
registry.

### 13.1 The validation-time registry

The validation registry lives in `demu-config` as one static table.
An entry carries:

- the `kind` key with the exact string, for example `sm83.ppu`,
- the domain requirement (every kind today requires a clock domain),
- the quirk name list of the kind's family.

A registration site compiles its key. A key that violates the
grammar aborts the program at startup. A bad key is a programmer
error, not a config error. Validation rejects a config kind that is
not in the registry, and a quirk that is not in the selected kind's
family list.

### 13.2 The assembly-time constructor registry

The machine assembler looks up one constructor function per kind.
The demu binary registers the constructors of the real machine
families. The framework tests register the `smoke` family's
constructors. A kind that validates but has no constructor for the
running binary is an assembly error with the kind named.

### 13.3 The full kind table

Every registered kind, with its constructor parameters, dividers,
and quirks. `S`, `I`, `B` mean string, integer, and boolean param
types. Defaults apply when the param is absent. A `bus` param
defaults to the first declared bus, else the fixed fallback the row
names, unless the row states a literal default. The machine
chapters explain each kind's role in its machine.

| Kind | Params (types, defaults) | Dividers (name, default) | Quirks |
|---|---|---|---|
| `sm83.cpu` | `bus` S (first bus, else `main`) | — | `oam_bug`, `wave_ram_undoc`, `halt_undoc` |
| `sm83.wram` | `cgb` B (false) | — | — |
| `sm83.hram` | `size` I (0x7F) | — | — |
| `sm83.bootrom` | `cgb` B (false) | — | — |
| `sm83.cart` | `mbc` S (mapper override: values `rom`, `none`, `mbc1`, `mbc2`, `mbc3`, `mbc5`; any other value is ignored), `battery` B (false) | — | — |
| `sm83.dma` | — | — | — |
| `sm83.hdma` | — | — | — |
| `sm83.timer` | `dmg` B (true) | — | — |
| `sm83.joypad` | — | — | — |
| `sm83.serial` | — | — | — |
| `sm83.io` | `cgb` B (false) | — | — |
| `sm83.ppu` | `cgb` B (false) | — | — |
| `sm83.apu` | `cgb` B (false) | — | — |
| `rp2A03.cpu`, `rp2A07.cpu`, `mos6502.cpu` | `bus` S, `pal` B (false; forced true for `rp2A07.cpu`) | `cpu` (12 NTSC, 16 PAL) | `undocumented` |
| `rp2A03.wram`, `rp2A07.wram`, `mos6502.wram` | `cgb` B (false) | — | — |
| `rp2A03.cart`, `rp2A07.cart`, `mos6502.cart` | `pal` B (false; forced true for the `rp2A07` kind prefix) | — | — |
| `rp2A03.dma`, `rp2A07.dma`, `mos6502.dma` | `bus` S (`cpu`) | `cpu` (12) | — |
| `rp2A03.joypad`, `rp2A07.joypad`, `mos6502.joypad` | — | — | — |
| `rp2A03.ppu`, `rp2A07.ppu`, `mos6502.ppu` | `pal` B, `pattern_bus` S (`ppu`) | — | — |
| `rp2A03.apu`, `rp2A07.apu`, `mos6502.apu` | `pal` B, `bus` S (`cpu`) | `cpu` (12/16) | — |
| `atari.tia` | `video` B (true; false makes the 7800 audio-and-IO slice) | `dot` (1) | — |
| `atari.antic` | `fetch_bus` S (`cpu`), `pal` B (false) | `dot` (2) | — |
| `atari.pokey` | — | `cpu` (1) | — |
| `atari.maria` | `fetch_bus` S (`cpu`) | `dot` (1) | — |
| `atari.riot` | — | `cpu` (1) | — |
| `atari.pia` | — | `cpu` (1) | — |
| `atari.ram`, `atari.rom` | `size` I required; `readonly` B (false; the `*.rom` kinds set it at registration) | — | — |
| `atari.cart2600` | — | — | — |
| `atari.cart5200` | — | — | — |
| `atari.cart7800` | — | — | — |
| `c64.vic` | `pal` B (false), `lines` I (per region; above the minimum), `fetch_bus` S (`cpu`), `fetch_base` I (0) | `line` (65) | — |
| `c64.sid` | `pal` B (false) | `cpu` (1) | — |
| `c64.cia` | `pal` B (false), `tod_div` I (per region; above zero), `nmi` B (false; true on the CIA2 entry that presses the NMI line) | `cpu` (1) | — |
| `a2.video` | — | `dot` (2) | — |
| `a2.io` | — | `cpu` (1) | — |
| `a2.disk2` | — | — | — |
| `wdc65c816.cpu` | `bus` S | `cpu` (6) | — |
| `snes.ppu` | `pal` B (false) | `dot` (4) | — |
| `snes.sys` | `pal` B (false), `bus` S (`cpu`) | `cpu` (6) | — |
| `snes.wram` | — | — | — |
| `snes.pad` | — | — | — |
| `snes.cart` | `form` S (`lorom`, or `hirom`), `sram` I (0), `sram_base_bank` I (0x30) | — | — |
| `apu.core` | — | `cpu` (1) | `port_race_or` |
| `iigs.doc` | — | `cpu` (1) | — |
| `iigs.mega2` | — | `cpu` (1) | — |
| `iigs.iwm` | — | — | — |
| `vl65nc02.cpu` | `bus` S | `cpu` (4) | `bbr_smb` |
| `mos65sc02.cpu` | `bus` S | `cpu` (4) | `bbr_smb` |
| `huc6280.cpu` | `bus` S | `cpu` (3) | — |
| `lynx.suzy` | `bus` S | `dot` (1) | — |
| `lynx.mikey` | — | `dot` (8), `us` (16) | — |
| `lynx.mapctl` | — | — | — |
| `lynx.ram` | `size` I required | — | — |
| `lynx.bootrom` | — | — | — |
| `lynx.cart` | `form` S (`raw`, or `lnx`), `page_size` I (0x100), `audin_cart` B (false) | — | — |
| `pce.vdc` | `bus` S, `palette_base` I (0x0500) | `dot` (4) | — |
| `pce.vce` | — | — | — |
| `pce.psg` | — | `psg` (40) | — |
| `pce.joypad` | `japan` B (false), `cd_addon` B (false) | — | — |
| `pce.ram` | `size` I required | — | — |
| `pce.cart` | `header` I (0) | — | — |
| `z80.cpu` | `bus` S, `io_bus` S, `contended` S (hex range list), `io_contend` S, `contend_from` I, `contend_to` I, `contend_base` I (0) | `cpu` (3) | `undocumented` |
| `z80.ram`, `z80.rom` | `size` I required | — | — |
| `z80.bankrom` | `size` I (0x8000; even, at least 2) | — | — |
| `zx.bankram` | — | — | — |
| `tms9918.vdp`, `sms.vdp`, `gg.vdp` | `extended_height` B (false) | `dot` (2) | — |
| `sn76489.psg` | — | `psg` (48) | — |
| `sega.io` | `form` S (`sg1000`, `sms`, or `gg`; required) | — | `gg_start` |
| `sega.cart` | `form` S (`sms`, or `sg1000`, `sega`, `codemasters`) | — | — |
| `zx.ula` | `form` S (`spectrum`, `zx80`, or `zx81`; required), `line_t` I (224), `frame_lines` I (312), `int_lines` I (1), `shadow_ram` B (false) | — | (`zx81_slow` on the zx81 form) |
| `ti85.mem` | — | — | — |
| `ti85.lcd` | — | — | — |
| `ti85.link` | — | — | — |
| `m68000.cpu` | `bus` S (`cpu`) | `cpu` (1) | — |
| `genesis.vdp` | `region` S (`ntsc`, or `pal`), `bus` S (`cpu`) | `dot` (1) | — |
| `genesis.arbiter` | — | — | — |
| `genesis.io` | — | — | — |
| `genesis.mapper` | `main_bus` S (`cpu`) | — | — |
| `genesis.cart` | `sram` I (0) | — | — |
| `ym2612.psg` | — | `psg` (144) | — |
| `ym2610.chip` | `adpcma` S (`adpcma`), `adpcmb` S (`adpcmb`) | `div` (144) | — |
| `neogeo.lspc` | `srom` S (`srom`), `crom` S (`crom`), `region` S, `bus` S | `dot` (2) | — |
| `neogeo.palette` | — | — | — |
| `neogeo.cart` | `bios` S (`bios`) | — | — |
| `neogeo.io` | `sound_bus` S (`sound`) | `master` (2) | — |
| `neogeo.soundio` | `main_bus` S (`cpu`) | — | — |
| `neogeo.m1rom` | — | — | — |
| `neogeo.rom` | `size` I required | — | — |
| `smoke.cpu` | — | `cpu` (1) | `trace` |
| `smoke.ram` | `size` I (0x1000) | — | — |
| `smoke.rom` | `size` I (0x10000) | — | — |

## 14. As-built notes for the DSL

The build records its boundaries here. The machine chapters carry
the per-machine records.

- The validation registry is one static table in `demu-config`. The
  device-family crates do not run registration code. The design
  phrase "each crate registers its kinds" reads here: the
  registration is centralized, and constructors register in the
  binary's assembly module.
- The assembled machine stores one boxed device value per declared
  device. The per-family enums exist inside the CPU crates as value
  types. The machine assembly does not switch on them.
- The `[[bus]]` `parent` field validates but does not drive the
  runtime decoder. The borrowed-decoder route is a per-device
  `borrow` decision, and a config declares nothing about it.
- The `[[signal]]` sections and the `open_collector` field validate
  and store. The runtime open-collector behavior is device-internal.
- The window `kind` field (`rom`, `ram`, `chr`) validates and
  stores. No runtime consumer reads it in this build.
- The connection `semantics` value carries the cross-domain gate.
  The routed-edge delivery is identical for every semantics value.
- The first clock name in name-sorted order is the primary domain. The
  the declaration order of `[clock.*]` tables does not matter.
- Boot ROM files: a flat static-memory device truncates a large
  image at the device capacity. A boot-rom device whose buffer
  starts empty (the `sm83.bootrom`, the `lynx.bootrom`, the smoke
  ROM) loads nothing from a `rom` path in this build, and the
  loader call sites drop their errors. The `BootRomNotFound` and
  `BootRomSize` variants exist but no code produces them.
- An unknown register name in a CPU boot preset is skipped
  silently. The machine chapter names the known registers.
- The demo binary registers no `smoke.*` constructors, so a session
  with the default `--config smoke` reports an assembly error. The
  framework tests use the smoke constructors directly. The demo
  binary also registers only `mos6502.cpu` and `mos6502.cart` of
  the six `mos6502.*` kinds. A config that selects the other plain
  6502 kinds assembles only where that binary or a test registers
  their constructors.
- A `[system]`-less document fails validation with an empty error
  report (no message names the missing section).
- Trigger conditions: the `break` and `watch` commands accept a
  condition token in their usage text but pass none in this build.
  The expression engine is reachable through the library API and
  through the design's embedding plan. The command surface wires
  conditions in a follow-up.
- The `Cycle` and `ClockEdge` trigger kinds have no runtime event
  path in this build. The other kinds route through the run loop.
- The `sm83.wave_ram_undoc` flag is declared, set by the bundled
  configs, and applied unconditionally by the audio device in this
  build.
- The `smoke.trace` quirk is declared and unused.
- The quirk key example of the design, `quirks.oam_bug`, does not
  resolve. The correct key form is `<family>.<quirk>`, for example
  `sm83.oam_bug`.
- The validate pipeline surfaces a grammar-rejected kind key, a
  zero-part frequency, and the other constructor-level errors as
  the `Schema` or `InvalidValue` variants with the field path. The
  `InvalidKindName`, `InvalidDeviceName`, and `FrequencyZero`
  variants belong to the newtype constructors. The pipeline wraps
  them.
- Static-memory devices truncate a large image at the device
  capacity. The boot-rom kinds load through the hex form: a `rom`
  path on an empty-capacity boot ROM loads nothing, and the loader
  call sites drop their errors in this build.

## 15. The embedded expression language

Strings of the trigger-registry conditions, and the command
surfaces that embed conditions, carry a small expression language
for conditions and predicates. As built, no config string field
parses as an expression. The registry resolves conditions through
the library API.

### 15.1 Grammar

```
expr    := or
or      := and ( "||" and )*
and     := cmp ( "&&" cmp )*
cmp     := sum ( ("==" | "!=" | "<" | "<=" | ">" | ">=") cmp )?
sum     := term ( ("+" | "-") term )*
term    := factor ( ("*" | "/" | "%") factor)*
factor  := "!" factor | "(" expr ")" | number | ident
number  := decimal | "0x" hex
ident   := [A-Za-z_][A-Za-z0-9_.]*
```

Precedence, low to high: `||`, `&&`, the comparison level, additive,
multiplicative, unary `!`, primary. The comparison level is
non-associative: one comparison per operand pair.

### 15.2 Evaluation semantics

- All arithmetic is unsigned 64-bit integer arithmetic. Addition,
  subtraction, and multiplication wrap.
- Division and remainder by zero evaluate to 0.
- Comparisons and the logical operators evaluate to 1 or 0. Nonzero
  is true.
- Unary `!` maps 0 to 1 and any nonzero to 0.
- An unknown identifier is a resolution error, reported at the
  creation site, not at fire time. The caller supplies the
  identifier table (the scope).
- A syntax error reports the byte offset in the string.

### 15.3 As-built wiring

The expression strings of the DSL parse into trigger conditions.
The trigger registry resolves every identifier against the caller's
scope at registration, so a condition that names a value outside
the scope is rejected at creation, not at fire time. The command
surface of this build constructs conditions only through the
library API. The machine chapters name the values each CPU exposes
through its register table.