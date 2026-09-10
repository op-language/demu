//! Signal lines.
//!
//! A line carries one bit between devices. A line has a level, edge
//! events, and, where the hardware needs it, open-collector semantics:
//! several drivers pull the line low, no driver pulls it high.

/// The level of a signal line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Level {
    /// The line is low.
    Low,
    /// The line is high.
    High,
}

impl Level {
    /// The level as a boolean: high is true.
    pub fn is_high(&self) -> bool {
        matches!(self, Level::High)
    }
}

/// The electrical mode of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriveMode {
    /// One driver sets the level in both directions.
    PushPull,
    /// Drivers pull low only; the line is high when no driver pulls.
    OpenCollector,
}

/// A signal line instance.
#[derive(Debug, Clone)]
pub struct SignalLine {
    mode: DriveMode,
    level: Level,
    /// The number of active low drivers, for open-collector lines.
    pulls: u32,
}

impl SignalLine {
    /// A push-pull line at the given initial level.
    pub fn push_pull(level: Level) -> Self {
        Self {
            mode: DriveMode::PushPull,
            level,
            pulls: 0,
        }
    }

    /// An open-collector line. The idle level is high.
    pub fn open_collector() -> Self {
        Self {
            mode: DriveMode::OpenCollector,
            level: Level::High,
            pulls: 0,
        }
    }

    /// The current level.
    pub fn level(&self) -> Level {
        self.level
    }

    /// The electrical mode.
    pub fn mode(&self) -> DriveMode {
        self.mode
    }

    /// Set the level on a push-pull line.
    ///
    /// Returns true when the level changed, which is an edge event.
    pub fn set(&mut self, level: Level) -> bool {
        debug_assert!(
            self.mode == DriveMode::PushPull,
            "set on a push-pull line only"
        );
        if self.level == level {
            false
        } else {
            self.level = level;
            true
        }
    }

    /// One driver starts pulling the open-collector line low.
    ///
    /// Returns true when the line fell, which is an edge event.
    pub fn pull_low(&mut self) -> bool {
        debug_assert!(self.mode == DriveMode::OpenCollector);
        self.pulls += 1;
        if self.level != Level::Low {
            self.level = Level::Low;
            true
        } else {
            false
        }
    }

    /// One driver releases the open-collector line.
    ///
    /// Returns true when the line rose, which is an edge event.
    pub fn release(&mut self) -> bool {
        debug_assert!(self.mode == DriveMode::OpenCollector);
        self.pulls = self.pulls.saturating_sub(1);
        if self.pulls == 0 && self.level == Level::Low {
            self.level = Level::High;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pull_edges() {
        let mut line = SignalLine::push_pull(Level::Low);
        assert!(!line.set(Level::Low));
        assert!(line.set(Level::High));
        assert!(line.level().is_high());
    }

    #[test]
    fn open_collector_shared_drive() {
        let mut line = SignalLine::open_collector();
        assert!(line.level().is_high());
        assert!(line.pull_low());
        assert!(!line.level().is_high());
        // A second driver pulls; releasing one keeps the line low.
        assert!(!line.pull_low());
        assert!(!line.release());
        assert!(!line.level().is_high());
        // The last release raises the line.
        assert!(line.release());
        assert!(line.level().is_high());
        assert!(!line.release());
    }
}
