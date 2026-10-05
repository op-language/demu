//! Machine assembly and the run loop.
//!
//! A `Machine` owns the devices, the buses, and the scheduler. The smoke
//! machine from the bundled configuration demonstrates the assembly: a
//! clock domain, a ROM, a RAM, and a CPU shim. The run loop advances
//! domains in exact cross-domain order, dispatches events through the
//! per-visit context, and stops at sync points or when a run hook says
//! stop.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};

use rustc_hash::FxHashMap;

use crate::bus::{Bus, BusWindow, ConflictPolicy};
use crate::device::{
    Borrow, BusBank, DecodeHit, Device, DeviceSlices, EventBuffers, MAX_BORROW_DEPTH, MachineCtx,
    Route, SnapshotError, window_offset_of,
};
use crate::frequency::{Frequency, Tick};
use crate::savestate::{
    DeviceChunk, DomainState, LoadError, SaveError, SaveState, read_save, write_save,
};
use crate::scheduler::Scheduler;
use crate::triggers::TriggerEvent;

/// A machine assembly failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MachineError {
    /// The configuration names an entity the assembly cannot wire.
    #[error("machine assembly failed: {0}")]
    Assembly(String),

    /// A device failed to restore a snapshot.
    #[error("device {0}: {1}")]
    Restore(String, #[source] SnapshotError),
}

/// A constructor for one device kind.
pub type DeviceCtor =
    dyn Fn(&DeviceInstance, &SystemConfig) -> Result<Box<dyn Device>, MachineError> + Send + Sync;

/// The device constructor registry.
///
/// Device-family crates register constructors under their stable kind
/// key at assembly time. The configuration validation checked the kind
/// names against the static kind registry before assembly; the registry
/// supplies the compiled constructors.
#[derive(Default)]
pub struct DeviceCtors {
    ctors: FxHashMap<String, Box<DeviceCtor>>,
}

impl std::fmt::Debug for DeviceCtors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.ctors.keys()).finish()
    }
}

impl DeviceCtors {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one constructor under its kind key.
    pub fn register(&mut self, kind: &str, ctor: Box<DeviceCtor>) {
        self.ctors.insert(kind.to_string(), ctor);
    }

    /// Look up one constructor.
    pub fn get(&self, kind: &str) -> Option<&DeviceCtor> {
        self.ctors.get(kind).map(std::convert::AsRef::as_ref)
    }

    /// The registered kind keys, sorted.
    pub fn kinds(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.ctors.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// The constructors for the smoke device family.
pub fn smoke_ctors() -> DeviceCtors {
    let mut ctors = DeviceCtors::new();
    ctors.register(
        "smoke.cpu",
        Box::new(|d, _| Ok(Box::new(devices::SmokeCpu::new(d)))),
    );
    ctors.register(
        "smoke.ram",
        Box::new(|d, _| Ok(Box::new(devices::SmokeRam::new(d)))),
    );
    ctors.register(
        "smoke.rom",
        Box::new(|d, _| Ok(Box::new(devices::SmokeRom::new(d)))),
    );
    ctors.register(
        "smoke.bridge",
        Box::new(|d, _| Ok(Box::new(devices::SmokeBridge::new(d)))),
    );
    ctors
}

/// The run hook: receives every runtime event and returns true to stop.
pub type RunHook<'a> = &'a mut dyn FnMut(&TriggerEvent) -> bool;

/// The machine: devices, buses, and the scheduler in one run loop.
pub struct Machine {
    /// The resolved configuration.
    pub config: SystemConfig,
    /// The devices in configuration declaration order.
    pub devices: Vec<Box<dyn Device>>,
    /// The buses in configuration declaration order.
    pub buses: Vec<Bus>,
    /// The scheduler with one domain per configured clock.
    pub scheduler: Scheduler,
    /// The connection routes resolved to device slots.
    pub routes: Vec<Route>,
    /// The bus index by name.
    bus_index: FxHashMap<String, usize>,
    /// The configured domain index per device slot.
    slot_domain: Vec<usize>,
    /// Scratch decode buffer, reused across accesses.
    scratch: Vec<DecodeHit>,
    /// Event drain buffer, reused across dispatches.
    events: Vec<TriggerEvent>,
    /// The dirty slots of the last dispatch.
    dirty: Vec<usize>,
    /// Set when a run hook reported a stop; the run loop drains it.
    step_stopped: bool,
}

impl Machine {
    /// Assemble a machine from a resolved configuration with the smoke
    /// device constructors.
    pub fn new(config: SystemConfig) -> Result<Self, MachineError> {
        Self::assemble(config, &smoke_ctors())
    }

    /// Assemble a machine from a resolved configuration and a device
    /// constructor registry.
    ///
    /// The configuration selects kinds by name; the registry supplies
    /// the compiled constructors. Buses and windows wire up from the
    /// configuration; connections become routed signal edges.
    pub fn assemble(config: SystemConfig, ctors: &DeviceCtors) -> Result<Self, MachineError> {
        let domains: Vec<(String, Frequency)> = config
            .clocks
            .values()
            .map(|c| {
                (
                    c.name.clone(),
                    Frequency::new(c.frequency.num(), c.frequency.den())
                        .expect("config validation checked the frequency"),
                )
            })
            .collect();
        let scheduler = Scheduler::new(domains);
        let mut bus_index = FxHashMap::default();
        let mut buses = Vec::new();
        for (i, bus) in config.buses.iter().enumerate() {
            bus_index.insert(bus.name.clone(), i);
            let mut b = Bus::new(&bus.name, bus.address_bits);
            b.parent = bus.parent.clone();
            buses.push(b);
        }
        // Fill windows with device slot lookups.
        for (slot, device) in config.devices.iter().enumerate() {
            for window in config.windows.iter().filter(|w| w.device == device.name) {
                let Some(bus) = buses.iter_mut().find(|b| b.name == window.bus) else {
                    return Err(MachineError::Assembly(format!(
                        "window '{}' names unknown bus '{}'",
                        window.name, window.bus
                    )));
                };
                bus.add_window(BusWindow {
                    name: window.name.clone(),
                    device: slot,
                    base: window.base,
                    size: window.size,
                    mirror: window.mirror.map(|m| crate::bus::Mirror {
                        stride: m.stride,
                        count: m.count,
                    }),
                    overlay: window.overlay.clone().map(|name| {
                        let read_priority = window.overlay_read_priority.unwrap_or(0);
                        let write_priority = window.overlay_write_priority.unwrap_or(0);
                        crate::bus::Overlay {
                            name,
                            read_priority,
                            write_priority,
                        }
                    }),
                    conflict: window
                        .conflict
                        .map(conflict_of)
                        .unwrap_or(ConflictPolicy::First),
                });
            }
        }
        // Resolve connection endpoints to routed slots. A bidirectional
        // connection delivers edges in both directions.
        let mut routes = Vec::new();
        for conn in &config.connections {
            let from = resolve_slot(&config, conn.from.device().as_ref())?;
            let to = resolve_slot(&config, conn.to.device().as_ref())?;
            routes.push(Route {
                from,
                from_line: conn.from.line().as_ref().to_string(),
                to,
                to_line: conn.to.line().as_ref().to_string(),
            });
            if conn.direction == demu_config::model::ConnectionDirection::Bidir {
                routes.push(Route {
                    from: to,
                    from_line: conn.to.line().as_ref().to_string(),
                    to: from,
                    to_line: conn.from.line().as_ref().to_string(),
                });
            }
        }
        let devices: Vec<Box<dyn Device>> = config
            .devices
            .iter()
            .map(|d| build_device(d, &config, ctors))
            .collect::<Result<Vec<_>, _>>()?;
        let slot_domain = config
            .devices
            .iter()
            .map(|d| scheduler.domain(&d.domain).unwrap_or(0))
            .collect();
        Ok(Self {
            config,
            devices,
            buses,
            scheduler,
            routes,
            bus_index,
            slot_domain,
            scratch: Vec::new(),
            events: Vec::new(),
            dirty: Vec::new(),
            step_stopped: false,
        })
    }

    /// The machine identity hash: the triple and the device list.
    pub fn machine_hash(&self) -> u64 {
        let mut hash: u64 = 0xCBF2_9CE4_8422_2325; // FNV offset basis
        for b in self.config.triple.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01B9);
        }
        for d in &self.config.devices {
            hash ^= 0xFF;
            hash = hash.wrapping_mul(0x0000_0100_0000_01B9);
            for b in d.kind.as_str().as_bytes() {
                hash ^= u64::from(*b);
                hash = hash.wrapping_mul(0x0000_0100_0000_01B9);
            }
        }
        hash
    }

    /// Boot every device to its configured boot state.
    pub fn boot(&mut self) {
        for (slot, device) in self.devices.iter_mut().enumerate() {
            let name = &self.config.devices[slot].name;
            let state = self.config.boot.get(name);
            device.boot(state);
        }
    }

    /// Replace the boot state of one device before boot.
    ///
    /// The CLI overrides the cartridge ROM path this way.
    ///
    /// # Errors
    ///
    /// Returns an error when the device name is unknown.
    pub fn override_boot(
        &mut self,
        device_name: &str,
        state: BootState,
    ) -> Result<(), MachineError> {
        let slot = resolve_slot(&self.config, device_name)?;
        let name = self.config.devices[slot].name.clone();
        self.config.boot.insert(name, state);
        Ok(())
    }

    /// The slot of one device by name.
    pub fn device_slot(&self, name: &str) -> Option<usize> {
        resolve_slot(&self.config, name).ok()
    }

    /// A device reference by name.
    pub fn device_by_name(&self, name: &str) -> Option<&dyn Device> {
        self.device_slot(name)
            .and_then(|s| self.devices.get(s))
            .map(|b| b.as_ref())
    }

    /// A mutable device reference by name.
    pub fn device_by_name_mut(&mut self, name: &str) -> Option<&mut dyn Device> {
        let slot = self.device_slot(name)?;
        match self.devices.get_mut(slot) {
            Some(device) => Some(device.as_mut() as &mut dyn Device),
            None => None,
        }
    }

    /// Reset every device to power-on state without boot data.
    pub fn reset(&mut self) {
        for d in &mut self.devices {
            d.reset();
        }
    }

    /// Run the machine until the primary domain reaches `target`.
    pub fn run_until(&mut self, target: Tick) {
        self.seed_queue();
        self.step_stopped = false;
        loop {
            if !self.step_once(target, &mut |_| false, false) {
                return;
            }
        }
    }

    /// Run the machine until the primary domain reaches `target`, or
    /// until the hook returns true for one runtime event.
    ///
    /// The hook receives the events devices emit; event collection is
    /// active only while `emit` is true.
    pub fn run_hook(&mut self, target: Tick, hook: RunHook, emit: bool) {
        self.seed_queue();
        self.step_stopped = false;
        loop {
            if !self.step_once(target, hook, emit) {
                return;
            }
        }
    }

    /// Seed every domain queue with the devices' declared next events.
    ///
    /// A device that declares `next_event` after boot is visited by the
    /// scheduler at that time. The domain index comes from the device's
    /// configured domain.
    fn seed_queue(&mut self) {
        for (slot, d) in self.devices.iter().enumerate() {
            if let Some(at) = d.next_event() {
                let domain = self.slot_domain[slot];
                self.scheduler.domains[domain].schedule(slot, at);
            }
        }
    }

    /// Re-seed one device's declared next event.
    ///
    /// Scheduling bumps the device's generation, so a stale queue entry
    /// from an earlier declaration is discarded on pop.
    fn reschedule(&mut self, slot: usize) {
        let domain = self.slot_domain[slot];
        if let Some(at) = self.devices[slot].next_event() {
            self.scheduler.domains[domain].schedule(slot, at);
        }
    }

    /// Step the machine once: advance the domain with the smallest
    /// instant, dispatch all events due there.
    ///
    /// Returns false when the primary domain reached the target, when
    /// the hook stopped the run, or when every queue is empty. Any
    /// pending event past the target's exact real time is out of
    /// scope: for the primary's own domain that is the raw tick
    /// target, and for a secondary domain the instant compares
    /// against the target's real time through the exact
    /// cross-multiplication in
    /// [`crate::scheduler::compare_instants`].
    fn step_once(
        &mut self,
        target: Tick,
        hook: &mut dyn FnMut(&TriggerEvent) -> bool,
        emit: bool,
    ) -> bool {
        // Find the domain with the smallest pending event instant, or the
        // smallest current instant when all queues are empty.
        let target_instant = (
            target,
            self.scheduler.domains[self.scheduler.primary].frequency,
        );
        let mut best: Option<usize> = None;
        for (i, d) in self.scheduler.domains.iter().enumerate() {
            let Some(next) = d.next_time() else { continue };
            if crate::scheduler::compare_instants((next, d.frequency), target_instant)
                == std::cmp::Ordering::Greater
            {
                continue;
            }
            match best {
                None => best = Some(i),
                Some(j) => {
                    let a = (next, d.frequency);
                    let b = (
                        self.scheduler.domains[j].next_time().unwrap_or(Tick(0)),
                        self.scheduler.domains[j].frequency,
                    );
                    if crate::scheduler::compare_instants(a, b) == std::cmp::Ordering::Less {
                        best = Some(i);
                    }
                }
            }
        }
        let Some(i) = best else { return false };
        let next = self.scheduler.domains[i]
            .next_time()
            .expect("the loop found a non-empty queue");
        self.scheduler.domains[i].advance_to(next);
        while let Some(event) = self.scheduler.domains[i].pop_due() {
            let tick = event.tick;
            let slot = event.device;
            if slot >= self.devices.len() {
                continue;
            }
            self.dispatch(slot, tick, hook, emit);
            if self.step_stopped {
                return false;
            }
            let dirty_slots: Vec<usize> = self.dirty.drain(..).filter(|d| *d != slot).collect();
            for dirty_slot in dirty_slots {
                self.reschedule(dirty_slot);
            }
            self.reschedule(slot);
        }
        if self.scheduler.domains[self.scheduler.primary].now >= target {
            return false;
        }
        true
    }

    /// Dispatch one device visit: build the context and hand over.
    ///
    /// The hook drains the events the visit emitted; a true return ends
    /// the run after the dispatch completes.
    fn dispatch(
        &mut self,
        slot: usize,
        tick: Tick,
        hook: &mut dyn FnMut(&TriggerEvent) -> bool,
        emit: bool,
    ) {
        self.events.clear();
        self.dirty.clear();
        let Machine {
            devices,
            buses,
            routes,
            bus_index,
            scratch,
            events,
            dirty,
            ..
        } = self;
        let (before, rest) = devices.split_at_mut(slot);
        let (device, after) = rest.split_first_mut().expect("dispatch slot exists");
        let mut ctx = MachineCtx::new(
            DeviceSlices {
                before,
                after,
                caller_slot: slot,
            },
            BusBank {
                buses,
                index: bus_index,
            },
            routes,
            scratch,
            EventBuffers { events, dirty },
            emit,
        );
        device.tick(tick, &mut ctx);
        for event in self.events.drain(..) {
            if hook(&event) {
                self.step_stopped = true;
                return;
            }
        }
    }

    /// The primary domain's current tick.
    pub fn now(&self) -> Tick {
        self.scheduler.domains[self.scheduler.primary].now
    }

    /// The primary bus's name, for debug reads and writes.
    pub fn primary_bus(&self) -> Option<&str> {
        self.buses.first().map(|b| b.name.as_str())
    }

    /// A side-effect-free read through the bus decoder.
    pub fn peek(&self, bus_name: &str, address: crate::frequency::Address) -> u8 {
        let Some(bus_idx) = self.bus_index.get(bus_name).copied() else {
            return 0xFF;
        };
        let mut bus_idx = bus_idx;
        let mut address = address;
        for _ in 0..=MAX_BORROW_DEPTH {
            let Some(hit) = decode_candidates(&self.buses[bus_idx], address.0, false)
                .into_iter()
                .find(|hit| {
                    self.devices
                        .get(hit.device)
                        .and_then(|d| d.bus())
                        .is_some_and(|bus| bus.decodes(address))
                })
            else {
                return 0xFF;
            };
            let Some(device) = self.devices.get(hit.device) else {
                continue;
            };
            let Some(bus) = device.bus() else {
                continue;
            };
            if let Borrow::Bridge { bus, address: next } = bus.borrow(address) {
                let Some(next_idx) = self.bus_index.get(&bus).copied() else {
                    return 0xFF;
                };
                bus_idx = next_idx;
                address = crate::frequency::Address(next);
                continue;
            }
            return bus.peek(address);
        }
        0xFF
    }

    /// A debug write through the bus decoder, stamped at the current
    /// tick. Returns true when a device accepted the write.
    ///
    /// Window devices that borrow another bus (the design's
    /// DMA-parented form) route the access further; the write lands
    /// when the chain finds a local answerer.
    pub fn debug_write(
        &mut self,
        bus_name: &str,
        address: crate::frequency::Address,
        value: u8,
    ) -> bool {
        let Some(bus_idx) = self.bus_index.get(bus_name).copied() else {
            return false;
        };
        let tick = self.now();
        let mut bus_idx = bus_idx;
        let mut address = address;
        for _ in 0..=MAX_BORROW_DEPTH {
            let hops = decode_candidates(&self.buses[bus_idx], address.0, true);
            let Some(next) = hops
                .into_iter()
                .find(|hit| {
                    self.devices
                        .get(hit.device)
                        .and_then(|d| d.bus())
                        .is_some_and(|bus| bus.decodes(address))
                })
                .map(|hit| hit.device)
            else {
                return false;
            };
            // Ask the device's borrow decision before the answering
            // device's own write, then either route or land.
            let decision = {
                let Some(device) = self.devices.get_mut(next) else {
                    return false;
                };
                let Some(bus) = device.bus_mut() else {
                    return false;
                };
                bus.borrow(address)
            };
            if let Borrow::Bridge {
                bus,
                address: next_addr,
            } = decision
            {
                let Some(next_idx) = self.bus_index.get(&bus).copied() else {
                    return false;
                };
                bus_idx = next_idx;
                address = crate::frequency::Address(next_addr);
                continue;
            }
            let Some(device) = self.devices.get_mut(next) else {
                return false;
            };
            let Some(bus) = device.bus_mut() else {
                return false;
            };
            bus.write(
                crate::device::AccessWidth::Byte,
                address,
                u32::from(value),
                tick,
            );
            return true;
        }
        false
    }

    /// Save the full machine state.
    ///
    /// # Errors
    ///
    /// Returns an error when the writer fails.
    pub fn save<W: std::io::Write>(&self, out: &mut W) -> Result<(), SaveError> {
        let state = SaveState {
            machine_hash: self.machine_hash(),
            devices: self
                .devices
                .iter()
                .map(|d| DeviceChunk {
                    name: d.name().to_string(),
                    data: d.snapshot(),
                })
                .collect(),
            domains: self
                .scheduler
                .domains
                .iter()
                .map(|d| {
                    // The events sort by tick and sequence; the heap's
                    // internal layout is not canonical across a restore.
                    let mut events: Vec<(u64, u32, u64, u32)> = d
                        .queue
                        .iter()
                        .map(|e| (e.tick.0, e.device as u32, e.seq, e.generation))
                        .collect();
                    events.sort_unstable();
                    DomainState {
                        name: d.name.clone(),
                        frequency_num: d.frequency.num(),
                        frequency_den: d.frequency.den(),
                        now: d.now.0,
                        events,
                    }
                })
                .collect(),
        };
        write_save(out, &state)
    }

    /// Load a full machine state.
    ///
    /// # Errors
    ///
    /// Returns an error when the format or the machine hash does not
    /// match, or when a device rejects its chunk.
    pub fn load(&mut self, buf: &[u8]) -> Result<(), LoadError> {
        let state = read_save(buf, self.machine_hash())?;
        for chunk in &state.devices {
            if let Some(d) = self.devices.iter_mut().find(|d| d.name() == chunk.name) {
                d.restore(&chunk.data).map_err(|e| LoadError::Restore {
                    device: chunk.name.clone(),
                    source: e,
                })?;
            }
        }
        let mut domains = Vec::new();
        for saved in state.domains {
            domains.push(saved.into_domain()?);
        }
        self.scheduler.domains = domains;
        Ok(())
    }
}

/// Resolve one configured device name to its slot.
fn resolve_slot(config: &SystemConfig, name: &str) -> Result<usize, MachineError> {
    let key = demu_config::newtypes::DeviceName::new(name)
        .map_err(|e| MachineError::Assembly(format!("bad device name '{name}': {e}")))?;
    config
        .devices_by_name
        .get(&key)
        .copied()
        .ok_or_else(|| MachineError::Assembly(format!("unknown device '{name}'")))
}

/// Build one device from its configuration entry.
fn build_device(
    device: &DeviceInstance,
    config: &SystemConfig,
    ctors: &DeviceCtors,
) -> Result<Box<dyn Device>, MachineError> {
    match ctors.get(device.kind.as_str()) {
        Some(ctor) => ctor(device, config),
        None => Err(MachineError::Assembly(format!(
            "no device constructor for kind '{}' in machine '{}'",
            device.kind.as_str(),
            config.name
        ))),
    }
}

/// Decode an address on a bus into candidates in priority order.
///
/// The machine-level debug path reuses this; the per-visit context runs
/// its own allocation-free copy against a scratch buffer.
fn decode_candidates(bus: &Bus, address: u32, write: bool) -> Vec<DecodeHit> {
    let mut ranked: Vec<DecodeHit> = Vec::new();
    for (i, w) in bus.windows.iter().enumerate() {
        let Some(offset) = window_offset_of(w, address) else {
            continue;
        };
        let prio = match &w.overlay {
            Some(o) if write => o.write_priority,
            Some(o) => o.read_priority,
            None => 0,
        };
        ranked.push(DecodeHit {
            priority: u32::MAX - prio,
            window: i,
            device: w.device,
            offset,
        });
    }
    ranked.sort_unstable();
    ranked
}

/// Map a configured conflict policy to the core one.
fn conflict_of(policy: demu_config::model::ConflictPolicy) -> ConflictPolicy {
    match policy {
        demu_config::model::ConflictPolicy::First => ConflictPolicy::First,
        demu_config::model::ConflictPolicy::And => ConflictPolicy::And,
        demu_config::model::ConflictPolicy::LastWins => ConflictPolicy::LastWins,
    }
}

pub mod devices {
    //! The smoke device family: synthetic devices for framework tests.

    use super::*;

    /// The smoke bridge: a window on one bus whose device borrows
    /// another bus's decoder with a fixed address translation.
    ///
    /// The device exists to exercise the borrowed-decoder route: an
    /// access into the bridge window re-decodes on the target bus at
    /// the translated address, exactly like the DMA-parented bus form
    /// the design states.
    pub struct SmokeBridge {
        name: String,
        /// The borrowed bus name.
        to_bus: String,
        /// The window base on the caller's bus.
        from_base: u32,
        /// The translated base on the borrowed bus.
        to_base: u32,
        /// The window size in bytes.
        size: u32,
    }

    impl SmokeBridge {
        /// A bridge from its configuration entry.
        pub fn new(device: &DeviceInstance) -> Self {
            Self {
                name: device.name.to_string(),
                to_bus: device
                    .params
                    .get("to_bus")
                    .and_then(|v| v.as_str())
                    .unwrap_or("cpu")
                    .to_string(),
                from_base: device
                    .params
                    .get("from_base")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(0x8000) as u32,
                to_base: device
                    .params
                    .get("to_base")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(0) as u32,
                size: device
                    .params
                    .get("size")
                    .and_then(|v| v.as_integer())
                    .unwrap_or(0x8000) as u32,
            }
        }
    }

    impl crate::device::BusAccess for SmokeBridge {
        fn read(
            &mut self,
            _width: crate::device::AccessWidth,
            address: crate::frequency::Address,
            _tick: Tick,
        ) -> u32 {
            let _ = address;
            0xFF
        }

        fn write(
            &mut self,
            _width: crate::device::AccessWidth,
            _address: crate::frequency::Address,
            _value: u32,
            _tick: Tick,
        ) {
        }

        fn peek(&self, _address: crate::frequency::Address) -> u8 {
            0xFF
        }

        fn borrow(&self, address: crate::frequency::Address) -> crate::device::Borrow {
            let inside = address.0 >= self.from_base && address.0 < self.from_base + self.size;
            if !inside {
                return crate::device::Borrow::Local;
            }
            crate::device::Borrow::Bridge {
                bus: self.to_bus.clone(),
                address: (address.0 - self.from_base) + self.to_base,
            }
        }
    }

    impl Device for SmokeBridge {
        fn name(&self) -> &str {
            &self.name
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn bus(&self) -> Option<&dyn crate::device::BusAccess> {
            Some(self)
        }

        fn bus_mut(&mut self) -> Option<&mut dyn crate::device::BusAccess> {
            Some(self)
        }

        fn reset(&mut self) {}

        fn boot(&mut self, _state: Option<&BootState>) {}

        fn snapshot(&self) -> crate::device::Snapshot {
            crate::device::Snapshot::new()
        }

        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }

        fn notify_line(&mut self, _line: &str, _level: crate::signal::Level, _tick: Tick) {}

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    /// The smoke CPU shim: a device that schedules its own next tick and
    /// holds the boot registers.
    #[derive(Debug)]
    pub struct SmokeCpu {
        name: String,
        divider: u32,
        pub ip: u64,
        next: Option<Tick>,
        boot_registers: std::collections::BTreeMap<String, u64>,
    }

    impl SmokeCpu {
        /// A CPU shim from its configuration entry.
        pub fn new(device: &DeviceInstance) -> Self {
            Self {
                name: device.name.to_string(),
                divider: *device.dividers.get("cpu").unwrap_or(&1),
                ip: 0,
                next: None,
                boot_registers: std::collections::BTreeMap::new(),
            }
        }
    }

    impl crate::device::CpuDebug for SmokeCpu {
        fn registers(&self) -> std::collections::BTreeMap<String, u64> {
            let mut map = std::collections::BTreeMap::new();
            map.insert("ip".to_string(), self.ip);
            map
        }

        fn set_register(&mut self, name: &str, value: u64) -> bool {
            if name == "ip" {
                self.ip = value;
                true
            } else {
                false
            }
        }
    }

    impl Device for SmokeCpu {
        fn name(&self) -> &str {
            &self.name
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn cpu(&self) -> Option<&dyn crate::device::CpuDebug> {
            Some(self)
        }

        fn cpu_mut(&mut self) -> Option<&mut dyn crate::device::CpuDebug> {
            Some(self)
        }

        fn reset(&mut self) {
            self.ip = 0;
            self.next = None;
        }

        fn boot(&mut self, state: Option<&BootState>) {
            if let Some(BootState::Cpu { registers }) = state {
                self.boot_registers = registers.clone();
            }
            if let Some(value) = self.boot_registers.get("ip") {
                self.ip = *value;
            }
            let step = u64::from(self.divider.max(1));
            self.next = Some(Tick(step));
        }

        fn snapshot(&self) -> crate::device::Snapshot {
            let mut out = Vec::new();
            out.extend_from_slice(&self.ip.to_le_bytes());
            out.extend_from_slice(&self.next.map(|t| t.0).unwrap_or(u64::MAX).to_le_bytes());
            out
        }

        fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
            if data.len() != 16 {
                return Err(SnapshotError::Mismatch("cpu chunk must be 16 bytes".into()));
            }
            self.ip = u64::from_le_bytes(data[..8].try_into().expect("8 bytes"));
            let next = u64::from_le_bytes(data[8..].try_into().expect("8 bytes"));
            self.next = (next != u64::MAX).then_some(Tick(next));
            Ok(())
        }

        fn notify_line(&mut self, _line: &str, _level: crate::signal::Level, _tick: Tick) {}

        fn next_event(&self) -> Option<Tick> {
            self.next
        }

        fn tick(&mut self, tick: Tick, _ctx: &mut MachineCtx) {
            self.ip = self.ip.wrapping_add(1);
            let step = u64::from(self.divider.max(1));
            self.next = Some(Tick(tick.0 + step));
        }
    }

    /// The smoke RAM: a memory block with boot patterns and snapshots.
    pub struct SmokeRam {
        name: String,
        data: Vec<u8>,
    }

    impl SmokeRam {
        /// A RAM device from its configuration entry.
        pub fn new(device: &DeviceInstance) -> Self {
            let size = device
                .params
                .get("size")
                .and_then(|v| v.as_integer())
                .unwrap_or(0x1000) as usize;
            Self {
                name: device.name.to_string(),
                data: vec![0; size],
            }
        }

        /// The current memory bytes.
        pub fn data(&self) -> &[u8] {
            &self.data
        }

        /// The memory size in bytes.
        pub fn data_len(&self) -> usize {
            self.data.len()
        }

        /// Poke one debug byte at `index`.
        ///
        /// # Panics
        ///
        /// Panics when `index` is outside the memory. The caller resolves
        /// the index through the bus, which bounds it.
        pub fn poke(&mut self, index: usize, value: u8) {
            self.data[index] = value;
        }
    }

    impl crate::device::BusAccess for SmokeRam {
        fn read(
            &mut self,
            _width: crate::device::AccessWidth,
            address: crate::frequency::Address,
            _tick: Tick,
        ) -> u32 {
            let idx = address.0 as usize % self.data.len();
            u32::from(self.data[idx])
        }

        fn write(
            &mut self,
            _width: crate::device::AccessWidth,
            address: crate::frequency::Address,
            value: u32,
            _tick: Tick,
        ) {
            let idx = address.0 as usize % self.data.len();
            self.data[idx] = value as u8;
        }

        fn peek(&self, address: crate::frequency::Address) -> u8 {
            let idx = address.0 as usize % self.data.len();
            self.data[idx]
        }
    }

    impl Device for SmokeRam {
        fn name(&self) -> &str {
            &self.name
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn bus(&self) -> Option<&dyn crate::device::BusAccess> {
            Some(self)
        }

        fn bus_mut(&mut self) -> Option<&mut dyn crate::device::BusAccess> {
            Some(self)
        }

        fn reset(&mut self) {
            self.data.fill(0);
        }

        fn boot(&mut self, state: Option<&BootState>) {
            match state {
                Some(BootState::Ram { pattern }) => crate::boot::fill_ram(&mut self.data, *pattern),
                Some(BootState::Hex { bytes }) => {
                    self.data.fill(0);
                    let n = bytes.len().min(self.data.len());
                    self.data[..n].copy_from_slice(&bytes[..n]);
                }
                _ => self.data.fill(0),
            }
        }

        fn snapshot(&self) -> crate::device::Snapshot {
            self.data.clone()
        }

        fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
            if data.len() != self.data.len() {
                return Err(SnapshotError::Mismatch(
                    "ram chunk size does not match the device".into(),
                ));
            }
            self.data.copy_from_slice(data);
            Ok(())
        }

        fn notify_line(&mut self, _line: &str, _level: crate::signal::Level, _tick: Tick) {}

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    /// The smoke ROM: a read-only block.
    pub struct SmokeRom {
        name: String,
        data: Vec<u8>,
    }

    impl SmokeRom {
        /// A ROM device from its configuration entry.
        pub fn new(device: &DeviceInstance) -> Self {
            let size = device
                .params
                .get("size")
                .and_then(|v| v.as_integer())
                .unwrap_or(0x10000) as usize;
            Self {
                name: device.name.to_string(),
                data: vec![0; size],
            }
        }

        /// The current memory bytes.
        pub fn data(&self) -> &[u8] {
            &self.data
        }
    }

    impl crate::device::BusAccess for SmokeRom {
        fn read(
            &mut self,
            _width: crate::device::AccessWidth,
            address: crate::frequency::Address,
            _tick: Tick,
        ) -> u32 {
            let idx = address.0 as usize % self.data.len();
            u32::from(self.data[idx])
        }

        fn write(
            &mut self,
            _width: crate::device::AccessWidth,
            _address: crate::frequency::Address,
            _value: u32,
            _tick: Tick,
        ) {
        }

        fn peek(&self, address: crate::frequency::Address) -> u8 {
            let idx = address.0 as usize % self.data.len();
            self.data[idx]
        }
    }

    impl Device for SmokeRom {
        fn name(&self) -> &str {
            &self.name
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn bus(&self) -> Option<&dyn crate::device::BusAccess> {
            Some(self)
        }

        fn bus_mut(&mut self) -> Option<&mut dyn crate::device::BusAccess> {
            Some(self)
        }

        fn reset(&mut self) {}

        fn boot(&mut self, state: Option<&BootState>) {
            match state {
                Some(BootState::Hex { bytes }) => {
                    self.data.fill(0);
                    let n = bytes.len().min(self.data.len());
                    self.data[..n].copy_from_slice(&bytes[..n]);
                }
                Some(BootState::Rom { path }) => {
                    // A missing ROM file leaves the block zeroed; the
                    // error surfaces when the machine reads zeros.
                    let _ = crate::boot::load_rom(&mut self.data, std::path::Path::new(path));
                }
                _ => self.data.fill(0),
            }
        }

        fn snapshot(&self) -> crate::device::Snapshot {
            self.data.clone()
        }

        fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
            if data.len() != self.data.len() {
                return Err(SnapshotError::Mismatch(
                    "rom chunk size does not match the device".into(),
                ));
            }
            self.data.copy_from_slice(data);
            Ok(())
        }
        fn notify_line(&mut self, _line: &str, _level: crate::signal::Level, _tick: Tick) {}

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smoke_config() -> SystemConfig {
        demu_config::smoke_config().expect("smoke config is valid")
    }

    /// A two-bus bridge machine: the RAM answers on the main bus; the
    /// bridge window on the sound bus borrows the main bus's decoder
    /// at a translated address.
    fn bridge_machine(to_bus: &str, from_base: u32, to_base: u32) -> Machine {
        use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
        use demu_config::newtypes::{DeviceName, KindName};
        use std::collections::BTreeMap;

        let mut params = BTreeMap::new();
        params.insert("size".to_string(), toml::Value::Integer(32));
        let ram = DeviceInstance {
            name: DeviceName::new("ram0").expect("name"),
            kind: KindName::new("smoke.ram").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        };
        let mut bridge_params = BTreeMap::new();
        bridge_params.insert(
            "to_bus".to_string(),
            toml::Value::String(to_bus.to_string()),
        );
        bridge_params.insert(
            "from_base".to_string(),
            toml::Value::Integer(from_base as i64),
        );
        bridge_params.insert("to_base".to_string(), toml::Value::Integer(to_base as i64));
        bridge_params.insert("size".to_string(), toml::Value::Integer(0x8000));
        let bridge = DeviceInstance {
            name: DeviceName::new("bridge").expect("name"),
            kind: KindName::new("smoke.bridge").expect("kind"),
            domain: "main".to_string(),
            params: bridge_params,
            dividers: BTreeMap::new(),
        };
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(16_000_000, 1).expect("freq"),
                origin: "the test crystal".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ram.name.clone(), 0usize);
        by_name.insert(bridge.name.clone(), 1usize);
        let config = SystemConfig {
            name: "bridge-test".to_string(),
            triple: "test".to_string(),
            description: "the borrowed-decoder test machine".to_string(),
            clocks,
            devices: vec![ram, bridge],
            devices_by_name: by_name,
            buses: vec![
                CBus {
                    name: "cpu".to_string(),
                    address_bits: 24,
                    owner: None,
                    parent: None,
                },
                CBus {
                    name: "sound".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
            ],
            windows: vec![
                CWindow {
                    name: "w_ram".to_string(),
                    bus: "cpu".to_string(),
                    device: DeviceName::new("ram0").expect("name"),
                    base: 0,
                    size: 0x20,
                    kind: Some(demu_config::model::WindowKind::Ram),
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
                CWindow {
                    name: "w_bridge".to_string(),
                    bus: "sound".to_string(),
                    device: DeviceName::new("bridge").expect("name"),
                    base: from_base,
                    size: 0x8000,
                    kind: None,
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
            ],
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut machine =
            Machine::assemble(config, &smoke_ctors()).expect("bridge machine assembles");
        machine.boot();
        machine
    }

    fn ram_of(machine: &Machine) -> &devices::SmokeRam {
        machine
            .device_by_name("ram0")
            .and_then(|d| d.as_any().downcast_ref::<devices::SmokeRam>())
            .expect("ram0 exists")
    }

    fn poke_ram(machine: &mut Machine, index: usize, value: u8) -> Result<(), MachineError> {
        machine
            .device_by_name_mut("ram0")
            .and_then(|d| d.as_any_mut().downcast_mut::<devices::SmokeRam>())
            .ok_or_else(|| MachineError::Assembly("ram0 missing".into()))
            .map(|ram| ram.poke(index, value))
    }

    #[test]
    fn the_borrowed_decoder_routes_reads_and_writes() -> Result<(), MachineError> {
        use crate::frequency::Address;
        let mut machine = bridge_machine("cpu", 0x8000, 0x0000);
        poke_ram(&mut machine, 0x10, 0x5A)?;
        // A read on the sound bus bridges to the main bus's RAM.
        assert_eq!(machine.peek("sound", Address(0x8010)), 0x5A);
        // A debug write bridges the same way and lands on the RAM.
        assert!(machine.debug_write("sound", Address(0x8011), 0x21));
        assert_eq!(ram_of(&machine).data()[0x11], 0x21);
        // The direct forms still answer.
        assert_eq!(machine.peek("cpu", Address(0x10)), 0x5A);
        Ok(())
    }

    #[test]
    fn a_borrow_loop_answers_nothing() {
        use crate::frequency::Address;
        // The bridge translates back into its own window, so every
        // hop loops; the depth cap stops the access.
        let mut machine = bridge_machine("sound", 0x8000, 0x8000);
        assert_eq!(machine.peek("sound", Address(0x8010)), 0xFF);
        assert!(!machine.debug_write("sound", Address(0x8011), 0x21));
    }

    #[test]
    fn smoke_machine_assembles() {
        let machine = Machine::new(smoke_config()).expect("smoke machine assembles");
        assert_eq!(machine.devices.len(), 3);
        assert_eq!(machine.buses.len(), 2);
        assert_ne!(machine.machine_hash(), 0);
    }

    #[test]
    fn smoke_machine_runs_and_is_deterministic() {
        let run = || -> (Tick, Vec<u8>) {
            let mut machine = Machine::new(smoke_config()).unwrap();
            machine.boot();
            machine.run_until(Tick(100));
            let cpu = machine
                .devices
                .iter()
                .find(|d| d.name() == "cpu0")
                .expect("cpu0 exists");
            (machine.now(), cpu.snapshot())
        };
        let (t1, s1) = run();
        let (t2, s2) = run();
        assert_eq!(t1, t2);
        assert_eq!(s1, s2, "identical runs produce identical state");
    }

    #[test]
    fn save_and_restore_reproduces_state() {
        let mut machine = Machine::new(smoke_config()).unwrap();
        machine.boot();
        machine.run_until(Tick(500));
        let mut buf = Vec::new();
        machine.save(&mut buf).unwrap();

        let mut restored = Machine::new(smoke_config()).unwrap();
        restored.load(&buf).unwrap();
        let mut buf2 = Vec::new();
        restored.save(&mut buf2).unwrap();
        assert_eq!(buf, buf2, "save after restore is byte-identical");
        assert_eq!(restored.now(), machine.now());
    }

    #[test]
    fn load_rejects_foreign_machine_hash() {
        let mut machine = Machine::new(smoke_config()).unwrap();
        machine.boot();
        let mut buf = Vec::new();
        machine.save(&mut buf).unwrap();
        // Corrupt the machine hash bytes.
        let mut broken = buf.clone();
        broken[8] ^= 0xFF;
        match restored_load(&broken) {
            Err(LoadError::MachineHash { .. }) => {}
            other => panic!("expected MachineHash, got {other:?}"),
        }
    }

    fn restored_load(buf: &[u8]) -> Result<(), LoadError> {
        let mut machine = Machine::new(smoke_config()).unwrap();
        machine.load(buf)
    }

    #[test]
    fn cpu_registers_round_trip() {
        let mut machine = Machine::new(smoke_config()).unwrap();
        machine.boot();
        let out = {
            let cpu = machine.device_by_name_mut("cpu0").expect("cpu0");
            cpu.cpu_mut()
                .expect("smoke cpu exposes registers")
                .set_register("ip", 0x1234)
        };
        assert!(out);
        let cpu = machine.device_by_name("cpu0").expect("cpu0");
        let regs = cpu.cpu().expect("cpu").registers();
        assert_eq!(regs.get("ip"), Some(&0x1234));
    }

    #[test]
    fn run_hook_completes_when_no_events_fire() {
        let mut machine = Machine::new(smoke_config()).unwrap();
        machine.boot();
        machine.run_until(Tick(100));
        let mut machine2 = Machine::new(smoke_config()).unwrap();
        machine2.boot();
        machine2.run_hook(
            Tick(100),
            &mut |event| matches!(event, crate::triggers::TriggerEvent::Line { .. }),
            true,
        );
        assert_eq!(machine2.now(), machine.now());
    }
}
