//! Window › Plug-ins: the menu row an installed panel gets, the floating window it opens into, and
//! the events its widgets send back.
//!
//! The panel under test is the real artifact from `examples/plugins/panel-rs` — the one a person
//! installs — not a fixture written to match this code.
//!
//! Two things about writing this test are worth knowing. The installed plug-ins *and* the open panels
//! are process-wide, so every other test in this binary drives this panel too: each of their frames
//! ticks it, and their sessions announce their own documents to it. Assertions are therefore about
//! what the panel does in answer to *this* test's events, never about text another test's document
//! could have moved. And the panel is stopped and removed at the end, so nothing is left ticking
//! behind the other 2,900 tests.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::*;

/// The example panel, as its `build.sh` leaves it (see `examples/plugins/panel-rs`).
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../plugins/tests/fixtures/panel.wasm");
const PANEL: &str = "org.photocraft.example.panel";
const ITEM: &str = "plugin.panel.item.org.photocraft.example.panel";
/// The note the test types into the panel's field.
const NOTE: &str = "chapter 1";
/// More of it, typed and then left by *clicking* a button rather than pressing Enter.
const TAIL: &str = " continued";

/// The whole app, with the example panel installed and a 40 × 28 document open. One test drives all
/// of it: the registry and the open panels are process-wide, so two of these running at once would be
/// testing each other.
fn harness() -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1100.0, 760.0)).with_max_steps(96).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
    });
    h.state_mut().run("plugin.install", json!({"path": EXAMPLE})).unwrap();
    h.state_mut().run("file.new", json!({"width": 40, "height": 28})).unwrap();
    h.run_steps(3);
    h
}

/// What the engine holds for the panel, including the view it last sent.
fn inspected(h: &mut Harness<'static, PhotocraftApp>) -> Value {
    h.state_mut().run("plugin.panel.inspect", json!({"id": PANEL})).expect("the panel is open")
}

/// One widget of the panel's view, as the plug-in sent it: `column` children, in order.
fn widget(h: &mut Harness<'static, PhotocraftApp>, index: usize) -> Value {
    inspected(h)["view"]["children"][index].clone()
}

/// The text of one of the panel's labels.
fn text(h: &mut Harness<'static, PhotocraftApp>, index: usize) -> String {
    widget(h, index)["text"].as_str().unwrap_or_default().to_string()
}

/// Is anything drawn with this label?
fn shown(h: &Harness<'_, PhotocraftApp>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

/// Whether the panel's row carries its checkmark — what the user sees in the menu, rather than the
/// state behind it.
fn row_checked(h: &Harness<'_, PhotocraftApp>) -> bool {
    crate::menus::menu_items(h.state()).into_iter().find(|i| i.id == ITEM).expect("the panel's row").checked == Some(true)
}

/// Focuses the panel's note field, reporting whether there was one to focus.
///
/// A panel's field has no accessible name of its own — the ABI gives a widget a placeholder, not a
/// label — so it cannot be found the way the buttons below are. It is found by role instead, in the
/// window the panel drew: the first (and here only) panel is placed at the canvas' top-left plus its
/// cascade offset, which is where the UI put it too.
fn focus_note_field(h: &mut Harness<'static, PhotocraftApp>) -> bool {
    let window = egui::Rect::from_min_size(h.state().last_canvas_rect.left_top() + vec2(40.0, 60.0), vec2(320.0, 240.0));
    h.query_all_by_role(egui::accesskit::Role::TextInput).find(|n| window.contains(n.rect().center())).map(|n| n.click()).is_some()
}

#[test]
fn a_panel_gets_a_window_row_that_opens_into_the_view_it_sent() {
    let mut h = harness();
    // A row under Window › Plug-ins, named by the title the module declares — not by its package
    // name, and not in Filter › Plug-ins, where filters are run on a layer.
    let items = crate::menus::menu_items(h.state());
    let row = items.iter().find(|i| i.id == ITEM).expect("the panel is listed in the menus");
    assert_eq!(row.label, "Document Info");
    assert_eq!(row.path, ["Window", "Plug-ins"]);
    assert!(row.enabled, "opening a panel needs no document and no selection");
    assert!(
        !items.iter().any(|i| i.id.contains(PANEL) && i.path == ["Filter", "Plug-ins"]),
        "a panel is not a filter, so it gets no row where filters are run"
    );
    // The path is the untranslated English menu path, like every other item's, so the row survives a
    // translated UI (#2540) — while the label stays the plug-in's own string, which is not ours to
    // translate.
    let ja = crate::i18n::Lang::from_code("ja").expect("registered language");
    assert_ne!(crate::i18n::tr(ja, "Window"), "Window", "the language must really translate the path");
    let lang = crate::i18n::language_scope(ja);
    let items = crate::menus::menu_items(h.state());
    let row = items.iter().find(|i| i.id == ITEM).expect("still listed in Japanese");
    assert_eq!(row.path, ["Window", "Plug-ins"]);
    assert_eq!(row.label, "Document Info");
    drop(lang);

    // Picking the row opens it: the plug-in's view is drawn, in the app's own theme.
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, ITEM, json!({})).unwrap();
    h.run_steps(3);
    assert!(row_checked(&h), "the row is checked while the panel is open");
    assert!(shown(&h, "Document Info"), "the window is titled with the plug-in's own title");
    assert_eq!(widget(&mut h, 1)["value"], "", "the field the plug-in sent, empty");
    assert_eq!(widget(&mut h, 2)["children"][0]["id"], "read", "and the buttons it sent");
    assert_eq!(text(&mut h, 4), "Reads 0, ticks 0", "nothing has happened yet");
    assert!(focus_note_field(&mut h), "its field is drawn");
    h.run_steps(1);

    // A press reaches the plug-in, which asks the host about the document and answers with a new view
    // — all inside the round trip, before the next frame is drawn.
    h.get_by_label("Read Document").click();
    h.run_steps(3);
    assert_eq!(text(&mut h, 4), "Reads 1, ticks 0", "the click reached the plug-in");
    // What the host answered is drawn in the panel's first label, but *which* document is in it is not
    // this test's to pin down: the panels are process-wide, so every other test's session announces its
    // own document while this one runs, and the last frame to arrive wins. The exact text —
    // `Document 40 x 28` — is asserted in the engine test, where one session owns the panel. What can
    // be read here is that the read came back answered rather than refused.
    let status = text(&mut h, 0);
    assert!(!status.contains("would not read"), "the host answered the read: {status}");

    // A panel can act, not only look: New Type Layer asks the host to run `type.create` (`doc.command`)
    // and the host answers with the command's own result. What is asserted here is the *document* — the
    // layer the host made — and not the panel's status line, which another test's document can move.
    h.get_by_label("New Type Layer").click();
    h.run_steps(3);
    let doc = h.state_mut().run("document.inspect", json!({})).unwrap();
    let layers = doc["layers"].as_array().cloned().unwrap_or_default();
    assert_eq!(layers.len(), 2, "the panel's command added a layer: {doc}");
    assert_eq!(layers[0]["kind"], "Type");
    assert_eq!(layers[0]["name"], "Balloon");
    let status = text(&mut h, 0);
    assert!(!status.contains("would not add"), "the host ran the command: {status}");

    // Watch Document arms a timer; its label changes because the plug-in said so, not because the host
    // did.
    h.get_by_label("Watch Document").click();
    h.run_steps(2);
    assert_eq!(inspected(&mut h)["timers"], 1, "the button armed a timer");
    assert!(shown(&h, "Stop Watching"), "and the plug-in redrew it");
    h.get_by_label("Stop Watching").click();
    h.run_steps(2);
    assert_eq!(inspected(&mut h)["timers"], 0, "the same button clears the timer it armed");

    // Typing stays on this side of the sandbox until the field is committed: Enter is the commit, and
    // only then does the plug-in hear about it — and only then does it store the note. (The buttons
    // above took the focus, so the field is clicked again first.)
    assert!(focus_note_field(&mut h));
    h.run_steps(1);
    h.event(egui::Event::Text(NOTE.into()));
    h.run_steps(1);
    assert_eq!(inspected(&mut h)["storage"].get("note"), None, "keystrokes do not cross the sandbox");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(inspected(&mut h)["storage"]["note"], NOTE, "the committed edit was stored");
    assert_eq!(widget(&mut h, 1)["value"], NOTE, "and the plug-in sent it back in the field");
    // The panel says so in its status line as well ("Note saved to plug-in storage"), but that line
    // belongs to the document and any other test's session can rewrite it between two frames, so the
    // acknowledgement is left to the engine test: the note above is what proves the commit happened.

    // Clicking elsewhere in the panel ends the edit too, and that click must not lose it: a letterer
    // pastes a chapter into a script box and presses a button, and what they typed has to reach the
    // plug-in. What the panel then *says* is not asserted: its status line follows the document, and
    // another test's session announces its own (or none) to this panel between frames — with three
    // thousand tests in one binary, that line is not this test's to read. That a click sees the edit
    // it overtook is asserted below, directly on `flush_typing`, where no other test can interfere.
    assert!(focus_note_field(&mut h));
    h.run_steps(1);
    h.event(egui::Event::Text(TAIL.into()));
    h.run_steps(1);
    h.get_by_label("Read Document").click();
    h.run_steps(3);
    let stored = inspected(&mut h)["storage"]["note"].as_str().unwrap_or_default().to_string();
    assert!(stored.ends_with(TAIL), "the click did not lose the edit: {stored:?}");
    let typed = widget(&mut h, 1)["value"].as_str().unwrap_or_default().to_string();
    assert!(typed.ends_with(TAIL), "and the field still holds what was typed: {typed:?}");

    // Picking the row again closes the window; the same row is the way back.
    crate::menus::invoke(h.state_mut(), &ctx, ITEM, json!({})).unwrap();
    h.run_steps(2);
    assert!(!row_checked(&h));
    assert!(!shown(&h, "Read Document"), "the window went with it");
    crate::menus::invoke(h.state_mut(), &ctx, ITEM, json!({"show": true})).unwrap();
    h.run_steps(2);
    assert!(row_checked(&h), "`show` opens it whether or not it was open");
    // What a field holds is its value, not a label, so it is read from the view rather than found
    // drawn. Hiding a window keeps the panel's instance, so the note is still there.
    assert_eq!(widget(&mut h, 1)["value"].as_str(), Some(typed.as_str()), "and it opens as it was left");

    // Restarting is allowed while it runs (it is what a stopped panel needs, and a panel that is
    // running is simply started over). It starts a *new* instance, so this is where plug-in storage
    // proves itself: the note comes back with `init`.
    crate::menus::invoke(h.state_mut(), &ctx, ITEM, json!({"restart": true})).unwrap();
    h.run_steps(2);
    assert_eq!(inspected(&mut h)["state"], "running");
    assert_eq!(text(&mut h, 4), "Reads 0, ticks 0", "a restarted panel starts at `init` again");
    assert_eq!(widget(&mut h, 1)["value"].as_str(), Some(typed.as_str()), "with what the last instance stored");

    // The hazard the flush exists for cannot be reached through *this* panel: its note field is drawn
    // before its buttons and egui reports the focus loss in the same frame, so the edit happens to be
    // queued first even with no flush at all. A panel whose buttons come first — a preview pane above
    // its script box — queues the click first instead and would letter the script from before the
    // user's paste. So the flush is driven directly, with a click and an edit the same frame collected.
    let frame = super::Pending {
        events: vec![(PANEL.to_string(), "read".to_string(), "ui.click", Value::Null)],
        typing: vec![(PANEL.to_string(), "note".to_string(), json!("half-typed"))],
    };
    let ctx = h.ctx.clone();
    super::flush_typing(h.state_mut(), &ctx, &frame);
    h.run_steps(2);
    assert_eq!(inspected(&mut h)["storage"]["note"], "half-typed", "a click delivers the edit it overtook");

    // Leave nothing behind: the panel is process-wide, and the tests after this one should not have to
    // share a running plug-in.
    h.state_mut().run("plugin.panel.close", json!({"id": PANEL, "stop": true})).unwrap();
    h.state_mut().run("plugin.remove", json!({"id": PANEL})).unwrap();
    assert!(photocraft_engine::plugin_panel_cmds::open_ids().is_empty(), "no panel is left open");
}

/// The lettering page of a real panel, as the ABI lets a plug-in draw it: its page switcher (a
/// label beside a dropdown), the script box, the line row, the size slider, and the buttons.
fn lettering_page() -> Value {
    json!({"type":"column","children":[
        {"type":"row","children":[{"type":"label","text":"Panel","level":"muted"},
            {"type":"choice","id":"page","value":"lettering","options":[
                {"value":"lettering","label":"Lettering"},{"value":"styles","label":"Styles"}]}]},
        {"type":"separator"},
        {"type":"text","id":"script","value":"","placeholder":"One line of dialogue per row. Start a line with a style's prefix to set it in that style.","multiline":true,"maxLength":65536},
        {"type":"label","text":"The script is empty: paste one line of dialogue per row.","level":"muted"},
        {"type":"row","children":[{"type":"label","text":"Line"},
            {"type":"number","id":"lineNo","value":1.0,"min":1.0,"max":1.0,"step":1.0,"slider":false},
            {"type":"button","id":"prev","label":"◀"},{"type":"button","id":"next","label":"▶"},
            {"type":"button","id":"first","label":"⏮"},{"type":"button","id":"last","label":"⏭"}]},
        {"type":"column","children":[{"type":"label","text":"Dialogue: (ignored)"},
            {"type":"label","text":"This line has no text, so the buttons skip it.","level":"muted"}]},
        {"type":"row","children":[{"type":"label","text":"Text size"},
            {"type":"number","id":"textScale","value":100.0,"min":1.0,"max":999.0,"step":1.0,"slider":true},
            {"type":"label","text":"%"}]},
        {"type":"separator"},
        {"type":"row","children":[{"type":"button","id":"create","label":"Create","primary":true},
            {"type":"button","id":"insert","label":"Insert"},{"type":"button","id":"align","label":"Align"}]},
        {"type":"label","text":"no document open · no type layer selected","level":"muted"}]})
}

/// The width a real panel window is drawn at, frame by frame.
///
/// The window is the one the app draws ([`window`](super::window)), not a copy of it, because this is
/// where the defect was: an area is sized from its content *every* frame, so a body that sticks out
/// past the panel by a fixed overhang widens the window by that overhang, and again the frame after —
/// the width is what the body is given. 400 px leaves room for every widget below; nothing needs to
/// be wider than the panel, so the width should settle and stay.
fn window_widths(view: Value, frames: usize) -> Vec<f32> {
    let panel = PanelDraw {
        id: "test.widening".into(),
        title: "TyperTools".into(),
        min_width: 400,
        error: None,
        visible: true,
        view: Some(serde_json::from_value(view).expect("a view the host accepts")),
        values: Map::new(),
        progress: None,
        notices: Vec::new(),
    };
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    // The screen a panel is laid out in; only its size matters here.
    let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 760.0));
    let mut widths = Vec::new();
    for _ in 0..frames {
        let mut out = ctx.run_ui(egui::RawInput::default(), |root| {
            let ctx = root.ctx().clone();
            let t = Tokens::get(&ctx);
            let mut pending = Pending::default();
            let _ = super::window(&ctx, &t, &panel, 0, canvas, &mut pending, false);
        });
        // The panel is really drawn; its pixels are not what is being measured.
        out.textures_delta.clear();
        widths.push(ctx.memory(|m| m.area_rect(egui::Id::new(("plugin-panel-v2", "test.widening")))).map_or(0.0, |rect| rect.width()));
    }
    widths
}

/// The screen the window tests lay a panel out in: only its size matters here.
fn canvas() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 760.0))
}

/// One panel, as the window tests draw it, with no engine behind it.
fn test_panel(id: &str, view: Value) -> PanelDraw {
    PanelDraw {
        id: id.into(),
        title: "TyperTools".into(),
        min_width: 400,
        error: None,
        visible: true,
        view: Some(serde_json::from_value(view).expect("a view the host accepts")),
        values: Map::new(),
        progress: None,
        notices: Vec::new(),
    }
}

/// The panel window's outer rectangle, as egui has it after the frame.
fn panel_window_rect(ctx: &egui::Context, id: &str) -> Option<egui::Rect> {
    ctx.memory(|m| m.area_rect(egui::Id::new(("plugin-panel-v2", id))))
}

/// Draws one frame of the panel's real window, with the input egui would have received.
fn draw_panel(ctx: &egui::Context, panel: &PanelDraw, input: egui::RawInput, opening: bool) -> Vec<egui::epaint::ClippedShape> {
    draw_panel_pending(ctx, panel, input, opening).0
}

/// [`draw_panel`], and the interactions the frame collected.
fn draw_panel_pending(
    ctx: &egui::Context,
    panel: &PanelDraw,
    input: egui::RawInput,
    opening: bool,
) -> (Vec<egui::epaint::ClippedShape>, Pending) {
    let mut pending = Pending::default();
    let mut out = ctx.run_ui(input, |root| {
        let ctx = root.ctx().clone();
        let t = Tokens::get(&ctx);
        let _ = super::window(&ctx, &t, panel, 0, canvas(), &mut pending, opening);
    });
    out.textures_delta.clear();
    (out.shapes, pending)
}

/// The painted text shape whose text contains `needle`.
fn painted_text<'a>(shapes: &'a [egui::epaint::ClippedShape], needle: &str) -> Option<&'a egui::epaint::TextShape> {
    shapes.iter().find_map(|shape| match &shape.shape {
        egui::Shape::Text(text) if text.galley.job.text.contains(needle) => Some(text),
        _ => None,
    })
}

/// Where a piece of painted text starts, if it was painted at all.
fn painted_y(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<f32> {
    painted_text(shapes, needle).map(|text| text.pos.y)
}

/// Clicks `at` over three frames and answers with the interactions the release collected.
fn click_at(ctx: &egui::Context, panel: &PanelDraw, at: egui::Pos2) -> Pending {
    let mut collected = Pending::default();
    for pressed in [None, Some(true), Some(false)] {
        let mut input = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        input.events.push(match pressed {
            None => egui::Event::PointerMoved(at),
            Some(pressed) => egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
        });
        let (_, pending) = draw_panel_pending(ctx, panel, input, false);
        if pressed == Some(false) {
            collected = pending;
        }
    }
    collected
}

/// A page of `rows` plain labels: tall enough to scroll, with nothing in it that scrolls on its own.
fn tall_page(rows: usize) -> Value {
    let children: Vec<Value> = (0..rows).map(|i| json!({"type":"label","text":format!("Row {i}")})).collect();
    json!({"type":"column","children":children})
}

#[test]
fn a_panel_body_fills_the_window_and_its_footer_stays_at_the_bottom() {
    // A page shorter than the window, and something in the host's footer. The body used to take
    // only the height of its content (egui's `auto_shrink`), so the window opened at egui's 420 px
    // default with the progress row right under the body and the lower half empty — the styles
    // below the fold with a large empty area under them.
    let mut panel = test_panel("test.fill", json!({"type":"column","children":[
        {"type":"label","text":"A short page"},
        {"type":"label","text":"with two lines in it","level":"muted"}]}));
    panel.progress = Some(Progress { fraction: Some(0.5), text: Some("Rendering".into()), cancellable: false, done: false });
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut shapes = Vec::new();
    for _ in 0..6 {
        let input = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        shapes = draw_panel(&ctx, &panel, input, true);
    }
    let window = panel_window_rect(&ctx, "test.fill").expect("the panel's window is drawn");
    assert!(window.height() >= 600.0, "the panel opens at {} px tall on a 760 px screen: {window:?}", window.height());
    assert!(window.bottom() <= canvas().bottom() + 1.0, "and it fits on the screen: {window:?}");
    let footer = painted_y(&shapes, "Rendering").expect("the progress text is drawn");
    assert!(
        footer > window.bottom() - 90.0,
        "the host's footer is at the bottom of the window, not under the body: {footer} in {window:?}"
    );
}

#[test]
fn a_reopened_panel_starts_at_the_top_of_its_body() {
    // The offset of a scroll area is saved with its id, and egui's memory is saved to `ui.ron`, so
    // a panel used yesterday could open scrolled into the middle of its script. Opening is what
    // resets it: close the panel (a frame it is not drawn in), open it again, and the first line is
    // at the top where it belongs.
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let panel = test_panel("test.scroll", tall_page(60));
    let mut shapes = Vec::new();
    for _ in 0..4 {
        let frame = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        shapes = draw_panel(&ctx, &panel, frame, true);
    }
    let window = panel_window_rect(&ctx, "test.scroll").expect("the panel's window is drawn");
    let first = painted_y(&shapes, "Row 0").expect("the first row is drawn when the panel opens");
    let top = window.top() + 40.0;
    assert!((first - top).abs() < 120.0, "the panel opens at its first row: {first} vs {top}");

    // Scroll the body to its end with the wheel over it.
    let pointer = window.center();
    for _ in 0..10 {
        let mut frame = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        frame.events.push(egui::Event::PointerMoved(pointer));
        frame.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -240.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        });
        shapes = draw_panel(&ctx, &panel, frame, false);
    }
    let scrolled = painted_y(&shapes, "Row 0");
    assert!(scrolled.is_none_or(|y| y < top - 20.0), "the wheel scrolled the body: {scrolled:?}");

    // Close it: a frame no panel is drawn in, as `windows` does for a closed panel.
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    let frame = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
    let reopened = draw_panel(&ctx, &panel, frame, true);
    let first = painted_y(&reopened, "Row 0").expect("the first row is drawn again");
    assert!(
        (first - top).abs() < 120.0,
        "a reopened panel starts at its first row, not at the old offset: {first} vs {top}"
    );
}

/// How far right the tree is drawn when the panel gives it `width` pixels.
///
/// A `row` is a *wrapping* layout, and egui answers `available_width` there with the whole row's
/// width rather than the room left on the line — so a widget drawn after a label is handed the
/// label's width over again and ends up past the panel's right edge. That overhang is what the
/// window, sized from its content every frame, grew by.
fn body_extent(view: Value, width: f32) -> f32 {
    let panel = PanelDraw {
        id: "test.extent".into(),
        title: "TyperTools".into(),
        min_width: 400,
        error: None,
        visible: true,
        view: Some(serde_json::from_value(view).expect("a view the host accepts")),
        values: Map::new(),
        progress: None,
        notices: Vec::new(),
    };
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut extent = 0.0;
    for _ in 0..3 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |root| {
            let ctx = root.ctx().clone();
            let t = Tokens::get(&ctx);
            let view = panel.view.as_ref().expect("a view");
            egui::Frame::NONE.show(root, |ui| {
                ui.set_min_width(width);
                ui.set_max_width(width);
                let mut pending = Pending::default();
                body(ui, &t, &panel, view, &mut pending, Flow::Scrolling, None);
                extent = ui.min_rect().width();
            });
        });
        out.textures_delta.clear();
    }
    extent
}

/// Whether the width settled and then never moved again.
fn settled(widths: &[f32]) -> bool {
    let last = widths[widths.len() - 1];
    widths[widths.len() / 2..].iter().all(|w| (w - last).abs() < 0.5)
}

#[test]
fn a_panel_window_is_as_wide_as_its_panel_and_no_wider() {
    // The lettering page a real plug-in opens on. The dropdown in its first row is what widened the
    // TyperTools panel without end: a `row`'s children are handed the whole row's width (see
    // `body`), so the dropdown was drawn a label's width past the panel, and the window — sized from
    // its content every frame — grew by that much again each frame.
    let widths = window_widths(lettering_page(), 24);
    assert!(settled(&widths), "the panel widened itself, frame by frame: {widths:?}");
    assert!(widths[23] < 500.0, "and it stayed the width its manifest asks for: {}", widths[23]);

    // A widget with a label as long as the ABI allows, in a row: a `choice` sizes its own button to
    // its text, and a plug-in may send one of those. It is drawn, clipped at the panel's edge,
    // rather than making the panel as wide as the text — the same three hundred pixels, forever.
    let wide = json!({"type":"column","children":[
        {"type":"row","children":[{"type":"label","text":"Panel"},{"type":"choice","id":"page","value":"a",
            "options":[{"value":"a","label":"x".repeat(240)}]}]}]});
    let widths = window_widths(wide, 24);
    assert!(settled(&widths), "a plug-in's long dropdown widened the panel: {widths:?}");
    assert!(widths[23] < 500.0, "the panel is as wide as the panel, not as wide as the widget: {}", widths[23]);
}

/// A script box, with a chapter in it, is painted inside its own 150 px and no further.
///
/// A `TextEdit` is as tall as its text. Before the box scrolled, a long script was laid out at 150
/// px (so the styles below started there) but painted at its full height, over the styles and
/// through them — the way the TypeR/TyperTools port first drew. The layout height being right was
/// never the whole story; what is painted has to be clipped too.
#[test]
fn a_long_script_is_clipped_to_its_box() {
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut text = "A line of dialogue, long enough to wrap.\n".repeat(80);
    let mut box_bottom = 0.0;
    let mut shapes = Vec::new();
    // Twice: the galley of a field being edited is laid out on the frame after it is first drawn.
    for _ in 0..2 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |root| {
            egui::Frame::NONE.show(root, |ui| {
                ui.set_max_width(400.0);
                let top = ui.min_rect().bottom();
                let _ = multiline_field(ui, egui::Id::new("long-script"), &mut text, "", 150.0, None);
                box_bottom = ui.min_rect().bottom();
                assert!((box_bottom - top - 150.0).abs() < 1.0, "the script box takes its 150 px: {}", box_bottom - top);
            });
        });
        out.textures_delta.clear();
        shapes = out.shapes;
    }
    let painted: Vec<&egui::epaint::ClippedShape> = shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Text(_))).collect();
    assert!(!painted.is_empty(), "the script is drawn");
    for shape in painted {
        assert!(
            shape.clip_rect.bottom() <= box_bottom + 1.0,
            "script painted past its box, over the styles below: clip {:?}, box bottom {box_bottom}",
            shape.clip_rect
        );
    }
}

#[test]
fn a_panels_widgets_are_drawn_inside_the_panel() {
    // The page switcher's dropdown, after its label, in the panel's 400 pixels: it is given the room
    // that is left on the line, so the row ends where the panel does. With the whole row's width
    // instead (what egui reports inside a wrapping `row`) it ended 39 px past the panel — and the
    // window, sized from its content every frame, followed it, 39 px wider every frame, without end.
    let extent = body_extent(lettering_page(), 400.0);
    assert!(extent <= 400.5, "the tree is drawn {extent} px wide in a 400 px panel");
}

#[test]
fn the_body_stays_painted_while_it_is_scrolled() {
    // Clicking the body's scroll bar near its bottom scrolls there with a single click, which is how
    // the control channel drives a panel (it has no wheel). The body draws its tree inside a rect the
    // scroll area translates up by the scroll offset; clipping the tree to that whole rect, as well
    // as to the panel width, emptied the clip the moment the body was scrolled — every widget was
    // culled and the panel went blank under its header, with the scroll bar still drawn.
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let panel = test_panel("test.bar", tall_page(60));
    let mut shapes = Vec::new();
    for _ in 0..4 {
        let input = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        shapes = draw_panel(&ctx, &panel, input, true);
    }
    let window = panel_window_rect(&ctx, "test.bar").expect("the panel's window is drawn");
    let at = pos2(window.right() - 6.0, window.bottom() - 20.0);
    // Hover first, so the bar is laid out and there to hit; then press and release on its trough
    // near the bottom, which puts the end of the page there.
    for pressed in [None, Some(true), Some(false)] {
        let mut input = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        input.events.push(match pressed {
            None => egui::Event::PointerMoved(at),
            Some(pressed) => egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
        });
        shapes = draw_panel(&ctx, &panel, input, false);
    }
    let last = painted_y(&shapes, "Row 59").expect("the end of the page is painted after the scroll");
    assert!(last > window.top() + 40.0 && last < window.bottom(), "and it is inside the window: {last} in {window:?}");
}

#[test]
fn a_row_wraps_its_buttons_in_a_narrow_panel() {
    // A style row at the 250 px a lettering panel asks for: a label, the size step and the four
    // arrow buttons. Each button used to be drawn inside `add_enabled_ui`, whose child `Ui` is put
    // at the cursor and then advanced past without the wrapping row ever being asked whether it
    // fits — so the arrows ran off the panel's right edge one after another, and the row's grown
    // rect widened the column, which stopped the font line under it wrapping too.
    let mut panel = test_panel("test.wrap", json!({"type":"column","children":[
        {"type":"row","children":[
            {"type":"label","text":"Line"},
            {"type":"number","id":"lineNo","value":1.0,"min":1.0,"max":1.0,"step":1.0,"slider":false},
            {"type":"button","id":"prev","label":"◀"},{"type":"button","id":"next","label":"▶"},
            {"type":"button","id":"first","label":"⏮"},{"type":"button","id":"last","label":"⏭"}]},
        {"type":"label","level":"muted","text":"Font: 000 10 Cents Comic [TeddyBear] Regular · 14 pt"}]}));
    panel.min_width = 250;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut shapes = Vec::new();
    for _ in 0..6 {
        let input = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        shapes = draw_panel(&ctx, &panel, input, true);
    }
    let window = panel_window_rect(&ctx, "test.wrap").expect("the panel's window is drawn");
    assert!(window.width() < 320.0, "the row widened the panel: {window:?}");
    let mut arrows = Vec::new();
    let mut font_line = None;
    for shape in &shapes {
        let egui::Shape::Text(text) = &shape.shape else { continue };
        if ["◀", "▶", "⏮", "⏭"].iter().any(|a| text.galley.job.text == *a) {
            let right = text.pos.x + text.galley.size().x;
            assert!(text.pos.x >= window.left() && right <= window.right(), "{:?} is drawn past the panel: x {}..{right} in {window:?}", text.galley.job.text, text.pos.x);
            arrows.push(text.pos.y);
        }
        if text.galley.job.text.starts_with("Font:") {
            font_line = Some(text);
        }
    }
    assert_eq!(arrows.len(), 4, "all four arrows are drawn: {arrows:?}");
    let first = arrows[0];
    assert!(arrows.iter().any(|y| (*y - first).abs() > 4.0), "the arrows sat on one line, off the panel: {arrows:?}");
    // The font line below the row wraps at the panel, not at the width the overflowing row forced.
    let line = font_line.expect("the font line is drawn");
    assert!(line.galley.job.wrap.max_width <= window.width(), "the font line is given {} px in a {}-px window", line.galley.job.wrap.max_width, window.width());
}

#[test]
fn a_list_row_reports_its_index_and_marks_the_current_one() {
    // The style list of a lettering panel: numbered rows, the current line highlighted, and a click
    // telling the plug-in which row was picked. A row is one line, so the click is unambiguous.
    let mut panel = test_panel(
        "test.list",
        json!({"type":"column","children":[
            {"type":"list","id":"lines","selected":1,"numbered":true,"rows":[
                {"text":"First line"},{"text":"Second line"},{"text":"Third line"}]}]}),
    );
    panel.min_width = 250;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut shapes = Vec::new();
    for _ in 0..6 {
        shapes = draw_panel(&ctx, &panel, egui::RawInput { screen_rect: Some(canvas()), ..Default::default() }, true);
    }
    let window = panel_window_rect(&ctx, "test.list").expect("the panel's window is drawn");
    assert!(window.width() < 320.0, "the list stayed inside the panel: {window:?}");
    // Every row and every gutter number is drawn.
    for text in ["1", "2", "3", "First line", "Second line", "Third line"] {
        assert!(painted_y(&shapes, text).is_some(), "{text:?} was not drawn");
    }
    // The selected row is the current one: its text takes the accent, the rows around it do not.
    let colour_of = |needle: &str| painted_text(&shapes, needle).and_then(|t| t.galley.job.sections.first().map(|s| s.format.color));
    let t = Tokens::get(&ctx);
    assert_eq!(colour_of("Second line"), Some(t.accent_text));
    assert_eq!(colour_of("First line"), Some(t.text));

    // A click on the third row sends its index to the plug-in.
    let third = painted_text(&shapes, "Third line").expect("the third row is drawn");
    let at = pos2(window.left() + 40.0, third.pos.y + third.galley.size().y / 2.0);
    let events = click_at(&ctx, &panel, at);
    assert!(
        events.events.iter().any(|(panel, widget, kind, value)| panel == "test.list" && widget == "lines" && *kind == "ui.click" && value == &json!(2)),
        "a click on the third row reports index 2: {:?}",
        events.events
    );
}

#[test]
fn scroll_panes_share_the_panel_and_keep_their_footers_visible() {
    // The lettering layout the widgets exist for: a script pane above a styles pane, each scrolling
    // on its own and each with a pinned footer (the counter and the navigation) that stays put.
    let script: Vec<Value> = (0..80).map(|i| json!({"type":"label","text":format!("Script {i}")})).collect();
    let styles: Vec<Value> = (0..40).map(|i| json!({"type":"label","text":format!("Style {i}")})).collect();
    let mut panel = test_panel(
        "test.panes",
        json!({"type":"column","children":[
            {"type":"scroll","id":"script","weight":2,"children":script,"footer":[
                {"type":"label","text":"Pinned counter"}]},
            {"type":"scroll","id":"styles","weight":1,"children":styles,"footer":[
                {"type":"label","text":"Pinned styles"}]}]}),
    );
    panel.min_width = 250;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut shapes = Vec::new();
    for _ in 0..6 {
        shapes = draw_panel(&ctx, &panel, egui::RawInput { screen_rect: Some(canvas()), ..Default::default() }, true);
    }
    let window = panel_window_rect(&ctx, "test.panes").expect("the panel's window is drawn");
    assert!(window.width() < 320.0, "the panes stayed inside the panel: {window:?}");
    // Both panes start at their own first row: the script's 80 lines did not push the styles out.
    let first_script = painted_y(&shapes, "Script 0").expect("the script pane shows its first line");
    let first_style = painted_y(&shapes, "Style 0").expect("the styles pane shows its own first row");
    assert!(first_style > first_script, "the styles pane is below the script pane: {first_style} vs {first_script}");
    // Both pinned footers are inside the window, the second one at the bottom.
    let counter = painted_y(&shapes, "Pinned counter").expect("the script pane's footer is drawn");
    let styles_footer = painted_y(&shapes, "Pinned styles").expect("the styles pane's footer is drawn");
    for y in [counter, styles_footer] {
        assert!(y > window.top() + 40.0 && y < window.bottom(), "a footer is inside the window: {y} in {window:?}");
    }
    assert!(styles_footer > counter, "each footer is at the bottom of its own pane: {styles_footer} vs {counter}");

    // The wheel over the script pane scrolls that pane, and only that one.
    let at = pos2(window.center().x, first_script + 10.0);
    for _ in 0..6 {
        let mut input = egui::RawInput { screen_rect: Some(canvas()), ..Default::default() };
        input.events.push(egui::Event::PointerMoved(at));
        input.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -240.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        });
        shapes = draw_panel(&ctx, &panel, input, false);
    }
    assert!(painted_y(&shapes, "Script 0").is_none_or(|y| y < first_script - 20.0), "the script pane scrolled: {:?}", painted_y(&shapes, "Script 0"));
    assert!(painted_y(&shapes, "Style 0").is_some(), "the styles pane did not move with it");
    assert!(painted_y(&shapes, "Pinned counter").is_some(), "and the pinned footer stays");
}

#[test]
fn a_label_named_font_comes_from_the_host_and_an_unknown_one_falls_back() {
    // A plug-in never touches a font file: it names a family and the host resolves it in its own
    // database. An installed family is used for the label; an unknown one is the panel font, not a
    // crash and not a stopped panel. The whole tree is still drawn while the font is being
    // registered (egui takes it from the next pass).
    let panel = test_panel(
        "test.font",
        json!({"type":"column","children":[
            {"type":"label","text":"Known","font":"Inter"},
            {"type":"label","text":"Unknown","font":"No Such Family Anywhere"},
            {"type":"list","id":"styles","rows":[
                {"text":"Comic","font":"No Such Family Anywhere","color":"#ff8800"}]}]}),
    );
    // The host resolves families in its shared font database, which a desktop build scans in the
    // background on first use. Wait for that scan (bounded) so the bundled `Inter` is there; the
    // panel itself never waits for it — it draws in its panel font and asks for a repaint.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while photocraft_text::try_shared().is_none() {
        assert!(std::time::Instant::now() < deadline, "the system font scan never finished");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut shapes = Vec::new();
    for _ in 0..4 {
        shapes = draw_panel(&ctx, &panel, egui::RawInput { screen_rect: Some(canvas()), ..Default::default() }, true);
    }
    let family = |needle: &str| painted_text(&shapes, needle).and_then(|t| t.galley.job.sections.first().map(|s| s.format.font_id.family.clone()));
    assert_eq!(family("Known"), Some(egui::FontFamily::Name("plugin:Inter".into())), "the host resolved the family it was given");
    assert_eq!(family("Unknown"), Some(egui::FontFamily::Proportional), "an unknown family is the panel font");
    // A row's colour is used as given, and a row font that does not exist falls back the same way.
    let comic = painted_text(&shapes, "Comic").expect("the row is drawn");
    assert_eq!(comic.galley.job.sections[0].format.color, Color32::from_rgb(0xff, 0x88, 0x00));
}

#[test]
fn an_icon_button_draws_its_glyph_and_reports_a_click() {
    // A row of compact icon buttons, as a lettering panel's line navigation: the glyph is what is
    // drawn, the label is the tooltip, and a click reports the widget id like any other button.
    let mut panel = test_panel(
        "test.icons",
        json!({"type":"row","children":[
            {"type":"label","text":"Line"},
            {"type":"button","id":"prev","label":"Previous line","icon":"◀"},
            {"type":"button","id":"next","label":"Next line","icon":"▶"}]}),
    );
    panel.min_width = 250;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, Default::default());
    let mut shapes = Vec::new();
    for _ in 0..6 {
        shapes = draw_panel(&ctx, &panel, egui::RawInput { screen_rect: Some(canvas()), ..Default::default() }, true);
    }
    for glyph in ["◀", "▶"] {
        assert!(painted_y(&shapes, glyph).is_some(), "{glyph:?} is drawn as an icon");
    }
    let next = painted_text(&shapes, "▶").expect("the next button is drawn");
    let at = next.pos + next.galley.size() / 2.0;
    let events = click_at(&ctx, &panel, at);
    assert!(
        events.events.iter().any(|(panel, widget, kind, value)| panel == "test.icons" && widget == "next" && *kind == "ui.click" && value.is_null()),
        "the icon button sent its click: {:?}",
        events.events
    );
}
