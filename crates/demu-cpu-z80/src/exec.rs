//! The instruction set: one instruction per core visit.
//!
//! The access sequence follows the published NMOS Z80: one opcode
//! fetch costs 4 T-states, one memory access 3, one port access 4
//! with its automatic wait state, and the index-register forms pay
//! the internal address add as the published totals require: the
//! read forms add 5 T-states between the displacement read and the
//! operand access, the immediate store 2 before the store, and the
//! index-CB forms double the operand read. The repeated block
//! instructions refetch their opcode bytes once per iteration, so
//! the refresh counter gains two per iteration and each iteration
//! total matches the published 21 T-states with 16 for the last
//! one.
//!
//! The undocumented forms run when the `undocumented` quirk flag is
//! set: the index-register halves (IXH, IXL, IYH, IYL), the SLL
//! shift, the `OUT (C),0` port store, the `IN (C)` flag-only input,
//! and the index-register IN and OUT forms. With the flag clear the
//! forms run effect-free with the same timing, with the index halves
//! reading as zero.
//!
//! The flag bits 3 and 5 copy the result or operand bits where the
//! NMOS core copies them: arithmetic results, bit tests of
//! registers, and the address of a memory bit test. Interrupt
//! acceptance happens at instruction boundaries only: the maskable
//! level takes once per high period, an EI defers the acceptance by
//! one boundary, and HALT parks the core until an NMI edge or an
//! accepted interrupt wakes it. The block forms run to their end
//! without sampling, since the boundary is the only sample point.
//!
//! The tests drive the same step function over a plain byte map
//! through the [`crate::cpu::Cycles`] trait, so the asserted T-state
//! totals cover the published table cells and the asserted reads
//! cover the address forms.

use crate::cpu::{
    Access, Cycles, FLAG_C, FLAG_H, FLAG_N, FLAG_P, FLAG_S, FLAG_X3, FLAG_X5, FLAG_Z, Z80,
};
use demu_core::Address;
use demu_core::Level;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::triggers::TriggerEvent;

/// Step one instruction from `tick`: an interrupt service first when
/// one is pending, else the instruction at the program counter.
///
/// The core reschedules itself to the end of the instruction. A core
/// parked with HALT leaves only through an accepted interrupt or an
/// NMI edge. The wrapper drives the `halt` line on a parked-state
/// transition, so the ULA of the ZX81 watches the parked core and
/// takes over the display line while the core is down.
pub fn step(cpu: &mut Z80, tick: Tick, ctx: &mut MachineCtx) {
    let was = cpu.halted;
    {
        let mut bus = Access::for_cpu(cpu, tick, ctx);
        step_with(cpu, tick, &mut bus);
    }
    if cpu.halted != was {
        let level = if cpu.halted { Level::High } else { Level::Low };
        ctx.notify("halt", level, tick);
    }
}

/// One instruction over an arbitrary [`Cycles`] bus.
pub fn step_with<C: Cycles>(cpu: &mut Z80, tick: Tick, bus: &mut C) {
    if cpu.reset_pending {
        cpu.reset_pending = false;
        // The published power-on shape: one 6 T-state reset sequence.
        // The register form and the program counter already reset.
        bus.wait(6);
        cpu.schedule_after(tick, bus.cycles());
        return;
    }
    if cpu.halted {
        let service = cpu.nmi_edge || cpu.int_acceptable();
        if !service {
            // Park: the scheduler re-seeds when a line wakes the core.
            cpu.next = None;
            return;
        }
    }
    // An EI defers the maskable acceptance by one boundary: the
    // boundary right after EI runs with the delay, the one after
    // that accepts.
    let deferred = cpu.ei_delay;
    cpu.ei_delay = false;
    if cpu.nmi_edge {
        cpu.take_nmi();
        service_nmi(cpu, bus);
        cpu.schedule_after(tick, bus.cycles());
        return;
    }
    if cpu.int_acceptable() && !deferred {
        cpu.int_taken = true;
        service_int(cpu, bus);
        cpu.schedule_after(tick, bus.cycles());
        return;
    }
    let pc = cpu.regs.pc;
    let op = bus.m1(pc);
    cpu.r_bump();
    cpu.regs.pc = pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(u32::from(pc))));
    if op == 0xDD || op == 0xFD {
        // The last prefix of a prefix chain wins.
        let side = if op == 0xDD { Index::X } else { Index::Y };
        exec_indexed(cpu, bus, side);
    } else if op == 0xED {
        exec_ed(cpu, bus);
    } else if op == 0xCB {
        exec_cb(cpu, bus);
    } else {
        exec_group(cpu, bus, op, Index::None, cpu_hl(cpu));
    }
    cpu.schedule_after(tick, bus.cycles());
}

/// One accepted NMI service: 11 T-states with the stack decrement
/// inside the acknowledge cycle, a push of the program counter, and
/// a jump to 0x0066 with IFF1 cleared. The refresh counter gains
/// two.
fn service_nmi<C: Cycles>(cpu: &mut Z80, bus: &mut C) {
    cpu.r_bump();
    cpu.r_bump();
    bus.wait(5);
    cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
    bus.wr(cpu.regs.sp, (cpu.regs.pc >> 8) as u8);
    cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
    bus.wr(cpu.regs.sp, cpu.regs.pc as u8);
    cpu.regs.pc = 0x0066;
    cpu.iff1 = false;
    cpu.halted = false;
}

/// One accepted maskable service in the selected mode.
fn service_int<C: Cycles>(cpu: &mut Z80, bus: &mut C) {
    cpu.r_bump();
    cpu.r_bump();
    cpu.halted = false;
    match cpu.im {
        // Mode 0: the accepted data byte runs as an instruction with
        // the program counter at the interrupted address. A RST on
        // the data bus pushes and jumps as it would in memory.
        0 => {
            bus.wait(7);
            let v = bus.rd_io(0x0000);
            exec_group(cpu, bus, v, Index::None, cpu_hl(cpu));
        }
        // Mode 1: 13 T-states with the stack decrement inside the
        // acknowledge cycle, to 0x0038.
        1 => {
            bus.wait(7);
            cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
            bus.wr(cpu.regs.sp, (cpu.regs.pc >> 8) as u8);
            cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
            bus.wr(cpu.regs.sp, cpu.regs.pc as u8);
            cpu.regs.pc = 0x0038;
        }
        // Mode 2: 19 T-states. The vector low byte reads from the
        // port bus at 0x0000 and the program counter jumps to the
        // interrupt page register paired with it.
        _ => {
            bus.wait(7);
            cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
            bus.wr(cpu.regs.sp, (cpu.regs.pc >> 8) as u8);
            cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
            bus.wr(cpu.regs.sp, cpu.regs.pc as u8);
            let lo = bus.rd_io(0x0000);
            bus.wait(2);
            cpu.regs.pc = (u16::from(cpu.regs.i) << 8) | u16::from(lo);
        }
    }
}

/// The index side of one prefixed instruction chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Index {
    None,
    X,
    Y,
}

/// One 8-bit operand slot after an index remap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op8 {
    B,
    C,
    D,
    E,
    H,
    L,
    A,
    /// The index-register high byte (an undocumented slot).
    Hx,
    /// The index-register low byte (an undocumented slot).
    Lx,
    /// The IY high byte (an undocumented slot).
    Hy,
    /// The IY low byte (an undocumented slot).
    Ly,
    /// The memory at the effective address.
    Mem,
}

/// Decode the 3-bit register field with the index halves remapped.
fn decode8(index: u8, pre: Index) -> Op8 {
    match index {
        0 => Op8::B,
        1 => Op8::C,
        2 => Op8::D,
        3 => Op8::E,
        4 => match pre {
            Index::X => Op8::Hx,
            Index::Y => Op8::Hy,
            _ => Op8::H,
        },
        5 => match pre {
            Index::X => Op8::Lx,
            Index::Y => Op8::Ly,
            _ => Op8::L,
        },
        6 => Op8::Mem,
        _ => Op8::A,
    }
}

/// Read one operand slot. A core without the undocumented quirk
/// reads the index halves as zero.
fn get8<C: Cycles>(cpu: &mut Z80, bus: &mut C, slot: Op8, ea: u16) -> u8 {
    match slot {
        Op8::B => cpu.regs.b,
        Op8::C => cpu.regs.c,
        Op8::D => cpu.regs.d,
        Op8::E => cpu.regs.e,
        Op8::H => cpu.regs.h,
        Op8::L => cpu.regs.l,
        Op8::A => cpu.regs.a,
        Op8::Hx => {
            if cpu.undocumented {
                (cpu.regs.ix >> 8) as u8
            } else {
                0
            }
        }
        Op8::Lx => {
            if cpu.undocumented {
                cpu.regs.ix as u8
            } else {
                0
            }
        }
        Op8::Hy => {
            if cpu.undocumented {
                (cpu.regs.iy >> 8) as u8
            } else {
                0
            }
        }
        Op8::Ly => {
            if cpu.undocumented {
                cpu.regs.iy as u8
            } else {
                0
            }
        }
        Op8::Mem => bus.rd(ea),
    }
}

/// Write one operand slot. A core without the undocumented quirk
/// discards the index halves.
fn set8<C: Cycles>(cpu: &mut Z80, bus: &mut C, slot: Op8, ea: u16, v: u8) {
    match slot {
        Op8::B => cpu.regs.b = v,
        Op8::C => cpu.regs.c = v,
        Op8::D => cpu.regs.d = v,
        Op8::E => cpu.regs.e = v,
        Op8::H => cpu.regs.h = v,
        Op8::L => cpu.regs.l = v,
        Op8::A => cpu.regs.a = v,
        Op8::Hx => {
            if cpu.undocumented {
                cpu.regs.ix = (cpu.regs.ix & 0x00FF) | (u16::from(v) << 8);
            }
        }
        Op8::Lx => {
            if cpu.undocumented {
                cpu.regs.ix = (cpu.regs.ix & 0xFF00) | u16::from(v);
            }
        }
        Op8::Hy => {
            if cpu.undocumented {
                cpu.regs.iy = (cpu.regs.iy & 0x00FF) | (u16::from(v) << 8);
            }
        }
        Op8::Ly => {
            if cpu.undocumented {
                cpu.regs.iy = (cpu.regs.iy & 0xFF00) | u16::from(v);
            }
        }
        Op8::Mem => bus.wr(ea, v),
    }
}

/// The 16-bit pair of one 2-bit field; 3 selects the stack pointer
/// in the load forms. The push and pop forms turn field 3 into the
/// AF pair before calling here.
fn pair_of(field: u8, pre: Index, cpu: &Z80) -> u16 {
    match field {
        0 => u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c),
        1 => u16::from(cpu.regs.d) << 8 | u16::from(cpu.regs.e),
        2 => match pre {
            Index::X => cpu.regs.ix,
            Index::Y => cpu.regs.iy,
            _ => u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l),
        },
        _ => cpu.regs.sp,
    }
}

fn set_pair(cpu: &mut Z80, field: u8, pre: Index, v: u16) {
    match field {
        0 => {
            cpu.regs.b = (v >> 8) as u8;
            cpu.regs.c = v as u8;
        }
        1 => {
            cpu.regs.d = (v >> 8) as u8;
            cpu.regs.e = v as u8;
        }
        2 => match pre {
            Index::X => cpu.regs.ix = v,
            Index::Y => cpu.regs.iy = v,
            _ => {
                cpu.regs.h = (v >> 8) as u8;
                cpu.regs.l = v as u8;
            }
        },
        _ => cpu.regs.sp = v,
    }
}

/// The AF pair.
fn af_pair(cpu: &Z80) -> u16 {
    u16::from(cpu.regs.a) << 8 | u16::from(cpu.regs.f)
}

/// The HL pair.
fn cpu_hl(cpu: &Z80) -> u16 {
    u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l)
}

/// A parity of one byte: true when an even count of bits set.
fn parity(v: u8) -> bool {
    v.count_ones().is_multiple_of(2)
}

/// Set the sign, zero, parity, and copy flags of a result byte.
fn result_flags(cpu: &mut Z80, v: u8) {
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_S | FLAG_Z | FLAG_P | FLAG_X3 | FLAG_X5);
    if v & 0x80 != 0 {
        *f |= FLAG_S;
    }
    if v == 0 {
        *f |= FLAG_Z;
    }
    if parity(v) {
        *f |= FLAG_P;
    }
    if v & 0x08 != 0 {
        *f |= FLAG_X3;
    }
    if v & 0x20 != 0 {
        *f |= FLAG_X5;
    }
}

/// Copy the bits 3 and 5 of one byte onto the flag copies.
fn copy_bits(cpu: &mut Z80, v: u8) {
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_X3 | FLAG_X5);
    if v & 0x08 != 0 {
        *f |= FLAG_X3;
    }
    if v & 0x20 != 0 {
        *f |= FLAG_X5;
    }
}

/// The 8-bit arithmetic unit. Kinds 0-3 = ADD/ADC/SUB/SBC, 4 = AND,
/// 5 = XOR, 6 = OR, 7 = CP. The result lands in A except for the CP
/// kind; the published flag forms apply and the copy flags take the
/// result byte bits.
fn alu8(cpu: &mut Z80, kind: u8, b: u8) {
    let a = cpu.regs.a;
    match kind {
        0 | 1 => {
            let cin = u8::from(kind == 1 && cpu.regs.f & FLAG_C != 0);
            let sum = u32::from(a) + u32::from(b) + u32::from(cin);
            let f = &mut cpu.regs.f;
            *f &= !(FLAG_C | FLAG_H | FLAG_N | FLAG_P);
            if sum > 0xFF {
                *f |= FLAG_C;
            }
            if (u32::from(a) & 0xF) + (u32::from(b) & 0xF) + u32::from(cin) > 0xF {
                *f |= FLAG_H;
            }
            if (a ^ sum as u8) & (b ^ sum as u8) & 0x80 != 0 {
                *f |= FLAG_P;
            }
            let v = sum as u8;
            result_flags(cpu, v);
            cpu.regs.a = v;
        }
        2 | 3 => {
            let bin = u8::from(kind == 3 && cpu.regs.f & FLAG_C != 0);
            let diff = i16::from(a) - i16::from(b) - i16::from(bin);
            let f = &mut cpu.regs.f;
            *f &= !(FLAG_C | FLAG_H | FLAG_P);
            *f |= FLAG_N;
            if diff < 0 {
                *f |= FLAG_C;
            }
            if (a & 0x0F) < (b & 0x0F) + bin {
                *f |= FLAG_H;
            }
            if (a ^ b) & (a ^ (diff as u8)) & 0x80 != 0 {
                *f |= FLAG_P;
            }
            let v = diff as u8;
            result_flags(cpu, v);
            // The SUB and SBC A forms store the result.
            cpu.regs.a = v;
        }
        // The AND form sets the half carry; the OR and XOR forms
        // clear it. None set the subtract flag.
        4 => {
            let r = a & b;
            cpu.regs.f = (cpu.regs.f & !(FLAG_C | FLAG_N)) | FLAG_H;
            result_flags(cpu, r);
            cpu.regs.a = r;
        }
        5 | 6 => {
            let r = if kind == 5 { a ^ b } else { a | b };
            cpu.regs.f &= !(FLAG_C | FLAG_H | FLAG_N);
            result_flags(cpu, r);
            cpu.regs.a = r;
        }
        _ => {
            // CP: the subtract flags with no store.
            let diff = i16::from(a) - i16::from(b);
            let f = &mut cpu.regs.f;
            *f &= !(FLAG_C | FLAG_H | FLAG_P);
            *f |= FLAG_N;
            if diff < 0 {
                *f |= FLAG_C;
            }
            if (a & 0x0F) < (b & 0x0F) {
                *f |= FLAG_H;
            }
            if (a ^ b) & (a ^ (diff as u8)) & 0x80 != 0 {
                *f |= FLAG_P;
            }
            result_flags(cpu, diff as u8);
        }
    }
}

/// The decimal adjust over the published correction table.
fn daa(cpu: &mut Z80) {
    let a = cpu.regs.a;
    let f = cpu.regs.f;
    let sub = f & FLAG_N != 0;
    let mut adj = 0u16;
    if f & FLAG_H != 0 || (a & 0x0F) > 9 {
        adj |= 6;
    }
    if f & FLAG_C != 0 || a > 0x99 {
        adj |= 0x60;
    }
    if sub {
        let v = a.wrapping_sub(adj as u8);
        result_flags(cpu, v);
        cpu.regs.a = v;
        // The subtract correction does not change the carry.
        cpu.regs.f &= !(FLAG_H | FLAG_X3 | FLAG_X5);
    } else {
        let sum = u16::from(a) + adj;
        let v = sum as u8;
        result_flags(cpu, v);
        if sum > 0xFF {
            cpu.regs.f |= FLAG_C;
        }
        cpu.regs.a = v;
        cpu.regs.f &= !(FLAG_H | FLAG_X3 | FLAG_X5);
    }
}

/// The 16-bit add or subtract with the published carry, half-carry,
/// overflow, subtract, and copy flags. The sign and zero flags stay
/// untouched; the ADC/SBC HL forms set them from the result.
fn add_sub16(cpu: &mut Z80, a: u16, b: u16, carry_in: bool, subtract: bool) -> u16 {
    let (v, carry, carry11, overflow) = if subtract {
        let diff = i32::from(a) - i32::from(b) - i32::from(carry_in as u8);
        let borrow11 =
            (i32::from(a) & 0x0FFF) - (i32::from(b) & 0x0FFF) - i32::from(carry_in as u8) < 0;
        let v = diff as u16;
        let overflow = (a & 0x8000) != (b & 0x8000) && (v & 0x8000) != (a & 0x8000);
        (v, diff < 0, borrow11, overflow)
    } else {
        let sum = u32::from(a) + u32::from(b) + u32::from(carry_in as u8);
        let carry11 =
            (u32::from(a) & 0x0FFF) + (u32::from(b) & 0x0FFF) + u32::from(carry_in as u8) > 0x0FFF;
        let v = sum as u16;
        let overflow = (a & 0x8000) == (b & 0x8000) && (v & 0x8000) != (a & 0x8000);
        (v, sum > 0xFFFF, carry11, overflow)
    };
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_C | FLAG_H | FLAG_P | FLAG_N | FLAG_X3 | FLAG_X5);
    if subtract {
        *f |= FLAG_N;
    }
    if carry {
        *f |= FLAG_C;
    }
    if carry11 {
        *f |= FLAG_H;
    }
    if overflow {
        *f |= FLAG_P;
    }
    copy_bits(cpu, v as u8);
    v
}

/// The ADC or SBC HL,ss form: the common 16-bit flags plus the sign
/// and zero of the whole result.
fn adc_sbc16(cpu: &mut Z80, a: u16, b: u16, subtract: bool) {
    let carry = cpu.regs.f & FLAG_C != 0;
    let v = add_sub16(cpu, a, b, carry, subtract);
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_Z | FLAG_S);
    if v == 0 {
        *f |= FLAG_Z;
    }
    if v & 0x8000 != 0 {
        *f |= FLAG_S;
    }
    set_pair(cpu, 2, Index::None, v);
}

/// Condition test of the `cc` field.
fn cond(cpu: &Z80, cc: u8) -> bool {
    let f = cpu.regs.f;
    match cc {
        0 => f & FLAG_Z == 0,
        1 => f & FLAG_Z != 0,
        2 => f & FLAG_C == 0,
        3 => f & FLAG_C != 0,
        4 => f & FLAG_P == 0,
        5 => f & FLAG_P != 0,
        6 => f & FLAG_S == 0,
        _ => f & FLAG_S != 0,
    }
}

/// Fetch a 16-bit immediate, low byte first.
fn fetch16<C: Cycles>(cpu: &mut Z80, bus: &mut C) -> u16 {
    let lo = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    let hi = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    u16::from(lo) | (u16::from(hi) << 8)
}

/// Push a 16-bit pair: the high byte first, with the published one
/// T-state internal step.
fn push2<C: Cycles>(cpu: &mut Z80, bus: &mut C, v: u16) {
    bus.wait(1);
    cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
    bus.wr(cpu.regs.sp, (v >> 8) as u8);
    cpu.regs.sp = cpu.regs.sp.wrapping_sub(1);
    bus.wr(cpu.regs.sp, v as u8);
}

/// Pop a 16-bit pair: the low byte from (SP).
fn pop2<C: Cycles>(cpu: &mut Z80, bus: &mut C) -> u16 {
    let lo = bus.rd(cpu.regs.sp);
    cpu.regs.sp = cpu.regs.sp.wrapping_add(1);
    let hi = bus.rd(cpu.regs.sp);
    cpu.regs.sp = cpu.regs.sp.wrapping_add(1);
    u16::from(lo) | (u16::from(hi) << 8)
}

/// A relative jump: the displacement reads, then a taken jump pays
/// 5 internal T-states and applies the two's-complement offset.
fn jump_relative<C: Cycles>(cpu: &mut Z80, bus: &mut C, taken: bool) {
    let e = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    if taken {
        bus.wait(5);
        cpu.regs.pc = cpu.regs.pc.wrapping_add(e as i8 as i16 as u16);
    }
}

/// DJNZ: the B decrement gates the jump; the published totals are 13
/// taken with 8 not taken over the opcode, displacement, and
/// internal steps.
fn djnz<C: Cycles>(cpu: &mut Z80, bus: &mut C) {
    let e = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    bus.wait(1);
    cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    if cpu.regs.b != 0 {
        bus.wait(5);
        cpu.regs.pc = cpu.regs.pc.wrapping_add(e as i8 as i16 as u16);
    }
}

/// The accumulator rotations: RLCA, RRCA, RLA, RRA.
fn rotate_a(cpu: &mut Z80, kind: u8) {
    let a = cpu.regs.a;
    let carry = cpu.regs.f & FLAG_C != 0;
    let v = match kind {
        0 => a.rotate_left(1),                  // RLCA
        1 => a.rotate_right(1),                 // RRCA
        2 => (a << 1) | u8::from(carry),        // RLA
        _ => (a >> 1) | (u8::from(carry) << 7), // RRA
    };
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_C | FLAG_H | FLAG_N | FLAG_S | FLAG_Z | FLAG_P | FLAG_X3 | FLAG_X5);
    let new_carry = match kind {
        0 | 2 => a & 0x80 != 0,
        _ => a & 1 != 0,
    };
    if new_carry {
        *f |= FLAG_C;
    }
    copy_bits(cpu, v);
    cpu.regs.a = v;
}

/// Increment one byte with the published flags; the signed overflow
/// lands in the parity bit after the result flags.
fn inc8(cpu: &mut Z80, v: u8) -> u8 {
    let r = v.wrapping_add(1);
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_Z | FLAG_H | FLAG_N | FLAG_S | FLAG_P | FLAG_X3 | FLAG_X5);
    if (v & 0x0F) == 0x0F {
        *f |= FLAG_H;
    }
    result_flags(cpu, r);
    if v == 0x7F {
        cpu.regs.f |= FLAG_P;
    }
    r
}

/// Decrement one byte with the published flags; the signed overflow
/// lands in the parity bit after the result flags.
fn dec8(cpu: &mut Z80, v: u8) -> u8 {
    let r = v.wrapping_sub(1);
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_Z | FLAG_H | FLAG_S | FLAG_P | FLAG_X3 | FLAG_X5);
    *f |= FLAG_N;
    if (v & 0x0F) == 0 {
        *f |= FLAG_H;
    }
    result_flags(cpu, r);
    if v == 0x80 {
        cpu.regs.f |= FLAG_P;
    }
    r
}

/// The CB shift family. Kind 6 is the SLL shift, an undocumented
/// form.
fn cb_shift(cpu: &mut Z80, kind: u8, v: u8) -> u8 {
    let carry = cpu.regs.f & FLAG_C != 0;
    let r = match kind {
        0 => v.rotate_left(1),                                 // RLC
        1 => v.rotate_right(1),                                // RRC
        2 => v.rotate_left(1) & !1 | u8::from(carry),          // RL
        3 => v.rotate_right(1) & !0x80 | ((carry as u8) << 7), // RR
        4 => v << 1,                                           // SLA
        5 => (v >> 1) | (v & 0x80),                            // SRA
        6 => (v << 1) | 1,                                     // SLL
        _ => v >> 1,                                           // SRL
    };
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_C | FLAG_H | FLAG_N | FLAG_P | FLAG_S | FLAG_Z | FLAG_X3 | FLAG_X5);
    let new_carry = match kind {
        0 | 2 | 4 | 6 => v & 0x80 != 0,
        _ => v & 1 != 0,
    };
    if new_carry {
        *f |= FLAG_C;
    }
    result_flags(cpu, r);
    r
}

/// A bit test with the published whole-flag form: the half carry
/// sets, the subtract flag clears, parity follows zero, the sign
/// copies bit 7 of the bit-7 test, the carry survives, and the copy
/// flags take the source bits.
fn bit(cpu: &mut Z80, n: u8, v: u8, copy: u8) {
    let carry = cpu.regs.f & FLAG_C;
    let zero = v & (1 << n) == 0;
    cpu.regs.f = FLAG_H
        | carry
        | if zero { FLAG_Z | FLAG_P } else { 0 }
        | if n == 7 && v & 0x80 != 0 { FLAG_S } else { 0 }
        | if copy & 0x08 != 0 { FLAG_X3 } else { 0 }
        | if copy & 0x20 != 0 { FLAG_X5 } else { 0 };
}

fn set_bit(v: u8, n: u8, on: bool) -> u8 {
    if on { v | (1 << n) } else { v & !(1 << n) }
}

/// The whole 256-byte table of the plain and prefixed instructions.
/// The index prefix remaps the half-register and memory slots of the
/// 8-bit forms and the HL pair of the 16-bit forms; `ea` carries the
/// effective address of the memory slot (the HL pair when no prefix
/// remaps it).
fn exec_group<C: Cycles>(cpu: &mut Z80, bus: &mut C, op: u8, pre: Index, ea: u16) {
    let y = (op >> 3) & 7;
    let p = (op >> 4) & 3;
    match op {
        // NOP and the self-transfers LD r,r (the diagonal of the LD
        // matrix; the prefixed forms of the index-half diagonals are
        // the same identities).
        0x00 | 0x40 | 0x49 | 0x52 | 0x5B | 0x64 | 0x6D | 0x7F => {}
        // LD dd,nn and the index loads (10 T-states).
        0x01 | 0x11 | 0x21 | 0x31 => {
            let v = fetch16(cpu, bus);
            set_pair(cpu, p, pre, v);
        }
        0x02 => {
            // LD (BC),A
            bus.wr(
                u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c),
                cpu.regs.a,
            );
        }
        0x0A => {
            cpu.regs.a = bus.rd(u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c));
        }
        0x12 => {
            bus.wr(
                u16::from(cpu.regs.d) << 8 | u16::from(cpu.regs.e),
                cpu.regs.a,
            );
        }
        0x1A => {
            cpu.regs.a = bus.rd(u16::from(cpu.regs.d) << 8 | u16::from(cpu.regs.e));
        }
        // INC ss (8 T-states) and DEC ss (6).
        0x03 | 0x13 | 0x23 | 0x33 => {
            bus.wait(4);
            let v = pair_of(p, pre, cpu).wrapping_add(1);
            set_pair(cpu, p, pre, v);
        }
        0x0B | 0x1B | 0x2B | 0x3B => {
            bus.wait(2);
            let v = pair_of(p, pre, cpu).wrapping_sub(1);
            set_pair(cpu, p, pre, v);
        }
        // LD (nn),pair (16) and pair,(nn) (16).
        0x22 => {
            let a = fetch16(cpu, bus);
            let v = pair_of(2, pre, cpu);
            bus.wr(a, v as u8);
            bus.wr(a.wrapping_add(1), (v >> 8) as u8);
        }
        0x2A => {
            let a = fetch16(cpu, bus);
            let lo = bus.rd(a);
            let hi = bus.rd(a.wrapping_add(1));
            set_pair(cpu, 2, pre, u16::from(lo) | (u16::from(hi) << 8));
        }
        // LD (nn),A (13) and A,(nn) (13).
        0x32 => {
            let a = fetch16(cpu, bus);
            bus.wr(a, cpu.regs.a);
        }
        0x3A => {
            let a = fetch16(cpu, bus);
            cpu.regs.a = bus.rd(a);
        }
        0x08 => {
            // EX AF,AF'
            std::mem::swap(&mut cpu.regs.a, &mut cpu.regs.a2);
            std::mem::swap(&mut cpu.regs.f, &mut cpu.regs.f2);
        }
        0xD9 => {
            // EXX
            std::mem::swap(&mut cpu.regs.b, &mut cpu.regs.b2);
            std::mem::swap(&mut cpu.regs.c, &mut cpu.regs.c2);
            std::mem::swap(&mut cpu.regs.d, &mut cpu.regs.d2);
            std::mem::swap(&mut cpu.regs.e, &mut cpu.regs.e2);
            std::mem::swap(&mut cpu.regs.h, &mut cpu.regs.h2);
            std::mem::swap(&mut cpu.regs.l, &mut cpu.regs.l2);
        }
        0x10 => djnz(cpu, bus),
        0x18 => jump_relative(cpu, bus, true),
        0x20 | 0x28 | 0x30 | 0x38 => jump_relative(cpu, bus, cond(cpu, (op >> 3) & 3)),
        0x27 => daa(cpu),
        0x2F => {
            // CPL
            cpu.regs.a = !cpu.regs.a;
            cpu.regs.f |= FLAG_H | FLAG_N;
            copy_bits(cpu, cpu.regs.a);
        }
        0x37 => {
            // SCF
            cpu.regs.f |= FLAG_C;
            cpu.regs.f &= !(FLAG_H | FLAG_N);
            copy_bits(cpu, cpu.regs.a);
        }
        0x3F => {
            // CCF
            let carry = cpu.regs.f & FLAG_C != 0;
            cpu.regs.f = cpu.regs.f & !FLAG_C | if carry { 0 } else { FLAG_C };
            cpu.regs.f |= FLAG_H;
            cpu.regs.f &= !FLAG_N;
            copy_bits(cpu, cpu.regs.a);
        }
        // LD SP,HL (or the index pair): 6 T-states.
        0xF9 => {
            bus.wait(2);
            cpu.regs.sp = pair_of(2, pre, cpu);
        }
        0xE3 => {
            // EX (SP),HL or the index pair: 19 or 23 T-states.
            let old = pair_of(2, pre, cpu);
            let low = bus.rd(cpu.regs.sp);
            let high = bus.rd(cpu.regs.sp.wrapping_add(1));
            bus.wait(1);
            bus.wr(cpu.regs.sp, old as u8);
            bus.wr(cpu.regs.sp.wrapping_add(1), (old >> 8) as u8);
            bus.wait(2);
            set_pair(cpu, 2, pre, u16::from(low) | (u16::from(high) << 8));
        }
        0xE9 => cpu.regs.pc = pair_of(2, pre, cpu),
        // PUSH of the four pairs (11); AF pushes with the pair form.
        0xC5 | 0xD5 | 0xE5 | 0xF5 => {
            let v = if p == 3 {
                af_pair(cpu)
            } else {
                pair_of(p, pre, cpu)
            };
            push2(cpu, bus, v);
        }
        // POP of the four pairs (10); AF pops into A and F.
        0xC1 | 0xD1 | 0xE1 | 0xF1 => {
            let v = pop2(cpu, bus);
            if p == 3 {
                cpu.regs.a = (v >> 8) as u8;
                cpu.regs.f = v as u8;
            } else {
                set_pair(cpu, p, pre, v);
            }
        }
        0xFB => {
            // EI: the flag pair sets; one boundary defers the
            // maskable acceptance.
            cpu.iff1 = true;
            cpu.iff2 = true;
            cpu.ei_delay = true;
        }
        0xF3 => {
            cpu.iff1 = false;
            cpu.iff2 = false;
        }
        0xEB => {
            // EX DE,HL
            let de = pair_of(1, Index::None, cpu);
            set_pair(cpu, 1, Index::None, cpu_hl(cpu));
            set_pair(cpu, 2, Index::None, de);
        }
        // ADD HL,ss or ADD IX,IY,ss (15 T-states).
        0x09 | 0x19 | 0x29 | 0x39 => {
            bus.wait(7);
            let a = pair_of(2, pre, cpu);
            let b = pair_of(p, pre, cpu);
            let v = add_sub16(cpu, a, b, false, false);
            set_pair(cpu, 2, pre, v);
        }
        0xD3 => {
            // OUT (n),A: the port high byte carries the accumulator.
            let n = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            bus.wr_io(u16::from(cpu.regs.a) << 8 | u16::from(n), cpu.regs.a);
        }
        0xDB => {
            // IN A,(n): the port high byte carries the accumulator.
            let n = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.a = bus.rd_io(u16::from(cpu.regs.a) << 8 | u16::from(n));
        }
        0xC3 | 0xC2 | 0xCA | 0xD2 | 0xDA | 0xE2 | 0xEA | 0xF2 | 0xFA => {
            // JP nn and the conditional forms (10 T-states either way).
            let a = fetch16(cpu, bus);
            if op == 0xC3 || cond(cpu, y) {
                cpu.regs.pc = a;
            }
        }
        0xCD => {
            let a = fetch16(cpu, bus);
            push2(cpu, bus, cpu.regs.pc);
            cpu.regs.pc = a;
        }
        0xC9 => {
            let v = pop2(cpu, bus);
            cpu.regs.pc = v;
        }
        // RET cc: 11 taken, 5 not taken; the internal step pads both.
        0xC0 | 0xC8 | 0xD0 | 0xD8 | 0xE0 | 0xE8 | 0xF0 | 0xF8 => {
            bus.wait(1);
            if cond(cpu, y) {
                cpu.regs.pc = pop2(cpu, bus);
            }
        }
        // CALL cc,nn (17 taken, 10 not taken).
        0xC4 | 0xCC | 0xD4 | 0xDC | 0xE4 | 0xEC | 0xF4 | 0xFC => {
            let a = fetch16(cpu, bus);
            if cond(cpu, y) {
                push2(cpu, bus, cpu.regs.pc);
                cpu.regs.pc = a;
            }
        }
        // RST p (11 T-states).
        0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
            push2(cpu, bus, cpu.regs.pc);
            cpu.regs.pc = u16::from(op & 0x38);
        }
        0x76 => cpu.halted = true,
        _ => exec_8bit(cpu, bus, op, pre, ea),
    }
}

/// The 8-bit groups of the table: the LD matrix, the immediate
/// loads, the increments and decrements, and the ALU rows.
fn exec_8bit<C: Cycles>(cpu: &mut Z80, bus: &mut C, op: u8, pre: Index, ea: u16) {
    let y = (op >> 3) & 7;
    let z = op & 7;
    match op {
        // LD r,r' over the 40-7F matrix.
        0x40..=0x7F => {
            let v = get8(cpu, bus, decode8(z, pre), ea);
            set8(cpu, bus, decode8(y, pre), ea, v);
        }
        // The immediate loads with the destination in the y field.
        0x06 | 0x0E | 0x16 | 0x1E | 0x26 | 0x2E | 0x36 | 0x3E => {
            let n = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            if pre != Index::None && y == 6 {
                // The indexed immediate store pays its two T-states
                // before the store.
                bus.wait(2);
            }
            set8(cpu, bus, decode8(y, pre), ea, n);
        }
        // INC r (4) and INC (HL) (11).
        0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x3C => {
            let slot = decode8(y, pre);
            let v = get8(cpu, bus, slot, ea);
            let r = inc8(cpu, v);
            set8(cpu, bus, slot, ea, r);
        }
        0x34 => {
            let v = bus.rd(ea);
            bus.wait(1);
            let r = inc8(cpu, v);
            bus.wr(ea, r);
        }
        // DEC r (4) and DEC (HL) (11).
        0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x3D => {
            let slot = decode8(y, pre);
            let v = get8(cpu, bus, slot, ea);
            let r = dec8(cpu, v);
            set8(cpu, bus, slot, ea, r);
        }
        0x35 => {
            let v = bus.rd(ea);
            bus.wait(1);
            let r = dec8(cpu, v);
            bus.wr(ea, r);
        }
        // The ALU rows: 4 T-states on registers and immediate, 7 on
        // (HL).
        0x80..=0xBF => {
            let b = get8(cpu, bus, decode8(z, pre), ea);
            alu8(cpu, y, b);
        }
        0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
            let n = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            alu8(cpu, (op >> 3) & 7, n);
        }
        // The accumulator rotations (4 T-states).
        0x07 | 0x0F | 0x17 | 0x1F => rotate_a(cpu, (op >> 3) & 3),
        _ => {}
    }
}

/// The CB table header: fetch the operation byte, then run it.
fn exec_cb<C: Cycles>(cpu: &mut Z80, bus: &mut C) {
    let pc = cpu.regs.pc;
    let op = bus.m1(pc);
    cpu.r_bump();
    cpu.regs.pc = pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(u32::from(pc))));
    cb_byte(cpu, bus, op);
}

/// One CB operation byte over the HL slot: the shift forms at 8/15
/// T-states, the bit tests at 8/12, and the RES/SET writes of the
/// upper half.
fn cb_byte<C: Cycles>(cpu: &mut Z80, bus: &mut C, op: u8) {
    let n = (op >> 3) & 7;
    let z = op & 7;
    let ea = cpu_hl(cpu);
    if op < 0x40 {
        // A shift form.
        let v = get8(cpu, bus, decode8(z, Index::None), ea);
        if z == 6 {
            bus.wait(1);
        }
        if n == 6 && !cpu.undocumented {
            // The SLL form without the quirk flag runs effect-free
            // with its timing.
            return;
        }
        let r = cb_shift(cpu, n, v);
        set8(cpu, bus, decode8(z, Index::None), ea, r);
    } else if op < 0x80 {
        // A bit test; the register form copies the operand bits, the
        // memory form the address high byte.
        let v = get8(cpu, bus, decode8(z, Index::None), ea);
        if z == 6 {
            bus.wait(1);
            bit(cpu, n, v, (ea >> 8) as u8);
        } else {
            bit(cpu, n, v, v);
        }
    } else if z == 6 {
        // RES/SET of a memory byte.
        let v = bus.rd(ea);
        bus.wait(1);
        bus.wr(ea, set_bit(v, n, op >= 0xC0));
    } else {
        // RES/SET of a register.
        let slot = decode8(z, Index::None);
        let v = get8(cpu, bus, slot, ea);
        set8(cpu, bus, slot, ea, set_bit(v, n, op >= 0xC0));
    }
}

/// The index-prefixed forms: fetch the second opcode, remap, and
/// read the displacement where a memory slot appears.
fn exec_indexed<C: Cycles>(cpu: &mut Z80, bus: &mut C, pre: Index) {
    let pc = cpu.regs.pc;
    let op2 = bus.m1(pc);
    cpu.r_bump();
    cpu.regs.pc = pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(u32::from(pc))));
    if op2 == 0xCB {
        exec_indexed_cb(cpu, bus, pre);
        return;
    }
    if op2 == 0xDD || op2 == 0xFD {
        // The last prefix wins.
        exec_indexed(cpu, bus, if op2 == 0xDD { Index::X } else { Index::Y });
        return;
    }
    if op2 == 0xED {
        exec_ed(cpu, bus);
        return;
    }
    if op2 == 0x76 {
        // HALT under an index prefix reads no displacement.
        cpu.halted = true;
        return;
    }
    let y = (op2 >> 3) & 7;
    let z = op2 & 7;
    let disp = match op2 {
        0x34..=0x36 => true,
        0x40..=0x7F => z == 6 || (op2 >= 0x70 && y == 6),
        0x80..=0xBF => z == 6,
        _ => false,
    };
    let base = match pre {
        Index::X => cpu.regs.ix,
        _ => cpu.regs.iy,
    };
    if !disp {
        exec_group(cpu, bus, op2, pre, base);
        return;
    }
    let d = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    let ea = base.wrapping_add(d as i8 as i16 as u16);
    // The published 5 T-states of the address add sit between the
    // displacement read and the operand access; the immediate store
    // pays its 2 inside the store form.
    if op2 != 0x36 {
        bus.wait(5);
    }
    exec_group(cpu, bus, op2, pre, ea);
}

/// The DD CB and FD CB forms: the displacement, a dummy read of the
/// located byte, the operand read, and the shift or bit result. The
/// shift forms write back for 23 T-states; the bit tests run a third
/// read for 20.
fn exec_indexed_cb<C: Cycles>(cpu: &mut Z80, bus: &mut C, pre: Index) {
    let d = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    let pc = cpu.regs.pc;
    let op2 = bus.m1(pc);
    cpu.r_bump();
    cpu.regs.pc = pc.wrapping_add(1);
    let base = match pre {
        Index::X => cpu.regs.ix,
        _ => cpu.regs.iy,
    };
    let ea = base.wrapping_add(d as i8 as i16 as u16);
    let n = (op2 >> 3) & 7;
    let z = op2 & 7;
    // The located byte reads once; the published totals come from
    // the three fetches, the displacement, the read, and the two
    // closing T-states (plus the write of a shift or a RES/SET of
    // memory).
    let v = bus.rd(ea);
    let result = if op2 < 0x40 {
        // A shift form.
        if n == 6 && !cpu.undocumented {
            None
        } else {
            Some(cb_shift(cpu, n, v))
        }
    } else if op2 < 0x80 {
        // A bit test: the copy flags take the address high byte.
        bit(cpu, n, v, (ea >> 8) as u8);
        None
    } else if z == 6 {
        // RES/SET of the located byte.
        Some(set_bit(v, n, op2 >= 0xC0))
    } else if cpu.undocumented {
        // The undocumented register RES/SET through the index
        // prefix.
        let slot = decode8(z, Index::None);
        let cur = get8(cpu, bus, slot, ea);
        set8(cpu, bus, slot, ea, set_bit(cur, n, op2 >= 0xC0));
        None
    } else {
        None
    };
    if let Some(r) = result {
        bus.wr(ea, r);
    }
    bus.wait(2);
}

/// The ED table.
fn exec_ed<C: Cycles>(cpu: &mut Z80, bus: &mut C) {
    let pc = cpu.regs.pc;
    let op = bus.m1(pc);
    cpu.r_bump();
    cpu.regs.pc = pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(u32::from(pc))));
    let y = (op >> 3) & 7;
    match op {
        // IN r,(C) (12 T-states) with the undocumented index forms.
        0x40 | 0x48 | 0x50 | 0x58 | 0x60 | 0x68 | 0x78 => {
            let gated = cpu.undocumented || !matches!(op, 0x60 | 0x68);
            let v = bus.rd_io(u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c));
            let f = &mut cpu.regs.f;
            *f &= !(FLAG_S | FLAG_Z | FLAG_P | FLAG_H | FLAG_N | FLAG_X3 | FLAG_X5);
            if v & 0x80 != 0 {
                *f |= FLAG_S;
            }
            if v == 0 {
                *f |= FLAG_Z;
            }
            if parity(v) {
                *f |= FLAG_P;
            }
            if v & 0x08 != 0 {
                *f |= FLAG_X3;
            }
            if v & 0x20 != 0 {
                *f |= FLAG_X5;
            }
            if gated {
                set8(cpu, bus, decode8(y, Index::None), 0, v);
            }
        }
        // IN (C): the undocumented flag-only form. The port read and
        // the flag copies run only with the quirk; without it the
        // form falls to the unknown-form fallback.
        0x70 if cpu.undocumented => {
            let v = bus.rd_io(u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c));
            let f = &mut cpu.regs.f;
            *f &= !(FLAG_S | FLAG_Z | FLAG_P | FLAG_H | FLAG_N | FLAG_X3 | FLAG_X5);
            if v & 0x80 != 0 {
                *f |= FLAG_S;
            }
            if v == 0 {
                *f |= FLAG_Z;
            }
            if parity(v) {
                *f |= FLAG_P;
            }
            if v & 0x08 != 0 {
                *f |= FLAG_X3;
            }
            if v & 0x20 != 0 {
                *f |= FLAG_X5;
            }
        }
        // OUT (C),r (12 T-states); ED 71 stores zero (the
        // undocumented form).
        0x41 | 0x49 | 0x51 | 0x59 | 0x61 | 0x69 | 0x79 => {
            let gated = cpu.undocumented || !matches!(op, 0x61 | 0x69);
            let v = get8(cpu, bus, decode8(y, Index::None), 0);
            if gated {
                bus.wr_io(u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c), v);
            }
        }
        0x71 => {
            // The undocumented zero store runs only with the quirk.
            if cpu.undocumented {
                bus.wr_io(u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c), 0);
            }
        }
        // SBC HL,ss and ADC HL,ss (15 T-states).
        0x42 | 0x52 | 0x62 | 0x72 | 0x4A | 0x5A | 0x6A | 0x7A => {
            bus.wait(7);
            let a = cpu_hl(cpu);
            let b = pair_of((op >> 4) & 3, Index::None, cpu);
            let subtract = matches!(op, 0x42 | 0x52 | 0x62 | 0x72);
            adc_sbc16(cpu, a, b, subtract);
        }
        // LD dd,(nn) (20 T-states).
        0x4B | 0x5B | 0x6B | 0x7B => {
            let a = fetch16(cpu, bus);
            let lo = bus.rd(a);
            let hi = bus.rd(a.wrapping_add(1));
            set_pair(
                cpu,
                (op >> 4) & 3,
                Index::None,
                u16::from(lo) | (u16::from(hi) << 8),
            );
        }
        // LD (nn),dd (20 T-states).
        0x43 | 0x53 | 0x63 | 0x73 => {
            let a = fetch16(cpu, bus);
            let v = pair_of((op >> 4) & 3, Index::None, cpu);
            bus.wr(a, v as u8);
            bus.wr(a.wrapping_add(1), (v >> 8) as u8);
        }
        // NEG (8 T-states: the two fetches alone).
        0x44 | 0x4C | 0x54 | 0x5C | 0x64 | 0x6C | 0x74 | 0x7C => neg(cpu),
        // RETN and RETI (14 T-states): the return pops and the
        // shadow interrupt flip-flop hands back.
        0x45 | 0x4D | 0x55 | 0x5D | 0x65 | 0x6D | 0x75 | 0x7D => {
            cpu.iff1 = cpu.iff2;
            cpu.regs.pc = pop2(cpu, bus);
        }
        // IM 0/1/2 over the published row (8 T-states).
        0x46 | 0x4E | 0x56 | 0x5E | 0x66 | 0x6E | 0x76 | 0x7E => {
            cpu.im = match (op >> 3) & 7 {
                0 | 1 | 4 => 0,
                2 | 5 => 1,
                _ => 2,
            };
        }
        // LD I,A (9), LD R,A (9), LD A,I (9), and LD A,R (9).
        0x47 | 0x4F | 0x57 | 0x5F => {
            bus.wait(1);
            match op {
                0x47 => cpu.regs.i = cpu.regs.a,
                0x4F => cpu.regs.r = cpu.regs.a & 0x7F | cpu.regs.r & 0x80,
                0x57 => load_i_or_r(cpu, cpu.regs.i),
                _ => load_i_or_r(cpu, cpu.regs.r),
            }
        }
        // The block forms; the repeats refetch once per iteration.
        0xA0..=0xB3 | 0xB8..=0xBB => {
            let repeats = matches!(op, 0xB0..=0xB3 | 0xB8..=0xBB);
            block(cpu, bus, op, repeats);
        }
        // RRD and RLD (18 T-states): the nibbles rotate between the
        // accumulator's low nibble and the two nibbles of (HL).
        0x67 | 0x6F => {
            let ea = cpu_hl(cpu);
            let v = bus.rd(ea);
            bus.wait(4);
            let a_low = cpu.regs.a & 0x0F;
            let v = if op == 0x67 {
                // RRD: the low nibble of (HL) moves to A, the low
                // nibble of A moves to the high nibble, and the high
                // nibble of (HL) moves to the low nibble.
                cpu.regs.a = cpu.regs.a & 0xF0 | (v & 0x0F);
                (a_low << 4) | (v >> 4)
            } else {
                // RLD: the high nibble of (HL) moves to A, the low
                // nibble of A moves to the low nibble, and the low
                // nibble of (HL) moves to the high nibble.
                cpu.regs.a = cpu.regs.a & 0xF0 | (v >> 4);
                ((v & 0x0F) << 4) | a_low
            };
            bus.wr(ea, v);
            result_flags(cpu, cpu.regs.a);
            cpu.regs.f &= !(FLAG_H | FLAG_N | FLAG_X3 | FLAG_X5);
        }
        // The unknown ED forms run as 8 T-state no-ops (the
        // published undefined behavior of an unknown prefix byte).
        _ => {}
    }
}

/// The flags of the LD A,I and LD A,R forms: sign, zero, and parity
/// from the byte, the overflow from IFF2, and the half carry and
/// subtract flags clear.
fn load_i_or_r(cpu: &mut Z80, v: u8) {
    cpu.regs.a = v;
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_S | FLAG_Z | FLAG_P | FLAG_H | FLAG_N | FLAG_X3 | FLAG_X5);
    if v & 0x80 != 0 {
        *f |= FLAG_S;
    }
    if v == 0 {
        *f |= FLAG_Z;
    }
    if cpu.iff2 {
        *f |= FLAG_P;
    }
}

/// One block form of the ED A0-BB range: kind = op & 3 selects the
/// load, compare, input, and output forms; the D bit selects the
/// decrement directions; the repeating forms refetch the opcode pair
/// once per iteration.
fn block<C: Cycles>(cpu: &mut Z80, bus: &mut C, op: u8, repeats: bool) {
    let kind = op & 3;
    let dec = op & 0x08 != 0;
    loop {
        let hl = cpu_hl(cpu);
        let de = u16::from(cpu.regs.d) << 8 | u16::from(cpu.regs.e);
        let port = u16::from(cpu.regs.b) << 8 | u16::from(cpu.regs.c);
        match kind & 3 {
            0 => {
                // LDI/LDD
                let v = bus.rd(hl);
                bus.wr(de, v);
                step_hl(cpu, dec);
                step_de(cpu, dec);
                dec_bc(cpu);
                let f = &mut cpu.regs.f;
                *f &= !(FLAG_H | FLAG_N | FLAG_P | FLAG_X3 | FLAG_X5);
                if cpu.regs.b != 0 || cpu.regs.c != 0 {
                    *f |= FLAG_P;
                }
            }
            1 => {
                // CPI/CPD: the compare flags with the carry preserved
                // and the half carry of the subtraction; the
                // DE pointer stays.
                let carry = cpu.regs.f & FLAG_C;
                let v = bus.rd(hl);
                alu8(cpu, 7, v);
                cpu.regs.f = cpu.regs.f & !FLAG_C | carry;
                step_hl(cpu, dec);
                dec_bc(cpu);
                let f = &mut cpu.regs.f;
                *f &= !FLAG_P;
                *f &= !(FLAG_X3 | FLAG_X5);
                if cpu.regs.b != 0 || cpu.regs.c != 0 {
                    *f |= FLAG_P;
                }
            }
            2 => {
                // INI/IND: the port read lands at (HL); the DE
                // pointer stays.
                let v = bus.rd_io(port);
                bus.wr(hl, v);
                step_hl(cpu, dec);
                dec_bc(cpu);
                io_block_flags(cpu, v);
            }
            _ => {
                // OUTI/OUTD: the (HL) byte drops to the port.
                let v = bus.rd(hl);
                bus.wr_io(port, v);
                step_hl(cpu, dec);
                dec_bc(cpu);
                io_block_flags(cpu, v);
            }
        }
        if repeats && repeat_gate(cpu, kind) {
            // The refetch rides the published 21 T-states: 8 for the
            // opcode pair and the form's repeat tail after the
            // accesses.
            bus.m1(cpu.regs.pc.wrapping_sub(2));
            cpu.r_bump();
            bus.m1(cpu.regs.pc.wrapping_sub(1));
            cpu.r_bump();
            bus.wait(BLOCK_REPEAT_TAIL[(kind & 3) as usize]);
            continue;
        }
        bus.wait(BLOCK_TAIL[(kind & 3) as usize]);
        break;
    }
}

/// The published closing steps of the block forms by kind: the load
/// forms close with 2 T-states, the compare forms with 5, and the
/// input and output forms with 1.
const BLOCK_TAIL: [u32; 4] = [2, 5, 1, 1];

/// The closing steps of a repeated block iteration that continues:
/// the load forms 7, the compare forms 10, and the input and output
/// forms 6.
const BLOCK_REPEAT_TAIL: [u32; 4] = [7, 10, 6, 6];

/// The repeat gate: the load and compare forms repeat while the BC
/// pair reads nonzero; the input and output forms repeat while B
/// alone reads nonzero.
fn repeat_gate(cpu: &Z80, kind: u8) -> bool {
    if kind & 3 >= 2 {
        cpu.regs.b != 0
    } else {
        cpu.regs.b != 0 || cpu.regs.c != 0
    }
}

/// The half-carry and zero flags of the INI/IND/OUTI/OUTD forms: the
/// half carry rides the published sum of the data byte, the L
/// register, and one.
fn io_block_flags(cpu: &mut Z80, v: u8) {
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_H | FLAG_Z | FLAG_X3 | FLAG_X5);
    *f |= FLAG_N;
    if cpu.regs.b == 0 {
        *f |= FLAG_Z;
    }
    if u16::from(v) + u16::from(cpu.regs.l) + 1 > 0xFF {
        *f |= FLAG_H;
    }
}

/// Step the HL pointer of a block form.
fn step_hl(cpu: &mut Z80, dec: bool) {
    let hl = cpu_hl(cpu);
    let next = if dec {
        hl.wrapping_sub(1)
    } else {
        hl.wrapping_add(1)
    };
    cpu.regs.h = (next >> 8) as u8;
    cpu.regs.l = next as u8;
}

/// Step the DE pointer of a block form; the decrement forms of the
/// load row step down with the source pointer.
fn step_de(cpu: &mut Z80, dec: bool) {
    let de = u16::from(cpu.regs.d) << 8 | u16::from(cpu.regs.e);
    let next = if dec {
        de.wrapping_sub(1)
    } else {
        de.wrapping_add(1)
    };
    cpu.regs.d = (next >> 8) as u8;
    cpu.regs.e = next as u8;
}

/// Decrement the BC pair of a block form: the low byte rolls into
/// the high byte.
fn dec_bc(cpu: &mut Z80) {
    cpu.regs.c = cpu.regs.c.wrapping_sub(1);
    if cpu.regs.c == 0xFF {
        cpu.regs.b = cpu.regs.b.wrapping_sub(1);
    }
}

/// The two's-complement negate (NEG): A = 0 - A.
fn neg(cpu: &mut Z80) {
    let a = cpu.regs.a;
    let v = a.wrapping_neg();
    let f = &mut cpu.regs.f;
    *f &= !(FLAG_C | FLAG_H | FLAG_P | FLAG_X3 | FLAG_X5);
    *f |= FLAG_N;
    if a != 0 {
        *f |= FLAG_C;
    }
    if a & 0x0F != 0 {
        *f |= FLAG_H;
    }
    result_flags(cpu, v);
    if a == 0x80 {
        cpu.regs.f |= FLAG_P;
    }
    cpu.regs.a = v;
}
