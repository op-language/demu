//! The `demu` binary.

fn main() {
    // The CLI and shell arrive in Phase 3. Until then the binary refuses to
    // run and reports the usage-error exit code from the CLI contract.
    eprintln!("demu is not implemented yet");
    std::process::exit(2);
}
