//! Machine assembly and the run loop.
//!
//! A `Machine` owns the devices, the buses, and the scheduler. The smoke
//! machine from the bundled configuration demonstrates the assembly: a
//! clock domain, a ROM, a RAM, and a CPU shim. The run loop advances
//! domains in exact cross-domain order, dispatches events, and stops at
//! sync points.

use demu_config::model::{BootState, SystemConfig};

use crate::bus::{Bus, BusWindow, ConflictPolicy};
use crate::device::{Device, SnapshotError};
use crate::frequency::{Frequency, Tick};
use crate::savestate::{
    DeviceChunk, DomainState, LoadError, SaveError, SaveState, read_save, write_save,
};
use crate::scheduler::Scheduler;

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
}

impl Machine {
    /// Assemble a machine from a resolved configuration.
    ///
    /// The devices and buses come from the device-family crates at
    /// assembly time; this core constructor wires the smoke CPU shim and
    /// memory devices so that the framework runs before the real device
    /// families land.
    pub fn new(config: SystemConfig) -> Result<Self, MachineError> {
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
        let mut buses = Vec::new();
        for bus in &config.buses {
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
        let devices: Vec<Box<dyn Device>> = config
            .devices
            .iter()
            .map(|d| build_device(d, &config))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            config,
            devices,
            buses,
            scheduler,
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
        for d in &mut self.devices {
            d.boot();
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
        loop {
            if !self.step_once(target) {
                return;
            }
        }
    }

    /// Step the machine once: advance the domain with the smallest
    /// instant, dispatch all events due there.
    ///
    /// Returns false when the primary domain reached the target, or when
    /// every queue is empty.
    fn step_once(&mut self, target: Tick) -> bool {
        // Find the domain with the smallest pending event instant, or the
        // smallest current instant when all queues are empty.
        let mut best: Option<usize> = None;
        for (i, d) in self.scheduler.domains.iter().enumerate() {
            let Some(next) = d.next_time() else { continue };
            if next > target && d.name == self.scheduler.primary_name() {
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
            if slot < self.devices.len() {
                self.devices[slot].tick(tick);
            }
        }
        if self.scheduler.domains[self.scheduler.primary].now >= target {
            return false;
        }
        true
    }

    /// The primary domain's current tick.
    pub fn now(&self) -> Tick {
        self.scheduler.domains[self.scheduler.primary].now
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
                .map(|d| DomainState {
                    name: d.name.clone(),
                    frequency_num: d.frequency.num(),
                    frequency_den: d.frequency.den(),
                    now: d.now.0,
                    events: d
                        .queue
                        .iter()
                        .map(|e| (e.tick.0, e.device as u32, e.seq))
                        .collect(),
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

/// Map a configured conflict policy to the core one.
fn conflict_of(policy: demu_config::model::ConflictPolicy) -> ConflictPolicy {
    match policy {
        demu_config::model::ConflictPolicy::First => ConflictPolicy::First,
        demu_config::model::ConflictPolicy::And => ConflictPolicy::And,
        demu_config::model::ConflictPolicy::LastWins => ConflictPolicy::LastWins,
    }
}

/// Build one device from its configuration entry.
fn build_device(
    device: &demu_config::model::DeviceInstance,
    config: &SystemConfig,
) -> Result<Box<dyn Device>, MachineError> {
    match device.kind.as_str() {
        "smoke.cpu" => Ok(Box::new(devices::SmokeCpu::new(device))),
        "smoke.ram" => Ok(Box::new(devices::SmokeRam::new(device))),
        "smoke.rom" => Ok(Box::new(devices::SmokeRom::new(device))),
        other => Err(MachineError::Assembly(format!(
            "no device constructor for kind '{other}' in machine '{}'",
            config.name
        ))),
    }
}

pub mod devices {
    //! The smoke device family: synthetic devices for framework tests.

    use super::*;

    /// The smoke CPU shim: a device that schedules its own next tick and
    /// holds the boot registers.
    #[derive(Debug)]
    pub struct SmokeCpu {
        name: String,
        divider: u32,
        boot_registers: std::collections::BTreeMap<String, u64>,
        pub ip: u64,
        next: Option<Tick>,
    }

    impl SmokeCpu {
        /// A CPU shim from its configuration entry.
        pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
            Self {
                name: device.name.to_string(),
                divider: *device.dividers.get("cpu").unwrap_or(&1),
                boot_registers: std::collections::BTreeMap::new(),
                ip: 0,
                next: None,
            }
        }

        /// Attach the resolved boot state for the boot phase.
        pub fn attach_boot(&mut self, state: &BootState) {
            if let BootState::Cpu { registers } = state {
                self.boot_registers = registers.clone();
            }
        }
    }

    impl Device for SmokeCpu {
        fn name(&self) -> &str {
            &self.name
        }

        fn reset(&mut self) {
            self.ip = 0;
            self.next = None;
        }

        fn boot(&mut self) {
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

        fn tick(&mut self, _tick: Tick) {
            self.ip = self.ip.wrapping_add(1);
            let step = u64::from(self.divider.max(1));
            self.next = Some(Tick(_tick.0 + step));
        }
    }

    /// The smoke RAM: a memory block with boot patterns and snapshots.
    pub struct SmokeRam {
        name: String,
        data: Vec<u8>,
        boot: Option<BootState>,
    }

    impl SmokeRam {
        /// A RAM device from its configuration entry.
        pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
            let size = device
                .params
                .get("size")
                .and_then(|v| v.as_integer())
                .unwrap_or(0x1000) as usize;
            Self {
                name: device.name.to_string(),
                data: vec![0; size],
                boot: None,
            }
        }

        /// Attach the resolved boot state.
        pub fn attach_boot(&mut self, state: BootState) {
            self.boot = Some(state);
        }

        /// The current memory bytes.
        pub fn data(&self) -> &[u8] {
            &self.data
        }
    }

    impl Device for SmokeRam {
        fn name(&self) -> &str {
            &self.name
        }

        fn reset(&mut self) {
            self.data.fill(0);
        }

        fn boot(&mut self) {
            match &self.boot {
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
    }

    /// The smoke ROM: a read-only block.
    pub struct SmokeRom {
        name: String,
        data: Vec<u8>,
        boot: Option<BootState>,
    }

    impl SmokeRom {
        /// A ROM device from its configuration entry.
        pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
            let size = device
                .params
                .get("size")
                .and_then(|v| v.as_integer())
                .unwrap_or(0x10000) as usize;
            Self {
                name: device.name.to_string(),
                data: vec![0; size],
                boot: None,
            }
        }

        /// Attach the resolved boot state.
        pub fn attach_boot(&mut self, state: BootState) {
            self.boot = Some(state);
        }
    }

    impl Device for SmokeRom {
        fn name(&self) -> &str {
            &self.name
        }

        fn reset(&mut self) {}

        fn boot(&mut self) {
            match &self.boot {
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
                    "rom chunk size does not match the device".into(),
                ));
            }
            self.data.copy_from_slice(data);
            Ok(())
        }

        fn notify_line(&mut self, _line: &str, _level: crate::signal::Level, _tick: Tick) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smoke_config() -> SystemConfig {
        demu_config::smoke_config().expect("smoke config is valid")
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
}
