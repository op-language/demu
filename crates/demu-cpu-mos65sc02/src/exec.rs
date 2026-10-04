//! The instruction set: one instruction per core visit.
//!
//! The access sequence follows the published CMOS 65C02 (Rockwell
//! R65C02) as the Lynx runs it: read-modify-write instructions read
//! the effective address twice and write the result once, taken
//! branches cost one extra cycle and never a page cross, and the
//! immediate bit test sets only the zero flag. The bit-test branch
//! instructions run five cycles with one more when taken. The decimal
//! forms of ADC and SBC are the CMOS forms that the Handy core
//! documents for the Lynx, with the flag behavior from the same
//! source. The page-bit instructions SMB, RMB, BBS, and BBR run in the
//! plain kind; the Lynx core treats them as two-cycle NOPs. Every
//! other byte runs as an operand-consuming NOP, so a bad byte costs
//! its addressing work and nothing else.
//!
//! The tests drive the same step function over a plain byte map through
//! the [`crate::cpu::Cycles`] trait, so the asserted cycle totals cover
//! the published table cells and the asserted reads cover the address
//! forms.

use demu_core::Address;
use demu_core::frequency::Tick;
use demu_core::triggers::TriggerEvent;

use crate::cpu::Mos65Sc02;
use crate::cpu::{
    Access, Cycles, FLAG_B, FLAG_C, FLAG_D, FLAG_I, FLAG_N, FLAG_UNUSED, FLAG_V, FLAG_Z,
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
    /// Absolute with the X index in the store shape that always pays
    /// the wrong-address dummy read.
    AbsXStore,
    /// Absolute with the Y index, in the same shape.
    AbsYStore,
    /// Indirect through a zero-page pointer with the X index.
    IndX,
    /// Indirect through a zero-page pointer with the Y index.
    IndY,
    /// Indirect with the Y index in the same shape.
    IndYStore,
    /// Indirect through the zero-page pointer alone.
    Ind,
}

/// The addressing mode of one opcode of the ALU class `cc = 01`.
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

/// The store-shaped partner of one read mode: the shape that always
/// pays the wrong-address dummy read.
fn store_of(mode: Mode) -> Mode {
    match mode {
        Mode::AbsX => Mode::AbsXStore,
        Mode::AbsY => Mode::AbsYStore,
        Mode::IndY => Mode::IndYStore,
        other => other,
    }
}

/// The read-modify-write mode of one opcode of the class `cc = 10`.
/// Only the mode fields one, three, five, and seven occur there.
fn rmw_mode(op: u8) -> Mode {
    match (op >> 2) & 0x07 {
        1 => Mode::Zp,
        5 => Mode::ZpX,
        3 => Mode::Abs,
        7 => Mode::AbsXStore,
        // The class carries no other shift or increment opcode.
        _ => Mode::Zp,
    }
}

/// Step one instruction from `tick`: an interrupt service first when
/// one is pending, else the instruction at the program counter.
///
/// The core reschedules itself to the end of the instruction. A core
/// parked with STP returns only through a reset; a core held by the
/// sleep line idles.
pub fn step(cpu: &mut Mos65Sc02, tick: Tick, ctx: &mut demu_core::device::MachineCtx) {
    if cpu.map_unit {
        cpu.sync_map(tick, ctx);
    }
    let mut bus = Access::new(ctx, tick, cpu.bus_name.clone(), cpu.cpu_div);
    step_with(cpu, tick, &mut bus);
}

/// One instruction over an arbitrary [`Cycles`] bus.
pub fn step_with<C: Cycles>(cpu: &mut Mos65Sc02, tick: Tick, bus: &mut C) {
    if cpu.reset_pending {
        reset_sequence(cpu, bus);
        cpu.reset_pending = false;
        let cycles = bus.cycles();
        cpu.schedule_after(tick, cycles);
        return;
    }
    if cpu.stopped {
        cpu.next = None;
        return;
    }
    if cpu.interrupt_pending() {
        let nmi = cpu.take_nmi();
        service_interrupt(cpu, bus, nmi, false);
        let cycles = bus.cycles();
        cpu.schedule_after(tick, cycles);
        return;
    }
    let start = cpu.regs.pc;
    let opcode = bus.rd(start);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(u32::from(start))));
    execute(cpu, bus, opcode);
    let cycles = bus.cycles();
    // The WAI and STP arms park the core; the outer schedule pauses
    // while a park state holds.
    if !cpu.stopped && !cpu.waiting {
        cpu.schedule_after(tick, cycles);
    }
}

/// The hardware interrupt service: two idle cycles, three pushes, and
/// the vector read. The break flag states the software-interrupt form.
fn service_interrupt<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, nmi: bool, software: bool) {
    if !software {
        bus.wait();
        bus.rd(cpu.regs.pc);
    }
    let pc = cpu.regs.pc;
    push(cpu, bus, (pc >> 8) as u8);
    push(cpu, bus, (pc & 0xFF) as u8);
    push(cpu, bus, cpu.push_p(software));
    let (low, high) = if nmi {
        (0xFFFA, 0xFFFB)
    } else {
        (0xFFFE, 0xFFFF)
    };
    let vec_lo = u16::from(bus.rd(low));
    let vec_hi = u16::from(bus.rd(high));
    cpu.regs.p |= FLAG_I;
    cpu.regs.p &= !FLAG_B;
    // The CMOS interrupt entry leaves the decimal flag unchanged.
    cpu.regs.pc = (vec_hi << 8) | vec_lo;
}

/// Write the stack byte at the current pointer and decrement it.
fn push<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, value: u8) {
    let s = cpu.regs.s;
    bus.wr(0x0100 | u16::from(s), value);
    cpu.regs.s = s.wrapping_sub(1);
}

/// Read the stack byte the pointer names and advance the pointer.
fn pop<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C) -> u8 {
    let s = cpu.regs.s.wrapping_add(1);
    cpu.regs.s = s;
    bus.rd(0x0100 | u16::from(s))
}

/// The power-on reset sequence: idle cycles, the three dummy pushes
/// that decrement the stack pointer, and the vector read.
fn reset_sequence<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C) {
    for _ in 0..4 {
        bus.rd(cpu.regs.pc);
    }
    bus.wait();
    cpu.regs.s = cpu.regs.s.wrapping_sub(3);
    let vec_lo = u16::from(bus.rd(0xFFFC));
    let vec_hi = u16::from(bus.rd(0xFFFD));
    cpu.regs.pc = (vec_hi << 8) | vec_lo;
    cpu.irq_line = false;
    cpu.nmi_line = false;
    cpu.nmi_edge = false;
    cpu.waiting = false;
    cpu.stopped = false;
}

/// The address of one operand, with the exact access sequence of the
/// mode. Indexed loads read the wrong address only on a page cross;
/// index-shaped stores always pay the wrong-address read.
fn operand_addr<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, mode: Mode) -> u16 {
    match mode {
        // The immediate form reads through the helper above.
        Mode::Imm => 0,
        Mode::Zp => {
            let z = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            u16::from(z)
        }
        Mode::ZpX | Mode::ZpY => {
            let z = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            bus.rd(u16::from(z));
            let index = match mode {
                Mode::ZpX => cpu.regs.x,
                _ => cpu.regs.y,
            };
            (u16::from(z) + u16::from(index)) & 0xFF
        }
        Mode::Abs | Mode::AbsX | Mode::AbsY | Mode::AbsXStore | Mode::AbsYStore => {
            let lo = u16::from(bus.rd(cpu.regs.pc));
            let hi = u16::from(bus.rd(cpu.regs.pc.wrapping_add(1)));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            let base = (hi << 8) | lo;
            let (index, store_shaped) = match mode {
                Mode::Abs => (0, false),
                Mode::AbsX => (cpu.regs.x, false),
                Mode::AbsY => (cpu.regs.y, false),
                Mode::AbsXStore | Mode::AbsYStore => (
                    if mode == Mode::AbsXStore {
                        cpu.regs.x
                    } else {
                        cpu.regs.y
                    },
                    true,
                ),
                _ => (0, false),
            };
            let low_sum = lo + u16::from(index);
            let page_crossed = low_sum > 0xFF;
            let broken = (base & 0xFF00) | (low_sum & 0xFF);
            if page_crossed || store_shaped {
                bus.rd(broken);
                return base + u16::from(index);
            }
            broken
        }
        Mode::IndX => {
            let z = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            let ptr = (u16::from(z) + u16::from(cpu.regs.x)) & 0xFF;
            let lo = bus.rd(ptr);
            let hi = bus.rd((ptr + 1) & 0xFF);
            (u16::from(hi) << 8) | u16::from(lo)
        }
        Mode::IndY | Mode::IndYStore => {
            let z = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            let vec_lo = u16::from(bus.rd(u16::from(z)));
            let vec_hi = u16::from(bus.rd((u16::from(z) + 1) & 0xFF));
            let vec = (vec_hi << 8) | vec_lo;
            let low_sum = vec_lo + u16::from(cpu.regs.y);
            let page_crossed = low_sum > 0xFF;
            let broken = (vec & 0xFF00) | (low_sum & 0xFF);
            if page_crossed || matches!(mode, Mode::IndYStore) {
                bus.rd(broken);
                return vec + u16::from(cpu.regs.y);
            }
            broken
        }
        Mode::Ind => {
            let z = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            // The CMOS indirect read does not wrap in its page.
            let lo = u16::from(bus.rd(u16::from(z)));
            let hi = u16::from(bus.rd(u16::from(z).wrapping_add(1)));
            (hi << 8) | lo
        }
    }
}

/// Read one operand through the mode's access pattern.
fn read_m<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, mode: Mode) -> u8 {
    if mode == Mode::Imm {
        return immediate(cpu, bus);
    }
    let addr = operand_addr(cpu, bus, mode);
    bus.rd(addr)
}

/// Write one operand through the store shape of its mode.
fn store_m<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, mode: Mode, value: u8) {
    let addr = operand_addr(cpu, bus, store_of(mode));
    bus.wr(addr, value);
}

/// Read one operand twice, mutate it, and write the result once, as
/// the CMOS read-modify-write sequence does.
fn rmw_m<C: Cycles>(
    cpu: &mut Mos65Sc02,
    bus: &mut C,
    mode: Mode,
    apply: impl Fn(&mut Mos65Sc02, u8) -> u8,
) {
    let addr = operand_addr(cpu, bus, mode);
    let value = bus.rd(addr);
    bus.rd(addr);
    let result = apply(cpu, value);
    bus.wr(addr, result);
}

/// The eight ALU forms over one addressing mode: ORA, AND, EOR, ADC,
/// STA, LDA, CMP, and SBC.
fn execute_alu<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, op: u8, mode: Mode) {
    match op {
        0 => {
            let m = read_m(cpu, bus, mode);
            let a = cpu.regs.a | m;
            set_nz(cpu, a);
            cpu.regs.a = a;
        }
        1 => {
            let m = read_m(cpu, bus, mode);
            let a = cpu.regs.a & m;
            set_nz(cpu, a);
            cpu.regs.a = a;
        }
        2 => {
            let m = read_m(cpu, bus, mode);
            let a = cpu.regs.a ^ m;
            set_nz(cpu, a);
            cpu.regs.a = a;
        }
        3 => {
            let m = read_m(cpu, bus, mode);
            adc(cpu, m);
        }
        4 => {
            let addr = operand_addr(cpu, bus, store_of(mode));
            bus.wr(addr, cpu.regs.a);
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
            sbc(cpu, m);
        }
    }
}

/// One executed instruction. The cycle count accumulates on the access
/// helper; every arm reads or writes in instruction order.
fn execute<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, opcode: u8) {
    // The page-bit group: the Rockwell encodings put RMB_n and SMB_n
    // at `0x07 + 0x10*n` and `0x87 + 0x10*n`, BBR_n at `0x0F + 0x10*n`
    // and BBS_n at `0x8F + 0x10*n`.
    if opcode & 0x0F == 0x0F {
        if cpu.bbr_smb {
            execute_page_bit(cpu, bus, opcode);
        } else {
            // The Lynx core omits the bit-test instructions; the byte
            // runs as a two-cycle NOP.
            bus.wait();
        }
        return;
    }
    if opcode & 0x0F == 0x07 {
        if cpu.bbr_smb {
            execute_page_store(cpu, bus, opcode);
        } else {
            let _ = read_m(cpu, bus, Mode::Zp);
        }
        return;
    }
    // The ALU class `cc = 01`: ORA, AND, EOR, ADC, STA, LDA, CMP, and
    // SBC in their eight addressing modes. The immediate cell of the
    // STA row carries the CMOS immediate bit test instead, so that
    // byte stays out of the class.
    if opcode & 0x03 == 0x01 && opcode != 0x89 {
        let mode = alu_mode(opcode);
        execute_alu(cpu, bus, opcode >> 5, mode);
        return;
    }
    // The zero-page indirect address form the CMOS adds inside the
    // class `cc = 10`, mode slot four.
    if opcode & 0x03 == 0x02 && (opcode >> 2) & 0x07 == 4 {
        execute_alu(cpu, bus, opcode >> 5, Mode::Ind);
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
        0x86 => store_m(cpu, bus, Mode::Zp, cpu.regs.x),
        0x96 => store_m(cpu, bus, Mode::ZpY, cpu.regs.x),
        0x8E => store_m(cpu, bus, Mode::Abs, cpu.regs.x),
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
        0x84 => store_m(cpu, bus, Mode::Zp, cpu.regs.y),
        0x94 => store_m(cpu, bus, Mode::ZpX, cpu.regs.y),
        0x8C => store_m(cpu, bus, Mode::Abs, cpu.regs.y),
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
        // BIT: the immediate form sets only the zero flag, the form
        // the Handy core documents for the Lynx.
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
        // Memory shifts: the CMOS sequence reads twice and writes once.
        0x06 | 0x16 | 0x0E | 0x1E => rmw_m(cpu, bus, rmw_mode(opcode), asl),
        0x26 | 0x36 | 0x2E | 0x3E => rmw_m(cpu, bus, rmw_mode(opcode), rol),
        0x46 | 0x56 | 0x4E | 0x5E => rmw_m(cpu, bus, rmw_mode(opcode), lsr),
        0x66 | 0x76 | 0x6E | 0x7E => rmw_m(cpu, bus, rmw_mode(opcode), ror),
        // Memory increments and decrements.
        0xE6 | 0xF6 | 0xEE | 0xFE => rmw_m(cpu, bus, rmw_mode(opcode), |c, v| {
            let r = v.wrapping_add(1);
            set_nz(c, r);
            r
        }),
        0xC6 | 0xD6 | 0xCE | 0xDE => rmw_m(cpu, bus, rmw_mode(opcode), |c, v| {
            let r = v.wrapping_sub(1);
            set_nz(c, r);
            r
        }),
        // Test and set or reset bits, the CMOS read-modify-write
        // forms. The zero flag tests the old value.
        0x04 => {
            let addr = operand_addr(cpu, bus, Mode::Zp);
            let v = bus.rd(addr);
            bus.rd(addr);
            set_zero(cpu, cpu.regs.a & v == 0);
            bus.wr(addr, v | cpu.regs.a);
        }
        0x0C => {
            let addr = operand_addr(cpu, bus, Mode::Abs);
            let v = bus.rd(addr);
            bus.rd(addr);
            set_zero(cpu, cpu.regs.a & v == 0);
            bus.wr(addr, v | cpu.regs.a);
        }
        0x14 => {
            let addr = operand_addr(cpu, bus, Mode::Zp);
            let v = bus.rd(addr);
            bus.rd(addr);
            set_zero(cpu, cpu.regs.a & v == 0);
            bus.wr(addr, v & !cpu.regs.a);
        }
        0x1C => {
            let addr = operand_addr(cpu, bus, Mode::Abs);
            let v = bus.rd(addr);
            bus.rd(addr);
            set_zero(cpu, cpu.regs.a & v == 0);
            bus.wr(addr, v & !cpu.regs.a);
        }
        // Store zero.
        0x64 => store_m(cpu, bus, Mode::Zp, 0),
        0x74 => store_m(cpu, bus, Mode::ZpX, 0),
        0x9C => store_m(cpu, bus, Mode::Abs, 0),
        0x9E => store_m(cpu, bus, Mode::AbsX, 0),
        // Transfers.
        0xAA => {
            bus.wait();
            let a = cpu.regs.a;
            set_nz(cpu, a);
            cpu.regs.x = a;
        }
        0x8A => {
            bus.wait();
            let x = cpu.regs.x;
            set_nz(cpu, x);
            cpu.regs.a = x;
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
        // Stack operations. The pull forms spend two idle cycles as
        // the NMOS forms do.
        0x48 => {
            bus.wait();
            push(cpu, bus, cpu.regs.a);
        }
        0x08 => {
            bus.wait();
            push(cpu, bus, cpu.push_p(true));
        }
        0xDA => {
            bus.wait();
            push(cpu, bus, cpu.regs.x);
        }
        0x5A => {
            bus.wait();
            push(cpu, bus, cpu.regs.y);
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
            cpu.regs.p = pop(cpu, bus) | FLAG_UNUSED;
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
            let lo = u16::from(bus.rd(cpu.regs.pc));
            let hi = u16::from(bus.rd(cpu.regs.pc.wrapping_add(1)));
            cpu.regs.pc = (hi << 8) | lo;
        }
        0x6C => {
            let lo = u16::from(bus.rd(cpu.regs.pc));
            let hi = u16::from(bus.rd(cpu.regs.pc.wrapping_add(1)));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            // The CMOS indirect read does not wrap in its page.
            let ptr = (hi << 8) | lo;
            let target_lo = u16::from(bus.rd(ptr));
            let target_hi = u16::from(bus.rd(ptr.wrapping_add(1)));
            cpu.regs.pc = (target_hi << 8) | target_lo;
        }
        0x7C => {
            let lo = u16::from(bus.rd(cpu.regs.pc));
            let hi = u16::from(bus.rd(cpu.regs.pc.wrapping_add(1)));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            let ptr = ((hi << 8) | lo).wrapping_add(u16::from(cpu.regs.x));
            bus.wait();
            let target_lo = u16::from(bus.rd(ptr));
            let target_hi = u16::from(bus.rd(ptr.wrapping_add(1)));
            cpu.regs.pc = (target_hi << 8) | target_lo;
        }
        0x20 => {
            let lo = u16::from(bus.rd(cpu.regs.pc));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            // The second cycle reads the stack without use.
            bus.rd(0x0100 | u16::from(cpu.regs.s));
            // The return address names the third byte of the
            // instruction, which the high operand read replaces.
            let return_pc = cpu.regs.pc;
            push(cpu, bus, (return_pc >> 8) as u8);
            push(cpu, bus, (return_pc & 0xFF) as u8);
            let hi = u16::from(bus.rd(cpu.regs.pc));
            cpu.regs.pc = (hi << 8) | lo;
        }
        0x60 => {
            // Six cycles: the throwaway address reads at the return
            // position, the stack reads, and the post-pull dummy.
            bus.rd(cpu.regs.pc);
            bus.rd(0x0100 | u16::from(cpu.regs.s));
            let lo = u16::from(pop(cpu, bus));
            let hi = u16::from(pop(cpu, bus));
            cpu.regs.pc = (hi << 8) | lo;
            bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
        }
        0x40 => {
            // Six cycles: the throwaway address read, the stack
            // read, and the three pulls.
            bus.rd(cpu.regs.pc);
            bus.rd(0x0100 | u16::from(cpu.regs.s));
            cpu.regs.p = pop(cpu, bus) | FLAG_UNUSED;
            let lo = u16::from(pop(cpu, bus));
            let hi = u16::from(pop(cpu, bus));
            cpu.regs.pc = (hi << 8) | lo;
        }
        0x00 => {
            // BRK: the software form pushes the break flag, and the
            // next byte advances the program counter once more.
            bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            service_interrupt(cpu, bus, false, true);
        }
        // Branches: the CMOS branch costs one extra cycle when taken
        // and never a page cross. BRA always branches.
        0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 | 0x80 => {
            let taken = opcode == 0x80 || branch_taken(cpu, opcode);
            let offset = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            if taken {
                bus.wait();
                cpu.regs.pc = cpu.regs.pc.wrapping_add_signed(i16::from(offset as i8));
            }
        }
        // WAI and STP park the core. The parked WAI wakes on the next
        // interrupt edge; only the reset state releases STP.
        0xCB => {
            bus.wait();
            cpu.waiting = true;
            cpu.next = None;
        }
        0xDB => {
            bus.wait();
            cpu.stopped = true;
            cpu.next = None;
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

/// The page-bit group: BBR_n branches when zero-page bit `n` is clear,
/// BBS_n when it is set. The pointer byte names the zero page and the
/// branch offset follows it. The published table costs five cycles,
/// with one more when the branch is taken; the branch never pays a
/// page-cross cycle.
fn execute_page_bit<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, opcode: u8) {
    let bit = u32::from((opcode >> 4) & 0x07);
    let set = opcode & 0x80 != 0;
    let z = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    let value = bus.rd(u16::from(z));
    let offset = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    let opaque_bit = 1u8 << bit;
    let taken = if set {
        value & opaque_bit != 0
    } else {
        value & opaque_bit == 0
    };
    bus.wait();
    if taken {
        bus.wait();
        cpu.regs.pc = cpu.regs.pc.wrapping_add_signed(i16::from(offset as i8));
    }
}

/// The RMB and SMB group: the zero-page RMW store of one bit. The
/// five-cycle form reads the pointer, reads the byte, and writes the
/// bit back; no flag changes.
fn execute_page_store<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, opcode: u8) {
    let bit = 1u8 << u32::from((opcode >> 4) & 0x07);
    let set = opcode & 0x80 != 0;
    let addr = operand_addr(cpu, bus, Mode::Zp);
    let value = bus.rd(addr);
    bus.wait();
    let masked = if set { value | bit } else { value & !bit };
    bus.wr(addr, masked);
}

/// The immediate operand read.
fn immediate<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C) -> u8 {
    let value = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    value
}

/// The bit test: zero from the accumulator against the operand; the
/// memory forms also carry negative and overflow from the operand.
fn bit(cpu: &mut Mos65Sc02, m: u8, memory: bool) {
    set_zero(cpu, cpu.regs.a & m == 0);
    if memory {
        set_flag(cpu, FLAG_N, m & FLAG_N != 0);
        set_flag(cpu, FLAG_V, m & FLAG_V != 0);
    }
}

/// The add with carry, binary and binary-coded decimal, in the CMOS
/// form the Handy core documents for the Lynx. Zero and negative
/// follow the final accumulator value.
fn adc(cpu: &mut Mos65Sc02, m: u8) {
    if cpu.regs.p & FLAG_D == 0 {
        let c = u16::from(cpu.regs.p & FLAG_C != 0);
        let a = u16::from(cpu.regs.a);
        let sum = a + u16::from(m) + c;
        cpu.regs.p &= !(FLAG_V | FLAG_C);
        if ((a ^ (sum & 0xFF)) & (u16::from(m) ^ (sum & 0xFF)) & 0x80) == 0x80 {
            cpu.regs.p |= FLAG_V;
        }
        if sum & 0xFF00 != 0 {
            cpu.regs.p |= FLAG_C;
        }
        cpu.regs.a = sum as u8;
        set_nz(cpu, cpu.regs.a);
        return;
    }
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let a = cpu.regs.a;
    let mut lo = u16::from(a & 0x0F) + u16::from(m & 0x0F) + c;
    let mut hi = u16::from(a & 0xF0) + u16::from(m & 0xF0);
    cpu.regs.p &= !(FLAG_V | FLAG_C);
    if lo > 0x09 {
        hi += 0x10;
        lo += 0x06;
    }
    // The decimal overflow follows the high nibble: the sign change
    // between A, the operand, and the high-nibble result.
    if ((a ^ m) & (a ^ (hi as u8)) & 0x80) == 0x80 {
        cpu.regs.p |= FLAG_V;
    }
    if hi > 0x90 {
        hi += 0x60;
    }
    if hi & 0xFF00 != 0 {
        cpu.regs.p |= FLAG_C;
    }
    cpu.regs.a = ((lo & 0x0F) | (hi & 0xF0)) as u8;
    set_nz(cpu, cpu.regs.a);
}

/// The subtract with borrow, binary and binary-coded decimal, in the
/// CMOS form the Handy core documents for the Lynx.
fn sbc(cpu: &mut Mos65Sc02, m: u8) {
    let borrow = u16::from(cpu.regs.p & FLAG_C == 0);
    let a = u16::from(cpu.regs.a);
    let sum = a.wrapping_sub(u16::from(m)).wrapping_sub(borrow);
    cpu.regs.p &= !(FLAG_V | FLAG_C);
    if ((a ^ (sum & 0xFF)) & (u16::from(m) ^ (sum & 0xFF)) & 0x80) == 0x80 {
        cpu.regs.p |= FLAG_V;
    }
    if cpu.regs.p & FLAG_D != 0 {
        let mut lo = (a & 0x0F).wrapping_sub((u16::from(m) & 0x0F) + borrow);
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
        cpu.regs.a = ((lo & 0x0F) | (hi & 0xF0)) as u8;
        set_nz(cpu, cpu.regs.a);
        return;
    }
    if sum & 0xFF00 == 0 {
        cpu.regs.p |= FLAG_C;
    }
    cpu.regs.a = sum as u8;
    set_nz(cpu, cpu.regs.a);
}

/// The compare: subtract without store, with the carry set when the
/// left side does not borrow.
fn compare(cpu: &mut Mos65Sc02, left: u8, right: u8) {
    let cmp = left.wrapping_sub(right);
    set_nz(cpu, cmp);
    set_flag(cpu, FLAG_C, left >= right);
}

fn set_nz(cpu: &mut Mos65Sc02, value: u8) {
    set_zero(cpu, value == 0);
    set_negative(cpu, value & FLAG_N != 0);
}

fn set_zero(cpu: &mut Mos65Sc02, set: bool) {
    set_flag(cpu, FLAG_Z, set);
}

fn set_negative(cpu: &mut Mos65Sc02, set: bool) {
    set_flag(cpu, FLAG_N, set);
}

fn set_flag(cpu: &mut Mos65Sc02, flag: u8, set: bool) {
    if set {
        cpu.regs.p |= flag;
    } else {
        cpu.regs.p &= !flag;
    }
}

fn asl(cpu: &mut Mos65Sc02, value: u8) -> u8 {
    set_flag(cpu, FLAG_C, value & 0x80 != 0);
    let result = value << 1;
    set_nz(cpu, result);
    result
}

fn lsr(cpu: &mut Mos65Sc02, value: u8) -> u8 {
    set_flag(cpu, FLAG_C, value & 1 != 0);
    let result = value >> 1;
    set_nz(cpu, result);
    result
}

fn rol(cpu: &mut Mos65Sc02, value: u8) -> u8 {
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let result = (value << 1) | c as u8;
    set_flag(cpu, FLAG_C, value & 0x80 != 0);
    set_nz(cpu, result);
    result
}

fn ror(cpu: &mut Mos65Sc02, value: u8) -> u8 {
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let result = (value >> 1) | ((c as u8) << 7);
    set_flag(cpu, FLAG_C, value & 1 != 0);
    set_nz(cpu, result);
    result
}

/// True when the branch condition holds.
fn branch_taken(cpu: &Mos65Sc02, opcode: u8) -> bool {
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

/// The reserved-cell NOP forms of the CMOS core: every byte the
/// table does not list runs as an operand-consuming NOP with no
/// machine effect. The shapes follow the addressing work the cells
/// share with the NMOS undocumented forms.
fn execute_nop_form<C: Cycles>(cpu: &mut Mos65Sc02, bus: &mut C, opcode: u8) {
    match opcode {
        // The jam cells. The other cc=10 cells of this column run as
        // the real CMOS zero-page-indirect ALU forms, so they never
        // reach the NOP shape.
        0x02 | 0x22 | 0x42 | 0x62 => {
            bus.wait();
        }
        // The immediate forms consume one byte. The cells 0x80 and
        // 0x89 run as the real BRA and BIT # forms.
        0x82 | 0xC2 | 0xE2 | 0x0B | 0x2B | 0x4B | 0x6B | 0xCB | 0xEB => {
            let _ = immediate(cpu, bus);
        }
        // The zero-page form. The rows 0x04, 0x14, 0x0C, and 0x1C run
        // as the real TSB and TRB forms, the zero-page store rows run
        // as the real STZ forms, and the rows $x7 run as the page-bit
        // group; those cells never reach this shape.
        0x44 => {
            let _ = read_m(cpu, bus, Mode::Zp);
        }
        0x54 | 0xD4 | 0xF4 => {
            let _ = read_m(cpu, bus, Mode::ZpX);
        }
        0xDC | 0xFC | 0x9B | 0xBB => {
            let _ = read_m(cpu, bus, Mode::AbsX);
        }
        0x1B | 0x3B | 0x5B | 0x7B | 0x93 => {
            let _ = read_m(cpu, bus, Mode::IndY);
        }
        0xA3 | 0x83 | 0xC3 | 0xE3 => {
            let _ = read_m(cpu, bus, Mode::IndX);
        }
        _ => {
            let _ = read_m(cpu, bus, Mode::AbsY);
        }
    }
}
