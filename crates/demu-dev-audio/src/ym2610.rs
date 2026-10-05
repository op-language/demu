//! The Yamaha YM2610 (OPNB) sound generator: four FM channels of four
//! operators, the three-channel SSG of the YM2149 form, six fixed-rate
//! ADPCM-A channels and one variable-rate ADPCM-B channel over two
//! external sample ROMs, the two interval timers and the LFO — the
//! `ym2610.chip` device kind of the Neo Geo AES.
//!
//! The chip answers eight-bit register writes at its four register
//! ports: port 0 latches the address byte for A1=0 (the SSG, the
//! ADPCM-B and the FM-common-and-channels-1-2 form), port 1 stores
//! data there, port 2 latches for A1=1 (the ADPCM-A block and FM
//! channels 3-4), and port 3 stores there. Each A1 bank keeps its own
//! latched address and a data write never clears it. All register
//! writes queue and apply at the next FM-sample boundary in write
//! order, the deterministic form this build models, and a data write
//! sets the busy form for its published window.
//!
//! # The pinned register surface
//!
//! Per-operator registers sit in the seven base groups `$31-$3E`
//! (detune and multiple), `$41-$4E` (total level), `$51-$5E`
//! (key scale and attack rate), `$61-$6E` (amplitude-LFO enable and
//! first decay rate), `$71-$7E` (secondary decay rate), `$81-$8E`
//! (decay level and release rate) and `$91-$9E` (SSG-EG). The
//! operator address walk inside a channel follows the published
//! odd/even layout: for channels 1 and 3 (the odd slot rows) the
//! operators sit at `*1`, `*5`, `*9`, `*D` and for channels 2 and 4
//! at `*2`, `*6`, `*A`, `*E`, and in that address order the operators
//! are OP1, OP3, OP2, OP4 (the published operator order of the
//! neogeodev FM table; the die-verified operator maps of the ymfm and
//! Nuked-OPN2 cores carry the same wiring). Registers at the unused
//! positions of a row store into the register shadow and do nothing.
//!
//! Per-channel registers sit at `$A1`/`$A5` and `$A2`/`$A6` per bank
//! (channels 1 and 2 in the A1=0 bank, channels 3 and 4 in the A1=1
//! bank), the published high-register-first pair form: the high byte
//! (the three-bit block over the three upper frequency-number bits)
//! latches first and the low byte completes the fourteen-bit form,
//! per the caution from the application manual. The feedback and
//! algorithm forms sit at `$B1`/`$B2` and the output and
//! LFO-sensitivity forms at `$B5`/`$B6`.
//!
//! Bank A1=0 answers, per the published allocation:
//!
//! | Range | Register | Content |
//! |-------|----------|---------|
//! | `$00-$0D` | SSG | the YM2149 register block; readable |
//! | `$10-$1B` | ADPCM-B | control 1 at `$10`, control 2 at `$11`, start `$12/$13`, end `$14/$15`, delta-N `$19/$1A`, level `$1B` |
//! | `$1C` | Flag control | write-one resets and masks, write-zero unmasks the ADPCM end flags |
//! | `$21` | FM test | stores into the register shadow, no effect |
//! | `$22` | LFO | bit 3 enable, bits 2-0 rate |
//! | `$24`/`$25` | Timer A | the ten-bit ganged load: `$24` the eight MSBs, `$25` the two LSBs |
//! | `$26` | Timer B | the eight-bit load |
//! | `$27` | Mode | bits 7-6 the channel-2 forms (CSM on the top bit, multi frequency on the next), bit 5 reset B, bit 4 reset A, bit 3 enable B, bit 2 enable A, bit 1 load B, bit 0 load A |
//! | `$28` | Key on/off | bits 7-4 the four operators (bit 7 = OP4 down to bit 4 = OP1), bits 2-0 the channel code |
//! | `$29-$2F` | Test | stores land in the register shadow, no effect |
//! | `$30-$B6` | FM operators | channels 1-2 |
//!
//! Bank A1=1 answers `$00-$2D` as the ADPCM-A register block (the
//! dump/key-on register `$00`, the total level `$01`, the test `$02`,
//! the per-channel pan and level `$08-$0D`, and the per-channel start
//! and end address pairs `$10-$15`/`$18-$1D` and `$20-$25`/`$28-$2D`),
//! `$30-$B6` as FM channels 3-4, and the rest as the register shadow.
//!
//! # The port protocol, the status and the readback
//!
//! The four ports decode by the low two bits of the address the
//! window wires them at: port 0 the A1=0 address form, port 1 the
//! A1=0 data form, port 2 the A1=1 address form, port 3 the A1=1 data
//! form. A data write whose bank disagrees with the latched
//! address-bit is ignored, per the published pairing caution. Status
//! 0 rides any port 0 read: bit 7 the busy form, bit 1 the Timer B
//! overflow flag and bit 0 the Timer A one, per the application
//! manual's Status 0 table. Status 1 rides any port 2 read: bit 7 the
//! ADPCM-B end flag and bits 5-0 the ADPCM-A channel end flags,
//! masked through the flag-control register. Only the SSG registers
//! read back: a lower data-port read with the latched register
//! address below `$0E` answers the SSG register value, the `$FF`
//! address answers 1 as the published chip identification code
//! (reading register `$FF`), and other addresses answer 0.
//!
//! # The timers
//!
//! Timer A counts the ten-bit load range up one step per FM sample
//! and Timer B the eight-bit range up one step per sixteen FM
//! samples. The manual publishes the intervals as `72×(1024−load)`
//! and `1152×(256−load)` half-clock ticks, which the up-counting walk
//! reproduces exactly with the sample period at 144 chip clocks. On
//! reaching the top the timer sets its overflow form and reloads
//! from the register value so the timer keeps cycling; the counters
//! themselves stay inaccessible, per the published form. The
//! mode-register enable bits gate the flags and the IRQ, the
//! write-one reset bits clear the flags, and load one starts the
//! timer with a reload and load zero stops it. While either enabled
//! overflow flag sits set the `irq` output line drives level high
//! and drops again when both clear; the line moves only through
//! machine-context notify and only on real edges.
//!
//! # Channel-2 special modes
//!
//! The mode register's bits 7-6 select the channel-2 forms: multi
//! frequency (any nonzero form) runs each of channel 2's operators
//! at its own F-Number from the supplementary pairs, and CSM (the
//! top-bit form) re-keys channel 2 when timer A expires, the
//! automatic key form the manual describes. The multi-frequency
//! pairs follow the die-extracted form the ymfm core and the
//! Nuked-OPN2 core agree on: operator 1 reads the `$A9`/`$AD` pair,
//! operator 2 the `$AA`/`$AE` pair, operator 3 the `$A8`/`$AC` pair,
//! and operator 4 keeps the channel's own `$A2`/`$A6` pair. Each
//! pair is written high-byte first per the published caution, and
//! the pairs live in the A1=0 bank (written through ports 4/5).
//! The CSM key pulse of this build holds the key for one FM sample
//! and then releases it from the current envelope.
//!
//! # The pitch and the sine path
//!
//! The phase accumulator is twenty bits and wraps at `$100000`; one
//! FM sample advances it by the frequency step. The step follows the
//! die-verified OPN arithmetic: the frequency number doubled then
//! shifted up by the block and down by two, plus the detune, clamped
//! to seventeen bits, then times the multiple in its x.1 form; the
//! multiple register of zero runs the operator at half rate. The
//! detune adds the published per-key-code table value with the
//! register's third bit negating it, per the verified chip table.
//! The sine path uses the two chip forms: the quarter-wave log-sine
//! table and the exponential power table, over the wave index (the
//! top ten bits of the phase; the tenth bit signs the wave and the
//! ninth mirrors the quarter). An operator reads silent once its
//! envelope passes the published quiet form. This build states no
//! pitch in hertz: the manual's published figures were computed at
//! the nominal 8 MHz clock and the Neo Geo clock form lives in the
//! configuration, like the ym2612 file does.
//!
//! # The envelope generator
//!
//! Each operator runs the ten-bit envelope over the published states
//! attack, first decay, secondary decay and release. Key on resets
//! the phase and starts the attack; the published instant form
//! (attack rates at or past the key-scaled 62) lands at full level at
//! once, and the attack otherwise shrinks the gap toward the top by
//! the per-step fraction. Key off starts the release from the current
//! form. The decay and release forms climb the attenuation by the
//! per-step increment and clamp at the ten-bit top; the first decay
//! hands over at the sustain level and a sustain level of fifteen
//! acts like thirty-one, per the published form. Envelope steps run
//! once every third FM sample, the die-verified envelope-cycle form,
//! and the published 64-row per-rate table picks the per-step
//! increments by rate value and step phase: rates below four never
//! move and the top rates reach the ceiling fastest. The published
//! rate scaling follows: the final rate is two times the register
//! rate plus the key code divided down (per the rate-scaling form
//! key code / 8, /4, /2 or /1) rounded toward zero; the register rate
//! of zero keeps the rate at zero.
//!
//! The total level enters the output attenuation in eights, the
//! about-0.75 dB per-step published form covering the whole 127 of
//! the chip, and the decay level enters in the published eight-fold
//! comparison form.
//!
//! # The SSG-EG forms
//!
//! The four-bit SSG-EG register carries the enabled bit and the
//! three-bit type. Per the die-verified behavior the envelope runs
//! the published eight types: the zero, two, four and six forms
//! repeat, with type two and six alternating the inversion each
//! cycle; the one, three, five and seven forms run once and then
//! hold, the five and seven forms inverted per the type forms. The
//! repeat forms restart the attack when the envelope crosses the mid
//! form, with the phase-reset types (the even repeat forms below the
//! alternating bit) also resetting the phase. Under the mid form the
//! stepping runs at four times the rate, per the die form.
//!
//! # The SSG
//!
//! The SSG is the YM2149 form: three tone channels and one shared
//! noise generator behind the per-channel mixer enables. SSG clocks
//! run at 250 kHz at the nominal 8 MHz master, one tick every 32 chip
//! clocks (four and a half ticks per FM sample, accumulated exactly).
//! A tone square toggles once per tone-period tick for a full period
//! of two times the programmed value, the manual's published
//! `8M/(64×TP)` form with the twelve-bit period; period zero lands as
//! period one, per the published minimum resolution. The noise LFSR
//! (the 17-bit register with the bit-0-xor-bit-3 input, seed one,
//! bit 0 the output) shifts once per two period ticks behind its
//! prescale, the manual's `8M/(64×NP)` form over the five-bit period.
//! The shared envelope walker takes one of its 32 steps per
//! envelope-period tick, the manual's `8M/(1024×EP)` form over the
//! 32-step published cycle; the shape register write restarts the
//! walk per the published form, with the continue-free shapes
//! mapping to the ramp's own hold form and the shape bit fields
//! walking the classic mapping. A channel gates its output through
//! the tone and noise enables (the removed-I/O note of the die: an
//! enable carries the constant pass-through), then rides either its
//! fixed level (the zero level still reads one, per the published
//! note) or the shared envelope row. The three channels mix at the
//! MAME-derived amplitude table for the SSG DAC, and the mixed form
//! folds to one mono value with the 2-over-9 scaling the SSG mono
//! mixing of the ymfm core uses.
//!
//! # The ADPCM engines
//!
//! Both ADPCM engines decode 4-bit Yamaha forms from their external
//! ROM; the two images do not ride the chip's own boot state but the
//! named ROM devices' boot states, read at assembly time. All
//! addresses address in units of 256 bytes through the start and end
//! register pairs, per the published resolution, and both engines
//! read outside the ROM image by wrapping into it, the house form
//! for a short synthetic image.
//!
//! ADPCM-A: six channels at the fixed 18.5 kHz form (one nibble per
//! third FM sample, the published 8 MHz / 12 prescale / 6 clocks per
//! access walk over the six channels). The dump register `$00` keys
//! on and dumps: a zero start bit plays and a one stops, per the
//! published dump form, and repeated key ons restart the sample per
//! the die form. The twelve-bit accumulator wraps like the MSM5205
//! form on this chip, the delta walks the published 49-entry forecast
//! table, and the channel and total levels enter as combined
//! attenuators in the 0.75 dB steps, with the instrument level on
//! five bits and the total level on six. A channel ends when its
//! address compares against the chunk just past its end register
//! pair on the twenty-bit window, the ym2610 die form with the end
//! chunk inclusive, and the unmasked end sets its flag.
//!
//! ADPCM-B: one channel with the variable rate from 1.85 kHz to 55.5
//! kHz. Control 1 at `$10` carries start (bit 7), repeat (bit 4) and
//! reset (bit 0); the external-memory form is forced on and
//! recording disabled on this chip, per the published form. The rate
//! register is the sixteen-bit delta-N at `$19`/`$1A`: the engine
//! advances a sixteen-bit fraction once per FM sample and decodes one
//! nibble on each wrap, with linear interpolation between the
//! previous and the current decoded form, the published
//! linear-interpolation form. The level `$1B` scales the output
//! linearly, the loudest value `$FF`. The end address is inclusive:
//! the sample plays through the last chunk of its end register pair,
//! then it repeats or stops, and the unmasked stop sets the end
//! flag.
//!
//! # The output and the snapshot
//!
//! One visit of the device is one FM sample at 144 chip clocks. FM
//! channel outputs follow the 13-bit no-intermediate-clip form the
//! die-verified cores use: the per-operator algorithm walk halves
//! each operator output and the per-channel result clips at 32767,
//! each channel wave riding the shared log-sine and power tables.
//! ADPCM outputs ride their engine forms. The mono mix averages the
//! two published outputs and adds the SSG mono form, then saturates
//! into the 16-bit output; the sample rate is the published 8 MHz
//! chip clock over the 144-chip-clock sample period, truncated. The
//! `div` divider names the domain ticks per FM sample.
//!
//! The device snapshots under the `YMNB` tag, version 1: the register
//! shadow, the address and frequency latches, the supplementary
//! pairs, the FM operator phases envelopes and keys, the FM channel
//! forms, the timers, the LFO counters, the SSG state, the ADPCM
//! engines, the end flags and the run flags. The ROM bytes do not
//! ride the chunk and the restored device re-arms its schedule.
//!
//! Sources: the Yamaha YM2610 Application Manual (the ajworld.net
//! transcription, the two manual sections as one document) for the
//! port and A1 allocation form, the wait times, the status tables,
//! the timer equations, the SSG register set and equations, the
//! ADPCM-A and ADPCM-B register sets, the address forms, the dump
//! and control forms, the LFO frequency table and the PMS/AMS depth
//! tables; the neogeodev wiki pages (SSG, FM, ADPCM, YM2610 timers,
//! YM2610 registers) for the same facts confirmed a second way, the
//! ports 6/7 split, the SSG readback form, the published channel
//! codes and the ADPCM flag-control process; the ymfm FM core (Aaron
//! Giles) for the die-extracted sine and power tables, the per-rate
//! envelope stepping table, the envelope state machine, the operator
//! map with its published address order, the algorithm arithmetic,
//! the detune and pitch-LFO tables and the ADPCM decode law; and the
//! Nuked-OPN2 core (Alexey Khokholov, built from chip studies) for
//! the same facts confirmed a second way, the special-mode
//! frequency-pair form and the internal scan order.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{AudioSource, Device, Snapshot, SnapshotError};

/// The FM sample rate: the published 8 MHz chip clock over the
/// 144-chip-clock sample period, truncated.
const SAMPLE_RATE: u32 = 55_555;
/// The mixed sample buffer cap; the media pipeline drains earlier.
const SAMPLE_CAP: usize = 0x4000;
/// The domain ticks per FM sample when the `div` divider is absent:
/// the published sample period of 144 chip clocks at the chip clock
/// the clock domain carries.
const DEFAULT_DIV: u32 = 144;
/// The FM sample period in chip clocks, the published rate model.
const FM_SAMPLE_PERIOD: u32 = 144;
/// The SSG tick period in chip clocks: the published 250 kHz PSG
/// sample form.
const SSG_SAMPLE_PERIOD: u32 = 32;
/// The chip runs four FM channels of four operators (the YM2610B
/// variant runs six, outside this build).
const FM_CHANNELS: usize = 4;
const OPS: usize = 4;
/// The SSG runs three tone channels and one shared noise form.
const SSG_CHANNELS: usize = 3;
/// ADPCM-A runs six channels.
const ADPCMA_CHANNELS: usize = 6;
/// The envelope reads silent above this ten-bit attenuation, the
/// published quiet form of the chip.
const EG_QUIET: u32 = 0x380;
/// The ten-bit envelope ceiling.
const EG_MAX: u32 = 0x3FF;
/// The SSG-EG cycle boundary form.
const EG_MID: u32 = 0x200;
/// Timer A counts up through a ten-bit range and Timer B through an
/// eight-bit range, per the published timer forms.
const TIMER_A_LIMIT: u32 = 0x400;
const TIMER_B_LIMIT: u32 = 0x100;
/// Timer B advances one count per sixteen FM samples.
const TIMER_B_SAMPLER: u32 = 16;
/// The phase accumulator wraps at this twenty-bit form.
const PHASE_LIMIT: u32 = 0x10_0000;
/// The LFO triangle covers 128 counts per cycle.
const LFO_PHASE: u32 = 0x7F;
/// The envelope counter masks here each sample; the checks read the
/// low bits only, so the mask form stays exact.
const ENV_COUNTER_MASK: u32 = 0x00FF_FFFF;
/// The per-channel output clip of the FM path: the 13-bit
/// no-intermediate-clip form of the OPN family clips here.
const CHANNEL_CLIP: i32 = 32_767;
/// The register file per bank.
const REG_FILE: usize = 0x100;
/// The ADPCM-A register block length (the published `$00-$2D` form).
const ADPCMA_REG_FILE: usize = 0x30;
/// The ADPCM-B register block length (the internal `$00-$10` form).
const ADPCMB_REG_FILE: usize = 0x11;
/// The SSG register block length.
const SSG_REG_FILE: usize = 0x10;
/// The end-flag mask default: the six ADPCM-A flags and the ADPCM-B
/// one unmasked, the dead bit 6 masked, per the device reset of the
/// MAME core.
const EOS_FLAG_MASK: u8 = 0xBF;
/// The published register-write wait window in chip clocks, per the
/// application manual's wait-time table: a data write clears the
/// busy form after 83 clocks (an address write clears after 17 and
/// rides as the bus discipline).
const BUSY_DATA_CYCLES: u32 = 83;
/// The ADPCM-B internal status forms per the published register
/// behavior.
const ADPCMB_EOS: u8 = 0x01;
const ADPCMB_PLAYING: u8 = 0x04;

/// Per-step increment reads as the four-bit field for one of the
/// eight cycle phases, low nibble first, per the die-verified
/// stepping table of the ymfm core.
///
/// | Rate | Per-step increments |
/// |------|---------------------|
/// | 0-3 | none |
/// | 4-47 | the one-and-zero die walk |
/// | 48-51 | one per step |
/// | 52-59 | twos then fours |
/// | 60-63 | eights |
const ADSR_INCREMENTS: [u32; 64] = [
    0x0000_0000,
    0x0000_0000,
    0x1010_1010,
    0x1010_1010, // 0-3
    0x1010_1010,
    0x1010_1010,
    0x1110_1110,
    0x1110_1110, // 4-7
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 8-11
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 12-15
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 16-19
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 20-23
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 24-27
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 28-31
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 32-35
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 36-39
    0x1010_1010,
    0x1011_1010,
    0x1110_1110,
    0x1111_1110, // 40-43
    0x1110_1110,
    0x1110_1110,
    0x1110_1110,
    0x1110_1110, // 44-47
    0x1111_1111,
    0x2111_2111,
    0x2121_2121,
    0x2221_2221, // 48-51
    0x2222_2222,
    0x4222_4222,
    0x4242_4242,
    0x4442_4442, // 52-55
    0x4444_4444,
    0x8444_8444,
    0x8484_8484,
    0x8884_8884, // 56-59
    0x8888_8888,
    0x8888_8888,
    0x8888_8888,
    0x8888_8888, // 60-63
];

/// The published detune table: per key code (the five-bit form of
/// the three block bits over the two note bits) against the two
/// register magnitude bits, per the die-verified table of the ymfm
/// core. The detune register's third bit flips the sign.
const DETUNE: [[u8; 4]; 32] = [
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 1, 2, 2],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 5],
    [0, 2, 4, 5],
    [0, 2, 4, 6],
    [0, 2, 4, 6],
    [0, 2, 5, 7],
    [0, 2, 5, 8],
    [0, 3, 6, 8],
    [0, 3, 6, 9],
    [0, 3, 7, 10],
    [0, 4, 8, 11],
    [0, 4, 8, 12],
    [0, 4, 9, 13],
    [0, 5, 10, 14],
    [0, 5, 11, 16],
    [0, 6, 12, 17],
    [0, 6, 13, 19],
    [0, 7, 14, 20],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
];

/// The published frequency-number note form: the four frequency
/// number bits above the fourth pick the fifth bit of the key code,
/// per the YM2608-manual equation folded to a constant.
const FNUM_NOTE_BITS: u16 = 0xFE80;

/// FM samples per LFO count per published rate: the manual's LFO
/// table (3.98 through 72.2 Hz) converted over the 128-count cycle
/// at the 55555 Hz sample rate.
const LFO_DIVIDERS: [u32; 8] = [109, 78, 72, 68, 63, 45, 9, 6];

/// The pitch-LFO shift pairs per sensitivity and depth, two nibbles
/// per depth: the top seven frequency-number bits shift by each
/// nibble shift and sum, per the die-verified cheap-multiply table
/// of the ymfm core.
const LFO_PM_SHIFTS: [[u8; 8]; 8] = [
    [0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77, 0x77],
    [0x77, 0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x72],
    [0x77, 0x77, 0x77, 0x72, 0x72, 0x72, 0x17, 0x17],
    [0x77, 0x77, 0x72, 0x72, 0x17, 0x17, 0x12, 0x12],
    [0x77, 0x77, 0x72, 0x17, 0x17, 0x17, 0x12, 0x07],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
    [0x77, 0x77, 0x17, 0x12, 0x07, 0x07, 0x02, 0x01],
];
// The published log-sine quarter-wave table: the eight bits
// of the quarter phase pick the attenuation of the absolute
// sine in the 4.8 form, the OPN die form extracted per the
// ymfm core. The quarter starts at the wave zero crossing.
const LOG_SIN: [u16; 256] = [
    0x0859, 0x06C3, 0x0607, 0x058B, 0x052E, 0x04E4, 0x04A6, 0x0471, 0x0443, 0x041A, 0x03F5, 0x03D3,
    0x03B5, 0x0398, 0x037E, 0x0365, 0x034E, 0x0339, 0x0324, 0x0311, 0x02FF, 0x02ED, 0x02DC, 0x02CD,
    0x02BD, 0x02AF, 0x02A0, 0x0293, 0x0286, 0x0279, 0x026D, 0x0261, 0x0256, 0x024B, 0x0240, 0x0236,
    0x022C, 0x0222, 0x0218, 0x020F, 0x0206, 0x01FD, 0x01F5, 0x01EC, 0x01E4, 0x01DC, 0x01D4, 0x01CD,
    0x01C5, 0x01BE, 0x01B7, 0x01B0, 0x01A9, 0x01A2, 0x019B, 0x0195, 0x018F, 0x0188, 0x0182, 0x017C,
    0x0177, 0x0171, 0x016B, 0x0166, 0x0160, 0x015B, 0x0155, 0x0150, 0x014B, 0x0146, 0x0141, 0x013C,
    0x0137, 0x0133, 0x012E, 0x0129, 0x0125, 0x0121, 0x011C, 0x0118, 0x0114, 0x010F, 0x010B, 0x0107,
    0x0103, 0x00FF, 0x00FB, 0x00F8, 0x00F4, 0x00F0, 0x00EC, 0x00E9, 0x00E5, 0x00E2, 0x00DE, 0x00DB,
    0x00D7, 0x00D4, 0x00D1, 0x00CD, 0x00CA, 0x00C7, 0x00C4, 0x00C1, 0x00BE, 0x00BB, 0x00B8, 0x00B5,
    0x00B2, 0x00AF, 0x00AC, 0x00A9, 0x00A7, 0x00A4, 0x00A1, 0x009F, 0x009C, 0x0099, 0x0097, 0x0094,
    0x0092, 0x008F, 0x008D, 0x008A, 0x0088, 0x0086, 0x0083, 0x0081, 0x007F, 0x007D, 0x007A, 0x0078,
    0x0076, 0x0074, 0x0072, 0x0070, 0x006E, 0x006C, 0x006A, 0x0068, 0x0066, 0x0064, 0x0062, 0x0060,
    0x005E, 0x005C, 0x005B, 0x0059, 0x0057, 0x0055, 0x0053, 0x0052, 0x0050, 0x004E, 0x004D, 0x004B,
    0x004A, 0x0048, 0x0046, 0x0045, 0x0043, 0x0042, 0x0040, 0x003F, 0x003E, 0x003C, 0x003B, 0x0039,
    0x0038, 0x0037, 0x0035, 0x0034, 0x0033, 0x0031, 0x0030, 0x002F, 0x002E, 0x002D, 0x002B, 0x002A,
    0x0029, 0x0028, 0x0027, 0x0026, 0x0025, 0x0024, 0x0023, 0x0022, 0x0021, 0x0020, 0x001F, 0x001E,
    0x001D, 0x001C, 0x001B, 0x001A, 0x0019, 0x0018, 0x0017, 0x0017, 0x0016, 0x0015, 0x0014, 0x0014,
    0x0013, 0x0012, 0x0011, 0x0011, 0x0010, 0x000F, 0x000F, 0x000E, 0x000D, 0x000D, 0x000C, 0x000C,
    0x000B, 0x000A, 0x000A, 0x0009, 0x0009, 0x0008, 0x0008, 0x0007, 0x0007, 0x0007, 0x0006, 0x0006,
    0x0005, 0x0005, 0x0005, 0x0004, 0x0004, 0x0004, 0x0003, 0x0003, 0x0003, 0x0002, 0x0002, 0x0002,
    0x0002, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0000, 0x0000, 0x0000, 0x0000,
    0x0000, 0x0000, 0x0000, 0x0000,
];
// The published exponential power table: the low eight bits of the
// combined attenuation pick a ten-bit mantissa with the implied
// leading bit, pre-shifted left by two so one right shift per
// integer octave turns a 5.8 attenuation into a 13-bit linear
// volume, per the die-extracted ymfm form.
const POWER_TABLE: [u16; 256] = [
    0x1FE8, 0x1FD4, 0x1FBC, 0x1FA8, 0x1F90, 0x1F7C, 0x1F68, 0x1F50, 0x1F3C, 0x1F24, 0x1F10, 0x1EFC,
    0x1EE4, 0x1ED0, 0x1EB8, 0x1EA4, 0x1E90, 0x1E7C, 0x1E64, 0x1E50, 0x1E3C, 0x1E28, 0x1E10, 0x1DFC,
    0x1DE8, 0x1DD4, 0x1DC0, 0x1DA8, 0x1D94, 0x1D80, 0x1D6C, 0x1D58, 0x1D44, 0x1D30, 0x1D1C, 0x1D08,
    0x1CF4, 0x1CE0, 0x1CCC, 0x1CB8, 0x1CA4, 0x1C90, 0x1C7C, 0x1C68, 0x1C54, 0x1C40, 0x1C2C, 0x1C18,
    0x1C08, 0x1BF4, 0x1BE0, 0x1BCC, 0x1BB8, 0x1BA4, 0x1B94, 0x1B80, 0x1B6C, 0x1B58, 0x1B48, 0x1B34,
    0x1B20, 0x1B10, 0x1AFC, 0x1AE8, 0x1AD4, 0x1AC4, 0x1AB0, 0x1AA0, 0x1A8C, 0x1A78, 0x1A68, 0x1A54,
    0x1A44, 0x1A30, 0x1A20, 0x1A0C, 0x19FC, 0x19E8, 0x19D8, 0x19C4, 0x19B4, 0x19A0, 0x1990, 0x197C,
    0x196C, 0x195C, 0x1948, 0x1938, 0x1924, 0x1914, 0x1904, 0x18F0, 0x18E0, 0x18D0, 0x18C0, 0x18AC,
    0x189C, 0x188C, 0x1878, 0x1868, 0x1858, 0x1848, 0x1838, 0x1824, 0x1814, 0x1804, 0x17F4, 0x17E4,
    0x17D4, 0x17C0, 0x17B0, 0x17A0, 0x1790, 0x1780, 0x1770, 0x1760, 0x1750, 0x1740, 0x1730, 0x1720,
    0x1710, 0x1700, 0x16F0, 0x16E0, 0x16D0, 0x16C0, 0x16B0, 0x16A0, 0x1690, 0x1680, 0x1670, 0x1664,
    0x1654, 0x1644, 0x1634, 0x1624, 0x1614, 0x1604, 0x15F8, 0x15E8, 0x15D8, 0x15C8, 0x15BC, 0x15AC,
    0x159C, 0x158C, 0x1580, 0x1570, 0x1560, 0x1550, 0x1544, 0x1534, 0x1524, 0x1518, 0x1508, 0x14F8,
    0x14EC, 0x14DC, 0x14D0, 0x14C0, 0x14B0, 0x14A4, 0x1494, 0x1488, 0x1478, 0x146C, 0x145C, 0x1450,
    0x1440, 0x1430, 0x1424, 0x1418, 0x1408, 0x13FC, 0x13EC, 0x13E0, 0x13D0, 0x13C4, 0x13B4, 0x13A8,
    0x139C, 0x138C, 0x1380, 0x1370, 0x1364, 0x1358, 0x1348, 0x133C, 0x1330, 0x1320, 0x1314, 0x1308,
    0x12F8, 0x12EC, 0x12E0, 0x12D4, 0x12C4, 0x12B8, 0x12AC, 0x12A0, 0x1290, 0x1284, 0x1278, 0x126C,
    0x1260, 0x1250, 0x1244, 0x1238, 0x122C, 0x1220, 0x1214, 0x1208, 0x11F8, 0x11EC, 0x11E0, 0x11D4,
    0x11C8, 0x11BC, 0x11B0, 0x11A4, 0x1198, 0x118C, 0x1180, 0x1174, 0x1168, 0x115C, 0x1150, 0x1144,
    0x1138, 0x112C, 0x1120, 0x1114, 0x1108, 0x10FC, 0x10F0, 0x10E4, 0x10D8, 0x10CC, 0x10C0, 0x10B4,
    0x10A8, 0x10A0, 0x1094, 0x1088, 0x107C, 0x1070, 0x1064, 0x1058, 0x1050, 0x1044, 0x1038, 0x102C,
    0x1020, 0x1018, 0x100C, 0x1000,
];

/// The published operator-connection forms per algorithm: per row,
/// which previous output sum feeds operators 2, 3 and 4, and which of
/// operators 1, 2 and 3 enter the channel sum. Operator 4 always
/// enters the channel sum, per the eight-published-algorithm table.
///
/// | Alg | Form |
/// |-----|------|
/// | 0 | 1 → 2 → 3 → 4 |
/// | 1 | (1+2) → 3 → 4 |
/// | 2 | 1 → 2 → 3, into 4 |
/// | 3 | (1 → 2) and 3, into 4 |
/// | 4 | (1 → 2) and (3 → 4) |
/// | 5 | (1 → 2), (1 → 3), (1 → 4) |
/// | 6 | (1 → 2), 3, 4 |
/// | 7 | 1, 2, 3, 4 |
const ALG_OP2_IN: [usize; 8] = [1, 0, 0, 1, 1, 1, 1, 0];
const ALG_OP3_IN: [usize; 8] = [2, 5, 2, 0, 0, 1, 0, 0];
const ALG_OP4_IN: [usize; 8] = [3, 3, 6, 7, 3, 1, 0, 0];
const ALG_SUM_MASK: [u8; 8] = [0, 0, 0, 0, 0b010, 0b011, 0b011, 0b111];

/// The operator slot in register address order (bits 2-3 of the
/// per-operator row byte) mapped to the operator index of the
/// algorithm tables. The published address walk puts OP1 first, then
/// OP3, then OP2, then OP4, per the neogeodev FM table and the die
/// operator map.
const SLOT_WALK: [usize; 4] = [0, 2, 1, 3];

/// The ADPCM-A forecast table: the 49 published step sizes.
const ADPCMA_STEPS: [i32; 49] = [
    16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130,
    143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796,
    876, 963, 1060, 1166, 1282, 1411, 1552,
];

/// The ADPCM-A step-index adjustments per sample code, the published
/// form: the first four codes shrink the forecast, the rest grow it
/// in rank order.
const ADPCMA_STEP_INC: [i32; 8] = [-1, -1, -1, -1, 2, 5, 7, 9];

/// The ADPCM-B step scales per sample code in the x.64 form: 0.9,
/// 0.9, 0.9, 0.9, 1.2, 1.6, 2.0 and 2.4, per the published form.
const ADPCMB_STEP_SCALE: [u32; 8] = [57, 57, 57, 57, 77, 102, 128, 153];
const ADPCMB_STEP_MIN: i64 = 127;
const ADPCMB_STEP_MAX: i64 = 24_576;

/// The SSG amplitude table: the 32 per-level DAC outputs in the
/// MAME-derived form, biased so level zero reads silent, per the ymfm
/// SSG core.
const SSG_AMPLITUDES: [i32; 32] = [
    0, 32, 78, 141, 178, 222, 262, 306, 369, 441, 509, 585, 701, 836, 965, 1112, 1334, 1595, 1853,
    2146, 2576, 3081, 3576, 4135, 5000, 6006, 7023, 8155, 9963, 11_976, 14_132, 16_382,
];

/// The envelope states in the published walk order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EgState {
    /// The rise toward full from key on.
    Attack,
    /// The first steep fall to the sustain level.
    Decay,
    /// The long tail while the key holds.
    Sustain,
    /// The fall after key off.
    Release,
}

impl EgState {
    /// The state back from the snapshot form.
    fn from_code(code: u8) -> EgState {
        match code {
            0 => EgState::Attack,
            1 => EgState::Decay,
            2 => EgState::Sustain,
            _ => EgState::Release,
        }
    }

    /// The per-state index into the derived rate table.
    fn slot(self) -> usize {
        match self {
            EgState::Attack => 0,
            EgState::Decay => 1,
            EgState::Sustain => 2,
            EgState::Release => 3,
        }
    }
}

/// One FM operator: the register forms and the running state.
#[derive(Debug, Clone, Copy)]
struct Operator {
    /// The detune group: the three-bit detune over the four-bit
    /// multiple.
    dt_mul: u8,
    /// The seven-bit total level.
    tl: u8,
    /// The two-bit rate scaling over the five-bit attack rate.
    ksr_ar: u8,
    /// The amplitude-LFO enable over the five-bit first decay rate.
    am_d1r: u8,
    /// The five-bit secondary decay rate.
    d2r: u8,
    /// The four-bit decay level over the four-bit release rate.
    d1l_rr: u8,
    /// The four-bit SSG-EG form.
    ssg: u8,
    /// The 20-bit phase accumulator.
    phase: u32,
    /// The ten-bit envelope attenuation.
    env_att: u32,
    /// The envelope state.
    state: EgState,
    /// The key state.
    key: bool,
    /// The SSG-EG inversion state.
    ssg_inv: bool,
}

impl Operator {
    /// The power-up operator: released and silent.
    fn silent() -> Self {
        Self {
            dt_mul: 0,
            tl: 0,
            ksr_ar: 0,
            am_d1r: 0,
            d2r: 0,
            d1l_rr: 0,
            ssg: 0,
            phase: 0,
            env_att: EG_MAX,
            state: EgState::Release,
            key: false,
            ssg_inv: false,
        }
    }

    /// The multiple as an x.1 value per the published form: the
    /// register zero runs at half rate and the others count ones
    /// through fifteens.
    fn multiple(&self) -> u32 {
        let m = u32::from(self.dt_mul & 0x0F);
        if m == 0 { 1 } else { m * 2 }
    }

    /// The three-bit detune register.
    fn detune(&self) -> u8 {
        (self.dt_mul >> 4) & 0x07
    }

    /// The two-bit rate-scaling register.
    fn ksr(&self) -> u8 {
        (self.ksr_ar >> 6) & 0x03
    }

    /// The five-bit register rate per state, per the published
    /// groups; the release takes the published doubled form with the
    /// low bit always one.
    fn state_rate(&self, state: EgState) -> u8 {
        match state {
            EgState::Attack => self.ksr_ar & 0x1F,
            EgState::Decay => self.am_d1r & 0x1F,
            EgState::Sustain => self.d2r & 0x1F,
            EgState::Release => (self.d1l_rr & 0x0F) * 2 + 1,
        }
    }

    /// The sustain level in the ten-bit envelope form, per the
    /// published form: the four-bit register form where fifteen acts
    /// like thirty-one, scaled into the envelope attenuation.
    fn sustain_level(&self) -> u32 {
        let sl = u32::from((self.d1l_rr >> 4) & 0x0F);
        (sl | ((sl + 1) & 0x10)) << 5
    }

    /// The amplitude-LFO enable of this operator.
    fn am_enabled(&self) -> bool {
        self.am_d1r & 0x80 != 0
    }

    /// The SSG-EG enabled bit.
    fn ssg_enabled(&self) -> bool {
        self.ssg & 0x08 != 0
    }

    /// The three-bit SSG-EG type.
    fn ssg_type(&self) -> u8 {
        self.ssg & 0x07
    }
}

/// One FM channel: the register forms.
#[derive(Debug, Clone, Copy)]
struct Channel {
    /// The fourteen-bit frequency form: three bits of block over
    /// eleven bits of frequency number.
    block_freq: u16,
    /// The three-bit self-feedback level over the three-bit algorithm.
    fb_alg: u8,
    /// The left bit over the right bit over the amplitude-sensitivity
    /// field over the pitch-sensitivity field.
    out_ams_fms: u8,
}

impl Channel {
    /// The power-up channel form.
    fn silent() -> Self {
        Self {
            block_freq: 0,
            fb_alg: 0,
            out_ams_fms: 0,
        }
    }

    /// The three-bit self-feedback level.
    fn fb(&self) -> u8 {
        (self.fb_alg >> 3) & 0x07
    }

    /// The three-bit algorithm.
    fn alg(&self) -> u8 {
        self.fb_alg & 0x07
    }

    /// The left output enable.
    fn out_l(&self) -> bool {
        self.out_ams_fms & 0x80 != 0
    }

    /// The right output enable.
    fn out_r(&self) -> bool {
        self.out_ams_fms & 0x40 != 0
    }

    /// The two-bit amplitude-LFO sensitivity.
    fn ams(&self) -> u8 {
        (self.out_ams_fms >> 4) & 0x03
    }

    /// The three-bit pitch-LFO sensitivity.
    fn fms(&self) -> u8 {
        self.out_ams_fms & 0x07
    }
}

/// The per-operator derived values, recomputed once per FM sample
/// from the register forms and the channel frequency.
#[derive(Debug, Clone, Copy, Default)]
struct OpCache {
    /// The published key code: the five-bit form of the block and
    /// note bits.
    keycode: u32,
    /// The detune value in the frequency-number form, signed.
    detune: i32,
    /// The phase step per FM sample in the 20-bit form, the static
    /// form without the pitch LFO.
    step: u32,
    /// The x.1 multiple and the block shift, per the step form.
    multiple: u32,
    block: u32,
    /// The doubled frequency number and the seven top bits, per the
    /// pitch-LFO form.
    fnum12: u32,
    fnum_top7: u32,
    /// The pitch-LFO form: the activity and the per-channel
    /// sensitivity.
    pm_active: bool,
    pm_sens: u8,
    /// The key-scaled envelope rate per state.
    rates: [u32; 4],
    /// The sustain level in the ten-bit envelope form.
    sustain: u32,
    /// The total level in the ten-bit envelope form.
    total: u32,
}

/// One queued register write, applied at the next sample boundary.
/// The address rides the published nine-bit register form with the
/// bank in bit 8.
#[derive(Debug, Clone, Copy)]
struct QueuedWrite {
    addr: u16,
    data: u8,
}

/// The SSG: the YM2149 register block, the three tone generators and
/// the shared noise and envelope walkers, per the MAME SSG form the
/// YM2610 runs.
struct Ssg {
    /// The SSG register block; the removed I/O slots keep their
    /// stored form.
    regs: [u8; SSG_REG_FILE],
    /// The tone counters and the square states per channel.
    tone_count: [u32; SSG_CHANNELS],
    tone_state: [u8; SSG_CHANNELS],
    /// The noise period counter, its prescale toggle and the 17-bit
    /// LFSR with the seed-one form.
    noise_count: u32,
    noise_prescale: u8,
    rng: u32,
    /// The shared envelope walk: the period count, the signed step,
    /// the inversion flip, the shape's hold and alternate forms and
    /// the parked state.
    env_count: u32,
    env_step: i32,
    env_flip: u8,
    env_hold: bool,
    env_alt: bool,
    env_holding: bool,
}

impl Ssg {
    /// The power-up SSG: counters rest, the LFSR carries the
    /// published seed and nothing sounds.
    fn new() -> Self {
        Self {
            regs: [0; SSG_REG_FILE],
            tone_count: [0; SSG_CHANNELS],
            tone_state: [0; SSG_CHANNELS],
            noise_count: 0,
            noise_prescale: 0,
            rng: 1,
            env_count: 0,
            env_step: 0,
            env_flip: 0,
            env_hold: false,
            env_alt: false,
            env_holding: false,
        }
    }

    /// The twelve-bit tone period per channel: the fine byte over
    /// the four coarse bits, per the published resolution.
    fn tone_period(&self, ch: usize) -> u32 {
        u32::from(self.regs[2 * ch]) | (u32::from(self.regs[2 * ch + 1] & 0x0F) << 8)
    }

    /// The five-bit noise period, shared.
    fn noise_period(&self) -> u32 {
        u32::from(self.regs[6] & 0x1F)
    }

    /// The sixteen-bit envelope period, shared.
    fn envelope_period(&self) -> u32 {
        u32::from(self.regs[0x0B]) | (u32::from(self.regs[0x0C]) << 8)
    }

    /// The mixer-enable negatives: register `$07` carries one bit per
    /// tone and one per noise channel.
    fn tone_disabled(&self, ch: usize) -> bool {
        self.regs[7] >> ch & 0x01 != 0
    }

    fn noise_disabled(&self, ch: usize) -> bool {
        self.regs[7] >> (3 + ch) & 0x01 != 0
    }

    fn envelope_mode(&self, ch: usize) -> bool {
        self.regs[0x08 + ch] & 0x10 != 0
    }

    fn amplitude(&self, ch: usize) -> u8 {
        self.regs[0x08 + ch] & 0x0F
    }

    fn envelope_shape(&self) -> u8 {
        self.regs[0x0D] & 0x0F
    }

    /// Register a shape write per the published form: the walk
    /// restarts, the attack bit sets the rise form, and a
    /// continue-free shape maps to the ramp's own hold form.
    fn write_shape(&mut self) {
        let shape = self.envelope_shape();
        let rising = shape & 0x04 != 0;
        self.env_flip = if rising { 31 } else { 0 };
        if shape & 0x08 == 0 {
            // The continue-free forms run one ramp and hold where the
            // ramp ends; the alternate state carries the ramp's end.
            self.env_hold = true;
            self.env_alt = rising;
        } else {
            self.env_hold = shape & 0x01 != 0;
            self.env_alt = shape & 0x02 != 0;
        }
        self.env_step = 31;
        self.env_holding = false;
    }

    /// One SSG tick: the tone squares, the noise LFSR behind its
    /// prescale and the shared envelope walk, per the die forms.
    fn tick(&mut self) {
        for ch in 0..SSG_CHANNELS {
            self.tone_count[ch] += 1;
            // A period of zero lands as period one, per the published
            // minimum resolution.
            let period = self.tone_period(ch).max(1);
            if self.tone_count[ch] >= period {
                self.tone_state[ch] ^= 1;
                self.tone_count[ch] = 0;
            }
        }
        self.noise_count += 1;
        if self.noise_count >= self.noise_period() {
            // The LFSR shifts every second period tick behind the
            // prescale; a period of zero reaches every tick and lands
            // as period one, per the published form.
            self.noise_count = 0;
            self.noise_prescale ^= 1;
            if self.noise_prescale == 0 {
                let bit = (self.rng ^ (self.rng >> 3)) & 1;
                self.rng = (self.rng >> 1) | (bit << 17);
            }
        }
        if !self.env_holding {
            self.env_count += 1;
            if self.env_count >= self.envelope_period() {
                self.env_count = 0;
                self.env_step -= 1;
                if self.env_step < 0 {
                    if self.env_hold {
                        if self.env_alt {
                            self.env_flip ^= 31;
                        }
                        self.env_holding = true;
                        self.env_step = 0;
                    } else {
                        // The repeat walk flips the inversion each
                        // cycle when the shape alternates; the mask
                        // bit-5 check reads the wrap count, per the
                        // die form.
                        if self.env_alt && (self.env_step & 32) != 0 {
                            self.env_flip ^= 31;
                        }
                        self.env_step &= 31;
                    }
                }
            }
        }
    }

    /// The envelope volume of the shared walk: the down-counter of
    /// the step rides the inversion flip, per the die form.
    fn envelope_volume(&self) -> u32 {
        let step = if self.env_step < 0 {
            (self.env_step & 31) as u32
        } else {
            self.env_step as u32
        };
        step ^ (u32::from(self.env_flip) & 31)
    }

    /// The per-channel amplitude per the mixer gate and the level
    /// forms, per the die-verified SSG mixer: an enable-negative
    /// carries the constant pass-through and the zero level still
    /// reads one on the fixed form.
    fn amplitude_of(&self, ch: usize) -> i32 {
        let noise_pass = self.noise_disabled(ch) || self.rng & 1 == 1;
        let tone_pass = self.tone_disabled(ch) || self.tone_state[ch] != 0;
        if !(noise_pass && tone_pass) {
            return 0;
        }
        let volume = if self.envelope_mode(ch) {
            self.envelope_volume()
        } else {
            // The fixed level scales up to match the envelope rows:
            // amplitude 15 maps to the envelope's full form, per the
            // published note.
            let v = u32::from(self.amplitude(ch)) * 2;
            if v != 0 { v | 1 } else { 0 }
        };
        SSG_AMPLITUDES[(volume & 31) as usize]
    }

    /// The mono mixed form: the three channels sum and scale down to
    /// the published mono form, per the SSG mono mixing of the ymfm
    /// core.
    fn mono(&self) -> i32 {
        let sum = self.amplitude_of(0) + self.amplitude_of(1) + self.amplitude_of(2);
        sum * 2 / 9
    }
}

/// One ADPCM-A channel: the running state behind the shared register
/// block.
#[derive(Debug, Clone, Copy, Default)]
struct AdpcmACh {
    playing: bool,
    curnibble: u8,
    curbyte: u8,
    curaddress: u32,
    /// The 12-bit decode accumulator in its wrapped form.
    accum: u16,
    /// The forecast-table index, clamped between 0 and 48.
    step_index: i32,
}

/// The ADPCM-B channel: register block plus the running state of the
/// single variable-rate form.
struct AdpcmB {
    /// The internal register block for the external `$10-$1B` writes.
    regs: [u8; ADPCMB_REG_FILE],
    /// The internal status: the play and ready bits, with the end
    /// bit landing in the device-level status.
    status: u8,
    curnibble: u8,
    curbyte: u8,
    /// The sixteen-bit fraction position.
    position: u32,
    curaddress: u32,
    /// The 16-bit decode accumulator and its previous form.
    accum: i32,
    prev_accum: i32,
    /// The forecast step.
    step: i32,
}

impl AdpcmB {
    /// The power-up ADPCM-B: ready and silent, per the published
    /// reset form.
    fn new() -> Self {
        Self {
            regs: [0; ADPCMB_REG_FILE],
            status: 0,
            curnibble: 0,
            curbyte: 0,
            position: 0,
            curaddress: 0,
            accum: 0,
            prev_accum: 0,
            step: ADPCMB_STEP_MIN as i32,
        }
    }

    /// The start address in the 256-byte resolution, LSB over MSB.
    fn start(&self) -> u32 {
        u32::from(self.regs[2]) | u32::from(self.regs[3]) << 8
    }

    /// The end address in the 256-byte resolution, LSB over MSB.
    fn end(&self) -> u32 {
        u32::from(self.regs[4]) | u32::from(self.regs[5]) << 8
    }

    /// The sixteen-bit rate fraction, LSB over MSB.
    fn delta_n(&self) -> u32 {
        u32::from(self.regs[9]) | u32::from(self.regs[0x0A]) << 8
    }

    /// The eight-bit level.
    fn level(&self) -> i64 {
        i64::from(self.regs[0x0B])
    }

    /// The pan enables of control 2.
    fn pan_l(&self) -> bool {
        self.regs[1] & 0x80 != 0
    }

    fn pan_r(&self) -> bool {
        self.regs[1] & 0x40 != 0
    }

    /// The repeat bit of control 1.
    fn repeat(&self) -> bool {
        self.regs[0] & 0x10 != 0
    }

    /// Load the start address and reset the decode form: the
    /// sixteen-bit resolution register form shifts up to the ROM
    /// byte address with the chunk's low byte forced to zero, per
    /// the published start-address form.
    fn load_start(&mut self) {
        self.status = (self.status & !ADPCMB_EOS) | ADPCMB_PLAYING;
        self.curaddress = self.start() << 8;
        self.curnibble = 0;
        self.curbyte = 0;
        self.position = 0;
        self.accum = 0;
        self.prev_accum = 0;
        self.step = ADPCMB_STEP_MIN as i32;
    }

    /// Control 1 write per the published form: a set start bit loads
    /// and plays, a clear bit drops the end form, and the reset form
    /// resets the channel. The external-memory form stays forced on
    /// by the write path's published forcing.
    fn write_control_1(&mut self, data: u8, device_eos: &mut u8) {
        self.regs[0] = data;
        if data & 0x80 != 0 {
            self.load_start();
        } else {
            self.status &= !ADPCMB_EOS;
            *device_eos &= !0x80;
        }
        if data & 0x01 != 0 {
            self.status = 0;
            self.curaddress = 0;
            self.curnibble = 0;
            self.curbyte = 0;
            self.position = 0;
            self.accum = 0;
            self.prev_accum = 0;
            self.step = ADPCMB_STEP_MIN as i32;
        }
    }
}

/// The Yamaha YM2610 (OPNB) sound generator.
pub struct Ym2610 {
    name: String,
    /// The domain ticks per FM sample, from the `div` divider.
    div: u64,
    /// The external ADPCM sample ROM bytes, read from the named ROM
    /// devices' boot states at assembly time.
    rom_a: Vec<u8>,
    rom_b: Vec<u8>,
    /// The latched nine-bit register address with the bank in bit 8.
    address: u16,
    /// The register shadow per bank. The lower bank answers the SSG,
    /// ADPCM-B, common and channels 1-2 forms; the upper bank the
    /// ADPCM-A block and channels 3-4.
    regs: [[u8; REG_FILE]; 2],
    /// The SSG engine.
    ssg: Ssg,
    /// The ADPCM-A shared registers and its six channel forms.
    adpcma_regs: [u8; ADPCMA_REG_FILE],
    adpcma_ch: [AdpcmACh; ADPCMA_CHANNELS],
    /// The ADPCM-B engine.
    adpcmb: AdpcmB,
    /// The ADPCM end flags with their published mask.
    eos_status: u8,
    flag_mask: u8,
    /// The frequency latches per bank: the channel-pair latch of the
    /// `$A4` row family and the supplementary-pair latch of the
    /// `$AC` row family, per the published internal form.
    freq_latch: [[u8; 2]; 2],
    /// The supplementary pair values for the channel-2 special form,
    /// in the pair-number order of the published addresses.
    ch2_pairs: [u16; 3],
    /// The channel-2 special forms and the CSM key pulse.
    special: bool,
    csm: bool,
    csm_pulse: bool,
    /// The per-channel register forms.
    channels: [Channel; FM_CHANNELS],
    /// The per-operator running state.
    ops: [Operator; FM_CHANNELS * OPS],
    /// The per-operator derived cache.
    caches: [OpCache; FM_CHANNELS * OPS],
    /// The per-channel operator-1 feedback forms: the two previous
    /// outputs per the published feedback path.
    feedback: [[i32; 2]; FM_CHANNELS],
    /// Timer A: the ten-bit ganged register, the counter, the load,
    /// the enable and the overflow flag, per the published forms.
    timer_a_reg: u16,
    timer_a_cnt: u32,
    timer_a_load: bool,
    timer_a_enable: bool,
    timer_a_flag: bool,
    /// Timer B: the eight-bit register, the counter, the
    /// sixteen-sample divider, the load, the enable and the flag.
    timer_b_reg: u8,
    timer_b_cnt: u32,
    timer_b_sub: u32,
    timer_b_load: bool,
    timer_b_enable: bool,
    timer_b_flag: bool,
    /// The global LFO: enable, rate, the divider counter and the
    /// triangle phase, and the derived per-sample forms.
    lfo_enable: bool,
    lfo_rate: u8,
    lfo_sub: u32,
    lfo_phase: u32,
    lfo_am: u32,
    lfo_pm: i32,
    /// The envelope counter in the published x.2 form: the per-sample
    /// sub-counter over the unit count.
    env_counter: u32,
    /// The SSG clock phase in chip clocks, for the exact 250 kHz walk
    /// against the 144-chip-clock FM sample.
    ssg_clocks: u32,
    /// The busy form window in chip clocks.
    busy_cycles: u32,
    /// The currently driven `irq` level, tracked to notify on edges.
    irq_on: bool,
    /// Register writes queued to the next FM-sample boundary.
    pending: Vec<QueuedWrite>,
    /// The mixed mono samples.
    samples: Vec<i16>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

/// The snapshot chunk length: tag and version, the two register
/// files, the address latch, the frequency latches, the supplementary
/// pairs, the run flags, the FM operator records, the FM channel
/// forms, the timers, the LFO counters, the envelope counter, the SSG
/// engine, the ADPCM-A engine and the ADPCM-B engine.
const OPN_SNAPSHOT_LEN: usize = 5
    + REG_FILE * 2 // the register shadow per bank
    + 2 // the address latch
    + 4 // the frequency latches, two per bank
    + 6 // the supplementary pairs
    + 2 // the run-flag bytes
    + (FM_CHANNELS * OPS) * 15 // the per-operator records
    + FM_CHANNELS * 4 // the per-channel forms
    + 15 // the timer registers and counters
    + 9 // the LFO rate and counters
    + 4 // the envelope counter
    + 1 // the SSG walk-flags byte
    + SSG_REG_FILE
    + SSG_CHANNELS * 3 // the SSG tone counters and squares
    + 1 // the SSG noise count
    + 4 // the SSG noise LFSR
    + 4 // the SSG envelope count
    + 2 // the SSG envelope step and flip
    + ADPCMA_REG_FILE
    + ADPCMA_CHANNELS * 13 // the ADPCM-A channel records
    + ADPCMB_REG_FILE
    + 3 // the ADPCM-B status and nibble walk
    + 5 * 4 // the ADPCM-B position, address and decode forms
    + 2 // the end flags and mask
    + 4; // the busy window

impl Ym2610 {
    /// A chip from its configuration entry.
    ///
    /// The `div` divider names the domain ticks per FM sample and
    /// defaults to 144, the published sample period at the 8 MHz chip
    /// clock. The `adpcma` and `adpcmb` parameters name the two
    /// external sample ROM devices; their boot states ride the
    /// configuration and land at this entry point: a `Hex` boot state
    /// clones its bytes and a `Rom` boot state reads the file path,
    /// missing files keeping an empty image (the established
    /// pattern).
    pub fn new(device: &DeviceInstance, config: &SystemConfig) -> Result<Self, String> {
        let param = |key: &str| -> Option<&str> { device.params.get(key).and_then(|v| v.as_str()) };
        let adpcma_device = param("adpcma").unwrap_or("adpcma");
        let adpcmb_device = param("adpcmb").unwrap_or("adpcmb");
        let div = u64::from(device.dividers.get("div").copied().unwrap_or(DEFAULT_DIV)).max(1);
        Ok(Self {
            name: device.name.to_string(),
            div,
            rom_a: boot_image_of(config, adpcma_device),
            rom_b: boot_image_of(config, adpcmb_device),
            address: 0,
            regs: [[0; REG_FILE]; 2],
            ssg: Ssg::new(),
            adpcma_regs: [0; ADPCMA_REG_FILE],
            adpcma_ch: [AdpcmACh::default(); ADPCMA_CHANNELS],
            adpcmb: AdpcmB::new(),
            eos_status: 0,
            flag_mask: EOS_FLAG_MASK,
            freq_latch: [[0; 2]; 2],
            ch2_pairs: [0; 3],
            special: false,
            csm: false,
            csm_pulse: false,
            channels: [Channel::silent(); FM_CHANNELS],
            ops: [Operator::silent(); FM_CHANNELS * OPS],
            caches: [OpCache::default(); FM_CHANNELS * OPS],
            feedback: [[0; 2]; FM_CHANNELS],
            timer_a_reg: 0,
            timer_a_cnt: 0,
            timer_a_load: false,
            timer_a_enable: false,
            timer_a_flag: false,
            timer_b_reg: 0,
            timer_b_cnt: 0,
            timer_b_sub: 0,
            timer_b_load: false,
            timer_b_enable: false,
            timer_b_flag: false,
            lfo_enable: false,
            lfo_rate: 0,
            lfo_sub: 0,
            lfo_phase: 0,
            lfo_am: 0,
            lfo_pm: 0,
            env_counter: 0,
            ssg_clocks: 0,
            busy_cycles: 0,
            irq_on: false,
            pending: Vec::new(),
            samples: Vec::new(),
            next: None,
            last_tick: None,
        })
    }

    /// The register port within the sound window: the low two
    /// address bits per the wired window, as the published port
    /// split wires the chip.
    fn port_of(&self, address: Address) -> Option<u8> {
        Some((address.0 & 0x03) as u8)
    }

    /// Status 0: bit 7 the busy form, bit 1 Timer B and bit 0 Timer
    /// A, per the application manual's Status 0 table.
    fn status0(&self) -> u8 {
        (u8::from(self.busy_cycles > 0) << 7)
            | (u8::from(self.timer_b_flag) << 1)
            | u8::from(self.timer_a_flag)
    }

    /// Status 1: the ADPCM end flags masked through the flag-control
    /// register, per the published form.
    fn status1(&self) -> u8 {
        self.eos_status & self.flag_mask
    }

    /// The read-back form of one port per the published read
    /// surface: status 0 on the lower address port, the SSG
    /// register or the identification code on the lower data port,
    /// status 1 on the upper address port and zero on the upper
    /// data port.
    fn observe(&self, port: u8) -> u8 {
        match port {
            0 => self.status0(),
            1 => {
                if self.address < 0x0E {
                    self.ssg.regs[usize::from(self.address)]
                } else if self.address == 0xFF {
                    1
                } else {
                    0
                }
            }
            2 => self.status1(),
            _ => 0,
        }
    }

    /// One port write byte: the address ports latch (the lower write
    /// clears the bank form, the upper sets it, per the published
    /// address register form) and the data ports queue the store; a
    /// data write whose bank disagrees with the latched address-bit
    /// is ignored, per the published pairing caution. A data write
    /// raises the busy form for its published window.
    fn write_byte(&mut self, port: u8, byte: u8) {
        let bank = u16::from(port & 0x02 != 0) << 8;
        match port & 0x03 {
            0 => self.address = u16::from(byte),
            2 => self.address = 0x100 | u16::from(byte),
            _ => {
                if self.address >> 8 == bank >> 8 {
                    self.busy_cycles = BUSY_DATA_CYCLES;
                    self.pending.push(QueuedWrite {
                        addr: self.address,
                        data: byte,
                    });
                }
            }
        }
    }
}

/// The boot image of one named ROM device: a `Hex` boot state clones
/// its bytes, a `Rom` boot state reads the file path, missing files
/// and unknown devices keep an empty image (the established pattern).
fn boot_image_of(config: &SystemConfig, device: &str) -> Vec<u8> {
    let Some(slot) = config
        .devices_by_name
        .iter()
        .find_map(|(name, slot)| (name.as_str() == device).then_some(*slot))
    else {
        return Vec::new();
    };
    let Some(instance) = config.devices.get(slot) else {
        return Vec::new();
    };
    match config.boot.get(&instance.name) {
        Some(BootState::Hex { bytes }) => bytes.clone(),
        Some(BootState::Rom { path }) => std::fs::read(path).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The register stores: the drain, the published surface decode and
/// the per-side-effect forms.
impl Ym2610 {
    /// The store per the published register surface. The address
    /// carries the bank in bit 8; the lower bank routes the SSG, the
    /// ADPCM-B, the flag control and the FM forms, and the upper one
    /// the ADPCM-A block and the FM of channels 3-4.
    fn store(&mut self, addr: u16, data: u8) {
        let bank = usize::from(addr >> 8);
        let low = usize::from(addr & 0xFF);
        if bank == 0 {
            match low {
                0..=0x0D => {
                    self.ssg.regs[low] = data;
                    if low == 0x0D {
                        self.ssg.write_shape();
                    }
                }
                0x10..=0x1B => {
                    if low == 0x10 {
                        // The chip forces the external-memory form on
                        // and recording off, per the published form.
                        let forced = (data | 0x20) & !0x40;
                        self.adpcmb.write_control_1(forced, &mut self.eos_status);
                    } else {
                        self.adpcmb.regs[low - 0x10] = data;
                    }
                    self.regs[0][low] = data;
                }
                0x1C => {
                    // The flag control: write-one resets and masks,
                    // write-zero unmasks, per the published form.
                    self.flag_mask = !data;
                    self.eos_status &= !data;
                }
                _ => self.store_fm(addr, data),
            }
        } else if low < 0x30 {
            self.adpcma_regs[low] = data;
            self.regs[1][low] = data;
            if low == 0 {
                self.write_adpcma_control(data);
            }
        } else {
            self.store_fm(addr, data);
        }
    }

    /// The FM store per the published register surface.
    fn store_fm(&mut self, addr: u16, data: u8) {
        let bank = usize::from(addr >> 8);
        self.regs[bank][usize::from(addr & 0xFF)] = data;
        match addr & 0xFF {
            0x22 => {
                self.lfo_enable = data & 0x08 != 0;
                self.lfo_rate = data & 0x07;
            }
            0x24 => {
                self.timer_a_reg = (u16::from(data) << 2) | (self.timer_a_reg & 0x3);
            }
            0x25 => {
                self.timer_a_reg = (self.timer_a_reg & 0x3FC) | u16::from(data & 0x3);
            }
            0x26 => self.timer_b_reg = data,
            0x27 => self.write_mode(data),
            0x28 => self.write_key_on(data),
            0x30..=0x9F => self.store_op(addr, data),
            0xA0..=0xAF => self.store_freq(addr, data),
            0xB0..=0xB7 => self.store_channel(addr, data),
            _ => {}
        }
    }

    /// The per-operator store per the published addressing: the row
    /// base selects the register, bits two-three the operator (in
    /// the published walk) and bit zero the channel within the pair;
    /// the unused positions land nowhere per the published layout.
    fn store_op(&mut self, addr: u16, data: u8) {
        let low = (addr & 0xFF) as u8;
        let parity = low & 0x03;
        if parity != 1 && parity != 2 {
            return;
        }
        let bank = usize::from(addr >> 8);
        let ch = bank * 2 + usize::from(parity == 2);
        let index = ch * OPS + SLOT_WALK[usize::from((low >> 2) & 0x03)];
        match low >> 4 {
            0x3 => self.ops[index].dt_mul = data & 0x7F,
            0x4 => self.ops[index].tl = data & 0x7F,
            0x5 => self.ops[index].ksr_ar = data,
            0x6 => self.ops[index].am_d1r = data,
            0x7 => self.ops[index].d2r = data & 0x1F,
            0x8 => self.ops[index].d1l_rr = data,
            _ => self.ops[index].ssg = data & 0x0F,
        }
    }

    /// The mode store per the published bit form: the channel-2
    /// special forms (multi frequency on any nonzero setting and CSM
    /// on the published top-bit form), the enables, the write-one
    /// flag resets, and the load edges that start each timer from
    /// its register value. Load zero stops the timer without
    /// resetting it.
    fn write_mode(&mut self, data: u8) {
        let load_a = data & 0x01 != 0;
        let load_b = data & 0x02 != 0;
        self.timer_a_enable = data & 0x04 != 0;
        self.timer_b_enable = data & 0x08 != 0;
        if data & 0x10 != 0 {
            self.timer_a_flag = false;
        }
        if data & 0x20 != 0 {
            self.timer_b_flag = false;
        }
        if load_a && !self.timer_a_load {
            self.timer_a_cnt = u32::from(self.timer_a_reg);
        }
        if load_b && !self.timer_b_load {
            self.timer_b_cnt = u32::from(self.timer_b_reg);
            self.timer_b_sub = 0;
        }
        self.timer_a_load = load_a;
        self.timer_b_load = load_b;
        self.special = data & 0xC0 != 0;
        self.csm = data & 0xC0 == 0x80;
    }

    /// The key-on/off store per the published operator and channel
    /// forms: bits 2-0 walk the published channel codes (001 = 1,
    /// 010 = 2, 101 = 3, 110 = 4, with the removed positions of the
    /// chip landing nothing) and bits 7-4 the four operators from
    /// bit 7 = OP4 down to bit 4 = OP1.
    fn write_key_on(&mut self, data: u8) {
        let opbits = data >> 4;
        let ch = match usize::from(data & 0x07) {
            1 => 0,
            2 => 1,
            5 => 2,
            6 => 3,
            _ => return,
        };
        for op in 0..OPS {
            if opbits >> op & 1 != 0 {
                self.key_on(ch * OPS + op);
            } else {
                self.key_off(ch * OPS + op);
            }
        }
    }

    /// Key on per the published form: the phase restarts and the
    /// attack starts from the current envelope; the published
    /// instant form (the key-scaled attack rate at or past 62) lands
    /// at full level now.
    fn key_on(&mut self, op_index: usize) {
        let instant = self.caches[op_index].rates[0] >= 62;
        let op = &mut self.ops[op_index];
        op.key = true;
        op.state = EgState::Attack;
        op.phase = 0;
        if instant {
            op.env_att = 0;
        }
    }

    /// Key off per the published form: the release starts at the
    /// current envelope form.
    fn key_off(&mut self, op_index: usize) {
        let op = &mut self.ops[op_index];
        op.key = false;
        op.state = EgState::Release;
    }

    /// The frequency-pair stores per the published latch form: the
    /// high stores its six bits into the holding latch of its row
    /// family per bank, and the low stores both its byte and the
    /// held latch. The channel-pair rows address the channels in
    /// their low bits (the first of the pair on the odd forms) and
    /// the supplementary rows carry the pair number; the gap and the
    /// removed-channel forms land nowhere.
    fn store_freq(&mut self, addr: u16, data: u8) {
        let low = (addr & 0xFF) as u8;
        let bank = usize::from(addr >> 8);
        let family = usize::from(low & 0x08 != 0);
        let slot = usize::from(low & 0x03);
        if slot > 2 {
            return;
        }
        if low & 0x04 != 0 {
            self.freq_latch[bank][family] = data & 0x3F;
        } else if family == 0 {
            // The pair rows: slot 1 is the first channel of the bank,
            // slot 2 the second; the removed slot-0 form lands
            // nowhere on the plain chip.
            if slot == 0 {
                return;
            }
            let ch = bank * 2 + slot - 1;
            self.channels[ch].block_freq =
                (u16::from(self.freq_latch[bank][0]) << 8) | u16::from(data);
        } else if bank == 0 {
            // The supplementary pairs live in the lower bank.
            self.ch2_pairs[slot] = (u16::from(self.freq_latch[0][1]) << 8) | u16::from(data);
        }
    }

    /// The per-channel stores for the feedback/algorithm row and the
    /// output-sensitivity row; the pair positions walk the channels
    /// and the unused forms land nowhere.
    fn store_channel(&mut self, addr: u16, data: u8) {
        let low = (addr & 0xFF) as u8;
        let parity = low & 0x03;
        if parity != 1 && parity != 2 {
            return;
        }
        let bank = usize::from(addr >> 8);
        let ch = bank * 2 + usize::from(parity == 2);
        if low & 0x07 < 4 {
            self.channels[ch].fb_alg = data & 0x3F;
        } else {
            self.channels[ch].out_ams_fms = data;
        }
    }

    /// The ADPCM-A control store per the published dump form: the
    /// start bit zero plays the selected channels and a one stops
    /// them, with repeated key ons restarting the sample.
    fn write_adpcma_control(&mut self, data: u8) {
        for ch in 0..ADPCMA_CHANNELS {
            if data >> ch & 1 != 0 {
                let on = data & 0x80 == 0;
                self.adpcma_keyonoff(ch, on);
            }
        }
    }

    /// Key on one ADPCM-A channel: the address loads from the start
    /// register pair and the decode form resets, per the published
    /// key-on form.
    fn adpcma_keyonoff(&mut self, ch: usize, on: bool) {
        let start =
            u32::from(self.adpcma_regs[ch + 0x10]) | (u32::from(self.adpcma_regs[ch + 0x18]) << 8);
        let chrec = &mut self.adpcma_ch[ch];
        chrec.playing = on;
        if on {
            chrec.curaddress = start << 8;
            chrec.curnibble = 0;
            chrec.curbyte = 0;
            chrec.accum = 0;
            chrec.step_index = 0;
        }
    }

    /// Recompute the per-operator caches from the registers, per the
    /// published frequency and rate arithmetic. In the channel-2
    /// special form operators 1 to 3 read the published
    /// supplementary pairs and operator 4 keeps the channel pair,
    /// per the die-extracted form.
    fn recache(&mut self) {
        for (ch, chform) in self.channels.iter().enumerate() {
            let base = chform.block_freq;
            let use_special = ch == 1 && self.special;
            let pm = if self.lfo_enable && chform.fms() != 0 {
                (true, chform.fms())
            } else {
                (false, 0)
            };
            for (op, num) in (0..OPS).zip([base; OPS]) {
                let fnum = if op > 0 && use_special {
                    match op {
                        1 => self.ch2_pairs[2],
                        2 => self.ch2_pairs[0],
                        _ => self.ch2_pairs[1],
                    }
                } else {
                    num
                };
                let op_index = ch * OPS + op;
                self.caches[op_index] = Self::op_cache_of(&self.ops[op_index], fnum, pm);
            }
        }
    }

    /// One per-operator cache per the published form: the keycode,
    /// the detune, the frequency step and the key-scaled envelope
    /// rates, plus the sustain and total levels in the ten-bit form.
    fn op_cache_of(op: &Operator, fnum: u16, pm: (bool, u8)) -> OpCache {
        let mut out = OpCache::default();
        // The published five-bit key code: the block-and-note bits
        // over the fourteen-bit form, with the note bit read from
        // the published constant.
        let code = u32::from(fnum);
        let mut keycode = ((code >> 10) & 0x0F) << 1;
        keycode |= u32::from((FNUM_NOTE_BITS >> ((code >> 7) & 0x0F)) & 1);
        out.keycode = keycode;
        // The detune per the published table; the register's third
        // bit flips the sign.
        let dt = op.detune();
        let size = DETUNE[keycode as usize][usize::from(dt & 0x03)];
        out.detune = if dt >= 4 {
            -i32::from(size)
        } else {
            i32::from(size)
        };
        // The published key-code rate scaling: the five-bit key code
        // divided down per the rate-scaling form, then two times the
        // register rate plus that, clamped at 63. The register rate
        // of zero keeps rate zero.
        let ksv = keycode >> u32::from(op.ksr() ^ 3);
        for (slot, state) in [
            (0, EgState::Attack),
            (1, EgState::Decay),
            (2, EgState::Sustain),
            (3, EgState::Release),
        ] {
            let raw = u32::from(op.state_rate(state));
            out.rates[slot] = if raw == 0 { 0 } else { (raw * 2 + ksv).min(63) };
        }
        out.sustain = op.sustain_level();
        out.total = u32::from(op.tl & 0x7F) << 3;
        // The frequency step fields, per the die-verified form: the
        // frequency number doubled, shifted by the block, down two,
        // plus the detune, clamped to seventeen bits, then the x.1
        // multiple. The pitch LFO bends the number through the
        // published shift-pair table when the channel sensitivity
        // allows, so the active form re-derives per sample.
        out.multiple = op.multiple();
        out.block = (code >> 11) & 0x07;
        out.fnum12 = (code & 0x7FF) << 1;
        out.fnum_top7 = (code >> 4) & 0x7F;
        out.pm_active = pm.0;
        out.pm_sens = pm.1;
        out.step = frequency_step_of(out.fnum12, out.block, out.detune, out.multiple);
        out
    }
}

/// The frequency step per the die-verified form: the frequency number
/// doubled, shifted by the block, down two, plus the detune, clamped
/// at seventeen bits, then times the x.1 multiple.
fn frequency_step_of(fnum12: u32, block: u32, detune: i32, multiple: u32) -> u32 {
    let mut step = (fnum12 << block) >> 2;
    step = (step.wrapping_add_signed(detune)) & 0x1_FFFF;
    (step * multiple) >> 1
}

/// The pitch-LFO bent frequency step: the bend rides the top seven
/// frequency-number bits per the published shift-pair table and
/// folds into the same step arithmetic.
fn pm_step_of(
    fnum12: u32,
    block: u32,
    detune: i32,
    multiple: u32,
    raw_pm: i32,
    fnum_top7: u32,
    sens: u8,
) -> u32 {
    let raw = i64::from(raw_pm);
    let size = (raw.unsigned_abs() & 0x07) as usize;
    let shifts = LFO_PM_SHIFTS[usize::from(sens)][size];
    let mut bend: i32 =
        ((fnum_top7 >> u32::from(shifts & 0x0F)) + (fnum_top7 >> u32::from(shifts >> 4))) as i32;
    if sens > 5 {
        bend <<= u32::from(sens - 5);
    }
    bend >>= 2;
    let bend = if raw < 0 { -bend } else { bend };
    let bent = fnum12.wrapping_add_signed(bend) & 0xFFF;
    frequency_step_of(bent, block, detune, multiple)
}

/// The per-sample DSP: the LFO, the envelope and phase stepping, the
/// per-operator volume, the per-channel algorithm mix, the timers
/// and the output.
impl Ym2610 {
    /// The per-sample LFO advance per the published divider table;
    /// the LFO counters rest at zero while disabled and the rest
    /// position adds no modulation per the published form.
    fn run_lfo(&mut self) {
        if !self.lfo_enable {
            self.lfo_sub = 0;
            self.lfo_phase = 0;
            self.lfo_am = 0;
            self.lfo_pm = 0;
            return;
        }
        self.lfo_sub += 1;
        if self.lfo_sub >= LFO_DIVIDERS[usize::from(self.lfo_rate)] {
            self.lfo_sub = 0;
            self.lfo_phase = (self.lfo_phase + 1) & LFO_PHASE;
        }
        // The triangle: the amplitude reads the low six counts with
        // the first half inverted, and the pitch reads the three-bit
        // reflected form negated over the cycle halves, per the
        // die-verified counter forms.
        let phase = self.lfo_phase;
        let mut am = (phase & 0x3F) << 1;
        if phase & 0x40 == 0 {
            am ^= 0x7E;
        }
        self.lfo_am = am >> 1;
        let mut pm = i64::from((phase >> 2) & 0x07) as i32;
        if (phase >> 5) & 0x01 != 0 {
            pm ^= 7;
        }
        self.lfo_pm = if (phase >> 6) & 0x01 != 0 { -pm } else { pm };
    }

    /// The LFO amplitude offset per channel, per the published
    /// doubling shift over the per-channel sensitivity: the shift
    /// form picks 7, 3, 1 and 0 per the sensitivity forms.
    fn am_offset_of(&self, ch: usize) -> u32 {
        let am_shift = (1u32 << (self.channels[ch].ams() ^ 3)) - 1;
        (self.lfo_am << 1) >> am_shift
    }

    /// One envelope step for one operator, per the published state
    /// machine and the die-verified rate table.
    fn clock_envelope(&mut self, op_index: usize, unit: u32) {
        // The attack hands over to the first decay at full level, and
        // the first decay hands to the long decay at the sustain
        // level, immediately, per the published form.
        if self.ops[op_index].state == EgState::Attack && self.ops[op_index].env_att == 0 {
            self.ops[op_index].state = EgState::Decay;
        }
        if self.ops[op_index].state == EgState::Decay
            && self.ops[op_index].env_att >= self.caches[op_index].sustain
        {
            self.ops[op_index].state = EgState::Sustain;
        }
        let state = self.ops[op_index].state;
        let rate = self.caches[op_index].rates[state.slot()];
        if rate < 4 {
            // The published rate forms below four never step.
            return;
        }
        // The per-rate cadence per the die form: the envelope counter
        // shifted into the 5.11 form steps when the low eleven bits
        // clear; the same check lands as a modulo of the unit count.
        let rate_shift = rate >> 2;
        let step_due = if rate_shift >= 11 {
            true
        } else {
            unit.trailing_zeros() >= 11 - rate_shift
        };
        if !step_due {
            return;
        }
        // The per-step increment from the published rate table row,
        // cycling through the eight phases of the step group.
        let step_number = if rate_shift >= 11 {
            unit & 0x07
        } else {
            unit >> (11 - rate_shift) & 0x07
        };
        let increment = (ADSR_INCREMENTS[rate as usize] >> (step_number * 4)) & 0xF;
        if increment == 0 {
            return;
        }
        let op = &self.ops[op_index];
        if op.state == EgState::Attack {
            // The exponential attack: the gap toward the top shrinks
            // by the increment fraction, per the chip form. The
            // published instant form (rates 62 and 63) is handled at
            // key on.
            if rate < 62 {
                let att = i64::from(op.env_att);
                let next = att + (((!att) * i64::from(increment)) >> 4);
                self.ops[op_index].env_att = next.max(0) as u32;
            }
            return;
        }
        // The decay forms climb the attenuation; the SSG-EG form
        // under the mid form runs at four times the speed per the
        // die form, and everything clamps at the ceiling.
        let att = i64::from(op.env_att);
        let next = if op.ssg_enabled() {
            if att >= i64::from(EG_MID) {
                return;
            }
            att + (i64::from(increment) << 2)
        } else {
            att + i64::from(increment)
        };
        self.ops[op_index].env_att = next.max(0).min(i64::from(EG_MAX)) as u32;
    }

    /// The SSG-EG cycle per the die-verified behavior: the work
    /// happens when the envelope passes the mid form; the hold types
    /// stop there and the repeat types restart the attack, with the
    /// phase-reset types also restarting the accumulator.
    fn clock_ssg(&mut self, op_index: usize) {
        if self.ops[op_index].env_att & EG_MID == 0 {
            return;
        }
        let mode = self.ops[op_index].ssg_type();
        if mode & 0x01 != 0 {
            // The hold forms: the inversion lands per the published
            // end form.
            let inverted = (mode & 0x04 != 0) ^ (mode & 0x02 != 0);
            self.ops[op_index].ssg_inv = inverted;
            if self.ops[op_index].state != EgState::Attack {
                self.ops[op_index].env_att = if inverted { EG_MID } else { EG_MAX };
            }
        } else {
            // The repeat forms: the alternating forms flip the
            // inversion each cycle, the decay forms restart the
            // attack at the current envelope, and the phase-reset
            // forms clear the phase on every restart.
            if mode & 0x02 != 0 {
                let op = &mut self.ops[op_index];
                op.ssg_inv = !op.ssg_inv;
            }
            if matches!(self.ops[op_index].state, EgState::Decay | EgState::Sustain) {
                // The restart keeps the envelope where it crossed per
                // the die form; the attack form then re-descends.
                self.ops[op_index].state = EgState::Attack;
            }
            if mode & 0x02 == 0 {
                self.ops[op_index].phase = 0;
            }
        }
        if self.ops[op_index].state == EgState::Release {
            self.ops[op_index].env_att = EG_MAX;
        }
    }

    /// One per-operator visit: the SSG-EG clock, the envelope step
    /// on the unit boundary, and the phase step, per the chip's
    /// per-operator pipeline.
    fn clock_op(&mut self, op_index: usize) {
        if self.ops[op_index].ssg_enabled() {
            self.clock_ssg(op_index);
        } else {
            self.ops[op_index].ssg_inv = false;
        }
        if self.env_counter & 3 == 0 {
            let unit = self.env_counter >> 2;
            self.clock_envelope(op_index, unit);
        }
        let cache = self.caches[op_index];
        let step = if cache.pm_active {
            pm_step_of(
                cache.fnum12,
                cache.block,
                cache.detune,
                cache.multiple,
                self.lfo_pm,
                cache.fnum_top7,
                cache.pm_sens,
            )
        } else {
            cache.step
        };
        let op = &mut self.ops[op_index];
        op.phase = (op.phase.wrapping_add(step)) & (PHASE_LIMIT - 1);
    }

    /// The timers step per FM sample per the published forms: Timer A
    /// counts one step per sample, Timer B per sixteen samples; both
    /// count up while loaded, set their flag at the top when
    /// enabled, and reload from the register value.
    fn step_timers(&mut self) {
        if self.timer_a_load {
            self.timer_a_cnt += 1;
            if self.timer_a_cnt & TIMER_A_LIMIT != 0 {
                if self.timer_a_enable {
                    self.timer_a_flag = true;
                }
                self.timer_a_cnt = u32::from(self.timer_a_reg);
                // The CSM form keys channel 2 on the expiry itself per
                // the published form: the key lands live for one
                // sample, then the release form takes over.
                if self.csm {
                    for op in 0..OPS {
                        self.key_on(OPS + op);
                    }
                    self.csm_pulse = true;
                }
            }
        }
        if self.timer_b_load {
            self.timer_b_sub += 1;
            if self.timer_b_sub >= TIMER_B_SAMPLER {
                self.timer_b_sub = 0;
                self.timer_b_cnt += 1;
                if self.timer_b_cnt & TIMER_B_LIMIT != 0 {
                    if self.timer_b_enable {
                        self.timer_b_flag = true;
                    }
                    self.timer_b_cnt = u32::from(self.timer_b_reg);
                }
            }
        }
    }

    /// The `irq` line follows the enabled overflow flags per the
    /// published level form; the notify moves only on real edges.
    fn refresh_irq(&mut self, ctx: &mut MachineCtx, tick: Tick) {
        let active = (self.timer_a_flag && self.timer_a_enable)
            || (self.timer_b_flag && self.timer_b_enable);
        if active == self.irq_on {
            return;
        }
        self.irq_on = active;
        let level = if active { Level::High } else { Level::Low };
        ctx.notify("irq", level, tick);
    }

    /// Re-derive the per-sample LFO forms from the restored triangle
    /// phase; the forms are pure functions of the counter.
    fn rederive_lfo(&mut self) {
        let phase = self.lfo_phase;
        let mut am = (phase & 0x3F) << 1;
        if phase & 0x40 == 0 {
            am ^= 0x7E;
        }
        self.lfo_am = am >> 1;
        let mut pm = ((phase >> 2) & 0x07) as i32;
        if (phase >> 5) & 0x01 != 0 {
            pm ^= 7;
        }
        self.lfo_pm = if (phase >> 6) & 0x01 != 0 { -pm } else { pm };
    }
}

/// The external ROM lookups wrap inside the image, the recorded house
/// form; an empty image answers zero.
impl Ym2610 {
    fn rom_a_byte(&self, address: u32) -> u8 {
        if self.rom_a.is_empty() {
            return 0;
        }
        self.rom_a[(address as usize) % self.rom_a.len()]
    }

    fn rom_b_byte(&self, address: u32) -> u8 {
        if self.rom_b.is_empty() {
            return 0;
        }
        self.rom_b[(address as usize) % self.rom_b.len()]
    }
}

/// The ADPCM-A engine: one channel visit decodes one nibble and the
/// output scales through the combined levels, per the decode law of
/// the ymfm core.
impl Ym2610 {
    /// Clock one ADPCM-A channel one nibble; returns true when the
    /// channel just reached its end address and its flag should set,
    /// per the channel end form on the twenty-bit window.
    fn clock_adpcma(&mut self, ch: usize) -> bool {
        if !self.adpcma_ch[ch].playing {
            self.adpcma_ch[ch].accum = 0;
            return false;
        }
        if self.adpcma_ch[ch].curnibble == 0 {
            // Stop when the fetch lands on the chunk just past the
            // end pair; the end is inclusive on this chip, per the
            // die form on the twenty-bit window.
            let end_reg = u32::from(self.adpcma_regs[ch + 0x20])
                | (u32::from(self.adpcma_regs[ch + 0x28]) << 8);
            let end = (end_reg + 1) << 8;
            let cur = self.adpcma_ch[ch].curaddress;
            if ((cur ^ end) & 0xF_FFFF) == 0 {
                let chrec = &mut self.adpcma_ch[ch];
                chrec.playing = false;
                chrec.accum = 0;
                return true;
            }
            let byte = self.rom_a_byte(cur);
            let chrec = &mut self.adpcma_ch[ch];
            chrec.curbyte = byte;
            chrec.curaddress = cur.wrapping_add(1);
            chrec.curnibble = 1;
            let data = chrec.curbyte >> 4;
            self.apply_adpcma_delta(ch, data);
        } else {
            let data = self.adpcma_ch[ch].curbyte & 0x0F;
            self.adpcma_ch[ch].curnibble = 0;
            self.apply_adpcma_delta(ch, data);
        }
        false
    }

    /// Apply one ADPCM-A nibble: the delta walks the published table
    /// and the twelve-bit accumulator wraps like the MSM5205 form,
    /// per the decode law.
    fn apply_adpcma_delta(&mut self, ch: usize, data: u8) {
        let index = self.adpcma_ch[ch].step_index.clamp(0, 48);
        let size = i64::from(ADPCMA_STEPS[index as usize]);
        let delta = (i64::from(2) * i64::from(data & 0x07) + 1) * size / 8;
        let signed = if data & 0x08 != 0 { -delta } else { delta };
        let chrec = &mut self.adpcma_ch[ch];
        chrec.accum = (i64::from(chrec.accum) + signed) as u16 & 0x0FFF;
        let next = chrec.step_index + ADPCMA_STEP_INC[usize::from(data & 0x07)];
        chrec.step_index = next.clamp(0, 48);
    }

    /// The per-channel output per the combined levels, per the
    /// decode law of the ymfm core: the volume combines the
    /// instrument and total attenuators, and the DAC form masks the
    /// low two bits of the linear value.
    fn adpcma_output(&self, ch: usize) -> i32 {
        let chrec = &self.adpcma_ch[ch];
        if !chrec.playing {
            return 0;
        }
        let vol = ((u32::from(self.adpcma_regs[ch + 0x08]) & 0x1F) ^ 0x1F)
            + ((u32::from(self.adpcma_regs[1]) & 0x3F) ^ 0x3F);
        if vol >= 63 {
            return 0;
        }
        let mul = 15 - (vol & 7) as i64;
        let shift = 5 + (vol >> 3) as i64;
        // The 12-bit accumulator shifts up four bits and reads as a
        // signed 16-bit form, per the die form; the DAC form masks
        // the low two bits of the linear value.
        // The accumulator reads as a signed 16-bit form: the <<4
        // read with the top bit set reading negative, per the die
        // form.
        let signed = i64::from((chrec.accum << 4) as i16);
        let linear = (signed * mul) >> shift;
        (linear & !3) as i32
    }
}

/// The per-sample pipeline and the timer lines.
impl Ym2610 {
    /// One mixed sample per visit: the queued writes land, the
    /// per-operator pipeline walks, the channels, ADPCM engines and
    /// the SSG mix, and the mono form rides into the bounded sample
    /// buffer.
    fn render_sample(&mut self) {
        // The queued writes land at the boundary in write order;
        // the register stores walk the published surface and the
        // caches re-derive per store, so key-on reads the landed
        // registers per the chip's prepared form.
        let writes = std::mem::take(&mut self.pending);
        for w in &writes {
            self.store(w.addr, w.data);
            self.recache();
        }
        // The LFO walks its count per the divider before the operator
        // pipeline.
        self.run_lfo();
        // The envelope counter walks its published x.2 form: the
        // sub-counter per sample with the unit wrap at the divider.
        self.env_counter = (self.env_counter + 1) & ENV_COUNTER_MASK;
        if self.env_counter & 3 == 3 {
            self.env_counter = (self.env_counter + 1) & ENV_COUNTER_MASK;
        }
        // Every operator: the SSG clock, the envelope step, the phase
        // step, per the chip's pipeline.
        for op_index in 0..FM_CHANNELS * OPS {
            self.clock_op(op_index);
        }
        // The ADPCM-A engine clocks one nibble per channel on every
        // envelope cycle, per the published cadence, and its ended
        // channels set their flags (masked at the status read).
        if self.env_counter & 3 == 0 {
            for ch in 0..ADPCMA_CHANNELS {
                if self.clock_adpcma(ch) {
                    self.eos_status |= 1 << ch;
                }
            }
        }
        self.clock_adpcmb();
        // The SSG form walks its 250 kHz counters: exactly four and a
        // half ticks per FM sample, per the published rate model.
        self.ssg_clocks += FM_SAMPLE_PERIOD;
        while self.ssg_clocks >= SSG_SAMPLE_PERIOD {
            self.ssg_clocks -= SSG_SAMPLE_PERIOD;
            self.ssg.tick();
        }
        // The CSM form releases channel 2 after one sample.
        if self.csm_pulse {
            self.csm_pulse = false;
            for op in 0..OPS {
                self.key_off(OPS + op);
            }
        }
        // The channels mix their four operators; the 13-bit
        // no-intermediate-clip form clips per channel.
        let mut left: i64 = 0;
        let mut right: i64 = 0;
        for ch in 0..FM_CHANNELS {
            let form = self.channels[ch];
            if !(form.out_l() || form.out_r()) {
                continue;
            }
            let out = i64::from(self.channel_output(ch));
            if form.out_l() {
                left += out;
            }
            if form.out_r() {
                right += out;
            }
        }
        for ch in 0..ADPCMA_CHANNELS {
            let pan = self.adpcma_regs[ch + 0x08];
            let out = i64::from(self.adpcma_output(ch));
            if pan & 0x80 != 0 {
                left += out;
            }
            if pan & 0x40 != 0 {
                right += out;
            }
        }
        let out = i64::from(self.adpcmb_output());
        if self.adpcmb.pan_l() {
            left += out;
        }
        if self.adpcmb.pan_r() {
            right += out;
        }
        let ssg_mono = i64::from(self.ssg.mono());
        // The mono mix averages the two published outputs and adds
        // the SSG form, saturating into the 16-bit output like the
        // house PSG mixer.
        let total = (left + right) / 2 + ssg_mono;
        let sample = total.clamp(-32_768, 32_767) as i16;
        if self.samples.len() >= SAMPLE_CAP {
            self.samples.clear();
        }
        self.samples.push(sample);
        // The busy window drains in chip clocks.
        self.busy_cycles = self.busy_cycles.saturating_sub(FM_SAMPLE_PERIOD);
    }

    /// The channel output per the published algorithm form over the
    /// four operators, with the operator-1 feedback, ending at the
    /// per-channel clip of the no-intermediate-clip form.
    fn channel_output(&mut self, ch: usize) -> i32 {
        let alg = usize::from(self.channels[ch].alg());
        let form = self.channels[ch];
        // The operator-1 feedback per the published shift form: the
        // sum of the two previous outputs over level zero through
        // seven.
        let fb = u32::from(form.fb());
        let mut opmod: u32 = 0;
        if fb != 0 {
            let sum = self.feedback[ch][0].wrapping_add(self.feedback[ch][1]) as i64;
            opmod = (sum >> (10 - fb) as i64) as u32;
        }
        let am = self.am_offset_of(ch);
        // The per-operator volumes walk the connection table; each
        // modulator output halves into the next phase per the
        // published path.
        let op1 = self.op_volume(ch * OPS, self.ops[ch * OPS].phase, opmod, am);
        self.feedback[ch][0] = self.feedback[ch][1];
        self.feedback[ch][1] = op1;
        let mut opout = [0i32; 8];
        opout[1] = op1;
        let input2 = opout[ALG_OP2_IN[alg]] as u32;
        opout[2] = self.op_volume(
            ch * OPS + 1,
            self.ops[ch * OPS + 1].phase.wrapping_add(input2 >> 1),
            0,
            am,
        );
        opout[5] = opout[1].wrapping_add(opout[2]);
        opout[3] = self.op_volume(ch * OPS + 2, self.ops[ch * OPS + 2].phase, 0, am);
        let input3 = opout[ALG_OP3_IN[alg]] as u32;
        opout[3] = self.op_volume(
            ch * OPS + 2,
            self.ops[ch * OPS + 2].phase.wrapping_add(input3 >> 1),
            0,
            am,
        );
        opout[6] = opout[1].wrapping_add(opout[3]);
        opout[7] = opout[2].wrapping_add(opout[3]);
        let input4 = opout[ALG_OP4_IN[alg]] as u32;
        let mut result = self.op_volume(
            ch * OPS + 3,
            self.ops[ch * OPS + 3].phase.wrapping_add(input4 >> 1),
            0,
            am,
        );
        result >>= 1;
        let sums = u32::from(ALG_SUM_MASK[alg]);
        if sums & 0x01 != 0 {
            result += opout[1] >> 1;
        }
        if sums & 0x02 != 0 {
            result += opout[2] >> 1;
        }
        if sums & 0x04 != 0 {
            result += opout[3] >> 1;
        }
        result.clamp(-CHANNEL_CLIP - 1, CHANNEL_CLIP)
    }

    /// The per-operator volume per the chip's forms: the modulated
    /// phase indexes the log-sine quarter wave (mirrored per the
    /// eighth bit, signed per the ninth) into the power table with
    /// the envelope, amplitude-LFO and total-level attenuations
    /// shifted up two bits.
    fn op_volume(&self, op_index: usize, phase: u32, mod_in: u32, am: u32) -> i32 {
        let op = &self.ops[op_index];
        if op.env_att > EG_QUIET {
            return 0;
        }
        // The wave index: the modulated phase through the twenty-bit
        // wrap, per the die-verified forms; the top ten bits index
        // the wave with the sign and the quarter mirror.
        let mixed = phase.wrapping_add(mod_in);
        let wave = ((mixed & (PHASE_LIMIT - 1)) >> 10) as usize;
        let quarter = if wave & 0x100 != 0 {
            0xFF - (wave & 0xFF)
        } else {
            wave & 0xFF
        };
        // The envelope attenuation in the ten-bit form: the SSG-EG
        // inversion, the amplitude LFO per the operator enable, and
        // the total level, clamped at the top.
        let mut att = op.env_att;
        if op.ssg_inv {
            att = EG_MID.wrapping_sub(att) & EG_MAX;
        }
        if op.am_enabled() {
            att += am;
        }
        att += self.caches[op_index].total;
        let att = (att.min(EG_MAX)) as u64;
        // The combined attenuation in the 5.8 form and the power
        // lookup, per the die forms.
        let combined = u64::from(LOG_SIN[quarter]) + (att << 2);
        let value = i64::from(POWER_TABLE[(combined & 0xFF) as usize]) >> (combined >> 8);
        let value = value as i32;
        if wave & 0x200 != 0 { -value } else { value }
    }

    /// Clock the ADPCM-B engine one fraction per FM sample: the
    /// position advances by the delta-N and a wrapped step decodes a
    /// nibble with its own end and repeat forms, per the decode law.
    fn clock_adpcmb(&mut self) {
        if self.adpcmb.regs[0] & 0x80 == 0 {
            // A clear start bit drops the playing form per the
            // internal status walk.
            self.adpcmb.status &= !ADPCMB_PLAYING;
            return;
        }
        if self.adpcmb.status & ADPCMB_PLAYING == 0 {
            return;
        }
        let position = self.adpcmb.position as u64 + u64::from(self.adpcmb.delta_n());
        self.adpcmb.position = (position & 0xFFFF) as u32;
        if position < 0x1_0000 {
            return;
        }
        // Fetch at the first nibble, per the die form.
        if self.adpcmb.curnibble == 0 {
            self.adpcmb.curbyte = self.rom_b_byte(self.adpcmb.curaddress);
        }
        // Extract the nibble from the current byte.
        let data = if self.adpcmb.curnibble == 0 {
            self.adpcmb.curbyte >> 4
        } else {
            self.adpcmb.curbyte & 0x0F
        };
        self.adpcmb.curnibble ^= 1;
        // After the second nibble: the end form, the limit form or
        // the address advance, per the die form; the form's stop
        // path leaves the last decode unscaled, per the die order.
        if self.adpcmb.curnibble == 0 {
            let end = (self.adpcmb.end() + 1) << 8;
            if self.adpcmb.curaddress + 1 == end {
                // The end chunk just played through, inclusive.
                if self.adpcmb.repeat() {
                    self.adpcmb.load_start();
                } else {
                    self.adpcmb.accum = 0;
                    self.adpcmb.prev_accum = 0;
                    self.adpcmb.status = (self.adpcmb.status & !ADPCMB_PLAYING) | ADPCMB_EOS;
                    self.eos_status |= 0x80;
                    return;
                }
            } else if self.adpcmb.curaddress == 0xFF_FFFF {
                // The published limit form of the chip: the
                // sixteen-megabyte window wraps.
                self.adpcmb.curaddress = 0;
            } else {
                self.adpcmb.curaddress = self.adpcmb.curaddress.wrapping_add(1) & 0xFF_FFFF;
            }
        }
        // Remember the previous decoded form for the interpolation.
        self.adpcmb.prev_accum = self.adpcmb.accum;
        let delta = (i64::from(2) * i64::from(data & 0x07) + 1) * i64::from(self.adpcmb.step) / 8;
        let signed = if data & 0x08 != 0 { -delta } else { delta };
        self.adpcmb.accum = (self.adpcmb.accum as i64 + signed).clamp(-32_768, 32_767) as i32;
        // The step scaling per the published x.64 form.
        let scaled = i64::from(self.adpcmb.step)
            * i64::from(ADPCMB_STEP_SCALE[usize::from(data & 0x07)])
            / 64;
        self.adpcmb.step = scaled.clamp(ADPCMB_STEP_MIN, ADPCMB_STEP_MAX) as i32;
    }

    /// The ADPCM-B output: the linear interpolation between the
    /// previous and current decoded forms scaled by the level, per
    /// the output form of the ymfm core at the published right shift.
    fn adpcmb_output(&self) -> i32 {
        // The interpolation rides the position fraction and its
        // complement, then the level form at 8-plus-two right shifts.
        let pos = i64::from(self.adpcmb.position);
        let inv = i64::from(self.adpcmb.position ^ 0xFFFF) + 1;
        let mixed =
            (i64::from(self.adpcmb.prev_accum) * inv + i64::from(self.adpcmb.accum) * pos) >> 16;
        let result = mixed * self.adpcmb.level();
        (result >> 10) as i32
    }
}

/// The bus and device trait forms.
impl BusAccess for Ym2610 {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.observe(self.port_of(address).unwrap_or(0)))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if let Some(port) = self.port_of(address) {
            self.write_byte(port, value as u8);
        }
    }

    fn peek(&self, address: Address) -> u8 {
        self.observe(self.port_of(address).unwrap_or(0))
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for Ym2610 {
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

    fn audio(&self) -> Option<&dyn AudioSource> {
        Some(self)
    }

    fn audio_mut(&mut self) -> Option<&mut dyn demu_core::AudioSource> {
        Some(self)
    }

    fn reset(&mut self) {
        // The power-up form: registers rest at zero, envelopes rest
        // quiet and released, the timers stopped, the LFO off, the
        // ADPCM engines silent and the schedule parked until the
        // next boot. The external ROM bytes keep their wiring form.
        self.address = 0;
        self.regs = [[0; REG_FILE]; 2];
        self.ssg = Ssg::new();
        self.adpcma_regs = [0; ADPCMA_REG_FILE];
        self.adpcma_ch = [AdpcmACh::default(); ADPCMA_CHANNELS];
        self.adpcmb = AdpcmB::new();
        self.eos_status = 0;
        self.flag_mask = EOS_FLAG_MASK;
        self.freq_latch = [[0; 2]; 2];
        self.ch2_pairs = [0; 3];
        self.special = false;
        self.csm = false;
        self.csm_pulse = false;
        self.channels = [Channel::silent(); FM_CHANNELS];
        self.ops = [Operator::silent(); FM_CHANNELS * OPS];
        self.caches = [OpCache::default(); FM_CHANNELS * OPS];
        self.feedback = [[0; 2]; FM_CHANNELS];
        self.timer_a_reg = 0;
        self.timer_a_cnt = 0;
        self.timer_a_load = false;
        self.timer_a_enable = false;
        self.timer_a_flag = false;
        self.timer_b_reg = 0;
        self.timer_b_cnt = 0;
        self.timer_b_sub = 0;
        self.timer_b_load = false;
        self.timer_b_enable = false;
        self.timer_b_flag = false;
        self.lfo_enable = false;
        self.lfo_rate = 0;
        self.lfo_sub = 0;
        self.lfo_phase = 0;
        self.lfo_am = 0;
        self.lfo_pm = 0;
        self.env_counter = 0;
        self.ssg_clocks = 0;
        self.busy_cycles = 0;
        self.irq_on = false;
        self.pending.clear();
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        // The chip's own boot state carries no sample data: the two
        // external ROM images ride their named devices' boot states
        // and landed at assembly time. Boot resets and re-arms the
        // schedule.
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(OPN_SNAPSHOT_LEN);
        out.extend_from_slice(b"YMNB");
        out.push(1);
        out.extend_from_slice(&self.regs[0]);
        out.extend_from_slice(&self.regs[1]);
        out.extend_from_slice(&self.address.to_le_bytes());
        for bank in &self.freq_latch {
            out.extend_from_slice(bank);
        }
        for pair in &self.ch2_pairs {
            out.extend_from_slice(&pair.to_le_bytes());
        }
        let mut flags = u8::from(self.special)
            | (u8::from(self.csm) << 1)
            | (u8::from(self.csm_pulse) << 2)
            | (u8::from(self.irq_on) << 3);
        flags |= self.lfo_rate << 4;
        flags |= u8::from(self.timer_a_load) << 7;
        out.push(flags);
        let flags_b = u8::from(self.timer_b_load)
            | (u8::from(self.timer_a_enable) << 1)
            | (u8::from(self.timer_b_enable) << 2)
            | (u8::from(self.timer_a_flag) << 3)
            | (u8::from(self.timer_b_flag) << 4);
        out.push(flags_b);
        for op in &self.ops {
            out.push(op.dt_mul);
            out.push(op.tl);
            out.push(op.ksr_ar);
            out.push(op.am_d1r);
            out.push(op.d2r);
            out.push(op.d1l_rr);
            out.push(op.ssg);
            out.extend_from_slice(&op.phase.to_le_bytes());
            out.extend_from_slice(&(op.env_att as u16).to_le_bytes());
            out.push(op.state as u8);
            out.push(u8::from(op.key) | (u8::from(op.ssg_inv) << 1));
        }
        for ch in &self.channels {
            out.extend_from_slice(&ch.block_freq.to_le_bytes());
            out.push(ch.fb_alg);
            out.push(ch.out_ams_fms);
        }
        out.extend_from_slice(&self.timer_a_reg.to_le_bytes());
        out.extend_from_slice(&self.timer_a_cnt.to_le_bytes());
        out.push(self.timer_b_reg);
        out.extend_from_slice(&self.timer_b_cnt.to_le_bytes());
        out.extend_from_slice(&self.timer_b_sub.to_le_bytes());
        out.push(self.lfo_rate);
        out.extend_from_slice(&self.lfo_sub.to_le_bytes());
        out.extend_from_slice(&self.lfo_phase.to_le_bytes());
        out.extend_from_slice(&self.env_counter.to_le_bytes());
        // The SSG engine: the walk flags byte, the register block,
        // the tone counters and squares, the noise walker and the
        // envelope walk.
        let flags_c = u8::from(self.ssg.env_holding) | (self.ssg.noise_prescale << 1);
        out.push(flags_c);
        out.extend_from_slice(&self.ssg.regs);
        for ch in 0..SSG_CHANNELS {
            out.extend_from_slice(&(self.ssg.tone_count[ch] as u16).to_le_bytes());
            out.push(self.ssg.tone_state[ch]);
        }
        out.push(self.ssg.noise_count as u8);
        out.extend_from_slice(&self.ssg.rng.to_le_bytes());
        out.extend_from_slice(&self.ssg.env_count.to_le_bytes());
        out.push(self.ssg.env_step as u8);
        out.push(self.ssg.env_flip);
        // The ADPCM-A engine: the shared registers then the six
        // channel forms.
        out.extend_from_slice(&self.adpcma_regs);
        for chrec in &self.adpcma_ch {
            out.push(u8::from(chrec.playing));
            out.push(chrec.curnibble);
            out.push(chrec.curbyte);
            out.extend_from_slice(&chrec.curaddress.to_le_bytes());
            out.extend_from_slice(&chrec.accum.to_le_bytes());
            out.extend_from_slice(&(chrec.step_index as u32).to_le_bytes());
        }
        // The ADPCM-B engine: the registers then the channel form.
        out.extend_from_slice(&self.adpcmb.regs);
        out.push(self.adpcmb.status);
        out.push(self.adpcmb.curnibble);
        out.push(self.adpcmb.curbyte);
        out.extend_from_slice(&self.adpcmb.position.to_le_bytes());
        out.extend_from_slice(&self.adpcmb.curaddress.to_le_bytes());
        out.extend_from_slice(&self.adpcmb.accum.to_le_bytes());
        out.extend_from_slice(&self.adpcmb.prev_accum.to_le_bytes());
        out.extend_from_slice(&self.adpcmb.step.to_le_bytes());
        out.push(self.eos_status);
        out.push(self.flag_mask);
        out.extend_from_slice(&self.busy_cycles.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != OPN_SNAPSHOT_LEN || &data[..4] != b"YMNB" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "ym2610 chunk must be {OPN_SNAPSHOT_LEN} bytes"
            )));
        }
        let mut pos = 5usize;
        for bank in &mut self.regs {
            bank.copy_from_slice(&data[pos..pos + REG_FILE]);
            pos += REG_FILE;
        }
        self.address = le_u16(&data[pos..pos + 2]);
        pos += 2;
        for bank in &mut self.freq_latch {
            bank.copy_from_slice(&data[pos..pos + 2]);
            pos += 2;
        }
        for pair in &mut self.ch2_pairs {
            *pair = le_u16(&data[pos..pos + 2]);
            pos += 2;
        }
        let flags = data[pos];
        pos += 1;
        self.special = flags & 0x01 != 0;
        self.csm = flags & 0x02 != 0;
        self.csm_pulse = flags & 0x04 != 0;
        self.irq_on = flags & 0x08 != 0;
        self.lfo_rate = (flags >> 4) & 0x07;
        self.timer_a_load = flags & 0x80 != 0;
        let flags_b = data[pos];
        pos += 1;
        self.timer_b_load = flags_b & 0x01 != 0;
        self.timer_a_enable = flags_b & 0x02 != 0;
        self.timer_b_enable = flags_b & 0x04 != 0;
        self.timer_a_flag = flags_b & 0x08 != 0;
        self.timer_b_flag = flags_b & 0x10 != 0;
        for op in &mut self.ops {
            *op = Operator::silent();
            op.dt_mul = data[pos];
            op.tl = data[pos + 1];
            op.ksr_ar = data[pos + 2];
            op.am_d1r = data[pos + 3];
            op.d2r = data[pos + 4];
            op.d1l_rr = data[pos + 5];
            op.ssg = data[pos + 6];
            op.phase = le_u32(&data[pos + 7..pos + 11]);
            op.env_att = u32::from(le_u16(&data[pos + 11..pos + 13]));
            op.state = EgState::from_code(data[pos + 13]);
            op.key = data[pos + 14] & 0x01 != 0;
            op.ssg_inv = data[pos + 14] & 0x02 != 0;
            pos += 15;
        }
        for ch in &mut self.channels {
            ch.block_freq = le_u16(&data[pos..pos + 2]);
            pos += 2;
            ch.fb_alg = data[pos];
            pos += 1;
            ch.out_ams_fms = data[pos];
            pos += 1;
        }
        self.timer_a_reg = le_u16(&data[pos..pos + 2]);
        pos += 2;
        self.timer_a_cnt = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.timer_b_reg = data[pos];
        pos += 1;
        self.timer_b_cnt = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.timer_b_sub = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.lfo_rate = data[pos];
        pos += 1;
        self.lfo_sub = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.lfo_phase = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.env_counter = le_u32(&data[pos..pos + 4]);
        pos += 4;
        let flags_c = data[pos];
        pos += 1;
        self.ssg.env_holding = flags_c & 0x01 != 0;
        self.ssg.noise_prescale = (flags_c >> 1) & 0x01;
        self.ssg
            .regs
            .copy_from_slice(&data[pos..pos + SSG_REG_FILE]);
        pos += SSG_REG_FILE;
        for ch in 0..SSG_CHANNELS {
            self.ssg.tone_count[ch] = u32::from(le_u16(&data[pos..pos + 2]));
            pos += 2;
            self.ssg.tone_state[ch] = data[pos];
            pos += 1;
        }
        self.ssg.noise_count = u32::from(data[pos]);
        pos += 1;
        self.ssg.rng = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.ssg.env_count = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.ssg.env_step = data[pos] as i8 as i32;
        pos += 1;
        self.ssg.env_flip = data[pos];
        pos += 1;
        self.adpcma_regs
            .copy_from_slice(&data[pos..pos + ADPCMA_REG_FILE]);
        pos += ADPCMA_REG_FILE;
        for chrec in &mut self.adpcma_ch {
            *chrec = AdpcmACh::default();
            chrec.playing = data[pos] != 0;
            chrec.curnibble = data[pos + 1];
            chrec.curbyte = data[pos + 2];
            chrec.curaddress = le_u32(&data[pos + 3..pos + 7]);
            chrec.accum = le_u16(&data[pos + 7..pos + 9]);
            chrec.step_index = le_u32(&data[pos + 9..pos + 13]) as i32;
            pos += 13;
        }
        self.adpcmb
            .regs
            .copy_from_slice(&data[pos..pos + ADPCMB_REG_FILE]);
        pos += ADPCMB_REG_FILE;
        self.adpcmb.status = data[pos];
        pos += 1;
        self.adpcmb.curnibble = data[pos];
        pos += 1;
        self.adpcmb.curbyte = data[pos];
        pos += 1;
        self.adpcmb.position = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.adpcmb.curaddress = le_u32(&data[pos..pos + 4]);
        pos += 4;
        self.adpcmb.accum = le_u32(&data[pos..pos + 4]) as i32;
        pos += 4;
        self.adpcmb.prev_accum = le_u32(&data[pos..pos + 4]) as i32;
        pos += 4;
        self.adpcmb.step = le_u32(&data[pos..pos + 4]) as i32;
        pos += 4;
        self.eos_status = data[pos];
        pos += 1;
        self.flag_mask = data[pos];
        pos += 1;
        self.busy_cycles = le_u32(&data[pos..pos + 4]);
        self.rederive_lfo();
        self.recache();
        self.pending.clear();
        self.samples.clear();
        // The schedule re-arms at the next boundary, the restored
        // device runs at its own cadence per the snapshot idiom.
        self.next = Some(Tick(0));
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        self.render_sample();
        self.step_timers();
        self.refresh_irq(ctx, tick);
        self.next = Some(Tick(tick.0 + self.div));
    }
}

impl AudioSource for Ym2610 {
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

/// One little-endian u16 from snapshot bytes whose length the caller
/// validated.
fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// One little-endian u32 from snapshot bytes whose length the caller
/// validated.
fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_config::newtypes::{DeviceName, Frequency, KindName};
    use demu_core::device::{BusBank, DeviceSlices, EventBuffers, Route};
    use demu_core::machine::{DeviceCtors, Machine, MachineError};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    /// The base of the chip's port window in the test machine: the
    /// Neo Geo wires the YM2610 at the Z80 sound-bus ports $04-$07.
    const PORT_BASE: u32 = 0x0004;

    fn instance() -> DeviceInstance {
        DeviceInstance {
            name: DeviceName::new("ym0").expect("name"),
            kind: KindName::new("ym2610.chip").expect("kind"),
            domain: "sound".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    /// The mini machine form: one sound domain and the chip window
    /// at the published Neo Geo port form, with the boot states
    /// supplied by the caller.
    fn sys_config(boot: BTreeMap<DeviceName, BootState>, div: u32) -> SystemConfig {
        let mut ym = instance();
        ym.dividers = BTreeMap::from([("div".to_string(), div)]);
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "sound".to_string(),
            ClockDomain {
                name: "sound".to_string(),
                frequency: Frequency::new(8_000_000, 1).expect("freq"),
                origin: "the 8 MHz chip clock the machine carries".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ym.name.clone(), 0usize);
        SystemConfig {
            name: "neogeo-test".to_string(),
            triple: "z80-neogeo-aes".to_string(),
            description: "the YM2610 test machine".to_string(),
            clocks,
            devices: vec![ym],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "sound".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "ym".to_string(),
                bus: "sound".to_string(),
                device: DeviceName::new("ym0").expect("name"),
                base: PORT_BASE,
                size: 4,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            }],
            connections: Vec::new(),
            signals: Vec::new(),
            boot,
            quirks: BTreeMap::new(),
        }
    }

    fn boot_map(rom_a: Vec<u8>, rom_b: Vec<u8>) -> BTreeMap<DeviceName, BootState> {
        BTreeMap::from([
            (
                DeviceName::new("adpcma").expect("name"),
                BootState::Hex { bytes: rom_a },
            ),
            (
                DeviceName::new("adpcmb").expect("name"),
                BootState::Hex { bytes: rom_b },
            ),
        ])
    }

    fn chip() -> Ym2610 {
        Ym2610::new(
            &instance(),
            &sys_config(boot_map(Vec::new(), Vec::new()), 144),
        )
        .expect("chip")
    }

    fn chip_with_rom(rom_a: Vec<u8>, rom_b: Vec<u8>) -> Ym2610 {
        Ym2610::new(&instance(), &sys_config(boot_map(rom_a, rom_b), 144)).expect("chip")
    }

    /// One window write per address: the four ports of the window at
    /// the wired base.
    fn wp(s: &mut Ym2610, port: u32, byte: u8) {
        s.write(
            AccessWidth::Byte,
            Address(PORT_BASE + port),
            u32::from(byte),
            Tick(0),
        );
    }
    /// The A1=0 address and data forms.
    fn aad(s: &mut Ym2610, addr: u8) {
        wp(s, 0, addr);
    }
    fn adc(s: &mut Ym2610, data: u8) {
        wp(s, 1, data);
    }
    /// The A1=1 forms (the ADPCM-A block and FM channels 3-4).
    fn bid(s: &mut Ym2610, addr: u8) {
        wp(s, 2, addr);
    }
    fn bdc(s: &mut Ym2610, data: u8) {
        wp(s, 3, data);
    }

    /// The register-store pair for one FM channel: channels 1 and 2
    /// live in the lower bank (ports 4/5) and channels 3 and 4 in
    /// the upper bank (ports 6/7), per the published port split.
    fn fm_store(s: &mut Ym2610, fm_ch: usize, reg: u8, data: u8) {
        if fm_ch < 2 {
            aad(s, reg);
            adc(s, data);
        } else {
            bid(s, reg);
            bdc(s, data);
        }
    }

    /// One per-operator register write helper: the row base carries
    /// the operator in bits two-three and the channel-pair position
    /// in bit zero, per the published layout; repeated key ons and
    /// bank routing ride the helper.
    fn opw(s: &mut Ym2610, base: u8, slot: u8, fm_ch: usize, data: u8) {
        let pos = if fm_ch.is_multiple_of(2) { 1u8 } else { 2u8 };
        fm_store(s, fm_ch, base | (slot << 2) | pos, data);
    }

    /// One device visit under an empty context: the render sample
    /// and the line form ride the visit.
    fn tick(s: &mut Ym2610, tick: Tick) {
        let mut before: Vec<Box<dyn Device>> = Vec::new();
        let mut after: Vec<Box<dyn Device>> = Vec::new();
        let mut buses: Vec<demu_core::Bus> = Vec::new();
        let mut scratch: Vec<demu_core::DecodeHit> = Vec::new();
        let mut events: Vec<demu_core::TriggerEvent> = Vec::new();
        let mut dirty: Vec<usize> = Vec::new();
        let index = Default::default();
        let mut ctx = MachineCtx::new(
            DeviceSlices {
                before: before.as_mut_slice(),
                after: after.as_mut_slice(),
                caller_slot: 0,
            },
            BusBank {
                buses: buses.as_mut_slice(),
                index: &index,
            },
            &[],
            &mut scratch,
            EventBuffers {
                events: &mut events,
                dirty: &mut dirty,
            },
            false,
        );
        s.tick(tick, &mut ctx);
    }

    /// One visit with the `irq` line delivered into a spy device
    /// through a route, so the notifications are the only
    /// observation path.
    struct Spy {
        name: String,
        edges: Rc<RefCell<Vec<bool>>>,
    }

    impl Device for Spy {
        fn name(&self) -> &str {
            &self.name
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn reset(&mut self) {}
        fn boot(&mut self, _state: Option<&BootState>) {}
        fn notify_line(&mut self, _line: &str, level: Level, _tick: Tick) {
            self.edges.borrow_mut().push(level.is_high());
        }
        fn snapshot(&self) -> Snapshot {
            Snapshot::new()
        }
        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }
        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    fn tick_irq(s: &mut Ym2610, tick: Tick, edges: &Rc<RefCell<Vec<bool>>>) {
        let mut before: Vec<Box<dyn Device>> = Vec::new();
        let mut after: Vec<Box<dyn Device>> = vec![Box::new(Spy {
            name: "spy".to_string(),
            edges: edges.clone(),
        })];
        let mut buses: Vec<demu_core::Bus> = Vec::new();
        let mut scratch: Vec<demu_core::DecodeHit> = Vec::new();
        let mut events: Vec<demu_core::TriggerEvent> = Vec::new();
        let mut dirty: Vec<usize> = Vec::new();
        let index = Default::default();
        let route = Route {
            from: 0,
            from_line: "irq".to_string(),
            to: 1,
            to_line: "irq".to_string(),
        };
        let routes = [route];
        let mut ctx = MachineCtx::new(
            DeviceSlices {
                before: before.as_mut_slice(),
                after: after.as_mut_slice(),
                caller_slot: 0,
            },
            BusBank {
                buses: buses.as_mut_slice(),
                index: &index,
            },
            &routes,
            &mut scratch,
            EventBuffers {
                events: &mut events,
                dirty: &mut dirty,
            },
            false,
        );
        s.tick(tick, &mut ctx);
    }

    /// One sample visit for the direct forms: renders without the
    /// timer lines.
    pub(crate) fn run_sample(s: &mut Ym2610) {
        s.render_sample();
    }

    #[test]
    fn the_four_ports_latch_and_store_per_bank() {
        let mut s = chip();
        // The lower bank pair: the address latches and the data
        // stores ride the latched address at the next sample
        // boundary.
        aad(&mut s, 0x40);
        adc(&mut s, 0x5A);
        run_sample(&mut s);
        assert_eq!(s.regs[0][0x40], 0x5A);
        // The upper bank pair answers the same way.
        bid(&mut s, 0x40);
        bdc(&mut s, 0x26);
        run_sample(&mut s);
        assert_eq!(s.regs[1][0x40], 0x26);
        assert!(s.pending.is_empty());
        // The latched address survives the stores per the published
        // form; the next lower data write lands at the same register.
        aad(&mut s, 0x40);
        adc(&mut s, 0x21);
        run_sample(&mut s);
        assert_eq!(s.regs[0][0x40], 0x21);
    }

    #[test]
    fn the_upper_address_sets_the_bank_bit() {
        let mut s = chip();
        bid(&mut s, 0xA1);
        assert_eq!(s.address, 0x1A1, "the bank bit rides bit 8");
        // The lower address write clears the bank form again.
        aad(&mut s, 0x40);
        assert_eq!(s.address, 0x40);
    }

    #[test]
    fn the_bank_bit_gates_cross_bank_stores() {
        let mut s = chip();
        // The upper address latched, then a lower data write: the
        // pairing caution ignores the data.
        bid(&mut s, 0xA1);
        adc(&mut s, 0x77);
        assert!(s.pending.is_empty());
        // The lower address and an upper data form ignore too.
        aad(&mut s, 0x40);
        bdc(&mut s, 0x99);
        assert!(s.pending.is_empty());
        // The paired stores go through.
        adc(&mut s, 0x11);
        assert_eq!(s.pending.len(), 1);
        run_sample(&mut s);
        assert_eq!(s.regs[0][0x40], 0x11);
        bid(&mut s, 0xA1);
        bdc(&mut s, 0x22);
        run_sample(&mut s);
        assert_eq!(s.regs[1][0xA1], 0x22);
    }

    #[test]
    fn the_data_read_answers_ssg_register_and_id_code() {
        let mut s = chip();
        // The SSG register store and the readback, per the published
        // read form; the store lands at the next sample boundary, so
        // the readback answers the applied register.
        aad(&mut s, 0x03);
        adc(&mut s, 0x0B); // Channel B coarse tune.
        run_sample(&mut s);
        aad(&mut s, 0x03);
        assert_eq!(
            s.read(AccessWidth::Byte, Address(PORT_BASE + 1), Tick(0)),
            0x0B
        );
        assert_eq!(s.peek(Address(PORT_BASE + 1)), 0x0B);
        // The status reads answer their published forms. A data write
        // rides the busy form; run a visit to drain it.
        adc(&mut s, 0x55);
        run_sample(&mut s);
        assert_eq!(s.read(AccessWidth::Byte, Address(PORT_BASE), Tick(0)), 0);
        assert_eq!(
            s.read(AccessWidth::Byte, Address(PORT_BASE + 2), Tick(0)),
            0
        );
        assert_eq!(
            s.read(AccessWidth::Byte, Address(PORT_BASE + 3), Tick(0)),
            0
        );
        // Register $FF answers the published identification code.
        aad(&mut s, 0xFF);
        assert_eq!(
            s.read(AccessWidth::Byte, Address(PORT_BASE + 1), Tick(0)),
            1
        );
        // Other non-SSG addresses answer 0.
        aad(&mut s, 0x21);
        assert_eq!(
            s.read(AccessWidth::Byte, Address(PORT_BASE + 1), Tick(0)),
            0
        );
    }

    #[test]
    fn the_busy_form_rides_the_published_window() {
        let mut s = chip();
        aad(&mut s, 0x40);
        // An address write alone raises no busy form.
        assert_eq!(s.status0() & 0x80, 0);
        adc(&mut s, 0x10);
        assert_eq!(s.status0() & 0x80, 0x80, "the data write set the form");
        // A visit drains the 83-chip-clock window (the sample takes
        // 144), so the form clears by the next read window.
        run_sample(&mut s);
        assert_eq!(s.status0() & 0x80, 0, "the window drained");
        // A second data write sets the form again.
        adc(&mut s, 0x20);
        assert_eq!(s.status0() & 0x80, 0x80);
    }

    #[test]
    fn ssg_tone_period_and_render() {
        let mut s = chip();
        // Channel A tone: the period rides the fine and coarse pair
        // in the published form.
        aad(&mut s, 0x01);
        adc(&mut s, 0x02); // Coarse tune 2.
        aad(&mut s, 0x00);
        adc(&mut s, 0x38); // Fine tune $38.
        run_sample(&mut s); // The stores land at the boundary.
        assert_eq!(s.ssg.tone_period(0), 0x238);
        // Tone enabled at register $07, fixed level 15; a short
        // period walks the square inside the window.
        aad(&mut s, 0x07);
        adc(&mut s, 0x00);
        aad(&mut s, 0x08);
        adc(&mut s, 0x0F);
        aad(&mut s, 0x01);
        adc(&mut s, 0x00); // Coarse tune back to 0.
        aad(&mut s, 0x00);
        adc(&mut s, 0x02); // Fine tune 2: a short period.
        for _ in 0..60 {
            run_sample(&mut s);
        }
        let voiced = s.take_samples();
        assert!(!voiced.is_empty(), "the visits rendered");
        assert!(voiced.iter().any(|v| *v != 0), "the SSG tone sounded");
        // The square walks: the samples contain both the level form
        // and silence.
        assert!(voiced.contains(&0), "the square toggled");
        // The LFSR walked: the state leaves the seed form.
        assert_ne!(s.ssg.rng, 1);
    }

    #[test]
    fn ssg_envelope_generator_walks_the_shape() {
        let mut s = chip();
        // The shared envelope: period 2, the shape $0C repeat-rise
        // form per the published shape table.
        aad(&mut s, 0x0C);
        adc(&mut s, 0x00);
        aad(&mut s, 0x0B);
        adc(&mut s, 0x02);
        aad(&mut s, 0x0D);
        adc(&mut s, 0x0C);
        assert!(!s.ssg.env_hold, "shape $0C holds nothing");
        // Channel A rides the envelope through its mode bit.
        aad(&mut s, 0x08);
        adc(&mut s, 0x10);
        aad(&mut s, 0x00);
        adc(&mut s, 0x04);
        aad(&mut s, 0x01);
        adc(&mut s, 0x00);
        for _ in 0..200 {
            run_sample(&mut s);
        }
        let run = s.take_samples();
        assert!(run.iter().any(|v| *v != 0), "the enveloped tone sounded");
        // The walk repeats: the step stays inside the 32-step cycle.
        let step = s.ssg.env_step;
        assert!((-1..=31).contains(&step), "the walk stays on cycle form");
    }

    #[test]
    fn ssg_noise_channel_drives_the_gate() {
        let mut s = chip();
        // Noise enabled on channel A only, fixed level 15: the
        // register $07 carries the tone-enable negative for A.
        aad(&mut s, 0x07);
        adc(&mut s, 0x01); // Tone A off; tone A enabled-negative set.
        aad(&mut s, 0x06);
        adc(&mut s, 0x01);
        aad(&mut s, 0x08);
        adc(&mut s, 0x0F);
        for _ in 0..60 {
            run_sample(&mut s);
        }
        let run = s.take_samples();
        assert!(run.iter().any(|v| *v != 0), "the noise channel sounded");
        // The LFSR walked: the state left the seed form.
        assert_ne!(s.ssg.rng, 1);
    }

    /// The FM voice form: the channel runs the algorithm `alg` at
    /// the frequency form (`hi` the block-and-top form and `low` the
    /// eight-bit low byte), the total levels of the modulators at
    /// `mod_tl` and the carriers at the full form, the instant
    /// attack per the published rate-31 forms, and the key on rides
    /// the published operator form.
    pub(crate) fn fm_setup(alg: u8, hi: u8, low: u8, fm_ch: usize, mod_tl: u8) -> Ym2610 {
        let mut s = chip();
        // The channel forms: channels 1 and 3 (the even indices) sit
        // on the odd positions of the rows, channels 2 and 4 on the
        // even ones, per the published port layout.
        let fam = if fm_ch.is_multiple_of(2) { 0u8 } else { 1u8 };
        // The frequency form: the high register first per the
        // published caution.
        fm_store(&mut s, fm_ch, 0xA5 + fam, hi);
        fm_store(&mut s, fm_ch, 0xA1 + fam, low);
        // The per-operator groups: the multiple one, the rate
        // scaling zero, the instant attack per the published rate
        // forms, and the decay rate zero so the voice holds at full
        // level (the published continue-without-decaying form).
        let tls = [mod_tl, mod_tl, 0, 0];
        for i in 0..4u8 {
            opw(&mut s, 0x30, i, fm_ch, 0x01);
            opw(&mut s, 0x40, i, fm_ch, tls[i as usize]);
            opw(&mut s, 0x50, i, fm_ch, 0x1F);
            opw(&mut s, 0x60, i, fm_ch, 0x00);
            opw(&mut s, 0x70, i, fm_ch, 0x00);
            opw(&mut s, 0x80, i, fm_ch, 0x0F);
            opw(&mut s, 0x90, i, fm_ch, 0x00);
        }
        fm_store(&mut s, fm_ch, 0xB1 + fam, alg & 0x07);
        fm_store(&mut s, fm_ch, 0xB5 + fam, 0xC0);
        // The published channel codes: 001, 010, 101, 110.
        let key = 0xF0 | channel_code(fm_ch);
        aad(&mut s, 0x28);
        adc(&mut s, key);
        run_sample(&mut s);
        s
    }

    /// The published channel code for the FM channel index.
    fn channel_code(fm_ch: usize) -> u8 {
        match fm_ch {
            0 => 0x01,
            1 => 0x02,
            2 => 0x05,
            _ => 0x06,
        }
    }

    /// The zero-crossing count of the sample run: the count of the
    /// sign flips on the mixed mono form.
    fn zero_crossings(samples: &[i16]) -> usize {
        samples
            .windows(2)
            .filter(|w| w[1].signum() != w[0].signum())
            .count()
    }

    #[test]
    fn the_fm_channel_outputs_a_periodic_pattern() {
        let mut s = fm_setup(0, 0x08, 0x80, 0, 0x00);
        // The frequency form carries block one with the frequency
        // number 128: the die-verified step form gives the 8192-sample
        // period. Run under the buffer cap and check the period and
        // the crossings.
        for _ in 0..9000 {
            run_sample(&mut s);
        }
        let samples = s.take_samples();
        assert!(
            samples.iter().any(|v| *v != 0),
            "the mixed form carried sound"
        );
        let crossings = zero_crossings(&samples);
        assert!(crossings >= 1, "the mixed form crossed zero");
        assert!(crossings <= 6, "the crossings walk the period form");
        // The pattern repeats per the period form: a visit inside the
        // window repeats at the next period's same offset.
        let period = 8192usize;
        assert_eq!(samples[8], samples[period + 8]);
    }

    #[test]
    fn the_key_off_silences_the_channel() {
        let mut s = fm_setup(0, 0x08, 0x80, 0, 0x00);
        for _ in 0..100 {
            run_sample(&mut s);
        }
        assert!(s.take_samples().iter().any(|v| *v != 0));
        // The key off per the published operator form: the channel-1
        // code with the operator bits clear starts the release; the
        // release rate 15 walks the ceiling and the envelope holds
        // quiet per the published quiet form.
        aad(&mut s, 0x28);
        adc(&mut s, 0x01);
        run_sample(&mut s);
        for _ in 0..3000 {
            run_sample(&mut s);
        }
        s.take_samples();
        for _ in 0..3000 {
            run_sample(&mut s);
        }
        assert!(
            s.take_samples().iter().all(|v| *v == 0),
            "the release form held the channel silent"
        );
    }

    #[test]
    fn the_fm_codes_walk_the_four_channels() {
        for fm_ch in 1..4usize {
            let mut s = fm_setup(7, 0x08, 0x80, fm_ch, 0x00);
            for _ in 0..600 {
                run_sample(&mut s);
            }
            let voiced = s.take_samples();
            assert!(voiced.iter().any(|v| *v != 0), "the channel voice sounded");
        }
    }

    /// An ADPCM-A run helper: the ROM bytes, the pan/level settings
    /// and the start/end address registers; the callers key the
    /// channel on and run visits themselves.
    fn adpcma_setup(rom: Vec<u8>, start_lsb: u8, start_msb: u8, end_lsb: u8) -> (Ym2610, u8) {
        let mut s = chip_with_rom(rom, Vec::new());
        bid(&mut s, 0x08);
        bdc(&mut s, 0xDF); // Both outputs, level 31.
        bid(&mut s, 0x01);
        bdc(&mut s, 0x3F); // Total level full.
        bid(&mut s, 0x10);
        bdc(&mut s, start_lsb);
        bid(&mut s, 0x18);
        bdc(&mut s, start_msb);
        bid(&mut s, 0x20);
        bdc(&mut s, end_lsb);
        bid(&mut s, 0x28);
        bdc(&mut s, 0); // End MSB 0: the sample sits in the first bank.
        run_sample(&mut s); // The setup registers land.
        (s, 0x01)
    }

    #[test]
    fn adpcm_a_renders_a_tiny_stream() {
        // The 4-bit stream: constant +2-index walking codes step the
        // 12-bit accumulator upward.
        let mut rom = vec![0u8; 512];
        for slot in rom.iter_mut().take(128) {
            *slot = 0x11; // +3-forecast codes twice per byte.
        }
        let (mut s, key) = adpcma_setup(rom, 0x21, 0x1D, 0x22);
        bid(&mut s, 0x00);
        bdc(&mut s, key); // Start bit 0: play the set channels.
        run_sample(&mut s);
        // The key-on loaded the 256-byte-formed address: the LSB
        // byte over the MSB byte, shifted up one page form, per the
        // published address pair.
        assert_eq!(s.adpcma_ch[0].curaddress, (0x21 | (0x1D << 8)) << 8);
        assert!(s.adpcma_ch[0].playing, "the channel plays");
        // The decode walks and the render carries sound.
        for _ in 0..300 {
            run_sample(&mut s);
        }
        let voiced = s.take_samples();
        assert!(voiced.iter().any(|v| *v != 0), "the ADPCM-A stream sounded");
    }

    #[test]
    fn adpcm_a_runs_out_and_sets_its_flag() {
        let rom = vec![0x0F; 512]; // +15-forecast codes climb fast.
        let (mut s, key) = adpcma_setup(rom, 0x00, 0x00, 0x01);
        bid(&mut s, 0x00);
        bdc(&mut s, key);
        // The half-KiB sample walks 1024 nibbles, one per third
        // visit; run past it and expect the end form.
        let mut ended = false;
        for _ in 0..4000 {
            s.render_sample();
            if s.eos_status & 0x01 != 0 {
                ended = true;
                break;
            }
        }
        assert!(ended, "the channel ended");
        assert_eq!(s.eos_status & 0x01, 0x01, "the channel-1 end flag set");
        assert_eq!(s.status1() & 0x01, 0x01, "status 1 rides the flag");
        assert_eq!(s.flag_mask & 0x01, 0x01, "the flag mask kept the bit");
        // The write-one reset-and-mask form clears the flag and
        // masks it; the write-zero unmasks, per the published
        // flag-control process.
        aad(&mut s, 0x1C);
        adc(&mut s, 0x01);
        run_sample(&mut s);
        assert_eq!(s.flag_mask & 0x01, 0, "the write-one masked the flag");
        assert_eq!(s.eos_status & 0x01, 0, "the write-one reset the flag");
        aad(&mut s, 0x1C);
        adc(&mut s, 0x00);
        run_sample(&mut s);
        assert_eq!(s.flag_mask & 0x01, 0x01, "the write-zero unmasked");
    }

    #[test]
    fn adpcm_b_renders_the_variable_rate_stream() {
        // The +15 codes climb the 16-bit accumulator fast.
        let mut rom = vec![0u8; 512];
        for slot in rom.iter_mut().take(128) {
            *slot = 0xFF;
        }
        let mut s = chip_with_rom(Vec::new(), rom);
        aad(&mut s, 0x11);
        adc(&mut s, 0xC0); // Both outputs.
        // The start/end register pairs ride the published example:
        // a sample at 0x0400 through 0x0800 (the inclusive end).
        aad(&mut s, 0x12);
        adc(&mut s, 0x04);
        aad(&mut s, 0x13);
        adc(&mut s, 0x00);
        aad(&mut s, 0x14);
        adc(&mut s, 0x07);
        aad(&mut s, 0x15);
        adc(&mut s, 0x00);
        // The delta-N rides the quarter-rate form of the published
        // converter: about a quarter of the top 55.5 kHz.
        let dn: u16 = 16_384;
        aad(&mut s, 0x19);
        adc(&mut s, (dn & 0xFF) as u8);
        aad(&mut s, 0x1A);
        adc(&mut s, (dn >> 8) as u8);
        aad(&mut s, 0x1B);
        adc(&mut s, 0xFF);
        run_sample(&mut s); // The setup registers land.
        // Start per the published control form: set with the dump
        // bit clear; the load lands at the next boundary.
        aad(&mut s, 0x10);
        adc(&mut s, 0x80);
        run_sample(&mut s);
        assert_eq!(s.adpcmb.curaddress, 0x0400, "the start address loaded");
        for _ in 0..600 {
            run_sample(&mut s);
        }
        let voiced = s.take_samples();
        assert!(voiced.iter().any(|v| *v != 0), "the ADPCM-B stream sounded");
    }

    #[test]
    fn timer_a_counts_the_published_sample_period() {
        let mut s = chip();
        aad(&mut s, 0x24);
        adc(&mut s, 0xFF);
        aad(&mut s, 0x25);
        adc(&mut s, 0x03);
        aad(&mut s, 0x27);
        adc(&mut s, 0x05);
        // The writes land at the first boundary's head; the load edge
        // starts the timer at the register value and the count runs
        // per sample, so the published all-ones 1023 form with the
        // enable overflows on the first visit and reloads from the
        // register, per the keep-cycling form.
        tick(&mut s, Tick(0));
        assert_eq!(s.timer_a_reg, 0x3FF);
        assert!(s.timer_a_flag);
        assert_eq!(s.timer_a_cnt, 1023);
        // The write-one reset form clears the flag when it lands per
        // the published form; the load and the enable persist.
        aad(&mut s, 0x27);
        adc(&mut s, 0x15);
        run_sample(&mut s);
        assert!(!s.timer_a_flag, "the write-one reset form cleared it");
        // A write-zero form of the reset bits never clears anything.
        aad(&mut s, 0x27);
        adc(&mut s, 0x05);
        run_sample(&mut s);
        assert!(!s.timer_a_flag);
    }

    #[test]
    fn timer_b_counts_the_published_divider_form() {
        let mut s = chip();
        aad(&mut s, 0x26);
        adc(&mut s, 0xFC);
        aad(&mut s, 0x27);
        adc(&mut s, 0x0A);
        // The divider: one count per sixteen FM samples; the range
        // of 252 to 256 lands the overflow on the fourth count, so
        // the flag raises on the sixty-fourth sample.
        for i in 0..63u64 {
            tick(&mut s, Tick(i));
            assert!(!s.timer_b_flag, "no flag before the divider completes");
        }
        tick(&mut s, Tick(63));
        assert!(s.timer_b_flag, "the count completed");
    }

    #[test]
    fn the_timer_overflow_edges_the_irq_line_through_notify_only() {
        let mut s = chip();
        let edges = Rc::new(RefCell::new(Vec::new()));
        aad(&mut s, 0x24);
        adc(&mut s, 0xFF);
        aad(&mut s, 0x25);
        adc(&mut s, 0x03);
        aad(&mut s, 0x27);
        adc(&mut s, 0x05);
        for i in 0..4u64 {
            tick_irq(&mut s, Tick(i * 200), &edges);
        }
        // The overflow on the first visit raised the line through a
        // notify edge; the keep-cycling reload never moves the line
        // again (the flag re-raises inside the same window), so the
        // edge list holds the single rise.
        assert_eq!(&*edges.borrow(), &[true]);
    }

    #[test]
    fn snapshot_round_trips_byte_identical_and_rearms() {
        let mut s = chip_with_rom(vec![0x11u8; 0x100], vec![0x0Fu8; 0x100]);
        // The voice, the timers, the pair latches and a run of
        // samples: the phases, envelopes and counters advance.
        aad(&mut s, 0xA5);
        adc(&mut s, 0x08);
        aad(&mut s, 0xA1);
        adc(&mut s, 0x80);
        aad(&mut s, 0x24);
        adc(&mut s, 0x40);
        aad(&mut s, 0x27);
        adc(&mut s, 0x05);
        for (base, hi) in [
            (0x51u8, 0x1Fu8),
            (0x61, 0x2F),
            (0x71, 0x1F),
            (0x81, 0x0F),
            (0x91, 0x0A),
        ] {
            opw(&mut s, base, 0, 0, hi);
        }
        aad(&mut s, 0xB1);
        adc(&mut s, 0x07);
        aad(&mut s, 0xB5);
        adc(&mut s, 0xC0);
        aad(&mut s, 0x28);
        adc(&mut s, 0xF1);
        aad(&mut s, 0x22);
        adc(&mut s, 0x0D);
        // The ADPCM-A and ADPCM-B voices.
        bid(&mut s, 0x08);
        bdc(&mut s, 0xDF);
        bid(&mut s, 0x01);
        bdc(&mut s, 0x3F);
        bid(&mut s, 0x10);
        bdc(&mut s, 0x21);
        bid(&mut s, 0x18);
        bdc(&mut s, 0x1D);
        bid(&mut s, 0x20);
        bdc(&mut s, 0x22);
        bid(&mut s, 0x28);
        bdc(&mut s, 0x1D);
        bid(&mut s, 0x00);
        bdc(&mut s, 0x01);
        aad(&mut s, 0x10);
        adc(&mut s, 0x80);
        aad(&mut s, 0x11);
        adc(&mut s, 0xC0);
        for i in 0..3u64 {
            tick(&mut s, Tick(i));
        }
        let snap = s.snapshot();
        assert_eq!(snap.len(), OPN_SNAPSHOT_LEN);
        let mut twin = chip_with_rom(vec![0x11u8; 0x100], vec![0x0Fu8; 0x100]);
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip kept the bytes");
        assert!(twin.samples.is_empty());
        assert_eq!(twin.next, Some(Tick(0)));
        assert!(twin.last_tick.is_none());
        // The restored chip renders on its own registers: the voices
        // walked on from the restore point.
        for _ in 0..40 {
            run_sample(&mut twin);
        }
        assert!(
            twin.take_samples().iter().any(|v| *v != 0),
            "the restored voice rendered"
        );
    }

    #[test]
    fn restore_rejects_bad_chunks() {
        let mut s = chip();
        let snap = s.snapshot();
        assert!(matches!(
            s.restore(&snap[..snap.len() - 1]),
            Err(SnapshotError::Mismatch(_))
        ));
        s.restore(&snap).expect("restore");
        assert!(matches!(
            s.restore(&[0u8; OPN_SNAPSHOT_LEN]),
            Err(SnapshotError::Mismatch(_))
        ));
    }

    #[test]
    fn machine_renders_ym2610_samples_through_the_window() {
        let mut ctors = DeviceCtors::new();
        ctors.register(
            "ym2610.chip",
            Box::new(|d, c| {
                Ym2610::new(d, c)
                    .map(Box::new)
                    .map(|b: Box<Ym2610>| b as Box<dyn Device>)
                    .map_err(MachineError::Assembly)
            }),
        );
        let config = sys_config(boot_map(vec![0x11u8; 512], Vec::new()), 2);
        let mut machine = Machine::assemble(config, &ctors).expect("assemble");
        machine.boot();
        assert!(
            machine.device_by_name("ym0").is_some(),
            "assembly registered the chip"
        );
        // The chip through the wired window form: the ports decode
        // the low two address bits, so the address register rides
        // port 4 and the data register port 5.
        let base = PORT_BASE;
        // ADPCM-A: the total level and pans on, and the key-on with
        // the dump bit clear per the published process.
        assert!(machine.debug_write("sound", Address(base + 2), 0x01));
        assert!(machine.debug_write("sound", Address(base + 3), 0x3F));
        assert!(machine.debug_write("sound", Address(base + 2), 0x08));
        assert!(machine.debug_write("sound", Address(base + 3), 0xDF));
        assert!(machine.debug_write("sound", Address(base + 2), 0x00));
        assert!(machine.debug_write("sound", Address(base + 3), 0x01));
        machine.run_until(Tick(9000));
        let first = drain(&mut machine);
        assert!(
            first.iter().any(|v| *v != 0),
            "the chip rendered through the window"
        );
        // A second window run keeps the render walking.
        machine.run_until(Tick(16000));
        let second = drain(&mut machine);
        assert!(!second.is_empty(), "the second drain renders on");
    }

    /// Drain the chip's samples through the machine slot.
    fn drain(machine: &mut Machine) -> Vec<i16> {
        machine
            .device_by_name_mut("ym0")
            .expect("device")
            .as_any_mut()
            .downcast_mut::<Ym2610>()
            .expect("the chip")
            .take_samples()
    }

    /// Drive one fresh machine through the same port script and
    /// collect its samples.
    fn drive_deterministic_run() -> Vec<i16> {
        let mut ctors = DeviceCtors::new();
        ctors.register(
            "ym2610.chip",
            Box::new(|d, c| {
                Ym2610::new(d, c)
                    .map(Box::new)
                    .map(|b: Box<Ym2610>| b as Box<dyn Device>)
                    .map_err(MachineError::Assembly)
            }),
        );
        let config = sys_config(boot_map(vec![0x11; 512], vec![0x0F; 512]), 2);
        let mut machine = Machine::assemble(config, &ctors).expect("assemble");
        machine.boot();
        // The ADPCM-A key-on form plus a sustained FM voice, at the
        // wired window ports, in fixed order.
        let base = PORT_BASE;
        for (reg, data) in [
            (0x01u8, 0x3Fu8),
            (0x08, 0xDF),
            (0x10, 0x00),
            (0x18, 0x00),
            (0x00, 0x01),
        ] {
            assert!(machine.debug_write("sound", Address(base + 2), reg));
            assert!(machine.debug_write("sound", Address(base + 3), data));
        }
        machine.run_until(Tick(12000));
        drain(&mut machine)
    }

    #[test]
    fn the_same_machine_run_twice_sound_identical() {
        let first = drive_deterministic_run();
        let second = drive_deterministic_run();
        assert!(!first.is_empty(), "the first run rendered");
        assert_eq!(first, second, "the identical run sounds identical");
        assert!(first.iter().any(|v| *v != 0), "the run carried sound");
    }
}
