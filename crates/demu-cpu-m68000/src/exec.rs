//! The NMOS 68000 instruction map: one instruction per core visit.
//!
//! ## Timing model
//!
//! One word fetch or one byte/word data access costs 4 CPU clocks; a
//! longword access costs 8 as two word accesses with the high word
//! first; one extension word costs 4. Instruction bodies perform the
//! real bus sequence and pay the published Appendix B total through
//! internal clocks, so every access carries its exact tick and the
//! consumed count matches the row.
//!
//! Effective-address calculation time (byte,word | long): `Dn`/`An`
//! 0|0; `(An)` 4|8; `(An)+` 4|8; `-(An)` 6|10; `d16,An` 8|12;
//! `d8,An,Xn` 10|14; `abs.W` 8|12; `abs.L` 12|16; `d16,PC` 8|12;
//! `d8,PC,Xn` 10|14; immediate 4|8. The index register's size does
//! not affect totals. The two clocks of address computation appear
//! only in the predecrement and indexed read forms; a `MOVE` store to
//! `-(An)` folds them into the store, so `MOVE.L Dx,-(An)` totals 12.
//! `MOVE` totals the fetch, the source calculation, and the store
//! shape: `MOVE.W Dn,(An)` 8; `MOVE.W (An),(An)` 12; `MOVE.L
//! (An),(An)` 20; `MOVE.L -(An),Dn` 14.
//!
//! The rows follow the Motorola user's manual Appendix B, read from
//! the NTNU transcription and cross-checked against the Neo Geo and
//! MegaDrive wiki mirrors plus the Musashi core's cost tables.
//! Exception totals: address error 50, illegal, privilege, trace,
//! and TRAPV-taken 34, TRAP 38, CHK-trap 44 + EA, divide fault 38 +
//! EA, the interrupt auto-vector entry 44 with the acknowledge cycle
//! internal, `rte` 20, `rtr` 20, `rts` 16, `reset` 132, and `stop`
//! published 4 (the flow pays the real fetch and extension word, 8).
//!
//! ## Pinned exception frame form
//!
//! The source is the Musashi core used by MAME (`m68ki_stack_frame_3word`
//! and the generated `rte` handler), which agrees with the user's
//! manual exception section:
//!
//! - The entry pushes the 32-bit program counter first, then the
//!   16-bit status register. After both pushes the frame reads
//!   `SP+0` = status word, `SP+2` = return address longword (high
//!   word first).
//! - `rte` pulls the status word first, then the longword return
//!   address. `rtr` pulls the condition-code word first, then the
//!   longword. `rts` pulls one longword.
//! - Privilege violation, illegal, and the 1010/1111 lines stack the
//!   faulting instruction's own address. Trace, TRAP, TRAPV-taken,
//!   CHK-taken, divide fault, and interrupts stack the next
//!   instruction address.
//! - A stack push or pull that would land on an odd byte address
//!   raises the address error (vector 3, 50 clocks, the faulting
//!   instruction's own address stacked). The entry's own writes round
//!   down so the entry never faults on itself. Data effective-address
//!   accesses round odd addresses to the even pair.
//!
//! The machine's boot preset seeds the supervisor stack pointer and
//! the program counter; the core reads no reset vectors. `STOP`
//! parks the core with the new mask installed and an empty scheduler
//! slot; a mask-opening pin pull or the hold release re-seeds it
//! through `notify_line`. Trace ranks above a pending interrupt at a
//! boundary, matching the published exception priority.
//!
//! No 68010+ forms: reserved and later-CPU encodings run the illegal
//! entry at vector 4.

use crate::M68000;
use demu_core::Address;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;

/// The exception vectors; each handler address is `4 * vector`.
mod vectors {
    pub const ADDRESS_ERROR: u32 = 3;
    pub const ILLEGAL: u32 = 4;
    pub const DIVIDE_BY_ZERO: u32 = 5;
    pub const CHK: u32 = 6;
    pub const TRAPV: u32 = 7;
    pub const PRIVILEGE: u32 = 8;
    pub const TRACE: u32 = 9;
    pub const A_LINE: u32 = 10;
    pub const F_LINE: u32 = 11;
    pub const AUTOVEC_BASE: u32 = 24;
    pub const TRAP_BASE: u32 = 32;
}

/// The exception-entry budgets (the published Appendix B rows).
const CYCLES_EXCEPTION_SHORT: u64 = 34;
const CYCLES_TRAP: u64 = 38;
const CYCLES_INTERRUPT: u64 = 44;
const CYCLES_DIVIDE_FAULT: u64 = 38;
const CYCLES_ADDRESS_ERROR: u64 = 50;
const CYCLES_RESET: u64 = 132;

/// The bus-flow abstraction: the machine access path and the tests
/// share this set of stamped bus operations.
pub trait Flow {
    /// A word fetch.
    fn fetch_word(&mut self, address: u32) -> u16;
    /// A byte read.
    fn rbyte(&mut self, address: u32) -> u8;
    /// A word read.
    fn rword(&mut self, address: u32) -> u16;
    /// A longword read (the high word first).
    fn rlong(&mut self, address: u32) -> u32;
    /// A byte write.
    fn wbyte(&mut self, address: u32, value: u8);
    /// A word write.
    fn wword(&mut self, address: u32, value: u16);
    /// A longword write (the high word first).
    fn wlong(&mut self, address: u32, value: u32);
    /// Pay internal CPU clocks.
    fn wait(&mut self, cycles: u64);
    /// The CPU clocks consumed so far.
    fn cycles(&self) -> u64;
    /// Report one runtime event from the core.
    fn emit(&mut self, event: TriggerEvent);
}

impl Flow for crate::Access<'_, '_> {
    fn fetch_word(&mut self, address: u32) -> u16 {
        crate::Access::fetch_word(self, address)
    }

    fn rbyte(&mut self, address: u32) -> u8 {
        crate::Access::read_byte(self, address)
    }

    fn rword(&mut self, address: u32) -> u16 {
        crate::Access::read_word(self, address)
    }

    fn rlong(&mut self, address: u32) -> u32 {
        crate::Access::read_long(self, address)
    }

    fn wbyte(&mut self, address: u32, value: u8) {
        crate::Access::write_byte(self, address, value)
    }

    fn wword(&mut self, address: u32, value: u16) {
        crate::Access::write_word(self, address, value)
    }

    fn wlong(&mut self, address: u32, value: u32) {
        crate::Access::write_long(self, address, value)
    }

    fn wait(&mut self, cycles: u64) {
        crate::Access::wait(self, cycles)
    }

    fn cycles(&self) -> u64 {
        crate::Access::cycles(self)
    }

    fn emit(&mut self, event: TriggerEvent) {
        crate::Access::emit(self, event)
    }
}

/// Operand size: byte, word, or long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Size {
    B,
    W,
    L,
}

impl Size {
    /// The register-file index for `reg_load` and `reg_store`.
    fn reg_bits(self) -> u8 {
        match self {
            Size::B => 0,
            Size::W => 1,
            Size::L => 2,
        }
    }

    /// The words moved per data access.
    fn words(self) -> u64 {
        match self {
            Size::L => 2,
            _ => 1,
        }
    }

    /// The bytes moved per `(An)+` or `-(An)` step. The byte stack
    /// forms at A7 still step two.
    fn step(self, reg: u8) -> u32 {
        match self {
            Size::B if reg != 7 => 1,
            Size::B => 2,
            Size::W => 2,
            Size::L => 4,
        }
    }

    /// The value mask.
    fn mask(self) -> u32 {
        match self {
            Size::B => 0xFF,
            Size::W => 0xFFFF,
            Size::L => 0xFFFF_FFFF,
        }
    }

    /// The sign bit weight.
    fn sign_bit(self) -> u32 {
        match self {
            Size::B => 0x80,
            Size::W => 0x8000,
            Size::L => 0x8000_0000,
        }
    }

    /// Write a sized value into a register, merging low bytes.
    fn into_data(self, old: u32, value: u32) -> u32 {
        let value = value & self.mask();
        match self {
            Size::B => old & 0xFFFF_FF00 | value,
            Size::W => old & 0xFFFF_0000 | value,
            Size::L => value,
        }
    }
}

/// Sign-extend the low byte of `v`.
fn sext8(v: u32) -> u32 {
    let v8 = (v & 0xFF) as u8;
    if v8 & 0x80 != 0 {
        0xFFFF_FF00 | u32::from(v8)
    } else {
        u32::from(v8)
    }
}

/// Sign-extend the low word of `v`.
fn sext16(v: u32) -> u32 {
    let v16 = (v & 0xFFFF) as u16;
    if v16 & 0x8000 != 0 {
        0xFFFF_0000 | u32::from(v16)
    } else {
        u32::from(v16)
    }
}

/// The effective-address kinds, in the instruction word's mode order
/// with the program-counter forms and the immediate appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dn,
    An,
    Ind,
    Pinc,
    Pdec,
    Disp,
    Index,
    Absw,
    Absl,
    Pcd,
    Pci,
    Imm,
}

impl Kind {
    /// The address-computation wait after the extension words.
    fn calc_wait(self) -> u64 {
        match self {
            Kind::Pdec | Kind::Index | Kind::Pci => 2,
            _ => 0,
        }
    }

    /// The extension words the address form reads as bus words.
    fn ext_words(self) -> u64 {
        match self {
            Kind::Dn | Kind::An | Kind::Ind | Kind::Pinc | Kind::Pdec | Kind::Imm => 0,
            Kind::Disp | Kind::Index | Kind::Absw | Kind::Pcd | Kind::Pci => 1,
            Kind::Absl => 2,
        }
    }

    /// The published calculation time for a read operand: the
    /// extension words, the computed wait, and the operand fetch.
    fn read_cost(self, size: Size) -> u64 {
        match self {
            Kind::Dn | Kind::An => 0,
            Kind::Imm => 4 * size.words(),
            _ => self.ext_words() * 4 + self.calc_wait() + 4 * size.words(),
        }
    }

    /// The published store shape: the extension words, the computed
    /// wait, and the store words.
    fn write_cost(self, size: Size) -> u64 {
        match self {
            Kind::Dn | Kind::An => 0,
            _ => self.ext_words() * 4 + self.calc_wait() + 4 * size.words(),
        }
    }

    /// The folded store shape for the `MOVE` predecrement destination.
    fn write_cost_folded(self, size: Size) -> u64 {
        match self {
            Kind::Pdec => self.ext_words() * 4 + 4 * size.words(),
            _ => self.write_cost(size),
        }
    }
}

/// A resolved effective address.
#[derive(Debug, Clone, Copy)]
struct Ea {
    kind: Kind,
    reg: u8,
    /// The resolved data address for the memory forms.
    addr: u32,
}

/// Read one extension word at the program counter and advance it.
fn ext_word<F: Flow>(cpu: &mut M68000, bus: &mut F) -> u16 {
    let v = bus.fetch_word(cpu.pc);
    cpu.pc = cpu.pc.wrapping_add(2);
    v
}

/// Sign-extended displacement of the extension word just read.
fn ext_disp<F: Flow>(cpu: &mut M68000, bus: &mut F) -> u32 {
    sext16(u32::from(ext_word(cpu, bus)))
}

/// The index-register load for a brief indexed extension word: a word
/// index sign-extends, an address-register index loads whole.
fn index_value(cpu: &M68000, extension: u16) -> u32 {
    let idx = ((extension >> 12) & 7) as usize;
    if extension & 0x8000 != 0 {
        cpu.a[idx]
    } else {
        sext16(cpu.d[idx])
    }
}

/// Decode and evaluate one effective address, fetching extension
/// words and moving the `(An)+` and `-(An)` registers.
fn ea_eval<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, size: Size) -> Ea {
    let areg = cpu.a[reg as usize];
    match mode {
        0 => Ea {
            kind: Kind::Dn,
            reg,
            addr: 0,
        },
        1 => Ea {
            kind: Kind::An,
            reg,
            addr: 0,
        },
        2 => Ea {
            kind: Kind::Ind,
            reg,
            addr: areg,
        },
        3 => {
            let step = size.step(reg);
            cpu.a[reg as usize] = areg.wrapping_add(step);
            Ea {
                kind: Kind::Pinc,
                reg,
                addr: areg,
            }
        }
        4 => {
            let step = size.step(reg);
            let at = areg.wrapping_sub(step);
            cpu.a[reg as usize] = at;
            Ea {
                kind: Kind::Pdec,
                reg,
                addr: at,
            }
        }
        5 => Ea {
            kind: Kind::Disp,
            reg,
            addr: areg.wrapping_add(ext_disp(cpu, bus)),
        },
        6 => Ea {
            kind: Kind::Index,
            reg,
            addr: {
                let extension = ext_word(cpu, bus);
                areg.wrapping_add(index_value(cpu, extension))
                    .wrapping_add(sext8(u32::from(extension & 0xFF)))
            },
        },
        7 => match reg {
            0 => Ea {
                kind: Kind::Absw,
                reg,
                addr: sext16(u32::from(ext_word(cpu, bus))),
            },
            1 => Ea {
                kind: Kind::Absl,
                reg,
                addr: {
                    let v = bus.rlong(cpu.pc);
                    cpu.pc = cpu.pc.wrapping_add(4);
                    v
                },
            },
            2 => Ea {
                kind: Kind::Pcd,
                reg,
                addr: cpu.pc.wrapping_add(ext_disp(cpu, bus)),
            },
            3 => Ea {
                kind: Kind::Pci,
                reg,
                addr: {
                    let base = cpu.pc;
                    let extension = ext_word(cpu, bus);
                    base.wrapping_add(index_value(cpu, extension))
                        .wrapping_add(sext8(u32::from(extension & 0xFF)))
                },
            },
            4 => Ea {
                kind: Kind::Imm,
                reg,
                addr: cpu.pc,
            },
            _ => Ea {
                kind: Kind::Dn,
                reg,
                addr: 0,
            },
        },
        _ => Ea {
            kind: Kind::Dn,
            reg,
            addr: 0,
        },
    }
}

/// Pay the address-computation wait for the forms that carry one.
fn ea_calc<F: Flow>(bus: &mut F, ea: Ea) {
    bus.wait(ea.kind.calc_wait());
}

/// Read the operand at the effective address, then pay the
/// calculation wait where the published row keeps it outside the
/// bus words.
fn ea_read<F: Flow>(cpu: &mut M68000, bus: &mut F, ea: Ea, size: Size) -> u32 {
    let value = match ea.kind {
        Kind::Dn => cpu.reg_load(size.reg_bits(), ea.reg as usize, false),
        Kind::An => match size {
            Size::B => cpu.a[ea.reg as usize] & 0xFF,
            Size::W => cpu.a[ea.reg as usize] & 0xFFFF,
            Size::L => cpu.a[ea.reg as usize],
        },
        Kind::Imm => match size {
            Size::B => u32::from(ext_word(cpu, bus) as u8),
            Size::W => u32::from(ext_word(cpu, bus)),
            Size::L => {
                let v = bus.rlong(cpu.pc);
                cpu.pc = cpu.pc.wrapping_add(4);
                v
            }
        },
        _ => match size {
            Size::B => u32::from(bus.rbyte(ea.addr)),
            Size::W => u32::from(bus.rword(ea.addr)),
            Size::L => bus.rlong(ea.addr),
        },
    };
    ea_calc(bus, ea);
    value
}

/// Store a sized value at the effective address. The store's
/// calculation wait rides the fill of the running total.
fn ea_store<F: Flow>(cpu: &mut M68000, bus: &mut F, ea: Ea, size: Size, value: u32) {
    match ea.kind {
        Kind::Dn => cpu.reg_store(size.reg_bits(), ea.reg as usize, value, false),
        Kind::An => cpu.reg_store(size.reg_bits(), ea.reg as usize, value, true),
        _ => match size {
            Size::B => bus.wbyte(ea.addr, value as u8),
            Size::W => bus.wword(ea.addr, value as u16),
            Size::L => bus.wlong(ea.addr, value),
        },
    }
}

/// Fill the instruction remainder with internal clocks to reach the
/// published total.
fn fill<F: Flow>(bus: &mut F, total: u64) {
    let consumed = bus.cycles();
    if consumed < total {
        bus.wait(total - consumed);
    }
}

/// The condition-code predicates in the published code order:
/// true, false, hi, ls, cc, cs, ne, eq, vc, vs, pl, mi, ge, lt, gt, le.
fn condition(cpu: &M68000, code: u8) -> bool {
    match code & 0xF {
        0x0 => true,
        0x1 => false,
        0x2 => !cpu.carry && !cpu.zero,
        0x3 => cpu.carry || cpu.zero,
        0x4 => !cpu.carry,
        0x5 => cpu.carry,
        0x6 => !cpu.zero,
        0x7 => cpu.zero,
        0x8 => !cpu.over,
        0x9 => cpu.over,
        0xA => !cpu.neg,
        0xB => cpu.neg,
        0xC => cpu.neg == cpu.over,
        0xD => cpu.neg != cpu.over,
        0xE => !cpu.zero && cpu.neg == cpu.over,
        _ => cpu.zero || cpu.neg != cpu.over,
    }
}

/// Set N and Z from a sized result.
fn set_nz_value(cpu: &mut M68000, size: Size, value: u32) {
    let value = value & size.mask();
    cpu.zero = value == 0;
    cpu.neg = value & size.sign_bit() != 0;
}

/// The logic-op flag shape: N and Z from the result, V and C clear.
fn set_logic_flags(cpu: &mut M68000, size: Size, value: u32) {
    set_nz_value(cpu, size, value);
    cpu.over = false;
    cpu.carry = false;
}

/// Addition with carry-in. N, Z, V, C, and X all install.
fn add_flags(cpu: &mut M68000, size: Size, a: u32, b: u32, carry: u32) -> u32 {
    let mask = u64::from(size.mask());
    let sum = u64::from(a & size.mask()) + u64::from(b & size.mask()) + u64::from(carry & 1);
    let result = (sum & mask) as u32;
    cpu.carry = if size == Size::L {
        sum >> 32 & 1 != 0
    } else {
        sum > mask
    };
    cpu.extend = cpu.carry;
    let sa = a & size.sign_bit() != 0;
    let sb = b & size.sign_bit() != 0;
    let sr = result & size.sign_bit() != 0;
    cpu.over = sa == sb && sr != sa;
    set_nz_value(cpu, size, result);
    result
}

/// Subtraction with borrow-in. N, Z, V, C, and X all install.
fn sub_flags(cpu: &mut M68000, size: Size, a: u32, b: u32, borrow: u32) -> u32 {
    let mask = size.mask();
    let total = i128::from(a & mask) - i128::from(b & mask) - i128::from(borrow & 1);
    let result = ((total as u64) & u64::from(mask)) as u32;
    cpu.carry = total < 0;
    cpu.extend = cpu.carry;
    let sa = a & size.sign_bit() != 0;
    let sb = b & size.sign_bit() != 0;
    let sr = result & size.sign_bit() != 0;
    cpu.over = sb != sa && sr != sa;
    set_nz_value(cpu, size, result);
    result
}

/// Step one instruction from `tick`: the pending trace entry first,
/// the interrupt service next, else the instruction at the program
/// counter. A parked core wakes into the entry when the decoded level
/// sits above the mask, otherwise the slot stays empty until a line
/// wake re-seeds it.
pub fn step(cpu: &mut M68000, tick: Tick, ctx: &mut MachineCtx) {
    if cpu.held {
        cpu.next = None;
        return;
    }
    let was = cpu.halted;
    {
        let mut access = crate::Access::for_cpu(cpu, tick, ctx);
        step_with(cpu, tick, &mut access);
    }
    if cpu.halted != was {
        let level = if cpu.halted { Level::High } else { Level::Low };
        ctx.notify("halt", level, tick);
    }
}

/// Step one instruction over an arbitrary [`Flow`] bus.
pub fn step_with<F: Flow>(cpu: &mut M68000, tick: Tick, bus: &mut F) {
    if cpu.halted {
        run_parked(cpu, bus);
        finish(cpu, tick, bus);
        return;
    }
    if cpu.trace {
        exception_short(cpu, bus, vectors::TRACE, CYCLES_EXCEPTION_SHORT);
        finish(cpu, tick, bus);
        return;
    }
    let level = cpu.interrupt_level();
    if level > cpu.i_field {
        exception_interrupt(cpu, bus, level);
        finish(cpu, tick, bus);
        return;
    }
    let pc = cpu.pc;
    let op = bus.fetch_word(pc);
    bus.emit(TriggerEvent::Fetch(Address(pc)));
    cpu.pc = pc.wrapping_add(2);
    run_one(cpu, bus, op, pc);
    if cpu.trace {
        exception_short(cpu, bus, vectors::TRACE, CYCLES_EXCEPTION_SHORT);
    }
    finish(cpu, tick, bus);
}

/// The parked core: a level above the mask wakes it into the entry.
fn run_parked<F: Flow>(cpu: &mut M68000, bus: &mut F) {
    let level = cpu.interrupt_level();
    if level > cpu.i_field {
        cpu.halted = false;
        exception_interrupt(cpu, bus, level);
        return;
    }
    cpu.next = None;
}

/// Publish the after-instruction schedule.
fn finish<F: Flow>(cpu: &mut M68000, tick: Tick, bus: &mut F) {
    cpu.next = if cpu.halted {
        None
    } else {
        Some(Tick(tick.0 + cpu.div * bus.cycles()))
    };
}

/// The interrupt entry: the internal acknowledge clock, the frame,
/// and the serviced level's mask install. The program counter stays
/// at the next instruction.
fn exception_interrupt<F: Flow>(cpu: &mut M68000, bus: &mut F, level: u8) {
    bus.wait(4);
    exception_common(
        cpu,
        bus,
        vectors::AUTOVEC_BASE + u32::from(level),
        CYCLES_INTERRUPT,
        Some(level),
        cpu.pc,
    );
}

/// The short exception entry with the advanced program counter.
fn exception_short<F: Flow>(cpu: &mut M68000, bus: &mut F, vector: u32, budget: u64) {
    exception_common(cpu, bus, vector, budget, None, cpu.pc);
}

/// The exception entry with the faulting instruction's own address.
fn exception_at<F: Flow>(cpu: &mut M68000, bus: &mut F, vector: u32, budget: u64, op_pc: u32) {
    exception_common(cpu, bus, vector, budget, None, op_pc);
}

/// The exception body: the handler address as one longword vector
/// read, the saved status word, and the frame push in the pinned
/// order. Supervisor state enters, trace clears, and an interrupt
/// installs its mask.
fn exception_common<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    vector: u32,
    budget: u64,
    mask: Option<u8>,
    frame_pc: u32,
) {
    let handler = bus.rlong(vector.wrapping_mul(4));
    let saved = cpu.sr_word();
    cpu.sup = true;
    cpu.trace = false;
    if let Some(level) = mask {
        cpu.i_field = level;
    }
    push_frame(cpu, bus, saved, frame_pc);
    cpu.pc = handler;
    fill(bus, budget);
}

/// Push the return address longword, then the status word (the pinned
/// Musashi short-frame order). The entry's writes round down so a
/// misaligned stack cannot fault this path.
fn push_frame<F: Flow>(cpu: &mut M68000, bus: &mut F, status: u16, frame_pc: u32) {
    let sp = cpu.sp().wrapping_sub(4) & !1;
    cpu.set_sp(sp);
    bus.wlong(sp, frame_pc);
    let sp = cpu.sp().wrapping_sub(2) & !1;
    cpu.set_sp(sp);
    bus.wword(sp, status);
}

/// The address-error entry for a misaligned stack shape.
fn address_error<F: Flow>(cpu: &mut M68000, bus: &mut F, op_pc: u32) {
    exception_at(
        cpu,
        bus,
        vectors::ADDRESS_ERROR,
        CYCLES_ADDRESS_ERROR,
        op_pc,
    );
}

/// Push one longword from the current stack pointer. Returns false
/// when the push would land odd; the address error ran inside.
fn push_long<F: Flow>(cpu: &mut M68000, bus: &mut F, value: u32, op_pc: u32) -> bool {
    let sp = cpu.sp().wrapping_sub(4);
    if sp & 1 != 0 {
        cpu.set_sp(sp);
        address_error(cpu, bus, op_pc);
        return false;
    }
    cpu.set_sp(sp);
    bus.wlong(sp, value);
    true
}

/// Pull one word at the current stack pointer. None means the address
/// error ran and the caller abandons the instruction.
fn pull_word<F: Flow>(cpu: &mut M68000, bus: &mut F, op_pc: u32) -> Option<u16> {
    let sp = cpu.sp();
    if sp & 1 != 0 {
        cpu.set_sp(sp.wrapping_add(2));
        address_error(cpu, bus, op_pc);
        return None;
    }
    let v = bus.rword(sp);
    cpu.set_sp(sp.wrapping_add(2));
    Some(v)
}

/// Pull one longword at the current stack pointer.
fn pull_long<F: Flow>(cpu: &mut M68000, bus: &mut F, op_pc: u32) -> Option<u32> {
    let sp = cpu.sp();
    if sp & 1 != 0 {
        cpu.set_sp(sp.wrapping_add(4));
        address_error(cpu, bus, op_pc);
        return None;
    }
    let v = bus.rlong(sp);
    cpu.set_sp(sp.wrapping_add(4));
    Some(v)
}

/// Run one instruction body by its opcode word at `op_pc` (the
/// address the opcode was fetched from, for the fault frames).
fn run_one<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let line = op >> 12;
    match line {
        0x0 => line_zero(cpu, bus, op, ((op >> 3) & 7) as u8, (op & 7) as u8, op_pc),
        0x1..=0x3 => {
            let size = match line {
                0x1 => Size::B,
                0x2 => Size::L,
                _ => Size::W,
            };
            let op6 = ((op >> 6) & 7) as u8;
            if op6 == 1 {
                // The address-register destination is the MOVEA slot
                // in the word and long lines only.
                if line == 0x1 {
                    exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
                    return;
                }
                let src = ea_eval(cpu, bus, ((op >> 3) & 7) as u8, (op & 7) as u8, size);
                movea_op(cpu, bus, src, size, ((op >> 9) & 7) as u8);
                return;
            }
            let Some(dst) = move_dst(cpu, bus, op6, ((op >> 9) & 7) as u8, size, op_pc) else {
                return;
            };
            let src = ea_eval(cpu, bus, ((op >> 3) & 7) as u8, (op & 7) as u8, size);
            move_op(cpu, bus, src, dst, size);
        }
        0x4 => line_four(cpu, bus, op, ((op >> 3) & 7) as u8, (op & 7) as u8, op_pc),
        0x5 => line_five(cpu, bus, op, ((op >> 3) & 7) as u8, (op & 7) as u8, op_pc),
        0x6 => {
            let cc = ((op >> 8) & 0xF) as u8;
            match cc {
                // Condition 0 branches always (BRA), condition 1 is
                // the subroutine branch (BSR).
                0 => branch_op(cpu, bus, true, op),
                1 => bsr_op(cpu, bus, op, op_pc),
                _ => branch_op(cpu, bus, condition(cpu, cc), op),
            }
        }
        0x7 => moveq_op(cpu, ((op >> 9) & 7) as u8, (op & 0xFF) as u8),
        0x8 | 0x9 | 0xD => alu_line(cpu, bus, op, op_pc),
        0xA => exception_at(cpu, bus, vectors::A_LINE, CYCLES_EXCEPTION_SHORT, op_pc),
        0xB => line_b(cpu, bus, op, op_pc),
        0xC => c_line(cpu, bus, op, op_pc),
        0xE => shift_line(cpu, bus, op, op_pc),
        _ => exception_at(cpu, bus, vectors::F_LINE, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// The `MOVE` destination decoder from bits 8-6. The data and memory
/// alterable forms and the two absolute forms load; the rest illegal.
fn move_dst<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    mode: u8,
    reg: u8,
    size: Size,
    op_pc: u32,
) -> Option<Ea> {
    match mode {
        0 | 2 | 3 | 4 | 5 | 6 => Some(ea_eval(cpu, bus, mode, reg, size)),
        7 if reg <= 1 => Some(ea_eval(cpu, bus, mode, reg, size)),
        _ => {
            exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
            None
        }
    }
}

/// The `MOVE` body: the source read then the store, filled to the
/// published row's fetch + source + store shape. Flags stay.
fn move_op<F: Flow>(cpu: &mut M68000, bus: &mut F, src: Ea, dst: Ea, size: Size) {
    let value = ea_read(cpu, bus, src, size);
    ea_store(cpu, bus, dst, size, value);
    let total = 4 + src.kind.read_cost(size) + dst.kind.write_cost_folded(size);
    fill(bus, total);
}

/// `MOVEA`: the word form sign-extends into the register; flags stay.
fn movea_op<F: Flow>(cpu: &mut M68000, bus: &mut F, src: Ea, size: Size, dst: u8) {
    let value = ea_read(cpu, bus, src, size);
    let value = if size == Size::W {
        sext16(value)
    } else {
        value
    };
    cpu.a[dst as usize] = value;
    fill(bus, 4 + src.kind.read_cost(size));
}

/// The data-alterable set: register direct plus the memory forms.
fn data_alterable(mode: u8, reg: u8) -> bool {
    match mode {
        0 | 2 | 3 | 4 | 5 | 6 => true,
        7 => reg <= 3,
        _ => false,
    }
}

/// The read-source set: data, memory, absolute, PC forms, immediate.
fn read_source_set(mode: u8, reg: u8) -> bool {
    match mode {
        0 | 2 | 3 | 4 | 5 | 6 => true,
        7 => reg <= 4,
        _ => false,
    }
}

/// The control-mode set for JMP, JSR, LEA, PEA, and the MOVEM forms.
fn control_mode(mode: u8, reg: u8) -> bool {
    match mode {
        2 | 5 | 6 => true,
        7 => reg <= 3,
        _ => false,
    }
}

/// Line 0000: the immediate group, the bit operations, and MOVEP.
fn line_zero<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, mode: u8, reg: u8, op_pc: u32) {
    let op9 = ((op >> 9) & 7) as u8;
    let bit8 = (op & 0x100) != 0;
    let op2 = ((op >> 6) & 3) as u8;
    if !bit8 {
        match op9 {
            0 | 1 | 2 | 3 | 5 | 6 => imm_op(cpu, bus, op9, op2, mode, reg, op_pc),
            // The static bit operations with an immediate bit number.
            4 => static_bit_op(cpu, bus, op, op6_of(op), mode, reg, op_pc),
            _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
        }
        return;
    }
    match (mode, op2) {
        // The dynamic bit operations with a register bit source.
        (0, 0..=3) => dynamic_bit_reg(cpu, bus, op9, op2, (op & 7) as usize),
        // MOVEP reuses the mode-1 slot inside the line-0 map.
        (1, 0..=3) => movep_op(cpu, bus, op9, op2, (op & 7) as usize),
        (1, _) => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
        // The dynamic bit operations on a memory byte.
        (2..=6, 0..=3) | (7, 0..=1) => dynamic_bit_mem(cpu, bus, op9, op2, mode, reg),
        _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// The three-bit operation field of the static bit rows.
fn op6_of(op: u16) -> u8 {
    ((op >> 6) & 7) as u8
}

/// The immediate group: ORI, ANDI, SUBI, ADDI, EORI, CMPI with the
/// CCR and SR status slots at mode 7 reg 4.
fn imm_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op9: u8, op2: u8, mode: u8, reg: u8, op_pc: u32) {
    if mode == 7 && reg == 4 {
        // The status slots: byte sets CCR, word sets SR. Only the
        // logical immediates reach them; the subtract, add, and
        // compare encodings stay illegal.
        if op9 == 2 || op9 == 3 || op9 == 6 || op2 == 3 {
            exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
            return;
        }
        if op2 == 1 && !cpu.sup {
            exception_at(cpu, bus, vectors::PRIVILEGE, CYCLES_EXCEPTION_SHORT, op_pc);
            return;
        }
        let size = imm_size(op2);
        let value = imm_operand(cpu, bus, size);
        if op2 == 1 {
            // The SR slot: the logical op applies to the status word.
            let sr = u32::from(cpu.sr_word());
            let word = match op9 {
                0 => sr | value,
                1 => sr & value,
                _ => sr ^ value,
            };
            cpu.set_sr_word(word as u16);
        } else {
            let ccr = u32::from(cpu.ccr_byte());
            let byte = match op9 {
                0 => ccr | value,
                1 => ccr & value,
                _ => ccr ^ value,
            };
            cpu.set_ccr(byte as u8);
        }
        fill(bus, 20);
        return;
    }
    if op2 > 2 || mode == 1 || !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let size = imm_size(op2);
    let ea = ea_eval(cpu, bus, mode, reg, size);
    match op9 {
        6 => {
            // CMPI reads the operand then compares.
            let dst = ea_read(cpu, bus, ea, size);
            let imm = imm_operand(cpu, bus, size);
            sub_flags(cpu, size, dst, imm, 0);
            cpu.extend = false;
            fill(bus, cmpi_total(size, ea));
        }
        _ => {
            let dst = ea_read(cpu, bus, ea, size);
            let imm = imm_operand(cpu, bus, size);
            let (result, keep_logic) = match op9 {
                0 => (dst | imm, true),
                1 => (dst & imm, true),
                2 => (sub_flags(cpu, size, dst, imm, 0), false),
                3 => (add_flags(cpu, size, dst, imm, 0), false),
                _ => (dst ^ imm, true),
            };
            if keep_logic {
                set_logic_flags(cpu, size, result);
            } else {
                set_nz_value(cpu, size, result);
            }
            ea_store(cpu, bus, ea, size, result);
            fill(bus, imm_rw_total(size, ea));
        }
    }
}

/// The immediate operand read at the program counter.
fn imm_operand<F: Flow>(cpu: &mut M68000, bus: &mut F, size: Size) -> u32 {
    match size {
        Size::B => u32::from(ext_word(cpu, bus) as u8),
        Size::W => u32::from(ext_word(cpu, bus)),
        Size::L => {
            let v = bus.rlong(cpu.pc);
            cpu.pc = cpu.pc.wrapping_add(4);
            v
        }
    }
}

/// The two-bit immediate size decode: 0 byte, 1 word, 2 long.
fn imm_size(size_bits: u8) -> Size {
    match size_bits {
        1 => Size::W,
        2 => Size::L,
        _ => Size::B,
    }
}

/// The published immediate read-modify-write total: the register rows
/// at 8 and 16; the memory rows at 12 + EA and 20 + EA.
fn imm_rw_total(size: Size, ea: Ea) -> u64 {
    match ea.kind {
        Kind::Dn => match size {
            Size::L => 16,
            _ => 8,
        },
        _ => match size {
            Size::L => 20 + ea.kind.read_cost(size),
            _ => 12 + ea.kind.read_cost(size),
        },
    }
}

/// The published CMPI total: the register rows at 8 and 14, the
/// memory rows at 8 + EA-read and 12 + EA-read.
fn cmpi_total(size: Size, ea: Ea) -> u64 {
    match ea.kind {
        Kind::Dn => match size {
            Size::L => 14,
            _ => 8,
        },
        _ => match size {
            Size::L => 12 + ea.kind.read_cost(size),
            _ => 8 + ea.kind.read_cost(size),
        },
    }
}

/// The static bit operations (BTST, BCHG, BCLR, BSET) with an
/// immediate bit number.
fn static_bit_op<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    op: u16,
    op_index: u8,
    mode: u8,
    reg: u8,
    op_pc: u32,
) {
    if op_index > 3 {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    if mode == 0 {
        // The register form is long.
        let bit = u32::from(ext_word(cpu, bus) as u8) & 0x1F;
        let dst = (op & 7) as usize;
        cpu.zero = cpu.d[dst] & (1u32 << bit) == 0;
        cpu.over = false;
        cpu.carry = false;
        if op_index != 0 {
            cpu.d[dst] = bit_modify(op_index, cpu.d[dst], bit);
        }
        fill(bus, static_reg_total(op_index));
        return;
    }
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::B);
    let bit = u32::from(ext_word(cpu, bus) as u8) & 7;
    let value = ea_read(cpu, bus, ea, Size::B);
    bit_flags(cpu, value, bit);
    if op_index != 0 {
        ea_store(cpu, bus, ea, Size::B, bit_modify(op_index, value, bit));
    }
    fill(bus, static_mem_total(op_index, ea));
}

/// Set the flags for a tested bit: Z set when the bit is clear.
fn bit_flags(cpu: &mut M68000, value: u32, bit: u32) {
    cpu.zero = value & (1u32 << bit) == 0;
    cpu.over = false;
    cpu.carry = false;
}

/// The modified value for the changing bit operations.
fn bit_modify(op_index: u8, value: u32, bit: u32) -> u32 {
    let mask = 1u32 << bit;
    match op_index {
        1 => value ^ mask,
        2 => value & !mask,
        _ => value | mask,
    }
}

/// The register static-bit totals: BTST 10, BCHG 12, BCLR 14, BSET 12.
fn static_reg_total(op_index: u8) -> u64 {
    match op_index {
        0 => 10,
        1 => 12,
        2 => 14,
        _ => 12,
    }
}

/// The memory static-bit totals: BTST 8 + EA, the rest 12 + EA.
fn static_mem_total(op_index: u8, ea: Ea) -> u64 {
    let ear = ea.kind.read_cost(Size::B);
    match op_index {
        0 => 8 + ear,
        _ => 12 + ear,
    }
}

/// The dynamic bit operations on a data register. The register form
/// is long with the bit index over 32 bit positions.
fn dynamic_bit_reg<F: Flow>(cpu: &mut M68000, bus: &mut F, op9: u8, op_index: u8, reg: usize) {
    let bit = cpu.d[op9 as usize] & 0x1F;
    cpu.zero = cpu.d[reg] & (1u32 << bit) == 0;
    cpu.over = false;
    cpu.carry = false;
    if op_index != 0 {
        cpu.d[reg] = bit_modify(op_index, cpu.d[reg], bit);
    }
    let total = dynamic_reg_total(op_index);
    fill(bus, total);
}

/// The register dynamic-bit totals: BTST 6, BCHG 8, BCLR 10, BSET 8.
fn dynamic_reg_total(op_index: u8) -> u64 {
    match op_index {
        0 => 6,
        1 => 8,
        2 => 10,
        _ => 8,
    }
}

/// The dynamic bit operations on a memory byte.
fn dynamic_bit_mem<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    op9: u8,
    op_index: u8,
    mode: u8,
    reg: u8,
) {
    let ea = ea_eval(cpu, bus, mode, reg, Size::B);
    let bit = cpu.d[op9 as usize] & 7;
    let value = ea_read(cpu, bus, ea, Size::B);
    bit_flags(cpu, value, bit);
    if op_index != 0 {
        ea_store(cpu, bus, ea, Size::B, bit_modify(op_index, value, bit));
    }
    let ear = ea.kind.read_cost(Size::B);
    let total = match op_index {
        0 => 4 + ear,
        _ => 8 + ear,
    };
    fill(bus, total);
}

/// MOVEP: the byte-lane peripheral move at a +2 address stride.
fn movep_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op9: u8, op2: u8, an: usize) {
    let base = cpu.a[an].wrapping_add(ext_disp(cpu, bus));
    let to_reg = op2 <= 1;
    let is_long = op2 == 1 || op2 == 3;
    let total = if is_long { 24 } else { 16 };
    if to_reg {
        let mut value = 0u32;
        let words = if is_long { 4 } else { 2 };
        for i in 0..words {
            let b = u32::from(bus.rbyte(base.wrapping_add(i * 2)));
            value |= b << (8 * (words - 1 - i));
        }
        if is_long {
            cpu.d[op9 as usize] = value;
        } else {
            cpu.d[op9 as usize] = cpu.d[op9 as usize] & 0xFFFF_0000 | value;
        }
    } else {
        let src = cpu.d[op9 as usize];
        let words = if is_long { 4 } else { 2 };
        for i in 0..words {
            let shift = 8 * (words - 1 - i);
            bus.wbyte(base.wrapping_add(i * 2), ((src >> shift) & 0xFF) as u8);
        }
    }
    fill(bus, total);
}

/// Line 0100: the single-operand group, the status moves, and the
/// control instructions. The LEA and CHK rows carry their register in
/// bits 11-9 with bit 8 set, so they decode before the register arms;
/// the remaining single-operand rows hold bit 8 clear.
fn line_four<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, mode: u8, reg: u8, op_pc: u32) {
    let op9 = ((op >> 9) & 7) as u8;
    let op2 = ((op >> 6) & 3) as u8;
    let op6 = ((op >> 6) & 7) as u8;
    let bit8 = (op & 0x100) != 0;
    let size = single_size(op2);
    if bit8 && op6 == 7 {
        // LEA: the destination register in bits 11-9, control modes.
        lea_op(cpu, bus, mode, reg, op9, op_pc);
        return;
    }
    if bit8 && op6 == 6 {
        // CHK with the word form: the checked register in bits 11-9.
        chk_op(cpu, bus, op9, mode, reg, op_pc);
        return;
    }
    if bit8 {
        // The bit-8 rows outside LEA and CHK (the long CHK among
        // them) are not 68000 instructions.
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    match op9 {
        0 => match op2 {
            3 => move_from_sr_op(cpu, bus, mode, reg, op_pc),
            _ => single_rmw(cpu, bus, mode, reg, size, Rmw::Negx, op_pc),
        },
        1 => match op2 {
            3 => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
            _ => single_rmw(cpu, bus, mode, reg, size, Rmw::Clr, op_pc),
        },
        2 => match op2 {
            3 => move_to_ccr_op(cpu, bus, mode, reg, op_pc),
            _ => single_rmw(cpu, bus, mode, reg, size, Rmw::Neg, op_pc),
        },
        3 => match op2 {
            3 => {
                if !cpu.sup {
                    exception_at(cpu, bus, vectors::PRIVILEGE, CYCLES_EXCEPTION_SHORT, op_pc);
                    return;
                }
                move_to_sr_op(cpu, bus, mode, reg, op_pc);
            }
            _ => single_rmw(cpu, bus, mode, reg, size, Rmw::Not, op_pc),
        },
        4 => line_four_r(cpu, bus, op, mode, reg, op_pc),
        5 => match op2 {
            3 => tas_op(cpu, bus, mode, reg, op_pc),
            _ => tst_op(cpu, bus, mode, reg, size, op_pc),
        },
        // 0x4C00-0x4C7F: reserved; 0x4C90+: MOVEM memory into the
        // register list with the word and long rows.
        6 => {
            if op6 == 2 || op6 == 3 {
                movem_load(cpu, bus, mode, reg, op_pc, op6 == 3);
            } else {
                exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
            }
        }
        7 => {
            if op2 == 2 {
                jsr_op(cpu, bus, mode, reg, op_pc);
                return;
            }
            if op2 == 3 {
                if !control_mode(mode, reg) {
                    exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
                    return;
                }
                jmp_op(cpu, bus, mode, reg);
                return;
            }
            if op2 == 0 {
                exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
                return;
            }
            line_four_system(cpu, bus, op, mode, reg, op_pc);
        }
        _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// The single-operand size decode: third row reserved for the
/// status-word slot.
fn single_size(op2: u8) -> Size {
    match op2 {
        1 => Size::W,
        2 => Size::L,
        _ => Size::B,
    }
}

/// The single-operand kinds.
#[derive(Clone, Copy)]
enum Rmw {
    Negx,
    Clr,
    Neg,
    Not,
}

/// NEGX, CLR, NEG, NOT over the data-alterable set.
fn single_rmw<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    mode: u8,
    reg: u8,
    size: Size,
    kind: Rmw,
    op_pc: u32,
) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let value = ea_read(cpu, bus, ea, size);
    // NEGX keeps the accumulated Z form: it clears only on a nonzero
    // result and keeps the old value on a zero result; the other RMW
    // rows install Z from the result.
    let old_zero = cpu.zero;
    let result = match kind {
        Rmw::Negx => {
            let r = sub_flags(cpu, size, 0, value, 1);
            cpu.zero = if r != 0 { false } else { old_zero };
            r
        }
        Rmw::Clr => {
            set_nz_value(cpu, size, 0);
            cpu.over = false;
            cpu.carry = false;
            0
        }
        Rmw::Neg => sub_flags(cpu, size, 0, value, 0),
        Rmw::Not => {
            let v = !value & size.mask();
            set_logic_flags(cpu, size, v);
            v
        }
    };
    ea_store(cpu, bus, ea, size, result);
    let ear = match ea.kind {
        Kind::Dn | Kind::An => 0,
        _ => ea.kind.read_cost(size),
    };
    let total = match (ea.kind, size) {
        (Kind::Dn | Kind::An, Size::L) => 6,
        (Kind::Dn | Kind::An, _) => 4,
        (_, Size::L) => 12 + ear,
        _ => 8 + ear,
    };
    fill(bus, total);
}

/// The NBCD, SWAP, EXT, PEA, and MOVEM rows of the register group.
fn line_four_r<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, mode: u8, reg: u8, op_pc: u32) {
    match ((op >> 6) & 7, mode) {
        (0, _) => nbcd_op(cpu, bus, mode, reg, op_pc),
        (1, 0) => {
            // SWAP the halves of a data register.
            let dst = (op & 7) as usize;
            let swapped = cpu.d[dst].rotate_left(16);
            cpu.d[dst] = swapped;
            set_nz_value(cpu, Size::L, swapped);
            cpu.over = false;
            cpu.carry = false;
            fill(bus, 4);
        }
        (1, _) => pea_op(cpu, bus, mode, reg, op_pc),
        (2, 0) => ext_op(cpu, bus, (op & 7) as usize, false),
        (2, _) => movem_store(cpu, bus, op, mode, reg, op_pc, false),
        (3, 0) => ext_op(cpu, bus, (op & 7) as usize, true),
        (3, _) => movem_store(cpu, bus, op, mode, reg, op_pc, true),
        _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// NBCD as a decimal negate with the extend borrow.
fn nbcd_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, op_pc: u32) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::B);
    let dst = ea_read(cpu, bus, ea, Size::B);
    let result = nbcd_value(cpu, dst);
    ea_store(cpu, bus, ea, Size::B, result);
    let total = match ea.kind {
        Kind::Dn => 6,
        _ => 8 + ea.kind.read_cost(Size::B),
    };
    fill(bus, total);
}

/// The decimal negate with the published flag shape, following the
/// pinned core's body: the borrow result corrects to the ten's
/// complement, Z clears only, and X C set with the borrow.
fn nbcd_value(cpu: &mut M68000, dst: u32) -> u32 {
    let res = (0u32.wrapping_sub(dst & 0xFF)).wrapping_sub(u32::from(cpu.extend));
    if res == 0 {
        cpu.over = false;
        cpu.carry = false;
        cpu.extend = false;
        cpu.neg = false;
        return 0;
    }
    let fixed = if (res | dst) & 0xF == 0 {
        res & 0xF0 | 6
    } else {
        res
    };
    let res = fixed.wrapping_add(0x9A) & 0xFF;
    cpu.carry = true;
    cpu.extend = true;
    cpu.over = false;
    cpu.neg = res & 0x80 != 0;
    if res != 0 {
        cpu.zero = false;
    }
    res
}

/// EXT to word or long over a data register.
fn ext_op<F: Flow>(cpu: &mut M68000, bus: &mut F, reg: usize, to_long: bool) {
    let d = cpu.d[reg];
    let (sext, size) = if to_long {
        (sext16(d & 0xFFFF), Size::L)
    } else {
        (sext8(d & 0xFF), Size::W)
    };
    cpu.d[reg] = size.into_data(d, sext);
    set_nz_value(cpu, size, sext);
    cpu.over = false;
    cpu.carry = false;
    fill(bus, 4);
}

/// TST on any read operand; the address-register direct forms read.
fn tst_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, size: Size, op_pc: u32) {
    if mode != 1 && !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let value = ea_read(cpu, bus, ea, size);
    set_nz_value(cpu, size, value);
    cpu.over = false;
    cpu.carry = false;
    let total = match ea.kind {
        Kind::Dn | Kind::An => 4,
        _ => 4 + ea.kind.read_cost(size),
    };
    fill(bus, total);
}

/// TAS over the data-alterable set with its read-modify shape.
fn tas_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, op_pc: u32) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::B);
    let value = ea_read(cpu, bus, ea, Size::B);
    set_nz_value(cpu, Size::B, value);
    cpu.over = false;
    cpu.carry = false;
    ea_store(cpu, bus, ea, Size::B, value | 0x80);
    let total = match ea.kind {
        Kind::Dn | Kind::An => 4,
        _ => 10 + ea.kind.read_cost(Size::B),
    };
    fill(bus, total);
}

/// MOVE from SR into a data-alterable destination.
fn move_from_sr_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, op_pc: u32) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::W);
    ea_store(cpu, bus, ea, Size::W, u32::from(cpu.sr_word()));
    let total = match ea.kind {
        Kind::Dn => 6,
        _ => 8 + ea.kind.read_cost(Size::W),
    };
    fill(bus, total);
}

/// MOVE to CCR from any read source.
fn move_to_ccr_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, op_pc: u32) {
    if !read_source_set(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::W);
    let value = ea_read(cpu, bus, ea, Size::W);
    cpu.set_ccr(value as u8);
    fill(bus, 12 + ea.kind.read_cost(Size::W));
}

/// MOVE to SR from any read source; supervisor only.
fn move_to_sr_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, op_pc: u32) {
    if !read_source_set(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::W);
    let value = ea_read(cpu, bus, ea, Size::W);
    cpu.set_sr_word(value as u16);
    fill(bus, 12 + ea.kind.read_cost(Size::W));
}

/// PEA through the control-mode set.
fn pea_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, op_pc: u32) {
    if !control_mode(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::L);
    let total = match ea.kind {
        Kind::Ind => 12,
        Kind::Index | Kind::Absl | Kind::Pci => 20,
        _ => 16,
    };
    if !push_long(cpu, bus, ea.addr, op_pc) {
        return;
    }
    fill(bus, total);
}

/// MOVEM the register list to memory through the allowed forms.
fn movem_store<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    _op: u16,
    mode: u8,
    reg: u8,
    op_pc: u32,
    is_long: bool,
) {
    let okay = match mode {
        2 | 4 | 5 | 6 => true,
        7 => reg <= 1,
        _ => false,
    };
    if !okay {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let size = if is_long { Size::L } else { Size::W };
    // The predecrement store form must not consume the EA evaluation's
    // own decrement; capture the raw register before the eval.
    let raw_predec = cpu.a[reg as usize % 8];
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let list = ext_word(cpu, bus);
    let mut n = 0u64;
    if ea.kind == Kind::Pdec {
        // The predecrement form walks the mask from the highest set
        // bit down. The mask itself is not bit-reversed: bit 0 names
        // D0 in every form. The cursor starts at the raw register
        // value (the EA evaluation's decrement is discarded here) and
        // each set bit pays one decrement before its store.
        let mut cursor = raw_predec;
        for bit in (0..16u32).rev() {
            if list & (1 << bit) != 0 {
                let idx = bit as usize;
                let value = cpu.reg_load(size.reg_bits(), idx, idx >= 8);
                cursor = cursor.wrapping_sub(size.step(0));
                match size {
                    Size::W => bus.wword(cursor, value as u16),
                    _ => bus.wlong(cursor, value),
                }
                n += 1;
            }
        }
        cpu.a[ea.reg as usize] = cursor;
    } else {
        // The displaced forms pay one read-ahead cycle.
        let mut cursor = ea.addr;
        if ea.kind.ext_words() > 0 {
            let _ = bus.rword(ea.addr);
        }
        for bit in 0..16u32 {
            if list & (1 << bit) != 0 {
                let idx = bit as usize;
                let value = cpu.reg_load(size.reg_bits(), idx, idx >= 8);
                match size {
                    Size::W => bus.wword(cursor, value as u16),
                    _ => bus.wlong(cursor, value),
                }
                cursor = cursor.wrapping_add(size.step(0));
                n += 1;
            }
        }
    }
    let extra = match ea.kind {
        Kind::Ind | Kind::Pdec | Kind::Pinc => 0,
        _ => ea.kind.ext_words() * 4 + ea.kind.calc_wait(),
    };
    let total = 8 + n * 4 + extra;
    fill(bus, total);
}

/// MOVEM memory into the register list through the allowed forms.
fn movem_load<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    mode: u8,
    reg: u8,
    op_pc: u32,
    is_long: bool,
) {
    let okay = match mode {
        2 | 3 | 5 | 6 => true,
        7 => reg <= 3,
        _ => false,
    };
    if !okay {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let size = if is_long { Size::L } else { Size::W };
    let is_pinc = mode == 3;
    let start = if is_pinc {
        cpu.a[reg as usize]
    } else {
        ea_eval(cpu, bus, mode, reg, size).addr
    };
    let list = ext_word(cpu, bus);
    let mut n = 0u64;
    let mut cursor = start;
    // The load forms pay two read-ahead cycles.
    let _ = bus.rword(start);
    let _ = bus.rword(start);
    for bit in 0..16u32 {
        if list & (1 << bit) != 0 {
            let idx = bit as usize;
            let value = match size {
                Size::W => u32::from(bus.rword(cursor)),
                _ => bus.rlong(cursor),
            };
            if size == Size::W && idx >= 8 {
                // Word loads into an address register sign-extend.
                cpu.a[idx] = sext16(value);
            } else if idx >= 8 {
                cpu.a[idx] = value;
            } else {
                cpu.d[idx] = size.into_data(cpu.d[idx], value);
            }
            cursor = cursor.wrapping_add(size.step(0));
            n += 1;
        }
    }
    if is_pinc {
        cpu.a[reg as usize] = start.wrapping_add(size.step(0).wrapping_mul(n as u32));
    }
    let per = if is_long { 8 } else { 4 };
    let total = 12 + n * per + ea_kind_ext(mode, reg);
    fill(bus, total);
}

/// The extension-word and calculation clocks a MOVEM load's address
/// form carries on top of the base and the register transfers.
fn ea_kind_ext(mode: u8, reg: u8) -> u64 {
    match (mode, reg) {
        (2, _) | (3, _) => 0,
        (5, _) | (7, 0) | (7, 2) => 4,
        (6, _) | (7, 3) => 6,
        (7, 1) => 8,
        _ => 0,
    }
}

/// CHK (word) against a data register with the boundary trap. The
/// checked register rides bits 11-9; the bound comes from the operand.
fn chk_op<F: Flow>(cpu: &mut M68000, bus: &mut F, checked: u8, mode: u8, reg: u8, op_pc: u32) {
    if !read_source_set(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::W);
    let bound = ea_read(cpu, bus, ea, Size::W);
    let value = cpu.d[checked as usize] & 0xFFFF;
    let signed_value = i64::from(value as i32 as i16 as i32);
    let signed_bound = i64::from(bound as i32 as i16 as i32);
    let ear = ea.kind.read_cost(Size::W);
    if signed_value < 0 || signed_value > signed_bound {
        // Both trap cases drive N from the operand's own sign, per
        // the published CHK rows.
        cpu.neg = signed_value < 0;
        cpu.zero = value == 0;
        cpu.over = false;
        cpu.carry = false;
        exception_short(cpu, bus, vectors::CHK, 44 + ear);
        return;
    }
    cpu.over = false;
    cpu.carry = false;
    fill(bus, 10 + ear);
}

/// The line-4 op-9-equals-7 group with the system forms on the mode
/// field: links, stack pointer moves, and the low codes.
fn line_four_system<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    op: u16,
    mode: u8,
    reg: u8,
    op_pc: u32,
) {
    match mode {
        0 => {
            // TRAP #0-15: the vector number in the low nibble.
            exception_short(
                cpu,
                bus,
                vectors::TRAP_BASE + u32::from(op & 0xF),
                CYCLES_TRAP,
            );
        }
        2 => link_op(cpu, bus, reg, op_pc),
        3 => unlk_op(cpu, bus, reg, op_pc),
        4 | 5 => {
            // MOVE to/from USP: supervisor only, per the published
            // privilege rows of the same group.
            if !cpu.sup {
                exception_at(cpu, bus, vectors::PRIVILEGE, CYCLES_EXCEPTION_SHORT, op_pc);
                return;
            }
            move_usp_op(cpu, bus, reg, mode == 4);
        }
        6 => match reg {
            0 => {
                // RESET: the published 132 clocks, supervisor only.
                if !cpu.sup {
                    exception_at(cpu, bus, vectors::PRIVILEGE, CYCLES_EXCEPTION_SHORT, op_pc);
                    return;
                }
                fill(bus, CYCLES_RESET);
            }
            1 => {
                // NOP.
                fill(bus, 4);
            }
            2 => {
                // STOP: read the new status word; supervisor only.
                if !cpu.sup {
                    exception_at(cpu, bus, vectors::PRIVILEGE, CYCLES_EXCEPTION_SHORT, op_pc);
                    return;
                }
                let word = ext_word(cpu, bus);
                cpu.set_sr_word(word);
                cpu.halted = true;
            }
            3 => {
                // RTE: supervisor only.
                if !cpu.sup {
                    exception_at(cpu, bus, vectors::PRIVILEGE, CYCLES_EXCEPTION_SHORT, op_pc);
                    return;
                }
                rte_op(cpu, bus, op_pc);
            }
            4 => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
            5 => {
                // RTS.
                match pull_long(cpu, bus, op_pc) {
                    None => {}
                    Some(pc) => {
                        cpu.pc = pc;
                        fill(bus, 16);
                    }
                }
            }
            6 => {
                // TRAPV: the trap entry at vector 7 when taken.
                if cpu.over {
                    exception_short(cpu, bus, vectors::TRAPV, CYCLES_EXCEPTION_SHORT);
                    return;
                }
                fill(bus, 4);
            }
            _ => {
                // RTR: the condition-code word first, then the long.
                match pull_word(cpu, bus, op_pc) {
                    None => {}
                    Some(ccr) => match pull_long(cpu, bus, op_pc) {
                        None => {}
                        Some(pc) => {
                            cpu.set_ccr(ccr as u8);
                            cpu.pc = pc;
                            fill(bus, 20);
                        }
                    },
                }
            }
        },
        _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// LINK: push the frame pointer and take the displacement.
fn link_op<F: Flow>(cpu: &mut M68000, bus: &mut F, reg: u8, op_pc: u32) {
    let an = cpu.a[reg as usize];
    if !push_long(cpu, bus, an, op_pc) {
        return;
    }
    cpu.a[reg as usize] = cpu.sp();
    let disp = ext_disp(cpu, bus);
    cpu.set_sp(cpu.sp().wrapping_add(disp));
    fill(bus, 16);
}

/// UNLK: the frame pointer pulls the stack pointer.
fn unlk_op<F: Flow>(cpu: &mut M68000, bus: &mut F, reg: u8, op_pc: u32) {
    let an = cpu.a[reg as usize];
    if an & 1 != 0 {
        cpu.set_sp(an);
        address_error(cpu, bus, op_pc);
        return;
    }
    let v = bus.rlong(an);
    cpu.a[reg as usize] = v;
    cpu.set_sp(v);
    fill(bus, 12);
}

/// MOVE to or from the user stack pointer; supervisor only.
fn move_usp_op<F: Flow>(cpu: &mut M68000, bus: &mut F, reg: u8, to_usp: bool) {
    if to_usp {
        cpu.usp = cpu.a[reg as usize];
    } else {
        cpu.a[reg as usize] = cpu.usp;
    }
    fill(bus, 4);
}

/// JSR through the control-mode set with the return push.
fn jsr_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, op_pc: u32) {
    if !control_mode(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::L);
    let total = match ea.kind {
        Kind::Ind => 16,
        Kind::Index | Kind::Pci => 22,
        Kind::Absl => 20,
        _ => 18,
    };
    if !push_long(cpu, bus, cpu.pc, op_pc) {
        return;
    }
    cpu.pc = ea.addr;
    fill(bus, total);
}

/// JMP through the control-mode set.
fn jmp_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8) {
    let ea = ea_eval(cpu, bus, mode, reg, Size::L);
    let total = match ea.kind {
        Kind::Ind => 8,
        Kind::Index | Kind::Pci => 14,
        Kind::Absl => 12,
        _ => 10,
    };
    cpu.pc = ea.addr;
    fill(bus, total);
}

/// LEA through the control-mode set: no operand fetch, the address
/// lands whole in the register.
fn lea_op<F: Flow>(cpu: &mut M68000, bus: &mut F, mode: u8, reg: u8, dst: u8, op_pc: u32) {
    if !control_mode(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::L);
    let total = match ea.kind {
        Kind::Ind => 4,
        Kind::Index | Kind::Pci => 12,
        Kind::Absl => 12,
        _ => 8,
    };
    cpu.a[dst as usize] = ea.addr;
    fill(bus, total);
}

/// RTE: pull the status word then the return address (the pinned
/// order), reinstalling supervisor state and the mask.
fn rte_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op_pc: u32) {
    let Some(status) = pull_word(cpu, bus, op_pc) else {
        return;
    };
    let Some(pc) = pull_long(cpu, bus, op_pc) else {
        return;
    };
    cpu.set_sr_word(status);
    cpu.pc = pc;
    fill(bus, 20);
}

/// Line 0101: ADDQ/SUBQ over the register and memory forms with Scc
/// and DBcc in the size-3 rows.
fn line_five<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, mode: u8, reg: u8, op_pc: u32) {
    let op9 = ((op >> 9) & 7) as u8;
    let op2 = ((op >> 6) & 3) as u8;
    let bit8 = (op & 0x100) != 0;
    if op2 == 3 {
        if mode == 1 {
            // DBcc: the condition in the four top bits.
            dbcc_op(cpu, bus, ((op >> 8) & 0xF) as u8, reg);
            return;
        }
        scc_op(cpu, bus, ((op >> 8) & 0xF) as u8, mode, reg, op_pc);
        return;
    }
    // The quick data: the three-bit field counts 1 to 8, with 0
    // naming 8.
    let quick = if op9 == 0 { 8 } else { op9 };
    if bit8 {
        subq_op(cpu, bus, quick, op2, mode, reg, op_pc);
    } else {
        addq_op(cpu, bus, quick, op2, mode, reg, op_pc);
    }
}

/// ADDQ over data, address, and memory forms.
fn addq_op<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    quick: u8,
    size_bits: u8,
    mode: u8,
    reg: u8,
    op_pc: u32,
) {
    let size = imm_size(size_bits);
    if mode == 1 {
        // Address registers: word or long sizes, flag-free.
        if size == Size::B {
            exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
            return;
        }
        cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_add(u32::from(quick));
        fill(bus, 8);
        return;
    }
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let dst = ea_read(cpu, bus, ea, size);
    let result = add_flags(cpu, size, dst, u32::from(quick), 0);
    ea_store(cpu, bus, ea, size, result);
    fill(bus, quick_total(size, ea));
}

/// SUBQ over data, address, and memory forms.
fn subq_op<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    quick: u8,
    size_bits: u8,
    mode: u8,
    reg: u8,
    op_pc: u32,
) {
    let size = imm_size(size_bits);
    if mode == 1 {
        if size == Size::B {
            exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
            return;
        }
        cpu.a[reg as usize] = cpu.a[reg as usize].wrapping_sub(u32::from(quick));
        fill(bus, 8);
        return;
    }
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let dst = ea_read(cpu, bus, ea, size);
    let result = sub_flags(cpu, size, dst, u32::from(quick), 0);
    ea_store(cpu, bus, ea, size, result);
    fill(bus, quick_total(size, ea));
}

/// The quick totals: registers 4 and 8; memory 8 + EA and 12 + EA.
fn quick_total(size: Size, ea: Ea) -> u64 {
    match ea.kind {
        Kind::Dn | Kind::An => match size {
            Size::L => 8,
            _ => 4,
        },
        _ => match size {
            Size::L => 12 + ea.kind.read_cost(size),
            _ => 8 + ea.kind.read_cost(size),
        },
    }
}

/// Scc: 0xFF when true, 0 when false, over the data-alterable set.
fn scc_op<F: Flow>(cpu: &mut M68000, bus: &mut F, cc: u8, mode: u8, reg: u8, op_pc: u32) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let taken = condition(cpu, cc);
    let ea = ea_eval(cpu, bus, mode, reg, Size::B);
    let value = u32::from(taken) * 0xFF;
    if ea.kind == Kind::Dn {
        cpu.d[ea.reg as usize] = cpu.d[ea.reg as usize] & 0xFFFF_FF00 | value;
    } else {
        ea_store(cpu, bus, ea, Size::B, value);
    }
    let total = match (ea.kind, taken) {
        (Kind::Dn | Kind::An, true) => 6,
        (Kind::Dn | Kind::An, false) => 4,
        _ => 8 + ea.kind.read_cost(Size::B),
    };
    fill(bus, total);
}

/// DBcc: the condition-true exit and the count-then-branch loop.
fn dbcc_op<F: Flow>(cpu: &mut M68000, bus: &mut F, cc: u8, reg: u8) {
    if condition(cpu, cc) {
        // The condition holds: consume the extension word and exit.
        let _ = ext_word(cpu, bus);
        fill(bus, 12);
        return;
    }
    let count = cpu.d[reg as usize].wrapping_sub(1) & 0xFFFF;
    cpu.d[reg as usize] = cpu.d[reg as usize] & 0xFFFF_0000 | count;
    // The displacement reads from the instruction's base: the target
    // counts from the address of the extension word.
    let base = cpu.pc;
    let offset = ext_disp(cpu, bus);
    if count != 0xFFFF {
        cpu.pc = base.wrapping_add(offset);
        fill(bus, 10);
    } else {
        fill(bus, 14);
    }
}

/// Bcc: the taken totals stay 10 for both displacement forms; the
/// not-taken rows are 8 for the short form and 12 for the word form.
fn branch_op<F: Flow>(cpu: &mut M68000, bus: &mut F, taken: bool, op: u16) {
    let short = op & 0xFF;
    if short == 0 {
        // The word displacement counts from the extension word's own
        // address.
        let base = cpu.pc;
        let offset = ext_disp(cpu, bus);
        if taken {
            cpu.pc = base.wrapping_add(offset);
            fill(bus, 10);
        } else {
            fill(bus, 12);
        }
        return;
    }
    if taken {
        cpu.pc = cpu.pc.wrapping_add(sext8(u32::from(short)));
        fill(bus, 10);
    } else {
        fill(bus, 8);
    }
}

/// BSR: push the return address after the displacement words.
fn bsr_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let short = op & 0xFF;
    let return_pc;
    if short == 0 {
        // The word displacement counts from the extension word's own
        // address.
        let base = cpu.pc;
        let offset = ext_disp(cpu, bus);
        return_pc = cpu.pc;
        cpu.pc = base.wrapping_add(offset);
    } else {
        return_pc = cpu.pc;
        cpu.pc = cpu.pc.wrapping_add(sext8(u32::from(short)));
    }
    if !push_long(cpu, bus, return_pc, op_pc) {
        return;
    }
    fill(bus, 18);
}

/// MOVEQ: the data byte sign-extends into the register.
fn moveq_op(cpu: &mut M68000, dst: u8, data: u8) {
    let value = sext8(u32::from(data));
    cpu.d[dst as usize] = value;
    set_nz_value(cpu, Size::L, value);
    cpu.over = false;
    cpu.carry = false;
}

/// The ALU lines 8, 9, and D: OR, SUB, and ADD in their row shapes.
/// Line 8's size-3 rows divide and the extended rows carry SBCD and
/// ADDX and SUBX; the structure carries both.
fn alu_line<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let line = (op >> 12) as u8;
    let reg9 = ((op >> 9) & 7) as u8;
    let op6 = ((op >> 6) & 7) as u8;
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    match op6 {
        // The extended rows: the register pairs at mode 0 and the
        // predecrement pairs at mode 1, for SUBX, SBCD, ADDX, and the
        // CMPM pairs of line 1011. The row's own register-to-memory
        // forms ride the same slots with a memory effective address.
        4..=6 if mode < 2 => match line {
            // SBCD: byte rows only; the word and long slots are
            // reserved.
            8 => {
                if op6 == 4 {
                    if mode == 1 {
                        extended_mem(cpu, bus, op, false, true);
                    } else {
                        extended_reg(cpu, bus, op, false, true);
                    }
                } else {
                    exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
                }
            }
            0x9 | 0xD => {
                if mode == 1 {
                    extended_mem(cpu, bus, op, line == 0xD, false);
                } else {
                    extended_reg(cpu, bus, op, line == 0xD, false);
                }
            }
            _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
        },
        // The to-register rows.
        0..=2 => {
            let size = size_of_op6(op6);
            let okay = match line {
                // OR, SUB, and ADD take the data-alterable set
                // without An direct; the PC and immediate sources
                // extend it for the read-only forms.
                8 => data_alterable(mode, reg) || read_source_set_extra(mode, reg),
                _ => data_alterable(mode, reg) || mode == 7 && reg > 3,
            };
            if !okay {
                exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
                return;
            }
            let dst_reg = reg9 as usize;
            let ea = ea_eval(cpu, bus, mode, reg, size);
            let dst = cpu.d[dst_reg];
            let src = ea_read(cpu, bus, ea, size);
            let long_reg_source = size == Size::L && (ea.kind == Kind::Dn || ea.kind == Kind::Imm);
            match line {
                8 => {
                    let result = dst | src;
                    set_logic_flags(cpu, size, result);
                    cpu.d[dst_reg] = size.into_data(dst, result);
                }
                0x9 => {
                    let result = sub_flags(cpu, size, dst, src, 0);
                    cpu.d[dst_reg] = size.into_data(dst, result);
                }
                _ => {
                    let result = add_flags(cpu, size, dst, src, 0);
                    cpu.d[dst_reg] = size.into_data(dst, result);
                }
            }
            fill(bus, alu_dn_total(size, ea, long_reg_source));
        }
        // The address-register rows of SUB and ADD; the divide rows
        // of line 8 land in the same slots.
        3 => {
            if line == 8 {
                div_op(cpu, bus, reg9, mode, reg, op_pc, false);
                return;
            }
            addr_line(cpu, bus, op, op_pc);
        }
        // The register-to-memory rows.
        4..=6 => {
            let size = size_of_op6(op6 - 4);
            let okay = mode != 1 && mode != 0 && data_alterable(mode, reg);
            if !okay {
                exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
                return;
            }
            let src_reg = reg9 as usize;
            let ea = ea_eval(cpu, bus, mode, reg, size);
            let src = cpu.reg_load(size.reg_bits(), src_reg, false);
            let dst = ea_read(cpu, bus, ea, size);
            let result = match line {
                0x8 => dst | src,
                0x9 => sub_flags(cpu, size, dst, src, 0),
                _ => add_flags(cpu, size, dst, src, 0),
            };
            if line == 8 {
                set_logic_flags(cpu, size, result);
            }
            ea_store(cpu, bus, ea, size, result);
            let total = match size {
                Size::L => 12 + ea.kind.read_cost(size),
                _ => 8 + ea.kind.read_cost(size),
            };
            fill(bus, total);
        }
        // The long address-register rows and the signed divide.
        7 => {
            if line == 8 {
                div_op(cpu, bus, reg9, mode, reg, op_pc, true);
                return;
            }
            addr_line(cpu, bus, op, op_pc);
        }
        _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// The PC-relative and immediate sources that extend the plain set.
fn read_source_set_extra(mode: u8, reg: u8) -> bool {
    mode == 7 && (2..=4).contains(&reg)
}

/// The op-6 size decode for the ALU rows: 0 byte, 1 word, 2 long.
fn size_of_op6(op6: u8) -> Size {
    match op6 {
        1 => Size::W,
        2 => Size::L,
        _ => Size::B,
    }
}

/// The ALU-to-register totals with the raised long base for register
/// and immediate sources.
fn alu_dn_total(size: Size, ea: Ea, long_reg_source: bool) -> u64 {
    let ear = ea.kind.read_cost(size);
    match size {
        Size::L if long_reg_source => 8 + ear,
        Size::L => 6 + ear,
        _ => 4 + ear,
    }
}

/// ADDA/SUBA over the word and long rows: the source adds to or
/// subtracts from the address register, flag-free. The published long
/// row raises its 6 to 8 for the register-direct and immediate
/// sources.
fn addr_line<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let line = (op >> 12) as u8;
    let is_long = (op >> 6) & 7 == 7;
    let size = if is_long { Size::L } else { Size::W };
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    if !read_source_set(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let src = ea_read(cpu, bus, ea, size);
    let dst = ((op >> 9) & 7) as usize;
    let value = cpu.a[dst];
    let result = match line {
        0x9 => value.wrapping_sub(src),
        _ => value.wrapping_add(src),
    };
    cpu.a[dst] = result;
    let ear = ea.kind.read_cost(size);
    let raised = is_long && (ea.kind == Kind::Dn || ea.kind == Kind::An || ea.kind == Kind::Imm);
    let total = if raised {
        8 + ear
    } else if is_long {
        6 + ear
    } else {
        8 + ear
    };
    fill(bus, total);
}

/// CMPA in the word and long rows: the comparison installs the flags
/// and the extend bit stays.
fn cmpa_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let is_long = (op >> 6) & 7 == 7;
    let size = if is_long { Size::L } else { Size::W };
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    if !read_source_set(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let src = ea_read(cpu, bus, ea, size);
    let value = cpu.a[((op >> 9) & 7) as usize];
    sub_flags(cpu, size, value, src, 0);
    cpu.extend = false;
    fill(bus, 6 + ea.kind.read_cost(size));
}

/// Line 1011: CMP to registers, CMPA, EOR in all shapes, CMPM.
fn line_b<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let reg9 = ((op >> 9) & 7) as u8;
    let op6 = ((op >> 6) & 7) as u8;
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    match op6 {
        // The compare rows to a data register (bit 8 clear).
        0..=2 => cmp_op(cpu, bus, op6, reg9, mode, reg, op_pc),
        3 => cmpa_op(cpu, bus, op, op_pc),
        // The EOR rows with the register as the source share the
        // slots with the post-increment CMPM pairs at mode 1.
        4..=6 if mode == 1 => cmpm_op(cpu, bus, op),
        4..=6 => eor_op(cpu, bus, op6 - 4, reg9, mode, reg, op_pc),
        7 => cmpa_op(cpu, bus, op, op_pc),
        _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// CMP of an operand against a data register.
fn cmp_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op6: u8, dst: u8, mode: u8, reg: u8, op_pc: u32) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let size = size_of_op6(op6);
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let value = ea_read(cpu, bus, ea, size);
    let dst_value = cpu.d[dst as usize];
    sub_flags(cpu, size, dst_value, value, 0);
    cpu.extend = false;
    let ear = ea.kind.read_cost(size);
    let total = match size {
        Size::L => 6 + ear,
        _ => 4 + ear,
    };
    fill(bus, total);
}

/// EOR of a data register into a register or memory destination. The
/// register-source column only allows the data register rows.
fn eor_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op6: u8, src: u8, mode: u8, reg: u8, op_pc: u32) {
    let size = match op6 {
        1 => Size::W,
        2 => Size::L,
        _ => Size::B,
    };
    if mode == 0 {
        // EOR register, register.
        let dst = (reg & 7) as usize;
        let result = cpu.d[dst] ^ cpu.d[src as usize];
        cpu.d[dst] = size.into_data(cpu.d[dst], result);
        set_logic_flags(cpu, size, result);
        let total = match size {
            Size::L => 8,
            _ => 4,
        };
        fill(bus, total);
        return;
    }
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let dst = ea_read(cpu, bus, ea, size);
    let src_value = cpu.d[src as usize];
    let result = dst ^ src_value;
    set_logic_flags(cpu, size, result);
    ea_store(cpu, bus, ea, size, result);
    let total = match size {
        Size::L => 12 + ea.kind.read_cost(size),
        _ => 8 + ea.kind.read_cost(size),
    };
    fill(bus, total);
}

/// CMPM through the post-increment pair rows: both EAs are An with
/// the post-increment, the registers named by the two fields.
fn cmpm_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16) {
    let size = imm_size(((op >> 6) & 3) as u8);
    let dst_an = ((op >> 9) & 7) as usize;
    let src_an = (op & 7) as usize;
    // Both EAs read from their registers at the pre-increment value
    // and then step.
    let dst_at = cpu.a[dst_an];
    cpu.a[dst_an] = dst_at.wrapping_add(size.step(0));
    let src_at = cpu.a[src_an];
    cpu.a[src_an] = src_at.wrapping_add(size.step(0));
    let dst = match size {
        Size::W => u32::from(bus.rword(dst_at)),
        _ => bus.rlong(dst_at),
    };
    let src = match size {
        Size::W => u32::from(bus.rword(src_at)),
        _ => bus.rlong(src_at),
    };
    sub_flags(cpu, size, dst, src, 0);
    cpu.extend = false;
    let total = match size {
        Size::L => 20,
        _ => 12,
    };
    fill(bus, total);
}

/// Line 1100: AND in all row shapes, with ABCD, the multiply rows,
/// and the exchange forms in their slots.
fn c_line<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let reg9 = ((op >> 9) & 7) as u8;
    let op6 = ((op >> 6) & 7) as u8;
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    match op6 {
        // The to-register AND rows.
        0..=2 => and_dn(cpu, bus, op6, reg9, mode, reg, op_pc),
        // MULU: the register in bits 11-9, the read-source set.
        3 => mul_op(cpu, bus, reg9, mode, reg, op_pc, false),
        4 => {
            if mode < 2 {
                // ABCD: byte rows, the register form at mode 0 and
                // the predecrement pair at mode 1.
                abcd_op(cpu, bus, op, mode == 1);
                return;
            }
            and_mem(cpu, bus, 0, reg9, mode, reg, op_pc);
        }
        5 => {
            if (op & 0xF1F8) == 0xC140 {
                exg_op(cpu, bus, reg9, (op & 7) as u8, false, false);
                return;
            }
            if (op & 0xF1F8) == 0xC148 {
                exg_op(cpu, bus, reg9, (op & 7) as u8, false, true);
                return;
            }
            and_mem(cpu, bus, 1, reg9, mode, reg, op_pc);
        }
        6 => {
            if (op & 0xF1F8) == 0xC188 {
                exg_op(cpu, bus, reg9, (op & 7) as u8, true, true);
                return;
            }
            if mode < 2 {
                exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
                return;
            }
            and_mem(cpu, bus, 2, reg9, mode, reg, op_pc);
        }
        // MULS: the signed rows in the bit-8 slot.
        7 => mul_op(cpu, bus, reg9, mode, reg, op_pc, true),
        _ => exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc),
    }
}

/// AND an operand into a data register.
fn and_dn<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    op6: u8,
    dst_reg: u8,
    mode: u8,
    reg: u8,
    op_pc: u32,
) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let size = size_of_op6(op6);
    let dst = dst_reg as usize;
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let dst_value = cpu.d[dst];
    let src = ea_read(cpu, bus, ea, size);
    let result = dst_value & src;
    set_logic_flags(cpu, size, result);
    cpu.d[dst] = size.into_data(dst_value, result);
    let long_reg_source = size == Size::L && (ea.kind == Kind::Dn || ea.kind == Kind::Imm);
    fill(bus, alu_dn_total(size, ea, long_reg_source));
}

/// AND a data register into a memory destination.
fn and_mem<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    size_index: u8,
    src_reg: u8,
    mode: u8,
    reg: u8,
    op_pc: u32,
) {
    if mode == 1 || !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let size = size_of_op6(size_index);
    let ea = ea_eval(cpu, bus, mode, reg, size);
    let src_value = cpu.reg_load(size.reg_bits(), src_reg as usize, false);
    let dst = ea_read(cpu, bus, ea, size);
    let result = dst & src_value;
    set_logic_flags(cpu, size, result);
    ea_store(cpu, bus, ea, size, result);
    let total = match size {
        Size::L => 12 + ea.kind.read_cost(size),
        _ => 8 + ea.kind.read_cost(size),
    };
    fill(bus, total);
}

/// EXG of two registers in the three published pairs. Each pair's
/// fields name the registers: bits 11-9 then bits 2-0.
fn exg_op<F: Flow>(cpu: &mut M68000, bus: &mut F, x: u8, y: u8, x_addr: bool, y_addr: bool) {
    let xa = x as usize;
    let ya = y as usize;
    if x_addr && y_addr {
        cpu.a.swap(xa, ya);
    } else if x_addr {
        // EXG Dx,Ay: the data and address registers exchange whole.
        std::mem::swap(&mut cpu.a[xa], &mut cpu.d[ya]);
    } else {
        cpu.d.swap(xa, ya);
    }
    fill(bus, 6);
}

/// ABCD over the register and predecrement pair forms.
fn abcd_op<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, memory: bool) {
    let x = ((op >> 9) & 7) as usize;
    let y = (op & 7) as usize;
    if !memory {
        let result = bcd_add_result(cpu, cpu.d[y] & 0xFF, cpu.d[x] & 0xFF);
        cpu.d[x] = cpu.d[x] & 0xFFFF_FF00 | result;
        fill(bus, 6);
        return;
    }
    let src_addr = cpu.a[y].wrapping_sub(Size::B.step(if y == 7 { 7 } else { y as u8 }));
    cpu.a[y] = src_addr;
    let dst_addr = cpu.a[x].wrapping_sub(Size::B.step(if x == 7 { 7 } else { x as u8 }));
    cpu.a[x] = dst_addr;
    let src_byte = bus.rbyte(src_addr);
    let dst_byte = bus.rbyte(dst_addr);
    let result = bcd_add_result(cpu, u32::from(src_byte), u32::from(dst_byte));
    bus.wbyte(dst_addr, result as u8);
    fill(bus, 18);
}

/// The BCD add with the published flag shape. Z clears only.
fn bcd_add_result(cpu: &mut M68000, src: u32, dst: u32) -> u32 {
    let x = u32::from(cpu.extend);
    let mut low = (dst & 0xF) + (src & 0xF) + x;
    let mut carry_low = 0u32;
    if low > 9 {
        carry_low = 1;
        low += 6;
    }
    let mut res = low + ((dst >> 4) & 0xF) + ((src >> 4) & 0xF) + (carry_low << 4);
    let over = res > 0x9F;
    if over {
        res -= 0xA0;
    }
    cpu.carry = over;
    cpu.extend = over;
    cpu.over = false;
    cpu.neg = res & 0x80 != 0;
    if res != 0 {
        cpu.zero = false;
    }
    res & 0xFF
}

/// The BCD subtract with the borrow chain and the published flag
/// shape. Z clears only; C and X take the borrow out; N rides the
/// result's top bit.
fn bcd_sub_result(cpu: &mut M68000, src: u32, dst: u32) -> u32 {
    let mut low = (dst & 0xF)
        .wrapping_sub(src & 0xF)
        .wrapping_sub(u32::from(cpu.extend));
    let mut corf = 0u32;
    if low > 0xF {
        corf = 6;
    }
    low = low
        .wrapping_add((dst >> 4) & 0xF)
        .wrapping_sub((src >> 4) & 0xF);
    let over = low > 0xFF;
    if over {
        low = low.wrapping_add(0xA0);
    }
    cpu.carry = over || low < corf;
    cpu.extend = cpu.carry;
    cpu.over = false;
    let res = (low.wrapping_sub(corf)) & 0xFF;
    cpu.neg = res & 0x80 != 0;
    if res != 0 {
        cpu.zero = false;
    }
    res
}

/// MULU/MULS word forms over the read operand set; the maximum row
/// pays the published 70.
fn mul_op<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    dst: u8,
    mode: u8,
    reg: u8,
    op_pc: u32,
    signed: bool,
) {
    if !read_source_set(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::W);
    let src = ea_read(cpu, bus, ea, Size::W);
    let dst_reg = dst as usize;
    let result = mul_result(cpu, dst_reg, src, signed);
    cpu.d[dst_reg] = result;
    set_nz_value(cpu, Size::L, result);
    cpu.over = false;
    cpu.carry = false;
    fill(bus, 70 + ea.kind.read_cost(Size::W));
}

/// The multiply product with the signed and unsigned shapes.
fn mul_result(cpu: &M68000, dst_reg: usize, src: u32, signed: bool) -> u32 {
    if signed {
        let a = i64::from(cpu.d[dst_reg] as u16 as i16 as i32);
        let b = i64::from(src as u16 as i16 as i32);
        (a.wrapping_mul(b) as u64 & 0xFFFF_FFFF) as u32
    } else {
        let a = cpu.d[dst_reg] & 0xFFFF;
        let b = src & 0xFFFF;
        a.wrapping_mul(b)
    }
}

/// Division over the word rows: the quotient and remainder land in
/// the register, the divide fault raises vector 5, overflow leaves V.
fn div_op<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    dst: u8,
    mode: u8,
    reg: u8,
    op_pc: u32,
    signed: bool,
) {
    if !read_source_set(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::W);
    let divisor = ea_read(cpu, bus, ea, Size::W);
    let dst_reg = dst as usize;
    let ear = ea.kind.read_cost(Size::W);
    if divisor == 0 {
        // The divide fault with the operands unchanged.
        cpu.over = false;
        cpu.carry = false;
        cpu.zero = false;
        cpu.neg = false;
        exception_short(cpu, bus, vectors::DIVIDE_BY_ZERO, CYCLES_DIVIDE_FAULT + ear);
        return;
    }
    match div_result(cpu, dst_reg, divisor, signed) {
        DivResult::Overflow => {
            cpu.over = true;
            cpu.carry = false;
            cpu.zero = false;
            cpu.neg = false;
        }
        DivResult::Quotient {
            quotient,
            remainder,
        } => {
            cpu.over = false;
            cpu.carry = false;
            cpu.zero = quotient == 0;
            cpu.neg = quotient & 0x8000 != 0;
            cpu.d[dst_reg] = (remainder << 16) | quotient;
        }
    }
    let total = if signed { 158 } else { 140 };
    fill(bus, total + ear);
}

/// The divide result shape.
enum DivResult {
    Overflow,
    Quotient { quotient: u32, remainder: u32 },
}

/// The word division with the truncated signed shape.
fn div_result(cpu: &M68000, dst_reg: usize, divisor: u32, signed: bool) -> DivResult {
    let dividend = cpu.d[dst_reg];
    if signed {
        let a = i64::from(dividend as i32);
        let b = i64::from(divisor as u16 as i16 as i32);
        if a == i64::from(i32::MIN) && b == -1 {
            return DivResult::Overflow;
        }
        let quotient = a / b;
        let remainder = a % b;
        if !(-0x8000..=0x7FFF).contains(&quotient) {
            return DivResult::Overflow;
        }
        DivResult::Quotient {
            quotient: (quotient as u32) & 0xFFFF,
            remainder: (remainder as u32) & 0xFFFF,
        }
    } else {
        let a = u64::from(dividend);
        let b = u64::from(divisor & 0xFFFF);
        let quotient = a / b;
        if quotient > 0xFFFF {
            return DivResult::Overflow;
        }
        DivResult::Quotient {
            quotient: quotient as u32,
            remainder: (a % b) as u32,
        }
    }
}

/// Line 1110: the shift and rotate rows.
///
/// Register forms: bits 7-6 hold the size, bit 8 the direction (1
/// shifts left), bits 4-3 the operation (0 arith, 1 logical, 2
/// rotate-through-extend, 3 rotate), bit 5 the dynamic count flag,
/// bits 11-9 the count (0 names 8) or the count register, and bits
/// 2-0 the destination register. Memory forms reuse bits 7-6 as 3
/// with an operation of bits 11-9 and a data-alterable word
/// destination in bits 5-0.
fn shift_line<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, op_pc: u32) {
    let size_bits = ((op >> 6) & 3) as u8;
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let direction = (op & 0x100) != 0;
    let dynamic = (op & 0x20) != 0;
    if size_bits == 3 {
        let operation = ((op >> 9) & 7) as u8;
        shift_mem_op(cpu, bus, operation, direction, mode, reg, op_pc);
        return;
    }
    let operation = ((op >> 3) & 3) as u8;
    let size = shift_size(size_bits);
    let dst = reg as usize;
    let count = if dynamic {
        u64::from(cpu.d[((op >> 9) & 7) as usize] & 0x3F)
    } else {
        let encoded = ((op >> 9) & 7) as u8;
        u64::from(if encoded == 0 { 8 } else { encoded })
    };
    let (result, carry) = shift_reg_run(cpu, dst, operation, direction, size, count);
    let value = cpu.d[dst] & size.mask();
    let width = match size {
        Size::B => 8u32,
        Size::W => 16,
        Size::L => 32,
    };
    cpu.d[dst] = size.into_data(cpu.d[dst], result);
    set_nz_value(cpu, size, result);
    cpu.over = false;
    cpu.carry = carry;
    if operation == 0 && direction && count != 0 {
        // The left arithmetic overflow: the bits the shift moves past
        // the top, together with the one that lands in the sign, must
        // run as a clean copy of the sign.
        let steps = count.min(64);
        cpu.over = if steps < u64::from(width) {
            let kept = value >> (width - steps as u32 - 1);
            !(kept == 0 || kept == (1u32 << (steps + 1)) - 1)
        } else {
            value != 0
        };
    }
    if operation != 3 && count != 0 {
        // The shifts and the extended rotates carry X with the last
        // bit out; the plain rotates leave X.
        cpu.extend = carry;
    }
    let base = if size == Size::L { 8 } else { 6 };
    fill(bus, base + 2 * count);
}

/// The memory shift form: one word step through a memory operand.
fn shift_mem_op<F: Flow>(
    cpu: &mut M68000,
    bus: &mut F,
    operation: u8,
    direction: bool,
    mode: u8,
    reg: u8,
    op_pc: u32,
) {
    if !data_alterable(mode, reg) {
        exception_at(cpu, bus, vectors::ILLEGAL, CYCLES_EXCEPTION_SHORT, op_pc);
        return;
    }
    let ea = ea_eval(cpu, bus, mode, reg, Size::W);
    let value = ea_read(cpu, bus, ea, Size::W);
    let (result, carry) = shift_word_once(cpu.extend, operation, direction, value);
    cpu.zero = result == 0;
    cpu.neg = result & 0x8000 != 0;
    cpu.over = false;
    if operation == 0 && direction {
        // The one-step left arithmetic overflow: the sign and the bit
        // that enters it must agree.
        let kept = (value & 0xFFFF) >> 14;
        cpu.over = !(kept == 0 || kept == 3);
    }
    cpu.carry = carry;
    if operation != 3 {
        // The shifts and the extended rotates carry X with the bit
        // out; the plain rotates leave X.
        cpu.extend = carry;
    }
    ea_store(cpu, bus, ea, Size::W, result);
    let total = 8 + ea.kind.read_cost(Size::W);
    fill(bus, total);
}

/// The shift family's size decode.
fn shift_size(size_bits: u8) -> Size {
    match size_bits {
        1 => Size::W,
        2 => Size::L,
        _ => Size::B,
    }
}

/// Run a register shift or rotate for `count` steps with the
/// published flag behavior: shifts drive C and X from the last bit
/// moved out; the extended rotates keep X through every step; a zero
/// count clears C and leaves N Z against the original value; a left
/// shift signals overflow on the sign change.
fn shift_reg_run(
    cpu: &M68000,
    dst: usize,
    operation: u8,
    direction: bool,
    size: Size,
    count: u64,
) -> (u32, bool) {
    let width = match size {
        Size::B => 8u32,
        Size::W => 16,
        Size::L => 32,
    };
    let mask = size.mask();
    let value = cpu.d[dst] & mask;
    if count == 0 {
        // The zero count hands the value back; C clears and X, N, and
        // Z hold their state.
        return (value, false);
    }
    let sign_bit = size.sign_bit();
    let sign_fill = operation == 0 && (value & sign_bit) != 0;
    let mut xbit = u32::from(cpu.extend);
    let mut r = value;
    let mut carry = false;
    for _ in 0..count.min(64) {
        if direction {
            // The bit out of the top goes to X and C; the round
            // entry comes from the extend for the extended rotate,
            // from the own top bit for the plain rotate, and from
            // zero for the shifts.
            let top = (r >> (width - 1)) & 1;
            let in_bit = match operation {
                2 => xbit,
                3 => top,
                _ => 0,
            };
            r = (r << 1) & mask | in_bit;
            xbit = top;
            carry = top != 0;
        } else {
            let bottom = r & 1;
            let fill = match operation {
                0 => {
                    if sign_fill {
                        sign_bit
                    } else {
                        0
                    }
                }
                2 => xbit << (width - 1),
                3 => bottom << (width - 1),
                _ => 0,
            };
            r = (r >> 1) | fill;
            xbit = bottom;
            carry = bottom != 0;
        }
    }
    (r & mask, carry)
}

/// One shift of a word for the memory form with the operation's own
/// fill: the arithmetic shapes ride the sign, the rotates bring the
/// extend or the own bit around. Returns the result and the bit out.
fn shift_word_once(extend: bool, operation: u8, direction: bool, value: u32) -> (u32, bool) {
    if direction {
        let top = (value >> 15) & 1;
        let in_bit = if operation == 2 {
            u32::from(extend)
        } else if operation == 3 {
            top
        } else {
            0
        };
        ((value << 1) & 0xFFFF | in_bit, top != 0)
    } else {
        let bottom = value & 1;
        let fill = if operation == 0 {
            u32::from(value & 0x8000 != 0) << 15
        } else if operation == 2 {
            u32::from(extend) << 15
        } else if operation == 3 {
            bottom << 15
        } else {
            0
        };
        ((value >> 1) | fill, bottom != 0)
    }
}

/// ADDX/SUBX/SBCD register rows through the extend flag with the
/// multiprecision Z behavior.
fn extended_reg<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, add: bool, bcd: bool) {
    let dst = ((op >> 9) & 7) as usize;
    let src = (op & 7) as usize;
    if bcd {
        // The SBCD byte rows: the decimal subtract with the borrow.
        let result = bcd_sub_result(cpu, cpu.d[src] & 0xFF, cpu.d[dst] & 0xFF);
        cpu.d[dst] = cpu.d[dst] & 0xFFFF_FF00 | result;
        fill(bus, 6);
        return;
    }
    // The register rows: byte, word, long in the size rows.
    let size = match ((op >> 6) & 3) as u8 {
        1 => Size::W,
        2 => Size::L,
        _ => Size::B,
    };
    let a = cpu.d[dst];
    let b = cpu.d[src];
    // Z accumulates in the extended forms: it clears on a nonzero
    // result and keeps its old value otherwise.
    let old_zero = cpu.zero;
    let result = match add {
        true => add_flags(cpu, size, a, b, u32::from(cpu.extend)),
        false => sub_flags(cpu, size, a, b, u32::from(cpu.extend)),
    };
    cpu.zero = if result != 0 { false } else { old_zero };
    cpu.d[dst] = size.into_data(a, result);
    fill(bus, extended_reg_total(size));
}

/// The ADDX/SUBX register totals: 4 for byte and word, 8 for long.
fn extended_reg_total(size: Size) -> u64 {
    match size {
        Size::L => 8,
        _ => 4,
    }
}

/// ADDX/SUBX/ABCD/SBCD memory rows through the predecrement pair.
fn extended_mem<F: Flow>(cpu: &mut M68000, bus: &mut F, op: u16, add: bool, bcd: bool) {
    let x = ((op >> 9) & 7) as usize;
    let y = (op & 7) as usize;
    let size = match ((op >> 6) & 3) as u8 {
        1 => Size::W,
        2 => Size::L,
        _ => Size::B,
    };
    let step = size.step(0);
    let src_addr = cpu.a[y].wrapping_sub(step);
    cpu.a[y] = src_addr;
    let dst_addr = cpu.a[x].wrapping_sub(step);
    cpu.a[x] = dst_addr;
    if bcd {
        // The BCD byte rows through the memory.
        let src_byte = bus.rbyte(src_addr);
        let dst_byte = bus.rbyte(dst_addr);
        let result = if add {
            bcd_add_result(cpu, u32::from(src_byte), u32::from(dst_byte))
        } else {
            bcd_sub_result(cpu, u32::from(src_byte), u32::from(dst_byte))
        };
        bus.wbyte(dst_addr, result as u8);
        fill(bus, 18);
        return;
    }
    let src = match size {
        Size::W => u32::from(bus.rword(src_addr)),
        _ => bus.rlong(src_addr),
    };
    let dst = match size {
        Size::W => u32::from(bus.rword(dst_addr)),
        _ => bus.rlong(dst_addr),
    };
    // Z accumulates in the extended forms: it clears on a nonzero
    // result and keeps its old value otherwise.
    let old_zero = cpu.zero;
    let result = match add {
        true => add_flags(cpu, size, dst, src, u32::from(cpu.extend)),
        false => sub_flags(cpu, size, dst, src, u32::from(cpu.extend)),
    };
    cpu.zero = if result != 0 { false } else { old_zero };
    match size {
        Size::W => bus.wword(dst_addr, result as u16),
        _ => bus.wlong(dst_addr, result),
    }
    let total = match size {
        Size::L => 30,
        _ => 18,
    };
    fill(bus, total);
}
