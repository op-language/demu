//! Motorola 68000 CPU core.
//!
//! The core runs one instruction per visit and stamps every bus
//! access with its exact domain tick. The `cpu` divider names the
//! domain ticks per 68000 cycle; the machine's clock domain carries
//! the master crystal and the divider produces the published CPU
//! clock. Interrupt levels arrive on the three IPL lines
//! (`ipl0`, `ipl1`, `ipl2`) — the decoded level's bits, open-collector
//! driven with the machine's high-assert convention. `dma_hold` pauses
//! the core between instructions while a device seizes the bus.
//!
//! Kinds registered here:
//!
//! - `m68000.cpu` — the NMOS 68000 core.

mod exec;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod exec_tests;

use std::collections::BTreeMap;

use demu_config::model::{BootState, DeviceInstance};
use demu_core::device::{AccessWidth, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{CpuDebug, Device, Snapshot, SnapshotError};

/// One open-collector IPL line: the pull count of the drivers that
/// assert the line.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct OcPin {
    pulls: u32,
}

impl OcPin {
    /// True while the line sits asserted from at least one driver.
    fn low(&self) -> bool {
        self.pulls > 0
    }

    fn pull(&mut self) -> bool {
        self.pulls += 1;
        true
    }

    fn release(&mut self) -> bool {
        self.pulls = self.pulls.saturating_sub(1);
        true
    }

    fn clear(&mut self) {
        self.pulls = 0;
    }
}

/// The 68000 core state.
#[derive(Debug)]
pub struct M68000 {
    name: String,
    /// The domain ticks per CPU cycle, from the `cpu` divider.
    pub(crate) div: u64,
    /// The memory bus the core drives.
    pub(crate) bus_name: String,
    /// The data registers D0-D7.
    pub(crate) d: [u32; 8],
    /// The address registers A0-A6 plus A7, which splits into the
    /// user and supervisor stack pointers through the S bit.
    pub(crate) a: [u32; 8],
    pub(crate) usp: u32,
    pub(crate) ssp: u32,
    pub(crate) pc: u32,
    /// The status bits: trace, supervisor, interrupt mask 0-7, and
    /// the CCR XNZVC.
    pub(crate) trace: bool,
    pub(crate) sup: bool,
    pub(crate) i_field: u8,
    pub(crate) carry: bool,
    pub(crate) over: bool,
    pub(crate) zero: bool,
    pub(crate) neg: bool,
    pub(crate) extend: bool,
    /// The three interrupt lines, open-collector driven: every
    /// driver that asserts pulls its line, and the last release
    /// clears it, so the machine wires both CPUs' interrupt sources
    /// through declared connections.
    pub(crate) ipl: [OcPin; 3],
    /// The bus hold from a device that seized the bus.
    pub(crate) held: bool,
    /// The core parked with STOP.
    pub halted: bool,
    pub(crate) next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl M68000 {
    /// Wake the parked core at the next CPU boundary after the edge's
    /// tick stamp: the wake never rewinds the schedule, and only a
    /// core with no pending visit takes it.
    pub(crate) fn wake(&mut self, tick: Tick) {
        if self.next.is_none() {
            self.next = Some(Tick(tick.0 + self.div));
            self.last_tick = None;
        }
    }

    /// A core from its configuration entry.
    pub fn new(device: &DeviceInstance, config: &demu_config::model::SystemConfig) -> Self {
        let mut bus_name = String::from("cpu");
        if let Some(v) = device.params.get("bus").and_then(|v| v.as_str()) {
            bus_name = v.to_string();
        }
        let div = device.dividers.get("cpu").copied().unwrap_or(1).max(1) as u64;
        let _ = config;
        Self {
            name: device.name.to_string(),
            div,
            bus_name,
            d: [0; 8],
            a: [0; 8],
            usp: 0,
            ssp: 0,
            pc: 0,
            trace: false,
            sup: false,
            i_field: 0,
            carry: false,
            over: false,
            zero: false,
            neg: false,
            extend: false,
            ipl: [OcPin::default(); 3],
            held: false,
            halted: false,
            next: None,
            last_tick: None,
        }
    }

    /// The stack pointer for the current state.
    pub(crate) fn sp(&self) -> u32 {
        if self.sup { self.ssp } else { self.usp }
    }

    /// The stack pointer for the current state with a writeback.
    pub(crate) fn set_sp(&mut self, v: u32) {
        if self.sup {
            self.ssp = v;
        } else {
            self.usp = v;
        }
    }

    /// The currently decoded interrupt level: the asserted IPL lines
    /// form the level's binary digits.
    pub(crate) fn interrupt_level(&self) -> u8 {
        let mut level = 0u8;
        for (i, pin) in self.ipl.iter().enumerate() {
            if pin.low() {
                level |= 1 << i;
            }
        }
        level
    }

    /// Decode the full status-register word.
    pub(crate) fn sr_word(&self) -> u16 {
        (u16::from(self.trace) << 15)
            | (u16::from(self.sup) << 13)
            | u16::from(self.i_field) << 8
            | (u16::from(self.extend) << 4)
            | (u16::from(self.neg) << 3)
            | (u16::from(self.zero) << 2)
            | (u16::from(self.over) << 1)
            | u16::from(self.carry)
    }

    /// Install a status word; the T and S bits move with it.
    pub(crate) fn set_sr_word(&mut self, word: u16) {
        self.trace = (word & 0x8000) != 0;
        self.sup = (word & 0x2000) != 0;
        self.i_field = ((word >> 8) & 0x07) as u8;
        self.extend = (word & 0x0010) != 0;
        self.neg = (word & 0x0008) != 0;
        self.zero = (word & 0x0004) != 0;
        self.over = (word & 0x0002) != 0;
        self.carry = (word & 0x0001) != 0;
    }

    /// Install a condition-code byte; the T and S bits stay.
    pub(crate) fn set_ccr(&mut self, byte: u8) {
        self.extend = (byte & 0x10) != 0;
        self.neg = (byte & 0x08) != 0;
        self.zero = (byte & 0x04) != 0;
        self.over = (byte & 0x02) != 0;
        self.carry = (byte & 0x01) != 0;
    }

    /// The CCR byte.
    pub(crate) fn ccr_byte(&self) -> u8 {
        ((self.extend as u8) << 4)
            | ((self.neg as u8) << 3)
            | ((self.zero as u8) << 2)
            | ((self.over as u8) << 1)
            | self.carry as u8
    }

    /// Store `size` bytes of `value` into a register.
    pub(crate) fn reg_store(&mut self, size: u8, idx: usize, data: u32, address: bool) {
        let data = match size {
            0 => data & 0xFF,
            1 => data & 0xFFFF,
            _ => data,
        };
        if address {
            self.a[idx] = data;
        } else {
            // The data registers merge into the low bytes.
            self.d[idx] = match size {
                0 => self.d[idx] & 0xFFFF_FF00 | data,
                1 => self.d[idx] & 0xFFFF_0000 | data,
                _ => data,
            };
        }
    }

    /// Load `size` bytes from a register value.
    pub(crate) fn reg_load(&self, size: u8, idx: usize, address: bool) -> u32 {
        let src = if address { self.a[idx] } else { self.d[idx] };
        match size {
            0 => src & 0xFF,
            1 => src & 0xFF_FF,
            _ => src,
        }
    }
}

/// The access helper: stamps one bus access at the exact tick and
/// counts the cycles of the running instruction.
pub(crate) struct Access<'a, 'c> {
    pub ctx: &'c mut MachineCtx<'a>,
    pub tick: Tick,
    pub k: u64,
    pub cpu_div: u64,
    pub bus_name: String,
}

impl std::fmt::Debug for Access<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Access").field("k", &self.k).finish()
    }
}

impl<'a, 'c> Access<'a, 'c> {
    /// An access helper for one instruction at its start tick.
    pub fn for_cpu(cpu: &M68000, tick: Tick, ctx: &'c mut MachineCtx<'a>) -> Self {
        Self {
            ctx,
            tick,
            k: 0,
            cpu_div: cpu.div,
            bus_name: cpu.bus_name.clone(),
        }
    }

    /// The stamp of an access after `extra` CPU cycles.
    #[inline]
    pub fn stamp(&self, extra: u64) -> Tick {
        Tick(self.tick.0 + self.cpu_div * (self.k + extra))
    }

    /// Pay `t` CPU cycles.
    #[inline]
    pub fn wait(&mut self, t: u64) {
        self.k += t;
    }

    /// The number of CPU cycles consumed so far.
    #[inline]
    pub fn cycles(&self) -> u64 {
        self.k
    }

    /// A word fetch at the program counter's word address.
    #[inline]
    pub fn fetch_word(&mut self, address: u32) -> u16 {
        let even = address & !1;
        let hi = self.ctx.read(
            &self.bus_name,
            AccessWidth::Byte,
            Address(even),
            self.stamp(0),
        );
        let lo = self.ctx.read(
            &self.bus_name,
            AccessWidth::Byte,
            Address(even + 1),
            self.stamp(0),
        );
        self.wait(4);
        (hi as u16) << 8 | lo as u16
    }

    /// A word read at the (aligned) address.
    #[inline]
    pub fn read_word(&mut self, address: u32) -> u16 {
        self.fetch_word(address)
    }

    /// A longword read: two word reads, the high word first.
    #[inline]
    pub fn read_long(&mut self, address: u32) -> u32 {
        let hi = self.read_word(address) as u32;
        let lo = self.read_word(address.wrapping_add(2)) as u32;
        hi << 16 | lo
    }

    /// A byte read; the odd address reads its own lane.
    #[inline]
    pub fn read_byte(&mut self, address: u32) -> u8 {
        let v = self.ctx.read(
            &self.bus_name,
            AccessWidth::Byte,
            Address(address & 0xFF_FFFF),
            self.stamp(0),
        );
        self.wait(4);
        v as u8
    }

    /// A word write at the (aligned) address.
    #[inline]
    pub fn write_word(&mut self, address: u32, value: u16) {
        let even = address & !1;
        self.ctx.write(
            &self.bus_name,
            AccessWidth::Byte,
            Address(even),
            u32::from(value >> 8),
            self.stamp(0),
        );
        self.ctx.write(
            &self.bus_name,
            AccessWidth::Byte,
            Address(even + 1),
            u32::from(value & 0xFF),
            self.stamp(0),
        );
        self.wait(4);
    }

    /// A longword write: two word writes, the high word first.
    #[inline]
    pub fn write_long(&mut self, address: u32, value: u32) {
        self.write_word(address, (value >> 16) as u16);
        self.write_word(address.wrapping_add(2), value as u16);
    }

    /// A byte write; the odd address writes its own lane.
    #[inline]
    pub fn write_byte(&mut self, address: u32, value: u8) {
        self.ctx.write(
            &self.bus_name,
            AccessWidth::Byte,
            Address(address & 0xFF_FFFF),
            u32::from(value),
            self.stamp(0),
        );
        self.wait(4);
    }

    /// Report a runtime event from the core.
    #[inline]
    pub fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        self.ctx.emit(event);
    }
}

/// The control lines the core answers.
pub const CONTROL_LINES: &[&str] = &["ipl0", "ipl1", "ipl2", "dma_hold"];

/// The line the core drives while a HALT parks it.
pub const DRIVEN_LINES: &[&str] = &["halt"];

/// The device enum of the CPU family.
#[derive(Debug)]
pub enum CpuDevice {
    /// The 68000 core.
    M68k(M68000),
}

impl Device for CpuDevice {
    fn name(&self) -> &str {
        match self {
            CpuDevice::M68k(c) => c.name(),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        match self {
            CpuDevice::M68k(c) => c.as_any(),
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        match self {
            CpuDevice::M68k(c) => c.as_any_mut(),
        }
    }

    fn cpu(&self) -> Option<&dyn CpuDebug> {
        match self {
            CpuDevice::M68k(c) => Some(c),
        }
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn CpuDebug> {
        match self {
            CpuDevice::M68k(c) => Some(c),
        }
    }

    fn reset(&mut self) {
        match self {
            CpuDevice::M68k(c) => c.reset(),
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match self {
            CpuDevice::M68k(c) => c.boot(state),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match self {
            CpuDevice::M68k(c) => c.snapshot(),
        }
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        match self {
            CpuDevice::M68k(c) => c.restore(data),
        }
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match self {
            CpuDevice::M68k(c) => c.notify_line(line, level, tick),
        }
    }

    fn next_event(&self) -> Option<Tick> {
        match self {
            CpuDevice::M68k(c) => c.next_event(),
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        match self {
            CpuDevice::M68k(c) => c.tick(tick, ctx),
        }
    }
}

impl Device for M68000 {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn cpu(&self) -> Option<&dyn CpuDebug> {
        Some(self)
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn CpuDebug> {
        Some(self)
    }

    fn reset(&mut self) {
        self.d = [0; 8];
        self.a = [0; 8];
        self.usp = 0;
        self.ssp = 0;
        self.pc = 0;
        self.trace = false;
        self.sup = false;
        self.i_field = 0;
        self.carry = false;
        self.over = false;
        self.zero = false;
        self.neg = false;
        self.extend = false;
        self.ipl = [OcPin::default(); 3];
        self.held = false;
        self.halted = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Cpu { registers }) = state {
            let names: Vec<String> = registers.keys().map(String::clone).collect();
            for name in names {
                let value = registers[&name];
                let _ = CpuDebug::set_register(self, &name, value);
            }
        }
        // The power-on form: the boot preset in the machine's
        // configuration names the supervisor stack pointer and the
        // program counter directly; the core performs no reset-vector
        // reads of its own.
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        // The pinned form: tag and version (5), the register file
        // (64), USP SSP PC (12), the status word (2), the three IPL
        // pin pull counts (12), the control flags (2), and the next
        // and last ticks (16): 113 bytes.
        let mut out = Vec::with_capacity(113);
        out.extend_from_slice(b"M68K");
        out.push(1);
        for v in self.d {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in self.a {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.usp.to_le_bytes());
        out.extend_from_slice(&self.ssp.to_le_bytes());
        out.extend_from_slice(&self.pc.to_le_bytes());
        out.extend_from_slice(&self.sr_word().to_le_bytes());
        for pin in &self.ipl {
            out.extend_from_slice(&pin.pulls.to_le_bytes());
        }
        out.push(u8::from(self.held));
        out.push(u8::from(self.halted));
        out.extend_from_slice(&self.next.map(|t| t.0).unwrap_or(u64::MAX).to_le_bytes());
        out.extend_from_slice(
            &self
                .last_tick
                .map(|t| t.0)
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        const FORM: usize = 113;
        if data.len() != FORM {
            return Err(SnapshotError::Mismatch(format!(
                "m68000 chunk must be {FORM} bytes, got {}",
                data.len()
            )));
        }
        if &data[..4] != b"M68K" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad m68000 chunk tag".into()));
        }
        // The form is length-checked above, so the reads' slicing is
        // exact.
        let mut at = 5usize;
        for slot in self.d.iter_mut() {
            *slot = u32::from_le_bytes(data[at..at + 4].try_into().expect("form-checked"));
            at += 4;
        }
        for slot in self.a.iter_mut() {
            *slot = u32::from_le_bytes(data[at..at + 4].try_into().expect("form-checked"));
            at += 4;
        }
        self.usp = u32::from_le_bytes(data[at..at + 4].try_into().expect("form-checked"));
        at += 4;
        self.ssp = u32::from_le_bytes(data[at..at + 4].try_into().expect("form-checked"));
        at += 4;
        self.pc = u32::from_le_bytes(data[at..at + 4].try_into().expect("form-checked"));
        at += 4;
        self.set_sr_word(u16::from_le_bytes(
            data[at..at + 2].try_into().expect("form-checked"),
        ));
        at += 2;
        for pin in self.ipl.iter_mut() {
            let pulls = u32::from_le_bytes(data[at..at + 4].try_into().expect("form-checked"));
            if pulls > 15 {
                return Err(SnapshotError::Mismatch(
                    "m68000 chunk carries an impossible IPL pull count".into(),
                ));
            }
            pin.clear();
            for _ in 0..pulls {
                pin.pull();
            }
            at += 4;
        }
        self.held = data[at] != 0;
        self.halted = data[at + 1] != 0;
        at += 2;
        let next = u64::from_le_bytes(data[at..at + 8].try_into().expect("form-checked"));
        self.next = (next != u64::MAX).then_some(Tick(next));
        at += 8;
        let last = u64::from_le_bytes(data[at..at + 8].try_into().expect("form-checked"));
        self.last_tick = (last != u64::MAX).then_some(Tick(last));
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        // The three IPL lines are open-collector and follow the
        // machine's line convention: a driver asserts with a high
        // level and releases with a low one.
        let pin = match line {
            "ipl0" => Some(0usize),
            "ipl1" => Some(1usize),
            "ipl2" => Some(2usize),
            _ => None,
        };
        if let Some(i) = pin {
            if level == Level::High {
                self.ipl[i].pull();
            } else {
                self.ipl[i].release();
            }
            if level == Level::High && !self.held {
                // A new assert wakes the parked core; the scheduler
                // re-seeds the slot this notification marked.
                self.wake(tick);
            }
        }
        if line == "dma_hold" {
            self.held = level == Level::High;
            if self.held {
                self.next = None;
            } else {
                self.wake(tick);
            }
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
        crate::exec::step(self, tick, ctx);
    }
}

impl CpuDebug for M68000 {
    fn registers(&self) -> BTreeMap<String, u64> {
        let mut map = BTreeMap::new();
        for i in 0..8 {
            map.insert(format!("D{i}"), u64::from(self.d[i]));
            map.insert(format!("A{i}"), u64::from(self.a[i]));
        }
        map.insert("PC".to_string(), u64::from(self.pc));
        map.insert("SR".to_string(), u64::from(self.sr_word()));
        map.insert("CC".to_string(), u64::from(self.ccr_byte()));
        map.insert("USP".to_string(), u64::from(self.usp));
        map.insert("SSP".to_string(), u64::from(self.ssp));
        map.insert("ISP".to_string(), u64::from(self.ssp));
        map.insert("SP".to_string(), u64::from(self.sp()));
        map
    }

    fn set_register(&mut self, name: &str, value: u64) -> bool {
        let v32 = value as u32;
        let upper = name.to_ascii_uppercase();
        if upper.len() == 2
            && upper.starts_with('D')
            && matches!(upper.as_bytes().get(1), Some(b'0'..=b'7'))
        {
            let idx = (upper.as_bytes()[1] - b'0') as usize;
            self.d[idx] = v32;
            true
        } else if upper.len() == 2
            && upper.starts_with('A')
            && matches!(upper.as_bytes().get(1), Some(b'0'..=b'7'))
        {
            let idx = (upper.as_bytes()[1] - b'0') as usize;
            self.a[idx] = v32;
            true
        } else {
            match upper.as_str() {
                "PC" => {
                    self.pc = v32;
                    true
                }
                "SR" => {
                    // The debug write sets the whole word; the
                    // supervisor and trace bits follow the value.
                    self.set_sr_word(value as u16);
                    true
                }
                "CC" => {
                    self.set_ccr(value as u8);
                    true
                }
                "USP" => {
                    self.usp = v32;
                    true
                }
                "SSP" | "ISP" => {
                    self.ssp = v32;
                    true
                }
                "SP" => {
                    self.set_sp(v32);
                    true
                }
                _ => false,
            }
        }
    }
}
