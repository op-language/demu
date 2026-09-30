//! The NES cartridge device: iNES loading, PRG banking, CHR banks,
//! and the UNROM, MMC1, and MMC5 mappers.
//!
//! The boot state carries the iNES file. The loader parses the
//! 16-byte header, loads the program and character banks, and
//! allocates the writable character RAM for a header without
//! character data. The cartridge answers the CPU bus at the program
//! ranges and the PPU bus at the pattern range, so the PPU fetches
//! walk the mapper's banks. An eight-kilobyte program RAM answers
//! $6000-$7FFF on every mapper.
//!
//! The MMC5 support carries the program page registers of the common
//! mode, the character pages, and the scanline IRQ counter; the
//! extended RAM bank and the split-cartridge modes are out of this
//! scope.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The mapper family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NesMapper {
    /// A plain cartridge with fixed banks.
    Nrom,
    /// UNROM: one switchable 16-KiB bank at $8000 and the last bank
    /// fixed at $C000.
    Uxrom,
    /// MMC1 with the five-write shift register.
    Mmc1,
    /// MMC5 with the bank registers and the scanline IRQ.
    Mmc5,
}

/// The parsed iNES header fields demu uses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NesHeader {
    /// The program banks of 16 KiB.
    pub prg_banks: u8,
    /// The character banks of 8 KiB.
    pub chr_banks: u8,
    /// The mapper number.
    pub mapper: u8,
    /// The mirror bit of flags 6: 0 horizontal, 1 vertical.
    pub mirroring: u8,
    /// The battery flag of flags 6.
    pub battery: bool,
}

/// The cartridge.
#[derive(Debug)]
pub struct NesCart {
    name: String,
    /// True on the PAL region cart kind; the frame-sync count of the
    /// scanline IRQ depends on the region's vblank length.
    pal: bool,
    /// The program ROM image.
    prg: Vec<u8>,
    /// The character image or the writable character RAM.
    chr: Vec<u8>,
    /// The character RAM accepts writes.
    chr_writable: bool,
    /// The program RAM at $6000-$7FFF.
    prg_ram: Vec<u8>,
    /// The mapper family.
    mapper: NesMapper,
    /// The parsed header.
    header: NesHeader,
    // Mapper state.
    /// The UNROM bank register at $8000.
    uc_bank: u8,
    /// The MMC1 shift register state.
    mmc1_shift: u32,
    /// The remaining bits the MMC1 shift register wants.
    mmc1_count: u8,
    /// The MMC1 control register with the PRG and CHR modes.
    mmc1_ctrl: u8,
    /// The MMC1 character page at $0000.
    mmc1_chr0: u8,
    /// The MMC1 character page at $1000.
    mmc1_chr1: u8,
    /// The MMC1 program page at $E000.
    mmc1_prg: u8,
    /// The MMC5 program banks at $5114 to $5117.
    mmc5_prg: [u8; 4],
    /// The MMC5 character banks at $5120 to $512B.
    mmc5_chr: [u8; 8],
    /// The MMC5 character mode: 0 an 8-KiB window, 1 two 4-KiB, 2
    /// eight 1-KiB.
    mmc5_chr_mode: u8,
    /// The MMC5 IRQ scanline target.
    mmc5_irq_line: u8,
    /// The MMC5 scanline counter, reloaded on the vblank start.
    mmc5_irq_counter: u8,
    /// The MMC5 IRQ enable.
    mmc5_irq_on: bool,
    /// The MMC5 IRQ flag from the scanline counter.
    mmc5_irq_pending: bool,
    /// True once the boot ran and the image loaded.
    loaded: bool,
    /// A pending IRQ level raise for the next visit.
    irq_raise_pending: bool,
    /// A pending IRQ level release for the next visit.
    irq_lower_pending: bool,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl NesCart {
    /// A cartridge from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            pal: device.kind.as_str().starts_with("rp2A07")
                || device
                    .params
                    .get("pal")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            prg: Vec::new(),
            chr: Vec::new(),
            chr_writable: false,
            prg_ram: vec![0; 0x2000],
            mapper: NesMapper::Nrom,
            header: NesHeader::default(),
            uc_bank: 0,
            mmc1_shift: 0x10,
            mmc1_count: 0,
            mmc1_ctrl: 0x1C,
            mmc1_chr0: 0,
            mmc1_chr1: 1,
            mmc1_prg: 0,
            mmc5_prg: [0; 4],
            mmc5_chr: [0; 8],
            mmc5_chr_mode: 2,
            mmc5_irq_line: 0,
            mmc5_irq_counter: 0,
            mmc5_irq_on: false,
            mmc5_irq_pending: false,
            loaded: false,
            irq_raise_pending: false,
            irq_lower_pending: false,
            next: None,
            last_tick: None,
        }
    }

    /// Parse an iNES header.
    ///
    /// # Errors
    ///
    /// Returns an error when the magic or the size is wrong.
    pub fn parse_header(image: &[u8]) -> Result<NesHeader, SnapshotError> {
        if image.len() < 16 || &image[..4] != b"NES\x1a" {
            return Err(SnapshotError::Mismatch(
                "not an iNES cartridge image".into(),
            ));
        }
        let flags6 = image[6];
        Ok(NesHeader {
            prg_banks: image[4],
            chr_banks: image[5],
            mapper: (image[7] & 0xF0) | (flags6 >> 4),
            mirroring: flags6 & 0x01,
            battery: flags6 & 0x02 != 0,
        })
    }

    /// Parse the header and size the banks from the image.
    fn load(&mut self, image: &[u8]) {
        if image.len() < 16 || &image[..4] != b"NES\x1a" {
            // A raw program image loads as a plain cartridge with
            // writable character RAM.
            self.prg = image.to_vec();
            self.chr = vec![0; 0x2000];
            self.chr_writable = true;
            self.header = NesHeader {
                prg_banks: (self.prg.len() as u8 / 16).max(1),
                chr_banks: 0,
                mapper: 0,
                mirroring: 1,
                battery: false,
            };
            self.mapper = NesMapper::Nrom;
            self.loaded = true;
            return;
        }
        if let Ok(header) = Self::parse_header(image) {
            self.header = header;
            let prg_bytes = usize::from(self.header.prg_banks) * 0x4000;
            let start = 16usize;
            self.prg = image
                .get(start..start.saturating_add(prg_bytes))
                .unwrap_or(&image[start.min(image.len())..])
                .to_vec();
            // A zero character bank count selects the writable RAM.
            self.chr_writable = self.header.chr_banks == 0;
            self.chr = vec![
                0;
                if self.chr_writable {
                    0x2000
                } else {
                    usize::from(self.header.chr_banks) * 0x2000
                }
            ];
            let chr_source = image.get(16 + usize::from(self.header.prg_banks) * 0x4000..);
            if let Some(source) = chr_source {
                let take = self.chr.len().min(source.len());
                self.chr[..take].copy_from_slice(&source[..take]);
            }
            self.mapper = match self.header.mapper {
                1 => NesMapper::Mmc1,
                2 => NesMapper::Uxrom,
                5 => NesMapper::Mmc5,
                _ => NesMapper::Nrom,
            };
        }
        self.prg_ram = vec![0; 0x2000];
        self.loaded = true;
    }

    /// The mirror mode from the header: 0 horizontal, 1 vertical.
    pub fn mirroring(&self) -> u8 {
        self.header.mirroring
    }

    /// True when the boot ran and an image loaded.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// The mapper number of the loaded image.
    pub fn mapper_number(&self) -> u8 {
        self.header.mapper
    }

    /// The program banks of 16 KiB.
    fn prg_bank_count(&self) -> usize {
        (self.prg.len() / 0x4000).max(1)
    }

    /// The program banks of 8 KiB, for the MMC5 windows.
    fn prg_8k_count(&self) -> usize {
        (self.prg.len() / 0x2000).max(1)
    }

    /// The character banks of 4 KiB.
    fn chr_4k_count(&self) -> usize {
        (self.chr.len() / 0x1000).max(1)
    }

    /// The index of the program ROM byte at a CPU address.
    fn prg_index(&self, addr: u16) -> usize {
        match self.mapper {
            NesMapper::Mmc5 => {
                let window = usize::from((addr >> 13) & 0x03);
                let bank = self.mmc5_bank_at(window);
                bank * 0x2000 + usize::from(addr & 0x1FFF)
            }
            _ => {
                let banks = self.prg_bank_count();
                let bank = match self.mapper {
                    NesMapper::Nrom => {
                        if addr >= 0xC000 {
                            banks.saturating_sub(1)
                        } else {
                            0
                        }
                    }
                    NesMapper::Uxrom => {
                        if addr >= 0xC000 {
                            banks.saturating_sub(1)
                        } else {
                            usize::from(self.uc_bank) % banks
                        }
                    }
                    _ => {
                        let mode = (self.mmc1_ctrl >> 2) & 0x03;
                        let _selected = usize::from(self.mmc1_prg & 0x0F) % banks;
                        match mode {
                            3 => {
                                if addr < 0xC000 {
                                    usize::from(self.mmc1_prg) % banks
                                } else {
                                    banks.saturating_sub(1)
                                }
                            }
                            2 => {
                                if addr < 0xC000 {
                                    0
                                } else {
                                    usize::from(self.mmc1_prg) % banks
                                }
                            }
                            _ => {
                                // The 32-KiB window of an even bank.
                                usize::from(self.mmc1_prg & 0x0E) % banks
                                    + usize::from(addr >= 0xC000)
                            }
                        }
                    }
                };
                bank * 0x4000 + usize::from(addr & 0x3FFF)
            }
        }
    }

    /// The MMC5 eight-KiB program bank of one window; the last
    /// window carries the fixed high bank.
    fn mmc5_bank_at(&self, window: usize) -> usize {
        let banks = self.prg_8k_count();
        match window {
            3 => banks.saturating_sub(1),
            _ => usize::from(self.mmc5_prg[window.min(3)] & 0x1F) % banks,
        }
    }

    /// The index of the character byte at a PPU address.
    fn chr_index(&self, addr: u16) -> usize {
        let index = usize::from(addr & 0x1FFF);
        match self.mapper {
            NesMapper::Mmc1 => {
                if self.mmc1_ctrl & 0x10 == 0 {
                    // One eight-kilobyte window.
                    usize::from(self.mmc1_chr0 & 0x0F) * 0x1000 + index
                } else {
                    let bank = if addr < 0x1000 {
                        self.mmc1_chr0
                    } else {
                        self.mmc1_chr1
                    };
                    usize::from(bank & 0x0F) * 0x1000 + index
                }
            }
            NesMapper::Mmc5 => match self.mmc5_chr_mode {
                0 => {
                    usize::from(self.mmc5_chr[4] & 0x3F) * 0x1000 % (self.chr.len().max(1)) + index
                }
                _ => {
                    let page = usize::from(addr >> 10) & 0x07;
                    let bank = usize::from(self.mmc5_chr[page & 0x07] & 0x3F) % self.chr_4k_count();
                    usize::from(addr & 0x03FF) + bank * 0x400
                }
            },
            _ => index % self.chr.len().max(1),
        }
    }

    /// The mapper register write at a program address.
    fn write_mapper(&mut self, addr: u16, value: u8) {
        if self.mapper == NesMapper::Mmc5 && matches!(addr, 0x5100..=0x52FF) {
            match addr {
                0x5100 => {
                    // The mode register: this scope runs the
                    // per-window mode; the value records.
                }
                0x5114..=0x5117 => {
                    let slot = usize::from(addr - 0x5114);
                    self.mmc5_prg[slot] = value;
                }
                0x5120..=0x512B => {
                    let slot = usize::from(addr - 0x5120) % 8;
                    self.mmc5_chr[slot] = value;
                }
                0x5203 => self.mmc5_irq_line = value & 0x7F,
                0x5204 => {
                    if value & 0x40 != 0 && !self.mmc5_irq_on {
                        self.mmc5_irq_on = true;
                        self.arm_self();
                    }
                    if !self.mmc5_irq_on && self.irq_raise_pending {
                        self.irq_raise_pending = false;
                        self.irq_lower_pending = true;
                        self.arm_self();
                    }
                    self.mmc5_irq_on = value & 0x40 != 0;
                    if value & 0x80 == 0 {
                        self.mmc5_irq_pending = false;
                    }
                }
                _ => {}
            }
            return;
        }
        match self.mapper {
            NesMapper::Nrom => {}
            NesMapper::Uxrom => {
                self.uc_bank = value & 0x0F;
            }
            NesMapper::Mmc1 => {
                if value & 0x80 != 0 {
                    // The reset command clears the shift register.
                    self.mmc1_shift = 0x10;
                    self.mmc1_count = 0;
                    return;
                }
                self.mmc1_shift = (self.mmc1_shift >> 1) | (u32::from(value & 0x01) << 4);
                self.mmc1_count += 1;
                if self.mmc1_count < 5 {
                    return;
                }
                self.mmc1_count = 0;
                let data = (self.mmc1_shift & 0x1F) as u8;
                self.mmc1_shift = 0x10;
                match addr {
                    0x8000..=0x9FFF => self.mmc1_ctrl = data,
                    0xA000..=0xBFFF => self.mmc1_chr0 = data & 0x0F,
                    0xC000..=0xDFFF => self.mmc1_chr1 = data & 0x0F,
                    _ => self.mmc1_prg = data & 0x0F,
                }
            }
            NesMapper::Mmc5 => {}
        }
    }

    /// Arm the scheduler visit that raises the IRQ level.
    fn arm_self(&mut self) {
        self.next = Some(Tick(0));
        self.last_tick = None;
    }
}

impl BusAccess for NesCart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let addr16 = u16::try_from(address.0).unwrap_or(0xFFFF);
        match u16::try_from(address.0).unwrap_or(0xFFFF) {
            0x0000..=0x1FFF => {
                let index = self.chr_index(addr16);
                u32::from(self.chr.get(index).copied().unwrap_or(0))
            }
            0x6000..=0x7FFF => u32::from(self.prg_ram[usize::from(addr16 & 0x1FFF)]),
            0x8000..=0xFFFF => {
                let index = self.prg_index(addr16);
                u32::from(self.prg.get(index).copied().unwrap_or(0))
            }
            // The MMC5 status register returns its flags.
            0x5204 if self.mapper == NesMapper::Mmc5 => {
                // The status read returns the flags and clears the
                // IRQ flag; the level release follows on the visit.
                let enabled = self.mmc5_irq_on;
                let pending = self.mmc5_irq_pending;
                self.mmc5_irq_pending = false;
                if pending {
                    self.irq_lower_pending = true;
                    self.irq_raise_pending = false;
                    self.arm_self();
                }
                u32::from(pending) << 7 | u32::from(enabled) << 6 | 0x40
            }
            _ => 0,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        let addr16 = u16::try_from(address.0).unwrap_or(0xFFFF);
        match u16::try_from(address.0).unwrap_or(0xFFFF) {
            0x0000..=0x1FFF => {
                if self.chr_writable {
                    let index = self.chr_index(addr16);
                    if index < self.chr.len() {
                        self.chr[index] = v;
                    }
                }
            }
            0x6000..=0x7FFF => self.prg_ram[usize::from(addr16 & 0x1FFF)] = v,
            0x8000..=0xFFFF => self.write_mapper(addr16, v),
            _ => {}
        }
    }

    /// The raw debug read.
    fn peek(&self, address: Address) -> u8 {
        let addr16 = u16::try_from(address.0).unwrap_or(0xFFFF);
        match u16::try_from(address.0).unwrap_or(0xFFFF) {
            0x0000..=0x1FFF => self.chr.get(self.chr_index(addr16)).copied().unwrap_or(0),
            0x6000..=0x7FFF => self.prg_ram[usize::from(addr16 & 0x1FFF)],
            0x8000..=0xFFFF => self.prg.get(self.prg_index(addr16)).copied().unwrap_or(0),
            _ => 0,
        }
    }

    fn decodes(&self, address: Address) -> bool {
        address.0 < 0x2000 || matches!(address.0, 0x6000..=0xFFFF)
    }
}

impl Device for NesCart {
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
        // A reset keeps the ROM images and clears the mapper state.
        self.uc_bank = 0;
        self.mmc1_shift = 0x10;
        self.mmc1_count = 0;
        self.mmc1_ctrl = 0x1C;
        self.mmc1_chr0 = 0;
        self.mmc1_chr1 = 1;
        self.mmc1_prg = 0;
        self.mmc5_prg = [0; 4];
        self.mmc5_chr = [0; 8];
        self.mmc5_chr_mode = 2;
        self.mmc5_irq_line = 0;
        self.mmc5_irq_counter = 0;
        self.mmc5_irq_on = false;
        self.mmc5_irq_pending = false;
        self.irq_raise_pending = false;
        self.irq_lower_pending = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Rom { path }) => {
                self.load(&std::fs::read(path).unwrap_or_default());
            }
            Some(BootState::Hex { bytes }) => self.load(bytes),
            _ => self.load(&[]),
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(24 + self.prg.len() + self.chr.len() + self.prg_ram.len());
        out.extend_from_slice(b"NESC");
        out.push(self.header.prg_banks);
        out.push(self.header.chr_banks);
        out.push(self.header.mapper);
        out.push(self.header.mirroring);
        out.push(u8::from(self.header.battery));
        out.push(self.pal.into());
        out.push(self.uc_bank);
        out.extend_from_slice(&self.mmc1_shift.to_le_bytes());
        out.push(self.mmc1_count);
        out.push(self.mmc1_ctrl);
        out.push(self.mmc1_chr0);
        out.push(self.mmc1_chr1);
        out.push(self.mmc1_prg);
        out.extend_from_slice(&self.mmc5_prg);
        out.extend_from_slice(&self.mmc5_chr);
        out.push(self.mmc5_chr_mode);
        out.push(self.mmc5_irq_line);
        out.push(self.mmc5_irq_counter);
        out.push(u8::from(self.mmc5_irq_on));
        out.push(u8::from(self.mmc5_irq_pending));
        out.extend_from_slice(&(self.prg.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.prg);
        out.extend_from_slice(&(self.chr.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.chr);
        out.extend_from_slice(&self.prg_ram);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 37 || &data[..4] != b"NESC" {
            return Err(SnapshotError::Mismatch("bad nes cart chunk".into()));
        }
        let mut pos = 4usize;
        self.header.prg_banks = data[pos];
        pos += 1;
        self.header.chr_banks = data[pos];
        pos += 1;
        self.header.mapper = data[pos];
        pos += 1;
        self.header.mirroring = data[pos] & 0x01;
        pos += 1;
        self.header.battery = data[pos] != 0;
        pos += 1;
        self.pal = data[pos] != 0;
        pos += 1;
        self.uc_bank = data[pos];
        pos += 1;
        self.mmc1_shift = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.mmc1_count = data[pos];
        pos += 1;
        self.mmc1_ctrl = data[pos];
        pos += 1;
        self.mmc1_chr0 = data[pos];
        pos += 1;
        self.mmc1_chr1 = data[pos];
        pos += 1;
        self.mmc1_prg = data[pos];
        pos += 1;
        self.mmc5_prg.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.mmc5_chr.copy_from_slice(&data[pos..pos + 8]);
        pos += 8;
        self.mmc5_chr_mode = data[pos];
        pos += 1;
        self.mmc5_irq_line = data[pos];
        pos += 1;
        self.mmc5_irq_counter = data[pos];
        pos += 1;
        self.mmc5_irq_on = data[pos] != 0;
        pos += 1;
        self.mmc5_irq_pending = data[pos] != 0;
        pos += 1;
        self.mapper = match self.header.mapper {
            1 => NesMapper::Mmc1,
            2 => NesMapper::Uxrom,
            5 => NesMapper::Mmc5,
            _ => NesMapper::Nrom,
        };
        let read_len = |buf: &[u8], pos: &mut usize| -> Result<usize, SnapshotError> {
            if buf.len() < *pos + 4 {
                return Err(SnapshotError::Mismatch("cart chunk truncated".into()));
            }
            let len = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().expect("4 bytes"));
            *pos += 4;
            Ok(usize::try_from(len).unwrap_or(0))
        };
        let prg_len = read_len(data, &mut pos)?;
        if pos + prg_len > data.len() {
            return Err(SnapshotError::Mismatch("cart prg exceeds chunk".into()));
        }
        self.prg = data[pos..pos + prg_len].to_vec();
        pos += prg_len;
        let chr_len = read_len(data, &mut pos)?;
        if pos + chr_len > data.len() {
            return Err(SnapshotError::Mismatch("cart chr exceeds chunk".into()));
        }
        self.chr = data[pos..pos + chr_len].to_vec();
        pos += chr_len;
        self.chr_writable = self.header.chr_banks == 0;
        let ram_len = data.len().saturating_sub(pos);
        self.prg_ram = data[pos..pos + ram_len].to_vec();
        self.irq_raise_pending = false;
        self.irq_lower_pending = false;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    /// The MMC5 scanline counter runs on the scanline edges. The
    /// vblank start reloads the counter and the frame's scanlines
    /// clock it down; the IRQ asserts when it passes the target.
    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        if line == "vblank" && level == Level::High {
            // The reload distance counts the scanline edges between
            // the vblank start and the target line: 20 NTSC or 72 PAL
            // lines before the pre-render, plus the one-line lag.
            let lead = if self.pal { 72 } else { 20 };
            self.mmc5_irq_counter = self.mmc5_irq_line.wrapping_add(lead + 1);
            return;
        }
        if line != "scanline" || level != Level::High {
            return;
        }
        if !self.mapper_serves() {
            return;
        }
        self.mmc5_irq_counter = self.mmc5_irq_counter.wrapping_sub(1);
        if self.mapper == NesMapper::Mmc5 && self.mmc5_irq_on && self.mmc5_irq_counter & 0x40 != 0 {
            // The counter wraps past the programmed scanline: the
            // IRQ asserts and holds until the status read.
            if !self.mmc5_irq_pending {
                self.mmc5_irq_pending = true;
                self.irq_raise_pending = true;
                self.arm_self();
            }
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        if self.irq_raise_pending {
            self.irq_raise_pending = false;
            ctx.notify("irq", Level::High, tick);
        }
        if self.irq_lower_pending {
            self.irq_lower_pending = false;
            ctx.notify("irq", Level::Low, tick);
        }
        self.next = None;
    }
}

impl NesCart {
    /// True when the mapper needs visit scheduling.
    fn mapper_serves(&self) -> bool {
        self.mapper == NesMapper::Mmc5
    }
}
