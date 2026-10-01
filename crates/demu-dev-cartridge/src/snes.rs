//! The SNES cartridge: the LoROM and HiROM image loaders with the
//! optional SRAM.
//!
//! The device answers the address regions that the config routes to
//! it. The ROM decode by the absolute address:
//!
//! - LoROM: the ROM offset = `(bank & 0x3F) * 0x8000 + (off &
//!   0x7FFF)`, taken modulo the image length so smaller images repeat
//!   the way the real bus repeats banks.
//! - HiROM: the ROM offset = `(bank & 0x3F) * 0x10000 + (off &
//!   0xFFFF)`.
//!
//! The bank and offset both come from the 24-bit address: the bank =
//! bits 16-23, the offset = bits 0-15. The header and the vector
//! table therefore answer at `$00FFC0` (HiROM, the file's words
//! `0xFFC0`) and `$00FFC0`/`$00FFFC` in LoROM at the file's
//! `0x7FC0`/`$0x7FFC`.
//!
//! SRAM sits behind its own region the config routes (the classic
//! LoROM = banks 30-3F and B0-BF at `$6000-$7FFF`); the decode is the
//! region's low-16 window. Writes land in the internal SRAM; reads
//! answer it.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The word address the LoROM form shifts one bank.
const LOROM_BANK: u32 = 0x8000;

/// The cart: the image bytes, the form, and the optional SRAM.
#[derive(Debug)]
pub struct SnesCart {
    name: String,
    /// The image form: `lorom` or `hirom`.
    form: Form,
    rom: Vec<u8>,
    sram: Vec<u8>,
    /// The SRAM base offset inside the mapped SRAM image: the
    /// classic SRAM banks start with bank 0x30 and 0xB0, so the
    /// second mirror set starts 0x10 banks later.
    sram_base_bank: u8,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

/// The image form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The 32-KiB-bank form.
    LoRom,
    /// The direct 64-KiB-bank form.
    HiRom,
}

impl SnesCart {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let form = match device
            .params
            .get("form")
            .and_then(|v| v.as_str())
            .unwrap_or("lorom")
        {
            "hirom" => Form::HiRom,
            _ => Form::LoRom,
        };
        let sram_size = device
            .params
            .get("sram")
            .and_then(|v| v.as_integer())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0);
        let sram_base_bank = device
            .params
            .get("sram_base_bank")
            .and_then(|v| v.as_integer())
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(0x30);
        Self {
            name: device.name.to_string(),
            form,
            rom: Vec::new(),
            sram: vec![0; sram_size],
            sram_base_bank,
            next: None,
            last_tick: None,
        }
    }

    /// The ROM offset for one address, or None when the form's
    /// window does not cover it (the low half of a LoROM bank).
    fn rom_offset(&self, a: u32) -> Option<usize> {
        let bank = a >> 16;
        let off = a & 0xFFFF;
        match self.form {
            Form::LoRom => {
                if off < LOROM_BANK {
                    return None;
                }
                let n = self
                    .rom
                    .len()
                    .checked_next_multiple_of(LOROM_BANK as usize)
                    .unwrap_or(1)
                    .max(1);
                let raw = (usize::try_from(bank & 0x3F).unwrap_or(0) * 0x8000
                    + usize::try_from(off - LOROM_BANK).unwrap_or(0))
                    % n;
                Some(raw)
            }
            Form::HiRom => {
                let n = self.rom.len().max(1);
                Some(
                    (usize::try_from(bank & 0x3F).unwrap_or(0) * 0x10000
                        + usize::try_from(off).unwrap_or(0))
                        % n,
                )
            }
        }
    }

    /// True when the address answers from the SRAM region: the low
    /// window's upper half, in the banks set for the SRAM form.
    fn sram_slot(&self, a: u32) -> Option<usize> {
        if self.sram.is_empty() {
            return None;
        }
        let off16 = a & 0xFFFF;
        if !(0x6000..=0x7FFF).contains(&off16) {
            return None;
        }
        let bank = a >> 16;
        // The standard LoROM form: banks 0x30-0x3F bank set and the
        // mirror at 0x50-0x5F above it.
        let idx =
            usize::try_from((bank.wrapping_sub(u32::from(self.sram_base_bank))) & 0x0F).ok()?;
        let base = off16 - 0x6000;
        let span = 0x2000;
        let n = self.sram.len();
        Some((idx * span + usize::try_from(base).unwrap_or(0)) % n)
    }
}

impl BusAccess for SnesCart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0;
        if let Some(slot) = self.sram_slot(a) {
            return u32::from(self.sram[slot]);
        }
        if let Some(off) = self.rom_offset(a) {
            return self.rom.get(off).copied().unwrap_or(0).into();
        }
        0
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let a = address.0;
        if let Some(slot) = self.sram_slot(a) {
            self.sram[slot] = value as u8;
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0;
        if let Some(slot) = self.sram_slot(a) {
            return self.sram[slot];
        }
        self.rom_offset(a)
            .and_then(|off| self.rom.get(off).copied())
            .unwrap_or(0)
    }
}

impl Device for SnesCart {
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
        // The image contents are the boot state; the reset only
        // clears the SRAM.
        self.sram.iter_mut().for_each(|b| *b = 0);
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Rom { path }) => {
                self.rom = std::fs::read(path).unwrap_or_default();
            }
            Some(BootState::Hex { bytes }) => self.rom = bytes.clone(),
            _ => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(9 + self.sram.len());
        out.extend_from_slice(b"SCRT");
        out.push(1);
        out.push(match self.form {
            Form::LoRom => 0,
            Form::HiRom => 1,
        });
        out.push(self.sram_base_bank);
        out.extend_from_slice(&(self.sram.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.sram);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 11 || &data[..4] != b"SCRT" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad snes cart chunk".into()));
        }
        self.form = match data[5] {
            0 => Form::LoRom,
            1 => Form::HiRom,
            _ => return Err(SnapshotError::Mismatch("bad form byte".into())),
        };
        self.sram_base_bank = data[6];
        let sram_len = u32::from_le_bytes(data[7..11].try_into().expect("4 bytes")) as usize;
        let pos = 11usize;
        if pos + sram_len != data.len() {
            return Err(SnapshotError::Mismatch("bad snes cart chunk".into()));
        }
        self.sram = data[pos..pos + sram_len].to_vec();
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::newtypes::{DeviceName, KindName};

    fn device(form: &str, sram: i64) -> demu_config::model::DeviceInstance {
        let mut params = std::collections::BTreeMap::new();
        params.insert("form".to_string(), toml::Value::String(form.to_string()));
        params.insert("sram".to_string(), toml::Value::Integer(sram));
        demu_config::model::DeviceInstance {
            name: DeviceName::new("cart").expect("name"),
            kind: KindName::new("snes.cart").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: [("cpu".to_string(), 6)].into_iter().collect(),
        }
    }

    fn lorom() -> SnesCart {
        SnesCart::new(&device("lorom", 0))
    }

    fn hirom() -> SnesCart {
        SnesCart::new(&device("hirom", 0))
    }

    #[test]
    fn lorom_bank_mapping_walks_the_image() {
        let mut c = lorom();
        let mut rom = vec![0u8; 0x18000]; // three 32K banks
        rom[0x0000] = 0x11;
        rom[0x7FFC] = 0x22;
        rom[0x7FFD] = 0x33;
        rom[0x10000 + 0x123] = 0x44;
        c.rom = rom;
        // $00:$8000 = the file's 0x0000.
        assert_eq!(c.peek(Address(0x008000)), 0x11);
        // $00:$FFFC = the file's 0x7FFC.
        assert_eq!(c.peek(Address(0x00FFFC)), 0x22);
        assert_eq!(c.peek(Address(0x00FFFD)), 0x33);
        // $20:$8123: LoROM decodes bank & 0x3F, so the raw offset is
        // the file's 0x100123. The image holds three 32-KiB banks, so
        // the offset wraps modulo the image length to the file's
        // 0x10123.
        assert_eq!(
            c.peek(Address(0x208123)),
            0x44,
            "bank 0x20 wraps modulo the image to its block 2"
        );
    }

    #[test]
    fn hirom_bank_mapping_maps_full_banks() {
        let mut c = hirom();
        let mut rom = vec![0u8; 0x20000];
        rom[0xFFC0] = 0x77;
        rom[0xFFFC] = 0x81;
        rom[0x1FFFD] = 0x55;
        c.rom = rom;
        // $00:$FFC0 = the file's 0xFFC0 (the header).
        assert_eq!(c.peek(Address(0x00FFC0)), 0x77);
        assert_eq!(c.peek(Address(0x00FFFC)), 0x81, "the reset vector");
        // $01:$FFFD = the file's 0x1FFFD.
        assert_eq!(c.peek(Address(0x01FFFD)), 0x55);
    }

    #[test]
    fn sram_answers_in_its_region() {
        let mut c = SnesCart::new(&device("lorom", 0x2000));
        c.write(AccessWidth::Byte, Address(0x306000), 0x42, Tick(0));
        assert_eq!(c.peek(Address(0x306000)), 0x42);
        c.write(AccessWidth::Byte, Address(0xB07FFF), 0x99, Tick(0));
        assert_eq!(c.peek(Address(0xB07FFF)), 0x99, "the mirrored bank upper");
        // The region below stays ROM (0 when unset).
        assert_eq!(c.peek(Address(0x305FFF)), 0);
    }

    #[test]
    fn boot_state_loads_the_hex_image() {
        let mut c = lorom();
        c.boot(Some(&BootState::Hex {
            bytes: vec![0xE8, 0x00, 0x7C],
        }));
        assert_eq!(c.peek(Address(0x008000)), 0xE8);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut c = SnesCart::new(&device("lorom", 0x2000));
        c.boot(Some(&BootState::Hex {
            bytes: vec![0xFF; 16],
        }));
        c.write(AccessWidth::Byte, Address(0x306000), 0x42, Tick(0));
        let snap = c.snapshot();
        let mut twin = lorom();
        twin.boot(Some(&BootState::Hex {
            bytes: vec![0xFF; 16],
        }));
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
    }
}
