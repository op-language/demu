//! The Integrated WOZ Machine (IWM) of the Apple IIgs.
//!
//! The IWM sits at the internal disk port `$C0E0-$C0EF`, the same
//! 16-register window the Disk II card uses in slot 6 on the IIe.
//! The register map (per the Apple IIgs Hardware Reference and its
//! Technical Note 30 corrections):
//!
//! - `$C0E0-$C0E7` = the four stepper phases as off/on pairs: even
//!   registers de-energize the phase, odd registers pulse it.
//! - `$C0E8`/`$C0E9` = the drive disable/enable switches.
//! - `$C0EA`/`$C0EB` = the drive one/two select.
//! - `$C0EC`-$C0EF` = the Q7/Q6 state pair. The pair selects the
//!   operation: Q7=0, Q6=0 reads the data register, Q7=0, Q6=1 reads
//!   the status register, Q7=1, Q6=0 reads the handshake register,
//!   and Q7=1, Q6=1 writes the data register (drive enabled) or the
//!   mode register (drive disabled).
//!
//! The IWM states the same shared 5.25-inch drive core that the
//! Disk II card drives, so a boot image loads into drive one the
//! same way. The 3.5-inch media form, the variable-speed read path,
//! and the asynchronous write handshake are not modeled in this
//! build: the status reads stand for the classic write-protect sense
//! bit, the handshake read answers the immediate-ready form, and the
//! write path latches the byte without media writes, exactly like
//! the Disk II card's media model.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use crate::drive::{Drive, load_boot_image, pulse_phase};

/// The IWM with the disk-port register window and two drives.
#[derive(Debug)]
pub struct Iwm {
    name: String,
    drives: [Drive; 2],
    /// True while drive one is the selected one.
    drive_one: bool,
    /// The enable switch state at `$C0E9`, which also gates the
    /// data reads and picks the data/mode write form.
    drive_enabled: bool,
    /// The stored mode register byte (bits: 4 = the 8-MHz read
    /// clock, 2 = the 1-second timer latch, 1 = the asynchronous
    /// handshake form).
    mode: u8,
    last_phase: usize,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Iwm {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            drives: [Drive::new(), Drive::new()],
            drive_one: true,
            drive_enabled: false,
            mode: 0,
            last_phase: 0,
            next: None,
            last_tick: None,
        }
    }

    /// The drive that the select switches point at, without state
    /// changes; the read-only form backs the peek path.
    fn drive_ref(&self) -> &Drive {
        if self.drive_one {
            &self.drives[0]
        } else {
            &self.drives[1]
        }
    }

    /// The drive that the select switches point at.
    fn drive(&mut self) -> &mut Drive {
        if self.drive_one {
            &mut self.drives[0]
        } else {
            &mut self.drives[1]
        }
    }

    /// Pulse one stepper phase through the shared drive core.
    fn pulse(&mut self, phase: usize) {
        let mut last = self.last_phase;
        pulse_phase(self.drive(), phase, &mut last);
        self.last_phase = last;
    }

    /// The status byte of the Q7=0, Q6=1 read: the write-protect
    /// sense at bit 6.
    fn status_byte(&self) -> u8 {
        u8::from(self.drive_ref().protected) << 6
    }

    /// The handshake byte of the Q7=1, Q6=0 read: the immediate-ready
    /// form of the simplified write path.
    fn handshake_byte(&self) -> u8 {
        0x80
    }

    /// The data read of the Q7=0, Q6=0 form: the head byte and the
    /// head advance while the drive runs.
    fn data_read(&mut self) -> u8 {
        let enabled = self.drive_enabled;
        let drive = self.drive();
        let latch = drive.latch;
        if !enabled {
            return latch;
        }
        if let Some(image) = &drive.image {
            let idx = drive.slot().min(image.len().saturating_sub(1));
            drive.latch = image[idx];
        }
        drive.head += 1;
        drive.latch
    }

    /// The address decode of one access.
    fn access(&mut self, address: u32, value: Option<u8>) -> u8 {
        let reg = (address & 0x0F) as usize;
        match (reg, value, self.drive_enabled) {
            (0..=7, _, _) => {
                // The phase pairs: the odd register pulses the phase
                // and the even register only de-energizes it.
                if reg & 1 == 1 {
                    self.pulse(reg >> 1);
                }
                0
            }
            (8, _, _) => {
                self.drive_enabled = false;
                0
            }
            (9, _, _) => {
                self.drive_enabled = true;
                0
            }
            (0x0A, _, _) => {
                self.drive_one = true;
                0
            }
            (0x0B, _, _) => {
                self.drive_one = false;
                0
            }
            (0x0C, _, _) if value.is_none() => self.data_read(),
            (0x0D, _, _) if value.is_none() => self.status_byte(),
            (0x0E, _, _) if value.is_none() => self.handshake_byte(),
            // A write when disabled stores the mode register.
            (0x0F, Some(v), false) => {
                self.mode = v;
                v
            }
            // A write when enabled stores the data latch.
            (0x0F, Some(v), true) => {
                self.drive().latch = v;
                v
            }
            _ => {
                if reg == 0x0F && value.is_none() {
                    return self.drive().latch;
                }
                0
            }
        }
    }
}

impl BusAccess for Iwm {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.access(address.0, None))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let _ = self.access(address.0, Some(value as u8));
    }

    /// The readable states without the side effects: the latch, the
    /// status bits, and zero for the pulse and select registers.
    fn peek(&self, address: Address) -> u8 {
        match (address.0 & 0x0F) as usize {
            0x0C => self.drive_ref().latch,
            0x0D => self.status_byte(),
            0x0E => self.handshake_byte(),
            _ => 0,
        }
    }
}

impl Device for Iwm {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn bus(&self) -> Option<&dyn BusAccess> {
        Some(self)
    }

    fn bus_mut(&mut self) -> Option<&mut dyn BusAccess> {
        Some(self)
    }

    fn reset(&mut self) {
        self.drives = [Drive::new(), Drive::new()];
        self.drive_one = true;
        self.drive_enabled = false;
        self.mode = 0;
        self.last_phase = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        let bytes = match state {
            Some(BootState::Rom { path }) => std::fs::read(path).unwrap_or_default(),
            Some(BootState::Hex { bytes }) => bytes.clone(),
            _ => Vec::new(),
        };
        load_boot_image(&mut self.drives[0], bytes);
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(0x100);
        out.extend_from_slice(b"IIWM");
        out.push(1);
        for drive in &self.drives {
            out.push(drive.half_track);
            out.extend_from_slice(&(drive.head as u32).to_le_bytes());
            out.push(u8::from(drive.protected));
            out.push(drive.latch);
            let len = drive.image.as_ref().map(|v| v.len() as u32).unwrap_or(0);
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(drive.image.as_deref().unwrap_or(&[]));
        }
        out.push(u8::from(self.drive_one));
        out.push(u8::from(self.drive_enabled));
        out.push(self.mode);
        out.push(self.last_phase as u8);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        // The restore walks the chunk with one guard: every read range
        // must fit, and the walk must land exactly on the end, so a
        // truncated or padded chunk rejects instead of slicing short.
        let guard = |pos: usize, need: usize| -> Result<(), SnapshotError> {
            if pos + need > data.len() {
                return Err(SnapshotError::Mismatch("bad iwm chunk".into()));
            }
            Ok(())
        };
        if data.len() < 4 + 1 || &data[..4] != b"IIWM" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad iwm chunk".into()));
        }
        let mut pos = 5usize;
        for drive in &mut self.drives {
            guard(pos, 7)?;
            drive.half_track = data[pos];
            drive.head =
                u32::from_le_bytes(data[pos + 1..pos + 5].try_into().expect("4 bytes")) as usize;
            drive.protected = data[pos + 5] != 0;
            drive.latch = data[pos + 6];
            pos += 7;
            guard(pos, 4)?;
            let len = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) as usize;
            pos += 4;
            drive.image = if len == 0 {
                None
            } else {
                guard(pos, len)?;
                Some(data[pos..pos + len].to_vec())
            };
            pos += len;
        }
        guard(pos, 4)?;
        self.drive_one = data[pos] != 0;
        self.drive_enabled = data[pos + 1] != 0;
        self.mode = data[pos + 2];
        self.last_phase = (data[pos + 3] as usize) & 3;
        pos += 4;
        if pos != data.len() {
            return Err(SnapshotError::Mismatch("bad iwm chunk".into()));
        }
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::newtypes::{DeviceName, KindName};

    fn device() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: DeviceName::new("iwm").expect("name"),
            kind: KindName::new("iigs.iwm").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    fn iwm() -> Iwm {
        Iwm::new(&device())
    }

    #[test]
    fn phase_pairs_pulse_odd_registers_only() {
        let mut d = iwm();
        // The first phase pulse only arms the phase rule.
        d.access(0xC0E1, None);
        assert_eq!(d.drives[0].half_track, 0);
        d.access(0xC0E1, None);
        assert_eq!(d.drives[0].half_track, 0, "the same phase repeats dead");
        d.access(0xC0E3, None);
        assert_eq!(d.drives[0].half_track, 1, "the PH1 on register steps");
        d.access(0xC0E2, None);
        assert_eq!(d.drives[0].half_track, 1, "the off register does not step");
        d.access(0xC0E1, None);
        assert_eq!(d.drives[0].half_track, 0, "the rewind pulses back");
    }

    #[test]
    fn enable_gates_the_data_read_forms() {
        let mut d = iwm();
        let mut image = vec![0u8; crate::drive::TRACKS * crate::drive::TRACK_SPAN];
        image[0] = 0xAA;
        image[1] = 0xBB;
        d.drives[0].image = Some(image);
        // The disabled drive holds the latch.
        assert_eq!(d.access(0xC0EC, None), 0);
        d.access(0xC0E9, None);
        assert!(d.drive_enabled);
        assert_eq!(d.access(0xC0EC, None), 0xAA);
        assert_eq!(d.access(0xC0EC, None), 0xBB, "the head advances");
        d.access(0xC0E8, None);
        assert!(!d.drive_enabled);
        assert_eq!(d.access(0xC0EC, None), 0xBB, "the disabled form holds");
    }

    #[test]
    fn status_and_handshake_read_the_sense_bits() {
        let mut d = iwm();
        d.access(0xC0E9, None);
        d.drives[0].protected = true;
        assert_eq!(d.access(0xC0ED, None), 0x40, "the write protect at bit 6");
        assert_eq!(d.access(0xC0EE, None), 0x80, "the immediate-ready form");
        d.drives[0].protected = false;
        assert_eq!(d.access(0xC0ED, None), 0);
    }

    #[test]
    fn selects_split_the_drives() {
        let mut d = iwm();
        d.access(0xC0E9, None);
        d.access(0xC0EB, None);
        assert!(!d.drive_one, "drive two selected");
        d.write(AccessWidth::Byte, Address(0xC0EF), 0x5A, Tick(0));
        assert_eq!(d.drives[1].latch, 0x5A);
        assert_eq!(d.drives[0].latch, 0);
        d.access(0xC0EA, None);
        d.write(AccessWidth::Byte, Address(0xC0EF), 0x33, Tick(0));
        assert_eq!(d.drives[0].latch, 0x33);
    }

    #[test]
    fn the_mode_register_writes_when_disabled() {
        let mut d = iwm();
        // Drive disabled: the Q7/Q6-high write stores the mode byte.
        d.write(AccessWidth::Byte, Address(0xC0EF), 0b0001_0010, Tick(0));
        assert_eq!(d.mode, 0b0001_0010);
        assert_eq!(d.drives[0].latch, 0, "no data latch while disabled");
    }

    #[test]
    fn boot_carries_the_hex_image() {
        let mut d = iwm();
        let bytes = vec![7u8; 64];
        d.boot(Some(&BootState::Hex { bytes }));
        d.access(0xC0E9, None);
        assert_eq!(d.access(0xC0EC, None), 7, "the image answers");
    }

    #[test]
    fn snapshot_round_trip() {
        let mut d = iwm();
        d.access(0xC0E9, None);
        d.pulse(1);
        d.drives[1].protected = true;
        d.write(AccessWidth::Byte, Address(0xC0EF), 0x3C, Tick(0));
        // Disable the drive: the Q7/Q6-high write stores the mode
        // register instead of the data latch.
        d.access(0xC0E8, None);
        d.write(AccessWidth::Byte, Address(0xC0EF), 0b0000_0010, Tick(0));
        let snap = d.snapshot();
        let mut twin = iwm();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.mode, 0b0000_0010);
        assert!(!twin.drive_enabled, "the disabled form restores");
        assert!(twin.drives[1].protected);
    }

    #[test]
    fn restore_rejects_a_bad_chunk() {
        let mut d = iwm();
        let snapshot = d.snapshot();
        let mut broken = snapshot.clone();
        broken.push(0);
        assert!(d.restore(&broken).is_err());
        assert!(d.restore(&[0; 8]).is_err());
    }
}
