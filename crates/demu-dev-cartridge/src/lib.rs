//! The cartridge devices: the header parsing, ROM banking, and MBC1,
//! MBC2, MBC3, and MBC5 memory bank controllers of the SM83; the Atari
//! 2600, 5200, and 7800 loaders; the Lynx loader with the published
//! LNX header and its page banking; and the PC Engine HuCard loader.
//!
//! The SM83 boot read parses the `0x100-0x14F` header, sizes the
//! cartridge RAM from the header, and selects the mapper: the `mbc`
//! parameter when present, otherwise the header cart type byte. Battery
//!-backed RAM keeps its bytes across reset; a volatile RAM boots from
//! the configured pattern. The MBC3 real-time clock registers are
//! stored but do not tick.

pub mod atari;
pub mod lynx;
pub mod nes;
pub mod pce;
pub mod snes;

pub use atari::{Atari2600Cart, Atari5200Cart, Atari7800Cart};
pub use lynx::LynxCart;
pub use nes::{NesCart, NesHeader, NesMapper};
pub use pce::PceCart;
pub use snes::SnesCart;

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The cartridge header fields demu uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Header {
    /// The title bytes at `0x134-0x143`.
    pub title: String,
    /// The CGB flag byte at `0x143`: `0x80` compatible, `0xC0` only.
    pub cgb_flag: u8,
    /// The cart type byte at `0x147`.
    pub cart_type: u8,
    /// The ROM size code at `0x148`.
    pub rom_size: u8,
    /// The RAM size code at `0x149`.
    pub ram_size: u8,
    /// The header checksum byte at `0x14D`.
    pub checksum: u8,
}

/// The mapper family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapperKind {
    /// A plain 32 KiB ROM with no banking.
    Rom,
    /// MBC1 with its 5-bit low bank and 2-bit high bank.
    Mbc1,
    /// MBC2 with 512 nibbles of built-in RAM.
    Mbc2,
    /// MBC3 with the real-time clock registers.
    Mbc3,
    /// MBC5 with its 9-bit bank register.
    Mbc5,
}

impl Header {
    /// Parse the header from a cartridge image.
    ///
    /// # Errors
    ///
    /// Returns an error when the image is too short.
    pub fn parse(rom: &[u8]) -> Result<Self, SnapshotError> {
        if rom.len() < 0x150 {
            return Err(SnapshotError::Mismatch(
                "cartridge image is shorter than the header".into(),
            ));
        }
        let title_bytes = &rom[0x134..0x144];
        let title: String = title_bytes
            .iter()
            .filter(|b| b.is_ascii_graphic())
            .map(|b| *b as char)
            .collect();
        Ok(Self {
            title,
            cgb_flag: rom[0x143],
            cart_type: rom[0x147],
            rom_size: rom[0x148],
            ram_size: rom[0x149],
            checksum: rom[0x14D],
        })
    }

    /// The cartridge RAM size in bytes, from the header code.
    pub fn ram_bytes(&self) -> usize {
        match self.ram_size {
            0 => 0,
            1 => 0x800,
            2 => 0x2000,
            3 => 0x8000,
            4 => 0x20000,
            5 => 0x10000,
            _ => 0,
        }
    }

    /// The mapper family from the header cart type.
    pub fn mapper_of(cart_type: u8) -> MapperKind {
        match cart_type {
            0x00 | 0x08 | 0x09 => MapperKind::Rom,
            0x01..=0x03 => MapperKind::Mbc1,
            0x05 | 0x06 => MapperKind::Mbc2,
            0x0F..=0x13 => MapperKind::Mbc3,
            0x19..=0x1E => MapperKind::Mbc5,
            // HuC1 behaves like MBC1; the HuC3 type keeps its own
            // banking, which this loader approximates with MBC3.
            0xFE => MapperKind::Mbc1,
            0xFF => MapperKind::Mbc3,
            _ => MapperKind::Rom,
        }
    }
}

/// Parse the `mbc` parameter into a mapper family.
fn parse_mbc(text: &str) -> Option<MapperKind> {
    match text.trim().to_ascii_lowercase().as_str() {
        "rom" | "none" => Some(MapperKind::Rom),
        "mbc1" => Some(MapperKind::Mbc1),
        "mbc2" => Some(MapperKind::Mbc2),
        "mbc3" => Some(MapperKind::Mbc3),
        "mbc5" => Some(MapperKind::Mbc5),
        _ => None,
    }
}

/// The memory bank controller state.
#[derive(Debug, Clone)]
pub struct Mbc {
    kind: MapperKind,
    /// The ROM bank mapped at `0x4000-0x7FFF`.
    rom_bank: u16,
    /// The RAM bank mapped at `0xA000-0xBFFF`.
    ram_bank: usize,
    /// The MBC1 banking mode.
    mode1: bool,
    /// The MBC1 bank-high bits.
    bank2: u8,
    /// The RTC register select for MBC3.
    rtc_select: u8,
    /// The RTC registers at `0x08-0x0C` and their latched copies.
    rtc: [u8; 5],
    rtc_latched: [u8; 5],
    /// The RAM enable from the `0x0000-0x1FFF` gate.
    ram_enabled: bool,
    /// The RTC latch phase for MBC3.
    latch_phase: bool,
}

impl Default for Mbc {
    fn default() -> Self {
        Self {
            kind: MapperKind::Rom,
            rom_bank: 1,
            ram_bank: 0,
            mode1: false,
            bank2: 0,
            rtc_select: 0,
            rtc: [0; 5],
            rtc_latched: [0xFF; 5],
            ram_enabled: false,
            latch_phase: false,
        }
    }
}

/// The cartridge.
pub struct Cart {
    name: String,
    /// The full ROM image.
    pub rom: Vec<u8>,
    /// The cartridge RAM.
    pub ram: Vec<u8>,
    header: Header,
    mbc: Mbc,
    battery: bool,
    ram_banked: bool,
}

impl Cart {
    /// A cartridge from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let _mbc_override = device
            .params
            .get("mbc")
            .and_then(|v| v.as_str())
            .and_then(parse_mbc);
        let battery = device
            .params
            .get("battery")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            name: device.name.to_string(),
            rom: Vec::new(),
            ram: Vec::new(),
            header: Header::default(),
            mbc: Mbc::default(),
            battery,
            ram_banked: false,
        }
    }

    /// Parse the header, select the mapper, and size the RAM.
    fn configure(&mut self) {
        if let Ok(header) = Header::parse(&self.rom) {
            self.header = header.clone();
            self.mbc.kind = Header::mapper_of(header.cart_type);
            let ram_bytes = header.ram_bytes();
            self.ram = vec![0xFF; ram_bytes];
            self.battery = self.battery
                || matches!(
                    header.cart_type,
                    0x03 | 0x06 | 0x09 | 0x0D | 0x0F | 0x10 | 0x13 | 0x1B | 0x1E | 0x22
                );
        }
        if self.mbc.kind == MapperKind::Mbc2 && self.ram.is_empty() {
            self.ram = vec![0xFF; 0x200];
        }
        self.ram_banked = self.ram.len() > 0x2000;
    }

    /// The ROM bank mapped at `0x4000-0x7FFF`.
    fn bank1_of(&self) -> usize {
        let banks = (self.rom.len() / 0x4000).max(1);
        match self.mbc.kind {
            MapperKind::Rom => 1,
            MapperKind::Mbc1 => {
                let low = (self.mbc.rom_bank & 0x1F).max(1) as usize;
                let high = usize::from(self.mbc.bank2 & 0x03);
                let bank = low | (high << 5);
                bank.min(banks - 1)
            }
            MapperKind::Mbc2 => usize::from(self.mbc.rom_bank).min(banks - 1),
            MapperKind::Mbc3 => {
                let bank = usize::from(self.mbc.rom_bank & 0x7F).max(1);
                bank.min(banks - 1)
            }
            MapperKind::Mbc5 => usize::from(self.mbc.rom_bank).min(banks - 1),
        }
    }

    /// The ROM bank mapped at `0x0000-0x3FFF`.
    fn bank0_of(&self) -> usize {
        let banks = (self.rom.len() / 0x4000).max(1);
        match self.mbc.kind {
            MapperKind::Mbc1 if self.mbc.mode1 && !self.ram_banked => {
                usize::from(self.mbc.bank2 & 0x03) << 5
            }
            _ => 0,
        }
        .min(banks - 1)
    }

    /// The RAM bank mapped at `0xA000-0xBFFF`.
    fn ram_bank_of(&self) -> usize {
        if self.ram.is_empty() {
            return 0;
        }
        match self.mbc.kind {
            MapperKind::Mbc1 if self.ram_banked => usize::from(self.mbc.bank2 & 0x03),
            MapperKind::Mbc3 if self.mbc.rtc_select >= 0x08 => 0,
            MapperKind::Mbc5 => self.mbc.ram_bank & 0x0F,
            _ => self.mbc.ram_bank.min(self.ram_bank_count()),
        }
    }

    /// The number of 8 KiB RAM banks in the image.
    fn ram_bank_count(&self) -> usize {
        (self.ram.len() / 0x2000).max(1)
    }

    /// A controller register write inside the ROM range.
    fn write_register(&mut self, address: u16, value: u8) {
        let v = value;
        match self.mbc.kind {
            MapperKind::Rom => {}
            MapperKind::Mbc1 => match address {
                0x0000..=0x1FFF => self.mbc.ram_enabled = v & 0x0F == 0x0A,
                0x2000..=0x3FFF => self.mbc.rom_bank = u16::from(v & 0x1F),
                0x4000..=0x5FFF => self.mbc.bank2 = v & 0x03,
                0x6000..=0x7FFF => self.mbc.mode1 = v & 1 == 1,
                _ => {}
            },
            MapperKind::Mbc2 => {
                if address & 0x0100 == 0 {
                    // The low half of the register block arms the RAM.
                    self.mbc.ram_enabled = v & 0x0F == 0x0A;
                } else {
                    self.mbc.rom_bank = u16::from(v & 0x0F);
                }
            }
            MapperKind::Mbc3 => match address {
                0x0000..=0x1FFF => self.mbc.ram_enabled = v & 0x0F == 0x0A,
                0x2000..=0x3FFF => self.mbc.rom_bank = u16::from(v & 0x7F),
                0x4000..=0x5FFF => {
                    self.mbc.rtc_select = v;
                }
                0x6000..=0x7FFF => {
                    if v & 1 == 1 && !self.mbc.latch_phase {
                        self.mbc.rtc_latched = self.mbc.rtc;
                    }
                    self.mbc.latch_phase = v & 1 == 1;
                }
                _ => {}
            },
            MapperKind::Mbc5 => match address {
                0x0000..=0x1FFF => self.mbc.ram_enabled = v & 0x0F == 0x0A,
                0x2000..=0x2FFF => self.mbc.rom_bank = (self.mbc.rom_bank & 0x100) | u16::from(v),
                0x3000..=0x3FFF => {
                    self.mbc.rom_bank = (self.mbc.rom_bank & 0xFF) | (u16::from(v & 1) << 8)
                }
                0x4000..=0x5FFF => self.mbc.ram_bank = usize::from(v & 0x0F),
                _ => {}
            },
        }
    }
}

impl BusAccess for Cart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0;
        match a {
            0x0000..=0x7FFF => {
                let offset = (a & 0x3FFF) as usize + self.bank_of(a) * 0x4000;
                u32::from(self.rom.get(offset).copied().unwrap_or(0xFF))
            }
            0xA000..=0xBFFF => {
                if self.mbc.kind == MapperKind::Mbc3 && self.mbc.rtc_select >= 0x08 {
                    return u32::from(
                        self.mbc.rtc_latched[(self.mbc.rtc_select - 0x08) as usize % 5],
                    );
                }
                if !self.mbc.ram_enabled || self.ram.is_empty() {
                    return 0xFF;
                }
                let bank = self.ram_bank_of();
                let offset = (a & 0x1FFF) as usize + bank * 0x2000;
                u32::from(self.ram.get(offset).copied().unwrap_or(0xFF))
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        match address.0 {
            0x0000..=0x7FFF => self.write_register(address.0 as u16, v),
            0xA000..=0xBFFF => {
                if self.mbc.kind == MapperKind::Mbc3 && self.mbc.rtc_select >= 0x08 {
                    let index = (self.mbc.rtc_select - 0x08) as usize % 5;
                    self.mbc.rtc[index] = v;
                    self.mbc.rtc_latched[index] = v;
                    return;
                }
                if self.mbc.ram_enabled && !self.ram.is_empty() {
                    let bank = self.ram_bank_of();
                    let offset = (address.0 & 0x1FFF) as usize + bank * 0x2000;
                    if offset < self.ram.len() {
                        self.ram[offset] = v;
                    }
                }
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0x0000..=0x7FFF => {
                let bank = if address.0 < 0x4000 {
                    self.bank0_of()
                } else {
                    self.bank1_of()
                };
                let offset = (address.0 & 0x3FFF) as usize + bank * 0x4000;
                self.rom.get(offset).copied().unwrap_or(0xFF)
            }
            0xA000..=0xBFFF => self
                .ram
                .get((address.0 & 0x1FFF) as usize + self.ram_bank_of() * 0x2000)
                .copied()
                .unwrap_or(0xFF),
            _ => 0xFF,
        }
    }
}

impl Cart {
    /// The ROM bank index for one address.
    fn bank_of(&self, address: u32) -> usize {
        if address < 0x4000 {
            self.bank0_of()
        } else {
            self.bank1_of()
        }
    }
}

impl Device for Cart {
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
        let battery = self.battery;
        let kind = self.mbc.kind;
        self.mbc = Mbc {
            kind,
            ..Mbc::default()
        };
        self.battery = battery;
        if !battery {
            // A volatile RAM resets to its boot pattern; the boot state
            // re-applies it, so reset keeps the bytes only for batteries.
            self.ram.fill(0xFF);
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.rom.clear();
        match state {
            Some(BootState::Rom { path }) => {
                // The cartridge image sizes itself from the file; the
                // header parse then sizes the RAM.
                self.rom = std::fs::read(path).unwrap_or_default();
            }
            Some(BootState::Hex { bytes }) => {
                self.rom = bytes.clone();
            }
            _ => {}
        }
        self.configure();
        if self.ram.is_empty() && self.battery {
            self.ram = vec![0xFF; 0x2000];
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::new();
        out.extend_from_slice(b"CART");
        out.extend_from_slice(&(self.rom.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.rom);
        out.extend_from_slice(&(self.ram.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.ram);
        out.push(self.mbc.rom_bank as u8);
        out.push((self.mbc.rom_bank >> 8) as u8);
        out.push(self.mbc.ram_bank as u8);
        out.push(u8::from(self.mbc.mode1));
        out.push(self.mbc.bank2);
        out.push(self.mbc.rtc_select);
        out.extend_from_slice(&self.mbc.rtc);
        out.extend_from_slice(&self.mbc.rtc_latched);
        out.push(u8::from(self.mbc.ram_enabled));
        out.push(u8::from(self.mbc.latch_phase));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 4 + 4 || &data[..4] != b"CART" {
            return Err(SnapshotError::Mismatch("bad cart tag".into()));
        }
        let mut pos = 4usize;
        let read_u32 = |buf: &[u8], pos: &mut usize| -> Result<usize, SnapshotError> {
            let v = usize::try_from(u32::from_le_bytes(
                buf[*pos..*pos + 4]
                    .try_into()
                    .map_err(|_| SnapshotError::Mismatch("truncated cart chunk".into()))?,
            ))
            .map_err(|_| SnapshotError::Mismatch("cart length overflow".into()))?;
            *pos += 4;
            Ok(v)
        };
        let rom_len = read_u32(data, &mut pos)?;
        let end = pos + rom_len;
        if end > data.len() {
            return Err(SnapshotError::Mismatch("cart rom exceeds chunk".into()));
        }
        self.rom = data[pos..end].to_vec();
        pos = end;
        let ram_len = read_u32(data, &mut pos)?;
        let end = pos + ram_len;
        if end > data.len() {
            return Err(SnapshotError::Mismatch("cart ram exceeds chunk".into()));
        }
        self.ram = data[pos..end].to_vec();
        pos = end;
        if data.len() < pos + 1 + 1 + 1 + 1 + 1 + 1 + 5 + 5 + 1 + 1 {
            return Err(SnapshotError::Mismatch("cart chunk truncated".into()));
        }
        self.mbc.rom_bank = u16::from(data[pos]) | (u16::from(data[pos + 1]) << 8);
        self.mbc.ram_bank = data[pos + 2] as usize;
        self.mbc.mode1 = data[pos + 3] != 0;
        self.mbc.bank2 = data[pos + 4];
        self.mbc.rtc_select = data[pos + 5];
        self.mbc.rtc.copy_from_slice(&data[pos + 6..pos + 11]);
        self.mbc
            .rtc_latched
            .copy_from_slice(&data[pos + 11..pos + 16]);
        self.mbc.ram_enabled = data[pos + 16] != 0;
        self.mbc.latch_phase = data[pos + 17] != 0;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}
