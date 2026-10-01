//! The instruction-level tests over a plain byte map.
//!
//! The tests drive the same step function the machine uses through a
//! small test bus: every byte defaults to zero and the reads and the
//! writes log in cycle order, so the asserted cycle totals cover the
//! published table cells and the asserted reads cover the address
//! forms. Data for an instruction always lands before the step that
//! reads it.

use super::*;
use crate::cpu::{FLAG_M, FLAG_N, FLAG_X};
use demu_config::model::{Bus, DeviceInstance, SystemConfig};
use demu_core::{Device, Level};

/// The program position the test boots into.
const PROGRAM: u16 = 0x8000;

/// The reset vector's 24-bit address.
const VEC_RESET_U32: u32 = 0x00FFFC;

/// A plain byte map for the tests.
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
}

fn cpu_of(divider: u32) -> Wdc65c816 {
    use demu_config::newtypes::{DeviceName, KindName};
    let instance = DeviceInstance {
        name: DeviceName::new("cpu0").expect("device name"),
        kind: KindName::new("wdc65c816.cpu").expect("kind name"),
        domain: "main".to_string(),
        params: Default::default(),
        dividers: [("cpu".to_string(), divider)].into_iter().collect(),
    };
    let config = SystemConfig {
        name: "test".to_string(),
        triple: "wdc65c816".to_string(),
        description: String::new(),
        clocks: Default::default(),
        devices: Vec::new(),
        devices_by_name: Default::default(),
        buses: vec![Bus {
            name: "cpu".to_string(),
            address_bits: 24,
            owner: None,
            parent: None,
        }],
        windows: Vec::new(),
        connections: Vec::new(),
        signals: Vec::new(),
        boot: Default::default(),
        quirks: Default::default(),
    };
    Wdc65c816::new(&instance, &config)
}

fn step_one(cpu: &mut Wdc65c816, bus: &mut TestBus) -> u32 {
    let before = bus.cycles();
    crate::exec::step_with(cpu, Tick(0), bus);
    bus.cycles() - before
}

fn run_steps(cpu: &mut Wdc65c816, bus: &mut TestBus, count: usize) {
    for _ in 0..count {
        step_one(cpu, bus);
    }
}

/// A core through the real boot path: `reset`, the boot state, and
/// the reset sequence that reads the vector through the bus. The
/// vector points at `PROGRAM` and the program counter sits there.
fn boot() -> (Wdc65c816, TestBus) {
    let mut bus = TestBus::default();
    bus.store(VEC_RESET_U32, PROGRAM as u8);
    bus.store(VEC_RESET_U32 + 1, (PROGRAM >> 8) as u8);
    let mut cpu = cpu_of(6);
    cpu.reset();
    cpu.boot(None);
    step_one(&mut cpu, &mut bus);
    (cpu, bus)
}

/// A core already switched to native mode through `CLC; XCE`, with
/// the program counter sitting at `PROGRAM + 2`.
fn boot_native() -> (Wdc65c816, TestBus) {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0x18, 0xFB]);
    run_steps(&mut cpu, &mut bus, 2);
    assert!(!cpu.regs.e, "the program entered native mode");
    assert_eq!(cpu.regs.s & 0xFF00, 0x0100, "the stack kept its page");
    (cpu, bus)
}

// --- The immediate forms and the widths --------------------------------------

#[test]
fn emulation_immediate_loads_eight_bits() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0xA9, 0x15]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c & 0x00FF, 0x15);
    assert_eq!(cpu.regs.pc, 0x8002);
    assert_eq!(cycles, 2, "the immediate read costs two cycles");
    assert_eq!(cpu.regs.p & FLAG_N, 0);
}

#[test]
fn native_immediate_loads_sixteen_bits() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xC2, 0x20, 0xA9, 0xCD, 0xAB]);
    run_steps(&mut cpu, &mut bus, 1);
    assert!(cpu.m_wide());
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c, 0xABCD);
    assert_eq!(cycles, 3, "the wide immediate read costs one cycle more");
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N, "the flag read the result bit");
}

#[test]
fn native_add_sixteen_bits_sets_carry_and_overflow() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(
        0x8002,
        &[0xC2, 0x20, 0x18, 0xA9, 0x00, 0x70, 0x69, 0x02, 0x40],
    );
    run_steps(&mut cpu, &mut bus, 4);
    // C = $7000 + $4002 has two positive inputs and a negative sum:
    // the overflow sets and the carry clears.
    assert_eq!(cpu.regs.c, 0xB002);
    assert_eq!(cpu.regs.p & FLAG_C, 0);
    assert_eq!(cpu.regs.p & FLAG_V, FLAG_V, "the same-sign sum overflowed");
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N);
}

#[test]
fn native_subtract_borrows() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(
        0x8002,
        &[0xC2, 0x20, 0x38, 0xA9, 0x00, 0x30, 0xE9, 0x01, 0x30],
    );
    run_steps(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.regs.c, 0xFFFF, "$3000 - $3001 borrows");
    assert_eq!(cpu.regs.p & FLAG_C, 0, "the borrow cleared the carry");
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N);
}

#[test]
fn emulation_decimal_adds() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0xF8, 0xA9, 0x09, 0x69, 0x01]);
    run_steps(&mut cpu, &mut bus, 4);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x10,
        "the decimal add of 9 and 1 is 10"
    );
    assert_eq!(cpu.regs.p & FLAG_C, 0, "$10 carries only past $99");
}

#[test]
fn emulation_decimal_add_carry_and_subtract() {
    let (mut cpu, mut bus) = boot();
    // SED; A = $90; ADC #$20: $110 wraps to $10 with the carry.
    bus.load(u32::from(PROGRAM), &[0xF8, 0xA9, 0x90, 0x69, 0x20]);
    run_steps(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.regs.c & 0x00FF, 0x10);
    assert_eq!(cpu.regs.p & FLAG_C, FLAG_C, "the decimal carry set");
    // SEC; A = $10; SBC #$01: $09, no borrow out. The carry from the
    // add makes the borrow-in zero.
    cpu.regs.pc = 0x8005;
    bus.store(0x8005, 0x38);
    bus.load(0x8006, &[0xA9, 0x10, 0xE9, 0x01]);
    run_steps(&mut cpu, &mut bus, 3);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x09,
        "the decimal subtract of 1 from 10"
    );
    assert_eq!(cpu.regs.p & FLAG_C, FLAG_C, "$10 - $01 does not borrow");
    // With the carry set the borrow-in is zero: $00 - $01 = $99 with
    // the borrow out leaving the carry clear. The SBC runs at $800C.
    cpu.regs.pc = 0x800A;
    bus.load(0x800A, &[0xA9, 0x00, 0xE9, 0x01]);
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.regs.c & 0x00FF, 0x99, "the decimal underflow wraps");
    assert_eq!(cpu.regs.p & FLAG_C, 0, "the borrow left the carry clear");
}

#[test]
fn native_decimal_add_works_for_eight_bits() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xF8, 0xA9, 0x09, 0x18, 0x69, 0x01]);
    run_steps(&mut cpu, &mut bus, 5);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x10,
        "the native 8-bit form adds decimal"
    );
    // Wide form with the flag set: the word arithmetic stays binary --
    // the decimal flag applies only to the 8-bit accumulator form.
    let (mut cpu, mut bus) = boot_native();
    cpu.regs.p &= !FLAG_X;
    bus.load(
        0x8002,
        &[0xF8, 0xC2, 0x20, 0xA9, 0x09, 0x00, 0x18, 0x69, 0x01, 0x00],
    );
    run_steps(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.regs.c, 0x000A, "the wide form adds binary");
}

// --- The addressing modes ----------------------------------------------------

#[test]
fn direct_page_reads_live_in_bank_zero() {
    let (mut cpu, mut bus) = boot();
    bus.store(0x0040, 0x77);
    bus.load(u32::from(PROGRAM), &[0xA5, 0x40]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c & 0x00FF, 0x77);
    assert_eq!(cycles, 3, "the direct read costs three cycles");
    assert!(bus.reads.contains(&0x40), "the direct address is bank zero");
}

#[test]
fn indexed_direct_page_wraps_in_emulation() {
    let (mut cpu, mut bus) = boot();
    bus.store(0x0004, 0x55);
    // LDX #$14; LDA $F0,X: the offset wraps to $04.
    bus.load(u32::from(PROGRAM), &[0xA2, 0x14, 0xB5, 0xF0]);
    run_steps(&mut cpu, &mut bus, 2);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x55,
        "the offset wrapped within page zero"
    );
}

#[test]
fn indexed_direct_pays_index_add_cycle() {
    let (mut cpu, mut bus) = boot();
    bus.store(0x00F8, 0x99);
    // LDA $F0,X with X = 8, inside the page: the add cycle still costs.
    bus.load(u32::from(PROGRAM), &[0xB5, 0xF0]);
    cpu.regs.x = 0x08;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c & 0x00FF, 0x99);
    assert_eq!(cycles, 4, "the direct-indexed read paid its add cycle");
}

#[test]
fn indexed_direct_pointer_wrap_reads_page_zero() {
    let (mut cpu, mut bus) = boot();
    bus.store(0x0002, 0x0E);
    bus.store(0x0003, 0x00);
    bus.store(0x000E, 0x55);
    // LDA ($F0,X) with X = $12: the pointer address wraps to $02 and
    // the index add shows as an internal cycle.
    bus.load(u32::from(PROGRAM), &[0xA1, 0xF0]);
    cpu.regs.x = 0x12;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x55,
        "the pointer value named the data byte"
    );
    assert_eq!(cycles, 6, "the pointer walk costs six cycles");
    assert!(
        bus.reads.contains(&0x02),
        "the wrapped pointer address was read"
    );
    assert!(
        bus.reads.contains(&0x03),
        "the pointer's second byte was read"
    );
}

#[test]
fn indexed_direct_spans_freely_in_native() {
    let (mut cpu, mut bus) = boot_native();
    // REP #$10 widens the index; LDX #$0010; LDA $F0,X crosses the
    // zero page in native mode.
    bus.load(0x8002, &[0xC2, 0x10, 0xA2, 0x10, 0x00, 0xB5, 0xF0]);
    run_steps(&mut cpu, &mut bus, 2);
    bus.store(0x00F0, 0x00);
    bus.store(0x0100, 0x88);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x88,
        "the free sum crossed the zero page"
    );
    assert_eq!(cycles, 4, "the 8-bit accumulator read");
    assert!(bus.reads.contains(&0x0100), "the crossed address was read");
}

#[test]
fn pointer_walk_wraps_in_emulation_and_walks_linearly_in_native() {
    // Emulation mode: the pointer at $FF walks its high byte from
    // page zero, the 6502 form.
    let (mut cpu, mut bus) = boot();
    bus.store(0x0000, 0x03);
    bus.store(0x00FF, 0x06);
    bus.store(0x0306, 0x99);
    bus.load(u32::from(PROGRAM), &[0xB2, 0xFF]);
    step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x99,
        "the pointer wrapped to page zero"
    );
    assert_eq!(bus.read_count(0x0100), 0, "the walk did not leave the page");

    // Native mode: the same pointer walks $FF and $0100 linearly.
    let (mut cpu2, mut bus2) = boot_native();
    bus2.store(0x00FF, 0x06);
    bus2.store(0x0100, 0x03);
    bus2.store(0x0306, 0x99);
    bus2.load(0x8002, &[0xB2, 0xFF]);
    step_one(&mut cpu2, &mut bus2);
    assert_eq!(cpu2.regs.c & 0x00FF, 0x99, "the pointer walked into $0100");
    assert_eq!(bus2.read_count(0x0100), 1, "the walk read the next page");
}

#[test]
fn long_pointer_walks_linearly_into_next_page() {
    // [dp] with the pointer at $00FE: the walk increments past $FF,
    // per the data sheet's direct addressing exception.
    let (mut cpu, mut bus) = boot();
    bus.store(0x00FE, 0x06);
    bus.store(0x00FF, 0x00);
    bus.store(0x0100, 0x01);
    bus.store(0x010006, 0x5A);
    bus.load(u32::from(PROGRAM), &[0xA7, 0xFE]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c & 0x00FF, 0x5A, "the long pointer read bank 1");
    assert_eq!(cycles, 6, "the long pointer form costs six cycles");
    assert_eq!(bus.read_count(0x0100), 1, "the bank byte walked past $FF");
}

#[test]
fn long_pointer_y_index_applies() {
    let (mut cpu, mut bus) = boot();
    bus.store(0x0020, 0x00);
    bus.store(0x0021, 0x80);
    bus.store(0x0022, 0x01);
    bus.store(0x018000, 0x5A);
    bus.load(u32::from(PROGRAM), &[0xB7, 0x20]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x5A,
        "the Y index landed on the long base"
    );
    assert_eq!(cycles, 6);
}

#[test]
fn data_bank_register_scopes_absolute_data() {
    let (mut cpu, mut bus) = boot_native();
    // LDA #$02; PHA; PLB moves the data bank.
    bus.load(0x8002, &[0xA9, 0x02, 0x48, 0xAB]);
    run_steps(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.regs.dbr, 0x02, "PLB set the data bank");
    bus.store(0x020500, 0x99);
    bus.load(0x8006, &[0xAD, 0x00, 0x05]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.c & 0x00FF,
        0x99,
        "the absolute read sat in the data bank"
    );
    assert_eq!(cycles, 4);
}

#[test]
fn word_read_wraps_within_data_bank() {
    let (mut cpu, mut bus) = boot_native();
    // LDA #$02; PHA; PLB; REP #$20; LDA $FFFF.
    bus.load(
        0x8002,
        &[0xA9, 0x02, 0x48, 0xAB, 0xC2, 0x20, 0xAD, 0xFF, 0xFF],
    );
    run_steps(&mut cpu, &mut bus, 4);
    bus.store(0x02FFFF, 0x21);
    bus.store(0x020000, 0x43);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c, 0x4321, "the wide read wrapped within the bank");
    assert_eq!(cycles, 5, "the wide absolute read costs five cycles");
}

#[test]
fn program_bank_scopes_fetches() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0x5C, 0x00, 0x00, 0x03]);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pbr, 0x03);
    assert_eq!(cpu.regs.pc, 0x0000);
    bus.store(0x030000, 0xEA);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 2, "the opcode fetched in the new bank");
}

#[test]
fn jump_indirect_keeps_program_bank() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0x6C, 0x20, 0x90]);
    bus.store(0x9020, 0x00);
    bus.store(0x9021, 0x40);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x4000);
    assert_eq!(cpu.regs.pbr, 0, "the program bank stayed");
}

#[test]
fn indexed_jump_reads_pointer_in_program_bank() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0x7C, 0xFF, 0x12]);
    bus.store(0x1307, 0x00);
    bus.store(0x1308, 0x80);
    cpu.regs.x = 0x08;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x8000);
    assert_eq!(cycles, 6, "the indexed jump keeps its fix-up read");
    assert!(
        bus.reads.contains(&0x1207),
        "the fix-up read went to the base page"
    );
}

#[test]
fn relative_long_branch_is_four_cycles() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0x82, 0xF0, 0x02]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cpu.regs.pc, 0x82F3,
        "the signed offset added past the operand"
    );
    assert_eq!(cycles, 4);
}

// --- The register widths -----------------------------------------------------

#[test]
fn rep_and_sep_flip_register_widths() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xC2, 0x30, 0xE2, 0x10]);
    run_steps(&mut cpu, &mut bus, 3);
    assert!(
        cpu.m_wide() && !cpu.x_wide(),
        "SEP #$10 narrowed only the index"
    );
}

#[test]
fn register_selects_relock_in_emulation() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0xC2, 0x30]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert!(!cpu.m_wide() && !cpu.x_wide());
    assert_eq!(
        cpu.regs.p & (FLAG_M | FLAG_X),
        FLAG_M | FLAG_X,
        "the selects relocked after the emulation-mode REP"
    );
    assert_eq!(cycles, 3);
}

#[test]
fn exchange_carry_and_emulation_round_trip() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0x18, 0xFB]);
    run_steps(&mut cpu, &mut bus, 2);
    assert!(!cpu.regs.e);
    assert_eq!(
        cpu.regs.p & FLAG_C,
        FLAG_C,
        "the carry took the emulation bit"
    );
    // SEC; XCE re-enters; the register forms force.
    cpu.regs.x = 0x1234;
    bus.load(0x8002, &[0x38, 0xFB]);
    run_steps(&mut cpu, &mut bus, 2);
    assert!(cpu.regs.e);
    assert_eq!(
        cpu.regs.p & FLAG_C,
        0,
        "the carry took the old emulation bit"
    );
    assert_eq!(cpu.regs.s & 0xFF00, 0x0100, "the stack moved to page one");
    assert_eq!(cpu.regs.x, 0x0034, "the index truncated on entry");
    assert_eq!(cpu.regs.p & (FLAG_M | FLAG_X), FLAG_M | FLAG_X);
}

#[test]
fn accumulator_transfer_moves_wide_when_index_wide() {
    let (mut cpu, mut bus) = boot_native();
    // REP #$10 widens the index; the accumulator stays 8-bit.
    bus.load(0x8002, &[0xC2, 0x10]);
    run_steps(&mut cpu, &mut bus, 1);
    assert!(cpu.x_wide());
    cpu.regs.c = 0x0142;
    bus.load(0x8004, &[0xAA]);
    cpu.regs.pc = 0x8004;
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.x, 0x0142, "the full C moved although M is set");
    assert_eq!(cpu.regs.p & FLAG_N, 0, "the flags read the moved value");
}

#[test]
fn transfer_truncates_at_eight_bit_destination() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xE2, 0x10, 0xA9, 0x77]);
    run_steps(&mut cpu, &mut bus, 2);
    assert!(!cpu.x_wide());
    cpu.regs.c = 0x0188;
    bus.load(0x8006, &[0xAA]);
    cpu.regs.pc = 0x8006;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.x, 0x0088, "the low byte moved");
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N, "the flags read the moved byte");
    assert_eq!(cycles, 2);
}

#[test]
fn stack_transfers_follow_mode() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.c = 0x1234;
    bus.load(u32::from(PROGRAM), &[0x1B]);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.s, 0x0134, "the stack kept its page-one form");
    bus.load(0x8001, &[0xBA]);
    cpu.regs.pc = 0x8001;
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.x, 0x0034, "the stack low byte moved");
}

#[test]
fn direct_register_transfers_move_sixteen_bits() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.c = 0x0400;
    bus.load(u32::from(PROGRAM), &[0x5B]);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.d, 0x0400, "the direct register kept 16 bits");
    bus.load(0x8001, &[0x7B]);
    cpu.regs.pc = 0x8001;
    cpu.regs.c = 0;
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c, 0x0400, "the transfer moved 16 bits back");
    assert_eq!(cpu.regs.p & FLAG_N, 0);
}

#[test]
fn xba_swaps_bytes_and_sets_flags_from_new_low() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.c = 0x12AA;
    bus.load(u32::from(PROGRAM), &[0xEB]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c & 0x00FF, 0x12, "the high byte became the low");
    assert_eq!(cpu.regs.c >> 8, 0xAA, "the low byte became the high");
    assert_eq!(cycles, 3);
}

// --- The stack forms ---------------------------------------------------------

#[test]
fn sixteen_bit_push_writes_high_byte_first() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xC2, 0x20, 0xA9, 0x34, 0x12, 0x48]);
    run_steps(&mut cpu, &mut bus, 2);
    step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(0x01FD), 0x12, "the high byte wrote first");
    assert_eq!(bus.byte(0x01FC), 0x34, "the low byte wrote second");
    assert_eq!(cpu.regs.s, 0x01FB);
}

#[test]
fn sixteen_bit_pull_reads_low_byte_first() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xC2, 0x20]);
    run_steps(&mut cpu, &mut bus, 2);
    cpu.regs.s = 0x01F9;
    bus.store(0x01FA, 0x88);
    bus.store(0x01FB, 0x66);
    bus.load(0x8006, &[0x68]);
    cpu.regs.pc = 0x8006;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c, 0x6688, "the low byte pulled first");
    assert_eq!(cycles, 5, "the wide pull costs one cycle more");
}

#[test]
fn emulation_stack_wraps_within_page_one() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.s = 0x0100;
    bus.load(u32::from(PROGRAM), &[0x48]);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.s, 0x01FF, "the pointer wrapped within page one");
}

#[test]
fn status_push_carries_break_bit_and_pull_forces_selects() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0x08, 0x28]);
    run_steps(&mut cpu, &mut bus, 2);
    let push_byte = bus.byte(0x01FD);
    assert_eq!(
        push_byte & 0x30,
        0x30,
        "the push carried the break and unused bits"
    );
    assert_eq!(cpu.regs.s, 0x01FD, "the pull balanced the push");
    assert_eq!(cpu.regs.p & 0x30, 0x30, "the pull forced the selects");
}

// --- The interrupts ----------------------------------------------------------

#[test]
fn hardware_interrupt_pushes_cleared_break_bit() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.p &= !FLAG_I;
    cpu.notify_line("irq", Level::High, Tick(0));
    bus.store(0xFFFE, 0x00);
    bus.store(0xFFFF, 0x81);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7, "the emulation interrupt costs seven cycles");
    assert_eq!(cpu.regs.pc, 0x8100);
    assert_eq!(cpu.regs.p & FLAG_I, FLAG_I, "the interrupt disable set");
    assert_eq!(cpu.regs.p & FLAG_D, 0, "the decimal flag cleared");
    assert_eq!(cpu.regs.pbr, 0);
    // Push layout: PC high at $01FD, PC low at $01FC, status at $01FB.
    assert_eq!(bus.byte(0x01FD), 0x80, "the return high byte pushed first");
    assert_eq!(bus.byte(0x01FC), 0x00, "the return low byte");
    assert_eq!(bus.byte(0x01FB) & 0x30, 0x20, "bit 4 clear, bit 5 set");
}

#[test]
fn break_pushes_break_bit_and_shares_vector() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0x00, 0x5A]);
    bus.store(0xFFFE, 0x00);
    bus.store(0xFFFF, 0x81);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0x8100);
    let pushed_p = bus.byte(0x01FB);
    assert_eq!(pushed_p & 0x30, 0x30, "the break push set bit 4");
    assert_eq!(bus.byte(0x01FD), 0x80, "the return high byte");
    assert_eq!(
        bus.byte(0x01FC),
        0x02,
        "the return address pointed past BRK"
    );
}

#[test]
fn native_break_pushes_program_bank_to_native_vector() {
    let (mut cpu, mut bus) = boot_native();
    cpu.regs.pbr = 0x02;
    bus.store(0x028002, 0x00);
    bus.store(0x028003, 0x5A);
    bus.store(0xFFE6, 0x00);
    bus.store(0xFFE7, 0x81);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 8, "the native break added the bank push");
    assert_eq!(cpu.regs.pc, 0x8100);
    assert_eq!(bus.byte(0x01FD), 0x02, "the program bank pushed first");
    assert_eq!(bus.byte(0x01FC), 0x80, "the return high byte");
    assert_eq!(
        bus.byte(0x01FB),
        0x04,
        "the return named the instruction end"
    );
    assert_eq!(
        bus.byte(0x01FA) & 0x30,
        0x30,
        "the native push set the break bit and kept the M select"
    );
    assert_eq!(cpu.regs.pbr, 0x00, "the vector cleared the program bank");
}

#[test]
fn native_break_sets_the_break_bit_with_the_index_register_wide() {
    let (mut cpu, mut bus) = boot_native();
    cpu.regs.p &= !FLAG_X; // the index registers stay sixteen bit
    bus.load(u32::from(PROGRAM), &[0x00, 0x5A]);
    bus.store(0xFFE6, 0x00);
    bus.store(0xFFE7, 0x81);
    step_one(&mut cpu, &mut bus);
    assert_eq!(
        bus.byte(0x01FA) & 0x10,
        0x10,
        "the break bit set in the push"
    );
    assert_eq!(
        bus.byte(0x01FA) & 0x20,
        0x20,
        "the M select carries its live form"
    );
}

#[test]
fn native_irq_pushes_the_break_bit_clear() {
    let (mut cpu, mut bus) = boot_native();
    cpu.regs.p |= FLAG_X; // the index select set, so the old form pushed bit 4 set
    cpu.regs.p &= !FLAG_I;
    cpu.notify_line("irq", Level::High, Tick(0));
    bus.store(0xFFEE, 0x00);
    bus.store(0xFFEF, 0x81);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cycles, 8,
        "the native hardware interrupt costs eight cycles"
    );
    assert_eq!(cpu.regs.pc, 0x8100);
    assert_eq!(
        bus.byte(0x01FA) & 0x10,
        0,
        "the hardware form pushes break clear"
    );
}

#[test]
fn cop_vectors_follow_mode() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0x02, 0x00]);
    bus.store(0xFFF4, 0x00);
    bus.store(0xFFF5, 0xC0);
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.pc, 0xC000, "the emulation COP vector is $FFF4");

    let (mut cpu2, mut bus2) = boot_native();
    bus2.load(0x8002, &[0x02, 0x00]);
    bus2.store(0xFFE4, 0x00);
    bus2.store(0xFFE5, 0xC0);
    step_one(&mut cpu2, &mut bus2);
    assert_eq!(cpu2.regs.pc, 0xC000, "the native COP vector is $FFE4");
}

#[test]
fn nmi_service_in_both_modes() {
    let (mut cpu, mut bus) = boot();
    cpu.notify_line("nmi", Level::High, Tick(0));
    bus.store(0xFFFA, 0x00);
    bus.store(0xFFFB, 0xD0);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7);
    assert_eq!(cpu.regs.pc, 0xD000);

    let (mut cpu2, mut bus2) = boot_native();
    bus2.load(0x8002, &[0xEA]);
    run_steps(&mut cpu2, &mut bus2, 2);
    cpu2.notify_line("nmi", Level::High, Tick(0));
    bus2.store(0xFFEA, 0x00);
    bus2.store(0xFFEB, 0xD0);
    let cycles = step_one(&mut cpu2, &mut bus2);
    assert_eq!(cycles, 8, "the native NMI adds the bank push");
    assert_eq!(cpu2.regs.pc, 0xD000);
}

#[test]
fn return_interrupt_restores_state() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.s = 0x01FC;
    bus.store(0x01FD, 0x37);
    bus.store(0x01FE, 0x50);
    bus.store(0x01FF, 0x12);
    bus.load(u32::from(PROGRAM), &[0x40]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 6);
    assert_eq!(cpu.regs.pc, 0x1250);
    assert_eq!(
        cpu.regs.p & 0x07,
        0x07,
        "the pulled flags carried C, Z, and I"
    );
    assert_eq!(
        cpu.regs.p & (FLAG_M | FLAG_X),
        FLAG_M | FLAG_X,
        "the selects forced"
    );
    assert_eq!(cpu.regs.s, 0x01FF, "the pulls restored the pointer");
}

#[test]
fn return_interrupt_native_pulls_bank() {
    let (mut cpu, mut bus) = boot_native();
    // The pushed form: PBR at $01FC, PC high at low, then status.
    bus.load(0x8002, &[0x40]);
    cpu.regs.s = 0x01F9;
    bus.store(0x01FA, 0x37);
    bus.store(0x01FB, 0x21);
    bus.store(0x01FC, 0x77);
    bus.store(0x01FD, 0x09);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7, "the native return pulled the bank byte");
    assert_eq!(cpu.regs.pc, 0x7721);
    assert_eq!(cpu.regs.pbr, 0x09, "the program bank restored");
}

#[test]
fn subroutine_return_form_round_trips() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0x20, 0x00, 0x90]);
    let call = step_one(&mut cpu, &mut bus);
    assert_eq!(call, 6);
    assert_eq!(cpu.regs.pc, 0x9000);
    assert_eq!(bus.byte(0x01FD), 0x80, "the pushed return high byte");
    assert_eq!(
        bus.byte(0x01FC),
        0x04,
        "the pushed return named the last byte"
    );
    bus.load(0x9000, &[0x60]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 6);
    assert_eq!(cpu.regs.pc, 0x8005, "the return landed past the JSR");
    assert_eq!(cpu.regs.s, 0x01FD, "the stack balanced");
}

#[test]
fn long_call_and_return_round_trip() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0x22, 0x00, 0x00, 0x05]);
    let call = step_one(&mut cpu, &mut bus);
    assert_eq!(call, 8, "the long call costs eight cycles");
    assert_eq!(cpu.regs.pc, 0x0000);
    assert_eq!(cpu.regs.pbr, 0x05);
    assert_eq!(bus.byte(0x01FD), 0x00, "the bank pushed first");
    assert_eq!(bus.byte(0x01FC), 0x80, "the return high byte");
    assert_eq!(
        bus.byte(0x01FB),
        0x05,
        "the return named the last operand byte"
    );
    bus.load(0x050000, &[0x6B]);
    let ret = step_one(&mut cpu, &mut bus);
    assert_eq!(ret, 6, "the return long costs six cycles");
    assert_eq!(cpu.regs.pc, 0x8006);
    assert_eq!(cpu.regs.pbr, 0x00, "the program bank restored");
    assert_eq!(cpu.regs.s, 0x01FD, "the stack balanced");
}

// --- Read-modify-write, bit tests, stores ------------------------------------

#[test]
fn memory_rotate_and_increment_forms() {
    let (mut cpu, mut bus) = boot();
    bus.store(0x0040, 0x0F);
    bus.load(u32::from(PROGRAM), &[0x26, 0x40]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(0x0040), 0x1E, "ROL applied without the carry");
    assert_eq!(cycles, 5);

    let (mut cpu2, mut bus2) = boot_native();
    bus2.load(0x8002, &[0xC2, 0x20, 0xEE, 0x40, 0x00]);
    run_steps(&mut cpu2, &mut bus2, 1);
    bus2.store(0x0040, 0xFF);
    bus2.store(0x0041, 0x0F);
    let cycles = step_one(&mut cpu2, &mut bus2);
    assert_eq!(bus2.byte(0x0040), 0x00, "the low byte wrapped");
    assert_eq!(
        bus2.byte(0x0041),
        0x10,
        "the carry carried to the high byte"
    );
    assert_eq!(cycles, 8, "the wide read-modify-write added its cycles");
}

#[test]
fn test_and_set_and_reset_bits() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.c = 0x60;
    bus.store(0x0050, 0x18);
    bus.load(u32::from(PROGRAM), &[0x04, 0x50]);
    step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(0x0050), 0x78, "TSB set the accumulator bits");
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z, "no bits overlap, so Z sets");
    cpu.regs.c = 0x10;
    bus.load(0x8002, &[0x14, 0x50]);
    cpu.regs.pc = 0x8002;
    step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(0x0050), 0x68, "TRB cleared the accumulator bit");
    assert_eq!(cpu.regs.p & FLAG_Z, 0);
}

#[test]
fn store_zero_and_index_forms() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.x = 0x0042;
    bus.load(u32::from(PROGRAM), &[0x86, 0x30]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(0x0030), 0x42);
    assert_eq!(cycles, 3);
}

#[test]
fn wide_store_zero_writes_both_bytes() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xC2, 0x20, 0x9C, 0x00, 0x90]);
    run_steps(&mut cpu, &mut bus, 1);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(0x9000), 0);
    assert_eq!(bus.byte(0x9001), 0, "the wide store wrote both bytes");
    assert_eq!(cycles, 5, "the wide store costs one cycle more");
}

// --- The block move and the instruction states -------------------------------

#[test]
fn block_move_transfers_one_byte_per_visit() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xC2, 0x30, 0x54, 0x02, 0x01]);
    run_steps(&mut cpu, &mut bus, 1);
    for i in 0..3u32 {
        bus.store(0x011000 + i, 0x50 + i as u8);
        bus.store(0x022000 + i, 0);
    }
    cpu.regs.c = 2;
    cpu.regs.x = 0x1000;
    cpu.regs.y = 0x2000;
    cpu.regs.pc = 0x8004;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 7, "one byte per visit costs seven cycles");
    assert_eq!(bus.byte(0x022000), 0x50, "the first byte moved");
    assert_eq!(cpu.regs.x, 0x1001, "the source index advanced");
    assert_eq!(cpu.regs.y, 0x2001, "the destination index advanced");
    assert_eq!(cpu.regs.c, 1, "the count decremented");
    assert_eq!(cpu.regs.pc, 0x8004, "the instruction re-entered");
    assert_eq!(
        cpu.regs.dbr, 0x02,
        "the destination bank took the data bank"
    );
    loop {
        step_one(&mut cpu, &mut bus);
        if cpu.regs.pc != 0x8004 {
            break;
        }
    }
    assert_eq!(cpu.regs.c, 0xFFFF, "the count finished at the wrap");
    assert_eq!(bus.byte(0x022002), 0x52, "the last byte moved");
    assert_eq!(cpu.regs.x, 0x1003);
    assert_eq!(cpu.regs.y, 0x2003);
    assert_eq!(cpu.regs.s, 0x01FD, "the block move left no stack drift");
}

#[test]
fn block_move_decrements_in_positive_direction() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xC2, 0x30, 0x44, 0x07, 0x07]);
    run_steps(&mut cpu, &mut bus, 1);
    bus.store(0x071005, 0xAA);
    bus.store(0x073005, 0);
    cpu.regs.c = 0;
    cpu.regs.x = 0x1005;
    cpu.regs.y = 0x3005;
    cpu.regs.pc = 0x8004;
    step_one(&mut cpu, &mut bus);
    assert_eq!(bus.byte(0x073005), 0xAA, "the byte moved down");
    assert_eq!(cpu.regs.x, 0x1004, "the source stepped back");
    assert_eq!(cpu.regs.y, 0x3004);
    assert_eq!(cpu.regs.c, 0xFFFF, "the one-byte move finished at the wrap");
    assert_eq!(cpu.regs.dbr, 0x07, "the destination bank loaded");
    assert_eq!(
        cpu.regs.pc, 0x8007,
        "the single move finished the instruction"
    );
}

#[test]
fn wait_instruction_sleeps_until_interrupt() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0xCB]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 3, "WAI spends three cycles");
    assert!(cpu.waiting);
    assert_eq!(cpu.next_event(), None, "the core keeps no scheduled visit");
    cpu.regs.p &= !FLAG_I;
    cpu.notify_line("irq", Level::High, Tick(31));
    assert!(!cpu.waiting, "the line woke the core");
    assert_eq!(
        cpu.next_event(),
        Some(Tick(36)),
        "the wake scheduled a boundary"
    );
    bus.store(0xFFFE, 0x00);
    bus.store(0xFFFF, 0x40);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cycles, 7,
        "the interrupt service ran in full after the wake"
    );
    assert_eq!(cpu.regs.pc, 0x4000);
}

#[test]
fn wait_wake_with_masked_interrupt_resumes() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0xCB, 0xEA]);
    run_steps(&mut cpu, &mut bus, 1);
    assert!(cpu.waiting);
    // The interrupt disable is set: the wake releases the core but no
    // service runs, per the data sheet's WAI description.
    cpu.notify_line("irq", Level::High, Tick(31));
    assert!(!cpu.waiting);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 2, "the instruction after WAI ran instead");
    assert_eq!(cpu.regs.pc, 0x8002);
}

#[test]
fn stop_instruction_releases_through_reset() {
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0xDB]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 3, "STP spends three cycles");
    assert!(cpu.stopped);
    assert_eq!(cpu.next_event(), None);
    cpu.notify_line("irq", Level::High, Tick(31));
    assert_eq!(
        cpu.next_event(),
        None,
        "the stop releases only through reset"
    );
    // A fresh reset re-enters the power-on sequence.
    let mut cpu2 = cpu_of(6);
    cpu2.reset();
    cpu2.boot(None);
    assert_eq!(
        cpu2.next_event(),
        Some(Tick(0)),
        "the reset rescheduled the sequence"
    );
}

#[test]
fn reset_line_reenters_power_on_sequence() {
    let (mut cpu, mut bus) = boot_native();
    cpu.notify_line("reset", Level::High, Tick(24));
    assert_eq!(cpu.next_event(), Some(Tick(24)), "the reset line scheduled");
    step_one(&mut cpu, &mut bus);
    assert!(cpu.regs.e, "the reset sequence left emulation mode");
    assert_eq!(
        cpu.regs.s, 0x01FD,
        "the stack pointer came back to page one"
    );
    assert_eq!(cpu.regs.pc, PROGRAM, "the reset vector read again");
    assert_ne!(cpu.next_event(), None, "the sequence rescheduled the core");
}

// --- The published cycle cells ----------------------------------------------

#[test]
fn published_cycle_cells() {
    // LDA dp,X in emulation mode: 4 cycles counting the index add.
    let (mut cpu, mut bus) = boot();
    bus.load(u32::from(PROGRAM), &[0xA2, 0x02, 0xB5, 0x50]);
    run_steps(&mut cpu, &mut bus, 1);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "the direct-indexed read paid its add cycle");
    // LDA (dp) in emulation mode: 5.
    bus.load(0x8004, &[0xB2, 0x40]);
    cpu.regs.pc = 0x8004;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 5);
    // LDA abs in native 16-bit: 5.
    let (mut cpu2, mut bus2) = boot_native();
    bus2.load(0x8002, &[0xC2, 0x20, 0xAD, 0x00, 0x90]);
    run_steps(&mut cpu2, &mut bus2, 1);
    let cycles = step_one(&mut cpu2, &mut bus2);
    assert_eq!(cycles, 5, "the wide read cost one cycle more");
    // STA abs,X with the 16-bit accumulator: 6 (the fix-up read +
    // the two-byte store).
    bus2.load(0x8007, &[0x9D, 0x00, 0x90]);
    cpu2.regs.pc = 0x8007;
    let cycles = step_one(&mut cpu2, &mut bus2);
    assert_eq!(
        cycles, 6,
        "the indexed store kept its fix-up read plus a word write"
    );
    assert_eq!(bus2.byte(0x9000), 0);
    assert_eq!(bus2.byte(0x9001), 0);
}

#[test]
fn branch_cycles_follow_mode() {
    let (mut cpu, mut bus) = boot();
    cpu.regs.p |= FLAG_C;
    // Taken without a crossing: 3.
    bus.load(u32::from(PROGRAM), &[0xB0, 0x08]);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 3);
    assert_eq!(cpu.regs.pc, 0x800A);
    // Taken across a page in emulation mode: 4.
    bus.load(0x80FA, &[0xB0, 0x10]);
    cpu.regs.pc = 0x80FA;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "the crossed emulation branch paid a cycle");
    assert_eq!(cpu.regs.pc, 0x810C);
    // Not taken: 2.
    bus.load(0x810C, &[0xB0, 0x01]);
    cpu.regs.pc = 0x810C;
    cpu.regs.p &= !0x01;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 2);
    assert_eq!(cpu.regs.pc, 0x810E);
}

#[test]
fn direct_page_relocation_pays_low_byte_penalty() {
    let (mut cpu, mut bus) = boot_native();
    // REP #$20; TCD with C = $1200 relocates the direct page aligned.
    bus.load(0x8002, &[0xC2, 0x20]);
    step_one(&mut cpu, &mut bus);
    cpu.regs.c = 0x1200;
    bus.load(0x8004, &[0x5B, 0xA5, 0x40]);
    cpu.regs.pc = 0x8004;
    step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.d, 0x1200);
    bus.store(0x1240, 0x66);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(
        cycles, 4,
        "the wide read paid one cycle, the aligned D none"
    );
    assert_eq!(cpu.regs.c & 0x00FF, 0x66);
    assert!(bus.reads.contains(&0x1240), "the direct address carried D");
    // An unaligned direct page pays one more.
    cpu.regs.c = 0x1234;
    bus.load(0x8007, &[0x5B, 0xA5, 0x40]);
    cpu.regs.pc = 0x8007;
    step_one(&mut cpu, &mut bus);
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 5, "the unaligned direct page paid its penalty");
}

#[test]
fn stack_relative_forms() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xEA]);
    run_steps(&mut cpu, &mut bus, 1);
    cpu.regs.s = 0x01E0;
    // LDA $10,S reads at $01F0.
    bus.store(0x01F0, 0x44);
    bus.load(0x8003, &[0xA3, 0x10]);
    cpu.regs.pc = 0x8003;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c & 0x00FF, 0x44);
    assert_eq!(cycles, 4);
    // LDA ($30,S),Y: the pointer sits on the stack, the target in the
    // data bank.
    cpu.regs.y = 0;
    bus.store(0x01F0, 0x00);
    bus.store(0x01F1, 0x06);
    bus.store(0x0600, 0x66);
    bus.load(0x8005, &[0xB3, 0x10]);
    cpu.regs.pc = 0x8005;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cpu.regs.c & 0x00FF, 0x66, "the stack indirect indexed read");
    assert_eq!(cycles, 7, "the indexed stack form keeps its adder cycles");
}

#[test]
fn effective_address_pushes() {
    let (mut cpu, mut bus) = boot_native();
    bus.load(0x8002, &[0xF4, 0x20, 0x90, 0x62, 0x10, 0x00, 0xD4, 0x40]);
    run_steps(&mut cpu, &mut bus, 1);
    cpu.regs.s = 0x01FC;
    cpu.regs.pc = 0x8002;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 5, "PEA costs five cycles");
    assert_eq!(bus.byte(0x01FC), 0x90, "PEA pushed the high byte");
    assert_eq!(bus.byte(0x01FB), 0x20, "PEA pushed the low byte second");
    assert_eq!(cpu.regs.s, 0x01FA);
    // PER pushes PC plus the signed offset.
    cpu.regs.pc = 0x8005;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 6, "PER costs six cycles");
    assert_eq!(bus.byte(0x01FA), 0x80, "the PER high byte");
    assert_eq!(
        bus.byte(0x01F9),
        0x18,
        "the PER address = the instruction end + $10"
    );
    // PEI pushes the direct-page pointer's value.
    bus.store(0x0040, 0x34);
    bus.store(0x0041, 0x12);
    cpu.regs.pc = 0x8008;
    let cycles = step_one(&mut cpu, &mut bus);
    assert_eq!(cycles, 6, "PEI costs six cycles");
    assert_eq!(bus.byte(0x01F8), 0x12, "PEI pushed the pointer's high byte");
    assert_eq!(bus.byte(0x01F7), 0x34, "PEI pushed the pointer's low byte");
}

// --- The table is total ------------------------------------------------------

#[test]
fn every_opcode_runs_deterministically_in_both_modes() {
    for e in [true, false] {
        for op in 0..=255u8 {
            let (mut cpu, mut bus) = boot();
            if !e {
                bus.load(u32::from(PROGRAM), &[0x18, 0xFB]);
                run_steps(&mut cpu, &mut bus, 2);
            }
            cpu.regs.pc = 0x9000;
            bus.load(0x9000, &[op, 0xEA, 0x00, 0x90]);
            step_one(&mut cpu, &mut bus);
            let snap = cpu.snapshot();

            let (mut twin, mut bus2) = boot();
            if !e {
                bus2.load(u32::from(PROGRAM), &[0x18, 0xFB]);
                run_steps(&mut twin, &mut bus2, 2);
            }
            twin.regs.pc = 0x9000;
            bus2.load(0x9000, &[op, 0xEA, 0x00, 0x90]);
            step_one(&mut twin, &mut bus2);
            assert_eq!(
                twin.snapshot(),
                snap,
                "opcode {op:02X} was deterministic in the first run only (e = {e})"
            );
            assert!(bus.cycles() >= 2, "opcode {op:02X} ran too short");
        }
    }
}
