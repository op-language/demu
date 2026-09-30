//! The SM83 instruction executor.
//!
//! One call executes one instruction at its start tick. The executor
//! walks the instruction's M-cycles in order; each bus access stamps its
//! exact tick, four dots per M-cycle. The core turns the returned
//! M-cycle count into the next visit tick.

use crate::cpu::{Bus, Sm83};
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;

/// Flag bits of the `F` register.
pub const F_Z: u8 = 0x80;
pub const F_N: u8 = 0x40;
pub const F_H: u8 = 0x20;
pub const F_C: u8 = 0x10;

/// Execute one instruction at `tick`.
pub fn step(cpu: &mut Sm83, tick: Tick, ctx: &mut demu_core::device::MachineCtx) {
    // The interrupt dispatch replaces an instruction when IME allows it,
    // except on the visit right after EI.
    if cpu.suppress_dispatch {
        cpu.suppress_dispatch = false;
    } else if let Some(bits) = cpu.pending_interrupt()
        && cpu.ime
    {
        let m = dispatch_interrupt(cpu, tick, ctx, bits);
        cpu.schedule_after(tick, m);
        return;
    }
    let mut bus = Bus::new(ctx, tick, cpu.bus_name.clone(), cpu.ticks_per_t);
    let op_addr = cpu.regs.pc;
    let op = cpu.read8(cpu.regs.pc, &mut bus);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(demu_core::Address(u32::from(op_addr))));
    let m = execute(cpu, op, &mut bus);
    cpu.schedule_after(tick, m);
}

/// Dispatch one interrupt: five M-cycles.
///
/// Two wait states run first, the `IF` bit clears as the return address
/// pushes, and the last M-cycle sets the handler vector.
fn dispatch_interrupt(
    cpu: &mut Sm83,
    tick: Tick,
    ctx: &mut demu_core::device::MachineCtx,
    bits: u8,
) -> u32 {
    let mut bus = Bus::new(ctx, tick, cpu.bus_name.clone(), cpu.ticks_per_t);
    let bit = bits.trailing_zeros() as u8;
    let vector = 0x40u16 + 8 * u16::from(bit);
    bus.wait();
    bus.wait();
    // M3: acknowledge the interrupt and push the high byte of PC.
    cpu.iflags &= !(1 << bit);
    cpu.ime = false;
    let pc = cpu.regs.pc;
    let sp = cpu.regs.sp.wrapping_sub(1);
    cpu.regs.sp = sp;
    bus.wr(sp, (pc >> 8) as u8);
    let sp = cpu.regs.sp.wrapping_sub(1);
    cpu.regs.sp = sp;
    bus.wr(sp, pc as u8);
    cpu.regs.pc = vector;
    bus.wait();
    5
}

/// Execute one opcode, returning its total M-cycle count including the
/// opcode fetch.
fn execute(cpu: &mut Sm83, op: u8, bus: &mut Bus) -> u32 {
    match op {
        0x00 => 1, // NOP
        0x01 => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            cpu.regs.b = hi;
            cpu.regs.c = lo;
            3
        }
        0x02 => {
            let addr = pair(cpu.regs.b, cpu.regs.c);
            cpu.write8(addr, cpu.regs.a, bus);
            2
        }
        0x03 => {
            let addr = pair(cpu.regs.b, cpu.regs.c);
            cpu.idu_op(addr, bus);
            set_bc(cpu, addr.wrapping_add(1));
            2
        }
        0x04 => {
            inc8(cpu, |r| &mut r.b);
            1
        }
        0x05 => {
            dec8(cpu, |r| &mut r.b);
            1
        }
        0x06 => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.b = v;
            2
        }
        0x07 => {
            let a = cpu.regs.a;
            cpu.regs.a = a.rotate_left(1);
            cpu.regs.f = 0;
            if a & 0x80 != 0 {
                cpu.regs.f |= F_C;
            }
            1
        }
        0x08 => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            let addr = pair(hi, lo);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            let sp = cpu.regs.sp;
            bus.wr(addr, sp as u8);
            bus.wr(addr.wrapping_add(1), (sp >> 8) as u8);
            5
        }
        0x09 => {
            let d = pair(cpu.regs.b, cpu.regs.c);
            add_hl(cpu, d);
            2
        }
        0x0A => {
            let addr = pair(cpu.regs.b, cpu.regs.c);
            cpu.regs.a = cpu.read8(addr, bus);
            2
        }
        0x0B => {
            let addr = pair(cpu.regs.b, cpu.regs.c);
            cpu.idu_op(addr, bus);
            set_bc(cpu, addr.wrapping_sub(1));
            2
        }
        0x0C => {
            inc8(cpu, |r| &mut r.c);
            1
        }
        0x0D => {
            dec8(cpu, |r| &mut r.c);
            1
        }
        0x0E => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.c = v;
            2
        }
        0x0F => {
            let a = cpu.regs.a;
            cpu.regs.a = a.rotate_right(1);
            cpu.regs.f = 0;
            if a & 1 == 1 {
                cpu.regs.f |= F_C;
            }
            1
        }
        0x10 => {
            stop(cpu, bus);
            2
        }
        0x11 => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            cpu.regs.d = hi;
            cpu.regs.e = lo;
            3
        }
        0x12 => {
            let addr = pair(cpu.regs.d, cpu.regs.e);
            cpu.write8(addr, cpu.regs.a, bus);
            2
        }
        0x13 => {
            let addr = pair(cpu.regs.d, cpu.regs.e);
            cpu.idu_op(addr, bus);
            set_de(cpu, addr.wrapping_add(1));
            2
        }
        0x14 => {
            inc8(cpu, |r| &mut r.d);
            1
        }
        0x15 => {
            dec8(cpu, |r| &mut r.d);
            1
        }
        0x16 => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.d = v;
            2
        }
        0x17 => {
            let carry = cpu.regs.f & F_C;
            let a = cpu.regs.a;
            cpu.regs.a = (a << 1) | (carry >> 4);
            cpu.regs.f = 0;
            if a & 0x80 != 0 {
                cpu.regs.f |= F_C;
            }
            1
        }
        0x18 => {
            let e8 = bus.rd(cpu.regs.pc) as i8;
            bus.wait();
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1).wrapping_add(e8 as u16);
            3
        }
        0x19 => {
            let d = pair(cpu.regs.d, cpu.regs.e);
            add_hl(cpu, d);
            2
        }
        0x1A => {
            let addr = pair(cpu.regs.d, cpu.regs.e);
            cpu.regs.a = cpu.read8(addr, bus);
            2
        }
        0x1B => {
            let addr = pair(cpu.regs.d, cpu.regs.e);
            cpu.idu_op(addr, bus);
            set_de(cpu, addr.wrapping_sub(1));
            2
        }
        0x1C => {
            inc8(cpu, |r| &mut r.e);
            1
        }
        0x1D => {
            dec8(cpu, |r| &mut r.e);
            1
        }
        0x1E => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.e = v;
            2
        }
        0x1F => {
            let carry = cpu.regs.f & F_C;
            let a = cpu.regs.a;
            cpu.regs.a = (a >> 1) | (carry << 3);
            cpu.regs.f = 0;
            if a & 1 == 1 {
                cpu.regs.f |= F_C;
            }
            1
        }
        0x20 | 0x28 | 0x30 | 0x38 => jr_cc(cpu, op, bus),
        0x21 => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            cpu.regs.h = hi;
            cpu.regs.l = lo;
            3
        }
        0x22 => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.write8(addr, cpu.regs.a, bus);
            set_hl(cpu, addr.wrapping_add(1));
            cpu.idu_op(addr, bus);
            2
        }
        0x23 => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.idu_op(addr, bus);
            set_hl(cpu, addr.wrapping_add(1));
            2
        }
        0x24 => {
            inc8(cpu, |r| &mut r.h);
            1
        }
        0x25 => {
            dec8(cpu, |r| &mut r.h);
            1
        }
        0x26 => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.h = v;
            2
        }
        0x27 => {
            daa(cpu);
            1
        }
        0x29 => {
            let d = pair(cpu.regs.h, cpu.regs.l);
            add_hl(cpu, d);
            2
        }
        0x2A => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.regs.a = cpu.read8(addr, bus);
            set_hl(cpu, addr.wrapping_add(1));
            cpu.idu_op(addr, bus);
            2
        }
        0x2B => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.idu_op(addr, bus);
            set_hl(cpu, addr.wrapping_sub(1));
            2
        }
        0x2C => {
            inc8(cpu, |r| &mut r.l);
            1
        }
        0x2D => {
            dec8(cpu, |r| &mut r.l);
            1
        }
        0x2E => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.l = v;
            2
        }
        0x2F => {
            cpu.regs.a = !cpu.regs.a;
            cpu.regs.f |= F_N | F_H;
            1
        }
        0x31 => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            cpu.regs.sp = pair(hi, lo);
            3
        }
        0x32 => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.write8(addr, cpu.regs.a, bus);
            set_hl(cpu, addr.wrapping_sub(1));
            cpu.idu_op(addr, bus);
            2
        }
        0x33 => {
            let sp = cpu.regs.sp;
            cpu.idu_op(sp, bus);
            cpu.regs.sp = sp.wrapping_add(1);
            2
        }
        0x34 => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            let v = cpu.read8(addr, bus);
            bus.wait();
            let r = v.wrapping_add(1);
            cpu.regs.f &= !(F_Z | F_N | F_H);
            if r == 0 {
                cpu.regs.f |= F_Z;
            }
            if (v & 0x0F) == 0x0F {
                cpu.regs.f |= F_H;
            }
            cpu.write8(addr, r, bus);
            3
        }
        0x35 => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            let v = cpu.read8(addr, bus);
            bus.wait();
            let r = v.wrapping_sub(1);
            cpu.regs.f &= !(F_Z | F_N | F_H);
            if r == 0 {
                cpu.regs.f |= F_Z;
            }
            cpu.regs.f |= F_N;
            if (v & 0x0F) == 0 {
                cpu.regs.f |= F_H;
            }
            cpu.write8(addr, r, bus);
            3
        }
        0x36 => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            let addr = pair(cpu.regs.h, cpu.regs.l);
            bus.wait();
            cpu.write8(addr, v, bus);
            3
        }
        0x37 => {
            cpu.regs.f &= !(F_N | F_H);
            cpu.regs.f |= F_C;
            1
        }
        0x39 => {
            let d = cpu.regs.sp;
            add_hl(cpu, d);
            2
        }
        0x3A => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.regs.a = cpu.read8(addr, bus);
            set_hl(cpu, addr.wrapping_sub(1));
            cpu.idu_op(addr, bus);
            2
        }
        0x3B => {
            let sp = cpu.regs.sp;
            cpu.idu_op(sp, bus);
            cpu.regs.sp = sp.wrapping_sub(1);
            2
        }
        0x3C => {
            inc8(cpu, |r| &mut r.a);
            1
        }
        0x3D => {
            dec8(cpu, |r| &mut r.a);
            1
        }
        0x3E => {
            let v = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            cpu.regs.a = v;
            2
        }
        0x3F => {
            let carry = cpu.regs.f & F_C;
            cpu.regs.f &= !(F_N | F_H | F_C);
            if carry == 0 {
                cpu.regs.f |= F_C;
            }
            1
        }
        0x40..=0x7F => {
            if op == 0x76 {
                return halt(cpu);
            }
            let src = op & 7;
            let dst = (op >> 3) & 7;
            let value = read_r(cpu, src, bus);
            write_r(cpu, dst, value, bus);
            if src == 6 || dst == 6 { 2 } else { 1 }
        }
        0x80..=0xBF => {
            let kind = (op >> 3) & 7;
            let src = op & 7;
            let v = read_r(cpu, src, bus);
            match kind {
                0 => add_a(cpu, v),
                1 => adc_a(cpu, v),
                2 => sub_a(cpu, v),
                3 => sbc_a(cpu, v),
                4 => and_a(cpu, v),
                5 => xor_a(cpu, v),
                6 => or_a(cpu, v),
                _ => cp_a(cpu, v),
            }
            if src == 6 { 2 } else { 1 }
        }
        0xC0 | 0xC8 | 0xD0 | 0xD8 => ret_cc(cpu, op, bus),
        0xC1 | 0xD1 | 0xE1 | 0xF1 => {
            let (hi, lo) = pop16(cpu, bus);
            match op {
                0xC1 => {
                    cpu.regs.b = hi;
                    cpu.regs.c = lo;
                }
                0xD1 => {
                    cpu.regs.d = hi;
                    cpu.regs.e = lo;
                }
                0xE1 => {
                    cpu.regs.h = hi;
                    cpu.regs.l = lo;
                }
                _ => {
                    cpu.regs.a = hi;
                    cpu.regs.f = lo & 0xF0;
                }
            }
            3
        }
        0xC2 | 0xCA | 0xD2 | 0xDA => jp_cc(cpu, op, bus),
        0xC3 => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            bus.wait();
            cpu.regs.pc = pair(hi, lo);
            4
        }
        0xC4 | 0xCC | 0xD4 | 0xDC | 0xCD => call_cc(cpu, op, bus),
        0xC5 | 0xD5 | 0xE5 | 0xF5 => {
            let (hi, lo) = match op {
                0xC5 => (cpu.regs.b, cpu.regs.c),
                0xD5 => (cpu.regs.d, cpu.regs.e),
                0xE5 => (cpu.regs.h, cpu.regs.l),
                _ => (cpu.regs.a, cpu.regs.f),
            };
            bus.wait();
            let sp = cpu.regs.sp.wrapping_sub(1);
            cpu.regs.sp = sp;
            bus.wr(sp, hi);
            let sp = cpu.regs.sp.wrapping_sub(1);
            cpu.regs.sp = sp;
            bus.wr(sp, lo);
            4
        }
        0xC6 => alu_imm(cpu, 0, bus),
        0xCE => alu_imm(cpu, 1, bus),
        0xD6 => alu_imm(cpu, 2, bus),
        0xDE => alu_imm(cpu, 3, bus),
        0xE6 => alu_imm(cpu, 4, bus),
        0xEE => alu_imm(cpu, 5, bus),
        0xF6 => alu_imm(cpu, 6, bus),
        0xFE => alu_imm(cpu, 7, bus),
        0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
            let vector = u16::from(op & 0x38);
            let pc = cpu.regs.pc;
            bus.wait();
            let sp = cpu.regs.sp.wrapping_sub(1);
            cpu.regs.sp = sp;
            bus.wr(sp, (pc >> 8) as u8);
            let sp = cpu.regs.sp.wrapping_sub(1);
            cpu.regs.sp = sp;
            bus.wr(sp, pc as u8);
            cpu.regs.pc = vector;
            4
        }
        0xC9 => {
            let (hi, lo) = pop16(cpu, bus);
            bus.wait();
            cpu.regs.pc = pair(hi, lo);
            4
        }
        0xD9 => {
            let (hi, lo) = pop16(cpu, bus);
            bus.wait();
            cpu.regs.pc = pair(hi, lo);
            cpu.ime = true;
            4
        }
        0xE0 => {
            let n = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            bus.wait();
            cpu.write8(0xFF00 + u16::from(n), cpu.regs.a, bus);
            3
        }
        0xE2 => {
            bus.wait();
            cpu.write8(0xFF00 + u16::from(cpu.regs.c), cpu.regs.a, bus);
            2
        }
        0xE8 => {
            let raw = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            bus.wait();
            bus.wait();
            add_sp_e8(cpu, raw);
            4
        }
        0xE9 => {
            cpu.regs.pc = pair(cpu.regs.h, cpu.regs.l);
            1
        }
        0xEA => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            cpu.write8(pair(hi, lo), cpu.regs.a, bus);
            4
        }
        0xF0 => {
            let n = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            bus.wait();
            cpu.regs.a = cpu.read8(0xFF00 + u16::from(n), bus);
            3
        }
        0xF2 => {
            bus.wait();
            cpu.regs.a = cpu.read8(0xFF00 + u16::from(cpu.regs.c), bus);
            2
        }
        0xF3 => {
            cpu.ime = false;
            cpu.ime_pending = false;
            1
        }
        0xFB => {
            cpu.ime_pending = true;
            1
        }
        0xF8 => {
            let raw = bus.rd(cpu.regs.pc);
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
            let sp = cpu.regs.sp;
            let result = sp.wrapping_add(i8::from_be_bytes([raw]) as u16);
            cpu.regs.f = 0;
            let sum = u32::from(sp & 0xFF) + u32::from(raw);
            if sum > 0xF {
                cpu.regs.f |= F_H;
            }
            if sum > 0xFF {
                cpu.regs.f |= F_C;
            }
            set_hl(cpu, result);
            3
        }
        0xF9 => {
            bus.wait();
            cpu.regs.sp = pair(cpu.regs.h, cpu.regs.l);
            2
        }
        0xFA => {
            let lo = bus.rd(cpu.regs.pc);
            let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
            cpu.regs.a = cpu.read8(pair(hi, lo), bus);
            4
        }
        0xCB => cb(cpu, bus),
        // The invalid opcodes lock the core; the core idles from here.
        0xD3 | 0xDB | 0xDD | 0xE3 | 0xE4 | 0xEB | 0xEC | 0xED | 0xF4 | 0xFC | 0xFD => {
            cpu.enter_halt();
            1
        }
    }
}

/// The STOP instruction.
///
/// With the CGB speed switch armed, the switch runs and the CPU pauses
/// for the switch duration. Without it, the CPU enters stop mode until
/// an interrupt arrives. The divider chain freezes for both cases, and
/// the `stop` line tells the timer.
fn stop(cpu: &mut Sm83, bus: &mut Bus) {
    let key1 = cpu.read8(0xFF4D, bus);
    if key1 & 0x01 != 0 {
        let old = cpu.ticks_per_t;
        cpu.ticks_per_t = if old == 1 { 2 } else { 1 };
        bus.notify("speed", Level::High, 1);
        bus.notify("stop", Level::High, 1);
        cpu.enter_stop(8200, bus.tick());
        cpu.arm_resume_notify();
    } else {
        bus.notify("stop", Level::High, 1);
        cpu.enter_stop(0, bus.tick());
        cpu.arm_resume_notify();
    }
}

/// The HALT instruction.
///
/// With a pending interrupt and IME clear, the halt bug applies: the
/// program counter does not advance past the halt byte.
fn halt(cpu: &mut Sm83) -> u32 {
    if cpu.halt_bug && cpu.pending_interrupt().is_some() && !cpu.ime {
        // The halt bug: the program counter fails its increment and the
        // halt byte refetches; the core does not enter halt mode.
        cpu.regs.pc = cpu.regs.pc.wrapping_sub(1);
        return 1;
    }
    cpu.enter_halt();
    1
}

/// One conditional jump relative.
fn jr_cc(cpu: &mut Sm83, op: u8, bus: &mut Bus) -> u32 {
    let e8 = bus.rd(cpu.regs.pc) as i8;
    let taken = match op {
        0x20 => cpu.regs.f & F_Z == 0,
        0x28 => cpu.regs.f & F_Z != 0,
        0x30 => cpu.regs.f & F_C == 0,
        _ => cpu.regs.f & F_C != 0,
    };
    if taken {
        bus.wait();
        cpu.regs.pc = cpu.regs.pc.wrapping_add(1).wrapping_add(e8 as u16);
        3
    } else {
        cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
        2
    }
}

/// One conditional return.
fn ret_cc(cpu: &mut Sm83, op: u8, bus: &mut Bus) -> u32 {
    let taken = match op {
        0xC0 => cpu.regs.f & F_Z == 0,
        0xC8 => cpu.regs.f & F_Z != 0,
        0xD0 => cpu.regs.f & F_C == 0,
        _ => cpu.regs.f & F_C != 0,
    };
    if taken {
        let (hi, lo) = pop16(cpu, bus);
        bus.wait();
        cpu.regs.pc = pair(hi, lo);
        5
    } else {
        bus.wait();
        2
    }
}

/// One conditional jump.
fn jp_cc(cpu: &mut Sm83, op: u8, bus: &mut Bus) -> u32 {
    let lo = bus.rd(cpu.regs.pc);
    let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
    let taken = match op {
        0xC2 => cpu.regs.f & F_Z == 0,
        _ => cpu.regs.f & F_Z != 0,
    };
    if taken {
        bus.wait();
        cpu.regs.pc = pair(hi, lo);
        4
    } else {
        cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
        3
    }
}

/// One conditional call.
fn call_cc(cpu: &mut Sm83, op: u8, bus: &mut Bus) -> u32 {
    let lo = bus.rd(cpu.regs.pc);
    let hi = bus.rd(cpu.regs.pc.wrapping_add(1));
    let taken = match op {
        0xC4 => cpu.regs.f & F_Z == 0,
        0xCC => cpu.regs.f & F_Z != 0,
        0xD4 => cpu.regs.f & F_C == 0,
        _ => cpu.regs.f & F_C != 0,
    };
    if taken {
        bus.wait();
        let pc = cpu.regs.pc.wrapping_add(2);
        let sp = cpu.regs.sp.wrapping_sub(1);
        cpu.regs.sp = sp;
        bus.wr(sp, (pc >> 8) as u8);
        let sp = cpu.regs.sp.wrapping_sub(1);
        cpu.regs.sp = sp;
        bus.wr(sp, pc as u8);
        cpu.regs.pc = pair(hi, lo);
        6
    } else {
        cpu.regs.pc = cpu.regs.pc.wrapping_add(2);
        3
    }
}

/// Pop two bytes from the stack.
fn pop16(cpu: &mut Sm83, bus: &mut Bus) -> (u8, u8) {
    let lo = bus.rd(cpu.regs.sp);
    let hi = bus.rd(cpu.regs.sp.wrapping_add(1));
    cpu.regs.sp = cpu.regs.sp.wrapping_add(2);
    (hi, lo)
}

/// The immediate-form ALU group.
fn alu_imm(cpu: &mut Sm83, kind: u8, bus: &mut Bus) -> u32 {
    let v = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    match kind {
        0 => add_a(cpu, v),
        1 => adc_a(cpu, v),
        2 => sub_a(cpu, v),
        3 => sbc_a(cpu, v),
        4 => and_a(cpu, v),
        5 => xor_a(cpu, v),
        6 => or_a(cpu, v),
        _ => cp_a(cpu, v),
    }
    2
}

/// The `CB` prefix table.
fn cb(cpu: &mut Sm83, bus: &mut Bus) -> u32 {
    let op = bus.rd(cpu.regs.pc);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    let idx = op & 7;
    let bit = (op >> 3) & 7;
    let prefix = op >> 6;
    match prefix {
        0 => {
            // RLC, RRC, RL, RR, SLA, SRA, SWAP, SRL.
            let value = read_r(cpu, idx, bus);
            let (result, shifted_out) = match bit {
                0 => (value.rotate_left(1), value & 0x80 != 0),
                1 => (value.rotate_right(1), value & 1 != 0),
                2 => {
                    let carry = (cpu.regs.f & F_C) >> 4;
                    ((value << 1) | carry, value & 0x80 != 0)
                }
                3 => {
                    let carry = (cpu.regs.f & F_C) >> 4;
                    ((value >> 1) | (carry << 7), value & 1 != 0)
                }
                4 => ((value << 1), value & 0x80 != 0),
                5 => ((value >> 1) | (value & 0x80), value & 1 != 0),
                6 => (value.rotate_right(4), false),
                _ => ((value >> 1) | (value & 0x80), value & 1 != 0),
            };
            let _ = shifted_out;
            if bit == 6 {
                // SWAP does not touch carry.
                cpu.regs.f = if result == 0 { F_Z } else { 0 };
            } else {
                cpu.regs.f = if result == 0 { F_Z } else { 0 };
                if shifted_out {
                    cpu.regs.f |= F_C;
                }
            }
            write_r(cpu, idx, result, bus);
            if idx == 6 { 4 } else { 2 }
        }
        1 => {
            // BIT: the value read decides Z; H sets and N clears.
            let value = read_r(cpu, idx, bus);
            if value & (1 << u32::from(bit)) == 0 {
                cpu.regs.f |= F_Z | F_H;
            } else {
                cpu.regs.f &= !F_Z;
                cpu.regs.f |= F_H;
            }
            cpu.regs.f &= !F_N;
            if idx == 6 { 3 } else { 2 }
        }
        2 => {
            // RES
            let value = read_r(cpu, idx, bus);
            write_r(cpu, idx, value & !(1u8 << bit), bus);
            if idx == 6 { 4 } else { 2 }
        }
        _ => {
            // SET
            let value = read_r(cpu, idx, bus);
            write_r(cpu, idx, value | (1u8 << bit), bus);
            if idx == 6 { 4 } else { 2 }
        }
    }
}

/// The 8-bit increment with its flags.
fn inc8(cpu: &mut Sm83, f: impl Fn(&mut crate::cpu::Regs) -> &mut u8) {
    let v = *f(&mut cpu.regs);
    let r = v.wrapping_add(1);
    *f(&mut cpu.regs) = r;
    cpu.regs.f &= !(F_Z | F_N | F_H);
    if r == 0 {
        cpu.regs.f |= F_Z;
    }
    if (v & 0x0F) == 0x0F {
        cpu.regs.f |= F_H;
    }
}

/// The 8-bit decrement with its flags.
fn dec8(cpu: &mut Sm83, f: impl Fn(&mut crate::cpu::Regs) -> &mut u8) {
    let v = *f(&mut cpu.regs);
    let r = v.wrapping_sub(1);
    *f(&mut cpu.regs) = r;
    cpu.regs.f &= !(F_Z | F_H);
    cpu.regs.f |= F_N;
    if r == 0 {
        cpu.regs.f |= F_Z;
    }
    if (v & 0x0F) == 0 {
        cpu.regs.f |= F_H;
    }
}

/// Add a 16-bit value to `HL` with its flags.
fn add_hl(cpu: &mut Sm83, d: u16) {
    let hl = pair(cpu.regs.h, cpu.regs.l);
    let r = hl.wrapping_add(d);
    cpu.regs.f &= !F_Z;
    cpu.regs.f &= !(F_N | F_H | F_C);
    if (hl & 0x0FFF) + (d & 0x0FFF) > 0x0FFF {
        cpu.regs.f |= F_H;
    }
    if u32::from(hl) + u32::from(d) > 0xFFFF {
        cpu.regs.f |= F_C;
    }
    set_hl(cpu, r);
}

/// `ADD SP, e8`: the result lands in `SP`; the flags read the unsigned
/// low bytes and `Z` and `N` clear.
fn add_sp_e8(cpu: &mut Sm83, raw: u8) {
    let sp = cpu.regs.sp;
    let e8 = i8::from_be_bytes([raw]);
    cpu.regs.sp = sp.wrapping_add(e8 as u16);
    cpu.regs.f = 0;
    let sum = u32::from(sp & 0xFF) + u32::from(raw);
    if sum > 0xF {
        cpu.regs.f |= F_H;
    }
    if sum > 0xFF {
        cpu.regs.f |= F_C;
    }
}

/// The 8-bit add family.
fn add_a(cpu: &mut Sm83, v: u8) {
    let a = cpu.regs.a;
    let r = a.wrapping_add(v);
    cpu.regs.f = 0;
    if r == 0 {
        cpu.regs.f |= F_Z;
    }
    if (u32::from(a & 0x0F) + u32::from(v & 0x0F)) > 0xF {
        cpu.regs.f |= F_H;
    }
    if u32::from(a) + u32::from(v) > 0xFF {
        cpu.regs.f |= F_C;
    }
    cpu.regs.a = r;
}

/// The add-with-carry family.
fn adc_a(cpu: &mut Sm83, v: u8) {
    let a = cpu.regs.a;
    let c = (cpu.regs.f & F_C) >> 4;
    let r = a.wrapping_add(v).wrapping_add(c);
    cpu.regs.f = 0;
    if r == 0 {
        cpu.regs.f |= F_Z;
    }
    if (u32::from(a & 0x0F) + u32::from(v & 0x0F) + u32::from(c)) > 0xF {
        cpu.regs.f |= F_H;
    }
    if u32::from(a) + u32::from(v) + u32::from(c) > 0xFF {
        cpu.regs.f |= F_C;
    }
    cpu.regs.a = r;
}

/// The subtract family.
fn sub_a(cpu: &mut Sm83, v: u8) {
    let a = cpu.regs.a;
    let r = a.wrapping_sub(v);
    cpu.regs.f = F_N;
    if r == 0 {
        cpu.regs.f |= F_Z;
    }
    if (a & 0x0F) < (v & 0x0F) {
        cpu.regs.f |= F_H;
    }
    if a < v {
        cpu.regs.f |= F_C;
    }
    cpu.regs.a = r;
}

/// The subtract-with-carry family.
fn sbc_a(cpu: &mut Sm83, v: u8) {
    let a = cpu.regs.a;
    let c = (cpu.regs.f & F_C) >> 4;
    let r = a.wrapping_sub(v).wrapping_sub(c);
    cpu.regs.f = F_N;
    if r == 0 {
        cpu.regs.f |= F_Z;
    }
    if (a & 0x0F) < (v & 0x0F) + c {
        cpu.regs.f |= F_H;
    }
    if u32::from(a) < u32::from(v) + u32::from(c) {
        cpu.regs.f |= F_C;
    }
    cpu.regs.a = r;
}

/// The logic-AND family.
fn and_a(cpu: &mut Sm83, v: u8) {
    cpu.regs.a &= v;
    cpu.regs.f = F_H;
    if cpu.regs.a == 0 {
        cpu.regs.f |= F_Z;
    }
}

/// The logic-XOR family.
fn xor_a(cpu: &mut Sm83, v: u8) {
    cpu.regs.a ^= v;
    cpu.regs.f = if cpu.regs.a == 0 { F_Z } else { 0 };
}

/// The logic-OR family.
fn or_a(cpu: &mut Sm83, v: u8) {
    cpu.regs.a |= v;
    cpu.regs.f = if cpu.regs.a == 0 { F_Z } else { 0 };
}

/// The compare family: `A` is unchanged.
fn cp_a(cpu: &mut Sm83, v: u8) {
    let a = cpu.regs.a;
    let r = a.wrapping_sub(v);
    cpu.regs.f = F_N;
    if r == 0 {
        cpu.regs.f |= F_Z;
    }
    if (a & 0x0F) < (v & 0x0F) {
        cpu.regs.f |= F_H;
    }
    if a < v {
        cpu.regs.f |= F_C;
    }
}

/// The decimal adjust of the accumulator.
fn daa(cpu: &mut Sm83) {
    let mut a = cpu.regs.a;
    let mut f = cpu.regs.f;
    if f & F_N == 0 {
        if f & F_H != 0 || (a & 0x0F) > 9 {
            a = a.wrapping_add(0x06);
        }
        if f & F_C != 0 || a > 0x9F {
            a = a.wrapping_add(0x60);
            f |= F_C;
        } else {
            f &= !F_C;
        }
    } else {
        if f & F_H != 0 {
            a = a.wrapping_sub(0x06);
        }
        if f & F_C != 0 {
            a = a.wrapping_sub(0x60);
        }
    }
    f &= !(F_H | F_Z);
    if a == 0 {
        f |= F_Z;
    }
    cpu.regs.a = a;
    cpu.regs.f = f;
}

/// The 8-bit register value by index; index 6 reads `(HL)`.
fn read_r(cpu: &mut Sm83, idx: u8, bus: &mut Bus) -> u8 {
    match idx {
        0 => cpu.regs.b,
        1 => cpu.regs.c,
        2 => cpu.regs.d,
        3 => cpu.regs.e,
        4 => cpu.regs.h,
        5 => cpu.regs.l,
        6 => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.read8(addr, bus)
        }
        _ => cpu.regs.a,
    }
}

/// Write the 8-bit register by index.
fn write_r(cpu: &mut Sm83, idx: u8, value: u8, bus: &mut Bus) {
    match idx {
        0 => cpu.regs.b = value,
        1 => cpu.regs.c = value,
        2 => cpu.regs.d = value,
        3 => cpu.regs.e = value,
        4 => cpu.regs.h = value,
        5 => cpu.regs.l = value,
        6 => {
            let addr = pair(cpu.regs.h, cpu.regs.l);
            cpu.write8(addr, value, bus);
        }
        _ => cpu.regs.a = value,
    }
}

/// Join two bytes into a big-endian 16-bit value.
fn pair(hi: u8, lo: u8) -> u16 {
    (u16::from(hi) << 8) | u16::from(lo)
}

fn set_bc(cpu: &mut Sm83, v: u16) {
    cpu.regs.b = (v >> 8) as u8;
    cpu.regs.c = v as u8;
}

fn set_de(cpu: &mut Sm83, v: u16) {
    cpu.regs.d = (v >> 8) as u8;
    cpu.regs.e = v as u8;
}

fn set_hl(cpu: &mut Sm83, v: u16) {
    cpu.regs.h = (v >> 8) as u8;
    cpu.regs.l = v as u8;
}
