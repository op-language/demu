//! The serial devices: the SM83 serial port and the TI-85 link port.
//!
//! The SM83 serial device.
//!
//! The shift register moves one byte over eight bits. With the internal
//! clock selected, the transfer takes 512 T-cycles per bit (16 on the
//! CGB fast clock), and the completion raises the serial interrupt.
//! With the external clock selected and no link peer, the transfer never
//! starts, which matches an idle link cable.
//!
//! The TI-85 link port carries the two cable lines of the machine's
//! link protocol: the latched drive levels and enables, and the remote
//! levels that arrive as injected link events.

pub mod ti85;

pub use ti85::Ti85Link;

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The serial port.
pub struct Serial {
    name: String,
    /// The shift register data, register `SB`.
    pub sb: u8,
    /// The control register `SC`.
    pub sc: u8,
    /// The bits moved so far in the active transfer.
    bits: u8,
    /// The tick of the next bit edge.
    next: Option<Tick>,
    /// Domain dots per T-cycle: 1 at normal speed, 2 at double speed.
    ticks_per_t: u32,
    last_tick: Option<Tick>,
}

impl Serial {
    /// A serial port from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            sb: 0x00,
            sc: 0x7E,
            bits: 0,
            next: None,
            ticks_per_t: 1,
            last_tick: None,
        }
    }
}

impl Device for Serial {
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
        self.sb = 0x00;
        self.sc = 0x7E;
        self.bits = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(10);
        out.extend_from_slice(b"SRL0");
        out.push(self.sb);
        out.push(self.sc);
        out.push(self.bits);
        out.extend_from_slice(&self.ticks_per_t.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 11 || &data[..4] != b"SRL0" {
            return Err(SnapshotError::Mismatch("bad serial chunk".into()));
        }
        self.sb = data[4];
        self.sc = data[5];
        self.bits = data[6];
        self.ticks_per_t = u32::from_le_bytes(data[7..11].try_into().expect("4 bytes")).clamp(1, 2);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, _level: Level, _tick: Tick) {
        // The serial clock chain runs twice as fast in double speed.
        if line == "speed" {
            self.ticks_per_t = if self.ticks_per_t == 1 { 2 } else { 1 };
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        // The transfer completes after eight bit periods.
        if let Some(start) = self.next.map(|_| tick) {
            let _ = start;
            self.sc &= !0x80;
            self.sb = 0xFF;
            self.next = None;
            ctx.notify("int", Level::High, tick);
        }
    }
}

impl BusAccess for Serial {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 {
            0xFF01 => u32::from(self.sb),
            0xFF02 => u32::from(self.sc) | 0x7E,
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let v = value as u8;
        match address.0 {
            0xFF01 => self.sb = v,
            0xFF02 => {
                self.sc = v & 0x83;
                if v & 0x80 != 0 && v & 0x01 != 0 {
                    // The internal clock starts the transfer; one bit
                    // takes 512 T-cycles, 16 on the fast clock.
                    let bit: u32 = if v & 0x02 != 0 { 16 } else { 512 };
                    let dots = u64::from(bit) / u64::from(self.ticks_per_t);
                    self.next = Some(Tick(tick.0 + dots * 8));
                } else {
                    self.next = None;
                }
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0xFF01 => self.sb,
            0xFF02 => (self.sc & 0x83) | 0x7E,
            _ => 0xFF,
        }
    }
}
