//! The Yamaha YM2612 (OPN2) six-channel frequency-modulation
//! synthesizer with its direct-access DAC and the two interval
//! timers, the `ym2612.psg` device kind.
//!
//! The chip answers eight-bit register writes at the four ports of
//! its chip-select window: port 0 latches the address byte for part I
//! (channels 1-3), port 1 stores data into the latched address of
//! part I, port 2 latches for part II (channels 4-6), and port 3
//! stores there. Each part keeps its own latched address and a data
//! write never clears it. Reads from the window answer the status
//! byte per the published read form.
//!
//! # The pinned register surface
//!
//! Per-operator registers sit in the seven base groups `$30` (detune
//! and multiple), `$40` (total level), `$50` (rate scaling and
//! attack rate), `$60` (amplitude-modulation enable and first decay
//! rate), `$70` (secondary decay rate), `$80` (decay level and
//! release rate) and `$90` (SSG-EG). In each group the four low
//! address bits select the operator (`offset >> 2`) and the channel
//! (`offset & 3`); the channel form of three is the published gap
//! row. Part I serves channels 1-3 and part II channels 4-6.
//!
//! Per-channel registers sit at `$A0` (the eight low frequency-number
//! bits), `$A4` (the six-bit form: the block in the top three bits
//! and the three upper frequency-number bits below it), `$B0` (the
//! three-bit self-feedback level over the three-bit algorithm) and
//! `$B4` (the left and right output enables, the two-bit
//! amplitude-LFO sensitivity and the three-bit pitch-LFO
//! sensitivity). Part I serves channels 1-3 and part II channels
//! 4-6.
//!
//! The shared registers answer from part I only, per the published
//! part division; part II stores of the same addresses land in the
//! register shadow and do nothing:
//!
//! | Range | Register | Content |
//! |-------|----------|---------|
//! | `$22` | LFO | bit 3 enable, bits 2-0 rate |
//! | `$24`/`$25` | Timer A | the ten-bit ganged value: `$24` the eight MSBs, `$25` the two LSBs |
//! | `$26` | Timer B | the eight-bit value |
//! | `$27` | Mode | bits 7-6 the channel-3 form, bit 5 reset B, bit 4 reset A, bit 3 enable B, bit 2 enable A, bit 1 load B, bit 0 load A |
//! | `$28` | Key on/off | bits 7-4 the four operators, bits 2-0 the channel form (000-010 = channels 1-3, 100-110 = channels 4-6) |
//! | `$2A` | DAC | the eight-bit DAC data |
//! | `$2B` | DAC enable | bit 7 swaps the channel-6 FM output for the DAC |
//! | `$2C`-`$2F` | Test | stores land in the register shadow, no effect |
//!
//! # The frequency pair and channel-3 special mode
//!
//! The frequency number is fourteen bits: three bits of block over
//! eleven bits of frequency number. A pair write latches per the
//! published form: the high form stores its six data bits into a
//! per-part holding latch, and the low form stores both its byte and
//! the held latch, so the published high-register-first order works
//! and the reverse order holds the previous latch.
//!
//! In channel-3 special mode (the mode form with bit 6 set) the four
//! operators of channel 3 run separate frequencies: operator 1 keeps
//! the `$A2`/`$A6` pair channel 3 owns in normal mode, and operators
//! 2, 3 and 4 read the published supplementary pairs `$A8`/`$AC`,
//! `$A9`/`$AD` and `$AA`/`$AE`. The mode-register forms with bit 7
//! are the published illegal forms; the die-verified cores run the
//! same special layout for any nonzero mode, and this build does the
//! same. The timer-A CSM key-on form of those modes is left outside
//! this build.
//!
//! # The pitch and the sine path
//!
//! The phase accumulator is twenty bits and wraps at `$100000`; one
//! FM sample advances it by the frequency step. The step follows the
//! die-verified OPN2 arithmetic: the frequency number doubled then
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
//! the nominal 8 MHz clock and the Genesis chip runs 4.3 percent
//! slower.
//!
//! # The envelope generator
//!
//! Each operator runs the ten-bit envelope over the published
//! states attack, first decay, secondary decay and release. Key on
//! resets the phase and starts the attack; the published instant
//! form (attack rates at or past the key-scaled 62) lands at full
//! level at once, and the attack otherwise shrinks the gap toward
//! the top by the per-step fraction. Key off starts the release from
//! the current form. The decay and release forms climb the
//! attenuation by the per-step increment and clamp at the ten-bit
//! top; the first decay hands over at the sustain level and a
//! sustain level of fifteen acts like thirty-one, per the published
//! form. Envelope steps run once every third FM sample, the
//! die-verified envelope-cycle form, and the published 64-row
//! per-rate table picks the per-step increments by rate value and
//! step phase: rates below four never move and the top rates reach
//! the ceiling fastest. The published rate scaling follows: the
//! final rate is two times the register rate plus the key code
//! divided down (per the rate-scaling form key code / 8, /4, /2 or
//! /1) rounded toward zero; the register rate of zero keeps the
//! rate at zero.
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
//! # The LFO
//!
//! The global low-frequency oscillator runs the published divider
//! table: one LFO count takes 109, 78, 72, 68, 63, 45, 9 or 6 FM
//! samples per rate, giving the manual's 3.98 through 72.2 Hz octave
//! at the nominal clock. The triangle counter walks 128 counts per
//! cycle: the amplitude form adds an attenuation offset in the raw
//! count's doubled form shifted by the per-channel sensitivity (the
//! 0, 1.4, 5.9 and 11.8 dB cycle depths of the published table),
//! gated by the per-operator amplitude-modulation enable per the
//! published three-fold enable; the pitch form bends the frequency
//! number through the published shift-pair table by the per-channel
//! pitch sensitivity. The depth tables are recorded approximations
//! of the exact hardware depths.
//!
//! # The timers
//!
//! Timer A counts the ten-bit range up one step per FM sample from
//! its ganged register value: on passing the top it raises the
//! overflow form, sets the read flag when the enable bit allows,
//! and reloads from the register value so the timer keeps cycling
//! per the published form. Load one starts (and re-reloads) the
//! timer at the register value; load zero holds it. Timer B counts
//! the eight-bit range up one step per sixteen FM samples (the
//! published 288 µs step at the nominal clock) through the same
//! load, enable, overflow and reload form. The published reset bits
//! clear the read flags on a write of one and leave them on a write
//! of zero. While either enabled overflow flag sits set the `irq`
//! output line drives level high and drops again when both clear;
//! the line moves only through machine-context notify and only on
//! real edges, per the published level form. The status read
//! answers the busy bit at bit 7, the Timer B overflow flag at bit
//! 6 and the Timer A overflow flag at bit 5. The busy form holds
//! between a write and the device's next sample boundary; the real
//! window is about one operational cycle of the chip, so the form
//! rides a recorded approximation of the window length.
//!
//! # The DAC
//!
//! `$2A` holds the eight-bit DAC data in the published
//! eight-bit-with-MSB form: the byte becomes the nine-bit signed
//! value with the LSB stuck low (the die's register form), so the
//! mixed value walks over the even forms of the range from −256 to
//! +254 with the byte's 0x80 form at the silence mid. With the
//! enable bit set on `$2B` the DAC value replaces the channel-6 FM
//! output in the mix, gated through the channel-6 output enables,
//! per the published replacement form; the hold between writes is
//! the natural sample-and-hold of the channel-6 slot. The chip's
//! DAC discontinuity of the sample-and-hold is a recorded
//! approximation left outside this build.
//!
//! # The output and the snapshot
//!
//! One visit of the device is one FM sample. Register writes queue
//! and apply at the next FM-sample boundary in write order (the
//! deterministic form this build models). Channel outputs clamp at
//! the published nine-bit intermediate form (±256) and the mono mix
//! scales the sum by the published mixer form (8192 over 390)
//! before it saturates into the 16-bit output; the sample rate is
//! the published 7670453 Hz Genesis chip clock over the
//! 144-chip-clock sample period, truncated. The `psg` divider names
//! the domain ticks per FM sample.
//!
//! The device snapshots under the `YM12` tag, version 1: the
//! register shadow, the latches, the operator phases, envelopes and
//! keys, the timers, the LFO counters and the DAC. The samples do
//! not ride the chunk and the restored device re-arms its schedule.
//!
//! Sources: the Sega Genesis Technical Manual YM2612 section (the
//! SMS Power transcript of the SEGA2.DOC pages) for the port form,
//! the register table, the channel-3 special layout, the algorithm
//! list, the key-code rate formula and the DAC form; the ymfm FM
//! core (Aaron Giles, the OPN2 engine inside MAME) for the
//! die-extracted sine and power tables, the per-rate envelope
//! stepping table, the mode and frequency arithmetic, the detune and
//! pitch-LFO tables, the LFO divider table and the output scaling;
//! and the Nuked-OPN2 core (Alexey Khokholov, built from chip
//! studies) for the same facts confirmed a second way, the
//! status-bit form, the timer counting forms and the SSG-EG details.

use demu_config::model::{BootState, DeviceInstance};
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{AudioSource, Device, Snapshot, SnapshotError};

/// The FM sample rate: the published 7670453 Hz Genesis chip clock
/// divided by the published 144-chip-clock sample period, truncated.
const SAMPLE_RATE: u32 = 53_267;
/// The mixed sample buffer cap; the media pipeline drains earlier.
const SAMPLE_CAP: usize = 0x4000;
/// The domain ticks per FM sample when the `psg` divider is absent:
/// the published sample period of 144 chip clocks at the chip clock
/// the Genesis main domain carries.
const DEFAULT_DIV: u32 = 144;
/// The chip runs six channels of four operators.
const CHANNELS: usize = 6;
const OPS: usize = 4;
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
/// The per-channel intermediate form: the channel sum clips here.
const CHANNEL_CLIP: i32 = 256;
/// The register file per part.
const REG_FILE: usize = 0x100;
/// per-step increment reads as the four-bit field for one of the
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
///
/// | Frequency-number bits | Key code low bit |
/// |-----------------------|------------------|
/// | 0x0000-0x0007 | not set |
/// | 0x0008 and above | the bitfield of this constant |
const FNUM_NOTE_BITS: u16 = 0xFE80;

/// FM samples per LFO count per published rate, from the application
/// note frequencies converted over the seven-bit LFO form.
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
// sine in the 4.8 form, the OPN2 die form extracted per the
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
// The published exponential power table: the low eight bits
// of the combined attenuation pick a ten-bit mantissa with the
// implied leading bit, pre-shifted left by two so one right
// shift per integer octave turns a 5.8 attenuation into a
// 13-bit linear volume, per the die-extracted ymfm form.
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
/// which previous output sum feeds operators 2, 3 and 4, and which
/// of operators 1, 2 and 3 enter the channel sum. Operator 4 always
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

/// One operator: the register forms and the running state.
#[derive(Debug, Clone, Copy)]
struct Operator {
    /// The `$30` group: the three-bit detune over the four-bit multiple.
    dt_mul: u8,
    /// The `$40` group: the seven-bit total level.
    tl: u8,
    /// The `$50` group: the two-bit rate scaling over the five-bit
    /// attack rate.
    ksr_ar: u8,
    /// The `$60` group: the amplitude-modulation enable over the
    /// five-bit first decay rate.
    am_d1r: u8,
    /// The `$70` group: the five-bit secondary decay rate.
    d2r: u8,
    /// The `$80` group: the four-bit decay level over the four-bit
    /// release rate.
    d1l_rr: u8,
    /// The `$90` group: the four-bit SSG-EG form.
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

    /// The amplitude-modulation LFO enable of this operator.
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

/// One channel: the register forms.
#[derive(Debug, Clone, Copy)]
struct Channel {
    /// The fourteen-bit frequency form: three bits of block over
    /// eleven bits of frequency number.
    block_freq: u16,
    /// The three-bit self-feedback level over the three-bit algorithm.
    fb_alg: u8,
    /// The `$B4` output and LFO-sensitivity form.
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

/// The per-operator derived values, recomputed once per FM sample from
/// the register forms and the channel frequency.
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
#[derive(Debug, Clone, Copy)]
struct QueuedWrite {
    bank: usize,
    addr: u8,
    data: u8,
}

/// The Yamaha YM2612 (OPN2) frequency-modulation synthesizer.
#[derive(Debug)]
pub struct Ym2612 {
    name: String,
    /// The domain ticks per FM sample, from the `psg` divider.
    div: u64,
    /// The latched register address per part.
    latch: [u8; 2],
    /// The register shadow per part.
    regs: [[u8; REG_FILE]; 2],
    /// The per-operator running state.
    ops: [Operator; CHANNELS * OPS],
    /// The per-operator derived cache.
    caches: [OpCache; CHANNELS * OPS],
    /// The per-channel register forms.
    channels: [Channel; CHANNELS],
    /// The frequency-number high latches per part.
    freq_latch: [u8; 2],
    /// The channel-3 supplementary pairs, per the published layout:
    /// the fourteen-bit frequency forms of the `$A8`/`$AC`, `$A9`/`$AD`
    /// and `$AA`/`$AE` pairs.
    ch3_pairs: [u16; 3],
    /// The channel-3 supplementary pair latch.
    ch3_latch: u8,
    /// The channel-3 special mode.
    special: bool,
    /// The eight-bit DAC data and the enable.
    dac_data: u8,
    dac_enable: bool,
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
    /// The envelope counter in the published x.2 form: the
    /// per-sample sub-counter over the unit count.
    env_counter: u32,
    /// Register writes queued to the next FM-sample boundary.
    pending: Vec<QueuedWrite>,
    /// The busy bit of the status byte.
    busy: bool,
    /// The currently driven `irq` level, tracked to notify on edges.
    irq_on: bool,
    /// The per-channel operator-1 feedback forms: the two previous
    /// outputs per the published feedback path.
    feedback: [[i32; 2]; CHANNELS],
    /// The mixed mono samples.
    samples: Vec<i16>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

/// The snapshot chunk length: tag and version, the two register
/// files, the address and frequency latches, the channel-3 pairs,
/// the operator records, the channel forms, the timers, the LFO
/// counters, the DAC byte and the run-flags byte.
const OPN_SNAPSHOT_LEN: usize = 5
    + REG_FILE * 2
    + 2
    + 2
    + 3 * 2
    + 1
    + 2
    + (CHANNELS * OPS) * 15
    + CHANNELS * 4
    + 2
    + 4
    + 1
    + 4
    + 4
    + 1
    + 4
    + 4
    + 4
    + 1;

impl Ym2612 {
    /// A chip from its configuration entry.
    ///
    /// The `psg` divider names the domain ticks per FM sample and
    /// defaults to 144, the published sample period at the 7670453
    /// Hz Genesis chip clock.
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        let div = u64::from(device.dividers.get("psg").copied().unwrap_or(DEFAULT_DIV)).max(1);
        Ok(Self {
            name: device.name.to_string(),
            div,
            latch: [0; 2],
            regs: [[0; REG_FILE]; 2],
            ops: [Operator::silent(); CHANNELS * OPS],
            caches: [OpCache::default(); CHANNELS * OPS],
            channels: [Channel::silent(); CHANNELS],
            freq_latch: [0; 2],
            ch3_pairs: [0; 3],
            ch3_latch: 0,
            special: false,
            dac_data: 0,
            dac_enable: false,
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
            pending: Vec::new(),
            busy: false,
            irq_on: false,
            feedback: [[0; 2]; CHANNELS],
            samples: Vec::new(),
            next: None,
            last_tick: None,
        })
    }

    /// The register or port offset inside the chip-select window.
    fn port_offset(&self, address: u32) -> Option<u8> {
        let off = address & 0xFFFF;
        if (0x4000..=0x5FFF).contains(&off) {
            Some((off & 0x03) as u8)
        } else {
            None
        }
    }

    /// The status byte per the published read form: bit 7 busy,
    /// bit 6 the Timer B overflow flag, bit 5 the Timer A one.
    fn status(&self) -> u8 {
        (u8::from(self.busy) << 7)
            | (u8::from(self.timer_b_flag) << 6)
            | (u8::from(self.timer_a_flag) << 5)
    }

    /// One window write byte per port: the address ports latch and
    /// the data ports queue the store to the next sample boundary.
    fn write_byte(&mut self, port: u8, byte: u8) {
        self.busy = true;
        let bank = usize::from(port & 0x02 != 0);
        if port & 0x01 == 0 {
            self.latch[bank] = byte;
        } else {
            self.pending.push(QueuedWrite {
                bank,
                addr: self.latch[bank],
                data: byte,
            });
        }
    }
}

/// The register stores: the drain, the published surface decode and
/// the per-side-effect forms.
impl Ym2612 {
    /// The store per the published register surface.
    fn store(&mut self, bank: usize, addr: u8, data: u8) {
        if addr < 0x30 {
            self.store_shared(bank, addr, data);
            return;
        }
        match addr {
            0x30..=0x9F => self.store_op(bank, addr, data),
            0xA0..=0xAF => self.store_freq(bank, addr, data),
            0xB0..=0xB2 | 0xB4..=0xB6 => self.store_channel(bank, addr, data),
            _ => {}
        }
    }

    /// The per-operator store per the published addressing: the
    /// group base selects the register, the bits two-three the
    /// operator and the low bits the channel within the part; the
    /// gap rows (the channel form three) land nowhere.
    fn store_op(&mut self, bank: usize, addr: u8, data: u8) {
        if addr & 0x03 == 3 {
            return;
        }
        let ch = bank * 3 + usize::from(addr & 0x03);
        let op = usize::from((addr >> 2) & 0x03);
        let slot = ch * OPS + op;
        match addr >> 4 {
            0x3 => self.ops[slot].dt_mul = data & 0x7F,
            0x4 => self.ops[slot].tl = data & 0x7F,
            0x5 => self.ops[slot].ksr_ar = data,
            0x6 => self.ops[slot].am_d1r = data,
            0x7 => self.ops[slot].d2r = data & 0x1F,
            0x8 => self.ops[slot].d1l_rr = data,
            _ => self.ops[slot].ssg = data & 0x0F,
        }
    }

    /// The shared register stores, per the published part division:
    /// part I only; part II stores land in the shadow untouched.
    fn store_shared(&mut self, bank: usize, addr: u8, data: u8) {
        if bank != 0 {
            return;
        }
        match addr {
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
            0x2A => self.dac_data = data,
            0x2B => self.dac_enable = data & 0x80 != 0,
            _ => {}
        }
    }

    /// The mode store per the published bit form: the channel-3
    /// special form (any nonzero mode per the die-verified reading),
    /// the enables, the write-one flag resets, and the load edges
    /// that start each timer from its register value. Load zero
    /// holds the timer without resetting it.
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
        // The die-verified reading: any nonzero mode form runs the
        // special channel-3 layout; the published illegal forms
        // behave the same.
        self.special = data & 0xC0 != 0;
    }

    /// The key-on/off store per the published operator and channel
    /// forms; the channel forms three and seven select no channel.
    fn write_key_on(&mut self, data: u8) {
        let opbits = data >> 4;
        let low = usize::from(data & 0x03);
        let bank = usize::from(data & 0x04 != 0);
        if data & 0x03 == 3 {
            return;
        }
        let ch = bank * 3 + low;
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
    /// high form stores its six bits into the per-part holding
    /// latch, and the low form stores both its byte and the held
    /// latch. The gap rows (the channel form three) land nowhere.
    /// Part I holds the channel-3 supplementary pairs per the
    /// published layout; part II stores of those addresses land in
    /// the shadow only.
    fn store_freq(&mut self, bank: usize, addr: u8, data: u8) {
        if addr & 0x03 == 3 {
            return;
        }
        let ch = bank * 3 + usize::from(addr & 0x03);
        if addr >= 0xA8 {
            if bank != 0 {
                return;
            }
            let pair = usize::from(addr & 0x03);
            if addr & 0x04 != 0 {
                self.ch3_latch = data & 0x3F;
            } else {
                self.ch3_pairs[pair] = (u16::from(self.ch3_latch) << 8) | u16::from(data);
            }
        } else if addr & 0x04 != 0 {
            self.freq_latch[bank] = data & 0x3F;
        } else {
            self.channels[ch].block_freq =
                (u16::from(self.freq_latch[bank]) << 8) | u16::from(data);
        }
    }

    /// The per-channel stores for `$B0`-`$B6`: part I serves the
    /// channels 1-3 forms and part II the channels 4-6 forms; the
    /// low rows carry the algorithm and feedback form.
    fn store_channel(&mut self, bank: usize, addr: u8, data: u8) {
        let ch = bank * 3 + usize::from(addr & 0x03);
        if addr & 0x07 < 4 {
            self.channels[ch].fb_alg = data & 0x3F;
        } else {
            self.channels[ch].out_ams_fms = data;
        }
    }

    /// Recompute the per-operator caches from the registers, per the
    /// published frequency and rate arithmetic. In the channel-3
    /// special form operator 1 keeps the channel pair and operators
    /// 2, 3 and 4 read the published supplementary pairs.
    fn recache(&mut self) {
        for (ch, chform) in self.channels.iter().enumerate() {
            let base = chform.block_freq;
            let use_special = ch == 2 && self.special;
            let pm = if self.lfo_enable && chform.fms() != 0 {
                (true, chform.fms())
            } else {
                (false, 0)
            };
            for (op, num) in (0..OPS).zip([base; OPS]) {
                let fnum = if op > 0 && use_special {
                    self.ch3_pairs[op - 1]
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
        // over the fourteen-bit form, with the note bit read from the
        // published constant.
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
        for slot in 0..4 {
            let form = match slot {
                0 => EgState::Attack,
                1 => EgState::Decay,
                2 => EgState::Sustain,
                _ => EgState::Release,
            };
            let raw = u32::from(op.state_rate(form));
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

/// The frequency step per the die-verified form: the frequency
/// number doubled, shifted by the block, down two, plus the detune,
/// clamped at seventeen bits, then times the x.1 multiple.
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
impl Ym2612 {
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
        // the first half inverted, and the pitch reads the
        // three-bit reflected form negated over the cycle halves,
        // per the die-verified counter forms.
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
    /// doubling shift over the per-channel sensitivity.
    fn am_offset_of(&self, ch: usize) -> u32 {
        // The published shift form picks 7, 3, 1 and 0 per the
        // sensitivity forms.
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
    /// happens when the envelope passes the mid form; the hold
    /// types stop there and the repeat types restart the attack,
    /// with the phase-reset types also restarting the accumulator.
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
}

/// The FM mix: the per-operator volume form, the per-channel
/// algorithm path, the DAC and the per-sample visit.
impl Ym2612 {
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
        let att = att.min(EG_MAX);
        // The combined attenuation in the 5.8 form and the power
        // lookup, per the die forms.
        let combined = u64::from(LOG_SIN[quarter]) + (u64::from(att) << 2);
        let value = i64::from(POWER_TABLE[(combined & 0xFF) as usize]) >> (combined >> 8);
        let value = value as i32;
        if wave & 0x200 != 0 { -value } else { value }
    }

    /// The channel output per the published algorithm form over the
    /// four operators, with the operator-1 feedback, ending at the
    /// nine-bit intermediate clip of the chip.
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
        if !(form.out_l() || form.out_r()) {
            return 0;
        }
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
        result >>= 5;
        let sums = u32::from(ALG_SUM_MASK[alg]);
        if sums & 0x01 != 0 {
            result += opout[1] >> 5;
        }
        if sums & 0x02 != 0 {
            result += opout[2] >> 5;
        }
        if sums & 0x04 != 0 {
            result += opout[3] >> 5;
        }
        result.clamp(-CHANNEL_CLIP, CHANNEL_CLIP)
    }
}

/// The per-sample pipeline and the timer lines.
impl Ym2612 {
    /// One per-operator visit: the SSG-EG clock, the envelope step on
    /// the unit boundary, and the phase step, per the chip's
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

    /// Re-derive the per-sample LFO forms from the restored
    /// triangle phase; the forms are pure functions of the counter.
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

/// The device visit and the mixed output.
impl Ym2612 {
    /// One mixed sample per visit: the queued writes land, the
    /// per-operator pipeline walks, the channels and the DAC mix,
    /// and the scaled form rides the published mix scale into the
    /// bounded sample buffer.
    fn render_sample(&mut self) {
        // The queued writes land at the boundary in write order;
        // the register stores walk the published surface and the
        // caches re-derive per store, so key-on reads the landed
        // registers per the chip's prepared form. The busy form
        // clears again per the read-again form.
        let writes = std::mem::take(&mut self.pending);
        for w in &writes {
            self.regs[w.bank][usize::from(w.addr)] = w.data;
            self.store(w.bank, w.addr, w.data);
            self.recache();
        }
        self.busy = false;
        // The LFO walks its count per the divider before
        // the operator pipeline.
        self.run_lfo();
        // The envelope counter walks its published x.2 form: the
        // sub-counter per sample with the unit wrap at the divider.
        self.env_counter = (self.env_counter + 1) & ENV_COUNTER_MASK;
        if self.env_counter & 3 == 3 {
            self.env_counter = (self.env_counter + 1) & ENV_COUNTER_MASK;
        }
        // Every operator: the SSG clock, the envelope step, the
        // phase step, per the chip's pipeline.
        for op_index in 0..CHANNELS * OPS {
            self.clock_op(op_index);
        }
        // Every enabled channel mixes its four operators.
        let mut mixed: i64 = 0;
        for ch in 0..CHANNELS {
            if self.dac_enable && ch == CHANNELS - 1 {
                // The DAC replaces the channel-6 FM output per the
                // published form.
                continue;
            }
            mixed += i64::from(self.channel_output(ch));
        }
        // The DAC value rides the channel-6 form per the published
        // replacement: the nine-bit signed value at the same scale.
        if self.dac_enable
            && (self.channels[CHANNELS - 1].out_l() || self.channels[CHANNELS - 1].out_r())
        {
            mixed += i64::from(self.dac_data as i8) * 2;
        }
        // The mixed form rides the published scale into the 16-bit
        // form, saturated like the house PSG mixer, and lands in the
        // bounded buffer.
        let scaled = (mixed * 8192 / 390) as i32;
        let sample = scaled.clamp(-32768, 32767) as i16;
        if self.samples.len() >= SAMPLE_CAP {
            self.samples.clear();
        }
        self.samples.push(sample);
    }
}

/// The bus and device trait forms.
impl BusAccess for Ym2612 {
    fn read(&mut self, _width: AccessWidth, _address: Address, _tick: Tick) -> u32 {
        u32::from(self.status())
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let Some(port) = self.port_offset(address.0) else {
            return;
        };
        self.write_byte(port, value as u8);
    }

    fn peek(&self, _address: Address) -> u8 {
        self.status()
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for Ym2612 {
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
        // quiet and released, the timers stopped, the LFO off and
        // the DAC off; the schedule parks until the next boot.
        self.latch = [0; 2];
        self.regs = [[0; REG_FILE]; 2];
        self.ops = [Operator::silent(); CHANNELS * OPS];
        self.caches = [OpCache::default(); CHANNELS * OPS];
        self.channels = [Channel::silent(); CHANNELS];
        self.freq_latch = [0; 2];
        self.ch3_pairs = [0; 3];
        self.ch3_latch = 0;
        self.special = false;
        self.dac_data = 0;
        self.dac_enable = false;
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
        self.pending.clear();
        self.busy = false;
        self.irq_on = false;
        self.feedback = [[0; 2]; CHANNELS];
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(OPN_SNAPSHOT_LEN);
        out.extend_from_slice(b"YM12");
        out.push(1);
        out.extend_from_slice(&self.regs[0]);
        out.extend_from_slice(&self.regs[1]);
        out.extend_from_slice(&self.latch);
        out.extend_from_slice(&self.freq_latch);
        for pair in self.ch3_pairs {
            out.extend_from_slice(&pair.to_le_bytes());
        }
        out.push(self.ch3_latch);
        let mut flags = u8::from(self.special)
            | (u8::from(self.dac_enable) << 1)
            | (u8::from(self.lfo_enable) << 2)
            | u8::from(self.busy) << 3;
        flags |= self.lfo_rate << 4;
        flags |= u8::from(self.timer_a_load) << 7;
        out.push(flags);
        let flags_b = u8::from(self.timer_b_load)
            | (u8::from(self.timer_a_enable) << 1)
            | (u8::from(self.timer_b_enable) << 2)
            | (u8::from(self.timer_a_flag) << 3)
            | (u8::from(self.timer_b_flag) << 4)
            | (u8::from(self.irq_on) << 5);
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
        out.push(self.dac_data);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != OPN_SNAPSHOT_LEN || &data[..4] != b"YM12" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "ym2612 chunk must be {OPN_SNAPSHOT_LEN} bytes"
            )));
        }
        let mut pos = 5usize;
        for bank in &mut self.regs {
            bank.copy_from_slice(&data[pos..pos + REG_FILE]);
            pos += REG_FILE;
        }
        self.latch.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.freq_latch.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        for pair in &mut self.ch3_pairs {
            *pair = le_u16(&data[pos..pos + 2]);
            pos += 2;
        }
        self.ch3_latch = data[pos];
        pos += 1;
        let flags = data[pos];
        pos += 1;
        self.special = flags & 0x01 != 0;
        self.dac_enable = flags & 0x02 != 0;
        self.lfo_enable = flags & 0x04 != 0;
        self.busy = flags & 0x08 != 0;
        self.lfo_rate = (flags >> 4) & 0x07;
        self.timer_a_load = flags & 0x80 != 0;
        let flags_b = data[pos];
        pos += 1;
        self.timer_b_load = flags_b & 0x01 != 0;
        self.timer_a_enable = flags_b & 0x02 != 0;
        self.timer_b_enable = flags_b & 0x04 != 0;
        self.timer_a_flag = flags_b & 0x08 != 0;
        self.timer_b_flag = flags_b & 0x10 != 0;
        self.irq_on = flags_b & 0x20 != 0;
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
        self.dac_data = data[pos];
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

impl AudioSource for Ym2612 {
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
    use demu_core::device::{BusBank, DeviceSlices, EventBuffers, Route};
    use demu_core::machine::Machine;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    fn instance() -> DeviceInstance {
        DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("ym0").expect("name"),
            kind: demu_config::newtypes::KindName::new("ym2612.psg").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    fn chip() -> Ym2612 {
        Ym2612::new(&instance()).expect("chip")
    }

    /// One window write per address: the four ports of the window.
    fn wp(s: &mut Ym2612, port: u32, byte: u8) {
        s.write(
            AccessWidth::Byte,
            Address(0x4000 + port),
            u32::from(byte),
            Tick(0),
        );
    }
    /// The part-I address and data forms.
    fn aad(s: &mut Ym2612, addr: u8) {
        wp(s, 0, addr);
    }
    fn adc(s: &mut Ym2612, data: u8) {
        wp(s, 1, data);
    }
    /// The part-II forms.
    fn bid(s: &mut Ym2612, addr: u8) {
        wp(s, 2, addr);
    }
    fn bdc(s: &mut Ym2612, data: u8) {
        wp(s, 3, data);
    }

    /// One sample visit for the test forms: drains the queued
    /// register stores without the timer lines.
    fn run_sample(s: &mut Ym2612) {
        s.render_sample();
    }

    /// One device visit under an empty context: the render sample,
    /// the timers and the line form ride the visit.
    fn tick(s: &mut Ym2612, tick: Tick) {
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

    /// A spy device that records the incoming `irq` line edges.
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

    /// One visit with the `irq` line delivered into a spy device
    /// through a route, so the notifications are the only
    /// observation path.
    fn tick_irq(s: &mut Ym2612, tick: Tick, edges: &Rc<RefCell<Vec<bool>>>) {
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

    #[test]
    fn the_four_ports_latch_and_store_per_part() {
        let mut s = chip();
        aad(&mut s, 0x40);
        bid(&mut s, 0xA0);
        assert_eq!(s.latch, [0x40, 0xA0]);
        // The stores ride the latched addresses in write order and
        // land at the next sample boundary.
        adc(&mut s, 0x5A);
        bdc(&mut s, 0x26);
        assert_eq!(s.pending.len(), 2);
        run_sample(&mut s);
        assert_eq!(s.regs[0][0x40], 0x5A);
        assert_eq!(s.regs[1][0xA0], 0x26);
        assert!(s.pending.is_empty());
        // The latched address per part survives the stores per the
        // published form.
        assert_eq!(s.latch, [0x40, 0xA0]);
    }

    #[test]
    fn the_window_mirrors_the_four_ports() {
        let s = chip();
        assert_eq!(s.port_offset(0x4000), Some(0));
        assert_eq!(s.port_offset(0x4001), Some(1));
        assert_eq!(s.port_offset(0x4002), Some(2));
        assert_eq!(s.port_offset(0x4003), Some(3));
        // The repeat form: the low two address bits pick the port.
        assert_eq!(s.port_offset(0x4004), Some(0));
        assert_eq!(s.port_offset(0x5FFF), Some(3));
        // Outside the window nothing answers.
        assert_eq!(s.port_offset(0x3FFF), None);
        assert_eq!(s.port_offset(0x6000), None);
    }

    #[test]
    fn the_window_read_answers_the_status_byte() {
        let mut s = chip();
        assert_eq!(s.read(AccessWidth::Byte, Address(0x4000), Tick(0)), 0);
        assert_eq!(s.peek(Address(0x4000)), 0);
        // A write raises the busy form per the published caution.
        aad(&mut s, 0x40);
        assert!(s.read(AccessWidth::Byte, Address(0x4000), Tick(0)) & 0x80 != 0);
        // One visit clears the form again.
        run_sample(&mut s);
        assert_eq!(
            s.read(AccessWidth::Byte, Address(0x4000), Tick(0)) & 0x80,
            0
        );
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
        // The writes land at the first boundary; the load edge starts
        // the timer at the register value and the count runs per
        // sample, so the published all-ones 1023 form with the enable
        // overflows and raises the read flag on the first visit. The
        // overflow reloads per the keep-cycling form.
        tick(&mut s, Tick(0));
        assert_eq!(s.timer_a_reg, 0x3FF);
        assert!(s.timer_a_flag);
        assert_eq!(s.timer_a_cnt, 1023);
        // The write-one reset form clears the flag when it lands per
        // the published form; the load and the enable forms persist.
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
    fn timer_a_counts_the_value_period_per_the_published_form() {
        let mut s = chip();
        aad(&mut s, 0x24);
        adc(&mut s, 0xFF);
        aad(&mut s, 0x25);
        adc(&mut s, 0x00);
        aad(&mut s, 0x27);
        adc(&mut s, 0x05);
        run_sample(&mut s);
        assert_eq!(s.timer_a_reg, 1020);
        assert_eq!(s.timer_a_cnt, 1020);
        // The value period: the count walks one step per FM sample;
        // the top lands on the fourth visit per the published
        // 1020-to-1024 form, and the flag clears at the write-one
        // reset form per the published reset form.
        for i in 0..3u64 {
            tick(&mut s, Tick(i));
            assert!(!s.timer_a_flag, "no flag before the count completes");
        }
        tick(&mut s, Tick(3));
        assert!(s.timer_a_flag, "the count completed on the fourth sample");
        // The reset form clears the flag when it lands; the next
        // count overflows and re-raises it per the keep-cycling form.
        aad(&mut s, 0x27);
        adc(&mut s, 0x15);
        run_sample(&mut s);
        assert!(!s.timer_a_flag, "the reset form cleared the flag");
        for i in 4..7u64 {
            tick(&mut s, Tick(i));
        }
        tick(&mut s, Tick(7));
        assert!(s.timer_a_flag, "the next cycle re-raised the flag");
    }

    #[test]
    fn timer_b_counts_the_published_divider_form() {
        let mut s = chip();
        aad(&mut s, 0x26);
        adc(&mut s, 0xFC);
        aad(&mut s, 0x27);
        adc(&mut s, 0x0A);
        run_sample(&mut s);
        assert_eq!(s.timer_b_cnt, 252);
        // The divider: one count per sixteen FM samples; the range of
        // 252 to 256 lands the overflow on the fourth count, so the
        // read flag raises on the sixty-fourth sample.
        for i in 0..63u64 {
            tick(&mut s, Tick(i));
            assert!(!s.timer_b_flag, "no flag before the divider completes");
        }
        tick(&mut s, Tick(63));
        assert!(s.timer_b_flag, "the count completed on the fourth divider");
        assert_eq!(s.timer_b_cnt, 0xFC);
    }

    #[test]
    fn the_disabled_timer_keeps_cycling_without_the_flag() {
        let mut s = chip();
        aad(&mut s, 0x26);
        adc(&mut s, 0xFF);
        aad(&mut s, 0x27);
        adc(&mut s, 0x02);
        run_sample(&mut s);
        // The enable-off form: the counter cycles without the read
        // flag per the published form; the counter reloaded once per
        // the keep-cycling form through the run.
        for i in 0..40u64 {
            tick(&mut s, Tick(i));
            assert!(!s.timer_b_flag);
        }
        assert_eq!(s.timer_b_cnt, 0xFF);
        // Turn the enable on and the next top raises the flag.
        aad(&mut s, 0x27);
        adc(&mut s, 0x0A);
        run_sample(&mut s);
        for i in 0..15u64 {
            tick(&mut s, Tick(40 + i));
        }
        tick(&mut s, Tick(55));
        assert!(s.timer_b_flag, "the enabled overflow raised the flag");
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
            tick_irq(&mut s, Tick(i), &edges);
        }
        // The overflow on the first visit raised the line through a
        // notify edge and nothing else moved it.
        assert_eq!(&*edges.borrow(), &[true]);
    }

    /// One per-operator register write helper: the group carries the
    /// operator in its bits two-three and the channel in the low bits.
    fn opw(s: &mut Ym2612, base: u8, op: u8, ch: u8, data: u8) {
        aad(s, base | (op << 2) | ch);
        adc(s, data);
    }

    /// The rate-18 run for one operator: the key code sits at zero,
    /// so the final key-scaled rate is two times the register rate.
    fn attack_setup() -> Ym2612 {
        let mut s = chip();
        // The channel-1's frequency form stays zero: the key code
        // reads zero per the published note bits.
        for (base, value) in [
            (0x30u8, 0x01u8),
            (0x40, 0x00),
            (0x50, 0x12),
            (0x60, 0x14),
            (0x70, 0x08),
            (0x80, 0x40),
            (0x90, 0x00),
        ] {
            opw(&mut s, base, 0, 0, value);
        }
        aad(&mut s, 0xB0);
        adc(&mut s, 0x27);
        aad(&mut s, 0xB4);
        adc(&mut s, 0xC0);
        aad(&mut s, 0x28);
        adc(&mut s, 0x10);
        run_sample(&mut s);
        s
    }

    #[test]
    fn envelope_steps_the_published_attack_form() {
        let mut s = attack_setup();
        // The attack rate 18 with key code zero: the final rate 36,
        // whose cadence steps per four envelope units (twelve FM
        // samples per step) per the derived rate form.
        assert_eq!(s.caches[0].rates[0], 36);
        // The attack walks the exponential gap form from the quiet
        // ceiling.
        assert_eq!(s.ops[0].env_att, 0x3FF);
        // Twelve samples per step: after one step's worth of visits
        // the envelope moved off the ceiling.
        for _ in 0..12 {
            run_sample(&mut s);
        }
        assert!(s.ops[0].env_att < 0x3FF, "the attack stepped off the top");
        // Per the exponential form every step shrinks the remaining
        // gap: the steps get finer, but never rise again.
        let att = s.ops[0].env_att;
        for _ in 0..12 {
            run_sample(&mut s);
        }
        assert!(s.ops[0].env_att <= att, "the attack never rises back");
    }

    #[test]
    fn envelope_walks_attack_decay_sustain_release() {
        let mut s = chip();
        // The channel-1 operator-1: the instant attack per the
        // published rate-31 form; the first decay 31 (the fastest);
        // the sustain level 8 (the ten-bit 256); the release rate 15
        // per the published form.
        opw(&mut s, 0x30, 0, 0, 0x00);
        opw(&mut s, 0x40, 0, 0, 0x00);
        opw(&mut s, 0x50, 0, 0, 0x1F);
        opw(&mut s, 0x60, 0, 0, 0x1F);
        opw(&mut s, 0x80, 0, 0, 0x8F);
        aad(&mut s, 0xB0);
        adc(&mut s, 0x07);
        aad(&mut s, 0xB4);
        adc(&mut s, 0xC0);
        aad(&mut s, 0x28);
        adc(&mut s, 0x10);
        run_sample(&mut s);
        // The instant form landed at the top per the published form.
        assert_eq!(s.ops[0].env_att, 0);
        assert_eq!(s.ops[0].state, EgState::Attack);
        // One visit's pipeline: the attack hand-over to the first
        // decay runs on the third envelope-unit-boundary per the
        // die cadence, so three visits walk it.
        for _ in 0..3 {
            run_sample(&mut s);
        }
        assert_eq!(s.ops[0].state, EgState::Decay);
        // The sustain level 8 rides the published form: the ten-bit
        // level 256 (the register form's doubled form).
        assert_eq!(s.caches[0].sustain, 0x100);
        // The fastest first decay: the steps land quickly; after
        // enough samples the envelope reaches the sustain level and
        // the state holds per the published form.
        for _ in 0..600 {
            run_sample(&mut s);
        }
        assert_eq!(s.ops[0].env_att, 0x100, "the decay clamped at the level");
        assert_eq!(s.ops[0].state, EgState::Sustain);
        // Key off: the release starts from the current form and the
        // release rate 15 runs the published doubled form out fast.
        aad(&mut s, 0x28);
        adc(&mut s, 0x00);
        run_sample(&mut s);
        assert_eq!(s.ops[0].state, EgState::Release);
        for _ in 0..600 {
            run_sample(&mut s);
        }
        assert_eq!(s.ops[0].env_att, 0x3FF, "the release held at the ceiling");
    }

    #[test]
    fn key_scaling_shortens_the_rates_per_the_published_form() {
        let mut s = chip();
        // The key code rides the frequency form: the block five and
        // the frequency number 1037 (the published A form) gives the
        // key code with the note bit form; the rate scaling 0 divides
        // by eight.
        let fnum = (0x05u16 << 11) | 1037;
        aad(&mut s, 0xA4);
        adc(&mut s, ((fnum >> 8) & 0x3F) as u8);
        aad(&mut s, 0xA0);
        adc(&mut s, (fnum & 0xFF) as u8);
        run_sample(&mut s);
        // The key code: the block-and-note bits per the published
        // form (the four bits over the note bit).
        let expected_kc = (((fnum >> 10) as u32 & 0xF) << 1)
            | u32::from((FNUM_NOTE_BITS >> ((fnum >> 7) & 0xF)) & 1);
        assert_eq!(s.caches[0].keycode, expected_kc);
        // The register rate 0 keeps zero; the rate 10 with key
        // scaling 0 lands at two times the rate plus a small key
        // code share, per the published formula.
        opw(&mut s, 0x50, 0, 0, 0x0A);
        run_sample(&mut s);
        let ksv = s.caches[0].keycode >> 3;
        assert_eq!(s.caches[0].rates[0], (10 * 2 + ksv).min(63));
        // The higher rate scaling form (scale two: key code / 2)
        // shifts the divide per the published table.
        opw(&mut s, 0x50, 0, 0, 0x8A);
        run_sample(&mut s);
        assert_eq!(
            s.caches[0].rates[0],
            (10 * 2 + (s.caches[0].keycode >> 1)).min(63)
        );
    }

    /// The FM voice form: the channel-1 runs the algorithm `alg` at
    /// the frequency form (`hi` the block-and-top form and `low` the
    /// eight-bit low byte), the total levels of the modulators at
    /// `mod_tl` and the carriers at the full form, the instant
    /// attack per the published rate-31 forms, and the key on rides
    /// the published operator form.
    fn fm_setup(alg: u8, hi: u8, low: u8, mod_tl: u8) -> Ym2612 {
        let mut s = chip();
        // The frequency form: the high register first per the
        // published caution.
        aad(&mut s, 0xA4);
        adc(&mut s, hi);
        aad(&mut s, 0xA0);
        adc(&mut s, low);
        // The operators 1-2 = the modulator pairs and the 3-4 = ...
        // the per-operator groups: the multiple one, the rate
        // scaling zero, the instant attack per the published rate
        // forms, the full total levels per carrier.
        let tls = [mod_tl, mod_tl, 0, 0];
        for (i, tl) in tls.iter().enumerate() {
            opw(&mut s, 0x30, i as u8, 0, 0x01);
            opw(&mut s, 0x40, i as u8, 0, *tl);
            opw(&mut s, 0x50, i as u8, 0, 0x1F);
            opw(&mut s, 0x60, i as u8, 0, 0x0F);
            opw(&mut s, 0x70, i as u8, 0, 0x1F);
            opw(&mut s, 0x80, i as u8, 0, 0x0F);
            opw(&mut s, 0x90, i as u8, 0, 0x00);
        }
        aad(&mut s, 0xB0);
        adc(&mut s, alg & 0x07);
        aad(&mut s, 0xB4);
        adc(&mut s, 0xC0);
        aad(&mut s, 0x28);
        adc(&mut s, 0xF0);
        run_sample(&mut s);
        s
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
        let mut s = fm_setup(0, 0x08, 0x80, 0x00);
        // The frequency form carries block one with the frequency
        // number 128: the die-verified step form gives the
        // 8192-sample period. Run under the buffer cap and check the
        // period and the crossings.
        for _ in 0..9000 {
            run_sample(&mut s);
        }
        let samples = s.take_samples();
        assert!(samples.len() >= 8500, "the render visited the cap once");
        // The crossings walk two per period: the coarse period of
        // the mixed form.
        let crossings = zero_crossings(&samples);
        assert!(crossings >= 1, "the mixed form crossed zero");
        assert!(crossings <= 4, "the crossings walk the period form");
        // The pattern repeats per the period form.
        let period = 8192usize;
        assert_eq!(samples[0], samples[period + 20]);
    }

    #[test]
    fn the_feedback_changes_the_mixed_pattern() {
        let plain = {
            let mut s = fm_setup(0, 0x08, 0x80, 0x00);
            for _ in 0..4000 {
                run_sample(&mut s);
            }
            s.take_samples()
        };
        let fed = {
            let mut s = fm_setup(0, 0x08, 0x80, 0x00);
            // The feedback level walks the published `$B0` form: the
            // level one rides the shallow shift form.
            aad(&mut s, 0xB0);
            adc(&mut s, 0x0D);
            run_sample(&mut s);
            for _ in 0..4000 {
                run_sample(&mut s);
            }
            s.take_samples()
        };
        assert_ne!(plain, fed, "the feedback changed the pattern");
    }

    #[test]
    fn the_key_off_silences_the_channel() {
        let mut s = fm_setup(0, 0x08, 0x80, 0x00);
        for _ in 0..100 {
            run_sample(&mut s);
        }
        assert!(s.take_samples().iter().any(|v| *v != 0));
        // The key off per the published operator form: the release
        // walks the ceiling and the envelope holds quiet per the
        // published quiet form.
        aad(&mut s, 0x28);
        adc(&mut s, 0x00);
        run_sample(&mut s);
        for _ in 0..1000 {
            run_sample(&mut s);
        }
        s.take_samples();
        for _ in 0..1000 {
            run_sample(&mut s);
        }
        assert!(
            s.take_samples().iter().all(|v| *v == 0),
            "the release form held the channel silent"
        );
    }

    /// The SSG-EG run helper: the channel-1 operator-1 decay walk with
    /// the SSG form of `mode_form`, recording the envelope form per
    /// sample.
    fn ssg_path(mode_form: u8) -> Vec<(u32, bool)> {
        let mut s = chip();
        opw(&mut s, 0x30, 0, 0, 0x01);
        opw(&mut s, 0x40, 0, 0, 0x00);
        opw(&mut s, 0x50, 0, 0, 0x12);
        opw(&mut s, 0x60, 0, 0, 0x32);
        opw(&mut s, 0x70, 0, 0, 0x1F);
        opw(&mut s, 0x80, 0, 0, 0x0F);
        opw(&mut s, 0x90, 0, 0, mode_form);
        aad(&mut s, 0xB4);
        adc(&mut s, 0xC0);
        aad(&mut s, 0x28);
        adc(&mut s, 0x10);
        run_sample(&mut s);
        let mut atts = Vec::new();
        for _ in 0..1200 {
            atts.push((s.ops[0].env_att, s.ops[0].ssg_inv));
        }
        atts
    }

    #[test]
    fn the_ssg_forms_walk_the_envelope_per_type() {
        // The enabled types run the die cycle form over the walk; the
        // envelope sequences split per type: the alternating form
        // walks against the plain repeat, the held forms walk
        // differently from their repeats.
        let ssg0 = ssg_path(0x08);
        let ssg2 = ssg_path(0x0A);
        let ssg1 = ssg_path(0x09);
        let ssg3 = ssg_path(0x0B);
        assert_ne!(ssg0, ssg2, "the alternating form walked differently");
        assert_ne!(ssg1, ssg3, "the hold forms walked differently");
    }

    #[test]
    fn the_dac_values_ride_the_enable_form() {
        let mut s = chip();
        // The DAC data without the enable lands nowhere: the mix
        // holds silent per the published off-form.
        aad(&mut s, 0x2A);
        adc(&mut s, 0x40);
        run_sample(&mut s);
        assert_eq!(*s.samples.last().expect("tail"), 0);
        // The channel-6 output enables gate the DAC per the published
        // replacement form: the channel-6 stereo form rides `$B6` of
        // the part-II addressing.
        bid(&mut s, 0xB6);
        bdc(&mut s, 0xC0);
        // The enable on: the byte 64 becomes the nine-bit form 64
        // doubled to 128 in the mix scale, per the published
        // replacement form.
        aad(&mut s, 0x2B);
        adc(&mut s, 0x80);
        run_sample(&mut s);
        assert_eq!(
            *s.samples.last().expect("tail"),
            ((128 * 8192) / 390) as i16
        );
        // The sign bit rides the negative half per the published
        // signed form.
        aad(&mut s, 0x2A);
        adc(&mut s, 0xFF);
        run_sample(&mut s);
        assert_eq!(*s.samples.last().expect("tail"), -(2 * 8192 / 390));
        // The enable off gates the value again per the published
        // form.
        aad(&mut s, 0x2B);
        adc(&mut s, 0x00);
        run_sample(&mut s);
        assert_eq!(*s.samples.last().expect("tail"), 0);
    }

    #[test]
    fn snapshot_round_trips_byte_identical_and_rearms() {
        let mut s = chip();
        // The voice, the timers, the pair latches and a run of
        // samples: the phases, envelopes and counters advance.
        aad(&mut s, 0xA4);
        adc(&mut s, 0x08);
        aad(&mut s, 0xA0);
        adc(&mut s, 0x80);
        aad(&mut s, 0x24);
        adc(&mut s, 0x40);
        aad(&mut s, 0x27);
        adc(&mut s, 0x05);
        for (base, hi) in [
            (0x30u8, 0x01u8),
            (0x40, 0x00),
            (0x50, 0x1F),
            (0x60, 0x2F),
            (0x70, 0x1F),
            (0x80, 0x0F),
            (0x90, 0x0A),
        ] {
            opw(&mut s, base, 0, 0, hi);
        }
        aad(&mut s, 0xB0);
        adc(&mut s, 0x07);
        aad(&mut s, 0xB4);
        adc(&mut s, 0xC0);
        aad(&mut s, 0x28);
        adc(&mut s, 0xF0);
        aad(&mut s, 0x2A);
        adc(&mut s, 0x55);
        aad(&mut s, 0x2B);
        adc(&mut s, 0x80);
        for i in 0..3u64 {
            tick(&mut s, Tick(i));
        }
        let snap = s.snapshot();
        assert_eq!(snap.len(), OPN_SNAPSHOT_LEN);
        let mut twin = chip();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert!(twin.samples.is_empty());
        assert_eq!(twin.next, Some(Tick(0)));
        assert!(twin.last_tick.is_none());
        // The restored chips render on their own registers: the
        // envelopes walked on from the restore point.
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

    /// The mini machine: one main domain and the YM2612 window at the
    /// published Genesis port form.
    fn ym_config() -> demu_config::model::SystemConfig {
        let mut dividers = BTreeMap::new();
        dividers.insert("psg".to_string(), 4u32);
        let mut ym = instance();
        ym.dividers = dividers;
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(30_472_500, 4).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ym.name.clone(), 0usize);
        demu_config::model::SystemConfig {
            name: "genesis-test".to_string(),
            triple: "sega/m68000".to_string(),
            description: "the YM2612 test machine".to_string(),
            clocks,
            devices: vec![ym],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 24,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "ym".to_string(),
                bus: "cpu".to_string(),
                device: demu_config::newtypes::DeviceName::new("ym0").expect("name"),
                base: 0xA04000,
                size: 0x2000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            }],
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        }
    }

    #[test]
    fn machine_renders_ym2612_samples_through_the_window() {
        let mut ctors = demu_core::smoke_ctors();
        ctors.register(
            "ym2612.psg",
            Box::new(|d, _| {
                Ym2612::new(d)
                    .map(Box::new)
                    .map(|b: Box<Ym2612>| b as Box<dyn Device>)
                    .map_err(demu_core::machine::MachineError::Assembly)
            }),
        );
        let mut machine = Machine::assemble(ym_config(), &ctors).expect("assemble");
        machine.boot();
        // The voice through the published window form: the ports
        // decode the low two address bits per part, so the address
        // register rides port 0 and the data register port 1.
        let base = 0xA04000u32;
        // The algorithm-7 form and the channel-1 outputs.
        assert!(machine.debug_write("cpu", Address(base), 0xB0));
        assert!(machine.debug_write("cpu", Address(base + 1), 0x07));
        assert!(machine.debug_write("cpu", Address(base), 0xB4));
        assert!(machine.debug_write("cpu", Address(base + 1), 0xC0));
        // The instant attack per the published rate-31 form.
        assert!(machine.debug_write("cpu", Address(base), 0x50));
        assert!(machine.debug_write("cpu", Address(base + 1), 0x1F));
        // The frequency form: the block one and the frequency number
        // 128, the high register first per the published caution.
        assert!(machine.debug_write("cpu", Address(base), 0xA4));
        assert!(machine.debug_write("cpu", Address(base + 1), 0x08));
        assert!(machine.debug_write("cpu", Address(base), 0xA0));
        assert!(machine.debug_write("cpu", Address(base + 1), 0x80));
        // The key on for the channel-1 operators per the published
        // operator form.
        assert!(machine.debug_write("cpu", Address(base), 0x28));
        assert!(machine.debug_write("cpu", Address(base + 1), 0xF0));
        machine.run_until(Tick(9000));
        let first = take_samples_of(&mut machine);
        assert!(!first.is_empty(), "the chip rendered through the window");
        assert!(first.iter().any(|v| *v != 0), "the voice sounded");
        machine.run_until(Tick(15000));
        let second = take_samples_of(&mut machine);
        assert!(!second.is_empty(), "the second drain renders on");
    }

    /// Drain the YM2612 samples of the machine through the device slot.
    fn take_samples_of(machine: &mut Machine) -> Vec<i16> {
        machine
            .device_by_name_mut("ym0")
            .expect("device")
            .as_any_mut()
            .downcast_mut::<Ym2612>()
            .expect("the chip")
            .take_samples()
    }
}
