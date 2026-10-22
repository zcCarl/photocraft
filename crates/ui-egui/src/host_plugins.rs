//! Plug-ins provided by the page that embeds PhotoCraft (the web build's host bridge). The host
//! registers each with a name and a parameter spec; it is listed under Filter › Plug-ins (or under
//! its own top-level menu, after Filter, when it names one), opens
//! the generated parameter dialog, and OK hands the chosen values back to the host as an
//! invocation. The host does the work and drives the editor through its bridge methods.

use std::sync::Mutex;

use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::menus::MenuItem;

/// Menu and command ids of host plug-ins: `plugin.host.<id>`.
pub const PREFIX: &str = "plugin.host.";

#[derive(Clone, Debug, PartialEq)]
pub struct HostPlugin {
    pub id: String,
    pub name: String,
    /// Parameter notation, as for WebAssembly plug-ins (`{"key":text, "size":"a|b"="a"}`).
    pub params: String,
    /// Shown in its dialog (what running it costs, where it runs).
    pub note: String,
    /// A top-level menu of its own, shown after Filter; empty lists it under Filter › Plug-ins.
    pub menu: String,
    /// Greyed out with no document open (a plug-in that works on the canvas).
    pub needs_document: bool,
}

static PLUGINS: Mutex<Vec<HostPlugin>> = Mutex::new(Vec::new());
static INVOKED: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// Adds or replaces a host plug-in (same id).
pub fn register(plugin: HostPlugin) {
    let mut list = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|p| p.id != plugin.id);
    list.push(plugin);
}

pub fn list() -> Vec<HostPlugin> {
    PLUGINS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn get(id: &str) -> Option<HostPlugin> {
    list().into_iter().find(|p| p.id == id)
}

/// Invocations not yet passed to the host: `{plugin, params, document}`.
pub fn take_invocations() -> Vec<Value> {
    std::mem::take(&mut *INVOKED.lock().unwrap_or_else(|e| e.into_inner()))
}

fn item(app: &PhotocraftApp, p: HostPlugin, path: Vec<String>) -> MenuItem {
    let dots = if p.params.trim().is_empty() || p.name.ends_with('…') { "" } else { "…" };
    let enabled = enabled(app, &p);
    MenuItem { id: format!("{PREFIX}{}", p.id), label: format!("{}{dots}", p.name), path, shortcut: None, enabled, checked: None, color: None }
}

fn enabled(app: &PhotocraftApp, p: &HostPlugin) -> bool {
    !p.needs_document || app.session.active().is_some()
}

/// The plug-ins listed under Filter › Plug-ins (`path`): those without a menu of their own.
pub(crate) fn menu_items(app: &PhotocraftApp, path: &[String]) -> Vec<MenuItem> {
    list().into_iter().filter(|p| p.menu.is_empty()).map(|p| item(app, p, path.to_vec())).collect()
}

/// The top-level menus host plug-ins name, in registration order.
pub fn top_menus() -> Vec<String> {
    let mut menus: Vec<String> = Vec::new();
    for p in list() {
        if !p.menu.is_empty() && !menus.contains(&p.menu) {
            menus.push(p.menu);
        }
    }
    menus
}

/// The rows of the plug-ins' own top-level menus.
pub(crate) fn top_menu_items(app: &PhotocraftApp) -> Vec<MenuItem> {
    list().into_iter().filter(|p| !p.menu.is_empty()).map(|p| {
        let path = vec![p.menu.clone()];
        item(app, p, path)
    }).collect()
}

pub(crate) fn is_enabled(app: &PhotocraftApp, id: &str) -> Option<bool> {
    let pid = id.strip_prefix(PREFIX)?;
    Some(get(pid).is_some_and(|p| enabled(app, &p)))
}

/// The menu item: its dialog when it takes parameters and none were given, else the invocation.
pub(crate) fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let pid = id.strip_prefix(PREFIX)?;
    let Some(plugin) = get(pid) else { return Some(Err(format!("no host plug-in {pid:?}"))) };
    let given = params.as_object().is_some_and(|o| !o.is_empty());
    if plugin.params.trim().is_empty() || given {
        return Some(invoke(app, id, params));
    }
    let d = crate::filter_dialog::open_with_spec(app, id, &plugin.name, &plugin.params, Map::new());
    if let Some(dialog) = app.ui.dialog_mut(d) {
        // The host runs it: there is nothing to preview on the canvas.
        dialog.fields.remove("__preview");
        if !plugin.note.is_empty() {
            dialog.fields.insert("__note".into(), json!(plugin.note));
        }
    }
    Some(Ok(json!({"dialog": d})))
}

/// `plugin.host.<id>` run as a command (the dialog's OK): queues it for the host.
pub(crate) fn invoke(app: &PhotocraftApp, id: &str, params: &Value) -> Result<Value, String> {
    let pid = id.strip_prefix(PREFIX).ok_or("not a host plug-in")?;
    let plugin = get(pid).ok_or_else(|| format!("no host plug-in {pid:?}"))?;
    if !enabled(app, &plugin) {
        return Err("open a document first".into());
    }
    let document = app.session.active().map(|s| s.doc.name.clone());
    INVOKED.lock().unwrap_or_else(|e| e.into_inner()).push(json!({"plugin": pid, "params": params, "document": document}));
    Ok(json!({"queued": pid}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_plugin_is_listed_opens_its_dialog_and_hands_ok_to_the_host() {
        register(HostPlugin { id: "test.split".into(), name: "Split".into(), params: r#"{"说明":text, "尺寸":"1K|2K"="1K"}"#.into(), note: "costs".into(), menu: String::new(), needs_document: true });
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        let item = crate::menus::menu_items(&app).into_iter().find(|i| i.id == "plugin.host.test.split").expect("listed");
        assert_eq!(item.path, ["Filter", "Plug-ins"]);
        assert!(!crate::menus::is_enabled(&app, "plugin.host.test.split"), "no document yet");
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        assert!(crate::menus::is_enabled(&app, "plugin.host.test.split"));
        let r = crate::menus::invoke(&mut app, &ctx, "plugin.host.test.split", json!({})).unwrap();
        let d = r["dialog"].as_u64().unwrap();
        let fields = &app.ui.dialog_mut(d).unwrap().fields;
        assert_eq!(fields.get("尺寸"), Some(&json!("1K")));
        assert_eq!(fields.get("__note"), Some(&json!("costs")));
        assert!(!fields.contains_key("__preview"));
        take_invocations();
        crate::dialogs::confirm(&mut app, d).unwrap();
        let sent = take_invocations();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["plugin"], "test.split");
        assert_eq!(sent[0]["params"]["尺寸"], "1K");
    }

    #[test]
    fn host_plugin_with_its_own_menu_gets_a_top_level_title_after_filter() {
        register(HostPlugin { id: "test.own.config".into(), name: "Setup".into(), params: r#"{"key":secret}"#.into(), note: String::new(), menu: "Gorilla".into(), needs_document: false });
        let tops = crate::menus::top_menus();
        let filter = tops.iter().position(|t| t == "Filter").unwrap();
        assert_eq!(tops[filter + 1], "Gorilla");
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let items = crate::menus::menu_items(&app);
        let item = items.iter().find(|i| i.id == "plugin.host.test.own.config").expect("listed");
        assert_eq!(item.path, ["Gorilla"]);
        assert!(item.enabled, "needs no document");
        assert!(!items.iter().any(|i| i.id == "plugin.host.test.own.config" && i.path.len() > 1), "not also under Filter");
        assert_eq!(crate::filter_dialog::parse_spec(r#"{"key":secret}"#)[0].kind, crate::filter_dialog::Kind::Secret);
        PLUGINS.lock().unwrap().retain(|p| p.id != "test.own.config");
    }
}
