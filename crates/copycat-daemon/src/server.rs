//! The daemon's event loop.
//!
//! Everything that can happen — a copy, a request, a hotkey, a timer — arrives
//! on one channel and is handled on one thread, so the state machine is never
//! touched concurrently and needs no locks of its own. Threads exist only where
//! the OS forces them: a polling clipboard watcher, a listener, one per client.
//!
//! Effects are performed here and confirmed back into the core. That ordering
//! is the whole reason a failed paste cannot advance a cursor.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use copycat_core::{
    ClipId, ClipPayload, ClipSummary, Core, CoreError, DuplicatePolicy, ErrorKind, Observation,
    PREVIEW_CHARS, PasteRequest,
};
use copycat_protocol::{
    Action, Capability, PROTOCOL_VERSION, Request, ResultBody, Response, StatusReport,
};

use crate::bindings::Bindings;
use crate::config::Config;
use crate::paths::Paths;
use crate::platform::hotkey::HotkeyRegistry;
use crate::platform::{ClipboardBackend, DisplayServer, PasteInjector, Platform};
use crate::store::{KeyStorage, Store, now_ms};

pub enum DaemonEvent {
    ClipboardChanged(ClipPayload),
    Request { request: Request, reply: Sender<Response> },
    /// A registered global shortcut fired, identified by the platform's id.
    Hotkey(u32),
    /// The key observed after a leader trigger, or `None` if the window closed.
    LeaderKey(Option<String>),
    Tick,
    /// Sent by the Unix signal handler. Windows has no signal path yet, so
    /// `copycat daemon stop` over IPC is the only way to shut down there.
    #[cfg_attr(windows, allow(dead_code))]
    Shutdown,
}

type SharedClipboard = Arc<Mutex<Box<dyn ClipboardBackend>>>;

pub struct Server {
    core: Core,
    config: Config,
    paths: Paths,
    store: Option<Store>,
    key_storage: KeyStorage,
    clipboard: SharedClipboard,
    injector: Box<dyn PasteInjector>,
    clipboard_name: String,
    injector_name: String,
    bindings: Bindings,
    hotkeys: HotkeyRegistry,
    /// Index the leader trigger was registered under, if it was.
    leader_index: Option<usize>,
    leader_busy: Arc<AtomicBool>,
    display_server: DisplayServer,
    platform_notes: Vec<Capability>,
    events: Sender<DaemonEvent>,
    started: Instant,
    running: bool,
}

impl Server {
    pub fn new(
        config: Config,
        paths: Paths,
        platform: Platform,
        events: Sender<DaemonEvent>,
    ) -> Self {
        let (store, key_storage) = open_store(&config, &paths);
        let mut core = Core::new(config.core());

        if let Some(store) = &store {
            match store.recent(config.history.hot_items) {
                Ok(events) => {
                    tracing::info!(restored = events.len(), "restored hot history");
                    core.restore(events);
                }
                Err(error) => tracing::warn!(error = %error, "could not restore history"),
            }
            if let Err(error) = store.prune(config.history.retention_days, now_ms()) {
                tracing::warn!(error = %error, "retention pass failed");
            }
        }

        let bindings = Bindings::compile(&config);
        let clipboard_name = platform.clipboard.name();
        let injector_name = platform.injector.name();
        let display_server = platform.display_server;

        let mut server = Server {
            core,
            config,
            paths,
            store,
            key_storage,
            clipboard: Arc::new(Mutex::new(platform.clipboard)),
            injector: platform.injector,
            clipboard_name,
            injector_name,
            bindings,
            hotkeys: HotkeyRegistry::new(display_server),
            leader_index: None,
            leader_busy: Arc::new(AtomicBool::new(false)),
            display_server,
            platform_notes: platform.notes,
            events,
            started: Instant::now(),
            running: true,
        };
        server.register_bindings();
        server
    }

    fn register_bindings(&mut self) {
        for (index, (trigger, _)) in self.bindings.hotkeys.iter().enumerate() {
            self.hotkeys.register(trigger, index);
        }

        let Some(trigger) = self.bindings.leader_trigger.clone() else { return };
        let support = self.display_server.leader_support();
        if !support.is_available() {
            // Register nothing rather than register a trigger that fires and
            // then cannot read the sequence: a key that swallows a keystroke
            // and does nothing is worse than a key that was never bound.
            self.platform_notes.push(Capability {
                name: "leader-sequences".into(),
                available: false,
                detail: support.explain(self.display_server.as_str()),
            });
            return;
        }
        let index = self.bindings.hotkeys.len();
        self.hotkeys.register(&trigger, index);
        self.leader_index = Some(index);
    }

    pub fn shared_clipboard(&self) -> SharedClipboard {
        Arc::clone(&self.clipboard)
    }

    /// The hash the watcher should treat as "already seen", so an unchanged
    /// clipboard is not re-recorded every time the daemon restarts.
    pub fn restored_hash(&self) -> Option<copycat_core::ContentHash> {
        self.core.history().latest().map(|event| event.content_hash)
    }

    pub fn hotkey_registry(&self) -> &HotkeyRegistry {
        &self.hotkeys
    }

    pub fn run(mut self, events: Receiver<DaemonEvent>) -> Result<()> {
        tracing::info!(
            socket = %self.paths.socket.display(),
            clipboard = %self.clipboard_name,
            injector = %self.injector_name,
            display_server = self.display_server.as_str(),
            key_storage = self.key_storage.as_str(),
            hotkeys = self.hotkeys.registered_count(),
            "daemon ready"
        );
        if self.key_storage.is_degraded() {
            tracing::warn!(
                mode = self.key_storage.as_str(),
                "persisted payloads are protected by a key file, not the OS keyring"
            );
        }

        while self.running {
            match events.recv() {
                Ok(event) => self.handle(event),
                Err(_) => break,
            }
        }
        tracing::info!("daemon stopped");
        Ok(())
    }

    fn handle(&mut self, event: DaemonEvent) {
        match event {
            DaemonEvent::ClipboardChanged(payload) => self.on_clipboard(payload),
            DaemonEvent::Request { request, reply } => {
                let response = self.respond(request);
                let _ = reply.send(response);
            }
            DaemonEvent::Hotkey(id) => self.on_hotkey(id),
            DaemonEvent::LeaderKey(key) => self.on_leader_key(key),
            DaemonEvent::Tick => self.on_tick(),
            DaemonEvent::Shutdown => self.running = false,
        }
    }

    // ------------------------------------------------------------- clipboard

    fn on_clipboard(&mut self, payload: ClipPayload) {
        // Drop representations the config does not want before anything is
        // recorded, so a disabled format never reaches the database at all.
        let payload = self.filter_representations(payload);
        let hash = payload.content_hash();
        match self.core.observe(payload, now_ms()) {
            Observation::Recorded { id, entered_session } => {
                tracing::debug!(
                    clip_id = %id,
                    hash = %hash.prefix(),
                    entered_session,
                    "captured"
                );
                self.persist(id);
            }
            Observation::Internal { token } => {
                tracing::debug!(token, hash = %hash.prefix(), "suppressed our own write");
            }
            Observation::Paused => tracing::debug!(hash = %hash.prefix(), "capture paused"),
            Observation::Empty => {}
            Observation::TooLarge { bytes } => {
                tracing::debug!(bytes, limit = self.config.history.max_item_bytes, "clip too large")
            }
        }
    }

    fn filter_representations(&self, mut payload: ClipPayload) -> ClipPayload {
        payload
            .representations
            .retain(|r| self.config.wants_media_type(&r.media_type));
        payload
    }

    fn persist(&self, id: ClipId) {
        let Some(store) = &self.store else { return };
        if !self.config.history.persist {
            return;
        }
        let Some(event) = self.core.history().get(id) else { return };
        if let Err(error) = store.insert(event) {
            tracing::warn!(clip_id = %id, error = %error, "could not persist");
        }
    }

    // --------------------------------------------------------------- hotkeys

    fn on_hotkey(&mut self, id: u32) {
        let Some(index) = self.hotkeys.binding_for(id) else { return };

        if Some(index) == self.leader_index {
            self.arm_leader();
            return;
        }
        let Some((trigger, action)) = self.bindings.hotkeys.get(index).cloned() else { return };
        tracing::debug!(%trigger, "hotkey");
        self.run_bound_action(&trigger, action);
    }

    /// Watch for the next key on a worker thread.
    ///
    /// The grab blocks for up to the leader timeout, and the event loop must
    /// stay responsive: a clipboard change arriving mid-sequence still has to be
    /// recorded.
    fn arm_leader(&mut self) {
        if self.leader_busy.swap(true, Ordering::SeqCst) {
            return; // a sequence is already in flight
        }
        let timeout = Duration::from_millis(self.bindings.leader_timeout_ms);
        let display_server = self.display_server;
        let events = self.events.clone();
        let busy = Arc::clone(&self.leader_busy);

        std::thread::spawn(move || {
            let observed = match crate::platform::hotkey::observe_next_key(display_server, timeout) {
                Ok(key) => key,
                Err(error) => {
                    tracing::warn!(error = %error.message, "leader sequence failed");
                    None
                }
            };
            busy.store(false, Ordering::SeqCst);
            let _ = events.send(DaemonEvent::LeaderKey(observed));
        });
    }

    fn on_leader_key(&mut self, key: Option<String>) {
        let Some(key) = key else {
            tracing::debug!("leader sequence timed out");
            return;
        };
        let Some(action) = self.bindings.sequence(&key).cloned() else {
            tracing::info!(%key, "no leader binding for this key");
            return;
        };
        self.run_bound_action(&key, action);
    }

    fn run_bound_action(&mut self, trigger: &str, action: Action) {
        match self.dispatch(action) {
            Ok(_) => {}
            // A binding has nobody to return an exit code to, so the log is the
            // only place a failure can surface.
            Err(error) => tracing::warn!(%trigger, code = %error.code, "binding failed: {}", error.message),
        }
    }

    fn on_tick(&mut self) {
        if let Some(store) = &self.store
            && let Err(error) = store.prune(self.config.history.retention_days, now_ms())
        {
            tracing::warn!(error = %error, "retention pass failed");
        }
    }

    /// Add or replace a binding, persist it, and re-register.
    ///
    /// Validation happens before the file is touched, and through the same
    /// parser a request uses, so a binding that would be rejected at load time
    /// never reaches the config in the first place.
    fn set_binding(
        &mut self,
        kind: copycat_protocol::BindingKind,
        trigger: &str,
        action: &str,
        args: &serde_json::Value,
    ) -> Result<(), CoreError> {
        if trigger.trim().is_empty() {
            return Err(CoreError::invalid("empty_trigger", "a binding needs a trigger"));
        }
        // TOML has no null, so "no arguments" is an empty table.
        let as_toml = match args {
            serde_json::Value::Null => toml::Value::Table(Default::default()),
            other => toml::Value::try_from(other).map_err(|e| {
                CoreError::invalid("bad_arguments", format!("arguments are not valid TOML: {e}"))
            })?,
        };
        crate::bindings::resolve(action, &as_toml)
            .map_err(|reason| CoreError::invalid("unknown_action", reason))?;

        crate::config_edit::set_binding(&self.paths.config_file, kind, trigger, action, args)
            .map_err(|e| CoreError::new(ErrorKind::StorageUnavailable, "config_write_failed", format!("{e:#}")))?;
        self.reload()
    }

    fn remove_binding(
        &mut self,
        kind: copycat_protocol::BindingKind,
        trigger: &str,
    ) -> Result<(), CoreError> {
        let removed =
            crate::config_edit::remove_binding(&self.paths.config_file, kind, trigger)
                .map_err(|e| CoreError::new(ErrorKind::StorageUnavailable, "config_write_failed", format!("{e:#}")))?;
        if !removed {
            return Err(CoreError::not_found(
                "binding_not_found",
                format!("no {} binding on {trigger}", kind.as_str()),
            ));
        }
        self.reload()
    }

    /// Change the leader chord, or arm and disarm the leader.
    fn set_leader(
        &mut self,
        trigger: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<(), CoreError> {
        if trigger.is_none() && enabled.is_none() {
            return Err(CoreError::invalid(
                "nothing_to_change",
                "give a trigger, an enabled flag, or both",
            ));
        }
        if let Some(trigger) = trigger {
            if trigger.trim().is_empty() {
                return Err(CoreError::invalid("empty_trigger", "the leader needs a chord"));
            }
            // Checked before the file is touched: a leader that cannot be
            // parsed could never be armed, and finding that out at the next
            // restart would be a poor way to learn it.
            crate::platform::hotkey::parse_trigger(trigger).map_err(|reason| {
                CoreError::invalid("bad_trigger", format!("{trigger} is not a usable chord: {reason}"))
            })?;
        }

        crate::config_edit::set_leader(&self.paths.config_file, trigger, enabled).map_err(|e| {
            CoreError::new(ErrorKind::StorageUnavailable, "config_write_failed", format!("{e:#}"))
        })?;
        self.reload()
    }

    /// Re-read the config file and rebuild bindings (SIGHUP, `bind reload`).
    pub fn reload(&mut self) -> Result<(), CoreError> {
        let config = Config::load(&self.paths.config_file)
            .map_err(|e| CoreError::invalid("config_invalid", format!("{e:#}")))?;
        self.bindings = Bindings::compile(&config);
        self.config = config;
        // Registrations are rebuilt from scratch; the previous manager's
        // grabs are released when it drops.
        self.hotkeys = HotkeyRegistry::new(self.display_server);
        self.leader_index = None;
        self.register_bindings();
        tracing::info!(hotkeys = self.hotkeys.registered_count(), "config reloaded");
        Ok(())
    }

    // -------------------------------------------------------------- requests

    fn respond(&mut self, request: Request) -> Response {
        if request.version != PROTOCOL_VERSION {
            return Response::error(
                request.id,
                CoreError::invalid(
                    "protocol_version",
                    format!(
                        "client speaks protocol {} but this daemon speaks {PROTOCOL_VERSION}",
                        request.version
                    ),
                ),
            );
        }
        let id = request.id.clone();
        match self.dispatch(request.action) {
            Ok(result) => Response::ok(id, result),
            Err(error) => Response::error(id, error),
        }
    }

    fn policy(&self, raw: bool) -> DuplicatePolicy {
        if raw { DuplicatePolicy::Preserve } else { DuplicatePolicy::Collapse }
    }

    fn duplicates(&self, requested: Option<DuplicatePolicy>) -> DuplicatePolicy {
        requested.unwrap_or(self.config.defaults.duplicate_policy)
    }

    fn dispatch(&mut self, action: Action) -> Result<ResultBody, CoreError> {
        let now = now_ms();
        match action {
            Action::PasteLatest { raw } => {
                let id = self.core.resolve_offset(0, raw)?;
                let request = self.core.begin_paste_clip(id)?;
                self.perform_paste(request)
            }
            Action::PasteOffset { offset, raw } => {
                let id = self.core.resolve_offset(offset, raw)?;
                let request = self.core.begin_paste_clip(id)?;
                self.perform_paste(request)
            }
            Action::PasteId { id } => {
                self.hydrate(id)?;
                let request = self.core.begin_paste_clip(id)?;
                self.perform_paste(request)
            }
            Action::PasteNext { peek } => {
                let request = self.core.begin_paste_next(peek)?;
                self.perform_paste(request)
            }

            Action::StackStart { duplicates } => Ok(ResultBody::SessionStarted(
                self.core.stack_start(self.duplicates(duplicates), now),
            )),
            Action::QueueStart { last, duplicates } => Ok(ResultBody::SessionStarted(
                self.core.queue_start_last(last, self.duplicates(duplicates), now)?,
            )),
            Action::QueueCapture { duplicates } => Ok(ResultBody::SessionStarted(
                self.core.queue_capture(self.duplicates(duplicates), now),
            )),
            Action::QueueSeal => {
                Ok(ResultBody::Session { session: Some(self.core.queue_seal()?) })
            }
            Action::GroupCapture { delimiter, duplicates } => Ok(ResultBody::SessionStarted(
                self.core.group_capture(delimiter, self.duplicates(duplicates), now),
            )),
            Action::GroupPaste => {
                let request = self.core.begin_paste_group_session()?;
                self.perform_paste(request)
            }
            Action::GroupPasteLast { last, delimiter, raw } => {
                let request = self.core.begin_paste_group_last(last, delimiter, raw)?;
                self.perform_paste(request)
            }

            Action::SessionStatus => {
                Ok(ResultBody::Session { session: self.core.session().map(|s| s.summary()) })
            }
            Action::SessionStop => Ok(ResultBody::Session { session: self.core.session_stop() }),
            Action::SessionReset => {
                Ok(ResultBody::Session { session: Some(self.core.session_reset()?) })
            }

            Action::HistoryList { limit, raw } => Ok(ResultBody::Clips {
                clips: self.list(limit, raw)?,
                truncated: false,
            }),
            Action::HistoryShow { id } => {
                self.hydrate(id)?;
                let event = self
                    .core
                    .history()
                    .get(id)
                    .ok_or_else(|| CoreError::not_found("clip_not_found", format!("no clip {id}")))?;
                Ok(ResultBody::Clip {
                    clip: event.summary(PREVIEW_CHARS),
                    text: event.payload.as_text().map(str::to_string),
                })
            }
            Action::HistorySearch { query, limit } => {
                let (clips, truncated) = self.search(&query, limit)?;
                Ok(ResultBody::Clips { clips, truncated })
            }
            Action::HistoryDelete { id } => {
                let in_hot = self.core.delete(id).is_ok();
                let in_store = match &self.store {
                    Some(store) => store.delete(id).map_err(storage_error)?,
                    None => false,
                };
                if !in_hot && !in_store {
                    return Err(CoreError::not_found("clip_not_found", format!("no clip {id}")));
                }
                Ok(ResultBody::Removed { count: 1 })
            }
            Action::HistoryClear { keep_pinned } => {
                let hot = self.core.clear(keep_pinned);
                let persisted = match &self.store {
                    Some(store) => store.clear(keep_pinned).map_err(storage_error)?,
                    None => 0,
                };
                Ok(ResultBody::Removed { count: hot.max(persisted) })
            }
            Action::HistoryPin { id, pinned } => {
                let in_hot = self.core.set_pinned(id, pinned).is_ok();
                let in_store = match &self.store {
                    Some(store) => store.set_pinned(id, pinned).map_err(storage_error)?,
                    None => false,
                };
                if !in_hot && !in_store {
                    return Err(CoreError::not_found("clip_not_found", format!("no clip {id}")));
                }
                Ok(ResultBody::Done)
            }
            Action::HistoryPause => {
                self.core.set_paused(true);
                Ok(ResultBody::Done)
            }
            Action::HistoryResume => {
                self.core.set_paused(false);
                Ok(ResultBody::Done)
            }

            Action::BindList => {
                let (sequences, hotkeys) = self.bindings.describe();
                let mut rejected = self.bindings.rejected.clone();
                rejected.extend_from_slice(self.hotkeys.rejected());
                Ok(ResultBody::Bindings {
                    leader: self.bindings.leader_trigger.clone(),
                    sequences,
                    hotkeys,
                    rejected,
                })
            }
            Action::BindReload => {
                self.reload()?;
                Ok(ResultBody::Done)
            }
            Action::BindSet { kind, trigger, action, args } => {
                self.set_binding(kind, &trigger, &action, &args)?;
                self.dispatch(Action::BindList)
            }
            Action::BindRemove { kind, trigger } => {
                self.remove_binding(kind, &trigger)?;
                self.dispatch(Action::BindList)
            }
            Action::BindLeader { trigger, enabled } => {
                self.set_leader(trigger.as_deref(), enabled)?;
                self.dispatch(Action::BindList)
            }
            Action::ConfigShow => Ok(ResultBody::Config {
                path: self.paths.config_file.display().to_string(),
                toml: toml::to_string_pretty(&self.config)
                    .unwrap_or_else(|e| format!("# could not render config: {e}")),
            }),

            Action::Status => Ok(ResultBody::Status(Box::new(self.status()))),
            Action::Doctor => Ok(ResultBody::Doctor(Box::new(crate::doctor::report(self)))),
            Action::DaemonStop => {
                self.running = false;
                Ok(ResultBody::Done)
            }
        }
    }

    /// Pull a clip back into hot history from the store so it can be pasted or
    /// shown by id even after eviction.
    fn hydrate(&mut self, id: ClipId) -> Result<(), CoreError> {
        if self.core.history().get(id).is_some() {
            return Ok(());
        }
        let Some(store) = &self.store else { return Ok(()) };
        match store.get(id) {
            Ok(Some(event)) => {
                self.core.restore(vec![event]);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(error) => Err(storage_error(error)),
        }
    }

    fn list(&self, limit: usize, raw: bool) -> Result<Vec<ClipSummary>, CoreError> {
        let mut clips = self.core.history().summaries(self.policy(raw));
        clips.truncate(limit);

        if clips.len() < limit
            && let Some(store) = &self.store
        {
            let oldest = clips.last().map(|clip| clip.id);
            let more = store
                .older_than(oldest, limit - clips.len())
                .map_err(storage_error)?;
            clips.extend(more);
        }
        Ok(clips)
    }

    fn search(&self, query: &str, limit: usize) -> Result<(Vec<ClipSummary>, bool), CoreError> {
        let mut clips = self.core.history().search(query, limit, DuplicatePolicy::Collapse);
        let mut truncated = false;

        if clips.len() < limit
            && let Some(store) = &self.store
        {
            let (more, hit_bound) = store
                .search(query, limit - clips.len(), self.config.history.search_scan_limit)
                .map_err(storage_error)?;
            truncated = hit_bound;
            // Hot history and the store overlap; the hot entry wins because it
            // carries the duplicate run length.
            for clip in more {
                if !clips.iter().any(|existing| existing.id == clip.id) {
                    clips.push(clip);
                }
            }
        }
        Ok((clips, truncated))
    }

    // ----------------------------------------------------------------- paste

    /// Write, inject, confirm — in that order, and only confirm what worked.
    fn perform_paste(&mut self, request: PasteRequest) -> Result<ResultBody, CoreError> {
        let preview = request.payload.preview(PREVIEW_CHARS);
        let bytes = request.payload.byte_len();

        // Arm before writing: the watcher may see the change before `write`
        // has even returned.
        self.core.arm_suppression(request.hash, now_ms());

        if let Err(error) = self.clipboard.lock().unwrap().write(&request.payload) {
            self.core.abort_paste();
            return Err(error);
        }

        let injected = match self.injector.inject() {
            Ok(()) => true,
            // A capability this platform simply does not have is not a failed
            // paste. The value is on the clipboard and the user presses paste
            // themselves, so the item was consumed and the cursor should move.
            // Any other failure means the keystroke was lost, and the cursor
            // must not move (STATE_MACHINE, "paste transaction").
            Err(error) if error.kind == ErrorKind::PlatformUnavailable => {
                tracing::debug!(code = %error.code, "pasted without injection");
                false
            }
            Err(error) => {
                self.core.abort_paste();
                return Err(error);
            }
        };

        let session = self.core.commit_paste();
        Ok(ResultBody::Pasted {
            clip_id: request.clip_id,
            preview,
            bytes,
            skipped_non_text: request.skipped_non_text,
            injected,
            session,
        })
    }

    // ---------------------------------------------------------------- status

    pub fn status(&self) -> StatusReport {
        StatusReport {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_ms: self.started.elapsed().as_millis() as u64,
            core: self.core.status(),
            os_clipboard: self
                .clipboard
                .lock()
                .ok()
                .and_then(|mut c| c.read().ok())
                .map(|payload| payload.preview(PREVIEW_CHARS)),
            clipboard_backend: self.clipboard_name.clone(),
            watch_interval_ms: self.config.platform.watch_interval_ms,
            key_storage: self.key_storage.as_str().to_string(),
            persistence: match (&self.store, self.config.history.persist) {
                (Some(_), true) => "on".into(),
                (Some(_), false) => "off (disabled in config)".into(),
                (None, _) => "off (no key available)".into(),
            },
            socket_path: self.paths.socket.display().to_string(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn key_storage(&self) -> KeyStorage {
        self.key_storage
    }

    pub fn display_server(&self) -> DisplayServer {
        self.display_server
    }

    pub fn platform_notes(&self) -> &[Capability] {
        &self.platform_notes
    }

    pub fn clipboard_backend_name(&self) -> &str {
        &self.clipboard_name
    }

    pub fn injector_name(&self) -> &str {
        &self.injector_name
    }

    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// Whether the backend can tell a repeat copy from no copy at all.
    pub fn detects_repeat_copies(&self) -> bool {
        self.clipboard.lock().map(|mut c| c.change_token().is_some()).unwrap_or(false)
    }

    pub fn readable_media_types(&self) -> Vec<String> {
        self.clipboard
            .lock()
            .map(|c| c.readable_media_types())
            .unwrap_or_default()
    }
}

fn storage_error(error: anyhow::Error) -> CoreError {
    CoreError::new(ErrorKind::StorageUnavailable, "storage_error", format!("{error:#}"))
}

fn open_store(config: &Config, paths: &Paths) -> (Option<Store>, KeyStorage) {
    if !config.history.persist {
        return (None, KeyStorage::MemoryOnly);
    }
    let (cipher, storage) = crate::store::PayloadCipher::open(
        &paths.key_file,
        config.privacy.allow_key_file_fallback,
    );
    let Some(cipher) = cipher else {
        tracing::warn!("no key available; history will not be persisted");
        return (None, storage);
    };
    match Store::open(&paths.database, cipher) {
        Ok(store) => (Some(store), storage),
        Err(error) => {
            tracing::error!(error = %error, "could not open the history database");
            (None, KeyStorage::MemoryOnly)
        }
    }
}

/// Poll the clipboard and report changes.
///
/// Polling rather than subscribing is the portable baseline: macOS exposes only
/// `NSPasteboard.changeCount` and has no notification to subscribe to (ADR-014).
pub fn spawn_watcher(
    clipboard: SharedClipboard,
    interval: Duration,
    seed: Option<copycat_core::ContentHash>,
    events: Sender<DaemonEvent>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last_hash = seed;
        let mut last_token: Option<u64> = None;
        let mut first_poll = true;
        let mut complained = false;

        loop {
            std::thread::sleep(interval);

            let (token, read) = {
                let mut backend = clipboard.lock().unwrap();
                (backend.change_token(), backend.read())
            };

            // With a change token, a repeat copy of the same text is a real
            // event and must reach the log. Without one, content is all there
            // is to compare, and repeats are simply invisible.
            let changed = match (token, last_token) {
                (Some(current), Some(previous)) => current != previous,
                (Some(_), None) => true,
                (None, _) => true,
            };
            last_token = token.or(last_token);
            if !changed {
                continue;
            }

            match read {
                Ok(payload) => {
                    complained = false;
                    if payload.is_empty() {
                        continue;
                    }
                    let hash = payload.content_hash();

                    // Whatever is on the clipboard at startup is already in
                    // restored history; recording it again on every restart
                    // would fill the log with copies nobody made.
                    if first_poll {
                        first_poll = false;
                        if last_hash == Some(hash) {
                            continue;
                        }
                    }
                    if token.is_none() && last_hash == Some(hash) {
                        continue;
                    }

                    last_hash = Some(hash);
                    if events.send(DaemonEvent::ClipboardChanged(payload)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    // One line, not one per poll: a missing display would
                    // otherwise fill the log four times a second.
                    if !complained {
                        complained = true;
                        tracing::warn!(error = %error.message, "clipboard read failed");
                    }
                }
            }
        }
    })
}

/// Periodic housekeeping: retention, and a place for a config reload signal to
/// be noticed.
pub fn spawn_ticker(events: Sender<DaemonEvent>, interval: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(interval);
            if events.send(DaemonEvent::Tick).is_err() {
                return;
            }
        }
    })
}

/// Forward platform hotkey events onto the daemon's channel.
pub fn spawn_hotkey_listener(events: Sender<DaemonEvent>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let receiver = global_hotkey::GlobalHotKeyEvent::receiver();
        while let Ok(event) = receiver.recv() {
            // Fire on press only; the release event would run every binding
            // twice.
            if event.state() != global_hotkey::HotKeyState::Pressed {
                continue;
            }
            if events.send(DaemonEvent::Hotkey(event.id())).is_err() {
                return;
            }
        }
    })
}
