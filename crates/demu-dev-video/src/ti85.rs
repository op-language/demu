//! The TI-85 liquid-crystal display driver as the `ti85.lcd` device
//! kind.
//!
//! The TI-85 display driver is memory-mapped on the CPU bus. This
//! build pins the following as-built model:
//!
//! - The display bit buffer spans `$FC00-$FFFF`, 1024 bytes = 64 rows
//!   of 16 bytes, row-major, with bit 7 the leftmost pixel of the
//!   row. Every read and write inside the span hits the bit buffer
//!   directly, with no auto-increment and no address latching.
//! - The driver page `$FEC0-$FEFF` rides inside the buffer span; the
//!   device answers those 64 bytes first, so the bit-buffer bytes
//!   under the driver page (buffer rows 44-47) stay unreachable in
//!   this build.
//! - The driver decodes three addresses: `$FEC0` write latches the
//!   next command, `$FEC1` write supplies the data byte and runs the
//!   latched command, and `$FEC2` write latches the keypad column
//!   while `$FEC2` read returns the row mask of the latched column.
//!
//! # Driver commands
//!
//! The `$FEC0` write stores the command byte and arms the data step.
//! The `$FEC1` write executes the armed command with its data byte and
//! disarms:
//!
//! | Command | Behavior |
//! |---------|----------|
//! | `0x01` | fill the display bit buffer with the data byte, so a `0` data byte clears |
//! | `0x02` | home the cursor: accepted, stored with no effect, this build keeps no cursor state |
//! | `0x08` | display off: the frame publishes the paper color for every pixel |
//! | `0x0C` | display on: the frame republishes the bit buffer |
//!
//! Any other command byte is accepted and takes no effect. A `$FEC1`
//! write without an armed command takes no effect. The `$FEC0` read
//! returns the armed command byte, or `$FF` when no command arms; the
//! `$FEC1` read and the `$FEC3-$FEFF` reads return `$00`.
//!
//! # Keypad scan
//!
//! The keypad is a 7-row by 8-column matrix; the device holds one
//! pressed bit per key. The `$FEC2` write latches the scanned column
//! from bits 2-0. The `$FEC2` read returns one row byte for the
//! latched column: bits 0-6 carry rows 0-6 with bit 7 fixed at 1, a
//! pressed key drives its row bit low, so a set bit reads released and
//! a clear bit reads pressed.
//!
//! # Key matrix
//!
//! The device accepts the 35 key names of the TI-85 set through the
//! event grammar `press <key>` and `release <key>` and rejects every
//! other event with `false`. The matrix places the keys in the listed
//! order, row by row: column = index mod 8 and row = index / 8, with
//! rows 5 and 6 empty:
//!
//! | Row | Columns 0 through 7 |
//! |-----|---------------------|
//! | 0 | second, mode, alpha, xvar, stat, down, left, right |
//! | 1 | up, enter, plus, minus, mult, div, seven, eight |
//! | 2 | nine, caret, four, five, six, log, one, two |
//! | 3 | three, ln, zero, dot, neg, graph, f1, f2 |
//! | 4 | f3, f4, f5 |
//!
//! # Frame and snapshot
//!
//! The frame is 128x64; a lit pixel reads the ink RGB `(24, 84, 24)`
//! and an unlit pixel reads the paper RGB `(232, 240, 232)`. The
//! cached frame refreshes at the end of every bit-buffer write, after
//! the fill, on-off, and data commands, and after the restore and the
//! boot; `frames` counts those refreshes, with the restore
//! republishing without counting so the re-snapshot after a restore
//! matches the pre-restore snapshot byte for byte.
//!
//! The snapshot tag is `TILC`, version 1: the 1024-bit-buffer bytes,
//! the armed command byte, the latch phase, the display-enable flag,
//! the keypad column latch, the 56 key bits as seven `$u64$` rows, and
//! the refresh counter.
//!
//! Sources: the published TI-85 hardware reference documents and the
//! TI-85 ROM disassembly notes for the keypad matrix, with the
//! as-built driver command set of this package.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, InputSink, Snapshot, SnapshotError};

/// The base of the display bit-buffer span.
const DISPLAY_BASE: u32 = 0xFC00;
/// The byte count of the display bit-buffer span, 64 rows of 16.
const DISPLAY_BYTES: usize = 0x0400;
/// The base of the driver page inside the display span.
const DRIVER_BASE: u32 = 0xFEC0;
/// The exclusive end of the driver page inside the display span.
const DRIVER_END: u32 = 0xFF00;
/// The display bit width.
const LCD_W: usize = 128;
/// The display bit height.
const LCD_H: usize = 64;
/// The keypad matrix rows.
const KEY_ROWS: usize = 7;
/// The keypad matrix columns.
const KEY_COLS: usize = 8;
/// No command arms.
const CMD_NONE: u8 = 0xFF;
/// The snapshot byte length: tag, version, the buffer, the command
/// bytes, the display flag, the column latch, 56 key bits, and the
/// counter.
const SNAP_LEN: usize = 4 + 1 + DISPLAY_BYTES + 1 + 1 + 1 + 1 + KEY_ROWS * KEY_COLS + 8;

/// The lit pixel color.
const INK: [u8; 3] = [24, 84, 24];
/// The unlit pixel color.
const PAPER: [u8; 3] = [232, 240, 232];

/// The accepted key names in the row-major matrix order: row =
/// index / 8, column = index % 8.
const KEY_NAMES: [&str; 35] = [
    "second", "mode", "alpha", "xvar", "stat", "down", "left", "right", "up", "enter", "plus",
    "minus", "mult", "div", "seven", "eight", "nine", "caret", "four", "five", "six", "log", "one",
    "two", "three", "ln", "zero", "dot", "neg", "graph", "f1", "f2", "f3", "f4", "f5",
];

/// The matrix slot of one key name: the row and the column.
fn key_slot(name: &str) -> Option<(usize, usize)> {
    let index = KEY_NAMES.iter().position(|n| *n == name)?;
    Some((index / KEY_COLS, index % KEY_COLS))
}

/// The TI-85 display driver.
#[derive(Debug)]
pub struct Ti85Lcd {
    name: String,
    /// The display bit buffer, 64 rows of 16 bytes.
    buffer: Box<[u8; DISPLAY_BYTES]>,
    /// The display enable from the off and on commands.
    display_on: bool,
    /// The armed command byte, `CMD_NONE` when nothing arms.
    cmd_latch: u8,
    /// The latch phase: 0 idle, 1 command armed.
    cmd_phase: u8,
    /// The latched keypad column, bits 2-0.
    key_column: u8,
    /// The key state: bit `col` of `keys[row]` = pressed.
    keys: [u64; KEY_ROWS],
    /// The published frame in RGB888.
    frame: Frame,
    /// The refresh count, published as the frame counter.
    frames: u64,
}

impl Ti85Lcd {
    /// A display driver from its configuration entry.
    pub fn new(device: &DeviceInstance, _config: &SystemConfig) -> Self {
        let mut lcd = Self {
            name: device.name.to_string(),
            buffer: Box::new([0; DISPLAY_BYTES]),
            display_on: true,
            cmd_latch: CMD_NONE,
            cmd_phase: 0,
            key_column: 0,
            keys: [0; KEY_ROWS],
            frame: Frame {
                width: LCD_W as u32,
                height: LCD_H as u32,
                pixels: vec![0; LCD_W * LCD_H * 3],
            },
            frames: 0,
        };
        lcd.refresh_frame();
        lcd
    }

    /// Copy the bit buffer into the cached frame in the ink and paper
    /// colors; the disabled display publishes the paper color whole.
    fn refresh_frame(&mut self) {
        self.republish();
        self.frames += 1;
    }

    /// The frame copy without the counter step; the restore uses this
    /// form to keep the re-snapshot byte-identical.
    fn republish(&mut self) {
        self.frame.pixels.clear();
        self.frame.pixels.reserve(LCD_W * LCD_H * 3);
        for row in 0..LCD_H {
            for px in 0..LCD_W {
                let byte = if self.display_on {
                    self.buffer[row * 16 + (px >> 3)]
                } else {
                    0
                };
                let lit = (byte >> (7 - (px & 7))) & 1 != 0;
                let rgb = if lit { INK } else { PAPER };
                self.frame.pixels.extend_from_slice(&rgb);
            }
        }
    }

    /// Execute the armed command with its data byte.
    fn run_command(&mut self, data: u8) {
        match self.cmd_latch {
            // The fill command: a zero data byte clears.
            0x01 => {
                self.buffer.fill(data);
                self.refresh_frame();
            }
            // The home command: this build keeps no cursor state.
            0x02 => {}
            0x08 => {
                self.display_on = false;
                self.refresh_frame();
            }
            0x0C => {
                self.display_on = true;
                self.refresh_frame();
            }
            _ => {}
        }
    }

    /// The keypad row mask of the latched column: bit 7 fixed at 1, a
    /// pressed key drives its row bit low.
    fn key_row_byte(&self) -> u8 {
        let mut out = 0x80;
        let col = usize::from(self.key_column & (KEY_COLS as u8 - 1));
        for row in 0..KEY_ROWS {
            if (self.keys[row] >> col) & 1 == 0 {
                out |= 1 << row;
            }
        }
        out
    }

    /// The driver-page byte at one address, or `None` outside the page.
    fn driver_byte(&self, a: u32) -> Option<u8> {
        if !(DRIVER_BASE..DRIVER_END).contains(&a) {
            return None;
        }
        Some(match a {
            0xFEC0 => {
                if self.cmd_phase == 1 {
                    self.cmd_latch
                } else {
                    CMD_NONE
                }
            }
            0xFEC2 => self.key_row_byte(),
            _ => 0,
        })
    }

    /// The buffer offset of one display address, or `None` under the
    /// driver page.
    fn buffer_offset(a: u32) -> Option<usize> {
        let off = (a.checked_sub(DISPLAY_BASE)?) as usize;
        if off >= DISPLAY_BYTES || (DRIVER_BASE..DRIVER_END).contains(&a) {
            return None;
        }
        Some(off)
    }

    /// One pressed/release event against the matrix; unknown events
    /// and names return false.
    fn apply_event(&mut self, event: &str) -> bool {
        let Some((verb, key)) = event.split_once(' ') else {
            return false;
        };
        let Some((row, col)) = key_slot(key.trim()) else {
            return false;
        };
        match verb {
            "press" => self.keys[row] |= 1 << col,
            "release" => self.keys[row] &= !(1 << col),
            _ => return false,
        }
        true
    }
}

impl BusAccess for Ti85Lcd {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0;
        match a {
            // The driver page answers first.
            0xFEC0..=0xFEFF => u32::from(self.driver_byte(a).unwrap_or(0)),
            0xFC00..=0xFFFF => u32::from(self.buffer[Self::buffer_offset(a).unwrap_or(0)]),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        let a = address.0;
        match a {
            0xFEC0 => {
                self.cmd_latch = v;
                self.cmd_phase = 1;
            }
            0xFEC1 => {
                if self.cmd_phase == 1 {
                    self.cmd_phase = 0;
                    self.run_command(v);
                }
            }
            0xFEC2 => self.key_column = v & (KEY_COLS as u8 - 1),
            0xFEC3..=0xFEFF => {}
            0xFC00..=0xFFFF => {
                if let Some(off) = Self::buffer_offset(a) {
                    self.buffer[off] = v;
                    self.refresh_frame();
                }
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0;
        match a {
            0xFEC0..=0xFEFF => self.driver_byte(a).unwrap_or(0),
            0xFC00..=0xFFFF => self.buffer[Self::buffer_offset(a).unwrap_or(0)],
            _ => 0xFF,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl InputSink for Ti85Lcd {
    fn inject(&mut self, event: &str) -> bool {
        self.apply_event(event)
    }
}

impl Device for Ti85Lcd {
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

    fn video(&self) -> Option<&dyn VideoSource> {
        Some(self)
    }

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.buffer.fill(0);
        self.display_on = true;
        self.cmd_latch = CMD_NONE;
        self.cmd_phase = 0;
        self.key_column = 0;
        self.keys = [0; KEY_ROWS];
        self.refresh_frame();
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SNAP_LEN);
        out.extend_from_slice(b"TILC");
        out.push(1);
        out.extend_from_slice(self.buffer.as_ref());
        out.push(self.cmd_latch);
        out.push(self.cmd_phase);
        out.push(u8::from(self.display_on));
        out.push(self.key_column);
        for row in &self.keys {
            out.extend_from_slice(&row.to_le_bytes());
        }
        out.extend_from_slice(&self.frames.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != SNAP_LEN || data[0..4] != *b"TILC" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "the ti85 lcd chunk must be {SNAP_LEN} bytes, tag TILC, and version 1"
            )));
        }
        let mut pos = 5usize;
        self.buffer.copy_from_slice(&data[pos..pos + DISPLAY_BYTES]);
        pos += DISPLAY_BYTES;
        self.cmd_latch = data[pos];
        pos += 1;
        self.cmd_phase = data[pos];
        pos += 1;
        self.display_on = data[pos] != 0;
        pos += 1;
        self.key_column = data[pos];
        pos += 1;
        for row in 0..KEY_ROWS {
            self.keys[row] = u64::from(data[pos])
                | u64::from(data[pos + 1]) << 8
                | u64::from(data[pos + 2]) << 16
                | u64::from(data[pos + 3]) << 24
                | u64::from(data[pos + 4]) << 32
                | u64::from(data[pos + 5]) << 40
                | u64::from(data[pos + 6]) << 48
                | u64::from(data[pos + 7]) << 56;
            pos += 8;
        }
        self.frames = u64::from(data[pos])
            | u64::from(data[pos + 1]) << 8
            | u64::from(data[pos + 2]) << 16
            | u64::from(data[pos + 3]) << 24
            | u64::from(data[pos + 4]) << 32
            | u64::from(data[pos + 5]) << 40
            | u64::from(data[pos + 6]) << 48
            | u64::from(data[pos + 7]) << 56;
        // The restore republishes without a count step.
        self.republish();
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

impl VideoSource for Ti85Lcd {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => Some(self.buffer.to_vec()),
            VideoLayer::Framebuffer => Some(self.frame.pixels.clone()),
            _ => None,
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_config::newtypes::{DeviceName, Frequency, KindName};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    fn lcd_instance() -> DeviceInstance {
        DeviceInstance {
            name: DeviceName::new("lcd").expect("name"),
            kind: KindName::new("ti85.lcd").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    /// One machine with the `$FC00-$FFFF` window on the `cpu` bus.
    fn lcd_machine() -> Machine {
        let lcd = lcd_instance();
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: Frequency::new(6_500_000, 1).expect("freq"),
                origin: "test".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(lcd.name.clone(), 0usize);
        let config = SystemConfig {
            name: "ti85-test".to_string(),
            triple: "z80/ti85".to_string(),
            description: "the ti85 test machine".to_string(),
            clocks,
            devices: vec![lcd],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "lcd".to_string(),
                bus: "cpu".to_string(),
                device: lcd_instance().name,
                base: 0xFC00,
                size: 0x0400,
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
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register(
            "ti85.lcd",
            Box::new(|d, c| Ok(Box::new(Ti85Lcd::new(d, c)))),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("assemble");
        machine.boot();
        machine
    }

    fn lcd_of(machine: &mut Machine) -> &mut Ti85Lcd {
        let any = machine
            .device_by_name_mut("lcd")
            .expect("device")
            .as_any_mut();
        any.downcast_mut::<Ti85Lcd>().expect("the lcd")
    }

    #[test]
    fn a_display_write_lights_the_frame_pixel() {
        let mut machine = lcd_machine();
        assert!(machine.debug_write("cpu", Address(0xFC00), 0x80));
        let video = machine.device_by_name("lcd").expect("device").video();
        let frame = video.expect("video").frame().expect("frame");
        assert_eq!((frame.width, frame.height), (128, 64));
        assert_eq!(frame.pixels.len(), 128 * 64 * 3);
        assert_eq!(frame.pixels[0..3], INK);
        assert_eq!(frame.pixels[3..6], PAPER);
    }

    #[test]
    fn buffer_bits_place_the_pixels_row_major() {
        let mut s = Ti85Lcd::new(&lcd_instance(), &demo_config());
        // Row 5, byte 2, bit 4: bit 7 is the leftmost pixel, so byte 2
        // covers columns 16-23 and bit 4 lights column 19.
        let port = 0xFC00 + 5 * 16 + 2;
        s.write(AccessWidth::Byte, Address(port), 0x10, Tick(0));
        let base = (5 * 128 + 19) * 3;
        assert_eq!(s.frame.pixels[base..base + 3], INK);
        assert_eq!(s.frame.pixels[0..3], PAPER);
        // The rows hold 16 bytes, so row 6 sits at `$FCE0`.
        assert_eq!(s.dump_layer(VideoLayer::Vram).expect("vram").len(), 1024);
    }

    #[test]
    fn the_clear_command_zeroes_the_buffer() {
        let mut machine = lcd_machine();
        for a in 0xFC00..0xFC04 {
            machine.debug_write("cpu", Address(a), 0xFF);
        }
        assert!(machine.debug_write("cpu", Address(0xFEC0), 0x01));
        assert!(machine.debug_write("cpu", Address(0xFEC1), 0x00));
        let buf = lcd_of(&mut machine).buffer.clone();
        assert!(buf.iter().all(|b| *b == 0));
    }

    #[test]
    fn the_fill_command_takes_the_data_byte() {
        let mut machine = lcd_machine();
        assert!(machine.debug_write("cpu", Address(0xFEC0), 0x01));
        assert!(machine.debug_write("cpu", Address(0xFEC1), 0xFF));
        let video = machine.device_by_name("lcd").expect("device").video();
        let frame = video.expect("video").frame().expect("frame");
        let all_ink = frame.pixels.as_chunks::<3>().0.iter().all(|c| *c == INK);
        assert!(all_ink);
    }

    #[test]
    fn the_display_off_command_publishes_paper() {
        let mut machine = lcd_machine();
        assert!(machine.debug_write("cpu", Address(0xFC00), 0xFF));
        assert!(machine.debug_write("cpu", Address(0xFEC0), 0x08));
        assert!(machine.debug_write("cpu", Address(0xFEC1), 0x00));
        let video = machine.device_by_name("lcd").expect("device").video();
        let frame = video.expect("video").frame().expect("frame");
        assert!(frame.pixels.as_chunks::<3>().0.iter().all(|c| *c == PAPER));
        // The on command republishes the bit buffer.
        assert!(machine.debug_write("cpu", Address(0xFEC0), 0x0C));
        assert!(machine.debug_write("cpu", Address(0xFEC1), 0x00));
        let video = machine.device_by_name("lcd").expect("device").video();
        let frame = video.expect("video").frame().expect("frame");
        assert_eq!(frame.pixels[0..3], INK);
    }

    #[test]
    fn the_home_command_stores_without_effect() {
        let mut machine = lcd_machine();
        machine.debug_write("cpu", Address(0xFC00), 0xAA);
        let frames = lcd_of(&mut machine).frames;
        assert!(machine.debug_write("cpu", Address(0xFEC0), 0x02));
        assert!(machine.debug_write("cpu", Address(0xFEC1), 0x00));
        let s = lcd_of(&mut machine);
        assert_eq!(s.buffer[0], 0xAA);
        // The harmless command still consumed the latch.
        assert_eq!(s.cmd_phase, 0);
        assert_eq!(s.frames, frames);
    }

    #[test]
    fn a_data_write_without_a_command_takes_no_effect() {
        let mut machine = lcd_machine();
        machine.debug_write("cpu", Address(0xFC00), 0x55);
        let frames = lcd_of(&mut machine).frames;
        assert!(machine.debug_write("cpu", Address(0xFEC1), 0x01));
        let s = lcd_of(&mut machine);
        assert_eq!(s.buffer[0], 0x55);
        assert_eq!(s.frames, frames);
    }

    #[test]
    fn the_command_read_reports_the_armed_byte() {
        let mut s = Ti85Lcd::new(&lcd_instance(), &demo_config());
        s.write(AccessWidth::Byte, Address(0xFEC0), 0x0C, Tick(0));
        assert_eq!(
            s.read(AccessWidth::Byte, Address(0xFEC0), Tick(0)),
            u32::from(0x0Cu8)
        );
        s.write(AccessWidth::Byte, Address(0xFEC1), 0x00, Tick(0));
        assert_eq!(
            s.read(AccessWidth::Byte, Address(0xFEC0), Tick(0)),
            u32::from(0xFFu8)
        );
    }

    #[test]
    fn a_key_press_drives_its_row_bit_low() {
        let mut machine = lcd_machine();
        // The release row reads with every row bit set.
        assert!(machine.debug_write("cpu", Address(0xFEC2), 0x01));
        assert_eq!(machine.peek("cpu", Address(0xFEC2)), 0xFF);
        // The `enter` key sits at row 1, column 1: press, then the read
        // carries the row 1 bit low.
        {
            let lcd = lcd_of(&mut machine);
            assert!(lcd.inject("press enter"));
        }
        assert_eq!(machine.peek("cpu", Address(0xFEC2)), 0xFD);
        // The release restores the idle row.
        {
            let lcd = lcd_of(&mut machine);
            assert!(lcd.inject("release enter"));
        }
        assert_eq!(machine.peek("cpu", Address(0xFEC2)), 0xFF);
    }

    #[test]
    fn unknown_keys_and_events_reject() {
        let mut s = Ti85Lcd::new(&lcd_instance(), &demo_config());
        assert!(!s.inject("press bogus"));
        assert!(!s.inject("tap enter"));
        assert!(!s.inject("enter"));
        assert!(s.inject("press f5"));
        // The `f5` key sits at row 4, column 2.
        assert_eq!(s.keys[4] & (1 << 2), 1 << 2);
    }

    #[test]
    fn the_column_latch_masks_the_byte() {
        let mut machine = lcd_machine();
        // The `zero` key sits at row 3, column 2: column 2 or the masked
        // byte 10 both scan it.
        {
            let lcd = lcd_of(&mut machine);
            assert!(lcd.inject("press zero"));
        }
        assert!(machine.debug_write("cpu", Address(0xFEC2), 0x02));
        assert_eq!(machine.peek("cpu", Address(0xFEC2)), 0xF7);
        assert!(machine.debug_write("cpu", Address(0xFEC2), 0x0A));
        assert_eq!(machine.peek("cpu", Address(0xFEC2)), 0xF7);
    }

    #[test]
    fn the_snapshot_round_trips() {
        let mut machine = lcd_machine();
        machine.debug_write("cpu", Address(0xFC10), 0x99);
        {
            let lcd = lcd_of(&mut machine);
            assert!(lcd.inject("press seven"));
            lcd.display_on = false;
            lcd.refresh_frame();
        }
        let counter = lcd_of(&mut machine).frames;
        let snap = lcd_of(&mut machine).snapshot();
        assert_eq!(snap.len(), SNAP_LEN);
        let mut twin = Ti85Lcd::new(&lcd_instance(), &demo_config());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        // The restored display off flag publishes paper.
        assert!(
            twin.frame
                .pixels
                .as_chunks::<3>()
                .0
                .iter()
                .all(|c| *c == PAPER)
        );
        // The restored key sits at row 1, column 6.
        assert_eq!(twin.keys[1] & (1 << 6), 1 << 6);
        // The restore kept the counter exact.
        assert_eq!(twin.frames, counter);
    }

    #[test]
    fn the_snapshot_rejects_a_short_chunk() {
        let mut s = Ti85Lcd::new(&lcd_instance(), &demo_config());
        let snap = s.snapshot();
        assert!(s.restore(&snap[..SNAP_LEN - 1]).is_err());
        assert!(s.restore(&snap).is_ok());
    }

    #[test]
    fn a_restore_republishes_the_frame() {
        let mut s = Ti85Lcd::new(&lcd_instance(), &demo_config());
        s.write(AccessWidth::Byte, Address(0xFC00), 0xFF, Tick(0));
        let counter = s.frames;
        let snap = s.snapshot();
        let mut twin = Ti85Lcd::new(&lcd_instance(), &demo_config());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.frame.pixels[0..3], INK);
        assert_eq!(twin.frames, counter);
    }

    /// The smallest configuration shape for direct device tests.
    fn demo_config() -> SystemConfig {
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: Frequency::from_hz(6_500_000).expect("freq"),
                origin: "test".to_string(),
            },
        );
        SystemConfig {
            name: "demo".to_string(),
            triple: "z80/ti85".to_string(),
            description: String::new(),
            clocks,
            devices: Vec::new(),
            devices_by_name: BTreeMap::new(),
            buses: Vec::new(),
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        }
    }
}
