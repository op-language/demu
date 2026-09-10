//! Device traits and the memory-access model.
//!
//! Four traits define device behavior: [`Device`] for lifecycle, `BusAccess`
//! for memory-mapped components, `MappedDevice` for address-space
//! ownership, and [`crate::signal::SignalLine`] for wiring.

use crate::frequency::Tick;
use crate::signal::Level;

/// The width of one bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessWidth {
    /// One byte.
    Byte,
    /// Two bytes, little-endian as the machine declares.
    Word,
    /// Four bytes.
    Dword,
}

impl AccessWidth {
    /// The width in bytes.
    pub fn bytes(&self) -> u32 {
        match self {
            AccessWidth::Byte => 1,
            AccessWidth::Word => 2,
            AccessWidth::Dword => 4,
        }
    }
}

/// A device state snapshot as raw bytes.
pub type Snapshot = Vec<u8>;

/// Every simulated component implements `Device`.
pub trait Device {
    /// The stable device name from the configuration.
    fn name(&self) -> &str;

    /// Restore power-on state without the configured boot data.
    fn reset(&mut self);

    /// Apply the configured boot state.
    fn boot(&mut self);

    /// Write the full device state.
    fn snapshot(&self) -> Snapshot;

    /// Read a snapshot written by the same device version.
    ///
    /// # Errors
    ///
    /// Returns an error when the chunk does not match this device's
    /// expectations.
    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError>;

    /// Receive a signal-line edge from a connected device.
    ///
    /// `tick` is the receiving device's domain tick at the edge.
    fn notify_line(&mut self, line: &str, level: Level, tick: Tick);

    /// The next domain tick at which this device needs a scheduler visit,
    /// or `None` when the device is idle.
    fn next_event(&self) -> Option<Tick> {
        None
    }

    /// The scheduler visited the device at `tick`.
    fn tick(&mut self, _tick: Tick) {}
}

/// A restore failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// The chunk is too short, has a bad tag, or describes another device.
    #[error("snapshot mismatch: {0}")]
    Mismatch(String),
}

/// Memory-mapped components implement `BusAccess`.
pub trait BusAccess {
    /// A read at `address` on the device's domain at `tick`.
    fn read(&mut self, width: AccessWidth, address: crate::frequency::Address, tick: Tick) -> u32;

    /// A write at `address` on the device's domain at `tick`.
    fn write(
        &mut self,
        width: AccessWidth,
        address: crate::frequency::Address,
        value: u32,
        tick: Tick,
    );

    /// A side-effect-free read for debug commands.
    fn peek(&self, address: crate::frequency::Address) -> u8;
}

/// A resolved address window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The window name from the configuration.
    pub name: String,
    /// The base address.
    pub base: u32,
    /// The size in bytes.
    pub size: u32,
    /// The mirror layout, if any.
    pub mirror: Option<crate::bus::Mirror>,
}

impl Window {
    /// The inclusive last address of the primary window.
    pub fn end(&self) -> u32 {
        self.base + self.size - 1
    }

    /// True when the address falls inside the primary window or a mirror.
    pub fn contains(&self, address: u32) -> bool {
        if (address >= self.base && address <= self.end()) || self.mirror_hit(address) {
            return true;
        }
        false
    }

    /// Translate an address inside a mirror to the primary range.
    pub fn mirror_hit(&self, address: u32) -> bool {
        match self.mirror {
            Some(m) => {
                let stride = u64::from(m.stride);
                let primary_end = u64::from(self.end());
                let addr = u64::from(address);
                if addr > primary_end {
                    let offset = addr - primary_end - 1;
                    stride > 0
                        && offset % stride < u64::from(self.size)
                        && offset / stride < u64::from(m.count)
                } else {
                    false
                }
            }
            None => false,
        }
    }
}

/// A device that occupies address space implements `MappedDevice`.
pub trait MappedDevice {
    /// The address windows the device owns.
    fn windows(&self) -> &[Window];

    /// The extra cycles the bus must spend at `address`.
    fn wait_states(&self, address: u32) -> u32 {
        let _ = address;
        0
    }
}
