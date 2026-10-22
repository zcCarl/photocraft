//! Plug-ins provided by the page that embeds PhotoCraft (the web build's host bridge). The host
//! registers each with a name and a parameter spec; it is listed under Filter › Plug-ins, opens
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

pub(crate) fn menu_items(app: &PhotocraftApp, path: &[String]) -> Vec<MenuItem> {
    let enabled = app.session.active().is_some();
    list()
        .into_iter()
        .map(|p| {
            let dots = if p.params.trim().is_empty() || p.name.ends_with('…') { "" } else { "…" };
            MenuItem { id: format!("{PREFIX}{}", p.id), label: format!("{}{dots}", p.name), path: path.to_vec(), shortcut: None, enabled, checked: None, color: None }
        })
        .collect()
}

pub(crate) fn is_enabled(app: &PhotocraftApp, id: &str) -> Option<bool> {
    let pid = id.strip_prefix(PREFIX)?;
    Some(get(pid).is_some() && app.session.active().is_some())
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
    get(pid).ok_or_else(|| format!("no host plug-in {pid:?}"))?;
    let document = app.session.active().map(|s| s.doc.name.clone());
    INVOKED.lock().unwrap_or_else(|e| e.into_inner()).push(json!({"plugin": pid, "params": params, "document": document}));
    Ok(json!({"queued": pid}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_plugin_is_listed_opens_its_dialog_and_hands_ok_to_the_host() {
        register(HostPlugin { id: "test.split".into(), name: "Split".into(), params: r#"{"说明":text, "尺寸":"1K|2K"="1K"}"#.into(), note: "costs".into() });
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
}
