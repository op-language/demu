//! The SNES controller port: the manual serial reads at `$4016`/`$4017`.
//!
//! One device answers both controller ports. The auto-joypad read of
//! the 5A22 lives in the system device and walks these ports through
//! the bus; this device only carries the shift state and the
//! latched-up button words that the injected pad state feeds.
//!
//! A strobe write of 1 to `$4016` re-loads both shift registers from
//! the button words, which is how a program re-arms its reads; a
//! write of 0 ends the strobe without a reload.
//!
//! Buttons: a 16-bit word per controller, bit order per the classic
//! controller: B Y Select Start Up Down Left Right A X L R followed
//! by four zero bits. Each read after the strobe drops one bit. Reads
//! past the 16 buttons return 1, the open-bus high form that real
//! controllers show after their shift drained.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The button bits in the word, low bit first.
const NAME_MASK: u16 = 0x0001;

/// The buttons per controller before the drained reads start.
const BUTTON_COUNT: u8 = 16;

/// The pad with two controllers.
#[derive(Debug)]
pub struct SnesPad {
    name: String,
    /// The injected button words per controller.
    buttons: [u16; 2],
    /// The shift latches: the remaining word per controller, refilled
    /// by a strobe.
    shift: [u16; 2],
    /// The reads each port made since the last refill; 1 after the
    /// 16 buttons is the drained open-bus high form.
    drained: [u8; 2],
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl SnesPad {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            buttons: [0; 2],
            shift: [0; 2],
            drained: [0; 2],
            next: None,
            last_tick: None,
        }
    }

    /// The next shift bit of one port, 1 when drained past 16 bits.
    fn shift_bit(&mut self, port: usize) -> u8 {
        if self.drained[port] >= BUTTON_COUNT {
            self.drained[port] = self.drained[port].saturating_add(1);
            return 1;
        }
        self.drained[port] += 1;
        let bit = u8::from(self.shift[port] & NAME_MASK != 0);
        self.shift[port] >>= 1;
        bit
    }

    /// The bus read of one port's serial line.
    fn port_read(&mut self, port: usize) -> u8 {
        self.shift_bit(port)
    }

    /// Refill the shift latches from the button words.
    fn refill(&mut self) {
        self.shift = self.buttons;
        self.drained = [0; 2];
    }
}

impl BusAccess for SnesPad {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 & 0x3FF {
            0x016 => u32::from(self.port_read(0)),
            0x017 => u32::from(self.port_read(1)),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if address.0 & 0x3FF == 0x016 && value & 1 != 0 {
            // A 1 on the strobe refills both shift registers.
            self.refill();
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 & 0x3FF {
            0x016 => u8::from(self.shift[0] & NAME_MASK != 0),
            0x017 => u8::from(self.shift[1] & NAME_MASK != 0),
            _ => 0xFF,
        }
    }
}

impl InputSink for SnesPad {
    fn inject(&mut self, event: &str) -> bool {
        // Grammar: `pad1 <word>` / `pad2 <word>` set the button
        // words; a `-release` suffix clears them.
        let form = event.trim().to_ascii_lowercase();
        let (which, rest) = if let Some(w) = form.strip_prefix("pad1 ") {
            (0usize, w)
        } else if let Some(w) = form.strip_prefix("pad2 ") {
            (1usize, w)
        } else {
            return false;
        };
        if let Ok(v) = u16::from_str_radix(rest.trim_end_matches("-release"), 16) {
            self.buttons[which] = v;
            self.refill();
            return true;
        }
        false
    }
}

impl Device for SnesPad {
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

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.buttons = [0; 2];
        self.shift = [0; 2];
        self.drained = [0; 2];
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        // The pad is a passive shift register: no periodic visit.
        self.next = None;
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(18);
        out.extend_from_slice(b"SPD1");
        out.push(2);
        for b in self.buttons {
            out.extend_from_slice(&b.to_le_bytes());
        }
        for b in self.shift {
            out.extend_from_slice(&b.to_le_bytes());
        }
        out.extend_from_slice(&self.drained);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 4 + 4 + 2;
        if data.len() != expected || &data[..4] != b"SPD1" || data[4] != 2 {
            return Err(SnapshotError::Mismatch("bad snes pad chunk".into()));
        }
        let mut pos = 5usize;
        for b in &mut self.buttons {
            *b = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
        }
        for b in &mut self.shift {
            *b = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
        }
        self.drained = [data[pos], data[pos + 1]];
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
            name: DeviceName::new("pad").expect("name"),
            kind: KindName::new("snes.pad").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    fn pad() -> SnesPad {
        SnesPad::new(&device())
    }

    #[test]
    fn strobe_refills_and_shift_walks_bits() {
        let mut p = pad();
        assert!(p.inject("pad1 0003"));
        // The strobe write refills.
        p.write(AccessWidth::Byte, Address(0x004016), 1, Tick(0));
        assert_eq!(p.read(AccessWidth::Byte, Address(0x004016), Tick(0)), 1);
        assert_eq!(p.read(AccessWidth::Byte, Address(0x004016), Tick(0)), 1);
    }

    #[test]
    fn second_port_reads_its_own_buttons() {
        let mut p = pad();
        assert!(p.inject("pad2 0002"));
        p.write(AccessWidth::Byte, Address(0x004016), 1, Tick(0));
        assert_eq!(p.read(AccessWidth::Byte, Address(0x004017), Tick(0)), 0);
        assert_eq!(p.read(AccessWidth::Byte, Address(0x004017), Tick(0)), 1);
    }

    #[test]
    fn injected_release_clears_buttons() {
        let mut p = pad();
        assert!(p.inject("pad1 00ff"));
        assert!(p.inject("pad1 0000-release"));
        // The next strobe shifts zero buttons: 16 low reads.
        p.write(AccessWidth::Byte, Address(0x004016), 1, Tick(0));
        for _ in 0..16 {
            assert_eq!(p.read(AccessWidth::Byte, Address(0x004016), Tick(0)), 0);
        }
    }

    #[test]
    fn drained_reads_return_the_open_bus_high() {
        let mut p = pad();
        assert!(p.inject("pad1 0fff"));
        p.write(AccessWidth::Byte, Address(0x004016), 1, Tick(0));
        // The 12 set buttons drain first: the low bits answer 1 and
        // then the walk reads the zero fill.
        for wanted in [1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0] {
            assert_eq!(
                p.read(AccessWidth::Byte, Address(0x004016), Tick(0)),
                wanted
            );
        }
        // Past the 16 buttons the reads answer the open-bus high.
        for _ in 0..4 {
            assert_eq!(p.read(AccessWidth::Byte, Address(0x004016), Tick(0)), 1);
        }
        // Another strobe re-arms the walk.
        p.write(AccessWidth::Byte, Address(0x004016), 1, Tick(0));
        assert_eq!(p.read(AccessWidth::Byte, Address(0x004016), Tick(0)), 1);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut p = pad();
        assert!(p.inject("pad1 1234"));
        p.write(AccessWidth::Byte, Address(0x004016), 1, Tick(0));
        // Walk two reads so the drained counters are mid-walk.
        p.read(AccessWidth::Byte, Address(0x004016), Tick(0));
        p.read(AccessWidth::Byte, Address(0x004017), Tick(0));
        let snap = p.snapshot();
        let mut twin = pad();
        twin.inject("pad1 1234");
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        // The restored counters continue where the snapshot stood:
        // the next read lands the same byte on both sides.
        let byte_p = p.read(AccessWidth::Byte, Address(0x004016), Tick(0));
        let byte_t = twin.read(AccessWidth::Byte, Address(0x004016), Tick(0));
        assert_eq!(byte_p, byte_t);
        assert_eq!(twin.snapshot(), p.snapshot());
    }
}
