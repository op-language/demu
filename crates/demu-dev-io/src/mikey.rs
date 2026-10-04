//! The Lynx Mikey die: the timers, the LCD line engine, the joypad
//! port, the cart page select line, and the serial surface of the
//! vl65nc02 machine.
//!
//! The Mikey ASIC holds the custom 65C02 core beside the LCD video
//! engine, eight countdown timers, the joypad port, the ComLynx serial
//! link, and the control register page. This build models the non-CPU
//! half of the die. The CPU core crate owns the memory-map lines of
//! the chip (the `suzy_ram`, `mikey_ram`, `rom_ram`, and `vec_ram`
//! overlays that the vl65nc02 map unit drives itself), and this device
//! carries the rest of the die. The `mikey_ram` inbound line gates the
//! register view of this device: while the level stands high, the
//! register page drops out of the bus decode and the RAM underneath
//! answers, the gate form the Suzy die and the boot ROM share.
//!
//! # Register surface
//!
//! The Mikey register page starts at `$FD00`. The layout follows the
//! published Lynx register table (the cc65 Lynx target headers
//! `_mikey.h` and `_suzy.h`, the Epyx register map, and the Atari Lynx
//! programming documentation):
//!
//! - The timer block runs `$FD00-$FD1F`, four bytes per timer: the
//!   backup (reload) value at `$FD00 + 4n`, the static control byte
//!   `CTLA` at `$FD01 + 4n`, the current count at `$FD02 + 4n`, and
//!   the dynamic control byte `CTLB` at `$FD03 + 4n`. The published
//!   special timers: timer 0 is the line-period (HBL) timer, timer 2
//!   the VBL timer, and timer 4 the UART baud timer.
//! - `CTLA` bits: 7 enables the interrupt on expiry, 6 is a
//!   write-only trigger that resets the timer done flag, 4 enables the
//!   reload from the backup on expiry, 3 enables the count, and bits
//!   2-0 select the source period: selects 0 through 6 are the 1, 2,
//!   4, 8, 16, 32, and 64 microsecond periods, and select 7 links the
//!   timer to its cascade partner. Bit 5 stays stored; the published
//!   sources assign it no function.
//! - `CTLB` bits: 3 is the timer done flag. The last-clock and borrow
//!   bits (2 through 0) stay stored zeros; the published references
//!   name them (Last Clock, Borrow-in, Borrow-out) but pin no
//!   behaviour, so this build records them as out of model.
//! - The cascade chains `0->2->4` and `1->3`: a linked timer (select 7)
//!   takes one decrement at the expiry of its predecessor, and a timer
//!   whose zero form completes a full predecessor period expires. A
//!   select-7 timer whose cascade partner lies outside this build (the
//!   published audio channels feed the rest of the chain) does not
//!   count and waits; this is recorded rather than faked.
//! - Expiry form: a timer expires `(count + 1)` source periods after
//!   the count was loaded. The loaded value shows for one full period
//!   and the expiry fires at the end of the period in which the count
//!   shows zero; this is the published form ("a timer that starts at 5
//!   expires after 6, not 5, periods") and it reproduces the published
//!   boot values, where the timer 2 backup of 104 yields the 105 line
//!   periods of one frame. A reloading timer reloads the backup at the
//!   expiry and carries on. A plain timer sets the done flag, holds
//!   the count at zero, and stops: the published rule that an active
//!   timer with the done flag set does not count down unless it
//!   reloads. The software re-arms it with the done-reset bit of
//!   `CTLA` or a fresh count write.
//! - `INTRST` (`$FD80`) clears the interrupt bits the write carries,
//!   and `INTSET` (`$FD81`) sets them; both return the pending set on
//!   a read. The published bit order runs timer 0 at bit 0 through
//!   timer 7 at bit 7, with the published names HBL for bit 0, VBL for
//!   bit 2, and the serial interrupt riding timer 4's bit. Every
//!   interrupt edge on the Lynx comes from a timer; no joypad event
//!   touches the line.
//! - The joypad register `$FCB0` reads the pressed state from bit 7
//!   down: up, down, left, right, option 1, option 2, inner fire
//!   (button B), outer fire (button A). A pressed control reads set,
//!   the published `JOY_UP_MASK` through `JOY_BTN_2_MASK` forms.
//!   `$FCB1` reads the switches: bit 0 is the pause button, and bits
//!   1-2 carry the published cart slot inactive lines, which read
//!   high in this build because the slot IO lines of the published
//!   machine are outside it.
//! - The cart page select: bit 4 of the IODAT register `$FD8B` is the
//!   published AUDIN line. The device notifies the level change on the
//!   `audin` line, and the cartridge device flips its bank page on the
//!   level. The `$FD86` byte keeps the published `audin` register
//!   value, `$FD87` the `sysctl1` byte, `$FD8A` the parallel-port
//!   direction byte, `$FD8C`/`$FD8D` the serial control and data
//!   bytes, and `$FD92`-`$FD95` the display control, the magic P
//!   count, and the display base address.
//!
//! # Display engine
//!
//! The LCD line engine counts dot clocks and derives the line and
//! frame counters. The published master clock is 16 MHz. The divider
//! entry `dot` carries the domain ticks per LCD dot and defaults to 8,
//! which gives the published 2 MHz LCD dot clock. The divider entry
//! `us` carries the ticks per microsecond for the timer source periods
//! and defaults to 16, the published master divider. The recorded
//! timing forms:
//!
//! - One line period is 317 dots. The published line rate is about
//!   6300 Hz at the 2 MHz dot clock; that clock gives 317.46 dots per
//!   line and the engine truncates to 317. This build's line rate is
//!   2000000/317 or about 6309 Hz, a recorded 0.15 percent
//!   approximation.
//! - One frame is 105 line periods: the published 102 display rows
//!   plus 3 blanking periods. This build shows the rows at line
//!   periods 2 through 103: two blanking periods above the display and
//!   one below. The published references pin the row count but not the
//!   exact split of the three blanking periods, so the 2/1 split is
//!   the recorded defensible form.
//! - The frame rate lands at 105 x 317 dots over the 2 MHz dot clock,
//!   about 60.06 Hz; the published refresh forms sit near 60 Hz.
//! - The engine runs while the display-enable bit (`DISPCTL` bit 0) is
//!   set; the game loader of the real machine sets it after boot. The
//!   dot counter at a domain tick is the tick remainder inside the
//!   current line period, and the scheduled visits land at the line
//!   boundaries only.
//!
//! The line counter of the engine is internal state here. The device
//! notifies four lines: `irq` when the interrupt pendings change,
//! `audin` when the cart page select state changes, and
//! `cart_strobe`/`cart_data` when the cart address strobe of SYSCTL1
//! bit 0 or the cart address data of IODAT bit 1 changes. The cartridge
//! device builds its stream address from the strobe and data lines and
//! the AUDIN page select.
//!
//! # Recorded approximations
//!
//! - The ComLynx serial link is outside this build: the serial control
//!   and data registers store the written bytes and read them back
//!   (the open-bus style). The live TX-ready and receive bits of the
//!   published `SERCTL` form are recorded here.
//! - The unmodeled bytes of the `$FD00` page (the audio channel and
//!   attenuation block, the magtape ready bits, the test registers,
//!   and the palette) read 0xFF and ignore writes; the demu audio and
//!   video devices carry their own surfaces over the page.
//! - The `$FCB2`/`$FCB3` RCART registers of the published serial cart
//!   port form read 0xFF. The demu cartridge device models the cart
//!   page as a flat window with the bank line, not the serial address
//!   counter form.
//! - A control or count write re-anchors the affected timer's
//!   countdown at the write tick with the current count. The published
//!   countdown runs free of the write instants; the realignment stays
//!   inside one source period and is recorded.
//! - The magtape ready bits, the IODIR direction gating, and the
//!   parallel-port pin semantics stay stored bytes.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The timers of the Mikey.
const TIMER_COUNT: usize = 8;

/// The static control `CTLA` bit: the interrupt on expiry.
const CTL_IRQ: u8 = 0x80;
/// The static control `CTLA` bit: the write-only reset of the done flag.
const CTL_RESET_DONE: u8 = 0x40;
/// The static control `CTLA` bit: the reload from the backup on expiry.
const CTL_RELOAD: u8 = 0x10;
/// The static control `CTLA` bit: the count enable.
const CTL_COUNT: u8 = 0x08;
/// The `CTLA` source select that links a timer to its cascade partner.
const SEL_LINK: u8 = 7;
/// The dynamic `CTLB` bit: the timer done flag.
const CTLB_DONE: u8 = 0x08;
/// The IODAT bit that drives the cart page select line.
const AUDIN_BIT: u8 = 0x10;
/// The SYSCTL1 bit that carries the cart address strobe.
const SYSCTL_STROBE: u8 = 0x01;
/// The IODAT bit that carries the cart address data.
const CART_DATA_BIT: u8 = 0x02;

/// The line periods of one LCD frame (published: 102 display rows plus
/// 3 blanking periods).
const LINES_PER_FRAME: u32 = 105;
/// The dot clocks of one line period (recorded: the published line
/// rate of about 6300 Hz truncates from 317.46 dots).
const DOTS_PER_LINE: u32 = 317;

/// One Mikey countdown timer.
#[derive(Debug, Clone, Copy)]
struct MikeyTimer {
    /// The backup (reload) value, the preset the expiry reloads.
    backup: u8,
    /// The static control byte.
    ctla: u8,
    /// The dynamic control byte; the done flag is bit 3.
    ctlb: u8,
    /// The count value at the anchor tick.
    value: u8,
    /// The domain tick the current countdown anchored at.
    anchor: u64,
    /// The standalone expiry tick, present only for timers that count
    /// on their own source period.
    due: Option<u64>,
}

impl MikeyTimer {
    /// A timer that does not count: the published reset form.
    fn disabled() -> Self {
        Self {
            backup: 0,
            ctla: 0,
            ctlb: 0,
            value: 0,
            anchor: 0,
            due: None,
        }
    }
}

/// The Lynx Mikey device: the timers, the LCD line engine, the joypad
/// port, the cart page select line, and the serial surface.
#[derive(Debug)]
pub struct LynxMikey {
    name: String,
    /// Domain ticks per LCD dot, the `dot` divider entry.
    dot_div: u64,
    /// Domain ticks per microsecond, the `us` divider entry.
    us_ticks: u64,
    /// The eight countdown timers.
    timers: [MikeyTimer; TIMER_COUNT],
    /// The interrupt pendings, one bit per timer in the published order.
    pendings: u8,
    /// The IRQ level the device last notified.
    irq_out: bool,
    /// The AUDIN level the device last notified.
    audin_out: bool,
    /// The cart address strobe level the device last notified.
    strobe_out: bool,
    /// The cart address data level the device last notified.
    data_out: bool,
    /// The stored register bytes of the control page.
    audin: u8,
    sysctl1: u8,
    iodir: u8,
    iodat: u8,
    serctl: u8,
    serdat: u8,
    dispctl: u8,
    pkbkup: u8,
    /// The display base address, two stored bytes.
    scrbase: [u8; 2],
    /// The joypad pressed state, one bit per control in the published
    /// register layout.
    joy: u8,
    /// The pause state of the published switches register.
    pause: bool,
    /// The machine RAM view gate of the `mikey_ram` line: while the
    /// level stands high, the register page drops out of the bus
    /// decode and the RAM underneath answers, the form the Suzy die
    /// and the boot ROM share.
    ram_view: bool,
    /// The line counter of the LCD engine.
    line: u32,
    /// The domain tick of the current line's start.
    line_tick: u64,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl LynxMikey {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let dot_div = u64::from(device.dividers.get("dot").copied().unwrap_or(8)).max(1);
        let us_ticks = u64::from(device.dividers.get("us").copied().unwrap_or(16)).max(1);
        Self {
            name: device.name.to_string(),
            dot_div,
            us_ticks,
            timers: std::array::from_fn(|_| MikeyTimer::disabled()),
            pendings: 0,
            irq_out: false,
            audin_out: false,
            strobe_out: false,
            data_out: false,
            audin: 0,
            sysctl1: 0,
            iodir: 0,
            iodat: 0,
            serctl: 0,
            serdat: 0,
            dispctl: 0,
            pkbkup: 0,
            scrbase: [0; 2],
            joy: 0,
            pause: false,
            ram_view: false,
            line: 0,
            line_tick: 0,
            next: None,
            last_tick: None,
        }
    }

    /// The source select of one timer.
    fn select(&self, i: usize) -> u8 {
        self.timers[i].ctla & 0x07
    }

    /// True when the timer's count runs: the count is enabled and the
    /// done flag is clear or the expiry reloads.
    fn can_count(&self, i: usize) -> bool {
        let t = &self.timers[i];
        t.ctla & CTL_COUNT != 0 && (t.ctlb & CTLB_DONE == 0 || t.ctla & CTL_RELOAD != 0)
    }

    /// The source period of one countdown step in domain ticks.
    fn period_ticks(&self, i: usize) -> u64 {
        u64::from(1u32 << self.select(i)) * self.us_ticks
    }

    /// The cascade predecessor of a linked timer; the published chains
    /// run `0->2->4` and `1->3`.
    fn predecessor(i: usize) -> Option<usize> {
        match i {
            2 => Some(0),
            3 => Some(1),
            4 => Some(2),
            _ => None,
        }
    }

    /// The current count of a timer at a tick.
    ///
    /// A standalone timer's count drops one step per source period
    /// since the anchor. A linked timer updates its count only at its
    /// predecessor's expiries. A stopped timer holds its count.
    fn count_at(&self, i: usize, tick: Tick) -> u8 {
        let t = &self.timers[i];
        if !self.can_count(i) || t.due.is_none() {
            return t.value;
        }
        let step = self.period_ticks(i).max(1);
        let steps = tick.0.saturating_sub(t.anchor) / step;
        let steps = u8::try_from(steps).unwrap_or(u8::MAX);
        t.value.saturating_sub(steps)
    }

    /// The switches byte: the published cart slot inactive lines read
    /// high and the pause bit carries the injected pause state.
    fn switches_byte(&self) -> u8 {
        0x06 | u8::from(self.pause)
    }

    /// True while the LCD engine runs: the display-enable bit of the
    /// published `DISPCTL`.
    fn engine_enabled(&self) -> bool {
        self.dispctl & 0x01 != 0
    }

    /// The line period in domain ticks.
    fn line_period(&self) -> u64 {
        u64::from(DOTS_PER_LINE) * self.dot_div
    }

    /// Schedule the next visit at or after the current tick: the
    /// earliest of the LCD line boundary and the timer expiries.
    fn schedule(&mut self, _tick: Tick) {
        let mut best = if self.engine_enabled() {
            Some(self.line_tick + self.line_period())
        } else {
            None
        };
        for (i, t) in self.timers.iter().enumerate() {
            let Some(due) = t.due else { continue };
            if self.select(i) == SEL_LINK || !self.can_count(i) {
                continue;
            }
            best = Some(best.map_or(due, |b| b.min(due)));
        }
        self.next = best.map(Tick);
    }

    /// Schedule an immediate visit so the level outputs track a
    /// register change; the notification happens on the visit path.
    /// The immediate visit keeps any earlier due event.
    fn refresh(&mut self, tick: Tick) {
        let at = tick.0.min(self.next.unwrap_or(Tick(u64::MAX)).0);
        self.next = Some(Tick(at));
        self.last_tick = None;
    }

    /// Process one timer expiry at a tick.
    ///
    /// The done flag sets, the interrupt pending sets per the control
    /// bit, the linked successors take one cascade step, and the
    /// expiry either reloads the backup or holds the count at zero
    /// with the timer stopped.
    fn expire(&mut self, i: usize, tick: Tick, ctx: &mut MachineCtx) {
        self.timers[i].ctlb |= CTLB_DONE;
        if self.timers[i].ctla & CTL_IRQ != 0 {
            self.pendings |= 1u8 << i;
        }
        let successors: Vec<usize> = (0..TIMER_COUNT)
            .filter(|&j| j != i && Self::predecessor(j) == Some(i))
            .collect();
        for j in successors {
            self.cascade_step(j, tick, ctx);
        }
        let reload = self.timers[i].ctla & CTL_RELOAD != 0;
        let linked = self.select(i) == SEL_LINK;
        let backup = self.timers[i].backup;
        if reload {
            let period = self.period_ticks(i).max(1);
            self.timers[i].value = backup;
            self.timers[i].anchor = tick.0;
            self.timers[i].due = (!linked).then(|| tick.0 + (u64::from(backup) + 1) * period);
        } else {
            self.timers[i].value = 0;
            self.timers[i].due = None;
        }
    }

    /// One cascade step of a linked timer at its predecessor's expiry:
    /// a count whose zero form completed a full predecessor period
    /// expires, any other count drops one step.
    fn cascade_step(&mut self, j: usize, tick: Tick, ctx: &mut MachineCtx) {
        if !self.can_count(j) {
            return;
        }
        if self.timers[j].value == 0 {
            self.expire(j, tick, ctx);
        } else {
            self.timers[j].value -= 1;
        }
    }

    /// Write the static control byte of one timer.
    ///
    /// The done-reset trigger clears the done flag, the write-only bit
    /// stays unstored, and the countdown re-anchors at the write tick
    /// with the current count.
    fn write_ctla(&mut self, i: usize, value: u8, tick: Tick) {
        if value & CTL_RESET_DONE != 0 {
            self.timers[i].ctlb &= !CTLB_DONE;
        }
        self.timers[i].ctla = value & !CTL_RESET_DONE;
        let show = self.count_at(i, tick);
        self.reanchor(i, tick, show);
    }

    /// Write the count of one timer as a fresh load.
    fn write_cnt(&mut self, i: usize, value: u8, tick: Tick) {
        self.timers[i].due = None;
        self.reanchor(i, tick, value);
    }

    /// Re-anchor a timer at a tick with a count value.
    fn reanchor(&mut self, i: usize, tick: Tick, value: u8) {
        let linked = self.select(i) == SEL_LINK;
        let counts = self.can_count(i) && !linked;
        let period = self.period_ticks(i).max(1);
        self.timers[i].value = value;
        self.timers[i].anchor = tick.0;
        self.timers[i].due = counts.then(|| tick.0 + (u64::from(value) + 1) * period);
    }

    /// Process the visit at `tick`: the standalone timer expiries in
    /// index order, the LCD line boundaries, the level outputs, and
    /// the next schedule.
    fn visit(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        for i in 0..TIMER_COUNT {
            let due = self.timers[i].due;
            if due.is_some_and(|d| d <= tick.0) && self.can_count(i) && self.select(i) != SEL_LINK {
                self.expire(i, tick, ctx);
            }
        }
        if self.engine_enabled() {
            let period = self.line_period();
            while self.line_tick + period <= tick.0 {
                self.line_tick += period;
                self.line = (self.line + 1) % LINES_PER_FRAME;
            }
        }
        let want_irq = self.pendings != 0;
        if want_irq != self.irq_out {
            self.irq_out = want_irq;
            let level = if want_irq { Level::High } else { Level::Low };
            ctx.notify("irq", level, tick);
        }
        let want_audin = self.iodat & AUDIN_BIT != 0;
        if want_audin != self.audin_out {
            self.audin_out = want_audin;
            let level = if want_audin { Level::High } else { Level::Low };
            ctx.notify("audin", level, tick);
        }
        let want_strobe = self.sysctl1 & SYSCTL_STROBE != 0;
        if want_strobe != self.strobe_out {
            self.strobe_out = want_strobe;
            let level = if want_strobe { Level::High } else { Level::Low };
            ctx.notify("cart_strobe", level, tick);
        }
        let want_data = self.iodat & CART_DATA_BIT != 0;
        if want_data != self.data_out {
            self.data_out = want_data;
            let level = if want_data { Level::High } else { Level::Low };
            ctx.notify("cart_data", level, tick);
        }
        self.schedule(tick);
    }

    /// Read one Mikey page or joypad register byte.
    fn reg_read(&mut self, address: u32, tick: Tick) -> u8 {
        if address < 0xFC00 {
            return 0xFF;
        }
        match address {
            0xFD00..=0xFD1F => {
                let i = ((address - 0xFD00) >> 2) as usize;
                match address & 3 {
                    0 => self.timers[i].backup,
                    1 => self.timers[i].ctla,
                    2 => self.count_at(i, tick),
                    _ => self.timers[i].ctlb,
                }
            }
            0xFD80 | 0xFD81 => self.pendings,
            0xFD86 => self.audin,
            0xFD87 => self.sysctl1,
            0xFD8A => self.iodir,
            0xFD8B => self.iodat,
            0xFD8C => self.serctl,
            0xFD8D => self.serdat,
            0xFD92 => self.dispctl,
            0xFD93 => self.pkbkup,
            0xFD94 | 0xFD95 => self.scrbase[(address - 0xFD94) as usize],
            0xFCB0 => self.joy,
            0xFCB1 => self.switches_byte(),
            // The unmodeled page bytes read the open-bus high form.
            _ => 0xFF,
        }
    }

    /// Write one Mikey page or joypad register byte.
    fn reg_write(&mut self, address: u32, value: u8, tick: Tick) {
        if address < 0xFC00 {
            return;
        }
        match address {
            0xFD00..=0xFD1F => {
                let i = ((address - 0xFD00) >> 2) as usize;
                match address & 3 {
                    0 => self.timers[i].backup = value,
                    1 => self.write_ctla(i, value, tick),
                    2 => self.write_cnt(i, value, tick),
                    // The published dynamic control bits are read-only
                    // state of the countdown hardware.
                    _ => {}
                }
                self.schedule(tick);
            }
            0xFD80 => {
                self.pendings &= !value;
                self.refresh(tick);
            }
            0xFD81 => {
                self.pendings |= value;
                self.refresh(tick);
            }
            0xFD86 => self.audin = value,
            0xFD87 => {
                // The strobe ride: a bit change notifies at the
                // immediate visit.
                if value & SYSCTL_STROBE != self.sysctl1 & SYSCTL_STROBE {
                    self.sysctl1 = value;
                    self.refresh(tick);
                } else {
                    self.sysctl1 = value;
                }
            }
            0xFD8A => self.iodir = value,
            0xFD8B => {
                // The published AUDIN line rides this byte's bit 4 and
                // the cart address data rides bit 1; a change of
                // either notifies at the immediate visit.
                let touched = AUDIN_BIT | CART_DATA_BIT;
                if value & touched != self.iodat & touched {
                    self.iodat = value;
                    self.refresh(tick);
                } else {
                    self.iodat = value;
                }
            }
            0xFD8C => self.serctl = value,
            0xFD8D => self.serdat = value,
            0xFD92 => {
                let started = !self.engine_enabled() && value & 0x01 != 0;
                self.dispctl = value;
                if started {
                    // The engine anchors its dot counter at the write
                    // tick when the display enables.
                    self.line_tick = tick.0;
                    self.line = 0;
                }
                self.schedule(tick);
            }
            0xFD93 => self.pkbkup = value,
            0xFD94 | 0xFD95 => self.scrbase[(address - 0xFD94) as usize] = value,
            // The unmodeled page bytes and the cart port registers
            // ignore writes.
            _ => {}
        }
    }
}

/// The button masks of the published joypad byte, one bit per control.
const JOY_UP: u8 = 0x80;
const JOY_DOWN: u8 = 0x40;
const JOY_LEFT: u8 = 0x20;
const JOY_RIGHT: u8 = 0x10;
const JOY_OPTION1: u8 = 0x08;
const JOY_OPTION2: u8 = 0x04;
const JOY_INNER: u8 = 0x02;
const JOY_OUTER: u8 = 0x01;

/// The mask of one event name in the joypad byte, or zero.
fn joy_mask(button: &str) -> u8 {
    match button {
        "up" => JOY_UP,
        "down" => JOY_DOWN,
        "left" => JOY_LEFT,
        "right" => JOY_RIGHT,
        "a" => JOY_OUTER,
        "b" => JOY_INNER,
        "opt1" => JOY_OPTION1,
        "opt2" => JOY_OPTION2,
        _ => 0,
    }
}

impl InputSink for LynxMikey {
    fn inject(&mut self, event: &str) -> bool {
        let (verb, button) = event.split_once(' ').unwrap_or((event, ""));
        let pressed = match verb.trim().to_ascii_lowercase().as_str() {
            "press" => true,
            "release" => false,
            _ => return false,
        };
        let name = button.trim().to_ascii_lowercase();
        if name == "pause" {
            self.pause = pressed;
            return true;
        }
        let mask = joy_mask(&name);
        if mask == 0 {
            return false;
        }
        self.joy = if pressed {
            self.joy | mask
        } else {
            self.joy & !mask
        };
        true
    }
}

impl BusAccess for LynxMikey {
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        u32::from(self.reg_read(address.0, tick))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        self.reg_write(address.0, value as u8, tick);
    }

    fn peek(&self, address: Address) -> u8 {
        // The side-effect-free read answers the anchor-time state of
        // the countdown registers.
        if address.0 < 0xFC00 {
            return 0xFF;
        }
        match address.0 {
            0xFD00..=0xFD1F => {
                let i = ((address.0 - 0xFD00) >> 2) as usize;
                match address.0 & 3 {
                    0 => self.timers[i].backup,
                    1 => self.timers[i].ctla,
                    2 => self.timers[i].value,
                    _ => self.timers[i].ctlb,
                }
            }
            0xFD80 | 0xFD81 => self.pendings,
            0xFD86 => self.audin,
            0xFD87 => self.sysctl1,
            0xFD8A => self.iodir,
            0xFD8B => self.iodat,
            0xFD8C => self.serctl,
            0xFD8D => self.serdat,
            0xFD92 => self.dispctl,
            0xFD93 => self.pkbkup,
            0xFD94 | 0xFD95 => self.scrbase[(address.0 - 0xFD94) as usize],
            0xFCB0 => self.joy,
            0xFCB1 => self.switches_byte(),
            _ => 0xFF,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        // The machine RAM view gate: while the mikey_ram line drives
        // high, the RAM underneath answers.
        !self.ram_view
    }
}

impl Device for LynxMikey {
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
        self.timers = std::array::from_fn(|_| MikeyTimer::disabled());
        self.pendings = 0;
        self.irq_out = false;
        self.audin_out = false;
        self.audin = 0;
        self.sysctl1 = 0;
        self.iodir = 0;
        self.iodat = 0;
        self.serctl = 0;
        self.serdat = 0;
        self.dispctl = 0;
        self.pkbkup = 0;
        self.scrbase = [0; 2];
        self.joy = 0;
        self.pause = false;
        self.ram_view = false;
        self.line = 0;
        self.line_tick = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(4 + 1 + TIMER_COUNT * 21 + 27);
        out.extend_from_slice(b"LMIK");
        out.push(3);
        for t in &self.timers {
            out.push(t.backup);
            out.push(t.ctla);
            out.push(t.ctlb);
            out.push(t.value);
            out.extend_from_slice(&t.anchor.to_le_bytes());
            out.push(u8::from(t.due.is_some()));
            out.extend_from_slice(&t.due.unwrap_or(0).to_le_bytes());
        }
        out.push(self.pendings);
        out.push(u8::from(self.irq_out));
        out.push(u8::from(self.audin_out));
        out.push(self.audin);
        out.push(self.sysctl1);
        out.push(self.iodir);
        out.push(self.iodat);
        out.push(self.serctl);
        out.push(self.serdat);
        out.push(self.dispctl);
        out.push(self.pkbkup);
        out.extend_from_slice(&self.scrbase);
        out.push(self.line as u8);
        out.extend_from_slice(&self.line_tick.to_le_bytes());
        out.push(u8::from(self.strobe_out));
        out.push(u8::from(self.data_out));
        // The pressed pad, the pause flag, and the RAM view gate ride
        // the tail.
        out.push(self.joy);
        out.push(u8::from(self.pause));
        out.push(u8::from(self.ram_view));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + TIMER_COUNT * 21 + 27;
        if data.len() != expected || &data[..4] != b"LMIK" || data[4] != 3 {
            return Err(SnapshotError::Mismatch(format!(
                "mikey chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        for t in &mut self.timers {
            t.backup = data[pos];
            t.ctla = data[pos + 1];
            t.ctlb = data[pos + 2];
            t.value = data[pos + 3];
            t.anchor = u64::from_le_bytes(data[pos + 4..pos + 12].try_into().expect("8 bytes"));
            t.due = (data[pos + 12] != 0)
                .then(|| u64::from_le_bytes(data[pos + 13..pos + 21].try_into().expect("8 bytes")));
            pos += 21;
        }
        self.pendings = data[pos];
        self.irq_out = data[pos + 1] != 0;
        self.audin_out = data[pos + 2] != 0;
        self.audin = data[pos + 3];
        self.sysctl1 = data[pos + 4];
        self.iodir = data[pos + 5];
        self.iodat = data[pos + 6];
        self.serctl = data[pos + 7];
        self.serdat = data[pos + 8];
        self.dispctl = data[pos + 9];
        self.pkbkup = data[pos + 10];
        self.scrbase.copy_from_slice(&data[pos + 11..pos + 13]);
        self.line = u32::from(data[pos + 13]) % LINES_PER_FRAME;
        self.line_tick = u64::from_le_bytes(data[pos + 14..pos + 22].try_into().expect("8 bytes"));
        self.strobe_out = data[pos + 22] != 0;
        self.data_out = data[pos + 23] != 0;
        self.joy = data[pos + 24];
        self.pause = data[pos + 25] != 0;
        self.ram_view = data[pos + 26] != 0;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        // The inbound gate of the CPU's map unit: while the level
        // stands high, the register page sits behind the RAM view.
        if line == "mikey_ram" {
            self.ram_view = level == Level::High;
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

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{ClockDomain, Connection, ConnectionDirection, ConnectionSemantics};
    use demu_core::Machine;
    use demu_core::machine::{DeviceCtor, DeviceCtors};
    use std::collections::BTreeMap;

    const M16: u64 = 16_000_000;

    /// The device constructor of the test recorder, boxed for the
    /// assembly registry.
    fn recorder_ctor() -> Box<DeviceCtor> {
        Box::new(|device, _| Ok(Box::new(LineRecorder::new(device.name.as_ref()))))
    }

    /// A recorder device: it collects the notified line edges of the
    /// machine's connections.
    struct LineRecorder {
        name: String,
        lines: Vec<(String, Level, u64)>,
    }

    impl LineRecorder {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                lines: Vec::new(),
            }
        }
    }

    impl Device for LineRecorder {
        fn name(&self) -> &str {
            &self.name
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn reset(&mut self) {
            self.lines.clear();
        }

        fn boot(&mut self, _state: Option<&BootState>) {
            self.lines.clear();
        }

        fn snapshot(&self) -> Snapshot {
            Vec::new()
        }

        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }

        fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
            self.lines.push((line.to_string(), level, tick.0));
        }

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    /// A test machine: the Mikey on a 16 MHz main domain with a
    /// recorder connected to both device lines.
    fn machine() -> Machine {
        let mut ctors = DeviceCtors::new();
        ctors.register(
            "lynx.mikey",
            Box::new(|device, _| Ok(Box::new(LynxMikey::new(device)))),
        );
        ctors.register("test.lines", recorder_ctor());
        let mikey = demu_config::newtypes::DeviceName::new("mikey").expect("name");
        let lines = demu_config::newtypes::DeviceName::new("lines").expect("name");
        let devices = vec![
            demu_config::model::DeviceInstance {
                name: mikey.clone(),
                kind: demu_config::newtypes::KindName::new("lynx.mikey").expect("kind"),
                domain: "main".to_string(),
                params: BTreeMap::new(),
                dividers: [("dot".to_string(), 8), ("us".to_string(), 16)]
                    .into_iter()
                    .collect(),
            },
            demu_config::model::DeviceInstance {
                name: lines.clone(),
                kind: demu_config::newtypes::KindName::new("test.lines").expect("kind"),
                domain: "main".to_string(),
                params: BTreeMap::new(),
                dividers: BTreeMap::new(),
            },
        ];
        let devices_by_name: BTreeMap<demu_config::newtypes::DeviceName, usize> =
            [(mikey.clone(), 0), (lines.clone(), 1)]
                .into_iter()
                .collect();
        let config = demu_config::model::SystemConfig {
            name: "test".to_string(),
            triple: "test".to_string(),
            description: String::new(),
            clocks: [(
                "main".to_string(),
                ClockDomain {
                    name: "main".to_string(),
                    frequency: demu_config::newtypes::Frequency::from_hz(M16).expect("freq"),
                    origin: "test".to_string(),
                },
            )]
            .into_iter()
            .collect(),
            devices,
            devices_by_name,
            buses: vec![demu_config::model::Bus {
                name: "bus".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![
                demu_config::model::Window {
                    name: "mikey".to_string(),
                    bus: "bus".to_string(),
                    device: mikey.clone(),
                    base: 0xFD00,
                    size: 0x100,
                    kind: None,
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
                demu_config::model::Window {
                    name: "pads".to_string(),
                    bus: "bus".to_string(),
                    device: mikey.clone(),
                    base: 0xFCB0,
                    size: 4,
                    kind: None,
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
            ],
            connections: vec![
                Connection {
                    from: demu_config::newtypes::LinePath::parse("mikey.irq").expect("route"),
                    to: demu_config::newtypes::LinePath::parse("lines.irq").expect("route"),
                    semantics: ConnectionSemantics::Irq,
                    direction: ConnectionDirection::Forward,
                },
                Connection {
                    from: demu_config::newtypes::LinePath::parse("mikey.audin").expect("route"),
                    to: demu_config::newtypes::LinePath::parse("lines.bank").expect("route"),
                    semantics: ConnectionSemantics::Irq,
                    direction: ConnectionDirection::Forward,
                },
            ],
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut machine = Machine::assemble(config, &ctors).expect("assembly");
        machine.boot();
        machine
    }

    fn recorder(machine: &mut Machine) -> &mut LineRecorder {
        machine.devices[1]
            .as_any_mut()
            .downcast_mut::<LineRecorder>()
            .expect("recorder")
    }

    #[test]
    fn timer_arms_schedules_and_expires_once() {
        let mut m = machine();
        assert!(m.debug_write("bus", Address(0xFD00), 158), "backup write");
        assert!(m.debug_write("bus", Address(0xFD02), 3), "count write");
        assert_eq!(m.peek("bus", Address(0xFD02)), 3);
        assert!(m.debug_write("bus", Address(0xFD01), 0x88), "control write");
        // The timer expires (count + 1) 1-microsecond periods after the
        // last write, each 16 master ticks.
        assert_eq!(m.devices[0].next_event(), Some(Tick(64)));
        m.run_until(Tick(64));
        // The expiry raised the IRQ over the connection.
        assert_eq!(
            recorder(&mut m).lines,
            vec![("irq".to_string(), Level::High, 64)]
        );
        // The interrupt set and the done flag of the dynamic control.
        assert_eq!(m.peek("bus", Address(0xFD81)), 0x01);
        assert_eq!(m.peek("bus", Address(0xFD03)), 0x08);
        // No reload armed: the count holds and the timer went idle.
        assert_eq!(m.peek("bus", Address(0xFD02)), 0);
        assert_eq!(m.devices[0].next_event(), None);
        // The acknowledge drops the line at the immediate visit.
        assert!(m.debug_write("bus", Address(0xFD80), 0x01));
        m.run_until(Tick(64));
        assert_eq!(
            recorder(&mut m).lines,
            vec![
                ("irq".to_string(), Level::High, 64),
                ("irq".to_string(), Level::Low, 64)
            ]
        );
    }

    #[test]
    fn timer_reloads_from_backup() {
        let mut m = machine();
        m.debug_write("bus", Address(0xFD00), 158);
        m.debug_write("bus", Address(0xFD02), 1);
        m.debug_write("bus", Address(0xFD01), 0x98);
        // The first expiry reloads the backup of 158 and the next one
        // lands 159 periods later.
        assert_eq!(m.devices[0].next_event(), Some(Tick(32)));
        m.run_until(Tick(32));
        assert_eq!(m.peek("bus", Address(0xFD02)), 158);
        assert_eq!(m.devices[0].next_event(), Some(Tick(32 + 159 * 16)));
    }

    #[test]
    fn cascade_chain_carries_vbl_style_link() {
        let mut m = machine();
        // The published video pair: timer 0 counts microseconds as the
        // line timer, timer 2 links to it and reloads with a backup of
        // 2, so it expires on the third expiry of timer 0.
        m.debug_write("bus", Address(0xFD00), 2);
        m.debug_write("bus", Address(0xFD02), 2);
        m.debug_write("bus", Address(0xFD01), 0x18);
        m.debug_write("bus", Address(0xFD08), 2);
        m.debug_write("bus", Address(0xFD0A), 2);
        // The published video pair has interrupts and reloading on.
        m.debug_write("bus", Address(0xFD09), 0x9F);
        // The timer 0 expiries run at 48 master ticks apart; the
        // linked timer steps once per expiry and expires on the third.
        m.run_until(Tick(144));
        // The timer 0 expiries run at 48 master ticks apart.
        m.run_until(Tick(48 + 2 * (3 * 16)));
        assert_eq!(m.peek("bus", Address(0xFD0B)), 0x08, "timer 2 done");
        assert_eq!(m.peek("bus", Address(0xFD81)), 0x04, "timer 2 pending");
        // The reload put the backup back in the linked timer.
        assert_eq!(m.peek("bus", Address(0xFD0A)), 2);
    }

    #[test]
    fn interrupt_control_bytes_route_the_line() {
        let mut m = machine();
        // A software-set interrupt raises the line.
        m.debug_write("bus", Address(0xFD81), 0x04);
        m.run_until(Tick(0));
        assert_eq!(
            recorder(&mut m).lines,
            vec![("irq".to_string(), Level::High, 0)]
        );
        // The acknowledge drops it again.
        m.debug_write("bus", Address(0xFD80), 0x04);
        m.run_until(Tick(1));
        assert_eq!(
            recorder(&mut m).lines,
            vec![
                ("irq".to_string(), Level::High, 0),
                ("irq".to_string(), Level::Low, 0)
            ]
        );
    }

    #[test]
    fn audin_page_line_notifies_cart_bank() {
        let mut m = machine();
        m.debug_write("bus", Address(0xFD8B), 0x10);
        m.run_until(Tick(0));
        assert_eq!(
            recorder(&mut m).lines,
            vec![("bank".to_string(), Level::High, 0)]
        );
        assert_eq!(m.peek("bus", Address(0xFD8B)), 0x10);
        m.debug_write("bus", Address(0xFD8B), 0x00);
        m.run_until(Tick(1));
        assert_eq!(
            recorder(&mut m).lines,
            vec![
                ("bank".to_string(), Level::High, 0),
                ("bank".to_string(), Level::Low, 0)
            ]
        );
    }

    #[test]
    fn the_mikey_ram_line_gates_the_register_view() {
        let mut m = machine();
        let mikey = m.devices[0]
            .as_any_mut()
            .downcast_mut::<LynxMikey>()
            .expect("mikey");
        // The register page answers its decode before the gate.
        assert!(
            mikey.bus().expect("bus").decodes(Address(0xFD00)),
            "the register page decoded"
        );
        // The machine RAM view line high: the page answers nothing.
        mikey.notify_line("mikey_ram", Level::High, Tick(0));
        assert!(!mikey.bus().expect("bus").decodes(Address(0xFD00)));
        // The line low restores the page, and an unrelated line
        // leaves the gate alone.
        mikey.notify_line("mikey_ram", Level::Low, Tick(0));
        assert!(mikey.bus().expect("bus").decodes(Address(0xFD00)));
        mikey.notify_line("irq", Level::High, Tick(0));
        assert!(mikey.bus().expect("bus").decodes(Address(0xFD00)));
    }

    #[test]
    fn joypad_events_follow_published_bit_layout() {
        let mut m = machine();
        {
            let mikey = m.devices[0]
                .as_any_mut()
                .downcast_mut::<LynxMikey>()
                .expect("mikey");
            assert!(
                mikey.input_mut().expect("sink").inject("press up"),
                "up accepted"
            );
            assert!(mikey.input_mut().expect("sink").inject("press a"));
            assert!(mikey.input_mut().expect("sink").inject("release a"));
            assert!(mikey.input_mut().expect("sink").inject("press b"));
            assert!(mikey.input_mut().expect("sink").inject("press opt1"));
            assert!(mikey.input_mut().expect("sink").inject("press pause"));
            assert!(!mikey.input_mut().expect("sink").inject("press zzz"));
        }
        // The joypad byte reads the published bit layout from bit 7
        // down: up, option 1, and the inner fire set.
        assert_eq!(m.peek("bus", Address(0xFCB0)), 0x8A);
        // The switches register reads the published inactive form with
        // the pause bit set.
        assert_eq!(m.peek("bus", Address(0xFCB1)), 0x07);
    }

    #[test]
    fn engine_counts_lines_at_dot_boundaries() {
        let mut m = machine();
        // The published display control: DMA enable starts the engine.
        m.debug_write("bus", Address(0xFD92), 0x08 | 0x04 | 0x01);
        // 105 frame line periods of 317 dots at 8 ticks per dot.
        let per_line = u64::from(DOTS_PER_LINE) * 8;
        assert_eq!(m.devices[0].next_event(), Some(Tick(per_line)));
        // One full frame of boundaries wraps the line counter.
        m.run_until(Tick(per_line * u64::from(LINES_PER_FRAME)));
        let mikey = m.devices[0]
            .as_any_mut()
            .downcast_mut::<LynxMikey>()
            .expect("mikey");
        assert_eq!(mikey.line, 0, "the wrapped frame");
        assert_eq!(
            mikey.line_tick,
            Tick(per_line * u64::from(LINES_PER_FRAME)).0
        );
        // The engine stops with the display enable cleared.
        m.debug_write("bus", Address(0xFD92), 0x00);
        assert_eq!(m.devices[0].next_event(), None);
    }

    #[test]
    fn display_window_and_magic_p_store() {
        let mut m = machine();
        m.debug_write("bus", Address(0xFD94), 0x00);
        m.debug_write("bus", Address(0xFD95), 0xC0);
        m.debug_write("bus", Address(0xFD93), 41);
        assert_eq!(m.peek("bus", Address(0xFD94)), 0x00);
        assert_eq!(m.peek("bus", Address(0xFD95)), 0xC0);
        assert_eq!(m.peek("bus", Address(0xFD93)), 41);
    }

    #[test]
    fn serial_and_unmodeled_bytes_store_open_bus_style() {
        let mut m = machine();
        m.debug_write("bus", Address(0xFD8C), 0x20);
        m.debug_write("bus", Address(0xFD8D), 0x41);
        assert_eq!(m.peek("bus", Address(0xFD8C)), 0x20);
        assert_eq!(m.peek("bus", Address(0xFD8D)), 0x41);
        // The audio block bytes and the RCART registers read the open
        // bus high form in this build.
        assert_eq!(m.peek("bus", Address(0xFD20)), 0xFF);
        assert_eq!(m.peek("bus", Address(0xFCB2)), 0xFF);
    }

    #[test]
    fn snapshot_round_trips_and_rejects_wrong_length() {
        let mut m = machine();
        m.debug_write("bus", Address(0xFD00), 158);
        m.debug_write("bus", Address(0xFD02), 3);
        m.debug_write("bus", Address(0xFD01), 0x98);
        m.run_until(Tick(100));
        m.debug_write("bus", Address(0xFD8B), 0x10);
        // The pressed pad and the pause bit ride the chunk too.
        m.devices[0].input_mut().expect("sink").inject("press up");
        m.devices[0]
            .input_mut()
            .expect("sink")
            .inject("press pause");
        let snap = m.devices[0].snapshot();
        let mut twin = machine();
        twin.devices[0].restore(&snap).expect("restore");
        assert_eq!(twin.devices[0].snapshot(), snap);
        // The restored twin serves the pressed pad form and the pause
        // bit through its own register surface.
        let source_bus = m.devices[0].bus().expect("bus");
        let twin_bus = twin.devices[0].bus().expect("bus");
        let source_pad = source_bus.peek(Address(0xFCB0));
        let twin_pad = twin_bus.peek(Address(0xFCB0));
        assert_ne!(source_pad, 0x00, "the pressed pad form stood");
        assert_eq!(twin_pad, source_pad, "the pad form restored");
        let twin_switches = twin_bus.peek(Address(0xFCB1));
        let source_switches = source_bus.peek(Address(0xFCB1));
        assert_eq!(
            twin_switches & 0x01,
            source_switches & 0x01,
            "the pause bit restored"
        );
        assert!(twin.devices[0].restore(&snap[..snap.len() - 1]).is_err());
        assert!(twin.devices[0].restore(&[0u8; 4]).is_err());
    }

    #[test]
    fn identical_state_runs_identically() {
        let mut m = machine();
        m.debug_write("bus", Address(0xFD00), 158);
        m.debug_write("bus", Address(0xFD02), 3);
        m.debug_write("bus", Address(0xFD01), 0x98);
        m.debug_write("bus", Address(0xFD08), 104);
        m.debug_write("bus", Address(0xFD0A), 104);
        m.debug_write("bus", Address(0xFD09), 0x1F);
        m.debug_write("bus", Address(0xFD92), 0x01);
        let mut twin = machine();
        twin.debug_write("bus", Address(0xFD00), 158);
        twin.debug_write("bus", Address(0xFD02), 3);
        twin.debug_write("bus", Address(0xFD01), 0x98);
        twin.debug_write("bus", Address(0xFD08), 104);
        twin.debug_write("bus", Address(0xFD0A), 104);
        twin.debug_write("bus", Address(0xFD09), 0x1F);
        twin.debug_write("bus", Address(0xFD92), 0x01);
        m.run_until(Tick(4070));
        twin.run_until(Tick(4070));
        assert_eq!(twin.devices[0].snapshot(), m.devices[0].snapshot());
        assert_eq!(twin.devices[0].next_event(), m.devices[0].next_event());
    }
}
