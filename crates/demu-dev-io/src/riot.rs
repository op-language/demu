//! The RIOT 6532 of the 2600 and the 7800.
//!
//! One device carries the three units of the 6532: 128 bytes of RAM,
//! the interval timer, and the switch and port registers. The register
//! decode follows the chip pins: the offset is `addr & 7` for the port
//! and timer registers, bit 2 selects the timer section, and bit 4
//! selects the timer-write side with its interval in bits 1-0. Ports A
//! and B sit at the offsets 0-3 as data and data-direction pairs,
//! `INTIM` reads at the offset 4, and `INTFLG` reads at the offset 5.
//!
//! The timer counts down once per interval. It holds zero for one
//! interval, sets the flag, rolls to `$FF`, and then decrements once
//! per CPU cycle until a read of `INTIM` or `INTFLG` clears the flag
//! and reverts the decrement to the interval form. The exact hardware
//! edge alignment of that revert is approximate; the visible state
//! sequence matches the chip. The interrupt output pin of the 6532
//! stays unused because no console in this set connects it.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The timer's decrement phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerPhase {
    /// One decrement per interval.
    Count,
    /// The value holds zero for one interval before the wrap.
    ZeroHold,
    /// One decrement per CPU cycle after the wrap to `$FF`.
    Fast,
}

/// The interval timer, port registers, and RAM of the 6532.
#[derive(Debug)]
pub struct Riot {
    name: String,
    /// Domain ticks per CPU cycle, from the `cpu` divider entry.
    cpu_div: u64,
    /// The 128 bytes of RAM.
    ram: Box<[u8; 0x80]>,
    /// Port A output latch.
    swcha: u8,
    /// Port A data-direction register.
    ddra: u8,
    /// Port A external pin state that loose inputs read.
    ext_a: u8,
    /// Port B output latch.
    swchb: u8,
    /// Port B data-direction register.
    ddrb: u8,
    /// Port B external pin state that loose inputs read.
    ext_b: u8,
    /// The timer value.
    timer: u8,
    /// The timer phase.
    phase: TimerPhase,
    /// CPU cycles per decrement in the `Count` phase.
    interval: u64,
    /// True while the timer flag bit is set.
    flag: bool,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Riot {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let cpu_div = u64::from(device.dividers.get("cpu").copied().unwrap_or(1));
        Self {
            name: device.name.to_string(),
            cpu_div,
            ram: Box::new([0; 0x80]),
            swcha: 0,
            ddra: 0,
            ext_a: 0xFF,
            swchb: 0,
            ddrb: 0,
            ext_b: 0x0B,
            timer: 0,
            phase: TimerPhase::Count,
            interval: 1024,
            flag: false,
            next: None,
            last_tick: None,
        }
    }

    /// The port A pins: driven bits read the latch, loose bits the
    /// external switch state.
    fn port_a(&self) -> u8 {
        (self.swcha & self.ddra) | (self.ext_a & !self.ddra)
    }

    /// The port B pins.
    fn port_b(&self) -> u8 {
        (self.swchb & self.ddrb) | (self.ext_b & !self.ddrb)
    }

    /// Read the register at the raw window offset. The read of the
    /// timer and of the flag clears the flag and reverts the fast
    /// wrap mode to the interval form.
    fn reg_read(&mut self, offset: u8) -> u8 {
        match offset & 0x1F {
            0 => self.port_a(),
            1 => self.ddra,
            2 => self.port_b(),
            3 => self.ddrb,
            4 => {
                self.flag = false;
                self.revert_if_fast();
                self.timer
            }
            5 => {
                let out = u8::from(self.flag) << 7;
                self.flag = false;
                self.revert_if_fast();
                out
            }
            _ => 0,
        }
    }

    /// The revert from the per-cycle wrap mode back to the interval
    /// form, applied on a read of the timer or the flag.
    fn revert_if_fast(&mut self) {
        if self.phase == TimerPhase::Fast {
            self.phase = TimerPhase::Count;
        }
    }

    /// Write the register at the raw window offset.
    fn reg_write(&mut self, offset: u8, value: u8) {
        match offset & 0x1F {
            1 => self.ddra = value,
            3 => self.ddrb = value,
            // The writes at the offsets 4-7 with bit 4 set load the
            // timer and pick the interval. The bare offset 4 write is
            // the PA7 edge-detect control, which stays unmodeled.
            0x14 => self.arm_timer(value, 1),
            0x15 => self.arm_timer(value, 8),
            0x16 => self.arm_timer(value, 64),
            0x17 => self.arm_timer(value, 1024),
            _ => {}
        }
    }

    /// Load the timer value and the interval in CPU cycles.
    fn arm_timer(&mut self, value: u8, interval_cpu_cycles: u64) {
        self.timer = value;
        self.interval = interval_cpu_cycles;
        self.phase = TimerPhase::Count;
        self.flag = false;
    }

    /// One timer decrement at a CPU-cycle visit. Returns the CPU
    /// cycles until the next decrement; the Count and ZeroHold phases
    /// wait one interval, the Fast phase one CPU cycle.
    fn timer_step(&mut self) -> u64 {
        match self.phase {
            TimerPhase::Count => {
                if self.timer > 0 {
                    self.timer -= 1;
                }
                if self.timer == 0 {
                    self.phase = TimerPhase::ZeroHold;
                }
                self.interval * self.cpu_div.max(1)
            }
            TimerPhase::ZeroHold => {
                self.flag = true;
                self.timer = 0xFF;
                self.phase = TimerPhase::Fast;
                self.interval * self.cpu_div.max(1)
            }
            TimerPhase::Fast => {
                self.timer = self.timer.wrapping_sub(1);
                self.cpu_div.max(1)
            }
        }
    }
}

impl Riot {
    /// Apply one joystick or console-switch input event. A bare event
    /// name presses: it clears the active-low direction or switch bit.
    /// The `-release` suffix lifts the bit again. The switch names are
    /// `select` and `reset`; the joystick names carry the port.
    fn inject_event(&mut self, event: &str) -> bool {
        let form = event.trim().to_ascii_lowercase();
        let (name, release) = match form.strip_suffix("-release") {
            Some(n) => (n, true),
            None => (form.as_str(), false),
        };
        let (mask, is_b) = match name {
            "p0-up" => (0x40, false),
            "p0-down" => (0x80, false),
            "p0-left" => (0x20, false),
            "p0-right" => (0x10, false),
            "p1-up" => (0x04, false),
            "p1-down" => (0x08, false),
            "p1-left" => (0x02, false),
            "p1-right" => (0x01, false),
            "select" => (0x01, true),
            "reset" => (0x02, true),
            _ => return false,
        };
        let field: &mut u8 = if is_b {
            &mut self.ext_b
        } else {
            &mut self.ext_a
        };
        if release {
            *field |= mask;
        } else {
            *field &= !mask;
        }
        true
    }
}

impl BusAccess for Riot {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.reg_read((address.0 & 0x1F) as u8))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        // The register page selects on bits 9 and 7 together; every
        // other address is one of the RAM pages.
        if address.0 & 0x280 != 0x280 {
            self.ram[(address.0 & 0x7F) as usize] = value as u8;
            return;
        }
        self.reg_write((address.0 & 0x1F) as u8, value as u8);
    }

    fn peek(&self, address: Address) -> u8 {
        if address.0 & 0x280 != 0x280 {
            return self.ram[(address.0 & 0x7F) as usize];
        }
        match address.0 & 0x1F {
            0 => self.port_a(),
            1 => self.ddra,
            2 => self.port_b(),
            3 => self.ddrb,
            4 | 5 => self.timer,
            _ => 0,
        }
    }
}

impl Device for Riot {
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
        self.ram.iter_mut().for_each(|b| *b = 0);
        self.swcha = 0;
        self.ddra = 0;
        self.ext_a = 0xFF;
        self.swchb = 0;
        self.ddrb = 0;
        self.ext_b = 0x0B;
        self.timer = 0;
        self.phase = TimerPhase::Count;
        self.interval = 1024;
        self.flag = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Ram { pattern }) = state {
            demu_core::boot::fill_ram(self.ram.as_mut(), *pattern);
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(0x80 + 16);
        out.extend_from_slice(b"RIOT");
        out.push(1);
        out.extend_from_slice(&self.swcha.to_le_bytes());
        out.extend_from_slice(&self.ddra.to_le_bytes());
        out.extend_from_slice(&self.ext_a.to_le_bytes());
        out.extend_from_slice(&self.swchb.to_le_bytes());
        out.extend_from_slice(&self.ddrb.to_le_bytes());
        out.extend_from_slice(&self.ext_b.to_le_bytes());
        out.push(self.timer);
        out.push(match self.phase {
            TimerPhase::Count => 0,
            TimerPhase::ZeroHold => 1,
            TimerPhase::Fast => 2,
        });
        out.extend_from_slice(&self.interval.to_le_bytes());
        out.push(u8::from(self.flag));
        out.extend_from_slice(&self.ram[..]);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 6 + 2 + 8 + 1 + 0x80;
        if data.len() != expected || &data[..4] != b"RIOT" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "riot chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.swcha = data[pos];
        pos += 1;
        self.ddra = data[pos];
        pos += 1;
        self.ext_a = data[pos];
        pos += 1;
        self.swchb = data[pos];
        pos += 1;
        self.ddrb = data[pos];
        pos += 1;
        self.ext_b = data[pos];
        pos += 1;
        self.timer = data[pos];
        pos += 1;
        self.phase = match data[pos] {
            1 => TimerPhase::ZeroHold,
            2 => TimerPhase::Fast,
            _ => TimerPhase::Count,
        };
        pos += 1;
        self.interval = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.flag = data[pos] != 0;
        pos += 1;
        self.ram.copy_from_slice(&data[pos..pos + 0x80]);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, _ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        let after = self.timer_step();
        self.next = Some(Tick(tick.0 + after.max(1)));
    }
}

impl InputSink for Riot {
    fn inject(&mut self, event: &str) -> bool {
        self.inject_event(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("riot").expect("name"),
            kind: demu_config::newtypes::KindName::new("atari.riot").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 3)].into_iter().collect(),
        }
    }

    #[test]
    fn timer_counts_at_interval_then_wraps() {
        let mut riot = Riot::new(&instance());
        riot.boot(None);
        riot.arm_timer(1, 8);
        // The value decrements once after the interval, then holds
        // zero for one more interval; the steps return CPU cycles
        // (the interval of 8 cycles at the divider of 3).
        assert_eq!(riot.timer_step(), 24);
        assert_eq!(riot.timer, 0);
        assert_eq!(riot.phase, TimerPhase::ZeroHold);
        // The hold interval ends: the flag sets and the value rolls
        // to `$FF` with the per-cycle decrement.
        assert_eq!(riot.timer_step(), 24);
        assert!(riot.flag);
        assert_eq!(riot.timer, 0xFF);
        assert_eq!(riot.phase, TimerPhase::Fast);
        assert_eq!(riot.timer_step(), 3);
        assert_eq!(riot.timer, 0xFE);
    }

    #[test]
    fn read_clears_flag_and_reverts_mode() {
        let mut riot = Riot::new(&instance());
        riot.boot(None);
        riot.arm_timer(0, 8);
        riot.timer_step();
        assert_eq!(riot.phase, TimerPhase::ZeroHold);
        riot.timer_step();
        assert_eq!(riot.phase, TimerPhase::Fast);
        assert!(riot.flag);
        assert_eq!(riot.reg_read(4), 0xFF);
        assert!(!riot.flag);
        assert_eq!(riot.phase, TimerPhase::Count);
    }

    #[test]
    fn flag_read_carries_bit_seven() {
        let mut riot = Riot::new(&instance());
        riot.boot(None);
        riot.arm_timer(0, 8);
        riot.timer_step();
        riot.timer_step();
        assert_eq!(riot.reg_read(5), 0x80);
        assert!(!riot.flag);
    }

    #[test]
    fn ram_decodes_off_the_register_page() {
        let mut riot = Riot::new(&instance());
        // The RAM pages carry bit 7 without bit 9.
        riot.write(AccessWidth::Byte, Address(0x1EF), 0x42, Tick(0));
        assert_eq!(riot.peek(Address(0x1EF)), 0x42);
        assert_eq!(riot.peek(Address(0x0EF)), 0x42);
        // The register page ($xx80-$xx9F) decodes the data-direction
        // register, mirrored across the page.
        riot.write(AccessWidth::Byte, Address(0x0281), 0x55, Tick(0));
        assert_eq!(riot.peek(Address(0x0281)), 0x55);
    }

    #[test]
    fn joystick_bits_are_active_low() {
        let mut riot = Riot::new(&instance());
        assert!(riot.inject("p0-up"));
        assert_eq!(riot.port_a() & 0x40, 0, "up press is a zero bit");
        assert_eq!(riot.port_a() & 0x10, 0x10, "right released is one");
        assert!(riot.inject("reset"));
        assert_eq!(riot.port_b() & 0x02, 0, "the reset switch presses low");
        assert!(riot.inject("p0-up-release"));
        assert_eq!(riot.port_a(), 0xFF, "the lifted direction reads high");
    }

    #[test]
    fn direction_register_forces_latch_bits() {
        let mut riot = Riot::new(&instance());
        riot.reg_write(1, 0xFF);
        assert_eq!(riot.port_a(), 0);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut riot = Riot::new(&instance());
        riot.boot(None);
        riot.arm_timer(0x37, 64);
        riot.timer_step();
        let snapshot = riot.snapshot();
        let mut twin = Riot::new(&instance());
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.snapshot(), snapshot);
        assert_eq!(twin.timer, riot.timer);
        assert_eq!(twin.phase, riot.phase);
    }
}
