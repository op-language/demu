//! Device traits and the memory-access model.
//!
//! Four traits define device behavior: [`Device`] for lifecycle, `BusAccess`
//! for memory-mapped components, `MappedDevice` for address-space
//! ownership, and [`crate::signal::SignalLine`] for wiring.
//!
//! The scheduler hands every device visit a [`MachineCtx`]. The context
//! routes bus accesses through the address decoder, delivers signal-line
//! edges over declared connections, and collects trigger events for the
//! run loop.

use std::collections::BTreeMap;

use demu_config::model::BootState;

use crate::bus::Bus;
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

/// One composed display frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The pixel width.
    pub width: u32,
    /// The pixel height.
    pub height: u32,
    /// The pixels as RGB888, three bytes per pixel, row-major.
    pub pixels: Vec<u8>,
}

/// The video layer dump kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoLayer {
    /// The raw video memory.
    Vram,
    /// The tile map.
    NameTable,
    /// The object attribute memory.
    Oam,
    /// The palette memory.
    Palette,
    /// The composed framebuffer.
    Framebuffer,
}

/// A video device exposes the composed framebuffer and layer dumps.
pub trait VideoSource {
    /// The composed frame, when the device has produced one.
    fn frame(&self) -> Option<&Frame>;
    /// The dump of one layer.
    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>>;
    /// The number of complete frames since boot.
    fn frames(&self) -> u64;
}

/// A CPU exposes its registers for debug commands.
pub trait CpuDebug {
    /// The register values by name.
    fn registers(&self) -> BTreeMap<String, u64>;
    /// Set one register by name. Returns false for an unknown name.
    fn set_register(&mut self, name: &str, value: u64) -> bool;
}

/// A device that accepts injected input events.
pub trait InputSink {
    /// Apply one event. The event grammar is the device family's own.
    /// Returns false when the event is not recognized.
    fn inject(&mut self, event: &str) -> bool;
}

/// An audio device produces mixed samples.
pub trait AudioSource {
    /// The output sample rate.
    fn sample_rate(&self) -> u32;
    /// Drain the accumulated samples.
    fn take_samples(&mut self) -> Vec<i16>;
}

/// Every simulated component implements `Device`.
pub trait Device {
    /// The stable device name from the configuration.
    fn name(&self) -> &str;

    /// The device as `Any`, for shell-side downcasting in debug commands.
    fn as_any(&self) -> &dyn std::any::Any;

    /// The device as a mutable `Any`, for shell-side downcasting.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// The device as a bus component, when it is memory-mapped.
    fn bus(&self) -> Option<&dyn BusAccess> {
        None
    }

    /// The device as a mutable bus component.
    fn bus_mut(&mut self) -> Option<&mut dyn BusAccess> {
        None
    }

    /// The device as a video source, when it produces a display.
    fn video(&self) -> Option<&dyn VideoSource> {
        None
    }

    /// The device as a CPU, when it exposes debug registers.
    fn cpu(&self) -> Option<&dyn CpuDebug> {
        None
    }

    /// The device as a mutable CPU.
    fn cpu_mut(&mut self) -> Option<&mut dyn CpuDebug> {
        None
    }

    /// The device as an input sink, when it accepts injected input.
    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        None
    }

    /// The device as an audio source, when it produces samples.
    fn audio(&self) -> Option<&dyn AudioSource> {
        None
    }

    /// Restore power-on state without the configured boot data.
    fn reset(&mut self);

    /// Apply the configured boot state.
    fn boot(&mut self, state: Option<&BootState>);

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

    /// The scheduler visited the device at `tick`; the context routes bus
    /// accesses and notifications.
    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx);
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

    /// True while the device drives its windows at `address`.
    ///
    /// The address decoder consults this hook for every candidate
    /// window. A device that returns false (a banked boot ROM after
    /// unmap, for example) falls out of the decode, and the next
    /// candidate window answers instead.
    fn decodes(&self, address: crate::frequency::Address) -> bool {
        let _ = address;
        true
    }

    /// The borrow decision of the access at `address`.
    ///
    /// Most devices answer locally. A device whose window borrows the
    /// address decoder of another bus returns [`Borrow::Bridge`]: the
    /// context re-decodes the access on the borrowed bus at the
    /// translated address and the answering device there serves it,
    /// exactly like the design's DMA-parented bus form for a device
    /// that composes a cross-bus address itself.
    fn borrow(&self, address: crate::frequency::Address) -> Borrow {
        let _ = address;
        Borrow::Local
    }
}

/// The borrow decision of one target device for one access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Borrow {
    /// The device answers the access at the address itself.
    Local,
    /// The device composes a translated address on another bus; the
    /// context re-decodes the access there and that device answers.
    Bridge {
        /// The borrowed bus name.
        bus: String,
        /// The translated address on the borrowed bus.
        address: u32,
    },
}

/// The deepest borrow hop the context follows for one access.
///
/// A chain this deep means the machine's window wiring loops back on
/// itself; the access answers nothing after that point.
pub const MAX_BORROW_DEPTH: u8 = 4;

/// The routed-write hop: the target bus and the transfer form, plus
/// the entry address the trigger event carries.
struct WriteHop<'x> {
    bus_name: &'x str,
    width: AccessWidth,
    value: u32,
    tick: Tick,
    entry: crate::frequency::Address,
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
    ///
    /// A mirror repeat `k` (1 through `count`) sits at
    /// `base + k*stride` and carries the first `size` bytes of that
    /// stride period; the primary range itself is repeat 0. The
    /// repeat lands on the stride boundary so bank layouts repeat on
    /// their period, not flush after the primary range.
    pub fn mirror_hit(&self, address: u32) -> bool {
        let Some(m) = self.mirror else {
            return false;
        };
        let stride = u64::from(m.stride);
        if stride == 0 {
            return false;
        }
        let base = u64::from(self.base);
        let addr = u64::from(address);
        let primary_end = base + u64::from(self.size);
        if addr < primary_end {
            return false;
        }
        let rel = addr - base;
        let k = rel / stride;
        let off = rel % stride;
        k >= 1 && k <= u64::from(m.count) && off < u64::from(self.size)
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

/// A declared connection route between two device lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The source device slot.
    pub from: usize,
    /// The line the source drives.
    pub from_line: String,
    /// The target device slot.
    pub to: usize,
    /// The line the target receives.
    pub to_line: String,
}

/// One resolved decode candidate: `(device slot, offset inside window)`.
pub type Decode = (usize, u32);

/// One decode candidate with its priority ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DecodeHit {
    /// Higher priority answers first; ties keep declaration order.
    pub priority: u32,
    /// The window index on the bus.
    pub window: usize,
    /// The device slot.
    pub device: usize,
    /// The offset inside the window.
    pub offset: u32,
}

/// The bus table with its name index, shared by the visit context.
pub struct BusBank<'a> {
    /// The buses in declaration order.
    pub buses: &'a mut [Bus],
    /// The bus index by name.
    pub index: &'a rustc_hash::FxHashMap<String, usize>,
}

/// The per-dispatch drain buffers, shared by the visit context.
pub struct EventBuffers<'a> {
    /// The trigger-event drain.
    pub events: &'a mut Vec<crate::triggers::TriggerEvent>,
    /// The dirty slot re-seed list.
    pub dirty: &'a mut Vec<usize>,
}

/// The device table split around the calling slot.
pub struct DeviceSlices<'a> {
    /// Devices before the calling slot.
    pub before: &'a mut [Box<dyn Device>],
    /// Devices after the calling slot.
    pub after: &'a mut [Box<dyn Device>],
    /// The absolute slot of the calling device.
    pub caller_slot: usize,
}

/// The per-visit execution context.
///
/// The machine builds one context per device visit. The context routes
/// bus accesses through the address decoder, forwards declared
/// connection edges, and collects events for the run loop. The calling
/// device is excluded from bus dispatch: a device that must answer its
/// own addresses handles them before the context call.
pub struct MachineCtx<'a> {
    /// Devices before the calling slot.
    before: &'a mut [Box<dyn Device>],
    /// Devices after the calling slot.
    after: &'a mut [Box<dyn Device>],
    /// The absolute slot of the calling device.
    caller_slot: usize,
    buses: &'a mut [Bus],
    bus_index: &'a rustc_hash::FxHashMap<String, usize>,
    routes: &'a [Route],
    /// Scratch buffer for decode candidates, reused per call.
    scratch: &'a mut Vec<DecodeHit>,
    events: &'a mut Vec<crate::triggers::TriggerEvent>,
    /// The slots the visit marked for a re-seed.
    dirty: &'a mut Vec<usize>,
    emit: bool,
}

impl<'a> MachineCtx<'a> {
    /// Build a context around the calling slot.
    ///
    /// # Panics
    ///
    /// Panics when the caller slot does not exist; the machine always
    /// dispatches an existing slot.
    pub fn new(
        slices: DeviceSlices<'a>,
        BusBank { buses, index }: BusBank<'a>,
        routes: &'a [Route],
        scratch: &'a mut Vec<DecodeHit>,
        EventBuffers { events, dirty }: EventBuffers<'a>,
        emit: bool,
    ) -> Self {
        let DeviceSlices {
            before,
            after,
            caller_slot,
        } = slices;
        Self {
            before,
            after,
            caller_slot,
            buses,
            bus_index: index,
            routes,
            scratch,
            events,
            dirty,
            emit,
        }
    }

    /// A device by slot as a shared reference, excluding the caller.
    pub fn device(&self, slot: usize) -> Option<&dyn Device> {
        if slot == self.caller_slot {
            None
        } else if slot < self.caller_slot {
            match self.before.get(slot) {
                Some(device) => Some(device.as_ref() as &dyn Device),
                None => None,
            }
        } else {
            match self.after.get(slot - self.caller_slot - 1) {
                Some(device) => Some(device.as_ref() as &dyn Device),
                None => None,
            }
        }
    }

    /// A device by slot as a mutable reference, excluding the caller.
    pub fn device_mut(&mut self, slot: usize) -> Option<&mut dyn Device> {
        if slot == self.caller_slot {
            None
        } else if slot < self.caller_slot {
            match self.before.get_mut(slot) {
                Some(device) => Some(device.as_mut() as &mut dyn Device),
                None => None,
            }
        } else {
            match self.after.get_mut(slot - self.caller_slot - 1) {
                Some(device) => Some(device.as_mut() as &mut dyn Device),
                None => None,
            }
        }
    }

    fn bus_slot(&self, bus_name: &str) -> Option<usize> {
        self.bus_index.get(bus_name).copied()
    }

    /// Decode an address into candidates in priority order.
    ///
    /// Higher priority answers first; ties keep declaration order.
    /// Overlay groups carry per-direction priorities; plain windows use
    /// priority zero.
    fn decode(&mut self, bus_idx: usize, address: u32, write: bool) {
        let bus = &self.buses[bus_idx];
        self.scratch.clear();
        for (i, w) in bus.windows.iter().enumerate() {
            let Some(offset) = window_offset_of(w, address) else {
                continue;
            };
            let prio = match (&w.overlay, write) {
                (Some(o), true) => o.write_priority,
                (Some(o), false) => o.read_priority,
                (None, _) => 0,
            };
            self.scratch.push(DecodeHit {
                priority: u32::MAX - prio,
                window: i,
                device: w.device,
                offset,
            });
        }
        self.scratch.sort_unstable();
    }

    /// Pick the device that answers an address on a bus.
    ///
    /// The candidates are the decoded windows in priority order. A
    /// window whose device is not driving (a banked boot ROM after
    /// unmap) falls out, and the next candidate wins. A window owned by
    /// the calling device falls out too: a device handles its own
    /// addresses before it calls the context.
    pub fn pick(
        &mut self,
        bus_name: &str,
        address: crate::frequency::Address,
        write: bool,
    ) -> Option<Decode> {
        let bus_idx = self.bus_slot(bus_name)?;
        self.decode(bus_idx, address.0, write);
        for hit in self.scratch.iter().copied() {
            if hit.device == self.caller_slot {
                continue;
            }
            let Some(device) = self.device(hit.device) else {
                continue;
            };
            let Some(bus) = device.bus() else {
                continue;
            };
            if bus.decodes(address) {
                // The target may change its own schedule during the
                // access; the run loop re-seeds it after the dispatch.
                self.dirty.push(hit.device);
                return Some((hit.device, hit.offset));
            }
        }
        None
    }

    /// A bus-mediated read on the named bus.
    pub fn read(
        &mut self,
        bus_name: &str,
        width: AccessWidth,
        address: crate::frequency::Address,
        tick: Tick,
    ) -> u32 {
        self.emit(crate::triggers::TriggerEvent::Read(address));
        self.route_read(bus_name, width, address, tick, 0)
    }

    fn route_read(
        &mut self,
        bus_name: &str,
        width: AccessWidth,
        address: crate::frequency::Address,
        tick: Tick,
        depth: u8,
    ) -> u32 {
        let Some((slot, _offset)) = self.pick(bus_name, address, false) else {
            return 0xFF;
        };
        let borrow = {
            let Some(device) = self.device(slot) else {
                return 0xFF;
            };
            let Some(bus) = device.bus() else {
                return 0xFF;
            };
            bus.borrow(address)
        };
        if let Borrow::Bridge { bus, address } = borrow {
            if depth >= MAX_BORROW_DEPTH {
                return 0xFF;
            }
            return self.route_read(
                &bus,
                width,
                crate::frequency::Address(address),
                tick,
                depth + 1,
            );
        }
        let Some(device) = self.device_mut(slot) else {
            return 0xFF;
        };
        let Some(bus) = device.bus_mut() else {
            return 0xFF;
        };
        bus.read(width, address, tick)
    }

    /// A bus-mediated write on the named bus.
    pub fn write(
        &mut self,
        bus_name: &str,
        width: AccessWidth,
        address: crate::frequency::Address,
        value: u32,
        tick: Tick,
    ) {
        // The write trigger event names the entry address like the
        // read one does: triggers watch the address the accessing CPU
        // composed, not the address a bridge translates it to.
        let hop = WriteHop {
            bus_name,
            width,
            value,
            tick,
            entry: address,
        };
        self.route_write(&hop, address, 0)
    }

    fn route_write(&mut self, hop: &WriteHop, address: crate::frequency::Address, depth: u8) {
        let Some((slot, _offset)) = self.pick(hop.bus_name, address, true) else {
            return;
        };
        let borrow = {
            let Some(device) = self.device(slot) else {
                return;
            };
            let Some(bus) = device.bus() else {
                return;
            };
            bus.borrow(address)
        };
        match borrow {
            Borrow::Bridge { bus: _, address } if depth < MAX_BORROW_DEPTH => {
                self.route_write(hop, crate::frequency::Address(address), depth + 1);
            }
            // A borrow loop or an exhausted depth: the write lands
            // nowhere.
            Borrow::Bridge { .. } => {}
            Borrow::Local => {
                self.emit(crate::triggers::TriggerEvent::Write(hop.entry));
                let Some(device) = self.device_mut(slot) else {
                    return;
                };
                let Some(bus) = device.bus_mut() else {
                    return;
                };
                bus.write(hop.width, address, hop.value, hop.tick);
            }
        }
    }

    /// A side-effect-free read on the named bus.
    pub fn peek(&mut self, bus_name: &str, address: crate::frequency::Address) -> u8 {
        self.route_peek(bus_name, address, 0)
    }

    fn route_peek(&mut self, bus_name: &str, address: crate::frequency::Address, depth: u8) -> u8 {
        let Some((slot, _offset)) = self.pick(bus_name, address, false) else {
            return 0xFF;
        };
        let borrow = {
            let Some(device) = self.device(slot) else {
                return 0xFF;
            };
            let Some(bus) = device.bus() else {
                return 0xFF;
            };
            bus.borrow(address)
        };
        if let Borrow::Bridge { bus, address } = borrow {
            if depth >= MAX_BORROW_DEPTH {
                return 0xFF;
            }
            return self.route_peek(&bus, crate::frequency::Address(address), depth + 1);
        }
        let Some(device) = self.device(slot) else {
            return 0xFF;
        };
        device.bus().map(|b| b.peek(address)).unwrap_or(0xFF)
    }

    /// Deliver a signal-line edge from the calling device.
    ///
    /// The declared connections whose source is the caller and whose
    /// line matches receive the edge. `tick` carries the caller's
    /// domain tick; same-frequency domains deliver it unchanged.
    pub fn notify(&mut self, line: &str, level: Level, tick: Tick) {
        let routes: Vec<(usize, String)> = self
            .routes
            .iter()
            .filter(|r| r.from == self.caller_slot && r.from_line == line)
            .map(|r| (r.to, r.to_line.clone()))
            .collect();
        for (to, to_line) in routes {
            self.notify_target(&to_line, level, tick, to);
        }
    }

    fn notify_target(&mut self, line: &str, level: Level, tick: Tick, slot: usize) {
        self.dirty.push(slot);
        let Some(device) = self.device_mut(slot) else {
            return;
        };
        device.notify_line(line, level, tick);
        // The notified device may need scheduling now; the run loop
        // re-seeds every slot this dispatch marked dirty.
        let name = device.name().to_string();
        self.emit(crate::triggers::TriggerEvent::Line {
            device: name,
            line: line.to_string(),
            level,
        });
    }

    /// Report a runtime event to the run loop.
    pub fn emit(&mut self, event: crate::triggers::TriggerEvent) {
        if self.emit {
            self.events.push(event);
        }
    }
}

/// The offset of `address` inside the window's primary range or a
/// mirror, or `None` when the window does not cover the address.
///
/// A mirror repeat `k` (1 through `count`) sits at
/// `base + k*stride` and carries the first `size` bytes of the period,
/// so the primary range is repeat 0 and later repeats land on their
/// stride boundary.
pub fn window_offset_of(w: &crate::bus::BusWindow, address: u32) -> Option<u32> {
    if address >= w.base && address < w.base.saturating_add(w.size) {
        return Some(address - w.base);
    }
    let m = w.mirror?;
    let stride = u64::from(m.stride);
    if stride == 0 {
        return None;
    }
    let base = u64::from(w.base);
    let addr = u64::from(address);
    let primary_end = base + u64::from(w.size);
    if addr < primary_end {
        return None;
    }
    let rel = addr - base;
    let k = rel / stride;
    let off = rel % stride;
    if k >= 1 && k <= u64::from(m.count) && off < u64::from(w.size) {
        Some(off as u32)
    } else {
        None
    }
}

#[cfg(test)]
mod window_offset_tests {
    use super::window_offset_of;

    #[test]
    fn sparse_mirror_repeats_on_the_stride_boundary() {
        let mut w = crate::bus::BusWindow {
            name: "mem".to_string(),
            device: 0,
            base: 0x0000,
            size: 0x002000,
            mirror: Some(crate::bus::Mirror {
                stride: 0x010000,
                count: 63,
            }),
            overlay: None,
            conflict: crate::bus::ConflictPolicy::First,
        };
        // The primary range answers.
        assert_eq!(window_offset_of(&w, 0x0140), Some(0x0140));
        // The gap after the primary answers nothing: the register
        // page at $2140 stays free for another window.
        assert_eq!(window_offset_of(&w, 0x2140), None);
        // Repeat k=1 covers 0x010000-0x011FFF and maps into the
        // primary offset.
        assert_eq!(window_offset_of(&w, 0x010034), Some(0x0034));
        // Past the count the repeats stop.
        w.mirror = w.mirror.map(|m| crate::bus::Mirror {
            stride: m.stride,
            count: 1,
        });
        assert_eq!(window_offset_of(&w, 0x020000), None);
    }
}
