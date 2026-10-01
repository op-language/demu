//! The Disk II controller of the Apple II family.
//!
//! One device carries one controller card in the classic slot-6
//! address window `$C0E0-$C0EF`. The registers decode
//! address-exactly: any access to `$C0E0-$C0E3` pulses that phase of
//! the drive's stepper motor, `$C0E8`/`$C0E9` turn the drive motor on
//! and off, `$C0EA`/`$C0EB` pick drive one and two, `$C0EC` reads the
//! data latch, `$C0ED` writes it, `$C0EE` reads the write-protection
//! state, and `$C0EF` has no read effect.
//!
//! The stepping model is the classic four-phase one: a pulse of a
//! phase adjacent to the last pulsed phase advances or rewinds the
//! arm one half track, with the four phases at one quarter turn. The
//! arm carries up to 69 half tracks of the 35-track form. Each drive
//! latches its own arm position, and only the selected drive steps.
//! The drive core is the shared one of the `drive` module that the
//! IIgs IWM also uses.
//!
//! The media model simplifies the real controller: the device reads
//! its image as a raw byte stream without GCR sector framing. Each
//! of the 35 tracks holds `TRACK_SPAN` bytes, and every `$C0EC` read
//! returns the head byte and advances it. Writes latch the byte only,
//! and the image stays read-only in this build. The track-step and
//! media formats are simplifications recorded in the technical
//! design.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use crate::drive::{Drive, load_boot_image, pulse_phase};

/// The Disk II controller in its slot window.
#[derive(Debug)]
pub struct Disk2 {
    name: String,
    drives: [Drive; 2],
    /// True while drive one is the selected one.
    drive_one: bool,
    /// The motor of the two drives; the spindle runs the media reads.
    motor_on: bool,
    /// The last pulsed phase index, for the adjacent-step rule.
    last_phase: usize,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Disk2 {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            drives: [Drive::new(), Drive::new()],
            drive_one: true,
            motor_on: false,
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

    /// The address decode of one access.
    fn access(&mut self, address: u32, value: Option<u8>) -> u8 {
        let reg = (address & 0x0F) as usize;
        match (reg, value) {
            (0..=3, _) => {
                self.pulse(reg);
                0
            }
            (8, _) => {
                self.motor_on = true;
                0
            }
            (9, _) => {
                self.motor_on = false;
                0
            }
            (0x0A, _) => {
                self.drive_one = true;
                0
            }
            (0x0B, _) => {
                self.drive_one = false;
                0
            }
            (0x0C, Some(_)) => {
                let drive = self.drive();
                drive.latch
            }
            (0x0C, None) => {
                let drive = self.drive();
                if let Some(image) = &drive.image {
                    let idx = drive.slot().min(image.len().saturating_sub(1));
                    drive.latch = image[idx];
                }
                if self.motor_on {
                    self.drive().head += 1;
                }
                self.drive().latch
            }
            (0x0D, Some(v)) => {
                let drive = self.drive();
                drive.latch = v;
                v
            }
            (0x0D, None) => 0,
            (0x0E, _) => u8::from(self.drive().protected) << 6,
            _ => 0,
        }
    }
}

impl BusAccess for Disk2 {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.access(address.0, None))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let _ = self.access(address.0, Some(value as u8));
    }

    /// The readable states without the side effects: the data latch,
    /// the protection state, and zero for the pulse and select
    /// registers.
    fn peek(&self, address: Address) -> u8 {
        let drive = self.drive_ref();
        match (address.0 & 0x0F) as usize {
            0x0C => drive.latch,
            0x0E => u8::from(drive.protected) << 6,
            _ => 0,
        }
    }
}

impl Device for Disk2 {
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
        self.motor_on = false;
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
        if bytes.is_empty() {
            return;
        }
        // The boot image loads itself into drive one, trimmed to the
        // standard media size; the second drive stays empty until an
        // external command changes it.
        load_boot_image(&mut self.drives[0], bytes);
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(0x100);
        out.extend_from_slice(b"A2D2");
        out.push(2);
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
        out.push(u8::from(self.motor_on));
        out.push(self.last_phase as u8);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        // The restore walks the chunk with one guard: every read range
        // must fit, and the walk must land exactly on the end, so a
        // truncated or padded chunk rejects instead of slicing short.
        let guard = |pos: usize, need: usize| -> Result<(), SnapshotError> {
            if pos + need > data.len() {
                return Err(SnapshotError::Mismatch("bad disk2 chunk".into()));
            }
            Ok(())
        };
        if data.len() < 4 + 1 || &data[..4] != b"A2D2" || data[4] != 2 {
            return Err(SnapshotError::Mismatch("bad disk2 chunk".into()));
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
        guard(pos, 3)?;
        self.drive_one = data[pos] != 0;
        self.motor_on = data[pos + 1] != 0;
        self.last_phase = (data[pos + 2] as usize) & 3;
        pos += 3;
        if pos != data.len() {
            return Err(SnapshotError::Mismatch("bad disk2 chunk".into()));
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
    use crate::drive::{TRACK_SPAN, TRACKS};
    use demu_config::newtypes::{DeviceName, KindName};

    fn device() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: DeviceName::new("disk").expect("name"),
            kind: KindName::new("a2.disk2").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    fn disk() -> Disk2 {
        Disk2::new(&device())
    }

    #[test]
    fn stepper_walks_half_tracks() {
        let mut d = disk();
        d.pulse(0);
        d.pulse(1);
        assert_eq!(d.drives[0].half_track, 1, "one half track forward");
        d.pulse(2);
        assert_eq!(d.drives[0].half_track, 2);
        d.pulse(1);
        assert_eq!(d.drives[0].half_track, 1, "the rewind pulse steps back");
        d.pulse(1);
        assert_eq!(d.drives[0].half_track, 1, "the same phase pulses nothing");
    }

    #[test]
    fn track_read_walks_the_head() {
        let mut d = disk();
        let mut image = vec![0u8; TRACKS * TRACK_SPAN];
        image[0] = 0xAA;
        image[1] = 0xBB;
        image[TRACK_SPAN] = 0xCC;
        d.drives[0].image = Some(image);
        d.motor_on = true;
        assert_eq!(d.access(0xC0EC, None), 0xAA, "the first head byte");
        assert_eq!(d.access(0xC0EC, None), 0xBB, "the head advances");
        d.write(AccessWidth::Byte, Address(0xC0E0 + 1), 0, Tick(2));
        d.write(AccessWidth::Byte, Address(0xC0E0 + 2), 0, Tick(3));
        assert_eq!(d.drives[0].half_track, 2, "two forward pulses");
        assert_eq!(d.access(0xC0EE, None), 0, "the unprotected disk");
        assert_eq!(
            d.access(0xC0EC, None),
            0xCC,
            "the write step reads track one"
        );
    }

    #[test]
    fn select_and_motor_follow_the_registers() {
        let mut d = disk();
        d.access(0xC0E9, None);
        assert!(!d.motor_on);
        d.access(0xC0E8, None);
        assert!(d.motor_on);
        d.access(0xC0EB, None);
        assert!(!d.drive_one);
        d.write(AccessWidth::Byte, Address(0xC0ED), 0x5A, Tick(0));
        assert_eq!(d.drives[1].latch, 0x5A);
    }

    #[test]
    fn boot_carries_the_hex_image() {
        let mut d = disk();
        let bytes = vec![7u8; TRACKS * TRACK_SPAN / 2];
        d.boot(Some(&BootState::Hex { bytes }));
        assert_eq!(d.access(0xC0EC, None), 7, "the image answers");
    }

    #[test]
    fn snapshot_round_trip() {
        let mut d = disk();
        d.motor_on = true;
        d.pulse(1);
        d.drives[1].protected = true;
        d.write(AccessWidth::Byte, Address(0xC0ED), 0x3C, Tick(0));
        let snap = d.snapshot();
        let mut twin = disk();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
    }
}
