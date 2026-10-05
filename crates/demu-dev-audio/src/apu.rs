//! The S-SMP die of the Super Nintendo sound subsystem.
//!
//! One device carries the whole die: the SPC700 core, the 64 KiB sound
//! RAM, the four B-bus communication ports, the three timers, and the
//! bridge to the S-DSP engine. The core runs the full published SPC700
//! instruction set. The device visit executes one whole instruction
//! and reschedules by its published cycle count, the same model the
//! 6502 core uses; each instruction runs its RAM accesses through the
//! internal overlay in the documented order, and the internal cycles
//! pad the visit to the published count.
//!
//! The SPC clock runs one cycle per `cpu` divider step on the
//! `1024000` domain. Each visit advances the per-cycle engine for the
//! whole instruction length: the constant timer precounter and its
//! three timers, then the DSP, then the sample drain. The DSP bridge
//! steps the engine once per cycle and the engine drains one mixed
//! sample per 32 cycles, which is the 32000 Hz rate of the 1.024 MHz
//! clock.
//!
//! The SPC700 program boots from the configured boot state. `Hex` or
//! `Rom` preloads the RAM image, and the device then fetches the reset
//! vector from `$FFFE`/`$FF` of the loaded image. The 64-byte IPL ROM
//! does not exist here, so the boot handshake of the real chip plays
//! no part; the configuration preloads a program image instead.
//!
//! The memory model follows the chip: write-only registers read back
//! zero, and writes to the `$F0-$FF` register region copy into the RAM
//! underneath, so reads see the register layer and the overlay RAM
//! keeps a copy. The three timer outputs zero on read, and the
//! `$F2` index above `$7F` reads the mirrored engine register while
//! its write is ignored. The `(X)` byte mode applies the direct page
//! base like a `dp` operand, and the 1-bit operand mode carries a
//! 13-bit address over `$0000-$1FFF`, so the bit operations reach the
//! register region and both zero pages.
//!
//! ## Approximations
//!
//! - The IPL boot handshake of the real chip is not modeled. The boot
//!   state preloads the RAM image and the device fetches its own reset
//!   vector from that image. `$F1` bit 7 stores but maps nothing.
//! - The timer scalers run off one constant precounter: timers 0 and 1
//!   get a base tick every 128 cycles (8000 Hz) and timer 2 every 16
//!   (64000 Hz). A scaler target of zero counts 256 base ticks. The
//!   real divider chain sits two cycles away from the S-DSP sample
//!   write; that inter-chip phase is not modeled.
//! - `SLEEP` and `STOP` halt the core until reset. The real chip has
//!   no interrupt source, so both opcodes hang the CPU forever; the
//!   manual table's wake rules reduce to reset. In this model the
//!   timers and the DSP keep stepping during the halt, so the sample
//!   clock stays exact.
//! - A port write while the other side reads at the same instant can
//!   return the logical OR of the old and the new byte. The
//!   `apu.port_race_or` quirk turns that race form on: a port write
//!   opens a one-visit window, and a cross-side port access before
//!   the next visit boundary reads the OR of the previous and the
//!   new byte. Without the quirk the model hands back the plain
//!   latch. The window closes at the visit boundary rather than the
//!   exact sub-cycle strobe of the real chip.
//! - The `$F0` test register stores its byte; only the observed timer
//!   gate runs: the timers work unless the timer-enable bit (bit 0)
//!   is set or the timer-disable bit (bit 3) is clear. The RAM-write
//!   lock, the crash bit, and the wait state bits stay untied.
//! - `DIV YA,X` runs the closed-form divider the verified cores carry:
//!   with `Y < 2X` the quotient path returns the exact quotient and
//!   remainder, and past that the overflow formula folds both. `V`
//!   comes from `Y` against `X`, and `H` from the low nibbles, both
//!   read before the division.
//! - The 16-bit `ADDW`/`SUBW`/`CMPW`/`MOVW` flag rules follow the
//!   verified core form: `Z` from the whole word result, `N` from bit
//!   15, and the half-carry from the high byte's bit-4 carry.
//! - The published cycle counts come from the complete opcode table;
//!   the branch cells hold their not-taken count and a taken branch
//!   adds two. The internal cycles of an instruction pad the visit
//!   after the last access, which is the one placement the internal
//!   memory cannot observe.
//! - The DSP engine lives in the companion module; its own module doc
//!   carries its approximations.

use std::path::Path;

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use crate::dsp::Sdsp;

/// The PSW flag bits, high to low: N V P B H I Z C.
const FLAG_N: u8 = 0x80;
const FLAG_V: u8 = 0x40;
/// The direct page flag: `dp` addressing targets page 1 when set.
const FLAG_P: u8 = 0x20;
const FLAG_B: u8 = 0x10;
const FLAG_H: u8 = 0x08;
const FLAG_I: u8 = 0x04;
const FLAG_Z: u8 = 0x02;
const FLAG_C: u8 = 0x01;

/// The 1-bit operand address mask: the low 13 bits hold the address.
const BIT_ADDR_MASK: u16 = 0x1_FFF;

/// The engine drain interval in SPC cycles.
const SAMPLE_EVERY: u64 = 32;

/// The 64 KiB sound RAM size.
const SOUND_RAM: usize = 0x1_0000;

/// The snapshot header: the tag, the version, and the CPU, port,
/// timer, halt, race-window, and engine phase bytes that precede the
/// RAM image.
const SSMP_FIXED: usize = 54;

/// The published cycle count of each opcode, taken branch excluded.
/// The 2/4, 5/7, and 6/8 pairs start at the not-taken cell; a taken
/// branch adds two.
const CYCLES: [u8; 256] = [
    // 00 NOP   01 TCALL 02 SET1  03 BBS   04 OR    05 OR    06 OR    07 OR
    // 08 OR#   09 ORdp  0A OR1   0B ASLd  0C ASLa  0D PSW   0E TSET1 0F BRK
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 4, 6, 8,
    // 10 BPL   11 TCALL 12 CLR1  13 BBC   14 OR    15 OR    16 OR    17 OR
    // 18 ORd#  19 ORxy  1A DECW  1B ASLdX 1C ASLA  1D DECX  1E CMPXa 1F JMPiX
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 6, 5, 2, 2, 4, 6,
    // 20 CLRP  21 TCALL 22 SET1  23 BBS   24 AND   25 AND   26 AND   27 AND
    // 28 AND#  29 ANDdp 2A OR1N  2B ROLd  2C ROLa  2D A     2E CBNE  2F BRA
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 4, 5, 4,
    // 30 BMI   31 TCALL 32 CLR1  33 BBC   34 AND   35 AND   36 AND   37 AND
    // 38 ANDd# 39 ANDxy 3A INCW  3B ROLdX 3C ROLA  3D INCX  3E CMPXd 3F CALL
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 6, 5, 2, 2, 3, 8,
    // 40 SETP  41 TCALL 42 SET1  43 BBS   44 EOR   45 EOR   46 EOR   47 EOR
    // 48 EOR#  49 EORdp 4A AND1  4B LSRd  4C LSRa  4D X     4E TCLR1 4F PCALL
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 4, 6, 6,
    // 50 BVC   51 TCALL 52 CLR1  53 BBC   54 EOR   55 EOR   56 EOR   57 EOR
    // 58 EORd# 59 EORxy 5A CMPW  5B LSRdX 5C LSRA  5D DECY  5E CMPYa 5F JMP
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 4, 5, 2, 2, 4, 3,
    // 60 CLRC  61 TCALL 62 SET1  63 BBS   64 CMP   65 CMP   66 CMP   67 CMP
    // 68 CMP#  69 CMPdp 6A AND1N 6B RORd  6C RORa  6D Y     6E DBNZd 6F RET
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 4, 5, 5,
    // 70 BVS   71 TCALL 72 CLR1  73 BBC   74 CMP   75 CMP   76 CMP   77 CMP
    // 78 CMPd# 79 CMPxy 7A ADDW  7B RORdX 7C RORA  7D A=X   7E CMPYd 7F RETI
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 3, 6,
    // 80 SETC  81 TCALL 82 SET1  83 BBS   84 ADC   85 ADC   86 ADC   87 ADC
    // 88 ADC#  89 ADCdp 8A EOR1  8B DECd  8C DECa  8D Y#    8E PSW   8F d#
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 2, 4, 5,
    // 90 BCC   91 TCALL 92 CLR1  93 BBC   94 ADC   95 ADC   96 ADC   97 ADC
    // 98 ADCd# 99 ADCxy 9A SUBW  9B DECdX 9C DECA  9D X=SP  9E DIV   9F XCN
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 12, 5,
    // A0 EI    A1 TCALL A2 SET1  A3 BBS   A4 SBC   A5 SBC   A6 SBC   A7 SBC
    // A8 SBC#  A9 SBCdp AA MOV1C AB INCd   AC INCa  AD CMPY# AE A     AF (X)+A
    3, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 2, 4, 4,
    // B0 BCS   B1 TCALL B2 CLR1  B3 BBC   B4 SBC   B5 SBC   B6 SBC   B7 SBC
    // B8 SBCd# B9 SBCxy BA MOVW  BB INCdX BC INCA  BD SP=X  BE DAS   BF A,(X)+
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 3, 4,
    // C0 DI    C1 TCALL C2 SET1  C3 BBC   C4 d=A   C5 a=A   C6 (X)=A C7 [d+X]=A
    // C8 CMPX# C9 a=X   CA MOV1m CB d=Y   CC a=Y   CD X#    CE X     CF MUL
    3, 8, 4, 5, 4, 5, 4, 7, 2, 5, 6, 4, 5, 2, 4, 9,
    // D0 BNE   D1 TCALL D2 SET1  D3 BBC   D4 dX=A  D5 aX=A  D6 aY=A  D7 [d]+Y=A
    // D8 d=X   D9 dY=X  DA d=YA  DB dX=Y  DC DECY  DD A=Y   DE CBNE  DF DAA
    2, 8, 4, 5, 5, 6, 6, 7, 4, 5, 5, 5, 2, 2, 6, 3,
    // E0 CLRV  E1 TCALL E2 SET1  E3 BBC   E4 A=d   E5 A=a   E6 A=(X) E7 A=[d+X]
    // E8 A#    E9 X=a   EA NOT1  EB Y=d   EC Y=a   ED NOTC  EE Y     EF SLEEP
    2, 8, 4, 5, 3, 4, 3, 6, 2, 4, 5, 3, 4, 3, 4, 3,
    // F0 BEQ   F1 TCALL F2 CLR1  F3 BBC   F4 A=dX  F5 A=aX  F6 A=aY  F7 A=[d]+Y
    // F8 X=d   F9 X=dY  FA d=ds  FB Y=dX  FC INCY  FD Y=A   FE DBNZY FF STOP
    2, 8, 4, 5, 4, 5, 5, 6, 3, 4, 5, 4, 2, 2, 4, 3,
];

/// One S-SMP timer: the `$FA-$FC` scaler target, the stage 2
/// comparator, and the 4-bit stage 3 output counter.
#[derive(Debug, Clone, Copy)]
struct Timer {
    /// The scaler target; a zero target counts 256 base ticks.
    target: u8,
    /// The stage 2 counter: post-increment compare against the target.
    stage: u8,
    /// The stage 3 4-bit output counter, zeroed on read.
    out: u8,
    /// The `$F1` enable bit of the timer.
    enabled: bool,
}

impl Timer {
    fn new() -> Self {
        Self {
            target: 0,
            stage: 0,
            out: 0,
            enabled: false,
        }
    }

    /// One base tick of an enabled timer: stage 2 post-increments and
    /// compares, and an equal value passes a tick to stage 3, whose
    /// 4-bit wrap loses one overflow value.
    fn base_tick(&mut self) {
        self.stage = self.stage.wrapping_add(1);
        if self.stage == self.target {
            self.out = self.out.wrapping_add(1) & 0x0F;
            self.stage = 0;
        }
    }
}

/// The core halt kind. The S-SMP has no interrupt source, and only a
/// reset clears the halt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    Core,
    Sleep,
    Stop,
}

impl Halt {
    /// The snapshot byte form.
    fn halt_byte(self) -> u8 {
        match self {
            Halt::Core => 0,
            Halt::Sleep => 1,
            Halt::Stop => 2,
        }
    }

    /// The halt from the snapshot byte form.
    fn from_byte(b: u8) -> Result<Self, SnapshotError> {
        match b {
            0 => Ok(Halt::Core),
            1 => Ok(Halt::Sleep),
            2 => Ok(Halt::Stop),
            _ => Err(SnapshotError::Mismatch("bad ssmp halt byte".into())),
        }
    }
}

/// The accumulator ALU group: one op across the register and memory
/// forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AluOp {
    Or,
    And,
    Eor,
    Cmp,
    Adc,
    Sbc,
}

/// The shift and rotate family.
#[derive(Debug, Clone, Copy)]
enum ShiftOp {
    Asl,
    Rol,
    Lsr,
    Ror,
}

/// The increment and decrement family.
#[derive(Debug, Clone, Copy)]
enum AdjOp {
    Inc,
    Dec,
}

/// The S-SMP: the SPC700 core, the sound RAM, the B-bus ports, the
/// timers, and the S-DSP bridge.
#[derive(Debug)]
pub struct Apu {
    name: String,
    /// Domain ticks per SPC cycle, from the `cpu` divider entry.
    cpu_div: u64,
    a: u8,
    x: u8,
    y: u8,
    /// The stack pointer; the stack always lives in page 1.
    sp: u8,
    psw: u8,
    pc: u16,
    /// The 64 KiB sound RAM.
    ram: Vec<u8>,
    /// The S-DSP engine and its `$F2` register index.
    dsp: Sdsp,
    dsp_index: u8,
    /// The SPC to CPU latches, read by the CPU at `$2140-2143`.
    to_cpu: [u8; 4],
    /// The CPU to SPC latches, read by the SPC at `$F4-$F7`.
    from_cpu: [u8; 4],
    /// The quirk flag of the port race: a port write while the other
    /// side reads within the same port window returns the logical OR
    /// of the previous and the new byte.
    race_or: bool,
    /// The byte each output latch held before the latest SPC write,
    /// for the race OR form.
    to_cpu_stale: [u8; 4],
    /// The byte each input latch held before the latest CPU write,
    /// for the race OR form.
    from_cpu_stale: [u8; 4],
    /// The ports the CPU wrote since the last SPC-side visit: bit n
    /// = port n. The visit boundary closes the race window.
    cpu_wrote: u8,
    /// The ports the SPC wrote since the last CPU-side read of them:
    /// bit n = port n. The next SPC visit boundary closes the race
    /// window.
    spc_wrote: u8,
    /// The `$F1` control byte as last written.
    control: u8,
    /// The `$F0` test byte as last written.
    test: u8,
    /// The three timers, scaler targets `$FA-$FC`.
    timers: [Timer; 3],
    /// The constant stage 1 precounter, 0-127.
    precount: u8,
    /// The total SPC cycles; the sample drain rides its phase, and it
    /// stays in step with the engine's own sample counter.
    cycles: u64,
    halt: Halt,
    /// The drained samples, one per 32 SPC cycles.
    samples: Vec<i16>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Apu {
    /// A device from its configuration entry.
    pub fn new(
        device: &demu_config::model::DeviceInstance,
        config: &demu_config::model::SystemConfig,
    ) -> Self {
        let mut apu = Self {
            name: device.name.to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1),
            a: 0,
            x: 0,
            y: 0,
            sp: 0,
            psw: 0,
            pc: 0,
            ram: vec![0; SOUND_RAM],
            dsp: Sdsp::new(),
            dsp_index: 0,
            to_cpu: [0; 4],
            from_cpu: [0; 4],
            race_or: false,
            to_cpu_stale: [0; 4],
            from_cpu_stale: [0; 4],
            cpu_wrote: 0,
            spc_wrote: 0,
            control: 0,
            test: 0,
            timers: [Timer::new(), Timer::new(), Timer::new()],
            precount: 0,
            cycles: 0,
            halt: Halt::Core,
            samples: Vec::new(),
            next: None,
            last_tick: None,
        };
        apu.apply_quirks(config);
        apu.reset();
        apu
    }

    /// Apply this machine's quirk flags by name.
    fn apply_quirks(&mut self, config: &demu_config::model::SystemConfig) {
        for (name, value) in &config.quirks {
            if name == "apu.port_race_or" {
                self.race_or = value.as_flag();
            }
        }
    }

    // -- The register overlay at `$F0-$FF` ---------------------------------

    /// The read of one overlay register. Write-only registers read
    /// back zero; the timer outputs zero on read; the `$F2` index
    /// above `$7F` mirrors the `$00-$7F` engine registers on read.
    fn reg_read(&mut self, offset: u8) -> u8 {
        match offset {
            0x02 => self.dsp_index,
            0x03 => self.dsp.register(self.dsp_index),
            0x04..=0x07 => {
                let p = (offset & 0x03) as usize;
                // The race form: a CPU port write since the last
                // visit ORs the previous byte into the read.
                if self.race_or && self.cpu_wrote & (1 << p) != 0 {
                    let v = self.from_cpu_stale[p] | self.from_cpu[p];
                    self.cpu_wrote &= !(1 << p);
                    return v;
                }
                self.from_cpu[p]
            }
            0x0D..=0x0F => {
                let timer = (offset - 0x0D) as usize;
                let out = self.timers[timer].out;
                self.timers[timer].out = 0;
                out
            }
            // `$F8`/`$F9` are general-purpose RAM with no register side
            // effects; the write-through copy keeps them readable.
            0x08 | 0x09 => self.ram[0xF0 | usize::from(offset)],
            // `$F0`, `$F1`, and the scaler registers are write-only.
            _ => 0,
        }
    }

    /// The write of one overlay register. The RAM copy already landed;
    /// these are the register side effects. A `$F1` write enables and
    /// restarts, or disables, each timer per its bit, and clears the
    /// requested input latches; a timer target write never touches
    /// stage 2. The `$F0` test byte and `$F1` bit 7 just store, and
    /// writes to the counter registers have no effect.
    fn reg_write(&mut self, offset: u8, value: u8) {
        match offset {
            0x00 => self.test = value,
            0x01 => {
                self.control = value;
                for (bit, timer) in self.timers.iter_mut().enumerate() {
                    timer.enabled = value & (1 << bit) != 0;
                    timer.stage = 0;
                    timer.out = 0;
                }
                if value & 0x10 != 0 {
                    self.from_cpu[0] = 0;
                    self.from_cpu[1] = 0;
                }
                if value & 0x20 != 0 {
                    self.from_cpu[2] = 0;
                    self.from_cpu[3] = 0;
                }
            }
            0x02 => self.dsp_index = value,
            0x03 => {
                // An index past `$7F` writes are ignored.
                if self.dsp_index <= 0x7F {
                    self.dsp.set_register(self.dsp_index, value);
                }
            }
            0x04..=0x07 => {
                let p = (offset & 0x03) as usize;
                // The race window opens with the write: a CPU read
                // before the next visit boundary gets the OR form.
                if self.race_or {
                    self.to_cpu_stale[p] = self.to_cpu[p];
                    self.spc_wrote |= 1 << p;
                }
                self.to_cpu[p] = value;
            }
            0x0A..=0x0C => self.timers[(offset - 0x0A) as usize].target = value,
            // The `$F8`/`$F9` copies stand in the RAM copy already, and
            // the output counters reject writes.
            _ => {}
        }
    }

    /// The reset vector fetched from the current RAM image.
    fn reset_vector(&self) -> u16 {
        u16::from(self.ram[0xFFFE]) | u16::from(self.ram[0xFFFF]) << 8
    }

    // -- The addressing helpers --------------------------------------------

    /// The direct page base from the `P` flag: `$0000` or `$0100`.
    fn dp_page(&self) -> u16 {
        if self.psw & FLAG_P != 0 {
            0x0100
        } else {
            0x0000
        }
    }

    /// The direct page effective address of an 8-bit offset.
    fn addr_dp(&self, offset: u8) -> u16 {
        self.dp_page() | u16::from(offset)
    }

    /// The `dp+X` effective address; the sum wraps inside the page.
    fn addr_dpx(&self, offset: u8) -> u16 {
        self.addr_dp(offset.wrapping_add(self.x))
    }

    /// The `dp+Y` effective address.
    fn addr_dpy(&self, offset: u8) -> u16 {
        self.addr_dp(offset.wrapping_add(self.y))
    }

    // -- The internal bus ---------------------------------------------------

    /// A read of the sound RAM through the register overlay.
    fn rd8(&mut self, address: u16) -> u8 {
        if address & 0xFF00 == 0 && address & 0x00F0 == 0x00F0 {
            self.reg_read((address & 0x0F) as u8)
        } else {
            self.ram[usize::from(address)]
        }
    }

    /// A write of the sound RAM through the register overlay: the
    /// write copies to the RAM underneath and to the register layer.
    fn wr8(&mut self, address: u16, value: u8) {
        self.ram[usize::from(address)] = value;
        if address & 0xFF00 == 0 && address & 0x00F0 == 0x00F0 {
            self.reg_write((address & 0x0F) as u8, value);
        }
    }

    /// The 16-bit pointer word at an absolute address; the two bytes
    /// read in order, and the high byte fetch wraps at `$FFFF`.
    fn word_at(&mut self, address: u16) -> u16 {
        let low = self.rd8(address);
        let high = self.rd8(address.wrapping_add(1));
        u16::from(low) | u16::from(high) << 8
    }

    /// The 16-bit pointer word at a direct page offset; the pointer
    /// bytes wrap as two successive in-page bytes.
    fn dp_word(&mut self, offset: u8) -> u16 {
        let low = self.rd8(self.addr_dp(offset));
        let high = self.rd8(self.addr_dp(offset.wrapping_add(1)));
        u16::from(low) | u16::from(high) << 8
    }

    /// The 16-bit pointer word at `offset+X`; the pointer bytes wrap
    /// inside the direct page.
    fn dp_word_x(&mut self, offset: u8) -> u16 {
        let low = self.rd8(self.addr_dpx(offset));
        let high = self.rd8(self.addr_dpx(offset.wrapping_add(1)));
        u16::from(low) | u16::from(high) << 8
    }

    // -- The stack -----------------------------------------------------------

    /// A push: the byte lands at the current stack pointer, then the
    /// pointer decrements; the stack wraps inside page 1.
    fn push8(&mut self, value: u8) {
        self.ram[0x0100 + usize::from(self.sp)] = value;
        self.sp = self.sp.wrapping_sub(1);
    }

    /// A pop: the pointer increments, then the byte reads.
    fn pop8(&mut self) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        self.ram[0x0100 + usize::from(self.sp)]
    }

    // -- The flags and the ALU ------------------------------------------------

    /// Set `N` and `Z` from a value; the loads carry these flags.
    fn load_nz(&mut self, value: u8) {
        self.psw &= !(FLAG_N | FLAG_Z);
        if value & 0x80 != 0 {
            self.psw |= FLAG_N;
        }
        if value == 0 {
            self.psw |= FLAG_Z;
        }
    }

    /// The 8-bit add with the exact flag chain: `N`, `V`, the half
    /// carry `H`, `Z`, and `C`.
    fn alu_adc(&mut self, x: u8, y: u8) -> u8 {
        let r = u16::from(x) + u16::from(y) + u16::from(self.psw & FLAG_C != 0);
        let r8 = r as u8;
        self.psw &= !(FLAG_N | FLAG_V | FLAG_H | FLAG_Z | FLAG_C);
        if (x ^ y ^ r8) & 0x10 != 0 {
            self.psw |= FLAG_H;
        }
        if (!(x ^ y) & (x ^ r8)) & 0x80 != 0 {
            self.psw |= FLAG_V;
        }
        if r8 & 0x80 != 0 {
            self.psw |= FLAG_N;
        }
        if r8 == 0 {
            self.psw |= FLAG_Z;
        }
        if r > 0xFF {
            self.psw |= FLAG_C;
        }
        r8
    }

    /// The 8-bit subtract: the complement add; `V` and `H` ride the
    /// same chain.
    fn alu_sbc(&mut self, x: u8, y: u8) -> u8 {
        self.alu_adc(x, !y)
    }

    /// The 8-bit compare: `N`, `Z`, and the carry from `x >= y`.
    fn alu_cmp(&mut self, x: u8, y: u8) {
        self.psw &= !(FLAG_N | FLAG_Z | FLAG_C);
        if x.wrapping_sub(y) & 0x80 != 0 {
            self.psw |= FLAG_N;
        }
        if x == y {
            self.psw |= FLAG_Z;
        }
        if x >= y {
            self.psw |= FLAG_C;
        }
    }

    /// The accumulator ALU on one memory operand.
    fn alu_acc(&mut self, op: AluOp, y: u8) {
        match op {
            AluOp::Or => {
                self.a |= y;
                self.load_nz(self.a);
            }
            AluOp::And => {
                self.a &= y;
                self.load_nz(self.a);
            }
            AluOp::Eor => {
                self.a ^= y;
                self.load_nz(self.a);
            }
            AluOp::Cmp => self.alu_cmp(self.a, y),
            AluOp::Adc => self.a = self.alu_adc(self.a, y),
            AluOp::Sbc => self.a = self.alu_sbc(self.a, y),
        }
    }

    /// The ALU op of one accumulator row of the opcode matrix.
    fn op_alu(&self, opcode: u8) -> AluOp {
        match opcode & 0xE0 {
            0x00 => AluOp::Or,
            0x20 => AluOp::And,
            0x40 => AluOp::Eor,
            0x60 => AluOp::Cmp,
            0x80 => AluOp::Adc,
            _ => AluOp::Sbc,
        }
    }

    /// The `OR dd,ds` family: (dst) = (dst) op (src), with the source
    /// read first, the destination read before the write, and no write
    /// on compare.
    fn alu_dp_dp(&mut self, op: AluOp) {
        let offset = self.fetch8();
        let source = self.rd8(self.addr_dp(offset));
        let offset = self.fetch8();
        let dst = self.addr_dp(offset);
        let right = self.rd8(dst);
        self.alu_pair(op, source, right, dst);
    }

    /// The `OR d,#imm` family: the immediate first, then the target.
    fn alu_dp_imm(&mut self, op: AluOp) {
        let value = self.fetch8();
        let offset = self.fetch8();
        let dst = self.addr_dp(offset);
        let right = self.rd8(dst);
        self.alu_pair(op, value, right, dst);
    }

    /// The `OR (X),(Y)` family: (X) = (X) op (Y), the `Y`-addressed
    /// byte read first, and no write on compare.
    fn alu_ix_iy(&mut self, op: AluOp) {
        let source = self.rd8(self.dp_page() | u16::from(self.y));
        let dst = self.dp_page() | u16::from(self.x);
        let right = self.rd8(dst);
        self.alu_pair(op, source, right, dst);
    }

    /// The shared mem-to-mem tail: the flags then the write of the
    /// result back to the destination.
    fn alu_pair(&mut self, op: AluOp, left: u8, right: u8, dst: u16) {
        let result = match op {
            AluOp::Or => left | right,
            AluOp::And => left & right,
            AluOp::Eor => left ^ right,
            AluOp::Cmp => {
                self.alu_cmp(right, left);
                return;
            }
            AluOp::Adc => self.alu_adc(right, left),
            AluOp::Sbc => self.alu_sbc(right, left),
        };
        self.load_nz(result);
        self.wr8(dst, result);
    }

    // -- The shifts, rotates, and the adjust ops ------------------------------

    /// One shift or rotate of a value with its flags.
    fn shift(&mut self, op: ShiftOp, value: u8) -> u8 {
        self.psw &= !(FLAG_N | FLAG_Z | FLAG_C);
        let (out_bit, r) = match op {
            ShiftOp::Asl => (value & 0x80 != 0, value << 1),
            ShiftOp::Rol => {
                let in_bit = self.psw & FLAG_C != 0;
                (value & 0x80 != 0, value << 1 | u8::from(in_bit))
            }
            ShiftOp::Lsr => (value & 0x01 != 0, value >> 1),
            ShiftOp::Ror => {
                let in_bit = self.psw & FLAG_C != 0;
                (value & 0x01 != 0, u8::from(in_bit) << 7 | value >> 1)
            }
        };
        if out_bit {
            self.psw |= FLAG_C;
        }
        self.load_nz(r);
        r
    }

    /// The shift family on a direct page cell.
    fn shift_dp(&mut self, op: ShiftOp) {
        let offset = self.fetch8();
        let addr = self.addr_dp(offset);
        let value = self.rd8(addr);
        let r = self.shift(op, value);
        self.wr8(addr, r);
    }

    /// The shift family on a `dp+X` cell.
    fn shift_dpx(&mut self, op: ShiftOp) {
        let offset = self.fetch8();
        let addr = self.addr_dpx(offset);
        let value = self.rd8(addr);
        let r = self.shift(op, value);
        self.wr8(addr, r);
    }

    /// The shift family on an absolute cell.
    fn shift_abs(&mut self, op: ShiftOp) {
        let addr = self.fetch16();
        let value = self.rd8(addr);
        let r = self.shift(op, value);
        self.wr8(addr, r);
    }

    /// An increment or decrement with the `N` and `Z` flags.
    fn adjust(&mut self, op: AdjOp, value: u8) -> u8 {
        let r = match op {
            AdjOp::Inc => value.wrapping_add(1),
            AdjOp::Dec => value.wrapping_sub(1),
        };
        self.load_nz(r);
        r
    }

    /// The adjust family on a direct page cell.
    fn adjust_dp(&mut self, op: AdjOp) {
        let offset = self.fetch8();
        let addr = self.addr_dp(offset);
        let value = self.rd8(addr);
        let r = self.adjust(op, value);
        self.wr8(addr, r);
    }

    /// The adjust family on a `dp+X` cell.
    fn adjust_dpx(&mut self, op: AdjOp) {
        let offset = self.fetch8();
        let addr = self.addr_dpx(offset);
        let value = self.rd8(addr);
        let r = self.adjust(op, value);
        self.wr8(addr, r);
    }

    /// The adjust family on an absolute cell.
    fn adjust_abs(&mut self, op: AdjOp) {
        let addr = self.fetch16();
        let value = self.rd8(addr);
        let r = self.adjust(op, value);
        self.wr8(addr, r);
    }

    // -- The 16-bit ops --------------------------------------------------------

    /// The paired `YA` registers as a 16-bit word.
    fn word_reg(&self) -> u16 {
        u16::from(self.y) << 8 | u16::from(self.a)
    }

    /// `ADDW YA,dp`: the two-byte add with the carry chained into the
    /// high byte; `H` and `N` ride the high byte, and `Z` reads from
    /// the whole word.
    fn op_addw(&mut self, word: u16) {
        let ya = self.word_reg();
        self.psw &= !FLAG_C;
        self.a = self.alu_adc(ya as u8, word as u8);
        self.y = self.alu_adc((ya >> 8) as u8, (word >> 8) as u8);
        self.word_nz();
    }

    /// `SUBW YA,dp`: the two-byte subtract with the borrow chained;
    /// the same flag rule as the add.
    fn op_sbw(&mut self, word: u16) {
        let ya = self.word_reg();
        self.psw |= FLAG_C;
        self.a = self.alu_sbc(ya as u8, word as u8);
        self.y = self.alu_sbc((ya >> 8) as u8, (word >> 8) as u8);
        self.word_nz();
    }

    /// `CMPW YA,dp`: `N` and `Z` from the whole difference and `C`
    /// from the word form of `YA >= word`.
    fn op_cpw(&mut self, word: u16) {
        let ya = self.word_reg();
        let r = ya.wrapping_sub(word);
        self.psw &= !(FLAG_N | FLAG_Z | FLAG_C);
        if r & 0x8000 != 0 {
            self.psw |= FLAG_N;
        }
        if r == 0 {
            self.psw |= FLAG_Z;
        }
        if ya >= word {
            self.psw |= FLAG_C;
        }
    }

    /// `MOVW YA,dp`: the word load with `N` from bit 15 and `Z` from
    /// the whole word.
    fn op_ldw(&mut self, word: u16) {
        self.a = word as u8;
        self.y = (word >> 8) as u8;
        self.psw &= !(FLAG_N | FLAG_Z);
        if word & 0x8000 != 0 {
            self.psw |= FLAG_N;
        }
        if word == 0 {
            self.psw |= FLAG_Z;
        }
    }

    /// The `N` and `Z` flags of a whole 16-bit result.
    fn word_nz(&mut self) {
        let r = self.word_reg();
        self.psw &= !(FLAG_N | FLAG_Z);
        if r & 0x8000 != 0 {
            self.psw |= FLAG_N;
        }
        if r == 0 {
            self.psw |= FLAG_Z;
        }
    }

    /// `INCW dp` and `DECW dp`: the 16-bit adjust with both bytes
    /// read and written, the write order low then high.
    fn op_word_adjust(&mut self, op: AdjOp) {
        let offset = self.fetch8();
        let word = self.dp_word(offset);
        let r = match op {
            AdjOp::Inc => word.wrapping_add(1),
            AdjOp::Dec => word.wrapping_sub(1),
        };
        self.psw &= !(FLAG_N | FLAG_Z);
        if r & 0x8000 != 0 {
            self.psw |= FLAG_N;
        }
        if r == 0 {
            self.psw |= FLAG_Z;
        }
        self.wr8(self.addr_dp(offset), r as u8);
        self.wr8(self.addr_dp(offset.wrapping_add(1)), (r >> 8) as u8);
    }

    // -- The special arithmetic --------------------------------------------------

    /// `MUL YA`: the 16-bit product; `A` takes the low byte, `Y` the
    /// high byte, and `N`/`Z` read from `Y` only.
    fn op_mul(&mut self) {
        let product = u16::from(self.y) * u16::from(self.a);
        self.a = product as u8;
        self.y = (product >> 8) as u8;
        self.load_nz(self.y);
    }

    /// `DIV YA,X`: the verified closed form. With `Y < 2X` the
    /// quotient path returns the exact pair, and past it the overflow
    /// formula folds both; a zero divisor lands in the overflow path.
    /// `V` holds the quotient's ninth bit and `H` the low nibble
    /// compare, both read before the division, and `N`/`Z` read from
    /// `A` only.
    fn op_div(&mut self) {
        let ya = self.word_reg();
        let x = u16::from(self.x);
        let ys = self.y;
        self.psw &= !(FLAG_V | FLAG_H);
        if u16::from(ys) >= x {
            self.psw |= FLAG_V;
        }
        if ys & 0x0F >= self.x & 0x0F {
            self.psw |= FLAG_H;
        }
        let (quotient, remainder) = if u16::from(ys) < (x << 1) {
            // The quotient fits the 9-bit result: the exact pair.
            (ya / x, ya % x)
        } else {
            // The quotient cannot fit: the hardware folds the value.
            let span = 0x0100 - x;
            let diff = (u32::from(ya) - (u32::from(x) << 9)) as u16;
            (0xFF - diff / span, x + diff % span)
        };
        self.a = quotient as u8;
        self.y = remainder as u8;
        self.load_nz(self.a);
    }

    /// `XCN A`: the nibbles swap; `N` and `Z` read from the result.
    fn op_xcn(&mut self) {
        self.a = self.a.rotate_left(4);
        self.load_nz(self.a);
    }

    /// `DAA A`: the decimal adjust after `ADC`, on `C` and `H`. The
    /// `H` flag never changes.
    fn op_daa(&mut self) {
        if self.psw & FLAG_C != 0 || self.a > 0x99 {
            self.a = self.a.wrapping_add(0x60);
            self.psw |= FLAG_C;
        }
        if self.psw & FLAG_H != 0 || self.a & 0x0F > 0x09 {
            self.a = self.a.wrapping_add(0x06);
        }
        self.load_nz(self.a);
    }

    /// `DAS A`: the decimal adjust after `SBC`.
    fn op_das(&mut self) {
        if self.psw & FLAG_C == 0 || self.a > 0x99 {
            self.a = self.a.wrapping_sub(0x60);
            self.psw &= !FLAG_C;
        }
        if self.psw & FLAG_H == 0 || self.a & 0x0F > 0x09 {
            self.a = self.a.wrapping_sub(0x06);
        }
        self.load_nz(self.a);
    }

    // -- The flow ops ------------------------------------------------------------

    /// Fetch one byte of the program stream.
    fn fetch8(&mut self) -> u8 {
        let address = self.pc;
        self.pc = self.pc.wrapping_add(1);
        self.rd8(address)
    }

    /// Fetch a little-endian 16-bit program operand.
    fn fetch16(&mut self) -> u16 {
        let low = u16::from(self.fetch8());
        low | u16::from(self.fetch8()) << 8
    }

    /// A relative branch with a signed 8-bit offset.
    fn branch(&mut self, condition: bool) -> bool {
        let rel = self.fetch8() as i8;
        if condition {
            self.pc = self.pc.wrapping_add(rel as u16);
            true
        } else {
            false
        }
    }

    /// The direct page branch on a `P` flag bit: `BBS` with
    /// `when_set`, `BBC` without. The bit rides bits 5-7 of the
    /// opcode and the set or clear rides bit 4.
    fn branch_bit(&mut self, opcode: u8, when_set: bool) -> bool {
        let offset = self.fetch8();
        let value = self.rd8(self.addr_dp(offset));
        let taken = (value >> (opcode >> 5) & 0x01 != 0) == when_set;
        let rel = self.fetch8() as i8;
        if taken {
            self.pc = self.pc.wrapping_add(rel as u16);
        }
        taken
    }

    /// `TCALL n`: the hardware vector call to `$FFDE-2n`; the return
    /// address pushes after the vector reads.
    fn op_tcall(&mut self, opcode: u8) {
        let vector = 0xFFDE - (u16::from(opcode >> 4) << 1);
        let target = self.word_at(vector);
        self.push8((self.pc >> 8) as u8);
        self.push8(self.pc as u8);
        self.pc = target;
    }

    /// `RET`: two pops build the return address.
    fn op_ret(&mut self) {
        self.pc = u16::from(self.pop8()) | u16::from(self.pop8()) << 8;
    }

    /// `RETI`: the status byte pops first, then the address.
    fn op_reti(&mut self) {
        self.psw = self.pop8();
        self.op_ret();
    }

    /// `BRK`: the vector at `$FFDE` reads first, then the pushes; the
    /// break flag sets and the interrupt disable clears after, and the
    /// push of the status byte carries the pre-`BRK` byte.
    fn op_brk(&mut self) {
        let target = self.word_at(0xFFDE);
        self.push8((self.pc >> 8) as u8);
        self.push8(self.pc as u8);
        self.push8(self.psw);
        self.pc = target;
        self.psw |= FLAG_B;
        self.psw &= !FLAG_I;
    }

    // -- The 1-bit memory ops --------------------------------------------------------

    /// The 1-bit operations on a 13-bit address. The 16-bit operand
    /// carries the address low 13 bits and the bit index high 3 bits;
    /// the region runs `$0000-$1FFF`, the register region included.
    fn op_bit(&mut self, opcode: u8) {
        let operand = self.fetch16();
        let bit = 1u8 << (operand >> 13);
        let addr = operand & BIT_ADDR_MASK;
        match opcode >> 5 {
            // `OR1`: the carry ors the bit or its complement.
            0 | 1 => {
                let mem_bit = self.rd8(addr) & bit != 0;
                if mem_bit ^ (opcode & 0x20 != 0) {
                    self.psw |= FLAG_C;
                }
            }
            // `AND1`: the carry ands the bit or its complement.
            2 | 3 => {
                let mem_bit = self.rd8(addr) & bit != 0;
                if !(mem_bit ^ (opcode & 0x20 != 0)) {
                    self.psw &= !FLAG_C;
                }
            }
            // `EOR1`: the carry eors the bit.
            4 => {
                if self.rd8(addr) & bit != 0 {
                    self.psw ^= FLAG_C;
                }
            }
            // `MOV1 C,m.b`: the carry takes the bit.
            5 => {
                let mem_bit = self.rd8(addr) & bit;
                self.psw &= !FLAG_C;
                if mem_bit != 0 {
                    self.psw |= FLAG_C;
                }
            }
            // `MOV1 m.b,C`: the read precedes the write.
            6 => {
                let value = self.rd8(addr);
                let keep = if self.psw & FLAG_C != 0 { bit } else { 0 };
                self.wr8(addr, value & !bit | keep);
            }
            // `NOT1 m.b`: the read modify write flips the bit.
            _ => {
                let value = self.rd8(addr) ^ bit;
                self.wr8(addr, value);
            }
        }
    }

    /// `SET1` and `CLR1` on a direct page byte: the read then the
    /// write, the bit index riding bits 5-7 of the opcode and the set
    /// or clear riding bit 4.
    fn op_set_bit(&mut self, opcode: u8) {
        let offset = self.fetch8();
        let addr = self.addr_dp(offset);
        let bit = 1u8 << (opcode >> 5);
        let value = if opcode & 0x10 == 0 {
            self.rd8(addr) | bit
        } else {
            self.rd8(addr) & !bit
        };
        self.wr8(addr, value);
    }

    /// `TSET1 !a` and `TCLR1 !a`: `N` and `Z` set as for the compare
    /// before the change, then the second read and the write.
    fn op_test_bit(&mut self, set: bool) {
        let addr = self.fetch16();
        let old = self.rd8(addr);
        self.psw &= !(FLAG_N | FLAG_Z);
        if self.a.wrapping_sub(old) & 0x80 != 0 {
            self.psw |= FLAG_N;
        }
        if self.a == old {
            self.psw |= FLAG_Z;
        }
        let _ = self.rd8(addr);
        self.wr8(addr, if set { old | self.a } else { old & !self.a });
    }

    // -- The one-instruction executor ---------------------------------------------------

    /// One instruction from the current program counter: the whole
    /// instruction runs, the accesses walk the documented order, and
    /// the published cycle count returns for the schedule.
    fn step(&mut self) -> u32 {
        let opcode = self.fetch8();
        let mut taken = false;
        match opcode {
            // ---- The hardware vector calls, calls, and returns -------------
            0x01 | 0x11 | 0x21 | 0x31 | 0x41 | 0x51 | 0x61 | 0x71 | 0x81 | 0x91 | 0xA1 | 0xB1
            | 0xC1 | 0xD1 | 0xE1 | 0xF1 => self.op_tcall(opcode),
            0x3F => {
                let target = self.fetch16();
                self.push8((self.pc >> 8) as u8);
                self.push8(self.pc as u8);
                self.pc = target;
            }
            0x4F => {
                let page = self.fetch8();
                self.push8((self.pc >> 8) as u8);
                self.push8(self.pc as u8);
                self.pc = 0xFF00 | u16::from(page);
            }
            0x0F => self.op_brk(),
            0x6F => self.op_ret(),
            0x7F => self.op_reti(),
            0x5F => self.pc = self.fetch16(),
            0x1F => {
                let base = self.fetch16().wrapping_add(u16::from(self.x));
                self.pc = self.word_at(base);
            }
            // ---- The relative and tested branches ----------------------------
            0x10 => taken = self.branch(self.psw & FLAG_N == 0),
            0x30 => taken = self.branch(self.psw & FLAG_N != 0),
            0x50 => taken = self.branch(self.psw & FLAG_V == 0),
            0x70 => taken = self.branch(self.psw & FLAG_V != 0),
            0x90 => taken = self.branch(self.psw & FLAG_C == 0),
            0xB0 => taken = self.branch(self.psw & FLAG_C != 0),
            0xD0 => taken = self.branch(self.psw & FLAG_Z == 0),
            0xF0 => taken = self.branch(self.psw & FLAG_Z != 0),
            0x2F => {
                let _ = self.branch(true);
            }
            0x03 | 0x23 | 0x43 | 0x63 | 0x83 | 0xA3 | 0xC3 | 0xE3 => {
                taken = self.branch_bit(opcode, true);
            }
            0x13 | 0x33 | 0x53 | 0x73 | 0x93 | 0xB3 | 0xD3 | 0xF3 => {
                taken = self.branch_bit(opcode, false);
            }
            0x2E => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let value = self.rd8(addr);
                taken = self.branch(self.a != value);
            }
            0xDE => {
                let offset = self.fetch8();
                let addr = self.addr_dpx(offset);
                let value = self.rd8(addr);
                taken = self.branch(self.a != value);
            }
            0x6E => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let old = self.rd8(addr);
                let value = self.adjust(AdjOp::Dec, old);
                self.wr8(addr, value);
                taken = self.branch(value != 0);
            }
            0xFE => {
                self.y = self.y.wrapping_sub(1);
                taken = self.branch(self.y != 0);
            }
            // ---- The 1-bit and the flag families -----------------------------
            0x0A | 0x2A | 0x4A | 0x6A | 0x8A | 0xAA | 0xCA | 0xEA => self.op_bit(opcode),
            0x02 | 0x22 | 0x42 | 0x62 | 0x82 | 0xA2 | 0xC2 | 0xE2 | 0x12 | 0x32 | 0x52 | 0x72
            | 0x92 | 0xB2 | 0xD2 | 0xF2 => self.op_set_bit(opcode),
            0x0E => self.op_test_bit(true),
            0x4E => self.op_test_bit(false),
            0x60 => self.psw &= !FLAG_C,
            0x80 => self.psw |= FLAG_C,
            0xED => self.psw ^= FLAG_C,
            0xE0 => self.psw &= !(FLAG_V | FLAG_H),
            0x20 => self.psw &= !FLAG_P,
            0x40 => self.psw |= FLAG_P,
            0xC0 => self.psw &= !FLAG_I,
            0xA0 => self.psw |= FLAG_I,
            // ---- The accumulator ALU family ----------------------------------
            0x04 | 0x24 | 0x44 | 0x64 | 0x84 | 0xA4 => {
                let op = self.op_alu(opcode);
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let value = self.rd8(addr);
                self.alu_acc(op, value);
            }
            0x14 | 0x34 | 0x54 | 0x74 | 0x94 | 0xB4 => {
                let op = self.op_alu(opcode);
                let offset = self.fetch8();
                let addr = self.addr_dpx(offset);
                let value = self.rd8(addr);
                self.alu_acc(op, value);
            }
            0x05 | 0x25 | 0x45 | 0x65 | 0x85 | 0xA5 => {
                let op = self.op_alu(opcode);
                let addr = self.fetch16();
                let value = self.rd8(addr);
                self.alu_acc(op, value);
            }
            0x15 | 0x35 | 0x55 | 0x75 | 0x95 | 0xB5 => {
                let op = self.op_alu(opcode);
                let base = self.fetch16();
                let addr = base.wrapping_add(u16::from(self.x));
                let value = self.rd8(addr);
                self.alu_acc(op, value);
            }
            0x16 | 0x36 | 0x56 | 0x76 | 0x96 | 0xB6 => {
                let op = self.op_alu(opcode);
                let base = self.fetch16();
                let addr = base.wrapping_add(u16::from(self.y));
                let value = self.rd8(addr);
                self.alu_acc(op, value);
            }
            0x06 | 0x26 | 0x46 | 0x66 | 0x86 | 0xA6 => {
                let op = self.op_alu(opcode);
                let value = self.rd8(self.addr_dp(self.x));
                self.alu_acc(op, value);
            }
            0x07 | 0x27 | 0x47 | 0x67 | 0x87 | 0xA7 => {
                let op = self.op_alu(opcode);
                let offset = self.fetch8();
                let ptr = self.dp_word_x(offset);
                let value = self.rd8(ptr);
                self.alu_acc(op, value);
            }
            0x17 | 0x37 | 0x57 | 0x77 | 0x97 | 0xB7 => {
                let op = self.op_alu(opcode);
                let offset = self.fetch8();
                let ptr = self.dp_word(offset).wrapping_add(u16::from(self.y));
                let value = self.rd8(ptr);
                self.alu_acc(op, value);
            }
            0x08 | 0x28 | 0x48 | 0x68 | 0x88 | 0xA8 => {
                let op = self.op_alu(opcode);
                let value = self.fetch8();
                self.alu_acc(op, value);
            }
            0x09 | 0x29 | 0x49 | 0x69 | 0x89 | 0xA9 => {
                let op = self.op_alu(opcode);
                self.alu_dp_dp(op);
            }
            0x18 | 0x38 | 0x58 | 0x78 | 0x98 | 0xB8 => {
                let op = self.op_alu(opcode);
                self.alu_dp_imm(op);
            }
            0x19 | 0x39 | 0x59 | 0x79 | 0x99 | 0xB9 => {
                let op = self.op_alu(opcode);
                self.alu_ix_iy(op);
            }
            // ---- The shifts, rotates, and increments -------------------------
            0x0B => self.shift_dp(ShiftOp::Asl),
            0x0C => self.shift_abs(ShiftOp::Asl),
            0x1B => self.shift_dpx(ShiftOp::Asl),
            0x1C => self.a = self.shift(ShiftOp::Asl, self.a),
            0x2B => self.shift_dp(ShiftOp::Rol),
            0x2C => self.shift_abs(ShiftOp::Rol),
            0x3B => self.shift_dpx(ShiftOp::Rol),
            0x3C => self.a = self.shift(ShiftOp::Rol, self.a),
            0x4B => self.shift_dp(ShiftOp::Lsr),
            0x4C => self.shift_abs(ShiftOp::Lsr),
            0x5B => self.shift_dpx(ShiftOp::Lsr),
            0x5C => self.a = self.shift(ShiftOp::Lsr, self.a),
            0x6B => self.shift_dp(ShiftOp::Ror),
            0x6C => self.shift_abs(ShiftOp::Ror),
            0x7B => self.shift_dpx(ShiftOp::Ror),
            0x7C => self.a = self.shift(ShiftOp::Ror, self.a),
            0xAB => self.adjust_dp(AdjOp::Inc),
            0xAC => self.adjust_abs(AdjOp::Inc),
            0xBB => self.adjust_dpx(AdjOp::Inc),
            0xBC => self.a = self.adjust(AdjOp::Inc, self.a),
            0x3D => self.x = self.adjust(AdjOp::Inc, self.x),
            0xFC => self.y = self.adjust(AdjOp::Inc, self.y),
            0x8B => self.adjust_dp(AdjOp::Dec),
            0x8C => self.adjust_abs(AdjOp::Dec),
            0x9B => self.adjust_dpx(AdjOp::Dec),
            0x9C => self.a = self.adjust(AdjOp::Dec, self.a),
            0x1D => self.x = self.adjust(AdjOp::Dec, self.x),
            0xDC => self.y = self.adjust(AdjOp::Dec, self.y),
            // ---- The 16-bit ops ----------------------------------------------
            0x3A => self.op_word_adjust(AdjOp::Inc),
            0x1A => self.op_word_adjust(AdjOp::Dec),
            0x7A => {
                let offset = self.fetch8();
                let word = self.dp_word(offset);
                self.op_addw(word);
            }
            0x9A => {
                let offset = self.fetch8();
                let word = self.dp_word(offset);
                self.op_sbw(word);
            }
            0x5A => {
                let offset = self.fetch8();
                let word = self.dp_word(offset);
                self.op_cpw(word);
            }
            0xBA => {
                let offset = self.fetch8();
                let word = self.dp_word(offset);
                self.op_ldw(word);
            }
            0xDA => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let _ = self.rd8(addr);
                self.wr8(self.addr_dp(offset), self.a);
                self.wr8(self.addr_dp(offset.wrapping_add(1)), self.y);
            }
            // ---- The special arithmetic ---------------------------------------
            0xCF => self.op_mul(),
            0x9E => self.op_div(),
            0x9F => self.op_xcn(),
            0xDF => self.op_daa(),
            0xBE => self.op_das(),
            // ---- The compares of X and Y --------------------------------------
            0xC8 => {
                let value = self.fetch8();
                self.alu_cmp(self.x, value);
            }
            0xAD => {
                let value = self.fetch8();
                self.alu_cmp(self.y, value);
            }
            0x3E => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let value = self.rd8(addr);
                self.alu_cmp(self.x, value);
            }
            0x7E => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let value = self.rd8(addr);
                self.alu_cmp(self.y, value);
            }
            0x1E => {
                let addr = self.fetch16();
                let value = self.rd8(addr);
                self.alu_cmp(self.x, value);
            }
            0x5E => {
                let addr = self.fetch16();
                let value = self.rd8(addr);
                self.alu_cmp(self.y, value);
            }
            // ---- The loads ------------------------------------------------------
            0xE4 => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let value = self.rd8(addr);
                self.load_nz(value);
                self.a = value;
            }
            0xF4 => {
                let offset = self.fetch8();
                let addr = self.addr_dpx(offset);
                let value = self.rd8(addr);
                self.load_nz(value);
                self.a = value;
            }
            0xE5 | 0xE9 | 0xEC => {
                let addr = self.fetch16();
                let value = self.rd8(addr);
                self.load_nz(value);
                if opcode == 0xE5 {
                    self.a = value;
                } else if opcode == 0xE9 {
                    self.x = value;
                } else {
                    self.y = value;
                }
            }
            0xF5 => {
                let base = self.fetch16();
                let addr = base.wrapping_add(u16::from(self.x));
                let value = self.rd8(addr);
                self.load_nz(value);
                self.a = value;
            }
            0xF6 => {
                let base = self.fetch16();
                let addr = base.wrapping_add(u16::from(self.y));
                let value = self.rd8(addr);
                self.load_nz(value);
                self.a = value;
            }
            0xE6 => {
                let value = self.rd8(self.addr_dp(self.x));
                self.load_nz(value);
                self.a = value;
            }
            0xBF => {
                let value = self.rd8(self.addr_dp(self.x));
                self.x = self.x.wrapping_add(1);
                self.load_nz(value);
                self.a = value;
            }
            0xE7 => {
                let offset = self.fetch8();
                let ptr = self.dp_word_x(offset);
                let value = self.rd8(ptr);
                self.load_nz(value);
                self.a = value;
            }
            0xF7 => {
                let offset = self.fetch8();
                let ptr = self.dp_word(offset).wrapping_add(u16::from(self.y));
                let value = self.rd8(ptr);
                self.load_nz(value);
                self.a = value;
            }
            0xE8 | 0xCD | 0x8D => {
                let value = self.fetch8();
                self.load_nz(value);
                if opcode == 0xE8 {
                    self.a = value;
                } else if opcode == 0xCD {
                    self.x = value;
                } else {
                    self.y = value;
                }
            }
            0xF8 | 0xF9 => {
                // `X,d` reads plain; `X,d+Y` pays the index cycle.
                let offset = self.fetch8();
                let addr = if opcode == 0xF8 {
                    self.addr_dp(offset)
                } else {
                    self.addr_dpy(offset)
                };
                let value = self.rd8(addr);
                self.load_nz(value);
                self.x = value;
            }
            0xEB => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let value = self.rd8(addr);
                self.load_nz(value);
                self.y = value;
            }
            0xFB => {
                let offset = self.fetch8();
                let addr = self.addr_dpx(offset);
                let value = self.rd8(addr);
                self.load_nz(value);
                self.y = value;
            }
            0x7D => {
                self.load_nz(self.x);
                self.a = self.x;
            }
            0xDD => {
                self.load_nz(self.y);
                self.a = self.y;
            }
            0x5D => {
                self.load_nz(self.a);
                self.x = self.a;
            }
            0xFD => {
                self.load_nz(self.a);
                self.y = self.a;
            }
            0x9D => {
                self.load_nz(self.sp);
                self.x = self.sp;
            }
            0xBD => self.sp = self.x,
            // ---- The stores ------------------------------------------------------
            0xC4 => {
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let _ = self.rd8(addr);
                self.wr8(addr, self.a);
            }
            0xD4 => {
                let offset = self.fetch8();
                let addr = self.addr_dpx(offset);
                let _ = self.rd8(addr);
                self.wr8(addr, self.a);
            }
            0xC5 | 0xC9 | 0xCC => {
                let addr = self.fetch16();
                let _ = self.rd8(addr);
                let value = if opcode == 0xC5 {
                    self.a
                } else if opcode == 0xC9 {
                    self.x
                } else {
                    self.y
                };
                self.wr8(addr, value);
            }
            0xD5 => {
                let addr = self.fetch16().wrapping_add(u16::from(self.x));
                let _ = self.rd8(addr);
                self.wr8(addr, self.a);
            }
            0xD6 => {
                let addr = self.fetch16().wrapping_add(u16::from(self.y));
                let _ = self.rd8(addr);
                self.wr8(addr, self.a);
            }
            0xC6 => {
                let addr = self.addr_dp(self.x);
                let _ = self.rd8(addr);
                self.wr8(addr, self.a);
            }
            0xAF => {
                let addr = self.addr_dp(self.x);
                self.wr8(addr, self.a);
                self.x = self.x.wrapping_add(1);
            }
            0xC7 => {
                let offset = self.fetch8();
                let ptr = self.dp_word_x(offset);
                let _ = self.rd8(ptr);
                self.wr8(ptr, self.a);
            }
            0xD7 => {
                let offset = self.fetch8();
                let ptr = self.dp_word(offset).wrapping_add(u16::from(self.y));
                let _ = self.rd8(ptr);
                self.wr8(ptr, self.a);
            }
            0xD8 | 0xD9 | 0xCB | 0xDB => {
                let offset = self.fetch8();
                let addr = match opcode {
                    0xD8 => self.addr_dp(offset),
                    0xD9 => self.addr_dpy(offset),
                    0xCB => self.addr_dp(offset),
                    _ => self.addr_dpx(offset),
                };
                let value = match opcode {
                    0xD8 | 0xD9 => self.x,
                    _ => self.y,
                };
                let _ = self.rd8(addr);
                self.wr8(addr, value);
            }
            0xFA => {
                // The source byte comes first, and no read of the target.
                let offset = self.fetch8();
                let source = self.rd8(self.addr_dp(offset));
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                self.wr8(addr, source);
            }
            0x8F => {
                let value = self.fetch8();
                let offset = self.fetch8();
                let addr = self.addr_dp(offset);
                let _ = self.rd8(addr);
                self.wr8(addr, value);
            }
            // ---- The stack and idle forms --------------------------------------
            0x00 => {}
            0x0D => self.push8(self.psw),
            0x2D => self.push8(self.a),
            0x4D => self.push8(self.x),
            0x6D => self.push8(self.y),
            0x8E => self.psw = self.pop8(),
            0xAE => self.a = self.pop8(),
            0xCE => self.x = self.pop8(),
            0xEE => self.y = self.pop8(),
            // ---- The core idle and the halt pair ---------------------------------
            0xEF => self.halt = Halt::Sleep,
            0xFF => self.halt = Halt::Stop,
        }
        u32::from(CYCLES[usize::from(opcode)]) + u32::from(taken) * 2
    }

    // -- The per-cycle engine ------------------------------------------------------

    /// One SPC cycle: the constant stage 1 precounter and its three
    /// timers, then the DSP step, then the sample drain. The `$F0`
    /// test byte's timer bits gate the timers, and the powered-on
    /// `$0A` value passes the gate.
    fn cycle(&mut self) {
        self.precount = self.precount.wrapping_add(1) & 0x7F;
        let t0t1_tick = self.precount == 0;
        let t2_tick = self.precount & 0x0F == 0;
        let gate = self.test & 0x08 != 0 && self.test & 0x01 == 0;
        for (n, timer) in self.timers.iter_mut().enumerate() {
            let base_tick = if n == 2 { t2_tick } else { t0t1_tick };
            if gate && timer.enabled && base_tick {
                timer.base_tick();
            }
        }
        self.cycles += 1;
        let ram = &self.ram;
        self.dsp.step(ram);
        if self.cycles.is_multiple_of(SAMPLE_EVERY) {
            self.dsp.take_samples(&mut self.samples);
        }
    }
}

impl Device for Apu {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn bus(&self) -> Option<&dyn BusAccess> {
        Some(self)
    }

    fn bus_mut(&mut self) -> Option<&mut dyn BusAccess> {
        Some(self)
    }

    fn audio(&self) -> Option<&dyn demu_core::AudioSource> {
        Some(self)
    }

    fn audio_mut(&mut self) -> Option<&mut dyn demu_core::AudioSource> {
        Some(self)
    }

    fn reset(&mut self) {
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.sp = 0xEF;
        self.psw = FLAG_Z;
        self.halt = Halt::Core;
        self.ram[..0x00F0].fill(0);
        self.test = 0x0A;
        self.control = 0xB0;
        self.dsp_index = 0;
        self.dsp.reset();
        self.timers = [Timer::new(), Timer::new(), Timer::new()];
        self.to_cpu = [0; 4];
        self.from_cpu = [0; 4];
        self.to_cpu_stale = [0; 4];
        self.from_cpu_stale = [0; 4];
        self.cpu_wrote = 0;
        self.spc_wrote = 0;
        self.precount = 0;
        self.cycles = 0;
        self.samples.clear();
        self.pc = self.reset_vector();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Rom { path }) => {
                let _ = demu_core::boot::load_rom(&mut self.ram, Path::new(path));
            }
            Some(BootState::Hex { bytes }) => {
                let n = bytes.len().min(self.ram.len());
                self.ram[..n].copy_from_slice(&bytes[..n]);
            }
            Some(BootState::Ram { pattern }) => demu_core::boot::fill_ram(&mut self.ram, *pattern),
            _ => {}
        }
        self.pc = self.reset_vector();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SSMP_FIXED + SOUND_RAM + 0x150);
        out.extend_from_slice(b"SSMP");
        out.push(2);
        out.push(self.a);
        out.push(self.x);
        out.push(self.y);
        out.push(self.sp);
        out.push(self.psw);
        out.extend_from_slice(&self.pc.to_le_bytes());
        out.push(self.dsp_index);
        out.push(self.test);
        out.push(self.control);
        out.extend_from_slice(&self.to_cpu);
        out.extend_from_slice(&self.from_cpu);
        out.push(self.precount);
        out.extend_from_slice(&self.cycles.to_le_bytes());
        for timer in &self.timers {
            out.push(timer.target);
            out.push(timer.stage);
            out.push(timer.out);
            out.push(u8::from(timer.enabled));
        }
        out.push(self.halt.halt_byte());
        out.extend_from_slice(&self.to_cpu_stale);
        out.extend_from_slice(&self.from_cpu_stale);
        out.push(self.cpu_wrote & 0x0F | self.spc_wrote << 4);
        out.extend_from_slice(&self.ram);
        out.extend(self.dsp.snapshot());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < SSMP_FIXED + SOUND_RAM || &data[..4] != b"SSMP" || data[4] != 2 {
            return Err(SnapshotError::Mismatch(format!(
                "ssmp chunk needs a ram image of {} bytes tagged with SSMP version 2",
                SSMP_FIXED + SOUND_RAM
            )));
        }
        // The engine chunk validates ahead of the walk.
        self.dsp.restore(&data[SSMP_FIXED + SOUND_RAM..])?;
        self.a = data[5];
        self.x = data[6];
        self.y = data[7];
        self.sp = data[8];
        self.psw = data[9];
        self.pc = u16::from_le_bytes(data[10..12].try_into().expect("2 bytes"));
        self.dsp_index = data[12];
        self.test = data[13];
        self.control = data[14];
        self.to_cpu.copy_from_slice(&data[15..19]);
        self.from_cpu.copy_from_slice(&data[19..23]);
        self.precount = data[23];
        self.cycles = u64::from_le_bytes(data[24..32].try_into().expect("8 bytes"));
        for (i, timer) in self.timers.iter_mut().enumerate() {
            let at = 32 + i * 4;
            timer.target = data[at];
            timer.stage = data[at + 1];
            timer.out = data[at + 2];
            timer.enabled = data[at + 3] != 0;
        }
        self.halt = Halt::from_byte(data[44])?;
        self.to_cpu_stale.copy_from_slice(&data[45..49]);
        self.from_cpu_stale.copy_from_slice(&data[49..53]);
        self.cpu_wrote = data[53] & 0x0F;
        self.spc_wrote = data[53] >> 4;
        self.ram
            .copy_from_slice(&data[SSMP_FIXED..SSMP_FIXED + SOUND_RAM]);
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, _ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        // The SPC-write race window of the previous visit closes at
        // this visit's entry; a CPU read between the visits saw it.
        self.spc_wrote = 0;
        match self.halt {
            Halt::Core => {
                let cycles = self.step();
                for _ in 0..cycles {
                    self.cycle();
                }
                self.next = Some(Tick(tick.0 + self.cpu_div * u64::from(cycles)));
            }
            _ => {
                // The core waits for reset; the clock keeps running.
                self.cycle();
                self.next = Some(Tick(tick.0 + self.cpu_div));
            }
        }
        // The CPU-write race window closes at the visit's end; the
        // SPC's port reads during the visit were the last chance.
        self.cpu_wrote = 0;
    }
}

impl BusAccess for Apu {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let raw = address.0 & 0xFF;
        if (0x40..=0x7F).contains(&raw) {
            let p = (raw & 0x03) as usize;
            // The race form: an SPC port write since the last visit
            // ORs the previous byte into the read.
            if self.race_or && self.spc_wrote & (1 << p) != 0 {
                let v = self.to_cpu_stale[p] | self.to_cpu[p];
                self.spc_wrote &= !(1 << p);
                return u32::from(v);
            }
            u32::from(self.to_cpu[p])
        } else {
            0xFF
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let raw = address.0 & 0xFF;
        if (0x40..=0x7F).contains(&raw) {
            let p = (raw & 0x03) as usize;
            // The race window opens with the write: an SPC read
            // before the next visit boundary gets the OR form.
            if self.race_or {
                self.from_cpu_stale[p] = self.from_cpu[p];
                self.cpu_wrote |= 1 << p;
            }
            self.from_cpu[p] = value as u8;
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let raw = address.0 & 0xFF;
        if (0x40..=0x7F).contains(&raw) {
            self.to_cpu[(raw & 0x03) as usize]
        } else {
            0
        }
    }
}

impl demu_core::AudioSource for Apu {
    fn sample_rate(&self) -> u32 {
        32_000
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::newtypes::{DeviceName, KindName};
    use demu_core::AudioSource;

    /// The device the tests drive, with the `cpu` divider at one so
    /// one domain tick is one SPC cycle.
    fn device() -> Apu {
        device_with_quirks(Vec::new())
    }

    /// The device with the named quirk values applied.
    fn device_with_quirks(quirks: Vec<(String, demu_config::model::QuirkValue)>) -> Apu {
        let instance = demu_config::model::DeviceInstance {
            name: DeviceName::new("spc").expect("device name"),
            kind: KindName::new("apu.core").expect("kind name"),
            domain: "sound".to_string(),
            params: Default::default(),
            dividers: [("cpu".to_string(), 1u32)].into_iter().collect(),
        };
        Apu::new(&instance, &test_config(quirks))
    }

    /// A minimal system config for the sound die.
    fn test_config(
        quirks: Vec<(String, demu_config::model::QuirkValue)>,
    ) -> demu_config::model::SystemConfig {
        demu_config::model::SystemConfig {
            name: "spc-test".to_string(),
            triple: "wdc65c816-nintendo-snes".to_string(),
            description: String::new(),
            clocks: Default::default(),
            devices: Vec::new(),
            devices_by_name: Default::default(),
            buses: vec![demu_config::model::Bus {
                name: "cpu".to_string(),
                address_bits: 24,
                owner: None,
                parent: None,
            }],
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: Default::default(),
            quirks: quirks.into_iter().collect(),
        }
    }

    /// A 64 KiB RAM image: the program at `org` and the reset vector
    /// pointing back at it.
    fn image(org: u16, code: &[u8]) -> Vec<u8> {
        let mut ram = vec![0u8; SOUND_RAM];
        let base = usize::from(org);
        ram[base..base + code.len()].copy_from_slice(code);
        ram[0xFFFE] = (org & 0xFF) as u8;
        ram[0xFFFF] = (org >> 8) as u8;
        ram
    }

    fn boot(apu: &mut Apu, ram: &[u8]) {
        apu.boot(Some(&BootState::Hex {
            bytes: ram.to_vec(),
        }));
    }

    /// The running path of the device visit without the machine
    /// context: one whole instruction, then its cycle engine.
    fn drive(apu: &mut Apu, instructions: u32) {
        for _ in 0..instructions {
            if apu.halt != Halt::Core {
                break;
            }
            let cycles = apu.step();
            for _ in 0..cycles {
                apu.cycle();
            }
        }
    }

    /// True when the opcode carries a taken branch cell.
    fn branchy(opcode: u8) -> bool {
        matches!(
            opcode,
            0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 | 0x2E | 0xDE | 0x6E | 0xFE
        ) || opcode & 0x0F == 0x03
    }

    #[test]
    fn reset_pulls_vector_and_chip_state() {
        let mut apu = device();
        // The zero page clears and the stack points at the top of
        // page 1, with the direct-page flag clear and zero set.
        apu.ram[0x0030] = 0x11;
        apu.sp = 0;
        apu.reset();
        assert_eq!(apu.ram[0x0030], 0);
        assert_eq!(apu.sp, 0xEF);
        assert_eq!(apu.psw, FLAG_Z);
        assert_eq!(apu.pc, 0);
        // A boot image with a vector moves the program counter.
        boot(&mut apu, &image(0x0300, &[0x02]));
        assert_eq!(apu.pc, 0x0300);
    }

    #[test]
    fn boot_image_preloads_ram_over_the_zero_page() {
        let mut ram = image(0x0300, &[0x02]);
        ram[0x0040] = 0x55;
        ram[0x0100] = 0xCD;
        let mut apu = device();
        boot(&mut apu, &ram);
        // The preloaded bytes stand in RAM after the reset clearing.
        assert_eq!(apu.ram[0x0040], 0x55);
        assert_eq!(apu.ram[0x0100], 0xCD);
    }

    #[test]
    fn port_latches_round_trip_both_ways() {
        let mut apu = device();
        let code = &[
            0x8F, 0x44, 0xF4, // MOV $F4,#$44
            0xE4, 0xF5, // MOV A,$F5
            0xC4, 0x60, // MOV $60,A
            0x8F, 0x22, 0xF6, // MOV $F6,#$22
            0xEF, // SLEEP
        ];
        boot(&mut apu, &image(0x0300, code));
        // The CPU writes $2141 after boot; the SPC's $F5 reads it.
        bus_write(&mut apu, 0x2141, 0x99);
        drive(&mut apu, 20);
        assert_eq!(
            apu.ram[0x0060], 0x99,
            "the CPU write lands in the SPC input"
        );
        assert_eq!(
            bus_read(&mut apu, 0x2140),
            0x44,
            "the SPC write lands in the output"
        );
        assert_eq!(bus_read(&mut apu, 0x2142), 0x22);
    }

    #[test]
    fn b_bus_ports_decode_absolute_addresses() {
        let mut apu = device();
        bus_write(&mut apu, 0x2141, 0x99);
        assert_eq!(apu.rd8(0x00F5), 0x99);
        // The mirror at $2164 maps the same input latch.
        bus_write(&mut apu, 0x2164, 0x07);
        assert_eq!(apu.rd8(0x00F4), 0x07);
        apu.wr8(0x00F4, 0x12);
        assert_eq!(bus_read(&mut apu, 0x2140), 0x12);
        assert_eq!(apu.peek(Address(0x2140)), 0x12);
        // The mirror answers with the same latch, and a read past the
        // ports holds the open bus byte.
        assert_eq!(bus_read(&mut apu, 0x2144), 0x12);
        assert_eq!(bus_read(&mut apu, 0x2130), 0xFF);
    }

    #[test]
    fn timer2_counts_per_16_ticks() {
        let mut apu = device();
        apu.timers[2].target = 1;
        apu.timers[2].enabled = true;
        for _ in 0..16 {
            apu.cycle();
        }
        assert_eq!(apu.timers[2].out, 1);
        for _ in 0..5 * 16 {
            apu.cycle();
        }
        assert_eq!(apu.timers[2].out, 6);
        // The read returns the count and zeros it.
        assert_eq!(apu.rd8(0x00FF), 6);
        assert_eq!(apu.rd8(0x00FF), 0);
    }

    #[test]
    fn timer0_and_1_share_the_8000hz_base() {
        let mut apu = device();
        apu.timers[0].target = 2;
        apu.timers[0].enabled = true;
        apu.timers[1].target = 1;
        apu.timers[1].enabled = true;
        for _ in 0..4 * 128 {
            apu.cycle();
        }
        // Four base ticks: timer 0 halves them, timer 1 takes each one.
        assert_eq!(apu.timers[0].out, 2);
        assert_eq!(apu.timers[1].out, 4);
    }

    #[test]
    fn control_write_resets_timers_and_clears_ports() {
        let mut apu = device();
        bus_write(&mut apu, 0x2141, 0x07);
        bus_write(&mut apu, 0x2143, 0x09);
        apu.timers[2] = Timer {
            target: 20,
            stage: 7,
            out: 9,
            enabled: true,
        };
        apu.wr8(0x00F1, 0x10);
        assert_eq!(
            apu.from_cpu,
            [0, 0, 0, 9],
            "the PC10 bit clears ports 0 and 1"
        );
        // Every timer bit of the write resets its timer.
        assert!(!apu.timers[2].enabled);
        assert_eq!(apu.timers[2].out, 0);
        assert_eq!(apu.timers[2].stage, 0);
    }

    #[test]
    fn control_write_restarts_enabled_timers() {
        let mut apu = device();
        apu.timers[2] = Timer {
            target: 5,
            stage: 3,
            out: 9,
            enabled: false,
        };
        apu.wr8(0x00F1, 0x04);
        assert!(apu.timers[2].enabled);
        assert_eq!(apu.timers[2].stage, 0);
        assert_eq!(apu.timers[2].out, 0);
        for _ in 0..5 * 16 {
            apu.cycle();
        }
        // Five base ticks against the target 5: the first match ticks
        // the output and drops the stage back to zero for the rest.
        assert_eq!(apu.timers[2].out, 1);
        assert_eq!(apu.timers[2].stage, 0);
    }

    #[test]
    fn mul_places_product_low_and_high() {
        let mut apu = device();
        apu.y = 0x13;
        apu.a = 0x04;
        apu.op_mul();
        assert_eq!((apu.y, apu.a), (0x00, 0x4C));
        assert_eq!(apu.psw & FLAG_Z, FLAG_Z);
        apu.y = 0x1F;
        apu.a = 0x1F;
        apu.op_mul();
        assert_eq!((apu.y, apu.a), (0x03, 0xC1));
        assert_eq!(apu.psw & FLAG_Z, 0);
    }

    #[test]
    fn div_returns_exact_pair_and_flags() {
        let mut apu = device();
        apu.y = 0x00;
        apu.a = 0x07;
        apu.x = 0x02;
        apu.op_div();
        assert_eq!(apu.a, 0x03);
        assert_eq!(apu.y, 0x01);
        assert_eq!(apu.psw & FLAG_V, 0);
        // The exact boundary at 512/4 = 128 with no remainder.
        apu.y = 0x02;
        apu.a = 0x00;
        apu.x = 0x04;
        apu.op_div();
        assert_eq!(apu.a, 0x80);
        assert_eq!(apu.y, 0x00);
        assert_eq!(apu.psw & FLAG_V, 0);
    }

    #[test]
    fn div_overflow_and_zero_divisor_hold_v() {
        let mut apu = device();
        // The quotient runs past the 9-bit path: V holds.
        apu.y = 0x02;
        apu.a = 0x00;
        apu.x = 0x01;
        apu.op_div();
        assert_eq!(apu.psw & FLAG_V, FLAG_V);
        // A zero divisor folds through the overflow path.
        apu.y = 0x00;
        apu.a = 0x00;
        apu.x = 0x00;
        apu.op_div();
        assert_eq!(apu.psw & FLAG_V, FLAG_V);
    }

    #[test]
    fn bit_ops_reach_the_13bit_region() {
        let code = &[
            0x80, // SETC
            0xCA, 0x05, 0x41, // MOV1 $0105.2,C
            0xAA, 0x00, 0x20, // MOV1 C,$0000.1
            0x8F, 0x04, 0x02, // MOV $02,#$04
            0x0A, 0x02, 0x40, // OR1 C,$0000.2
            0x4A, 0x02, 0x40, // AND1 C,$0000.2
            0x6A, 0x02, 0x40, // AND1 C,~$0000.2
            0x2A, 0x02, 0xE0, // OR1 C,~$0000.7
            0xEA, 0xFF, 0xFF, // NOT1 $1FFF.7
            0xEF, // SLEEP
        ];
        let mut apu = device();
        boot(&mut apu, &image(0x0300, code));
        drive(&mut apu, 20);
        // The carry carried the set bit to the 13-bit address $0105,
        // cleared against $0000.1, and rode the bit tests.
        assert_eq!(apu.ram[0x0105], 0x04);
        assert_eq!(apu.ram[0x1FFF], 0x80);
        assert_eq!(apu.psw & FLAG_C, FLAG_C);
        // The $0000 cell holds only the two writes of the sequence.
        assert_eq!(apu.ram[0x0002], 0x04);
    }

    #[test]
    fn f8_and_f9_read_the_ram_underneath() {
        let mut apu = device();
        // The write-through copy stores into RAM, so the read of the
        // register-space fallback returns the stored bytes.
        apu.wr8(0x00F8, 0x5A);
        apu.wr8(0x00F9, 0xC3);
        assert_eq!(apu.rd8(0x00F8), 0x5A);
        assert_eq!(apu.rd8(0x00F9), 0xC3);
    }

    #[test]
    fn dsp_bridge_routes_f2_and_f3() {
        let mut apu = device();
        apu.wr8(0x00F2, 0x03);
        apu.wr8(0x00F3, 0x27);
        assert_eq!(apu.rd8(0x00F2), 0x03);
        assert_eq!(apu.rd8(0x00F3), 0x27);
        assert_eq!(apu.dsp.register(0x03), 0x27);
        // The index past $7F reads the mirrored register and its write
        // is ignored.
        apu.wr8(0x00F2, 0xA0);
        assert_eq!(apu.rd8(0x00F3), apu.dsp.register(0x20));
        apu.wr8(0x00F3, 0x99);
        assert_eq!(apu.dsp.register(0x20), 0);
    }

    #[test]
    fn audio_drains_one_sample_per_cycle_group() {
        let mut apu = device();
        for _ in 0..32 {
            apu.cycle();
        }
        assert_eq!(apu.take_samples().len(), 1);
        for _ in 0..5 * 32 {
            apu.cycle();
        }
        assert_eq!(apu.take_samples().len(), 5);
        assert_eq!(apu.sample_rate(), 32_000);
    }

    #[test]
    fn program_loop_counts_down_and_stores() {
        let code = &[
            0xE8, 0x00, // MOV A,#$00
            0xCD, 0x05, // MOV X,#$05
            0xBC, // INC A
            0x1D, // DEC X
            0xD0, 0xFC, // BNE -4: back to the increment
            0xC4, 0x60, // MOV $60,A
            0xD8, 0x61, // MOV $61,X
            0xEF, // SLEEP
        ];
        let mut apu = device();
        boot(&mut apu, &image(0x0300, code));
        drive(&mut apu, 30);
        assert_eq!(apu.ram[0x0060], 5);
        assert_eq!(apu.ram[0x0061], 0);
        assert_eq!(apu.a, 5);
        assert_ne!(apu.halt, Halt::Core);
    }

    #[test]
    fn word_ops_compute_and_flag() {
        let mut apu = device();
        apu.ram[0x0040] = 0x34;
        apu.ram[0x0041] = 0x12;
        apu.ram[0x0042] = 0xCD;
        apu.ram[0x0043] = 0x56;
        apu.pc = 0x8000;
        apu.ram[0x8000] = 0xBA; // MOVW YA,$40
        apu.ram[0x8001] = 0x40;
        apu.step();
        assert_eq!((apu.y, apu.a), (0x12, 0x34));
        apu.ram[0x8002] = 0x7A; // ADDW YA,$42
        apu.ram[0x8003] = 0x42;
        apu.pc = 0x8002;
        apu.step();
        assert_eq!((apu.y, apu.a), (0x69, 0x01));
        // SUBW removes the pair back: 0x6901 - 0x56CD = 0x1234, with
        // the carry showing no word borrow.
        apu.ram[0x8004] = 0x9A; // SUBW YA,$42
        apu.ram[0x8005] = 0x42;
        apu.pc = 0x8004;
        apu.step();
        assert_eq!((apu.y, apu.a), (0x12, 0x34));
        assert_eq!(apu.psw & FLAG_C, FLAG_C);
        // CMPW against a larger word shows the borrow with no carry
        // and the negative edge.
        apu.ram[0x0046] = 0x00;
        apu.ram[0x0047] = 0x20;
        apu.ram[0x8006] = 0x5A; // CMPW YA,$46
        apu.ram[0x8007] = 0x46;
        apu.pc = 0x8006;
        apu.step();
        assert_eq!(apu.psw & FLAG_C, 0);
        // XCN swaps the nibbles of A.
        apu.ram[0x8008] = 0x9F; // XCN A
        apu.pc = 0x8008;
        apu.step();
        assert_eq!(apu.a, 0x43);
    }

    #[test]
    fn every_opcode_terminates_on_its_published_cost() {
        for opcode in 0u16..256 {
            let mut apu = device();
            apu.pc = 0x8000;
            apu.ram[0x8000] = opcode as u8;
            let cost = apu.step();
            let cell = u32::from(CYCLES[opcode as usize]);
            assert!(
                cost == cell || (branchy(opcode as u8) && cost == cell + 2),
                "opcode {opcode:02X} cost {cost}, cell {cell}"
            );
        }
    }

    #[test]
    fn snapshot_round_trip_covers_device_state() {
        let mut apu = device();
        apu.a = 0x11;
        apu.x = 0x22;
        apu.y = 0x33;
        apu.sp = 0xAB;
        apu.psw = FLAG_N | FLAG_Z;
        apu.pc = 0x1234;
        apu.ram[0x1234] = 0x77;
        apu.wr8(0x00F2, 0x10);
        apu.wr8(0x00F3, 0x33);
        apu.timers[2] = Timer {
            target: 20,
            stage: 7,
            out: 3,
            enabled: true,
        };
        bus_write(&mut apu, 0x2141, 0x5A);
        apu.wr8(0x00F5, 0xA1);
        for _ in 0..7 {
            apu.cycle();
        }
        let snap = apu.snapshot();
        let mut twin = device();
        twin.restore(&snap).expect("restore the ssmp chunk");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.a, 0x11);
        assert_eq!(twin.pc, 0x1234);
        assert_eq!(twin.timers[2].target, 20);
        assert!(twin.timers[2].enabled);
        assert_eq!(twin.dsp.register(0x10), 0x33);
        // The SPC's $F5 read takes the CPU input, and the CPU's $2141
        // read takes the SPC output.
        assert_eq!(twin.rd8(0x00F5), 0x5A);
        assert_eq!(bus_read(&mut twin, 0x2141), 0xA1);
        // A chunk with a bad tag or short body rejects.
        let mut broken = device();
        assert!(broken.restore(&snap[2..]).is_err());
        assert!(broken.restore(&[]).is_err());
    }

    #[test]
    fn the_port_race_or_form_needs_the_quirk() {
        let writer = [
            0xE8, 0xAB, // MOV A,#$AB
            0xC4, 0xF6, // MOV $F6,A  (the output port two)
            0xE8, 0x54, // MOV A,#$54
            0xC4, 0xF6, // MOV $F6,A
        ];
        // Without the quirk the plain latch answers.
        let mut plain = device();
        boot(&mut plain, &image(0x0300, &writer));
        drive(&mut plain, 4);
        assert_eq!(bus_read(&mut plain, 0x2142), 0x54);

        // With the quirk the SPC-write window ORs the stale byte.
        let mut apu = device_with_quirks(vec![(
            "apu.port_race_or".to_string(),
            demu_config::model::QuirkValue::Bool(true),
        )]);
        boot(&mut apu, &image(0x0300, &writer));
        drive(&mut apu, 4);
        assert_eq!(bus_read(&mut apu, 0x2142), 0xAB | 0x54, "the race OR form");
        // The read consumed the window: the plain latch answers now.
        assert_eq!(bus_read(&mut apu, 0x2142), 0x54);
    }

    #[test]
    fn the_cpu_write_race_quals_the_spc_read() {
        // The SPC's first port access is the read itself; a store's
        // destination dummy read consumes the window ahead of it, so
        // the program reads before it stores.
        let reader = [
            0xE4, 0xF6, // MOV A,$F6  (the SPC read of port two)
            0xC4, 0x60, // MOV $60,A
            0xEF, // SLEEP parks the core
        ];
        let mut apu = device_with_quirks(vec![(
            "apu.port_race_or".to_string(),
            demu_config::model::QuirkValue::Bool(true),
        )]);
        boot(&mut apu, &image(0x0300, &reader));
        // The CPU writes the port twice before the SPC reads it: the
        // second write's window carries the first byte as the stale
        // form.
        bus_write(&mut apu, 0x2142, 0x0F);
        bus_write(&mut apu, 0x2142, 0xF0);
        drive(&mut apu, 2);
        assert_eq!(apu.ram[0x60], 0x0F | 0xF0, "the race OR form of the read");
    }

    #[test]
    fn snapshot_round_trip_keeps_the_halt() {
        let code = [
            0xE8, 0x00, // MOV A,#$00
            0xEF, // SLEEP
        ];
        let mut apu = device();
        boot(&mut apu, &image(0x0300, &code));
        drive(&mut apu, 5);
        assert_ne!(apu.halt, Halt::Core);
        let snap = apu.snapshot();
        let mut twin = device();
        twin.restore(&snap).expect("restore the ssmp chunk");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.halt, apu.halt);
        // The halted core stays halted: the driven ticks step no
        // instruction, so the program counter stays put.
        let pc = twin.pc;
        drive(&mut twin, 5);
        assert_eq!(twin.pc, pc);
    }

    /// The CPU-side write of one B-bus port byte.
    fn bus_write(apu: &mut Apu, address: u32, value: u8) {
        apu.write(
            AccessWidth::Byte,
            Address(address),
            u32::from(value),
            Tick(0),
        );
    }

    /// The CPU-side read of one B-bus port byte.
    fn bus_read(apu: &mut Apu, address: u32) -> u8 {
        apu.read(AccessWidth::Byte, Address(address), Tick(1)) as u8
    }
}
