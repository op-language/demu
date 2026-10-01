//! The Ensoniq DOC (ES5503) of the Apple IIgs.
//!
//! One device answers the sound GLU registers at `$C03C-$C03F`:
//! `$C03C` = sound control (bit 7 = status readback, bit 6 = the
//! address space select, bit 5 = address auto-increment, bits 0-3 =
//! the amplitude), `$C03D` = the data port, `$C03E`/`$C03F` = the
//! 16-bit address of the byte the next `$C03D` access touches.
//!
//! The internal side: 32 oscillators of the DOC family. Each carries
//! the frequency low/high ($00-3F page form), the volume, the
//! wavetable page, the control byte (mode + halt + the interrupt
//! enable), and the wavetable size field with the accumulator
//! resolution. The sample RAM = 64 KiB internal, addressed by the
//! GLU's address-select and auto-increment bits.
//!
//! The scan model: the device walks one oscillator's phase step per
//! visit at the divider the config names (the DOC clock at
//! 28636360/14 Hz on the IIgs, so the config sets the divider 14 and
//! one visit = one DOC tick); each oscillator's frequency divisor
//! steps its phase the published way and reads one wavetable byte
//! per scan period, multiplied by the volume. The oscillator's
//! interrupt fires when a one-shot oscillator reads past the end of
//! its wavetable.
//!
//! The integer audio: the per-visit drain pushes one mixed sample at
//! the scan rate through the `AudioSource` form. The nominal output
//! rate = the published IIgs crystal over the DOC divider.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{AudioSource, Device, Snapshot, SnapshotError};

/// 32 oscillators.
const OSCS: usize = 32;

/// 64 KiB of sample RAM.
const SAMPLE_RAM: usize = 0x10000;

/// The nominal scan rate: the published 28636360-Hz IIgs crystal
/// over the DOC divider 14, truncated to a whole sample rate.
const NOMINAL_RATE: u32 = 2_045_454;

/// One oscillator slot: frequency, volume, page, control, size form.
#[derive(Debug, Clone, Copy, Default)]
struct Osc {
    /// The frequency divisor: the low and high bytes form the
    /// 14-bit half at the low nibble's form.
    freq: u16,
    /// Output multiply's 8-bit volume.
    volume: u8,
    /// The wavetable page number.
    page: u8,
    /// The control byte: bits 7-4 = the channel out, 3 = IRQ enable,
    /// 2 = mode high, 1 = mode low, 0 = halt.
    control: u8,
    /// The size byte: bits 6 = 2nd-bank extended form; bits 5-3 =
    /// the size code; bits 2-0 = the accumulator resolution.
    size_form: u8,
    /// The phase accumulator's resolution: 9..16 bits form.
    acc: u32,
    /// The current sample readback ($C0+ register group).
    sample: u8,
    /// Wavetable cursor within the page.
    cursor: u16,
}

/// The DOC with the GLU face and the oscillator bank.
#[derive(Debug)]
pub struct IigsDoc {
    name: String,
    cpu_div: u64,
    /// The 16-bit address register for the GLU byte port.
    addr: u16,
    /// The controls.
    control: u8,
    /// Auto-increment of addr after a data access.
    auto_inc: bool,
    /// The oscillator bank.
    oscs: [Osc; OSCS],
    /// The enabled oscillator count's byte.
    enable: u8,
    /// The interrupt register byte (bit 7 = IRQ pending).
    irq: u8,
    /// 64K sample RAM.
    ram: Vec<u8>,
    /// Pushed samples since the last drain; the audio source drains
    /// into the machine service per visit.
    samples: Vec<i16>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl IigsDoc {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1),
            addr: 0,
            control: 0,
            auto_inc: false,
            oscs: [Osc::default(); OSCS],
            enable: 2,
            irq: 0,
            ram: vec![0; SAMPLE_RAM],
            samples: Vec::new(),
            next: None,
            last_tick: None,
        }
    }

    /// The oscillator count: half the enable byte, minimum 1.
    fn osc_count(&self) -> usize {
        let n = usize::from(self.enable >> 1)
            .max(usize::from(self.enable & 1))
            .max(1);
        n.clamp(1, OSCS)
    }

    /// True while the mode says free-run: the control bits 2-1.
    fn osc_run(&self, n: usize) -> bool {
        let c = self.oscs[n].control;
        // Bit 0 = halt: clears running.
        c & 0x01 == 0
    }

    /// One oscillator's frequency step: the 14-bit form per scan
    /// period.
    fn osc_step(&mut self, n: usize) {
        if !self.osc_run(n) {
            return;
        }
        let osc = &mut self.oscs[n];
        // The published frequency form: (freq << the resolution
        // form's shift) added to the accumulator per scan period.
        osc.acc = osc.acc.wrapping_add((u32::from(osc.freq) << 8) | 0x100);
        // Read one wavetable byte: the sample = the RAM at the
        // page << 12 + the cursor within the page.
        let page_base = usize::from(osc.page & 0x7F) << 12;
        let slot = osc.cursor as usize;
        let byte = self.ram.get(page_base + slot).copied().unwrap_or(0);
        osc.sample = byte;
        osc.cursor = osc.cursor.wrapping_add(1);
        // One-shot: the interrupt at the cursor's wrap.
        if osc.cursor == 0 && osc.control & 0x06 != 0 {
            osc.control |= 0x01; // halt
            self.irq = 0x80 | u8::try_from(n).map(|v| v << 1).unwrap_or(0);
        }
    }

    /// One visit's walk: the scan step of the enabled oscillators.
    fn scan(&mut self) {
        for n in 0..self.osc_count() {
            self.osc_step(n);
        }
        // Push one sample per scan: the sum of the osc volumes * the
        // samples, as an i16 mix. The AudioSource drains it.
        let mixed: i32 = self
            .oscs
            .iter()
            .take(self.osc_count())
            .map(|o| (i16::from(o.sample) - 0x80) as i32 * i16::from(o.volume) as i32)
            .sum();
        let pushed = mixed.clamp(-32768, 32767) as i16;
        self.samples.push(pushed);
    }

    /// The non-mutating data read for the `peek` path: the RAM byte
    /// or the register decode at the address port.
    fn data_byte(&self) -> u8 {
        if self.control & 0x40 != 0 {
            self.ram.get(usize::from(self.addr)).copied().unwrap_or(0)
        } else {
            self.doc_register(self.addr as u8)
        }
    }
}

impl AudioSource for IigsDoc {
    fn sample_rate(&self) -> u32 {
        NOMINAL_RATE
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

impl BusAccess for IigsDoc {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        // The window decodes $C03C-$C03F with the exact-offset form.
        match address.0 & 0x0F {
            0x0C => {
                // Sound control: bit 7 mirrors the IRQ pending of
                // the oscillator register $E0.
                u32::from(self.control & !0x80) | u32::from(self.irq & 0x80 != 0) << 7
            }
            0x0D => {
                // Sound data: the RAM byte or the register decode;
                // the auto-increment moves after the read.
                let b = self.data_byte();
                if self.auto_inc {
                    self.addr = self.addr.wrapping_add(1);
                }
                u32::from(b)
            }
            0x0E => u32::from(self.addr as u8),
            _ => u32::from((self.addr >> 8) as u8),
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        match address.0 & 0x0F {
            0x0C => {
                self.control = v;
                self.auto_inc = v & 0x20 != 0;
            }
            0x0D => {
                let a = self.addr;
                if self.control & 0x40 != 0 {
                    if let Some(slot) = self.ram.get_mut(usize::from(a)) {
                        *slot = v;
                    }
                } else {
                    self.doc_set_register(a as u8, v);
                }
                if self.auto_inc {
                    self.addr = self.addr.wrapping_add(1);
                }
            }
            0x0E => self.addr = (self.addr & 0xFF00) | u16::from(v),
            _ => self.addr = (u16::from(v) << 8) | (self.addr & 0x00FF),
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 & 0x0F {
            0x0C => self.control,
            0x0D => self.data_byte(),
            0x0E => self.addr as u8,
            _ => (self.addr >> 8) as u8,
        }
    }
}

impl IigsDoc {
    /// The DOC register decode by the 8-bit register space index:
    /// the map steps per oscillator group of 32, then the two
    /// dedicated bytes at `$E0`/`$E1`.
    fn doc_register(&self, idx: u8) -> u8 {
        match idx {
            0xE0 => self.irq,
            0xE1 => self.enable,
            0x00..=0x1F => self.oscs[usize::from(idx) & 0x1F].freq as u8,
            0x20..=0x3F => (self.oscs[usize::from(idx) & 0x1F].freq >> 8) as u8,
            0x40..=0x5F => self.oscs[usize::from(idx) & 0x1F].volume,
            0x60..=0x7F => self.oscs[usize::from(idx) & 0x1F].page,
            0x80..=0x9F => self.oscs[usize::from(idx) & 0x1F].control,
            0xA0..=0xBF => self.oscs[usize::from(idx) & 0x1F].size_form,
            // The $C0-$DF group answers the byte the oscillator last
            // read; the real chip's position readback is a recorded
            // approximation.
            0xC0..=0xDF => self.oscs[usize::from(idx) & 0x1F].sample,
            _ => 0,
        }
    }

    /// The mirror of `doc_register` for the write path.
    fn doc_set_register(&mut self, idx: u8, v: u8) {
        let n = usize::from(idx) % OSCS;
        match idx {
            0xE1 => self.enable = v,
            0x00..=0x1F => self.oscs[n].freq = (self.oscs[n].freq & 0xFF00) | u16::from(v),
            0x20..=0x3F => self.oscs[n].freq = (self.oscs[n].freq & 0x00FF) | (u16::from(v) << 8),
            0x40..=0x5F => self.oscs[n].volume = v,
            0x60..=0x7F => self.oscs[n].page = v,
            0x80..=0x9F => self.oscs[n].control = v,
            0xA0..=0xBF => self.oscs[n].size_form = v,
            _ => {}
        }
    }
}

impl InputSink for IigsDoc {
    fn inject(&mut self, event: &str) -> bool {
        // Grammar: `doc <ram-slot> <byte>` pokes the sample RAM.
        let form = event.trim().to_ascii_lowercase();
        let parsed = form
            .strip_prefix("doc ")
            .and_then(|w| w.split_once(' '))
            .and_then(|(a, v)| {
                let slot = a.parse::<u32>().ok()?;
                let slot = usize::try_from(slot).ok()?;
                let value = v.parse::<u8>().ok()?;
                Some((slot, value))
            });
        match parsed {
            Some((slot, value)) if slot < self.ram.len() => {
                self.ram[slot] = value;
                true
            }
            _ => false,
        }
    }
}

impl Device for IigsDoc {
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

    fn audio(&self) -> Option<&dyn AudioSource> {
        Some(self)
    }

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.addr = 0;
        self.control = 0;
        self.auto_inc = false;
        self.oscs = [Osc::default(); OSCS];
        self.enable = 2;
        self.irq = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Hex { bytes }) = state {
            let n = bytes.len().min(self.ram.len());
            self.ram[..n].copy_from_slice(&bytes[..n]);
        }
        self.samples.clear();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(0x100);
        out.extend_from_slice(b"IIDC");
        out.push(1);
        out.extend_from_slice(&self.addr.to_le_bytes());
        out.push(self.control);
        out.push(self.enable);
        out.push(self.irq);
        for osc in &self.oscs {
            out.extend_from_slice(&osc.freq.to_le_bytes());
            out.push(osc.volume);
            out.push(osc.page);
            out.push(osc.control);
            out.push(osc.size_form);
            out.extend_from_slice(&osc.acc.to_le_bytes());
            out.push(osc.sample);
            out.extend_from_slice(&osc.cursor.to_le_bytes());
        }
        out.extend_from_slice(&self.ram);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let head = 5 + 2 + 1 + 1 + 1;
        let per_osc = 2 + 1 + 1 + 1 + 1 + 4 + 1 + 2;
        let fixed = head + per_osc * OSCS;
        if data.len() < fixed + SAMPLE_RAM || &data[..4] != b"IIDC" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad iigs doc chunk".into()));
        }
        self.addr = u16::from_le_bytes(data[5..7].try_into().expect("2 bytes"));
        self.control = data[7];
        self.auto_inc = data[7] & 0x20 != 0;
        self.enable = data[8];
        self.irq = data[9];
        let mut pos = head;
        for osc in &mut self.oscs {
            osc.freq = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            osc.volume = data[pos + 2];
            osc.page = data[pos + 3];
            osc.control = data[pos + 4];
            osc.size_form = data[pos + 5];
            osc.acc = u32::from_le_bytes(data[pos + 6..pos + 10].try_into().expect("4 bytes"));
            osc.sample = data[pos + 10];
            osc.cursor = u16::from_le_bytes(data[pos + 11..pos + 13].try_into().expect("2 bytes"));
            pos += per_osc;
        }
        self.ram.copy_from_slice(&data[pos..pos + SAMPLE_RAM]);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, _ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        self.scan();
        self.next = Some(Tick(tick.0 + self.cpu_div));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("doc").expect("name"),
            kind: demu_config::newtypes::KindName::new("iigs.doc").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    #[test]
    fn glu_registers_round_trip_the_address_and_data() {
        let mut d = IigsDoc::new(&device());
        d.write(AccessWidth::Byte, Address(0x00C03E), 0x10, Tick(0));
        d.write(AccessWidth::Byte, Address(0x00C03F), 0x20, Tick(0));
        assert_eq!(d.addr, 0x2010);
    }

    #[test]
    fn frequency_registers_round_trip() {
        let mut d = IigsDoc::new(&device());
        // Address $C03F+1: the auto-increment... simple direct: set
        // the addr, select the register space, write the freq lo.
        d.addr = 0x0001; // $01 = the oscillator 1's freq lo
        d.write(AccessWidth::Byte, Address(0x00C03D), 0x07, Tick(0));
        assert_eq!(d.oscs[1].freq, 0x07);
        // The high byte at the offset $21.
        d.addr = 0x0021;
        d.write(AccessWidth::Byte, Address(0x00C03D), 0x03, Tick(0));
        assert_eq!(d.oscs[1].freq, 0x0307);
    }

    #[test]
    fn oscillator_steps_and_halts_at_the_page_end() {
        let mut d = IigsDoc::new(&device());
        d.oscs[0].freq = 0x100;
        d.oscs[0].page = 0;
        d.oscs[0].control = 0x02; // one-shot mode
        d.ram[0] = 0xAA;
        d.scan();
        assert_eq!(d.oscs[0].sample, 0xAA);
    }

    #[test]
    fn register_groups_read_back_the_same_groups_they_write() {
        let mut d = IigsDoc::new(&device());
        d.addr = 0x0040;
        d.write(AccessWidth::Byte, Address(0x00C03D), 0x42, Tick(0)); // osc 0 volume
        d.addr = 0x0063;
        d.write(AccessWidth::Byte, Address(0x00C03D), 0x05, Tick(0)); // osc 3 page
        d.addr = 0x0084;
        d.write(AccessWidth::Byte, Address(0x00C03D), 0x0A, Tick(0)); // osc 4 control
        d.addr = 0x00A5;
        d.write(AccessWidth::Byte, Address(0x00C03D), 0x0B, Tick(0)); // osc 5 size form
        // The readback mirrors the same groups.
        d.addr = 0x0084;
        assert_eq!(d.peek(Address(0x00C03D)), 0x0A);
        d.addr = 0x00A5;
        assert_eq!(d.read(AccessWidth::Byte, Address(0x00C03D), Tick(0)), 0x0B);
        d.addr = 0x0063;
        assert_eq!(d.read(AccessWidth::Byte, Address(0x00C03D), Tick(0)), 0x05);
        // The volume group at $40.
        d.addr = 0x0040;
        assert_eq!(d.read(AccessWidth::Byte, Address(0x00C03D), Tick(0)), 0x42);
    }

    #[test]
    fn data_reads_auto_increment_when_the_control_bit_says_so() {
        let mut d = IigsDoc::new(&device());
        d.write(AccessWidth::Byte, Address(0x00C03C), 0x40 | 0x20, Tick(0));
        d.write(AccessWidth::Byte, Address(0x00C03E), 0x10, Tick(0));
        d.write(AccessWidth::Byte, Address(0x00C03F), 0x00, Tick(0));
        assert_eq!(d.read(AccessWidth::Byte, Address(0x00C03D), Tick(0)), 0);
        assert_eq!(d.addr, 0x0011, "the read moved the address");
        // Without the bit the address stands still.
        d.write(AccessWidth::Byte, Address(0x00C03C), 0x40, Tick(0));
        d.addr = 0x0020;
        d.read(AccessWidth::Byte, Address(0x00C03D), Tick(0));
        assert_eq!(d.addr, 0x0020);
    }

    #[test]
    fn oneshot_halt_sets_the_irq_and_status_bit() {
        let mut d = IigsDoc::new(&device());
        d.oscs[0].freq = 0x100;
        d.oscs[0].page = 0;
        d.oscs[0].control = 0x0A; // IRQ enable + one-shot mode, halt clear
        d.oscs[0].cursor = u16::MAX;
        d.oscs[0].acc = 0;
        // The step at the cursor's last slot wraps the cursor and
        // raises the one-shot interrupt.
        d.scan();
        assert_eq!(d.oscs[0].control & 0x01, 0x01, "the one-shot halted");
        assert_eq!(d.irq & 0x80, 0x80, "the IRQ bit set");
        // The sound control readback shows it.
        let status = d.read(AccessWidth::Byte, Address(0x00C03C), Tick(0));
        assert_eq!(status & 0x80, 0x80);
    }

    #[test]
    fn audio_source_drains_the_scan_mix() {
        let mut d = IigsDoc::new(&device());
        d.oscs[0].freq = 0x100;
        d.oscs[0].volume = 0xFF;
        d.ram[0] = 0xFF;
        assert_eq!(d.sample_rate(), NOMINAL_RATE);
        d.scan();
        assert_eq!(d.samples.len(), 1);
        // The byte 0xFF mixed at full volume: (255 - 128) * 255.
        let out = d.take_samples();
        assert_eq!(out.first().copied(), Some(32385));
        assert!(d.take_samples().is_empty(), "the drain empties the queue");
    }

    #[test]
    fn injected_sample_ram_visible() {
        let mut d = IigsDoc::new(&device());
        assert!(d.inject("doc 16 5"));
        assert_eq!(d.ram[16], 5);
        // An unknown event form answers false.
        assert!(!d.inject("doc 99999999 5"));
        assert!(!d.inject("joypad 4"));
    }

    #[test]
    fn snapshot_round_trip() {
        let mut d = IigsDoc::new(&device());
        d.addr = 0x1234;
        d.oscs[2].freq = 0x1234;
        d.ram[7] = 0x42;
        let snap = d.snapshot();
        let mut twin = IigsDoc::new(&device());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
    }
}
