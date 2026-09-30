//! The instruction set: one instruction per core visit.
//!
//! Every memory access carries its exact domain tick, one CPU cycle
//! apart. Read-modify-write instructions write the old value back
//! before the new one, indexed reads perform the wrong-address dummy
//! read on a page cross, and branches spend their extra cycles as real
//! bus reads, so watchpoints and DMA observe the cycle-true access
//! sequence of the NMOS 6502.
//!
//! The undocumented opcodes run the NMOS behavior when the
//! `undocumented` quirk flag is set. Without the flag they run as
//! operand-consuming NOPs, and the jam opcodes run as two-cycle NOPs
//! instead of a lockup. The high-byte-gated store forms of the SH* set
//! run their stable shape.

use demu_core::Address;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::triggers::TriggerEvent;

use crate::cpu::{
    Access, FLAG_B, FLAG_C, FLAG_D, FLAG_I, FLAG_N, FLAG_UNUSED, FLAG_V, FLAG_Z, Mos6502,
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
    /// Absolute indexed in a form that always pays the dummy read.
    AbsXStore,
    /// Absolute indexed with the Y index, in the same form.
    AbsYStore,
    /// Indirect through a zero-page pointer with the X index.
    IndX,
    /// Indirect through a zero-page pointer with the Y index.
    IndY,
    /// Indirect with the Y index, in the form that always pays the
    /// dummy read.
    IndYStore,
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
/// The core reschedules itself to the end of the instruction. A jammed
/// core leaves only through `reset`.
pub fn step(cpu: &mut Mos6502, tick: Tick, ctx: &mut MachineCtx) {
    let mut bus = Access::new(ctx, tick, cpu.bus_name.clone(), cpu.cpu_div);
    if cpu.jammed {
        cpu.next = None;
        return;
    }
    if cpu.reset_pending {
        reset_sequence(cpu, &mut bus);
        cpu.reset_pending = false;
        let cycles = bus.cycles();
        cpu.schedule_after(tick, cycles);
        return;
    }
    if cpu.interrupt_pending() {
        let nmi = cpu.take_nmi();
        service_interrupt(cpu, &mut bus, nmi, false);
        let cycles = bus.cycles();
        cpu.schedule_after(tick, cycles);
        return;
    }
    let start = cpu.regs.pc;
    let opcode = bus.rd(start);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(u32::from(start))));
    execute(cpu, &mut bus, opcode);
    let cycles = bus.cycles();
    cpu.schedule_after(tick, cycles);
}

/// The hardware interrupt service: two idle cycles, three pushes, and
/// the vector read. The break flag states the software-interrupt form.
fn service_interrupt(cpu: &mut Mos6502, bus: &mut Access, nmi: bool, software: bool) {
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
    cpu.regs.pc = (vec_hi << 8) | vec_lo;
}

/// Write the stack byte at the current pointer and decrement it.
fn push(cpu: &mut Mos6502, bus: &mut Access, value: u8) {
    let s = cpu.regs.s;
    bus.wr(0x0100 | u16::from(s), value);
    cpu.regs.s = s.wrapping_sub(1);
}

/// Read the stack byte the pointer names and advance the pointer.
fn pop(cpu: &mut Mos6502, bus: &mut Access) -> u8 {
    let s = cpu.regs.s.wrapping_add(1);
    cpu.regs.s = s;
    bus.rd(0x0100 | u16::from(s))
}

/// The power-on reset sequence: idle cycles, the three dummy pushes
/// that decrement the stack pointer, and the vector read.
fn reset_sequence(cpu: &mut Mos6502, bus: &mut Access) {
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
    cpu.jammed = false;
}

/// The address of one operand, with the exact access sequence of the
/// mode. Indexed loads read the wrong address only on a page cross;
/// index-shaped stores always pay the wrong-address read.
fn operand_addr(cpu: &mut Mos6502, bus: &mut Access, mode: Mode) -> u16 {
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
    }
}

/// Read one operand through the mode's access pattern.
fn read_m(cpu: &mut Mos6502, bus: &mut Access, mode: Mode) -> u8 {
    if mode == Mode::Imm {
        return immediate(cpu, bus);
    }
    let addr = operand_addr(cpu, bus, mode);
    bus.rd(addr)
}

/// Write one operand through the store shape of its mode.
fn store_m(cpu: &mut Mos6502, bus: &mut Access, mode: Mode, value: u8) {
    let addr = operand_addr(cpu, bus, store_of(mode));
    bus.wr(addr, value);
}

/// Read one operand, mutate it, and write the old value back before
/// the new one, as the NMOS read-modify-write sequence does. The
/// result of the mutation returns for the unofficial combos.
fn rmw_m(cpu: &mut Mos6502, bus: &mut Access, mode: Mode, apply: impl Fn(&mut Mos6502, u8) -> u8) {
    let addr = operand_addr(cpu, bus, store_of(mode));
    let value = bus.rd(addr);
    let result = apply(cpu, value);
    bus.wr(addr, value);
    bus.wr(addr, result);
}

/// The addressing modes of the unofficial read-modify-write group.
fn illegal_rmw_mode(opcode: u8) -> Mode {
    match (opcode >> 2) & 0x07 {
        1 => Mode::Zp,
        2 => Mode::ZpX,
        3 => Mode::Abs,
        4 => Mode::IndY,
        5 => Mode::ZpY,
        6 => Mode::AbsY,
        0 => Mode::IndX,
        _ => Mode::AbsX,
    }
}

/// One executed instruction. The cycle count accumulates on the access
/// helper; every arm reads or writes in instruction order.
fn execute(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    if is_undocumented(opcode) {
        if cpu.undocumented {
            execute_undocumented(cpu, bus, opcode);
        } else {
            execute_nop_form(cpu, bus, opcode);
        }
        return;
    }
    // The ALU class `cc = 01`: ORA, AND, EOR, ADC, STA, LDA, CMP, and
    // SBC in their eight addressing modes.
    if opcode & 0x03 == 0x01 {
        let mode = alu_mode(opcode);
        match opcode >> 5 {
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
        // CPX, CPY, BIT.
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
        0x24 => {
            let m = read_m(cpu, bus, Mode::Zp);
            bit(cpu, m);
        }
        0x2C => {
            let m = read_m(cpu, bus, Mode::Abs);
            bit(cpu, m);
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
        // Memory shifts.
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
        // Register increments and decrements.
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
        // Stack operations.
        0x48 => {
            bus.wait();
            push(cpu, bus, cpu.regs.a);
        }
        0x08 => {
            bus.wait();
            push(cpu, bus, cpu.push_p(true));
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
            // The indirect pointer wraps in its own page.
            let ptr = (hi << 8) | lo;
            let target_lo = u16::from(bus.rd(ptr));
            let target_hi = u16::from(bus.rd((ptr & 0xFF00) | ((ptr + 1) & 0xFF)));
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
        // Branches.
        0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 => {
            let taken = branch_taken(cpu, opcode);
            let offset = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            if taken {
                let from = cpu.regs.pc;
                let target = from.wrapping_add_signed(i16::from(offset as i8));
                bus.rd(from);
                if target & 0xFF00 != from & 0xFF00 {
                    bus.rd(target);
                }
                cpu.regs.pc = target;
            }
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
        // through the undocumented forms above. A stray byte costs
        // two cycles and no effect, which keeps the run robust.
        _ => bus.wait(),
    }
}

/// The immediate operand read.
fn immediate(cpu: &mut Mos6502, bus: &mut Access) -> u8 {
    let value = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    value
}

/// The bit test: zero from the accumulator, negative and overflow
/// from the operand.
fn bit(cpu: &mut Mos6502, m: u8) {
    set_zero(cpu, cpu.regs.a & m == 0);
    set_negative(cpu, m & FLAG_N != 0);
    set_flag(cpu, FLAG_V, m & FLAG_V != 0);
}

/// The add with carry, binary and binary-coded decimal.
fn adc(cpu: &mut Mos6502, m: u8) {
    let c = carry(cpu);
    let binary = u16::from(cpu.regs.a) + u16::from(m) + c;
    if cpu.regs.p & FLAG_D == 0 {
        cpu.regs.a = binary as u8;
        set_zero(cpu, binary & 0xFF == 0);
        set_negative(cpu, binary & 0x80 != 0);
        set_flag(cpu, FLAG_C, binary > 0xFF);
        return;
    }
    // The NMOS decimal adjust: fix the low nibble first, set the
    // flags from the halfway-adjusted sum, and set the carry when the
    // high nibble exceeds nine. The zero flag follows the binary sum.
    let a = u16::from(cpu.regs.a);
    let mut lo = (a & 0x0F) + (u16::from(m) & 0x0F) + c;
    if lo > 9 {
        lo += 6;
    }
    let hi = (a >> 4) + (u16::from(m) >> 4) + (lo >> 4);
    let adjusted = ((hi << 4) | (lo & 0x0F)) as u8;
    set_negative(cpu, adjusted & FLAG_N != 0);
    set_flag(
        cpu,
        FLAG_V,
        ((cpu.regs.a ^ m) & (cpu.regs.a ^ adjusted) & 0x80) == 0x80,
    );
    cpu.regs.a = adjusted;
    set_zero(cpu, binary & 0xFF == 0);
    set_flag(cpu, FLAG_C, hi > 9);
    if hi > 9 {
        cpu.regs.a = cpu.regs.a.wrapping_add(0x60);
    }
}

/// The subtract with borrow, binary and binary-coded decimal.
fn sbc(cpu: &mut Mos6502, m: u8) {
    let borrow = u16::from(cpu.regs.p & FLAG_C == 0);
    let a = cpu.regs.a;
    let result = i32::from(a) - i32::from(m) - i32::from(borrow);
    let wrapped = (result & 0xFF) as u8;
    set_zero(cpu, wrapped == 0);
    set_negative(cpu, wrapped & 0x80 != 0);
    set_flag(cpu, FLAG_V, ((a ^ m) & (a ^ wrapped) & 0x80) == 0x80);
    set_flag(cpu, FLAG_C, result >= 0);
    if cpu.regs.p & FLAG_D == 0 {
        cpu.regs.a = wrapped;
        return;
    }
    // The NMOS decimal form: the borrow chain through the two
    // nibbles with the six corrections, and the flags from the
    // binary result.
    let mut lo = i32::from(a & 0x0F) - i32::from(m & 0x0F) - i32::from(borrow);
    let mut hi = i32::from(a >> 4) - i32::from(m >> 4);
    if lo & 0x10 != 0 {
        lo -= 6;
        hi -= 1;
    }
    set_flag(cpu, FLAG_C, hi & 0x10 == 0);
    if hi & 0x10 != 0 {
        hi -= 6;
    }
    cpu.regs.a = (((hi & 0x0F) << 4) | (lo & 0x0F)) as u8;
}

/// The compare: subtract without store, with the carry set when the
/// left side does not borrow.
fn compare(cpu: &mut Mos6502, left: u8, right: u8) {
    let cmp = left.wrapping_sub(right);
    set_nz(cpu, cmp);
    set_flag(cpu, FLAG_C, left >= right);
}

fn set_nz(cpu: &mut Mos6502, value: u8) {
    set_zero(cpu, value == 0);
    set_negative(cpu, value & FLAG_N != 0);
}

fn set_zero(cpu: &mut Mos6502, set: bool) {
    if set {
        cpu.regs.p |= FLAG_Z;
    } else {
        cpu.regs.p &= !FLAG_Z;
    }
}

fn set_negative(cpu: &mut Mos6502, set: bool) {
    if set {
        cpu.regs.p |= FLAG_N;
    } else {
        cpu.regs.p &= !FLAG_N;
    }
}

fn set_flag(cpu: &mut Mos6502, flag: u8, set: bool) {
    if set {
        cpu.regs.p |= flag;
    } else {
        cpu.regs.p &= !flag;
    }
}

fn carry(cpu: &Mos6502) -> u16 {
    u16::from(cpu.regs.p & FLAG_C != 0)
}

fn asl(cpu: &mut Mos6502, value: u8) -> u8 {
    set_flag(cpu, FLAG_C, value & 0x80 != 0);
    let result = value << 1;
    set_nz(cpu, result);
    result
}

fn lsr(cpu: &mut Mos6502, value: u8) -> u8 {
    set_flag(cpu, FLAG_C, value & 1 != 0);
    let result = value >> 1;
    set_nz(cpu, result);
    result
}

fn rol(cpu: &mut Mos6502, value: u8) -> u8 {
    let c = carry(cpu);
    let result = (value << 1) | c as u8;
    set_flag(cpu, FLAG_C, value & 0x80 != 0);
    set_nz(cpu, result);
    result
}

fn ror(cpu: &mut Mos6502, value: u8) -> u8 {
    let c = carry(cpu);
    let result = (value >> 1) | ((c as u8) << 7);
    set_flag(cpu, FLAG_C, value & 1 != 0);
    set_nz(cpu, result);
    result
}

/// True for every undocumented opcode.
fn is_undocumented(opcode: u8) -> bool {
    matches!(
        opcode,
        // The jam opcodes.
        0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2
            | 0xF2
            // The NOP forms.
            | 0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA
            | 0x80 | 0x82 | 0x89 | 0xC2 | 0xE2
            | 0x04 | 0x44 | 0x64 | 0x0C
            | 0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4
            | 0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC
            // LAX, SAX, and the read-modify-write group.
            | 0xA7 | 0xB7 | 0xAF | 0xBF | 0xA3 | 0xB3
            | 0x87 | 0x97 | 0x8F | 0x83
            | 0xC7 | 0xD7 | 0xCF | 0xDF | 0xDB | 0xC3 | 0xD3
            | 0xE7 | 0xF7 | 0xEF | 0xFF | 0xFB | 0xE3 | 0xF3
            | 0x07 | 0x17 | 0x0F | 0x1F | 0x1B | 0x03 | 0x13
            | 0x27 | 0x37 | 0x2F | 0x3F | 0x3B | 0x23 | 0x33
            | 0x47 | 0x57 | 0x4F | 0x5F | 0x5B | 0x43 | 0x53
            | 0x67 | 0x77 | 0x6F | 0x7F | 0x7B | 0x63 | 0x73
            // Immediate specials.
            | 0x0B | 0x2B | 0x4B | 0x6B | 0xCB | 0xEB
            // LAS and the high-byte-gated store forms.
            | 0xBB | 0x9B | 0x93 | 0x9F | 0x9C | 0x9E
    )
}

/// The undocumented opcode forms with the quirk flag on.
fn execute_undocumented(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    match opcode {
        // The jam opcodes: the core locks until reset.
        0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52 | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => {
            bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.jammed = true;
        }
        // NOP with an implied or immediate operand.
        0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA => bus.wait(),
        0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 => {
            let m = immediate(cpu, bus);
            debug_assert!(matches!(opcode, 0x80 | 0x82 | 0x89 | 0xC2 | 0xE2));
            let _ = m;
        }
        // NOP with a zero-page, absolute, or indexed operand.
        0x04 | 0x44 | 0x64 => {
            read_m(cpu, bus, Mode::Zp);
        }
        0x0C => {
            read_m(cpu, bus, Mode::Abs);
        }
        0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 => {
            read_m(cpu, bus, Mode::ZpX);
        }
        0x1C | 0x3C | 0x5C | 0x7C | 0xDC | 0xFC => {
            read_m(cpu, bus, Mode::AbsX);
        }
        // LAX: load both the accumulator and the X index.
        0xA7 | 0xB7 | 0xAF | 0xBF | 0xA3 | 0xB3 => {
            let mode = lax_mode(opcode);
            let m = read_m(cpu, bus, mode);
            set_nz(cpu, m);
            cpu.regs.a = m;
            cpu.regs.x = m;
        }
        // SAX: store the accumulator AND the X index, without flags.
        0x87 => store_m(cpu, bus, Mode::Zp, cpu.regs.a & cpu.regs.x),
        0x97 => store_m(cpu, bus, Mode::ZpY, cpu.regs.a & cpu.regs.x),
        0x8F => store_m(cpu, bus, Mode::Abs, cpu.regs.a & cpu.regs.x),
        0x83 => {
            let addr = operand_addr(cpu, bus, Mode::IndX);
            bus.wr(addr, cpu.regs.a & cpu.regs.x);
        }
        // ANC: AND, with the carry from the result's bit seven.
        0x0B | 0x2B => {
            let m = immediate(cpu, bus);
            let a = cpu.regs.a & m;
            set_nz(cpu, a);
            cpu.regs.a = a;
            set_flag(cpu, FLAG_C, a & FLAG_N != 0);
        }
        // ALR: AND immediate, then the accumulator shifts right.
        0x4B => {
            let m = immediate(cpu, bus);
            let a = cpu.regs.a & m;
            cpu.regs.a = lsr(cpu, a);
        }
        // ARR: AND immediate, then rotate right. The carry takes the
        // result's bit six, and the overflow the exclusive or of the
        // result's top bits.
        0x6B => {
            let m = immediate(cpu, bus);
            let a = cpu.regs.a & m;
            let c_in = carry(cpu);
            let result = (a >> 1) | ((c_in as u8) << 7);
            set_flag(cpu, FLAG_C, result & 0x40 != 0);
            set_flag(cpu, FLAG_V, (result ^ (result << 1)) & 0x80 != 0);
            set_nz(cpu, result);
            cpu.regs.a = result;
        }
        // SBX: the X index takes (A AND X) minus the immediate.
        0xCB => {
            let m = immediate(cpu, bus);
            let left = cpu.regs.a & cpu.regs.x;
            compare(cpu, left, m);
            cpu.regs.x = left.wrapping_sub(m);
        }
        // 0xEB is the undocumented SBC immediate.
        0xEB => {
            let m = immediate(cpu, bus);
            sbc(cpu, m);
        }
        // LAS: the stack register ANDs with the operand.
        0xBB => {
            let m = read_m(cpu, bus, Mode::AbsY);
            let value = cpu.regs.s & m;
            set_nz(cpu, value);
            cpu.regs.a = value;
            cpu.regs.x = value;
            cpu.regs.s = value;
        }
        // The unstable high-byte-gated store forms run their stable
        // shape: the stored byte carries the high-byte gate.
        0x9C | 0x9E | 0x9B | 0x93 | 0x9F => unstable_store(cpu, bus, opcode),
        // The unofficial read-modify-write group: the memory value
        // shifts or decrements, and the accumulator consumes it.
        0xC7 | 0xD7 | 0xCF | 0xDF | 0xDB | 0xC3 | 0xD3 => dcp(cpu, bus, opcode),
        0xE7 | 0xF7 | 0xEF | 0xFF | 0xFB | 0xE3 | 0xF3 => isc_form(cpu, bus, opcode),
        0x07 | 0x17 | 0x0F | 0x1F | 0x1B | 0x03 | 0x13 => slo_form(cpu, bus, opcode),
        0x27 | 0x37 | 0x2F | 0x3F | 0x3B | 0x23 | 0x33 => rla_form(cpu, bus, opcode),
        0x47 | 0x57 | 0x4F | 0x5F | 0x5B | 0x43 | 0x53 => sre_form(cpu, bus, opcode),
        0x67 | 0x77 | 0x6F | 0x7F | 0x7B | 0x63 | 0x73 => rra_form(cpu, bus, opcode),
        // Every undocumented byte routes to a form above.
        _ => bus.wait(),
    }
}

fn lax_mode(opcode: u8) -> Mode {
    match opcode {
        0xA7 => Mode::Zp,
        0xB7 => Mode::ZpY,
        0xAF => Mode::Abs,
        0xBF => Mode::AbsY,
        0xA3 => Mode::IndX,
        _ => Mode::IndY,
    }
}

/// The high-byte-gated store forms: SHY, SHX, SHS, and SHA.
///
/// The stored byte carries the index product gated by the high
/// address byte plus one. This stable shape omits the page-cross
/// instability of the real circuits.
fn unstable_store(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    // The stack pointer takes A AND X on SHS.
    if opcode == 0x9B {
        cpu.regs.s = cpu.regs.a & cpu.regs.x;
    }
    // SHY indexes with X, the others with Y. SHA with the Y-indexed
    // base walks a zero-page pointer first.
    let (lo, high, index) = match opcode {
        0x9C => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            (u16::from(lo), u16::from(hi), cpu.regs.x)
        }
        0x93 => {
            let z = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            let vec_lo = bus.rd(u16::from(z));
            let vec_hi = bus.rd((u16::from(z) + 1) & 0xFF);
            let vec = (u16::from(vec_hi) << 8) | u16::from(vec_lo);
            let low_sum = (vec & 0xFF).wrapping_add(u16::from(cpu.regs.y));
            let broken = (vec & 0xFF00) | (low_sum & 0xFF);
            bus.rd(broken);
            (broken & 0xFF, broken >> 8, cpu.regs.y)
        }
        _ => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            (u16::from(lo), u16::from(hi), cpu.regs.y)
        }
    };
    let addr = (high << 8) + lo + u16::from(index);
    if opcode != 0x93 {
        // The indexed absolute forms pay the wrong-address read.
        bus.rd((high << 8) | ((lo + u16::from(index)) & 0xFF));
    }
    let high_byte = (addr >> 8) as u8;
    let gate = high_byte.wrapping_add(1);
    let value = match opcode {
        0x9C => cpu.regs.y & gate,
        0x9E => cpu.regs.x & gate,
        0x9B => cpu.regs.s & gate,
        _ => cpu.regs.a & cpu.regs.x & gate,
    };
    bus.wr(addr, value);
}

/// The unofficial read-modify-write helpers: the memory value shifts
/// or decrements, and the accumulator then consumes it.
fn slo_form(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    rmw_m(cpu, bus, illegal_rmw_mode(opcode), |c, v| {
        let shifted = asl(c, v);
        let a = c.regs.a | shifted;
        set_nz(c, a);
        c.regs.a = a;
        shifted
    });
}

fn rla_form(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    rmw_m(cpu, bus, illegal_rmw_mode(opcode), |c, v| {
        let rolled = rol(c, v);
        let a = c.regs.a & rolled;
        set_nz(c, a);
        c.regs.a = a;
        rolled
    });
}

fn sre_form(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    rmw_m(cpu, bus, illegal_rmw_mode(opcode), |c, v| {
        let shifted = lsr(c, v);
        let a = c.regs.a ^ shifted;
        set_nz(c, a);
        c.regs.a = a;
        shifted
    });
}

fn rra_form(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    rmw_m(cpu, bus, illegal_rmw_mode(opcode), |c, v| {
        let rotated = ror(c, v);
        adc(c, rotated);
        rotated
    });
}

fn dcp(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    rmw_m(cpu, bus, illegal_rmw_mode(opcode), |c, v| {
        let decremented = v.wrapping_sub(1);
        let a = c.regs.a;
        compare(c, a, decremented);
        decremented
    });
}

fn isc_form(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    rmw_m(cpu, bus, illegal_rmw_mode(opcode), |c, v| {
        let incremented = v.wrapping_add(1);
        sbc(c, incremented);
        incremented
    });
}

/// The undocumented opcodes without the quirk: the operands read
/// through their mode, and no machine effect lands.
fn execute_nop_form(cpu: &mut Mos6502, bus: &mut Access, opcode: u8) {
    match opcode {
        // Implied forms, including the jams in the safe mode.
        0x1A | 0x3A | 0x5A | 0x7A | 0xDA | 0xFA | 0x02 | 0x12 | 0x22 | 0x32 | 0x42 | 0x52
        | 0x62 | 0x72 | 0x92 | 0xB2 | 0xD2 | 0xF2 => bus.wait(),
        // Immediates.
        0x80 | 0x82 | 0x89 | 0xC2 | 0xE2 | 0x0B | 0x2B | 0x4B | 0x6B | 0xCB | 0xEB => {
            immediate(cpu, bus);
        }
        // Every operand form reads with no machine effect.
        _ => {
            read_m(cpu, bus, illegal_read_mode(opcode));
        }
    }
}

/// The read-shaped mode of one undocumented opcode.
fn illegal_read_mode(opcode: u8) -> Mode {
    match opcode {
        0x04 | 0x44 | 0x64 | 0xA7 | 0x87 | 0xC7 | 0xE7 | 0x07 | 0x27 | 0x47 | 0x67 => Mode::Zp,
        0x14 | 0x34 | 0x54 | 0x74 | 0xD4 | 0xF4 | 0xB7 | 0x97 | 0xD7 | 0xF7 | 0x17 | 0x37
        | 0x57 | 0x77 => Mode::ZpX,
        0x0C | 0xAF | 0x8F | 0xCF | 0xEF | 0x0F | 0x2F | 0x4F | 0x6F => Mode::Abs,
        0x1B | 0x3B | 0x5B | 0x7B | 0x93 => Mode::IndY,
        0xA3 | 0x83 | 0xC3 | 0xE3 => Mode::IndX,
        0x9B | 0x9C | 0x9E | 0xBB => Mode::AbsX,
        _ => Mode::AbsY,
    }
}
/// True when the branch condition holds.
fn branch_taken(cpu: &Mos6502, opcode: u8) -> bool {
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
