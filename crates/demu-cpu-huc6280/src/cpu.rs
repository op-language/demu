//! The HuC6280 core: registers, timing, the MPR mapping unit, and the
//! on-die timer and interrupt controller.
//!
//! One domain tick is one 21.47727 MHz master-crystal period. One CPU
//! cycle is 3 domain ticks at the die's high-speed form, which is the
//! PC Engine's 7.15909 MHz CPU clock; a `dividers.cpu` entry overrides
//! the configured divider. Every bus access of an instruction lands on
//! a cycle boundary and carries its exact tick, so watchpoints and DMA
//! see the true access time. The core reschedules itself after every
//! instruction and reads the reset vector through the bus at boot, so
//! the boot ROM decides the entry point.
//!
//! ### The mapping unit
//!
//! The MPR banks translate each 64K logical address onto an 8K
//! physical window: the bank of the access's logical page picks the
//! MPR, and the physical address is the bank shifted over the 8K page
//! offset, `(bank << 13) | (addr & 0x1FFF)`, for every bank value
//! 0x00..0xFF. Two accesses force their bank: the zero page and the
//! T-mode cell always route through MPR 1, the die's rule for the page
//! zero and stack window, and so do the stack accesses. The TAM and
//! TMA instructions reach the MPRs directly and no translation runs
//! for them.
//!
//! The reset mapping is the PC Engine's documented reset state, which
//! the Mednafen PC Engine core and Ootake program identically:
//! `[0xFF, 0xF8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]`. Bank 0xFF names
//! the die's own hardware page at 0x1FE000..0x1FFFFF and bank 0xF8
//! names the 8K work RAM at 0x1F0000..0x1F7FFF; the configuration
//! declares the windows at those physical addresses.
//!
//! ### The on-die units
//!
//! The die intercepts every access whose bank byte is 0xFF, the
//! hardware page. The timer answers at the I/O offsets 0x0C00 (the
//! counter preset and readback), 0x0C01 (the run control), and 0x0C07
//! (the rate select), and the interrupt controller answers at
//! 0x1400..0x1403. Every other offset of the hardware page, the joypad
//! port at 0x1F00 and the PSG at 0x0800..0x080F included, routes
//! through the bus. An intercepted access consumes its cycle and
//! touches no bus and no trigger stream.
//!
//! The interrupt controller follows the published Hudson form: the
//! mask register answers at $1402, where a write sets the disable bits
//! (bit 2 the timer, bit 1 IRQ1, bit 0 IRQ2) and a read returns the
//! mask; the status read answers at $1403; and the timer acknowledge
//! is the write to $1403. The pre-2007 MAME forms (the
//! disable write at $1400 and the enable write at $1401) stay as
//! recorded legacy alias writes beside the published register. The
//! modern MAME core, the Hudson manual, and the MagicKit register
//! reference all name the published form; the real BIOS and game code
//! write the mask at $1402.
//!
//! The vector table sits through MPR 7's current bank: BRK and IRQ2 at
//! $FFF6, IRQ1 at $FFF8, the timer at $FFFA, NMI at $FFFC, and reset
//! at $FFFE. The reset vector is the one exception: the die reads it
//! from bank 0 no matter which bank MPR 7 holds, which is why the PC
//! Engine resets out of ROM bank zero.
//!
//! ### Recorded approximations
//!
//! - The die adds one cycle to each access inside the VDC and VCE
//!   windows (the MAME core models the penalty); this core keeps the
//!   published per-opcode cycle cells and pays none.
//! - The HuC6280 latches data written to the hardware page into an
//!   internal buffer whose high bits some reads return; the modeled
//!   read answers only the die's own register bits and the addressed
//!   device answers its own reads.
//! - The interrupt service priority follows the published form and the
//!   MAME changelog note: NMI, then the timer, then IRQ1, then IRQ2.
//! - The timer decrement interval runs 1024 CPU cycles divided by the
//!   rate select plus one, the recorded formula this build chose. The
//!   MAME core keeps the interval fixed at 1024 CPU cycles and stores
//!   no rate select register at all, so the selects above the reset
//!   form are a demu-side model of the select byte. The counter is the
//!   die's 7-bit form: the preset write masks the value to 7 bits.
//! - The timer decrements on the CPU's instruction-visit path: each
//!   visit applies the decrements that came due since the previous
//!   visit, so an underflow lands at the visit and not on its exact
//!   tick. The internal tick base keeps the remainder, so save and
//!   restore reproduce the schedule exactly.
//! - The register windows also mirror across the hardware page with
//!   the die's decoder strides; this core answers each window at its
//!   base offsets only.
//! - The T-flag mode covers the ADC, AND, EOR, ORA, and SBC
//!   instructions in every addressing form, which the MAME core and
//!   the SET description of the pce_cpu.html opcode reference agree
//!   on: the instruction completes its operand reads, the operation
//!   runs on the zero-page cell that X names, the result stores back
//!   to the same cell, and the accumulator keeps its value. The MAME
//!   form costs the plain instruction 3 cycles more and one more in
//!   decimal mode. The die decodes every instruction with the T flag
//!   clear except the participating forms, the SET instruction, and
//!   the status restores, so a non-participating instruction leaves
//!   the flag clear; the MAME clear-at-decode form is the model here.
//! - The interrupt entry clears the decimal flag and sets the
//!   interrupt disable, and the break instruction clears the T flag
//!   before its push (the MAME form; the CMOS 65C02 leaves the decimal
//!   flag unchanged). A hardware push carries the T flag as it stood.
//! - The reset sequence follows the 6502 power-on form with three
//!   dummy push cycles, which lands the stack pointer at $FD; the MAME
//!   core states the reset stack pointer as $FF instead, and the BIOS
//!   of the machine sets its own stack either way.
//! - The `LDM` immediate-load form that some third-party notes list
//!   for the die does not appear in the published table or in the
//!   MAME and Mednafen cores; opcode $A9 is the immediate load of the
//!   accumulator, per the table.

use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// Flag bits of the status register.
///
/// The published HuC6280 status byte reads N V T B D I Z C (the
/// opcode reference of pce_cpu.html and the MAME h6280 flag layout
/// agree): the die's T flag holds the bit 5 position that a plain 6502
/// leaves unused, and bit 4 is the break flag that only has meaning in
/// a pushed status byte.
pub const FLAG_C: u8 = 0x01;
pub const FLAG_Z: u8 = 0x02;
pub const FLAG_I: u8 = 0x04;
pub const FLAG_D: u8 = 0x08;
/// The break flag; set on a BRK stack push, clear on a hardware push.
pub const FLAG_B: u8 = 0x10;
/// The T flag, the memory-operation flag of the SET instruction.
pub const FLAG_T: u8 = 0x20;
pub const FLAG_V: u8 = 0x40;
pub const FLAG_N: u8 = 0x80;

/// The vector table's cells, read through MPR 7's current bank.
pub const VEC_BRK_IRQ2: u16 = 0xFFF6;
pub const VEC_IRQ1: u16 = 0xFFF8;
pub const VEC_TIMER: u16 = 0xFFFA;
pub const VEC_NMI: u16 = 0xFFFC;
pub const VEC_RESET: u16 = 0xFFFE;

/// The MPR banks the PC Engine documents at reset: MPR 0 holds the
/// hardware page at 0x1FE000..0x1FFFFF and MPR 1 holds the 8K work RAM
/// at 0x1F0000..0x1F7FFF. The Mednafen PC Engine core and Ootake
/// program the same reset banks.
pub const DEFAULT_MPRS: [u8; 8] = [0xFF, 0xF8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

/// The interrupt disable mask bit of the timer source.
pub const MASK_TIMER: u8 = 0x04;
/// The interrupt disable mask bit of the IRQ1 (VDC) source.
pub const MASK_IRQ1: u8 = 0x02;
/// The interrupt disable mask bit of the IRQ2 (CD-ROM) source.
pub const MASK_IRQ2: u8 = 0x01;

/// The register file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regs {
    /// The accumulator.
    pub a: u8,
    /// The X and Y index registers.
    pub x: u8,
    pub y: u8,
    /// The status flags (B carries the push form; T carries the live
    /// memory-operation flag).
    pub p: u8,
    /// The stack pointer.
    pub s: u8,
    /// The program counter.
    pub pc: u16,
}

impl Default for Regs {
    fn default() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            // After power-up the interrupt disable is set and the break
            // flag holds its always-set outside-interrupt form, the
            // state the MAME core states as the reset P.
            p: FLAG_I | FLAG_B,
            // The power-on stack pointer starts its life unset; the
            // reset sequence's three dummy push cycles land it.
            s: 0,
            pc: 0,
        }
    }
}

/// The on-die timer of the die.
///
/// The die counts down a 7-bit counter, reloading it from the preset
/// whenever a decrement underflows and raising the timer pending bit.
/// The decrement interval runs `1024 * cpu_div / (rate + 1)` domain
/// ticks on the die's own decrement clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timer {
    /// The reload value the $0C00 write latched, masked to 7 bits.
    pub preset: u8,
    /// The live counter; a decrement at zero underflows, reloads the
    /// preset, and raises the pending bit.
    pub counter: u8,
    /// The rate select 0..7 the $0C07 write holds.
    pub rate: u8,
    /// True while the counter runs.
    pub enabled: bool,
    /// The internal decrement base in domain ticks, exact across save
    /// and restore.
    pub tick_base: u64,
}

/// The on-die interrupt controller of the die.
///
/// The controller holds the disable mask the $1402 write keeps and the
/// $1402 read returns, the asserted levels of the two external lines,
/// and the timer pending bit the counter raises and the $1403
/// acknowledge clears. The $1400 and $1401 writes keep the mask too,
/// as the recorded legacy alias writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Irq {
    /// The disable mask: a set bit disables its source (bit 2 the
    /// timer, bit 1 IRQ1, bit 0 IRQ2), the published Hudson form.
    pub mask: u8,
    /// The current level of the IRQ1 line.
    pub irq1: bool,
    /// The current level of the IRQ2 line.
    pub irq2: bool,
    /// The latched timer pending, cleared by the acknowledge write.
    pub timer_pending: bool,
}

impl Irq {
    /// The pending status byte the $1403 read answers.
    fn status(&self) -> u8 {
        let timer = u8::from(self.timer_pending) << 2;
        let irq1 = u8::from(self.irq1) << 1;
        timer | irq1 | u8::from(self.irq2)
    }
}

/// An interrupt source of the die, in the priority order the core
/// services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptSource {
    /// A latched NMI edge.
    Nmi,
    /// The IRQ1 line, the VDC.
    Irq1,
    /// The die's own timer.
    Timer,
    /// The IRQ2 line, the CD-ROM interface.
    Irq2,
}

/// The service vector of one interrupt source.
pub fn interrupt_vector(source: InterruptSource) -> u16 {
    match source {
        InterruptSource::Nmi => VEC_NMI,
        InterruptSource::Irq1 => VEC_IRQ1,
        InterruptSource::Timer => VEC_TIMER,
        InterruptSource::Irq2 => VEC_BRK_IRQ2,
    }
}

/// The die's routing rule for one logical access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessRule {
    /// The bank of the access's own 8K logical page.
    Generic,
    /// The zero-page rule: the die routes the zero page and the
    /// T-mode cell through MPR 1.
    Zp,
    /// The stack rule: the die routes the stack through MPR 1.
    Stack,
    /// The ST0, ST1, and ST2 route: the die routes the three fixed
    /// hardware-page ports through MPR 0.
    Mpr0,
    /// The reset-vector route: the die reads the reset vector from
    /// bank 0 no matter which bank MPR 7 holds.
    Bank0,
}

/// The HuC6280 core.
pub struct Huc6280 {
    name: String,
    /// The name of the bus this CPU accesses.
    pub bus_name: String,
    /// Domain ticks per CPU cycle: 3 at the high-speed form.
    pub cpu_div: u64,
    /// The configured cycle divider, restored by `reset`.
    pub(crate) base_div: u64,
    /// The register file.
    pub regs: Regs,
    /// The MPR banks of the mapping unit.
    pub(crate) mprs: [u8; 8],
    /// The current level of the NMI line, for edge detection.
    pub(crate) nmi_line: bool,
    /// A latched NMI edge waiting for the next instruction boundary.
    pub(crate) nmi_edge: bool,
    /// The on-die timer unit.
    pub(crate) timer: Timer,
    /// The on-die interrupt controller.
    pub(crate) irq: Irq,
    /// The power-on reset sequence waits on the next visit.
    pub(crate) reset_pending: bool,
    pub(crate) next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl std::fmt::Debug for Huc6280 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Huc6280")
            .field("name", &self.name)
            .field("regs", &self.regs)
            .field("mprs", &self.mprs)
            .field("cpu_div", &self.cpu_div)
            .field("timer", &self.timer)
            .field("irq", &self.irq)
            .field("reset_pending", &self.reset_pending)
            .finish()
    }
}

impl Huc6280 {
    /// A core from its configuration entry.
    ///
    /// The `cpu` divider entry, when present, overrides the default of
    /// 3 domain ticks per cycle.
    pub fn new(
        device: &demu_config::model::DeviceInstance,
        config: &demu_config::model::SystemConfig,
    ) -> Self {
        let bus_name = device
            .params
            .get("bus")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                config
                    .buses
                    .first()
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| "cpu".to_string())
            });
        let base_div = device
            .dividers
            .get("cpu")
            .copied()
            .map(|d| u64::from(if d == 0 { 1 } else { d }))
            .unwrap_or(3);
        Self {
            name: device.name.to_string(),
            bus_name,
            cpu_div: base_div,
            base_div,
            regs: Regs::default(),
            mprs: DEFAULT_MPRS,
            nmi_line: false,
            nmi_edge: false,
            timer: Timer::default(),
            irq: Irq::default(),
            reset_pending: false,
            next: None,
            last_tick: None,
        }
    }

    /// Schedule the next core visit `m` CPU cycles after `tick`.
    pub fn schedule_after(&mut self, tick: Tick, m: u32) {
        self.next = Some(Tick(tick.0 + self.cpu_div * u64::from(m)));
    }

    /// The first cycle boundary at or after `tick`.
    fn next_boundary(&self, tick: Tick) -> Option<Tick> {
        let boundary = tick.0.div_ceil(self.cpu_div) * self.cpu_div;
        Some(Tick(boundary))
    }

    /// Wake an idle core at the next cycle boundary.
    fn wake(&mut self, tick: Tick) {
        if self.next.is_none() {
            self.next = self.next_boundary(tick);
            self.last_tick = None;
        }
    }

    /// The die's translation of one logical access: the 21-bit physical
    /// address and whether the bank byte names the on-die hardware page.
    ///
    /// The physical address is `(bank << 13) | (addr & 0x1FFF)` for
    /// every bank value, so the hardware page sits in bank 0xFF and the
    /// translation covers the whole 2 megabyte form for all of them.
    pub fn route(&self, addr: u16, rule: AccessRule) -> (u32, bool) {
        let offset = addr & 0x1FFF;
        let bank = match rule {
            AccessRule::Generic => self.mprs[((addr >> 13) & 0x07) as usize],
            AccessRule::Zp | AccessRule::Stack => self.mprs[1],
            AccessRule::Mpr0 => self.mprs[0],
            AccessRule::Bank0 => 0,
        };
        let phys = (u32::from(bank) << 13) | u32::from(offset);
        (phys, bank == 0xFF)
    }

    /// The die answers an internal register read at this hardware-page
    /// I/O offset, or `None` when the offset routes through the bus.
    ///
    /// The timer counter readback gives the live 7-bit countdown; the
    /// mask readback at `0x1402` gives the interrupt disable mask; the
    /// status read at `0x1403` gives the pending bits, bit 2 the timer,
    /// bit 1 IRQ1, bit 0 IRQ2.
    pub(crate) fn on_die_read(&self, offset: u16) -> Option<u8> {
        match offset {
            0x0C00 => Some(self.timer.counter),
            0x1402 => Some(self.irq.mask),
            0x1403 => Some(self.irq.status()),
            _ => None,
        }
    }

    /// The die absorbs an internal register write at this hardware-page
    /// I/O offset and reports whether it took the cycle; `at` is the
    /// access tick, which re-arms the timer decrement base.
    pub(crate) fn on_die_write(&mut self, offset: u16, value: u8, at: Tick) -> bool {
        match offset {
            // The counter preload: the write latches the reload value,
            // and the live counter keeps counting until its reload.
            0x0C00 => {
                self.timer.preset = value & 0x7F;
                true
            }
            // The run control: bit 0 runs the counter. A write with the
            // bit set while the counter is stopped reloads the counter
            // from the preset and re-arms the base at the access tick;
            // the counter keeps its value through a stop.
            0x0C01 => {
                if value & 0x01 != 0 {
                    if !self.timer.enabled {
                        self.timer.enabled = true;
                        self.timer.counter = self.timer.preset;
                        self.timer.tick_base = at.0;
                    }
                } else {
                    self.timer.enabled = false;
                }
                true
            }
            // The rate select: the decrement interval divides by the
            // select plus one from the following decrement.
            0x0C07 => {
                self.timer.rate = value & 0x07;
                true
            }
            // The interrupt mask: a set bit disables (masks) its
            // source, and the readback returns the mask, both at the
            // published $1402 register.
            0x1402 => {
                self.irq.mask = value & 0x07;
                true
            }
            // The acknowledge: the write clears the timer pending bit.
            0x1403 => {
                self.irq.timer_pending = false;
                true
            }
            // The legacy mask forms of the pre-2007 MAME core, kept as
            // recorded alias writes beside the published register: the
            // disable at $1400 and the enable at $1401.
            0x1400 => {
                self.irq.mask = value & 0x07;
                true
            }
            0x1401 => {
                self.irq.mask &= !(value & 0x07);
                true
            }
            _ => false,
        }
    }

    /// The decrement interval of the timer in domain ticks: 1024 CPU
    /// cycles divided by the rate select plus one.
    pub(crate) fn timer_interval(&self, rate: u8) -> u64 {
        let rate = u64::from(rate & 0x07);
        self.cpu_div * 1024 / (rate + 1)
    }

    /// Apply the timer decrements that came due since the last visit.
    ///
    /// Each decrement counts the live counter down; a decrement at
    /// zero underflows, reloads the preset, and raises the timer
    /// pending bit. The tick base advances to the last applied
    /// decrement, which keeps the interval remainder exact for save
    /// and restore.
    pub(crate) fn run_timer(&mut self, tick: Tick) {
        if !self.timer.enabled {
            return;
        }
        let interval = self.timer_interval(self.timer.rate);
        let elapsed = tick.0.saturating_sub(self.timer.tick_base);
        let steps = elapsed / interval;
        if steps == 0 {
            return;
        }
        self.timer.tick_base += steps * interval;
        for _ in 0..steps {
            if self.timer.counter == 0 {
                self.timer.counter = self.timer.preset;
                self.irq.timer_pending = true;
            } else {
                self.timer.counter -= 1;
            }
        }
    }

    /// The pending interrupt source at this instruction boundary,
    /// highest priority first: NMI, then the timer, then IRQ1, then
    /// IRQ2, per the published priority and the MAME changelog note.
    pub fn interrupt_pending(&self) -> bool {
        if self.nmi_edge {
            return true;
        }
        // The interrupt-disable flag gates the three line sources the
        // 6502 way; the controller disable mask gates them further.
        if self.regs.p & FLAG_I != 0 {
            return false;
        }
        let mask = self.irq.mask;
        (self.irq.irq1 && mask & MASK_IRQ1 == 0)
            || (self.irq.timer_pending && mask & MASK_TIMER == 0)
            || (self.irq.irq2 && mask & MASK_IRQ2 == 0)
    }

    /// Consume the pending interrupt source for one service, highest
    /// priority first. The NMI edge is consumed here; the level and
    /// latched sources of the controller stay until the controller
    /// clears them, so an entry repeats while its source stands.
    pub(crate) fn take_interrupt(&mut self) -> Option<InterruptSource> {
        if self.nmi_edge {
            self.nmi_edge = false;
            return Some(InterruptSource::Nmi);
        }
        // The interrupt-disable flag gates the three line sources the
        // 6502 way; the controller disable mask gates them further.
        if self.regs.p & FLAG_I != 0 {
            return None;
        }
        let mask = self.irq.mask;
        if self.irq.timer_pending && mask & MASK_TIMER == 0 {
            return Some(InterruptSource::Timer);
        }
        if self.irq.irq1 && mask & MASK_IRQ1 == 0 {
            return Some(InterruptSource::Irq1);
        }
        if self.irq.irq2 && mask & MASK_IRQ2 == 0 {
            return Some(InterruptSource::Irq2);
        }
        None
    }

    /// The pushed status form: the break flag set on a software push
    /// and clear on a hardware push, with the T flag at bit 5 carrying
    /// the live die bit.
    pub fn push_p(&self, software: bool) -> u8 {
        if software {
            self.regs.p | FLAG_B
        } else {
            self.regs.p & !FLAG_B
        }
    }
}

impl Device for Huc6280 {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn cpu(&self) -> Option<&dyn demu_core::CpuDebug> {
        Some(self)
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        Some(self)
    }

    fn reset(&mut self) {
        self.regs = Regs::default();
        self.mprs = DEFAULT_MPRS;
        self.cpu_div = self.base_div;
        self.nmi_line = false;
        self.nmi_edge = false;
        self.timer = Timer::default();
        self.irq = Irq::default();
        self.reset_pending = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Cpu { registers }) = state {
            let debug: &mut dyn demu_core::CpuDebug = self;
            for (name, value) in registers {
                debug.set_register(name, *value);
            }
        }
        // The power-on reset sequence runs on the first visit and reads
        // the reset vector through bank 0.
        self.reset_pending = true;
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(b"H6280");
        out.push(1);
        out.extend_from_slice(&[
            self.regs.a,
            self.regs.x,
            self.regs.y,
            self.regs.s,
            self.regs.p,
        ]);
        out.extend_from_slice(&self.regs.pc.to_le_bytes());
        out.extend_from_slice(&self.mprs);
        out.push(u8::from(self.nmi_line));
        out.push(u8::from(self.nmi_edge));
        out.push(u8::from(self.reset_pending));
        out.extend_from_slice(&[self.irq.mask, 0]);
        out.extend_from_slice(&[
            u8::from(self.irq.irq1),
            u8::from(self.irq.irq2),
            u8::from(self.irq.timer_pending),
        ]);
        out.extend_from_slice(&[self.timer.preset, self.timer.counter, self.timer.rate]);
        out.push(u8::from(self.timer.enabled));
        out.extend_from_slice(&self.timer.tick_base.to_le_bytes());
        out.extend_from_slice(&self.cpu_div.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 5 + 1 + 5 + 2 + 8 + 3 + 2 + 3 + 3 + 1 + 8 + 8;
        if data.len() != expected {
            return Err(SnapshotError::Mismatch(format!(
                "huc6280 chunk must be {expected} bytes, got {}",
                data.len()
            )));
        }
        if &data[..5] != b"H6280" {
            return Err(SnapshotError::Mismatch("bad huc6280 tag".into()));
        }
        if data[5] != 1 {
            return Err(SnapshotError::Mismatch(
                "unsupported huc6280 chunk version".into(),
            ));
        }
        self.regs.a = data[6];
        self.regs.x = data[7];
        self.regs.y = data[8];
        self.regs.s = data[9];
        // The break flag holds its live set form and the T flag comes
        // from the die state.
        self.regs.p = data[10] | FLAG_B;
        self.regs.pc = u16::from_le_bytes([data[11], data[12]]);
        self.mprs = [
            data[13], data[14], data[15], data[16], data[17], data[18], data[19], data[20],
        ];
        self.nmi_line = data[21] != 0;
        self.nmi_edge = data[22] != 0;
        self.reset_pending = data[23] != 0;
        self.irq.mask = data[24] & 0x07;
        self.irq.irq1 = data[26] != 0;
        self.irq.irq2 = data[27] != 0;
        self.irq.timer_pending = data[28] != 0;
        self.timer.preset = data[29] & 0x7F;
        self.timer.counter = data[30] & 0x7F;
        self.timer.rate = data[31] & 0x07;
        self.timer.enabled = data[32] != 0;
        self.timer.tick_base = le64(&data[33..41]);
        self.cpu_div = le64(&data[41..49]).clamp(1, 64);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match line {
            "nmi" => {
                let was = self.nmi_line;
                self.nmi_line = level == Level::High;
                // The NMI input is edge sensitive: a second pulse while
                // the line stays asserted does not re-latch.
                if self.nmi_line && !was {
                    self.nmi_edge = true;
                    self.wake(tick);
                }
            }
            "irq1" => {
                self.irq.irq1 = level == Level::High;
                if self.irq.irq1 {
                    self.wake(tick);
                }
            }
            "irq2" => {
                self.irq.irq2 = level == Level::High;
                if self.irq.irq2 {
                    self.wake(tick);
                }
            }
            _ => {}
        }
    }

    fn next_event(&self) -> Option<Tick> {
        if let Some(next) = self.next {
            return Some(next);
        }
        // The armed timer keeps the core visited by itself: the next
        // decrement sits one interval past the last applied one, and
        // the visit applies the due decrements before the core runs.
        if self.timer.enabled {
            Some(Tick(
                self.timer.tick_base + self.timer_interval(self.timer.rate),
            ))
        } else {
            None
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        self.run_timer(tick);
        crate::exec::step(self, tick, ctx);
    }
}

/// The little-endian 64-bit value of a snapshot byte slice.
fn le64(bytes: &[u8]) -> u64 {
    let mut value = 0u64;
    for (shift, byte) in bytes.iter().enumerate().take(8) {
        value |= u64::from(*byte) << (shift * 8);
    }
    value
}

impl demu_core::CpuDebug for Huc6280 {
    fn registers(&self) -> std::collections::BTreeMap<String, u64> {
        let mut map = std::collections::BTreeMap::new();
        map.insert("A".to_string(), u64::from(self.regs.a));
        map.insert("X".to_string(), u64::from(self.regs.x));
        map.insert("Y".to_string(), u64::from(self.regs.y));
        map.insert("S".to_string(), u64::from(self.regs.s));
        map.insert("SP".to_string(), u64::from(u16::from(self.regs.s) | 0x100));
        map.insert("P".to_string(), u64::from(self.regs.p));
        map.insert("PC".to_string(), u64::from(self.regs.pc));
        for (i, bank) in self.mprs.iter().enumerate() {
            map.insert(format!("MPR{i}"), u64::from(*bank));
        }
        map.insert("IM".to_string(), u64::from(self.irq.mask));
        map
    }

    fn set_register(&mut self, name: &str, value: u64) -> bool {
        let lo = value as u8;
        if let Some(bank) = name.to_ascii_uppercase().strip_prefix("MPR") {
            match bank.parse::<usize>() {
                Ok(i) if i <= 7 => {
                    self.mprs[i] = lo;
                    true
                }
                _ => false,
            }
        } else {
            match name.to_ascii_uppercase().as_str() {
                "A" => self.regs.a = lo,
                "X" => self.regs.x = lo,
                "Y" => self.regs.y = lo,
                "S" | "SP" => self.regs.s = lo,
                "P" => self.regs.p = lo | FLAG_B,
                "PC" => self.regs.pc = value as u16,
                _ => return false,
            }
            true
        }
    }
}

/// A bus access helper bound to one instruction's cycle counter.
///
/// Every access stamps its tick as the start tick plus one cycle per
/// preceding access. The helper forwards the trigger events, because
/// the machine context is exclusively borrowed here and the die's own
/// register reads and writes stay in the core's hands.
pub struct Access<'a, 'c> {
    ctx: &'c mut MachineCtx<'a>,
    tick: Tick,
    k: u32,
    cpu_div: u64,
    bus_name: String,
}

impl<'a, 'c> Access<'a, 'c> {
    /// An access helper for one instruction at its start tick.
    pub fn new(ctx: &'c mut MachineCtx<'a>, tick: Tick, bus_name: String, cpu_div: u64) -> Self {
        Self {
            ctx,
            tick,
            k: 0,
            cpu_div,
            bus_name,
        }
    }

    /// The stamp of the access at the current cycle index.
    pub fn stamp(&self) -> Tick {
        Tick(self.tick.0 + self.cpu_div * u64::from(self.k))
    }

    /// Read one byte at cycle `k`, then advance the index.
    #[inline]
    pub fn rd(&mut self, addr: u32) -> u8 {
        let at = self.stamp();
        self.k += 1;
        self.ctx.read(
            &self.bus_name,
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(addr),
            at,
        ) as u8
    }

    /// Write one byte at cycle `k`, then advance the index.
    #[inline]
    pub fn wr(&mut self, addr: u32, value: u8) {
        let at = self.stamp();
        self.k += 1;
        self.ctx.write(
            &self.bus_name,
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(addr),
            u32::from(value),
            at,
        );
    }

    /// Skip one internal cycle.
    #[inline]
    pub fn wait(&mut self) {
        self.k += 1;
    }

    /// The number of cycles consumed so far.
    pub fn cycles(&self) -> u32 {
        self.k
    }

    /// Report a runtime event from the core.
    pub fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        self.ctx.emit(event);
    }
}

/// The bus abstraction of one instruction's accesses over the die's
/// own 21-bit physical addresses: the machine context stamps exact
/// ticks, and the instruction tests run over a plain byte map with the
/// same step function.
pub trait Cycles {
    /// Read one byte at a 21-bit physical address; access stamping
    /// stays with the implementation.
    fn rd(&mut self, addr: u32) -> u8;
    /// Write one byte at a 21-bit physical address.
    fn wr(&mut self, addr: u32, value: u8);
    /// Skip one internal cycle.
    fn wait(&mut self);
    /// The number of cycles consumed so far.
    fn cycles(&self) -> u32;
    /// Report one runtime event from the core.
    fn emit(&mut self, event: demu_core::triggers::TriggerEvent);
}

impl Cycles for Access<'_, '_> {
    fn rd(&mut self, addr: u32) -> u8 {
        Access::rd(self, addr)
    }

    fn wr(&mut self, addr: u32, value: u8) {
        Access::wr(self, addr, value)
    }

    fn wait(&mut self) {
        Access::wait(self)
    }

    fn cycles(&self) -> u32 {
        Access::cycles(self)
    }

    fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        Access::emit(self, event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(divider: Option<u32>) -> demu_config::model::DeviceInstance {
        let dividers_raw = match divider {
            Some(d) => format!("\n[device.dividers]\ncpu = {d}"),
            None => String::new(),
        };
        let raw = format!(
            r#"
            [system]
            name = "test"
            triple = "test"
            [[bus]]
            name = "cpu"
            address_bits = 21
            owner = "cpu0"
            [[device]]
            name = "cpu0"
            kind = "huc6280.cpu"
            domain = "main"{dividers_raw}
            [clock.main]
            frequency = "21477272"
            "#
        );
        demu_config::validate(&demu_config::RawConfig::parse(&raw).expect("parse"))
            .expect("validate")
            .0
            .devices
            .remove(0)
    }

    fn config_of(kind: &str) -> demu_config::model::SystemConfig {
        let raw = format!(
            r#"
            [system]
            name = "test"
            triple = "test"
            [[bus]]
            name = "cpu"
            address_bits = 21
            owner = "cpu0"
            [clock.main]
            frequency = "21477272"
            [[device]]
            name = "cpu0"
            kind = "{kind}"
            domain = "main"
            "#
        );
        demu_config::validate(&demu_config::RawConfig::parse(&raw).expect("parse"))
            .expect("validate")
            .0
    }

    #[test]
    fn mapping_default_and_overridden_divider() {
        // The default divider is the die's 3 domain ticks per cycle.
        let cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        assert_eq!(cpu.cpu_div, 3);
        assert_eq!(cpu.base_div, 3);
        assert_eq!(cpu.mprs, DEFAULT_MPRS);
        // A dividers.cpu entry overrides the default.
        let tuned = Huc6280::new(&instance(Some(6)), &config_of("huc6280.cpu"));
        assert_eq!(tuned.cpu_div, 6);
        assert_eq!(tuned.base_div, 6);
    }

    #[test]
    fn power_on_state() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.reset();
        cpu.boot(None);
        assert_eq!(cpu.regs.p & FLAG_I, FLAG_I);
        assert_eq!(cpu.regs.p & FLAG_B, FLAG_B);
        assert_eq!(cpu.regs.p & FLAG_T, 0);
        assert_eq!(cpu.regs.s, 0);
        assert_eq!(cpu.mprs, DEFAULT_MPRS);
        assert_eq!(cpu.next, Some(Tick(0)));
        // The boot preset overrides the power-on form.
        let presets = demu_config::model::BootState::Cpu {
            registers: [("s".to_string(), 0x40), ("MPR7".to_string(), 0x10)]
                .into_iter()
                .collect(),
        };
        cpu.boot(Some(&presets));
        assert_eq!(cpu.regs.s, 0x40);
        assert_eq!(cpu.mprs[7], 0x10);
    }

    #[test]
    fn reset_clears_the_die_state() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.boot(None);
        cpu.mprs = [0x05; 8];
        cpu.irq.mask = 0x07;
        cpu.irq.irq1 = true;
        cpu.irq.timer_pending = true;
        cpu.timer.enabled = true;
        cpu.timer.preset = 0x40;
        cpu.reset();
        assert_eq!(cpu.mprs, DEFAULT_MPRS);
        assert_eq!(cpu.irq.mask, 0);
        assert!(!cpu.irq.irq1);
        assert!(!cpu.irq.timer_pending);
        assert!(!cpu.timer.enabled);
        assert_eq!(cpu.timer.preset, 0);
        assert_eq!(cpu.regs.p & FLAG_I, FLAG_I);
        assert_eq!(cpu.regs.p & FLAG_B, FLAG_B);
        assert_eq!(cpu.cpu_div, cpu.base_div);
    }

    #[test]
    fn nmi_latches_once_per_edge() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.boot(None);
        cpu.notify_line("nmi", Level::High, Tick(0));
        assert!(cpu.nmi_edge);
        cpu.notify_line("nmi", Level::High, Tick(12));
        assert!(cpu.take_interrupt().is_some());
        assert!(cpu.take_interrupt().is_none());
        cpu.notify_line("nmi", Level::Low, Tick(24));
        cpu.notify_line("nmi", Level::High, Tick(24));
        assert!(cpu.nmi_edge, "a new edge latches again");
    }

    #[test]
    fn line_levels_reach_the_controller() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.boot(None);
        cpu.notify_line("irq1", Level::High, Tick(0));
        cpu.notify_line("irq2", Level::High, Tick(0));
        assert!(cpu.irq.irq1);
        assert!(cpu.irq.irq2);
        // The mask register reads back its stored form.
        assert_eq!(cpu.on_die_read(0x1402), Some(0x00), "the mask readback");
        // The pending bits answer at the status read.
        assert_eq!(
            cpu.on_die_read(0x1403),
            Some(0x03),
            "the pending bits read back"
        );
        // The disable mask masks the sources; a service clears nothing.
        cpu.irq.mask = 0x07;
        assert!(cpu.take_interrupt().is_none());
        // An unrecognized line stays inert: no latching on the die.
        cpu.notify_line("reset", Level::High, Tick(0));
        assert!(!cpu.nmi_edge);
    }

    #[test]
    fn timer_counts_at_the_select_interval() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        // The rate select divides the 1024-cycle base at the divider.
        assert_eq!(cpu.timer_interval(0), 3072);
        assert_eq!(cpu.timer_interval(7), 384);
        // The $0C07 write stores the select and the interval follows.
        cpu.on_die_write(0x0C07, 0x01, Tick(0));
        assert_eq!(cpu.timer.rate, 1);
        assert_eq!(cpu.timer_interval(cpu.timer.rate), 3072 / 2);
        cpu.on_die_write(0x0C07, 0x00, Tick(0));
        assert_eq!(cpu.timer.rate, 0);
        // The preset masks to the die's 7-bit counter form.
        cpu.on_die_write(0x0C00, 0xFF, Tick(0));
        assert_eq!(cpu.timer.preset, 0x7F);
        // The run write reloads the counter from the preset.
        cpu.on_die_write(0x0C01, 0x01, Tick(10));
        assert!(cpu.timer.enabled);
        assert_eq!(cpu.timer.counter, 0x7F);
        assert_eq!(cpu.timer.tick_base, 10);
        // A repeat run write does not reload.
        cpu.on_die_write(0x0C01, 0x01, Tick(4000));
        assert_eq!(cpu.timer.counter, 0x7F);
        // One decrement lands at the base plus the interval.
        cpu.run_timer(Tick(10 + 3072));
        assert_eq!(cpu.timer.counter, 0x7E);
        assert_eq!(cpu.timer.tick_base, 10 + 3072);
        // The underflow reloads the preset and raises the pending bit.
        cpu.run_timer(Tick(10 + 3072 * 128));
        assert!(cpu.irq.timer_pending);
        assert_eq!(cpu.timer.counter, cpu.timer.preset);
        // The stop write keeps the counter; a restart reloads it again.
        let mid = cpu.timer.counter;
        cpu.on_die_write(0x0C01, 0x00, Tick(10 + 3072 * 129));
        assert!(!cpu.timer.enabled);
        assert_eq!(cpu.timer.counter, mid);
        cpu.on_die_write(0x0C01, 0x01, Tick(10 + 3072 * 130));
        assert_eq!(cpu.timer.counter, 0x7F);
    }

    #[test]
    fn service_priority_and_vector_table() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.boot(None);
        cpu.regs.p &= !FLAG_I;
        // All three line sources stand: the priority order runs the
        // timer, then IRQ1, then IRQ2, with NMI above all of them. The
        // take consumes the NMI edge alone; the level and latched
        // sources stay until the controller clears them.
        cpu.notify_line("nmi", Level::High, Tick(0));
        cpu.notify_line("irq1", Level::High, Tick(0));
        cpu.notify_line("irq2", Level::High, Tick(0));
        cpu.irq.timer_pending = true;
        assert_eq!(cpu.take_interrupt(), Some(InterruptSource::Nmi));
        assert_eq!(cpu.take_interrupt(), Some(InterruptSource::Timer));
        cpu.irq.timer_pending = false;
        assert_eq!(cpu.take_interrupt(), Some(InterruptSource::Irq1));
        cpu.irq.irq1 = false;
        assert_eq!(cpu.take_interrupt(), Some(InterruptSource::Irq2));
        cpu.irq.irq2 = false;
        assert!(cpu.take_interrupt().is_none());
        // The vectors answer through MPR 7's bank by source.
        assert_eq!(interrupt_vector(InterruptSource::Nmi), 0xFFFC);
        assert_eq!(interrupt_vector(InterruptSource::Irq1), 0xFFF8);
        assert_eq!(interrupt_vector(InterruptSource::Timer), 0xFFFA);
        assert_eq!(interrupt_vector(InterruptSource::Irq2), 0xFFF6);
    }

    #[test]
    fn interrupt_disable_flag_gates_the_sources() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.boot(None);
        cpu.regs.p |= FLAG_I;
        cpu.irq.irq1 = true;
        cpu.irq.timer_pending = true;
        cpu.irq.irq2 = true;
        assert!(cpu.take_interrupt().is_none(), "the disable flag holds");
        cpu.regs.p &= !FLAG_I;
        assert!(cpu.take_interrupt().is_some());
    }

    #[test]
    fn acknowledge_clears_the_timer_pending() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.irq.timer_pending = true;
        assert_eq!(cpu.on_die_read(0x1403), Some(0x04));
        cpu.on_die_write(0x1403, 0, Tick(0));
        assert_eq!(cpu.on_die_read(0x1403), Some(0x00));
    }

    #[test]
    fn snapshot_round_trip() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.boot(None);
        cpu.regs.a = 0x42;
        cpu.regs.pc = 0x1234;
        cpu.mprs = [0x11, 0xF8, 0x20, 0x00, 0x00, 0x00, 0x00, 0x10];
        cpu.irq.mask = 0x05;
        cpu.irq.timer_pending = true;
        cpu.timer.enabled = true;
        cpu.timer.preset = 0x35;
        cpu.timer.counter = 0x12;
        cpu.timer.rate = 3;
        cpu.timer.tick_base = 0x1234_5678;
        cpu.regs.p |= FLAG_I;
        let snapshot = cpu.snapshot();
        cpu.boot(None);
        cpu.restore(&snapshot).expect("restore");
        assert_eq!(cpu.regs.a, 0x42);
        assert_eq!(cpu.regs.pc, 0x1234);
        assert_eq!(cpu.mprs[2], 0x20);
        assert_eq!(cpu.mprs[7], 0x10);
        assert_eq!(cpu.irq.mask, 0x05);
        assert!(cpu.irq.timer_pending);
        assert!(cpu.timer.enabled);
        assert_eq!(cpu.timer.preset, 0x35);
        assert_eq!(cpu.timer.rate, 3);
        assert_eq!(cpu.timer.tick_base, 0x1234_5678);
        // The restore leaves the core idle for the machine to schedule.
        assert_eq!(cpu.next, None);
        // The break flag holds its set form after the restore.
        assert_eq!(cpu.regs.p & FLAG_B, FLAG_B);
    }

    #[test]
    fn restore_rejects_the_wrong_chunk() {
        let mut cpu = Huc6280::new(&instance(None), &config_of("huc6280.cpu"));
        cpu.reset();
        let too_short = vec![0u8; 10];
        assert!(cpu.restore(&too_short).is_err());
        let mut wrong_tag = cpu.snapshot();
        wrong_tag[0] = b'X';
        assert!(cpu.restore(&wrong_tag).is_err());
        let mut wrong_version = cpu.snapshot();
        wrong_version[4] = 2;
        assert!(cpu.restore(&wrong_version).is_err());
    }
}
