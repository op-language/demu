//! The Neo Geo 68000-side support silicon of the IO area: the input
//! die (`neogeo.io`) that answers the controller, DIP, watchdog,
//! sound-latch, and register rows of `$300000-$3BFFFF`, and the
//! sound-side latch die (`neogeo.soundio`) that answers the Z80's
//! ports `$00`, `$0C`, and `$08`/`$18` on its io bus.
//!
//! # The 68000-side rows (`neogeo.io`)
//!
//! The die decodes the block, byte lane, and word offset of every
//! access internally. The MAME address map mirrors each row pair
//! across the block: the even odd-lanes repeat so the byte read at
//! any even address in `$300000-$31FFFF` answers P1, and any odd
//! address pair carries the DIP lane and the watchdog kick (MAME's
//! `mirror(0x01fffe)` forms; Charles MacDonald's watchdog note: "do a
//! byte write to any odd address or a word write to any even address
//! within the $300000-$31FFFF range ... byte writes to even addresses
//! will not reset the watchdog"). The demu 68k's word write issues
//! its byte lanes, so word writes reach the odd lane and kick.
//!
//! | Address | Read | Write |
//! |---------|------|-------|
//! | `$300000-$31FFFF` even bytes | `REG_P1CNT`: the joypad port 1 bits, active low: 0 = Up, 1 = Down, 2 = Left, 3 = Right, 4 = A, 5 = B, 6 = C, 7 = D (MAME `neogeo_ctrl` `joystick.cpp` port table) | — |
//! | `$300000-$31FFFF` odd bytes | `REG_DIPSW`: the no-DIP AES form answers `$FF` (the AES hardware has no switch block; MAME's AES map leaves the byte out) | the watchdog kick |
//! | `$300081` | `REG_SYSTYPE`: the AES system-type byte `$00` (bit 7 clear is the AES detect form; the MVS boards wire the test-button and slot-count byte here through the NEO-F0, out of the AES build) | the watchdog kick lane keeps |
//! | `$320000-$33FFFF` even bytes | `REG_SOUND`: the Z80's reply byte (the 68k reads the AUDIO input port; MAME's AES port puts `get_audio_result` on bits 15-8, the byte lane demu reads here) | the 68k's sound command: stored, the crossing self-schedules |
//! | `$320000-$33FFFF` odd bytes | `REG_STATUS_A`: the AES form reads `$00` (MAME's AES AUDIO port marks bits 0-7 active-high unused/unknown, so they read zero; the MVS NEO-F0's coin, service, and RTC bits stay out of the AES build) | MAME leaves the lane unmapped; the die drops it |
//! | `$340000-$35FFFF` even bytes | `REG_P2CNT`: the joypad port 2 bits, active low, same bit order | — |
//! | `$360000-$37FFFF` | the open row answers `$FF` (MAME's `unmapped_r` reads the data-bus residue; the die answers the miss form) | dropped |
//! | `$380000-$39FFFF` even bytes | `REG_STATUS_B`: bit 7 = the hardware type (0 = AES, 1 = MVS), bit 6 = the memory-card write-protect, bits 5 and 4 = the card-presence lanes high with no card, bits 3-0 = the start/select nibble in the high form `$F` (MAME's AES `IN2` word: the type custom on bit 15, `get_memcard_status` on bits 14-12, the controller start/select bits on bits 11-8) | — |
//! | `$380000-$39FFFF` odd bytes | `$FF` (the IN2 word's low lane: active-low unused) | the output rows |
//! | `$3A0000-$3BFFFF` | the latch reads answer `$FF` (MAME maps the block read to `unmapped_r`) | the `74HC259` addressable-latch writes |
//!
//! The odd-lane output rows of `$3800xx` (`REG_POUTPUT` `$380001`,
//! `REG_CRDBANK` `$380011` (bits 2-0), `REG_LEDLATCHES` `$380031`,
//! `REG_LEDDATA` `$380041`, `REG_RTCCTRL` `$380051`, per
//! neogeodev.org `Memory mapped registers`) store their bytes. Their
//! published behavior sits on MVS-only hardware: the memory card, the
//! LED and marquee latch strobes of the NEO-F0, and the uPD4990 RTC
//! control are out of this build's scope and the rows keep their
//! stored state only. The remaining odd lanes (the `$380061`-`$3800E7`
//! coin-counter and coin-lockout bookkeeping) drop.
//!
//! ## The system latch (`$3A0000-$3BFFFF`, the 74HC259)
//!
//! MAME wires the block to `hc259_device::write_a3` with the data
//! mask `umask16(0x00ff)`: only the odd byte lanes decode, the latch
//! index rides address bits 2-1 and the DATA bit is address bit 4
//! ("Data written doesn't matter. Odd byte writes only. Registers go
//! in pairs, bit 4 of the address is actually the data bit",
//! neogeodev `Memory mapped registers`). The die keeps the eight
//! latch bits and drives the two published pairs on edges only, on
//! its next visit:
//!
//! | Reset | Set | Latch | Line |
//! |-------|-----|-------|------|
//! | `$3A0003` | `$3A0013` | 1 | `vecbios`: High = the BIOS vectors, Low = the P ROM's |
//! | `$3A000F` | `$3A001F` | 7 | `palbank`: Low = palette bank 0, High = palette bank 1 |
//!
//! The other six latch bits (`REG_NOSHADOW`/`REG_SHADOW` `$3A0001`/
//! `$3A0011`, the memory-card unlock rows, `REG_BRDFIX`/`REG_CRTFIX`,
//! and the MVS backup-RAM unlock rows) store with no line in this
//! build. The reset form of all eight is clear, the `74HC259` clear
//! state Charles MacDonald records for the shared reset line.
//!
//! ## The sound-latch crossings
//!
//! The real hardware holds two `generic_latch_8` devices between the
//! CPUs (MAME `m_soundlatch`, `m_soundlatch2`); the 68k's `$320000`
//! write fills the first and the Z80's port `$00` reads and clears
//! it, while the Z80's port `$0C` write fills the second and the
//! 68k's `$320000` read answers it. In demu the two latch dies are
//! separate devices that reach each other over the buses:
//!
//! - The command flow: the 68k's byte write at `$320000` stores the
//!   byte and self-schedules; on the next visit the die writes the
//!   byte onto the sound io bus at `$00010000` — the address the demu
//!   Z80 core issues port IO at (`Address(0x1_0000 | port)`), so port
//!   `$00` of the sound die answers — with a Word access. A Byte
//!   write at port `$00` is the sound CPU's ack form, so the width
//!   distinguishes the two write flows (the CPU cores issue byte
//!   lanes only; the crossing form is a device's own).
//! - The reply flow: the sound die writes its reply onto the main bus
//!   at `$320000` with a Word access from its next visit; the byte
//!   lanes are the 68k's forms (a byte write = the command, a byte
//!   read = the reply), so the width distinguishes the flows again.
//! - The later write wins per latch: a second command before a visit
//!   forwards the latest byte once; a second reply overwrites.
//!
//! ## The watchdog
//!
//! MAME's watchdog times out after 3,244,030 master-clock ticks with
//! no kick. The die arms at boot and re-arms per kick; on expiry it
//! pulses the `wdreset` line (Level Low, the active-low `/RESET`
//! pulse form the sibling arbiter's `zreset` line uses) once and
//! disarms until the next kick. The deadline converts to the die's
//! domain ticks through the `master` divider (the master ticks per
//! domain tick, default 2 for the 12 MHz 68000 domain on the
//! 24 MHz master), rounded up. Machine definitions route the
//! `wdreset` line to the CPU reset lines.
//!
//! # The sound-side latch die (`neogeo.soundio`)
//!
//! The die answers the Z80's io-bus port page (the config declares
//! the window with the `$100` port-page mirrors, the demu Z80's
//! `Address(0x1_0000 | port)` form):
//!
//! | Port | Read | Write |
//! |------|------|-------|
//! | `$00` (`mirror(0xff00)`) | the sound command byte (MAME `soundcommand_r`: the `generic_latch_8` read keeps the byte) | Byte: the ack/clear — data and the NMI flag drop, per MAME's `clear_w` with `set_separate_acknowledge(false)`; Word: the 68k side's command crossing arrives |
//! | `$08` | the bank-port read form answers 0 (MAME `audio_cpu_bank_select_r`; the switching side lives in the M1 die's window) | the NMI enable (`out ($08) enables the nmi`, MAME) |
//! | `$0C` (`mirror(0xff00)`) | `$FF` | the reply write: stored, the crossing to the 68k's `$320000` self-schedules (MAME `soundlatch2` write) |
//! | `$18` | the bank-port mirror row answers 0 | the NMI disable (`out ($18) disables it`) |
//! | `$04-$07` | — | the YM2610's ports; the die's decode scope falls out so the chip's own window answers |
//!
//! The NMI form follows the input merger MAME wires: the NMI asserts
//! while a command pends and the kill-enable lets it, and the Z80
//! latches one service per rising edge (`nmi_edge`). This build
//! drives a pulse form: an arrival (or an
//! enable while a command pends) schedules a High edge on the next
//! visit and a Low on the following one, one service per arrival. A
//! second arrival while the pulse is mid-flight or still pending
//! updates the latch only, matching the merger's held level: the
//! driver's port `$00` read answers the latest command, and the next
//! ack-then-arrival sequence latches the next service. The ack write
//! and the disable write cancel a pending High that has not delivered
//! yet (real hardware acks only after the latch holds the byte; the
//! one-tick crossing delay's own form), while a High that already
//! delivered keeps its scheduled Low so the line returns low. The
//! disable form is the reset form: MAME's `machine_reset` clears the
//! merger input.
//!
//! # Sources
//!
//! MAME `src/mame/snk/neogeo.cpp` (BSD-3-Clause): `base_main_map`,
//! `neogeo_main_map`, `aes_base_main_map`, `audio_io_map`, the
//! controller read handlers, `io_control_w`, `audio_cpu_enable_nmi_w`,
//! `audio_cpu_bank_select_r`, `audio_command_w`, `get_audio_result`,
//! `get_memcard_status`, the watchdog time, and the machine reset
//! form; MAME `src/devices/machine/74259.cpp` for the `write_a3`
//! addressable-latch form; MAME `src/devices/bus/neogeo_ctrl/ctrl.cpp`
//! and `joystick.cpp` for the controller bit order; Charles
//! MacDonald's `Neo*Geo MVS Hardware Notes` for the watchdog kick
//! form and the system-latch table; neogeodev.org `Memory mapped
//! registers` for the register names, the odd-lane form, and the
//! latch pairs; and the demu Z80 core's io-bus address form and the
//! demu 68k core's byte-lane split for the crossing widths.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::InputSink;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The watchdog timeout in master-clock ticks (MAME's watchdog time;
/// Charles MacDonald measured /RESET low for 3,244,030 mclks).
const WDT_MASTER_TICKS: u64 = 3_244_030;
/// The io-bus address the demu Z80 core issues port IO at.
const PORT_FLAG: u32 = 0x0001_0000;
/// The base of the 68k-side reply and command window.
const SOUND_WINDOW: u32 = 0x0032_0000;
/// The snapshot tag and version of the 68k-side die.
const IO_TAG: &[u8; 4] = b"NINO";
const IO_VERSION: u8 = 1;
/// The snapshot tag and version of the sound-side die.
const SNIO_TAG: &[u8; 4] = b"NGSN";
const SNIO_VERSION: u8 = 1;

/// The snapshot read helpers; they reject short chunks with the
/// established mismatch form.
fn snapshot_byte(data: &[u8], at: usize) -> Result<u8, SnapshotError> {
    data.get(at)
        .copied()
        .ok_or_else(|| SnapshotError::Mismatch("snapshot chunk ends inside a field".into()))
}

fn snapshot_u64(data: &[u8], at: usize) -> Result<u64, SnapshotError> {
    let tail = data
        .get(at..at + 8)
        .ok_or_else(|| SnapshotError::Mismatch("snapshot chunk ends inside a field".into()))?;
    Ok(u64::from_le_bytes(tail.try_into().map_err(|_| {
        SnapshotError::Mismatch("snapshot u64 read failed".into())
    })?))
}

/// Code a pending line level for the snapshot: 0 none, 1 low, 2 high.
fn pending_code(pending: Option<Level>) -> u8 {
    match pending {
        None => 0,
        Some(Level::Low) => 1,
        Some(Level::High) => 2,
    }
}

/// Decode a pending line level code from the snapshot.
fn pending_of(code: u8) -> Result<Option<Level>, SnapshotError> {
    match code {
        0 => Ok(None),
        1 => Ok(Some(Level::Low)),
        2 => Ok(Some(Level::High)),
        _ => Err(SnapshotError::Mismatch("bad pending level code".into())),
    }
}

/// The 68000-side IO die of the Neo Geo.
#[derive(Debug)]
pub struct NeoIo {
    name: String,
    /// The bus that carries the sound die's port windows (the
    /// `sound_bus` parameter, default `sound`).
    sound_bus: String,
    /// The held controller bits of ports 1 and 2 (1 = held; active
    /// low on the bus).
    p1: u8,
    p2: u8,
    /// The `74HC259` latch bits.
    latch: u8,
    /// The stored MVS-only output rows.
    poutput: u8,
    card_bank: u8,
    led_latches: u8,
    led_data: u8,
    rtc_ctrl: u8,
    /// The sound command byte the 68k's `$320000` write latched.
    sound_command: u8,
    /// True while a command forward is due on the next visit.
    forward_due: bool,
    /// The reply byte the sound side's crossing stored.
    sound_reply: u8,
    /// The pending edge levels of the `palbank` and `vecbios` lines.
    pal_pending: Option<Level>,
    vec_pending: Option<Level>,
    /// The watchdog deadline in domain ticks, or none while disarmed.
    wdt_deadline: Option<Tick>,
    /// The master ticks per domain tick (the `master` divider).
    master_div: u64,
    /// The next visit the die self-schedules.
    next: Option<Tick>,
}

impl NeoIo {
    /// An IO die from its configuration entry.
    ///
    /// Parameters: `sound_bus` (the bus that carries the sound die's
    /// port windows, default `sound`). The `master` divider names the
    /// master ticks per domain tick for the watchdog, default 2.
    pub fn new(device: &DeviceInstance, _config: &SystemConfig) -> Result<Self, String> {
        let sound_bus = device
            .params
            .get("sound_bus")
            .and_then(|v| v.as_str())
            .unwrap_or("sound")
            .to_string();
        Ok(Self {
            name: device.name.to_string(),
            sound_bus,
            p1: 0,
            p2: 0,
            latch: 0,
            poutput: 0,
            card_bank: 0,
            led_latches: 0,
            led_data: 0,
            rtc_ctrl: 0,
            sound_command: 0,
            forward_due: false,
            sound_reply: 0,
            pal_pending: None,
            vec_pending: None,
            wdt_deadline: None,
            master_div: u64::from(device.dividers.get("master").copied().unwrap_or(2)).max(1),
            next: None,
        })
    }

    /// The watchdog timeout in domain ticks, rounded up.
    fn wdt_period(&self) -> u64 {
        WDT_MASTER_TICKS.div_ceil(self.master_div)
    }

    /// Keep the earlier self-schedule of the two forms.
    fn arm(&mut self, at: Tick) {
        self.next = Some(match self.next {
            Some(n) if n.0 <= at.0 => n,
            _ => at,
        });
    }

    /// The watchdog kick form: the deadline moves a full period past
    /// the kick, the MAME restart form.
    fn kick_watchdog(&mut self, tick: Tick) {
        let deadline = Tick(tick.0.saturating_add(self.wdt_period()));
        self.wdt_deadline = Some(deadline);
        self.arm(deadline);
    }

    /// The `74HC259` latch write: the index rides address bits 2-1 of
    /// the block cell, the data bit is address bit 4, and the value
    /// carries nothing. The published latch pairs drive their lines.
    fn write_latch(&mut self, address: u32, tick: Tick) {
        let cell = address & 0x1F;
        let index = (cell >> 1) & 7;
        let data = (cell >> 4) & 1;
        let bit = 1u8 << index;
        let updated = if data == 1 {
            self.latch | bit
        } else {
            self.latch & !bit
        };
        if updated == self.latch {
            return;
        }
        self.latch = updated;
        if index == 7 {
            // REG_PALBANK0 resets bank 0 ($3A000F), REG_PALBANK1 sets
            // bank 1 ($3A001F).
            self.pal_pending = Some(if data == 1 { Level::High } else { Level::Low });
        }
        if index == 1 {
            // REG_SWPBIOS resets the BIOS vectors ($3A0003),
            // REG_SWPROM sets the cart's ($3A0013).
            self.vec_pending = Some(if data == 1 { Level::Low } else { Level::High });
        }
        self.arm(Tick(tick.0.saturating_add(1)));
    }

    /// The controller byte of one port: the held bits invert to the
    /// active-low idle form `$FF`.
    fn controller_byte(&self, held: u8) -> u32 {
        u32::from(!held)
    }
}

impl InputSink for NeoIo {
    fn inject(&mut self, event: &str) -> bool {
        // The grammar: `p1 press <bits>` / `p1 lift <bits>` / `p2
        // ...`, with the bits = a decimal number over the controller
        // bits 0-7 (Up, Down, Left, Right, A, B, C, D); a press holds
        // the bits, which read low on the bus.
        let form = event.trim().to_ascii_lowercase();
        let Some((cell, rest)) = form.split_once(' ') else {
            return false;
        };
        let slot = match cell {
            "p1" => 0usize,
            "p2" => 1usize,
            _ => return false,
        };
        let Some((verb, tail)) = rest.split_once(' ') else {
            return false;
        };
        let press = match verb {
            "press" => true,
            "lift" => false,
            _ => return false,
        };
        let Some(bits) = tail.trim().parse::<u8>().ok() else {
            return false;
        };
        let held = match slot {
            0 => self.p1,
            _ => self.p2,
        };
        let updated = if press { held | bits } else { held & !bits };
        if slot == 0 {
            self.p1 = updated;
        } else {
            self.p2 = updated;
        }
        true
    }
}

impl BusAccess for NeoIo {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0;
        let block = a >> 17;
        let lane = a & 1;
        match block {
            // $300000-$31FFFF: P1 on the even bytes, the no-DIP form
            // on the odd bytes, the system type at $300081.
            24 if lane == 0 => self.controller_byte(self.p1),
            24 if (a & 0x0001_FFFF) == 0x81 => 0x00,
            24 => 0xFF,
            // $320000-$33FFFF: the reply on the even bytes, the AES
            // status A form $00 on the odd.
            25 if lane == 0 => u32::from(self.sound_reply),
            25 => 0x00,
            // $340000-$35FFFF: P2 on the even bytes.
            26 if lane == 0 => self.controller_byte(self.p2),
            26 => 0xFF,
            // $380000-$39FFFF: the status B byte on the even bytes.
            // Bit 7 = the type (0 = AES), bits 6-4 = the card forms
            // high with no card, bits 3-0 = the start/select nibble
            // in its idle form.
            28 if lane == 0 => 0x7F,
            28 => 0xFF,
            _ => 0xFF,
        }
    }

    fn write(&mut self, width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let a = address.0;
        let at_next = Tick(tick.0.saturating_add(1));
        match width {
            AccessWidth::Word | AccessWidth::Dword => {
                // The sound side's reply crossing: the CPU cores
                // issue byte lanes, so the word form at the reply
                // window carries the crossing; the store is instant.
                if (a >> 17) == 25 && (a & 0x0001_FFFF) == 0 {
                    self.sound_reply = (value & 0xFF) as u8;
                }
            }
            AccessWidth::Byte => {
                let block = a >> 17;
                let lane = a & 1;
                match block {
                    // The watchdog kick: any odd byte of the row.
                    24 if lane == 1 => self.kick_watchdog(tick),
                    24 => {}
                    // The sound command: the even bytes, MAME's write
                    // mirror form; the odd lane drops.
                    25 if lane == 0 => {
                        self.sound_command = value as u8;
                        self.forward_due = true;
                        self.arm(at_next);
                    }
                    25 => {}
                    // The output rows on the odd byte lanes.
                    28 if lane == 1 => {
                        let word = (a & 0x0001_FFFF) >> 1;
                        match word {
                            0 => self.poutput = value as u8,
                            8 => self.card_bank = (value & 0x07) as u8,
                            0x18 => self.led_latches = value as u8,
                            0x20 => self.led_data = value as u8,
                            0x28 => self.rtc_ctrl = value as u8,
                            _ => {}
                        }
                    }
                    28 => {}
                    // The 74HC259 latch writes.
                    29 if lane == 1 => self.write_latch(a, tick),
                    29 => {}
                    _ => {}
                }
            }
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0;
        let lane = a & 1;
        match a >> 17 {
            24 if lane == 0 => !self.p1,
            24 if (a & 0x0001_FFFF) == 0x81 => 0x00,
            24 => 0xFF,
            25 if lane == 0 => self.sound_reply,
            25 => 0x00,
            26 if lane == 0 => !self.p2,
            26 => 0xFF,
            28 if lane == 0 => 0x7F,
            28 => 0xFF,
            _ => 0xFF,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for NeoIo {
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

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        // The 74HC259's clear form: all latch bits clear, so the BIOS
        // vectors serve and palette bank 0 answers.
        self.p1 = 0;
        self.p2 = 0;
        self.latch = 0;
        self.poutput = 0;
        self.card_bank = 0;
        self.led_latches = 0;
        self.led_data = 0;
        self.rtc_ctrl = 0;
        self.sound_command = 0;
        self.forward_due = false;
        self.sound_reply = 0;
        self.pal_pending = None;
        self.vec_pending = None;
        self.wdt_deadline = None;
        self.next = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        // The watchdog arms at boot, the MAME watchdog form.
        self.reset();
        let deadline = Tick(self.wdt_period());
        self.wdt_deadline = Some(deadline);
        self.next = Some(deadline);
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(34);
        out.extend_from_slice(IO_TAG);
        out.push(IO_VERSION);
        out.push(self.p1);
        out.push(self.p2);
        out.push(self.latch);
        out.push(self.sound_command);
        out.push(u8::from(self.forward_due));
        out.push(self.sound_reply);
        out.push(pending_code(self.pal_pending));
        out.push(pending_code(self.vec_pending));
        out.push(self.poutput);
        out.push(self.card_bank);
        out.push(self.led_latches);
        out.push(self.led_data);
        out.push(self.rtc_ctrl);
        out.extend_from_slice(
            &self
                .wdt_deadline
                .map(|t| t.0)
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&self.next.map(|t| t.0).unwrap_or(u64::MAX).to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 34 || &data[..4] != IO_TAG || snapshot_byte(data, 4)? != IO_VERSION {
            return Err(SnapshotError::Mismatch(format!(
                "the neo io chunk must be 34 bytes of tag {IO_TAG:?}"
            )));
        }
        self.p1 = snapshot_byte(data, 5)?;
        self.p2 = snapshot_byte(data, 6)?;
        self.latch = snapshot_byte(data, 7)?;
        self.sound_command = snapshot_byte(data, 8)?;
        self.forward_due = snapshot_byte(data, 9)? != 0;
        self.sound_reply = snapshot_byte(data, 10)?;
        self.pal_pending = pending_of(snapshot_byte(data, 11)?)?;
        self.vec_pending = pending_of(snapshot_byte(data, 12)?)?;
        self.poutput = snapshot_byte(data, 13)?;
        self.card_bank = snapshot_byte(data, 14)?;
        self.led_latches = snapshot_byte(data, 15)?;
        self.led_data = snapshot_byte(data, 16)?;
        self.rtc_ctrl = snapshot_byte(data, 17)?;
        let wdt = snapshot_u64(data, 18)?;
        self.wdt_deadline = (wdt != u64::MAX).then_some(Tick(wdt));
        let next = snapshot_u64(data, 26)?;
        self.next = (next != u64::MAX).then_some(Tick(next));
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.next = None;
        let wdt_due = self.wdt_deadline.filter(|d| d.0 <= tick.0);
        // The latch edges first, then the sound-command forward, then
        // the watchdog pulse; the order is documented and stable.
        if let Some(level) = self.pal_pending.take() {
            ctx.notify("palbank", level, tick);
        }
        if let Some(level) = self.vec_pending.take() {
            ctx.notify("vecbios", level, tick);
        }
        if self.forward_due {
            self.forward_due = false;
            ctx.write(
                &self.sound_bus,
                AccessWidth::Word,
                Address(PORT_FLAG),
                u32::from(self.sound_command),
                tick,
            );
        }
        if wdt_due.is_some() {
            self.wdt_deadline = None;
            ctx.notify("wdreset", Level::Low, tick);
        }
        // A still-future deadline re-arms the visit.
        self.next = self.wdt_deadline;
    }
}

/// The sound-side latch die of the Neo Geo.
#[derive(Debug)]
pub struct NeoSoundIo {
    name: String,
    /// The bus whose decoder answers the reply crossing at `$320000`
    /// (the `main_bus` parameter, default `cpu`).
    main_bus: String,
    /// The sound command latch: the 68k's byte, the Z80's read.
    latch: u8,
    /// The reply latch: the Z80's `$0C` write, the 68k's read.
    reply: u8,
    /// The NMI enable flag; the reset form is disabled.
    nmi_en: bool,
    /// True while a command sits unread.
    nmi_pending: bool,
    /// The pulse form: 0 idle, 1 the High edge due, 2 the Low drop due.
    nmi_stage: u8,
    /// True while a reply crossing is due on the next visit.
    reply_due: bool,
    /// The next visit the die self-schedules.
    next: Option<Tick>,
}

impl NeoSoundIo {
    /// A sound-side latch die from its configuration entry.
    ///
    /// Parameters: `main_bus` (the bus the reply crossing writes to,
    /// default `cpu`).
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        let main_bus = device
            .params
            .get("main_bus")
            .and_then(|v| v.as_str())
            .unwrap_or("cpu")
            .to_string();
        Ok(Self {
            name: device.name.to_string(),
            main_bus,
            latch: 0,
            reply: 0,
            nmi_en: false,
            nmi_pending: false,
            nmi_stage: 0,
            reply_due: false,
            next: None,
        })
    }

    /// The port number of an access: the demu Z80 marks its port IO
    /// with the `$10000` flag on the io bus.
    fn port_of(address: u32) -> u8 {
        (address & 0xFF) as u8
    }

    /// Keep the earlier self-schedule of the two forms.
    fn arm(&mut self, at: Tick) {
        self.next = Some(match self.next {
            Some(n) if n.0 <= at.0 => n,
            _ => at,
        });
    }

    /// The command-byte arrival from the crossing: the latch updates,
    /// and while the NMI lets it the pulse form schedules one High
    /// edge then its Low drop. A second arrival while the pulse is
    /// mid-flight or pending updates the latch only, the merger's
    /// held-level form.
    fn command_arrival(&mut self, value: u8, tick: Tick) {
        self.latch = value;
        if !self.nmi_pending {
            self.nmi_pending = true;
            if self.nmi_en && self.nmi_stage == 0 {
                self.nmi_stage = 1;
                self.arm(Tick(tick.0.saturating_add(1)));
            }
        }
    }

    /// The ack write: clears the latch and the NMI pending, and
    /// cancels a High edge that has not delivered yet (a delivered
    /// High keeps its Low drop).
    fn ack_write(&mut self, tick: Tick) {
        self.latch = 0;
        self.nmi_pending = false;
        if self.nmi_stage == 1 {
            self.nmi_stage = 0;
            self.next = if self.reply_due {
                Some(Tick(tick.0.saturating_add(1)))
            } else {
                None
            };
        }
    }

    /// The NMI disable: the flag drops and a not-yet-delivered High
    /// cancels, per the merger's held level.
    fn nmi_disable(&mut self, tick: Tick) {
        self.nmi_en = false;
        if self.nmi_stage == 1 {
            self.nmi_stage = 0;
            self.next = if self.reply_due {
                Some(Tick(tick.0.saturating_add(1)))
            } else {
                None
            };
        }
    }
}

impl BusAccess for NeoSoundIo {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match Self::port_of(address.0) {
            0x00 => u32::from(self.latch),
            // The bank ports' read form answers 0 per MAME's
            // `audio_cpu_bank_select_r`; the switch effect lives in
            // the M1 die's own window.
            0x08..=0x0B | 0x18..=0x1B => 0,
            _ => 0xFF,
        }
    }

    fn write(&mut self, width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let at_next = Tick(tick.0.saturating_add(1));
        match width {
            // The 68k side's command crossing: the CPU cores issue
            // byte lanes, so the word form carries the crossing.
            AccessWidth::Word | AccessWidth::Dword => {
                if Self::port_of(address.0) == 0x00 {
                    self.command_arrival((value & 0xFF) as u8, tick);
                }
            }
            AccessWidth::Byte => match Self::port_of(address.0) {
                0x00 => self.ack_write(tick),
                0x08 => {
                    // The NMI enable: with a pending command the
                    // merger's level rises, per MAME's form.
                    self.nmi_en = true;
                    if self.nmi_pending && self.nmi_stage == 0 {
                        self.nmi_stage = 1;
                        self.arm(at_next);
                    }
                }
                0x18 => self.nmi_disable(tick),
                0x0C => {
                    // The reply: stored, and the crossing to the
                    // 68k's `$320000` self-schedules.
                    self.reply = (value & 0xFF) as u8;
                    self.reply_due = true;
                    if self.nmi_stage == 0 {
                        self.arm(at_next);
                    }
                }
                _ => {}
            },
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match Self::port_of(address.0) {
            0x00 => self.latch,
            0x08..=0x0B | 0x18..=0x1B => 0,
            _ => 0xFF,
        }
    }

    fn decodes(&self, address: Address) -> bool {
        // The port-page scope: the command, ack, reply, and NMI
        // forms. The YM2610's `$04-$07` ports fall out so the chip's
        // own window answers them.
        (address.0 & PORT_FLAG) != 0
            && matches!(Self::port_of(address.0), 0x00 | 0x08 | 0x0C | 0x18)
    }
}

impl Device for NeoSoundIo {
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

    fn reset(&mut self) {
        // The reset form disables the NMI (MAME's machine_reset
        // clears the merger input) and empties both latches.
        self.latch = 0;
        self.reply = 0;
        self.nmi_en = false;
        self.nmi_pending = false;
        self.nmi_stage = 0;
        self.reply_due = false;
        self.next = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(17);
        out.extend_from_slice(SNIO_TAG);
        out.push(SNIO_VERSION);
        out.push(self.latch);
        out.push(self.reply);
        out.push(self.nmi_stage);
        let mut flags = 0u8;
        if self.nmi_en {
            flags |= 0x01;
        }
        if self.nmi_pending {
            flags |= 0x02;
        }
        if self.reply_due {
            flags |= 0x04;
        }
        out.push(flags);
        out.extend_from_slice(&self.next.map(|t| t.0).unwrap_or(u64::MAX).to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 17 || &data[..4] != SNIO_TAG || snapshot_byte(data, 4)? != SNIO_VERSION {
            return Err(SnapshotError::Mismatch(format!(
                "the neo sound io chunk must be 17 bytes of tag {SNIO_TAG:?}"
            )));
        }
        self.latch = snapshot_byte(data, 5)?;
        self.reply = snapshot_byte(data, 6)?;
        self.nmi_stage = snapshot_byte(data, 7)?;
        let flags = snapshot_byte(data, 8)?;
        self.nmi_en = flags & 0x01 != 0;
        self.nmi_pending = flags & 0x02 != 0;
        self.reply_due = flags & 0x04 != 0;
        let next = snapshot_u64(data, 9)?;
        self.next = (next != u64::MAX).then_some(Tick(next));
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.next = None;
        // The pulse form: the High edge, then the Low drop on the
        // following visit, one service per arrival. The reply
        // crossing rides the same visit order after the edges.
        if self.nmi_stage == 1 {
            ctx.notify("nmi", Level::High, tick);
            self.nmi_stage = 2;
            self.next = Some(Tick(tick.0.saturating_add(1)));
        } else if self.nmi_stage == 2 {
            ctx.notify("nmi", Level::Low, tick);
            self.nmi_stage = 0;
        }
        if self.reply_due {
            self.reply_due = false;
            ctx.write(
                &self.main_bus,
                AccessWidth::Word,
                Address(SOUND_WINDOW),
                u32::from(self.reply),
                tick,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_config::newtypes::{DeviceName, Frequency, KindName};
    use demu_core::machine::{DeviceCtors, Machine, MachineError};
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// The period ticks of the die's watchdog under the default
    /// master divider of 2.
    const WDT_PERIOD: u64 = WDT_MASTER_TICKS.div_ceil(2);

    /// The io instance for the die forms.
    fn io() -> NeoIo {
        let device = DeviceInstance {
            name: DeviceName::new("io").expect("name"),
            kind: KindName::new("neogeo.io").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        NeoIo::new(&device, &sound_bus_config()).expect("io")
    }

    /// The io configuration with the sound_bus parameter wired.
    fn sound_bus_config() -> SystemConfig {
        let device = DeviceInstance {
            name: DeviceName::new("soundside").expect("name"),
            kind: KindName::new("neogeo.soundio").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        neo_config(vec![device], "sound")
    }

    /// The builder of the base configuration with the named buses.
    fn neo_config(devices: Vec<DeviceInstance>, sound_bus: &str) -> SystemConfig {
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: Frequency::new(12_000_000, 1).expect("freq"),
                origin: "test".to_string(),
            },
        );
        let mut devices_by_name = BTreeMap::new();
        for (i, device) in devices.iter().enumerate() {
            devices_by_name.insert(device.name.clone(), i);
        }
        SystemConfig {
            name: "neogeo-io-test".to_string(),
            triple: "test".to_string(),
            description: "The IO die mini test machine.".to_string(),
            clocks,
            devices,
            devices_by_name,
            buses: vec![
                CBus {
                    name: "cpu".to_string(),
                    address_bits: 24,
                    owner: None,
                    parent: None,
                },
                CBus {
                    name: sound_bus.to_string(),
                    address_bits: 17,
                    owner: None,
                    parent: None,
                },
            ],
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        }
    }

    /// A sound-side die instance.
    fn sound_io() -> NeoSoundIo {
        let device = DeviceInstance {
            name: DeviceName::new("soundio").expect("name"),
            kind: KindName::new("neogeo.soundio").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        NeoSoundIo::new(&device).expect("soundio")
    }

    /// Runs one die-level visit against an empty machine context; the
    /// lspc test module's owned-context form.
    fn visit<T>(go: impl FnOnce(&mut MachineCtx) -> T) -> T {
        let mut before: [Box<dyn Device>; 0] = [];
        let mut after: [Box<dyn Device>; 0] = [];
        let mut buses: [demu_core::bus::Bus; 0] = [];
        let index = Default::default();
        let routes: Vec<demu_core::device::Route> = Vec::new();
        let mut scratch: Vec<demu_core::device::DecodeHit> = Vec::new();
        let mut dirty: Vec<usize> = Vec::new();
        let mut events: Vec<demu_core::triggers::TriggerEvent> = Vec::new();
        let mut ctx = MachineCtx::new(
            demu_core::device::DeviceSlices {
                before: &mut before,
                after: &mut after,
                caller_slot: 0,
            },
            demu_core::device::BusBank {
                buses: &mut buses,
                index: &index,
            },
            routes.as_slice(),
            &mut scratch,
            demu_core::device::EventBuffers {
                events: &mut events,
                dirty: &mut dirty,
            },
            false,
        );
        go(&mut ctx)
    }

    #[test]
    fn word_write_is_the_reply_crossing_and_byte_the_command() {
        let mut v = io();
        // Word form at $320000: the sound side's reply store.
        v.write(AccessWidth::Word, Address(0x0032_0000), 0x77, Tick(0));
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0032_0000), Tick(0)),
            0x77
        );
        // Byte form at the even lane: the 68k's command latch; the
        // forward schedules and the reply read keeps.
        v.write(AccessWidth::Byte, Address(0x0032_0000), 0x5A, Tick(0));
        assert!(v.forward_due);
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0032_0000), Tick(0)),
            0x77
        );
        // The odd byte lane carries no command form.
        v.write(AccessWidth::Byte, Address(0x0032_0001), 0x11, Tick(0));
        assert_eq!(v.sound_command, 0x5A);
        // The mirror form: the even lanes of the row all take it.
        v.write(AccessWidth::Byte, Address(0x0032_0042), 0x33, Tick(0));
        assert_eq!(v.sound_command, 0x33);
    }

    #[test]
    fn controller_reads_follow_the_published_bit_order() {
        let mut v = io();
        // The active-low idle form.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0030_0000), Tick(0)),
            0xFF
        );
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0034_0000), Tick(0)),
            0xFF
        );
        // Up = bit 0, Down = 1, Left = 2, Right = 3, A = 4, B = 5,
        // C = 6, D = 7 (MAME's joystick port).
        assert!(v.inject("p1 press 16"));
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0030_0000), Tick(0)),
            0xEF
        );
        assert!(v.inject("p2 press 9"));
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0034_0000), Tick(0)),
            0xF6
        );
        // Even-byte mirror forms answer the same registers.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0031_0000), Tick(0)),
            0xEF
        );
        assert!(v.inject("p1 lift 16"));
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0030_0000), Tick(0)),
            0xFF
        );
        // The grammar rejects foreign forms.
        assert!(!v.inject("p3 press 1"));
        assert!(!v.inject("p1 press 300"));
        assert!(!v.inject("junk"));
    }

    #[test]
    fn status_reads_answer_the_aes_forms() {
        let mut v = io();
        // The no-DIP form on the odd lane, the system type byte.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0030_0001), Tick(0)),
            0xFF
        );
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0030_0081), Tick(0)),
            0x00
        );
        // The AES status A form (MAME's AUDIO port low lane).
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0032_0001), Tick(0)),
            0x00
        );
        // The status B byte: the AES idle form $7F.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0038_0000), Tick(0)),
            0x7F
        );
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0038_0001), Tick(0)),
            0xFF
        );
        // The open row and the 259 block answers.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0036_0000), Tick(0)),
            0xFF
        );
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x003A_0001), Tick(0)),
            0xFF
        );
        assert_eq!(v.peek(Address(0x0030_0081)), 0x00);
        assert_eq!(v.peek(Address(0x0034_0000)), 0xFF);
    }

    #[test]
    fn output_rows_store_their_latch_bytes() {
        let mut v = io();
        v.write(AccessWidth::Byte, Address(0x0038_0001), 0xAB, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0038_0011), 0x1F, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0038_0031), 0x12, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0038_0041), 0x34, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0038_0051), 0x56, Tick(0));
        assert_eq!(v.poutput, 0xAB);
        assert_eq!(v.card_bank, 0x07, "the card bank keeps bits 2-0");
        assert_eq!(v.led_latches, 0x12);
        assert_eq!(v.led_data, 0x34);
        assert_eq!(v.rtc_ctrl, 0x56);
        assert_eq!(v.latch, 0, "the 259 latch keeps clear");
        // The even lanes carry no output form.
        v.write(AccessWidth::Byte, Address(0x0038_0010), 0x77, Tick(0));
        assert_eq!(v.card_bank, 0x07);
        // The coin-counter rows and other lanes drop.
        v.write(AccessWidth::Byte, Address(0x0038_0061), 0x55, Tick(0));
        assert_eq!(v.poutput, 0xAB);
    }

    #[test]
    fn watchdog_kicks_rearm_and_expiry_pulses_once() {
        let mut v = io();
        v.boot(None);
        assert_eq!(v.wdt_period(), WDT_PERIOD);
        // The even-lane writes stay silent: no kick, the MAME/Charles
        // MacDonald form ("byte writes to even addresses will not
        // reset the watchdog").
        let armed = v.wdt_deadline;
        v.write(AccessWidth::Byte, Address(0x0030_0000), 0xFF, Tick(7));
        v.write(AccessWidth::Byte, Address(0x0030_0004), 0xFF, Tick(9));
        assert_eq!(v.wdt_deadline, armed);
        // The odd-lane writes kick and move the deadline.
        v.write(AccessWidth::Byte, Address(0x0030_0003), 0x00, Tick(11));
        assert_eq!(v.wdt_deadline, Some(Tick(11 + WDT_PERIOD)));
        // The deadline holds before the expiry.
        visit(|ctx| v.tick(Tick(WDT_PERIOD + 11 - 1), ctx));
        assert!(v.wdt_deadline.is_some());
        // At the expiry the deadline clears and the pulse was due.
        visit(|ctx| v.tick(Tick(WDT_PERIOD + 11), ctx));
        assert!(v.wdt_deadline.is_none());
        // No further pulse without a kick.
        visit(|ctx| v.tick(Tick(WDT_PERIOD * 2), ctx));
        assert!(v.wdt_deadline.is_none());
        // A kick re-arms a full period past the kick.
        v.kick_watchdog(Tick(500));
        assert_eq!(v.wdt_deadline, Some(Tick(500 + WDT_PERIOD)));
    }

    #[test]
    fn latch_writes_follow_the_published_pairs() {
        let mut v = io();
        // $3A0013: the P ROM vectors (latch 1 sets, a change fires).
        v.write(AccessWidth::Byte, Address(0x003A_0013), 0xFF, Tick(0));
        assert_eq!(v.latch & 0x02, 0x02);
        assert_eq!(v.vec_pending, Some(Level::Low));
        // $3A0003: the BIOS vectors (latch 1 resets).
        v.write(AccessWidth::Byte, Address(0x003A_0003), 0xFF, Tick(0));
        assert_eq!(v.latch & 0x02, 0);
        assert_eq!(v.vec_pending, Some(Level::High));
        // $3A001F: palette bank 1 (latch 7 sets).
        v.write(AccessWidth::Byte, Address(0x003A_001F), 0x77, Tick(0));
        assert_eq!(v.latch & 0x80, 0x80);
        assert_eq!(v.pal_pending, Some(Level::High));
        // $3A000F: palette bank 0 (latch 7 resets).
        v.write(AccessWidth::Byte, Address(0x003A_000F), 0x00, Tick(0));
        assert_eq!(v.latch & 0x80, 0);
        assert_eq!(v.pal_pending, Some(Level::Low));
        // The other latches store with no pending line.
        v.write(AccessWidth::Byte, Address(0x003A_0011), 0x00, Tick(0));
        assert_eq!(v.latch & 0x01, 0x01, "the shadow latch bit stores");
        assert_eq!(v.pal_pending, Some(Level::Low));
        // The even lanes and the value carry nothing.
        let before = v.latch;
        v.write(AccessWidth::Byte, Address(0x003A_0010), 0xFF, Tick(0));
        assert_eq!(v.latch, before);
        // The block mirrors: the same cell at $3A2013 writes latch 1.
        v.write(AccessWidth::Byte, Address(0x003A_2013), 0x00, Tick(0));
        assert_eq!(v.latch & 0x02, 0x02);
        assert_eq!(v.vec_pending, Some(Level::Low));
    }

    #[test]
    fn io_snapshot_round_trips() {
        let mut v = io();
        v.inject("p1 press 41");
        v.inject("p2 press 2");
        v.write(AccessWidth::Byte, Address(0x003A_001F), 0, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0032_0000), 0x42, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0038_0011), 0x05, Tick(0));
        v.kick_watchdog(Tick(9));
        let snap = v.snapshot();
        assert_eq!(&snap[..4], b"NINO");
        let mut w = io();
        w.restore(&snap).expect("restore");
        assert_eq!(w.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(w.p1, 41);
        assert_eq!(w.p2, 2);
        assert_eq!(w.latch & 0x80, 0x80);
        assert_eq!(w.sound_command, 0x42);
        assert!(w.forward_due);
        assert_eq!(w.card_bank, 0x05);
        assert_eq!(w.wdt_deadline, Some(Tick(9 + WDT_PERIOD)));
        assert!(w.restore(&snap[..4]).is_err());
        let mut foreign = w.snapshot();
        foreign[0] = b'X';
        assert!(w.restore(&foreign).is_err());
    }

    #[test]
    fn soundio_words_arrive_and_bytes_ack() {
        let mut v = sound_io();
        // The word form carries the 68k's command crossing.
        v.write(AccessWidth::Word, Address(0x0001_0000), 0x42, Tick(0));
        assert_eq!(v.latch, 0x42);
        assert!(v.nmi_pending);
        assert!(v.next.is_none(), "the pulse waits for the enable");
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0001_0000), Tick(0)),
            0x42
        );
        // The byte form is the ack: the latch and the pending drop.
        v.write(AccessWidth::Byte, Address(0x0001_0000), 0x00, Tick(0));
        assert_eq!(v.latch, 0);
        assert!(!v.nmi_pending);
    }

    #[test]
    fn soundio_nmi_pulse_gates_on_the_enable() {
        let mut v = sound_io();
        v.write(AccessWidth::Word, Address(0x0001_0000), 0x42, Tick(0));
        assert!(v.nmi_stage == 0, "disabled by reset");
        // The enable with a pending command schedules the pulse (the
        // merger's level rise).
        v.write(AccessWidth::Byte, Address(0x0001_0008), 0x00, Tick(0));
        assert!(v.nmi_en);
        assert_eq!(v.nmi_stage, 1);
        // The second arrival updates the latch only.
        v.write(AccessWidth::Word, Address(0x0001_0000), 0x5A, Tick(0));
        assert_eq!(v.latch, 0x5A);
        assert_eq!(v.nmi_stage, 1);
    }

    #[test]
    fn soundio_ack_cancels_a_yet_undelivered_pulse() {
        let mut v = sound_io();
        v.write(AccessWidth::Byte, Address(0x0001_0008), 0, Tick(0));
        v.write(AccessWidth::Word, Address(0x0001_0000), 0x42, Tick(0));
        assert_eq!(v.nmi_stage, 1);
        // The ack before the pulse's first tick cancels it.
        v.write(AccessWidth::Byte, Address(0x0001_0000), 0, Tick(0));
        assert_eq!(v.nmi_stage, 0);
        assert!(v.next.is_none());
        // The delivered High keeps its Low drop through an ack.
        v.write(AccessWidth::Word, Address(0x0001_0000), 0x42, Tick(0));
        v.nmi_stage = 2;
        v.write(AccessWidth::Byte, Address(0x0001_0000), 0, Tick(0));
        assert_eq!(v.nmi_stage, 2, "the Low drop survives");
        // The disable cancels a pending High and drops the flag.
        v.nmi_stage = 1;
        v.write(AccessWidth::Byte, Address(0x0001_0018), 0, Tick(0));
        assert!(!v.nmi_en);
        assert_eq!(v.nmi_stage, 0);
        assert!(v.next.is_none());
    }

    #[test]
    fn soundio_reply_writes_schedule_the_crossing() {
        let mut v = sound_io();
        v.write(AccessWidth::Byte, Address(0x0001_000C), 0x77, Tick(0));
        assert_eq!(v.reply, 0x77);
        assert!(v.reply_due);
        assert!(v.next.is_some());
        // The ports outside the forms answer the open io form.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0001_0030), Tick(0)),
            0xFF
        );
        // The bank-port read form answers 0 (MAME's value).
        assert_eq!(v.read(AccessWidth::Byte, Address(0x0001_0009), Tick(0)), 0);
        // The YM2610's ports fall out of the decode scope.
        assert!(!v.decodes(Address(0x0001_0004)));
        assert!(v.decodes(Address(0x0001_0000)));
        assert!(v.decodes(Address(0x0001_180C)));
        assert!(!v.decodes(Address(0x0000_0000)));
    }

    #[test]
    fn soundio_and_snapshot_round_trip() {
        let mut v = sound_io();
        v.write(AccessWidth::Byte, Address(0x0001_0008), 0, Tick(0));
        v.write(AccessWidth::Word, Address(0x0001_0000), 0x42, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0001_000C), 0x24, Tick(0));
        let snap = v.snapshot();
        assert_eq!(&snap[..4], b"NGSN");
        let mut w = sound_io();
        w.restore(&snap).expect("restore");
        assert_eq!(w.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(w.latch, 0x42);
        assert_eq!(w.reply, 0x24);
        assert!(w.nmi_en);
        assert_eq!(w.nmi_stage, 1);
        assert!(w.reply_due);
        assert!(w.next.is_some());
        let mut short = snap.clone();
        short.truncate(10);
        assert!(w.restore(&short).is_err());
    }

    /// The mini machine with the io die, the sound die, and the
    /// line probe, over the cpu and sound buses.
    fn machine_config() -> SystemConfig {
        let io = DeviceInstance {
            name: DeviceName::new("io").expect("name"),
            kind: KindName::new("neogeo.io").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::from([(
                "sound_bus".to_string(),
                toml::Value::String("sound".to_string()),
            )]),
            dividers: BTreeMap::from([("master".to_string(), 2u32)]),
        };
        let soundio = DeviceInstance {
            name: DeviceName::new("soundio").expect("name"),
            kind: KindName::new("neogeo.soundio").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let probe = DeviceInstance {
            name: DeviceName::new("probe").expect("name"),
            kind: KindName::new("test.probe").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let devices = vec![io, soundio, probe];
        let mut config = neo_config(devices, "sound");
        config.windows = vec![
            CWindow {
                name: "w_io".to_string(),
                bus: "cpu".to_string(),
                device: DeviceName::new("io").expect("name"),
                base: 0x300000,
                size: 0xC0000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
            CWindow {
                name: "w_portpage".to_string(),
                bus: "sound".to_string(),
                device: DeviceName::new("soundio").expect("name"),
                base: 0x10000,
                size: 0x20,
                kind: None,
                mirror: Some(demu_config::model::Mirror {
                    stride: 0x100,
                    count: 255,
                }),
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
        ];
        config.connections = vec![
            neo_connection("io.palbank", "probe.palbank"),
            neo_connection("io.vecbios", "probe.vecbios"),
            neo_connection("io.wdreset", "probe.wdreset"),
            neo_connection("soundio.nmi", "probe.nmi"),
        ];
        config
    }

    /// One forward connection declaration.
    fn neo_connection(from: &str, to: &str) -> demu_config::model::Connection {
        use demu_config::newtypes::{DeviceName, LineName, LinePath};
        let line_of = |path: &str| {
            let (device, line) = path.split_once('.').expect("dotted line path");
            LinePath::new(
                DeviceName::new(device).expect("device name"),
                LineName::new(line).expect("line name"),
            )
        };
        demu_config::model::Connection {
            from: line_of(from),
            to: line_of(to),
            semantics: demu_config::model::ConnectionSemantics::Irq,
            direction: demu_config::model::ConnectionDirection::Forward,
        }
    }

    /// The machine-test line probe: records every level it receives.
    #[derive(Debug, Default)]
    struct LineProbe {
        seen: RefCell<Vec<(String, Level)>>,
    }

    impl Device for LineProbe {
        fn name(&self) -> &str {
            "probe"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn reset(&mut self) {
            self.seen.borrow_mut().clear();
        }

        fn boot(&mut self, _state: Option<&BootState>) {}

        fn snapshot(&self) -> Snapshot {
            Vec::new()
        }

        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }

        fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
            self.seen.borrow_mut().push((line.to_string(), level));
        }

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    fn ctors_test() -> DeviceCtors {
        let mut ctors = DeviceCtors::new();
        ctors.register(
            "neogeo.io",
            Box::new(|d, cfg| {
                Ok(
                    Box::new(NeoIo::new(d, cfg).map_err(MachineError::Assembly)?)
                        as Box<dyn Device>,
                )
            }),
        );
        ctors.register(
            "neogeo.soundio",
            Box::new(|d, _| {
                Ok(
                    Box::new(NeoSoundIo::new(d).map_err(MachineError::Assembly)?)
                        as Box<dyn Device>,
                )
            }),
        );
        ctors.register(
            "test.probe",
            Box::new(|_, _| Ok(Box::new(LineProbe::default()) as Box<dyn Device>)),
        );
        ctors
    }

    /// The probe's recorded lines.
    fn seen_of(machine: &Machine) -> Vec<(String, Level)> {
        machine
            .device_by_name("probe")
            .and_then(|d| d.as_any().downcast_ref::<LineProbe>())
            .map(|p| p.seen.borrow().clone())
            .expect("probe exists")
    }

    #[test]
    fn sound_latch_crosses_and_pulses_nmi() {
        let mut machine = Machine::assemble(machine_config(), &ctors_test()).expect("assemble");
        machine.boot();
        // The NMI enable write on the sound side's port page.
        assert!(machine.debug_write("sound", Address(0x0001_0008), 0x00));
        // The 68k's command write: byte at the even lane.
        assert!(machine.debug_write("cpu", Address(0x0032_0000), 0x5A));
        machine.run_until(Tick(6));
        // The crossing landed in the sound die's latch.
        assert_eq!(machine.peek("sound", Address(0x0001_0000)), 0x5A);
        // The pulse pairs arrived in order.
        assert_eq!(
            seen_of(&machine),
            vec![
                ("nmi".to_string(), Level::High),
                ("nmi".to_string(), Level::Low)
            ]
        );
    }

    #[test]
    fn reply_crosses_to_the_io_window() {
        let mut machine = Machine::assemble(machine_config(), &ctors_test()).expect("assemble");
        machine.boot();
        // The Z80's reply write at port $0C.
        assert!(machine.debug_write("sound", Address(0x0001_000C), 0x77));
        machine.run_until(Tick(6));
        // The 68k reads the reply at $320000.
        assert_eq!(machine.peek("cpu", Address(0x0032_0000)), 0x77);
    }

    #[test]
    fn sound_latch_ack_flow_reads_and_clears() {
        let mut machine = Machine::assemble(machine_config(), &ctors_test()).expect("assemble");
        machine.boot();
        assert!(machine.debug_write("sound", Address(0x0001_0008), 0x00));
        assert!(machine.debug_write("cpu", Address(0x0032_0000), 0x5A));
        machine.run_until(Tick(6));
        assert_eq!(machine.peek("sound", Address(0x0001_0000)), 0x5A);
        // The ack clears the latch without further NMI work.
        assert!(machine.debug_write("sound", Address(0x0001_0000), 0x00));
        assert_eq!(machine.peek("sound", Address(0x0001_0000)), 0x00);
        let before = seen_of(&machine).len();
        machine.run_until(Tick(9));
        assert_eq!(seen_of(&machine).len(), before);
    }

    #[test]
    fn palbank_and_vecbios_drives_follow_the_latch_pairs() {
        let mut machine = Machine::assemble(machine_config(), &ctors_test()).expect("assemble");
        machine.boot();
        // $3A001F: palette bank 1. The value contributes nothing.
        assert!(machine.debug_write("cpu", Address(0x003A_001F), 0xFF));
        machine.run_until(Tick(3));
        assert_eq!(
            seen_of(&machine),
            vec![("palbank".to_string(), Level::High)]
        );
        // $3A000F: palette bank 0.
        assert!(machine.debug_write("cpu", Address(0x003A_000F), 0x00));
        machine.run_until(Tick(6));
        assert_eq!(
            seen_of(&machine),
            vec![
                ("palbank".to_string(), Level::High),
                ("palbank".to_string(), Level::Low)
            ]
        );
        // $3A0013: the P ROM vectors (the reset form of latch 1 is
        // clear, so the set write is the change that fires).
        assert!(machine.debug_write("cpu", Address(0x003A_0013), 0x00));
        machine.run_until(Tick(9));
        assert_eq!(
            seen_of(&machine),
            vec![
                ("palbank".to_string(), Level::High),
                ("palbank".to_string(), Level::Low),
                ("vecbios".to_string(), Level::Low)
            ]
        );
        // $3A0003: back to the BIOS vectors.
        assert!(machine.debug_write("cpu", Address(0x003A_0003), 0x00));
        machine.run_until(Tick(12));
        assert_eq!(
            seen_of(&machine),
            vec![
                ("palbank".to_string(), Level::High),
                ("palbank".to_string(), Level::Low),
                ("vecbios".to_string(), Level::Low),
                ("vecbios".to_string(), Level::High)
            ]
        );
    }

    #[test]
    fn controller_injection_reads_through_the_machine_bus() {
        let mut machine = Machine::assemble(machine_config(), &ctors_test()).expect("assemble");
        machine.boot();
        assert!(machine.debug_write("cpu", Address(0x0030_0000), 0xFF));
        let io_slot = machine.device_slot("io").expect("io slot");
        machine.devices[io_slot]
            .input_mut()
            .expect("the io takes input")
            .inject("p1 press 255");
        assert_eq!(machine.peek("cpu", Address(0x0030_0000)), 0x00);
        assert!(
            machine.devices[io_slot]
                .input_mut()
                .expect("the io takes input")
                .inject("p1 lift 255")
        );
        assert_eq!(machine.peek("cpu", Address(0x0030_0000)), 0xFF);
    }

    #[test]
    fn watchdog_pulses_once_per_kick_episode() {
        let mut machine = Machine::assemble(machine_config(), &ctors_test()).expect("assemble");
        machine.boot();
        // A kick at a later tick moves the deadline a full period past.
        machine.run_until(Tick(500));
        assert!(machine.debug_write("cpu", Address(0x0030_0001), 0xEE));
        machine.run_until(Tick(500 + WDT_PERIOD + 4));
        assert_eq!(seen_of(&machine), vec![("wdreset".to_string(), Level::Low)]);
    }

    #[test]
    fn machine_save_round_trip_is_byte_identical() {
        let mut machine = Machine::assemble(machine_config(), &ctors_test()).expect("assemble");
        machine.boot();
        assert!(machine.debug_write("sound", Address(0x0001_0008), 0x00));
        assert!(machine.debug_write("cpu", Address(0x0032_0000), 0x5A));
        assert!(machine.debug_write("cpu", Address(0x003A_001F), 0xFF));
        machine.run_until(Tick(64));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        // The twin machine restored from the save saves identically.
        let mut twin = Machine::assemble(machine_config(), &ctors_test()).expect("assemble twin");
        twin.boot();
        twin.load(&buf).expect("load");
        let mut buf2 = Vec::new();
        twin.save(&mut buf2).expect("save twin");
        assert_eq!(buf, buf2, "save after restore is byte-identical");
        // A second identical run saves the same bytes.
        let mut again = Machine::assemble(machine_config(), &ctors_test()).expect("assemble again");
        again.boot();
        assert!(again.debug_write("sound", Address(0x0001_0008), 0x00));
        assert!(again.debug_write("cpu", Address(0x0032_0000), 0x5A));
        assert!(again.debug_write("cpu", Address(0x003A_001F), 0xFF));
        again.run_until(Tick(64));
        let mut buf3 = Vec::new();
        again.save(&mut buf3).expect("save again");
        assert_eq!(buf, buf3, "identical runs save byte-identical state");
    }
}
