//! `plugin.panel.*`: ABI v2 panel plug-ins — one live instance the host drives with events, which
//! answers with operations for the host to carry out.
//!
//! A panel keeps one instance per plug-in for the life of the process. Hiding it only tells it so:
//! its state, timers and blobs stay. Only a failure, `plugin.remove` or an explicit
//! `plugin.panel.close {"stop":true}` ends the instance, so a panel's own model survives exactly as
//! long as the app does.
//!
//! Every command that touches a panel drives it the same way — queue an event, pump the queue, carry
//! out what comes back, repeat until the panel settles — so a click in the panel and the same
//! `plugin.panel.event` from a test or an agent are one code path.
//!
//! What this build carries out, and what it refuses:
//!
//! | Operation | Here |
//! |---|---|
//! | `ui.view`, `ui.set`, `ui.progress`, `ui.notice` | kept for the UI to render (`plugin.panel.inspect`) |
//! | `timer.set`, `timer.clear` | armed on the instance; `plugin.panel.tick` fires them |
//! | `storage.set` | kept per plug-in, in memory for now |
//! | `blob.free` | releases the blob |
//! | `doc.info` | answered from [`crate::inspect::document`] — the size, the layer stack, the selection bounds |
//! | `doc.command` | run, if the manifest asks for the capability and the host's own list allows it (see [`permissions`]) |
//! | `doc.capture`, `doc.commit`, `http.request` | **refused**, with an `ok: false` result |
//!
//! Those refusals are answers, not silence: the panel is told what happened, so it can say so. The
//! pixels and the network are the host work still to come; the sandbox, the limits and the failure
//! handling they will run under are already here.
//!
//! Instances are driven on the calling thread, under one process-wide lock. A hostile panel is
//! bounded by the 2 s wall-clock budget of a single event and is then stopped for good, so it can
//! delay other panels by at most that; giving each instance its own worker thread is the next step.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use photocraft_plugins::instance::{Failures, Instance};
use photocraft_plugins::ops::{self, NoticeLevel, OpId, Operation};
use photocraft_plugins::view::Widget;
use photocraft_plugins::{Capabilities, DocumentCap, Error as PluginError, Plugin, registry};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// Most events one command pumps through a panel before giving up: a panel that answers every event
/// with an event of its own would otherwise never return.
const MAX_PUMP_STEPS: usize = 64;
/// Notices kept per panel for `plugin.panel.inspect`.
const MAX_NOTICES: usize = 32;
/// Stored values one panel may keep.
const MAX_STORAGE_KEYS: usize = 256;
/// The locale the host reports in `init` (PhotoCraft's UI is English).
const LOCALE: &str = "en-US";

/// A refused operation, as the pane's `result` event reports it.
const NO_CAPTURE: &str = "this host does not capture pixels yet: `doc.capture` was not carried out";
const NO_COMMIT: &str = "this host does not add layers yet: `doc.commit` was not carried out";
const NO_NETWORK: &str = "this host makes no network requests: `http.request` was not carried out";

/// The panels open in this process, by plug-in id.
static PANELS: Mutex<BTreeMap<String, Panel>> = Mutex::new(BTreeMap::new());
/// The failure window per plug-in, keyed like [`PANELS`]. Deliberately outside [`Panel`]: it has to
/// outlive the instance it is about, or a crash-looping plug-in could never be recognised as one.
static FAILURES: Mutex<BTreeMap<String, Failures>> = Mutex::new(BTreeMap::new());
/// What plug-ins have stored, by plug-in id — outside [`Panel`] for the same reason as [`FAILURES`],
/// and because the ABI says storage belongs to the plug-in rather than to a run of it: a panel that
/// lost its settings when it was restarted, or hidden and shown again, would have nowhere to keep
/// anything. In memory here; a file per plug-in (the ABI's 1 MiB) is the next step, at which point
/// this map becomes the cache in front of it.
static STORAGE: Mutex<BTreeMap<String, Map<String, Value>>> = Mutex::new(BTreeMap::new());
/// Mixed into each `init` so a panel can seed a PRNG; v2 gives a plug-in no other randomness.
static ENTROPY: AtomicU64 = AtomicU64::new(0);

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn err(e: PluginError) -> EngineError {
    EngineError::Other(e.to_string())
}

fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

fn panels() -> MutexGuard<'static, BTreeMap<String, Panel>> {
    PANELS.lock().unwrap_or_else(|e| e.into_inner())
}

fn failures() -> MutexGuard<'static, BTreeMap<String, Failures>> {
    FAILURES.lock().unwrap_or_else(|e| e.into_inner())
}

fn storages() -> MutexGuard<'static, BTreeMap<String, Map<String, Value>>> {
    STORAGE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Milliseconds since the Unix epoch: the `now` every event carries, and the only clock a plug-in
/// gets. The web build has no clock in `std`; there a panel is bounded by its fuel budget instead.
fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }
}

/// 32 bytes of hex from the clock and a counter — not cryptographic, and not meant to be: the ABI
/// promises randomness only so a panel can seed a PRNG.
fn entropy() -> String {
    let mut h = now_ms() ^ ENTROPY.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut out = String::with_capacity(64);
    for _ in 0..4 {
        h ^= h << 13;
        h ^= h >> 7;
        h ^= h << 17;
        out.push_str(&format!("{h:016x}"));
    }
    out
}

/// The host facts an `init` carries.
///
/// `widgets` is the newer widget features this build draws (see
/// [`photocraft_plugins::view::WIDGET_FEATURES`]): a panel built once for many PhotoCraft versions
/// checks it before using a `list`, a `scroll` pane, a coloured or font-named label, or an icon
/// button, and falls back to the plain layout when the host does not list one.
fn host_info() -> Value {
    json!({
        "name": "PhotoCraft",
        "version": crate::build_info::VERSION,
        "build": crate::build_info::short_sha(),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "widgets": photocraft_plugins::view::WIDGET_FEATURES,
    })
}

/// What the host has actually granted, which is narrower than what the manifest asked for.
///
/// `document` is the capabilities whose operations this build carries out: reading (`doc.info`) and
/// changing the document (`doc.command`) both work, so a manifest that asked for them has them;
/// pixels (`doc.capture`) and added layers from blobs (`doc.commit`) do not, and no network
/// transport exists, so `network` is always null. `commands` names the exact engines commands
/// [`permissions`] lets this panel run — a panel offers what it was granted, not what it asked for.
fn grants(caps: &Capabilities) -> Value {
    json!({
        "storage": caps.storage,
        "document": caps.document,
        "network": Value::Null,
        "commands": permissions::granted(caps),
    })
}

/// The panel's progress bar, if it asked for one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `None` while the total is unknown: an indeterminate bar.
    pub fraction: Option<f64>,
    pub text: Option<String>,
    pub cancellable: bool,
    /// Set on the operation that retires the bar.
    pub done: bool,
}

/// One `ui.notice`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
}

/// What a panel asks the UI to show: its widget tree, the values typed into it, its progress bar and
/// the notices it has raised. The UI renders this; it never sees the plug-in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelView {
    pub view: Option<Widget>,
    pub values: Map<String, Value>,
    pub progress: Option<Progress>,
    pub notices: Vec<Notice>,
}

/// One engine command a panel asked for, taken out of the panel and run by the caller. Commands are
/// never run from inside [`apply`]: that would run arbitrary engine code while the panel lock is
/// held, and a granted command that touched a panel would then wait for itself.
struct Ask {
    id: OpId,
    command: String,
    params: Value,
}

/// A running panel: its instance, and everything the host holds on its behalf.
struct Panel {
    instance: Instance,
    view: PanelView,
    storage: Map<String, Value>,
    visible: bool,
    /// The active document and revision the panel last heard about, so one change is announced once.
    seen: Option<(u64, u64)>,
    /// Events pumped since the panel was started (a runaway panel is visible in `plugin.panel.inspect`).
    steps: u64,
    /// Commands the panel asked for that the host has not run yet (see [`Ask`]).
    asked: Vec<Ask>,
}

impl Panel {
    fn new(plugin: Arc<Plugin>) -> Result<Panel> {
        // Whatever this plug-in stored last time, including in an instance that has since stopped.
        let storage = storages().get(plugin.id()).cloned().unwrap_or_default();
        Ok(Panel {
            instance: Instance::start(plugin).map_err(err)?,
            view: PanelView::default(),
            storage,
            visible: false,
            seen: None,
            steps: 0,
            asked: Vec::new(),
        })
    }

    /// Why the panel has stopped, if it has.
    fn stopped(&self) -> Option<&PluginError> {
        self.instance.stopped_reason()
    }

    /// What this panel's manifest asked for — a request, which [`permissions`] or a capability check
    /// turns into an answer.
    fn caps(&self) -> &Capabilities {
        &self.instance.plugin().manifest().capabilities
    }

    /// Whether this panel may use a document capability, or why it may not.
    ///
    /// A manifest is a request, not a grant: what it asks for is checked here against what this
    /// build actually carries out, and the refusal is what the panel hears back.
    fn granted(&self, cap: DocumentCap) -> std::result::Result<(), String> {
        if self.caps().document.contains(&cap) {
            Ok(())
        } else {
            Err(format!(
                "this panel's manifest does not ask for `{}` in `capabilities.document`",
                match cap {
                    DocumentCap::Read => "read",
                    DocumentCap::AddLayers => "addLayers",
                }
            ))
        }
    }
}

/// What a UI needs to draw one panel, and nothing else.
///
/// A snapshot, deliberately: the UI must never draw while the panel lock is held, because drawing
/// sends events back into the panel. So the panel is read under the lock, the snapshot is let go of,
/// and only then is anything drawn — and drawing may drive the panel again, freely.
#[derive(Clone, Debug)]
pub struct PanelDraw {
    pub id: String,
    /// The dock-tab label: the plug-in's `panel.title`, which is what a letterer recognises.
    pub title: String,
    /// Narrowest the user may drag it, from the manifest.
    pub min_width: u32,
    /// Set when the panel has stopped: the UI shows this and offers to restart it.
    pub error: Option<String>,
    pub visible: bool,
    pub view: Option<Widget>,
    pub values: Map<String, Value>,
    pub progress: Option<Progress>,
    pub notices: Vec<Notice>,
}

/// A snapshot of every open panel, in id order, for the UI to draw.
pub fn draw_list() -> Vec<PanelDraw> {
    panels()
        .values()
        .map(|panel| {
            let plugin = panel.instance.plugin();
            PanelDraw {
                id: plugin.id().to_string(),
                title: plugin.manifest().panel.as_ref().map(|p| p.title.clone()).unwrap_or_else(|| plugin.manifest().name.clone()),
                min_width: plugin.manifest().panel.as_ref().map(|p| p.min_width).unwrap_or(260),
                error: panel.stopped().map(ToString::to_string),
                visible: panel.visible,
                view: panel.view.view.clone(),
                values: panel.view.values.clone(),
                progress: panel.view.progress.clone(),
                notices: panel.view.notices.clone(),
            }
        })
        .collect()
}

/// Whether any panel is open at all, without building a snapshot: the UI asks once a frame.
pub fn any_open() -> bool {
    !panels().is_empty()
}

/// What the host knows while it is carrying out a batch of operations.
struct HostCtx<'a> {
    now: u64,
    /// The active document, as [`crate::inspect::document`] describes it.
    doc: Option<&'a Value>,
}

/// Queues the `result` event for an operation that carried an id.
fn queue_result(panel: &mut Panel, ctx: &HostCtx, id: &OpId, outcome: std::result::Result<Value, String>) {
    let event = match outcome {
        Ok(fields) => ops::result(ctx.now, id, true, fields),
        Err(message) => ops::result(ctx.now, id, false, json!({"error": message})),
    };
    panel.instance.queue_mut().push(event);
}

/// Carries out one operation.
///
/// The match is exhaustive on purpose: a new operation in the ABI must be decided here, not fall
/// into a default that quietly ignores it.
fn apply(panel: &mut Panel, ctx: &HostCtx, op: Operation) -> Result<()> {
    match op {
        // What the panel asks the UI to show.
        Operation::UiView { view } => panel.view.view = Some(view),
        Operation::UiSet { values } => {
            for (key, value) in values {
                panel.view.values.insert(key, value);
            }
        }
        // `done` retires the bar rather than showing a full one.
        Operation::UiProgress { fraction, text, cancellable, done } => {
            panel.view.progress = (!done).then_some(Progress { fraction, text, cancellable, done });
        }
        Operation::UiNotice { level, text } => {
            panel.view.notices.push(Notice { level, text });
            if panel.view.notices.len() > MAX_NOTICES {
                panel.view.notices.remove(0);
            }
        }
        // The instance's own state.
        Operation::TimerSet { id, ms } => panel.instance.timers_mut().set(&id, ms, ctx.now).map_err(err)?,
        Operation::TimerClear { id } => {
            panel.instance.timers_mut().clear(&id);
        }
        Operation::StorageSet { key, value } => {
            if value.is_null() {
                // The ABI's delete (see `ops::Operation::StorageSet`). Without it a panel could only
                // ever add keys, and its 256 would be a one-way door.
                panel.storage.remove(&key);
            } else {
                if !panel.storage.contains_key(&key) && panel.storage.len() >= MAX_STORAGE_KEYS {
                    return Err(err(PluginError::Limit(format!("a panel may keep {MAX_STORAGE_KEYS} stored values"))));
                }
                panel.storage.insert(key, value);
            }
            // Written through, so the next instance of this plug-in — a restart after a failure, or
            // the panel reopened after `stop` — reads back what this one stored.
            storages().insert(panel.instance.plugin().id().to_string(), panel.storage.clone());
        }
        Operation::BlobFree { blob } => {
            panel.instance.blobs_mut().free(&blob);
        }
        // The document and the network: answered, at least one of them not yet performed.
        Operation::DocInfo { id } => match (panel.granted(DocumentCap::Read), ctx.doc) {
            (Err(why), _) => queue_result(panel, ctx, &id, Err(why)),
            (Ok(()), Some(doc)) => queue_result(panel, ctx, &id, Ok(doc.clone())),
            (Ok(()), None) => queue_result(panel, ctx, &id, Err("no document is open".to_string())),
        },
        // Run by the caller, with the panel lock released: see [`Ask`]. A command the host does not
        // let a panel run, or a capability the manifest did not ask for, is answered here and now,
        // so a panel hears why rather than waiting for a result that never comes.
        Operation::DocCommand { id, command, params } => match permissions::check(panel.caps(), &command) {
            Err(why) => queue_result(panel, ctx, &id, Err(why)),
            Ok(()) => panel.asked.push(Ask { id, command, params }),
        },
        Operation::DocCapture { id, .. } => queue_result(panel, ctx, &id, Err(NO_CAPTURE.to_string())),
        Operation::DocCommit { id, .. } => queue_result(panel, ctx, &id, Err(NO_COMMIT.to_string())),
        Operation::HttpRequest { id, .. } => queue_result(panel, ctx, &id, Err(NO_NETWORK.to_string())),
    }
    Ok(())
}

/// Records a failure against the plug-in, returning whether the window is now tripped — three
/// failures inside a minute, after which only the user's `plugin.panel.restart` starts it again.
fn note_failure(id: &str, now: u64) -> bool {
    let mut all = failures();
    all.entry(id.to_string()).or_default().record(now)
}

/// Sends one event and carries out everything the panel answers with, until it settles.
fn drive(panel: &mut Panel, event: Value, ctx: &HostCtx) -> Result<()> {
    panel.instance.queue_mut().push(event);
    drain(panel, ctx)
}

/// Carries out what the panel has queued: send each event, apply the operations it answers with,
/// and keep going while those operations queue events of their own.
fn drain(panel: &mut Panel, ctx: &HostCtx) -> Result<()> {
    let mut pumped = 0usize;
    while let Some(event) = panel.instance.queue_mut().pop() {
        pumped += 1;
        if pumped > MAX_PUMP_STEPS {
            let error = PluginError::Failed(format!("the panel queued more than {MAX_PUMP_STEPS} events without settling"));
            panel.instance.stop(error.clone());
            return Err(err(error));
        }
        let ops = match panel.instance.send(&event) {
            Ok(ops) => ops,
            Err(error) => {
                note_failure(panel.instance.plugin().id(), ctx.now);
                return Err(err(error));
            }
        };
        for op in ops {
            apply(panel, ctx, op)?;
        }
    }
    panel.steps += pumped as u64;
    Ok(())
}

/// Runs the commands a panel asked for, and the events their answers produce, until it settles.
///
/// Called with the panel lock **not** held. A granted command is ordinary engine code: it may be
/// slow, it may ask the panel something in turn, and it must never run underneath the panel lock —
/// a command that touched a panel would otherwise wait for itself.
fn run_asked(s: &mut Session, id: &str, ctx: &HostCtx) -> Result<()> {
    for _ in 0..MAX_PUMP_STEPS {
        let asked = {
            let mut all = panels();
            match all.get_mut(id) {
                Some(panel) => std::mem::take(&mut panel.asked),
                // Gone: the plug-in was removed while its own command ran.
                None => return Ok(()),
            }
        };
        if asked.is_empty() {
            return Ok(());
        }
        // No panel lock is held here — that is the point of the queue.
        let answers: Vec<(OpId, std::result::Result<Value, String>)> = asked
            .into_iter()
            .map(|ask| {
                let outcome = s.execute(&ask.command, ask.params).map_err(|e| e.to_string());
                (ask.id, outcome)
            })
            .collect();
        let mut all = panels();
        let Some(panel) = all.get_mut(id) else { return Ok(()) };
        for (op_id, outcome) in answers {
            queue_result(panel, ctx, &op_id, outcome);
        }
        drain(panel, ctx)?;
    }
    // Asking for a command on every answer never settles: stop the panel, as a runaway event loop is
    // stopped, rather than run engine commands for it forever.
    let error = PluginError::Failed(format!("the panel asked for more than {MAX_PUMP_STEPS} commands without settling"));
    let mut all = panels();
    if let Some(panel) = all.get_mut(id) {
        panel.instance.stop(error.clone());
    }
    Err(err(error))
}

fn id_param(cmd: &str, p: &Value) -> Result<String> {
    p.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).ok_or_else(|| bad(cmd, "missing `id` (see plugin.list)"))
}

/// The installed panel plug-in `id` names, ready to start.
fn panel_plugin(cmd: &str, id: &str) -> Result<Arc<Plugin>> {
    let plugin = registry::get(id).ok_or_else(|| err(PluginError::NotFound(id.into())))?;
    if !plugin.is_panel() {
        return Err(bad(cmd, format!("plug-in {id:?} is a filter, not a panel")));
    }
    Ok(plugin)
}

/// The active document as `doc.info` describes it, or the one `doc` names, or `None`.
fn active_doc(s: &Session, p: &Value) -> Option<Value> {
    let doc = match p.get("doc").and_then(Value::as_u64) {
        Some(i) => usize::try_from(i).ok().and_then(|i| s.documents().get(i)),
        None => s.active(),
    };
    doc.map(crate::inspect::document)
}

/// Everything the host knows about one panel, for `plugin.panel.inspect` and `ui.inspect`.
fn inspect_panel(panel: &Panel) -> Value {
    let plugin = panel.instance.plugin();
    let caps = &plugin.manifest().capabilities;
    json!({
        "id": plugin.id(),
        "name": plugin.manifest().name,
        "abi": plugin.abi(),
        "state": if panel.stopped().is_some() { "stopped" } else { "running" },
        "error": panel.stopped().map(ToString::to_string),
        "visible": panel.visible,
        "view": panel.view.view.as_ref().and_then(|v| serde_json::to_value(v).ok()),
        "values": &panel.view.values,
        "progress": &panel.view.progress,
        "notices": &panel.view.notices,
        "storage": &panel.storage,
        "capabilities": serde_json::to_value(caps).unwrap_or(Value::Null),
        "grants": grants(caps),
        "timers": panel.instance.timers().len(),
        "blobs": panel.instance.blobs().len(),
        "blobBytes": panel.instance.blobs().total(),
        "queued": panel.instance.queue().len(),
        "events": panel.steps,
    })
}

// ---------- Commands ----------

fn open(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "plugin.panel.open";
    let id = id_param(CMD, p)?;
    let plugin = panel_plugin(CMD, &id)?;
    let doc = active_doc(s, p);
    let ctx = HostCtx { now: now_ms(), doc: doc.as_ref() };
    let caps = plugin.manifest().capabilities.clone();
    // The whole map work happens under the lock, and the panel is driven inside it; whatever it
    // asked for is run afterwards, with the lock down (see [`run_asked`]).
    {
        let mut all = panels();
        if !all.contains_key(&id) {
            if failures().get(&id).is_some_and(Failures::tripped) {
                return Err(EngineError::Other(format!("plug-in {id:?} failed repeatedly; start it again with plugin.panel.restart")));
            }
            let mut panel = Panel::new(plugin)?;
            panel.visible = true;
            // `init` carries what this plug-in stored in an earlier session, so a panel opens where it
            // left off rather than empty. The instance is seeded from the same map (see [`Panel::new`]).
            let init = ops::init(ctx.now, host_info(), LOCALE, &panel.storage, &entropy(), &grants(&caps));
            drive(&mut panel, init, &ctx)?;
            // A panel is told about a document only once it has changed, so the one it opened against is
            // not announced as news.
            panel.seen = s.active().map(|d| (d.doc.id.0, d.revision));
            all.insert(id.clone(), panel);
        } else if let Some(panel) = all.get_mut(&id) {
            if let Some(reason) = panel.stopped() {
                return Err(EngineError::Other(format!("plug-in {id:?} is stopped ({reason}); start it again with plugin.panel.restart")));
            }
            panel.visible = true;
            let event = ops::panel_visible(ctx.now, true);
            drive(panel, event, &ctx)?;
        }
    }
    run_asked(s, &id, &ctx)?;
    let all = panels();
    Ok(all.get(&id).map(inspect_panel).unwrap_or(Value::Null))
}

fn close(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "plugin.panel.close";
    let id = id_param(CMD, p)?;
    if p.get("stop").and_then(Value::as_bool).unwrap_or(false) {
        // The one way to end an instance without a failure: everything it held goes with it.
        let had = panels().remove(&id).is_some();
        failures().remove(&id);
        return Ok(json!({"id": id, "visible": false, "open": false, "stopped": had}));
    }
    let doc = active_doc(s, p);
    let ctx = HostCtx { now: now_ms(), doc: doc.as_ref() };
    {
        let mut all = panels();
        let Some(panel) = all.get_mut(&id) else {
            return Ok(json!({"id": id, "visible": false, "open": false}));
        };
        panel.visible = false;
        if panel.stopped().is_none() {
            let event = ops::panel_visible(ctx.now, false);
            drive(panel, event, &ctx)?;
        }
    }
    run_asked(s, &id, &ctx)?;
    Ok(json!({"id": id, "visible": false, "open": true}))
}

fn inspect(_s: &mut Session, p: &Value) -> Result<Value> {
    let id = id_param("plugin.panel.inspect", p)?;
    let all = panels();
    Ok(match all.get(&id) {
        Some(panel) => inspect_panel(panel),
        None => json!({"id": id, "state": "closed", "open": false}),
    })
}

/// What a click or an edit in the panel does. The host supplies `now` and the panel's own values: a
/// caller names a widget and a value, and nothing else.
fn event(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "plugin.panel.event";
    let id = id_param(CMD, p)?;
    let event = p.get("event").ok_or_else(|| bad(CMD, "missing `event`"))?;
    let kind = event.get("type").and_then(Value::as_str).ok_or_else(|| bad(CMD, "`event.type` must be a string"))?;
    let widget = |kind: &str| event.get("id").and_then(Value::as_str).ok_or_else(|| bad(CMD, format!("`{kind}` needs a widget `id`")));
    let doc = active_doc(s, p);
    let ctx = HostCtx { now: now_ms(), doc: doc.as_ref() };
    {
        let mut all = panels();
        let panel = all.get_mut(&id).ok_or_else(|| EngineError::Other(format!("plug-in {id:?} has no open panel (see plugin.panel.open)")))?;
        if let Some(reason) = panel.stopped() {
            return Err(EngineError::Other(format!("plug-in {id:?} is stopped ({reason}); start it again with plugin.panel.restart")));
        }
        let values = panel.view.values.clone();
        let event = match kind {
            "ui.click" => ops::ui_click(ctx.now, widget("ui.click")?, event.get("value").unwrap_or(&Value::Null), &values),
            "ui.change" => ops::ui_change(ctx.now, widget("ui.change")?, event.get("value").unwrap_or(&Value::Null), &values),
            "cancel" => ops::cancel(ctx.now),
            other => return Err(bad(CMD, format!("`{other}` is not a panel event (`ui.click`, `ui.change` or `cancel`)"))),
        };
        drive(panel, event, &ctx)?;
    }
    run_asked(s, &id, &ctx)?;
    let all = panels();
    let panel = all.get(&id).ok_or_else(|| EngineError::Other(format!("plug-in {id:?} has no open panel (see plugin.panel.open)")))?;
    Ok(inspect_panel(panel))
}

/// Advance the panels' clock: fire the timers that came due, and tell a panel when the active
/// document has changed. A UI calls this once a frame; a test calls it to make time pass.
///
/// A panel that fails while being ticked is stopped and named in `errors`, and the other panels are
/// ticked anyway: a UI calls this every frame, and one broken panel must not silence the rest — nor
/// fill the status bar with the same complaint sixty times a second.
fn tick(s: &mut Session, p: &Value) -> Result<Value> {
    let only = p.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let doc = active_doc(s, p);
    let ctx = HostCtx { now: now_ms(), doc: doc.as_ref() };
    let (doc_id, revision) = s.active().map(|d| (Some(d.doc.id.0), d.revision)).unwrap_or((None, 0));
    let mut fired = 0usize;
    let mut errors: Vec<Value> = Vec::new();
    // Panels that asked for a command while being ticked, to be run afterwards with the lock down.
    let mut asked_ids: Vec<String> = Vec::new();
    let panels_now;
    {
        let mut all = panels();
        if let Some(id) = only
            && !all.contains_key(id)
        {
            return Err(EngineError::Other(format!("plug-in {id:?} has no open panel")));
        }
        panels_now = all.len();
        for (id, panel) in all.iter_mut() {
            if only.is_some_and(|want| want != id) || panel.stopped().is_some() {
                continue;
            }
            for timer in panel.instance.timers_mut().take_due(ctx.now) {
                fired += 1;
                // A timer fires as the `result` of the operation that armed it.
                let event = ops::result(ctx.now, &OpId::Text(timer), true, json!({}));
                if let Err(e) = drive(panel, event, &ctx) {
                    errors.push(json!({"id": id, "error": e.to_string()}));
                    break;
                }
            }
            if panel.stopped().is_some() {
                continue;
            }
            let seen = doc_id.map(|d| (d, revision));
            if seen != panel.seen {
                panel.seen = seen;
                let event = ops::doc_changed(ctx.now, doc.as_ref().unwrap_or(&Value::Null), revision);
                if let Err(e) = drive(panel, event, &ctx) {
                    errors.push(json!({"id": id, "error": e.to_string()}));
                }
            }
            if !panel.asked.is_empty() {
                asked_ids.push(id.clone());
            }
        }
    }
    // A command a panel asked for while being ticked runs with the lock down, as everywhere else.
    for id in asked_ids {
        if let Err(e) = run_asked(s, &id, &ctx) {
            errors.push(json!({"id": id, "error": e.to_string()}));
        }
    }
    Ok(json!({"id": only, "timers": fired, "revision": revision, "panels": panels_now, "errors": errors}))
}

/// Start a stopped panel again. This is the user's explicit request, so it is also what clears the
/// failure window.
fn restart(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "plugin.panel.restart";
    let id = id_param(CMD, p)?;
    let plugin = panel_plugin(CMD, &id)?;
    let doc = active_doc(s, p);
    let ctx = HostCtx { now: now_ms(), doc: doc.as_ref() };
    let caps = plugin.manifest().capabilities.clone();
    failures().remove(&id);
    let mut panel = Panel::new(plugin)?;
    panel.visible = true;
    // A restart is the same plug-in starting again, so it gets its storage back — which for a panel
    // that failed mid-edit is the difference between picking up and starting over.
    let init = ops::init(ctx.now, host_info(), LOCALE, &panel.storage, &entropy(), &grants(&caps));
    drive(&mut panel, init, &ctx)?;
    panel.seen = s.active().map(|d| (d.doc.id.0, d.revision));
    panels().insert(id.clone(), panel);
    run_asked(s, &id, &ctx)?;
    let all = panels();
    Ok(all.get(&id).map(inspect_panel).unwrap_or(Value::Null))
}

/// Ends a plug-in's panel instance, if it has one. Called when a plug-in is removed or replaced.
///
/// What the plug-in stored is kept: replacing a plug-in is an upgrade of the same plug-in, and an
/// upgrade that forgot the user's settings would be a poor one. Removing it for good is
/// [`forget`]'s job.
pub(crate) fn stop_all(id: &str) {
    panels().remove(id);
    failures().remove(id);
}

/// Forgets everything the host held for a plug-in that is no longer installed: its stored values and
/// its failure window, so a future plug-in with the same id starts clean.
pub(crate) fn forget(id: &str) {
    stop_all(id);
    storages().remove(id);
}

/// Every open panel, for the UI and for `ui.inspect`.
pub fn inspect_open() -> Vec<Value> {
    panels().values().map(inspect_panel).collect()
}

/// The ids of the open panels, in order: every panel that has an instance, drawn or not.
pub fn open_ids() -> Vec<String> {
    panels().keys().cloned().collect()
}

/// The ids of the panels that are actually on screen, in order.
///
/// Not the same as [`open_ids`]: closing a panel's window hides it and keeps its instance, so a panel
/// can be open without being shown. Anything a user reads as "is this panel up?" — a menu row's
/// checkmark, whether picking that row shows or hides — means this.
pub fn visible_ids() -> Vec<String> {
    panels().iter().filter(|(_, panel)| panel.visible).map(|(id, _)| id.clone()).collect()
}

/// The plug-in panel command specs.
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "plugin.panel.open",
            label: "Open Plug-in Panel",
            // No menu item of its own: the UI lists the installed panels by name (Window › Plug-ins),
            // which is what a user picks from. This is the command behind those rows.
            menu: &[],
            shortcut: None,
            params: r##"{"id":text,"doc":index?}"##,
            enabled: always,
            run: open,
            journal: false,
        },
        CommandSpec {
            id: "plugin.panel.close",
            label: "Close Plug-in Panel",
            menu: &[],
            shortcut: None,
            params: r##"{"id":text,"stop":bool=false} (stop: end the instance too, dropping its state)"##,
            enabled: always,
            run: close,
            journal: false,
        },
        CommandSpec {
            id: "plugin.panel.inspect",
            label: "Inspect Plug-in Panel",
            menu: &[],
            shortcut: None,
            params: r##"{"id":text}"##,
            enabled: always,
            run: inspect,
            journal: false,
        },
        CommandSpec {
            id: "plugin.panel.event",
            label: "Send Plug-in Panel Event",
            menu: &[],
            shortcut: None,
            params: r##"{"id":text,"event":{"type":"ui.click"|"ui.change"|"cancel","id":text?,"value":json?}}"##,
            enabled: always,
            run: event,
            journal: false,
        },
        CommandSpec {
            id: "plugin.panel.tick",
            label: "Tick Plug-in Panels",
            menu: &[],
            shortcut: None,
            params: r##"{"id":text?} (fires the timers that came due; announces a changed document)"##,
            enabled: always,
            run: tick,
            journal: false,
        },
        CommandSpec {
            id: "plugin.panel.restart",
            label: "Restart Plug-in Panel",
            menu: &[],
            shortcut: None,
            params: r##"{"id":text}"##,
            enabled: always,
            run: restart,
            journal: false,
        },
    ]
}

#[cfg(test)]
#[path = "plugin_panel_cmds/tests.rs"]
mod tests;

mod permissions;
