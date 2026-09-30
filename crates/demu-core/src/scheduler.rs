//! Clock domains and the scheduler.
//!
//! Each domain counts its own crystal ticks. The scheduler keeps one event
//! queue per domain and orders events across domains by exact rational
//! comparison: for times `a/b` and `c/d`, it compares `a*d` with `c*b`.
//! No float math and no common grid exist anywhere.

use rustc_hash::FxHashMap;

use crate::frequency::{Divider, Frequency, Tick};

/// One pending event in a domain queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The domain tick at which the event fires.
    pub tick: Tick,
    /// The device slot in the machine's device table.
    pub device: usize,
    /// A stable sequence number for deterministic tie-breaking.
    pub seq: u64,
    /// The device's current generation; a stale event is discarded.
    pub generation: u32,
}

impl Ord for Event {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap order: the smallest tick and the lowest sequence first.
        other.tick.cmp(&self.tick).then(other.seq.cmp(&self.seq))
    }
}

impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A clock domain with its own event queue.
#[derive(Debug)]
pub struct ClockDomain {
    /// The domain name.
    pub name: String,
    /// The crystal frequency.
    pub frequency: Frequency,
    /// The current tick counter.
    pub now: Tick,
    /// The pending events.
    pub queue: std::collections::BinaryHeap<Event>,
    /// The next sequence number for tie-breaking.
    next_seq: u64,
    /// The event generation per device slot; scheduling bumps it, and a
    /// stale event in the heap is discarded on pop.
    pub gens: Vec<u32>,
}

impl ClockDomain {
    /// A domain with the given name and crystal.
    pub fn new(name: &str, frequency: Frequency) -> Self {
        Self {
            name: name.to_string(),
            frequency,
            now: Tick(0),
            queue: std::collections::BinaryHeap::new(),
            next_seq: 0,
            gens: Vec::new(),
        }
    }

    /// Schedule a device visit at an absolute domain tick.
    ///
    /// Each call bumps the device's generation, so an earlier event for
    /// the same device in the heap becomes stale and is discarded.
    pub fn schedule(&mut self, device: usize, at: Tick) {
        self.next_seq += 1;
        while self.gens.len() <= device {
            self.gens.push(0);
        }
        self.gens[device] = self.gens[device].wrapping_add(1);
        self.queue.push(Event {
            tick: at,
            device,
            seq: self.next_seq,
            generation: self.gens[device],
        });
    }

    /// Schedule a device visit after `div` ticks from now.
    pub fn schedule_after(&mut self, device: usize, div: Divider) {
        let at = Tick(self.now.0 + u64::from(div.get()));
        self.schedule(device, at);
    }

    /// The next event time, if any.
    ///
    /// Stale events do not count; the smallest live tick wins.
    pub fn next_time(&self) -> Option<Tick> {
        self.queue
            .iter()
            .filter(|e| self.gens.get(e.device).is_some_and(|g| *g == e.generation))
            .map(|e| e.tick)
            .min()
    }

    /// Pop the next event whose tick is due at or before `now`.
    ///
    /// Stale events (a generation that does not match the device's
    /// current generation) are discarded and the next one is returned.
    /// Returns `None` when the queue is empty or the next event is in
    /// the future.
    pub fn pop_due(&mut self) -> Option<Event> {
        while let Some(top) = self.queue.peek() {
            if top.tick > self.now {
                return None;
            }
            let event = self.queue.pop().expect("peeked");
            if self
                .gens
                .get(event.device)
                .is_some_and(|g| *g == event.generation)
            {
                return Some(event);
            }
        }
        None
    }

    /// Advance the domain counter to `at`.
    pub fn advance_to(&mut self, at: Tick) {
        self.now = at;
    }
}

/// Compare two domain instants exactly.
///
/// An instant's real time is `ticks * den / num` seconds. To compare
/// `(ta, na/da)` with `(tb, nb/db)`, cross-multiply:
/// `ta * da * nb` versus `tb * db * na`. The comparison is exact and
/// overflow-safe through `u128` products.
pub fn compare_instants(a: (Tick, Frequency), b: (Tick, Frequency)) -> std::cmp::Ordering {
    let left = u128::from(a.0.0) * u128::from(a.1.den()) * u128::from(b.1.num());
    let right = u128::from(b.0.0) * u128::from(b.1.den()) * u128::from(a.1.num());
    left.cmp(&right)
}

/// The scheduler: one queue per domain, dispatch in exact global order.
#[derive(Debug, Default)]
pub struct Scheduler {
    /// The domains in configuration declaration order.
    pub domains: Vec<ClockDomain>,
    /// The domain index by name.
    index: FxHashMap<String, usize>,
    /// The primary domain index.
    pub primary: usize,
}

impl Scheduler {
    /// A scheduler with one empty domain per name.
    pub fn new(domains: Vec<(String, Frequency)>) -> Self {
        let mut index = FxHashMap::default();
        let domain_list = domains
            .into_iter()
            .enumerate()
            .map(|(i, (name, frequency))| {
                index.insert(name.clone(), i);
                ClockDomain::new(&name, frequency)
            })
            .collect();
        Self {
            domains: domain_list,
            index,
            primary: 0,
        }
    }

    /// Look up a domain index by name.
    pub fn domain(&self, name: &str) -> Option<usize> {
        self.index.get(name).copied()
    }

    /// The name of the primary domain.
    pub fn primary_name(&self) -> &str {
        &self.domains[self.primary].name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_schedule_and_pop() {
        let mut d = ClockDomain::new("main", Frequency::from_hz(1000).unwrap());
        d.schedule(0, Tick(10));
        d.schedule(1, Tick(5));

        assert_eq!(d.next_time(), Some(Tick(5)));
        // Not due yet.
        assert!(d.pop_due().is_none());
        d.advance_to(Tick(5));
        let e = d.pop_due().expect("due");
        assert_eq!(e.device, 1);
        // Advance past both.
        d.advance_to(Tick(10));
        let e = d.pop_due().expect("due");
        assert_eq!(e.device, 0);
        assert!(d.pop_due().is_none());
    }

    #[test]
    fn tie_breaks_by_sequence() {
        let mut d = ClockDomain::new("main", Frequency::from_hz(1).unwrap());
        d.schedule(7, Tick(3));
        d.schedule(2, Tick(3));
        d.advance_to(Tick(3));
        let first = d.pop_due().expect("first");
        assert_eq!(first.device, 7, "registration order wins ties");
    }

    #[test]
    fn cross_domain_exact_comparison() {
        // NTSC master 236250000/11 Hz vs SPC700 1024000 Hz.
        // One second of each: 236250000/11 NTSC ticks, 1024000 SPC ticks.
        let a = (Tick(236250000 / 11), Frequency::new(236250000, 11).unwrap());
        let b = (Tick(1024000), Frequency::from_hz(1024000).unwrap());
        // 236250000/11 ticks of NTSC is 1 s; 1024000 SPC ticks is 1 s.
        // The NTSC tick count truncates, so the SPC instant is slightly
        // later in real time.
        assert_eq!(compare_instants(a, b), std::cmp::Ordering::Less);

        // The full-precision equal case: 11 s on both sides.
        let a11 = (Tick(236250000), Frequency::new(236250000, 11).unwrap());
        let b11 = (Tick(1024000 * 11), Frequency::from_hz(1024000).unwrap());
        assert_eq!(compare_instants(a11, b11), std::cmp::Ordering::Equal);

        let earlier = (Tick(1023999), Frequency::from_hz(1024000).unwrap());
        assert_eq!(compare_instants(earlier, b), std::cmp::Ordering::Less);
    }
}
