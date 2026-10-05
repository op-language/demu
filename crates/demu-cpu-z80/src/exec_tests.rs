//! The instruction-level tests over a plain byte map.
//!
//! The tests drive the same step function the machine uses through a
//! small test bus: every byte defaults to zero, the ports default to
//! 0xFF, and the reads, the writes, and the port accesses log in
//! T-state order, so the asserted totals cover the published table
//! cells and the asserted reads cover the address forms.
//!
//! The undocumented forms run under the `undocumented` quirk flag of
//! the test configuration; a dedicated core with the flag clear runs
//! the effect-free forms.

use crate::cpu::{Cycles, FLAG_C, FLAG_H, FLAG_N, FLAG_P, FLAG_S, FLAG_X3, FLAG_X5, FLAG_Z, Z80};
use crate::exec::step_with;

use demu_core::Device as _;
use demu_core::frequency::Tick;
use demu_core::triggers::TriggerEvent;

/// The program position the tests boot into.
const PROGRAM: u16 = 0x8000;

/// A plain byte map for the tests, with a port map.
#[derive(Default)]
struct TestBus {
    mem: Vec<u8>,
    ports: Vec<u8>,
    k: u32,
    reads: Vec<u16>,
    writes: Vec<(u16, u8)>,
    io_reads: Vec<u16>,
    io_writes: Vec<(u16, u8)>,
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

    /// One byte of memory, or zero.
    fn byte(&self, addr: u16) -> u8 {
        self.mem.get(addr as usize).copied().unwrap_or(0)
    }

    /// One port byte, initialized to 0xFF like the open bus.
    fn set_port(&mut self, port: u16, value: u8) {
        self.ports.resize(0x10000, 0xFF);
        self.ports[port as usize] = value;
    }

    /// The number of reads at one address.
    fn read_count(&self, addr: u16) -> usize {
        self.reads.iter().filter(|a| **a == addr).count()
    }
}

impl Cycles for TestBus {
    fn m1(&mut self, addr: u16) -> u8 {
        self.k += 4;
        self.reads.push(addr);
        self.byte(addr)
    }

    fn rd(&mut self, addr: u16) -> u8 {
        self.k += 3;
        self.reads.push(addr);
        self.byte(addr)
    }

    fn wr(&mut self, addr: u16, value: u8) {
        self.k += 3;
        self.writes.push((addr, value));
        self.store(addr, value);
    }

    fn rd_io(&mut self, port: u16) -> u8 {
        self.k += 4;
        self.io_reads.push(port);
        self.ports.get(port as usize).copied().unwrap_or(0xFF)
    }

    fn wr_io(&mut self, port: u16, value: u8) {
        self.k += 4;
        self.io_writes.push((port, value));
        self.ports.resize(0x10000, 0xFF);
        self.ports[port as usize] = value;
    }

    fn wait(&mut self, t: u32) {
        self.k += t;
    }

    fn cycles(&self) -> u32 {
        self.k
    }

    fn emit(&mut self, event: TriggerEvent) {
        self.events.push(event);
    }
}

/// A configuration that declares the tested core and the two buses.
fn config_of(undocumented: bool) -> demu_config::model::SystemConfig {
    let quirks = if undocumented {
        "\n[quirks]\n\"z80.undocumented\" = true\n"
    } else {
        ""
    };
    let raw = format!(
        r#"
        [system]
        name = "test"
        triple = "z80-test"
        [clock.main]
        frequency = "3500000"
        [[bus]]
        name = "cpu"
        address_bits = 16
        owner = "cpu0"
        [[bus]]
        name = "io"
        address_bits = 16
        owner = "cpu0"
        [[device]]
        name = "cpu0"
        kind = "z80.cpu"
        domain = "main"
        [device.dividers]
        cpu = 1
        {quirks}
        "#
    );
    demu_config::validate(&demu_config::RawConfig::parse(&raw).expect("test config parses"))
        .expect("test config validates")
        .0
}

/// A core for one instruction test, past the power-on reset.
fn cpu_of(undocumented: bool) -> Z80 {
    let config = config_of(undocumented);
    let mut cpu = Z80::new(&config.devices[0].clone(), &config);
    cpu.reset();
    cpu.boot(None);
    // One step runs the 6 T-state reset sequence; the tests then see
    // clean instruction steps.
    let mut bus = TestBus::default();
    bus.set_port(0x0000, 0x00);
    step_with(&mut cpu, Tick(0), &mut bus);
    cpu
}

/// One instruction run: load the program, reset the counters, step.
fn run(cpu: &mut Z80, bus: &mut TestBus, program: &[u8]) {
    for (i, b) in program.iter().enumerate() {
        bus.store(PROGRAM + i as u16, *b);
    }
    bus.k = 0;
    bus.reads.clear();
    bus.writes.clear();
    bus.io_reads.clear();
    bus.io_writes.clear();
    bus.events.clear();
    cpu.regs.pc = PROGRAM;
    step_with(cpu, Tick(0), bus);
}

/// The interrupt vectors park in an idle loop at 0xC000.
fn bus_with_vectors() -> TestBus {
    let mut bus = TestBus::default();
    bus.set_port(0x0000, 0x00);
    bus.store(0xC000, 0xEA);
    bus.store(0xC001, 0x38);
    bus.store(0xFFFE, 0x38);
    bus.store(0xFFFF, 0xC0);
    bus
}

#[test]
fn nop_costs_four_t_states() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0x00]);
    assert_eq!(bus.cycles(), 4);
    assert_eq!(
        bus.events,
        vec![TriggerEvent::Fetch(demu_core::Address(0x8000))]
    );
}

#[test]
fn ld_rr_moves_with_untouched_flags() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 0x12;
    run(&mut cpu, &mut bus, &[0x48]); // LD C,B
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.c, 0x12);
    cpu.regs.f = 0xFF;
    run(&mut cpu, &mut bus, &[0x41]); // LD B,C
    assert_eq!(cpu.regs.f, 0xFF, "the flag byte survives a plain load");
}

#[test]
fn ld_immediate_costs_seven() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0x3E, 0x5A]); // LD A,0x5A
    assert_eq!(bus.cycles(), 7);
    assert_eq!(cpu.regs.a, 0x5A);
}

#[test]
fn ld_hl_memory_forms_read_and_write() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x10;
    bus.store(0xC010, 0x77);
    run(&mut cpu, &mut bus, &[0x7E]); // LD A,(HL)
    assert_eq!(bus.cycles(), 7);
    assert_eq!(cpu.regs.a, 0x77);
    cpu.regs.a = 0x33;
    run(&mut cpu, &mut bus, &[0x77]); // LD (HL),A
    assert_eq!(bus.cycles(), 7);
    assert_eq!(bus.byte(0xC010), 0x33);
}

#[test]
fn add_immediate_sets_the_published_flags() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x3C;
    run(&mut cpu, &mut bus, &[0xC6, 0x3F]); // ADD A,0x3F
    assert_eq!(bus.cycles(), 7);
    assert_eq!(cpu.regs.a, 0x7B);
    assert_eq!(cpu.regs.f & FLAG_H, FLAG_H, "the half carry of 0xC + 0xF");
    assert_eq!(cpu.regs.f & (FLAG_Z | FLAG_S), 0);
    cpu.regs.a = 0xF0;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xC6, 0x81]); // ADD A,0x81
    assert_eq!(cpu.regs.a, 0x71);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C, "the carry out");
    assert_eq!(cpu.regs.f & FLAG_P, FLAG_P, "the sign overflow");
    assert_eq!(cpu.regs.f & FLAG_S, 0, "0x71 is positive");
}

#[test]
fn adc_takes_the_carry_in() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x01;
    cpu.regs.f = FLAG_C;
    run(&mut cpu, &mut bus, &[0x8F]); // ADC A,A
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0x03);
}

#[test]
fn sub_sbc_set_the_subtract_flag() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x40;
    cpu.regs.b = 0x80;
    run(&mut cpu, &mut bus, &[0x90]); // SUB B
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0xC0);
    assert_eq!(
        cpu.regs.f & (FLAG_C | FLAG_N | FLAG_S),
        FLAG_C | FLAG_N | FLAG_S
    );
    assert_eq!(cpu.regs.f & FLAG_H, 0, "the low nibbles do not borrow");
    cpu.regs.a = 0x10;
    cpu.regs.b = 0x18;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x90]);
    assert_eq!(cpu.regs.f & FLAG_H, FLAG_H, "the low-nibble borrow");
    // SBC with the borrow held.
    cpu.regs.a = 0x05;
    cpu.regs.b = 0x03;
    cpu.regs.f = FLAG_C;
    run(&mut cpu, &mut bus, &[0x98]); // SBC A,B
    assert_eq!(cpu.regs.a, 0x01);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
}

#[test]
fn cp_keeps_the_accumulator() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x40;
    cpu.regs.b = 0x40;
    run(&mut cpu, &mut bus, &[0xB8]); // CP B
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0x40);
    assert_eq!(cpu.regs.f & FLAG_Z, FLAG_Z);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
}

#[test]
fn the_logic_forms_set_their_flags() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0xF0;
    run(&mut cpu, &mut bus, &[0xE6, 0x08]); // AND 0x08
    assert_eq!(bus.cycles(), 7);
    assert_eq!(cpu.regs.a, 0x00);
    assert_eq!(cpu.regs.f & FLAG_Z | FLAG_H, FLAG_Z | FLAG_H, "AND sets H");
    cpu.regs.a = 0x0F;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xF6, 0x80]); // OR 0x80
    assert_eq!(cpu.regs.a, 0x8F);
    assert_eq!(cpu.regs.f & FLAG_S, FLAG_S);
    assert_eq!(cpu.regs.f & FLAG_H, 0, "OR clears H");
    cpu.regs.a = 0x0F;
    run(&mut cpu, &mut bus, &[0xEE, 0x0F]); // XOR 0x0F
    assert_eq!(cpu.regs.a, 0x00);
    assert_eq!(cpu.regs.f & FLAG_Z, FLAG_Z);
}

#[test]
fn inc_and_dec_carry_the_published_forms() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x7F;
    run(&mut cpu, &mut bus, &[0x3C]); // INC A
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0x80);
    assert_eq!(cpu.regs.f & FLAG_P, FLAG_P, "0x7F adds to the overflow");
    assert_eq!(cpu.regs.f & FLAG_N, 0);
    cpu.regs.a = 0x0F;
    cpu.regs.c = 0x0F;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x0C]); // INC C
    assert_eq!(cpu.regs.f & FLAG_H, FLAG_H);
    cpu.regs.a = 0x80;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x3D]); // DEC A
    assert_eq!(cpu.regs.a, 0x7F);
    assert_eq!(cpu.regs.f & FLAG_P, FLAG_P);
    assert_eq!(cpu.regs.f & FLAG_N, FLAG_N);
    cpu.regs.a = 0x10;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x3D]);
    assert_eq!(cpu.regs.f & FLAG_H, FLAG_H);
}

#[test]
fn inc_hl_memory_costs_eleven() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x02;
    run(&mut cpu, &mut bus, &[0x34]); // INC (HL)
    assert_eq!(bus.cycles(), 11);
    assert_eq!(bus.byte(0xC002), 1);
}

#[test]
fn daa_adds_and_corrects() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    // The published manual example: 0x45 + 0x38 left 0x7D with the
    // half carry clear; DAA corrects the low nibble.
    cpu.regs.a = 0x7D;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x27]); // DAA
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0x83);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
    assert_eq!(cpu.regs.f & FLAG_H, 0);
    // 0x68 + 0x53 left 0xBB; DAA carries out.
    cpu.regs.a = 0xBB;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x27]);
    assert_eq!(cpu.regs.a, 0x21);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    // The subtract form: 0x46 - 0x39 left 0x0D with the half carry
    // set; the correction takes the borrow.
    cpu.regs.a = 0x0D;
    cpu.regs.f = FLAG_N | FLAG_H;
    run(&mut cpu, &mut bus, &[0x27]);
    assert_eq!(cpu.regs.a, 0x07);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
    assert_eq!(cpu.regs.f & FLAG_N, FLAG_N, "the subtract flag survives");
    // A valid subtract result corrects nothing.
    cpu.regs.a = 0x44;
    cpu.regs.f = FLAG_N;
    run(&mut cpu, &mut bus, &[0x27]);
    assert_eq!(cpu.regs.a, 0x44);
}

#[test]
fn complement_and_carry_forms() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0xD7;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x2F]); // CPL
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0x28);
    assert_eq!(cpu.regs.f & (FLAG_H | FLAG_N), FLAG_H | FLAG_N);
    assert_eq!(
        cpu.regs.f & FLAG_X3,
        FLAG_X3,
        "the copy of the result bit 3"
    );
    assert_eq!(
        cpu.regs.f & FLAG_X5,
        FLAG_X5,
        "the copy of the result bit 5"
    );
    cpu.regs.f = FLAG_C;
    run(&mut cpu, &mut bus, &[0x3F]); // CCF
    assert_eq!(cpu.regs.f & FLAG_C, 0, "the carry complements");
    assert_eq!(cpu.regs.f & FLAG_H, FLAG_H, "H takes the old carry");
    assert_ne!(cpu.regs.f & FLAG_N, FLAG_N);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x37]); // SCF
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    assert_eq!(cpu.regs.f & FLAG_N, 0);
}

#[test]
fn the_accumulator_rotations() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0b1000_0001;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x07]); // RLCA
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 0b0000_0011);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    assert_eq!(cpu.regs.f & FLAG_Z, 0, "the rotations leave Z clear");
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x0F]); // RRCA
    assert_eq!(cpu.regs.a, 0b1000_0001);
    cpu.regs.f = FLAG_C;
    run(&mut cpu, &mut bus, &[0x17]); // RLA
    assert_eq!(cpu.regs.a, 0b0000_0011);
    cpu.regs.f = FLAG_C;
    run(&mut cpu, &mut bus, &[0x1F]); // RRA
    assert_eq!(cpu.regs.a, 0b1000_0001);
}

#[test]
fn the_pair_loads_and_their_totals() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0x21, 0x34, 0x12]); // LD HL,0x1234
    assert_eq!(bus.cycles(), 10);
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0x1234);
    run(&mut cpu, &mut bus, &[0x31, 0x00, 0xFF]); // LD SP,0xFF00
    assert_eq!(cpu.regs.sp, 0xFF00);
    assert_eq!(bus.cycles(), 10);
    cpu.regs.h = 0x80;
    cpu.regs.l = 0x00;
    run(&mut cpu, &mut bus, &[0xF9]); // LD SP,HL
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.regs.sp, 0x8000);
}

#[test]
fn the_pair_increments_and_decrements() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.h = 0x00;
    cpu.regs.l = 0xFF;
    run(&mut cpu, &mut bus, &[0x23]); // INC HL
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.regs.l, 0x00);
    assert_eq!(cpu.regs.h, 0x01);
    run(&mut cpu, &mut bus, &[0x2B]); // DEC HL
    assert_eq!(bus.cycles(), 6);
    assert_eq!(cpu.regs.l, 0xFF);
    assert_eq!(cpu.regs.h, 0x00);
}

#[test]
fn the_sixteen_bit_memory_loads() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.h = 0x12;
    cpu.regs.l = 0x34;
    run(&mut cpu, &mut bus, &[0x22, 0x00, 0xC1]); // LD (0xC100),HL
    assert_eq!(bus.cycles(), 16);
    assert_eq!(bus.byte(0xC100), 0x34);
    assert_eq!(bus.byte(0xC101), 0x12);
    bus.store(0xC200, 0x77);
    bus.store(0xC201, 0x66);
    run(&mut cpu, &mut bus, &[0x2A, 0x00, 0xC2]); // LD HL,(0xC200)
    assert_eq!(bus.cycles(), 16);
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0x6677);
    cpu.regs.a = 0x11;
    run(&mut cpu, &mut bus, &[0x32, 0x00, 0xC3]); // LD (0xC300),A
    assert_eq!(bus.cycles(), 13);
    assert_eq!(bus.byte(0xC300), 0x11);
    bus.store(0xC400, 0x55);
    run(&mut cpu, &mut bus, &[0x3A, 0x00, 0xC4]); // LD A,(0xC400)
    assert_eq!(bus.cycles(), 13);
    assert_eq!(cpu.regs.a, 0x55);
}

#[test]
fn add_hl_ss_totals_and_flags() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.h = 0x0F;
    cpu.regs.l = 0x00;
    cpu.regs.d = 0x01;
    cpu.regs.e = 0x00;
    run(&mut cpu, &mut bus, &[0x19]); // ADD HL,DE
    assert_eq!(bus.cycles(), 11);
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0x1000);
    assert_eq!(cpu.regs.f & FLAG_H, FLAG_H, "the bit-11 carry lands in H");
    assert_eq!(cpu.regs.f & FLAG_N, 0);
    cpu.regs.b = 0x80;
    cpu.regs.c = 0x00;
    cpu.regs.f = 0;
    cpu.regs.h = 0x80;
    cpu.regs.l = 0x00;
    run(&mut cpu, &mut bus, &[0x09]); // ADD HL,BC
    assert_eq!(
        cpu.regs.f & FLAG_P,
        FLAG_P,
        "the overflow of 0x8000 + 0x8000"
    );
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
}

#[test]
fn the_ex_forms() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 1;
    cpu.regs.f = 2;
    cpu.regs.a2 = 3;
    cpu.regs.f2 = 4;
    run(&mut cpu, &mut bus, &[0x08]); // EX AF,AF'
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.a, 3);
    assert_eq!(cpu.regs.f, 4);
    assert_eq!(cpu.regs.a2, 1);
    cpu.regs.b = 0x10;
    cpu.regs.h = 0x20;
    run(&mut cpu, &mut bus, &[0xD9]); // EXX
    assert_eq!(cpu.regs.b, 0x00);
    assert_eq!(cpu.regs.b2, 0x10, "the shadow held zero before the swap");
    assert_eq!(cpu.regs.h, 0x00);
    assert_eq!(cpu.regs.h2, 0x20);
    cpu.regs.d = 0x30;
    cpu.regs.e = 0x40;
    cpu.regs.h = 0x11;
    cpu.regs.l = 0x22;
    run(&mut cpu, &mut bus, &[0xEB]); // EX DE,HL
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.d, 0x11);
    assert_eq!(cpu.regs.e, 0x22);
    assert_eq!(cpu.regs.h, 0x30);
    assert_eq!(cpu.regs.l, 0x40);
    // EX (SP),HL
    cpu.regs.sp = 0xC100;
    bus.store(0xC100, 0x55);
    bus.store(0xC101, 0x66);
    cpu.regs.h = 0xAA;
    cpu.regs.l = 0xBB;
    run(&mut cpu, &mut bus, &[0xE3]);
    assert_eq!(bus.cycles(), 19);
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0x6655);
    assert_eq!(bus.byte(0xC100), 0xBB);
    assert_eq!(bus.byte(0xC101), 0xAA);
}

#[test]
fn the_push_and_pop_forms() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.sp = 0xC0FF;
    cpu.regs.b = 0x12;
    cpu.regs.c = 0x34;
    run(&mut cpu, &mut bus, &[0xC5]); // PUSH BC
    assert_eq!(bus.cycles(), 11);
    assert_eq!(bus.byte(0xC0FE), 0x12, "the high byte lands first");
    assert_eq!(bus.byte(0xC0FD), 0x34);
    assert_eq!(cpu.regs.sp, 0xC0FD);
    cpu.regs.b = 0;
    cpu.regs.c = 0;
    run(&mut cpu, &mut bus, &[0xC1]); // POP BC
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.regs.b, 0x12);
    assert_eq!(cpu.regs.c, 0x34);
    assert_eq!(cpu.regs.sp, 0xC0FF);
    cpu.regs.a = 0x40;
    cpu.regs.f = 0x80;
    run(&mut cpu, &mut bus, &[0xF5]); // PUSH AF
    assert_eq!(bus.byte(0xC0FE), 0x40);
    assert_eq!(bus.byte(0xC0FD), 0x80);
}

#[test]
fn jump_and_branch_totals() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0xC3, 0x00, 0xC0]); // JP 0xC000
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.regs.pc, 0xC000);
    cpu.regs.pc = PROGRAM;
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x00;
    run(&mut cpu, &mut bus, &[0xE9]); // JP (HL)
    assert_eq!(bus.cycles(), 4);
    assert_eq!(cpu.regs.pc, 0xC000);
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0x18, 0x04]); // JR +4
    assert_eq!(bus.cycles(), 12);
    assert_eq!(cpu.regs.pc, PROGRAM + 6);
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0x18, 0x04, 0x00, 0x00, 0x00]); // JR
    assert_eq!(bus.cycles(), 12);
    cpu.regs.pc = PROGRAM;
    cpu.regs.f = FLAG_Z;
    run(&mut cpu, &mut bus, &[0x28, 0x02, 0x00, 0x00]); // JR Z,+2
    assert_eq!(bus.cycles(), 12);
    cpu.regs.pc = PROGRAM;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x28, 0x02, 0x00, 0x00]); // JR Z (untaken)
    assert_eq!(bus.cycles(), 7);
    // JP cc stays 10 either way.
    cpu.regs.pc = PROGRAM;
    cpu.regs.f = FLAG_Z;
    run(&mut cpu, &mut bus, &[0xC2, 0x00, 0xC0]); // JP NZ (taken)
    assert_eq!(bus.cycles(), 10);
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0xC2, 0x00, 0xC0]); // JP NZ (untaken)
    assert_eq!(bus.cycles(), 10);
}

#[test]
fn djnz_loops_thirteen_or_eight() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 2;
    run(&mut cpu, &mut bus, &[0x10, 0x02, 0x00]); // DJNZ +2
    assert_eq!(bus.cycles(), 13);
    assert_eq!(cpu.regs.b, 1);
    cpu.regs.b = 1;
    run(&mut cpu, &mut bus, &[0x10, 0x02]);
    assert_eq!(bus.cycles(), 8);
}

#[test]
fn call_ret_and_rst_shapes() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.sp = 0xC0FF;
    run(&mut cpu, &mut bus, &[0xCD, 0x00, 0xC0]); // CALL 0xC000
    assert_eq!(bus.cycles(), 17);
    assert_eq!(cpu.regs.pc, 0xC000);
    assert_eq!(u16::from(bus.byte(0xC0FE)), (PROGRAM + 3) >> 8);
    assert_eq!(bus.byte(0xC0FD), (PROGRAM + 3) as u8);
    run(&mut cpu, &mut bus, &[0xC9]); // RET
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.regs.pc, PROGRAM + 3);
    cpu.regs.sp = 0xC0FF;
    cpu.regs.f = FLAG_Z;
    run(&mut cpu, &mut bus, &[0xC4, 0x00, 0xC0]); // CALL NZ (untaken)
    assert_eq!(bus.cycles(), 10, "the untaken call");
    assert_eq!(cpu.regs.sp, 0xC0FF);
    cpu.regs.pc = PROGRAM;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xC4, 0x00, 0xC0]); // CALL NZ (taken)
    assert_eq!(bus.cycles(), 17);
    cpu.regs.pc = PROGRAM;
    cpu.regs.sp = 0xC0FF;
    run(&mut cpu, &mut bus, &[0xCF]); // RST 08h
    assert_eq!(bus.cycles(), 11);
    assert_eq!(cpu.regs.pc, 0x0008);
    assert_eq!(u16::from(bus.byte(0xC0FE)), (PROGRAM + 1) >> 8);
    cpu.regs.sp = 0xC0FF;
    cpu.regs.pc = PROGRAM;
    cpu.regs.f = FLAG_Z;
    run(&mut cpu, &mut bus, &[0xC0]); // RET NZ (untaken: Z set means NZ false)
    assert_eq!(bus.cycles(), 5);
    assert_eq!(cpu.regs.sp, 0xC0FF);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xC0]); // RET NZ (taken)
    assert_eq!(bus.cycles(), 11);
}

#[test]
fn the_port_accumulator_forms() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    bus.set_port(0x1010, 0x5A);
    cpu.regs.a = 0x10;
    run(&mut cpu, &mut bus, &[0xDB, 0x10]); // IN A,(0x10)
    assert_eq!(bus.cycles(), 11);
    assert_eq!(
        bus.io_reads,
        vec![0x1010],
        "the port high byte carries the accumulator"
    );
    assert_eq!(cpu.regs.a, 0x5A);
    cpu.regs.a = 0x77;
    run(&mut cpu, &mut bus, &[0xD3, 0x10]); // OUT (0x10),A
    assert_eq!(bus.cycles(), 11);
    assert_eq!(bus.io_writes, vec![(0x7710, 0x77)]);
}

#[test]
fn the_ed_zero_store_follows_the_quirk() {
    // The undocumented OUT (C),0 runs only with the quirk.
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 0x10;
    cpu.regs.c = 0x20;
    run(&mut cpu, &mut bus, &[0xED, 0x71]); // OUT (C),0
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.io_writes, vec![(0x1020, 0x00)]);
    // A core without the quirk falls to the unknown-form fallback:
    // no bus access and the 8 T-states of the two fetches.
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 0x10;
    cpu.regs.c = 0x20;
    run(&mut cpu, &mut bus, &[0xED, 0x71]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(bus.io_writes, vec![], "the quirk-off form writes nothing");
    // The flag-only IN (C) gates the same way: no port read and no
    // flag change without the quirk.
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 0x10;
    cpu.regs.c = 0x20;
    cpu.regs.f = FLAG_Z;
    run(&mut cpu, &mut bus, &[0xED, 0x70]);
    assert_eq!(bus.cycles(), 8);
    assert_eq!(bus.io_reads, vec![], "the quirk-off form reads nothing");
    assert_eq!(cpu.regs.f, FLAG_Z, "the quirk-off form moves no flags");
}

#[test]
fn the_ed_port_and_pair_forms() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 0x10;
    cpu.regs.c = 0x20;
    bus.set_port(0x1020, 0x33);
    run(&mut cpu, &mut bus, &[0xED, 0x50]); // IN D,(C)
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.io_reads, vec![0x1020], "the port is the BC pair");
    assert_eq!(cpu.regs.d, 0x33);
    assert_eq!(cpu.regs.f & (FLAG_S | FLAG_H), 0, "0x33 clears S and H");
    cpu.regs.d = 0x55;
    run(&mut cpu, &mut bus, &[0xED, 0x51]); // OUT (C),D
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.io_writes, vec![(0x1020, 0x55)]);
    // The undocumented OUT (C),0.
    bus.io_writes.clear();
    run(&mut cpu, &mut bus, &[0xED, 0x71]); // OUT (C),0
    assert_eq!(bus.cycles(), 12);
    assert_eq!(bus.io_writes, vec![(0x1020, 0x00)]);
    // The undocumented IN (C): flags only.
    cpu.regs.f = 0;
    bus.set_port(0x1020, 0x80);
    run(&mut cpu, &mut bus, &[0xED, 0x70]);
    assert_eq!(bus.cycles(), 12);
    assert_eq!(cpu.regs.f & FLAG_S, FLAG_S);
    assert_eq!(cpu.regs.d, 0x55, "IN (C) stores nothing");
    // LD BC,(nn) and (nn),BC
    cpu.regs.b = 0x11;
    cpu.regs.c = 0x22;
    run(&mut cpu, &mut bus, &[0xED, 0x43, 0x00, 0xC1]); // LD (0xC100),BC
    assert_eq!(bus.cycles(), 20);
    assert_eq!(bus.byte(0xC100), 0x22);
    assert_eq!(bus.byte(0xC101), 0x11);
    bus.store(0xC200, 0x33);
    bus.store(0xC201, 0x44);
    run(&mut cpu, &mut bus, &[0xED, 0x4B, 0x00, 0xC2]); // LD BC,(0xC200)
    assert_eq!(bus.cycles(), 20);
    assert_eq!(cpu.regs.b, 0x44);
    assert_eq!(cpu.regs.c, 0x33);
}

#[test]
fn the_sbc_hl_form() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.h = 0x01;
    cpu.regs.l = 0x00;
    cpu.regs.b = 0x00;
    cpu.regs.c = 0x10;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xED, 0x42]); // SBC HL,BC
    assert_eq!(bus.cycles(), 15);
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0x00F0);
    assert_eq!(cpu.regs.f & FLAG_N, FLAG_N);
    assert_eq!(cpu.regs.f & FLAG_S, 0, "0x00F0 is positive");
    // ADC form.
    cpu.regs.h = 0x00;
    cpu.regs.l = 0x01;
    cpu.regs.f = 0;
    cpu.regs.b = 0x00;
    cpu.regs.c = 0x02;
    run(&mut cpu, &mut bus, &[0xED, 0x4A]); // ADC HL,BC
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0x0003);
}

#[test]
fn the_neg_and_register_io_loads() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x0F;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xED, 0x44]); // NEG
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.regs.a, 0xF1);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    cpu.regs.a = 0x80;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xED, 0x44]);
    assert_eq!(cpu.regs.a, 0x80);
    assert_eq!(cpu.regs.f & FLAG_P, FLAG_P, "the overflow of negating 0x80");
    cpu.regs.a = 0x00;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xED, 0x44]);
    assert_eq!(cpu.regs.f & FLAG_Z, FLAG_Z);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
    // LD I,A and the reads back.
    cpu.regs.a = 0x34;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xED, 0x47]); // LD I,A
    assert_eq!(bus.cycles(), 9);
    assert_eq!(cpu.regs.i, 0x34);
    assert_eq!(cpu.regs.f, 0, "LD I,A leaves no flags");
    // The LD A,R form: R's bit 7 copies I's bit 7 and the fetches
    // bump the count twice.
    cpu.regs.i = 0x00;
    cpu.regs.r = 0x83;
    cpu.regs.a = 0;
    run(&mut cpu, &mut bus, &[0xED, 0x5F]); // LD A,R
    assert_eq!(bus.cycles(), 9);
    assert_eq!(cpu.regs.a, 0x05, "R7 copies the clear I bit 7");
    assert_eq!(cpu.regs.f & FLAG_P, 0, "IFF2 is clear");
    cpu.regs.i = 0x80;
    run(&mut cpu, &mut bus, &[0xED, 0x5F]);
    assert_eq!(cpu.regs.a, 0x87, "R7 copies the set I bit 7");
    assert_eq!(cpu.regs.f & FLAG_S, FLAG_S);
    cpu.iff2 = true;
    run(&mut cpu, &mut bus, &[0xED, 0x5F]);
    assert_eq!(cpu.regs.f & FLAG_P, FLAG_P, "the parity reads IFF2");
}

#[test]
fn the_interrupt_modes_and_returns() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0xED, 0x56]); // IM 1
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.im, 1);
    run(&mut cpu, &mut bus, &[0xED, 0x5E]); // IM 2
    assert_eq!(cpu.im, 2);
    run(&mut cpu, &mut bus, &[0xED, 0x46]); // IM 0
    assert_eq!(cpu.im, 0);
    run(&mut cpu, &mut bus, &[0xED, 0x66]); // IM 0 from the row repeat
    assert_eq!(cpu.im, 0);
    run(&mut cpu, &mut bus, &[0xED, 0x76]); // IM 2 from the row repeat
    assert_eq!(cpu.im, 2);
    // RETN restores IFF1 from IFF2.
    cpu.iff1 = false;
    cpu.iff2 = true;
    cpu.regs.sp = 0xC0FE;
    bus.store(0xC0FE, 0x34);
    bus.store(0xC0FF, 0x12);
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0xED, 0x45]); // RETN
    assert_eq!(bus.cycles(), 14);
    assert!(cpu.iff1);
    assert_eq!(cpu.regs.pc, 0x1234);
}

#[test]
fn the_block_load_forms() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    // LDI: one byte from 0xC001 to 0xC101.
    bus.store(0xC001, 0x77);
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x01;
    cpu.regs.d = 0xC1;
    cpu.regs.e = 0x01;
    cpu.regs.b = 0x00;
    cpu.regs.c = 0x02;
    cpu.regs.f = FLAG_C | FLAG_H | FLAG_N;
    run(&mut cpu, &mut bus, &[0xED, 0xA0]); // LDI
    assert_eq!(bus.cycles(), 16);
    assert_eq!(bus.byte(0xC101), 0x77);
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0xC002);
    assert_eq!(u16::from(cpu.regs.d) << 8 | u16::from(cpu.regs.e), 0xC102);
    assert_eq!(cpu.regs.b, 0x00);
    assert_eq!(cpu.regs.c, 0x01);
    assert_eq!(cpu.regs.f & FLAG_P, FLAG_P, "PV follows the count");
    assert_eq!(cpu.regs.f & FLAG_N, 0);
    assert_eq!(cpu.regs.f & FLAG_H, 0);
    // LDIR: two iterations cost 21 + 16.
    bus.store(0xC002, 0x88);
    bus.store(0xC003, 0x99);
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x02;
    cpu.regs.d = 0xC1;
    cpu.regs.e = 0x02;
    cpu.regs.b = 0;
    cpu.regs.c = 2;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xED, 0xB0]); // LDIR
    assert_eq!(bus.cycles(), 21 + 16);
    assert_eq!(bus.byte(0xC102), 0x88);
    assert_eq!(bus.byte(0xC103), 0x99);
    assert!(cpu.regs.f & FLAG_P == 0, "the count emptied");
    // The refetch logged two more opcode fetches per iteration.
    let refetches = bus.read_count(PROGRAM);
    assert!(refetches >= 2, "the LDIR refetch ran");
    // LDD steps down.
    bus.store(0xC005, 0x66);
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x05;
    cpu.regs.d = 0xC1;
    cpu.regs.e = 0x05;
    cpu.regs.b = 0;
    cpu.regs.c = 1;
    run(&mut cpu, &mut bus, &[0xED, 0xA8]); // LDD
    assert_eq!(bus.cycles(), 16);
    assert_eq!(
        bus.byte(0xC105),
        0x66,
        "the store lands at DE before the step"
    );
    assert_eq!(u16::from(cpu.regs.d) << 8 | u16::from(cpu.regs.e), 0xC104);
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0xC004);
}

#[test]
fn the_block_search_and_io_forms() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    // CPI: match the byte.
    bus.store(0xC010, 0x77);
    cpu.regs.a = 0x77;
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x10;
    cpu.regs.b = 0;
    cpu.regs.c = 1;
    cpu.regs.f = FLAG_C;
    run(&mut cpu, &mut bus, &[0xED, 0xA1]); // CPI
    assert_eq!(bus.cycles(), 16);
    assert_eq!(cpu.regs.f & FLAG_Z, FLAG_Z);
    assert_eq!(cpu.regs.f & FLAG_N, FLAG_N);
    assert_eq!(cpu.regs.f & FLAG_P, 0, "the count emptied on this CPI");
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C, "the carry survives a CPI");
    // INI: the port byte into (HL); a one-count reaches zero.
    cpu.regs.b = 0x00;
    cpu.regs.c = 0x01;
    bus.set_port(0x0001, 0x44);
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x20;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xED, 0xA2]); // INI
    assert_eq!(bus.cycles(), 16);
    assert_eq!(bus.byte(0xC020), 0x44);
    assert!(cpu.regs.f & FLAG_Z != 0, "BC reached zero");
    assert_eq!(u16::from(cpu.regs.h) << 8 | u16::from(cpu.regs.l), 0xC021);
    // OUTI.
    cpu.regs.b = 0x10;
    cpu.regs.c = 0x20;
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x30;
    bus.store(0xC030, 0x12);
    bus.io_writes.clear();
    run(&mut cpu, &mut bus, &[0xED, 0xA3]); // OUTI
    assert_eq!(bus.io_writes.first().copied(), Some((0x1020, 0x12)));
}

#[test]
fn the_nibble_rotates() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0x34;
    bus.store(0xC000, 0xA5);
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x00;
    run(&mut cpu, &mut bus, &[0xED, 0x67]); // RRD
    assert_eq!(bus.cycles(), 18);
    assert_eq!(cpu.regs.a, 0x35, "A keeps its high nibble, takes the low");
    assert_eq!(
        bus.byte(0xC000),
        0x4A,
        "the high nibble moves to the low, A's low to the high"
    );
    // RLD from a fresh byte.
    cpu.regs.a = 0x34;
    bus.store(0xC000, 0xA5);
    run(&mut cpu, &mut bus, &[0xED, 0x6F]); // RLD
    assert_eq!(bus.cycles(), 18);
    assert_eq!(cpu.regs.a, 0x3A, "A takes the high nibble");
    assert_eq!(
        bus.byte(0xC000),
        0x54,
        "the low nibble moves to the high, A's low to the low"
    );
}

#[test]
fn the_cb_register_forms() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 0b1000_0010;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x00]); // RLC B
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.regs.b, 0b0000_0101);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    cpu.regs.h = 0x01;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x24]); // SLA H
    assert_eq!(cpu.regs.h, 0x02);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
    cpu.regs.e = 0x83;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x2B]); // SRA E
    assert_eq!(cpu.regs.e, 0xC1);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    cpu.regs.c = 0x40;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x39]); // SRL C
    assert_eq!(cpu.regs.c, 0x20);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
    // The SLL shift runs only with the quirk.
    cpu.regs.b = 0x80;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x30]); // SLL B
    assert_eq!(cpu.regs.b, 0x01);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    // A core without the quirk leaves the byte alone.
    let mut cpu = cpu_of(false);
    cpu.regs.b = 0x80;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x30]);
    assert_eq!(cpu.regs.b, 0x80, "the quirk-off SLL form writes nothing");
    // The RES and SET forms.
    cpu.regs.b = 0x00;
    run(&mut cpu, &mut bus, &[0xCB, 0xC0]); // SET 0,B
    assert_eq!(cpu.regs.b, 0x01);
    cpu.regs.a = 0x02;
    run(&mut cpu, &mut bus, &[0xCB, 0x8F]); // RES 1,A
    assert_eq!(cpu.regs.a & 0x02, 0);
    cpu.regs.c = 0x0F;
    run(&mut cpu, &mut bus, &[0xCB, 0x99]); // RES 3,C
    assert_eq!(cpu.regs.c, 0x07);
    cpu.regs.c = 0x0F;
    run(&mut cpu, &mut bus, &[0xCB, 0xD9]); // SET 3,C
    assert_eq!(cpu.regs.c, 0x0F);
}

#[test]
fn the_bit_tests_carry_the_copy_flags() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.b = 0b0000_1000;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x40]); // BIT 0,B
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.regs.f & FLAG_Z, FLAG_Z, "bit 0 is clear");
    assert_eq!(cpu.regs.f & FLAG_H, FLAG_H, "the bit test sets H");
    assert_eq!(cpu.regs.f & FLAG_C, 0);
    assert_eq!(cpu.regs.f & FLAG_X3, FLAG_X3, "the operand bit 3 copies");
    assert_eq!(cpu.regs.f & FLAG_X5, 0);
    cpu.regs.f = 0;
    cpu.regs.b = 0b0010_0000;
    run(&mut cpu, &mut bus, &[0xCB, 0x40]);
    assert_eq!(cpu.regs.f & FLAG_Z, FLAG_Z);
    assert_eq!(cpu.regs.f & FLAG_X5, FLAG_X5);
    cpu.regs.f = 0;
    cpu.regs.b = 0b0000_0010;
    run(&mut cpu, &mut bus, &[0xCB, 0x40]);
    assert_eq!(cpu.regs.f & (FLAG_Z | FLAG_P), FLAG_Z | FLAG_P);
    assert_eq!(cpu.regs.f & FLAG_C, 0);
    cpu.regs.f = FLAG_C;
    run(&mut cpu, &mut bus, &[0xCB, 0x40]);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C, "the carry survives");
    // BIT 7,(HL): the sign and the address copies.
    cpu.regs.h = 0xA0;
    cpu.regs.l = 0x00;
    bus.store(0xA000, 0x80);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x7E]); // BIT 7,(HL)
    assert_eq!(bus.cycles(), 12);
    assert_eq!(
        cpu.regs.f & FLAG_S,
        FLAG_S,
        "the bit-7 test reports the sign"
    );
    assert_eq!(cpu.regs.f & FLAG_X5, FLAG_X5, "the address bit 5 copies");
    assert_eq!(cpu.regs.f & FLAG_Z, 0);
}

#[test]
fn the_cb_memory_shift_costs_fifteen() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.h = 0xC0;
    cpu.regs.l = 0x10;
    bus.store(0xC010, 0x01);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0x06]); // RLC (HL)
    assert_eq!(bus.cycles(), 15);
    assert_eq!(bus.byte(0xC010), 0x02);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xCB, 0xC6]); // SET 0,(HL): a memory RMW form
    assert_eq!(bus.cycles(), 15);
    assert_eq!(bus.byte(0xC010), 0x03);
}

#[test]
fn the_index_loads_reach_the_displacement() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.ix = 0x1000;
    run(&mut cpu, &mut bus, &[0xDD, 0x7E, 0x08]); // LD A,(IX+8)
    assert_eq!(bus.cycles(), 19, "the published 5 T of the address add");
    assert!(bus.reads.contains(&0x1008));
    assert_eq!(cpu.regs.a, 0x00);
    bus.store(0x1008, 0x44);
    cpu.regs.a = 0;
    cpu.regs.ix = 0x1000;
    run(&mut cpu, &mut bus, &[0xDD, 0x7E, 0x08]);
    assert_eq!(cpu.regs.a, 0x44);
    // LD (IX+d),A
    cpu.regs.a = 0x66;
    run(&mut cpu, &mut bus, &[0xDD, 0x77, 0x10]); // LD (IX+0x10),A
    assert_eq!(bus.cycles(), 19);
    assert_eq!(bus.byte(0x1010), 0x66);
    // LD IX,nn and the pair pushes.
    cpu.regs.ix = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0x21, 0x34, 0x12]); // LD IX,0x1234
    assert_eq!(bus.cycles(), 14);
    assert_eq!(cpu.regs.ix, 0x1234);
    bus.store(0xC100, 0x78);
    bus.store(0xC101, 0x56);
    run(&mut cpu, &mut bus, &[0xDD, 0x2A, 0x00, 0xC1]); // LD IX,(0xC100)
    assert_eq!(bus.cycles(), 20);
    assert_eq!(cpu.regs.ix, 0x5678);
    cpu.regs.sp = 0xC0FF;
    run(&mut cpu, &mut bus, &[0xDD, 0xE5]); // PUSH IX
    assert_eq!(bus.cycles(), 15);
    assert_eq!(bus.byte(0xC0FE), 0x56);
    assert_eq!(bus.byte(0xC0FD), 0x78);
    cpu.regs.ix = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0xE1]); // POP IX
    assert_eq!(bus.cycles(), 14);
    assert_eq!(cpu.regs.ix, 0x5678);
    assert_eq!(cpu.regs.sp, 0xC0FF);
    // The 16-bit pair operations remap.
    cpu.regs.ix = 0x0FF0;
    run(&mut cpu, &mut bus, &[0xDD, 0x29]); // ADD IX,IX
    assert_eq!(bus.cycles(), 15);
    assert_eq!(cpu.regs.ix, 0x1FE0);
    // LD SP,IX
    run(&mut cpu, &mut bus, &[0xDD, 0xF9]);
    assert_eq!(bus.cycles(), 10);
    assert_eq!(cpu.regs.sp, 0x1FE0);
    // The negative displacement.
    cpu.regs.ix = 0x1000;
    cpu.regs.a = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0x7E, 0xF8]); // LD A,(IX-8)
    assert!(bus.reads.contains(&0x0FF8));
}

#[test]
fn the_indexed_immediate_store_costs_nineteen() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.ix = 0x1000;
    run(&mut cpu, &mut bus, &[0xDD, 0x36, 0x05, 0x77]); // LD (IX+5),0x77
    assert_eq!(bus.cycles(), 19);
    assert_eq!(bus.byte(0x1005), 0x77);
    // INC (IX+d) costs 23.
    bus.store(0x1005, 0x0F);
    cpu.regs.ix = 0x1000;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0x34, 0x05]); // INC (IX+5)
    assert_eq!(bus.cycles(), 23);
    assert_eq!(bus.byte(0x1005), 0x10);
    // The ALU with (IX+d) costs 19.
    cpu.regs.a = 0x10;
    cpu.regs.f = 0;
    cpu.regs.ix = 0x1000;
    run(&mut cpu, &mut bus, &[0xDD, 0x86, 0x05]); // ADD A,(IX+5)
    assert_eq!(bus.cycles(), 19);
    assert_eq!(cpu.regs.a, 0x20);
    // EX (SP),IX costs 23.
    cpu.regs.sp = 0xC0F0;
    bus.store(0xC0F0, 0x55);
    bus.store(0xC0F1, 0x66);
    cpu.regs.ix = 0xAABB;
    run(&mut cpu, &mut bus, &[0xDD, 0xE3]); // EX (SP),IX
    assert_eq!(bus.cycles(), 23);
    assert_eq!(cpu.regs.ix, 0x6655);
    assert_eq!(bus.byte(0xC0F0), 0xBB);
    assert_eq!(bus.byte(0xC0F1), 0xAA);
}

#[test]
fn the_prefix_chain_last_wins() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.ix = 0x2000;
    cpu.regs.iy = 0x3000;
    bus.store(0x3008, 0xEE);
    // DD FD 7E 08: the IY win, the read from IY+8.
    run(&mut cpu, &mut bus, &[0xDD, 0xFD, 0x7E, 0x08]);
    assert_eq!(cpu.regs.a, 0xEE);
    // The plain opcodes under a prefix keep their behavior.
    cpu.regs.pc = PROGRAM;
    cpu.regs.a = 0x42;
    run(&mut cpu, &mut bus, &[0xDD, 0x87]); // ADD A,A (plain under DD)
    assert_eq!(bus.cycles(), 8, "the prefix plus the plain opcode");
    assert_eq!(cpu.regs.a, 0x84);
}

#[test]
fn the_index_halves_follow_the_quirk() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.ix = 0x1234;
    run(&mut cpu, &mut bus, &[0xDD, 0x7C]); // LD A,IXH
    assert_eq!(bus.cycles(), 8);
    assert_eq!(cpu.regs.a, 0x12);
    run(&mut cpu, &mut bus, &[0xDD, 0x7D]); // LD A,IXL
    assert_eq!(cpu.regs.a, 0x34);
    run(&mut cpu, &mut bus, &[0xDD, 0x64]); // LD IXH,IXH
    run(&mut cpu, &mut bus, &[0xDD, 0x24]); // INC IXH
    assert_eq!(cpu.regs.ix >> 8, 0x13);
    // A core without the quirk reads zeros and discards.
    let mut cpu = cpu_of(false);
    cpu.regs.ix = 0x1234;
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0xDD, 0x7C]); // LD A,IXH
    assert_eq!(cpu.regs.a, 0x00, "the undocumented half reads zero");
    cpu.regs.a = 0x77;
    run(&mut cpu, &mut bus, &[0xDD, 0x64, 0x77]); // LD IXH,0x77
    assert_eq!(cpu.regs.ix >> 8, 0x12, "the store discards");
}

#[test]
fn the_indexed_cb_forms() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    cpu.regs.ix = 0x1000;
    bus.store(0x1004, 0x01);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0xCB, 0x04, 0x06]); // RLC (IX+4)
    assert_eq!(bus.cycles(), 23);
    assert_eq!(bus.byte(0x1004), 0x02);
    // A bit test costs 20 with the address copies; the fixture's
    // effective address carries bit 5 in its page byte.
    cpu.regs.ix = 0x2040;
    bus.store(0x2046, 0x40);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0xCB, 0x06, 0x46]); // BIT 0,(IX+6)
    assert_eq!(bus.cycles(), 20);
    assert_eq!(cpu.regs.f & FLAG_Z, FLAG_Z);
    assert_eq!(cpu.regs.f & FLAG_X5, FLAG_X5, "the address bit 5 copies");
    // The SLL form runs with the quirk and skips without.
    cpu.regs.ix = 0x1000;
    bus.store(0x1008, 0x80);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0xCB, 0x08, 0x30]); // SLL (IX+8)
    assert_eq!(bus.cycles(), 23);
    assert_eq!(bus.byte(0x1008), 0x01);
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    let mut cpu = cpu_of(false);
    cpu.regs.ix = 0x1000;
    bus.store(0x1008, 0x80);
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0xCB, 0x08, 0x30]);
    assert_eq!(
        bus.byte(0x1008),
        0x80,
        "the quirk-off SLL form writes nothing"
    );
}

#[test]
fn the_unknown_ed_forms_run_as_eight_t_state_nops() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0xED, 0x85]);
    assert_eq!(bus.cycles(), 8);
    run(&mut cpu, &mut bus, &[0xED, 0xC0]);
    assert_eq!(bus.cycles(), 8);
    run(&mut cpu, &mut bus, &[0xED, 0xBF]);
    assert_eq!(bus.cycles(), 8);
}

#[test]
fn halt_parks_until_nmi() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0x76]); // HALT
    assert!(cpu.halted);
    // The parked visit costs nothing and leaves the queue.
    bus.k = 0;
    step_with(&mut cpu, Tick(20), &mut bus);
    assert_eq!(bus.cycles(), 0, "the parked core costs nothing");
    assert_eq!(cpu.next_event(), None, "a parked core leaves the queue");
    // The NMI edge wakes and services in 11 T-states.
    cpu.notify_line("nmi", demu_core::Level::High, Tick(40));
    assert!(cpu.nmi_edge);
    bus.k = 0;
    step_with(&mut cpu, Tick(40), &mut bus);
    assert_eq!(bus.cycles(), 11);
    assert_eq!(cpu.regs.pc, 0x0066);
    assert!(!cpu.halted);
}

#[test]
fn halt_wakes_on_an_accepted_int() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    // HALT with IFF1 clear: the level arrives and does nothing.
    run(&mut cpu, &mut bus, &[0x76]);
    assert!(cpu.halted);
    cpu.notify_line("int", demu_core::Level::High, Tick(40));
    bus.k = 0;
    step_with(&mut cpu, Tick(40), &mut bus);
    assert!(cpu.halted, "a disabled interrupt does not wake the core");
    assert_eq!(bus.cycles(), 0);
    // With IFF1 and mode 1 the level accepts in 13 T-states.
    cpu.iff1 = true;
    cpu.im = 1;
    bus.k = 0;
    step_with(&mut cpu, Tick(48), &mut bus);
    assert_eq!(bus.cycles(), 13);
    assert_eq!(cpu.regs.pc, 0x0038);
    assert!(!cpu.halted);
}

#[test]
fn the_ei_delay_shields_one_boundary() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0xFB]); // EI
    assert!(cpu.iff1);
    assert!(cpu.ei_delay);
    // The line waits high; the boundary after EI does not accept.
    cpu.notify_line("int", demu_core::Level::High, Tick(8));
    cpu.regs.pc = PROGRAM;
    cpu.im = 1;
    run(&mut cpu, &mut bus, &[0x00, 0x00]); // NOP
    assert_eq!(cpu.regs.pc, PROGRAM + 1, "no service ran");
    // The following boundary accepts in 13 T-states.
    bus.k = 0;
    step_with(&mut cpu, Tick(16), &mut bus);
    assert_eq!(bus.cycles(), 13);
    assert_eq!(cpu.regs.pc, 0x0038);
}

#[test]
fn the_int_level_takes_once() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0xFB]); // EI
    cpu.notify_line("int", demu_core::Level::High, Tick(8));
    assert!(cpu.ei_delay, "the EI armed the one-boundary delay");
    cpu.regs.pc = PROGRAM;
    bus.store(PROGRAM, 0x00);
    cpu.im = 1;
    bus.k = 0;
    step_with(&mut cpu, Tick(8), &mut bus); // NOP: the delayed boundary accepts nothing
    assert_eq!(
        cpu.regs.pc,
        PROGRAM + 1,
        "the delay shielded the first boundary"
    );
    // The line stays high; the next boundary accepts.
    bus.k = 0;
    step_with(&mut cpu, Tick(16), &mut bus);
    assert_eq!(cpu.regs.pc, 0x0038);
    // The line stays high; the following boundary runs code.
    cpu.regs.pc = 0xC000;
    bus.store(0xC000, 0x00);
    bus.k = 0;
    step_with(&mut cpu, Tick(30), &mut bus);
    assert_eq!(cpu.regs.pc, 0xC001, "a held level does not reservice");
    // The line drop rearms the acceptance.
    cpu.notify_line("int", demu_core::Level::Low, Tick(40));
    cpu.notify_line("int", demu_core::Level::High, Tick(48));
    cpu.regs.pc = 0xC001;
    bus.k = 0;
    step_with(&mut cpu, Tick(48), &mut bus);
    assert_eq!(cpu.regs.pc, 0x0038, "the rearmed level services");
}

#[test]
fn the_nmi_service_and_retn_shape() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.pc = PROGRAM;
    cpu.regs.sp = 0xC0FF;
    cpu.notify_line("nmi", demu_core::Level::High, Tick(0));
    bus.k = 0;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 11);
    assert_eq!(cpu.regs.pc, 0x0066);
    assert_eq!(
        u16::from(bus.byte(0xC0FE)),
        PROGRAM >> 8,
        "the high byte lands at (SP)-1"
    );
    assert_eq!(bus.byte(0xC0FD), PROGRAM as u8);
    assert_eq!(cpu.regs.sp, 0xC0FD);
    assert!(!cpu.iff1);
    // RETN restores the shadow form.
    cpu.iff2 = true;
    bus.store(0xC0FD, 0x34);
    bus.store(0xC0FE, 0x12);
    cpu.regs.sp = 0xC0FD;
    cpu.regs.pc = PROGRAM;
    run(&mut cpu, &mut bus, &[0xED, 0x45]); // RETN
    assert_eq!(bus.cycles(), 14);
    assert!(cpu.iff1);
    assert_eq!(cpu.regs.pc, 0x1234);
}

#[test]
fn the_mode_two_service_pulls_the_vector() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.pc = PROGRAM;
    cpu.regs.sp = 0xC0FF;
    cpu.regs.i = 0x45;
    cpu.iff1 = true;
    cpu.ei_delay = false;
    bus.set_port(0x0000, 0x67);
    run(&mut cpu, &mut bus, &[0xED, 0x5E]); // IM 2
    cpu.notify_line("int", demu_core::Level::High, Tick(0));
    bus.k = 0;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(bus.cycles(), 19);
    assert_eq!(cpu.regs.pc, 0x4567);
    assert_eq!(bus.io_reads, vec![0x0000]);
}

#[test]
fn the_mode_zero_service_runs_the_supplied_byte() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.pc = PROGRAM;
    cpu.regs.sp = 0xC0FF;
    cpu.im = 0;
    cpu.iff1 = true;
    cpu.ei_delay = false;
    // A device-supplied RST 08h runs with the interrupted program
    // counter as the push value.
    bus.set_port(0x0000, 0xCF);
    cpu.notify_line("int", demu_core::Level::High, Tick(0));
    bus.k = 0;
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(cpu.regs.pc, 0x0008);
    assert_eq!(u16::from(bus.byte(0xC0FE)), PROGRAM >> 8);
    assert_eq!(bus.byte(0xC0FD), PROGRAM as u8);
    assert_eq!(
        bus.cycles(),
        7 + 11,
        "the 7 T-states of the accept and the RST"
    );
}

#[test]
fn the_refresh_counter_counts_opcodes() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.r = 0;
    for _ in 0..5 {
        run(&mut cpu, &mut bus, &[0x00]);
    }
    assert_eq!(cpu.regs.r & 0x7F, 5, "R counts the opcode fetches");
    // The prefixes contribute too.
    cpu.regs.ix = 0;
    run(&mut cpu, &mut bus, &[0xDD, 0x21, 0x00, 0x10]); // LD IX,0x1000
    assert_eq!(
        cpu.regs.r & 0x7F,
        7,
        "the DD byte and the opcode both count"
    );
    // Bit 7 copies I.
    cpu.regs.i = 0x80;
    cpu.r_bump();
    assert_eq!(cpu.regs.r & 0x80, 0x80);
}

#[test]
fn the_interrupts_bump_the_refresh_counter_twice() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.pc = PROGRAM;
    cpu.regs.sp = 0xC0FF;
    let r0 = cpu.regs.r;
    cpu.notify_line("nmi", demu_core::Level::High, Tick(0));
    step_with(&mut cpu, Tick(0), &mut bus);
    assert_eq!(cpu.regs.r & 0x7F, (r0 + 2) & 0x7F);
}

#[test]
fn the_ld_b_imm_indexed_form_skips_the_displacement() {
    let mut cpu = cpu_of(true);
    let mut bus = bus_with_vectors();
    run(&mut cpu, &mut bus, &[0xDD, 0x06, 0x22]); // LD B,0x22
    assert_eq!(bus.cycles(), 11, "4 for DD, 7 for the plain form");
    assert_eq!(cpu.regs.b, 0x22);
}

#[test]
fn the_scf_and_ccf_copy_flags() {
    let mut cpu = cpu_of(false);
    let mut bus = bus_with_vectors();
    cpu.regs.a = 0xA0;
    cpu.regs.f = 0;
    run(&mut cpu, &mut bus, &[0x3F]); // CCF
    assert_eq!(cpu.regs.f & FLAG_X5, FLAG_X5, "the CCF copies A bit 5");
    run(&mut cpu, &mut bus, &[0x37]); // SCF
    assert_eq!(cpu.regs.f & FLAG_C, FLAG_C);
    assert_eq!(cpu.regs.f & FLAG_H, 0);
    assert_eq!(cpu.regs.f & FLAG_X3, 0, "0xA0 bit 3 is clear");
    assert_eq!(cpu.regs.f & FLAG_X5, FLAG_X5);
}

#[test]
fn a_machine_boot_reschedules_past_the_reset() {
    let config = config_of(false);
    let mut cpu = Z80::new(&config.devices[0].clone(), &config);
    cpu.boot(None);
    assert_eq!(cpu.next_event(), Some(Tick(0)));
    let saved = cpu.snapshot();
    assert!(saved.starts_with(b"Z80A"));
    assert_eq!(
        saved.len(),
        78,
        "the empty-range, anchorless form stays at its documented size"
    );
    let mut twin = Z80::new(&config.devices[0].clone(), &config);
    twin.restore(&saved).expect("the chunk restores");
    assert!(twin.reset_pending);
    assert_eq!(twin.next_event(), None, "a restored core re-seeds");
}
