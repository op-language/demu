//! The bus model and the address decoder.
//!
//! A bus owns address windows. A window maps a range to one device. The
//! decoder resolves an address to a window slot in one linear scan per
//! bus; no trait objects and no allocation happen in the decode path.

use rustc_hash::FxHashMap;

/// The mirror layout of a window: repeats at a stride.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mirror {
    /// The stride in bytes.
    pub stride: u32,
    /// The number of repeats.
    pub count: u32,
}

/// The bus-conflict policy for one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// The first window in declaration order wins.
    First,
    /// Both devices answer; the AND of the bytes wins.
    And,
    /// The last window in declaration order wins.
    LastWins,
}

/// An overlay group: windows that may overlap, with per-direction
/// priority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    /// The overlay group name.
    pub name: String,
    /// The read priority; higher wins.
    pub read_priority: u32,
    /// The write priority; higher wins.
    pub write_priority: u32,
}

/// One declared window on a bus.
#[derive(Debug, Clone)]
pub struct BusWindow {
    /// The window name.
    pub name: String,
    /// The device slot in the machine's device table.
    pub device: usize,
    /// The base address.
    pub base: u32,
    /// The size in bytes.
    pub size: u32,
    /// The mirror layout, if any.
    pub mirror: Option<Mirror>,
    /// The overlay group, if any.
    pub overlay: Option<Overlay>,
    /// The bus-conflict policy.
    pub conflict: ConflictPolicy,
}

/// A bus with its address windows.
#[derive(Debug, Clone, Default)]
pub struct Bus {
    /// The bus name.
    pub name: String,
    /// The address width in bits.
    pub address_bits: u32,
    /// The parent bus whose decoder this bus borrows, if any.
    pub parent: Option<String>,
    /// The windows in declaration order.
    pub windows: Vec<BusWindow>,
    /// A fast index from a decoded page to the primary window index.
    ///
    /// Pages are `address >> 8`. The index holds the first window that
    /// covers the page; overlay resolution walks the window list.
    page_cache: FxHashMap<u32, usize>,
}

impl Bus {
    /// A bus with the given name and address width.
    pub fn new(name: &str, address_bits: u32) -> Self {
        Self {
            name: name.to_string(),
            address_bits,
            parent: None,
            windows: Vec::new(),
            page_cache: FxHashMap::default(),
        }
    }

    /// Append a window.
    pub fn add_window(&mut self, window: BusWindow) {
        self.windows.push(window);
        self.page_cache.clear();
    }

    /// The max address for the bus width.
    pub fn max_address(&self) -> u32 {
        if self.address_bits >= 32 {
            u32::MAX
        } else {
            (1u32 << self.address_bits) - 1
        }
    }

    /// Resolve an address to `(window index, translated offset)` in
    /// declaration order.
    ///
    /// The caller applies overlay priorities and conflict policies to the
    /// list of hits. Mirrors translate into the primary range before the
    /// call site sees the address; the translate form lives in
    /// [`crate::device::window_offset_of`], the one anchor
    /// implementation.
    pub fn hits(&self, address: u32) -> Vec<(usize, u32)> {
        let mut out = Vec::new();
        for (i, w) in self.windows.iter().enumerate() {
            if let Some(off) = crate::device::window_offset_of(w, address) {
                out.push((i, off));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(name: &str, device: usize, base: u32, size: u32) -> BusWindow {
        BusWindow {
            name: name.to_string(),
            device,
            base,
            size,
            mirror: None,
            overlay: None,
            conflict: ConflictPolicy::First,
        }
    }

    #[test]
    fn decode_hits_primary_range() {
        let mut bus = Bus::new("bus", 16);
        bus.add_window(window("rom", 0, 0x0000, 0x8000));
        bus.add_window(window("ram", 1, 0x8000, 0x8000));

        let hits = bus.hits(0x1234);
        assert_eq!(hits, vec![(0, 0x1234)]);
        let hits = bus.hits(0x9ABC);
        assert_eq!(hits, vec![(1, 0x1ABC)]);
    }

    #[test]
    fn mirror_translates_to_primary_offset() {
        let mut bus = Bus::new("bus", 16);
        let mut w = window("ram", 0, 0x0000, 0x0800);
        w.mirror = Some(Mirror {
            stride: 0x0800,
            count: 4,
        });
        bus.add_window(w);

        // Mirror repeat k=2 covers 0x1800-0x1FFF; 0x1834 maps to 0x34.
        let hits = bus.hits(0x1834);
        assert_eq!(hits, vec![(0, 0x0034)]);
        // The four repeats cover 0x0800 through 0x27FF; 0x2800 is outside.
        assert!(bus.hits(0x2800).is_empty());
    }

    #[test]
    fn sparse_mirror_repeats_on_the_stride_boundary() {
        let mut bus = Bus::new("bus", 24);
        let mut w = window("mem", 0, 0x000000, 0x002000);
        w.mirror = Some(Mirror {
            stride: 0x010000,
            count: 3,
        });
        bus.add_window(w);

        // Repeat k=1 covers 0x010000-0x011FFF; 0x010034 maps to 0x34.
        assert_eq!(bus.hits(0x010034), vec![(0, 0x0034)]);
        // The gap between the repeats answers nothing: the register
        // page above the first 8 KiB stays free.
        assert!(bus.hits(0x002140).is_empty());
        // Repeat k=3 covers 0x030000-0x031FFF; k=4 is out of the count.
        assert_eq!(bus.hits(0x030000), vec![(0, 0x0000)]);
        assert!(bus.hits(0x040000).is_empty());
    }

    #[test]
    fn overlay_produces_multiple_hits() {
        let mut bus = Bus::new("bus", 16);
        let mut a = window("bank0", 0, 0x8000, 0x4000);
        a.overlay = Some(Overlay {
            name: "sram".into(),
            read_priority: 0,
            write_priority: 0,
        });
        let mut b = window("bank1", 1, 0x8000, 0x4000);
        b.overlay = Some(Overlay {
            name: "sram".into(),
            read_priority: 1,
            write_priority: 1,
        });
        bus.add_window(a);
        bus.add_window(b);

        let hits = bus.hits(0x9000);
        assert_eq!(hits.len(), 2);
    }
}
