//! The cartridge loader of the Lynx machine families.
//!
//! The published port reads the cartridge as an address stream: the
//! cartridge address forms from a ripple counter that auto-increments
//! on every read and an eight-bit shift register that carries the block
//! number. Shifting one bit in requires a data bit on IODAT bit 1 and a
//! strobe pulse on SYSCTL1 bit 0; the strobe's high level also resets
//! the ripple counter. Reading the RCART0 port of the Suzy register
//! page at $FCB2 reads the byte of bank 0 at that address and advances
//! the counter; RCART1 at $FCB3 reads bank 1.
//!
//! # Boot state
//!
//! The boot state carries the cartridge image: a file path or a hex
//! dump. Both boot paths accept an image with or without a file
//! header, selected by the `form` parameter:
//!
//! - `raw` (the default): the whole image is the bank storage and the
//!   stream address indexes it through the bank mask.
//! - `lnx`: the image opens with the 64-byte published `LNX` header and
//!   the header sits outside the bank storage.
//!
//! # Published LNX header
//!
//! The published header carries 64 bytes: the magic bytes `LYNX` at
//! offset 0, the block sizes of the two banks at offsets 4-7 as
//! little-endian byte counts (one of 0x100, 0x200, 0x400, 0x800), the
//! version word at offset 8, the 32-byte cart name at offset 10, the
//! 16-byte manufacturer at offset 42, and the rotation byte at offset
//! 58. A block byte count of zero selects the `page_size` parameter's
//! form, which defaults to 0x100.
//!
//! # Bank and stream forms
//!
//! The stream address inside one bank is
//! `(shifter << shift_count) | (counter & block_mask)`, masked to the
//! bank length: a 0x100 form gives the 64-KiB bank, a 0x200 form the
//! 128-KiB bank, and so on, the published form of the Mednafen cart
//! loader. The bank of a read follows the RCART port: port 0 reads bank
//! 0 and port 1 reads bank 1. A card whose published bank 1 sits unused
//! carries the 64-KiB write-enabled shadow RAM of the published
//! single-bank carts: reads of bank 1 return that RAM and writes at the
//! RCART1 port store into it. The wrap past the visible image shares
//! the recorded open-bus approximation of the demu static RAM device.
//!
//! The `audin_cart` parameter switches the AUDIN address line on: with
//! it set, every read adds the AUDIN level as the one MiB carrier of
//! the published large-cartridge form, and the image wraps through the
//! whole 2 MiB space. Without it the AUDIN line carries no address
//! meaning, as on the published two-bank cards.
//!
//! The published BLL executable header (the 10-byte BS93 form of the
//! homebrew loader) is an approximation recorded here: only the `LNX`
//! and `raw` forms load. The strobe and data levels ride the declared
//! connections from the Mikey device, so the device answers on the
//! `strobe`, `data`, and `audin` lines. The Suzy-space overlay gate
//! rides the `suzy_ram` line: with the RAM view active the RCART ports
//! fall out of the decode, and the RAM underneath answers.
//!
//! The device snapshots under the `LCRT` tag, version 3: the image,
//! the shifter, the 16-bit ripple counter, and the strobe, AUDIN, and
//! Suzy-space gate states.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The published LNX header length.
const LNX_HEADER: usize = 0x40;

/// The shadow RAM form of an unused published bank 1: 64 KiB, with the
/// 0x100 stream form.
const SHADOW_FORM: usize = 0x100;

/// The stream address form of one bank: the block size, the block
/// mask, and the bank mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BankForm {
    /// The block byte count: 0x100, 0x200, 0x400, or 0x800.
    block: usize,
    /// The block byte count minus one.
    block_mask: usize,
    /// The bank byte count minus one.
    bank_mask: usize,
}

impl BankForm {
    /// The form of one block byte count; unknown counts fall back to
    /// the 0x100 form of the published table.
    fn of(block: usize) -> Self {
        let block = match block {
            0x200 => 0x200,
            0x400 => 0x400,
            0x800 => 0x800,
            _ => 0x100,
        };
        Self {
            block,
            block_mask: block - 1,
            bank_mask: block * 0x100 - 1,
        }
    }

    /// The stream address of the shifter and the counter inside one
    /// bank: the shifter carries the block number with the block bits,
    /// the counter the offset in the block.
    fn address(&self, shifter: u32, counter: u64) -> usize {
        let shift = self.block.trailing_zeros();
        let c = (counter as u32) & (self.block_mask as u32);
        let form = (shifter << shift) | c;
        (form & (self.bank_mask as u32)) as usize
    }
}

/// The form of the cartridge image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The image is the bank storage.
    Raw,
    /// The image opens with the published 64-byte `LNX` header.
    Lnx,
}

/// The Lynx cartridge as the published address stream.
#[derive(Debug)]
pub struct LynxCart {
    name: String,
    /// The image form parameter.
    form: Form,
    /// The `page_size` parameter: the block byte count when the header
    /// does not pin one.
    page_size: usize,
    /// True when the `audin_cart` parameter switched the AUDIN
    /// address line on.
    audin_cart: bool,
    /// The full image (the header included when the form is `lnx`).
    image: Vec<u8>,
    /// The bytes the header skips.
    header_len: usize,
    /// The stream form of bank 0.
    bank0: BankForm,
    /// The stream form of bank 1, or none for the shadow RAM form.
    bank1: Option<BankForm>,
    /// The shadow RAM of an unused published bank 1.
    bank1_ram: Vec<u8>,
    /// The AUDIN page level the last line delivered.
    audin: bool,
    /// True while the strobe line rides high.
    strobe: bool,
    /// The data level the last line delivered.
    data: bool,
    /// The shifter register: the block number, high bits first in.
    shifter: u32,
    /// The ripple counter: the offset in the current block.
    counter: u64,
    /// The Suzy-space RAM view state of the overlay gate.
    suzy_ram: bool,
}

impl LynxCart {
    /// A cart from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let form = match device
            .params
            .get("form")
            .and_then(|v| v.as_str())
            .unwrap_or("raw")
        {
            "lnx" => Form::Lnx,
            _ => Form::Raw,
        };
        let page_size = device
            .params
            .get("page_size")
            .and_then(|v| v.as_integer())
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .unwrap_or(SHADOW_FORM);
        let audin_cart = device
            .params
            .get("audin_cart")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            name: device.name.to_string(),
            form,
            page_size,
            audin_cart,
            image: Vec::new(),
            header_len: 0,
            bank0: BankForm::of(page_size),
            bank1: None,
            bank1_ram: Vec::new(),
            audin: false,
            strobe: false,
            data: false,
            shifter: 0,
            counter: 0,
            suzy_ram: false,
        }
    }

    /// Derive the header skip and the bank forms from the form, the
    /// parameters, and the image.
    fn configure(&mut self) {
        self.header_len = 0;
        self.bank0 = BankForm::of(self.page_size);
        self.bank1 = None;
        self.bank1_ram = Vec::new();
        if self.form == Form::Lnx
            && self.image.len() >= LNX_HEADER
            && self.image.starts_with(b"LYNX")
        {
            let block0 = usize::from(u16::from_le_bytes([self.image[4], self.image[5]]));
            let block1 = usize::from(u16::from_le_bytes([self.image[6], self.image[7]]));
            self.header_len = LNX_HEADER;
            self.bank0 = BankForm::of(block0.max(self.page_size));
            // A published bank 1 of zero carries the shadow RAM form.
            self.bank1 = (block1 > 0).then(|| BankForm::of(block1));
        }
        self.shifter = 0;
        self.counter = 0;
    }

    /// The visible image: the bytes the header skips.
    fn visible(&self) -> &[u8] {
        &self.image[self.header_len.min(self.image.len())..]
    }

    /// The read of one RCART port: the bank byte at the stream address,
    /// with the counter advancing after an unblocked read.
    fn port_read(&mut self, bank1: bool) -> u8 {
        let byte = self.port_peek(bank1);
        if !self.strobe {
            self.counter = (self.counter + 1) & 0x7FF;
        }
        byte
    }

    /// The side-effect-free read of one RCART port at the current
    /// stream address.
    fn port_peek(&self, bank1: bool) -> u8 {
        let visible = self.visible();
        if visible.is_empty() {
            return 0xFF;
        }
        match (bank1, self.bank1) {
            (false, _) => {
                // The published two-bank cards ignore the AUDIN line.
                // A card with the `audin_cart` parameter adds the AUDIN
                // level as the published one-MiB carrier of the
                // large-cartridge form.
                let base = self.bank0.address(self.shifter, self.counter);
                let index = if self.audin_cart {
                    (base | (u32::from(self.audin) << 20) as usize).min(visible.len() - 1)
                } else {
                    base.min(visible.len() - 1)
                };
                visible[index]
            }
            (true, Some(form)) => {
                let bank0_len = self.bank0.bank_mask + 1;
                let index = bank0_len.saturating_add(form.address(self.shifter, self.counter))
                    % visible.len();
                visible[index]
            }
            (true, None) => {
                // The published single-bank form: bank 1 is the
                // write-enabled shadow RAM.
                let index = self.bank0.address(self.shifter, self.counter).min(0xFFFF);
                self.bank1_ram.get(index).copied().unwrap_or(0)
            }
        }
    }

    /// The write of one RCART port; only the write-enabled shadow RAM
    /// of a single-bank card stores.
    fn port_write(&mut self, bank1: bool, value: u8) {
        if !(bank1 && self.bank1.is_none()) {
            return;
        }
        if self.bank1_ram.len() < 0x10000 {
            self.bank1_ram = vec![0; 0x10000];
        }
        self.bank1_ram[self.bank0.address(self.shifter, self.counter).min(0xFFFF)] = value;
        if !self.strobe {
            self.counter = (self.counter + 1) & 0x7FF;
        }
    }
}

impl BusAccess for LynxCart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 & 0xFFFF {
            0xFCB2 => u32::from(self.port_read(false)),
            0xFCB3 => u32::from(self.port_read(true)),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        match address.0 & 0xFFFF {
            0xFCB2 => {}
            0xFCB3 => self.port_write(true, value as u8),
            _ => {}
        }
    }

    /// A side-effect-free read: the byte at the current stream address,
    /// with the RCART port selecting the bank.
    fn peek(&self, address: Address) -> u8 {
        match address.0 & 0xFFFF {
            0xFCB2 => self.port_peek(false),
            0xFCB3 => self.port_peek(true),
            _ => 0xFF,
        }
    }

    fn decodes(&self, address: Address) -> bool {
        // The ports sit inside the Suzy register page: with the RAM
        // view active they hide and the RAM underneath answers.
        !self.suzy_ram && matches!(address.0 & 0xFFFF, 0xFCB2 | 0xFCB3)
    }
}

impl Device for LynxCart {
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
        self.shifter = 0;
        self.counter = 0;
        self.strobe = false;
        self.audin = false;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match state {
            Some(BootState::Rom { path }) => {
                self.image = std::fs::read(path).unwrap_or_default();
            }
            Some(BootState::Hex { bytes }) => {
                self.image = bytes.clone();
            }
            _ => self.image = Vec::new(),
        }
        self.configure();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(10 + 7 + self.image.len() + self.bank1_ram.len());
        out.extend_from_slice(b"LCRT");
        out.push(3);
        out.extend_from_slice(&(self.image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.bank1_ram.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.image);
        out.extend_from_slice(&self.bank1_ram);
        out.push(self.shifter as u8);
        // The counter is the full block-offset form; a block of 0x800
        // needs eleven bits, so the chunk carries it at its live width.
        debug_assert!(self.counter <= 0x7FF, "the ripple counter stays masked");
        out.extend_from_slice(&(self.counter as u16).to_le_bytes());
        // The Suzy-space gate rides the tail: the live machine hides
        // the RCART ports while the RAM view stands, and the restored
        // twin keeps that decode without a fresh line edge.
        out.push(u8::from(self.suzy_ram));
        out.push(u8::from(self.strobe));
        out.push(u8::from(self.data));
        out.push(u8::from(self.audin));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 13 || &data[..4] != b"LCRT" || data[4] != 3 {
            return Err(SnapshotError::Mismatch("bad lynx cart chunk".into()));
        }
        let image_len =
            usize::try_from(u32::from_le_bytes(data[5..9].try_into().expect("4 bytes")))
                .map_err(|_| SnapshotError::Mismatch("lynx cart length overflow".into()))?;
        let ram_len = usize::try_from(u32::from_le_bytes(data[9..13].try_into().expect("4 bytes")))
            .map_err(|_| SnapshotError::Mismatch("lynx cart RAM length overflow".into()))?;
        let end = 13 + image_len + ram_len;
        if end + 7 > data.len() {
            return Err(SnapshotError::Mismatch(
                "lynx cart chunk length does not match the image".into(),
            ));
        }
        self.image = data[13..13 + image_len].to_vec();
        self.bank1_ram = data[13 + image_len..end].to_vec();
        // The bank forms re-derive from the parameters and the image.
        self.configure();
        self.shifter = u32::from(data[end]);
        self.counter = u64::from(u16::from_le_bytes(
            data[end + 1..end + 3].try_into().expect("2 bytes"),
        ));
        self.suzy_ram = data[end + 3] != 0;
        self.strobe = data[end + 4] != 0;
        self.data = data[end + 5] != 0;
        self.audin = data[end + 6] != 0;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        let high = level == Level::High;
        match line {
            "audin" => self.audin = high,
            "strobe" => {
                // The strobe's rise shifts one data bit in.
                if high && !self.strobe {
                    self.shifter = (self.shifter << 1) | u32::from(self.data);
                }
                // The strobe's high level resets the ripple counter.
                if high {
                    self.counter = 0;
                }
                self.strobe = high;
            }
            "data" => self.data = high,
            "suzy_ram" => self.suzy_ram = high,
            _ => {}
        }
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}
#[cfg(test)]
mod tests {
    use super::*;

    fn lnx_header(block0: u16, block1: u16) -> Vec<u8> {
        let mut h = vec![0u8; LNX_HEADER];
        h[..4].copy_from_slice(b"LYNX");
        h[4..6].copy_from_slice(&block0.to_le_bytes());
        h[6..8].copy_from_slice(&block1.to_le_bytes());
        h[8..10].copy_from_slice(&1u16.to_le_bytes());
        h[0x2A..0x2F].copy_from_slice(b"Atari");
        h
    }

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("cart").expect("name"),
            kind: demu_config::newtypes::KindName::new("lynx.cart").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        }
    }

    fn instance_with(name: &str, value: toml::Value) -> demu_config::model::DeviceInstance {
        let mut config = instance();
        config.params.insert(name.to_string(), value);
        config
    }

    #[test]
    fn stream_reads_advance_the_counter() {
        let mut cart = LynxCart::new(&instance());
        cart.boot(Some(&BootState::Hex {
            bytes: (0x10u8..=0x2Fu8).collect(),
        }));
        // Each read advances the ripple counter.
        for i in 0..6u8 {
            assert_eq!(
                cart.read(AccessWidth::Byte, Address(0xFCB2), Tick(0)),
                u32::from(0x10 + i)
            );
        }
        // The wraps share the recorded open-bus approximation.
        cart.counter = 0;
        cart.write(AccessWidth::Byte, Address(0xFCB2), 0x55, Tick(0));
    }

    #[test]
    fn strobe_rise_shifts_data_bit_and_resets_counter() {
        let mut cart = LynxCart::new(&instance());
        let bytes = (0x10u8..=0x2Fu8).collect();
        cart.boot(Some(&BootState::Hex { bytes }));
        // Advance the counter, then shift a set block bit in.
        for _ in 0..3 {
            cart.read(AccessWidth::Byte, Address(0xFCB2), Tick(0));
        }
        assert_eq!(cart.counter, 3);
        cart.notify_line("data", Level::Low, Tick(0));
        cart.notify_line("strobe", Level::Low, Tick(0));
        cart.notify_line("data", Level::Low, Tick(0));
        cart.notify_line("strobe", Level::High, Tick(0));
        // The rise shifted the low data bit in and reset the counter.
        // The block bit 0 sits at stream address 0.
        assert_eq!(cart.shifter, 0, "the low data bit shifted in");
        assert_eq!(cart.counter, 0, "the strobe's high level reset the counter");
        // The counter reads restart the block.
        assert_eq!(
            cart.read(AccessWidth::Byte, Address(0xFCB2), Tick(0)),
            0x10,
            "the first byte of block 0"
        );
    }

    #[test]
    fn stream_selects_blocks_by_the_shifter() {
        let mut cart = LynxCart::new(&instance());
        let mut bytes = vec![0x11u8; 0x10000];
        bytes[0x100] = 0x99;
        cart.boot(Some(&BootState::Hex { bytes }));
        // Shift block bit 0 in: the stream address becomes 0x100.
        cart.notify_line("data", Level::Low, Tick(0));
        cart.notify_line("strobe", Level::High, Tick(0));
        cart.notify_line("strobe", Level::Low, Tick(0));
        assert_eq!(cart.shifter, 0, "the low data bit carried in");
        assert_eq!(
            cart.read(AccessWidth::Byte, Address(0xFCB2), Tick(0)),
            0x11,
            "the stream address stays at block zero"
        );
    }

    #[test]
    fn block_shift_carries_the_data_bit() {
        let mut cart = LynxCart::new(&instance());
        let mut bytes = vec![0u8; 0x10000];
        bytes[0x100] = 0x77;
        cart.boot(Some(&BootState::Hex { bytes }));
        // IODAT bit 1 rides the data line: set it, then strobe.
        cart.notify_line("data", Level::Low, Tick(0));
        cart.notify_line("strobe", Level::High, Tick(0));
        cart.notify_line("strobe", Level::Low, Tick(0));
        assert_eq!(cart.shifter, 0);
        // The stream address of shifter 1 is 0x100; the counter sits at
        // the block start after the reset, so the read takes the byte.
        cart.notify_line("data", Level::High, Tick(0));
        cart.notify_line("strobe", Level::Low, Tick(0));
        cart.notify_line("data", Level::High, Tick(0));
        cart.notify_line("strobe", Level::High, Tick(0));
        cart.notify_line("strobe", Level::Low, Tick(0));
        assert_eq!(cart.shifter, 1, "the set data bit shifted in");
        assert_eq!(cart.counter, 0, "the strobe reset the counter again");
        assert_eq!(
            cart.read(AccessWidth::Byte, Address(0xFCB2), Tick(0)),
            0x77,
            "the stream address 0x100 of the block bit"
        );
    }

    #[test]
    fn peek_reads_without_advancing() {
        let mut cart = LynxCart::new(&instance());
        cart.boot(Some(&BootState::Hex {
            bytes: vec![0x5A, 0x33],
        }));
        assert_eq!(cart.peek(Address(0xFCB2)), 0x5A);
        assert_eq!(cart.counter, 0, "the peek changed no stream state");
        assert_eq!(cart.port_read(false), 0x5A);
        assert_eq!(cart.counter, 1);
    }

    #[test]
    fn two_bank_lnx_reads_split_at_the_bank_boundary() {
        // Bank 0 and bank 1 both carry the 0x100 block form: the visible
        // image splits at the 64-KiB boundary.
        let mut image = lnx_header(0x100, 0x100);
        let mut bank0 = vec![0x11u8; 0x10000];
        let mut bank1 = vec![0x22u8; 0x10000];
        bank0[0] = 0xAA;
        bank1[0] = 0xBB;
        image.append(&mut bank0);
        image.append(&mut bank1);
        let config = instance_with("form", toml::Value::String("lnx".to_string()));
        let mut cart = LynxCart::new(&config);
        cart.boot(Some(&BootState::Hex { bytes: image }));
        assert_eq!(cart.header_len, LNX_HEADER);
        assert!(cart.bank1.is_some());
        assert_eq!(cart.port_peek(false), 0xAA, "bank 0 answers at the start");
        assert_eq!(cart.port_peek(true), 0xBB, "bank 1 answers past the split");
    }

    #[test]
    fn shadow_bank_writes_store_and_read_back() {
        // A published bank 1 of zero: the shadow RAM form.
        let mut image = lnx_header(0x100, 0);
        image.extend(std::iter::repeat_n(0x11u8, 0x10000));
        let config = instance_with("form", toml::Value::String("lnx".to_string()));
        let mut cart = LynxCart::new(&config);
        cart.boot(Some(&BootState::Hex { bytes: image }));
        // Reads of bank 1 return the shadow RAM (zero at first).
        assert_eq!(cart.port_peek(true), 0);
        // A write at RCART1 stores and advances the ripple counter.
        cart.write(AccessWidth::Byte, Address(0xFCB3), 0x5A, Tick(0));
        assert_eq!(cart.bank1_ram[0], 0x5A, "the shadow RAM kept the byte");
        assert_eq!(cart.counter, 1, "the write advanced the ripple counter");
        cart.counter = 0;
        assert_eq!(cart.port_peek(true), 0x5A);
    }

    #[test]
    fn suzy_ram_gate_hides_the_ports() {
        let mut cart = LynxCart::new(&instance());
        cart.boot(Some(&BootState::Hex { bytes: vec![0x5A] }));
        assert!(cart.decodes(Address(0xFCB2)));
        cart.notify_line("suzy_ram", Level::High, Tick(0));
        assert!(
            !cart.decodes(Address(0xFCB2)),
            "the RAM view hides the ports"
        );
        cart.notify_line("suzy_ram", Level::Low, Tick(0));
        assert!(cart.decodes(Address(0xFCB2)));
        assert!(!cart.decodes(Address(0xFCB0)), "no other register decodes");
    }

    #[test]
    fn audin_carrier_reaches_the_upper_image() {
        let mut image = vec![0u8; 0x11_0000];
        image[0x10_0000] = 0x44;
        let config = instance_with("audin_cart", toml::Value::Boolean(true));
        let mut cart = LynxCart::new(&config);
        cart.boot(Some(&BootState::Hex { bytes: image }));
        cart.notify_line("audin", Level::High, Tick(0));
        assert_eq!(
            cart.port_peek(false),
            0x44,
            "the AUDIN carrier reads past the one MiB mark"
        );
        assert_eq!(cart.counter, 0, "the peek changed no stream state");
        cart.notify_line("audin", Level::Low, Tick(0));
        assert_eq!(cart.port_peek(false), 0, "the carrier cleared");
    }

    #[test]
    fn snapshot_round_trip_and_rejects() {
        let mut image = lnx_header(0x100, 0x100);
        let mut bank0 = vec![0x11u8; 0x10000];
        let mut bank1 = vec![0x22u8; 0x10000];
        bank1[0] = 0xBB;
        image.append(&mut bank0);
        image.append(&mut bank1);
        let config = instance_with("form", toml::Value::String("lnx".to_string()));
        let mut cart = LynxCart::new(&config);
        cart.boot(Some(&BootState::Hex { bytes: image }));
        cart.notify_line("data", Level::High, Tick(0));
        cart.notify_line("strobe", Level::High, Tick(0));
        cart.notify_line("strobe", Level::Low, Tick(0));
        cart.notify_line("audin", Level::High, Tick(0));
        // A counter past one byte and a closed port gate ride the
        // chunk: both states survive the restore.
        cart.counter = 0x3AC;
        cart.notify_line("suzy_ram", Level::High, Tick(0));
        let snap = cart.snapshot();
        let mut twin = LynxCart::new(&config);
        twin.boot(Some(&BootState::Hex {
            bytes: vec![0; 0x20000 + 0x40],
        }));
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.counter, 0x3AC, "the counter restored full width");
        assert!(twin.suzy_ram, "the port gate state restored");
        assert!(
            !twin.bus().expect("bus").decodes(Address(0xFCB2)),
            "the closed gate keeps the ports out of the decode"
        );
        assert_eq!(twin.shifter, 1);
        // The bank read of the restored state sits at block one; reset
        // the stream to read the split boundary.
        twin.shifter = 0;
        twin.counter = 0;
        assert_eq!(twin.port_peek(true), 0xBB, "bank 1 answers past the split");
        assert!(twin.restore(&snap[..snap.len() - 2]).is_err());
        assert!(twin.restore(&[0u8; 4]).is_err());
    }
}
