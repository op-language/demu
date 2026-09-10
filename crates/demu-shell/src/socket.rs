//! The Unix-domain-socket server mode.
//!
//! `serve` accepts one connection at a time and speaks the JSON-lines
//! protocol. A disconnect closes the connection cleanly and the loop waits
//! for the next connection. POSIX only.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;

use crate::json;
use crate::service::MachineService;

/// Serve the JSON-lines protocol on a Unix-domain socket.
///
/// # Errors
///
/// Returns an error when the socket cannot be bound, read, or written.
pub fn serve(service: &mut MachineService, socket_path: &Path) -> Result<(), std::io::Error> {
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut reader = BufReader::new(stream.try_clone()?);
        json::run(service, &mut reader, &mut stream)?;
    }
    Ok(())
}

/// A one-shot socket session: accept one connection, run the protocol
/// until the client disconnects, then return.
///
/// This is the testable form of [`serve`]; it runs one session instead of
/// looping forever.
pub fn serve_one(
    service: &mut MachineService,
    listener: &UnixListener,
) -> Result<(), std::io::Error> {
    let (mut stream, _) = listener.accept()?;
    let mut reader = BufReader::new(stream.try_clone()?);
    json::run(service, &mut reader, &mut stream)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::machine::Machine;
    use std::io::{Read, Write};

    #[test]
    fn socket_round_trip() {
        let dir = std::env::temp_dir().join(format!("demu-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();

        // The service runs on its own thread; the core is single-threaded
        // by design, so no reference crosses the thread boundary.
        let server = std::thread::spawn(move || {
            let mut service = MachineService::new();
            service.load_machine(Machine::new(demu_config::smoke_config().unwrap()).unwrap());
            service.machine_mut().unwrap().boot();
            serve_one(&mut service, &listener).unwrap();
        });

        let mut client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        writeln!(client, r#"{{"id":7,"cmd":"info"}}"#).unwrap();
        writeln!(client, r#"{{"id":8,"cmd":"run","args":42}}"#).unwrap();
        client.flush().unwrap();
        // Half-close the write side so the server sees end of input and
        // returns after the responses; the client keeps the read side.
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut buf = String::new();
        let mut reader = BufReader::new(client);
        reader.read_to_string(&mut buf).unwrap();
        let line1 = buf
            .lines()
            .next()
            .expect("the first response line")
            .to_string();
        let line2 = buf
            .lines()
            .nth(1)
            .expect("the second response line")
            .to_string();

        server.join().unwrap();
        assert!(line1.contains("\"ok\":true"));
        assert!(line1.contains("\"id\":7"));
        assert!(line2.contains("\"id\":8"));
        let _ = std::fs::remove_file(&path);
    }
}
