//! The instruction set: one instruction per core visit.
//!
//! The access sequence follows the published HuC6280 opcode table (the
//! opcode reference of pce_cpu.html, cross-checked against the MAME
//! h6280 core on the assigned cells). The HuC6280 forms differ from the
//! CMOS 65C02 forms in three ways the table states: every memory
//! instruction costs one cycle more than its 6502 cell and the indexed
//! forms never pay a page-cross or wrong-address cycle, so an indexed
//! read or store keeps the read shape of its own cell; a taken branch
//! costs one more cycle than the CMOS form; and the read-modify-write
//! instructions read twice and write once over the die's own access
//! sequence, as the CMOS forms do. The tests drive the same step
//! function over a plain byte map through the [`crate::cpu::Cycles`]
//! trait, so the asserted cycle totals cover the published table cells
//! and the asserted reads cover the address forms.
//!
//! The die's routing sits in the [`crate::cpu::AccessRule`] kinds the
//! access helpers apply: the zero-page and stack accesses route
//! through MPR 1, the ST0, ST1, and ST2 strobes route through MPR 0,
//! the reset vector reads through bank 0, and every other access
//! selects the bank of its own 8K logical page. Accesses whose bank
//! byte is 0xFF land on the die: the timer registers at the I/O
//! offsets 0x0C00, 0x0C01, and 0x0C07, and the interrupt controller at
//! 0x1400..0x1403; the cycle is die-internal and no bus read or write
//! then runs.
//!
//! ### The unassigned table cells
//!
//! The opcode matrix of the die leaves these cells unassigned:
//! `$0B, $1B, $2B, $33, $3B, $4B, $5B, $5C, $63, $7B, $8B, $9B, $AB,
//! $BB, $CB, $DB, $DC, $E2, $EB, $FB, $FC`. Every one runs as an
//! operand consuming NOP in the addressing shape the CMOS cells share:
//! the immediate two-byte shape for `$0B, $2B, $4B, $6B, $8B, $AB, $CB,
//! $EB, $E2`, the absolute,X shape for `$5C, $DC, $FC, $9B, $BB`, the
//! indirect,Y shape for `$1B, $3B, $5B, $7B`, and the absolute,Y shape
//! for the rest. A stray byte costs its addressing work and nothing
//! else.

use demu_core::Address;
use demu_core::frequency::Tick;
use demu_core::triggers::TriggerEvent;

use crate::cpu::{
    Access, AccessRule, Cycles, FLAG_B, FLAG_C, FLAG_D, FLAG_I, FLAG_N, FLAG_T, FLAG_V, FLAG_Z,
    Huc6280, InterruptSource, VEC_RESET,
};

/// One addressing mode: the access pattern of the operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The immediate operand.
    Imm,
    /// Zero page.
    Zp,
    /// Zero page with the X index.
    ZpX,
    /// Zero page with the Y index.
    ZpY,
    /// Absolute.
    Abs,
    /// Absolute with the X index.
    AbsX,
    /// Absolute with the Y index.
    AbsY,
    /// Indirect through a zero-page pointer with the X index.
    IndX,
    /// Indirect through a zero-page pointer with the Y index.
    IndY,
    /// Indirect through a zero-page pointer alone.
    Ind,
}

/// The addressing mode of one opcode of the ALU class `cc = 01` and of
/// the die's zero-page indirect forms inside the class `cc = 10`,
/// mode slot four.
fn alu_mode(opcode: u8) -> Mode {
    match (opcode >> 2) & 0x07 {
        0 => Mode::IndX,
        1 => Mode::Zp,
        2 => Mode::Imm,
        3 => Mode::Abs,
        4 => Mode::IndY,
        5 => Mode::ZpX,
        6 => Mode::AbsY,
        _ => Mode::AbsX,
    }
}

/// The read-modify-write mode of one opcode of the class `cc = 10`.
/// Only the mode fields one, three, five, and seven occur there.
fn rmw_mode(op: u8) -> Mode {
    match (op >> 2) & 0x07 {
        1 => Mode::Zp,
        5 => Mode::ZpX,
        3 => Mode::Abs,
        7 => Mode::AbsX,
        // The class carries no other shift or increment opcode.
        _ => Mode::Zp,
    }
}

/// The ALU index of one opcode: the accumulator instruction and its
/// addressing mode, for the class `cc = 01` and for the die's
/// zero-page indirect forms inside the class `cc = 10`, mode slot
/// four. The class pattern does not cover the immediate bit test at
/// `$89`.
fn op_index(opcode: u8) -> Option<(u8, Mode)> {
    if opcode & 0x03 == 0x01 && opcode != 0x89 {
        Some((opcode >> 5, alu_mode(opcode)))
    } else if opcode & 0x03 == 0x02 && (opcode >> 2) & 0x07 == 4 {
        Some((opcode >> 5, Mode::Ind))
    } else {
        None
    }
}

/// Step one instruction from `tick`: an interrupt service first when
/// one is pending, else the instruction at the program counter.
///
/// The core reschedules itself to the end of the instruction.
pub fn step(cpu: &mut Huc6280, tick: Tick, ctx: &mut demu_core::device::MachineCtx) {
    let mut bus = Access::new(ctx, tick, cpu.bus_name.clone(), cpu.cpu_div);
    step_with(cpu, tick, &mut bus);
}

/// One instruction over an arbitrary [`Cycles`] bus.
pub fn step_with<C: Cycles>(cpu: &mut Huc6280, tick: Tick, bus: &mut C) {
    // The instruction ran under the divider it started with; a CSH or
    // a CSL instruction changes the divider for the next instruction.
    let div = cpu.cpu_div;
    if cpu.reset_pending {
        cpu.reset_pending = false;
        reset_sequence(cpu, bus);
        reschedule(cpu, tick, div, bus.cycles());
        return;
    }
    if let Some(source) = cpu.take_interrupt() {
        service_interrupt(cpu, bus, tick, source, false);
        reschedule(cpu, tick, div, bus.cycles());
        return;
    }
    let start = cpu.regs.pc;
    let opcode = bus_rd(cpu, bus, start, AccessRule::Generic);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(u32::from(start))));
    execute(cpu, bus, tick, opcode);
    reschedule(cpu, tick, div, bus.cycles());
}

/// Reschedule the core `m` used cycles after `tick`, under the divider
/// the instruction ran with.
fn reschedule(cpu: &mut Huc6280, tick: Tick, div: u64, cycles: u32) {
    cpu.next = Some(Tick(tick.0 + div * u64::from(cycles)));
}

/// Read one byte through the die's routing of one logical access.
///
/// An access whose routed bank is the die's own hardware page is a
/// die-internal cycle: the die answers the register windows and
/// everything else routes through the bus. The die's accesses consume
/// their cycle and keep the machine bus out of it.
fn bus_rd<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, addr: u16, rule: AccessRule) -> u8 {
    let (phys, on_die) = cpu.route(addr, rule);
    let die_value = if on_die {
        cpu.on_die_read(addr & 0x1FFF)
    } else {
        None
    };
    match die_value {
        Some(value) => {
            bus.wait();
            value
        }
        None => bus.rd(phys),
    }
}

/// Write one byte through the die's routing of one logical access.
///
/// The access tick re-arms the timer's decrement base when the enable
/// write restarts the counter.
fn bus_wr<C: Cycles>(
    cpu: &mut Huc6280,
    tick: Tick,
    bus: &mut C,
    addr: u16,
    rule: AccessRule,
    value: u8,
) {
    let (phys, on_die) = cpu.route(addr, rule);
    let took = if on_die {
        let at = Tick(tick.0 + cpu.cpu_div * u64::from(bus.cycles()));
        cpu.on_die_write(addr & 0x1FFF, value, at)
    } else {
        false
    };
    if took {
        bus.wait();
    } else {
        bus.wr(phys, value);
    }
}

/// The hardware interrupt service: two idle cycles, three pushes, and
/// the vector read. The break flag states the software-interrupt form.
/// The die clears the decimal flag and sets the interrupt disable at
/// the entry, and the break flag holds its set form outside of an
/// interrupt.
fn service_interrupt<C: Cycles>(
    cpu: &mut Huc6280,
    bus: &mut C,
    tick: Tick,
    source: InterruptSource,
    software: bool,
) {
    if !software {
        bus.wait();
        bus_rd(cpu, bus, cpu.regs.pc, AccessRule::Generic);
    }
    let pc = cpu.regs.pc;
    push(cpu, bus, tick, (pc >> 8) as u8);
    push(cpu, bus, tick, (pc & 0xFF) as u8);
    push(cpu, bus, tick, cpu.push_p(software));
    let vector = crate::cpu::interrupt_vector(source);
    let vec_lo = u16::from(bus_rd(cpu, bus, vector, AccessRule::Generic));
    let vec_hi = u16::from(bus_rd(
        cpu,
        bus,
        vector.wrapping_add(1),
        AccessRule::Generic,
    ));
    cpu.regs.p |= FLAG_I;
    cpu.regs.p &= !FLAG_D;
    cpu.regs.p |= FLAG_B;
    cpu.regs.pc = (vec_hi << 8) | vec_lo;
}

/// Write the stack byte at the current pointer and decrement it. The
/// die routes the stack through MPR 1.
fn push<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, tick: Tick, value: u8) {
    let s = cpu.regs.s;
    bus_wr(
        cpu,
        tick,
        bus,
        0x0100 | u16::from(s),
        AccessRule::Stack,
        value,
    );
    cpu.regs.s = s.wrapping_sub(1);
}

/// Read the stack byte the pointer names and advance the pointer.
fn pop<C: Cycles>(cpu: &mut Huc6280, bus: &mut C) -> u8 {
    let s = cpu.regs.s.wrapping_add(1);
    cpu.regs.s = s;
    bus_rd(cpu, bus, 0x0100 | u16::from(s), AccessRule::Stack)
}

/// The power-on reset sequence: idle cycles, the three dummy pushes
/// that decrement the stack pointer, and the vector read. The reset
/// vector is always read from bank 0 no matter which bank MPR 7 holds,
/// which is why the PC Engine resets out of ROM bank zero.
fn reset_sequence<C: Cycles>(cpu: &mut Huc6280, bus: &mut C) {
    for _ in 0..4 {
        bus_rd(cpu, bus, cpu.regs.pc, AccessRule::Generic);
    }
    bus.wait();
    cpu.regs.s = cpu.regs.s.wrapping_sub(3);
    let vec_lo = u16::from(bus_rd(cpu, bus, VEC_RESET, AccessRule::Bank0));
    let vec_hi = u16::from(bus_rd(cpu, bus, VEC_RESET + 1, AccessRule::Bank0));
    cpu.regs.pc = (vec_hi << 8) | vec_lo;
}

/// The operand-byte fetch at the program counter.
fn immediate<C: Cycles>(cpu: &mut Huc6280, bus: &mut C) -> u8 {
    let value = bus_rd(cpu, bus, cpu.regs.pc, AccessRule::Generic);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    value
}

/// The address of one operand, with the exact access sequence of the
/// mode, and the routing rule its data access takes. The zero-page
/// forms carry the die's MPR 1 rule; the die pays the idle cycle of
/// each read form before the data access, and the index addition of
/// the indexed forms is that same idle cycle.
fn operand_addr<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, mode: Mode) -> (u16, AccessRule) {
    match mode {
        Mode::Imm => (0, AccessRule::Generic),
        Mode::Zp => {
            let z = immediate(cpu, bus);
            bus.wait();
            (u16::from(z), AccessRule::Zp)
        }
        Mode::ZpX | Mode::ZpY => {
            let z = immediate(cpu, bus);
            // The index-add cycle reads the base cell through the die's
            // zero-page window.
            let _ = bus_rd(cpu, bus, u16::from(z), AccessRule::Zp);
            let index = match mode {
                Mode::ZpX => cpu.regs.x,
                _ => cpu.regs.y,
            };
            (
                u16::from(z).wrapping_add(u16::from(index)) & 0xFF,
                AccessRule::Zp,
            )
        }
        Mode::Abs => {
            let lo = immediate(cpu, bus);
            let hi = immediate(cpu, bus);
            bus.wait();
            (u16::from_le_bytes([lo, hi]), AccessRule::Generic)
        }
        Mode::AbsX | Mode::AbsY => {
            let lo = immediate(cpu, bus);
            let hi = immediate(cpu, bus);
            // The index-add cycle.
            bus.wait();
            let base = u16::from_le_bytes([lo, hi]);
            let index = match mode {
                Mode::AbsX => cpu.regs.x,
                _ => cpu.regs.y,
            };
            (base.wrapping_add(u16::from(index)), AccessRule::Generic)
        }
        Mode::IndX => {
            let z = immediate(cpu, bus);
            // The index add walks the pointer inside the zero-page
            // window, and the walk wraps inside the 8K bank window.
            let ptr = u16::from(z).wrapping_add(u16::from(cpu.regs.x)) & 0xFF;
            let lo = bus_rd(cpu, bus, ptr, AccessRule::Zp);
            let hi = bus_rd(cpu, bus, (ptr + 1) & 0x1FFF, AccessRule::Zp);
            bus.wait();
            bus.wait();
            ((u16::from(hi) << 8) | u16::from(lo), AccessRule::Generic)
        }
        Mode::IndY => {
            let z = immediate(cpu, bus);
            let lo = bus_rd(cpu, bus, u16::from(z), AccessRule::Zp);
            let hi = bus_rd(cpu, bus, (u16::from(z) + 1) & 0x1FFF, AccessRule::Zp);
            // The Y-index add cycle and the extra idle cycle of the
            // die's read form.
            bus.wait();
            bus.wait();
            let vec = (u16::from(hi) << 8) | u16::from(lo);
            (vec.wrapping_add(u16::from(cpu.regs.y)), AccessRule::Generic)
        }
        Mode::Ind => {
            let z = immediate(cpu, bus);
            // The zero-page pointer walk wraps inside the 8K bank
            // window.
            let lo = bus_rd(cpu, bus, u16::from(z), AccessRule::Zp);
            let hi = bus_rd(cpu, bus, (u16::from(z) + 1) & 0x1FFF, AccessRule::Zp);
            bus.wait();
            bus.wait();
            ((u16::from(hi) << 8) | u16::from(lo), AccessRule::Generic)
        }
    }
}

/// Read one operand through the mode's access pattern.
fn read_m<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, mode: Mode) -> u8 {
    if mode == Mode::Imm {
        return immediate(cpu, bus);
    }
    let (addr, rule) = operand_addr(cpu, bus, mode);
    bus_rd(cpu, bus, addr, rule)
}

/// Write one operand through the mode's access pattern; the die's
/// store forms keep the exact read shape of their cells.
fn store_m<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, tick: Tick, mode: Mode, value: u8) {
    let (addr, rule) = operand_addr(cpu, bus, mode);
    bus_wr(cpu, tick, bus, addr, rule, value);
}

/// Read one operand twice, mutate it, and write the result once, as
/// the die's read-modify-write sequence does.
fn rmw_m<C: Cycles>(
    cpu: &mut Huc6280,
    bus: &mut C,
    tick: Tick,
    mode: Mode,
    apply: impl Fn(&mut Huc6280, u8) -> u8,
) {
    let (addr, rule) = operand_addr(cpu, bus, mode);
    let value = bus_rd(cpu, bus, addr, rule);
    let _ = bus_rd(cpu, bus, addr, rule);
    let result = apply(cpu, value);
    bus_wr(cpu, tick, bus, addr, rule, result);
}

/// The add with carry, binary and binary-coded decimal, in the form
/// the MAME h6280 core documents: zero and negative follow the result,
/// and the decimal form never updates the overflow flag.
fn adc_result(cpu: &mut Huc6280, left: u8, m: u8) -> u8 {
    if cpu.regs.p & FLAG_D != 0 {
        let c = u16::from(cpu.regs.p & FLAG_C != 0);
        let mut lo = u16::from(left & 0x0F) + u16::from(m & 0x0F) + c;
        let mut hi = u16::from(left & 0xF0) + u16::from(m & 0xF0);
        cpu.regs.p &= !FLAG_C;
        if lo > 0x09 {
            hi += 0x10;
            lo += 0x06;
        }
        if hi > 0x90 {
            hi += 0x60;
        }
        if hi & 0xFF00 != 0 {
            cpu.regs.p |= FLAG_C;
        }
        let result = ((lo & 0x0F) | (hi & 0xF0)) as u8;
        set_nz(cpu, result);
        return result;
    }
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let a = u16::from(left);
    let sum = a + u16::from(m) + c;
    cpu.regs.p &= !(FLAG_V | FLAG_C);
    if ((a ^ u16::from(m)) & (a ^ (sum & 0xFF)) & 0x80) == 0x80 {
        cpu.regs.p |= FLAG_V;
    }
    if sum & 0xFF00 != 0 {
        cpu.regs.p |= FLAG_C;
    }
    let result = sum as u8;
    set_nz(cpu, result);
    result
}

/// The subtract with borrow, binary and binary-coded decimal, in the
/// form the MAME h6280 core documents: zero and negative follow the
/// result, and the decimal form never updates the overflow flag.
fn sbc_result(cpu: &mut Huc6280, left: u8, m: u8) -> u8 {
    let c = u16::from(cpu.regs.p & FLAG_C == 0);
    let a = u16::from(left);
    let sum = a.wrapping_sub(u16::from(m)).wrapping_sub(c);
    if cpu.regs.p & FLAG_D != 0 {
        // The decimal form does not update the overflow flag.
        cpu.regs.p &= !FLAG_C;
        let mut lo = (a & 0x0F).wrapping_sub((u16::from(m) & 0x0F) + c);
        let mut hi = (a & 0xF0).wrapping_sub(u16::from(m) & 0xF0);
        if lo & 0xF0 != 0 {
            lo = lo.wrapping_sub(6);
        }
        if lo & 0x80 != 0 {
            hi = hi.wrapping_sub(0x10);
        }
        if hi & 0x0F00 != 0 {
            hi = hi.wrapping_sub(0x60);
        }
        if sum & 0xFF00 == 0 {
            cpu.regs.p |= FLAG_C;
        }
        let result = ((lo & 0x0F) | (hi & 0xF0)) as u8;
        set_nz(cpu, result);
        return result;
    }
    cpu.regs.p &= !(FLAG_V | FLAG_C);
    if ((a ^ u16::from(m)) & (a ^ (sum & 0xFF)) & 0x80) == 0x80 {
        cpu.regs.p |= FLAG_V;
    }
    if sum & 0xFF00 == 0 {
        cpu.regs.p |= FLAG_C;
    }
    let result = sum as u8;
    set_nz(cpu, result);
    result
}

/// The zero-page cell the T mode operates on, read once and mutated.
///
/// The T mode of the five accumulator forms runs the plain
/// instruction's operand reads, then the operation's zero-page cell
/// that X names, with the result storing back to the cell and the T
/// flag clearing with the operation; the accumulator keeps its value.
/// The operational form costs the plain cell 3 cycles, and the decimal
/// arithmetic one more.
fn t_apply<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, tick: Tick, op: u8, operand: u8) {
    let addr = u16::from(cpu.regs.x);
    let cell = bus_rd(cpu, bus, addr, AccessRule::Zp);
    bus.wait();
    cpu.regs.p &= !FLAG_T;
    let result = match op {
        0 => {
            let r = cell | operand;
            set_nz(cpu, r);
            r
        }
        1 => {
            let r = cell & operand;
            set_nz(cpu, r);
            r
        }
        2 => {
            let r = cell ^ operand;
            set_nz(cpu, r);
            r
        }
        3 => {
            let r = adc_result(cpu, cell, operand);
            if cpu.regs.p & FLAG_D != 0 {
                bus.wait();
            }
            r
        }
        _ => {
            let r = sbc_result(cpu, cell, operand);
            if cpu.regs.p & FLAG_D != 0 {
                bus.wait();
            }
            r
        }
    };
    bus_wr(cpu, tick, bus, addr, AccessRule::Zp, result);
}

/// The eight ALU forms over one addressing mode: ORA, AND, EOR, ADC,
/// STA, LDA, CMP, and SBC. The five accumulator forms carry the die's
/// T-mode form; the decimal arithmetic of ADC and SBC costs one more
/// cycle.
fn execute_alu<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, tick: Tick, op: u8, mode: Mode) {
    match op {
        0 => {
            let m = read_m(cpu, bus, mode);
            if cpu.regs.p & FLAG_T != 0 {
                t_apply(cpu, bus, tick, op, m);
            } else {
                let a = cpu.regs.a | m;
                set_nz(cpu, a);
                cpu.regs.a = a;
            }
        }
        1 => {
            let m = read_m(cpu, bus, mode);
            if cpu.regs.p & FLAG_T != 0 {
                t_apply(cpu, bus, tick, op, m);
            } else {
                let a = cpu.regs.a & m;
                set_nz(cpu, a);
                cpu.regs.a = a;
            }
        }
        2 => {
            let m = read_m(cpu, bus, mode);
            if cpu.regs.p & FLAG_T != 0 {
                t_apply(cpu, bus, tick, op, m);
            } else {
                let a = cpu.regs.a ^ m;
                set_nz(cpu, a);
                cpu.regs.a = a;
            }
        }
        3 => {
            let m = read_m(cpu, bus, mode);
            if cpu.regs.p & FLAG_T != 0 {
                t_apply(cpu, bus, tick, op, m);
            } else if cpu.regs.p & FLAG_D != 0 {
                bus.wait();
                cpu.regs.a = adc_result(cpu, cpu.regs.a, m);
            } else {
                cpu.regs.a = adc_result(cpu, cpu.regs.a, m);
            }
        }
        4 => {
            let (addr, rule) = operand_addr(cpu, bus, mode);
            bus_wr(cpu, tick, bus, addr, rule, cpu.regs.a);
        }
        5 => {
            let m = read_m(cpu, bus, mode);
            set_nz(cpu, m);
            cpu.regs.a = m;
        }
        6 => {
            let m = read_m(cpu, bus, mode);
            compare(cpu, cpu.regs.a, m);
        }
        _ => {
            let m = read_m(cpu, bus, mode);
            if cpu.regs.p & FLAG_T != 0 {
                t_apply(cpu, bus, tick, op, m);
            } else if cpu.regs.p & FLAG_D != 0 {
                bus.wait();
                cpu.regs.a = sbc_result(cpu, cpu.regs.a, m);
            } else {
                cpu.regs.a = sbc_result(cpu, cpu.regs.a, m);
            }
        }
    }
}

/// One executed instruction. The cycle count accumulates on the access
/// helpers; every arm reads or writes in instruction order.
fn execute<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, tick: Tick, opcode: u8) {
    // The die decodes every instruction with the T flag clear: an
    // instruction that does not carry the T-mode form leaves the flag
    // cleared, and the forms that participate clear it with their
    // operation. The SET instruction and the stack restores of the
    // status byte keep the flag.
    if !matches!(op_index(opcode), Some((0 | 1 | 2 | 3 | 7, _))) {
        cpu.regs.p &= !FLAG_T;
    }
    // The page-bit group: BBR_n branches when zero-page bit n is clear,
    // BBS_n when it is set. The form costs six cycles not taken and
    // eight taken, the cells the MAME h6280 core states, and the
    // branch never pays a page-cross cycle.
    if opcode & 0x0F == 0x0F {
        let bit = u32::from((opcode >> 4) & 0x07);
        let set = opcode & 0x80 != 0;
        let z = immediate(cpu, bus);
        let value = bus_rd(cpu, bus, u16::from(z), AccessRule::Zp);
        let offset = immediate(cpu, bus);
        let opaque_bit = 1u8 << bit;
        let taken = if set {
            value & opaque_bit != 0
        } else {
            value & opaque_bit == 0
        };
        bus.wait();
        bus.wait();
        if taken {
            bus.wait();
            bus.wait();
            cpu.regs.pc = cpu.regs.pc.wrapping_add_signed(i16::from(offset as i8));
        }
        return;
    }
    // The RMB and SMB group: the zero-page read-modify-write store of
    // one bit, with two reads and one store, and no flag changes.
    if opcode & 0x0F == 0x07 {
        let bit = 1u8 << u32::from((opcode >> 4) & 0x07);
        let set = opcode & 0x80 != 0;
        let (addr, rule) = operand_addr(cpu, bus, Mode::Zp);
        let value = bus_rd(cpu, bus, addr, rule);
        let _ = bus_rd(cpu, bus, addr, rule);
        bus.wait();
        let masked = if set { value | bit } else { value & !bit };
        bus_wr(cpu, tick, bus, addr, rule, masked);
        return;
    }
    // The ALU class and the die's zero-page indirect forms.
    if let Some((op, mode)) = op_index(opcode) {
        execute_alu(cpu, bus, tick, op, mode);
        return;
    }
    match opcode {
        // LDX.
        0xA2 => {
            let m = immediate(cpu, bus);
            set_nz(cpu, m);
            cpu.regs.x = m;
        }
        0xA6 => {
            let m = read_m(cpu, bus, Mode::Zp);
            set_nz(cpu, m);
            cpu.regs.x = m;
        }
        0xB6 => {
            let m = read_m(cpu, bus, Mode::ZpY);
            set_nz(cpu, m);
            cpu.regs.x = m;
        }
        0xAE => {
            let m = read_m(cpu, bus, Mode::Abs);
            set_nz(cpu, m);
            cpu.regs.x = m;
        }
        0xBE => {
            let m = read_m(cpu, bus, Mode::AbsY);
            set_nz(cpu, m);
            cpu.regs.x = m;
        }
        // STX.
        0x86 => store_m(cpu, bus, tick, Mode::Zp, cpu.regs.x),
        0x96 => store_m(cpu, bus, tick, Mode::ZpY, cpu.regs.x),
        0x8E => store_m(cpu, bus, tick, Mode::Abs, cpu.regs.x),
        // LDY.
        0xA0 => {
            let m = immediate(cpu, bus);
            set_nz(cpu, m);
            cpu.regs.y = m;
        }
        0xA4 => {
            let m = read_m(cpu, bus, Mode::Zp);
            set_nz(cpu, m);
            cpu.regs.y = m;
        }
        0xB4 => {
            let m = read_m(cpu, bus, Mode::ZpX);
            set_nz(cpu, m);
            cpu.regs.y = m;
        }
        0xAC => {
            let m = read_m(cpu, bus, Mode::Abs);
            set_nz(cpu, m);
            cpu.regs.y = m;
        }
        0xBC => {
            let m = read_m(cpu, bus, Mode::AbsX);
            set_nz(cpu, m);
            cpu.regs.y = m;
        }
        // STY.
        0x84 => store_m(cpu, bus, tick, Mode::Zp, cpu.regs.y),
        0x94 => store_m(cpu, bus, tick, Mode::ZpX, cpu.regs.y),
        0x8C => store_m(cpu, bus, tick, Mode::Abs, cpu.regs.y),
        // CPX and CPY.
        0xE0 => {
            let m = immediate(cpu, bus);
            compare(cpu, cpu.regs.x, m);
        }
        0xE4 => {
            let m = read_m(cpu, bus, Mode::Zp);
            compare(cpu, cpu.regs.x, m);
        }
        0xEC => {
            let m = read_m(cpu, bus, Mode::Abs);
            compare(cpu, cpu.regs.x, m);
        }
        0xC0 => {
            let m = immediate(cpu, bus);
            compare(cpu, cpu.regs.y, m);
        }
        0xC4 => {
            let m = read_m(cpu, bus, Mode::Zp);
            compare(cpu, cpu.regs.y, m);
        }
        0xCC => {
            let m = read_m(cpu, bus, Mode::Abs);
            compare(cpu, cpu.regs.y, m);
        }
        // BIT: N and V come from the operand in every form, and the
        // immediate form carries the zero flag only.
        0x24 => {
            let m = read_m(cpu, bus, Mode::Zp);
            bit(cpu, m, true);
        }
        0x2C => {
            let m = read_m(cpu, bus, Mode::Abs);
            bit(cpu, m, true);
        }
        0x34 => {
            let m = read_m(cpu, bus, Mode::ZpX);
            bit(cpu, m, true);
        }
        0x3C => {
            let m = read_m(cpu, bus, Mode::AbsX);
            bit(cpu, m, true);
        }
        0x89 => {
            let m = immediate(cpu, bus);
            bit(cpu, m, false);
        }
        // Accumulator shifts.
        0x0A | 0x2A | 0x4A | 0x6A => {
            bus.wait();
            let a = cpu.regs.a;
            cpu.regs.a = match opcode {
                0x0A => asl(cpu, a),
                0x2A => rol(cpu, a),
                0x4A => lsr(cpu, a),
                _ => ror(cpu, a),
            };
        }
        // Memory shifts: the die's read-modify-write sequence reads
        // twice and writes once.
        0x06 | 0x16 | 0x0E | 0x1E => rmw_m(cpu, bus, tick, rmw_mode(opcode), asl),
        0x26 | 0x36 | 0x2E | 0x3E => rmw_m(cpu, bus, tick, rmw_mode(opcode), rol),
        0x46 | 0x56 | 0x4E | 0x5E => rmw_m(cpu, bus, tick, rmw_mode(opcode), lsr),
        0x66 | 0x76 | 0x6E | 0x7E => rmw_m(cpu, bus, tick, rmw_mode(opcode), ror),
        // Memory increments and decrements.
        0xE6 | 0xF6 | 0xEE | 0xFE => rmw_m(cpu, bus, tick, rmw_mode(opcode), |c, v| {
            let r = v.wrapping_add(1);
            set_nz(c, r);
            r
        }),
        0xC6 | 0xD6 | 0xCE | 0xDE => rmw_m(cpu, bus, tick, rmw_mode(opcode), |c, v| {
            let r = v.wrapping_sub(1);
            set_nz(c, r);
            r
        }),
        // TSB and TRB, the read-modify-write bit test forms. The N, V,
        // and Z flags carry the test, as the die's BIT forms do.
        0x04 => tsb_trb(cpu, bus, tick, Mode::Zp, true),
        0x0C => tsb_trb(cpu, bus, tick, Mode::Abs, true),
        0x14 => tsb_trb(cpu, bus, tick, Mode::Zp, false),
        0x1C => tsb_trb(cpu, bus, tick, Mode::Abs, false),
        // Store zero.
        0x64 => store_m(cpu, bus, tick, Mode::Zp, 0),
        0x74 => store_m(cpu, bus, tick, Mode::ZpX, 0),
        0x9C => store_m(cpu, bus, tick, Mode::Abs, 0),
        0x9E => store_m(cpu, bus, tick, Mode::AbsX, 0),
        // Transfers.
        0xAA => {
            bus.wait();
            let x = cpu.regs.x;
            set_nz(cpu, x);
            cpu.regs.a = x;
        }
        0x8A => {
            bus.wait();
            let a = cpu.regs.a;
            set_nz(cpu, a);
            cpu.regs.x = a;
        }
        0xA8 => {
            bus.wait();
            let a = cpu.regs.a;
            set_nz(cpu, a);
            cpu.regs.y = a;
        }
        0x98 => {
            bus.wait();
            let y = cpu.regs.y;
            set_nz(cpu, y);
            cpu.regs.a = y;
        }
        0xBA => {
            bus.wait();
            let s = cpu.regs.s;
            set_nz(cpu, s);
            cpu.regs.x = s;
        }
        0x9A => {
            bus.wait();
            cpu.regs.s = cpu.regs.x;
        }
        // The register swaps, 3 cycles each.
        0x22 => {
            bus.wait();
            bus.wait();
            std::mem::swap(&mut cpu.regs.a, &mut cpu.regs.x);
        }
        0x42 => {
            bus.wait();
            bus.wait();
            std::mem::swap(&mut cpu.regs.a, &mut cpu.regs.y);
        }
        0x02 => {
            bus.wait();
            bus.wait();
            std::mem::swap(&mut cpu.regs.x, &mut cpu.regs.y);
        }
        // The register clears, 2 cycles each.
        0x62 => {
            bus.wait();
            cpu.regs.a = 0;
        }
        0x82 => {
            bus.wait();
            cpu.regs.x = 0;
        }
        0xC2 => {
            bus.wait();
            cpu.regs.y = 0;
        }
        // Register increments and decrements, implied.
        0xE8 => {
            bus.wait();
            let x = cpu.regs.x.wrapping_add(1);
            set_nz(cpu, x);
            cpu.regs.x = x;
        }
        0xC8 => {
            bus.wait();
            let y = cpu.regs.y.wrapping_add(1);
            set_nz(cpu, y);
            cpu.regs.y = y;
        }
        0xCA => {
            bus.wait();
            let x = cpu.regs.x.wrapping_sub(1);
            set_nz(cpu, x);
            cpu.regs.x = x;
        }
        0x88 => {
            bus.wait();
            let y = cpu.regs.y.wrapping_sub(1);
            set_nz(cpu, y);
            cpu.regs.y = y;
        }
        0x1A => {
            bus.wait();
            let a = cpu.regs.a.wrapping_add(1);
            set_nz(cpu, a);
            cpu.regs.a = a;
        }
        0x3A => {
            bus.wait();
            let a = cpu.regs.a.wrapping_sub(1);
            set_nz(cpu, a);
            cpu.regs.a = a;
        }
        // Stack operations. The pull forms spend two idle cycles.
        0x48 => {
            bus.wait();
            push(cpu, bus, tick, cpu.regs.a);
        }
        0x08 => {
            bus.wait();
            push(cpu, bus, tick, cpu.push_p(true));
        }
        0xDA => {
            bus.wait();
            push(cpu, bus, tick, cpu.regs.x);
        }
        0x5A => {
            bus.wait();
            push(cpu, bus, tick, cpu.regs.y);
        }
        0x68 => {
            bus.wait();
            bus.wait();
            let value = pop(cpu, bus);
            set_nz(cpu, value);
            cpu.regs.a = value;
        }
        0x28 => {
            bus.wait();
            bus.wait();
            // The restore keeps the break flag set outside of an
            // interrupt, the die's B form.
            cpu.regs.p = pop(cpu, bus) | FLAG_B;
        }
        0xFA => {
            bus.wait();
            bus.wait();
            let value = pop(cpu, bus);
            set_nz(cpu, value);
            cpu.regs.x = value;
        }
        0x7A => {
            bus.wait();
            bus.wait();
            let value = pop(cpu, bus);
            set_nz(cpu, value);
            cpu.regs.y = value;
        }
        // Jumps and subroutine calls.
        0x4C => {
            let lo = immediate(cpu, bus);
            let hi = immediate(cpu, bus);
            bus.wait();
            cpu.regs.pc = (u16::from(hi) << 8) | u16::from(lo);
        }
        0x6C => {
            let lo = immediate(cpu, bus);
            let hi = immediate(cpu, bus);
            // The indirect pointer read does not wrap in its page.
            let ptr = (u16::from(hi) << 8) | u16::from(lo);
            bus.wait();
            let target_lo = bus_rd(cpu, bus, ptr, AccessRule::Generic);
            let target_hi = bus_rd(cpu, bus, ptr.wrapping_add(1), AccessRule::Generic);
            bus.wait();
            cpu.regs.pc = (u16::from(target_hi) << 8) | u16::from(target_lo);
        }
        0x7C => {
            let lo = immediate(cpu, bus);
            let hi = immediate(cpu, bus);
            let ptr = ((u16::from(hi) << 8) | u16::from(lo)).wrapping_add(u16::from(cpu.regs.x));
            // The index-add cycle of the die's jump form.
            bus.wait();
            let target_lo = bus_rd(cpu, bus, ptr, AccessRule::Generic);
            let target_hi = bus_rd(cpu, bus, ptr.wrapping_add(1), AccessRule::Generic);
            bus.wait();
            cpu.regs.pc = (u16::from(target_hi) << 8) | u16::from(target_lo);
        }
        0x20 => {
            let lo = immediate(cpu, bus);
            // The second cycle reads the stack without use.
            let _ = bus_rd(cpu, bus, 0x0100 | u16::from(cpu.regs.s), AccessRule::Stack);
            // The return address names the third byte of the
            // instruction, which the high operand read replaces.
            let return_pc = cpu.regs.pc;
            push(cpu, bus, tick, (return_pc >> 8) as u8);
            push(cpu, bus, tick, (return_pc & 0xFF) as u8);
            let hi = immediate(cpu, bus);
            bus.wait();
            cpu.regs.pc = (u16::from(hi) << 8) | u16::from(lo);
        }
        0x60 => {
            // The throwaway address reads at the return position, the
            // stack reads, the post-pull dummy, and the die's extra
            // idle cycle.
            bus_rd(cpu, bus, cpu.regs.pc, AccessRule::Generic);
            bus_rd(cpu, bus, 0x0100 | u16::from(cpu.regs.s), AccessRule::Stack);
            let lo = u16::from(pop(cpu, bus));
            let hi = u16::from(pop(cpu, bus));
            cpu.regs.pc = (hi << 8) | lo;
            bus_rd(cpu, bus, cpu.regs.pc, AccessRule::Generic);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            bus.wait();
        }
        0x40 => {
            // The throwaway address read, the stack read, and the
            // three pulls.
            bus_rd(cpu, bus, cpu.regs.pc, AccessRule::Generic);
            bus.wait();
            bus_rd(cpu, bus, 0x0100 | u16::from(cpu.regs.s), AccessRule::Stack);
            cpu.regs.p = pop(cpu, bus) | FLAG_B;
            let lo = u16::from(pop(cpu, bus));
            let hi = u16::from(pop(cpu, bus));
            cpu.regs.pc = (hi << 8) | lo;
        }
        0x00 => {
            // BRK: the break instruction clears the T flag before its
            // pushes, the padding byte advances the program counter
            // once more, and the pushes carry the break flag set.
            bus_rd(cpu, bus, cpu.regs.pc, AccessRule::Generic);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            bus.wait();
            cpu.regs.p &= !FLAG_T;
            service_interrupt(cpu, bus, tick, InterruptSource::Irq2, true);
        }
        // BSR: the two pushes of the branch-to-subroutine form, with
        // the branch cycles of the taken form around them.
        0x44 => {
            let offset = immediate(cpu, bus);
            bus.wait();
            bus.wait();
            push(cpu, bus, tick, (cpu.regs.pc >> 8) as u8);
            push(cpu, bus, tick, (cpu.regs.pc & 0xFF) as u8);
            bus.wait();
            bus.wait();
            cpu.regs.pc = cpu.regs.pc.wrapping_add_signed(i16::from(offset as i8));
        }
        // Branches: the die's taken branch costs two idle cycles and
        // never a page-cross cycle. BRA always branches.
        0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 => {
            let taken = branch_taken(cpu, opcode);
            let offset = immediate(cpu, bus);
            if taken {
                bus.wait();
                bus.wait();
                cpu.regs.pc = cpu.regs.pc.wrapping_add_signed(i16::from(offset as i8));
            }
        }
        0x80 => {
            let offset = immediate(cpu, bus);
            bus.wait();
            bus.wait();
            cpu.regs.pc = cpu.regs.pc.wrapping_add_signed(i16::from(offset as i8));
        }
        // ST0, ST1, and ST2: the immediate stores the die routes to
        // the three fixed hardware-page ports through MPR 0.
        0x03 => {
            let value = immediate(cpu, bus);
            bus.wait();
            bus_wr(cpu, tick, bus, 0x0000, AccessRule::Mpr0, value);
        }
        0x13 => {
            let value = immediate(cpu, bus);
            bus.wait();
            bus_wr(cpu, tick, bus, 0x0002, AccessRule::Mpr0, value);
        }
        0x23 => {
            let value = immediate(cpu, bus);
            bus.wait();
            bus_wr(cpu, tick, bus, 0x0003, AccessRule::Mpr0, value);
        }
        // TAM: each set bit of the immediate loads its MPR from the
        // accumulator. The translation does not run for it.
        0x53 => {
            let mask = immediate(cpu, bus);
            for i in 0..8 {
                if mask & (1 << i) != 0 {
                    cpu.mprs[i] = cpu.regs.a;
                }
            }
            bus.wait();
            bus.wait();
            bus.wait();
        }
        // TMA: the accumulator loads the MPR of the highest set bit of
        // the immediate, the selection the MAME core implements for
        // the die's one-bit forms.
        0x43 => {
            let mask = immediate(cpu, bus);
            for i in 0..8 {
                if mask & (1 << i) != 0 {
                    cpu.regs.a = cpu.mprs[i];
                }
            }
            bus.wait();
            bus.wait();
        }
        // The TST forms: the test of an immediate against a memory
        // byte, with N and V from the memory byte and Z from the test.
        0x83 => {
            let mask = immediate(cpu, bus);
            let (addr, rule) = operand_addr(cpu, bus, Mode::Zp);
            let m = bus_rd(cpu, bus, addr, rule);
            bus.wait();
            bus.wait();
            tst(cpu, mask, m);
        }
        0xA3 => {
            let mask = immediate(cpu, bus);
            let (addr, rule) = operand_addr(cpu, bus, Mode::ZpX);
            let m = bus_rd(cpu, bus, addr, rule);
            bus.wait();
            bus.wait();
            tst(cpu, mask, m);
        }
        0x93 => {
            let mask = immediate(cpu, bus);
            let (addr, rule) = operand_addr(cpu, bus, Mode::Abs);
            let m = bus_rd(cpu, bus, addr, rule);
            bus.wait();
            bus.wait();
            tst(cpu, mask, m);
        }
        0xB3 => {
            let mask = immediate(cpu, bus);
            let (addr, rule) = operand_addr(cpu, bus, Mode::AbsX);
            let m = bus_rd(cpu, bus, addr, rule);
            bus.wait();
            bus.wait();
            tst(cpu, mask, m);
        }
        // The five block moves: the two address operands and the 16-bit
        // length, 6 cycles per byte with the set-up cycles of the
        // published cell. The length of zero moves 65536 bytes.
        0x73 => block_move(cpu, bus, tick, Direction::Inc, Direction::Inc),
        0xC3 => block_move(cpu, bus, tick, Direction::Dec, Direction::Dec),
        0xD3 => block_move(cpu, bus, tick, Direction::Inc, Direction::None),
        0xE3 => block_move(cpu, bus, tick, Direction::Inc, Direction::Alt),
        0xF3 => block_move(cpu, bus, tick, Direction::Alt, Direction::Inc),
        // The speed forms: CSH returns the core to the configured
        // high-speed divider and CSL runs the low-speed form at four
        // phases per cycle against the high-speed three.
        0xD4 => {
            bus.wait();
            bus.wait();
            cpu.cpu_div = cpu.base_div;
        }
        0x54 => {
            bus.wait();
            bus.wait();
            cpu.cpu_div = cpu.base_div * 4;
        }
        // The T-flag instruction: SET leaves the flag set for the
        // following arithmetic operation.
        0xF4 => {
            bus.wait();
            cpu.regs.p |= FLAG_T;
        }
        // Flag operations.
        0x18 | 0x38 | 0x58 | 0x78 | 0xB8 | 0xD8 | 0xF8 => {
            bus.wait();
            match opcode {
                0x18 => cpu.regs.p &= !FLAG_C,
                0x38 => cpu.regs.p |= FLAG_C,
                0x58 => cpu.regs.p &= !FLAG_I,
                0x78 => cpu.regs.p |= FLAG_I,
                0xB8 => cpu.regs.p &= !FLAG_V,
                0xD8 => cpu.regs.p &= !FLAG_D,
                _ => cpu.regs.p |= FLAG_D,
            }
        }
        0xEA => bus.wait(),
        // No documented opcode remains; every other byte routes
        // through the NOP forms below. A stray byte costs its
        // addressing work and no machine effect, which keeps the run
        // robust.
        _ => execute_nop_form(cpu, bus, opcode),
    }
}

/// The TSB and TRB read-modify-write forms: the store of the
/// accumulator's bits, with N and V from the memory byte and Z from
/// the test of the accumulator against it.
fn tsb_trb<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, tick: Tick, mode: Mode, set: bool) {
    let (addr, rule) = operand_addr(cpu, bus, mode);
    let v = bus_rd(cpu, bus, addr, rule);
    let _ = bus_rd(cpu, bus, addr, rule);
    bit_flags(cpu, v);
    let result = if set { v | cpu.regs.a } else { v & !cpu.regs.a };
    bus_wr(cpu, tick, bus, addr, rule, result);
}

/// The N and V flags from the memory byte and the Z flag from the
/// accumulator's test, the die's BIT form.
fn bit_flags(cpu: &mut Huc6280, m: u8) {
    cpu.regs.p &= !(FLAG_N | FLAG_V | FLAG_Z);
    cpu.regs.p |= if m & FLAG_N != 0 { FLAG_N } else { 0 };
    cpu.regs.p |= if m & FLAG_V != 0 { FLAG_V } else { 0 };
    if cpu.regs.a & m == 0 {
        cpu.regs.p |= FLAG_Z;
    }
}

/// The die's bit test: N and V from the memory byte, Z from the
/// accumulator against the memory byte; the immediate form carries the
/// zero flag only.
fn bit(cpu: &mut Huc6280, m: u8, memory: bool) {
    cpu.regs.p &= !FLAG_Z;
    if cpu.regs.a & m == 0 {
        cpu.regs.p |= FLAG_Z;
    }
    if memory {
        bit_flags(cpu, m);
    }
}

/// The die's masked bit test: N and V from the memory byte, Z from the
/// immediate against the memory byte, with no accumulator effect.
fn tst(cpu: &mut Huc6280, imm: u8, m: u8) {
    cpu.regs.p &= !(FLAG_N | FLAG_V | FLAG_Z);
    cpu.regs.p |= if m & FLAG_N != 0 { FLAG_N } else { 0 };
    cpu.regs.p |= if m & FLAG_V != 0 { FLAG_V } else { 0 };
    if m & imm == 0 {
        cpu.regs.p |= FLAG_Z;
    }
}

/// The block-move direction of the die's transfer instructions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// The address increments every byte.
    Inc,
    /// The address decrements every byte.
    Dec,
    /// The address alternates between the base and the base plus one.
    Alt,
    /// The address stays fixed.
    None,
}

/// The five block transfers: TII moves increment to increment, TDD
/// moves decrement to decrement, TIN moves increment with a fixed
/// destination, TIA moves increment with an alternating destination,
/// and TAI moves with an alternating source and an incrementing
/// destination. The die's forms move 17 set-up cycles and 6 cycles per
/// byte, with a length of zero moving 65536 bytes.
fn block_move<C: Cycles>(
    cpu: &mut Huc6280,
    bus: &mut C,
    tick: Tick,
    src_step: Direction,
    dst_step: Direction,
) {
    let src_lo = immediate(cpu, bus);
    let src_hi = immediate(cpu, bus);
    let dst_lo = immediate(cpu, bus);
    let dst_hi = immediate(cpu, bus);
    let len_lo = immediate(cpu, bus);
    let len_hi = immediate(cpu, bus);
    let length = if len_hi == 0 && len_lo == 0 {
        0x10000
    } else {
        u32::from_le_bytes([len_lo, len_hi, 0, 0])
    };
    let mut src = (u16::from(src_hi) << 8) | u16::from(src_lo);
    let mut dst = (u16::from(dst_hi) << 8) | u16::from(dst_lo);
    let mut alternate = 0;
    // The die's set-up cycles ahead of the per-byte window.
    for _ in 0..10 {
        bus.wait();
    }
    for _ in 0..length {
        bus.wait();
        let from = match src_step {
            Direction::Alt => src.wrapping_add(alternate),
            _ => src,
        };
        let value = bus_rd(cpu, bus, from, AccessRule::Generic);
        bus.wait();
        bus.wait();
        let to = match dst_step {
            Direction::Alt => dst.wrapping_add(alternate),
            _ => dst,
        };
        bus_wr(cpu, tick, bus, to, AccessRule::Generic, value);
        bus.wait();
        match src_step {
            Direction::Inc => src = src.wrapping_add(1),
            Direction::Dec => src = src.wrapping_sub(1),
            _ => {}
        }
        match dst_step {
            Direction::Inc => dst = dst.wrapping_add(1),
            Direction::Dec => dst = dst.wrapping_sub(1),
            _ => {}
        }
        alternate ^= 1;
    }
}

/// The compare: subtract without store, with the carry set when the
/// left side does not borrow.
fn compare(cpu: &mut Huc6280, left: u8, right: u8) {
    let cmp = left.wrapping_sub(right);
    set_nz(cpu, cmp);
    set_flag(cpu, FLAG_C, left >= right);
}

fn set_nz(cpu: &mut Huc6280, value: u8) {
    set_flag(cpu, FLAG_Z, value == 0);
    set_flag(cpu, FLAG_N, value & FLAG_N != 0);
}

fn set_flag(cpu: &mut Huc6280, flag: u8, set: bool) {
    if set {
        cpu.regs.p |= flag;
    } else {
        cpu.regs.p &= !flag;
    }
}

fn asl(cpu: &mut Huc6280, value: u8) -> u8 {
    set_flag(cpu, FLAG_C, value & 0x80 != 0);
    let result = value << 1;
    set_nz(cpu, result);
    result
}

fn lsr(cpu: &mut Huc6280, value: u8) -> u8 {
    set_flag(cpu, FLAG_C, value & 1 != 0);
    let result = value >> 1;
    set_nz(cpu, result);
    result
}

fn rol(cpu: &mut Huc6280, value: u8) -> u8 {
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let result = (value << 1) | c as u8;
    set_flag(cpu, FLAG_C, value & 0x80 != 0);
    set_nz(cpu, result);
    result
}

fn ror(cpu: &mut Huc6280, value: u8) -> u8 {
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let result = (value >> 1) | ((c as u8) << 7);
    set_flag(cpu, FLAG_C, value & 1 != 0);
    set_nz(cpu, result);
    result
}

/// True when the branch condition holds.
fn branch_taken(cpu: &Huc6280, opcode: u8) -> bool {
    let p = cpu.regs.p;
    match opcode {
        0x10 => p & FLAG_N == 0,
        0x30 => p & FLAG_N != 0,
        0x50 => p & FLAG_V == 0,
        0x70 => p & FLAG_V != 0,
        0x90 => p & FLAG_C == 0,
        0xB0 => p & FLAG_C != 0,
        0xD0 => p & FLAG_Z == 0,
        0xF0 => p & FLAG_Z != 0,
        _ => false,
    }
}

/// The operand-consuming NOP forms without machine effect, the shape
/// every unassigned table cell of the die takes.
fn execute_nop_form<C: Cycles>(cpu: &mut Huc6280, bus: &mut C, opcode: u8) {
    match opcode {
        // The immediate two-byte forms consume their byte.
        0x0B | 0x2B | 0x4B | 0x6B | 0x8B | 0xAB | 0xCB | 0xEB | 0xE2 => {
            let _ = immediate(cpu, bus);
        }
        // The absolute,X forms read in their shape.
        0xDC | 0xFC | 0x9B | 0xBB | 0x5C => {
            let _ = read_m(cpu, bus, Mode::AbsX);
        }
        // The indirect,Y forms read in their shape.
        0x1B | 0x3B | 0x5B | 0x7B => {
            let _ = read_m(cpu, bus, Mode::IndY);
        }
        // The absolute,Y forms read in their shape.
        _ => {
            let _ = read_m(cpu, bus, Mode::AbsY);
        }
    }
}
