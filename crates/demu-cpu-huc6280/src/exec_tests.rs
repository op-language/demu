//! The instruction-level tests over a plain byte map.
//!
//! The tests drive the same step function the machine uses through a
//! small test bus over the die's 21-bit physical addresses: every byte
//! defaults to zero and the reads and writes log in cycle order, so the
//! asserted cycle totals cover the published table cells and the
//! asserted addresses cover the die's mapping. The tests place their
//! data through the die's own [`Huc6280::route`], so the asserted
//! addresses state the mapping rules directly: the boot reads the
//! reset vector from bank zero at 0x1FFE; the program sits in the
//! logical page 7 bank, which the core fetches through MPR 7's reset
//! bank of zero; the zero page and the stack sit in MPR 1's 8K window
//! at 0x1F0000; and the die's hardware page answers at the MPR 0
//! offsets it intercepts.

use super::*;
use crate::cpu::{AccessRule, Cycles, Huc6280, MASK_IRQ1, MASK_IRQ2, MASK_TIMER};
use crate::cpu::{FLAG_B, FLAG_C, FLAG_D, FLAG_I, FLAG_N, FLAG_T, FLAG_V, FLAG_Z};
use demu_config::model::{Bus, DeviceInstance, SystemConfig};
use demu_core::Level;

/// The program position the test boots into: the logical page 7 bank,
/// which the die fetches through MPR 7's reset bank of zero.
const PROGRAM: u16 = 0xE000;

/// The reset vector's physical address in bank zero.
const VEC_RESET_PHYS: u32 = 0x1FFE;

/// The zero-page window of MPR 1's reset bank.
const ZP_BASE: u32 = 0x1F0000;

/// A plain byte map over the die's physical addresses for the tests.
#[derive(Default)]
struct TestBus {
    mem: Vec<u8>,
    k: u32,
    reads: Vec<u32>,
    writes: Vec<(u32, u8)>,
}

impl TestBus {
    fn store(&mut self, addr: u32, value: u8) {
        let index = addr as usize;
        if index >= self.mem.len() {
            self.mem.resize(index + 1, 0);
        }
        self.mem[index] = value;
    }

    fn load(&mut self, addr: u32, bytes: &[u8]) {
        for (i, b) in bytes.iter().enumerate() {
            self.store(addr + i as u32, *b);
        }
    }

    fn byte(&self, addr: u32) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }

    fn read_count(&self, addr: u32) -> usize {
        self.reads.iter().filter(|a| **a == addr).count()
    }

    fn write_at(&self, index: usize) -> (u32, u8) {
        self.writes[index]
    }
}

impl Cycles for TestBus {
    fn rd(&mut self, addr: u32) -> u8 {
        self.k += 1;
        self.reads.push(addr);
        self.byte(addr)
    }

    fn wr(&mut self, addr: u32, value: u8) {
        self.k += 1;
        self.writes.push((addr, value));
        self.store(addr, value);
    }

    fn wait(&mut self) {
        self.k += 1;
    }

    fn cycles(&self) -> u32 {
        self.k
    }

    fn emit(&mut self, _event: demu_core::triggers::TriggerEvent) {}
}

fn cpu_of() -> Huc6280 {
    use demu_config::newtypes::{DeviceName, KindName};
    let instance = DeviceInstance {
        name: DeviceName::new("cpu0").expect("device name"),
        kind: KindName::new("huc6280.cpu").expect("kind name"),
        domain: "main".to_string(),
        params: Default::default(),
        dividers: Default::default(),
    };
    let config = SystemConfig {
        name: "test".to_string(),
        triple: "huc6280".to_string(),
        description: String::new(),
        clocks: Default::default(),
        devices: Vec::new(),
        devices_by_name: Default::default(),
        buses: vec![Bus {
            name: "cpu".to_string(),
            address_bits: 21,
            owner: None,
            parent: None,
        }],
        windows: Vec::new(),
        connections: Vec::new(),
        signals: Vec::new(),
        boot: Default::default(),
        quirks: Default::default(),
    };
    Huc6280::new(&instance, &config)
}

fn step_one(cpu: &mut Huc6280, bus: &mut TestBus) -> u32 {
    let before = bus.cycles();
    crate::exec::step_with(cpu, Tick(0), bus);
    bus.cycles() - before
}

fn run_steps(cpu: &mut Huc6280, bus: &mut TestBus, count: usize) {
    for _ in 0..count {
        step_one(cpu, bus);
    }
}

/// The physical address the die's reset banks place under one logical
/// address.
fn phys(cpu: &Huc6280, logical: u16) -> u32 {
    cpu.route(logical, AccessRule::Generic).0
}

/// A core through the real boot path: `reset`, the boot state, and the
/// reset sequence that reads the vector through bank 0. The vector
/// points at `PROGRAM` and the program counter sits there.
fn boot() -> (Huc6280, TestBus) {
    let mut bus = TestBus::default();
    bus.store(VEC_RESET_PHYS, PROGRAM as u8);
    bus.store(VEC_RESET_PHYS + 1, (PROGRAM >> 8) as u8);
    let mut cpu = cpu_of();
    cpu.reset();
    cpu.boot(None);
    step_one(&mut cpu, &mut bus);
    (cpu, bus)
}

// --- The addressing modes ----------------------------------------------------

#[test]
fn immediate_loads_run_two_cycles() {
    let (mut cpu, mut bus) = boot();
    bus.load(phys(&cpu, PROGRAM), &[0xA9, 0x15]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x15);
    assert_eq!(cpu.regs.pc, PROGRAM + 2);
    assert_eq!(cycles, 2, "the immediate read costs two cycles");
    assert_eq!(cpu.regs.p & FLAG_N, 0);
}

#[test]
fn zero_page_reads_route_through_mpr1() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 0x40, 0x77);
    bus.load(phys(&cpu, PROGRAM), &[0xA5, 0x40]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x77);
    assert_eq!(cycles, 4, "the die's zero-page form costs four cycles");
    assert_eq!(
        bus.read_count(ZP_BASE + 0x40),
        1,
        "the zero page routed through MPR 1"
    );
}

#[test]
fn zero_page_x_pays_the_add_cycle() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 0x48, 0x99);
    // LDX #$08; LDA $40,X lands at the indexed cell.
    bus.load(phys(&cpu, PROGRAM), &[0xA2, 0x08, 0xB5, 0x40]);
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.regs.a, 0x99);
    assert_eq!(bus.read_count(ZP_BASE + 0x48), 1, "the indexed cell read");
    // The index-add cycle read the base cell in the same window.
    assert_eq!(bus.read_count(ZP_BASE + 0x40), 1);
}

#[test]
fn absolute_reads_run_five_cycles() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 0x0060, 0x44);
    bus.load(phys(&cpu, PROGRAM), &[0xAD, 0x60, 0x20]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x44);
    assert_eq!(cycles, 5, "the absolute form costs five cycles");
    assert!(
        bus.reads.contains(&(ZP_BASE + 0x0060)),
        "the absolute address $2060 routed through MPR 1"
    );
}

#[test]
fn indexed_reads_pay_no_page_cross_penalty() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 1, 0x5A);
    // LDA $1FFF,X crosses the logical page with X = 2 and still costs
    // the plain five cycles, the die's indexed form.
    bus.load(phys(&cpu, PROGRAM), &[0xA2, 0x02, 0xBD, 0xFF, 0x1F]);
    run_steps(&mut cpu, &mut bus, 1);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x5A, "the crossed index landed in page one");
    assert_eq!(cycles, 5, "the die pays no page-cross cycle");
    // The stores keep the same shape: STA $1FFF,Y with Y = 1.
    bus.load(phys(&cpu, PROGRAM) + 5, &[0x99, 0xFF, 0x1F]);
    cpu.regs.y = 0x01;
    cpu.regs.a = 0x11;
    cpu.regs.pc = PROGRAM + 5;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 5, "the store shape matches the read shape");
    assert_eq!(bus.byte(ZP_BASE), 0x11, "the store landed past the page");
}

#[test]
fn indirect_reads_run_seven_cycles() {
    let (mut cpu, mut bus) = boot();
    // LDA ($40),Y: the pointer walks to $2000 and Y picks $2010, which
    // routes through MPR 1's own window.
    bus.store(ZP_BASE + 0x40, 0x00);
    bus.store(ZP_BASE + 0x41, 0x20);
    bus.store(ZP_BASE + 0x10, 0x88);
    cpu.regs.y = 0x10;
    bus.load(phys(&cpu, PROGRAM), &[0xB1, 0x40]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x88, "the pointer's target sat in page one");
    assert_eq!(cycles, 7, "the die's indexed form costs seven cycles");
    // LDA ($40) with no index costs the same seven cycles.
    bus.load(phys(&cpu, PROGRAM) + 2, &[0xB2, 0x40]);
    bus.store(ZP_BASE, 0x00);
    bus.store(ZP_BASE + 1, 0x20);
    bus.store(ZP_BASE, 0x66);
    cpu.regs.y = 0x00;
    cpu.regs.pc = PROGRAM + 2;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x66, "the indirect read landed in page one");
    assert_eq!(cycles, 7);
    // LDA ($40,X) walks the pointer with the index and costs seven.
    bus.load(phys(&cpu, PROGRAM) + 4, &[0xA1, 0x40]);
    bus.store(ZP_BASE + 0x02, 0x0E);
    bus.store(ZP_BASE + 0x03, 0x00);
    bus.store((0xFF << 13) | 0x0E, 0x55);
    cpu.regs.x = 0xC2;
    cpu.regs.pc = PROGRAM + 4;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x55);
    assert_eq!(cycles, 7, "the pointer walk costs seven cycles");
    assert!(
        bus.reads.contains(&(ZP_BASE + 0x02)),
        "the wrapped pointer read through MPR 1"
    );
}

#[test]
fn indirect_pointer_reads_wrap_or_walk_their_forms() {
    let (mut cpu, mut bus) = boot();
    // JMP (ind) reads the pointer's absolute form with no wrap and
    // costs seven. The pointer sits in page four's bank-zero window.
    bus.store((0x8050 & 0x1FFF) as u32, 0x10);
    bus.store((0x8050 & 0x1FFF) as u32 + 1, 0x90);
    bus.load(phys(&cpu, PROGRAM), &[0x6C, 0x50, 0x80]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.pc, 0x9010,
        "the jump's target sat in page four's bank"
    );
    assert_eq!(cycles, 7, "the die's indirect jump costs seven cycles");
    // JMP (ind,X) with X = 1 costs the indexed form's seven cycles.
    bus.store((0x8050 & 0x1FFF) as u32 + 1, 0x10);
    bus.store((0x8050 & 0x1FFF) as u32 + 2, 0x91);
    bus.load(phys(&cpu, PROGRAM) + 3, &[0x7C, 0x50, 0x80]);
    cpu.regs.x = 0x01;
    cpu.regs.pc = PROGRAM + 3;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x9110, "the index shifted the pointer");
    assert_eq!(cycles, 7);
    assert!(
        bus.reads.contains(&((0x8050 & 0x1FFF) as u32 + 1)),
        "the index add walked the pointer"
    );

    // The zero-page pointer walk wraps inside the 8K bank window: LDA
    // ($FF) reads its pointer's second byte one past the pointer, at
    // the window's own offset $100.
    bus.store(ZP_BASE + 0xFF, 0x03);
    bus.store(ZP_BASE + 0x0100, 0x21);
    bus.store(ZP_BASE + 0x0103, 0x99);
    bus.load(phys(&cpu, PROGRAM) + 6, &[0xB2, 0xFF]);
    cpu.regs.pc = PROGRAM + 6;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x99, "the pointer walked past the page end");
    assert_eq!(cycles, 7);
    assert!(
        bus.reads.contains(&(ZP_BASE + 0x0100)),
        "the walk read the pointer's second byte"
    );
}

#[test]
fn jump_absolute_costs_four_cycles() {
    let (mut cpu, mut bus) = boot();
    bus.load(phys(&cpu, PROGRAM), &[0x4C, 0x00, 0xE0]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, PROGRAM);
    assert_eq!(cycles, 4);
}

// --- The store forms ---------------------------------------------------------

#[test]
fn store_forms_keep_their_cells() {
    let (mut cpu, mut bus) = boot();
    bus.load(
        phys(&cpu, PROGRAM),
        &[0x85, 0x30, 0x8D, 0x00, 0x20, 0x9D, 0x00, 0x20],
    );
    cpu.regs.a = 0x42;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "the zero-page store costs four cycles");
    assert_eq!(bus.byte(ZP_BASE + 0x30), 0x42);
    cpu.regs.x = 0x42;
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(bus.byte(ZP_BASE), 0x42, "the absolute store landed");
    assert_eq!(bus.byte(ZP_BASE + 0x42), 0x42, "the indexed store landed");
    // STA (zp),Y keeps the die's seven-cycle indexed form.
    bus.store(ZP_BASE + 0x20, 0x00);
    bus.store(ZP_BASE + 0x21, 0x20);
    bus.load(phys(&cpu, PROGRAM) + 8, &[0x91, 0x20]);
    cpu.regs.y = 0x10;
    cpu.regs.pc = PROGRAM + 8;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7);
    assert_eq!(
        bus.byte(ZP_BASE + 0x10),
        0x42,
        "the indexed pointer store landed"
    );
}

#[test]
fn store_zero_forms_run_their_cells() {
    let (mut cpu, mut bus) = boot();
    bus.load(
        phys(&cpu, PROGRAM),
        &[0x64, 0x30, 0x9C, 0x00, 0x20, 0x9E, 0xF0, 0x1F],
    );
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "the zero-page STZ costs four cycles");
    assert_eq!(bus.byte(ZP_BASE + 0x30), 0);
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(bus.byte(ZP_BASE), 0);
    assert_eq!(bus.byte(ZP_BASE + 0x0FF0), 0, "the indexed STZ landed");
}

// --- The read-modify-write forms ---------------------------------------------

#[test]
fn memory_shifts_read_twice_and_write_once() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 0x40, 0x0F);
    bus.load(phys(&cpu, PROGRAM), &[0x06, 0x40]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        bus.byte(ZP_BASE + 0x40),
        0x1E,
        "ASL applied without the carry"
    );
    assert_eq!(cycles, 6, "the die's RMW form costs six cycles");
    assert_eq!(
        bus.read_count(ZP_BASE + 0x40),
        2,
        "the RMW read the byte twice"
    );
    assert_eq!(bus.writes.len(), 1, "the RMW wrote the result once");
}

#[test]
fn rmw_absolute_and_indexed_forms_run_their_cells() {
    let (mut cpu, mut bus) = boot();
    // INC $2000 with the value $FF wraps to zero.
    bus.store(ZP_BASE, 0xFF);
    bus.load(phys(&cpu, PROGRAM), &[0xEE, 0x00, 0x20]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(ZP_BASE), 0x00);
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z);
    assert_eq!(cycles, 7, "the absolute RMW costs seven cycles");
    // DEC $20FE,X with X = 2 crosses the page and still costs seven.
    bus.store(ZP_BASE + 0x0100, 0x20);
    bus.load(phys(&cpu, PROGRAM) + 3, &[0xDE, 0xFE, 0x20]);
    cpu.regs.x = 0x02;
    cpu.regs.pc = PROGRAM + 3;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(ZP_BASE + 0x0100), 0x1F);
    assert_eq!(cycles, 7, "the indexed RMW paid no page-cross cycle");
}

#[test]
fn bit_store_and_reset_forms_carry_the_test() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.a = 0x60;
    bus.store(ZP_BASE + 0x50, 0x18);
    bus.load(phys(&cpu, PROGRAM), &[0x04, 0x50]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        bus.byte(ZP_BASE + 0x50),
        0x78,
        "TSB set the accumulator bits"
    );
    assert_eq!(cycles, 6, "the zero-page TSB costs six cycles");
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z, "no bits overlap, so Z sets");
    assert!(cpu.regs.p & FLAG_N == 0 && cpu.regs.p & FLAG_V == 0);
    // TRB clears the bit the accumulator names.
    cpu.regs.a = 0x10;
    bus.load(phys(&cpu, PROGRAM) + 2, &[0x1C, 0x00, 0x20]);
    bus.store(ZP_BASE, 0x78);
    cpu.regs.pc = PROGRAM + 2;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(ZP_BASE), 0x68, "TRB cleared the accumulator bit");
    assert_eq!(cycles, 7, "the absolute TRB costs seven cycles");
    assert_eq!(cpu.regs.p & FLAG_Z, 0);
}

#[test]
fn bit_tests_carry_the_operands_flags() {
    let (mut cpu, mut bus) = boot();
    // BIT with the immediate sets only the zero flag.
    cpu.regs.a = 0x0F;
    bus.load(phys(&cpu, PROGRAM), &[0x89, 0x30]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 2, "the immediate bit test costs two cycles");
    assert_eq!(
        cpu.regs.p & FLAG_Z,
        FLAG_Z,
        "the accumulator test set the zero flag"
    );
    assert!(cpu.regs.p & FLAG_N == 0 && cpu.regs.p & FLAG_V == 0);
    // The memory forms carry N and V from the operand byte.
    bus.load(phys(&cpu, PROGRAM) + 2, &[0x24, 0x50]);
    bus.store(ZP_BASE + 0x50, 0xC0);
    cpu.regs.pc = PROGRAM + 2;
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.p & (FLAG_N | FLAG_V), FLAG_N | FLAG_V);
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z, "the accumulator bit test");
}

// --- The branches and the subroutine forms -----------------------------------

#[test]
fn taken_branches_cost_two_idle_cycles() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.p |= FLAG_C;
    // BCS taken without a crossing: 4.
    bus.load(phys(&cpu, PROGRAM), &[0xB0, 0x08]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "the die's taken branch costs four cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 10);
    // A taken branch across a logical page pays no page-cross cycle.
    bus.load(phys(&cpu, PROGRAM) + 10, &[0xB0, 0x10]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "the branch never pays a page-cross cycle");
    assert_eq!(cpu.regs.pc, PROGRAM + 28);
    // Not taken: 2.
    cpu.regs.p &= !FLAG_C;
    bus.load(phys(&cpu, PROGRAM) + 28, &[0xB0, 0x01]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 2, "the branch not taken costs two cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 30);
}

#[test]
fn branch_always_runs_four_cycles() {
    let (mut cpu, mut bus) = boot();
    bus.load(phys(&cpu, PROGRAM), &[0x80, 0x10]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "BRA always takes its branch cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 18);
}

#[test]
fn branch_to_subroutine_round_trips() {
    let (mut cpu, mut bus) = boot();
    // BSR #+$20 costs the published eight cycles and pushes the
    // instruction's end.
    bus.load(phys(&cpu, PROGRAM), &[0x44, 0x20]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 8, "the branch to subroutine costs eight cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 34, "the signed offset applied");
    assert_eq!(
        bus.byte(0x1F01FD),
        ((PROGRAM + 2) >> 8) as u8,
        "the return high byte"
    );
    assert_eq!(
        bus.byte(0x1F01FC),
        ((PROGRAM + 2) & 0xFF) as u8,
        "the return low byte"
    );
    // RTS pulls the return and adds one, per the return instruction.
    cpu.regs.pc = PROGRAM;
    bus.load(phys(&cpu, PROGRAM), &[0x60]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7, "the return from subroutine costs seven cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 3, "the return added one");
    assert_eq!(cpu.regs.s, 0xFD, "the stack balanced");
}

#[test]
fn subroutine_call_and_return_round_trip() {
    let (mut cpu, mut bus) = boot();
    bus.load(phys(&cpu, PROGRAM), &[0x20, 0x00, 0xE0]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7, "the subroutine call costs seven cycles");
    assert_eq!(cpu.regs.pc, PROGRAM, "the call jumped back to the program");
    assert_eq!(
        bus.byte(0x1F01FD),
        (PROGRAM >> 8) as u8,
        "the pushed return high"
    );
    assert_eq!(
        bus.byte(0x1F01FC),
        0x02,
        "the return named the high byte's own address"
    );
    // The call's target byte overwrote the program: restore the return
    // form's target and let the return land.
    bus.load(phys(&cpu, PROGRAM), &[0x60]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7);
    assert_eq!(cpu.regs.pc, PROGRAM + 3, "the return landed past the JSR");
    assert_eq!(cpu.regs.s, 0xFD, "the stack balanced");
}

// --- The interrupts ----------------------------------------------------------

#[test]
fn break_costs_eight_cycles_and_takes_the_irq2_vector() {
    let (mut cpu, mut bus) = boot();
    // The vector reads through MPR 7's bank: MPR 7 holds bank 0x20,
    // so the vector at $FFF6 reads at that bank's physical offsets.
    cpu.mprs[7] = 0x20;
    bus.store(0x41FF6, 0x00);
    bus.store(0x41FF7, 0x80);
    cpu.regs.pc = PROGRAM;
    bus.load(0x20 << 13, &[0x00, 0x5A]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 8, "the break costs the published eight cycles");
    assert_eq!(cpu.regs.pc, 0x8000, "the vector read through MPR 7's bank");
    let pushed_p = bus.byte(0x1F01FB);
    assert_eq!(
        pushed_p & FLAG_B,
        FLAG_B,
        "the break push set the break flag"
    );
    assert_eq!(pushed_p & FLAG_T, 0, "the break cleared the T flag");
    assert_eq!(
        bus.byte(0x1F01FD),
        ((PROGRAM + 2) >> 8) as u8,
        "the return high byte"
    );
    assert_eq!(bus.byte(0x1F01FC), 0x02, "the return past the padding byte");
    assert_eq!(cpu.regs.p & FLAG_I, FLAG_I, "the entry set the disable");
    assert_eq!(cpu.regs.p & FLAG_D, 0, "the entry cleared the decimal flag");
    assert_eq!(
        cpu.regs.p & FLAG_B,
        FLAG_B,
        "the break flag holds its set form"
    );
}

#[test]
fn interrupt_service_runs_seven_cycles_with_the_break_flag_clear() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.p &= !FLAG_I;
    cpu.regs.p |= FLAG_D;
    cpu.notify_line("nmi", Level::High, Tick(0));
    bus.store(0x1FFC, 0x00);
    bus.store(0x1FFD, 0xD0);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7, "the interrupt service costs seven cycles");
    assert_eq!(
        cpu.regs.pc, 0xD000,
        "the NMI vector named the service target"
    );
    assert_eq!(cpu.regs.p & FLAG_I, FLAG_I, "the disable set");
    assert_eq!(
        cpu.regs.p & FLAG_D,
        0,
        "the die's entry cleared the decimal"
    );
    let pushed = bus.byte(0x1F01FB);
    assert_eq!(pushed & FLAG_B, 0, "the hardware push carries break clear");
    assert_eq!(pushed & FLAG_T, 0, "the live T flag stayed clear");
    assert_eq!(
        bus.byte(0x1F01FD),
        (PROGRAM >> 8) as u8,
        "the return high byte"
    );
    assert_eq!(bus.byte(0x1F01FC), 0x00, "the return low byte");
    // RTI returns without an increment and restores the flags.
    cpu.regs.pc = PROGRAM;
    bus.load(phys(&cpu, PROGRAM), &[0x40]);
    cpu.regs.s = 0xFC;
    bus.store(0x1F01FD, 0x37);
    bus.store(0x1F01FE, 0x50);
    bus.store(0x1F01FF, 0x12);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7, "the return from interrupt costs seven cycles");
    assert_eq!(cpu.regs.pc, 0x1250);
    assert_eq!(cpu.regs.p & FLAG_C, FLAG_C, "the pulled carry");
    assert_eq!(cpu.regs.p & FLAG_B, FLAG_B, "the break form restored set");
}

#[test]
fn line_services_take_their_vectors_in_priority_order() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.p &= !FLAG_I;
    // The vectors read through MPR 7's reset bank.
    bus.store(0x1FF6, 0x00);
    bus.store(0x1FF7, 0x70);
    bus.store(0x1FF8, 0x00);
    bus.store(0x1FF9, 0x80);
    bus.store(0x1FFA, 0x00);
    bus.store(0x1FFB, 0x90);
    // IRQ1, the timer, and IRQ2 all stand.
    cpu.notify_line("irq1", Level::High, Tick(0));
    cpu.notify_line("irq2", Level::High, Tick(0));
    cpu.irq.timer_pending = true;
    // The timer ranks first, per the published priority order.
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7);
    assert_eq!(cpu.regs.pc, 0x9000, "the timer took $FFFA");
    assert!(cpu.irq.timer_pending, "the pending stays until the ack");
    // The acknowledge clears it, then IRQ1 services.
    cpu.on_die_write(0x1403, 0, Tick(0));
    assert!(!cpu.irq.timer_pending, "the acknowledge cleared the timer");
    cpu.regs.p &= !FLAG_I;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7);
    assert_eq!(cpu.regs.pc, 0x8000, "IRQ1 took $FFF8");
    assert!(cpu.irq.irq1, "the level source stays pending");
    // The line dropped; the acknowledge left nothing, and IRQ2's level
    // source services next.
    cpu.notify_line("irq1", Level::Low, Tick(0));
    cpu.regs.p &= !FLAG_I;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7);
    assert_eq!(cpu.regs.pc, 0x7000, "IRQ2 took $FFF6");
    // The lines drop and nothing remains pending.
    cpu.notify_line("irq1", Level::Low, Tick(0));
    cpu.notify_line("irq2", Level::Low, Tick(0));
    cpu.regs.p &= !FLAG_I;
    assert!(!cpu.interrupt_pending());
    // The NMI edge outranks the line sources when one stands again.
    cpu.irq.timer_pending = true;
    cpu.notify_line("nmi", Level::High, Tick(0));
    bus.store(0x1FFC, 0x00);
    bus.store(0x1FFD, 0xA0);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0xA000, "the NMI took $FFFC");
}

#[test]
fn disable_mask_and_disable_flag_gate_the_sources() {
    let (mut cpu, mut _bus) = boot();
    cpu.regs.p &= !FLAG_I;
    cpu.notify_line("irq1", Level::High, Tick(0));
    // The published mask register: a write sets the disable bits.
    cpu.on_die_write(0x1402, MASK_IRQ1, Tick(0));
    assert!(!cpu.interrupt_pending(), "the masked source holds");
    assert_eq!(
        cpu.on_die_read(0x1402),
        Some(MASK_IRQ1),
        "the mask readback"
    );
    // The status read reports the pending bits at $1403, bit 1 for
    // IRQ1.
    assert_eq!(
        cpu.on_die_read(0x1403),
        Some(0x02),
        "IRQ1's pending bit read"
    );
    // A written zero bit clears its disable bit; the published mask
    // also forms the enable.
    cpu.on_die_write(0x1402, 0, Tick(0));
    assert!(cpu.interrupt_pending(), "the unmasked source stands");
    // The disable flag blocks the line sources again.
    cpu.regs.p |= FLAG_I;
    assert!(!cpu.interrupt_pending(), "the disable flag holds");
    // The IRQ2 source shares the same layout at bit 0.
    cpu.notify_line("irq2", Level::High, Tick(0));
    cpu.regs.p &= !FLAG_I;
    cpu.on_die_write(0x1402, MASK_IRQ2, Tick(0));
    assert!(cpu.interrupt_pending(), "IRQ1 stood while IRQ2 was masked");
    // The legacy alias writes keep the mask too: the disable write at
    // $1400 and the bit-clear enable write at $1401.
    cpu.on_die_write(0x1400, MASK_IRQ1 | MASK_IRQ2 | MASK_TIMER, Tick(0));
    assert!(!cpu.interrupt_pending(), "the full mask holds");
    cpu.on_die_write(0x1401, MASK_TIMER, Tick(0));
    assert!(!cpu.interrupt_pending(), "the masked timer stayed");
    cpu.on_die_write(0x1401, MASK_IRQ1 | MASK_IRQ2 | MASK_TIMER, Tick(0));
    assert!(
        cpu.interrupt_pending(),
        "the full enable unblocked the levels"
    );
    cpu.notify_line("irq1", Level::Low, Tick(0));
    cpu.notify_line("irq2", Level::Low, Tick(0));
    assert!(!cpu.interrupt_pending());
}

// --- The die's own units -----------------------------------------------------

#[test]
fn tam_loads_the_selected_mprs() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.a = 0x05;
    bus.load(phys(&cpu, PROGRAM), &[0x53, 0x03]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.mprs[0], 0x05, "TAM loaded MPR0");
    assert_eq!(cpu.mprs[1], 0x05, "TAM loaded MPR1");
    assert_eq!(cpu.mprs[2], 0, "the cleared bit left its MPR");
    assert_eq!(cycles, 5, "TAM costs five cycles");
    // The new MPR1 translation applies to the zero page: MPR 1 names
    // bank 5 now, so the zero page reads at bank 5's window.
    bus.store((0x05 << 13) | 0x10, 0x66);
    bus.load(phys(&cpu, PROGRAM) + 2, &[0xA5, 0x10]);
    cpu.regs.pc = PROGRAM + 2;
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x66, "the zero page read through the new bank");
}

#[test]
fn tma_pulls_the_highest_selected_mpr() {
    let (mut cpu, mut bus) = boot();
    cpu.mprs = [0x10, 0x20, 0x30, 0x08, 0x00, 0x00, 0x00, 0x00];
    bus.load(phys(&cpu, PROGRAM), &[0x43, 0x06, 0x43, 0x80, 0x43, 0x01]);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x30, "the highest set bit picked MPR2");
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x00, "the MPR7 load answered bank zero");
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x10, "TMA1 picked MPR0");
    assert_eq!(cycles, 4, "TMA costs four cycles");
}

#[test]
fn strobe_writes_route_through_mpr0() {
    let (mut cpu, mut bus) = boot();
    // ST0, ST1, and ST2 write the hardware page's select ports.
    bus.load(phys(&cpu, PROGRAM), &[0x03, 0x11, 0x13, 0x22, 0x23, 0x33]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "the strobe costs the published four cycles");
    assert_eq!(bus.write_at(0), (0x1FE000, 0x11), "ST0 wrote the MPR0 port");
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(bus.write_at(1), (0x1FE002, 0x22), "ST1 wrote at offset two");
    assert_eq!(
        bus.write_at(2),
        (0x1FE003, 0x33),
        "ST2 wrote at offset three"
    );
    // A changed MPR 0 moves the write.
    cpu.mprs[0] = 0x40;
    bus.load(phys(&cpu, PROGRAM) + 6, &[0x03, 0x44]);
    cpu.regs.pc = PROGRAM + 6;
    step_one(&mut cpu, &mut bus);
    assert_eq!(bus.write_at(3), (0x80000, 0x44), "the write followed MPR 0");
}

#[test]
fn the_timer_writes_reach_the_die() {
    let (mut cpu, mut bus) = boot();
    // STA $0C00 latches the preset; STA $0C01 runs the counter.
    bus.load(
        phys(&cpu, PROGRAM),
        &[0xA9, 0x02, 0x8D, 0x00, 0x0C, 0xA9, 0x01, 0x8D, 0x01, 0x0C],
    );
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.timer.preset, 0x02, "the preset latched");
    run_steps(&mut cpu, &mut bus, 2);
    assert!(cpu.timer.enabled, "the enable write ran the counter");
    assert_eq!(
        cpu.timer.counter, 0x02,
        "the run write reloaded the counter"
    );
    // The counter decrements at the rate select's interval; the base
    // sits at the enable write's access tick.
    let interval = cpu.timer_interval(0x00);
    let base = cpu.timer.tick_base;
    cpu.run_timer(Tick(base + interval));
    assert_eq!(cpu.timer.counter, 0x01, "one decrement landed");
    assert!(!cpu.irq.timer_pending, "no underflow yet");
    // The decrement at zero underflows, reloads the preset, and
    // raises the pending bit; the die's accesses stayed off the bus.
    cpu.run_timer(Tick(base + interval * 2));
    assert_eq!(cpu.timer.counter, 0x00, "the second decrement landed");
    assert!(
        !cpu.irq.timer_pending,
        "the live zero has not underflowed yet"
    );
    cpu.run_timer(Tick(base + interval * 3));
    assert_eq!(cpu.timer.counter, 0x02, "the underflow reloaded the preset");
    assert!(
        cpu.irq.timer_pending,
        "the underflow raised the pending bit"
    );
    // The status read reports the timer's bit.
    assert_eq!(cpu.on_die_read(0x1403), Some(0x04));
    // The rate select divides the 1024-cycle base at the divider.
    assert_eq!(cpu.timer_interval(0x00), 3 * 1024);
    assert_eq!(cpu.timer_interval(0x07), 3 * 128);
}

#[test]
fn the_timer_pending_services_through_the_timer_vector() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.p &= !FLAG_I;
    cpu.irq.timer_pending = true;
    bus.store(0x1FFA, 0x00);
    bus.store(0x1FFB, 0xD0);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7);
    assert_eq!(cpu.regs.pc, 0xD000, "the timer service took $FFFA");
    assert_eq!(
        bus.byte(0x1F01FB) & FLAG_B,
        0,
        "the push carried break clear"
    );
    assert!(
        cpu.irq.timer_pending,
        "the service left the pending until the acknowledge"
    );
}

#[test]
fn io_reads_answer_from_the_die() {
    let (mut cpu, mut bus) = boot();
    cpu.irq.timer_pending = true;
    // LDA $1403 answers the controller's status with no bus read.
    bus.load(phys(&cpu, PROGRAM), &[0xAD, 0x03, 0x14]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.a, 0x04, "the status read answered from the die");
    assert_eq!(cycles, 5, "the die's register read costs its cycle");
    assert!(
        !bus.reads.contains(&0x1FF403),
        "the die's read stayed off the bus"
    );
    // The published mask write reaches the die through the bus.
    bus.load(phys(&cpu, PROGRAM) + 3, &[0xA9, 0x07, 0x8D, 0x02, 0x14]);
    cpu.regs.pc = PROGRAM + 3;
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        cpu.irq.mask, 0x07,
        "the mask write landed in the controller"
    );
}

// --- The block transfers -----------------------------------------------------

#[test]
fn transfer_increment_increment_moves_the_block() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 0x0010, 0x10);
    bus.store(ZP_BASE + 0x0011, 0x20);
    bus.store(ZP_BASE + 0x0012, 0x30);
    // TII 2010, 2100, #3: both addresses move through MPR 1's window.
    bus.load(
        phys(&cpu, PROGRAM),
        &[0x73, 0x10, 0x20, 0x00, 0x21, 0x03, 0x00],
    );
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 17 + 6 * 3, "17 cycles plus 6 per byte");
    assert_eq!(bus.byte(ZP_BASE + 0x0100), 0x10, "the first byte moved");
    assert_eq!(bus.byte(ZP_BASE + 0x0101), 0x20, "the source incremented");
    assert_eq!(bus.byte(ZP_BASE + 0x0102), 0x30, "the last byte moved");
    assert_eq!(bus.byte(ZP_BASE + 0x0010), 0x10, "the source stayed");
}

#[test]
fn transfer_increment_none_keeps_the_destination() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 0x0020, 0x11);
    bus.store(ZP_BASE + 0x0021, 0x12);
    bus.store(ZP_BASE + 0x0022, 0x13);
    // TIN 2020, 2100, #3: the destination stays fixed.
    bus.load(
        phys(&cpu, PROGRAM),
        &[0xD3, 0x20, 0x20, 0x00, 0x21, 0x03, 0x00],
    );
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 17 + 6 * 3);
    assert_eq!(
        bus.byte(ZP_BASE + 0x0100),
        0x13,
        "the last byte won the cell"
    );
    assert_eq!(
        bus.byte(ZP_BASE + 0x0101),
        0,
        "the destination kept its cell"
    );
    assert_eq!(bus.byte(ZP_BASE + 0x0021), 0x12, "the source incremented");
}

#[test]
fn transfer_alternates_and_decrements_move_their_forms() {
    let (mut cpu, mut bus) = boot();
    // TIA: the source increments, the destination alternates.
    bus.store(ZP_BASE + 0x0030, 0x21);
    bus.store(ZP_BASE + 0x0031, 0x22);
    bus.store(ZP_BASE + 0x0032, 0x23);
    bus.store(ZP_BASE + 0x0033, 0x24);
    bus.load(
        phys(&cpu, PROGRAM),
        &[0xE3, 0x30, 0x20, 0x00, 0x21, 0x04, 0x00],
    );
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 17 + 6 * 4);
    assert_eq!(
        bus.byte(ZP_BASE + 0x0100),
        0x23,
        "the alternation split the stream"
    );
    assert_eq!(
        bus.byte(ZP_BASE + 0x0101),
        0x24,
        "the odd cell took the odd byte"
    );
    // TAI: the source alternates, the destination increments.
    bus.load(
        phys(&cpu, PROGRAM) + 6,
        &[0xF3, 0x30, 0x20, 0x10, 0x21, 0x04, 0x00],
    );
    cpu.regs.pc = PROGRAM + 6;
    step_one(&mut cpu, &mut bus);
    assert_eq!(
        bus.byte(ZP_BASE + 0x0110),
        0x21,
        "the stream interleaved the source"
    );
    assert_eq!(bus.byte(ZP_BASE + 0x0111), 0x22, "the destination advanced");
    // TDD: both addresses decrement through the source's own cell.
    bus.load(
        phys(&cpu, PROGRAM) + 6,
        &[0xC3, 0x31, 0x20, 0x20, 0x20, 0x01, 0x00],
    );
    cpu.regs.pc = PROGRAM + 6;
    step_one(&mut cpu, &mut bus);
    assert_eq!(
        bus.byte(ZP_BASE + 0x0020),
        0x22,
        "the decrement form moved its byte"
    );
}

// --- The T-flag mode ---------------------------------------------------------

#[test]
fn the_t_mode_adds_into_the_zero_page() {
    let (mut cpu, mut bus) = boot();
    bus.store(ZP_BASE + 0x24, 0x20);
    // CLX; LDX #$24; LDA #$77; SEC; SET: the T-mode form follows.
    bus.load(
        phys(&cpu, PROGRAM),
        &[0x62, 0xA2, 0x24, 0xA9, 0x77, 0x38, 0xF4],
    );
    run_steps(&mut cpu, &mut bus, 5);
    // ADC #$10 with the carry set adds into the cell.
    bus.load(phys(&cpu, PROGRAM) + 7, &[0x69, 0x10]);
    cpu.regs.pc = PROGRAM + 7;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 5, "the T-mode form costs the plain cell plus three");
    assert_eq!(cpu.regs.a, 0x77, "the accumulator kept its value");
    assert_eq!(cycles, 5);
    assert_eq!(bus.byte(ZP_BASE + 0x24), 0x31, "the cell kept the result");
    assert_eq!(cpu.regs.a, 0x77, "the accumulator kept its value");
    assert_eq!(cpu.regs.p & FLAG_T, 0, "the operation cleared the T flag");
    assert!(
        bus.reads.contains(&(ZP_BASE + 0x24)),
        "the cell read in the window"
    );
}

#[test]
fn the_t_mode_carries_the_logical_and_subtract_forms() {
    let (mut cpu, mut bus) = boot();
    // SET; ORA #$F0 with the cell at $10: the cell takes the form and
    // the accumulator keeps its value.
    bus.store(ZP_BASE + 0x10, 0x01);
    cpu.regs.x = 0x10;
    cpu.regs.a = 0x77;
    bus.load(phys(&cpu, PROGRAM), &[0xF4, 0x09, 0xF0]);
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        bus.byte(ZP_BASE + 0x10),
        0xF1,
        "the ORA form landed in the cell"
    );
    assert_eq!(cpu.regs.a, 0x77, "the accumulator kept its value");
    // The AND form: SET; AND #$0E with the cell.
    bus.load(phys(&cpu, PROGRAM) + 3, &[0xF4, 0x29, 0x0E]);
    cpu.regs.pc = PROGRAM + 3;
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(bus.byte(ZP_BASE + 0x10), 0x00, "the AND form landed");
    // The EOR and SBC forms follow the same shape.
    bus.load(phys(&cpu, PROGRAM) + 6, &[0xF4, 0x49, 0xEE]);
    cpu.regs.pc = PROGRAM + 6;
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(bus.byte(ZP_BASE + 0x10), 0xEE, "the EOR form landed");
    bus.load(phys(&cpu, PROGRAM) + 9, &[0x38, 0xF4, 0xE9, 0x10]);
    cpu.regs.pc = PROGRAM + 9;
    run_steps(&mut cpu, &mut bus, 2);
    step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(ZP_BASE + 0x10), 0xDE, "the SBC form landed");
}

#[test]
fn the_t_flag_persists_until_an_operation_or_a_restore() {
    let (mut cpu, mut bus) = boot();
    bus.load(phys(&cpu, PROGRAM), &[0xF4, 0x88]);
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        cpu.regs.p & FLAG_T,
        0,
        "the non-participating instruction decoded the flag clear"
    );
    // The SET instruction leaves the flag, and the following plain
    // instruction decodes it away again.
    bus.load(phys(&cpu, PROGRAM) + 2, &[0xF4, 0xEA]);
    cpu.regs.pc = PROGRAM + 2;
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.p & FLAG_T, FLAG_T, "SET left the flag");
    step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.p & FLAG_T,
        0,
        "the next instruction decoded it away"
    );
    // The PLP restores the pushed byte's form, the T flag included.
    bus.load(phys(&cpu, PROGRAM) + 4, &[0xF4, 0x28]);
    cpu.regs.pc = PROGRAM + 4;
    cpu.regs.s = 0xFE;
    bus.store(0x1F01FF, FLAG_T | FLAG_C);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.p & FLAG_T, FLAG_T, "SET left the flag");
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.p & FLAG_T, FLAG_T, "the pull restored the T flag");
    assert_eq!(cpu.regs.p & FLAG_C, FLAG_C, "the pull restored the carry");
    // The break instruction clears the flag before its pushes, so the
    // break push carries it clear.
    bus.load(phys(&cpu, PROGRAM) + 6, &[0x00, 0x28]);
    cpu.regs.pc = PROGRAM + 6;
    cpu.regs.s = 0xFE;
    step_one(&mut cpu, &mut bus);
    assert_eq!(
        bus.byte(0x1F01FC) & FLAG_T,
        0,
        "the break push carried it clear"
    );
}

// --- The speed forms and the table ------------------------------------------

#[test]
fn the_speed_forms_switch_the_divider() {
    let (mut cpu, mut bus) = boot();
    bus.load(phys(&cpu, PROGRAM), &[0x54, 0xD4]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 3, "the speed forms cost three cycles");
    assert_eq!(cpu.cpu_div, 12, "CSL runs the low-speed form");
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.cpu_div, 3, "CSH returned the high-speed divider");
    // A reset restores the configured divider.
    cpu.cpu_div = 12;
    cpu.reset();
    assert_eq!(cpu.cpu_div, 3, "the reset restored the configured divider");
}

#[test]
fn the_page_bit_forms_run_their_cells() {
    let (mut cpu, mut bus) = boot();
    // RMB2 with the bit clear: the store lands the cleared byte.
    bus.store(ZP_BASE + 0x30, 0x04);
    bus.load(phys(&cpu, PROGRAM), &[0x27, 0x30]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(ZP_BASE + 0x30), 0x00, "RMB2 cleared the bit");
    assert_eq!(cycles, 7, "the RMB form costs seven cycles");
    assert_eq!(bus.read_count(ZP_BASE + 0x30), 2, "the form read twice");
    // BBR3 with the bit clear takes the branch.
    bus.load(phys(&cpu, PROGRAM) + 2, &[0x3F, 0x30, 0x10]);
    cpu.regs.pc = PROGRAM + 2;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 8, "the taken bit-test branch costs eight cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 21);
    // BBS0 with the bit clear does not branch.
    bus.load(phys(&cpu, PROGRAM) + 5, &[0x8F, 0x30, 0x10]);
    cpu.regs.pc = PROGRAM + 5;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 6, "the not-taken bit-test branch costs six cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 8);
}

#[test]
fn the_unassigned_cells_run_operand_consuming_nops() {
    let (mut cpu, mut bus) = boot();
    // The immediate shape consumes one operand byte.
    bus.load(phys(&cpu, PROGRAM), &[0x0B, 0xEA]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 2, "the immediate NOP shape costs two cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 2);
    // The default shape reads an absolute,Y operand.
    bus.load(phys(&cpu, PROGRAM) + 2, &[0x33, 0x10, 0x20]);
    cpu.regs.pc = PROGRAM + 2;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 5, "the absolute,Y NOP shape costs five cycles");
    assert_eq!(cpu.regs.pc, PROGRAM + 5);
    // The absolute,X shape of $5C reads the indexed operand: the base
    // sits in page zero and the index crosses into MPR 1's window.
    bus.load(phys(&cpu, PROGRAM) + 5, &[0x5C, 0xFE, 0x1F]);
    cpu.regs.x = 0x02;
    cpu.regs.pc = PROGRAM + 5;
    step_one(&mut cpu, &mut bus);
    assert!(
        bus.reads.contains(&ZP_BASE),
        "the $5C shape read the indexed operand"
    );
    // The indirect,Y shape of $1B reads through the zero-page window.
    bus.load(phys(&cpu, PROGRAM) + 8, &[0x1B, 0x30]);
    cpu.regs.pc = PROGRAM + 8;
    step_one(&mut cpu, &mut bus);
    assert!(
        bus.reads.contains(&(ZP_BASE + 0x30)),
        "the $1B shape read the pointer"
    );
}

#[test]
fn every_opcode_runs_deterministically() {
    for op in 0..=255u8 {
        let (mut cpu, mut bus) = boot();
        cpu.regs.pc = 0xE800;
        bus.load(phys(&cpu, 0xE800), &[op, 0xEA, 0x00, 0xE8, 0x00]);
        step_one(&mut cpu, &mut bus);
        let snap = cpu.snapshot();

        let (mut twin, mut bus2) = boot();
        twin.regs.pc = 0xE800;
        bus2.load(phys(&twin, 0xE800), &[op, 0xEA, 0x00, 0xE8, 0x00]);
        step_one(&mut twin, &mut bus2);
        assert_eq!(
            twin.snapshot(),
            snap,
            "opcode {op:02X} was deterministic in the first run only"
        );
        assert!(bus.cycles() >= 2, "opcode {op:02X} ran too short");
    }
}

// --- The published cycle cells ----------------------------------------------

#[test]
fn published_cycle_cells() {
    let (mut cpu, mut bus) = boot();
    bus.load(
        phys(&cpu, PROGRAM),
        &[
            0xA9, 0x01, // LDA #imm: 2
            0xA5, 0x20, // LDA zp: 4
            0xB5, 0x20, // LDA zp,X: 4
            0xAD, 0x00, 0x20, // LDA abs: 5
            0xB1, 0x20, // LDA (zp),Y: 7
            0xA1, 0x20, // LDA (zp,X): 7
            0x48, 0x68, 0x18, 0xEA, // PHA: 3, PLA: 4, CLC: 2, NOP: 2
        ],
    );
    bus.store(ZP_BASE + 0x20, 0x12);
    bus.store(ZP_BASE + 0x21, 0x00);
    let mut pc = PROGRAM;
    for (cycles, length) in [(2, 2), (4, 2), (4, 2), (5, 3), (7, 2), (7, 2)] {
        cpu.regs.pc = pc;
        let used = step_one(&mut cpu, &mut bus);
        assert_eq!(used, cycles, "the published cell of the load form");
        pc += length as u16;
    }
    let pushed = cpu.regs.a;
    for cycles in [3, 4, 2, 2] {
        let used = step_one(&mut cpu, &mut bus);
        assert_eq!(used, cycles, "the published cell of the stack form");
    }
    assert_eq!(cpu.regs.s, 0xFD, "the stack balanced");
    assert_eq!(cpu.regs.a, pushed, "the pull restored the pushed byte");
}

#[test]
fn reset_reads_the_vector_from_bank_zero() {
    // The boot's vector read sits at the bank-zero offsets whatever MPR
    // 7 carries; the program loads at bank zero's 0xE000 form.
    let mut bus = TestBus::default();
    bus.store(VEC_RESET_PHYS, 0x00);
    bus.store(VEC_RESET_PHYS + 1, 0xE0);
    let mut cpu = cpu_of();
    cpu.reset();
    cpu.boot(None);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, PROGRAM);
    assert_eq!(
        bus.read_count(VEC_RESET_PHYS),
        1,
        "the reset vector read from bank zero"
    );
    assert_eq!(
        bus.reads.iter().filter(|a| **a == 0x81FFE).count(),
        0,
        "the read never honored MPR 7"
    );
    // The boot's stack sequence ran the dummy pushes.
    assert_eq!(
        cpu.regs.s, 0xFD,
        "the reset's dummy pushes landed the pointer"
    );
    // A second reset reads the vector again through bank zero.
    cpu.mprs[7] = 0x40;
    bus.store(VEC_RESET_PHYS, 0x10);
    bus.store(VEC_RESET_PHYS + 1, 0x20);
    cpu.reset();
    cpu.boot(None);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x2010, "the reset honored bank zero again");
}
