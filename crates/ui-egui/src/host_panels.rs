//! Panels provided by the page that embeds PhotoCraft (the web build's host bridge). The host
//! sends a panel's whole content (`host.panel`): a title, a row of toolbar actions, a message,
//! rows with a status and their own actions, and a pager. PhotoCraft draws it as a floating
//! panel like its own; clicking an action or closing the panel queues an event for the host,
//! which does the work and sends the new content.

use std::sync::Mutex;

use egui::{RichText, Stroke, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

const WIDTH: f32 = 380.0;
const MAX_ROWS: usize = 200;
const MAX_ACTIONS: usize = 8;
const MAX_LINES: usize = 12;
const MAX_CHARS: usize = 2000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelAction {
    pub id: String,
    pub label: String,
    /// Drawn as a tab, selected or not; `None` is a plain button.
    pub selected: Option<bool>,
    pub enabled: bool,
}

impl Default for PanelAction {
    fn default() -> Self {
        Self { id: String::new(), label: String::new(), selected: None, enabled: true }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelRow {
    pub id: String,
    pub title: String,
    pub status: String,
    /// Status colour: "running", "ok", "error" or "" (dim).
    pub tone: String,
    /// Small faint line under the title (time, id).
    pub meta: String,
    pub lines: Vec<String>,
    pub error: String,
    pub actions: Vec<PanelAction>,
    /// Shown after the row's actions (what the last one did).
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HostPanel {
    pub id: String,
    pub title: String,
    /// Set by the host's `open` and cleared by the panel's close button.
    #[serde(skip)]
    pub open: bool,
    pub toolbar: Vec<PanelAction>,
    pub message: String,
    pub message_error: bool,
    pub rows: Vec<PanelRow>,
    /// Shown when there are no rows.
    pub empty: String,
    pub pager: Vec<PanelAction>,
    pub footer: String,
}

static PANELS: Mutex<Vec<HostPanel>> = Mutex::new(Vec::new());
static EVENTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());

pub fn list() -> Vec<HostPanel> {
    PANELS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Events not yet passed to the host: `{event: "panel.action", panel, action, row}` and
/// `{event: "panel.closed", panel}`.
pub fn take_events() -> Vec<Value> {
    std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn push_event(event: Value) {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).push(event);
}

fn clip(s: &mut String) {
    if s.chars().count() > MAX_CHARS {
        *s = s.chars().take(MAX_CHARS).collect();
    }
}

fn clip_actions(actions: &mut Vec<PanelAction>) {
    actions.truncate(MAX_ACTIONS);
    for a in actions {
        clip(&mut a.id);
        clip(&mut a.label);
    }
}

/// `host.panel {id, title, open?, toolbar?, message?, messageError?, rows?, empty?, pager?,
/// footer?}`: adds or replaces a panel's content. `open` shows or hides it; without it the panel
/// stays as it is, so a refresh never reopens a panel the user closed. Replies `{open}`.
pub fn update(params: &Value) -> Result<Value, String> {
    let mut panel: HostPanel = serde_json::from_value(params.clone()).map_err(|e| format!("not a panel: {e}"))?;
    let id = &panel.id;
    if id.is_empty() || id.len() > 100 || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
        return Err("`id` must be 1–100 characters of a-z, 0-9, . - _".into());
    }
    if panel.title.is_empty() || panel.title.len() > 200 {
        return Err("`title` must be 1–200 characters".into());
    }
    for s in [&mut panel.message, &mut panel.empty, &mut panel.footer] {
        clip(s);
    }
    clip_actions(&mut panel.toolbar);
    clip_actions(&mut panel.pager);
    panel.rows.truncate(MAX_ROWS);
    for row in &mut panel.rows {
        for s in [&mut row.id, &mut row.title, &mut row.status, &mut row.tone, &mut row.meta, &mut row.error, &mut row.note] {
            clip(s);
        }
        row.lines.truncate(MAX_LINES);
        row.lines.iter_mut().for_each(clip);
        clip_actions(&mut row.actions);
    }
    let open = params.get("open").and_then(Value::as_bool);
    let mut list = PANELS.lock().unwrap_or_else(|e| e.into_inner());
    let was = list.iter().find(|p| p.id == panel.id).is_some_and(|p| p.open);
    panel.open = open.unwrap_or(was);
    let reply = json!({"open": panel.open});
    list.retain(|p| p.id != panel.id);
    list.push(panel);
    Ok(reply)
}

fn close(id: &str) {
    let mut list = PANELS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = list.iter_mut().find(|p| p.id == id) {
        p.open = false;
    }
}

/// Draws the open host panels.
pub fn windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let open: Vec<HostPanel> = list().into_iter().filter(|p| p.open).collect();
    for (i, panel) in open.iter().enumerate() {
        let mut closed = false;
        let mut clicked: Option<(String, Option<String>)> = None;
        let max_height = (app.last_canvas_rect.height() - 170.0).max(160.0);
        let stagger = 24.0 * i as f32;
        crate::analysis_ui::panel_window(app, ctx, &format!("host-panel-{}", panel.id), &panel.title, vec2(-stagger, 40.0 + stagger), WIDTH, |ui| {
            closed = crate::analysis_ui::title_row(ui, &panel.title);
            body(ui, panel, max_height, &mut clicked);
        });
        if closed {
            close(&panel.id);
            push_event(json!({"event": "panel.closed", "panel": panel.id}));
        } else if let Some((action, row)) = clicked {
            push_event(json!({"event": "panel.action", "panel": panel.id, "action": action, "row": row}));
        }
    }
}

fn body(ui: &mut egui::Ui, p: &HostPanel, max_height: f32, clicked: &mut Option<(String, Option<String>)>) {
    let t = Tokens::get(ui.ctx());
    if !p.toolbar.is_empty() {
        ui.horizontal_wrapped(|ui| actions(ui, &p.toolbar, None, clicked));
        ui.add_space(4.0);
    }
    if !p.message.is_empty() {
        ui.add(egui::Label::new(RichText::new(&p.message).color(if p.message_error { t.warning } else { t.text_dim })).wrap());
        ui.add_space(4.0);
    }
    egui::ScrollArea::vertical().id_salt(("host-panel-rows", &p.id)).max_height(max_height).auto_shrink([false, true]).show(ui, |ui| {
        if p.rows.is_empty() && !p.empty.is_empty() {
            ui.add(egui::Label::new(RichText::new(&p.empty).color(t.text_faint)).wrap());
        }
        for row in &p.rows {
            row_card(ui, &t, row, clicked);
            ui.add_space(6.0);
        }
    });
    if !p.pager.is_empty() || !p.footer.is_empty() {
        crate::widgets::hairline(ui);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            actions(ui, &p.pager, None, clicked);
            if !p.footer.is_empty() {
                ui.label(RichText::new(&p.footer).color(t.text_dim));
            }
        });
    }
}

fn actions(ui: &mut egui::Ui, list: &[PanelAction], row: Option<&str>, clicked: &mut Option<(String, Option<String>)>) {
    for a in list {
        let resp = ui
            .add_enabled_ui(a.enabled, |ui| match a.selected {
                Some(on) => crate::widgets::pill_tab(ui, &a.label, on),
                None => crate::widgets::compact_button(ui, &a.label, 24.0),
            })
            .inner;
        if resp.clicked() {
            *clicked = Some((a.id.clone(), row.map(str::to_string)));
        }
    }
}

fn row_card(ui: &mut egui::Ui, t: &Tokens, row: &PanelRow, clicked: &mut Option<(String, Option<String>)>) {
    let tone = match row.tone.as_str() {
        "running" => t.accent,
        "ok" => t.text,
        "error" => t.warning,
        _ => t.text_dim,
    };
    egui::Frame::NONE.fill(t.field).stroke(Stroke::new(1.0, t.field_border)).corner_radius(t.radius).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Sides::new().shrink_left().truncate().show(
            ui,
            |ui| ui.label(RichText::new(&row.title).strong().color(t.text)),
            |ui| ui.label(RichText::new(&row.status).color(tone)),
        );
        if !row.meta.is_empty() {
            ui.label(RichText::new(&row.meta).size(11.0).color(t.text_faint));
        }
        for line in &row.lines {
            ui.add(egui::Label::new(RichText::new(line).color(t.text_dim)).wrap());
        }
        if !row.error.is_empty() {
            ui.add(egui::Label::new(RichText::new(&row.error).color(t.warning)).wrap());
        }
        if !row.actions.is_empty() || !row.note.is_empty() {
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                actions(ui, &row.actions, Some(&row.id), clicked);
                if !row.note.is_empty() {
                    ui.label(RichText::new(&row.note).color(t.text_dim));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    fn take(id: &str) -> Vec<Value> {
        let (mine, rest): (Vec<Value>, Vec<Value>) = take_events().into_iter().partition(|e| e["panel"] == id);
        EVENTS.lock().unwrap().extend(rest);
        mine
    }

    #[test]
    fn update_keeps_the_open_state_unless_the_host_sets_it() {
        assert_eq!(update(&json!({"id": "test.keep", "title": "T", "open": true})).unwrap()["open"], true);
        assert_eq!(update(&json!({"id": "test.keep", "title": "T", "rows": [{"id": "a", "title": "A"}]})).unwrap()["open"], true, "a refresh keeps it open");
        close("test.keep");
        assert_eq!(update(&json!({"id": "test.keep", "title": "T"})).unwrap()["open"], false, "and never reopens a closed panel");
        assert!(update(&json!({"id": "bad id", "title": "T"})).is_err());
        assert!(update(&json!({"id": "test.keep2", "title": ""})).is_err());
        PANELS.lock().unwrap().retain(|p| !p.id.starts_with("test.keep"));
    }

    #[test]
    fn row_actions_and_close_are_queued_for_the_host() {
        update(&json!({"id": "test.tasks", "title": "任务列表", "open": true,
            "toolbar": [{"id": "view.active", "label": "运行中", "selected": true}, {"id": "refresh", "label": "刷新"}],
            "rows": [{"id": "web-1", "title": "图像编辑", "status": "已完成", "tone": "ok", "lines": ["换成蓝色"], "actions": [{"id": "place", "label": "放入当前文档"}]}],
            "pager": [{"id": "page.next", "label": "下一页", "enabled": false}], "footer": "1 / 1"}))
        .unwrap();
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let mut h = Harness::builder().with_size(vec2(900.0, 700.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                let ctx = ui.ctx().clone();
                if ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    windows(app, &ctx);
                }
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(4);
        h.get_by_label("换成蓝色");
        h.get_by_label("放入当前文档").click();
        h.run_steps(2);
        assert_eq!(take("test.tasks"), [json!({"event": "panel.action", "panel": "test.tasks", "action": "place", "row": "web-1"})]);
        h.get_by_label("刷新").click();
        h.run_steps(2);
        assert_eq!(take("test.tasks"), [json!({"event": "panel.action", "panel": "test.tasks", "action": "refresh", "row": null})]);
        h.get_by_label("下一页").click();
        h.run_steps(2);
        assert!(take("test.tasks").is_empty(), "a disabled action sends nothing");
        h.get_by_label(crate::i18n::t("Close")).click();
        h.run_steps(2);
        assert_eq!(take("test.tasks"), [json!({"event": "panel.closed", "panel": "test.tasks"})]);
        assert!(!list().iter().any(|p| p.id == "test.tasks" && p.open));
        PANELS.lock().unwrap().retain(|p| p.id != "test.tasks");
    }
}
