# The demu manual

Welcome to the demu manual. demu is the cycle-accurate, deterministic
debugging emulator for the machines of the Op language. This manual
serves two readers. The cart author drives demu as the debugger. The
machine-definition author constructs new machines from the DSL.

## How to use this manual

| If you want to... | Read |
|---|---|
| Debug a cart with demu | the [REPL and protocol reference](repl-reference.md), then the machine chapter of your target |
| Run demu as a daemon for OpenCode | the [daemon and MCP reference](daemon-reference.md) |
| Understand one machine | the machine chapter (the list below) |
| Write or check a machine definition | the [machine-definition tutorial](../machine-definitions-tutorial.md), then the [DSL specification](../dsl-specification.md) |
| Look up a config field | the [config reference](config-reference.md) and the [DSL specification](../dsl-specification.md) |
| Record or replay gameplay media | the [media pipeline](media-pipeline.md) |
| Reproduce the acceptance runs | the [evidence directory](evidence/) |

## Manual chapters

The reference chapters:

- [The REPL and protocol reference](repl-reference.md) — the CLI, the
  three transports, every command with its text and JSON forms, and
  the error codes.
- [The config reference](config-reference.md) — the bundled config
  list, the section map, the per-machine clocks and quirks, and the
  field summaries.
- [The media pipeline](media-pipeline.md) — the input script grammar,
  the replay scheduler, the Matroska forms, FFV1 video, and the audio
  resampling.
- [The daemon and MCP reference](daemon-reference.md) — the daemon
  execution mode, the MCP endpoint, the five session tools, the
  passthrough command rules with path confinement, and the systemd
  user-unit install.

The machine chapters, one per system group:

- [The SM83 machines: Game Boy and Game Boy Color](sm83-machines.md)
- [The 6502 NES machines: NTSC and PAL](nes-machines.md)
- [The 6502 Atari consoles: 2600, 5200, 7800](atari-consoles.md)
- [The 6502 home machines: Atari 800, Commodore 64, Apple II](atari-home-machines.md)
- [The 65C816 machines: SNES and Apple IIgs](65c816-machines.md)
- [The Lynx and PC Engine machines](lynx-pcengine-machines.md)
- [The Z80 machines: Sega 8-bit, Sinclair, TI-85](z80-machines.md)
- [The 68000 machines: Genesis and Neo Geo AES](m68000-machines.md)

Each machine chapter carries the same shape: the clock domains, the
device table with the kind names, the windows, the connections, the
boot states, the quirks, the input grammar, the subsystem
explanations, the recorded approximations, and the debug notes.

## The companion documents

- `docs/technical-design.md` — the technical specification. This
  manual records the user-facing rules. The design document is the
  arbiter for any behavior question.
- `docs/dsl-specification.md` — the complete DSL grammar, the
  validation layers, the extends rules, and the kind registry
  contract.
- `docs/machine-definitions-tutorial.md` — the construction lessons.

## The machine-kind surface

Every machine chapter names the exact kind strings the bundled
configs declare, and the DSL specification section 13.3 lists every
registered kind with its params, dividers, and quirks. A config
selects kinds by name and cannot invent behavior.

## Acceptance evidence

The `evidence/` directory holds the acceptance evidence of the
manual. It holds the config-check run over every bundled config, a
full REPL session, a socket session, the CLI sentinel runs, and the
determinism pairs. Each file records its generation commands.