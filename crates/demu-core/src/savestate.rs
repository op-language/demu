//! Save states.
//!
//! `save-state` writes a versioned binary file. `load-state` rejects a
//! file whose format version or machine hash does not match the running
//! machine. Restore reproduces the exact running state, including the
//! scheduler queues and all domain tick counters.

pub mod snapshot {
    //! The binary save-state format.
    //!
    //! ```text
    //! magic "DEMU" | format_version u32 | machine_hash u64
    //! | device_count u32
    //! | (name_len u32 | name bytes | chunk_len u32 | chunk bytes)*
    //! | domain_count u32
    //! | (name_len u32 | name | freq_num u64 | freq_den u64
    //!    | now u64 | event_count u32
    //!    | (tick u64 | device u32 | seq u64)*)*
    //! | checksum u32
    //! ```

    /// The save-state format version.
    pub const FORMAT_VERSION: u32 = 1;

    /// The snapshot magic bytes.
    pub const MAGIC: &[u8; 4] = b"DEMU";
}

use std::io::Write;

use crate::frequency::Frequency;
use crate::scheduler::{ClockDomain, Event};

/// One saved device chunk.
#[derive(Debug, Clone)]
pub struct DeviceChunk {
    /// The device name.
    pub name: String,
    /// The device state bytes.
    pub data: Vec<u8>,
}

/// A complete machine snapshot: device chunks plus scheduler state.
#[derive(Debug, Clone, Default)]
pub struct SaveState {
    /// The machine identity hash.
    pub machine_hash: u64,
    /// The device chunks in machine order.
    pub devices: Vec<DeviceChunk>,
    /// The domain states in machine order.
    pub domains: Vec<DomainState>,
}

/// A save-state write failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SaveError {
    /// The write failed.
    #[error("failed to write save state: {0}")]
    Io(#[from] std::io::Error),
}

/// Write a save state in the binary format.
///
/// # Errors
///
/// Returns an error when the underlying writer fails.
pub fn write_save<W: Write>(out: &mut W, state: &SaveState) -> Result<(), SaveError> {
    let mut buf = Vec::new();
    buf.extend_from_slice(snapshot::MAGIC);
    buf.extend_from_slice(&snapshot::FORMAT_VERSION.to_le_bytes());
    buf.extend_from_slice(&state.machine_hash.to_le_bytes());
    buf.extend_from_slice(&(state.devices.len() as u32).to_le_bytes());
    for chunk in &state.devices {
        buf.extend_from_slice(&(chunk.name.len() as u32).to_le_bytes());
        buf.extend_from_slice(chunk.name.as_bytes());
        buf.extend_from_slice(&(chunk.data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&chunk.data);
    }
    buf.extend_from_slice(&(state.domains.len() as u32).to_le_bytes());
    for d in &state.domains {
        buf.extend_from_slice(&(d.name.len() as u32).to_le_bytes());
        buf.extend_from_slice(d.name.as_bytes());
        buf.extend_from_slice(&d.frequency_num.to_le_bytes());
        buf.extend_from_slice(&d.frequency_den.to_le_bytes());
        buf.extend_from_slice(&d.now.to_le_bytes());
        buf.extend_from_slice(&(d.events.len() as u32).to_le_bytes());
        for (tick, device, seq) in &d.events {
            buf.extend_from_slice(&tick.to_le_bytes());
            buf.extend_from_slice(&device.to_le_bytes());
            buf.extend_from_slice(&seq.to_le_bytes());
        }
    }
    let checksum = checksum32(&buf);
    buf.extend_from_slice(&checksum.to_le_bytes());
    out.write_all(&buf)?;
    Ok(())
}

/// A load-state failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The file failed a format check.
    #[error("invalid save state: {0}")]
    Format(&'static str),

    /// The file carries another format version.
    #[error("save state version {found} does not match this machine's version {expected}")]
    Version {
        /// The version in the file.
        found: u32,
        /// The version the machine supports.
        expected: u32,
    },

    /// The file belongs to another machine.
    #[error("save state machine hash {found:#x} does not match {expected:#x}")]
    MachineHash {
        /// The hash in the file.
        found: u64,
        /// The hash of the running machine.
        expected: u64,
    },

    /// A device rejected its chunk.
    #[error("device {device} rejected its chunk: {source}")]
    Restore {
        /// The device name.
        device: String,
        /// The device's own error.
        source: crate::device::SnapshotError,
    },

    /// The read failed.
    #[error("failed to read save state: {0}")]
    Io(#[from] std::io::Error),
}

/// Read a save state.
///
/// # Errors
///
/// Returns an error when the format, version, machine hash, or checksum
/// does not match.
pub fn read_save(buf: &[u8], machine_hash: u64) -> Result<SaveState, LoadError> {
    let expected_len = buf.len();
    if expected_len < 24 {
        return Err(LoadError::Format("file is too short"));
    }
    let mut pos = 0usize;
    if &buf[..4] != snapshot::MAGIC {
        return Err(LoadError::Format("bad magic"));
    }
    pos += 4;
    let version = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("4 bytes"));
    pos += 4;
    if version != snapshot::FORMAT_VERSION {
        return Err(LoadError::Version {
            found: version,
            expected: snapshot::FORMAT_VERSION,
        });
    }
    let hash = u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("8 bytes"));
    pos += 8;
    if hash != machine_hash {
        return Err(LoadError::MachineHash {
            found: hash,
            expected: machine_hash,
        });
    }
    let checksum_stored = u32::from_le_bytes(buf[buf.len() - 4..].try_into().expect("4 bytes"));
    if checksum32(&buf[..buf.len() - 4]) != checksum_stored {
        return Err(LoadError::Format("checksum mismatch"));
    }

    let count = read_u32(buf, &mut pos)?;
    let mut devices = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = read_string(buf, &mut pos)?;
        let len = read_u32(buf, &mut pos)? as usize;
        let end = pos
            .checked_add(len)
            .ok_or(LoadError::Format("length overflow"))?;
        if end > buf.len() {
            return Err(LoadError::Format("device chunk exceeds file"));
        }
        devices.push(DeviceChunk {
            name,
            data: buf[pos..end].to_vec(),
        });
        pos = end;
    }
    let domains_len = read_u32(buf, &mut pos)?;
    let mut domains = Vec::with_capacity(domains_len as usize);
    for _ in 0..domains_len {
        let name = read_string(buf, &mut pos)?;
        let num = read_u64(buf, &mut pos)?;
        let den = read_u64(buf, &mut pos)?;
        let now = read_u64(buf, &mut pos)?;
        let events_len = read_u32(buf, &mut pos)?;
        let mut events = Vec::with_capacity(events_len as usize);
        for _ in 0..events_len {
            let tick = read_u64(buf, &mut pos)?;
            let device = read_u32(buf, &mut pos)?;
            let seq = read_u64(buf, &mut pos)?;
            events.push((tick, device, seq));
        }
        domains.push(DomainState {
            name,
            frequency_num: num,
            frequency_den: den,
            now,
            events,
        });
    }
    Ok(SaveState {
        machine_hash: hash,
        devices,
        domains,
    })
}

fn read_u32(buf: &[u8], pos: &mut usize) -> Result<u32, LoadError> {
    let end = pos
        .checked_add(4)
        .ok_or(LoadError::Format("length overflow"))?;
    if end > buf.len() {
        return Err(LoadError::Format("truncated u32"));
    }
    let v = u32::from_le_bytes(buf[*pos..end].try_into().expect("4 bytes"));
    *pos = end;
    Ok(v)
}

fn read_u64(buf: &[u8], pos: &mut usize) -> Result<u64, LoadError> {
    let end = pos
        .checked_add(8)
        .ok_or(LoadError::Format("length overflow"))?;
    if end > buf.len() {
        return Err(LoadError::Format("truncated u64"));
    }
    let v = u64::from_le_bytes(buf[*pos..end].try_into().expect("8 bytes"));
    *pos = end;
    Ok(v)
}

fn read_string(buf: &[u8], pos: &mut usize) -> Result<String, LoadError> {
    let len = read_u32(buf, pos)? as usize;
    let end = pos
        .checked_add(len)
        .ok_or(LoadError::Format("length overflow"))?;
    if end > buf.len() {
        return Err(LoadError::Format("string exceeds file"));
    }
    let s = std::str::from_utf8(&buf[*pos..end])
        .map_err(|_| LoadError::Format("name is not UTF-8"))?
        .to_string();
    *pos = end;
    Ok(s)
}

/// A saved clock domain.
#[derive(Debug, Clone)]
pub struct DomainState {
    /// The domain name.
    pub name: String,
    /// The crystal numerator.
    pub frequency_num: u64,
    /// The crystal denominator.
    pub frequency_den: u64,
    /// The current tick.
    pub now: u64,
    /// The pending events as `(tick, device, seq)`.
    pub events: Vec<(u64, u32, u64)>,
}

impl DomainState {
    /// Rebuild a live clock domain from the saved state.
    pub fn into_domain(self) -> Result<ClockDomain, LoadError> {
        let frequency = Frequency::new(self.frequency_num, self.frequency_den)
            .map_err(|_| LoadError::Format("domain frequency is invalid"))?;
        let mut domain = ClockDomain::new(&self.name, frequency);
        domain.now = crate::frequency::Tick(self.now);
        for (tick, device, seq) in self.events {
            domain.queue.push(Event {
                tick: crate::frequency::Tick(tick),
                device: device as usize,
                seq,
            });
        }
        Ok(domain)
    }
}

/// The FNV-1a 32-bit checksum over the buffer.
fn checksum32(buf: &[u8]) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for b in buf {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SaveState {
        SaveState {
            machine_hash: 0x1234_5678_9ABC_DEF0,
            devices: vec![DeviceChunk {
                name: "ram0".into(),
                data: vec![1, 2, 3, 4],
            }],
            domains: vec![DomainState {
                name: "main".into(),
                frequency_num: 4194304,
                frequency_den: 1,
                now: 777,
                events: vec![(1000, 1, 3), (1000, 0, 4)],
            }],
        }
    }

    #[test]
    fn round_trip_preserves_all_fields() {
        let mut buf = Vec::new();
        write_save(&mut buf, &sample()).unwrap();
        let back = read_save(&buf, 0x1234_5678_9ABC_DEF0).unwrap();
        assert_eq!(back.machine_hash, 0x1234_5678_9ABC_DEF0);
        assert_eq!(back.devices[0].name, "ram0");
        assert_eq!(back.devices[0].data, vec![1, 2, 3, 4]);
        assert_eq!(back.domains[0].now, 777);
        assert_eq!(back.domains[0].events.len(), 2);
        assert_eq!(back.domains[0].frequency_num, 4194304);
    }

    #[test]
    fn machine_hash_mismatch_is_rejected() {
        let mut buf = Vec::new();
        write_save(&mut buf, &sample()).unwrap();
        match read_save(&buf, 0xDEAD) {
            Err(LoadError::MachineHash { .. }) => {}
            other => panic!("expected MachineHash error, got {other:?}"),
        }
    }

    #[test]
    fn corrupted_data_is_rejected() {
        let mut buf = Vec::new();
        write_save(&mut buf, &sample()).unwrap();
        let last = buf.len() - 1;
        buf[last] ^= 0xFF;
        match read_save(&buf, 0x1234_5678_9ABC_DEF0) {
            Err(LoadError::Format(_)) => {}
            other => panic!("expected Format error, got {other:?}"),
        }
    }

    #[test]
    fn events_round_trip_through_domain() {
        let state = sample();
        let domain = state.domains[0].clone().into_domain().unwrap();
        assert_eq!(domain.now, crate::frequency::Tick(777));
        assert_eq!(domain.queue.len(), 2);
    }
}
