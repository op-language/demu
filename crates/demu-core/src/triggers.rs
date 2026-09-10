//! The trigger registry.
//!
//! A trigger watches one machine event and fires when the event happens
//! and, when present, the condition expression holds. Trigger kinds match
//! the technical design section 6.1.

use std::collections::BTreeMap;

use crate::expr::{Expr, Scope, parse_expr};
use crate::frequency::{Address, Tick};

/// A trigger kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerKind {
    /// A breakpoint at an instruction address.
    Breakpoint {
        /// The address.
        address: Address,
    },
    /// A memory read at an address.
    ReadWatch {
        /// The address.
        address: Address,
    },
    /// A memory write at an address.
    WriteWatch {
        /// The address.
        address: Address,
    },
    /// An I/O port read.
    IoRead {
        /// The port address.
        port: Address,
    },
    /// An I/O port write.
    IoWrite {
        /// The port address.
        port: Address,
    },
    /// A signal line level change.
    LineChange {
        /// The device name.
        device: String,
        /// The line name.
        line: String,
        /// The level that fires the trigger.
        level: crate::signal::Level,
    },
    /// A primary-domain cycle count reached.
    Cycle {
        /// The tick target.
        target: Tick,
    },
    /// A clock edge on a domain and divider.
    ClockEdge {
        /// The domain name.
        domain: String,
        /// The divider of the domain crystal.
        divider: u32,
    },
    /// An IRQ asserted.
    Irq,
    /// An instruction fetch completed.
    Fetch,
    /// A DMA event.
    Dma {
        /// The DMA operation: started or completed.
        phase: DmaPhase,
    },
    /// The vblank of the primary display.
    Vblank,
    /// A scanline reached.
    Scanline {
        /// The scanline number.
        number: u32,
    },
}

/// The DMA event phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaPhase {
    /// The transfer started.
    Start,
    /// The transfer completed.
    Complete,
}

/// A registered trigger.
#[derive(Debug, Clone)]
pub struct Trigger {
    /// The stable trigger id.
    pub id: u64,
    /// The kind.
    pub kind: TriggerKind,
    /// The optional condition expression.
    pub condition: Option<Expr>,
    /// The scope values the condition reads.
    pub scope: BTreeMap<String, u64>,
    /// How many times the trigger fired.
    pub hits: u64,
}

impl Trigger {
    /// Evaluate the condition. A trigger without a condition always fires.
    pub fn condition_passes(&self) -> bool {
        match &self.condition {
            None => true,
            Some(expr) => {
                let scope = Scope::new(&self.scope);
                expr.eval(&scope).unwrap_or(0) != 0
            }
        }
    }
}

/// The event a trigger watches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerEvent {
    /// A memory read at an address.
    Read(Address),
    /// A memory write at an address.
    Write(Address),
    /// An instruction fetch completed.
    Fetch,
    /// A DMA event.
    Dma(DmaPhase),
    /// A vblank.
    Vblank,
    /// A scanline.
    Scanline(u32),
    /// An IRQ.
    Irq,
    /// A line change.
    Line {
        /// The device name.
        device: String,
        /// The line name.
        line: String,
        /// The level.
        level: crate::signal::Level,
    },
}

/// The trigger registry.
#[derive(Debug, Default)]
pub struct TriggerRegistry {
    triggers: Vec<Trigger>,
    next_id: u64,
}

/// A trigger creation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TriggerError {
    /// The condition expression does not parse or names unknown values.
    #[error("trigger condition rejected: {0}")]
    Condition(String),
}

impl TriggerRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a trigger with an optional condition text.
    ///
    /// The condition parses once at creation. A condition that names a
    /// value missing from `scope` is rejected here, not at fire time.
    ///
    /// # Errors
    ///
    /// Returns an error when the condition text does not parse.
    pub fn add(
        &mut self,
        kind: TriggerKind,
        condition: Option<&str>,
        scope: BTreeMap<String, u64>,
    ) -> Result<u64, TriggerError> {
        let parsed = match condition {
            Some(text) => Some(
                parse_expr(text)
                    .and_then(|expr| {
                        // Validate every identifier resolves at creation.
                        let _ = expr.eval(&Scope::new(&scope))?;
                        Ok(expr)
                    })
                    .map_err(|e| TriggerError::Condition(e.to_string()))?,
            ),
            None => None,
        };
        self.next_id += 1;
        let id = self.next_id;
        self.triggers.push(Trigger {
            id,
            kind,
            condition: parsed,
            scope,
            hits: 0,
        });
        Ok(id)
    }

    /// The triggers that match this event and pass their condition.
    pub fn fire(&mut self, event: &TriggerKind) -> Vec<u64> {
        let mut fired = Vec::new();
        for t in &mut self.triggers {
            if &t.kind == event {
                t.hits += 1;
                if t.condition_passes() {
                    fired.push(t.id);
                }
            }
        }
        fired
    }

    /// The triggers that a runtime event fires.
    ///
    /// The event carries the runtime data (address, level, phase); the
    /// registry matches it against each registered trigger kind.
    pub fn fire_event(&mut self, event: &TriggerEvent) -> Vec<u64> {
        let mut fired = Vec::new();
        for t in &mut self.triggers {
            let matches = match (&t.kind, event) {
                (TriggerKind::ReadWatch { address: watch }, TriggerEvent::Read(a)) => a == watch,
                (TriggerKind::WriteWatch { address: watch }, TriggerEvent::Write(a)) => a == watch,
                (TriggerKind::IoRead { port }, TriggerEvent::Read(a)) => a == port,
                (TriggerKind::IoWrite { port }, TriggerEvent::Write(a)) => a == port,
                (TriggerKind::Irq, TriggerEvent::Irq) => true,
                (TriggerKind::Fetch, TriggerEvent::Fetch) => true,
                (TriggerKind::Dma { phase: want }, TriggerEvent::Dma(got)) => want == got,
                (TriggerKind::Vblank, TriggerEvent::Vblank) => true,
                (TriggerKind::Scanline { number: want }, TriggerEvent::Scanline(got)) => {
                    want == got
                }
                (
                    TriggerKind::LineChange {
                        device: wd,
                        line: wl,
                        level: want,
                    },
                    TriggerEvent::Line {
                        device: gd,
                        line: gl,
                        level: got,
                    },
                ) => wd == gd && wl == gl && want == got,
                _ => false,
            };
            if matches {
                t.hits += 1;
                if t.condition_passes() {
                    fired.push(t.id);
                }
            }
        }
        fired
    }

    /// All registered triggers.
    pub fn all(&self) -> &[Trigger] {
        &self.triggers
    }

    /// Remove a trigger by id.
    pub fn clear(&mut self, id: u64) -> bool {
        let before = self.triggers.len();
        self.triggers.retain(|t| t.id != id);
        self.triggers.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_fires_on_fetch_at_address() {
        let mut r = TriggerRegistry::new();
        let id = r
            .add(
                TriggerKind::Breakpoint {
                    address: Address(0x100),
                },
                None,
                BTreeMap::new(),
            )
            .unwrap();
        assert_eq!(
            r.fire(&TriggerKind::Fetch),
            Vec::<u64>::new(),
            "a fetch event does not fire a breakpoint"
        );
        assert_eq!(
            r.fire(&TriggerKind::Breakpoint {
                address: Address(0x100)
            }),
            vec![id]
        );
    }

    #[test]
    fn condition_gate_filters_fires() {
        let mut r = TriggerRegistry::new();
        let mut scope = BTreeMap::new();
        scope.insert("A".to_string(), 0u64);
        scope.insert("HL".to_string(), 0u64);
        let id = r
            .add(TriggerKind::Irq, Some("A == 0 && HL < 0x8000"), scope)
            .unwrap();
        assert_eq!(r.fire(&TriggerKind::Irq), vec![id]);
        // Change the scope value; the same registry re-evaluates.
        r.triggers[0].scope.insert("A".to_string(), 1);
        assert!(r.fire(&TriggerKind::Irq).is_empty());
        assert_eq!(r.triggers[0].hits, 2);
    }

    #[test]
    fn condition_with_unknown_ident_is_rejected_at_creation() {
        let mut r = TriggerRegistry::new();
        match r.add(TriggerKind::Irq, Some("NOPE == 1"), BTreeMap::new()) {
            Err(TriggerError::Condition(_)) => {}
            other => panic!("expected Condition error, got {other:?}"),
        }
    }

    #[test]
    fn clear_removes_by_id() {
        let mut r = TriggerRegistry::new();
        let id = r.add(TriggerKind::Vblank, None, BTreeMap::new()).unwrap();
        assert!(r.clear(id));
        assert!(!r.clear(id));
        assert_eq!(r.fire(&TriggerKind::Vblank), Vec::<u64>::new());
    }

    #[test]
    fn watchpoint_matches_address() {
        let mut r = TriggerRegistry::new();
        let id = r
            .add(
                TriggerKind::WriteWatch {
                    address: Address(0x2000),
                },
                None,
                BTreeMap::new(),
            )
            .unwrap();
        // The write event carries the address; the registry matches it
        // against the watch's address.
        let hits = r.fire_event(&TriggerEvent::Write(Address(0x2000)));
        assert_eq!(hits, vec![id]);
    }

    #[test]
    fn line_change_matches_device_and_level() {
        let mut r = TriggerRegistry::new();
        r.add(
            TriggerKind::LineChange {
                device: "ppu".into(),
                line: "vblank".into(),
                level: crate::signal::Level::High,
            },
            None,
            BTreeMap::new(),
        )
        .unwrap();
        let fired = r.fire_event(&TriggerEvent::Line {
            device: "ppu".into(),
            line: "vblank".into(),
            level: crate::signal::Level::High,
        });
        assert_eq!(fired.len(), 1);
    }
}
