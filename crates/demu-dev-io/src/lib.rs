//! The IO device family: the SM83 timer, joypad, and catch-all
//! register block, the NES and Atari controller and support devices,
//! the Apple II keyboard and switch block, the Disk II drive, the
//! CIA and PIA support chips, the SNES controller port, the Lynx
//! Mikey die's timers, LCD line engine, joypad, cart page select
//! line, and serial surface, and the PC Engine turbo-tap pad.
//!
//! The SM83 timer runs on a free-running system counter that
//! increments once per T-cycle. The timer register `DIV` is the
//! counter's bits 7 to 14, the `TIMA` source is a falling edge of the
//! selected system-counter bit, and the audio frame sequencer rides a
//! falling edge 512 times a second. Writes to `DIV` and `TAC`
//! reproduce the falling-edge glitches of the real circuit by
//! scheduling an immediate visit, and the delivered edges reach the
//! CPU and the audio device over the declared connections.

pub mod apple;
pub mod cia;
pub mod disk2;
mod drive;
pub mod iwm;
pub mod mega2;
pub mod mikey;
pub mod nes;
pub mod pce;
pub mod pia;
pub mod riot;
pub mod snespad;

pub use apple::AppleIo;
pub use cia::Cia;
pub use disk2::Disk2;
pub use iwm::Iwm;
pub use mega2::Mega2;
pub use mikey::LynxMikey;
pub use nes::Controller;
pub use pce::PceTurbotap;
pub use pia::Pia;
pub use riot::Riot;
pub use snespad::SnesPad;

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The timer.
///
/// The internal T-cycle counter advances once per T-cycle and freezes
/// while the CPU is stopped or switching speed. `DIV` reads the counter
/// bits 7 to 14, `TIMA` ticks on the falling edge of the selected
/// source bit, and the audio frame sequencer advances on a falling edge
/// 512 times a second.
pub struct Timer {
    name: String,
    /// The T-cycle count at the anchor tick.
    t_anchor: u64,
    /// The anchor tick in domain dots.
    tick_anchor: u64,
    /// Domain dots per T-cycle: 1 at normal speed, 2 at double speed.
    pub ticks_per_t: u32,
    /// The counter is frozen while the CPU is stopped or switching.
    frozen: bool,
    /// The frozen T-cycle count.
    frozen_t: u64,
    /// The timer counter register `TIMA`.
    pub tima: u8,
    /// The modulo register `TMA`.
    pub tma: u8,
    /// The control register `TAC`.
    pub tac: u8,
    /// The pending reload tick, from an overflow four T-cycles back.
    reload_due: Option<Tick>,
    /// True when a `TIMA` write cancelled the pending reload.
    reload_cancelled: bool,
    last_tick: Option<Tick>,
    next: Option<Tick>,
    /// This machine is monochrome: the disable glitch applies.
    dmg: bool,
}

impl Timer {
    /// A timer from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            t_anchor: 0,
            tick_anchor: 0,
            ticks_per_t: 1,
            frozen: false,
            frozen_t: 0,
            tima: 0,
            tma: 0,
            tac: 0x00,
            reload_due: None,
            reload_cancelled: false,
            last_tick: None,
            next: None,
            dmg: device
                .params
                .get("dmg")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        }
    }

    /// The domain tick of one future T-cycle count.
    fn tick_of(&self, t: u64) -> Tick {
        Tick(self.tick_anchor + (t - self.t_anchor) * u64::from(self.ticks_per_t))
    }

    /// The T-cycle count the selected source bit falls on, after `from`.
    fn next_edge(&self, from: u64, period: u64) -> Option<u64> {
        let next = from.div_ceil(period) * period;
        (next > from).then_some(next)
    }

    /// The timer counts `TIMA`.
    fn enabled(&self) -> bool {
        self.tac & 0x04 != 0
    }

    /// The system-counter value at one domain tick.
    ///
    /// The free-running counter counts dot periods, so the `DIV` byte
    /// and the audio frame sequencer keep their frequencies across the
    /// speed switch; the `TIMA` source period is the one that changes.
    fn sys_at(&self, tick: Tick) -> u64 {
        if self.frozen {
            return self.frozen_t;
        }
        self.t_anchor + tick.0.saturating_sub(self.tick_anchor)
    }

    /// The `TIMA` source period in dots for the `TAC` select.
    fn source_period(&self) -> u64 {
        let base = match self.tac & 0x03 {
            0 => 1024,
            1 => 16,
            2 => 64,
            _ => 256,
        };
        base * u64::from(self.ticks_per_t)
    }

    /// Schedule the next visit at or after `tick`.
    fn schedule_next(&mut self, tick: Tick) {
        if self.frozen {
            self.next = None;
            return;
        }
        let t = self.sys_at(tick);
        let mut best: Option<u64> = None;
        // The audio frame sequencer edge, always active.
        if let Some(edge) = self.next_edge(t, 8192) {
            best = Some(edge);
        }
        if self.enabled() {
            let period = self.source_period();
            if let Some(edge) = self.next_edge(t, period) {
                best = Some(best.map_or(edge, |b| b.min(edge)));
            }
        }
        let edge_tick = best.map(|e| self.tick_of(e));
        if let Some(due) = self.reload_due {
            self.next = match edge_tick {
                Some(e) if e.0 < due.0 => Some(e),
                _ => Some(due),
            };
            return;
        }
        self.next = edge_tick;
    }

    /// Deliver one falling edge of the selected source bit.
    ///
    /// The `TIMA` register reads zero through the reload window, and the
    /// reload lands one M-cycle later.
    fn tima_edge(&mut self, tick: Tick) {
        let (tima, overflowed) = self.tima.overflowing_add(1);
        if !overflowed && tima != 0 {
            self.tima = tima;
            return;
        }
        self.tima = 0x00;
        self.reload_due = Some(Tick(tick.0 + 4 * u64::from(self.ticks_per_t)));
        self.reload_cancelled = false;
    }

    /// One edge visit at `tick`: process the due edges in order.
    fn visit(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let t = self.sys_at(tick);
        // The audio frame sequencer edge: the bit falls at multiples of
        // 8192 dots.
        if t.is_multiple_of(8192) {
            ctx.notify("div_apu", Level::High, tick);
        }
        if self.enabled() {
            let period = self.source_period();
            if t.is_multiple_of(period) {
                self.tima_edge(tick);
            }
        }
        if let Some(due) = self.reload_due
            && due.0 <= tick.0
            && !self.reload_cancelled
        {
            self.tima = self.tma;
            self.reload_due = None;
            ctx.notify("int", Level::High, tick);
        }
        self.schedule_next(tick);
    }

    /// Freeze the counter for a STOP or a speed switch.
    fn freeze(&mut self, tick: Tick) {
        self.frozen_t = self.sys_at(tick);
        self.frozen = true;
        self.next = None;
    }

    /// Unfreeze after stop or the switch.
    fn unfreeze(&mut self, tick: Tick) {
        self.frozen = false;
        self.tick_anchor = tick.0;
        self.schedule_next(tick);
    }

    /// The `DIV` register byte at the counter's current value.
    fn div_byte(&self, tick: Tick) -> u8 {
        ((self.sys_at(tick) >> 7) & 0xFF) as u8
    }

    /// True when the system-counter bit that selects the source is set.
    fn selected_bit(&self, t: u64) -> bool {
        let period = self.source_period();
        (t & (period * 2 - 1)) >= period
    }
}

impl Device for Timer {
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
        self.tima = 0;
        self.tac = 0x00;
        self.reload_due = None;
        self.reload_cancelled = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(b"TIMR");
        out.push(self.tima);
        out.push(self.tma);
        out.push(self.tac);
        out.extend_from_slice(&self.t_anchor.to_le_bytes());
        out.extend_from_slice(&self.tick_anchor.to_le_bytes());
        out.extend_from_slice(&self.ticks_per_t.to_le_bytes());
        out.push(u8::from(self.frozen));
        out.extend_from_slice(&self.frozen_t.to_le_bytes());
        out.push(u8::from(self.reload_due.is_some()));
        out.extend_from_slice(&self.reload_due.map(|t| t.0).unwrap_or(0).to_le_bytes());
        out.push(u8::from(self.reload_cancelled));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 4 + 3 + 8 + 8 + 4 + 1 + 8 + 1 + 8 + 1 {
            return Err(SnapshotError::Mismatch("timer chunk size".into()));
        }
        if &data[..4] != b"TIMR" {
            return Err(SnapshotError::Mismatch("bad timer tag".into()));
        }
        self.tima = data[4];
        self.tma = data[5];
        self.tac = data[6];
        self.t_anchor = u64::from_le_bytes(data[7..15].try_into().expect("8 bytes"));
        self.tick_anchor = u64::from_le_bytes(data[15..23].try_into().expect("8 bytes"));
        self.ticks_per_t =
            u32::from_le_bytes(data[23..27].try_into().expect("4 bytes")).clamp(1, 2);
        self.frozen = data[27] != 0;
        self.frozen_t = u64::from_le_bytes(data[28..36].try_into().expect("8 bytes"));
        self.reload_due = if data[36] != 0 {
            Some(Tick(u64::from_le_bytes(
                data[37..45].try_into().expect("8 bytes"),
            )))
        } else {
            None
        };
        self.reload_cancelled = data[45] != 0;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match line {
            "speed" => {
                self.ticks_per_t = if self.ticks_per_t == 1 { 2 } else { 1 };
            }
            "stop" => match level {
                Level::High => self.freeze(tick),
                Level::Low => self.unfreeze(tick),
            },
            _ => {}
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
        self.visit(tick, ctx);
    }
}

impl BusAccess for Timer {
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        match address.0 {
            0xFF04 => u32::from(self.div_byte(tick)),
            0xFF05 => u32::from(self.tima),
            0xFF06 => u32::from(self.tma),
            0xFF07 => u32::from(self.tac) | 0xF8,
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let v = value as u8;
        let t = self.sys_at(tick);
        match address.0 {
            0xFF04 => {
                // The write resets the counter. A set selected bit falls
                // early and delivers its edge now, as does the audio
                // sequencer bit; both come from the immediate visit.
                self.t_anchor = 0;
                self.tick_anchor = tick.0;
                self.reload_due = None;
                self.reload_cancelled = false;
                self.next = Some(tick);
                self.last_tick = None;
            }
            0xFF05 => {
                // A write before the reload tick cancels it and wins; a
                // write at or after the reload tick is ignored.
                if let Some(due) = self.reload_due {
                    if tick.0 < due.0 {
                        self.tima = v;
                        self.reload_due = None;
                    }
                } else {
                    self.tima = v;
                }
            }
            0xFF06 => {
                self.tma = v;
                if let Some(due) = self.reload_due
                    && tick.0 == due.0
                {
                    self.tima = v;
                }
            }
            0xFF07 => {
                let old = self.tac;
                self.tac = v & 0x07;
                // The monochrome circuit ticks once when the selected
                // bit changes state or the timer disables while set.
                let old_period = match old & 0x03 {
                    0 => 1024,
                    1 => 16,
                    2 => 64,
                    _ => 256,
                };
                let old_bit = (t & (old_period * 2 - 1)) >= old_period;
                let old_enabled = old & 0x04 != 0;
                let mut tick_now = false;
                if old_enabled && self.enabled() && old_bit && !self.selected_bit(t) {
                    tick_now = true;
                }
                if old_enabled && !self.enabled() && self.dmg && old_bit {
                    tick_now = true;
                }
                if tick_now {
                    self.tima_edge(tick);
                }
                self.next = tick_now.then_some(tick);
                self.last_tick = None;
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0xFF04 => self.div_byte(Tick(self.tick_anchor)),
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => (self.tac & 0x07) | 0xF8,
            _ => 0xFF,
        }
    }
}

/// The joypad.
///
/// The register `P1` selects which nibble of the button state the read
/// returns; a press raises the joypad interrupt line over the declared
/// connection.
pub struct Joypad {
    name: String,
    /// The action buttons: A, B, Select, Start.
    action: u8,
    /// The directions: Right, Left, Up, Down.
    dpad: u8,
    /// The stored select bits of `P1`.
    select: u8,
    /// The stored undocumented upper bits.
    upper: u8,
}

impl Joypad {
    /// A joypad from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            action: 0,
            dpad: 0,
            select: 0x30,
            upper: 0,
        }
    }
}

/// Apply one injected input event to a button mask.
///
/// The event grammar: `press <button>` or `release <button>` with the
/// button names `a`, `b`, `select`, `start`, `right`, `left`, `up`,
/// `down`.
fn inject_button(button: &str, pressed: bool) -> Option<(u8, u8)> {
    let (a, d) = match button.trim().to_ascii_lowercase().as_str() {
        "a" => (0x01, 0),
        "b" => (0x02, 0),
        "select" => (0x04, 0),
        "start" => (0x08, 0),
        "right" => (0, 0x01),
        "left" => (0, 0x02),
        "up" => (0, 0x04),
        "down" => (0, 0x08),
        _ => return None,
    };
    if pressed {
        Some((a, d))
    } else {
        Some((!a, !d))
    }
}

impl demu_core::InputSink for Joypad {
    fn inject(&mut self, event: &str) -> bool {
        let (verb, button) = event.split_once(' ').unwrap_or((event, ""));
        let pressed = match verb.trim().to_ascii_lowercase().as_str() {
            "press" => true,
            "release" => false,
            _ => return false,
        };
        let Some((a, d)) = inject_button(button, pressed) else {
            return false;
        };
        if pressed {
            self.action |= a;
            self.dpad |= d;
        } else {
            self.action &= !a;
            self.dpad &= !d;
        }
        true
    }
}

impl Device for Joypad {
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

    fn input_mut(&mut self) -> Option<&mut dyn demu_core::InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.action = 0;
        self.dpad = 0;
        self.select = 0x30;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(5);
        out.extend_from_slice(b"JOYP");
        out.push(self.action);
        out.push(self.dpad);
        out.push(self.select);
        out.push(self.upper);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 8 || &data[..4] != b"JOYP" {
            return Err(SnapshotError::Mismatch("bad joypad chunk".into()));
        }
        self.action = data[4];
        self.dpad = data[5];
        self.select = data[6];
        self.upper = data[7];
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        None
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

impl BusAccess for Joypad {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        if address.0 != 0xFF00 {
            return 0xFF;
        }
        let mut result = 0x0F;
        if self.select & 0x10 == 0 {
            result &= !self.action & 0x0F;
        }
        if self.select & 0x20 == 0 {
            result &= !self.dpad & 0x0F;
        }
        u32::from(result | (self.select & 0x30) | self.upper & 0xC0)
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let _ = tick;
        if address.0 != 0xFF00 {
            return;
        }
        self.select = value as u8 & 0x30;
        self.upper = (value as u8) & 0xC0;
    }

    fn peek(&self, address: Address) -> u8 {
        if address.0 != 0xFF00 {
            return 0xFF;
        }
        let mut result = 0x0F;
        if self.select & 0x10 == 0 {
            result &= !self.action & 0x0F;
        }
        if self.select & 0x20 == 0 {
            result &= !self.dpad & 0x0F;
        }
        result | (self.select & 0x30) | (self.upper & 0xC0)
    }
}

/// The catch-all register block for the IO range.
///
/// Every specific owner's window overlays this one with a higher
/// priority, so the device answers only the registers nobody else
/// claims: the speed switch, the infrared port, and the undocumented
/// CGB registers.
pub struct Io {
    name: String,
    cgb: bool,
    /// The stored register bytes by address, minus `0xFF00`.
    regs: [u8; 0x80],
}

impl Io {
    /// A catch-all register block from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let cgb = device
            .params
            .get("cgb")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            name: device.name.to_string(),
            cgb,
            regs: [0x00; 0x80],
        }
    }

    /// The index of one address inside the block.
    fn index(address: u32) -> Option<usize> {
        let offset = address.checked_sub(0xFF00)?;
        (offset < 0x80).then_some(offset as usize)
    }
}

impl BusAccess for Io {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let Some(i) = Self::index(address.0) else {
            return 0xFF;
        };
        match address.0 {
            0xFF4D => {
                if !self.cgb {
                    return 0xFF;
                }
                let speed = (self.regs[0x4D] >> 7) & 1;
                u32::from(0x7E | (speed << 7) | (self.regs[0x4D] & 0x01))
            }
            0xFF74 if !self.cgb => 0xFF,
            _ => u32::from(self.regs[i]),
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let Some(i) = Self::index(address.0) else {
            return;
        };
        match address.0 {
            0xFF4D => {
                if self.cgb {
                    // Only the arm bit is writable; the speed bit reads
                    // the current mode.
                    self.regs[0x4D] = ((self.regs[0x4D] >> 7) & 1) << 7 | (value as u8 & 0x01);
                }
            }
            0xFF74 if !self.cgb => {}
            0xFF75 => self.regs[i] = (self.regs[i] & 0x8F) | (value as u8 & 0x70),
            _ => {
                self.regs[i] = value as u8;
            }
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let Some(i) = Self::index(address.0) else {
            return 0xFF;
        };
        if address.0 == 0xFF4D && !self.cgb {
            return 0xFF;
        }
        match address.0 {
            0xFF4D => {
                let speed = (self.regs[0x4D] >> 7) & 1;
                0x7E | (speed << 7) | (self.regs[0x4D] & 0x01)
            }
            _ => self.regs[i],
        }
    }
}

impl Device for Io {
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
        self.regs = [0x00; 0x80];
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        if self.cgb {
            self.regs[0x4D] = 0x7E;
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(5 + self.regs.len());
        out.extend_from_slice(b"IOBL");
        out.push(u8::from(self.cgb));
        out.extend_from_slice(&self.regs);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 4 + 1 + self.regs.len() || &data[..4] != b"IOBL" {
            return Err(SnapshotError::Mismatch("bad io chunk".into()));
        }
        self.cgb = data[4] != 0;
        self.regs.copy_from_slice(&data[5..]);
        Ok(())
    }

    fn notify_line(&mut self, line: &str, _level: Level, _tick: Tick) {
        // The speed switch updates the stored speed bit of KEY1.
        if line == "speed" {
            let bit = (self.regs[0x4D] >> 7) & 1;
            self.regs[0x4D] = (!bit & 1) << 7;
        }
    }

    fn next_event(&self) -> Option<Tick> {
        None
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}
