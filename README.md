# demu

`demu` is a cycle-accurate debugging emulator for the target machines that the
`op` language supports. It is a companion to `opc` (the compiler) and `cart`
(the build tool).

## Design goals

- Cycle accuracy. Each machine is exact for its hardware clocks, buses, and
  DMA behavior. There is no floating-point timing math.
- Determinism. The same ROM, the same inputs, and the same commands produce
  the same result every run.
- Machine-friendly control. A line protocol over stdio and a Unix-domain
  socket serves REPL commands for humans, opencode sessions, and LLM clients.
- Configuration-driven machines. A TOML system-configuration DSL constructs
  each emulated machine at startup.

## Status

The project is in early development. The workspace layout and the technical
design exist; system support arrives in phases.

## License

Apache-2.0. See `LICENSE`.