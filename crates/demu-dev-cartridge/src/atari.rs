//! The cartridge devices of the Atari machine families.
//!
//! The 2600 cart answers the mirrored 4-KiB window at `$1000-$1FFF`
//! with the flat, two-bank (`F8`), four-bank (`F6`), and eight-bank
//! (`F4`) schemes. A bankswitch hotspot is any access: reads and
//! writes in the tail of the window latch the bank.
//!
//! The 5200 cart is flat; the image sits end-aligned in the window at
//! `$4000-$BFFF`, so `$BFFE` carries the cart's start vector the BIOS
//! jumps through.
//!
//! The 7800 cart parses the classic 128-byte A78 header when present
//! and maps flat images down from `$FFFF`. Banked images page 16-KiB
//! chunks into `$8000-$BFFF` from the written value and fix the last
//! bank over `$C000-$FFFF`, hiding the BIOS the way the hardware does.
//! The audio-cartridge variants that hang POKEY inside the cart stay
//! unmodeled.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The 2600 bankswitch families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Atari2600Mode {
    /// One flat 2-KiB image, mirrored twice in the window.
    TwoK,
    /// One flat 4-KiB image.
    FourK,
    /// F8: two 4-KiB banks, hotspots at `$1FF8` and `$1FF9`.
    F8,
    /// F6: four banks, hotspots `$1FF6` through `$1FF9`.
    F6,
    /// F4: eight banks, hotspots `$1FF4` through `$1FFB`.
    F4,
}

/// The 2600 cartridge.
#[derive(Debug)]
pub struct Atari2600Cart {
    name: String,
    banks: Vec<Vec<u8>>,
    mode: Atari2600Mode,
    bank: usize,
}

impl Atari2600Cart {
    /// A cart from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            banks: Vec::new(),
            mode: Atari2600Mode::FourK,
            bank: 0,
        }
    }

    /// Split a raw image into banks by its size.
    fn configure(&mut self, bytes: &[u8]) {
        match bytes.len() {
            2048 => {
                self.mode = Atari2600Mode::TwoK;
                self.banks = vec![bytes.to_vec()];
            }
            4096 => {
                self.mode = Atari2600Mode::FourK;
                self.banks = vec![bytes.to_vec()];
            }
            8192 => {
                self.mode = Atari2600Mode::F8;
                self.banks = bytes.chunks(4096).map(<[u8]>::to_vec).collect();
            }
            16384 => {
                self.mode = Atari2600Mode::F6;
                self.banks = bytes.chunks(4096).map(<[u8]>::to_vec).collect();
            }
            32768 => {
                self.mode = Atari2600Mode::F4;
                self.banks = bytes.chunks(4096).map(<[u8]>::to_vec).collect();
            }
            _ => {
                self.mode = Atari2600Mode::FourK;
                self.banks = vec![bytes.to_vec()];
            }
        }
        self.bank = self.banks.len().saturating_sub(1);
    }

    /// The 4-KiB window byte at the bus offset 0-4095. Hotspot
    /// accesses latch the selected bank first.
    fn window_read(&mut self, offset: u16) -> u8 {
        self.hotspot(offset);
        let bank = self.bank.min(self.banks.len().saturating_sub(1));
        match self.mode {
            Atari2600Mode::TwoK => self.banks[bank][usize::from(offset % 2048)],
            _ => self.banks[bank][usize::from(offset % 4096)],
        }
    }

    /// The bankswitch hotspots switch on any access.
    fn hotspot(&mut self, offset: u16) {
        match self.mode {
            Atari2600Mode::F8 => match offset {
                0x0FF8 => self.bank = 0,
                0x0FF9 => self.bank = 1,
                _ => {}
            },
            Atari2600Mode::F6 => {
                if (0x0FF6..=0x0FF9).contains(&offset) {
                    self.bank = usize::from(offset - 0x0FF6);
                }
            }
            Atari2600Mode::F4 if (0x0FF4..=0x0FFB).contains(&offset) => {
                self.bank = usize::from(offset - 0x0FF4);
            }
            Atari2600Mode::F4 => {}
            _ => {}
        }
    }
}

impl BusAccess for Atari2600Cart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let offset = (address.0 & 0x0FFF) as u16;
        u32::from(self.window_read(offset))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, _value: u32, _tick: Tick) {
        // The hotspots switch on writes too.
        self.hotspot((address.0 & 0x0FFF) as u16);
    }

    fn peek(&self, address: Address) -> u8 {
        let offset = (address.0 & 0x0FFF) as u16;
        match self.mode {
            Atari2600Mode::TwoK => self
                .banks
                .first()
                .map_or(0, |b| b[usize::from(offset % 2048)]),
            _ => self
                .banks
                .get(self.bank)
                .and_then(|b| b.get(usize::from(offset % 4096)))
                .copied()
                .unwrap_or(0),
        }
    }
}

impl Device for Atari2600Cart {
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
        self.bank = self.banks.len().saturating_sub(1);
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.banks.clear();
        match state {
            Some(BootState::Rom { path }) => {
                let bytes = std::fs::read(path).unwrap_or_default();
                self.configure(&bytes);
            }
            Some(BootState::Hex { bytes }) => self.configure(bytes),
            _ => self.configure(&[]),
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(10 + self.banks.iter().map(Vec::len).sum::<usize>());
        out.extend_from_slice(b"A260");
        out.push(1);
        out.extend_from_slice(&(self.bank as u32).to_le_bytes());
        for b in &self.banks {
            out.extend_from_slice(b);
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 10 || &data[..4] != b"A260" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad atari 2600 cart chunk".into()));
        }
        let bank = u32::from_le_bytes(data[5..9].try_into().expect("4 bytes")) as usize;
        if bank >= self.banks.len() {
            return Err(SnapshotError::Mismatch("cart bank exceeds banks".into()));
        }
        self.bank = bank;
        let mut pos = 9usize;
        for b in &mut self.banks {
            if pos + b.len() > data.len() {
                return Err(SnapshotError::Mismatch("cart image exceeds chunk".into()));
            }
            let copy = b.len();
            b.copy_from_slice(&data[pos..pos + copy]);
            pos += copy;
        }
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

/// The 5200 cartridge: a flat image end-aligned in the window.
#[derive(Debug)]
pub struct Atari5200Cart {
    name: String,
    image: Vec<u8>,
}

impl Atari5200Cart {
    /// A cart from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            image: Vec::new(),
        }
    }

    /// The window offset of the image start, end-aligned at the top of
    /// the `$4000-$BFFF` window.
    fn base(&self) -> u32 {
        0xC000u32.saturating_sub(self.image.len() as u32)
    }
}

impl BusAccess for Atari5200Cart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.peek(address))
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, _value: u32, _tick: Tick) {
        // The flat cart has no writable space.
    }

    fn peek(&self, address: Address) -> u8 {
        if self.image.is_empty() {
            return 0;
        }
        let size = self.image.len() as u32;
        let off = address.0.saturating_sub(self.base());
        self.image[(off % size) as usize]
    }
}

impl Device for Atari5200Cart {
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

    fn reset(&mut self) {}

    fn boot(&mut self, state: Option<&BootState>) {
        match state {
            Some(BootState::Rom { path }) => {
                self.image = std::fs::read(path).unwrap_or_default();
            }
            Some(BootState::Hex { bytes }) => self.image = bytes.clone(),
            _ => self.image = Vec::new(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(5 + self.image.len());
        out.extend_from_slice(b"A520");
        out.push(1);
        out.extend_from_slice(&self.image);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 5 || &data[..4] != b"A520" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad atari 5200 cart chunk".into()));
        }
        self.image = data[5..].to_vec();
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

/// The 7800 cartridge with the A78 header and the banked schemes.
#[derive(Debug)]
pub struct Atari7800Cart {
    name: String,
    /// The 16-KiB chunks; a flat image carries one entry.
    banks: Vec<Vec<u8>>,
    banked: bool,
    bank: usize,
}

impl Atari7800Cart {
    /// A cart from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            banks: Vec::new(),
            banked: false,
            bank: 0,
        }
    }

    /// Configure from a raw or A78 image. A 128-byte header with the
    /// `ATARI7800` magic at offset 1 is stripped. Flat images stay
    /// whole; larger images split into 16-KiB chunks and page from the
    /// value written into the `$8000-$BFFF` window.
    fn configure(&mut self, bytes: &[u8]) {
        let payload: &[u8] = if bytes.len() > 128 && &bytes[1..10] == b"ATARI7800" {
            &bytes[128..]
        } else {
            bytes
        };
        let image = payload.to_vec();
        if image.len() <= 0xC000 {
            self.banked = false;
            self.banks = vec![image];
            self.bank = 0;
        } else {
            self.banked = true;
            self.banks = image.chunks(0x4000).map(<[u8]>::to_vec).collect();
            self.bank = self.banks.len().saturating_sub(1);
        }
    }

    /// A flat byte in the 48-KiB window: a 16-KiB image maps down
    /// from `$FFFF`, larger images fill `$4000-$FFFF`.
    fn flat_read(&self, address: u32) -> u8 {
        let image = match self.banks.first() {
            Some(b) => b,
            None => return 0,
        };
        let len = image.len() as u32;
        let base = u32::from(u16::MAX) + 1 - len;
        image[address.saturating_sub(base) as usize % image.len()]
    }
}

impl BusAccess for Atari7800Cart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.peek(address))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        // The banked schemes: the written value selects the bank.
        if self.banked && (0x8000..=0xBFFF).contains(&address.0) {
            self.bank = usize::from(value as u8) % self.banks.len().max(1);
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let off = address.0;
        if self.banked {
            if (0x8000..=0xBFFF).contains(&off) {
                let window = self.banks[self.bank.min(self.banks.len() - 1)].clone();
                return window[(off & 0x3FFF) as usize];
            }
            if off >= 0xC000 {
                let last = self
                    .banks
                    .last()
                    .map_or(0, |b| b.get((off & 0x3FFF) as usize).copied().unwrap_or(0));
                return last;
            }
            if off >= 0x4000 {
                let first = self
                    .banks
                    .first()
                    .map_or(0, |b| b.get((off & 0x3FFF) as usize).copied().unwrap_or(0));
                return first;
            }
            return 0;
        }
        if off >= 0x4000 {
            return self.flat_read(off);
        }
        0
    }
}

impl Device for Atari7800Cart {
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
        if self.banked {
            self.bank = self.banks.len().saturating_sub(1);
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.banks.clear();
        self.banked = false;
        self.bank = 0;
        match state {
            Some(BootState::Rom { path }) => {
                let bytes = std::fs::read(path).unwrap_or_default();
                self.configure(&bytes);
            }
            Some(BootState::Hex { bytes }) => self.configure(bytes),
            _ => self.configure(&[]),
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(11 + self.banks.iter().map(Vec::len).sum::<usize>());
        out.extend_from_slice(b"A780");
        out.push(1);
        out.push(u8::from(self.banked));
        out.extend_from_slice(&(self.bank as u32).to_le_bytes());
        for b in &self.banks {
            out.extend_from_slice(b);
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 9 || &data[..4] != b"A780" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad atari 7800 cart chunk".into()));
        }
        self.banked = data[5] != 0;
        self.bank = u32::from_le_bytes(data[6..10].try_into().expect("4 bytes")) as usize;
        if self.bank >= self.banks.len() {
            return Err(SnapshotError::Mismatch("cart bank exceeds banks".into()));
        }
        let mut pos = 10usize;
        for b in &mut self.banks {
            if pos + b.len() > data.len() {
                return Err(SnapshotError::Mismatch("cart image exceeds chunk".into()));
            }
            let copy = b.len();
            b.copy_from_slice(&data[pos..pos + copy]);
            pos += copy;
        }
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(name: &str, kind: &str) -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new(name).expect("name"),
            kind: demu_config::newtypes::KindName::new(kind).expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn f8_bankswitch_latches() {
        let mut cart = Atari2600Cart::new(&instance("cart", "atari.cart2600"));
        let mut rom = vec![0u8; 8192];
        rom[..4096].fill(0x11);
        rom[4096..].fill(0x22);
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        assert_eq!(cart.peek(Address(0x1000)), 0x22);
        cart.write(AccessWidth::Byte, Address(0x1FF8), 0, Tick(0));
        assert_eq!(cart.peek(Address(0x1000)), 0x11);
        cart.write(AccessWidth::Byte, Address(0x1FF9), 0, Tick(0));
        assert_eq!(cart.peek(Address(0x1000)), 0x22);
    }

    #[test]
    fn f6_four_banks_latch() {
        let mut cart = Atari2600Cart::new(&instance("cart", "atari.cart2600"));
        let mut rom = vec![0u8; 16384];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i / 4096) as u8 * 0x10;
        }
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        cart.write(AccessWidth::Byte, Address(0x1FF7), 0, Tick(0));
        assert_eq!(cart.peek(Address(0x1000)), 0x10);
        cart.write(AccessWidth::Byte, Address(0x1FF9), 0, Tick(0));
        assert_eq!(cart.peek(Address(0x1000)), 0x30);
    }

    #[test]
    fn two_k_mirrors_in_window() {
        let mut cart = Atari2600Cart::new(&instance("cart", "atari.cart2600"));
        let mut rom = vec![0u8; 2048];
        rom[0] = 0x77;
        rom[2047] = 0x88;
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        assert_eq!(cart.peek(Address(0x1000)), 0x77);
        assert_eq!(cart.peek(Address(0x1FFF)), 0x88);
        assert_eq!(cart.peek(Address(0x17FF)), 0x88);
    }

    #[test]
    fn cart5200_end_aligned() {
        let mut cart = Atari5200Cart::new(&instance("cart", "atari.cart5200"));
        let mut rom = vec![0u8; 8192];
        rom[0] = 0x55;
        rom[8191] = 0x66;
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        assert_eq!(cart.peek(Address(0xBFFF)), 0x66);
        assert_eq!(cart.peek(Address(0xA000)), 0x55);
    }

    #[test]
    fn cart7800_flat_maps_down_from_ffff() {
        let mut cart = Atari7800Cart::new(&instance("cart", "atari.cart7800"));
        let mut rom = vec![0u8; 0x4000];
        rom[0] = 0x44;
        rom[0x3FFA] = 0xFA; // the NMI vector pair start at $FFFA
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        assert_eq!(cart.peek(Address(0xC000)), 0x44);
        assert_eq!(cart.peek(Address(0xFFFA)), 0xFA);
    }

    #[test]
    fn cart7800_banked_pages_by_value() {
        let mut cart = Atari7800Cart::new(&instance("cart", "atari.cart7800"));
        let mut rom = vec![0u8; 128 * 1024];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i / 0x4000) as u8;
        }
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        // The last bank is fixed over $C000-$FFFF.
        assert_eq!(cart.peek(Address(0xC000)), 7);
        cart.write(AccessWidth::Byte, Address(0x8000), 2, Tick(0));
        assert_eq!(cart.peek(Address(0x8000)), 2);
        cart.write(AccessWidth::Byte, Address(0xBFFF), 5, Tick(0));
        assert_eq!(cart.peek(Address(0x8000)), 5);
    }

    #[test]
    fn a78_header_strips() {
        let mut cart = Atari7800Cart::new(&instance("cart", "atari.cart7800"));
        let mut image = vec![0u8; 128 + 0x4000];
        image[0] = 1;
        image[1..10].copy_from_slice(b"ATARI7800");
        image[128] = 0x99;
        cart.boot(Some(&BootState::Hex { bytes: image }));
        assert_eq!(cart.peek(Address(0xC000)), 0x99);
        assert_eq!(cart.peek(Address(0xFFFB)), 0);
    }

    #[test]
    fn snapshots_round_trip_with_banks() {
        let mut cart = Atari2600Cart::new(&instance("cart", "atari.cart2600"));
        let rom = vec![0x42u8; 32768];
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        cart.write(AccessWidth::Byte, Address(0x1FF6), 0, Tick(0));
        let snap = cart.snapshot();
        let mut twin = Atari2600Cart::new(&instance("cart", "atari.cart2600"));
        twin.boot(Some(&BootState::Hex {
            bytes: vec![0; 32768],
        }));
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        // $1FF6 is the third hotspot of F4 (banks count from $1FF4).
        assert_eq!(cart.bank, 2);
    }
}
