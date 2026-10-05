//! The instruction-level tests over a plain byte map.

use crate::M68000;
use crate::exec::{Flow, step_with};
use demu_core::Device as _;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;

/// The program position the tests boot into.
const PROGRAM: u32 = 0x8000;

/// A plain byte map for the tests. Every access logs in instruction
/// cycle order with its clock offset, so the asserted totals cover
/// the published rows and the asserted accesses cover the forms.
#[derive(Default)]
struct TestBus {
    mem: Vec<u8>,
    k: u64,
    reads: Vec<(u64, u32)>,
    writes: Vec<(u64, u32, u32)>,
    events: Vec<TriggerEvent>,
}

impl TestBus {
    /// Store one byte.
    fn store(&mut self, addr: u32, value: u8) {
        let index = addr as usize;
        if index >= self.mem.len() {
            self.mem.resize(index + 1, 0);
        }
        self.mem[index] = value;
    }

    /// Store one word big-endian.
    fn w(&mut self, addr: u32, value: u16) {
        self.store(addr, (value >> 8) as u8);
        self.store(addr + 1, value as u8);
    }

    /// One byte of memory, or zero.
    fn byte(&self, addr: u32) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }

    /// One word of memory, big-endian.
    fn word(&self, addr: u32) -> u16 {
        u16::from(self.byte(addr)) << 8 | u16::from(self.byte(addr + 1))
    }

    /// One longword of memory.
    fn long(&self, addr: u32) -> u32 {
        u32::from(self.word(addr)) << 16 | u32::from(self.word(addr + 2))
    }
}

impl Flow for TestBus {
    fn fetch_word(&mut self, address: u32) -> u16 {
        let even = address & !1;
        self.k += 4;
        self.reads.push((self.k - 4, even));
        self.reads.push((self.k - 4, even + 1));
        self.word(address)
    }

    fn rbyte(&mut self, address: u32) -> u8 {
        self.k += 4;
        self.reads.push((self.k - 4, address));
        self.byte(address)
    }

    fn rword(&mut self, address: u32) -> u16 {
        self.k += 4;
        self.reads.push((self.k - 4, address & !1));
        self.word(address)
    }

    fn rlong(&mut self, address: u32) -> u32 {
        let hi = self.rword(address);
        let lo = self.rword(address + 2);
        u32::from(hi) << 16 | u32::from(lo)
    }

    fn wbyte(&mut self, address: u32, value: u8) {
        self.k += 4;
        self.writes.push((self.k - 4, address, u32::from(value)));
        self.store(address, value);
    }

    fn wword(&mut self, address: u32, value: u16) {
        self.k += 4;
        let even = address & !1;
        self.writes.push((self.k - 4, even, u32::from(value >> 8)));
        self.writes
            .push((self.k - 4, even + 1, u32::from(value & 0xFF)));
        self.store(even, (value >> 8) as u8);
        self.store(even + 1, value as u8);
    }

    fn wlong(&mut self, address: u32, value: u32) {
        self.wword(address, (value >> 16) as u16);
        self.wword(address + 2, value as u16);
    }

    fn wait(&mut self, cycles: u64) {
        self.k += cycles;
    }

    fn cycles(&self) -> u64 {
        self.k
    }

    fn emit(&mut self, event: TriggerEvent) {
        self.events.push(event);
    }
}

/// A configuration that declares the tested core and one bus.
fn config_of() -> demu_config::model::SystemConfig {
    let raw = r#"
        [system]
        name = "test"
        triple = "m68000-test"
        [clock.main]
        frequency = "7670453"
        [[bus]]
        name = "cpu"
        address_bits = 24
        owner = "cpu0"
        [[device]]
        name = "cpu0"
        kind = "m68000.cpu"
        domain = "main"
        [device.params]
        bus = "cpu"
        [device.dividers]
        cpu = 1
        "#;
    demu_config::validate(&demu_config::RawConfig::parse(raw).expect("test config parses"))
        .expect("test config validates")
        .0
}

/// A core for one instruction test, past the boot.
fn cpu_of() -> M68000 {
    let config = config_of();
    let mut cpu = M68000::new(&config.devices[0].clone(), &config);
    cpu.reset();
    cpu.boot(None);
    cpu
}

/// Load the program and clear the bus counters.
fn prepare(cpu: &mut M68000, bus: &mut TestBus, words: &[u16]) {
    for (i, word) in words.iter().enumerate() {
        bus.w(PROGRAM + 2 * u32::try_from(i).expect("word count"), *word);
    }
    bus.k = 0;
    bus.reads.clear();
    bus.writes.clear();
    bus.events.clear();
    cpu.pc = PROGRAM;
}

/// One instruction run: load the program words, reset the counters,
/// step with a supervisor context and the stack at 0x2000.
fn run(cpu: &mut M68000, bus: &mut TestBus, words: &[u16]) {
    prepare(cpu, bus, words);
    cpu.sup = true;
    cpu.ssp = 0x2000;
    step_with(cpu, Tick(0), bus);
}

/// One instruction run that keeps the stack pointer and supervisor
/// state: the frame a previous exception or call pushed stays live.
fn run_keep_sp(cpu: &mut M68000, bus: &mut TestBus, words: &[u16]) {
    prepare(cpu, bus, words);
    cpu.sup = true;
    step_with(cpu, Tick(0), bus);
}

/// One instruction run in user state with the user stack at 0x2000.
fn run_user(cpu: &mut M68000, bus: &mut TestBus, words: &[u16]) {
    prepare(cpu, bus, words);
    cpu.sup = false;
    cpu.usp = 0x2000;
    step_with(cpu, Tick(0), bus);
}

/// The nop opcode, used as the idle landing for vectors.
const NOP: u16 = 0x4E71;

/// The vector handler landing at 0x3000.
const LANDING: u32 = 0x3000;

/// A bus with the vector table filled: every vector through 64 holds
/// the landing address as one longword, and the landing runs nop.
fn bus_with_vectors() -> TestBus {
    let mut bus = TestBus::default();
    bus.w(LANDING, NOP);
    for vector in 0..65u32 {
        bus.w(4 * vector, (LANDING >> 16) as u16);
        bus.w(4 * vector + 2, (LANDING & 0xFFFF) as u16);
    }
    bus
}

/// The EA field from a mode and register pair.
fn ea(mode: u8, reg: u8) -> u16 {
    u16::from(mode & 7) << 3 | u16::from(reg & 7)
}

/// The MOVE opcode: the line carries the size, with the destination
/// register in bits 11-9, the destination mode in bits 8-6, and the
/// source effective address in bits 5-0.
fn move_op(line: u16, dst_reg: u8, dst_mode: u8, src: u16) -> u16 {
    (line & 7) << 12 | u16::from(dst_reg & 7) << 9 | u16::from(dst_mode & 7) << 6 | (src & 0x3F)
}

/// The ALU opcode for the line-8, 9, B, C, D rows: the destination
/// register in bits 11-9, the row in bits 8-6, the effective address
/// in bits 5-0.
fn alu_op(line: u16, dst_reg: u8, row: u8, src: u16) -> u16 {
    line << 12 | u16::from(dst_reg & 7) << 9 | u16::from(row & 7) << 6 | (src & 0x3F)
}

/// The line-0 immediate opcode: the operation in bits 11-9, the size
/// in bits 7-6, the effective address in bits 5-0.
fn imm_op(op: u8, size: u8, src: u16) -> u16 {
    u16::from(op) << 9 | u16::from(size & 3) << 6 | (src & 0xFF)
}

#[test]
fn nop_costs_four() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[NOP]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(
        bus.events,
        vec![TriggerEvent::Fetch(demu_core::Address(PROGRAM))]
    );
}

#[test]
fn move_between_registers_costs_four() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x1234;
    run(&mut cpu, &mut bus, &[move_op(0x3, 1, 0, 0)]); // move.w d0,d1
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[1], 0x1234);
}

#[test]
fn move_word_register_to_memory_costs_eight() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x1234;
    cpu.a[2] = 0x5000;
    run(&mut cpu, &mut bus, &[move_op(0x3, 2, 2, 0)]); // move.w d0,(a2)
    assert_eq!(bus.cycles(), 8);
    assert_eq!(bus.word(0x5000), 0x1234);
}

#[test]
fn move_word_memory_to_memory_costs_twelve() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    cpu.a[2] = 0x5100;
    bus.w(0x5000, 0x0AA5);
    run(&mut cpu, &mut bus, &[move_op(0x3, 2, 2, ea(2, 0))]); // move.w (a0),(a2)
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.word(0x5100), 0x0AA5);
}

#[test]
fn move_long_memory_to_memory_costs_twenty() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    cpu.a[2] = 0x5100;
    bus.w(0x5000, 0x1122);
    bus.w(0x5002, 0x3344);
    run(&mut cpu, &mut bus, &[move_op(0x2, 2, 2, ea(2, 0))]); // move.l (a0),(a2)
    assert_eq!(bus.cycles(), 20);
    assert_eq!(bus.long(0x5100), 0x11223344);
}

#[test]
fn move_long_from_predecrement_costs_fourteen() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5008;
    bus.w(0x5004, 0x1A2B);
    bus.w(0x5006, 0x3C4D);
    run(&mut cpu, &mut bus, &[move_op(0x2, 1, 0, ea(4, 0))]); // move.l -(a0),d1
    assert_eq!(bus.cycles(), 14);
    assert_eq!(cpu.d[1], 0x1A2B3C4D);
    assert_eq!(cpu.a[0], 0x5004);
}

#[test]
fn move_long_to_predecrement_costs_twelve() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[2] = 0x99887766;
    cpu.a[1] = 0x5008;
    run(&mut cpu, &mut bus, &[move_op(0x2, 1, 4, 2)]); // move.l d2,-(a1)
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.long(0x5004), 0x99887766);
    assert_eq!(cpu.a[1], 0x5004);
}

#[test]
fn move_word_memory_to_predecrement_costs_twelve() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    cpu.a[2] = 0x5008;
    bus.w(0x5000, 0x77DD);
    run(&mut cpu, &mut bus, &[move_op(0x3, 2, 4, ea(2, 0))]); // move.w (a0),-(a2)
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.word(0x5006), 0x77DD);
    assert_eq!(cpu.a[2], 0x5006);
}

#[test]
fn movea_sign_extends_the_word_form() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    bus.w(0x5000, 0x8123);
    run(&mut cpu, &mut bus, &[move_op(0x3, 1, 1, ea(2, 0))]); // movea.w (a0),a1
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.a[1], 0xFFFF_8123);
}

#[test]
fn movea_from_immediate_costs_twelve_long() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    run(
        &mut cpu,
        &mut bus,
        &[move_op(0x2, 3, 1, ea(7, 4)), 0x1234, 0x5678], // movea.l #imm,a3
    );
    assert_eq!(bus.cycles(), 12);
    assert_eq!(cpu.a[3], 0x12345678);
}

#[test]
fn add_forms_with_their_rows() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // add.w d1,d0: 4 clocks with the register source.
    cpu.d[0] = 0x7FFF;
    cpu.d[1] = 1;
    run(&mut cpu, &mut bus, &[alu_op(0xD, 0, 1, 1)]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[0], 0x8000);
    assert!(cpu.over);
    assert!(!cpu.zero);
    // add.l d1,d0: 8 clocks with the register direct source.
    cpu.d[0] = 1;
    cpu.d[1] = 2;
    run(&mut cpu, &mut bus, &[alu_op(0xD, 0, 2, 1)]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[0], 3);
    // add.w (a0),d1: 8 = 4 + the (an) calculation.
    cpu.a[0] = 0x5000;
    cpu.d[1] = 5;
    bus.w(0x5000, 2);
    run(&mut cpu, &mut bus, &[alu_op(0xD, 1, 1, ea(2, 0))]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[1], 7);
    // add.w d1,(a0): the memory row pays 8 + the ea.
    cpu.d[1] = 5;
    cpu.d[0] = 0;
    run(&mut cpu, &mut bus, &[alu_op(0xD, 1, 5, ea(2, 0))]);
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.word(0x5000), 7, "5 + 2 through memory");
}

#[test]
fn sub_cmp_rows_share_the_shapes() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 5;
    cpu.d[1] = 7;
    // cmp.w d1,d0 stores nothing.
    run(&mut cpu, &mut bus, &[alu_op(0xB, 0, 1, 1)]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[0], 5);
    assert!(cpu.carry);
    cpu.d[0] = 5;
    // sub.w d1,d0: 4 clocks, the register source; the word result
    // merges into the low word.
    run(&mut cpu, &mut bus, &[alu_op(0x9, 0, 1, 1)]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[0], 0xFFFE);
    assert!(cpu.carry);
    assert!(cpu.neg);
}

#[test]
fn and_or_with_immediate_pay_the_register_total() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x0F0F;
    // andi.w #$0FF0,d0.
    run(&mut cpu, &mut bus, &[imm_op(1, 1, 0), 0x0FF0]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[0], 0x0F00);
    // ori.w #$000F,d0.
    run(&mut cpu, &mut bus, &[imm_op(0, 1, 0), 0x000F]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[0], 0x0F0F);
}

#[test]
fn ori_to_memory_costs_twelve_plus_ea() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    bus.w(0x5000, 0x0F00);
    run(&mut cpu, &mut bus, &[imm_op(0, 1, ea(2, 0)), 0x00FF]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(bus.word(0x5000), 0x0FFF);
}

#[test]
fn cmpi_register_rows_pay_eight_long_fourteen() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x1234;
    // cmpi.w #$1234,d0.
    run(&mut cpu, &mut bus, &[imm_op(6, 1, 0), 0x1234]);
    assert_eq!(bus.cycles(), 8);
    assert!(cpu.zero);
    // cmpi.l #$00001234,d0: the long immediate reads the high word
    // first.
    run(&mut cpu, &mut bus, &[imm_op(6, 2, 0), 0x0000, 0x1234]);
    assert_eq!(bus.cycles(), 14);
    assert!(cpu.zero);
}

#[test]
fn eor_with_register_costs_the_published_rows() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x0F0F;
    cpu.d[1] = 0xFF00;
    // eor.w d1,d0: the extended-bit shape with the source register in
    // bits 11-9 and the destination in bits 2-0.
    run(&mut cpu, &mut bus, &[alu_op(0xB, 1, 5, 0)]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[0], 0xF00F);
}

#[test]
fn addi_to_memory_costs_sixteen() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    bus.w(0x5000, 0x0010);
    // addi.w #$0020,(a0).
    run(&mut cpu, &mut bus, &[imm_op(3, 1, ea(2, 0)), 0x0020]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(bus.word(0x5000), 0x0030);
}

#[test]
fn addq_subq_across_the_forms() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0;
    // addq.w #2,d0: the data field 2 in bits 11-9.
    run(&mut cpu, &mut bus, &[0x5000 | 2 << 9 | 1 << 6]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[0], 2);
    // subq.w #8,a1: the data field 0 names 8; bit 8 marks the sub.
    cpu.a[1] = 0x1000;
    run(&mut cpu, &mut bus, &[0x5000 | 1 << 6 | 0x09 | 0x100]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.a[1], 0x0FF8);
    // addq.w #1,(a0): the memory form.
    cpu.a[0] = 0x5000;
    bus.w(0x5000, 0x0010);
    run(&mut cpu, &mut bus, &[0x5000 | 1 << 9 | 1 << 6 | 0x10]);
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.word(0x5000), 0x0011);
}

#[test]
fn shift_register_static_counts_pay_the_row() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0xC000;
    // lsl.w #1,d0: line E with bit 8 (left), bits 4-3 = 01 logical,
    // size 1 word, count field 1 in bits 11-9.
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 8) | (1 << 6) | (1 << 3) | (1 << 9)],
    );
    assert_eq!(bus.cycles(), 8, "6 base + 2 for one step");
    assert_eq!(cpu.d[0], 0x8000);
    assert!(cpu.carry, "the bit moved out");
    assert!(cpu.neg);
    // asr.w #1,d1.
    cpu.d[1] = 0x8003;
    run(&mut cpu, &mut bus, &[0xE000 | (1 << 6) | (1 << 9) | 1]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[1], 0xC001, "the sign refill");
    assert!(cpu.carry);
    // asl.w #3,d2: the overflow rides the bits that enter the sign;
    // the mixed region below the sign sets V.
    cpu.d[2] = 0x7000;
    cpu.extend = false;
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 8) | (1 << 6) | (3 << 9) | 2],
    );
    assert_eq!(bus.cycles(), 12, "6 base + 2 per three steps");
    assert_eq!(cpu.d[2], 0x8000);
    assert!(cpu.over, "the mixed bits enter the sign");
    assert!(cpu.carry, "the bits shifted out");
}

#[test]
fn a_zero_encoded_count_means_eight_steps() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x0001;
    // lsl.w #0,d0: the encoded 0 names eight; eight left steps of a
    // low bit gives 0x0100 with no bit moved out.
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 8) | (1 << 6) | (1 << 3)],
    );
    assert_eq!(bus.cycles(), 22, "6 + 2 per eight steps");
    assert_eq!(cpu.d[0], 0x0100);
    assert!(!cpu.carry, "no bit reached the top");
    // The dynamic count 0 is a no-op that clears C.
    cpu.d[1] = 0;
    cpu.d[2] = 0x8000;
    cpu.carry = true;
    cpu.extend = true;
    // lsl.w d1,d2: the dynamic bit 5 with the count register 1.
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 8) | (1 << 6) | (1 << 3) | 0x20 | (1 << 9) | 2],
    );
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.d[2], 0x8000);
    assert!(!cpu.carry, "a zero count clears C");
    assert!(cpu.extend, "X holds its previous value");
}

#[test]
fn dynamic_count_takes_the_register_and_pays_two_per_step() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[1] = 3;
    cpu.d[2] = 0x0F0F;
    // lsl.w d1,d2: dynamic, count 3.
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 8) | (1 << 6) | (1 << 3) | 0x20 | (1 << 9) | 2],
    );
    assert_eq!(bus.cycles(), 12, "6 + 2 per three steps");
    assert_eq!(cpu.d[2], 0x7878);
}

#[test]
fn shift_memory_form_costs_eight_plus_ea() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    bus.w(0x5000, 0x0100);
    // lsl.w (a0): the memory row with bit 8 left.
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 8) | (3 << 6) | (1 << 9) | 0x10],
    );
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.word(0x5000), 0x0200);
}

#[test]
fn rotate_extends_carry_shapes() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x0001;
    cpu.extend = true;
    // roxl.w #1,d0: left, bits 4-3 = 10 for the extended rotate, the
    // count 1 in bits 11-9.
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 8) | (1 << 6) | (2 << 3) | (1 << 9)],
    );
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[0], 0x0003, "the X bit rotates in");
    assert!(!cpu.carry);
    // roxr.w #1,d1 with the extend: the low bit out feeds X and C.
    cpu.d[1] = 0x0003;
    cpu.extend = true;
    run(
        &mut cpu,
        &mut bus,
        &[0xE000 | (1 << 6) | (2 << 3) | (1 << 9) | 1],
    );
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[1], 0x8001);
    assert!(cpu.carry);
    assert!(cpu.extend);
}

#[test]
fn branch_cond_totals() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // beq falls through at 8 for the short form.
    prepare(&mut cpu, &mut bus, &[0x6702]);
    cpu.carry = false;
    cpu.zero = false;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.pc, PROGRAM + 2);
    // The word form falls through at 12.
    prepare(&mut cpu, &mut bus, &[0x6700, 0x0002]);
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 12);
    assert_eq!(cpu.pc, PROGRAM + 4);
    // beq taken with a short displacement: 10.
    prepare(&mut cpu, &mut bus, &[0x6702]);
    cpu.zero = true;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.pc, PROGRAM + 4);
    // beq taken with a word displacement: 10.
    prepare(&mut cpu, &mut bus, &[0x6700, 0x0002]);
    cpu.zero = true;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.pc, PROGRAM + 2 + 2);
    // bra always takes: 10.
    prepare(&mut cpu, &mut bus, &[0x6010]);
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.pc, PROGRAM + 0x12);
}

#[test]
fn bsr_rts_round_trip() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // bsr.s +2 lands at PROGRAM+4; the landing runs rts.
    bus.w(PROGRAM + 4, 0x4E75);
    run(&mut cpu, &mut bus, &[0x6102]);
    assert_eq!(bus.cycles(), 18);
    assert_eq!(cpu.pc, PROGRAM + 4);
    assert_eq!(cpu.ssp, 0x1FFC, "the return address pushed");
    assert_eq!(bus.long(0x1FFC), PROGRAM + 2, "the return address");
    // rts pulls the longword: 16 clocks; the frame stays live.
    run_keep_sp(&mut cpu, &mut bus, &[0x4E75]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(cpu.pc, PROGRAM + 2);
    assert_eq!(cpu.ssp, 0x2000);
}

#[test]
fn link_and_unlk_round_trip() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.ssp = 0x2000;
    // link a6,#-8 pushes the frame pointer, so a6 starts at the top.
    cpu.a[6] = 0x2000;
    run(&mut cpu, &mut bus, &[0x4E56, (-8_i16) as u16]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(cpu.ssp, 0x1FF4);
    assert_eq!(cpu.a[6], 0x1FFC, "the frame pointer");
    assert_eq!(bus.long(0x1FFC), 0x2000);
    // unlk a6: the field names a6 in bits 2-0.
    run(&mut cpu, &mut bus, &[0x4E58 | 6]);
    assert_eq!(bus.cycles(), 12);
    assert_eq!(cpu.ssp, 0x2000);
    assert_eq!(cpu.a[6], 0x2000);
}

#[test]
fn dbcc_conditional_shapes() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // dbf d1 loops while the condition stays false: 10 taken. The
    // displacement counts from the extension word.
    cpu.d[1] = 3;
    prepare(&mut cpu, &mut bus, &[0x51C8 | 1, 0x0002]);
    cpu.carry = false;
    cpu.zero = false;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.d[1] & 0xFFFF, 2);
    assert_eq!(cpu.pc, PROGRAM + 4);
    // dbf exits at the -1 count: 14. The exit leaves the count one
    // below zero in the register.
    cpu.d[2] = 0;
    cpu.carry = false;
    run(&mut cpu, &mut bus, &[0x51C8 | 2, 0x0002]);
    assert_eq!(bus.cycles(), 14);
    assert_eq!(cpu.d[2] & 0xFFFF, 0xFFFF);
    // dbt with the condition true: 12 without a decrement.
    cpu.d[3] = 5;
    cpu.carry = true;
    run(&mut cpu, &mut bus, &[0x50C8 | 3, 0x0002]);
    assert_eq!(bus.cycles(), 12);
    assert_eq!(cpu.d[3] & 0xFFFF, 5);
}

#[test]
fn jmp_lea_pea_and_jsr_rows() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    // jmp (a0): the mode-2 cell.
    run(&mut cpu, &mut bus, &[0x4ED0]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.pc, 0x5000);
    // jmp $5000.w.
    run(&mut cpu, &mut bus, &[0x4EF8, 0x5000]);
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.pc, 0x5000);
    // lea (a0),a1: 4 clocks.
    run(&mut cpu, &mut bus, &[0x41C0 | 1 << 9 | ea(2, 0)]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.a[1], 0x5000);
    // lea $20(a2),a3: 8 clocks with the displacement.
    cpu.a[2] = 0x5000;
    run(&mut cpu, &mut bus, &[0x41C0 | 3 << 9 | ea(5, 2), 0x0020]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.a[3], 0x5020);
    // jsr (a0): 16 clocks with the return pushed.
    cpu.a[0] = 0x5000;
    cpu.ssp = 0x2000;
    run(&mut cpu, &mut bus, &[0x4E90]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(cpu.pc, 0x5000);
    assert_eq!(cpu.ssp, 0x1FFC);
    assert_eq!(bus.long(0x1FFC), PROGRAM + 2);
    // pea (a0): 12 clocks.
    cpu.ssp = 0x2000;
    cpu.a[0] = 0x5000;
    run(&mut cpu, &mut bus, &[0x4850]);
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.long(0x1FFC), 0x5000);
}

#[test]
fn bit_operations_across_the_forms() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x0000_0400;
    cpu.d[1] = 10;
    // btst.l d1,d0: 6 clocks and stores nothing.
    run(&mut cpu, &mut bus, &[0x0100 | 1 << 9]);
    assert_eq!(bus.cycles(), 6);
    assert!(!cpu.zero);
    assert_eq!(cpu.d[0], 0x0000_0400);
    // btst.l d1,d2 with a high bit.
    cpu.d[1] = 31;
    cpu.d[2] = 0x8000_0000;
    run(&mut cpu, &mut bus, &[0x0100 | 1 << 9 | 2]);
    assert_eq!(bus.cycles(), 6);
    assert!(!cpu.zero);
    // btst.b #2,(a0): 8 + the calculation.
    cpu.a[0] = 0x5000;
    bus.store(0x5000, 0x04);
    run(&mut cpu, &mut bus, &[0x0800 | 0x10, 2]);
    assert_eq!(bus.cycles(), 12);
    assert!(!cpu.zero);
    // bchg.l d1,d0: 8 clocks.
    cpu.d[0] = 0;
    cpu.d[1] = 3;
    run(&mut cpu, &mut bus, &[0x0140 | 1 << 9]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.d[0], 8);
    // bclr.l #3,d0: 14 clocks.
    cpu.d[0] = 8;
    run(&mut cpu, &mut bus, &[0x0880, 3]);
    assert_eq!(bus.cycles(), 14);
    assert_eq!(cpu.d[0], 0);
    // bset.b #7,(a0): 12 + the calculation.
    cpu.a[0] = 0x5000;
    bus.store(0x5000, 0x00);
    run(&mut cpu, &mut bus, &[0x08C0 | 0x10, 7]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(bus.byte(0x5000), 0x80);
    assert!(cpu.zero, "the tested bit was clear");
}

#[test]
fn mulu_muls_small_operands() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 3;
    // mulu.w #1,d0: the immediate form reads its extension word.
    run(&mut cpu, &mut bus, &[0xC0FC, 1]);
    assert_eq!(bus.cycles(), 74, "70 + the immediate calculation");
    assert_eq!(cpu.d[0], 3);
    // mulu.w (a1),d1.
    cpu.a[1] = 0x5000;
    bus.w(0x5000, 4);
    cpu.d[1] = 5;
    cpu.zero = true;
    run(&mut cpu, &mut bus, &[0xC0C0 | 1 << 9 | ea(2, 1)]);
    assert_eq!(bus.cycles(), 74, "70 + the (an) four");
    assert_eq!(cpu.d[1], 20);
    assert!(!cpu.zero);
    // muls.w with negatives: -3 * 2 = -6 (the signed marker in bit 8).
    cpu.d[2] = 0xFFFF_FFFD;
    run(&mut cpu, &mut bus, &[0xC5FC, 2]);
    assert_eq!(bus.cycles(), 74);
    assert_eq!(cpu.d[2], 0xFFFF_FFFA);
    assert!(cpu.neg);
}

#[test]
fn divu_divides_and_divs_overflows() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[1] = 0x5000;
    bus.w(0x5000, 3);
    // divu.w (a1),d0.
    cpu.d[0] = 10;
    run(&mut cpu, &mut bus, &[0x80C0 | ea(2, 1)]);
    assert_eq!(bus.cycles(), 144, "140 + the (an) four");
    assert_eq!(cpu.d[0], (1 << 16) | 3, "the quotient 3, the remainder 1");
    // divu overflow: the quotient exceeds 16 bits.
    cpu.d[0] = 0x7000_0000;
    run(&mut cpu, &mut bus, &[0x80C0 | ea(2, 1)]);
    assert_eq!(bus.cycles(), 144);
    assert!(cpu.over);
    assert_eq!(cpu.d[0], 0x7000_0000, "the operands stay");
    // divs with negatives: -10 / 5 = -2 with a zero remainder; the
    // register holds the remainder in the high word.
    bus.w(0x5000, 5);
    cpu.d[1] = 0xFFFF_FFF6;
    run(&mut cpu, &mut bus, &[0x81C0 | 1 << 9 | ea(2, 1)]);
    assert_eq!(bus.cycles(), 162, "158 + the (an) four");
    assert_eq!(cpu.d[1], 0xFFFE);
}

#[test]
fn divide_by_zero_enters_vector_five() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[1] = 0x5000;
    bus.w(0x5000, 0);
    cpu.d[0] = 0x1234;
    run(&mut cpu, &mut bus, &[0x80C0 | ea(2, 1)]);
    assert_eq!(bus.cycles(), 42, "38 + the (an) four");
    assert!(cpu.sup);
    assert_eq!(cpu.pc, LANDING);
    // The frame: the status word and the return address.
    assert_eq!(cpu.ssp, 0x2000 - 6);
    assert_eq!(bus.long(0x1FFC), PROGRAM + 2);
    assert_eq!(bus.word(0x1FFA), 0x2000);
    assert_eq!(cpu.d[0], 0x1234, "the register stays");
}

#[test]
fn status_moves_gated_by_privilege() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // move.w #$2000,sr in supervisor state: 12 + the immediate.
    run(&mut cpu, &mut bus, &[0x46FC, 0x2000]);
    assert_eq!(bus.cycles(), 16);
    assert!(cpu.sup);
    assert_eq!(cpu.i_field, 0);
    // move.w sr,d0 from supervisor: 6 clocks.
    cpu.d[0] = 0;
    run(&mut cpu, &mut bus, &[0x40C0]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.d[0] & 0xFFFF, u32::from(cpu.sr_word()));
    // In user state, move to sr runs the privilege entry: 34 clocks.
    cpu.sup = false;
    cpu.usp = 0x2000;
    run_user(&mut cpu, &mut bus, &[0x46FC, 0x2700]);
    assert_eq!(bus.cycles(), 34);
    assert!(cpu.sup, "the violation enters supervisor");
    assert_eq!(cpu.pc, LANDING);
    assert_eq!(cpu.ssp, 0x2000 - 6, "the violation pushed the frame");
    assert_eq!(bus.long(0x1FFC), PROGRAM, "the faulting address stacked");
    // The move to ccr stays unprivileged: 12 + the immediate.
    cpu.sup = false;
    cpu.usp = 0x2000;
    run_user(&mut cpu, &mut bus, &[0x44FC, 0x0F]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(u32::from(cpu.ccr_byte()) & 0x0F, 0x0F);
    assert!(cpu.carry);
    // move.w a0,usp: 4 clocks; the address register moves.
    cpu.sup = true;
    cpu.a[0] = 0x4000;
    run(&mut cpu, &mut bus, &[0x4E60]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.usp, 0x4000);
    // move.w usp,a0: 4 clocks.
    run(&mut cpu, &mut bus, &[0x4E68]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.a[0], 0x4000);
}

#[test]
fn immediate_status_rows_cost_twenty() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // ori to sr in supervisor state: 20 clocks.
    run(&mut cpu, &mut bus, &[0x007C, 0x0000]);
    assert_eq!(bus.cycles(), 20);
    assert_eq!(cpu.sr_word() & 0x00FF, 0);
    // andi to ccr stays unprivileged: 20 clocks; the and keeps the
    // set carry bit and the immediate applies.
    cpu.carry = true;
    run(&mut cpu, &mut bus, &[0x023C, 0x0101]);
    assert_eq!(bus.cycles(), 20);
    assert_eq!(u32::from(cpu.ccr_byte()), 0x01, "the and took the byte");
    assert!(cpu.carry);
}

#[test]
fn rte_rtr_and_trap_entries() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // trap #3: 38 clocks.
    run(&mut cpu, &mut bus, &[0x4E43]);
    assert_eq!(bus.cycles(), 38);
    assert!(cpu.sup);
    assert_eq!(cpu.pc, LANDING);
    assert_eq!(bus.long(0x1FFC), PROGRAM + 2);
    // rte pulls the status then the address: 20 clocks; the trap's
    // frame stays live.
    run_keep_sp(&mut cpu, &mut bus, &[0x4E73]);
    assert_eq!(bus.cycles(), 20);
    assert_eq!(cpu.pc, PROGRAM + 2);
    assert_eq!(cpu.ssp, 0x2000);
    // rtr pulls the condition code then the address: 20 clocks.
    prepare(&mut cpu, &mut bus, &[0x4E77]);
    bus.w(0x1FFA, 0x0001);
    bus.w(0x1FFC, 0x0000);
    bus.w(0x1FFE, 0xABCD);
    cpu.ssp = 0x1FFA;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 20);
    assert_eq!(cpu.pc, 0xABCD);
    assert!(cpu.carry);
}

#[test]
fn trapv_taken_and_illegal_shapes() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.over = true;
    // trapv taken: the 34-clock entry at the advanced address.
    run(&mut cpu, &mut bus, &[0x4E76]);
    assert_eq!(bus.cycles(), 34);
    assert_eq!(cpu.pc, LANDING);
    assert_eq!(bus.long(0x1FFC), PROGRAM + 2);
    // trapv falls through at 4 clocks.
    cpu.ssp = 0x2000;
    cpu.over = false;
    run(&mut cpu, &mut bus, &[0x4E76]);
    assert_eq!(bus.cycles(), 4);
    // The illegal instruction enters vector 4 with the faulting
    // address stacked.
    run(&mut cpu, &mut bus, &[0x4AFC]);
    assert_eq!(bus.cycles(), 34);
    assert_eq!(bus.long(0x1FFC), PROGRAM);
    // The 1010 line entry: 34 clocks.
    run(&mut cpu, &mut bus, &[0xA000]);
    assert_eq!(bus.cycles(), 34);
    assert_eq!(bus.long(0x1FFC), PROGRAM);
}

#[test]
fn chk_bounds_and_traps() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[0] = 0x5000;
    bus.w(0x5000, 100);
    // chk.w (a0),d1 inside the bounds: 10 + 4.
    cpu.d[1] = 5;
    run(&mut cpu, &mut bus, &[0x4180 | 1 << 9 | ea(2, 0)]);
    assert_eq!(bus.cycles(), 14);
    // Out of bounds: the trap entry at 44 + 4.
    cpu.ssp = 0x2000;
    cpu.d[2] = 200;
    run(&mut cpu, &mut bus, &[0x4180 | 2 << 9 | ea(2, 0)]);
    assert_eq!(bus.cycles(), 48);
    assert_eq!(cpu.pc, LANDING);
}

#[test]
fn ext_exg_swap_and_scc() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // ext.w d0: 4 clocks; the high word of the register stays.
    cpu.d[0] = 0x0000_0080;
    run(&mut cpu, &mut bus, &[0x4880]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[0], 0x0000_FF80);
    assert!(cpu.neg);
    // ext.l d1: 4 clocks.
    cpu.d[1] = 0x0000_7F80;
    run(&mut cpu, &mut bus, &[0x48C0 | 1]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[1], 0x0000_7F80);
    // exg d1,d2: 6 clocks.
    cpu.d[1] = 11;
    cpu.d[2] = 22;
    run(&mut cpu, &mut bus, &[0xC140 | 1 << 9 | 2]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.d[1], 22);
    assert_eq!(cpu.d[2], 11);
    // exg a1,a2: 6 clocks.
    cpu.a[1] = 33;
    cpu.a[2] = 44;
    run(&mut cpu, &mut bus, &[0xC188 | 1 << 9 | 2]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.a[1], 44);
    assert_eq!(cpu.a[2], 33);
    // swap d3: 4 clocks.
    cpu.d[3] = 0x1234_5678;
    run(&mut cpu, &mut bus, &[0x4840 | 3]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[3], 0x5678_1234);
    // sne d4 with Z set stores zero at 4; with Z clear stores 0xFF
    // at 6 (the true-condition register row).
    cpu.d[4] = 0;
    cpu.zero = false;
    cpu.carry = false;
    run(&mut cpu, &mut bus, &[0x56C0 | 4]);
    assert_eq!(bus.cycles(), 6, "the true-condition register row");
    assert_eq!(cpu.d[4] & 0xFF, 0xFF, "sne with Z clear");
    cpu.d[4] = 0;
    cpu.zero = true;
    cpu.carry = false;
    run(&mut cpu, &mut bus, &[0x56C0 | 4]);
    assert_eq!(bus.cycles(), 4, "the false-condition register row");
    assert_eq!(cpu.d[4] & 0xFF, 0, "sne with Z set");
}

#[test]
fn tas_and_nbcd_rows() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    // tas d0: 4 clocks.
    cpu.d[0] = 0x30;
    run(&mut cpu, &mut bus, &[0x4AC0]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.d[0] & 0xFF, 0xB0);
    assert!(!cpu.zero);
    // tas (a0): 10 + 4.
    cpu.a[0] = 0x5000;
    bus.store(0x5000, 0x40);
    run(&mut cpu, &mut bus, &[0x4AC0 | 0x10]);
    assert_eq!(bus.cycles(), 14);
    assert_eq!(bus.byte(0x5000), 0xC0);
    // nbcd d1 with zero and the X set: 6 clocks.
    cpu.d[1] = 0;
    cpu.extend = true;
    run(&mut cpu, &mut bus, &[0x4800 | 1]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.d[1] & 0xFF, 0x99, "the BCD negate of zero with X");
    assert!(cpu.carry);
}

#[test]
fn movep_walks_byte_lanes() {
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.a[1] = 0x5000;
    cpu.d[2] = 0x1234;
    // movep.w d2,$10(a1): the two byte lanes sit at the base and base
    // +2, with the high byte first: 16 clocks.
    run(&mut cpu, &mut bus, &[0x0188 | 2 << 9 | 1, 0x0010]);
    assert_eq!(bus.cycles(), 16);
    assert_eq!(bus.byte(0x5010), 0x12);
    assert_eq!(bus.byte(0x5012), 0x34);
    // movep.l $10(a3),d5: 24 clocks with four lanes (the read
    // direction rides the two one-bit size rows).
    cpu.a[3] = 0x5000;
    bus.store(0x5010, 0x12);
    bus.store(0x5012, 0x34);
    bus.store(0x5014, 0x56);
    bus.store(0x5016, 0x78);
    run(&mut cpu, &mut bus, &[0x0148 | 5 << 9 | 3, 0x0010]);
    assert_eq!(bus.cycles(), 24);
    assert_eq!(cpu.d[5], 0x12345678);
}

/// The interrupt test pulls the IPL lines at device level and runs
/// the core, asserting the pinned frame order.
#[test]
fn interrupt_entry_pushes_the_pinned_frame() {
    let config = config_of();
    let mut cpu = M68000::new(&config.devices[0].clone(), &config);
    cpu.reset();
    cpu.boot(None);
    let mut bus = bus_with_vectors();
    cpu.sup = true;
    cpu.ssp = 0x2000;
    cpu.pc = PROGRAM;
    assert_eq!(cpu.interrupt_level(), 0);
    // Level 1 asserts through ipl0.
    cpu.notify_line("ipl0", Level::High, Tick(0));
    assert_eq!(cpu.interrupt_level(), 1);
    assert_eq!(cpu.next, Some(Tick(0)), "the assert woke the slot");
    run(&mut cpu, &mut bus, &[NOP]);
    assert_eq!(bus.cycles(), 44, "the published interrupt entry");
    assert!(cpu.sup);
    assert_eq!(cpu.i_field, 1, "the serviced mask installed");
    assert_eq!(cpu.pc, LANDING, "the vector 25 landed");
    // The frame: the status word at SP+0 and the return address at
    // SP+2 (the pinned Musashi order).
    assert_eq!(cpu.ssp, 0x2000 - 6);
    assert_eq!(bus.word(0x1FFA), 0x2000, "the saved status");
    assert_eq!(bus.long(0x1FFC), PROGRAM, "the next instruction stacked");
    // The release clears the level.
    cpu.notify_line("ipl0", Level::Low, Tick(0));
    assert_eq!(cpu.interrupt_level(), 0);
}

#[test]
fn level_seven_wakes_a_stopped_core() {
    let config = config_of();
    let mut cpu = M68000::new(&config.devices[0].clone(), &config);
    cpu.reset();
    cpu.boot(None);
    let mut bus = bus_with_vectors();
    cpu.sup = true;
    cpu.ssp = 0x2000;
    cpu.pc = PROGRAM;
    // Stop with the mask 5.
    run(&mut cpu, &mut bus, &[0x4E72, 0x2500]);
    assert_eq!(bus.cycles(), 8, "the fetch and the extension word");
    assert!(cpu.halted);
    assert_eq!(cpu.next, None);
    assert_eq!(cpu.i_field, 5);
    // A level below the mask would not wake the core.
    cpu.notify_line("ipl0", Level::High, Tick(0));
    assert_eq!(cpu.interrupt_level(), 1);
    assert!(cpu.interrupt_level() <= cpu.i_field);
    // Level 7 opens the mask and enters.
    cpu.notify_line("ipl2", Level::High, Tick(0));
    cpu.notify_line("ipl1", Level::High, Tick(0));
    cpu.notify_line("ipl0", Level::High, Tick(0));
    assert_eq!(cpu.interrupt_level(), 7);
    cpu.halted = false;
    run(&mut cpu, &mut bus, &[NOP]);
    assert_eq!(bus.cycles(), 44);
    assert_eq!(cpu.pc, LANDING);
    assert!(!cpu.halted);
    assert_eq!(cpu.i_field, 7);
}

#[test]
fn snapshot_round_trips_and_two_cores_match() {
    let config = config_of();
    let mut cpu = M68000::new(&config.devices[0].clone(), &config);
    cpu.reset();
    cpu.boot(None);
    cpu.d[3] = 0xDEAD_BEEF;
    cpu.d[4] = 5;
    cpu.a[2] = 0x00C0_0000;
    cpu.pc = 0x1000;
    cpu.usp = 0x4000;
    cpu.ssp = 0x8000;
    cpu.sup = true;
    cpu.i_field = 3;
    cpu.notify_line("ipl1", Level::High, Tick(1));
    let saved = cpu.snapshot();
    assert_eq!(saved.len(), 113, "the pinned chunk length");
    let mut twin = M68000::new(&config.devices[0].clone(), &config);
    twin.restore(&saved).expect("round trip");
    assert_eq!(twin.d[3], 0xDEAD_BEEF);
    assert_eq!(twin.a[2], 0x00C0_0000);
    assert_eq!(twin.pc, 0x1000);
    assert_eq!(twin.usp, 0x4000);
    assert_eq!(twin.ssp, 0x8000);
    assert!(twin.sup);
    assert_eq!(twin.i_field, 3);
    assert_eq!(twin.interrupt_level(), 2, "the IPL pins restored");
    assert_eq!(twin.next, cpu.next);
    assert_eq!(twin.last_tick, cpu.last_tick, "the last tick restored");
    // Two cores from the same snapshot run deterministically.
    let mut third = M68000::new(&config.devices[0].clone(), &config);
    third.restore(&saved).expect("second restore");
    let mut bus_a = bus_with_vectors();
    let mut bus_b = bus_with_vectors();
    let program = [move_op(0x3, 4, 0, 5)];
    run(&mut twin, &mut bus_a, &program);
    run(&mut third, &mut bus_b, &program);
    assert_eq!(bus_a.cycles(), bus_b.cycles());
    assert_eq!(twin.d[4], third.d[4]);
}

#[test]
fn restore_rejects_bad_chunks() {
    let mut cpu = cpu_of();
    let saved = cpu.snapshot();
    assert!(cpu.restore(&saved[..saved.len() - 1]).is_err());
    assert!(cpu.restore(&[]).is_err());
    let mut tagged = saved.clone();
    tagged[0] = b'X';
    assert!(cpu.restore(&tagged).is_err());
    let mut version = saved;
    version[4] = 9;
    assert!(cpu.restore(&version).is_err());
}

#[test]
fn movem_predecrement_walks_mask_unreversed_and_ends_at_the_last_store() {
    // movem.l d1/d4,-(a0): the mask bit 4 names D4, the bit 1 names
    // D1. The two longwords land at a0-4 (D4, the highest set bit
    // first) and a0-8 (D1); a0 ends eight below its start, the form
    // the prologue/epilogue contract pins.
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[1] = 0x1111;
    cpu.d[4] = 0x4444;
    cpu.a[0] = 0x0000_1000;
    // movem.l with the list at the second word: the line 4 store form
    // (01001d001S): the longword store with the predecrement, a0.
    let list = 0b0000_0000_0001_0010u16; // bit 4 (D4) and bit 1 (D1)
    run(&mut cpu, &mut bus, &[0x48E0, list]);
    let stored: Vec<(u32, u32)> = bus
        .writes
        .iter()
        .map(|(_k, address, value)| (*address, *value))
        .collect();
    assert_eq!(
        stored,
        vec![
            (0x0000_0FFC, 0),
            (0x0000_0FFD, 0),
            (0x0000_0FFE, 0x44),
            (0x0000_0FFF, 0x44),
            (0x0000_0FF8, 0),
            (0x0000_0FF9, 0),
            (0x0000_0FFA, 0x11),
            (0x0000_0FFB, 0x11),
        ],
        "the D4 longword (0x00004444) stores at a0-4, then D1 (0x00001111) at a0-8"
    );
    assert_eq!(
        cpu.a[0], 0x0000_0FF8,
        "the register lands at the last store"
    );
}

#[test]
fn movem_mask_bit_order_names_d0_first() {
    // movem.w d0,-(a0): the list bit 0 names D0 on hardware even in
    // the predecrement form.
    let mut cpu = cpu_of();
    let mut bus = bus_with_vectors();
    cpu.d[0] = 0x00AA;
    cpu.a[0] = 0x0000_2000;
    run(&mut cpu, &mut bus, &[0x48A0, 0x0001]); // movem.w d0,-(a0)
    let stored: Vec<(u32, u32)> = bus
        .writes
        .iter()
        .map(|(_k, address, value)| (*address, *value))
        .collect();
    assert_eq!(stored, vec![(0x0000_1FFE, 0x00), (0x0000_1FFF, 0xAA)]);
}
