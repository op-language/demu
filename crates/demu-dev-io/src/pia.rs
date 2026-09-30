//! The PIA 6520 of the Atari 800.
//!
//! The register map is the chip's: `$D300` and `$D301` are the port A
//! and B data registers, `$D302` and `$D303` the PACTL and PBCTL
//! control registers. Each control register's bit 2 selects whether
//! the port address answers the data register or the data-direction
//! register; bit 7 carries the port's interrupt flag on read and
//! stays a write-enable line on write; bit 0 arms the interrupt of
//! the port's flag and bit 1 picks the edge the flag latches on. The
//! control-register writes carry the cassette motor on bit 3 of
//! PACTL and the SIO command line on bit 3 of PBCTL as output
//! latches. Data-register reads clear the port's interrupt flag.
//!
//! Port bits are active low: of PORTA, bits 7-4 are stick 1 and bits
//! 3-0 stick 0, with up/down/left/right reading zero when pressed.
//! Driven pins read the output latch, loose pins the external state.
//!
//! The interrupt pin of the 6520 is open-drain active-low in the
//! hardware; demu carries it as a level line, and the config wires
//! it in OR with the POKEY's on the CPU IRQ input.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// One port bank of the 6520: the output latch and the
/// data-direction register.
#[derive(Debug)]
struct Bank {
    output: u8,
    ddr: u8,
}

impl Bank {
    fn new() -> Self {
        Self { output: 0, ddr: 0 }
    }

    /// The pins: driven bits read the latch, loose bits the external
    /// state.
    fn pins(&self, external: u8) -> u8 {
        (self.output & self.ddr) | (external & !self.ddr)
    }
}

/// The PIA 6520 with both banks, the control pair, and interrupts.
#[derive(Debug)]
pub struct Pia {
    name: String,
    cpu_div: u64,
    banks: [Bank; 2],
    ext: [u8; 2],
    /// The PACTL and PBCTL bytes; bit 7 is set on read back only.
    ctl: [u8; 2],
    /// The level driven on the shared line; the source notifies on a
    /// change, so the line follows the flag state, not the visits.
    irq_out: bool,
    /// The per-port flag and enable bits.
    flag: [bool; 2],
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Pia {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1),
            banks: [Bank::new(), Bank::new()],
            ext: [0xFF; 2],
            ctl: [0x34, 0x3C],
            irq_out: false,
            flag: [false; 2],
            next: None,
            last_tick: None,
        }
    }

    /// Read one register per the chip decode.
    fn reg_read(&mut self, offset: u8) -> u8 {
        match offset & 0x03 {
            0 => {
                if self.ctl[0] & 0x04 != 0 {
                    self.flag[0] = false;
                    self.banks[0].pins(self.ext[0])
                } else {
                    self.banks[0].ddr
                }
            }
            1 => {
                if self.ctl[1] & 0x04 != 0 {
                    self.flag[1] = false;
                    self.banks[1].pins(self.ext[1])
                } else {
                    self.banks[1].ddr
                }
            }
            2 => self.ctl[0] | (u8::from(self.flag[0]) << 7),
            _ => self.ctl[1] | (u8::from(self.flag[1]) << 7),
        }
    }

    /// Write one register per the chip decode. Control writes store
    /// the low six bits and refresh the interrupt enables.
    fn reg_write(&mut self, offset: u8, value: u8) {
        match offset & 0x03 {
            0 => {
                if self.ctl[0] & 0x04 != 0 {
                    self.banks[0].output = value;
                } else {
                    self.banks[0].ddr = value;
                }
            }
            1 => {
                if self.ctl[1] & 0x04 != 0 {
                    self.banks[1].output = value;
                } else {
                    self.banks[1].ddr = value;
                }
            }
            2 => {
                self.ctl[0] = value & 0x3F;
            }
            _ => {
                self.ctl[1] = value & 0x3F;
            }
        }
    }

    /// True when any enabled flag presses the wired-OR line.
    fn irq_level(&self) -> bool {
        ((self.ctl[0] & 0x01 != 0) && self.flag[0]) || ((self.ctl[1] & 0x01 != 0) && self.flag[1])
    }
}

impl BusAccess for Pia {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.reg_read((address.0 & 0x03) as u8))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        self.reg_write((address.0 & 0x03) as u8, value as u8);
    }

    fn peek(&self, address: Address) -> u8 {
        let offset = (address.0 & 0x03) as u8;
        match offset {
            0 => {
                if self.ctl[0] & 0x04 != 0 {
                    self.banks[0].pins(self.ext[0])
                } else {
                    self.banks[0].ddr
                }
            }
            1 => {
                if self.ctl[1] & 0x04 != 0 {
                    self.banks[1].pins(self.ext[1])
                } else {
                    self.banks[1].ddr
                }
            }
            2 => self.ctl[0] | (u8::from(self.flag[0]) << 7),
            _ => self.ctl[1] | (u8::from(self.flag[1]) << 7),
        }
    }
}

impl InputSink for Pia {
    fn inject(&mut self, event: &str) -> bool {
        // Grammar: `p0-up` through `p1-right` press stick directions;
        // a `-release` suffix lifts the direction again.
        let form = event.trim().to_ascii_lowercase();
        let (name, release) = match form.strip_suffix("-release") {
            Some(n) => (n, true),
            None => (form.as_str(), false),
        };
        let bit = match name {
            "p0-up" => 0x01,
            "p0-down" => 0x02,
            "p0-left" => 0x04,
            "p0-right" => 0x08,
            "p1-up" => 0x10,
            "p1-down" => 0x20,
            "p1-left" => 0x40,
            "p1-right" => 0x80,
            _ => return false,
        };
        if release {
            self.ext[0] |= bit;
        } else {
            self.ext[0] &= !bit;
        }
        true
    }
}

impl Device for Pia {
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

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.banks = [Bank::new(), Bank::new()];
        self.ext = [0xFF; 2];
        self.ctl = [0x34, 0x3C];
        self.irq_out = false;
        self.flag = [false; 2];
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(b"PIA2");
        out.push(1);
        out.push(self.banks[0].output);
        out.push(self.banks[0].ddr);
        out.push(self.banks[1].output);
        out.push(self.banks[1].ddr);
        out.push(self.ext[0]);
        out.push(self.ext[1]);
        out.push(self.ctl[0]);
        out.push(self.ctl[1]);
        out.push(u8::from(self.flag[0]));
        out.push(u8::from(self.flag[1]));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 10;
        if data.len() != expected || &data[..4] != b"PIA2" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad pia chunk".into()));
        }
        let mut pos = 5usize;
        self.banks[0].output = data[pos];
        pos += 1;
        self.banks[0].ddr = data[pos];
        pos += 1;
        self.banks[1].output = data[pos];
        pos += 1;
        self.banks[1].ddr = data[pos];
        pos += 1;
        self.ext[0] = data[pos];
        pos += 1;
        self.ext[1] = data[pos];
        pos += 1;
        self.ctl[0] = data[pos];
        pos += 1;
        self.ctl[1] = data[pos];
        pos += 1;
        self.flag[0] = data[pos] != 0;
        pos += 1;
        self.flag[1] = data[pos] != 0;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        // The hardware flag latches on the edge the control bit 1
        // picks; demu carries them as level inputs from the config's
        // wiring, and a press latches regardless of the arm bit.
        if level != Level::Low {
            return;
        }
        if line == "irqa" {
            self.flag[0] = true;
        } else if line == "irqb" {
            self.flag[1] = true;
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
        let want = self.irq_level();
        if want != self.irq_out {
            self.irq_out = want;
            ctx.notify("irq", if want { Level::High } else { Level::Low }, tick);
        }
        self.next = Some(Tick(tick.0 + self.cpu_div));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pia() -> Pia {
        Pia {
            name: "pia".to_string(),
            cpu_div: 4,
            banks: [Bank::new(), Bank::new()],
            ext: [0xFF; 2],
            ctl: [0x34, 0x3C],
            irq_out: false,
            flag: [false; 2],
            next: None,
            last_tick: None,
        }
    }

    #[test]
    fn direction_selection_lives_in_control_bit() {
        let mut p = pia();
        // PACTL bit 2 clear: the port address answers the DDR.
        p.reg_write(2, 0x30); // bit 2 clear
        p.reg_write(0, 0xFF);
        assert_eq!(p.banks[0].ddr, 0xFF);
        // Bit 2 set: the port address answers the data register.
        p.reg_write(2, 0x3C); // bit 2 = 0x04 set
        p.reg_write(0, 0x37);
        assert_eq!(p.banks[0].output, 0x37);
        // Driven pins read the latch through the data read.
        assert_eq!(p.reg_read(0), 0x37);
    }

    #[test]
    fn stick_presses_are_active_low() {
        let mut p = pia();
        assert!(p.inject("p0-up"));
        let pins = p.reg_read(0);
        assert_eq!(pins & 0x01, 0, "up press reads zero");
        assert!(p.inject("p0-up-release"));
        assert_eq!(p.reg_read(0), 0xFF);
    }

    #[test]
    fn data_read_clears_flag_and_irq_follows_mask() {
        let mut p = pia();
        p.flag[0] = true;
        p.reg_write(2, 0x3D); // bit 0 arms, bit 2 selects data
        assert!(p.irq_level());
        let _ = p.reg_read(0);
        assert!(!p.flag[0], "the data read clears the flag");
        assert!(!p.irq_level());
    }

    #[test]
    fn snapshot_round_trip() {
        let mut p = pia();
        p.banks[0].output = 0x37;
        p.flag[1] = true;
        let snap = p.snapshot();
        let mut twin = pia();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
    }
}
