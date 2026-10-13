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

A bare `--config` triple resolves against the machine files installed
in the demu config directory `$XDG_CONFIG_HOME/demu/configs` (else
`~/.config/demu/configs`) before it falls back to the bundled copies,
so installed configs shadow bundled ones of the same name; see
`docs/manual/config-reference.md` for the search order. A non-loopback
deployment adds Host-header names with `daemon --allow-host`.

## Install and deployment

The daemon mode section above covers the endpoints; this section
covers the installs.

`packaging/install.sh` installs everything for one user: it checks the
`cargo install --path crates/demu` binary, installs the machine
configs into the demu config directory, installs the `docs/` manuals
into `~/.config/demu/docs`, merges the MCP entry of
`docs/examples/opencode.jsonc` and the skills of `.opencode/skills`
into the global `~/.config/opencode` configuration, installs the user
unit into `~/.config/systemd/user`, and enables the service.
`loginctl enable-linger` keeps it after logout.

To serve MCP clients from a server instead, `Dockerfile` and
`docker-compose.yml` build and run the daemon image with the MCP port
exposed; the compose defaults publish to the host loopback, and
`DEMU_PUBLISH` (with `DEMU_PUBLISH_PORT` for the host port) and
`DEMU_EXTRA_ARGS=--allow-host <host>` open the network path. The
daemon reference documents both paths.

The repository ships the OpenCode skill `demu-rom-dev` at
`.opencode/skills/demu-rom-dev/`. The skill teaches the ROM write, test, and
debug loop over the daemon's MCP tools. Install it one of two ways.
Copy the directory to `~/.config/opencode/skills/demu-rom-dev/` (the
install script does this). Or add
a `skills` array entry to the ROM project's `opencode.json` that points
at the demu checkout's `.opencode/skills` directory. The skill ID is
distinct from the socket-based `demu-debug` skill, so the two can
coexist.

## Status

The project is in early development. The workspace layout, the technical
design, and the MCP daemon exist; standalone emulators have not shipped.

## License

Apache-2.0. See `LICENSE`.