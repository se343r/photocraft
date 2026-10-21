//! Window › Plug-ins: the panels of installed ABI v2 panel plug-ins, drawn as floating windows in
//! the app's theme.
//!
//! A panel's own model stays in the plug-in's WebAssembly sandbox; the host draws the widget tree
//! the plug-in describes (see `docs/plugins-v2.md`) and sends back the events those widgets produce.
//! So this module owns almost no state: it takes a snapshot
//! ([`photocraft_engine::plugin_panel_cmds::draw_list`]), draws it, and sends each interaction as
//! `plugin.panel.event`. The one exception is the text being typed: an edit is committed when the
//! field is left, not on every keystroke, so a script box holding a chapter does not cross the
//! sandbox sixty times a second.
//!
//! Nothing in `theme::Tokens` is reachable from a plug-in — it names widgets and levels, never
//! colours or sizes — so a panel is on-theme in light and dark, in every theme, for free.

use std::sync::Arc;

use egui::{Align2, Color32, FontData, FontFamily, FontId, RichText, Sense, Stroke, pos2, vec2};
use photocraft_engine::plugin_panel_cmds::{PanelDraw, Progress};
use photocraft_plugins::ops::NoticeLevel;
use photocraft_plugins::view::{Level, ListRow, Widget};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::menus::MenuItem;
use crate::theme::Tokens;

/// Menu ids of installed panels: `plugin.panel.item.<plug-in id>`.
///
/// Not `plugin.panel.<id>`: that namespace is the engine's (`plugin.panel.open`, `plugin.panel.close`
/// and the rest), and the two must not be able to collide.
const PREFIX: &str = "plugin.panel.item.";

/// The engine's `plugin.panel.*` commands carry no menu of their own (`menu: &[]`), deliberately:
/// these rows are what a user picks from, and they are the only thing in this submenu.
const MENU: [&str; 2] = ["Window", "Plug-ins"];

fn item_id(plugin: &str) -> String {
    format!("{PREFIX}{plugin}")
}

fn plugin_id(item: &str) -> Option<&str> {
    item.strip_prefix(PREFIX)
}

/// Adds a row per installed panel.
pub fn insert_menu_items(app: &PhotocraftApp, items: &mut Vec<MenuItem>) {
    let plugins: Vec<String> = photocraft_plugins::registry::list().iter().filter(|p| p.is_panel()).map(|p| p.id().to_string()).collect();
    if plugins.is_empty() {
        return;
    }
    // The English path, untranslated like every other item's: the menu renderer groups rows by it and
    // translates only at display time (#2540).
    let path: Vec<String> = MENU.iter().map(|s| (*s).to_string()).collect();
    // After the last Window item, so the submenu is inside Window rather than appended to the bar.
    let at = items.iter().rposition(|i| i.path.first().is_some_and(|p| p == "Window")).map_or(items.len(), |p| p + 1);
    let rows: Vec<MenuItem> = plugins
        .into_iter()
        .map(|plugin| {
            let id = item_id(&plugin);
            MenuItem {
                label: title_of(&plugin),
                checked: checked(app, &id),
                id,
                path: path.clone(),
                shortcut: None,
                // Opening a panel needs no document and no selection: it is installed, or it is not.
                enabled: true,
                color: None,
            }
        })
        .collect();
    for (k, row) in rows.into_iter().enumerate() {
        items.insert(at + k, row);
    }
}

/// The name a user knows a panel by: its `panel.title`, which is what a letterer recognises, falling
/// back to the plug-in's own name.
fn title_of(plugin: &str) -> String {
    photocraft_plugins::registry::get(plugin)
        .map(|p| p.manifest().panel.as_ref().map(|panel| panel.title.clone()).unwrap_or_else(|| p.manifest().name.clone()))
        .unwrap_or_else(|| plugin.to_string())
}

/// Enabled state of a panel row (`None` for every other id, so the menu falls through to the rest).
pub fn is_enabled(_app: &PhotocraftApp, id: &str) -> Option<bool> {
    let plugin = plugin_id(id)?;
    Some(photocraft_plugins::registry::get(plugin).is_some_and(|p| p.is_panel()))
}

/// Whether the panel behind this row is on screen, for the row's checkmark.
///
/// On screen, not merely instantiated: a closed window keeps its plug-in running (the panel is told
/// with `panel.visible`), so a checkmark that followed the instance would stay on after the user
/// closed the window — and picking the row again would then close it a second time.
pub fn checked(_app: &PhotocraftApp, id: &str) -> Option<bool> {
    let plugin = plugin_id(id)?;
    Some(photocraft_engine::plugin_panel_cmds::visible_ids().contains(&plugin.to_string()))
}

/// Window › Plug-ins › *panel*: shows or hides it. `params.show` forces one or the other, and
/// `params.restart` starts a stopped panel over.
pub fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let plugin = plugin_id(id)?.to_string();
    if params.get("restart").and_then(Value::as_bool) == Some(true) {
        return Some(app.run("plugin.panel.restart", json!({"id": plugin})));
    }
    let open = photocraft_engine::plugin_panel_cmds::visible_ids().contains(&plugin);
    let show = params.get("show").and_then(Value::as_bool).unwrap_or(!open);
    let cmd = if show { "plugin.panel.open" } else { "plugin.panel.close" };
    Some(app.run(cmd, json!({"id": plugin})))
}

/// Draws every open panel, and advances the plug-ins' clocks by one frame.
pub fn windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !photocraft_engine::plugin_panel_cmds::any_open() {
        return;
    }
    // Once a frame, for every panel at once: this fires the timers that came due and tells a panel the
    // document changed. A panel that fails here is stopped and keeps its reason, which the window
    // below shows beside a Restart button — so the result is deliberately *not* put in the status bar,
    // where a panel failing every frame would repeat itself sixty times a second. It also goes through
    // `run_background`: a frame-by-frame command must not clear what the user is being told.
    let _ = app.run_background("plugin.panel.tick", json!({}));
    // Timers are the one thing a panel asks for that happens without the user. The tick above fired
    // the ones already due, so this asks for a frame when the nearest remaining one is — and at most
    // ~60 frames a second, so a panel that re-arms a zero timer cannot spin the editor.
    if let Some(ms) = photocraft_engine::plugin_panel_cmds::next_timer_in() {
        ctx.request_repaint_after(std::time::Duration::from_millis(ms.clamp(16, 60_000)));
    }
    let t = Tokens::get(ctx);
    let canvas = app.last_canvas_rect;
    let panels = photocraft_engine::plugin_panel_cmds::draw_list();
    let mut closed: Vec<String> = Vec::new();
    let mut restarted: Vec<String> = Vec::new();
    let mut cancelled: Vec<String> = Vec::new();
    // Interactions are collected and sent after drawing: a click may redraw the whole panel, and the
    // panel's own lock cannot be taken while its snapshot is being drawn from.
    let mut pending = Pending::default();
    // Which panels were drawn last frame: a panel that was not (it was closed, or the app has just
    // started) is opening now, and opens at the top of its body. egui persists a scroll area's
    // offset by id, and the app persists egui's memory to `ui.ron`, so without this a panel scrolls
    // to where it was left — including a scroll left over from yesterday — the moment it appears.
    let seen_id = egui::Id::new("plugin-panel-seen");
    let mut seen: std::collections::HashSet<String> = ctx.data(|d| d.get_temp(seen_id).unwrap_or_default());
    for (i, panel) in panels.iter().enumerate() {
        if !panel.visible {
            continue;
        }
        let opening = seen.insert(panel.id.clone());
        let asked = window(ctx, &t, panel, i, canvas, &mut pending, opening);
        if asked.close {
            closed.push(panel.id.clone());
        }
        if asked.restart {
            restarted.push(panel.id.clone());
        }
        if asked.cancel {
            cancelled.push(panel.id.clone());
        }
    }
    for id in closed {
        let _ = app.run("plugin.panel.close", json!({"id": id}));
    }
    seen.retain(|id| panels.iter().any(|p| p.visible && id == &p.id));
    ctx.data_mut(|d| d.insert_temp(seen_id, seen));
    for id in restarted {
        let result = app.run("plugin.panel.restart", json!({"id": id})).map(|_| ());
        status(app, result);
    }
    for id in cancelled {
        send_event(app, &id, "cancel", None, Value::Null);
    }
    flush_typing(app, ctx, &pending);
    for (panel, widget, kind, value) in pending.events {
        send_event(app, &panel, kind, Some(&widget), value);
    }
}

/// What a panel's window asked the host to do. Collected while drawing, because a command cannot be
/// run from inside the drawing closure: it borrows the app, and a panel's own lock is held by the
/// snapshot being drawn from.
#[derive(Default)]
struct AskedFor {
    close: bool,
    restart: bool,
    cancel: bool,
}

/// Draws one panel's window, and returns what it asked the caller to run.
///
/// Split out of [`windows`] so a test can drive the window a plug-in's view is really drawn in,
/// rather than a copy of it: the width rule below is easy to break and hard to see (`window_widths`
/// in the tests).
///
/// `opening` says the panel was not drawn last frame: its body is then put back at the top (see the
/// comment in [`windows`]).
fn window(ctx: &egui::Context, t: &Tokens, panel: &PanelDraw, index: usize, canvas: egui::Rect, pending: &mut Pending, opening: bool) -> AskedFor {
    let mut asked = AskedFor::default();
    // Where the window starts and how tall it starts: as tall as the screen leaves room for, so a
    // panel with a page of styles in it shows them, rather than opening at egui's 420 px default
    // with the rest below the fold.
    let default_pos = pos2(canvas.left() + 40.0 + 22.0 * index as f32, canvas.top() + 60.0 + 22.0 * index as f32);
    let default_height = (canvas.bottom() - default_pos.y - 32.0).clamp(320.0, 720.0);
    egui::Window::new(&panel.title)
        // Versioned: the id is what eframe keeps a window's saved size under, in `ui.ron`. Bumping
        // it is how a panel that was saved at the old 420 px opens at the new default instead. It
        // also moves the body's scroll area to a fresh id, so an old saved offset cannot return.
        .id(egui::Id::new(("plugin-panel-v2", panel.id.as_str())))
        .title_bar(false)
        .resizable(true)
        .default_size(vec2(panel.min_width as f32, default_height))
        .min_width(panel.min_width as f32)
        .min_height(320.0)
        // Cascade, so two panels do not open one exactly on top of the other.
        .default_pos(default_pos)
        .frame(float_frame(t))
        .show(ctx, |ui| {
            ui.set_min_width(panel.min_width as f32);
            if header(ui, panel, t) {
                asked.close = true;
            }
            crate::widgets::hairline(ui);
            ui.add_space(4.0);
            match &panel.error {
                Some(error) => stopped(ui, t, error, &mut asked),
                None => {
                    // The plug-in's own footer (Settings/Help in the TyperTools page) is part of its
                    // view and scrolls with it. This one is the host's (progress and notices), and it
                    // stays put at the bottom of the window while the body above it takes the rest:
                    // a body that only took the height of its content left the window's lower half
                    // empty, with the styles cut off above it.
                    let has_footer = panel.progress.is_some() || !panel.notices.is_empty();
                    if has_footer {
                        egui::Panel::bottom(egui::Id::new(("plugin-panel-footer", panel.id.as_str())))
                            .frame(egui::Frame::NONE)
                            .show(ui, |ui| footer(ui, t, panel, &mut asked));
                    }
                    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                        // A view with `scroll` panes owns the panel's height: inside the body's own
                        // scroll area it would be handed an endless one, and the panes could never
                        // share it. So such a view is drawn straight into the panel, with the real
                        // height as its budget; everything else keeps the scrolling body it has
                        // always had.
                        let pane_mode = panel.view.as_ref().is_some_and(photocraft_plugins::view::has_scroll);
                        match &panel.view {
                            // Everything the plug-in sent is drawn inside the panel's own width (see
                            // `at_panel_width`): a window is sized from its content every frame, so a
                            // body that sticks out makes it wider, and wider again the next frame.
                            Some(view) if pane_mode => at_panel_width(ui, |ui| body(ui, t, panel, view, pending, Flow::Panes, None)),
                            Some(view) => {
                                if opening {
                                    // The same id `ScrollArea::id_salt` derives (an `IdSalt` is hashed once
                                    // more when it becomes an id), so this is the body's saved offset.
                                    let salt = egui::IdSalt::new(("plugin-panel-body", panel.id.as_str()));
                                    let body = ui.make_persistent_id(salt);
                                    egui::scroll_area::State::default().store(ui.ctx(), body);
                                }
                                egui::ScrollArea::vertical()
                                    .id_salt(("plugin-panel-body", panel.id.as_str()))
                                    .max_height(ui.available_height())
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| at_panel_width(ui, |ui| body(ui, t, panel, view, pending, Flow::Scrolling, None)));
                            }
                            // A panel that has not answered `init` yet, or answered with no view.
                            None => {
                                ui.label(RichText::new(tl!("This plug-in has not drawn anything yet.")).color(t.text_faint).size(11.5));
                            }
                        }
                    });
                }
            }
        });
    asked
}

/// Draws a panel's body at the width the panel has, whatever the plug-in's tree asks for.
///
/// The tree goes into a child of the panel's own width, and only that width — with the height the
/// body really used, which is what the scroll area needs — is reported back to the scroll area. So
/// nothing a plug-in sends can widen the window: a widget whose own size is bigger than the panel
/// (a 256-character label or button, a `choice` that sizes itself to its text, `progress`) is drawn
/// past the panel's edge and clipped there, which is a panel that looks wrong rather than a panel
/// that grows.
fn at_panel_width(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let rect = ui.available_rect_before_wrap();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    // Clip the tree to the panel's width, and only to it. Inside a scroll area `rect` is the
    // viewport translated up by the scroll offset, so intersecting the whole rect with the clip
    // (its vertical range included) emptied the clip as soon as the body was scrolled — every
    // widget was culled and the panel's body went blank below the first viewport height. The width
    // is what this pins down; the vertical clip must stay the scroll area's own.
    let clip = child.clip_rect();
    child.set_clip_rect(clip.intersect(egui::Rect::from_x_y_ranges(rect.x_range(), clip.y_range())));
    add(&mut child);
    let used = child.min_rect();
    ui.allocate_rect(egui::Rect::from_min_size(rect.min, vec2(rect.width(), used.height())), Sense::hover());
}

/// What one frame of drawing collected.
#[derive(Default)]
struct Pending {
    /// Interactions to send, in the order the widgets were drawn: `(panel, widget, event, value)`.
    ///
    /// The event is explicit rather than derived from the value: a button's click carries `null`,
    /// and a `list` row's click carries the row index — both are `ui.click`, while a field that
    /// committed a value is `ui.change`.
    events: Vec<(String, String, &'static str, Value)>,
    /// Text fields that were being typed into when the frame ended: `(panel, widget, buffer)`.
    ///
    /// egui takes the focus from a text field when the user clicks somewhere else, but a field only
    /// hears about it — and so only commits — on the *next* frame. Without this, a button whose
    /// action reads the field would act on the value from before the user's last edit: paste a
    /// chapter into a script box and press Create, and Create would letter the previous script.
    /// Collected here and sent before any click the same frame produced.
    typing: Vec<(String, String, Value)>,
}

/// Sends the edits that a click would otherwise overtake, so a button acts on what the user just
/// typed rather than on the value before it. Each flushed field's buffer is dropped, so the focus
/// loss is not reported a second time next frame: the plug-in answers the change by echoing the value
/// back (a new view or `ui.set`), which is what the ABI already asks of it.
fn flush_typing(app: &mut PhotocraftApp, ctx: &egui::Context, pending: &Pending) {
    let clicks: Vec<&str> = pending.events.iter().filter(|(_, _, kind, _)| *kind == "ui.click").map(|(panel, ..)| panel.as_str()).collect();
    if clicks.is_empty() {
        return;
    }
    for (panel, widget, typed) in &pending.typing {
        if !clicks.contains(&panel.as_str()) {
            continue;
        }
        // Already committed this frame (the field lost the focus before the button was drawn).
        if pending.events.iter().any(|(p, w, kind, _)| p == panel && w == widget && *kind == "ui.change") {
            continue;
        }
        send_event(app, panel, "ui.change", Some(widget), typed.clone());
        ctx.data_mut(|d| d.remove::<(String, String)>(egui::Id::new(("plugin-panel-text", panel.as_str(), widget.as_str()))));
    }
}

fn float_frame(t: &Tokens) -> egui::Frame {
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.card_border))
        .corner_radius(egui::CornerRadius::same(t.radius_lg as u8))
        .shadow(egui::Shadow { offset: [0, 10], blur: 30, spread: 0, color: t.shadow })
        .inner_margin(egui::Margin::same(8))
}

/// The panel's title row: its name, and a Close box. Returns whether Close was pressed — the caller
/// runs the command, since this is inside a drawing closure.
fn header(ui: &mut egui::Ui, panel: &PanelDraw, t: &Tokens) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(&panel.title).color(t.text).size(12.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::icons::button(ui, "x", 20.0, false, tl!("Close")).clicked() {
                close = true;
            }
        });
    });
    close
}

/// A stopped panel: why it stopped, and the one way out.
fn stopped(ui: &mut egui::Ui, t: &Tokens, error: &str, asked: &mut AskedFor) {
    ui.label(RichText::new(tl!("This plug-in stopped.")).color(t.danger).size(12.0).strong());
    ui.add_space(2.0);
    ui.label(RichText::new(error).color(t.text_dim).size(11.5));
    ui.add_space(6.0);
    if crate::widgets::primary_button(ui, tl!("Restart Plug-in"), 130.0).clicked() {
        asked.restart = true;
    }
}

/// The panel's progress bar and the notices it has raised, below whatever it drew.
fn footer(ui: &mut egui::Ui, t: &Tokens, panel: &PanelDraw, asked: &mut AskedFor) {
    if let Some(progress) = &panel.progress {
        ui.add_space(6.0);
        progress_row(ui, t, progress, asked);
    }
    if panel.notices.is_empty() {
        return;
    }
    ui.add_space(4.0);
    crate::widgets::hairline(ui);
    // In the order the panel raised them; the host keeps the last few, not every one ever raised.
    for notice in &panel.notices {
        let colour = match notice.level {
            NoticeLevel::Info => t.text_dim,
            NoticeLevel::Warning => t.warning,
            NoticeLevel::Error => t.danger,
        };
        ui.label(RichText::new(&notice.text).color(colour).size(11.5));
    }
}

fn progress_row(ui: &mut egui::Ui, t: &Tokens, progress: &Progress, asked: &mut AskedFor) {
    ui.horizontal(|ui| {
        let bar = ui.add(egui::ProgressBar::new(progress.fraction.unwrap_or(0.0) as f32).desired_width(ui.available_width() - 66.0).fill(t.accent));
        if let Some(text) = &progress.text {
            bar.on_hover_text(text);
        }
        if progress.cancellable && crate::widgets::compact_button(ui, tl!("Cancel"), 18.0).clicked() {
            asked.cancel = true;
        }
    });
    if let Some(text) = &progress.text {
        ui.label(RichText::new(text).color(t.text_dim).size(11.0));
    }
}

/// A plug-in's button, drawn in the application's own style.
///
/// It is a `Widget` rather than a free function so that [`Ui::add_enabled`] can draw it disabled
/// without opening a child `Ui` — inside a `row` that would put the button at the cursor without the
/// wrapping layout getting a say in whether it fits (see the `Button` arm of [`body`]).
struct PanelButton<'a> {
    label: &'a str,
    primary: bool,
    /// A glyph drawn instead of the label, as a compact square button. The label is then the
    /// tooltip and the accessible name.
    icon: Option<&'a str>,
}

impl egui::Widget for PanelButton<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let Some(icon) = self.icon.filter(|i| !i.is_empty()) else {
            return if self.primary {
                crate::widgets::primary_button(ui, self.label, 0.0)
            } else {
                crate::widgets::secondary_button(ui, self.label, 0.0)
            };
        };
        // A compact square with the glyph centred: the icon buttons of a lettering panel's rows
        // (previous/next line, delete, a folder) sit beside a number and must not cost a word's
        // width. Drawn through `allocate_exact_size`, so a `row` decides whether it fits.
        let t = Tokens::get(ui.ctx());
        let size = 22.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
        let fill = if self.primary { t.accent_soft } else { Color32::TRANSPARENT };
        if fill != Color32::TRANSPARENT || resp.hovered() {
            ui.painter().rect_filled(rect, t.radius_sm, if resp.hovered() { t.hover } else { fill });
        }
        let tint = if resp.hovered() { t.text } else if self.primary { t.accent_text } else { t.icon };
        let galley = ui.painter().layout_no_wrap(icon.to_string(), FontId::proportional(13.0), tint);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, tint);
        let name = if self.label.is_empty() { icon } else { self.label };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), name));
        if self.label.is_empty() { resp } else { resp.on_hover_text(self.label) }
    }
}

/// How a view is laid out in the panel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// The ordinary body: a column of widgets inside the panel's own scrolling body.
    Scrolling,
    /// A view with `scroll` panes: the panel's free height is the budget, and a `column` shares it
    /// among its height-less panes by weight (see [`Widget::Scroll`]).
    Panes,
}

/// The least height a `scroll` pane is given, so a crowded panel still shows a row in each pane.
const MIN_PANE: f32 = 48.0;
/// The height a `scroll` pane gets when nothing gives it one (a pane in a `row`, say).
const DEFAULT_PANE: f32 = 240.0;
/// Most font families one session registers for panel labels.
///
/// egui cannot unregister a font, so a style library with a font per style would otherwise keep
/// every one of those files in the process for its lifetime. Past the cap the panel font is used:
/// the panel still draws, just not in each style's own face.
const MAX_PLUGIN_FONTS: usize = 24;
/// Most face data (font files) one session registers for panel labels.
const MAX_PLUGIN_FONT_BYTES: usize = 48 * 1024 * 1024;
/// The size a panel label, and a style name in its own font, is drawn at.
const PANEL_TEXT: f32 = 12.0;

/// Draws one widget tree, collecting every interaction into `pending`.
///
/// `pane_height` is only consulted by a `scroll`: when a `column` is sharing the panel's free
/// height (`Flow::Panes`) it hands each of its height-less panes its share through here.
fn body(ui: &mut egui::Ui, t: &Tokens, panel: &PanelDraw, widget: &Widget, pending: &mut Pending, flow: Flow, pane_height: Option<f32>) {
    // The room left on this line, not `ui.available_width()`: inside a `row` — a *wrapping* layout,
    // where a widget is allowed to take a whole line of its own — egui answers that with the whole
    // row's width. A dropdown or a field drawn after a label was therefore handed the label's width
    // again and stuck out of the panel by exactly that much, and a window is sized from its content
    // every frame: the lettering panel's first row (a label, then its page `choice`) widened the
    // panel by 39 px per frame, without end.
    let width = ui.available_rect_before_wrap().width();
    match widget {
        Widget::Column { children } => {
            ui.vertical(|ui| {
                // In a pane view the column shares the panel's free height among its height-less
                // panes. Each share comes from what is left at that pane, so the widgets above it
                // (a page switcher, a preview) are paid for first, and is proportional to the
                // weights left, so two panes keep their ratio. Anything that must stay visible
                // under a pane belongs in that pane's `footer`.
                //
                // Whatever follows the *last* pane is reserved before the panes are given anything.
                // A pane took the whole height that was left, so widgets under it had nowhere to go:
                // the tree came out taller than the panel it was drawn in, and the window — sized
                // from its content every frame — grew by exactly that much, every frame, without
                // end. The TypeR port ends its page with two status lines under its panes, which
                // grew its panel by 64 px a frame; a slower frame rate is a slower creep, not a
                // smaller one. Reserved from the bottom first, the panes share what is left and the
                // tree still fits.
                let panes = flow == Flow::Panes;
                let last_pane = if panes { children.iter().rposition(|child| pane_weight(child).is_some()) } else { None };
                let (taken, tail): (&[Widget], &[Widget]) = match last_pane {
                    Some(last) if last + 1 < children.len() => children.split_at(last + 1),
                    _ => (children.as_slice(), &[]),
                };
                if !tail.is_empty() {
                    // The panel's own id and this column's, so two columns in one view get a panel
                    // each. Drawn before the panes, because a `Panel` reserves its strip and leaves
                    // the rest to what is drawn after it.
                    let id = egui::Id::new(("plugin-panel-tail", panel.id.as_str(), ui.id()));
                    egui::Panel::bottom(id).frame(egui::Frame::NONE).show(ui, |ui| {
                        at_panel_width(ui, |ui| {
                            for child in tail {
                                body(ui, t, panel, child, pending, flow, None);
                            }
                        });
                    });
                }
                let mut weights_left: u32 = if panes { taken.iter().filter_map(pane_weight).map(u32::from).sum() } else { 0 };
                for child in taken {
                    match panes.then(|| pane_weight(child)).flatten() {
                        Some(weight) => {
                            let share = ui.available_height().max(MIN_PANE) * f32::from(weight) / weights_left.max(1) as f32;
                            weights_left = weights_left.saturating_sub(u32::from(weight));
                            body(ui, t, panel, child, pending, flow, Some(share));
                        }
                        None => body(ui, t, panel, child, pending, flow, None),
                    }
                }
            });
        }
        Widget::Row { children } => {
            ui.horizontal_wrapped(|ui| {
                for child in children {
                    body(ui, t, panel, child, pending, flow, None);
                }
            });
        }
        Widget::Section { title, collapsed, children } => {
            // Open/closed is egui's, remembered by id, so it survives redraws; `collapsed` is only
            // what the plug-in asks for before anyone has touched it.
            egui::CollapsingHeader::new(RichText::new(title).color(t.text_dim).size(11.5))
                .id_salt(("plugin-panel-section", panel.id.as_str(), title.as_str()))
                .default_open(!collapsed)
                .show(ui, |ui| {
                    for child in children {
                        // Scrolling, not the caller's flow: a pane inside a section is sized on its
                        // own (or gets `DEFAULT_PANE`), not out of the column's budget.
                        body(ui, t, panel, child, pending, Flow::Scrolling, None);
                    }
                });
        }
        Widget::Label { text, level, color, font } => {
            // A named colour wins over the level's theme colour; a family is resolved by the host,
            // and an unknown one is the panel font, not an error.
            let colour = color.as_deref().and_then(plugin_color).unwrap_or(match level {
                Some(Level::Muted) => t.text_faint,
                Some(Level::Warning) => t.warning,
                Some(Level::Error) => t.danger,
                Some(Level::Info) | None => t.text,
            });
            let rich = match font.as_deref().and_then(|family| plugin_font(ui.ctx(), family)) {
                Some(family) => RichText::new(text).color(colour).font(FontId::new(PANEL_TEXT, family)),
                None => RichText::new(text).color(colour).size(PANEL_TEXT),
            };
            // Labels wrap: a script line or a refusal can be long, and a panel is narrow.
            ui.add(egui::Label::new(rich).wrap());
        }
        Widget::Separator => crate::widgets::hairline(ui),
        Widget::Text { id, value, placeholder, multiline, max_length } => {
            let shown = panel.values.get(id).and_then(Value::as_str).unwrap_or(value);
            let key = egui::Id::new(("plugin-panel-text", panel.id.as_str(), id.as_str()));
            // The buffer is what is being typed; it is dropped the moment the panel's own value stops
            // matching the one the buffer was seeded from, so `ui.set` from the plug-in wins.
            let mut buf: (String, String) = ui.data(|d| d.get_temp(key)).unwrap_or_else(|| (shown.to_string(), shown.to_string()));
            if buf.0 != shown {
                buf = (shown.to_string(), shown.to_string());
            }
            let hint = placeholder.clone().unwrap_or_default();
            let resp = if *multiline {
                multiline_field(ui, key, &mut buf.1, &hint, 150.0, *max_length)
            } else {
                crate::widgets::text_field_ex(ui, key, &mut buf.1, &hint, width, false, *max_length)
            };
            if resp.lost_focus() && buf.1 != shown {
                pending.events.push((panel.id.clone(), id.clone(), "ui.change", Value::String(buf.1.clone())));
            }
            // Still in the field with an edit the plug-in has not heard about: a click elsewhere in
            // the panel ends the edit, so it has to be delivered before that click (see `Pending`).
            if resp.has_focus() && buf.1 != shown {
                pending.typing.push((panel.id.clone(), id.clone(), Value::String(buf.1.clone())));
            }
            if buf.1 == shown {
                ui.data_mut(|d| d.remove::<(String, String)>(key));
            } else {
                ui.data_mut(|d| d.insert_temp(key, buf));
            }
        }
        Widget::Number { id, value, min, max, step, slider } => {
            let shown = panel.values.get(id).and_then(Value::as_f64).unwrap_or(*value);
            let mut v = shown as f32;
            let range = (*min as f32)..=(*max as f32);
            let resp = if *slider { crate::widgets::slider(ui, &mut v, range, None) } else { crate::widgets::value_field(ui, &mut v, range, "", 64.0) };
            if resp.changed() && v.is_finite() {
                // `step` is honoured by snapping to its grid: a plug-in asking for whole numbers must
                // not be handed 2.9999998 because a value field was dragged.
                if let Some(step) = *step {
                    v = (*min as f32 + ((v - *min as f32) / step as f32).round() * step as f32).clamp(*min as f32, *max as f32);
                }
                let v = f64::from(v);
                if v != shown {
                    pending.events.push((panel.id.clone(), id.clone(), "ui.change", json!(v)));
                }
            }
        }
        Widget::Toggle { id, value, label } => {
            let shown = panel.values.get(id).and_then(Value::as_bool).unwrap_or(*value);
            let mut on = shown;
            if crate::widgets::toggle(ui, &mut on, label).changed() && on != shown {
                pending.events.push((panel.id.clone(), id.clone(), "ui.change", json!(on)));
            }
        }
        Widget::Choice { id, value, options } => {
            let shown = panel.values.get(id).and_then(Value::as_str).unwrap_or(value).to_string();
            let mut current = shown.clone();
            // A plug-in's labels are its own strings, not the application's: they are not translated.
            let list: Vec<(String, &str)> = options.iter().map(|o| (o.value.clone(), o.label.as_str())).collect();
            let salt = format!("plugin-panel-choice-{}-{id}", panel.id);
            if crate::widgets::dropdown_literal(ui, &salt, &mut current, &list, width) && current != shown {
                pending.events.push((panel.id.clone(), id.clone(), "ui.change", Value::String(current)));
            }
        }
        Widget::Button { id, label, primary, enabled, icon } => {
            let on = enabled.unwrap_or(true);
            // A widget rather than `add_enabled_ui`: that builds a child `Ui`, and a child is placed
            // at the cursor and then advanced past by `advance_cursor_after_rect` without the
            // wrapping layout ever being asked whether the button fits. A `row` of buttons (a style
            // row's size step and its four arrows on a 250 px panel) therefore never wrapped, and
            // every button after the first stuck out of the window — and, because the row's rect
            // then grew, widened the column's `max_rect`, so the labels below it stopped wrapping
            // too. `Ui::add_enabled` disables the same `Ui` and draws the widget through the row's
            // own allocation, so a disabled button wraps like an enabled one.
            let resp = ui.add_enabled(on, PanelButton { label, primary: *primary, icon: icon.as_deref() });
            if resp.clicked() {
                pending.events.push((panel.id.clone(), id.clone(), "ui.click", Value::Null));
            }
            if !on {
                resp.on_hover_text(tl!("The plug-in has disabled this."));
            }
        }
        Widget::List { id, selected, numbered, rows } => {
            list(ui, &Paint { t, panel }, id, *selected, *numbered, rows, pending);
        }
        Widget::Scroll { id, height, children, footer, .. } => {
            let height = height.map(|h| h as f32).or(pane_height).unwrap_or(DEFAULT_PANE);
            pane(ui, &Paint { t, panel }, id, height, children, footer, pending);
        }
        Widget::Progress { fraction } => {
            // `null` is an indeterminate bar: a plug-in that does not know how far along it is.
            ui.add(egui::ProgressBar::new(fraction.unwrap_or(0.0) as f32).desired_width(width).fill(t.accent));
        }
        Widget::Image { blob, .. } => {
            // An image is a host-owned blob, and this host has no `doc.capture` yet, so no blob has
            // ever been handed out. Say so rather than draw an empty box.
            ui.label(RichText::new(format!("[{blob}] {}", tl!("Image"))).color(t.text_faint).size(11.5))
                .on_hover_text(tl!("This host does not capture documents for plug-ins yet, so a panel has no images."));
        }
    }
}

/// A pane's weight, if this widget is a height-less `scroll`: the weight a `column` in pane mode
/// shares the panel's free height by.
fn pane_weight(widget: &Widget) -> Option<u8> {
    match widget {
        Widget::Scroll { height: None, weight, .. } => Some(*weight),
        _ => None,
    }
}

/// `#rrggbb` from a plug-in, as an egui colour.
///
/// Validation has already refused anything else, so this is only the conversion; an impossible
/// string is `None` (the theme colour is used) rather than a panic.
fn plugin_color(hex: &str) -> Option<Color32> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some(Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8))
}

/// What this session has already asked egui for, so a name is resolved once and a missing family
/// is not looked up every frame.
#[derive(Clone, Default)]
struct PluginFonts {
    /// Families already resolved (registered, or found not to be installed).
    tried: std::collections::HashSet<String>,
    /// Families actually registered.
    registered: usize,
    /// Face bytes actually handed to egui.
    bytes: usize,
}

fn fonts_id() -> egui::Id {
    egui::Id::new("plugin-panel-fonts")
}

/// The host's own family for a family a plug-in named, registered on first use.
///
/// A plug-in never sees a font file: it names a family, and the host resolves it in its own font
/// database and registers it under a namespaced family (`plugin:<family>`, so a plug-in can never
/// shadow the application's own `medium`/`semibold`). egui takes a new font from the *next* pass,
/// and asking for a family that is not bound panics, so the frame a name is first seen draws in
/// the panel font and asks for a repaint; from then on the named family is used.
///
/// Everything here is bounded and falls back silently: an unknown family, a face too large to keep
/// in the process, more families than [`MAX_PLUGIN_FONTS`] — the panel still draws its labels, just
/// in the panel font. A style library must never be able to stop a panel drawing.
fn plugin_font(ctx: &egui::Context, family: &str) -> Option<FontFamily> {
    let key: Arc<str> = format!("plugin:{family}").into();
    let font = FontFamily::Name(key.clone());
    // Bound already: egui has applied the registration.
    if ctx.fonts(|f| f.families().contains(&font)) {
        return Some(font);
    }
    let mut registry: PluginFonts = ctx.data(|d| d.get_temp(fonts_id())).unwrap_or_default();
    if registry.tried.contains(family) {
        return None;
    }
    if registry.registered >= MAX_PLUGIN_FONTS || registry.bytes >= MAX_PLUGIN_FONT_BYTES {
        // Out of budget: remember nothing, so a family that shows up later is not looked up again
        // and again, but also do not record it as tried — the next session (after a restart) may
        // have room.
        registry.tried.insert(family.to_string());
        ctx.data_mut(|d| d.insert_temp(fonts_id(), registry));
        return None;
    }
    let Some(engine) = photocraft_text::try_shared() else {
        // The system font scan is still running: the answer is not known yet, so ask again shortly.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        return None;
    };
    let face = engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner).fonts.ui_face(family);
    let Some((bytes, index)) = face else {
        registry.tried.insert(family.to_string());
        ctx.data_mut(|d| d.insert_temp(fonts_id(), registry));
        return None;
    };
    let size = bytes.len();
    if registry.bytes.saturating_add(size) > MAX_PLUGIN_FONT_BYTES {
        registry.tried.insert(family.to_string());
        ctx.data_mut(|d| d.insert_temp(fonts_id(), registry));
        return None;
    }
    registry.tried.insert(family.to_string());
    registry.registered += 1;
    registry.bytes = registry.bytes.saturating_add(size);
    ctx.data_mut(|d| d.insert_temp(fonts_id(), registry));
    let mut data = FontData::from_owned(bytes);
    data.index = index;
    crate::theme::size_ui_font(ctx, &key, &mut data);
    ctx.add_font(egui::epaint::text::FontInsert {
        name: key.to_string(),
        data,
        families: vec![egui::epaint::text::InsertFontFamily { family: font, priority: egui::epaint::text::FontPriority::Highest }],
    });
    ctx.request_repaint(); // the family is bound from the next pass
    None
}

/// What a widget needs that does not change down the tree.
struct Paint<'a> {
    t: &'a Tokens,
    panel: &'a PanelDraw,
}

/// A `list`: one selectable row per line, with an optional line-number gutter.
///
/// A click sends `ui.click` with the row's index, so a plug-in learns which line was picked
/// without matching text. Rows are a single line each, elided with `…` at the pane's edge: a
/// wrapped row would make the list's height depend on how a style name happens to break.
fn list(ui: &mut egui::Ui, paint: &Paint<'_>, id: &str, selected: Option<usize>, numbered: bool, rows: &[ListRow], pending: &mut Pending) {
    let t = paint.t;
    let width = ui.available_rect_before_wrap().width().max(16.0);
    const ROW_HEIGHT: f32 = 18.0;
    let digits = rows.len().max(1).to_string().len();
    let gutter = if numbered { digits as f32 * 7.0 + 8.0 } else { 0.0 };
    for (index, row) in rows.iter().enumerate() {
        let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::click());
        let is_selected = selected == Some(index);
        let painter = ui.painter();
        if is_selected {
            painter.rect_filled(rect, t.radius_sm, t.accent_soft);
            // A 2 px bar on the current row: the highlight has to read at a glance in both themes.
            painter.rect_filled(egui::Rect::from_min_size(rect.min, vec2(2.0, rect.height())), t.radius_sm, t.accent);
        } else if resp.hovered() {
            painter.rect_filled(rect, t.radius_sm, t.hover);
        }
        if numbered {
            let number = format!("{:>width$}", index + 1, width = digits);
            painter.text(pos2(rect.left() + gutter - 6.0, rect.center().y), Align2::RIGHT_CENTER, number, FontId::proportional(10.5), t.text_faint);
        }
        let font = row.font.as_deref().and_then(|family| plugin_font(ui.ctx(), family));
        let font_id = font.map_or_else(|| FontId::proportional(PANEL_TEXT), |family| FontId::new(PANEL_TEXT, family));
        let colour = row.color.as_deref().and_then(plugin_color).unwrap_or(if is_selected { t.accent_text } else { t.text });
        let room = (rect.right() - 6.0 - (rect.left() + 6.0 + gutter)).max(8.0);
        let mut job = egui::text::LayoutJob::single_section(row.text.clone(), egui::TextFormat { font_id, color: colour, ..Default::default() });
        job.wrap.max_width = room;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
        let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
        let at = pos2(rect.left() + 6.0 + gutter, rect.center().y - galley.size().y / 2.0);
        painter.galley(at, galley, colour);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), is_selected, &row.text));
        if resp.clicked() {
            pending.events.push((paint.panel.id.clone(), id.to_string(), "ui.click", json!(index)));
        }
    }
}

/// One `scroll` pane: its content scrolls on its own, its `footer` is pinned under it, and neither
/// is allowed past the panel's width.
fn pane(ui: &mut egui::Ui, paint: &Paint<'_>, id: &str, height: f32, children: &[Widget], footer: &[Widget], pending: &mut Pending) {
    let (t, panel) = (paint.t, paint.panel);
    let available = ui.available_rect_before_wrap();
    let width = available.width().max(16.0);
    // Never taller than the room there is (or than the ABI allows): a pane that asked for more
    // would push its own footer off the window.
    let height = height.clamp(MIN_PANE, photocraft_plugins::view::MAX_SCROLL_HEIGHT as f32).min(ui.available_height().max(MIN_PANE));
    let rect = egui::Rect::from_min_size(available.min, vec2(width, height));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    // The pane is clipped to its own width and height, like the body is to the panel's width.
    let clip = child.clip_rect();
    child.set_clip_rect(clip.intersect(egui::Rect::from_min_max(rect.min, rect.max)));
    if !footer.is_empty() {
        egui::Panel::bottom(egui::Id::new(("plugin-panel-pane-footer", panel.id.as_str(), id)))
            .frame(egui::Frame::NONE)
            .show(&mut child, |ui| {
                at_panel_width(ui, |ui| {
                    for widget in footer {
                        body(ui, t, panel, widget, pending, Flow::Scrolling, None);
                    }
                });
            });
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(&mut child, |ui| {
        egui::ScrollArea::vertical()
            .id_salt(("plugin-panel-scroll", panel.id.as_str(), id))
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .show(ui, |ui| {
                at_panel_width(ui, |ui| {
                    for widget in children {
                        body(ui, t, panel, widget, pending, Flow::Scrolling, None);
                    }
                });
            });
    });
    ui.allocate_rect(rect, Sense::hover());
}

/// The script box: [`crate::widgets::text_field`]'s box, taller and top-aligned, with its own
/// scrolling — a lettering panel is mostly made of these.
fn multiline_field(ui: &mut egui::Ui, id: egui::Id, text: &mut String, hint: &str, height: f32, char_limit: Option<usize>) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    // The room left on this line, like [`body`]'s: a script box inside a `row` would otherwise be as
    // wide as the whole row.
    let width = ui.available_rect_before_wrap().width().max(16.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height.max(48.0)), Sense::hover());
    crate::widgets::surface(ui, rect, t.field, false);
    if !t.bevel {
        ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    }
    let inner = rect.shrink2(vec2(6.0, 2.0));
    let mut edit = egui::TextEdit::multiline(text)
        .id(id)
        .hint_text(hint)
        .frame(egui::Frame::NONE)
        .font(FontId::proportional(12.5))
        .desired_width(inner.width())
        .min_size(inner.size());
    if let Some(limit) = char_limit {
        edit = edit.char_limit(limit);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    child.style_mut().visuals.selection.bg_fill = t.field_selection();
    // The box is a fixed height and the script scrolls inside it. A `TextEdit` is as tall as its
    // text, so left alone a chapter-length script was painted past the box and through the widgets
    // below it; the scroll area clips it to the box and gives it a scrollbar.
    egui::ScrollArea::vertical().id_salt((id, "scroll")).max_height(inner.height()).auto_shrink([false, false]).show(&mut child, |ui| ui.add(edit)).inner
}

/// Sends one event to a panel, reporting a refusal in the status bar: the panel stopped, or the event
/// was not one the ABI has.
fn send_event(app: &mut PhotocraftApp, panel: &str, kind: &str, widget: Option<&str>, value: Value) {
    let mut event = Map::new();
    event.insert("type".into(), json!(kind));
    if let Some(widget) = widget {
        event.insert("id".into(), json!(widget));
    }
    // A button's click has no value; a list row's click carries its index, and a committed field
    // carries what it committed. The event type says which widget it was.
    if !value.is_null() {
        event.insert("value".into(), value);
    }
    let sent = app.run("plugin.panel.event", json!({"id": panel, "event": Value::Object(event)})).map(|_| ());
    status(app, sent);
}

fn status(app: &mut PhotocraftApp, result: Result<(), String>) {
    if let Err(error) = result {
        app.ui.status = error;
        app.ui.status_error = true;
    }
}

#[cfg(test)]
#[path = "plugin_panel_ui_tests.rs"]
mod tests;
