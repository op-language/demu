//! The Neo Geo program-side cartridge silicon: the 68k-bus P ROM with
//! its banked second window and BIOS vector mux (`neogeo.cart`), and
//! the Z80-bus M1 ROM with the static window, the four banked sound
//! windows, and the bank ports (`neogeo.m1rom`) — the two cartridge
//! chips the machine's sound and program buses serve.
//!
//! # The P ROM chip (`neogeo.cart`)
//!
//! The cart answers two windows the configuration declares:
//!
//! | Window | Form |
//! |--------|------|
//! | `$000000-$0FFFFF` | the flat first MiB; `$000000-$00007F` routes through the BIOS vector mux |
//! | `$200000-$2FFFFF` | the banked second window |
//!
//! ## The banked second window
//!
//! The cart latches a bank number that shifts the `$200000-$2FFFFF`
//! read route. The pinned form (MAME `write_banksel`, which installs
//! the write handler at `$2FFFF0-$2FFFFF` over the read bank
//! `$200000-$2FFFFF`):
//!
//! | Fact | Value | Source |
//! |------|-------|--------|
//! | Switch write addresses | word writes to `$2FFFF0-$2FFFFF` | MAME `install_write_handler(0x2ffff0, 0x2fffff)` |
//! | Bank encoding | bits 2-0 of the written word, so the low data lane carries the bank in byte terms | MAME `int bank = data & 0x07` |
//! | Bank base | `(bank + 1) * 1 MiB` from the P ROM start: bank 0 selects the second MiB; the first MiB serves `$000000-$0FFFFF` flat | MAME `m_bank_base = (bank + 1) * 0x100000` |
//! | Empty-bank guard | when `(bank + 1) * 1 MiB >= len`, bank 0 applies and MAME logs a warning | MAME `bankswitch to empty bank` |
//! | No-banks guard | RAM of 1 MiB or less and a nonzero bank number: the latch keeps its value | MAME `bankswitch but no banks available` |
//! | Power-up bank | 1 MiB when the image exceeds 1 MiB, else base 0 (the first MiB mirrors into the window) | MAME `init_cpu` |
//!
//! Reads past the image end wrap through the modulo form (the
//! established `rom_map` form of the sibling cart) where MAME reads
//! its padded region tail — a recorded approximation the synthetic
//! images this build serves stay clear of for aligned banks.
//!
//! ## The BIOS vector mux
//!
//! The system latch bit 1 (`$3A0003` reset / `$3A0013` set, the
//! published `REG_SWPBIOS`/`REG_SWPROM` pair) switches the first
//! `$80` bytes of the `$000000` region between the BIOS ROM's vector
//! table and the cart's (Charles MacDonald's `System control
//! register`: "Vector table select (0= BIOS, 1= Cartridge)"; MAME's
//! `banked_vectors_r` answers `bios[offset]` or `rom[offset]`). The
//! 68k-side IO die drives the `vecbios` line: Level High selects the
//! BIOS bytes, Level Low the P ROM. The reset form is the BIOS
//! vectors, the `74HC259` clear state and MAME's
//! `m_use_cart_vectors = 0`. The mux answers bytes: the vector word
//! at `$000000-$00007F` rides the same byte lanes MAME's word read takes.
//!
//! The BIOS bytes come from the boot state of the device the `bios`
//! parameter names at construction (default `bios`); the die's own
//! boot state carries the P ROM image (`Hex` clones, `Rom` reads the
//! path, other states leave the image empty). Empty forms read `$FF`.
//!
//! # The M1 ROM chip (`neogeo.m1rom`)
//!
//! The Z80's program ROM. The image comes from the die's boot state.
//! The device answers the Z80's published memory form (MAME
//! `audio_map`) with bank latches per window:
//!
//! | Window | Size | Bank shift |
//! |--------|------|-----------|
//! | `$0000-$7FFF` | 32 KiB static, ROM offset 0 | — |
//! | `$8000-$BFFF` | 16 KiB, bank latch 3 | 14 |
//! | `$C000-$DFFF` | 8 KiB, bank latch 2 | 13 |
//! | `$E000-$EFFF` | 4 KiB, bank latch 1 | 12 |
//! | `$F000-$F7FF` | 2 KiB, bank latch 0 | 11 |
//!
//! Banked reads route `rom[(bank << shift) | offset]` modulo the
//! image size; the static window routes `rom[offset]` modulo. MAME's
//! audio-cart region layout banks from `0x10000` because the loader
//! stores the M1 image inside a region that also carries the sample
//! data below its own form — the demu M1 boot image is the cart's Z80
//! space directly, so the banks route from offset 0, and a combined
//! M1-and-sample boot image keeps both views the same way. Reads past
//! the image wrap through the modulo form; an empty ROM reads `$FF`.
//!
//! ## The bank ports (the resolution of the two candidate forms)
//!
//! Two candidate forms exist at Z80 ports `$08-$0B`:
//!
//! 1. The official-spec form: port `$08` write enables the audio NMI,
//!    port `$18` write disables it. MAME implements exactly this on
//!    the port WRITE side: `map(0x08, 0x08).mirror(0xff00)
//!    .select(0x0010).w(audio_cpu_enable_nmi_w)` with the handler
//!    comment "out ($08) enables the nmi, out ($18) disables it" and
//!    `m_audionmi->in_w<1>(BIT(~offset, 4))`.
//! 2. The bank form: MAME implements it on the port READ side:
//!    `map(0x08, 0x0b).mirror(0x00f0).select(0xff00)
//!    .r(audio_cpu_bank_select_r)` where the handler takes
//!    `m_bank_audio_cart[offset & 3]->set_entry(offset >> 8)` — the
//!    read of port `$08` plus slot (0-3) sets that bank window to the
//!    port address's upper byte, which the Z80 supplies with the `IN
//!    A,(n)` accumulator form (`A` rides the port's bits 8-15), and
//!    the read answers 0.
//!
//! The demu Z80 core issues port IO at `0x10000 | port` on its io bus
//! and forms the `IN A,(n)` port as `A << 8 | n`, so the upper byte
//! of the bus address carries the bank number. This chip therefore
//! owns the bank-select READ form on ports `$08-$0B` (and MAME's
//! `$10`-stride mirror rows): the read at an address whose low byte
//! sits in `$08-$0B` sets the bank window `low & 3` to the address's
//! byte 1 and answers 0. Writes carry no bank form at these ports
//! (the `$08`/`$18` writes belong to the sound-side IO die's NMI
//! enable ports); the earlier bank-per-write reading that one might
//! infer from M1 driver practice is corrected by the MAME source
//! forms above, and the driver code the read form targets
//! (`LD A,n; IN A,($08+n)`) selects banks through the primary ports
//! only.
//!
//! The reset form of the bank latches is all zero, the real-chip
//! form MAME's `init_audio` records ("On the real chip, initial banks
//! are all 0"); MAME's own `$1e/$0e/$06/$02` initial-entry hack for
//! unbanked early carts stays out of this build.
//!
//! # Sources
//!
//! MAME `src/mame/snk/neogeo.cpp` (BSD-3-Clause): the main map's
//! `banked_vectors_r` and the cart-slot installs, `write_banksel` and
//! `init_cpu`, the audio map, `audio_cpu_bank_select_r`,
//! `audio_cpu_enable_nmi_w`, and the audio_io_map; MAME
//! `src/devices/machine/74259.cpp` for the addressable-latch write
//! form the system latch implements; Charles MacDonald's `Neo*Geo MVS
//! Hardware Notes` (the memory map, the watchdog, and the `System
//! control register` sections) for the bank windows and the vector
//! select; neogeodev.org `Memory mapped registers` for the register
//! names `REG_SWPBIOS`/`REG_SWPROM`, `REG_PALBANK0/1`, and the audio
//! IO port forms; and the demu Z80 core's io-bus address form for the
//! port-space routing.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The P ROM's bank size: 1 MiB banks, the MAME form.
const BANK_SIZE: u64 = 0x0010_0000;
/// The banksel lane: the last 16 bytes of the banked window
/// (MAME's `$2ffff0-$2fffff` write handler).
const BANKSEL_LOW: u32 = 0x000F_FFF0;
/// The size of the BIOS vector window.
const VECTOR_BYTES: u32 = 0x80;
/// The snapshot tag and version of the P ROM chip.
const CART_TAG: &[u8; 4] = b"NGCT";
const CART_VERSION: u8 = 1;
/// The snapshot tag and version of the M1 ROM chip.
const M1_TAG: &[u8; 4] = b"NGM1";
const M1_VERSION: u8 = 1;

/// The snapshot read helpers; they reject short chunks with the
/// established mismatch form.
fn snapshot_byte(data: &[u8], at: usize) -> Result<u8, SnapshotError> {
    data.get(at)
        .copied()
        .ok_or_else(|| SnapshotError::Mismatch("snapshot chunk ends inside a field".into()))
}

fn snapshot_u32(data: &[u8], at: usize) -> Result<u32, SnapshotError> {
    let tail = data
        .get(at..at + 4)
        .ok_or_else(|| SnapshotError::Mismatch("snapshot chunk ends inside a field".into()))?;
    Ok(u32::from_le_bytes(tail.try_into().map_err(|_| {
        SnapshotError::Mismatch("snapshot u32 read failed".into())
    })?))
}

/// The P ROM cartridge chip of the Neo Geo.
#[derive(Debug)]
pub struct NeoCart {
    name: String,
    /// The P ROM bytes, loaded in boot.
    rom: Vec<u8>,
    /// The BIOS vector bytes, read from the named BIOS device's boot
    /// state at construction.
    bios: Vec<u8>,
    /// The accepted bank index (bits 2-0 of the switch write).
    bank: u8,
    /// The bank base the second window reads from, the MAME
    /// `m_bank_base` form.
    bank_base: u32,
    /// True while the `$000000-$00007F` window answers the BIOS
    /// vectors; false answers the P ROM's. The reset form is true.
    vecbios: bool,
}

impl NeoCart {
    /// A P ROM chip from its configuration entry.
    ///
    /// Parameters: `bios` (the BIOS ROM device whose boot state
    /// supplies the vector bytes, default `bios`).
    pub fn new(device: &DeviceInstance, config: &SystemConfig) -> Result<Self, String> {
        let bios = device
            .params
            .get("bios")
            .and_then(|v| v.as_str())
            .unwrap_or("bios")
            .to_string();
        Ok(Self {
            name: device.name.to_string(),
            rom: Vec::new(),
            bios: Self::load_boot_image(config, &bios),
            bank: 0,
            bank_base: 0,
            vecbios: true,
        })
    }

    /// Fetch the boot image of the named BIOS device; the device's
    /// own boot pass keeps its form, the die mirrors the bytes.
    fn load_boot_image(config: &SystemConfig, device: &str) -> Vec<u8> {
        let slot = config
            .devices_by_name
            .iter()
            .find_map(|(name, slot)| (name.as_str() == device).then_some(*slot));
        let Some(slot) = slot else {
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

    /// Accept a banksel write per the MAME form: bits 2-0 the bank,
    /// the no-banks guard for 1 MiB and smaller images, the
    /// empty-bank guard, and the `(bank + 1) * 1 MiB` base.
    fn accept_bank(&mut self, value: u32) {
        let index = value & 0x07;
        let len = self.rom.len() as u64;
        if len <= BANK_SIZE && index != 0 {
            // MAME logs and keeps the previous base: no banks exist.
            return;
        }
        let bank = if (u64::from(index) + 1) * BANK_SIZE >= len {
            0
        } else {
            index
        };
        self.bank = bank as u8;
        self.bank_base = ((u64::from(bank) + 1) * BANK_SIZE) as u32;
    }

    /// One ROM byte at a storage index, modulo the image.
    fn rom_byte(&self, index: u64) -> u32 {
        if self.rom.is_empty() {
            return 0xFF;
        }
        let len = self.rom.len() as u64;
        u32::from(self.rom[(index % len) as usize])
    }

    /// The byte of the flat `$000000-$0FFFFF` window: the vector mux
    /// over the first `$80`, then the flat form.
    fn flat_byte(&self, address: u32) -> u32 {
        if address < VECTOR_BYTES {
            let image = if self.vecbios { &self.bios } else { &self.rom };
            return match image.get(address as usize) {
                Some(b) => u32::from(*b),
                None => 0xFF,
            };
        }
        self.rom_byte(u64::from(address))
    }

    /// The byte of the banked `$200000-$2FFFFF` window.
    fn banked_byte(&self, address: u32) -> u32 {
        let offset = u64::from(address - 0x0020_0000);
        self.rom_byte(u64::from(self.bank_base) + offset)
    }

    /// The snapshot tail of the cartridge record: bank and vector
    /// latches only, the SRAM-free form.
    fn snapshot_tail(&self, out: &mut Snapshot) {
        out.push(CART_VERSION);
        out.push(self.bank);
        out.extend_from_slice(&self.bank_base.to_le_bytes());
        out.push(u8::from(self.vecbios));
    }
}

impl BusAccess for NeoCart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0;
        if a < 0x0010_0000 {
            self.flat_byte(a)
        } else if (a & 0x00F0_0000) == 0x0020_0000 {
            self.banked_byte(a)
        } else {
            0xFF
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let a = address.0;
        // The banksel lane: the low data lane of the `$2FFFF0-$2FFFFF`
        // words; the demu 68k's word write issues the lane bytes, so
        // the odd-lane byte carries the bank bits 2-0.
        if (a & 0x00F0_0000) == 0x0020_0000 && (a & 0x000F_FFF0) == BANKSEL_LOW && (a & 1) == 1 {
            self.accept_bank(value);
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0;
        if a < 0x0010_0000 {
            self.flat_byte(a) as u8
        } else if (a & 0x00F0_0000) == 0x0020_0000 {
            self.banked_byte(a) as u8
        } else {
            0xFF
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for NeoCart {
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
        self.bank = 0;
        self.bank_base = 0;
        self.vecbios = true;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        self.rom = match state {
            Some(BootState::Rom { path }) => std::fs::read(path).unwrap_or_default(),
            Some(BootState::Hex { bytes }) => bytes.clone(),
            _ => Vec::new(),
        };
        // The power-up bank: the second MiB for larger images, base 0
        // (the mirrored first MiB) otherwise, per MAME `init_cpu`.
        if self.rom.len() as u64 > BANK_SIZE {
            self.bank_base = BANK_SIZE as u32;
        } else {
            self.bank_base = 0;
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(11);
        out.extend_from_slice(CART_TAG);
        self.snapshot_tail(&mut out);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 11 || &data[..4] != CART_TAG || snapshot_byte(data, 4)? != CART_VERSION {
            return Err(SnapshotError::Mismatch(format!(
                "the neo cart chunk must be 11 bytes of tag {CART_TAG:?}"
            )));
        }
        self.bank = snapshot_byte(data, 5)?;
        self.bank_base = snapshot_u32(data, 6)?;
        self.vecbios = snapshot_byte(data, 10)? != 0;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        if line == "vecbios" {
            // High selects the BIOS vectors, Low the P ROM's: the
            // system latch's bit 1, reset to 0 by the 259's clear.
            self.vecbios = level.is_high();
        }
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

/// The Z80-bus M1 ROM chip of the Neo Geo.
#[derive(Debug)]
pub struct NeoM1Rom {
    name: String,
    /// The M1 ROM bytes, loaded in boot.
    rom: Vec<u8>,
    /// The bank latches of windows 3, 2, 1, 0 (the `$8000` 16 KiB,
    /// `$C000` 8 KiB, `$E000` 4 KiB, `$F000` 2 KiB forms).
    banks: [u8; 4],
}

impl NeoM1Rom {
    /// An M1 ROM chip from its configuration entry.
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        Ok(Self {
            name: device.name.to_string(),
            rom: Vec::new(),
            banks: [0, 0, 0, 0],
        })
    }

    /// One ROM byte at a storage index, modulo the image.
    fn rom_byte(&self, index: u64) -> u32 {
        if self.rom.is_empty() {
            return 0xFF;
        }
        let len = self.rom.len() as u64;
        u32::from(self.rom[(index % len) as usize])
    }

    /// The banked window byte for one latch index and shift.
    fn bank_window(&self, slot: usize, offset: u32, shift: u32) -> u32 {
        let index = (u64::from(self.banks[slot]) << shift) | u64::from(offset);
        self.rom_byte(index)
    }

    /// The bank-port read: the MAME `audio_cpu_bank_select_r` form —
    /// the bank window `low & 3` takes the address's byte 1 (the
    /// `IN A,(n)` accumulator carrier) and the read answers 0. The
    /// port set is the `$08-$0B` group plus the `$10`-stride mirror
    /// rows MAME's `mirror(0x00f0)` covers; other ports answer the
    /// open io form `$FF`.
    fn bank_port_read(&mut self, address: u32) -> u32 {
        if (address & 0x0C) == 0x08 {
            let slot = (address & 3) as usize;
            self.banks[slot] = ((address >> 8) & 0xFF) as u8;
            return 0;
        }
        0xFF
    }
}

impl BusAccess for NeoM1Rom {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0;
        if a >= 0x0001_0000 {
            // The sound-bus io window: the bank ports.
            return self.bank_port_read(a);
        }
        match a {
            0x0000..=0x7FFF => self.rom_byte(u64::from(a)),
            0x8000..=0xBFFF => self.bank_window(3, a - 0x8000, 14),
            0xC000..=0xDFFF => self.bank_window(2, a - 0xC000, 13),
            0xE000..=0xEFFF => self.bank_window(1, a - 0xE000, 12),
            0xF000..=0xF7FF => self.bank_window(0, a - 0xF000, 11),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, _value: u32, _tick: Tick) {
        // The chip carries no write form: the bank select is the read
        // form (MAME `audio_cpu_bank_select_r`), the NMI flag is the
        // write form of the sound-side IO die, and the ROM areas
        // ignore writes.
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0;
        if a >= 0x0001_0000 {
            // The bank ports' read form answers 0 with no side effect.
            return if (a & 0x0C) == 0x08 { 0 } else { 0xFF };
        }
        match a {
            0x0000..=0x7FFF => self.rom_byte(u64::from(a)) as u8,
            0x8000..=0xBFFF => self.bank_window(3, a - 0x8000, 14) as u8,
            0xC000..=0xDFFF => self.bank_window(2, a - 0xC000, 13) as u8,
            0xE000..=0xEFFF => self.bank_window(1, a - 0xE000, 12) as u8,
            0xF000..=0xF7FF => self.bank_window(0, a - 0xF000, 11) as u8,
            _ => 0xFF,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for NeoM1Rom {
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
        self.banks = [0; 4];
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        self.rom = match state {
            Some(BootState::Rom { path }) => std::fs::read(path).unwrap_or_default(),
            Some(BootState::Hex { bytes }) => bytes.clone(),
            _ => Vec::new(),
        };
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(9);
        out.extend_from_slice(M1_TAG);
        out.push(M1_VERSION);
        out.extend_from_slice(&self.banks);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 9 || &data[..4] != M1_TAG || snapshot_byte(data, 4)? != M1_VERSION {
            return Err(SnapshotError::Mismatch(format!(
                "the neo m1 rom chunk must be 9 bytes of tag {M1_TAG:?}"
            )));
        }
        for (slot, bank) in self.banks.iter_mut().enumerate() {
            *bank = snapshot_byte(data, 5 + slot)?;
        }
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The P ROM instance with the BIOS boot state of the `bios`
    /// device, and the test BIOS bytes.
    fn cart_with(rom: Vec<u8>, bios: Vec<u8>) -> NeoCart {
        let mut config = mini_config();
        config.boot.insert(
            demu_config::newtypes::DeviceName::new("bios").expect("name"),
            BootState::Hex { bytes: bios },
        );
        let slot = config
            .devices_by_name
            .get(&demu_config::newtypes::DeviceName::new("cart").expect("name"))
            .copied()
            .expect("the cart slot exists");
        let mut cart = NeoCart::new(&config.devices[slot], &config).expect("cart");
        cart.boot(Some(&BootState::Hex { bytes: rom }));
        cart
    }

    /// The mini configuration with the cart, the M1 ROM, and the BIOS
    /// device, for the constructor forms.
    fn mini_config() -> SystemConfig {
        let mut clocks = std::collections::BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            demu_config::model::ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(12_000_000, 1).expect("freq"),
                origin: "test".to_string(),
            },
        );
        let cart = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("cart").expect("name"),
            kind: demu_config::newtypes::KindName::new("neogeo.cart").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let m1 = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("m1rom").expect("name"),
            kind: demu_config::newtypes::KindName::new("neogeo.m1rom").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let bios = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("bios").expect("name"),
            kind: demu_config::newtypes::KindName::new("neogeo.lspc").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let devices = vec![cart, m1, bios];
        let mut devices_by_name = std::collections::BTreeMap::new();
        for (i, device) in devices.iter().enumerate() {
            devices_by_name.insert(device.name.clone(), i);
        }
        SystemConfig {
            name: "neogeo-cart-test".to_string(),
            triple: "test".to_string(),
            description: "The cart mini test machine.".to_string(),
            clocks,
            devices,
            devices_by_name,
            buses: vec![demu_config::model::Bus {
                name: "cpu".to_string(),
                address_bits: 24,
                owner: None,
                parent: None,
            }],
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: std::collections::BTreeMap::new(),
            quirks: std::collections::BTreeMap::new(),
        }
    }

    /// The read at an address, tick free.
    fn rd(v: &mut NeoCart, a: u32) -> u32 {
        v.read(AccessWidth::Byte, Address(a), Tick(0))
    }

    #[test]
    fn flat_window_reads_the_prom_modulo() {
        let mut rom = vec![0u8; 0x100];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        let mut v = cart_with(rom, vec![0xE0; 0x80]);
        assert_eq!(rd(&mut v, 0x80), 0x80);
        assert_eq!(rd(&mut v, 0x100), 0x00, "the modulo wraps the image");
        assert_eq!(rd(&mut v, 0x000F_FFFF), 0xFF);
    }

    #[test]
    fn vector_mux_flips_between_bios_and_prom() {
        let mut bios = vec![0x11u8; 0x80];
        bios[0] = 0xE0;
        bios[1] = 0xF9;
        let mut rom = vec![0u8; 0x200];
        rom[0] = 0x4E;
        rom[1] = 0x71;
        let mut v = cart_with(rom, bios);
        // The reset form: the BIOS vectors.
        assert_eq!(rd(&mut v, 0x0), 0xE0);
        assert_eq!(rd(&mut v, 0x1), 0xF9);
        // The cart form through the system latch's set write.
        v.notify_line("vecbios", Level::Low, Tick(0));
        assert_eq!(rd(&mut v, 0x0), 0x4E);
        assert_eq!(rd(&mut v, 0x1), 0x71);
        // The window boundary: the $80 byte stays the flat P ROM form.
        assert_eq!(rd(&mut v, 0x80), 0x00);
        // Back to the BIOS form.
        v.notify_line("vecbios", Level::High, Tick(1));
        assert_eq!(rd(&mut v, 0x0), 0xE0);
    }

    #[test]
    fn banked_window_switches_through_the_mame_form() {
        let mut rom = vec![0u8; 3 * 0x10_0000];
        rom[0x10_0000] = 0xA1; // the second MiB: bank 0
        rom[0x20_0000] = 0xB2; // the third MiB: bank 1
        rom[0x1F_FFFF] = 0xC3; // the second MiB's tail byte
        let mut v = cart_with(rom, Vec::new());
        // The power-up form: the second MiB.
        assert_eq!(rd(&mut v, 0x20_0000), 0xA1);
        // The banksel write: the odd-lane byte at the last word pair.
        v.write(AccessWidth::Byte, Address(0x2F_FFF1), 1, Tick(0));
        assert_eq!(rd(&mut v, 0x20_0000), 0xB2);
        // The empty-bank guard: bank 2 of a 3 MiB image falls to 0
        // (MAME's `bankswitch to empty bank` guard).
        v.write(AccessWidth::Byte, Address(0x2F_FFF1), 0x06, Tick(0));
        assert_eq!(rd(&mut v, 0x20_0000), 0xA1);
        // The modulo route: the window's tail reads the image tail.
        assert_eq!(rd(&mut v, 0x2F_FFFF), 0xC3);
    }

    #[test]
    fn small_images_keep_the_flat_bank_form() {
        let mut rom = vec![0u8; 0x1_0000];
        rom[0] = 0x77;
        let mut v = cart_with(rom, Vec::new());
        assert_eq!(rd(&mut v, 0x20_0000), 0x77, "the first MiB mirrors");
        // A nonzero bank on a small image: the latch keeps (MAME's
        // no-banks guard).
        v.write(AccessWidth::Byte, Address(0x2F_FFF1), 3, Tick(0));
        assert_eq!(rd(&mut v, 0x20_0000), 0x77);
        // The zero-bank write: the latch accepts per the MAME form and
        // the modulo route mirrors the image.
        v.write(AccessWidth::Byte, Address(0x2F_FFF1), 0, Tick(0));
        assert_eq!(rd(&mut v, 0x20_0000), 0x77);
    }

    #[test]
    fn empty_prom_reads_open_byte_forms() {
        let mut v = cart_with(Vec::new(), Vec::new());
        assert_eq!(rd(&mut v, 0x0000), 0xFF);
        assert_eq!(rd(&mut v, 0x0080), 0xFF);
        assert_eq!(rd(&mut v, 0x20_0000), 0xFF);
        assert_eq!(v.peek(Address(0x4000)), 0xFF);
    }

    #[test]
    fn banksel_even_lane_and_foreign_writes_carry_no_bank() {
        let mut rom = vec![0u8; 3 * 0x10_0000];
        rom[0x10_0000] = 0xA1;
        rom[0x20_0000] = 0xB2;
        let mut v = cart_with(rom, Vec::new());
        // The even lane carries no bank bits: no switch.
        v.write(AccessWidth::Byte, Address(0x2F_FFF0), 3, Tick(0));
        assert_eq!(rd(&mut v, 0x20_0000), 0xA1);
        // Writes outside the banksel lane carry none either.
        v.write(AccessWidth::Byte, Address(0x20_0004), 2, Tick(0));
        assert_eq!(rd(&mut v, 0x20_0000), 0xA1);
    }

    #[test]
    fn cart_snapshot_round_trips() {
        let mut rom = vec![0u8; 3 * 0x10_0000];
        rom[0x20_0000] = 0x5A; // the third MiB: bank 1
        let mut v = cart_with(rom, vec![0xE0; 0x80]);
        v.write(AccessWidth::Byte, Address(0x2F_FFF1), 0x01, Tick(0));
        v.notify_line("vecbios", Level::Low, Tick(0));
        let snap1 = v.snapshot();
        assert_eq!(&snap1[..4], b"NGCT");
        // The machine form: boot first (the power-on form), then the
        // snapshot restore. The boot images do not ride the chunk.
        let mut v2 = cart_with(Vec::new(), Vec::new());
        let mut twin_rom = vec![0u8; 3 * 0x10_0000];
        twin_rom[0x20_0000] = 0x5A;
        v2.boot(Some(&BootState::Hex { bytes: twin_rom }));
        assert_eq!(
            v2.read(AccessWidth::Byte, Address(0x20_0000), Tick(0)),
            0x00,
            "the boot's power-up form reads the second MiB"
        );
        v2.restore(&snap1).expect("restore");
        assert_eq!(v2.snapshot(), snap1, "the round trip is byte-identical");
        // The restored bank latch routes the third MiB again.
        assert_eq!(
            v2.read(AccessWidth::Byte, Address(0x20_0000), Tick(0)),
            0x5A
        );
        // The restored vector latch: the P ROM form through the mux.
        v2.rom[0] = 0x7E;
        assert_eq!(v2.peek(Address(0x0)), 0x7E);
        assert!(v2.restore(&[0, 1, 2, 3]).is_err());
    }

    #[test]
    fn cart_restore_rejects_foreign_chunks() {
        let mut v = cart_with(Vec::new(), Vec::new());
        let mut bad = v.snapshot();
        bad[0] = b'X';
        assert!(v.restore(&bad).is_err());
        let mut short = v.snapshot();
        short.truncate(6);
        assert!(v.restore(&short).is_err());
    }

    #[test]
    fn m1rom_static_window_reads_the_image() {
        let mut v = NeoM1Rom::new(&mini_config().devices[1]).expect("m1");
        let mut rom = vec![0u8; 0x8000];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        v.boot(Some(&BootState::Hex { bytes: rom }));
        assert_eq!(v.read(AccessWidth::Byte, Address(0x0000), Tick(0)), 0);
        assert_eq!(v.read(AccessWidth::Byte, Address(0x1234), Tick(0)), 0x34);
        assert_eq!(v.read(AccessWidth::Byte, Address(0x7FFF), Tick(0)), 0xFF);
    }

    #[test]
    fn m1rom_bank_windows_route_through_the_bank_ports() {
        let mut v = NeoM1Rom::new(&mini_config().devices[1]).expect("m1");
        let mut rom = vec![0u8; 0x40000];
        // Markers at the bank-aligned fetch points.
        rom[0x0000] = 0x11; // every window before any bank select
        rom[0x0800] = 0x12; // window 0 ($F000, 2 KiB), bank 1
        rom[0x1000] = 0x13; // window 1 ($E000, 4 KiB), bank 1
        rom[0x2000] = 0x14; // window 2 ($C000, 8 KiB), bank 1
        rom[0x4000] = 0x15; // window 3 ($8000, 16 KiB), bank 1
        rom[0x8000] = 0x16; // window 3, bank 2
        rom[0x6000] = 0x17; // window 2, bank 3
        rom[0x22000] = 0x19; // window 2, bank $11 (the mirror-row form)
        rom[0x30000] = 0x1A; // window 3, bank $018 wraps into range
        v.boot(Some(&BootState::Hex { bytes: rom }));
        // The reset form: all banks 0, every window reads offset 0.
        assert_eq!(v.read(AccessWidth::Byte, Address(0x8000), Tick(0)), 0x11);
        assert_eq!(v.read(AccessWidth::Byte, Address(0xC000), Tick(0)), 0x11);
        assert_eq!(v.read(AccessWidth::Byte, Address(0xE000), Tick(0)), 0x11);
        assert_eq!(v.read(AccessWidth::Byte, Address(0xF000), Tick(0)), 0x11);
        // The bank-port reads: the address's byte 1 = the bank, the
        // low byte = the port; the read answers 0 per MAME. Port $0B
        // = window 3, $0A = window 2, $09 = window 1, $08 = window 0;
        // the bank rides the port's byte 1.
        let port = |bank: u32, slot: u32| Address(0x0001_0000 | bank << 8 | 0x08 | slot);
        assert_eq!(v.read(AccessWidth::Byte, port(1, 3), Tick(0)), 0);
        assert_eq!(v.read(AccessWidth::Byte, Address(0x8000), Tick(0)), 0x15);
        assert_eq!(v.read(AccessWidth::Byte, port(3, 2), Tick(0)), 0);
        assert_eq!(v.read(AccessWidth::Byte, Address(0xC000), Tick(0)), 0x17);
        assert_eq!(v.read(AccessWidth::Byte, port(1, 1), Tick(0)), 0);
        assert_eq!(v.read(AccessWidth::Byte, Address(0xE000), Tick(0)), 0x13);
        assert_eq!(v.read(AccessWidth::Byte, port(1, 0), Tick(0)), 0);
        assert_eq!(v.read(AccessWidth::Byte, Address(0xF000), Tick(0)), 0x12);
        assert_eq!(v.read(AccessWidth::Byte, port(2, 3), Tick(0)), 0);
        assert_eq!(v.read(AccessWidth::Byte, Address(0x8000), Tick(0)), 0x16);
        // The mirror row: the MAME `mirror(0x00f0)` port group at
        // $18-$1B routes the same handler (port $0A with bank $11 in
        // the address's byte 1).
        assert_eq!(v.read(AccessWidth::Byte, Address(0x0001_110A), Tick(0)), 0);
        // Bank $11 of window 2: $11 << 13 = $22000, inside the image.
        let wrapped = 0x22_000usize;
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0xC000), Tick(0)),
            u32::from(v.rom[wrapped])
        );
        // The final select: bank $1B of window 3 wraps into range.
        assert_eq!(v.read(AccessWidth::Byte, port(0x1B, 3), Tick(0)), 0);
        let tail = (0x1Bu64 * 0x4000) % 0x40000;
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x8000), Tick(0)),
            u32::from(v.rom[tail as usize])
        );
    }

    #[test]
    fn m1rom_bank_ports_answer_the_port_form_only() {
        let mut v = NeoM1Rom::new(&mini_config().devices[1]).expect("m1");
        v.boot(Some(&BootState::Hex {
            bytes: vec![0u8; 0x1_0000],
        }));
        // Reads outside the bank ports answer the open byte form.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0001_0000), Tick(0)),
            0xFF
        );
        // Reads at the bank ports with a stray lane carry no bank.
        assert_eq!(
            v.read(AccessWidth::Byte, Address(0x0001_000C), Tick(0)),
            0xFF
        );
        // Writes carry no form: nothing changes.
        v.write(AccessWidth::Byte, Address(0x0001_0008), 0x7F, Tick(0));
        assert_eq!(v.read(AccessWidth::Byte, Address(0x8000), Tick(0)), 0);
    }

    #[test]
    fn m1rom_empty_and_short_images_read_open() {
        let mut v = NeoM1Rom::new(&mini_config().devices[1]).expect("m1");
        assert_eq!(v.read(AccessWidth::Byte, Address(0x1000), Tick(0)), 0xFF);
        assert_eq!(v.read(AccessWidth::Byte, Address(0x8010), Tick(0)), 0xFF);
        // A short image wraps through its modulo.
        let mut s = NeoM1Rom::new(&mini_config().devices[1]).expect("m1");
        s.boot(Some(&BootState::Hex {
            bytes: vec![0x10, 0x20],
        }));
        assert_eq!(s.read(AccessWidth::Byte, Address(0x0003), Tick(0)), 0x20);
        assert_eq!(s.read(AccessWidth::Byte, Address(0x8000), Tick(0)), 0x10);
    }

    #[test]
    fn m1rom_snapshot_round_trips() {
        let mut v = NeoM1Rom::new(&mini_config().devices[1]).expect("m1");
        let mut rom = vec![0u8; 0x40000];
        rom[0x22000] = 0x44; // window 2, bank $11
        rom[0x0C000] = 0x55; // window 3, bank 3
        v.boot(Some(&BootState::Hex { bytes: rom.clone() }));
        v.read(
            AccessWidth::Byte,
            Address(0x0001_0000 | 0x11 << 8 | 0x0A),
            Tick(0),
        );
        v.read(
            AccessWidth::Byte,
            Address(0x0001_0000 | 0x03 << 8 | 0x0B),
            Tick(0),
        );
        let snap = v.snapshot();
        assert_eq!(&snap[..4], b"NGM1");
        // The machine form: boot first, then the snapshot restore.
        let mut w = NeoM1Rom::new(&mini_config().devices[1]).expect("m1");
        w.boot(Some(&BootState::Hex { bytes: rom }));
        w.restore(&snap).expect("restore");
        assert_eq!(w.snapshot(), snap, "the round trip is byte-identical");
        // The restored banks route their windows.
        assert_eq!(w.read(AccessWidth::Byte, Address(0xC000), Tick(0)), 0x44);
        assert_eq!(w.read(AccessWidth::Byte, Address(0x8000), Tick(0)), 0x55);
        assert!(w.restore(&[0, 1, 2, 3]).is_err());
    }
}
