//! The memory devices: the SM83 work RAM, high RAM, and boot ROM, the
//! flat RAM and mask ROM of the Atari machine families, and the SNES
//! work RAM. The Lynx adds its boot ROM and map-control register. The
//! ZX Spectrum 128 adds its banked RAM controller and two-page banked
//! ROM.

pub mod bank_ram;
pub mod bank_rom;
pub mod lynx;
pub mod neogeo;
pub mod sneswram;
mod static_ram;

pub use bank_ram::ZxBankRam;
pub use bank_rom::Z80BankRom;
pub use lynx::{LynxBootrom, LynxMapCtl};
pub use neogeo::NeoPalette;
pub use sneswram::SnesWram;
pub use static_ram::StaticRam;

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The work RAM.
///
/// The DMG has one 8 KiB bank. The CGB has eight 4 KiB banks: bank 0
/// answers at `0xC000-0xCFFF` always and the selected bank answers at
/// `0xD000-0xDFFF`. The echo range mirrors the visible banks.
pub struct Wram {
    name: String,
    banks: Vec<Vec<u8>>,
    /// The selected bank for the `0xD000-0xDFFF` view; bank 0 reads bank 1.
    bank: usize,
    cgb: bool,
}

impl Wram {
    /// A work RAM device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let cgb = device
            .params
            .get("cgb")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let banks = if cgb {
            vec![vec![0; 0x1000]; 8]
        } else {
            vec![vec![0; 0x2000]; 1]
        };
        Self {
            name: device.name.to_string(),
            banks,
            bank: 1,
            cgb,
        }
    }

    /// The physical address inside the bank array.
    fn map(&self, address: u32) -> (usize, usize) {
        let a = address & 0x1FFF;
        if self.cgb {
            if address < 0xD000 {
                (0, a as usize)
            } else {
                (self.bank, a as usize)
            }
        } else {
            (0, address as usize & 0x1FFF)
        }
    }
}

impl BusAccess for Wram {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let (b, i) = self.map(address.0);
        u32::from(self.banks[b][i])
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if self.cgb && address.0 == 0xFF70 {
            // SVBK selects the bank for the switchable view; zero reads
            // bank 1.
            self.bank = 1.max((value as usize) & 0x07);
            return;
        }
        let (b, i) = self.map(address.0);
        self.banks[b][i] = value as u8;
    }

    fn peek(&self, address: Address) -> u8 {
        if self.cgb && address.0 == 0xFF70 {
            return (self.bank as u8) & 0x07;
        }
        let (b, i) = self.map(address.0);
        self.banks[b][i]
    }
}

impl Device for Wram {
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
        self.bank = 1;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Ram { pattern }) => {
                for bank in &mut self.banks {
                    demu_core::boot::fill_ram(bank, *pattern);
                }
            }
            Some(BootState::Hex { bytes }) => {
                let n = bytes.len().min(0x2000);
                self.banks[0][..n].copy_from_slice(&bytes[..n]);
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        let size = self.banks.first().map(Vec::len).unwrap_or(0);
        let mut out = Vec::with_capacity(1 + self.banks.len() * size);
        // A monochrome work RAM has no banks; the byte reads one.
        out.push(if self.cgb { self.bank as u8 } else { 0 });
        for bank in &self.banks {
            out.extend_from_slice(bank);
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let size = self.banks.first().map(Vec::len).unwrap_or(0);
        let total = 1 + self.banks.len() * size;
        if data.len() != total {
            return Err(SnapshotError::Mismatch(format!(
                "wram chunk must be {total} bytes"
            )));
        }
        self.bank = if self.cgb {
            (data[0] as usize).min(self.banks.len() - 1)
        } else {
            1
        };
        for (i, bank) in self.banks.iter_mut().enumerate() {
            let copy = bank.len();
            bank.copy_from_slice(&data[1 + i * size..1 + i * size + copy]);
        }
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}

impl Device for Hram {
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
            Some(BootState::Ram { pattern }) => demu_core::boot::fill_ram(&mut self.data, *pattern),
            Some(BootState::Hex { bytes }) => {
                self.data.fill(0xFF);
                let n = bytes.len().min(self.data.len());
                self.data[..n].copy_from_slice(&bytes[..n]);
            }
            _ => self.data.fill(0xFF),
        }
    }

    fn snapshot(&self) -> Snapshot {
        self.data.to_vec()
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != self.data.len() {
            return Err(SnapshotError::Mismatch(
                "hram chunk size does not match the device".into(),
            ));
        }
        self.data.copy_from_slice(data);
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}

/// The high RAM at `0xFF80-0xFFFE`.
pub struct Hram {
    name: String,
    data: Box<[u8]>,
}

impl Hram {
    /// A high RAM from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let size = device
            .params
            .get("size")
            .and_then(|v| v.as_integer())
            .map(|v| v as usize)
            .unwrap_or(0x7F);
        Self {
            name: device.name.to_string(),
            data: vec![0xFF; size].into_boxed_slice(),
        }
    }
}

impl BusAccess for Hram {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let i = (address.0 as usize - 0xFF80).min(self.data.len() - 1);
        u32::from(self.data[i])
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let i = (address.0 as usize - 0xFF80).min(self.data.len() - 1);
        self.data[i] = value as u8;
    }

    fn peek(&self, address: Address) -> u8 {
        let i = (address.0 as usize - 0xFF80).min(self.data.len() - 1);
        self.data[i]
    }
}

/// The boot ROM.
///
/// The DMG maps one 256-byte chunk at `0x0000`. The CGB maps a second
/// 2 KiB chunk at `0x0200-0x09FF` from reset. Every write to the
/// `0xFF50` bank register changes the mapping; the device falls out of
/// the address decode through [`BusAccess::decodes`] once the cartridge
/// window underneath answers instead.
pub struct Bootrom {
    name: String,
    data: Vec<u8>,
    cgb: bool,
    /// The low chunk at `0x0000-0x00FF` is mapped.
    chunk0: bool,
    /// The upper CGB chunk at `0x0200-0x08FF` is mapped.
    chunk1: bool,
}

impl Bootrom {
    /// A boot ROM from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let cgb = device
            .params
            .get("cgb")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            name: device.name.to_string(),
            data: Vec::new(),
            cgb,
            chunk0: true,
            chunk1: false,
        }
    }
}

impl Bootrom {
    /// The offset of `address` inside the boot ROM image.
    fn offset(&self, address: u32) -> Option<usize> {
        if (0x0000..=0x00FF).contains(&address) && self.chunk0 {
            return Some(address as usize);
        }
        if self.cgb && (0x0200..=0x09FF).contains(&address) && self.chunk1 {
            return Some(0x0100 + (address - 0x0200) as usize);
        }
        None
    }
}

impl BusAccess for Bootrom {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        self.peek(address).into()
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if address.0 == 0xFF50 {
            let value = value as u8;
            if !self.cgb || value == 0xFF {
                // Any DMG write unmaps; the CGB writes $FF to unmap all.
                self.chunk0 = false;
                self.chunk1 = false;
            } else if value == 0x00 {
                // The CGB maps the upper chunk in for its extra code.
                self.chunk1 = true;
            }
        }
    }

    fn peek(&self, address: Address) -> u8 {
        self.offset(address.0)
            .and_then(|o| self.data.get(o).copied())
            .unwrap_or(0xFF)
    }

    fn decodes(&self, address: Address) -> bool {
        match address.0 {
            0xFF50 => self.chunk0 || (self.cgb && self.chunk1),
            0x0000..=0x00FF => self.chunk0,
            0x0200..=0x09FF => self.cgb && self.chunk1,
            _ => false,
        }
    }
}

impl Device for Bootrom {
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
        self.chunk0 = true;
        self.chunk1 = false;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Rom { path }) => {
                let _ = demu_core::boot::load_rom(&mut self.data, std::path::Path::new(path));
            }
            Some(BootState::Hex { bytes }) => {
                self.data = bytes.clone();
            }
            _ => {}
        }
        // The CGB maps both chunks at reset; the $FF50 write at the end
        // of the low chunk reveals the cart and unmaps everything.
        if self.cgb && self.data.len() > 0x100 {
            self.chunk1 = true;
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(3 + self.data.len());
        out.push(u8::from(self.chunk0));
        out.push(u8::from(self.chunk1));
        out.push(u8::from(self.cgb));
        out.extend_from_slice(&self.data);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 3 {
            return Err(SnapshotError::Mismatch("bootrom chunk too short".into()));
        }
        self.chunk0 = data[0] != 0;
        self.chunk1 = data[1] != 0;
        self.cgb = data[2] != 0;
        self.data = data[3..].to_vec();
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}
