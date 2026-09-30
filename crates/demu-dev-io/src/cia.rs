//! The CIA 6526 of the Commodore 64.
//!
//! One struct serves both CIAs: the `nmi` parameter makes the device a
//! CIA2, which drives the CPU NMI line on its underflows instead of the
//! IRQ line. The register file sits at the 16-byte stride, so the device
//! decodes by the low four bits of the bus address. Offsets 0 to 3 hold
//! the two ports with their data-direction registers, 4 to 7 the two
//! timer counts, 8 to B the time-of-day fields, C the serial shift
//! register, D the interrupt control and status, and E/F the two timer
//! control bytes.
//!
//! The engine visits one CPU cycle at a time on the domain grid: on
//! each visit the started timers burn one countdown step, and the
//! time-of-day ticks along at the configured tod divider (60 Hz on
//! NTSC, 50 Hz on PAL).
//!
//! Timer semantics: a write to a count register goes to the latch, and
//! while the timer is stopped the write reloads the counter too. The
//! start edge and the force-load bit copy the latch into the counter.
//! The count burns down one CPU cycle at a step; reaching zero sets the
//! ICR flag, reloads from the latch in the same step, or, with the
//! one-shot bit set, stops the timer and clears its start bit.
//!
//! Deliberate simplifications:
//!
//! - Timer B always burns on the system clock; the input-processor and
//!   cascade modes of CRB bits 5-6 are not modeled.
//! - The serial shift register stores its byte and never shifts.
//! - The TOD keeps plain BCD fields without the 12-hour or PM forms,
//!   and register writes go straight to the counter fields. Writes
//!   with CRB bit 7 set go into the alarm registers instead, and the
//!   alarm compares latch ICR bit 2.
//! - The keyboard matrix is a 64-bit pressed set: PRA drives rows low,
//!   PRB reads columns, and no input changes the schedule.
//! - The CIA2 PRA writes the LSSI latch bits 0-1 carry the VIC bank
//!   select (VA14/VA15); the device does not route them elsewhere.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// One countdown unit of the chip.
#[derive(Debug, Clone, Copy, Default)]
struct CiaTimer {
    /// The remaining CPU cycles until the underflow.
    counter: u16,
    /// The reload latch.
    latch: u16,
}

/// The state of one 6526.
#[derive(Debug)]
pub struct Cia {
    name: String,
    /// Domain ticks per CPU cycle.
    cpu_div: u64,
    /// True on a CIA2: the interrupt output is the NMI line.
    nmi: bool,
    ta: CiaTimer,
    tb: CiaTimer,
    cra: u8,
    crb: u8,
    pra: u8,
    prb: u8,
    ddra: u8,
    ddrb: u8,
    sdr: u8,
    /// The TOD fields: tenths, seconds, minutes, hours, in BCD.
    tod: [u8; 4],
    /// The alarm fields, written through CRB bit 7.
    alarm: [u8; 4],
    /// CPU cycles per TOD increment; 60 Hz on NTSC, 50 Hz on PAL.
    tod_div: u64,
    /// TOD cycles accumulated toward the next increment.
    tod_acc: u64,
    icr_latch: u8,
    icr_mask: u8,
    /// The pressed keyboard keys, the bit index row*8+column.
    keys: u64,
    /// The domain tick of the next CPU-cycle step.
    step_due: u64,
    /// The interrupt level currently driven on the line.
    irq_out: bool,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Cia {
    /// The ICR flag bits: TA, TB, the alarm, and the serial and flag
    /// pins, which never latch in this model.
    const ICR_TA: u8 = 0x01;
    const ICR_TB: u8 = 0x02;
    const ICR_ALARM: u8 = 0x04;

    /// A CIA from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let pal = device
            .params
            .get("pal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let region_tod = if pal { 19_705u64 } else { 17_045u64 };
        let tod_div = device
            .params
            .get("tod_div")
            .and_then(|v| v.as_integer())
            .and_then(|v| u64::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(region_tod);
        Self {
            name: device.name.to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1),
            nmi: device
                .params
                .get("nmi")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            ta: CiaTimer::default(),
            tb: CiaTimer::default(),
            cra: 0,
            crb: 0,
            pra: 0,
            prb: 0,
            ddra: 0,
            ddrb: 0,
            sdr: 0,
            tod: [0; 4],
            alarm: [0; 4],
            tod_div,
            tod_acc: 0,
            icr_latch: 0,
            icr_mask: 0,
            keys: 0,
            step_due: 0,
            irq_out: false,
            next: None,
            last_tick: None,
        }
    }

    /// The line this device drives.
    fn int_line(&self) -> &'static str {
        if self.nmi { "nmi" } else { "irq" }
    }

    /// True when any unmasked source latches.
    fn irq_wanted(&self) -> bool {
        (self.icr_latch & self.icr_mask & 0x1F) != 0
    }

    /// Burn one CPU cycle on one started timer; returns true on the
    /// underflow, when the remaining count reaches zero.
    fn burn(t: &mut CiaTimer) -> bool {
        if t.counter > 0 {
            t.counter -= 1;
        }
        t.counter == 0
    }

    /// One CPU cycle: the started timers burn one step, and the TOD
    /// advances along its divider. Latching flags stay in the register
    /// until the software acknowledges them.
    fn step_cycle(&mut self) {
        if self.cra & 0x01 != 0 && Self::burn(&mut self.ta) {
            if self.cra & 0x04 != 0 {
                // The one-shot stops the timer and clears the start
                // bit; the count rests at zero.
                self.cra &= !0x01;
            } else {
                self.ta.counter = self.ta.latch;
            }
            self.icr_latch |= Self::ICR_TA;
        }
        if self.crb & 0x01 != 0 && Self::burn(&mut self.tb) {
            if self.crb & 0x04 != 0 {
                self.crb &= !0x01;
            } else {
                self.tb.counter = self.tb.latch;
            }
            self.icr_latch |= Self::ICR_TB;
        }
        self.tod_acc += 1;
        if self.tod_acc >= self.tod_div {
            self.tod_acc -= self.tod_div;
            self.tod_advance();
        }
    }

    /// Advance the TOD one tenth of a second and compare the alarm.
    fn tod_advance(&mut self) {
        if self.tod[0] < 9 {
            self.tod[0] += 1;
        } else {
            // The tenths wrap: the seconds tick, then the minutes and
            // the hours BCD-up with the wrap at 59 and 23.
            self.tod[0] = 0;
            if self.tod[1] < 0x59 {
                self.tod[1] = Self::bcd_inc(self.tod[1]);
            } else {
                self.tod[1] = 0;
                if self.tod[2] < 0x59 {
                    self.tod[2] = Self::bcd_inc(self.tod[2]);
                } else {
                    self.tod[2] = 0;
                    if self.tod[3] == 0x23 {
                        self.tod[3] = 0;
                    } else {
                        self.tod[3] = Self::bcd_inc(self.tod[3]);
                    }
                }
            }
            if self.tod == self.alarm {
                self.icr_latch |= Self::ICR_ALARM;
            }
        }
    }

    /// One BCD digit add with the carry into the tens digit.
    fn bcd_inc(v: u8) -> u8 {
        if v & 0x0F == 9 {
            (v & 0xF0) + 0x10
        } else {
            (v & 0xF0) | ((v & 0x0F) + 1)
        }
    }

    /// The external keyboard state for port B: a pressed key pulls its
    /// column bit low when any row that key sits on is driven low.
    fn keyboard_ext(&self) -> u8 {
        let mut out = 0xFFu8;
        for code in 0..64u8 {
            if self.keys >> code & 1 == 0 {
                continue;
            }
            let row = code / 8;
            let col = code % 8;
            if self.pra >> row & 1 == 0 {
                out &= !(1 << col);
            }
        }
        out
    }

    /// The register read at the low four bits.
    fn reg_read(&mut self, r: usize) -> u8 {
        match r {
            // The port bits: the driven outputs read the latch, the
            // released inputs float high.
            0 => (self.pra & self.ddra) | !self.ddra,
            1 => {
                let ext = self.keyboard_ext();
                (self.prb & self.ddrb) | (ext & !self.ddrb)
            }
            2 => self.ddra,
            3 => self.ddrb,
            4 => self.ta.counter as u8,
            5 => (self.ta.counter >> 8) as u8,
            6 => self.tb.counter as u8,
            7 => (self.tb.counter >> 8) as u8,
            8 => self.tod[0],
            9 => self.tod[1],
            10 => self.tod[2],
            11 => self.tod[3],
            12 => self.sdr,
            13 => {
                // The flag bits read and clear; bit 7 is the composite
                // request of all unmasked sources.
                let out = self.icr_latch | u8::from(self.irq_wanted()) << 7;
                self.icr_latch = 0;
                out
            }
            14 => self.cra,
            _ => self.crb,
        }
    }

    /// The pure register view for debug commands, without the read
    /// side effects.
    fn reg_peek(&self, r: usize) -> u8 {
        match r {
            0 => (self.pra & self.ddra) | !self.ddra,
            1 => {
                let ext = self.keyboard_ext();
                (self.prb & self.ddrb) | (ext & !self.ddrb)
            }
            13 => self.icr_latch | u8::from(self.irq_wanted()) << 7,
            r => self.plain_reg(r),
        }
    }

    /// The stored byte of a register without side effects.
    fn plain_reg(&self, r: usize) -> u8 {
        match r {
            2 => self.ddra,
            3 => self.ddrb,
            4 => self.ta.counter as u8,
            5 => (self.ta.counter >> 8) as u8,
            6 => self.tb.counter as u8,
            7 => (self.tb.counter >> 8) as u8,
            8 => self.tod[0],
            9 => self.tod[1],
            10 => self.tod[2],
            11 => self.tod[3],
            12 => self.sdr,
            14 => self.cra,
            _ => self.crb,
        }
    }

    /// The register write at the low four bits.
    fn reg_write(&mut self, r: usize, v: u8, tick: Tick) {
        match r {
            0 => self.pra = v,
            1 => self.prb = v,
            2 => self.ddra = v,
            3 => self.ddrb = v,
            4 => {
                self.ta.latch = (self.ta.latch & 0xFF00) | u16::from(v);
                if self.cra & 0x01 == 0 {
                    self.ta.counter = self.ta.latch;
                }
            }
            5 => {
                self.ta.latch = (self.ta.latch & 0x00FF) | (u16::from(v) << 8);
                if self.cra & 0x01 == 0 {
                    self.ta.counter = self.ta.latch;
                }
            }
            6 => {
                self.tb.latch = (self.tb.latch & 0xFF00) | u16::from(v);
                if self.crb & 0x01 == 0 {
                    self.tb.counter = self.tb.latch;
                }
            }
            7 => {
                self.tb.latch = (self.tb.latch & 0x00FF) | (u16::from(v) << 8);
                if self.crb & 0x01 == 0 {
                    self.tb.counter = self.tb.latch;
                }
            }
            8..=11 => {
                // CRB bit 7 routes the writes into the alarm registers.
                let field = if self.crb & 0x80 != 0 {
                    &mut self.alarm
                } else {
                    &mut self.tod
                };
                field[r - 8] = v;
            }
            12 => self.sdr = v,
            13 => {
                // Bit 7 of the write selects set or clear of the mask.
                if v & 0x80 != 0 {
                    self.icr_mask |= v & 0x1F;
                } else {
                    self.icr_mask &= !(v & 0x1F);
                }
                self.poke_level(tick);
            }
            14 => {
                let was = self.cra & 0x01 != 0;
                self.cra = v & 0x0F;
                if v & 0x10 != 0 {
                    self.ta.counter = self.ta.latch;
                }
                if !was && self.cra & 0x01 != 0 {
                    self.ta.counter = self.ta.latch;
                }
            }
            _ => {
                let was = self.crb & 0x01 != 0;
                // Bit 4 is the force-load strobe; the alarm select of
                // bit 7 and the input modes of bits 5-6 store.
                self.crb = v & !0x10;
                if v & 0x10 != 0 {
                    self.tb.counter = self.tb.latch;
                }
                if !was && self.crb & 0x01 != 0 {
                    self.tb.counter = self.tb.latch;
                }
            }
        }
    }

    /// The level re-check after the software acknowledged the flags or
    /// changed the mask: arm an immediate re-visit so the interrupt
    /// line follows within the same tick.
    fn poke_level(&mut self, tick: Tick) {
        if self.irq_wanted() != self.irq_out {
            let forced = Tick(tick.0 + 1);
            if self.next.is_none_or(|n| n.0 > forced.0) {
                self.next = Some(forced);
            }
        }
    }
}

impl BusAccess for Cia {
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        let out = self.reg_read((address.0 & 0x0F) as usize);
        if address.0 & 0x0F == 0x0D {
            // The acknowledge read may drop the line; re-visit now.
            self.poke_level(tick);
        }
        u32::from(out)
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        self.reg_write((address.0 & 0x0F) as usize, value as u8, tick);
    }

    fn peek(&self, address: Address) -> u8 {
        self.reg_peek((address.0 & 0x0F) as usize)
    }
}

impl InputSink for Cia {
    fn inject(&mut self, event: &str) -> bool {
        // The grammar: `key <code>` presses and `key <code>-release`
        // lifts, with the code = the row times 8 plus the column.
        let form = event.trim().to_ascii_lowercase();
        if let Some(code) = form
            .strip_prefix("key ")
            .and_then(|v| v.strip_suffix("-release"))
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| *v < 64)
        {
            self.keys &= !(1 << code);
            return true;
        }
        if let Some(code) = form
            .strip_prefix("key ")
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|v| *v < 64)
        {
            self.keys |= 1 << code;
            return true;
        }
        false
    }
}

impl Device for Cia {
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
        if self.nmi { None } else { Some(self) }
    }

    fn reset(&mut self) {
        self.ta = CiaTimer::default();
        self.tb = CiaTimer::default();
        self.cra = 0;
        self.crb = 0;
        self.pra = 0;
        self.prb = 0;
        self.ddra = 0;
        self.ddrb = 0;
        self.sdr = 0;
        self.tod = [0; 4];
        self.alarm = [0; 4];
        self.tod_acc = 0;
        self.icr_latch = 0;
        self.icr_mask = 0;
        self.keys = 0;
        self.step_due = 0;
        self.irq_out = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.step_due = 0;
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(57);
        out.extend_from_slice(b"CIA6");
        out.push(1);
        out.push(self.pra);
        out.push(self.prb);
        out.push(self.ddra);
        out.push(self.ddrb);
        out.push(self.sdr);
        out.push(self.cra);
        out.push(self.crb);
        out.extend_from_slice(&self.ta.counter.to_le_bytes());
        out.extend_from_slice(&self.ta.latch.to_le_bytes());
        out.extend_from_slice(&self.tb.counter.to_le_bytes());
        out.extend_from_slice(&self.tb.latch.to_le_bytes());
        out.push(self.icr_latch);
        out.push(self.icr_mask);
        out.extend_from_slice(&self.tod);
        out.extend_from_slice(&self.alarm);
        out.extend_from_slice(&self.tod_acc.to_le_bytes());
        out.extend_from_slice(&self.keys.to_le_bytes());
        out.extend_from_slice(&self.step_due.to_le_bytes());
        out.push(u8::from(self.irq_out));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 5 + 2 + 8 + 2 + 8 + 8 + 8 + 8 + 1;
        if data.len() != expected || &data[..4] != b"CIA6" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "cia chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.pra = data[pos];
        pos += 1;
        self.prb = data[pos];
        pos += 1;
        self.ddra = data[pos];
        pos += 1;
        self.ddrb = data[pos];
        pos += 1;
        self.sdr = data[pos];
        pos += 1;
        self.cra = data[pos];
        pos += 1;
        self.crb = data[pos];
        pos += 1;
        self.ta.counter = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.ta.latch = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.tb.counter = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.tb.latch = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.icr_latch = data[pos];
        pos += 1;
        self.icr_mask = data[pos];
        pos += 1;
        self.tod.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.alarm.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.tod_acc = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.keys = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.step_due = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.irq_out = data[pos] != 0;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        // The step grid keeps the CPU-cycle visits aligned; a re-seeded
        // level visit lands between grid points and burns nothing.
        if tick.0 >= self.step_due {
            self.step_cycle();
            self.step_due = tick.0 + self.cpu_div;
        }
        let want = self.irq_wanted();
        if want != self.irq_out {
            self.irq_out = want;
            ctx.notify(
                self.int_line(),
                if want { Level::High } else { Level::Low },
                tick,
            );
        }
        self.next = Some(Tick(self.step_due));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::InputSink;

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("cia1").expect("name"),
            kind: demu_config::newtypes::KindName::new("c64.cia").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    fn wr(c: &mut Cia, addr: u32, val: u8) {
        c.write(AccessWidth::Byte, Address(addr), u32::from(val), Tick(0));
    }

    fn rd(c: &mut Cia, addr: u32) -> u8 {
        c.read(AccessWidth::Byte, Address(addr), Tick(0)) as u8
    }

    /// Burn `n` CPU cycles through the step engine.
    fn burn(c: &mut Cia, n: usize) {
        for _ in 0..n {
            c.step_cycle();
        }
    }

    #[test]
    fn counter_writes_latch_and_reload_when_stopped() {
        let mut c = Cia::new(&instance());
        wr(&mut c, 0xDD04, 0x11);
        wr(&mut c, 0xDD05, 0x23);
        assert_eq!(c.ta.latch, 0x2311);
        assert_eq!(c.ta.counter, 0x2311, "the stopped write reloads too");
        assert_eq!(rd(&mut c, 0xDD04), 0x11);
        assert_eq!(rd(&mut c, 0xDD05), 0x23);
        // The start edge loads the counter; the burns then count down.
        wr(&mut c, 0xDD0E, 0x01);
        burn(&mut c, 3);
        assert_eq!(rd(&mut c, 0xDD04), 0x0E);
        assert_eq!(rd(&mut c, 0xDD05), 0x23);
    }

    #[test]
    fn underflow_latches_flag_and_reloads() {
        let mut c = Cia::new(&instance());
        wr(&mut c, 0xDD04, 0x03); // The latch of 3.
        wr(&mut c, 0xDD0E, 0x01);
        burn(&mut c, 5);
        assert!(c.icr_latch & Cia::ICR_TA != 0, "a flag latched");
        assert_eq!(c.ta.counter, 0x01, "the count reloaded and kept burning");
        // The mask raises the request; the read acknowledges it.
        wr(&mut c, 0xDD0D, 0x81); // Bit 7 set: this write sets the mask.
        assert!(c.irq_wanted());
        assert_eq!(rd(&mut c, 0xDD0D) & 0x7F, 0x01);
        assert!(!c.irq_wanted(), "the acknowledge read cleared the flags");
        // The timer keeps burning after the reload.
        assert_eq!(c.ta.counter, 0x01);
    }

    #[test]
    fn one_shot_stops_after_underflow() {
        let mut c = Cia::new(&instance());
        wr(&mut c, 0xDD04, 0x05);
        wr(&mut c, 0xDD0E, 0x05); // Start plus one-shot.
        burn(&mut c, 6);
        assert_eq!(c.icr_latch & Cia::ICR_TA, Cia::ICR_TA);
        assert!(c.cra & 0x01 == 0, "the one-shot cleared the start bit");
        assert_eq!(c.ta.counter, 0);
        burn(&mut c, 20);
        assert_eq!(c.icr_latch & Cia::ICR_TA, Cia::ICR_TA, "no further latches");
    }

    #[test]
    fn force_load_reloads_running_counter() {
        let mut c = Cia::new(&instance());
        wr(&mut c, 0xDD04, 0x42);
        wr(&mut c, 0xDD0E, 0x01);
        burn(&mut c, 2);
        assert_eq!(rd(&mut c, 0xDD04), 0x40);
        wr(&mut c, 0xDD0E, 0x10);
        assert_eq!(
            rd(&mut c, 0xDD04),
            0x42,
            "the force load restored the latch"
        );
    }

    #[test]
    fn timer_b_counts_down_the_same_way() {
        let mut c = Cia::new(&instance());
        wr(&mut c, 0xDD06, 0x02);
        wr(&mut c, 0xDD0F, 0x01);
        burn(&mut c, 2);
        assert_eq!(c.icr_latch & Cia::ICR_TB, Cia::ICR_TB);
        assert_eq!(
            rd(&mut c, 0xDD06),
            0x02,
            "the count reloaded from the latch"
        );
        assert_eq!(rd(&mut c, 0xDD07), 0x00);
    }

    #[test]
    fn tod_counts_bcd_and_alarms() {
        let mut c = Cia::new(&instance());
        c.tod_div = 3;
        // Write the time: 0x08 tenths, 0x01 seconds, 0x02 minutes, 0x03
        // hours. CRB is zero, so the writes land in the counter.
        wr(&mut c, 0xDD08, 0x08);
        wr(&mut c, 0xDD09, 0x01);
        wr(&mut c, 0xDD0A, 0x02);
        wr(&mut c, 0xDD0B, 0x03);
        burn(&mut c, 3);
        assert_eq!(rd(&mut c, 0xDD08), 9, "one tenth ticked");
        // CRB bit 7 routes the writes into the alarm registers; the
        // match lands when the tenths wrap and the seconds carry.
        wr(&mut c, 0xDD0F, 0x80);
        wr(&mut c, 0xDD08, 0x00);
        wr(&mut c, 0xDD09, 0x02);
        wr(&mut c, 0xDD0A, 0x02);
        wr(&mut c, 0xDD0B, 0x03);
        burn(&mut c, 3);
        assert!(c.icr_latch & Cia::ICR_ALARM != 0, "the alarm latched");
        assert_eq!(rd(&mut c, 0xDD08), 0, "the tenths wrapped into the match");
        assert_eq!(rd(&mut c, 0xDD09), 0x02, "the seconds carried once");
        // The next tick walks the fields out of the alarm match.
        burn(&mut c, 3);
        assert_eq!(rd(&mut c, 0xDD08), 1);
        // CRB back to zero routes the writes into the counter again.
        wr(&mut c, 0xDD0F, 0x00);
        wr(&mut c, 0xDD0B, 0x05);
        assert_eq!(rd(&mut c, 0xDD0B), 0x05);
    }

    #[test]
    fn keyboard_matrix_and_port_reads() {
        let mut c = Cia::new(&instance());
        // Port A drives all eight rows like the KERNAL scan setup.
        wr(&mut c, 0xDD02, 0xFF);
        wr(&mut c, 0xDD00, 0xFE);
        assert!(c.inject("key 0"));
        // PRA 0xFE drives row 0; key 0 = row 0, column 0.
        assert_eq!(rd(&mut c, 0xDD01), 0xFE, "only column 0 reads low");
        assert_eq!(rd(&mut c, 0xDD00), 0xFE, "PRA reads its latch back");
        assert!(c.inject("key 10"));
        // Key 10 = row 1, column 2, and row 1 is not driven.
        assert_eq!(rd(&mut c, 0xDD01), 0xFE);
        wr(&mut c, 0xDD00, 0xFD); // Now row 1 drives instead.
        // The external read is the released pattern with the pressed
        // columns pulled: only column 2 sits low now.
        assert_eq!(rd(&mut c, 0xDD01), 0xFB, "column 2 joins the low bits");
        // The lifted keys release the columns again.
        assert!(c.inject("key 0-release"));
        assert_eq!(rd(&mut c, 0xDD01), 0xFB);
        assert!(c.inject("key 10-release"));
        assert_eq!(rd(&mut c, 0xDD01), 0xFF);
        // Unknown keys are refused.
        assert!(!c.inject("key 64"));
        assert_eq!(rd(&mut c, 0xDD02), 0xFF);
    }

    #[test]
    fn cia2_drives_the_nmi_line_and_takes_no_keys() {
        let mut c = Cia::new(&instance());
        // The nmi parameter marks the CIA2 form; set it here to keep the
        // config-only parameter out of the shared instance helper.
        c.nmi = true;
        assert_eq!(c.int_line(), "nmi");
        assert!(c.input_mut().is_none(), "the CIA2 takes no key input");
        assert!(c.inject("key 0"), "the sink itself still holds keys");
        wr(&mut c, 0xDD00, 0xFE);
        assert_eq!(
            rd(&mut c, 0xDD01),
            0xFE,
            "the PRA row select works the same"
        );
        // The timer underflows to the same latches; the line name is the
        // only difference.
        wr(&mut c, 0xDD04, 0x02);
        wr(&mut c, 0xDD0E, 0x01);
        burn(&mut c, 2);
        assert_eq!(c.icr_latch, Cia::ICR_TA);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut c = Cia::new(&instance());
        wr(&mut c, 0xDD04, 0x11);
        wr(&mut c, 0xDD05, 0x23);
        wr(&mut c, 0xDD0E, 0x01);
        burn(&mut c, 10);
        c.icr_latch = 0x01;
        c.icr_mask = 0x03;
        c.tod = [3, 4, 5, 6];
        c.keys = 0x00FF_00FF;
        let snap = c.snapshot();
        let mut twin = Cia::new(&instance());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(twin.ta.counter, 0x2311 - 10, "the countdown preserved");
        assert_eq!(twin.keys, 0x00FF_00FF);
        assert_eq!(twin.cra, 0x01);
    }
}
