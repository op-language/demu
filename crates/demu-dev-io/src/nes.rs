//! The NES controller ports ($4016 and $4017).
//!
//! A high strobe write bit latches the button state into the read
//! shifters; the falling edge of the strobe loads the shifters. Each
//! read returns the next button bit, and the standard controller
//! reports ones after eight reads. The expansion port bits read as
//! zero. The device accepts injected `press` and `release` events
//! through the input-sink trait.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The bit order of the standard controller, per shifter read.
const BUTTONS: &[&str] = &["a", "b", "select", "start", "up", "down", "left", "right"];

/// The controller ports of the RP2A03.
#[derive(Debug)]
pub struct Controller {
    name: String,
    /// The button mask of each controller, one bit per button.
    buttons: [u8; 2],
    /// The strobe state of $4016 bit zero.
    strobe: bool,
    /// The latched shifters, one per controller.
    shifters: [u16; 2],
    /// The reads past the eight buttons, per controller.
    overreads: [u8; 2],
}

impl Controller {
    /// A controller device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            buttons: [0; 2],
            strobe: false,
            shifters: [0; 2],
            overreads: [0; 2],
        }
    }

    /// One controller read: the shifter's low bit, the right-shift
    /// past it, and the ones that follow after the eight buttons.
    /// Returns the bit and, past the buttons, the open-bus value.
    fn read_bit(&mut self, port: usize) -> (u8, u8) {
        let bit = self.shifters[port] & 0x01;
        if self.overreads[port] < 8 {
            self.overreads[port] += 1;
            self.shifters[port] >>= 1;
            (bit as u8, 0x00)
        } else {
            // The standard controller returns ones after eight reads.
            (0x01, 0x00)
        }
    }

    /// Latch the button state into the shifters, as the strobe
    /// falling edge does.
    fn latch(&mut self) {
        for (port, buttons) in self.buttons.iter().enumerate() {
            let mut shifter = 0u16;
            for (bit, name) in BUTTONS.iter().enumerate() {
                if button_mask(name) != 0 && buttons & button_mask(name) != 0 {
                    shifter |= 1 << bit;
                }
            }
            self.shifters[port] = shifter;
        }
        // The read pointer resets with the latch.
        self.overreads = [0; 2];
    }
}

/// The button mask of one name in the injected event grammar.
fn button_mask(name: &str) -> u8 {
    match name {
        "a" => 0x80,
        "b" => 0x40,
        "select" => 0x20,
        "start" => 0x10,
        "up" => 0x08,
        "down" => 0x04,
        "left" => 0x02,
        "right" => 0x01,
        _ => 0,
    }
}

impl InputSink for Controller {
    fn inject(&mut self, event: &str) -> bool {
        let (target, action) = event.split_once(' ').unwrap_or(("p1", event));
        let port = match target.trim().to_ascii_lowercase().as_str() {
            "p1" => 0,
            "p2" => 1,
            _ => 1,
        };
        let verb_button = action.trim();
        let (verb, button) = verb_button.split_once(' ').unwrap_or((verb_button, ""));
        let pressed = match verb.trim().to_ascii_lowercase().as_str() {
            "press" => true,
            "release" => false,
            _ => return false,
        };
        let mask = button_mask(&button.trim().to_ascii_lowercase());
        if mask == 0 {
            return false;
        }
        self.buttons[port] = if pressed {
            self.buttons[port] | mask
        } else {
            self.buttons[port] & !mask
        };
        // The shifters re-latch on the state change, like the
        // strobe's falling edge would.
        self.shifters[port] = 0;
        let mut shifter = 0u16;
        for (bit, name) in BUTTONS.iter().enumerate() {
            if self.buttons[port] & button_mask(name) != 0 {
                shifter |= 1 << bit;
            }
        }
        self.shifters[port] = shifter;
        true
    }
}

impl BusAccess for Controller {
    /// The controller read: the shifter's next bit with the
    /// right-shift per read, and ones after the eight buttons.
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 {
            0x4016 => {
                let port = 0usize;
                let (bit, shifted) = self.read_bit(port);
                u32::from(bit | 0x40 | shifted)
            }
            0x4017 => {
                let port = 1usize;
                let (bit, shifted) = self.read_bit(port);
                u32::from(bit | 0x40 | shifted)
            }
            _ => 0x40,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if address.0 != 0x4016 {
            return;
        }
        let strobe = value & 0x01 != 0;
        match (self.strobe, strobe) {
            (true, false) => {
                // The strobe falling edge loads the shifters.
                self.latch();
                self.strobe = false;
            }
            (false, true) => {
                self.strobe = true;
                self.latch();
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0x4016 => self.shifters[0] as u8 & 1 | 0x40,
            0x4017 => self.shifters[1] as u8 & 1 | 0x40,
            _ => 0x40,
        }
    }

    fn decodes(&self, address: Address) -> bool {
        matches!(address.0, 0x4016 | 0x4017)
    }
}

impl Device for Controller {
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
        self.buttons = [0; 2];
        self.strobe = false;
        self.shifters = [0; 2];
        self.overreads = [0; 2];
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(12);
        out.extend_from_slice(b"NCPT");
        out.extend_from_slice(&self.buttons);
        for shifter in &self.shifters {
            out.extend_from_slice(&shifter.to_le_bytes());
        }
        out.extend_from_slice(&self.overreads);
        out.push(u8::from(self.strobe));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 13 || &data[..4] != b"NCPT" {
            return Err(SnapshotError::Mismatch("bad controller chunk".into()));
        }
        self.buttons.copy_from_slice(&data[4..6]);
        for (i, shifter) in self.shifters.iter_mut().enumerate() {
            *shifter = u16::from_le_bytes(data[6 + i * 2..8 + i * 2].try_into().expect("2 bytes"));
        }
        self.overreads.copy_from_slice(&data[10..12]);
        self.strobe = data[12] != 0;
        Ok(())
    }

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn notify_line(&mut self, _line: &str, level: Level, _tick: Tick) {
        let _ = level;
    }

    fn next_event(&self) -> Option<Tick> {
        None
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::device::MachineCtx) {}
}
