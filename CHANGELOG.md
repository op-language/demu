# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The project uses the `0.y.z` scheme. Breaking changes bump `y`. Additions
bump `z`.

## [0.1.0]

### Added

- The `demu` workspace with 21 member crates that share the version `0.1.0`:
  the `demu` binary, the configuration DSL (`demu-config`), the simulation
  core (`demu-core`), the command shell (`demu-shell`), the symbol reader
  (`demu-symbols`), seven device-family crates, seven CPU cores, the audio
  resampler (`demu-resample`), and the Matroska pipeline (`demu-mkv`).
- The machine control protocol. A headless REPL reads text commands over
  stdin and stdout. `--json` switches to the JSON-lines protocol with the
  same command set. `--serve <path>` speaks the same JSON-lines protocol
  over a Unix-domain socket, one connection at a time. The commands cover
  run and step (`run`, `step`, `run-until`, `run-frame`), registers, memory
  reads, writes, fills, and dumps, breakpoints and watchpoints, screenshots
  as PPM or PNG, layer dumps (VRAM, name table, OAM, palette, framebuffer),
  save and load of the full machine state, symbol lookup, and input
  injection with input scripts.
- The deterministic CLI. `demu [OPTIONS] [ROM]` boots a machine from
  `--config <triple|file>`, loads `--symbols <file.opl>`, and ends with
  `--dump <path>` and `--sentinel <addr>:<value>`. The exit code is 0 for
  success or a sentinel pass, 1 for a sentinel miss, and 2 for a usage,
  configuration, or I/O error.
- The TOML system-configuration DSL. The `system`, `clock`, `device`,
  `bus`, `window`, `connection`, `signal`, `boot`, `quirks`, and `extends`
  sections construct each machine at startup. Device behavior lives in
  compiled kind modules that a static registry names. Configs select and
  parameterize kinds, and they cannot invent new behavior. Four validation
  layers (schema, device graph, semantic, and lint warnings) run from one
  entry point, `demu config --check <file|triple>`. `[extends]` merges a
  bundled or file base with named overrides and revalidates the result.
  The repository ships 34 bundled configs under `configs/`.
- The clock model. Each clock domain counts its own crystal ticks as exact
  integers. A frequency is a reduced rational `num/den`. Every device in a
  domain divides the crystal by an integer, which matches real divider
  chains. Unrelated crystals stay in separate domains. The scheduler orders
  cross-domain events by exact rational comparison and breaks ties in a
  deterministic device order. No floating-point math exists in the core.
  Nanosecond values in artifacts convert at the boundary with
  round-half-to-even.
- The media pipeline. An input script carries events at exact
  `#<domain>:<ticks>` stamps, accepts nanosecond stamps, comments, labels,
  and cyclic repeats, and validates against the system config. `mkv-input`
  writes the script to a Matroska input track with the system config
  embedded. `mkv-render` and `render-session` record FFV1 lossless video
  at one block per emulated display frame, a native-rate PCM track, and a
  resampled 48 kHz PCM track as the default-selected audio track. The
  resampler is a rational phase accumulator with exact integer math.
- The supported systems at release time. `opc` and `std` support 30 target
  triples, and the bundled configs cover 29 of them (all but
  `w65c02-commander-x16`). The set adds five demu-only forms: the two
  `mos6502` NES alias triples, the Commodore 64 PAL view, the Spectrum 128K
  memory-bank variant, and the `smoke` machine for framework tests:
  - SM83: Nintendo Game Boy and Game Boy Color.
  - RP2A03 and RP2A07 (the MOS 6502 core with the sound generator):
    Nintendo NES NTSC and PAL, with a `mos6502` alias triple for each.
  - MOS 6502: Atari 2600, 5200, 7800, and 800 (NTSC and PAL), Commodore 64
    (NTSC and PAL), and the Apple II family (II, IIc, IIe, enhanced IIe).
  - VLSI VL65NC02, the 65SC02 derivative: the Atari Lynx.
  - HuC6280: NEC PC Engine (the `mos6502-nec-pcengine` triple names the
    platform, and the machine runs the HuC6280).
  - WDC 65C816: SNES and Apple IIgs.
  - Z80: Sega SG-1000, Master System, Game Gear, Sinclair ZX80, ZX81, ZX
    Spectrum (48K and 128K), TI-85, and the Genesis and Neo Geo AES sound
    views.
  - Motorola 68000: Sega Genesis and Neo Geo AES.
- The `cart` integration. `cart` 0.9.0 drives demu through a run profile
  (`emulator = "demu"`) that starts `demu --config <triplet>` with the ROM
  path. The native demu test mode forwards the sentinel, the dump path,
  the symbol table, and the optional checks file of the `[test.demu]`
  section, pipes one `run-frame <N>` command, and decides pass and fail
  from the demu exit code. `cart debug` builds the ROM, starts demu with
  its socket server, and prints the socket path for LLM debug sessions.
  The demu-side checks schema is a follow-up item, so the current demu
  build rejects the forwarded checks flag.
- The documentation set: the technical design, the DSL specification, the
  machine-definition tutorial, the manual (index, REPL reference,
  configuration reference, media pipeline, and eight machine-group
  chapters), and the recorded acceptance evidence.