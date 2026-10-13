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
- MCP daemon. `demu daemon` serves the same command set as a loopback
  Model Context Protocol (MCP) HTTP endpoint, so OpenCode and other MCP
  clients drive emulator sessions as tools.
- Configuration-driven machines. A TOML system-configuration DSL constructs
  each emulated machine at startup.

## Daemon mode

`demu daemon` hosts emulator sessions as a Model Context Protocol (MCP)
server on the loopback interface. Start it by hand with
`demu daemon --http 127.0.0.1:6502`, or install it as a systemd user
daemon with the unit in `packaging/systemd/demu.service`. An MCP client
such as OpenCode then reaches the endpoint `http://127.0.0.1:6502/mcp`,
creates a named session from a config and a base64 ROM image, and
drives it with a passthrough tool for the full demu command set.
To connect OpenCode, copy `docs/examples/opencode.jsonc` into the
OpenCode configuration, or run
`opencode mcp add demu --global --url http://127.0.0.1:6502/mcp` and
set `"oauth": false` in the written entry; the URL must match the
`--http` bind. Sessions live in the daemon process and survive
client disconnects; they vanish with the daemon. The daemon binds
loopback only, with no token; see `docs/manual/daemon-reference.md`
for the tool reference, the trust model, the install steps, and the
OpenCode connection.

## Status

The project is in early development. The workspace layout, the technical
design, and the MCP daemon exist; standalone emulators have not shipped.

## License

Apache-2.0. See `LICENSE`.