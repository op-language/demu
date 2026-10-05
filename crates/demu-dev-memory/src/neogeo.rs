//! The Neo Geo palette RAM: the `neogeo.palette` device kind.
//!
//! Two banks of 8 KiB each, 4096 colors per bank of 16-bit color words.
//! The 68k answers through the window the configuration declares over
//! `$400000` (size `$2000`, with the published mirror over
//! `$402000-$7FFFFF`); both reads and writes land on the selected bank,
//! the MAME form (`paletteram_r` indexes `m_paletteram[m_palette_bank +
//! offset]`). Both banks boot zeroed.
//!
//! # Bank select
//!
//! The system latch of the real hardware selects the bank: a write of
//! `REG_PALBANK1` (`$3A001F`) selects bank 1 and `REG_PALBANK0`
//! (`$3A000F`) selects bank 0 through the NEO-B1 system latch. The
//! demu machine routes the decoded latch state to this device over the
//! `palbank` inbound line: Level High selects bank 1, Level Low bank 0.
//!
//! # Bus rules
//!
//! The device answers `AccessWidth::Word` and `AccessWidth::Dword`
//! reads and writes on any address whose low 13 bits index the 8 KiB
//! bank (this covers the primary window and every mirror repeat at its
//! stride, one of the modulo forms the address decoder hands through;
//! the mirror translation to the primary offset stays with the core
//! decoder). Byte reads answer `$FF` and byte writes are ignored.
//!
//! The published access form of the palette chip: the SNK hardware
//! specification marks the palette area `$400000-$401FFF` word-long,
//! and neogeodev.org records that a byte write drives both
//! write-enable lines of the RAM (`/WE` pins tied together), so one
//! byte value lands in both halves of the addressed word. Byte reads
//! drive one lane only. This build answers words as the
//! specification prescribes and leaves the byte-lane forms out of
//! build; the demu 68k issues palette updates as word accesses.
//!
//! # Color word
//!
//! One color word carries five-bit red, green, and blue plus one
//! common dark bit. Bit 15 is the dark bit; bits 14-12 hold
//! `R0 G0 B0`; bits 11-8 hold `R4 R3 R2 R1`; bits 7-4 hold
//! `G4 G3 G2 G1`; bits 3-0 hold `B4 B3 B2 B1`. A component is its
//! four MSBs shifted left one with the dark bit as the LSB, so
//! `$0F00` reads pure red, `$00F0` green, `$000F` blue, and `$8000`
//! black. neogeodev.org `Colors` pins the layout and MAME
//! `neogeo_v.cpp` `paletteram_w` implements the same packing.
//!
//! `dump_layer(VideoLayer::Palette)` answers the selected bank's 4096
//! color words as little-endian byte pairs, the word form the sibling
//! video devices use for their palette dumps. `frame()` answers
//! `None`; the palette publishes no frame.
//!
//! Sources: the SNK Neo Geo hardware specification (the address map
//! and `Color Palettes` sections), neogeodev.org pages `Palettes` and
//! `Colors`, and MAME `src/mame/snk/neogeo_v.cpp` with
//! `src/mame/snk/neogeo.cpp` for the bank select and the pen build.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The colors per bank.
const BANK_COLORS: usize = 4096;
/// The address mask of one 8 KiB bank.
const BANK_MASK: u32 = 0x1FFF;
/// The snapshot tag bytes.
const TAG: &[u8; 4] = b"NGPB";
/// The snapshot version byte.
const VERSION: u8 = 1;
/// The full snapshot length: tag, version, bank, 8192 color words.
const SNAPSHOT_LEN: usize = 4 + 1 + 1 + 2 * BANK_COLORS * 2;

fn push_word(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn peek_word(data: &[u8], offset: usize) -> Result<u16, SnapshotError> {
    let end = offset + 2;
    if data.len() < end {
        return Err(SnapshotError::Mismatch(
            "palette chunk ends inside a color word".to_string(),
        ));
    }
    let mut raw = [0u8; 2];
    raw.copy_from_slice(&data[offset..end]);
    Ok(u16::from_le_bytes(raw))
}

/// The banked palette RAM of the Neo Geo.
#[derive(Debug, Clone)]
pub struct NeoPalette {
    name: String,
    /// The two 8 KiB banks; index 0 is bank 0.
    banks: [Vec<u16>; 2],
    /// The selected bank the 68k window answers.
    bank: usize,
}

impl NeoPalette {
    /// A palette device from its configuration entry.
    pub fn new(device: &DeviceInstance, _config: &SystemConfig) -> Result<Self, String> {
        let _ = &device.params;
        Ok(Self {
            name: device.name.to_string(),
            banks: [vec![0; BANK_COLORS], vec![0; BANK_COLORS]],
            bank: 0,
        })
    }

    /// The selected bank: 0 or 1.
    pub fn selected_bank(&self) -> usize {
        self.bank
    }

    /// One color word of the selected bank, or 0 past the bank.
    pub fn color(&self, index: usize) -> u16 {
        self.banks[self.bank].get(index).copied().unwrap_or(0)
    }

    /// One color word of either bank, or 0 past the bank.
    pub fn bank_color(&self, bank: usize, index: usize) -> u16 {
        self.banks[bank & 1].get(index).copied().unwrap_or(0)
    }
}

impl NeoPalette {
    /// The offset of one address into the 8 KiB form. The primary
    /// window and each mirror repeat both reduce here, so the handler
    /// stays correct for raw addresses before and after the core
    /// decodes a mirror.
    fn offset_of(address: u32) -> usize {
        (address & BANK_MASK) as usize
    }

    /// The color index of one word-aligned address.
    fn color_index(address: u32) -> usize {
        Self::offset_of(address) >> 1
    }
}

impl BusAccess for NeoPalette {
    fn read(&mut self, width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match width {
            AccessWidth::Word | AccessWidth::Dword => {
                u32::from(self.color(Self::color_index(address.0)))
            }
            // The byte lane is not mapped; the specification prescribes
            // word and long-word accesses only.
            AccessWidth::Byte => 0xFF,
        }
    }

    fn write(&mut self, width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        match width {
            AccessWidth::Word | AccessWidth::Dword => {
                let index = Self::color_index(address.0);
                if index < BANK_COLORS {
                    self.banks[self.bank][index] = value as u16;
                }
            }
            AccessWidth::Byte => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        (self.color(Self::color_index(address.0)) >> 8) as u8
    }
}

impl Device for NeoPalette {
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

    fn reset(&mut self) {
        self.bank = 0;
        for bank in &mut self.banks {
            bank.fill(0);
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        // One bank image fills bank 0; the second stays zeroed.
        if let Some(BootState::Hex { bytes }) = state {
            let words = (bytes.len() / 2).min(BANK_COLORS);
            for (i, color) in self.banks[0][..words].iter_mut().enumerate() {
                *color = u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
            }
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SNAPSHOT_LEN);
        out.extend_from_slice(TAG);
        out.push(VERSION);
        out.push(self.bank as u8);
        for bank in &self.banks {
            for color in bank {
                push_word(&mut out, *color);
            }
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != SNAPSHOT_LEN || &data[..4] != TAG || data[4] != VERSION {
            return Err(SnapshotError::Mismatch(format!(
                "the palette chunk must be {SNAPSHOT_LEN} bytes of tag {TAG:?} version {VERSION}"
            )));
        }
        let mut pos = 6usize;
        self.bank = usize::from(data[5] & 1);
        for bank in &mut self.banks {
            for color in bank.iter_mut() {
                *color = peek_word(data, pos)?;
                pos += 2;
            }
        }
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        if line == "palbank" {
            self.bank = usize::from(level.is_high());
        }
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}

impl VideoSource for NeoPalette {
    fn frame(&self) -> Option<&demu_core::device::Frame> {
        None
    }

    /// The selected bank's color words, little-endian pairs: 8192
    /// bytes, the palette-dump convention of the sibling video
    /// devices.
    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Palette => {
                let mut out = Vec::with_capacity(BANK_COLORS * 2);
                for color in &self.banks[self.bank] {
                    push_word(&mut out, *color);
                }
                Some(out)
            }
            VideoLayer::Vram
            | VideoLayer::NameTable
            | VideoLayer::Oam
            | VideoLayer::Framebuffer => None,
        }
    }

    fn frames(&self) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(name: &str) -> DeviceInstance {
        DeviceInstance {
            name: demu_config::newtypes::DeviceName::new(name).unwrap(),
            kind: demu_config::newtypes::KindName::new("neogeo.palette").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        }
    }

    fn empty_config() -> SystemConfig {
        SystemConfig {
            name: "palette-test".to_string(),
            triple: "test".to_string(),
            description: "The palette unit-test configuration.".to_string(),
            clocks: std::collections::BTreeMap::new(),
            devices: Vec::new(),
            devices_by_name: std::collections::BTreeMap::new(),
            buses: Vec::new(),
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: std::collections::BTreeMap::new(),
            quirks: std::collections::BTreeMap::new(),
        }
    }

    fn device() -> NeoPalette {
        NeoPalette::new(&instance("pal"), &empty_config()).unwrap()
    }

    #[test]
    fn word_round_trip_writes_selected_bank() {
        let mut pal = device();
        pal.write(AccessWidth::Word, Address(0x400000), 0x1234, Tick(0));
        assert_eq!(
            pal.read(AccessWidth::Word, Address(0x400000), Tick(0)),
            0x1234
        );
        assert_eq!(pal.bank_color(0, 0), 0x1234);
        assert_eq!(pal.bank_color(1, 0), 0);

        // A mirror-style address decodes by the low 13 bits.
        pal.write(AccessWidth::Word, Address(0x402004), 0x0777, Tick(0));
        assert_eq!(
            pal.read(AccessWidth::Word, Address(0x402004), Tick(0)),
            0x0777
        );
        assert_eq!(pal.bank_color(0, 2), 0x0777);
    }

    #[test]
    fn byte_lane_is_unmapped() {
        let mut pal = device();
        pal.write(AccessWidth::Byte, Address(0x400000), 0xFF, Tick(0));
        assert_eq!(
            pal.read(AccessWidth::Byte, Address(0x400000), Tick(0)),
            0xFF
        );
        assert_eq!(pal.bank_color(0, 0), 0);
    }

    #[test]
    fn last_word_is_backdrop_index() {
        let mut pal = device();
        pal.write(AccessWidth::Word, Address(0x401FFE), 0x5AA5, Tick(0));
        assert_eq!(pal.bank_color(0, BANK_COLORS - 1), 0x5AA5);
    }

    #[test]
    fn palbank_line_flips_bank() {
        let mut pal = device();
        pal.write(AccessWidth::Word, Address(0x400010), 0x4020, Tick(0));
        pal.notify_line("palbank", Level::High, Tick(1));
        assert_eq!(pal.selected_bank(), 1);
        // The word written before the flip sat in bank 0.
        assert_eq!(pal.bank_color(0, 8), 0x4020);
        assert_eq!(pal.bank_color(1, 8), 0);
        // A write after the flip lands in bank 1.
        pal.write(AccessWidth::Word, Address(0x400000), 0x1F00, Tick(0));
        assert_eq!(pal.bank_color(1, 0), 0x1F00);
        assert_eq!(pal.bank_color(0, 0), 0);
        // The dump answers the selected bank.
        let dump = pal
            .dump_layer(VideoLayer::Palette)
            .expect("the palette dump");
        assert_eq!(dump.len(), BANK_COLORS * 2);
        assert_eq!(dump[0], 0x00);
        assert_eq!(dump[1], 0x1F);
        pal.notify_line("palbank", Level::Low, Tick(2));
        assert_eq!(pal.selected_bank(), 0);
    }

    #[test]
    fn boot_hex_fills_bank_zero() {
        let mut pal = device();
        pal.boot(Some(&BootState::Hex {
            bytes: vec![0x34, 0x12],
        }));
        assert_eq!(pal.bank_color(0, 0), 0x1234);
        assert_eq!(pal.bank_color(1, 0), 0);
        pal.reset();
        assert_eq!(pal.bank_color(0, 0), 0);
    }

    #[test]
    fn snapshot_round_trip_is_byte_identical() {
        let mut pal = device();
        pal.write(AccessWidth::Word, Address(0x400006), 0x1000, Tick(0));
        pal.write(AccessWidth::Word, Address(0x401FFE), 0x8000, Tick(0));
        pal.notify_line("palbank", Level::High, Tick(1));
        pal.write(AccessWidth::Word, Address(0x400080), 0x2468, Tick(0));
        pal.notify_line("palbank", Level::Low, Tick(2));

        let snap = pal.snapshot();
        assert_eq!(&snap[..4], TAG);
        let mut twin = device();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(twin.selected_bank(), 0);
        assert_eq!(twin.bank_color(0, 3), 0x1000);
        assert_eq!(twin.bank_color(1, 64), 0x2468);
    }

    #[test]
    fn restore_rejects_short_chunk() {
        let mut pal = device();
        assert!(matches!(
            pal.restore(&[0]),
            Err(SnapshotError::Mismatch(..))
        ));
    }
}
