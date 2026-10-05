//! Input scripts: the file grammar, validation, and the event scheduler.
//!
//! An input script is a text file with one event per line:
//!
//! ```text
//! #<domain>:<ticks> <device> <event>
//! #<ns> <device> <event>
//! ```
//!
//! The canonical stamp is the domain name and a tick count. The
//! nanosecond form `#<ns>` converts against the machine's primary clock
//! domain at load with the round-half-to-even boundary rule, so the two
//! forms describe the same timeline. The primary domain is the first
//! clock name in the resolved name-sorted clock map, the domain the
//! scheduler treats as primary. Blank lines are ignored. A `#`
//! before any other token or after the timestamp position begins a
//! comment that runs to the end of the line. A `label <name>` line marks
//! the start of a cyclic section; a `repeat <name> <count>` line repeats
//! the events since the matching label `count` times total.
//!
//! A repeated section shifts each copy by the section's own span: the
//! last event tick minus the first event tick. The first event of copy
//! `k` therefore lands exactly on the last event tick of copy `k-1`; the
//! copies fire in script order at that shared tick, which yields the
//! release-then-repress form cyclic input patterns need. A section must
//! use one timestamp form (all domain stamps on the same domain, or all
//! nanosecond stamps); mixed forms are a [`ScriptError::MixedRepeat`].
//!
//! Every timestamp that resolves stays exact: domain stamps keep their
//! tick values, and the replay scheduler fires an event when the primary
//! domain's exact rational time reaches the event instant.

use demu_config::model::SystemConfig;

use crate::frequency::{Frequency, Tick};
use crate::machine::Machine;
use crate::scheduler::compare_instants;
use crate::triggers::TriggerEvent;

/// One timed event of a resolved input script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEvent {
    /// The clock domain the event stamp is taken on.
    pub domain: String,
    /// The event's tick on its domain.
    pub ticks: u64,
    /// The target device name from the configuration.
    pub device: String,
    /// The event text in the device family's own grammar.
    pub event: String,
    /// The source line of the event, one-based, for error messages.
    pub line: usize,
}

/// A parse-time timestamp stamp before resolution against a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stamp {
    /// The canonical form: a named domain and a tick count.
    Domain { name: String, ticks: u64 },
    /// The nanosecond form, resolved at load against the primary domain.
    Nanos(u64),
}

/// One parsed script line before device validation.
#[derive(Debug, Clone)]
struct RawEvent {
    stamp: Stamp,
    device: String,
    event: String,
    line: usize,
}

/// An input script: the parsed timestamp grammar with cyclic sections.
#[derive(Debug, Clone, Default)]
pub struct InputScript {
    events: Vec<ScriptEvent>,
}

impl InputScript {
    /// Parse a script against a configuration: expand cyclic sections,
    /// convert nanosecond stamps, and validate the device and domain
    /// names.
    ///
    /// # Errors
    ///
    /// Returns a [`ScriptError`] with the offending source line when the
    /// grammar, the names, or a cyclic section is invalid.
    pub fn parse(text: &str, config: &SystemConfig) -> Result<Self, ScriptError> {
        let raw = parse_lines(text)?;
        let expanded = expand_repeats(raw)?;
        let mut events = Vec::with_capacity(expanded.len());
        for raw in expanded {
            let (domain, ticks) = match raw.stamp {
                Stamp::Domain { name, ticks } => {
                    if !config.clocks.contains_key(&name) {
                        return Err(ScriptError::UnknownDomain {
                            line: raw.line,
                            name,
                        });
                    }
                    (name, ticks)
                }
                Stamp::Nanos(ns) => {
                    let Some(domain) = config.clocks.values().next() else {
                        return Err(ScriptError::UnknownDomain {
                            line: raw.line,
                            name: String::new(),
                        });
                    };
                    // The nanosecond form converts against the machine's
                    // primary domain: the first clock name in the
                    // resolved name-sorted clock map, the domain the
                    // scheduler treats as primary.
                    let frequency = Frequency::new(domain.frequency.num(), domain.frequency.den())
                        .map_err(|_| ScriptError::Syntax {
                            line: raw.line,
                            message: "the clock domain frequency is invalid".to_string(),
                        })?;
                    (domain.name.clone(), frequency.ns_to_ticks(ns))
                }
            };
            let device = demu_config::newtypes::DeviceName::new(&raw.device).map_err(|_| {
                ScriptError::BadDevice {
                    line: raw.line,
                    name: raw.device.clone(),
                }
            })?;
            if !config.devices_by_name.contains_key(&device) {
                return Err(ScriptError::UnknownDevice {
                    line: raw.line,
                    name: raw.device,
                });
            }
            events.push(ScriptEvent {
                domain,
                ticks,
                device: raw.device,
                event: raw.event,
                line: raw.line,
            });
        }
        Ok(Self { events })
    }

    /// Assemble a script from events another source produced, for
    /// example a Matroska input track.
    ///
    /// The events replay through [`InputScheduler::fire_due`], which
    /// resolves the device and the domain against the running machine
    /// and rejects unknown names.
    #[must_use]
    pub fn from_events(events: Vec<ScriptEvent>) -> Self {
        Self { events }
    }

    /// The script events in script order.
    #[must_use]
    pub fn events(&self) -> &[ScriptEvent] {
        &self.events
    }
}

/// Parse the script lines into raw lines with cyclic-section markers.
///
/// A `label` line carries the label name; a `repeat` line carries the
/// name and the total run count.
fn parse_lines(text: &str) -> Result<Vec<RawLine>, ScriptError> {
    let mut lines = Vec::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // A `#` in the timestamp position begins an event stamp; a
        // standalone `#` token or a `#` later in the line begins a
        // comment.
        let (body, _comment) = match trimmed.split_once(" #") {
            Some((body, _)) => (body.trim(), true),
            None => (trimmed, false),
        };
        let mut words = body.split_whitespace();
        let first = words.next().unwrap_or("#");
        if let Some(rest) = first.strip_prefix('#') {
            if rest.is_empty() {
                continue; // a standalone `#` comment line
            }
            // A `#` token is an event stamp only when it carries the
            // stamp shape: a `domain:ticks` pair or a digit run. Any
            // other `#` token begins a comment line.
            if !rest.contains(':') && !rest.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            let stamp = parse_stamp(rest, line)?;
            let device = words
                .next()
                .ok_or(ScriptError::MissingDevice { line })?
                .to_string();
            let event: Vec<&str> = words.collect();
            if event.is_empty() {
                return Err(ScriptError::MissingEvent { line });
            }
            lines.push(RawLine::Event(RawEvent {
                stamp,
                device,
                event: event.join(" "),
                line,
            }));
            continue;
        }
        if first == "label" {
            let name = words
                .next()
                .ok_or(ScriptError::MissingLabelName { line })?
                .to_string();
            check_name(&name, line)?;
            lines.push(RawLine::Label { name, line });
            continue;
        }
        if first == "repeat" {
            let name = words
                .next()
                .ok_or(ScriptError::MissingLabelName { line })?
                .to_string();
            let count_text = words
                .next()
                .ok_or(ScriptError::MissingRepeatCount { line })?;
            let count = demu_config::model::parse_number(count_text).ok_or(
                ScriptError::BadRepeatCount {
                    line,
                    text: count_text.to_string(),
                },
            )?;
            lines.push(RawLine::Repeat { name, count, line });
            continue;
        }
        return Err(ScriptError::Syntax {
            line,
            message: format!("expected a timestamp event, `label`, or `repeat`, got `{first}`"),
        });
    }
    Ok(lines)
}

/// Parse one timestamp token into a stamp.
fn parse_stamp(text: &str, line: usize) -> Result<Stamp, ScriptError> {
    if let Some((domain, ticks_text)) = text.split_once(':') {
        let name = domain.trim();
        if name.is_empty() || name.chars().any(char::is_whitespace) || name.contains('#') {
            return Err(ScriptError::BadDomain {
                line,
                name: domain.to_string(),
            });
        }
        let ticks =
            demu_config::model::parse_number(ticks_text).ok_or(ScriptError::BadTimestamp {
                line,
                text: format!("#{text}"),
            })?;
        return Ok(Stamp::Domain {
            name: name.to_string(),
            ticks,
        });
    }
    let ns = demu_config::model::parse_number(text).ok_or(ScriptError::BadTimestamp {
        line,
        text: format!("#{text}"),
    })?;
    Ok(Stamp::Nanos(ns))
}

/// A label name: non-empty, no whitespace, no `#`.
fn check_name(name: &str, line: usize) -> Result<(), ScriptError> {
    if name.is_empty() || name.chars().any(char::is_whitespace) || name.contains('#') {
        return Err(ScriptError::BadName {
            line,
            name: name.to_string(),
        });
    }
    Ok(())
}

/// One parsed script line before cyclic expansion.
#[derive(Debug, Clone)]
enum RawLine {
    Event(RawEvent),
    Label {
        name: String,
        line: usize,
    },
    Repeat {
        name: String,
        count: u64,
        line: usize,
    },
}

/// Expand cyclic sections.
///
/// Each `repeat` copies the events of the section between its label and
/// the repeat line `count - 1` more times, shifted by the section span
/// per copy. The section keeps one stamp form and one domain.
fn expand_repeats(lines: Vec<RawLine>) -> Result<Vec<RawEvent>, ScriptError> {
    let mut events: Vec<RawEvent> = Vec::new();
    let mut labels: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for raw in lines {
        match raw {
            RawLine::Event(event) => events.push(event),
            RawLine::Label { name, line } => {
                check_name(&name, line)?;
                if labels.contains_key(&name) {
                    return Err(ScriptError::DuplicateLabel { line, name });
                }
                labels.insert(name, events.len());
            }
            RawLine::Repeat { name, count, line } => {
                let start = *labels.get(&name).ok_or(ScriptError::UnknownLabel {
                    line,
                    name: name.clone(),
                })?;
                if count == 0 {
                    return Err(ScriptError::BadRepeatCount {
                        line,
                        text: "0".to_string(),
                    });
                }
                let end = events.len();
                if end <= start {
                    return Err(ScriptError::EmptyRepeat { line, name });
                }
                // The copies clone out of the section first: pushing to
                // `events` while borrowing the slice would alias.
                let section: Vec<RawEvent> = events[start..end].to_vec();
                let span = stamp_span(&section).ok_or_else(|| ScriptError::MixedRepeat {
                    line,
                    name: name.clone(),
                })?;
                if span == 0 {
                    return Err(ScriptError::DegenerateRepeat { line, name });
                }
                for cycle in 1..count {
                    for source in &section {
                        let mut copy = source.clone();
                        copy.line = line;
                        shift_stamp(&mut copy.stamp, cycle * span);
                        events.push(copy);
                    }
                }
            }
        }
    }
    Ok(events)
}

/// The span of a cyclic section, `None` when the stamps cannot shift.
fn stamp_span(section: &[RawEvent]) -> Option<u64> {
    let first = section.first()?;
    let last = section.last()?;
    match (&first.stamp, &last.stamp) {
        (Stamp::Nanos(a), Stamp::Nanos(b)) => Some(b.saturating_sub(*a)),
        (
            Stamp::Domain {
                name: n1, ticks: a, ..
            },
            Stamp::Domain {
                name: n2, ticks: b, ..
            },
        ) if n1 == n2 => Some(b.saturating_sub(*a)),
        _ => None,
    }
}

/// Shift one stamp by `delta` in its own unit.
fn shift_stamp(stamp: &mut Stamp, delta: u64) {
    match stamp {
        Stamp::Nanos(ns) => *ns = ns.saturating_add(delta),
        Stamp::Domain { ticks, .. } => *ticks = ticks.saturating_add(delta),
    }
}

/// A script text failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScriptError {
    /// The timestamp does not parse.
    #[error("line {line}: bad timestamp `{text}`")]
    BadTimestamp { line: usize, text: String },
    /// The event line names no device.
    #[error("line {line}: the event line needs a device name")]
    MissingDevice { line: usize },
    /// The event line carries no event text.
    #[error("line {line}: the event line needs event text")]
    MissingEvent { line: usize },
    /// The device name is malformed.
    #[error("line {line}: bad device name `{name}`")]
    BadDevice { line: usize, name: String },
    /// The domain name is malformed.
    #[error("line {line}: bad domain `{name}`")]
    BadDomain { line: usize, name: String },
    /// The label or repeat name is malformed.
    #[error("line {line}: bad name `{name}`")]
    BadName { line: usize, name: String },
    /// The `label` line names no label.
    #[error("line {line}: the label line needs a name")]
    MissingLabelName { line: usize },
    /// The `repeat` line carries no count.
    #[error("line {line}: the repeat line needs a count")]
    MissingRepeatCount { line: usize },
    /// The repeat count does not parse or is zero.
    #[error("line {line}: bad repeat count `{text}`")]
    BadRepeatCount { line: usize, text: String },
    /// The line is not an event, a label, or a repeat.
    #[error("line {line}: {message}")]
    Syntax { line: usize, message: String },
    /// The script stamps a clock domain that the machine does not
    /// declare.
    #[error("line {line}: unknown clock domain `{name}`")]
    UnknownDomain { line: usize, name: String },
    /// The script names a device the configuration does not declare.
    #[error("line {line}: unknown device `{name}`")]
    UnknownDevice { line: usize, name: String },
    /// The label name is already in use.
    #[error("line {line}: duplicate label `{name}`")]
    DuplicateLabel { line: usize, name: String },
    /// The `repeat` names a label the script has not declared before it.
    #[error("line {line}: unknown label `{name}`")]
    UnknownLabel { line: usize, name: String },
    /// The repeated section has no events.
    #[error("line {line}: the repeated section `{name}` is empty")]
    EmptyRepeat { line: usize, name: String },
    /// The repeated section spans no time, so copies would coincide.
    #[error("line {line}: the repeated section `{name}` has zero span")]
    DegenerateRepeat { line: usize, name: String },
    /// The repeated section mixes stamp forms or domains.
    #[error("line {line}: the repeated section `{name}` mixes timestamp forms")]
    MixedRepeat { line: usize, name: String },
}

/// A replay-time failure of one input event.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InputRunError {
    /// The device's input sink rejected the event text.
    #[error("line {line}: device `{device}` rejected event `{event}`")]
    Rejected {
        /// The device that rejected the event.
        device: String,
        /// The rejected event text.
        event: String,
        /// The script line of the event, or 0 for another source.
        line: usize,
    },
    /// The device exists but exposes no input sink.
    #[error("device `{0}` accepts no input")]
    NotInputSink(String),
    /// The machine does not declare the device.
    #[error("unknown device `{0}`")]
    UnknownDevice(String),
    /// The machine does not declare the event's clock domain.
    #[error("unknown clock domain `{0}`")]
    UnknownDomain(String),
    /// The machine cannot reach the next event's exact time.
    #[error("the machine cannot reach the next input event")]
    Stalled,
}

/// The replay state of a loaded input script.
#[derive(Debug, Default)]
pub struct InputScheduler {
    events: Vec<ScriptEvent>,
    next: usize,
}

impl InputScheduler {
    /// A scheduler for a resolved script.
    #[must_use]
    pub fn new(script: InputScript) -> Self {
        Self {
            events: script.events,
            next: 0,
        }
    }

    /// An empty scheduler with no pending events.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The number of events not yet applied.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.events.len().saturating_sub(self.next)
    }

    /// The next unapplied event, if any.
    #[must_use]
    pub fn next_event(&self) -> Option<&ScriptEvent> {
        self.events.get(self.next)
    }

    /// The smallest primary-domain tick at which the next event's exact
    /// real time is reached, or `None` when no event is pending.
    ///
    /// The event fires at tick `p`, the smallest primary tick whose real
    /// time is at or past the event instant: `p = ceil(t * d_e * n_p /
    /// (n_e * d_p))`, compared exactly in `u128`.
    #[must_use]
    pub fn primary_stop_tick(&self, machine: &Machine) -> Option<Tick> {
        let event = self.next_event()?;
        let domain = machine.scheduler.domain(&event.domain)?;
        let ev_freq = machine.scheduler.domains[domain].frequency;
        let pr_freq = machine.scheduler.domains[machine.scheduler.primary].frequency;
        // Real time of the event instant: t * den_e / num_e seconds. The
        // primary reaches it at p = t * den_e * num_p / (num_e * den_p),
        // rounded up so the event fires no earlier than its instant.
        let numerator =
            u128::from(event.ticks) * u128::from(ev_freq.den()) * u128::from(pr_freq.num());
        let denominator = u128::from(ev_freq.num()) * u128::from(pr_freq.den());
        let stop = numerator.div_ceil(denominator);
        Some(Tick(u64::try_from(stop).unwrap_or(u64::MAX)))
    }

    /// Apply every pending event whose exact real time has passed.
    ///
    /// Events apply in script order. Each event injects into its device
    /// through the device family's own event grammar.
    ///
    /// # Errors
    ///
    /// Returns [`InputRunError`] when a device, its input sink, or its
    /// domain is unknown, or when the device rejects the event text.
    pub fn fire_due(&mut self, machine: &mut Machine) -> Result<usize, InputRunError> {
        let primary = machine.scheduler.primary;
        let now = machine.now();
        let now_freq = machine.scheduler.domains[primary].frequency;
        let mut fired = 0usize;
        while let Some(event) = self.events.get(self.next) {
            let Some(domain) = machine.scheduler.domain(&event.domain) else {
                return Err(InputRunError::UnknownDomain(event.domain.clone()));
            };
            let instant = (
                Tick(event.ticks),
                machine.scheduler.domains[domain].frequency,
            );
            if compare_instants(instant, (now, now_freq)) == std::cmp::Ordering::Greater {
                break;
            }
            let line = event.line;
            let device = event.device.clone();
            let text = event.event.clone();
            let applied = {
                let target = machine
                    .device_by_name_mut(&device)
                    .ok_or_else(|| InputRunError::UnknownDevice(device.clone()))?;
                let Some(sink) = target.input_mut() else {
                    return Err(InputRunError::NotInputSink(device));
                };
                sink.inject(&text)
            };
            if !applied {
                return Err(InputRunError::Rejected {
                    device,
                    event: text,
                    line,
                });
            }
            self.next += 1;
            fired += 1;
        }
        Ok(fired)
    }
}

/// Run the machine toward `target` while the scheduler applies its
/// events at their exact instants.
///
/// The loop splits the run at each pending event's stop tick: the
/// machine runs to the smaller of the target and the next event's exact
/// time, the due events inject in script order, and the run continues.
/// A runtime event that makes `hook` return true ends the run with the
/// events after the stop time pending for the next call.
///
/// # Errors
///
/// Returns [`InputRunError`] when an event cannot apply, or when the
/// machine parks and can no longer reach a pending event's time.
pub fn run_until_with_input(
    machine: &mut Machine,
    scheduler: &mut InputScheduler,
    target: Tick,
    hook: &mut dyn FnMut(&TriggerEvent) -> bool,
    emit: bool,
) -> Result<(), InputRunError> {
    let mut stopped = false;
    loop {
        // Events already due at the current time apply first, in
        // script order.
        scheduler.fire_due(machine)?;
        let mut inner = |event: &TriggerEvent| {
            let stop = hook(event);
            if stop {
                stopped = true;
            }
            stop
        };
        if scheduler.pending() == 0 {
            machine.run_hook(target, &mut inner, emit);
            return Ok(());
        }
        let event_stop = scheduler
            .primary_stop_tick(machine)
            .ok_or(InputRunError::Stalled)?;
        let stop = target.min(event_stop);
        machine.run_hook(stop, &mut inner, emit);
        if stopped {
            // A trigger hit inside the segment; the remaining events
            // keep their pending state for the next run.
            return Ok(());
        }
        let fired = scheduler.fire_due(machine)?;
        if machine.now() >= target {
            return Ok(());
        }
        // Each round either fires an event or runs the machine; with
        // neither, the machine is parked and cannot reach the pending
        // event.
        if fired == 0 {
            return Err(InputRunError::Stalled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::Machine;

    fn smoke_config() -> SystemConfig {
        demu_config::smoke_config().expect("smoke config is valid")
    }

    fn script_of(text: &str) -> InputScript {
        InputScript::parse(text, &smoke_config()).expect("script parses")
    }

    #[test]
    fn canonical_timestamps_parse() {
        let script = script_of("#main:100 cpu0 step\n");
        assert_eq!(script.events().len(), 1);
        let event = &script.events()[0];
        assert_eq!(event.domain, "main");
        assert_eq!(event.ticks, 100);
        assert_eq!(event.device, "cpu0");
    }

    #[test]
    fn hex_ticks_parse() {
        let script = script_of("#main:0x10 cpu0 nop\n");
        assert_eq!(script.events()[0].ticks, 16);
    }

    #[test]
    fn nanosecond_form_converts_against_primary_domain() {
        // The smoke machine's primary domain runs at 4194304 Hz. One
        // nanosecond is 4194304 / 1e9 ticks, so 238 ns rounds to 1.
        let script = script_of("#238 cpu0 nop\n");
        assert_eq!(script.events()[0].ticks, 1);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let script = script_of(
            "# a leading comment\n\
             \n\
             #main:1 cpu0 nop   # a trailing comment\n\
             #just a comment\n\
             #main:2 cpu0 nop\n",
        );
        assert_eq!(
            script.events().iter().map(|e| e.ticks).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn labels_and_repeats_expand_cyclic_sections() {
        let script = script_of(
            "#main:10 cpu0 nop\n\
             label walk\n\
             #main:20 cpu0 nop\n\
             #main:30 cpu0 nop\n\
             repeat walk 3\n\
             #main:40 cpu0 nop\n",
        );
        let ticks: Vec<u64> = script.events().iter().map(|e| e.ticks).collect();
        // The section spans 10 ticks; copy 1 shifts by 10, copy 2 by 20.
        assert_eq!(ticks, vec![10, 20, 30, 30, 40, 40, 50, 40]);
    }

    #[test]
    fn repeat_errors_for_unknown_empty_and_mixed_sections() {
        assert!(matches!(
            InputScript::parse("repeat walk 2\n", &smoke_config()),
            Err(ScriptError::UnknownLabel { .. })
        ));
        assert!(matches!(
            InputScript::parse("label walk\nrepeat walk 2\n", &smoke_config()),
            Err(ScriptError::EmptyRepeat { .. })
        ));
        assert!(matches!(
            InputScript::parse(
                "label walk\n#main:10 cpu0 nop\n#9 cpu0 nop\nrepeat walk 2\n",
                &smoke_config()
            ),
            Err(ScriptError::MixedRepeat { .. })
        ));
        assert!(matches!(
            InputScript::parse(
                "label walk\n#main:10 cpu0 nop\nlabel walk\n",
                &smoke_config()
            ),
            Err(ScriptError::DuplicateLabel { .. })
        ));
        // A zero-span section cannot cycle.
        assert!(matches!(
            InputScript::parse(
                "label walk\n#main:10 cpu0 nop\n#main:10 cpu0 nop\nrepeat walk 2\n",
                &smoke_config()
            ),
            Err(ScriptError::DegenerateRepeat { .. })
        ));
    }

    #[test]
    fn unknown_devices_and_domains_are_rejected() {
        assert!(matches!(
            InputScript::parse("#smp:10 cpu0 nop\n", &smoke_config()),
            Err(ScriptError::UnknownDomain { .. })
        ));
        assert!(matches!(
            InputScript::parse("#main:10 dsp0 nop\n", &smoke_config()),
            Err(ScriptError::UnknownDevice { .. })
        ));
        assert!(matches!(
            InputScript::parse("#main:10 2bad nop\n", &smoke_config()),
            Err(ScriptError::BadDevice { .. })
        ));
        assert!(matches!(
            InputScript::parse("#main: cpu0 nop\n", &smoke_config()),
            Err(ScriptError::BadTimestamp { .. })
        ));
        assert!(matches!(
            InputScript::parse("#main:zz cpu0 nop\n", &smoke_config()),
            Err(ScriptError::BadTimestamp { .. })
        ));
    }

    #[test]
    fn event_lines_without_device_or_text_are_rejected() {
        assert!(matches!(
            InputScript::parse("#main:10\n", &smoke_config()),
            Err(ScriptError::MissingDevice { .. })
        ));
        assert!(matches!(
            InputScript::parse("#main:10 cpu0\n", &smoke_config()),
            Err(ScriptError::MissingEvent { .. })
        ));
        assert!(matches!(
            InputScript::parse("nop\n", &smoke_config()),
            Err(ScriptError::Syntax { .. })
        ));
    }

    #[test]
    fn scheduler_fires_due_events_in_order() {
        let mut machine = Machine::new(smoke_config()).expect("machine assembles");
        machine.boot();
        let script = script_of(
            "#main:5 cpu0 nop\n\
             #main:5 cpu0 nop2\n\
             #main:9 cpu0 nop3\n",
        );
        let mut scheduler = InputScheduler::new(script);
        assert_eq!(scheduler.pending(), 3);

        // Before tick 5 nothing is due.
        assert_eq!(scheduler.fire_due(&mut machine).unwrap(), 0);
        machine.run_until(Tick(5));
        assert_eq!(scheduler.fire_due(&mut machine).unwrap(), 2);
        assert_eq!(scheduler.pending(), 1);

        // The stop tick lands on the next event's exact instant.
        assert_eq!(scheduler.primary_stop_tick(&machine), Some(Tick(9)));
        machine.run_until(Tick(9));
        assert_eq!(scheduler.fire_due(&mut machine).unwrap(), 1);
        assert_eq!(scheduler.pending(), 0);
        assert!(scheduler.next_event().is_none());
        assert!(scheduler.primary_stop_tick(&machine).is_none());

        // The events applied in script order at the exact instants.
        let cpu = machine
            .device_by_name("cpu0")
            .and_then(|d| {
                d.as_any()
                    .downcast_ref::<crate::machine::devices::SmokeCpu>()
            })
            .expect("cpu0 exists");
        assert_eq!(
            cpu.input_log(),
            &["nop".to_string(), "nop2".to_string(), "nop3".to_string()]
        );
    }

    #[test]
    fn scheduler_rejects_unknown_devices_and_bad_events() {
        let mut machine = Machine::new(smoke_config()).expect("machine assembles");
        machine.boot();
        // `rom0` is a ROM block and exposes no input sink.
        let script = InputScript::from_events(vec![ScriptEvent {
            domain: "main".to_string(),
            ticks: 1,
            device: "rom0".to_string(),
            event: "nop".to_string(),
            line: 1,
        }]);
        let mut scheduler = InputScheduler::new(script);
        machine.run_until(Tick(1));
        assert!(matches!(
            scheduler.fire_due(&mut machine),
            Err(InputRunError::NotInputSink(_))
        ));

        // A device the machine does not declare.
        let script = InputScript::from_events(vec![ScriptEvent {
            domain: "main".to_string(),
            ticks: 1,
            device: "ghost".to_string(),
            event: "nop".to_string(),
            line: 2,
        }]);
        let mut scheduler = InputScheduler::new(script);
        assert!(matches!(
            scheduler.fire_due(&mut machine),
            Err(InputRunError::UnknownDevice(_))
        ));
    }

    #[test]
    fn runtime_unknown_domains_are_rejected() {
        let mut machine = Machine::new(smoke_config()).expect("machine assembles");
        machine.boot();
        let script = InputScript::from_events(vec![ScriptEvent {
            domain: "ghost".to_string(),
            ticks: 1,
            device: "cpu0".to_string(),
            event: "nop".to_string(),
            line: 1,
        }]);
        let mut scheduler = InputScheduler::new(script);
        machine.run_until(Tick(1));
        assert!(matches!(
            scheduler.fire_due(&mut machine),
            Err(InputRunError::UnknownDomain(_))
        ));
    }

    #[test]
    fn run_until_with_input_runs_a_scripted_session() {
        let mut machine = Machine::new(smoke_config()).expect("machine assembles");
        machine.boot();
        let script = script_of("#main:2 cpu0 first\n#main:4 cpu0 second\n");
        let mut scheduler = InputScheduler::new(script);
        run_until_with_input(
            &mut machine,
            &mut scheduler,
            Tick(100),
            &mut |_| false,
            true,
        )
        .expect("the session runs");
        assert_eq!(scheduler.pending(), 0);
        let cpu = machine
            .device_by_name("cpu0")
            .and_then(|d| {
                d.as_any()
                    .downcast_ref::<crate::machine::devices::SmokeCpu>()
            })
            .expect("cpu0 exists");
        assert_eq!(
            cpu.input_log(),
            &["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn a_parked_machine_stalls_a_pending_event() {
        // A machine with no CPU: the queues stay empty, so a pending
        // event with no reachable time cannot apply.
        use demu_config::model::{Bus, ClockDomain, DeviceInstance, Window};
        use demu_config::newtypes::{DeviceName, KindName};
        let mut params = std::collections::BTreeMap::new();
        params.insert("size".to_string(), toml::Value::Integer(8));
        let ram = DeviceInstance {
            name: DeviceName::new("ram0").expect("name"),
            kind: KindName::new("smoke.ram").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: std::collections::BTreeMap::new(),
        };
        let clocks = std::iter::once((
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(4_194_304, 1).expect("freq"),
                origin: String::new(),
            },
        ))
        .collect();
        let mut devices_by_name = std::collections::BTreeMap::new();
        devices_by_name.insert(ram.name.clone(), 0usize);
        let config = SystemConfig {
            name: "parked".to_string(),
            triple: "parked".to_string(),
            description: String::new(),
            clocks,
            devices: vec![ram],
            devices_by_name,
            buses: vec![Bus {
                name: "bus".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![Window {
                name: "ram".to_string(),
                bus: "bus".to_string(),
                device: DeviceName::new("ram0").expect("name"),
                base: 0,
                size: 8,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            }],
            connections: Vec::new(),
            signals: Vec::new(),
            boot: std::collections::BTreeMap::new(),
            quirks: std::collections::BTreeMap::new(),
        };
        let mut machine =
            Machine::assemble(config, &crate::machine::smoke_ctors()).expect("assembles");
        machine.boot();
        let mut scheduler = InputScheduler::new(InputScript::from_events(vec![ScriptEvent {
            domain: "main".to_string(),
            ticks: u64::MAX,
            device: "ram0".to_string(),
            event: "nop".to_string(),
            line: 1,
        }]));
        assert!(matches!(
            run_until_with_input(
                &mut machine,
                &mut scheduler,
                Tick(u64::MAX),
                &mut |_| false,
                true
            ),
            Err(InputRunError::Stalled)
        ));
        assert_eq!(scheduler.pending(), 1, "the event stayed pending");
    }
}
