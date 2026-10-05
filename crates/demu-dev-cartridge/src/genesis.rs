//! The Sega Mega Drive cartridge: the flat program ROM with the
//! cartridge-side SRAM region, the `genesis.cart` device kind.
//!
//! # The published cartridge form
//!
//! The ROM answers flat inside its window. The cartridge SRAM sits at
//! the published $200000 region; the Sega mapper registers at
//! $A130F0-$A130FF control it: a write to $A130F1 (the SRAM control)
//! with bit 0 set enables the region and a write with bit 0 clear
//! returns the window to the ROM form, and the read of $A130F1
//! reports the same back. The SRAM keeps its bytes across the
//! session and rides the snapshot.
//!
//! The SRAM size comes from the `sram` parameter (0 disables the
//! region).

use demu_config::model::{BootState, DeviceInstance};
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The SRAM control readback form: the control byte with bit 0 set.
const SRAM_MASK: u8 = 0xFE;
/// The Sega mapper's SRAM control register, byte address.
const SEGA_SRAM_CONTROL: u32 = 0x00A1_30F1;

/// The Mega Drive cartridge.
#[derive(Debug)]
pub struct GenesisCart {
    name: String,
    /// The program ROM bytes.
    rom: Vec<u8>,
    /// The cartridge SRAM bytes (empty when the region is disabled).
    sram: Vec<u8>,
    /// True while the SRAM control bit is set.
    sram_enabled: bool,
}

impl GenesisCart {
    /// A cartridge from its configuration entry.
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        let sram_size = device
            .params
            .get("sram")
            .and_then(|v| v.as_integer())
            .unwrap_or(0)
            .clamp(0, 0x10000) as usize;
        Ok(Self {
            name: device.name.to_string(),
            rom: Vec::new(),
            sram: vec![0; sram_size],
            sram_enabled: false,
        })
    }

    /// The ROM's storage index of an address inside the window.
    fn rom_map(&self, address: u32) -> usize {
        address as usize % self.rom.len().max(1)
    }

    /// True when the address falls into the SRAM's published region.
    fn sram_hit(&self, address: u32) -> bool {
        (address & 0xFF_FFFF) & 0xFF_0000 == 0x20_0000
            && !self.sram.is_empty()
            && self.sram_enabled
            && u32::try_from(self.sram.len()).is_ok_and(|n| (address & 0xFFFF) < n)
    }

    fn sram_write(&mut self, address: Address, value: u32) {
        if self.sram_hit(address.0) {
            self.sram[(address.0 & 0xFFFF) as usize] = value as u8;
        }
    }
}

impl BusAccess for GenesisCart {
    fn read(&mut self, width: AccessWidth, address: Address, tick: Tick) -> u32 {
        let _ = (width, tick);
        if self.sram_hit(address.0) {
            u32::from(self.sram[(address.0 & 0xFFFF) as usize])
        } else if self.rom.is_empty() {
            0xFF
        } else {
            u32::from(self.rom[self.rom_map(address.0)])
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if address.0 == SEGA_SRAM_CONTROL {
            // The Sega mapper's SRAM control: bit 0 gates the region.
            self.sram_enabled = (value & 0x01) == 0x01;
            return;
        }
        self.sram_write(address, value);
    }

    fn peek(&self, address: Address) -> u8 {
        if self.sram_hit(address.0) {
            self.sram[(address.0 & 0xFFFF) as usize]
        } else if self.rom.is_empty() {
            0xFF
        } else {
            self.rom[self.rom_map(address.0)]
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        // The cart device always drives its windows; the disabled
        // SRAM region falls through the read and write handlers to
        // the ROM mirror form.
        true
    }
}

impl Device for GenesisCart {
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
        self.rom.clear();
        self.sram_enabled = false;
        for b in self.sram.iter_mut() {
            *b = 0;
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        self.rom = Vec::new();
        match state {
            Some(BootState::Rom { path }) => {
                let bytes = std::fs::read(path).unwrap_or_default();
                self.rom = bytes;
            }
            Some(BootState::Hex { bytes }) => {
                self.rom = bytes.clone();
            }
            Some(BootState::Ram { pattern }) => {
                // A pattern fill of the ROM storage, for synthetic
                // machines that boot pattern ROMs.
                let mut data = vec![0u8; self.rom.len()];
                demu_core::boot::fill_ram(&mut data, *pattern);
                self.rom = data;
            }
            _ => {}
        }
        for b in self.sram.iter_mut() {
            // The SRAM control readback form.
            *b &= SRAM_MASK;
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(10 + self.sram.len());
        out.extend_from_slice(b"MDCT");
        out.push(1);
        out.push(u8::from(self.sram_enabled));
        out.extend_from_slice(&(self.sram.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.sram);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 10 || &data[..4] != b"MDCT" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad mega drive cart chunk".into()));
        }
        self.sram_enabled = data[5] != 0;
        let len = u32::from_le_bytes(data[6..10].try_into().expect("4 bytes"));
        let len = usize::try_from(len).map_err(|_| {
            SnapshotError::Mismatch("mega drive cart chunk has a bad length".into())
        })?;
        if data.len() != 10 + len {
            return Err(SnapshotError::Mismatch(format!(
                "mega drive cart chunk must be 10 + {len} bytes"
            )));
        }
        if len != self.sram.len() {
            return Err(SnapshotError::Mismatch(format!(
                "mega drive cart chunk's SRAM form is {} bytes, the device holds {}",
                len,
                self.sram.len()
            )));
        }
        self.sram.copy_from_slice(&data[10..10 + len]);
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        None
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}
