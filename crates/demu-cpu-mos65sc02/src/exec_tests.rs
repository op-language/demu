//! The instruction-level tests over a plain byte map.
//!
//! The tests drive the same step function the machine uses through a
//! small test bus: every byte defaults to zero and the reads and the
//! writes log in cycle order, so the asserted cycle totals cover the
//! published table cells and the asserted reads cover the address
//! forms. Data for an instruction always lands before the step that
//! reads it.

use crate::cpu::{
    Cycles, FLAG_B, FLAG_C, FLAG_D, FLAG_I, FLAG_N, FLAG_UNUSED, FLAG_V, FLAG_Z, Mos65Sc02,
};
use crate::exec::step_with;

use demu_core::Device as _;
use demu_core::Level;
use demu_core::frequency::Tick;
use demu_core::triggers::TriggerEvent;

/// The program position the tests boot into.
const PROGRAM: u16 = 0x8000;

/// A plain byte map for the tests.
#[derive(Default)]
struct TestBus {
    mem: Vec<u8>,
    k: u32,
    reads: Vec<u16>,
    writes: Vec<(u16, u8)>,
    events: Vec<TriggerEvent>,
}

impl TestBus {
    /// One byte in the map.
    fn store(&mut self, addr: u16, value: u8) {
        let index = addr as usize;
        if index >= self.mem.len() {
            self.mem.resize(index + 1, 0);
        }
        self.mem[index] = value;
    }

    /// A byte at one address, or zero.
    fn byte(&self, addr: u16) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }

    /// The number of reads at one address.
    fn read_count(&self, addr: u16) -> usize {
        self.reads.iter().filter(|a| **a == addr).count()
    }
}

impl crate::cpu::Cycles for TestBus {
    fn rd(&mut self, addr: u16) -> u8 {
        self.k += 1;
        self.reads.push(addr);
        self.byte(addr)
    }

    fn wr(&mut self, addr: u16, value: u8) {
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

    fn emit(&mut self, event: TriggerEvent) {
        self.events.push(event);
    }
}

/// A configuration that declares the tested core and one bus.
fn config_of(kind: &str) -> demu_config::model::SystemConfig {
    let raw = format!(
        r#"
        [system]
        name = "test"
        triple = "test"
        [clock.main]
        frequency = "16000000"
        [[bus]]
        name = "cpu"
        address_bits = 16
        owner = "cpu0"
        [[device]]
        name = "cpu0"
        kind = "{kind}"
        domain = "main"
        "#
    );
    let (config, _) =
        demu_config::validate(&demu_config::RawConfig::parse(&raw).expect("test config parses"))
            .expect("test config validates");
    config
}

/// A core for one instruction test.
fn cpu_of(kind: &str) -> Mos65Sc02 {
    let config = config_of(kind);
    Mos65Sc02::new(&config.devices[0].clone(), &config)
}

/// A test bus over the published program vectors.
fn bus_with_vectors() -> TestBus {
    let mut bus = TestBus::default();
    // The interrupt vectors park in an idle loop at 0xC000.
    bus.store(0xC000, 0xEA);
    bus.store(0xC001, 0x38);
    bus.store(0xFFFA, 0x00);
    bus.store(0xFFFB, 0xC0);
    bus.store(0xFFFC, 0x00);
    bus.store(0xFFFD, 0xC0);
    bus.store(0xFFFE, 0x00);
    bus.store(0xFFFF, 0xC0);
    bus
}

/// One instruction run: load the program, reset the counters, step.
fn run(cpu: &mut Mos65Sc02, bus: &mut TestBus, program: &[u8]) {
    for (i, b) in program.iter().enumerate() {
        bus.store(PROGRAM + i as u16, *b);
    }
    bus.k = 0;
    bus.reads.clear();
    bus.writes.clear();
    bus.events.clear();
    cpu.regs.pc = PROGRAM;
    step_with(cpu, Tick(0), bus);
}

#[test]
fn lda_immediate_sets_flags() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0xA9, 0x80]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.a, 0x80);
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N);
    assert_eq!(cpu.regs.p & FLAG_Z, 0);
    run(&mut cpu, &mut bus, &[0xA9, 0x00]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z);
}

#[test]
fn alu_class_reads_the_published_modes() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    // ORA $0040 absolute: fetch, fetch lo, fetch hi, read = 4.
    bus.store(0x0040, 0x0F);
    run(&mut cpu, &mut bus, &[0x0D, 0x40, 0x00]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0x0F);
    // ORA $20 zero page: fetch, fetch zp, read = 3.
    bus.store(0x20, 0x20);
    run(&mut cpu, &mut bus, &[0x05, 0x20]);
    assert_eq!(bus.cycles(), 3);
    assert_eq!(cpu.regs.a, 0x2F);
    // ORA (zp),Y with a page cross pays the wrong-address read: fetch,
    // fetch zp, read lo, read hi, broken read, read = 6.
    cpu.regs.y = 0xFF;
    bus.store(0x10, 0x20);
    bus.store(0x11, 0x02);
    bus.store(0x021F, 0x01);
    bus.store(0x031F, 0xF0);
    run(&mut cpu, &mut bus, &[0x11, 0x10]);
    assert_eq!(bus.cycles(), 6);
    assert!(bus.reads.contains(&0x021F), "the broken read happened");
    assert_eq!(bus.read_count(0x031F), 1);
    assert_eq!(cpu.regs.a, 0xFF);
}

#[test]
fn index_store_pays_the_wrong_address_read() {
    // STA $0100,X: fetch, fetch lo, fetch hi, broken read, write = 5.
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0xEE;
    cpu.regs.x = 0x10;
    run(&mut cpu, &mut bus, &[0x9D, 0x00, 0x01]);
    assert_eq!(bus.cycles(), 5);
    assert!(bus.reads.contains(&0x0110), "the broken read happened");
    assert_eq!(bus.byte(0x0110), 0xEE);
}

#[test]
fn branches_take_one_extra_cycle_and_never_a_page_cross() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    // A taken branch that wraps over the page boundary.
    cpu.regs.p |= FLAG_C;
    run(&mut cpu, &mut bus, &[0xB0, 0xFF]);
    assert_eq!(bus.cycles(), 3, "taken branch with a page cross");
    assert_eq!(cpu.regs.pc, PROGRAM.wrapping_add(2).wrapping_sub(1));
    // A not-taken branch.
    cpu.regs.p &= !FLAG_C;
    run(&mut cpu, &mut bus, &[0xB0, 0xFF]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.pc, PROGRAM + 2);
}

#[test]
fn bra_always_branches() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0x80, 0x04]);
    assert_eq!(bus.cycles(), 3);
    assert_eq!(cpu.regs.pc, PROGRAM + 2 + 4);
}

#[test]
fn bit_forms_set_the_flags() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    // The immediate BIT sets only the zero flag.
    cpu.regs.a = 0x0F;
    run(&mut cpu, &mut bus, &[0x89, 0xF0]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(
        cpu.regs.p & FLAG_Z,
        FLAG_Z,
        "A&M is zero: the zero flag set"
    );
    assert_eq!(cpu.regs.p & FLAG_V, 0, "the immediate form leaves V");
    assert_eq!(cpu.regs.p & FLAG_N, 0, "the immediate form leaves N");
    cpu.regs.a = 0x00;
    run(&mut cpu, &mut bus, &[0x89, 0xF0]);
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z);
    // The memory BIT copies N and V from the operand.
    cpu.regs.a = 0x0F;
    bus.store(0x40, 0xC0);
    run(&mut cpu, &mut bus, &[0x24, 0x40]);
    assert_eq!(bus.cycles(), 3);
    assert_eq!(cpu.regs.p & FLAG_V, FLAG_V);
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N);
}

#[test]
fn test_and_set_bits_read_modify_write() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    // TSB $40: fetch, fetch zp, read, read, write = 5; the zero flag
    // reads the old value.
    cpu.regs.a = 0x80;
    bus.store(0x40, 0x01);
    run(&mut cpu, &mut bus, &[0x04, 0x40]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z, "A and M mask to zero");
    assert_eq!(bus.byte(0x40), 0x81);
    // TRB $40: the accumulator's bits clear from the byte.
    cpu.regs.a = 0x80;
    bus.store(0x40, 0x81);
    run(&mut cpu, &mut bus, &[0x14, 0x40]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(bus.byte(0x40), 0x01);
    // The absolute forms: the target sits away from the program
    // bytes.
    cpu.regs.a = 0x0F;
    bus.store(0x8040, 0xF1);
    run(&mut cpu, &mut bus, &[0x1C, 0x40, 0x80]);
    assert_eq!(bus.byte(0x8040), 0xF0);
    assert_eq!(bus.cycles(), 6);
}

#[test]
fn stz_forms_store_zero() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0x64, 0x40]);
    assert_eq!(bus.cycles(), 3);
    assert_eq!(bus.byte(0x40), 0);
    cpu.regs.x = 0x10;
    run(&mut cpu, &mut bus, &[0x74, 0x30]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(bus.byte(0x40), 0, "the store landed at $40");
    cpu.regs.x = 0x10;
    run(&mut cpu, &mut bus, &[0x9E, 0x00, 0x80]);
    assert_eq!(bus.cycles(), 5, "STZ $hhll,X pays the store shape");
    assert_eq!(bus.byte(0x8010), 0);
}

#[test]
fn push_and_pull_of_x_and_y() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.x = 0x42;
    cpu.regs.s = 0xFF;
    run(&mut cpu, &mut bus, &[0xDA]);
    assert_eq!(bus.cycles(), 3);
    assert_eq!(bus.byte(0x01FF), 0x42);
    assert_eq!(cpu.regs.s, 0xFE);
    cpu.regs.x = 0;
    run(&mut cpu, &mut bus, &[0xFA]);
    assert_eq!(bus.cycles(), 4, "the pull form spends two idle cycles");
    assert_eq!(cpu.regs.x, 0x42);
    assert_eq!(cpu.regs.p & FLAG_Z, 0);
}

#[test]
fn the_unused_bit_reads_one_in_pushes() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.s = 0xFF;
    run(&mut cpu, &mut bus, &[0x08]);
    assert_eq!(bus.byte(0x01FF) & FLAG_UNUSED, FLAG_UNUSED);
    assert_eq!(
        bus.byte(0x01FF) & FLAG_B,
        FLAG_B,
        "PHP pushes the break flag"
    );
    // The hardware pushes leave the break flag clear.
    cpu.regs.s = 0xFF;
    cpu.nmi_edge = true;
    bus.k = 0;
    bus.writes.clear();
    step_with(&mut cpu, Tick(0), &mut bus);
    // The pushes: the PC high byte, the PC low byte, then the status
    // byte at $01FC.
    let status_pushed = bus
        .writes
        .iter()
        .find(|(a, _)| *a == 0x01FD)
        .map(|(_, b)| *b)
        .expect("the status byte pushed");
    assert_eq!(status_pushed & FLAG_B, 0);
    assert_eq!(status_pushed & FLAG_UNUSED, FLAG_UNUSED);
}

#[test]
fn interrupt_service_walks_the_published_sequence() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    // The entry: two idle cycles, three pushes, and the vector read:
    // 7 cycles in total.
    cpu.regs.s = 0xFF;
    cpu.irq_line = true;
    cpu.regs.p &= !FLAG_I;
    bus.k = 0;
    bus.reads.clear();
    bus.writes.clear();
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 7);
    assert_eq!(cpu.regs.pc, 0xC000);
    assert_eq!(cpu.regs.p & FLAG_I, FLAG_I, "the entry raised the disable");
    assert_eq!(cpu.regs.p & FLAG_B, 0, "the hardware push leaves B clear");
    assert_eq!(cpu.regs.s, 0xFC);
}

#[test]
fn brk_is_seven_cycles_with_the_break_flag_pushed() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.s = 0xFF;
    bus.k = 0;
    bus.reads.clear();
    bus.writes.clear();
    run(&mut cpu, &mut bus, &[0x00, 0x00]);
    assert_eq!(bus.cycles(), 7);
    assert_eq!(cpu.regs.pc, 0xC000);
    // The status byte pushed last: the break flag set.
    let pushed = bus
        .writes
        .iter()
        .find(|(a, _)| *a == 0x01FD)
        .map(|(_, b)| *b)
        .expect("the status byte pushed");
    assert_eq!(pushed & FLAG_B, FLAG_B);
    assert_eq!(pushed & FLAG_UNUSED, FLAG_UNUSED);
}

#[test]
fn rts_reads_the_return_address_and_post_increments() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.s = 0xFD;
    bus.store(0x01FE, 0x03);
    bus.store(0x01FF, 0x80);
    run(&mut cpu, &mut bus, &[0x60]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.regs.pc, 0x8004);
    assert_eq!(cpu.regs.s, 0xFF);
}

#[test]
fn rti_pulls_the_status_and_the_pc() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.s = 0xFC;
    bus.store(0x01FD, FLAG_C | FLAG_UNUSED);
    bus.store(0x01FE, 0x03);
    bus.store(0x01FF, 0x80);
    run(&mut cpu, &mut bus, &[0x40]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.regs.p, FLAG_C | FLAG_UNUSED);
    assert_eq!(cpu.regs.pc, 0x8003, "the pulled PC continues directly");
}

#[test]
fn jsr_pushes_the_third_byte_position() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.s = 0xFF;
    bus.store(0xC000, 0x38);
    run(&mut cpu, &mut bus, &[0x20, 0x00, 0xC0]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.regs.pc, 0xC000);
    assert_eq!(bus.writes[0], (0x01FF, 0x80), "the high byte pushed first");
    assert_eq!(
        bus.writes[1],
        (0x01FE, 0x02),
        "the return address names byte three"
    );
}

#[test]
fn jmp_indirect_does_not_wrap_its_page() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    bus.store(0x7F80, 0x34);
    bus.store(0x7F81, 0x12);
    run(&mut cpu, &mut bus, &[0x6C, 0x80, 0x7F]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(cpu.regs.pc, 0x1234, "the high byte came from $7F80+1");
}

#[test]
fn jmp_indirect_x_adds_the_index_for_the_pointer() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.x = 0x02;
    bus.store(0x7F02, 0x34);
    bus.store(0x7F03, 0x12);
    run(&mut cpu, &mut bus, &[0x7C, 0x00, 0x7F]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.regs.pc, 0x1234);
}

#[test]
fn flag_operations_are_two_cycles() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    for (opcode, flag, set) in [
        (0x18, FLAG_C, false),
        (0x38, FLAG_C, true),
        (0x58, FLAG_I, false),
        (0x78, FLAG_I, true),
        (0xB8, FLAG_V, false),
        (0xD8, FLAG_D, false),
        (0xF8, FLAG_D, true),
    ] {
        cpu.regs.p = if set {
            cpu.regs.p & !flag
        } else {
            cpu.regs.p | flag
        };
        run(&mut cpu, &mut bus, &[opcode]);
        assert_eq!(bus.cycles(), 2);
        assert_eq!(
            cpu.regs.p & flag & FLAG_V,
            if set { flag & FLAG_V } else { 0 }
        );
        assert_eq!(
            cpu.regs.p & flag & !FLAG_V,
            if set { flag & !FLAG_V } else { 0 }
        );
    }
}

#[test]
fn the_accumulator_arithmetic_works_in_binary_and_decimal() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    // Binary ADC with an overflow.
    cpu.regs.a = 0x7F;
    run(&mut cpu, &mut bus, &[0x69, 0x01]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.a, 0x80);
    assert_eq!(cpu.regs.p & FLAG_V, FLAG_V);
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N);
    assert_eq!(cpu.regs.p & FLAG_C, 0);
    // Decimal ADC: $09 + $01 = $10, not $0A.
    cpu.regs.a = 0x09;
    cpu.regs.p |= FLAG_D;
    cpu.regs.p &= !FLAG_C;
    run(&mut cpu, &mut bus, &[0x69, 0x01]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.a, 0x10, "the decimal form adjusted the nibble");
    // Decimal ADC with a carry: $99 + $01 = $00 with C set.
    cpu.regs.a = 0x99;
    run(&mut cpu, &mut bus, &[0x69, 0x01]);
    assert_eq!(cpu.regs.a, 0x00);
    assert_eq!(cpu.regs.p & FLAG_C, FLAG_C);
    // Decimal SBC with no borrow in: $00 - $01 = $99, borrow out.
    cpu.regs.a = 0x00;
    cpu.regs.p |= FLAG_D | FLAG_C;
    run(&mut cpu, &mut bus, &[0xE9, 0x01]);
    assert_eq!(cpu.regs.a, 0x99);
    assert_eq!(cpu.regs.p & FLAG_C, 0, "the borrow cleared the carry");
    assert_eq!(bus.cycles(), 2);
}

#[test]
fn sbc_binary_borrows_by_the_carry() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x09;
    cpu.regs.p |= FLAG_C;
    run(&mut cpu, &mut bus, &[0xE9, 0x0A]);
    assert_eq!(cpu.regs.a, 0xFF);
    assert_eq!(cpu.regs.p & FLAG_C, 0, "the borrow cleared the carry");
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N);
    cpu.regs.a = 0x0A;
    cpu.regs.p |= FLAG_C;
    run(&mut cpu, &mut bus, &[0xE9, 0x0A]);
    assert_eq!(cpu.regs.a, 0x00);
    assert_eq!(cpu.regs.p & FLAG_C, FLAG_C);
    assert_eq!(cpu.regs.p & FLAG_Z, FLAG_Z);
}

#[test]
fn rmw_ops_read_twice_and_write_once() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    // INC $40: fetch, fetch zp, read, read, write = 5.
    bus.store(0x40, 0xFE);
    run(&mut cpu, &mut bus, &[0xE6, 0x40]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(bus.byte(0x40), 0xFF);
    assert_eq!(bus.read_count(0x40), 2, "the RMW read the address twice");
    assert_eq!(cpu.regs.p & FLAG_N, FLAG_N);
}

#[test]
fn transfers_and_register_ops_are_two_cycles() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x42;
    run(&mut cpu, &mut bus, &[0xAA]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.x, 0x42);
    // The implied increments and decrements.
    run(&mut cpu, &mut bus, &[0x1A]);
    assert_eq!(cpu.regs.a, 0x43);
    assert_eq!(bus.cycles(), 2);
    run(&mut cpu, &mut bus, &[0x3A]);
    assert_eq!(cpu.regs.a, 0x42);
    // TXS carries no flags.
    cpu.regs.x = 0x77;
    run(&mut cpu, &mut bus, &[0x9A]);
    assert_eq!(cpu.regs.s, 0x77);
    // TSX carries flags.
    run(&mut cpu, &mut bus, &[0xBA]);
    assert_eq!(cpu.regs.x, 0x77);
}

#[test]
fn memory_shifts_follow_the_rmw_shape() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    bus.store(0x40, 0x01);
    run(&mut cpu, &mut bus, &[0x46, 0x40]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(bus.byte(0x40), 0x00);
    assert_eq!(
        cpu.regs.p & FLAG_C,
        FLAG_C,
        "the shift moved the bit into the carry"
    );
}

#[test]
fn wai_parks_until_the_irq_wakes() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.p |= FLAG_I;
    run(&mut cpu, &mut bus, &[0xCB]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.next, None, "the core parked itself");
    assert!(cpu.waiting);
    // The IRQ line rising releases the park at the next cycle
    // boundary, one instruction after the wake tick.
    cpu.notify_line("irq", Level::High, Tick(100));
    assert_eq!(cpu.next, Some(Tick(104)));
    // The service ran when the disable allowed it.
    cpu.regs.p &= !FLAG_I;
    bus.k = 0;
    step_with(&mut cpu, Tick(104), &mut bus);
    assert_eq!(cpu.regs.pc, 0xC000);
}

#[test]
fn stp_parks_until_the_reset_state() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0xDB]);
    assert!(cpu.stopped);
    assert_eq!(cpu.next, None);
    // The scheduler visit does nothing while stopped.
    bus.k = 0;
    step_with(&mut cpu, Tick(1000), &mut bus);
    assert_eq!(bus.cycles(), 0);
    // The reset state released it.
    cpu.reset_pending = true;
    step_with(&mut cpu, Tick(1000), &mut bus);
    assert_eq!(cpu.regs.pc, 0xC000);
}

#[test]
fn the_reset_sequence_lands_at_the_vector() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    cpu.regs.s = 0x00;
    cpu.reset_pending = true;
    bus.k = 0;
    bus.reads.clear();
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(
        bus.cycles(),
        7,
        "four dummy reads, the idle, and the vector"
    );
    assert_eq!(cpu.regs.pc, 0xC000);
    assert_eq!(cpu.regs.s, 0xFD, "the three dummy pushes decremented S");
}

#[test]
fn the_page_bit_forms_run_in_the_plain_flavor() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    // BBS3 $40, +6: the bit three of $40 is set: branch. Five base
    // cycles, one more taken.
    bus.store(0x40, 0x08);
    run(&mut cpu, &mut bus, &[0xBF, 0x40, 0x06]);
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.regs.pc, PROGRAM + 3 + 6);
    // BBR3: the bit set: the branch is not taken: five cycles.
    run(&mut cpu, &mut bus, &[0x3F, 0x40, 0x06]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(cpu.regs.pc, PROGRAM + 3);
}

#[test]
fn the_page_bit_store_forms_set_and_clear_the_bit() {
    let mut cpu = cpu_of("mos65sc02.cpu");
    let mut bus = bus_with_vectors();
    // SMB1 $40 sets bit one.
    bus.store(0x40, 0x00);
    run(&mut cpu, &mut bus, &[0x97, 0x40]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(bus.byte(0x40), 0x02);
    // RMB1 $40 clears it again.
    run(&mut cpu, &mut bus, &[0x17, 0x40]);
    assert_eq!(bus.cycles(), 5);
    assert_eq!(bus.byte(0x40), 0x00);
}

#[test]
fn the_lynx_flavor_treats_the_page_bit_bytes_as_nops() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    // The bit-test byte runs as a two-cycle implied NOP and leaves
    // its operand byte for the next instruction.
    run(&mut cpu, &mut bus, &[0x1F, 0xEA]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.pc, PROGRAM + 1);
    // The RMB-shaped reserved byte reads its operand with no effect.
    bus.store(0x40, 0x11);
    run(&mut cpu, &mut bus, &[0x97, 0x40]);
    assert_eq!(bus.cycles(), 3);
    assert_eq!(bus.byte(0x40), 0x11, "no machine effect");
}

#[test]
fn the_nop_forms_consume_their_operands() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    // The absolute,X-shaped reserved byte reads its operand address.
    cpu.regs.x = 0x01;
    run(&mut cpu, &mut bus, &[0xDC, 0x00, 0x80]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.pc, PROGRAM + 3);
    // The immediate BIT # cell runs as its real form: one operand
    // byte, pc advances by two.
    run(&mut cpu, &mut bus, &[0x89, 0x10]);
    assert_eq!(cpu.regs.pc, PROGRAM + 2);
    // The jam-shaped cell is a two-cycle implied NOP.
    run(&mut cpu, &mut bus, &[0x02]);
    assert_eq!(bus.cycles(), 2);
    assert_eq!(cpu.regs.pc, PROGRAM + 1);
}

#[test]
fn fetch_events_emitted_per_instruction() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0xEA]);
    assert_eq!(bus.events.len(), 1);
    assert!(matches!(bus.events[0], TriggerEvent::Fetch(pos) if pos.0 == u32::from(PROGRAM)));
}

#[test]
fn snapshot_round_trip_keeps_the_register_file() {
    let mut cpu = cpu_of("vl65nc02.cpu");
    cpu.regs.a = 0x11;
    cpu.regs.x = 0x22;
    cpu.regs.y = 0x33;
    cpu.regs.s = 0x44;
    cpu.regs.p = 0x55 | FLAG_UNUSED;
    cpu.regs.pc = 0x6677;
    let saved = cpu.snapshot();
    let mut twin = cpu_of("vl65nc02.cpu");
    twin.restore(&saved).expect("restore");
    assert_eq!(twin.regs, cpu.regs);
    assert_eq!(twin.bbr_smb, cpu.bbr_smb);
    // The restored core takes no scheduler position; the machine
    // re-seeds it.
    assert_eq!(twin.next, None);
    assert!(twin.restore(&[0; 12]).is_err());
}
