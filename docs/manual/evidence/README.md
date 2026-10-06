# Acceptance evidence

This directory holds the acceptance evidence that the demu manual
references. Each file records its generation commands, so anyone can
reproduce the run. The evidence here is from real demu sessions and
the bundled configs, on Linux x86_64.

| File | Evidence |
|---|---|
| `config-checkall.txt` | `demu config --check` on every bundled config at once. All 34 configs pass with no warnings. |
| `gameboy-session.txt` | A full text-REPL debug session on the Game Boy: boot, run, run-until, registers, memory, triggers, screenshot, save and load state. |
| `socket-session.txt` | A JSON-lines session over a Unix-domain socket on the Game Boy, with the request and response lines verbatim. |
| `cli-evidence.txt` | The `--dump` and `--sentinel` flags: a matching sentinel exits 0, a mismatching sentinel exits 1 with the compare message. |
| `determinism-evidence.txt` | Two identical sessions whose screenshots hash byte-identically. |

Generate the evidence files with the commands inside each file. The
session transcripts use the dmg-demo cart from the demo projects.

Note the boundaries. The per-system acceptance suites (blargg,
mooneye, nestest, ZEXALL), the real-ROM BIOS checksum runs, and the
golden cross-checks against other emulators remain pending a
follow-up evidence pass with real ROM assets. The bundled configs
document their first-build approximations in the machine chapters and
in the technical design as-built notes.
