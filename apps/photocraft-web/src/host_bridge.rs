//! The host-page bridge: drive PhotoCraft from the page that embeds it in an `<iframe>`.
//!
//! The parent window posts `{id, method, params}` with `postMessage`; each request gets one reply
//! `{photocraft: 1, id, ok, result | error}` posted back to the parent. Only the parent window of
//! the same origin is listened to, and replies go to that origin only, so another page or frame
//! can neither drive the editor nor read a document through it.
//!
//! Methods (bytes travel as base64):
//! - `host.hello`: the protocol version, `{protocol, version}`
//! - `host.open {name, data}`: open encoded bytes (PSD, PNG, …) as a new document
//! - `host.export {format?: png|psd, layer?}`: encode the active document, or one layer of it
//! - `host.layers`: the active document's layers, bottom to top, with their bounds
//! - `host.place {name, data, x?, y?, smartObject?}`: place an image as a layer whose top-left is
//!   at `x, y` (document pixels, unscaled); without them it is centred
//! - `host.registerPlugin {id, name, params?, note?, menu?, needsDocument?}`: list a host plug-in
//!   under Filter › Plug-ins, or under its own top-level `menu` (shown after Filter); with
//!   `needsDocument: false` it stays enabled with no document open. Its dialog's OK posts the event `{photocraft: 1, event: "plugin.invoke", plugin, params, document}`
//! - `host.notify {title, lines?, error?}`: show a notice in the editor
//! - anything else is a desktop control-channel request (`engine.execute`, `ui.inspect`, …),
//!   forwarded to [`PhotocraftApp::with_control`]'s channel with its reply passed back

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use base64::Engine as _;
use photocraft_ui_egui::{ControlRequest, PhotocraftApp};
use serde_json::{Value, json};
use wasm_bindgen::JsCast as _;

/// The version of this message protocol, reported by `host.hello`.
pub const PROTOCOL: u32 = 1;

/// Requests larger than this are refused before they are decoded.
const MAX_REQUEST_CHARS: u32 = 256 * 1024 * 1024;

struct Incoming {
    id: Value,
    method: String,
    params: Value,
}

type Queue = Rc<RefCell<VecDeque<Result<Incoming, (Value, String)>>>>;

pub struct HostBridge {
    queue: Queue,
    control: Sender<ControlRequest>,
    /// Forwarded requests waiting for the app's reply.
    pending: Vec<(Value, Receiver<Value>)>,
    origin: String,
}

impl HostBridge {
    /// Listens for the parent's messages. `control` is the sending side of the channel given to
    /// [`PhotocraftApp::with_control`]. `None` when the page has no parent window (not embedded).
    pub fn start(control: Sender<ControlRequest>, ctx: egui::Context) -> Option<Self> {
        let window = web_sys::window()?;
        let parent = window.parent().ok().flatten()?;
        // A top-level page is its own parent.
        if js_sys::Object::is(&parent, &window) {
            return None;
        }
        let origin = window.location().origin().ok()?;
        let queue = Queue::default();
        listen(&window, parent, origin.clone(), queue.clone(), ctx);
        log::info!("photocraft-web: host bridge listening for {origin}");
        Some(Self { queue, control, pending: Vec::new(), origin })
    }

    /// Runs the queued requests: `host.*` here, the rest forwarded to the control channel (the app
    /// handles those in its next `logic`). Call before [`PhotocraftApp::logic`].
    pub fn dispatch(&mut self, app: &mut PhotocraftApp) {
        let queued: Vec<_> = self.queue.borrow_mut().drain(..).collect();
        for entry in queued {
            match entry {
                Err((id, error)) => self.reply(&id, Err(error)),
                Ok(req) if req.method.starts_with("host.") => {
                    let result = host_method(app, &req.method, &req.params);
                    self.reply(&req.id, result);
                }
                Ok(req) => {
                    // No deadline: `Instant::now` is unavailable on wasm32-unknown-unknown.
                    let (request, rx) = ControlRequest::new(req.method, req.params);
                    if self.control.send(request).is_err() {
                        self.reply(&req.id, Err("the control channel is closed".into()));
                    } else {
                        self.pending.push((req.id, rx));
                    }
                }
            }
        }
    }

    /// Posts the replies the app has produced. Call after [`PhotocraftApp::logic`]; replies that
    /// wait for a job or a screenshot arrive on a later frame.
    pub fn collect(&mut self) {
        let mut done = Vec::new();
        self.pending.retain(|(id, rx)| match rx.try_recv() {
            Ok(reply) => {
                done.push((id.clone(), Ok(reply)));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => {
                done.push((id.clone(), Err("the request ended without a reply".to_string())));
                false
            }
        });
        for (id, reply) in done {
            match reply {
                // Control replies already carry `ok` and `result` or `error`.
                Ok(envelope) => self.post(&id, envelope),
                Err(error) => self.reply(&id, Err(error)),
            }
        }
    }

    /// Posts the host plug-in invocations the app queued this frame as `plugin.invoke` events.
    pub fn events(&self) {
        for mut invocation in photocraft_ui_egui::host_plugins::take_invocations() {
            invocation["event"] = json!("plugin.invoke");
            invocation["photocraft"] = json!(PROTOCOL);
            self.send(invocation);
        }
    }

    /// Whether replies are still outstanding (the frame loop keeps running for them).
    pub fn waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    fn reply(&self, id: &Value, result: Result<Value, String>) {
        let envelope = match result {
            Ok(result) => json!({"ok": true, "result": result}),
            Err(error) => json!({"ok": false, "error": error}),
        };
        self.post(id, envelope);
    }

    fn post(&self, id: &Value, envelope: Value) {
        let mut message = match envelope {
            Value::Object(map) => map,
            other => {
                let mut map = serde_json::Map::new();
                map.insert("ok".into(), json!(true));
                map.insert("result".into(), other);
                map
            }
        };
        message.insert("photocraft".into(), json!(PROTOCOL));
        message.insert("id".into(), id.clone());
        self.send(Value::Object(message));
    }

    fn send(&self, message: Value) {
        let Some(parent) = web_sys::window().and_then(|w| w.parent().ok().flatten()) else { return };
        let text = message.to_string();
        match js_sys::JSON::parse(&text) {
            Ok(value) => {
                if let Err(e) = parent.post_message(&value, &self.origin) {
                    log::error!("host bridge: could not post a reply: {e:?}");
                }
            }
            Err(e) => log::error!("host bridge: could not encode a reply: {e:?}"),
        }
    }
}

/// Queues each message from `parent` at `origin`; every other message is ignored.
fn listen(window: &web_sys::Window, parent: web_sys::Window, origin: String, queue: Queue, ctx: egui::Context) {
    use wasm_bindgen::closure::Closure;
    let cb = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
        if e.origin() != origin || !e.source().is_some_and(|s| js_sys::Object::is(&s, &parent)) {
            return;
        }
        let parsed = parse(&e.data());
        // Messages that aren't requests at all (another library's traffic) get no reply.
        if let Some(entry) = parsed {
            queue.borrow_mut().push_back(entry);
            ctx.request_repaint();
        }
    });
    if window.add_event_listener_with_callback("message", cb.as_ref().unchecked_ref()).is_ok() {
        cb.forget();
    }
}

/// A request from structured-clone data: `None` when it isn't one (no string `method`).
fn parse(data: &wasm_bindgen::JsValue) -> Option<Result<Incoming, (Value, String)>> {
    let text = js_sys::JSON::stringify(data).ok()?;
    if text.length() > MAX_REQUEST_CHARS {
        return Some(Err((Value::Null, "the request is too large".into())));
    }
    let value: Value = serde_json::from_str(&String::from(text)).ok()?;
    let method = value.get("method")?.as_str()?.to_string();
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Some(Err((id, "`params` must be an object".into())));
    }
    Some(Ok(Incoming { id, method, params }))
}

fn host_method(app: &mut PhotocraftApp, method: &str, p: &Value) -> Result<Value, String> {
    match method {
        "host.hello" => Ok(json!({"protocol": PROTOCOL, "version": env!("CARGO_PKG_VERSION")})),
        "host.open" => {
            let name = name_param(p)?;
            let bytes = data_param(p)?;
            let warnings = app.open_bytes(&name, &bytes)?;
            Ok(json!({"warnings": warnings, "document": app.session.active().map(|d| d.doc.name.clone())}))
        }
        "host.export" => export(app, p),
        "host.layers" => layers(app),
        "host.place" => place(app, p),
        "host.registerPlugin" => register_plugin(p),
        "host.notify" => {
            let title = p.get("title").and_then(Value::as_str).ok_or("missing `title`")?;
            let lines = p.get("lines").and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            let error = p.get("error").and_then(Value::as_bool).unwrap_or(false);
            Ok(json!({"notice": photocraft_ui_egui::notices::post(app, title, lines, error, None)}))
        }
        _ => Err(format!("unknown host method `{method}`")),
    }
}

fn register_plugin(p: &Value) -> Result<Value, String> {
    let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let id = text("id");
    if id.is_empty() || id.len() > 100 || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return Err("`id` must be 1–100 characters of a-z, 0-9, . - _".into());
    }
    let name = text("name");
    if name.is_empty() || name.len() > 200 {
        return Err("`name` must be 1–200 characters".into());
    }
    let menu = text("menu");
    if menu.len() > 60 || menu.contains('/') {
        return Err("`menu` must be at most 60 characters, without `/`".into());
    }
    let needs_document = p.get("needsDocument").and_then(Value::as_bool).unwrap_or(true);
    use photocraft_ui_egui::host_plugins::{HostPlugin, PREFIX, register};
    register(HostPlugin { id: id.clone(), name, params: text("params"), note: text("note"), menu, needs_document });
    Ok(json!({"menu": format!("{PREFIX}{id}")}))
}

fn name_param(p: &Value) -> Result<String, String> {
    let name = p.get("name").and_then(Value::as_str).ok_or("missing `name`")?;
    if name.is_empty() || name.len() > 255 {
        return Err("`name` must be 1–255 characters".into());
    }
    Ok(name.to_string())
}

fn data_param(p: &Value) -> Result<Vec<u8>, String> {
    let data = p.get("data").and_then(Value::as_str).ok_or("missing `data` (base64)")?;
    base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| format!("`data` is not base64: {e}"))
}

fn export(app: &PhotocraftApp, p: &Value) -> Result<Value, String> {
    let format = match p.get("format").and_then(Value::as_str).unwrap_or("png") {
        f @ ("png" | "psd") => f,
        other => return Err(format!("unsupported format `{other}` (png or psd)")),
    };
    let state = app.session.active().ok_or("no document is open")?;
    let layer = match p.get("layer") {
        None | Some(Value::Null) => None,
        Some(v) => Some(photocraft_doc::LayerId(v.as_u64().ok_or("`layer` must be a layer id")?)),
    };
    // A layer is trimmed to its pixels; `left`/`top` say where they sit on the canvas.
    let (doc, (left, top)) = match layer {
        Some(id) => photocraft_engine::layer_menu_cmds::layer_document_at(&state.doc, id).map_err(|e| e.to_string())?,
        None => ((*state.doc).clone(), (0, 0)),
    };
    // `base.png` exports as `base.psd`, not `base.png.psd`.
    let stem = std::path::Path::new(&doc.name).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty());
    let name = format!("{}.{format}", stem.unwrap_or("photocraft"));
    let out = photocraft_io::export(&doc, &name, &photocraft_io::ExportOptions::default()).map_err(|e| e.to_string())?;
    Ok(json!({
        "name": name,
        "width": doc.size.width,
        "height": doc.size.height,
        "left": left,
        "top": top,
        "data": base64::engine::general_purpose::STANDARD.encode(&out.bytes),
        "warnings": out.warnings,
    }))
}

fn layers(app: &PhotocraftApp) -> Result<Value, String> {
    let state = app.session.active().ok_or("no document is open")?;
    let list: Vec<Value> = state
        .doc
        .walk()
        .into_iter()
        .map(|(_, depth, l)| json!({"id": l.id.0, "name": l.name, "visible": l.visible, "opacity": l.opacity, "depth": depth}))
        .collect();
    Ok(json!({
        "document": state.doc.name,
        "width": state.doc.size.width,
        "height": state.doc.size.height,
        "active": state.active_layer.map(|l| l.0),
        "layers": list,
    }))
}

fn place(app: &mut PhotocraftApp, p: &Value) -> Result<Value, String> {
    let name = name_param(p)?;
    let bytes = data_param(p)?;
    let mut params = json!({
        "fit": false,
        "smartObject": p.get("smartObject").and_then(Value::as_bool).unwrap_or(false),
    });
    let x = p.get("x").and_then(Value::as_f64);
    let y = p.get("y").and_then(Value::as_f64);
    if let (Some(x), Some(y)) = (x, y) {
        // `place_bytes` positions by centre: decode the size to turn the top-left into one.
        let size = photocraft_io::import(&name, &bytes).map_err(|e| format!("{name}: {e}"))?.document.size;
        params["scale"] = json!(100.0);
        params["center"] = json!([x + f64::from(size.width) / 2.0, y + f64::from(size.height) / 2.0]);
    } else if x.is_some() || y.is_some() {
        return Err("give both `x` and `y`, or neither".into());
    }
    photocraft_engine::file_cmds::place_bytes(&mut app.session, &name, bytes, None, &params).map_err(|e| e.to_string())
}
