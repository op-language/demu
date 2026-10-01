//! The instruction set: one instruction per core visit.
//!
//! Every memory access carries its exact domain tick, one CPU cycle
//! apart. The cycle totals follow the bus accesses plus the internal
//! waits, which reproduces the published instruction table: the
//! immediate reads 2 cycles (plus one when a 16-bit form reads two
//! bytes), the absolute read 4, the direct page read 3, the
//! read-modify-write adds its modify cycle, the indexed loads pay an
//! address fix-up read when a page boundary crosses or the index
//! register is 16 bits while indexed stores pay it always, and a
//! taken branch adds one cycle, with one more when an emulation-mode
//! branch crosses a page.
//!
//! Fixed conventions, from the W65C816S data sheet:
//!
//! - The direct page lives in bank zero. In emulation mode with an
//!   aligned direct register the direct-page arithmetic wraps within
//!   page zero (the 6502 form) and the 16-bit pointer walks wrap in
//!   page zero; native mode performs the free 16-bit sums with the
//!   pointer walk linear. The 24-pointer walks of `[dp]`, `[dp],Y`,
//!   and PEI increment linearly in both modes.
//! - The jump-indirect pointer of `JMP (abs)` walks linearly through
//!   bank zero (the 6502 page wrap is fixed); the pointer of the
//!   indexed indirect jump and call reads in the program bank.
//! - 16-bit pushes write the high byte first and the low byte
//!   second; 16-bit pulls read the low byte first. The two- and
//!   three-byte stack forms of the long calls, the effective-address
//!   pushes, and the returns step the raw 16-bit stack pointer even
//!   in emulation mode, restoring the page-one high byte afterwards.
//! - The interrupt vectors read from bank zero in both modes and set
//!   the program bank register to zero, clear the decimal flag, and
//!   set the interrupt disable.
//! - The block move spends seven cycles per byte: read, write, and
//!   five arithmetic cycles. The core re-enters the instruction
//!   (`PC - 3`) until the count register wraps, so the move stays
//!   interruptible exactly as the hardware does.

use demu_core::Address;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::triggers::TriggerEvent;

use crate::cpu::{
    Cycles, FLAG_C, FLAG_D, FLAG_I, FLAG_M, FLAG_N, FLAG_V, FLAG_X, FLAG_Z, Wdc65c816,
};

/// The reset vector offset in the zero bank.
const VEC_RESET: u16 = 0xFFFC;

/// One interrupt source: the vector offset follows the operating
/// mode. Emulation mode shares one vector for the hardware IRQ and
/// the software break; the pushed status byte's break bit
/// discriminates the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VectorSource {
    /// The level-sensitive hardware interrupt.
    Irq,
    /// The edge-latched hardware interrupt.
    Nmi,
    /// The software break of the emulation-mode shared vector.
    Brk,
    /// The coprocessor interrupt.
    Cop,
}

impl VectorSource {
    /// The vector offset in the zero bank.
    fn offset(self, e: bool) -> u16 {
        match self {
            // Emulation mode: COP $FFF4, NMI $FFFA, the shared BRK and
            // IRQ $FFFE; native mode: COP $FFE4, BRK $FFE6, NMI $FFEA,
            // IRQ $FFEE.
            Self::Cop => {
                if e {
                    0xFFF4
                } else {
                    0xFFE4
                }
            }
            Self::Brk => {
                if e {
                    0xFFFE
                } else {
                    0xFFE6
                }
            }
            Self::Nmi => {
                if e {
                    0xFFFA
                } else {
                    0xFFEA
                }
            }
            Self::Irq => {
                if e {
                    0xFFFE
                } else {
                    0xFFEE
                }
            }
        }
    }
}

/// One addressing mode: the access pattern of the operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The immediate operand bytes at the register width.
    Imm,
    /// Direct page.
    Dp,
    /// Direct page with the X index.
    DpX,
    /// Direct page with the Y index.
    DpY,
    /// Absolute in the data bank.
    Abs,
    /// Absolute with the X index.
    AbsX,
    /// Absolute with the Y index.
    AbsY,
    /// Direct page indirect with the X index.
    DpIx,
    /// Direct page indirect.
    Dpi,
    /// Direct page indirect with the Y index.
    DpiY,
    /// Direct page indirect long: a 24-bit pointer.
    DpLong,
    /// The 24-bit pointer with the Y index.
    DpLongY,
    /// Absolute long: the operand carries its own bank byte.
    Long,
    /// Absolute long with the X index.
    LongX,
    /// Stack relative.
    Src,
    /// Stack relative indirect with the Y index.
    SrcY,
}

/// The addressing mode of one opcode of the ALU groups.
///
/// The direct, indirect, immediate, absolute, and absolute-indexed
/// forms of the 6502 encoding stay in place; the 65816 builds the
/// stack-relative, long, and long-indexed forms from the opcodes the
/// NMOS 6502 left unused.
fn alu_mode(lo5: u8) -> Option<Mode> {
    let mode = match lo5 {
        0x01 => Mode::DpIx,
        0x03 => Mode::Src,
        0x05 => Mode::Dp,
        0x07 => Mode::DpLong,
        0x09 => Mode::Imm,
        0x0D => Mode::Abs,
        0x0F => Mode::Long,
        0x11 => Mode::DpiY,
        0x12 => Mode::Dpi,
        0x13 => Mode::SrcY,
        0x15 => Mode::DpX,
        0x17 => Mode::DpLongY,
        0x19 => Mode::AbsY,
        0x1D => Mode::AbsX,
        0x1F => Mode::LongX,
        _ => return None,
    };
    Some(mode)
}

/// Step one instruction at the machine level: builds the bus access
/// over the machine context and runs the instruction dispatch.
pub fn step(cpu: &mut Wdc65c816, tick: Tick, ctx: &mut MachineCtx) {
    let mut access = crate::cpu::Access::new(ctx, tick, cpu.bus_name.clone(), cpu.cpu_div);
    step_with(cpu, tick, &mut access);
}

/// One step of the core over any bus backend.
///
/// A pending reset sequence runs first, then a pending interrupt,
/// otherwise the instruction at the program counter. The core
/// reschedules itself to the end of the instruction. A `WAI` or `STP`
/// core keeps no scheduled visit; the wake happens on the lines.
pub fn step_with<B: Cycles>(cpu: &mut Wdc65c816, tick: Tick, bus: &mut B) {
    if cpu.reset_pending {
        reset_sequence(cpu, bus);
        cpu.reset_pending = false;
        let cycles = bus.cycles();
        cpu.schedule_after(tick, cycles);
        return;
    }
    if cpu.interrupt_pending() {
        let source = if cpu.take_nmi() {
            VectorSource::Nmi
        } else {
            VectorSource::Irq
        };
        service_interrupt(cpu, bus, source);
        let cycles = bus.cycles();
        cpu.schedule_after(tick, cycles);
        return;
    }
    let start = compose(cpu.regs.pbr, cpu.regs.pc);
    let opcode = bus.rd(start);
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    bus.emit(TriggerEvent::Fetch(Address(start)));
    execute(cpu, bus, opcode);
    let cycles = bus.cycles();
    if !cpu.stopped && !cpu.waiting {
        cpu.schedule_after(tick, cycles);
    }
}

/// Compose a 24-bit bus address from a bank byte and a 16-bit offset.
#[inline]
fn compose(bank: u8, offset: u16) -> u32 {
    (u32::from(bank) << 16) | u32::from(offset)
}

/// The program space address of the next operand byte.
#[inline]
fn prog_addr(cpu: &Wdc65c816, offset: u16) -> u32 {
    compose(cpu.regs.pbr, offset)
}

/// The high-byte position of a 16-bit data access; the offset wraps
/// within the bank and the bank byte stays.
#[inline]
fn bank_next(addr: u32) -> u32 {
    (addr & 0xFFFF_0000) | ((addr + 1) & 0x0000_FFFF)
}

/// One program byte: read the operand at the program counter.
fn program_byte<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B) -> u8 {
    let value = bus.rd(prog_addr(cpu, cpu.regs.pc));
    cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
    value
}

/// One 16-bit program operand in low-then-high order.
fn operand_word<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B) -> u16 {
    let lo = u16::from(program_byte(cpu, bus));
    let hi = u16::from(program_byte(cpu, bus));
    lo | (hi << 8)
}

/// One 24-bit absolute long program operand: low, high, bank.
fn operand_long<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B) -> (u8, u16) {
    let lo = u16::from(program_byte(cpu, bus));
    let hi = u16::from(program_byte(cpu, bus));
    let bank = program_byte(cpu, bus);
    (bank, lo | (hi << 8))
}

/// The immediate operand at the current width: one or two bytes.
fn immediate<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, wide: bool) -> u16 {
    let lo = u16::from(program_byte(cpu, bus));
    if wide {
        let hi = u16::from(program_byte(cpu, bus));
        lo | (hi << 8)
    } else {
        lo
    }
}

/// Skip one internal cycle.
#[inline]
fn bus_wait<B: Cycles>(bus: &mut B) {
    bus.wait();
}

/// The direct-page offset of a form with an optional index.
///
/// In emulation mode with an aligned direct register and the 8-bit
/// index form, the arithmetic wraps within page zero (the 6502 form);
/// otherwise it is the free 16-bit sum of the direct register, the
/// operand byte, and the index register.
fn dp_offset(cpu: &Wdc65c816, base: u8, index: u16) -> u16 {
    let aligned_wrap = cpu.regs.e && cpu.regs.d & 0x00FF == 0 && index <= 0x00FF;
    if aligned_wrap {
        (cpu.regs.d & 0xFF00) | u16::from(base.wrapping_add(index as u8))
    } else {
        cpu.regs.d.wrapping_add(u16::from(base)).wrapping_add(index)
    }
}

/// The direct-page access penalty: one instruction cycle when the low
/// byte of the direct-page register is not zero.
#[inline]
fn dp_penalty<B: Cycles>(cpu: &Wdc65c816, bus: &mut B) {
    if cpu.regs.d & 0x00FF != 0 {
        bus.wait();
    }
}

/// The stack-space address of an offset from the stack pointer: a
/// free 16-bit sum within the 64K of bank zero.
#[inline]
fn stack_addr(cpu: &Wdc65c816, offset: u16) -> u16 {
    cpu.regs.s.wrapping_add(offset)
}

/// The 6502-style page-zero wrap of the direct-page walk: only the
/// aligned emulation-mode form wraps; everything else walks linearly.
#[inline]
fn dp_wrapped(cpu: &Wdc65c816) -> bool {
    cpu.regs.e && cpu.regs.d & 0x00FF == 0
}

/// The 16-bit pointer of a direct-page indirect form: the pointer
/// bytes walk the direct page in bank zero. In emulation mode with an
/// aligned direct register the walk wraps within page zero (the 6502
/// form); otherwise, and for the 24-bit long-pointer forms, the bytes
/// read linearly.
fn dp_pointer<B: Cycles>(bus: &mut B, off: u16, linear: bool) -> u16 {
    let lo = u16::from(bus.rd(u32::from(off)));
    let ahead = if linear {
        off.wrapping_add(1)
    } else {
        off & 0xFF00 | off.wrapping_add(1) & 0x00FF
    };
    let hi = u16::from(bus.rd(u32::from(ahead)));
    lo | (hi << 8)
}

/// The 24-bit pointer of a direct-page indirect long form; the three
/// bytes always walk linearly through the direct page.
fn dp_pointer_long<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, base: u8) -> u32 {
    let off = dp_offset(cpu, base, 0);
    let lo = u16::from(bus.rd(u32::from(off)));
    let hi = u16::from(bus.rd(u32::from(off.wrapping_add(1))));
    let bank = bus.rd(u32::from(off.wrapping_add(2)));
    compose(bank, lo | (hi << 8))
}

/// Read one byte or word at a resolved address, low byte first.
fn read_at<B: Cycles>(bus: &mut B, addr: u32, wide: bool) -> u16 {
    let lo = u16::from(bus.rd(addr));
    if wide {
        let hi = u16::from(bus.rd(bank_next(addr)));
        lo | (hi << 8)
    } else {
        lo
    }
}

/// Write one byte or word at a resolved address, low byte first.
fn write_at<B: Cycles>(bus: &mut B, addr: u32, value: u16, wide: bool) {
    bus.wr(addr, (value & 0xFF) as u8);
    if wide {
        bus.wr(bank_next(addr), (value >> 8) as u8);
    }
}

/// Read one operand through the mode at the given width.
fn read_m<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, mode: Mode, wide: bool) -> u16 {
    if mode == Mode::Imm {
        return immediate(cpu, bus, wide);
    }
    let addr = operand_addr(cpu, bus, mode, false);
    read_at(bus, addr, wide)
}

/// Write one operand through the store shape of its mode, at the
/// given width.
fn store_m<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, mode: Mode, value: u16, wide: bool) {
    let addr = operand_addr(cpu, bus, mode, true);
    write_at(bus, addr, value, wide);
}

/// The address of one operand, with the exact access sequence of the
/// mode. The `store` shape selects the fix-up cycles of the store
/// forms: indexed stores pay their fix-up read always; indexed loads
/// pay it when the index register is 16 bits or the effective page
/// differs from the operand page.
fn operand_addr<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, mode: Mode, store: bool) -> u32 {
    let index_wide = cpu.x_wide();
    let index_x = if index_wide {
        cpu.regs.x
    } else {
        u16::from(cpu.regs.x as u8)
    };
    let index_y = if index_wide {
        cpu.regs.y
    } else {
        u16::from(cpu.regs.y as u8)
    };
    match mode {
        // The immediate form reads through the operand helpers.
        Mode::Imm => 0,
        Mode::Dp => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            u32::from(dp_offset(cpu, z, 0))
        }
        Mode::DpX | Mode::DpY => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            // The index add costs one internal cycle.
            bus.wait();
            let index = if mode == Mode::DpX { index_x } else { index_y };
            u32::from(dp_offset(cpu, z, index))
        }
        Mode::Abs => {
            let base = operand_word(cpu, bus);
            compose(cpu.regs.dbr, base)
        }
        Mode::AbsX | Mode::AbsY => {
            let base = operand_word(cpu, bus);
            let index = if mode == Mode::AbsX { index_x } else { index_y };
            let sum = base.wrapping_add(index);
            let fixup = store || index_wide || (sum & 0xFF00) != (base & 0xFF00);
            if fixup {
                bus.rd(compose(cpu.regs.dbr, (base & 0xFF00) | (sum & 0x00FF)));
            }
            compose(cpu.regs.dbr, sum)
        }
        Mode::DpIx => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            // The index add costs one internal cycle.
            bus.wait();
            let addr = dp_offset(cpu, z, index_x);
            // The pointer walk of (dp,X) follows the same wrap rule as
            // (dp): the aligned emulation form wraps, everything else
            // walks linearly.
            let linear = !dp_wrapped(cpu);
            let ptr = dp_pointer(bus, addr, linear);
            compose(cpu.regs.dbr, ptr)
        }
        Mode::Dpi => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            let addr = dp_offset(cpu, z, 0);
            let ptr = dp_pointer(bus, addr, !dp_wrapped(cpu));
            compose(cpu.regs.dbr, ptr)
        }
        Mode::DpiY => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            let addr = dp_offset(cpu, z, 0);
            let ptr = dp_pointer(bus, addr, !dp_wrapped(cpu));
            let sum = ptr.wrapping_add(index_y);
            let fixup = store || index_wide || (sum & 0xFF00) != (ptr & 0xFF00);
            if fixup {
                bus.rd(compose(cpu.regs.dbr, (ptr & 0xFF00) | (sum & 0x00FF)));
            }
            compose(cpu.regs.dbr, sum)
        }
        Mode::DpLong => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            dp_pointer_long(cpu, bus, z)
        }
        Mode::DpLongY => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            let long = dp_pointer_long(cpu, bus, z);
            let bank = (long >> 16) as u8;
            let sum = (long as u16).wrapping_add(index_y);
            compose(bank, sum)
        }
        Mode::Long | Mode::LongX => {
            let (bank, base) = operand_long(cpu, bus);
            let index = if mode == Mode::LongX { index_x } else { 0 };
            // The long-indexed sum carries into the bank byte.
            let sum = compose(bank, base).wrapping_add(u32::from(index));
            compose((sum >> 16) as u8, sum as u16)
        }
        Mode::Src => {
            let z = program_byte(cpu, bus);
            // The stack arithmetic costs one cycle.
            bus.wait();
            u32::from(stack_addr(cpu, u16::from(z)))
        }
        Mode::SrcY => {
            let z = program_byte(cpu, bus);
            // The stack arithmetic costs one cycle.
            bus.wait();
            let base = stack_addr(cpu, u16::from(z));
            let lo = u16::from(bus.rd(u32::from(base)));
            let hi = u16::from(bus.rd(u32::from(base.wrapping_add(1))));
            let ptr = lo | (hi << 8);
            let sum = ptr.wrapping_add(index_y);
            // The stack-relative indexed form always spends its
            // index-adder read, as the published cycle count does.
            bus.rd(compose(cpu.regs.dbr, (ptr & 0xFF00) | (sum & 0x00FF)));
            compose(cpu.regs.dbr, sum)
        }
    }
}

/// The ALU opcode groups: the eight operation rows over the full
/// addressing table. The store row is the accumulator store; the
/// immediate column of the store row carries the bit test instead.
fn execute_alu<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, row: u8, mode: Mode) {
    let wide = cpu.m_wide();
    let mask = if wide { 0xFFFF } else { 0x00FF };
    let visible = cpu.regs.c & mask;
    match row {
        0 => {
            let m = read_m(cpu, bus, mode, wide) & mask;
            let a = visible | m;
            cpu.regs.c = (cpu.regs.c & !mask) | a;
            set_nz_w(cpu, a, wide);
        }
        1 => {
            let m = read_m(cpu, bus, mode, wide) & mask;
            let a = visible & m;
            cpu.regs.c = (cpu.regs.c & !mask) | a;
            set_nz_w(cpu, a, wide);
        }
        2 => {
            let m = read_m(cpu, bus, mode, wide) & mask;
            let a = visible ^ m;
            cpu.regs.c = (cpu.regs.c & !mask) | a;
            set_nz_w(cpu, a, wide);
        }
        3 => {
            let m = read_m(cpu, bus, mode, wide) & mask;
            adc_w(cpu, m, wide);
        }
        4 => {
            store_m(cpu, bus, mode, visible, wide);
        }
        5 => {
            let m = read_m(cpu, bus, mode, wide) & mask;
            cpu.regs.c = (cpu.regs.c & !mask) | m;
            set_nz_w(cpu, m, wide);
        }
        6 => {
            let m = read_m(cpu, bus, mode, wide) & mask;
            compare_w(cpu, visible, m, wide);
        }
        _ => {
            let m = read_m(cpu, bus, mode, wide) & mask;
            sbc_w(cpu, m, wide);
        }
    }
}

/// The read-modify-write mode of one opcode of the shift and count
/// classes. Only the direct, direct-with-X, absolute, and
/// absolute-with-X forms occur there.
fn rmw_mode(op: u8) -> Mode {
    match (op >> 2) & 0x07 {
        1 => Mode::Dp,
        5 => Mode::DpX,
        3 => Mode::Abs,
        _ => Mode::AbsX,
    }
}

/// The read-modify-write shape: read the operand at the given width,
/// spend the modify cycle, and write the result back, low byte first.
fn rmw_w<B: Cycles>(
    cpu: &mut Wdc65c816,
    bus: &mut B,
    mode: Mode,
    wide: bool,
    apply: impl FnOnce(&mut Wdc65c816, u16) -> u16,
) {
    let addr = operand_addr(cpu, bus, mode, true);
    let value = read_at(bus, addr, wide);
    let result = apply(cpu, value & if wide { 0xFFFF } else { 0x00FF });
    bus_wait(bus);
    write_at(bus, addr, result, wide);
}

/// The memory shifts: one kind per call, the same shape.
fn rmw_shift<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, mode: Mode, wide: bool, kind: u8) {
    rmw_w(cpu, bus, mode, wide, move |c, v| match kind {
        0 => asl_w(c, v, wide),
        1 => rol_w(c, v, wide),
        2 => lsr_w(c, v, wide),
        _ => ror_w(c, v, wide),
    })
}

/// One executed instruction. The cycle count accumulates on the bus
/// access helper; every arm reads or writes in instruction order.
/// The dispatch and the arms together cover every byte of the table.
fn execute<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, opcode: u8) {
    // The immediate bit-test form shares the immediate column with
    // the ALU class of the store row, and affects only the zero flag.
    if opcode == 0x89 {
        let wide = cpu.m_wide();
        let m = immediate(cpu, bus, wide) & cpu.m_mask();
        set_zero(
            cpu,
            (cpu.regs.c & if wide { 0xFFFF } else { 0x00FF }) & m == 0,
        );
        return;
    }
    // The ALU groups: ORA, AND, EOR, ADC, STA, LDA, CMP, and SBC over
    // the fifteen addressed forms each.
    if let Some(mode) = alu_mode(opcode & 0x1F) {
        execute_alu(cpu, bus, opcode >> 5, mode);
        return;
    }
    let wide = cpu.m_wide();
    let mask = if wide { 0xFFFF } else { 0x00FF };
    let index_wide = cpu.x_wide();
    let x_mask = if index_wide { 0xFFFF } else { 0x00FF };
    match opcode {
        // The software interrupts and the returns.
        0x00 => {
            program_byte(cpu, bus);
            service_interrupt(cpu, bus, VectorSource::Brk);
        }
        0x02 => {
            program_byte(cpu, bus);
            service_interrupt(cpu, bus, VectorSource::Cop);
        }
        0x40 => {
            bus_wait(bus);
            bus_wait(bus);
            let value = pull8(cpu, bus);
            cpu.pull_p(value);
            let lo = u16::from(pull8(cpu, bus));
            let hi = u16::from(pull8(cpu, bus));
            cpu.regs.pc = (hi << 8) | lo;
            if !cpu.regs.e {
                // The native return pulls the program bank too.
                cpu.regs.pbr = pull8(cpu, bus);
            }
        }
        0x60 => {
            bus_wait(bus);
            bus_wait(bus);
            let lo = u16::from(pull8(cpu, bus));
            let hi = u16::from(pull8(cpu, bus));
            cpu.regs.pc = (hi << 8) | lo;
            bus.rd(prog_addr(cpu, cpu.regs.pc));
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
        }
        0x6B => {
            bus_wait(bus);
            bus_wait(bus);
            // The return long always pulls its bank byte over the raw
            // stack pointer.
            let lo = u16::from(pull_raw(cpu, bus));
            let hi = u16::from(pull_raw(cpu, bus));
            let bank = pull_raw(cpu, bus);
            restore_stack_high(cpu);
            cpu.regs.pc = (hi << 8) | lo;
            if !cpu.regs.e {
                cpu.regs.pbr = bank;
            }
            cpu.regs.pc = cpu.regs.pc.wrapping_add(1);
        }
        // The jumps.
        0x4C => {
            let target = operand_word(cpu, bus);
            cpu.regs.pc = target;
        }
        0x6C => {
            let base = operand_word(cpu, bus);
            // The indirect jump pointer walks linearly through bank
            // zero; the page wrap of the 6502 is fixed.
            let lo = u16::from(bus.rd(u32::from(base)));
            let hi = u16::from(bus.rd(u32::from(base.wrapping_add(1))));
            cpu.regs.pc = lo | (hi << 8);
        }
        0x7C => {
            let base = operand_word(cpu, bus);
            let index = if index_wide {
                cpu.regs.x
            } else {
                u16::from(cpu.regs.x as u8)
            };
            let sum = base.wrapping_add(index);
            // The indexed indirect jump always spends its fix-up
            // read; the pointer address sits in the program bank.
            bus.rd(prog_addr(cpu, (base & 0xFF00) | (sum & 0x00FF)));
            let lo = u16::from(bus.rd(prog_addr(cpu, sum)));
            let hi = u16::from(bus.rd(prog_addr(cpu, sum.wrapping_add(1))));
            cpu.regs.pc = lo | (hi << 8);
        }
        0x5C => {
            let (bank, base) = operand_long(cpu, bus);
            cpu.regs.pbr = bank;
            cpu.regs.pc = base;
        }
        0xDC => {
            let base = operand_word(cpu, bus);
            // The indirect long jump reads its 24-bit pointer from
            // bank zero, linearly.
            let lo = u16::from(bus.rd(u32::from(base)));
            let hi = u16::from(bus.rd(u32::from(base.wrapping_add(1))));
            let bank = bus.rd(u32::from(base.wrapping_add(2)));
            cpu.regs.pbr = bank;
            cpu.regs.pc = lo | (hi << 8);
        }
        0x20 => {
            let lo = program_byte(cpu, bus);
            let hi = program_byte(cpu, bus);
            // The pushed return address names the last operand byte;
            // the idle cycle reads the stack without use.
            let return_pc = cpu.regs.pc.wrapping_sub(1);
            stack_read_self(cpu, bus);
            push16(cpu, bus, return_pc);
            cpu.regs.pc = (u16::from(hi) << 8) | u16::from(lo);
        }
        0xFC => {
            let lo = program_byte(cpu, bus);
            // The return pushes happen before the second operand byte;
            // the pushed address names the high operand byte.
            let return_pc = cpu.regs.pc;
            push_raw(cpu, bus, (return_pc >> 8) as u8);
            push_raw(cpu, bus, (return_pc & 0xFF) as u8);
            restore_stack_high(cpu);
            let hi = program_byte(cpu, bus);
            let base = u16::from(lo) | (u16::from(hi) << 8);
            bus_wait(bus);
            let index = if index_wide {
                cpu.regs.x
            } else {
                u16::from(cpu.regs.x as u8)
            };
            let sum = base.wrapping_add(index);
            let ptr_lo = u16::from(bus.rd(prog_addr(cpu, sum)));
            let ptr_hi = u16::from(bus.rd(prog_addr(cpu, sum.wrapping_add(1))));
            cpu.regs.pc = ptr_lo | (ptr_hi << 8);
        }
        0x22 => {
            let lo = program_byte(cpu, bus);
            let hi = program_byte(cpu, bus);
            // The long call steps the raw stack pointer through its
            // three pushes: the bank byte before the last operand
            // read, the return address after it.
            push_raw(cpu, bus, cpu.regs.pbr);
            bus_wait(bus);
            let bank = program_byte(cpu, bus);
            let return_pc = cpu.regs.pc.wrapping_sub(1);
            push_raw(cpu, bus, (return_pc >> 8) as u8);
            push_raw(cpu, bus, (return_pc & 0xFF) as u8);
            restore_stack_high(cpu);
            cpu.regs.pbr = bank;
            cpu.regs.pc = u16::from(lo) | (u16::from(hi) << 8);
        }
        0x82 => {
            // The long branch: a signed 16-bit offset over the
            // program bank.
            let offset = operand_word(cpu, bus) as i16;
            bus_wait(bus);
            cpu.regs.pc = cpu.regs.pc.wrapping_add_signed(offset);
        }
        // The effective-address pushes.
        0xF4 => {
            let value = operand_word(cpu, bus);
            push_raw(cpu, bus, (value >> 8) as u8);
            push_raw(cpu, bus, (value & 0xFF) as u8);
            restore_stack_high(cpu);
        }
        0xD4 => {
            let z = program_byte(cpu, bus);
            dp_penalty(cpu, bus);
            let addr = dp_offset(cpu, z, 0);
            // The PEI pointer walk reads linearly, as the long forms
            // do, and the pushed address is the pointer's value.
            let lo = u16::from(bus.rd(u32::from(addr)));
            let hi = u16::from(bus.rd(u32::from(addr.wrapping_add(1))));
            let value = lo | (hi << 8);
            push_raw(cpu, bus, (value >> 8) as u8);
            push_raw(cpu, bus, (value & 0xFF) as u8);
            restore_stack_high(cpu);
        }
        0x62 => {
            // Push the address relative to the end of the instruction.
            let rel = operand_word(cpu, bus) as i16;
            bus_wait(bus);
            let target = cpu.regs.pc.wrapping_add_signed(rel);
            push_raw(cpu, bus, (target >> 8) as u8);
            push_raw(cpu, bus, (target & 0xFF) as u8);
            restore_stack_high(cpu);
        }
        // The block moves.
        0x54 => block_move(cpu, bus, true),
        0x44 => block_move(cpu, bus, false),
        // The zero store.
        0x64 => store_m(cpu, bus, Mode::Dp, 0, wide),
        0x9C => store_m(cpu, bus, Mode::Abs, 0, wide),
        0x74 => store_m(cpu, bus, Mode::DpX, 0, wide),
        0x9E => store_m(cpu, bus, Mode::AbsX, 0, wide),
        // The index-register stores.
        0x84 => store_m(cpu, bus, Mode::Dp, cpu.regs.y & x_mask, index_wide),
        0x8C => store_m(cpu, bus, Mode::Abs, cpu.regs.y & x_mask, index_wide),
        0x94 => store_m(cpu, bus, Mode::DpX, cpu.regs.y & x_mask, index_wide),
        0x86 => store_m(cpu, bus, Mode::Dp, cpu.regs.x & x_mask, index_wide),
        0x8E => store_m(cpu, bus, Mode::Abs, cpu.regs.x & x_mask, index_wide),
        0x96 => store_m(cpu, bus, Mode::DpY, cpu.regs.x & x_mask, index_wide),
        // The index-register loads.
        0xA0 => {
            let m = immediate(cpu, bus, index_wide) & x_mask;
            cpu.regs.y = if index_wide { m } else { u16::from(m as u8) };
            set_nz_w(cpu, m, index_wide);
        }
        0xA2 => {
            let m = immediate(cpu, bus, index_wide) & x_mask;
            cpu.regs.x = if index_wide { m } else { u16::from(m as u8) };
            set_nz_w(cpu, m, index_wide);
        }
        0xA4 => {
            let addr = operand_addr(cpu, bus, Mode::Dp, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 1, m, index_wide);
        }
        0xAC => {
            let addr = operand_addr(cpu, bus, Mode::Abs, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 1, m, index_wide);
        }
        0xB4 => {
            let addr = operand_addr(cpu, bus, Mode::DpX, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 1, m, index_wide);
        }
        0xBC => {
            let addr = operand_addr(cpu, bus, Mode::AbsX, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 1, m, index_wide);
        }
        0xA6 => {
            let addr = operand_addr(cpu, bus, Mode::Dp, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 0, m, index_wide);
        }
        0xAE => {
            let addr = operand_addr(cpu, bus, Mode::Abs, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 0, m, index_wide);
        }
        0xB6 => {
            let addr = operand_addr(cpu, bus, Mode::DpY, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 0, m, index_wide);
        }
        0xBE => {
            let addr = operand_addr(cpu, bus, Mode::AbsY, false);
            let m = read_at(bus, addr, index_wide);
            load_index(cpu, 0, m, index_wide);
        }
        // The index-register compares.
        0xC0 => {
            let m = immediate(cpu, bus, index_wide) & x_mask;
            compare_w(cpu, cpu.regs.y & x_mask, m, index_wide);
        }
        0xC4 | 0xCC => {
            let mode = if opcode == 0xC4 { Mode::Dp } else { Mode::Abs };
            let addr = operand_addr(cpu, bus, mode, false);
            let m = read_at(bus, addr, index_wide) & x_mask;
            compare_w(cpu, cpu.regs.y & x_mask, m, index_wide);
        }
        0xE0 => {
            let m = immediate(cpu, bus, index_wide) & x_mask;
            compare_w(cpu, cpu.regs.x & x_mask, m, index_wide);
        }
        0xE4 | 0xEC => {
            let mode = if opcode == 0xE4 { Mode::Dp } else { Mode::Abs };
            let addr = operand_addr(cpu, bus, mode, false);
            let m = read_at(bus, addr, index_wide) & x_mask;
            compare_w(cpu, cpu.regs.x & x_mask, m, index_wide);
        }
        // The bit tests through memory.
        0x24 | 0x2C => {
            let mode = if opcode == 0x24 { Mode::Dp } else { Mode::Abs };
            let addr = operand_addr(cpu, bus, mode, false);
            let m = read_at(bus, addr, wide) & mask;
            bit_w(cpu, m, wide);
        }
        0x34 | 0x3C => {
            let mode = if opcode == 0x34 {
                Mode::DpX
            } else {
                Mode::AbsX
            };
            let addr = operand_addr(cpu, bus, mode, false);
            let m = read_at(bus, addr, wide) & mask;
            bit_w(cpu, m, wide);
        }
        // The accumulator shifts.
        0x0A | 0x2A | 0x4A | 0x6A => {
            bus_wait(bus);
            let a = cpu.regs.c & mask;
            cpu.regs.c = (cpu.regs.c & !mask)
                | match opcode {
                    0x0A => asl_w(cpu, a, wide),
                    0x2A => rol_w(cpu, a, wide),
                    0x4A => lsr_w(cpu, a, wide),
                    _ => ror_w(cpu, a, wide),
                };
        }
        // The memory shifts.
        0x06 | 0x16 | 0x0E | 0x1E => rmw_shift(cpu, bus, rmw_mode(opcode), wide, 0),
        0x26 | 0x36 | 0x2E | 0x3E => rmw_shift(cpu, bus, rmw_mode(opcode), wide, 1),
        0x46 | 0x56 | 0x4E | 0x5E => rmw_shift(cpu, bus, rmw_mode(opcode), wide, 2),
        0x66 | 0x76 | 0x6E | 0x7E => rmw_shift(cpu, bus, rmw_mode(opcode), wide, 3),
        // The test-and-set / test-and-reset forms.
        0x04 | 0x0C => {
            let mode = if opcode == 0x04 { Mode::Dp } else { Mode::Abs };
            tsb_trb(cpu, bus, mode, wide, true);
        }
        0x14 | 0x1C => {
            let mode = if opcode == 0x14 { Mode::Dp } else { Mode::Abs };
            tsb_trb(cpu, bus, mode, wide, false);
        }
        // The memory increments and decrements.
        0xE6 | 0xF6 | 0xEE | 0xFE => rmw_w(cpu, bus, rmw_mode(opcode), wide, |c, v| {
            let r = v.wrapping_add(1);
            set_nz_w(c, r, c.m_wide());
            r
        }),
        0xC6 | 0xD6 | 0xCE | 0xDE => rmw_w(cpu, bus, rmw_mode(opcode), wide, |c, v| {
            let r = v.wrapping_sub(1);
            set_nz_w(c, r, c.m_wide());
            r
        }),
        // The register increments and decrements.
        0xE8 | 0xC8 | 0xCA | 0x88 => {
            bus_wait(bus);
            let which = u8::from(opcode == 0xC8 || opcode == 0x88);
            let delta = if opcode == 0xE8 || opcode == 0xC8 {
                1
            } else {
                -1
            };
            index_add(cpu, which, delta, index_wide);
        }
        0x1A | 0x3A => {
            bus_wait(bus);
            let delta = if opcode == 0x1A { 1u16 } else { u16::MAX };
            let a = (cpu.regs.c & mask).wrapping_add(delta);
            cpu.regs.c = (cpu.regs.c & !mask) | a;
            set_nz_w(cpu, a, wide);
        }
        // The transfers; the destination width carries the transfer
        // and the flag read.
        0xAA => {
            bus_wait(bus);
            cpu.regs.x = if index_wide {
                cpu.regs.c
            } else {
                u16::from(cpu.regs.c as u8)
            };
            set_nz_w(cpu, cpu.regs.x, index_wide);
        }
        0xA8 => {
            bus_wait(bus);
            cpu.regs.y = if index_wide {
                cpu.regs.c
            } else {
                u16::from(cpu.regs.c as u8)
            };
            set_nz_w(cpu, cpu.regs.y, index_wide);
        }
        0x8A => {
            bus_wait(bus);
            if wide {
                cpu.regs.c = cpu.regs.x;
                set_nz_w(cpu, cpu.regs.c, true);
            } else {
                cpu.regs.c = (cpu.regs.c & 0xFF00) | (cpu.regs.x & 0x00FF);
                set_nz_w(cpu, cpu.regs.c & 0x00FF, false);
            }
        }
        0x98 => {
            bus_wait(bus);
            if wide {
                cpu.regs.c = cpu.regs.y;
                set_nz_w(cpu, cpu.regs.c, true);
            } else {
                cpu.regs.c = (cpu.regs.c & 0xFF00) | (cpu.regs.y & 0x00FF);
                set_nz_w(cpu, cpu.regs.c & 0x00FF, false);
            }
        }
        0xBA => {
            bus_wait(bus);
            if index_wide {
                cpu.regs.x = cpu.regs.s;
                set_nz_w(cpu, cpu.regs.x, true);
            } else {
                cpu.regs.x = (cpu.regs.x & 0xFF00) | (cpu.regs.s & 0x00FF);
                set_nz_w(cpu, cpu.regs.x & 0x00FF, false);
            }
        }
        0x9A => {
            bus_wait(bus);
            if cpu.regs.e {
                cpu.regs.s = 0x0100 | u16::from(cpu.regs.x as u8);
            } else {
                cpu.regs.s = cpu.regs.x;
            }
        }
        0x1B => {
            bus_wait(bus);
            if cpu.regs.e {
                cpu.regs.s = 0x0100 | u16::from(cpu.regs.c as u8);
            } else {
                cpu.regs.s = cpu.regs.c;
            }
        }
        0x3B => {
            // The stack pointer transfer always moves 16 bits.
            bus_wait(bus);
            cpu.regs.c = cpu.regs.s;
            set_nz_w(cpu, cpu.regs.c, true);
        }
        0x5B => {
            bus_wait(bus);
            // The direct register transfer always moves 16 bits.
            cpu.regs.d = cpu.regs.c;
            set_nz_w(cpu, cpu.regs.d, true);
        }
        0x7B => {
            // The direct register transfer always moves 16 bits.
            bus_wait(bus);
            cpu.regs.c = cpu.regs.d;
            set_nz_w(cpu, cpu.regs.c, true);
        }
        0x9B => {
            bus_wait(bus);
            cpu.regs.y = if index_wide {
                cpu.regs.x
            } else {
                u16::from(cpu.regs.x as u8)
            };
            set_nz_w(cpu, cpu.regs.y, index_wide);
        }
        0xBB => {
            bus_wait(bus);
            cpu.regs.x = if index_wide {
                cpu.regs.y
            } else {
                u16::from(cpu.regs.y as u8)
            };
            set_nz_w(cpu, cpu.regs.x, index_wide);
        }
        0xEB => {
            // XBA: exchange B and A; the flags read the new low byte.
            bus_wait(bus);
            bus_wait(bus);
            cpu.regs.c = (cpu.regs.c >> 8) | ((cpu.regs.c & 0x00FF) << 8);
            set_nz_w(cpu, cpu.regs.c & 0x00FF, false);
        }
        0xFB => {
            // XCE: exchange carry and emulation.
            bus_wait(bus);
            let entering = cpu.regs.p & FLAG_C != 0;
            if cpu.regs.e {
                cpu.regs.p |= FLAG_C;
            } else {
                cpu.regs.p &= !FLAG_C;
            }
            cpu.regs.e = entering;
            if cpu.regs.e {
                // Entering emulation forces the register forms.
                cpu.regs.p |= FLAG_M | FLAG_X;
                cpu.regs.s = 0x0100 | (cpu.regs.s & 0x00FF);
                cpu.regs.x &= 0x00FF;
                cpu.regs.y &= 0x00FF;
            }
        }
        // The stack pushes: the high byte first, the low byte second.
        0x08 => {
            bus_wait(bus);
            push8(cpu, bus, cpu.push_p(true));
        }
        0x48 => {
            bus_wait(bus);
            if wide {
                push16(cpu, bus, cpu.regs.c & mask);
            } else {
                push8(cpu, bus, (cpu.regs.c & mask) as u8);
            }
        }
        0x5A => {
            bus_wait(bus);
            if index_wide {
                push16(cpu, bus, cpu.regs.y & x_mask);
            } else {
                push8(cpu, bus, (cpu.regs.y & x_mask) as u8);
            }
        }
        0xDA => {
            bus_wait(bus);
            if index_wide {
                push16(cpu, bus, cpu.regs.x & x_mask);
            } else {
                push8(cpu, bus, (cpu.regs.x & x_mask) as u8);
            }
        }
        0x4B => {
            bus_wait(bus);
            push8(cpu, bus, cpu.regs.pbr);
        }
        0x8B => {
            bus_wait(bus);
            push8(cpu, bus, cpu.regs.dbr);
        }
        0x0B => {
            bus_wait(bus);
            push_raw(cpu, bus, (cpu.regs.d >> 8) as u8);
            push_raw(cpu, bus, (cpu.regs.d & 0xFF) as u8);
            restore_stack_high(cpu);
        }
        // The stack pulls: the low byte first.
        0x28 => {
            bus_wait(bus);
            bus_wait(bus);
            let value = pull8(cpu, bus);
            cpu.pull_p(value);
        }
        0x68 => {
            bus_wait(bus);
            bus_wait(bus);
            if wide {
                let value = pull16(cpu, bus) & mask;
                cpu.regs.c = (cpu.regs.c & !mask) | value;
                set_nz_w(cpu, value, wide);
            } else {
                let value = pull8(cpu, bus) as u16;
                cpu.regs.c = (cpu.regs.c & !mask) | value;
                set_nz_w(cpu, value, false);
            }
        }
        0x7A => {
            bus_wait(bus);
            bus_wait(bus);
            let value: u16 = if index_wide {
                pull16(cpu, bus) & x_mask
            } else {
                u16::from(pull8(cpu, bus))
            };
            cpu.regs.y = if index_wide {
                value
            } else {
                u16::from(value as u8)
            };
            set_nz_w(cpu, value, index_wide);
        }
        0xFA => {
            bus_wait(bus);
            bus_wait(bus);
            let value: u16 = if index_wide {
                pull16(cpu, bus) & x_mask
            } else {
                u16::from(pull8(cpu, bus))
            };
            cpu.regs.x = if index_wide {
                value
            } else {
                u16::from(value as u8)
            };
            set_nz_w(cpu, value, index_wide);
        }
        0xAB => {
            bus_wait(bus);
            bus_wait(bus);
            cpu.regs.dbr = pull8(cpu, bus);
            set_nz8(cpu, cpu.regs.dbr);
        }
        0x2B => {
            bus_wait(bus);
            bus_wait(bus);
            let lo = u16::from(pull_raw(cpu, bus));
            let hi = u16::from(pull_raw(cpu, bus));
            cpu.regs.d = lo | (hi << 8);
            restore_stack_high(cpu);
            set_nz_w(cpu, cpu.regs.d, true);
        }
        // The status register groups.
        0xE2 | 0xC2 => {
            let bits = program_byte(cpu, bus);
            bus_wait(bus);
            if opcode == 0xE2 {
                cpu.regs.p |= bits;
            } else {
                cpu.regs.p &= !bits;
            }
            if cpu.regs.e {
                // Emulation mode locks the register selects.
                cpu.regs.p |= FLAG_M | FLAG_X;
            }
            if cpu.regs.p & FLAG_X != 0 {
                // A set index select truncates the index registers.
                cpu.regs.x &= 0x00FF;
                cpu.regs.y &= 0x00FF;
            }
        }
        0x18 | 0x38 | 0x58 | 0x78 | 0xB8 | 0xD8 | 0xF8 => {
            bus_wait(bus);
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
        // The branches.
        0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 => {
            branch(cpu, bus, branch_taken(cpu, opcode));
        }
        0x80 => branch(cpu, bus, true),
        // The idle and stop forms.
        0xEA => bus_wait(bus),
        0x42 => {
            // WDM: reserved for future expansion; the parameter byte
            // is consumed.
            program_byte(cpu, bus);
        }
        0xCB => {
            bus_wait(bus);
            bus_wait(bus);
            cpu.waiting = true;
            cpu.next = None;
        }
        0xDB => {
            bus_wait(bus);
            bus_wait(bus);
            cpu.stopped = true;
            cpu.next = None;
        }
        // The ALU dispatch and the arms cover every documented byte;
        // this defensive arm keeps the match total.
        _ => bus_wait(bus),
    }
}

/// The stack-space read of the return sequences, at the current
/// pointer, without a register effect.
fn stack_read_self<B: Cycles>(cpu: &Wdc65c816, bus: &mut B) {
    bus.rd(u32::from(stack_addr(cpu, 0)));
}

/// Write the stack byte at the current pointer and decrement it. The
/// page-one low byte steps in emulation mode; the free 16-bit stack
/// pointer steps in native mode.
fn push8<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, value: u8) {
    let at = stack_addr(cpu, 0);
    bus.wr(u32::from(at), value);
    step_stack(cpu, -1_i16);
}

/// Read the stack byte one above the current pointer and advance it.
fn pull8<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B) -> u8 {
    step_stack(cpu, 1);
    bus.rd(u32::from(cpu.regs.s))
}

/// One stack-pointer step in the emulation-mode page-one form.
#[inline]
fn step_stack(cpu: &mut Wdc65c816, delta: i16) {
    if cpu.regs.e {
        let low = (cpu.regs.s as u8).wrapping_add_signed(delta as i8);
        cpu.regs.s = 0x0100 | u16::from(low);
    } else {
        cpu.regs.s = cpu.regs.s.wrapping_add_signed(delta);
    }
}

/// Push one 16-bit value: the high byte first, the low byte second.
fn push16<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, value: u16) {
    push8(cpu, bus, (value >> 8) as u8);
    push8(cpu, bus, (value & 0xFF) as u8);
}

/// Pull one 16-bit value: the low byte first, the high byte second.
fn pull16<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B) -> u16 {
    let lo = u16::from(pull8(cpu, bus));
    let hi = u16::from(pull8(cpu, bus));
    lo | (hi << 8)
}

/// Push one stack byte with the raw 16-bit stack pointer arithmetic;
/// the push forms with two or three bytes step beyond the page-one
/// range when the low byte wraps.
#[inline]
fn push_raw<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, value: u8) {
    bus.wr(u32::from(cpu.regs.s), value);
    cpu.regs.s = cpu.regs.s.wrapping_sub(1);
}

/// Pull one stack byte over the raw 16-bit stack pointer.
#[inline]
fn pull_raw<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B) -> u8 {
    cpu.regs.s = cpu.regs.s.wrapping_add(1);
    bus.rd(u32::from(cpu.regs.s))
}

/// Restore the page-one high byte of the stack pointer after the
/// raw multi-byte stack forms ran, per the data sheet's stack range.
#[inline]
fn restore_stack_high(cpu: &mut Wdc65c816) {
    if cpu.regs.e {
        cpu.regs.s = 0x0100 | (cpu.regs.s & 0x00FF);
    }
}

/// Store a pulled index register at the index width, with the
/// negative and zero flags. An 8-bit pull writes only the low byte;
/// a set index select leaves the high byte as it was.
fn load_index(cpu: &mut Wdc65c816, which: u8, value: u16, index_wide: bool) {
    let value = if index_wide {
        value
    } else {
        u16::from(value as u8)
    };
    if which == 0 {
        cpu.regs.x = value;
    } else {
        cpu.regs.y = value;
    }
    set_nz_w(cpu, value, index_wide);
}

/// Increment or decrement one index register at the index width.
fn index_add(cpu: &mut Wdc65c816, which: u8, delta: i16, index_wide: bool) {
    let wide = index_wide;
    if which == 0 {
        cpu.regs.x = step_reg(cpu.regs.x, delta, wide);
        set_nz_w(cpu, cpu.regs.x, wide);
    } else {
        cpu.regs.y = step_reg(cpu.regs.y, delta, wide);
        set_nz_w(cpu, cpu.regs.y, wide);
    }
}

/// One register step: the low byte wraps on the page in the 8-bit
/// form and the full register wraps in the 16-bit one.
fn step_reg(value: u16, delta: i16, wide: bool) -> u16 {
    if wide {
        value.wrapping_add_signed(delta)
    } else {
        (value & 0xFF00) | u16::from((value as u8).wrapping_add_signed(delta as i8))
    }
}

/// The branch: the offset read, then the taken-branch cycles. The
/// core reads the program counter once for the add, and emulation
/// mode adds one more cycle when the target crosses a page.
fn branch<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, taken: bool) {
    let offset = program_byte(cpu, bus) as i8 as i16;
    if !taken {
        return;
    }
    let from = cpu.regs.pc;
    let target = from.wrapping_add_signed(offset);
    bus.rd(prog_addr(cpu, from));
    if cpu.regs.e && (target & 0xFF00) != (from & 0xFF00) {
        bus_wait(bus);
    }
    cpu.regs.pc = target;
}

/// True when the branch condition holds.
fn branch_taken(cpu: &Wdc65c816, opcode: u8) -> bool {
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

/// The block move: the destination bank byte (which loads into the
/// data bank), then the source bank byte, then one byte's transfer.
/// The visit costs seven cycles: the two operand bytes, the read, the
/// write, and two arithmetic cycles. The count register decrements
/// past the transfer; while it has not reached the full wrap the core
/// re-enters the instruction, which keeps the move interruptible.
fn block_move<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, forward: bool) {
    let dst_bank = program_byte(cpu, bus);
    let src_bank = program_byte(cpu, bus);
    cpu.regs.dbr = dst_bank;
    let delta = if forward { 1_i16 } else { -1_i16 };
    let index_wide = cpu.x_wide();
    let value = bus.rd(compose(src_bank, cpu.regs.x));
    bus.wr(compose(dst_bank, cpu.regs.y), value);
    bus_wait(bus);
    cpu.regs.x = step_reg(cpu.regs.x, delta, index_wide);
    cpu.regs.y = step_reg(cpu.regs.y, delta, index_wide);
    let count = cpu.regs.c;
    cpu.regs.c = cpu.regs.c.wrapping_sub(1);
    bus_wait(bus);
    if count != 0 {
        // The move continues on the next visit.
        cpu.regs.pc = cpu.regs.pc.wrapping_sub(3);
    }
}

/// The hardware and software interrupt service: the idle cycles, the
/// pushes (native also pushes the program bank), the status byte, and
/// the zero-bank vector read. The decimal flag clears and the
/// interrupt disable sets, and the program bank register goes to
/// zero.
fn service_interrupt<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, source: VectorSource) {
    let software = matches!(source, VectorSource::Brk | VectorSource::Cop);
    if !software {
        bus_wait(bus);
        stack_read_pc(cpu, bus);
    }
    if !cpu.regs.e {
        push8(cpu, bus, cpu.regs.pbr);
    }
    push16(cpu, bus, cpu.regs.pc);
    push8(cpu, bus, cpu.push_p(software));
    let vec = source.offset(cpu.regs.e);
    let lo = u16::from(bus.rd(u32::from(vec)));
    let hi = u16::from(bus.rd(u32::from(vec.wrapping_add(1))));
    cpu.regs.p &= !FLAG_D;
    cpu.regs.p |= FLAG_I;
    cpu.regs.pbr = 0;
    cpu.regs.pc = (hi << 8) | lo;
}

/// The program-space read of a held program counter during the
/// interrupt sequence.
#[inline]
fn stack_read_pc<B: Cycles>(cpu: &Wdc65c816, bus: &mut B) {
    bus.rd(prog_addr(cpu, cpu.regs.pc));
}

/// The power-on reset sequence: idle cycles, the three dummy pushes
/// that decrement the page-one stack pointer, and the zero-bank
/// vector read. The core always comes up in emulation mode with the
/// interrupt disable set, the register selects at eight bits, and the
/// decimal flag and the bank registers cleared.
fn reset_sequence<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B) {
    for _ in 0..4 {
        stack_read_pc(cpu, bus);
    }
    bus_wait(bus);
    cpu.regs.s = 0x0100 | u16::from((cpu.regs.s as u8).wrapping_sub(3));
    let vec_lo = u16::from(bus.rd(u32::from(VEC_RESET)));
    let vec_hi = u16::from(bus.rd(u32::from(VEC_RESET.wrapping_add(1))));
    cpu.regs.e = true;
    cpu.regs.p = FLAG_I | FLAG_M | FLAG_X;
    cpu.regs.d = 0;
    cpu.regs.dbr = 0;
    cpu.regs.pbr = 0;
    cpu.regs.pc = (vec_hi << 8) | vec_lo;
    cpu.irq_line = false;
    cpu.nmi_line = false;
    cpu.nmi_edge = false;
    cpu.stopped = false;
    cpu.waiting = false;
    cpu.halted_dma = false;
}

/// The test-and-set or test-and-reset bit forms: the zero flag reads
/// the accumulator product; the memory OR, or AND, with the
/// accumulator writes back.
fn tsb_trb<B: Cycles>(cpu: &mut Wdc65c816, bus: &mut B, mode: Mode, wide: bool, set: bool) {
    let mask = if wide { 0xFFFF } else { 0x00FF };
    rmw_w(cpu, bus, mode, wide, move |c, v| {
        let a = c.regs.c & mask;
        set_zero(c, a & v == 0);
        if set { v | a } else { v & !a }
    });
}

/// The add with carry: binary in native mode and binary-coded
/// decimal in emulation mode when the decimal flag is set.
/// Add a value to the accumulator. The 8-bit form runs decimal when
/// the decimal flag stands, in either operating mode; the 16-bit form
/// and native-mode operation with the flag clear stay binary.
fn adc_w(cpu: &mut Wdc65c816, m: u16, wide: bool) {
    let mask = if wide { 0xFFFF } else { 0x00FF };
    let sign = if wide { 0x8000 } else { 0x0080 };
    let a = cpu.regs.c & mask;
    let carry = u32::from(cpu.regs.p & FLAG_C != 0);
    if !wide && cpu.regs.p & FLAG_D != 0 {
        decimal_adc(cpu, a as u8, m as u8);
        return;
    }
    let sum = u32::from(a) + u32::from(m) + carry;
    let result = (sum as u16) & mask;
    cpu.regs.c = (cpu.regs.c & !mask) | result;
    set_nz_w(cpu, result, wide);
    set_flag(cpu, FLAG_V, ((a ^ result) & (m ^ result) & sign) != 0);
    set_flag(cpu, FLAG_C, sum > u32::from(mask));
}

/// The binary-coded decimal add of an 8-bit accumulator: the carry
/// from the nibble arithmetic, the zero flag from the binary sum.
fn decimal_adc(cpu: &mut Wdc65c816, a: u8, m: u8) {
    let carry = u16::from(cpu.regs.p & FLAG_C != 0);
    let mut lo = u16::from(a & 0x0F) + u16::from(m & 0x0F) + carry;
    if lo > 9 {
        lo += 6;
    }
    let mut hi = (u16::from(a) >> 4) + (u16::from(m) >> 4) + (lo >> 4);
    let decimal_carry = hi > 9;
    if decimal_carry {
        hi -= 10;
    }
    let result = (((hi & 0x0F) << 4) | (lo & 0x0F)) as u8;
    let binary = (u16::from(a) + u16::from(m) + carry) & 0x00FF;
    cpu.regs.c = (cpu.regs.c & 0xFF00) | u16::from(result);
    set_zero(cpu, binary == 0);
    set_negative(cpu, result & 0x80 != 0);
    set_flag(cpu, FLAG_V, ((a ^ result) & (m ^ result) & 0x80) != 0);
    set_flag(cpu, FLAG_C, decimal_carry);
}

/// The subtract with borrow: binary in native mode and
/// binary-coded decimal in an 8-bit accumulator when the decimal flag
/// is set. The borrow chain runs through the two nibbles; the carry
/// comes from the nibble arithmetic and the other flags from the
/// binary result.
fn sbc_w(cpu: &mut Wdc65c816, m: u16, wide: bool) {
    let mask = if wide { 0xFFFF } else { 0x00FF };
    let sign = if wide { 0x8000 } else { 0x0080 };
    let a = cpu.regs.c & mask;
    let borrow = u32::from(cpu.regs.p & FLAG_C == 0);
    if !wide && cpu.regs.p & FLAG_D != 0 {
        decimal_sbc(cpu, a as u8, m as u8);
        return;
    }
    let diff = i64::from(a) - i64::from(m) - i64::from(borrow);
    let result = (diff as u16) & mask;
    cpu.regs.c = (cpu.regs.c & !mask) | result;
    set_nz_w(cpu, result, wide);
    set_flag(cpu, FLAG_V, ((a ^ m) & (a ^ result) & sign) != 0);
    set_flag(cpu, FLAG_C, diff >= 0);
}

/// The binary-coded decimal subtract of emulation mode.
fn decimal_sbc(cpu: &mut Wdc65c816, a: u8, m: u8) {
    let borrow = i16::from(u8::from(cpu.regs.p & FLAG_C == 0));
    let mut lo = i16::from(a & 0x0F) - i16::from(m & 0x0F) - borrow;
    let mut hi = i16::from(a >> 4) - i16::from(m >> 4);
    if lo < 0 {
        lo += 10;
        hi -= 1;
    }
    let decimal_borrow = hi < 0;
    if decimal_borrow {
        hi += 10;
    }
    let result = (((hi & 0x0F) << 4) | (lo & 0x0F)) as u8;
    let binary = (i32::from(a) - i32::from(m) - i32::from(borrow)) & 0xFF;
    cpu.regs.c = (cpu.regs.c & 0xFF00) | u16::from(result);
    set_zero(cpu, binary as u16 == 0);
    set_negative(cpu, binary & 0x80 != 0);
    set_flag(cpu, FLAG_V, ((a ^ m) & (a ^ (binary as u8)) & 0x80) != 0);
    set_flag(cpu, FLAG_C, !decimal_borrow);
}

/// The compare: subtract without store; the carry sets when the left
/// side does not borrow.
fn compare_w(cpu: &mut Wdc65c816, left: u16, right: u16, wide: bool) {
    let diff = left.wrapping_sub(right);
    set_nz_w(cpu, diff, wide);
    set_flag(cpu, FLAG_C, left >= right);
}

/// The bit test: zero from the accumulator product, negative and
/// overflow from the operand's top bits.
fn bit_w(cpu: &mut Wdc65c816, m: u16, wide: bool) {
    let sign = if wide { 0x8000 } else { 0x0080 };
    set_zero(
        cpu,
        (cpu.regs.c & if wide { 0xFFFF } else { 0x00FF }) & m == 0,
    );
    set_negative(cpu, m & sign != 0);
    set_flag(cpu, FLAG_V, m & (sign >> 1) != 0);
}

fn set_nz_w(cpu: &mut Wdc65c816, value: u16, wide: bool) {
    if wide {
        set_zero(cpu, value == 0);
        set_negative(cpu, value & 0x8000 != 0);
    } else {
        set_nz8(cpu, value as u8);
    }
}

fn set_nz8(cpu: &mut Wdc65c816, value: u8) {
    set_zero(cpu, value == 0);
    set_negative(cpu, value & 0x80 != 0);
}

fn set_zero(cpu: &mut Wdc65c816, set: bool) {
    if set {
        cpu.regs.p |= FLAG_Z;
    } else {
        cpu.regs.p &= !FLAG_Z;
    }
}

fn set_negative(cpu: &mut Wdc65c816, set: bool) {
    if set {
        cpu.regs.p |= FLAG_N;
    } else {
        cpu.regs.p &= !FLAG_N;
    }
}

fn set_flag(cpu: &mut Wdc65c816, flag: u8, set: bool) {
    if set {
        cpu.regs.p |= flag;
    } else {
        cpu.regs.p &= !flag;
    }
}

/// The shift helpers at the width mask.
fn asl_w(cpu: &mut Wdc65c816, value: u16, wide: bool) -> u16 {
    let mask = if wide { 0xFFFF } else { 0x00FF };
    set_flag(cpu, FLAG_C, value & if wide { 0x8000 } else { 0x0080 } != 0);
    (value << 1) & mask
}

fn lsr_w(cpu: &mut Wdc65c816, value: u16, _wide: bool) -> u16 {
    set_flag(cpu, FLAG_C, value & 1 != 0);
    value >> 1
}

fn rol_w(cpu: &mut Wdc65c816, value: u16, wide: bool) -> u16 {
    let mask = if wide { 0xFFFF } else { 0x00FF };
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let result = ((value << 1) & mask) | c;
    set_flag(cpu, FLAG_C, value & if wide { 0x8000 } else { 0x0080 } != 0);
    result
}

fn ror_w(cpu: &mut Wdc65c816, value: u16, wide: bool) -> u16 {
    let c = u16::from(cpu.regs.p & FLAG_C != 0);
    let result = (value >> 1) | (c << if wide { 15 } else { 7 });
    set_flag(cpu, FLAG_C, value & 1 != 0);
    result
}
#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
