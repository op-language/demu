//! The HuCard loader of the PC Engine machine families.
//!
//! The published card port maps the card bytes into the physical
//! space from `$000000` up; the MPR banks of the HuC6280 select
//! contiguous 8-KiB slices of that space, so a flat card window sees
//! the card bytes at the physical offset. This loader answers that
//! flat window form: reads at the bus address fetch
//! `image[(physical address - $000000) modulo image length]`.
//!
//! # Boot state
//!
//! The boot state carries the card image: a file path or a hex dump.
//! The `header` parameter (an offset in bytes, default 0) skips the
//! image's first bytes on the bus: the published card images often
//! carry a preamble (the 512-byte form of the dump tools), and the
//! bus-visible image starts after the skip.
//!
//! # Recorded approximations
//!
//! - The card writes are ignored: the published cards carry no
//!   writable surface in this build.
//! - The mapper registers of the published large-card form (the
//!   windowing of cards beyond the flat bank space) are outside this
//!   build; the loader answers the flat mapped region only.
//! - Reads wrap modulo the visible image, the recorded open-bus
//!   approximation the demu static RAM device shares.
//!
//! The device snapshots under the `PCRT` tag, version 1: the image is
//! constant and returns through the boot state, so the snapshot
//! carries the header offset state only, like the other constant
//! image loaders of this crate.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The PC Engine HuCard over the flat card window.
#[derive(Debug)]
pub struct PceCart {
    name: String,
    /// The full card image (preamble included).
    image: Vec<u8>,
    /// The bytes the configured header skips on the bus.
    header: usize,
}

impl PceCart {
    /// A loader from its configuration entry.
    ///
    /// The `header` parameter gives the byte offset of the
    /// bus-visible image and defaults to 0.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let header = device
            .params
            .get("header")
            .and_then(|v| v.as_integer())
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        Self {
            name: device.name.to_string(),
            image: Vec::new(),
            header,
        }
    }

    /// The byte at a bus address over the flat card window, wrapped
    /// modulo the visible image.
    fn byte_at(&self, address: u32) -> u8 {
        if self.image.len() <= self.header {
            return 0xFF;
        }
        let visible = self.image.len() - self.header;
        let index = address as usize % visible;
        self.image[self.header + index]
    }
}

impl BusAccess for PceCart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.byte_at(address.0))
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, _value: u32, _tick: Tick) {
        // The card window has no writable form in this build.
    }

    fn peek(&self, address: Address) -> u8 {
        self.byte_at(address.0)
    }
}

impl Device for PceCart {
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
            Some(BootState::Hex { bytes }) => {
                self.image = bytes.clone();
            }
            _ => self.image = Vec::new(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(9);
        out.extend_from_slice(b"PCRT");
        out.push(1);
        out.extend_from_slice(&(self.header as u32).to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 9 || &data[..4] != b"PCRT" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "pc engine card chunk must be 9 bytes".into(),
            ));
        }
        let header = u32::from_le_bytes(data[5..9].try_into().expect("4 bytes"));
        self.header = usize::try_from(header)
            .map_err(|_| SnapshotError::Mismatch("card header overflow".into()))?;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("card").expect("name"),
            kind: demu_config::newtypes::KindName::new("pce.cart").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        }
    }

    fn instance_with(name: &str, size: i64) -> demu_config::model::DeviceInstance {
        let mut config = instance();
        config
            .params
            .insert(name.to_string(), toml::Value::Integer(size));
        config
    }

    #[test]
    fn hex_boot_answers_flat_reads() {
        let mut card = PceCart::new(&instance());
        let mut image = vec![0u8; 0x20000];
        image[0] = 0x11;
        image[0x1FFF] = 0x22;
        image[0x20000 - 1] = 0x33;
        card.boot(Some(&BootState::Hex { bytes: image }));
        assert_eq!(card.peek(Address(0)), 0x11);
        assert_eq!(card.peek(Address(0x1FFF)), 0x22);
        assert_eq!(
            card.read(AccessWidth::Byte, Address(0x1FFFF), Tick(0)),
            0x33
        );
    }

    #[test]
    fn header_parameter_skips_preamble() {
        let mut card = PceCart::new(&instance_with("header", 512));
        let mut image = vec![0u8; 512 + 0x10000];
        image[10] = 0xEE;
        image[512] = 0x11;
        image[512 + 0xFFFE] = 0x22;
        card.boot(Some(&BootState::Hex { bytes: image }));
        // The bus-visible image starts after the preamble.
        assert_eq!(card.peek(Address(0)), 0x11);
        assert_eq!(card.peek(Address(0xFFFE)), 0x22);
    }

    #[test]
    fn reads_wrap_modulo_visible_image() {
        let mut card = PceCart::new(&instance_with("header", 8));
        let mut image = vec![0u8; 8 + 0x400];
        image[8] = 0x55;
        image[8 + 0x3FF] = 0x66;
        card.boot(Some(&BootState::Hex { bytes: image }));
        assert_eq!(card.peek(Address(0)), 0x55);
        assert_eq!(card.peek(Address(0x3FF)), 0x66);
        // The physical address past the image wraps (the recorded
        // open-bus approximation).
        assert_eq!(card.peek(Address(0x400)), 0x55);
    }

    #[test]
    fn writes_ignore_and_image_stays() {
        let mut card = PceCart::new(&instance());
        let image = vec![0x99u8; 0x2000];
        card.boot(Some(&BootState::Hex { bytes: image }));
        card.write(AccessWidth::Byte, Address(0x10), 0x42, Tick(0));
        assert_eq!(card.peek(Address(0x10)), 0x99);
    }

    #[test]
    fn empty_image_reads_open_bus() {
        let mut card = PceCart::new(&instance());
        assert_eq!(card.peek(Address(0)), 0xFF);
        // A header longer than the image also guards to open bus.
        card.header = 0x1000;
        assert_eq!(card.peek(Address(0)), 0xFF);
    }

    #[test]
    fn snapshot_carries_header_state_only() {
        let mut image = vec![0u8; 512 + 0x100];
        image[512] = 0x11;
        let mut card = PceCart::new(&instance_with("header", 512));
        card.boot(Some(&BootState::Hex {
            bytes: image.clone(),
        }));
        let snap = card.snapshot();
        assert_eq!(&snap[..4], b"PCRT");
        let mut twin = PceCart::new(&instance_with("header", 512));
        twin.boot(Some(&BootState::Hex { bytes: image }));
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.peek(Address(0)), 0x11);
        assert!(twin.restore(&snap[..8]).is_err());
        assert!(twin.restore(&[0u8; 9]).is_err());
    }
}
