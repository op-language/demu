//! The Sega 8-bit cartridge loader of the Sega machine families, the
//! `sega.cart` device kind.
//!
//! The image form rides the `form` parameter:
//!
//! - `sg1000`: the flat SG-1000 card form.
//! - `sms` (the default): the flat Master System form.
//! - `sega`: the published Sega mapper over four 16-KiB windows with
//!   the latch writes at `$3FFE`, `$3FFF`, `$402C` and `$402D`.
//! - `codemasters`: the published Codemasters mapper with the latch
//!   writes at `$0000`, `$4000` and `$8000`.
//!
//! # Boot state
//!
//! The boot state carries the cartridge image: a file path or a hex
//! dump. An absent, unreadable, or empty image leaves the device on
//! the open-bus form, the convention the other constant image loaders
//! of this crate share. The image is constant and returns through the
//! boot state.
//!
//! # The flat forms
//!
//! A read at the bus address fetches `image[address modulo image
//! length]`; the recorded open-bus wrap form of the demu static RAM
//! loader. The flat forms carry no writable surface.
//!
//! # The Sega mapper
//!
//! Each 16-KiB window maps one page: `$0000-$3FFF` the page of latch
//! 0, `$4000-$7FFF` latch 1, `$8000-$BFFF` latch 2, and
//! `$C000-$FFFF` latch 3. The published latch writes carry one byte
//! and select the latched window: `$3FFE` writes latch 2, `$3FFF`
//! latch 0, `$402C` latch 1, and `$402D` latch 3. The page index
//! wraps `latch modulo page count`, the page count the image length
//! divided by 0x4000 with a count of one for shorter images; the
//! reads past a short image end answer the open-bus form. The
//! published power-up latches 0, 1, 2 and 2 map the first pages onto
//! their windows.
//!
//! # The Codemasters mapper
//!
//! The published byte rule: an `OUT ($0000)` stores `value + 1` as
//! the bank of `$8000-$BFFF`; an `OUT ($8000)` stores `value` as the
//! bank of `$4000-$7FFF`; an `OUT ($4000)` stores `value` as the bank
//! of `$C000-$FFFF`. The bank of `$0000-$3FFF` stays page 0. The page
//! index wraps through the same modulo form as the Sega mapper, and
//! the published power-up latches match the Sega form.
//!
//! # Recorded approximations
//!
//! - The window layout is the machine's: this device answers every
//!   address of its window, and the machine overlays the `$C000`
//!   RAM of the published SMS mapping above the cart device.
//! - The mapper variants with their own RAM or bank masking beyond
//!   the full-byte latch form are outside this build.
//!
//! The device snapshots under the `SGCT` tag, version 1: the form
//! class byte and the four window latches, the form-independent
//! payload; the flat forms carry zero latches.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The page size of the Sega and Codemasters mappers.
const PAGE: usize = 0x4000;
/// The snapshot chunk: tag and version, the class byte, the four
/// window latches.
const CART_SNAPSHOT_LEN: usize = 10;

/// The image form of the cartridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The flat SG-1000 form.
    Sg1000,
    /// The flat Master System form, the default.
    Sms,
    /// The published Sega mapper.
    Sega,
    /// The published Codemasters mapper.
    Codemasters,
}

impl Form {
    fn of(text: &str) -> Self {
        match text {
            "sg1000" => Form::Sg1000,
            "sega" => Form::Sega,
            "codemasters" => Form::Codemasters,
            _ => Form::Sms,
        }
    }

    /// The snapshot class byte.
    fn class(self) -> u8 {
        match self {
            Form::Sg1000 => 0,
            Form::Sms => 1,
            Form::Sega => 2,
            Form::Codemasters => 3,
        }
    }
}

/// The Sega 8-bit cartridge.
#[derive(Debug)]
pub struct SegaCart {
    name: String,
    form: Form,
    /// The full ROM image.
    image: Vec<u8>,
    /// The four window bank latches; the flat forms hold zeros.
    latches: [u8; 4],
}

impl SegaCart {
    /// A loader from its configuration entry.
    ///
    /// The `form` parameter selects the image form and defaults to
    /// `sms`.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let form = Form::of(
            device
                .params
                .get("form")
                .and_then(|v| v.as_str())
                .unwrap_or("sms"),
        );
        Self {
            name: device.name.to_string(),
            form,
            image: Vec::new(),
            latches: [0; 4],
        }
    }

    /// The page count of the image, at least one. The mapper latch
    /// wrap divides by this count.
    fn pages(&self) -> usize {
        (self.image.len() / PAGE).max(1)
    }

    /// The published power-up latches of the mapper forms: the first
    /// pages map onto their windows and the last latch repeats the
    /// third page. The flat forms hold zero latches.
    fn power_up_latches(&mut self) {
        self.latches = if matches!(self.form, Form::Sega | Form::Codemasters) {
            [0, 1, 2, 2]
        } else {
            [0; 4]
        };
    }

    /// The ROM byte at a bus address over the image form.
    fn byte_at(&self, address: u32) -> u8 {
        match self.form {
            Form::Sg1000 | Form::Sms => self.flat_byte(address),
            Form::Sega | Form::Codemasters => self.mapper_byte(address),
        }
    }

    /// The flat form byte: the image wraps at its length, the
    /// recorded open-bus wrap form.
    fn flat_byte(&self, address: u32) -> u8 {
        if self.image.is_empty() {
            return 0xFF;
        }
        self.image[address as usize % self.image.len()]
    }

    /// One mapper byte: the window of the address picks its latch,
    /// the latch wraps modulo the page count.
    fn mapper_byte(&self, address: u32) -> u8 {
        let window = (address >> 14) & 0x03;
        let latch = if self.form == Form::Codemasters && window == 0 {
            0
        } else {
            self.latches[window as usize]
        };
        let page = usize::from(latch) % self.pages();
        self.image
            .get(page * PAGE + (address & 0x3FFF) as usize)
            .copied()
            .unwrap_or(0xFF)
    }

    /// One latch write into the mapper controls; the other writes
    /// store nothing.
    fn write_control(&mut self, address: u32, value: u8) {
        match (self.form, address & 0xFFFF) {
            (Form::Sega, 0x3FFE) => self.latches[2] = value,
            (Form::Sega, 0x3FFF) => self.latches[0] = value,
            (Form::Sega, 0x402C) => self.latches[1] = value,
            (Form::Sega, 0x402D) => self.latches[3] = value,
            (Form::Codemasters, 0x0000) => {
                // The published form: the stored bank is value + 1.
                self.latches[2] = value.wrapping_add(1);
            }
            (Form::Codemasters, 0x8000) => self.latches[1] = value,
            (Form::Codemasters, 0x4000) => self.latches[3] = value,
            _ => {}
        }
    }
}

impl BusAccess for SegaCart {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.byte_at(address.0))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        self.write_control(address.0, value as u8);
    }

    fn peek(&self, address: Address) -> u8 {
        self.byte_at(address.0)
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for SegaCart {
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
        // The latches hold their published power-up state; the image
        // is constant.
        self.power_up_latches();
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
        self.power_up_latches();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(CART_SNAPSHOT_LEN);
        out.extend_from_slice(b"SGCT");
        out.push(1);
        out.push(self.form.class());
        out.extend_from_slice(&self.latches);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != CART_SNAPSHOT_LEN || &data[..4] != b"SGCT" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "sega cart chunk must be 10 bytes".into(),
            ));
        }
        self.form = match data[5] {
            0 => Form::Sg1000,
            2 => Form::Sega,
            3 => Form::Codemasters,
            _ => Form::Sms,
        };
        self.latches.copy_from_slice(&data[6..10]);
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_config::newtypes::{DeviceName, KindName};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    fn instance(form: &str) -> demu_config::model::DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("form".to_string(), toml::Value::String(form.to_string()));
        demu_config::model::DeviceInstance {
            name: DeviceName::new("cart").expect("name"),
            kind: KindName::new("sega.cart").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    /// A cartridge from the image and the form.
    fn cart(form: &str, image: Vec<u8>) -> SegaCart {
        let mut c = SegaCart::new(&instance(form));
        c.boot(Some(&BootState::Hex { bytes: image }));
        c
    }

    /// A 32-KiB flat image with marked byte corners.
    fn flat_image() -> Vec<u8> {
        let mut image = vec![0u8; 0x8000];
        image[0x0000] = 0x11;
        image[0x1FFF] = 0x22;
        image[0x7FFF] = 0x33;
        image
    }

    #[test]
    fn form_defaults_to_flat_sms() {
        let device = demu_config::model::DeviceInstance {
            name: DeviceName::new("cart").expect("name"),
            kind: KindName::new("sega.cart").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        assert_eq!(SegaCart::new(&device).form, Form::Sms);
    }

    #[test]
    fn flat_sg1000_wraps_at_the_image_length() {
        // An 8-KiB card with a 1-KiB tail.
        let mut image = vec![0u8; 0x2100];
        image[0x0000] = 0x11;
        image[0x00FF] = 0x22;
        image[0x1000] = 0x33;
        image[0x20FF] = 0x44;
        let mut c = cart("sg1000", image);
        assert_eq!(c.peek(Address(0x0000)), 0x11);
        assert_eq!(c.peek(Address(0x1000)), 0x33);
        assert_eq!(c.read(AccessWidth::Byte, Address(0x41FF), Tick(0)), 0x44);
        // The address past the image end wraps to the image start.
        assert_eq!(c.peek(Address(0x2100)), 0x11);
        assert_eq!(c.peek(Address(0x4200)), 0x11);
    }

    #[test]
    fn flat_sms_reads_and_ignores_writes() {
        let c = cart("sms", flat_image());
        assert_eq!(c.peek(Address(0x0000)), 0x11);
        assert_eq!(c.peek(Address(0x1FFF)), 0x22);
        assert_eq!(c.peek(Address(0x7FFF)), 0x33);
        let mut w = cart("sms", flat_image());
        w.write(AccessWidth::Byte, Address(0x0000), 0x42, Tick(0));
        assert_eq!(w.peek(Address(0x0000)), 0x11);
    }

    #[test]
    fn empty_image_answers_open_bus() {
        let mut c = SegaCart::new(&instance("sega"));
        c.boot(None);
        assert_eq!(c.peek(Address(0x0000)), 0xFF);
        assert_eq!(c.peek(Address(0x8000)), 0xFF);
        // An unreadable file path follows the same convention.
        let mut absent = SegaCart::new(&instance("sms"));
        absent.boot(Some(&BootState::Rom {
            path: "no-such-cart-file".to_string(),
        }));
        assert_eq!(absent.peek(Address(0x0200)), 0xFF);
    }

    #[test]
    fn sega_mapper_latches_route_the_windows() {
        let mut image = vec![0u8; 0x8000];
        image[0x0001] = 0xA1;
        image[0x4001] = 0xB1;
        let mut c = cart("sega", image);
        // The published power-up latches 0, 1, 2 and 2.
        assert_eq!(c.latches, [0, 1, 2, 2]);
        assert_eq!(c.peek(Address(0x0001)), 0xA1);
        assert_eq!(c.peek(Address(0x4001)), 0xB1);
        // The power-up latch 3 of 2 wraps modulo the two pages onto
        // page 0.
        assert_eq!(c.peek(Address(0xC001)), 0xA1);
        // The latch matrix: $3FFE latch 2, $3FFF latch 0, $402C latch
        // 1, $402D latch 3. The page index wraps modulo 2.
        c.write(AccessWidth::Byte, Address(0x3FFE), 3, Tick(0));
        c.write(AccessWidth::Byte, Address(0x3FFF), 1, Tick(0));
        c.write(AccessWidth::Byte, Address(0x402C), 0, Tick(0));
        c.write(AccessWidth::Byte, Address(0x402D), 3, Tick(0));
        assert_eq!(c.latches, [1, 0, 3, 3]);
        assert_eq!(c.peek(Address(0x0001)), 0xB1);
        assert_eq!(c.peek(Address(0x4001)), 0xA1);
        assert_eq!(c.peek(Address(0x8001)), 0xB1);
        assert_eq!(c.peek(Address(0xC001)), 0xB1);
    }

    #[test]
    fn sega_mapper_wraps_latches_modulo_pages() {
        let mut image = vec![0u8; 0x8000];
        image[0x0001] = 0xA1;
        image[0x4001] = 0xB1;
        let mut c = cart("sega", image);
        // The power-up $4000 window rides latch 1 at page 1.
        let window_page1 = c.peek(Address(0x4001));
        assert_eq!(window_page1, 0xB1);
        // Latch 5 over two pages wraps to page 1: the $8000 window
        // reads the same page-1 bytes.
        c.write(AccessWidth::Byte, Address(0x3FFE), 5, Tick(0));
        assert_eq!(c.peek(Address(0x8001)), window_page1);
        assert_eq!(c.peek(Address(0x8001)), 0xB1);
    }

    #[test]
    fn codemasters_latches_follow_the_published_rule() {
        let mut image = vec![0u8; 0x10000];
        image[0x0001] = 0xA1;
        image[0x4001] = 0xB1;
        image[0x8001] = 0xC1;
        image[0xC001] = 0xD1;
        let mut c = cart("codemasters", image);
        assert_eq!(c.latches, [0, 1, 2, 2]);
        // The first window stays page 0.
        assert_eq!(c.peek(Address(0x0001)), 0xA1);
        // OUT ($0000), 4 stores bank 5 for $8000-$BFFF; the page
        // index wraps modulo 4 onto page 1, the published value+1
        // form (a plain 4 would read page 0).
        c.write(AccessWidth::Byte, Address(0x0000), 4, Tick(0));
        assert_eq!(c.latches[2], 5);
        assert_eq!(c.peek(Address(0x8001)), 0xB1);
        // OUT ($8000), 3 stores bank 3 for $4000-$7FFF.
        c.write(AccessWidth::Byte, Address(0x8000), 3, Tick(0));
        assert_eq!(c.latches[1], 3);
        assert_eq!(c.peek(Address(0x4001)), 0xD1);
        // OUT ($4000), 2 stores bank 2 for $C000-$FFFF.
        c.write(AccessWidth::Byte, Address(0x4000), 2, Tick(0));
        assert_eq!(c.latches[3], 2);
        assert_eq!(c.peek(Address(0xC001)), 0xC1);
    }

    #[test]
    fn mapper_read_guards_short_images() {
        let mut image = vec![0u8; 0x100];
        image[0x0000] = 0x77;
        let c = cart("sega", image);
        // One page: every latch wraps to page 0 and the reads past
        // the image end answer the open-bus form.
        assert_eq!(c.peek(Address(0x0000)), 0x77);
        assert_eq!(c.peek(Address(0x4000)), 0x77);
        assert_eq!(c.peek(Address(0x4100)), 0xFF);
    }

    #[test]
    fn flat_reads_wrap_across_the_whole_window() {
        let mut image = vec![0u8; 0x2000];
        image[0x0001] = 0x11;
        image[0x1FFF] = 0x22;
        let c = cart("sms", image);
        // The bus mirrors carry the same wrap: the 8-KiB image
        // answers the full window.
        assert_eq!(c.peek(Address(0x8001)), 0x11);
        assert_eq!(c.peek(Address(0xFFFF)), 0x22);
    }

    #[test]
    fn reset_restores_the_power_up_latches() {
        let mut c = cart("sega", flat_image());
        c.write(AccessWidth::Byte, Address(0x3FFE), 7, Tick(0));
        c.write(AccessWidth::Byte, Address(0x402D), 1, Tick(0));
        c.reset();
        assert_eq!(c.latches, [0, 1, 2, 2]);
        assert_eq!(c.peek(Address(0x7FFF)), 0x33);
    }

    #[test]
    fn snapshot_round_trips_and_rejects_lengths() {
        let image = flat_image();
        let mut c = cart("sega", image.clone());
        c.write(AccessWidth::Byte, Address(0x3FFE), 7, Tick(0));
        c.write(AccessWidth::Byte, Address(0x402D), 1, Tick(0));
        let snap = c.snapshot();
        assert_eq!(snap.len(), CART_SNAPSHOT_LEN);
        assert_eq!(&snap[..4], b"SGCT");
        let mut twin = cart("sms", vec![0u8; 0x100]);
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.latches, [0, 1, 7, 1]);
        assert!(twin.restore(&snap[..snap.len() - 1]).is_err());
        let mut bad = snap.clone();
        bad[0] = b'X';
        assert!(twin.restore(&bad).is_err());
        // The flat forms snapshot the sms class and zero latches.
        let flat_snap = cart("sms", flat_image()).snapshot();
        assert_eq!(flat_snap[5], 1);
        assert_eq!(flat_snap[6..10], [0u8; 4]);
    }

    /// The mini machine: the cart over the full published $0000-$FFFF
    /// window of the SMS map.
    fn cart_config() -> demu_config::model::SystemConfig {
        let device = instance("sega");
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(3_579_545, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(device.name.clone(), 0usize);
        let mut boot = BTreeMap::new();
        let mut image = vec![0u8; 0x8000];
        image[0x4001] = 0xB1;
        boot.insert(device.name.clone(), BootState::Hex { bytes: image });
        demu_config::model::SystemConfig {
            name: "sega-test".to_string(),
            triple: "sega/z80".to_string(),
            description: "the Sega cart test machine".to_string(),
            clocks,
            devices: vec![device],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "cart".to_string(),
                bus: "cpu".to_string(),
                device: DeviceName::new("cart").expect("name"),
                base: 0x0000,
                size: 0x10000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            }],
            connections: Vec::new(),
            signals: Vec::new(),
            boot,
            quirks: BTreeMap::new(),
        }
    }

    #[test]
    fn machine_mapper_latches_route_the_peek_reads() {
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("sega.cart", Box::new(|d, _| Ok(Box::new(SegaCart::new(d)))));
        let mut machine = Machine::assemble(cart_config(), &ctors).expect("assemble");
        machine.boot();
        // The $3FFE latch write lands through the bus window and the
        // $8000 window reads the wrapped page.
        assert!(machine.debug_write("cpu", Address(0x3FFE), 3));
        assert_eq!(machine.peek("cpu", Address(0x8001)), 0xB1);
        assert!(machine.debug_write("cpu", Address(0x3FFF), 1));
        assert_eq!(machine.peek("cpu", Address(0x0001)), 0xB1);
    }
}
