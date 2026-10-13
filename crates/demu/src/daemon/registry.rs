//! The session registry: names, caps, and worker lifecycle.
//!
//! The registry owns every live session. Its mutex guards only the
//! name-to-handle lookups; all machine work runs on the session worker
//! threads. Sessions of dead workers prune on the next registry
//! operation, so a crashed worker frees its slot and reports an
//! unknown-session error afterwards.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc;

use serde_json::Value;

use super::confinement::ConfinementDir;
use super::session::{
    SessionCreated, SessionError, SessionJob, SessionName, SessionSpec, WorkerInstruction,
    WorkerJob, resolve_source, spawn_worker,
};

/// The bookkeeping slot of one live session.
struct SessionHandle {
    /// The registration number of this handle; a recreated session
    /// name always rises to a new generation.
    generation: u64,
    /// The job queue sender; dropping it ends the worker.
    queue: mpsc::Sender<WorkerJob>,
    /// The worker thread; joined on destroy.
    thread: std::thread::JoinHandle<()>,
}

impl SessionHandle {
    /// Drop the queue sender and wait for the worker to exit.
    fn shutdown(self) -> Result<(), SessionError> {
        drop(self.queue);
        match self.thread.join() {
            Ok(()) => Ok(()),
            Err(payload) => Err(SessionError::Worker(format!(
                "the session worker panicked: {}",
                panic_message(&payload)
            ))),
        }
    }
}

/// The readable text of a panic payload, when it carries one.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic payload of unknown type".to_owned()
    }
}

/// The registry of live emulator sessions.
pub struct Registry {
    /// The live sessions, keyed by their validated names.
    sessions: Mutex<HashMap<SessionName, SessionHandle>>,
    /// The maximum number of concurrent sessions.
    limit: usize,
    /// The next session registration number.
    next_generation: AtomicU64,
}

impl Registry {
    /// A registry with the given concurrent-session limit.
    pub fn new(limit: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            limit,
            next_generation: AtomicU64::new(0),
        }
    }

    /// Take the session map, recovering a poisoned lock.
    fn lock(&self) -> MutexGuard<'_, HashMap<SessionName, SessionHandle>> {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop the handles whose workers have died.
    fn prune(map: &mut HashMap<SessionName, SessionHandle>) {
        map.retain(|_, handle| !handle.thread.is_finished());
    }

    /// Create one session and return its info plus the configuration
    /// lint warnings.
    ///
    /// The name and the session cap check synchronously, then the
    /// configuration parses and validates synchronously for fast
    /// errors, then the worker spawns and reserves its slot. The
    /// machine boot happens on the worker; the boot result returns
    /// over the creation reply. A failed boot leaves nothing
    /// registered. A concurrent destroy removes the registration
    /// while the boot reply waits, and the create then reports the
    /// name unknown.
    pub fn create(&self, spec: SessionSpec) -> Result<SessionCreated, SessionError> {
        {
            let mut map = self.lock();
            Self::prune(&mut map);
            if map.contains_key(&spec.name) {
                return Err(SessionError::Duplicate(spec.name));
            }
            if map.len() >= self.limit {
                return Err(SessionError::LimitReached { limit: self.limit });
            }
        }
        let SessionSpec {
            name,
            config,
            rom,
            symbols_text,
            output_dir,
        } = spec;
        let (raw, source_text) = resolve_source(&config)?;
        let (system, warnings) = demu_config::validate(&raw).map_err(SessionError::Validation)?;
        // The output directory exists and anchors before the worker
        // starts, so the confinement root is already resolved.
        let confined = ConfinementDir::create(&output_dir)?;
        let (boot_tx, boot_rx) = mpsc::channel();
        let (queue_tx, queue_rx) = mpsc::channel();
        let instruction =
            WorkerInstruction::new(system, source_text, rom, symbols_text, confined.clone());
        let thread = spawn_worker(instruction, boot_tx, queue_rx).map_err(|e| {
            SessionError::Worker(format!("failed to spawn the session worker: {e}"))
        })?;
        // Reserve the slot atomically, re-checking the races. Each
        // rejection drops its worker on the same diverging path, so
        // the moved sender and thread never flow past a return.
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut map = self.lock();
            Self::prune(&mut map);
            if map.contains_key(&name) {
                drop(map);
                discard_worker(queue_tx, thread);
                return Err(SessionError::Duplicate(name));
            }
            if map.len() >= self.limit {
                drop(map);
                discard_worker(queue_tx, thread);
                return Err(SessionError::LimitReached { limit: self.limit });
            }
            map.insert(
                name.clone(),
                SessionHandle {
                    generation,
                    queue: queue_tx,
                    thread,
                },
            );
        }
        match boot_rx.recv() {
            Ok(Ok(())) => {
                // A concurrent destroy removes the registration while
                // the boot reply waits, and a later recreation
                // registers a new generation under the same name.
                // Either way this create's machine is gone, and
                // reporting success for a session nobody registered
                // would be wrong.
                let registered = self
                    .lock()
                    .get(&name)
                    .is_some_and(|handle| handle.generation == generation);
                if !registered {
                    return Err(SessionError::Unknown(name));
                }
                Ok(SessionCreated {
                    name,
                    output_dir: confined.root().to_path_buf(),
                    warnings,
                })
            }
            Ok(Err(error)) => {
                if let Some(handle) = self.forget(&name, generation) {
                    let _ = handle.shutdown();
                }
                Err(error)
            }
            Err(_) => {
                // The reply channel closed without a report: the worker
                // died mid-boot. The shutdown error carries the panic
                // payload when the worker panicked.
                if let Some(handle) = self.forget(&name, generation) {
                    return Err(handle.shutdown().err().unwrap_or_else(|| {
                        SessionError::Worker("session worker exited during boot".to_owned())
                    }));
                }
                Err(SessionError::Worker(
                    "session worker exited during boot".to_owned(),
                ))
            }
        }
    }

    /// The machine info of one session.
    pub fn info(&self, name: &SessionName) -> Result<Value, SessionError> {
        self.submit(name, Box::new(|service, _confined| service.info()))
    }

    /// Send one command job to a session worker and wait for its
    /// reply.
    ///
    /// The job runs on the worker thread against the session's machine
    /// service and confinement directory; the result returns over the
    /// job's one-shot reply channel.
    pub fn submit(&self, name: &SessionName, job: SessionJob) -> Result<Value, SessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let (queue, generation) = {
            let mut map = self.lock();
            Self::prune(&mut map);
            let handle = map
                .get(name)
                .ok_or_else(|| SessionError::Unknown(name.clone()))?;
            (handle.queue.clone(), handle.generation)
        };
        if queue
            .send(WorkerJob {
                job,
                reply: reply_tx,
            })
            .is_err()
        {
            self.forget(name, generation);
            return Err(SessionError::Worker(
                "session worker died before the job ran".to_owned(),
            ));
        }
        drop(queue);
        match reply_rx.recv() {
            Ok(result) => result.map_err(SessionError::from),
            Err(_) => {
                // A dead worker closed the reply channel; forgetting the
                // registration here frees the slot and the name
                // immediately instead of waiting for the next prune.
                self.forget(name, generation);
                Err(SessionError::Worker(
                    "session worker died before replying".to_owned(),
                ))
            }
        }
    }

    /// Forget the registration of a session name whose worker is gone,
    /// and return its handle for the caller to join or detach.
    ///
    /// The removal binds to the generation the caller observed, so a
    /// pruned and recreated session under the same name stays
    /// unharmed. The caller owns the failure that proves its own
    /// registration is dead; prune only covers deaths nobody observed.
    fn forget(&self, name: &SessionName, generation: u64) -> Option<SessionHandle> {
        let mut map = self.lock();
        let handle = map.get(name)?;
        if handle.generation != generation {
            return None;
        }
        map.remove(name)
    }

    /// Destroy one session and wait until its worker exits.
    ///
    /// Remaining queued jobs still run before the worker exits.
    pub fn destroy(&self, name: &SessionName) -> Result<(), SessionError> {
        let handle = {
            let mut map = self.lock();
            Self::prune(&mut map);
            map.remove(name)
                .ok_or_else(|| SessionError::Unknown(name.clone()))?
        };
        handle.shutdown()
    }
}

/// Drop a worker that lost its slot reservation, and wait for it.
fn discard_worker(queue: mpsc::Sender<WorkerJob>, thread: std::thread::JoinHandle<()>) {
    drop(queue);
    let _ = thread.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::session::ConfigSource;
    use demu_shell::service::CommandError;

    /// The temp path of a test fixture, without touching the file system.
    fn tmp_path(case: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("demu-daemon-{case}"))
    }

    /// A temp directory cleared of any prior test artifact.
    fn tmp_dir(case: &str) -> std::path::PathBuf {
        let path = tmp_path(case);
        std::fs::remove_dir_all(&path).ok();
        path
    }

    /// A minimal Game Boy cartridge image: a jump frame and a parked tail.
    fn gb_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x108].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01, 0x00, 0x00, 0x00, 0x00]);
        rom[0x150..0x152].copy_from_slice(&[0x76, 0x76]);
        rom
    }

    /// A valid `.opl` symbol table with one function at the rom origin.
    fn symbols_text() -> String {
        serde_json::json!({
            "version": 1,
            "target": "sm83-nintendo-gameboy",
            "sections": [
                {
                    "name": "rom0",
                    "org": "0x0150",
                    "symbols": [
                        {"name": "main", "offset": 0, "size": 4, "kind": "function"}
                    ]
                }
            ]
        })
        .to_string()
    }

    /// A session spec on the bundled Game Boy triple.
    fn spec(name: &str, case: &str) -> SessionSpec {
        SessionSpec {
            name: SessionName::new(name).expect("the test name validates"),
            config: ConfigSource::Triple("sm83-nintendo-gameboy".to_owned()),
            rom: None,
            symbols_text: None,
            output_dir: tmp_dir(&format!("{case}-{name}")),
        }
    }

    #[test]
    fn the_registry_enforces_the_session_cap() {
        let registry = Registry::new(2);
        let first = registry
            .create(spec("alpha", "cap"))
            .expect("first session");
        assert!(first.warnings.is_empty(), "the game boy config lints clean");
        let second = registry
            .create(spec("beta", "cap"))
            .expect("second session");
        assert!(second.output_dir.exists(), "the output directory exists");
        assert!(matches!(
            registry.create(spec("gamma", "cap")),
            Err(SessionError::LimitReached { limit: 2 })
        ));
        // Destroying frees the slot for a new session.
        registry
            .destroy(&SessionName::new("alpha").expect("name"))
            .expect("destroy");
        registry
            .create(spec("gamma", "cap"))
            .expect("third session after destroy");
        register_and_destroy_all(&registry, ["beta", "gamma"]);
    }

    #[test]
    fn create_info_and_destroy_run_a_game_boy_session() {
        let registry = Registry::new(4);
        let mut game = spec("roundtrip", "lifecycle");
        game.rom = Some(gb_rom());
        let created = registry.create(game).expect("the session creates");
        assert_eq!(created.name.as_str(), "roundtrip");
        assert_eq!(
            created.output_dir,
            tmp_path("lifecycle-roundtrip")
                .canonicalize()
                .expect("the directory canonicalizes")
        );
        let info = registry
            .info(&SessionName::new("roundtrip").expect("name"))
            .expect("info");
        assert_eq!(info["name"], "Nintendo Game Boy");
        assert_eq!(info["triple"], "sm83-nintendo-gameboy");
        assert!(
            info["devices"]
                .as_array()
                .expect("device list")
                .iter()
                .any(|d| d.as_str() == Some("cart")),
            "the cartridge device is present"
        );
        registry
            .destroy(&SessionName::new("roundtrip").expect("name"))
            .expect("destroy");
        // The destroyed session is unknown, and its name is reusable.
        assert!(matches!(
            registry.info(&SessionName::new("roundtrip").expect("name")),
            Err(SessionError::Unknown(_))
        ));
        registry
            .create(spec("roundtrip", "lifecycle"))
            .expect("the name is reusable");
        registry
            .destroy(&SessionName::new("roundtrip").expect("name"))
            .expect("destroy");
    }

    #[test]
    fn duplicate_names_reject() {
        let registry = Registry::new(4);
        registry
            .create(spec("dupe", "dupe"))
            .expect("first session");
        assert!(matches!(
            registry.create(spec("dupe", "dupe")),
            Err(SessionError::Duplicate(_))
        ));
        register_and_destroy_all(&registry, ["dupe"]);
    }

    #[test]
    fn symbols_load_and_resolve_over_the_wire() {
        let registry = Registry::new(4);
        let mut game = spec("symbols", "symbols");
        game.rom = Some(gb_rom());
        game.symbols_text = Some(symbols_text());
        registry.create(game).expect("the session creates");
        let resolved = registry
            .submit(
                &SessionName::new("symbols").expect("name"),
                Box::new(|service, _confined| {
                    let addr = service.symbols().and_then(|lookup| lookup.resolve("main"));
                    Ok(serde_json::json!({"main": addr}))
                }),
            )
            .expect("the job runs");
        assert_eq!(
            resolved["main"], 0x0150,
            "the symbol resolves to its section origin"
        );
        register_and_destroy_all(&registry, ["symbols"]);
    }

    #[test]
    fn a_broken_symbol_table_fails_the_boot() {
        let registry = Registry::new(4);
        let mut game = spec("badsymbols", "symbols");
        game.symbols_text = Some("{ not json".to_owned());
        let error = registry.create(game).expect_err("the boot fails");
        assert!(
            matches!(error, SessionError::Symbols(_)),
            "the error names the parse failure: {error}"
        );
        // Nothing stays registered after the failed boot.
        assert!(matches!(
            registry.info(&SessionName::new("badsymbols").expect("name")),
            Err(SessionError::Unknown(_))
        ));
    }

    #[test]
    fn a_broken_toml_rejects_synchronously() {
        let registry = Registry::new(4);
        let mut game = spec("badtoml", "toml");
        game.config = ConfigSource::Toml("[system".to_owned());
        assert!(matches!(
            registry.create(game),
            Err(SessionError::Config(_))
        ));
    }

    #[test]
    fn an_invalid_config_rejects_with_the_report() {
        let toml = r#"
[system]
name = "broken"
triple = "test"
description = "an unknown device kind"
[clock.main]
frequency = "1000000"
[[device]]
name = "mystery"
kind = "no.such.kind"
domain = "main"
"#;
        let registry = Registry::new(4);
        let mut game = spec("invalid", "validation");
        game.config = ConfigSource::Toml(toml.to_owned());
        assert!(matches!(
            registry.create(game),
            Err(SessionError::Validation(_))
        ));
    }

    #[test]
    fn an_unwritable_output_directory_fails_the_create() {
        let case = tmp_dir("outdir-guard");
        std::fs::create_dir_all(&case).expect("the guard dir creates");
        let blocker = case.join("blocker");
        std::fs::write(&blocker, b"x").expect("the blocker writes");
        let registry = Registry::new(4);
        let mut game = spec("noout", "outdir");
        game.output_dir = blocker.join("child");
        let error = registry.create(game).expect_err("the create fails");
        assert!(
            matches!(error, SessionError::OutputDir(_)),
            "the error names the output directory: {error}"
        );
    }

    #[test]
    fn rom_bytes_without_a_cartridge_device_fail_the_boot() {
        // The Apple II declares no cartridge device kind: the machine
        // assembles, and the ROM application fails over the wire.
        let registry = Registry::new(4);
        let mut game = spec("noapp", "noapp");
        game.config = ConfigSource::Triple("mos6502-apple-ii".to_owned());
        game.rom = Some(gb_rom());
        let error = registry.create(game).expect_err("the boot fails");
        assert!(
            matches!(error, SessionError::NoCartridge),
            "the error names the missing cartridge: {error}"
        );
    }

    #[test]
    fn a_config_without_demu_ctors_fails_the_boot() {
        // The `smoke.*` kinds validate against the core kind registry,
        // and the daemon installs only the real-family constructors,
        // so the assembly fails over the wire. The smoke triple never
        // serves as a bootable session.
        let registry = Registry::new(4);
        let mut game = spec("smokefall", "ctorfail");
        game.config = ConfigSource::Triple("smoke".to_owned());
        let error = registry.create(game).expect_err("the assembly fails");
        assert!(
            matches!(error, SessionError::Machine(_)),
            "the error names the assembly failure: {error}"
        );
        assert!(matches!(
            registry.info(&SessionName::new("smokefall").expect("name")),
            Err(SessionError::Unknown(_))
        ));
    }

    #[test]
    fn a_dead_worker_prunes_and_reports_unknown() {
        let registry = Registry::new(4);
        registry
            .create(spec("doomed", "prune"))
            .expect("the session creates");
        // A panicking job kills the worker thread; the reply channel
        // closes without a result.
        let reply = registry.submit(
            &SessionName::new("doomed").expect("name"),
            Box::new(|_service, _confined| -> Result<Value, CommandError> {
                panic!("the doomed worker stops")
            }),
        );
        assert!(
            matches!(reply, Err(SessionError::Worker(_))),
            "the failed job reports the death: {reply:?}"
        );
        assert!(
            matches!(
                registry.info(&SessionName::new("doomed").expect("name")),
                Err(SessionError::Unknown(_))
            ),
            "the dead session prunes"
        );
        // The pruned slot accepts a new session.
        registry
            .create(spec("reborn", "prune"))
            .expect("the freed slot serves a new session");
        register_and_destroy_all(&registry, ["reborn"]);
    }

    #[test]
    fn a_stale_failure_cleanup_spares_a_recreated_session() {
        let registry = Registry::new(4);
        registry
            .create(spec("alpha", "generation"))
            .expect("create");
        let name = SessionName::new("alpha").expect("name");
        let generation = registry
            .lock()
            .get(&name)
            .expect("the first session registers")
            .generation;
        // Simulate the race the generation guard covers: the dead
        // registration is pruned, the name is recreated, and only then
        // the observed-death cleanup of the first registration runs.
        registry.lock().remove(&name);
        registry
            .create(spec("alpha", "generation"))
            .expect("recreate");
        let reborn = registry
            .lock()
            .get(&name)
            .expect("the recreated session registers")
            .generation;
        assert_ne!(reborn, generation, "a recreation rises a generation");
        assert!(
            registry.forget(&name, generation).is_none(),
            "the stale cleanup spares the recreated session"
        );
        assert!(
            registry.lock().get(&name).is_some(),
            "the recreated session stays registered"
        );
        let handle = registry
            .forget(&name, reborn)
            .expect("the live cleanup removes its own registration");
        assert!(registry.lock().get(&name).is_none());
        drop(handle.queue);
        let _ = handle.thread.join();
    }

    /// Destroy the named test sessions, so every worker thread joins.
    fn register_and_destroy_all(
        registry: &Registry,
        names: impl IntoIterator<Item = &'static str>,
    ) {
        for name in names {
            registry
                .destroy(&SessionName::new(name).expect("the test name validates"))
                .expect("destroy joins the worker");
        }
    }
}
