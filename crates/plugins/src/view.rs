//! The declarative widget tree a panel draws (`ui.view`).
//!
//! A plug-in describes widgets; the host draws them with egui and `theme::Tokens`. A plug-in cannot
//! choose a colour, font or size, so every panel is on-theme in light and dark for free, and the
//! tree stays readable through `ui.inspect`.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Most widgets in one view.
pub const MAX_WIDGETS: usize = 512;
/// Deepest nesting in one view.
pub const MAX_DEPTH: usize = 16;
/// Most options in one `choice`.
pub const MAX_OPTIONS: usize = 256;
/// Longest text a `text` widget may hold.
///
/// A whole chapter of a lettering script goes in one `text` field, so this is generous — a page of
/// dialogue is a few kilobytes and the ABI already bounds one reply to 1 MiB (`ops::MAX_OPS_BYTES`),
/// which is what keeps a panel from filling memory with widgets rather than this.
pub const MAX_TEXT_LENGTH: usize = 65_536;
/// Longest label, title or placeholder.
pub const MAX_LABEL: usize = 256;
/// Most rows in one `list` (a lettering panel lists a page of styles and a script of lines).
pub const MAX_LIST_ROWS: usize = 1000;
/// Longest font family name a label or list row may name.
pub const MAX_FONT_NAME: usize = 128;
/// Tallest one `scroll` pane may ask for, in points.
pub const MAX_SCROLL_HEIGHT: f64 = 4000.0;

/// The widget features this host understands, as reported to a plug-in in `init.host.widgets`.
///
/// A panel that wants a newer widget (a list, a scroll pane, a coloured label) checks this list
/// first and falls back to the plain layout when the host it is running in does not have it:
/// plug-ins are built once and installed into every PhotoCraft that will take them.
pub const WIDGET_FEATURES: [&str; 5] = ["list", "scroll", "label.color", "label.font", "button.icon"];

/// How a `label` is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Muted,
    Info,
    Warning,
    Error,
}

/// One option of a `choice`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
}

/// One row of a `list`: a script line, a style name, a folder.
///
/// `color` and `font` are what a lettering panel needs to show its library the way the original
/// plug-in did — each style named in the font it letters in, beside a swatch of its colour. The host
/// resolves the family from its own font database (a plug-in never sees a font file), and falls back
/// to the panel font when the family is not installed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRow {
    pub text: String,
    /// `#rrggbb`, drawn on the row's text; the theme's own colour when absent.
    #[serde(default)]
    pub color: Option<String>,
    /// A font family's name, resolved by the host; the panel font when absent or unknown.
    #[serde(default)]
    pub font: Option<String>,
}

/// One node of the tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Widget {
    /// Children stacked vertically.
    Column {
        children: Vec<Widget>,
    },
    /// Children laid out in a line.
    Row {
        children: Vec<Widget>,
    },
    /// A collapsible group; its collapsed state is kept per plug-in by the host.
    Section {
        title: String,
        #[serde(default)]
        collapsed: bool,
        children: Vec<Widget>,
    },
    /// Plain text, no markup.
    Label {
        text: String,
        #[serde(default)]
        level: Option<Level>,
        /// `#rrggbb`; the level's theme colour when absent.
        #[serde(default)]
        color: Option<String>,
        /// A font family name, resolved by the host; the panel font when absent or unknown.
        #[serde(default)]
        font: Option<String>,
    },
    /// A prompt field. `multiline` is the script box: a whole chapter lives in one.
    Text {
        id: String,
        #[serde(default)]
        value: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        multiline: bool,
        #[serde(default)]
        max_length: Option<usize>,
    },
    /// A number, as a field or (with `slider`) a slider. `step` is honoured.
    Number {
        id: String,
        value: f64,
        min: f64,
        max: f64,
        #[serde(default)]
        step: Option<f64>,
        #[serde(default)]
        slider: bool,
    },
    Toggle {
        id: String,
        value: bool,
        label: String,
    },
    Choice {
        id: String,
        value: String,
        options: Vec<ChoiceOption>,
    },
    /// Sends `ui.click` when pressed.
    Button {
        id: String,
        label: String,
        #[serde(default)]
        primary: bool,
        #[serde(default)]
        enabled: Option<bool>,
        /// A short glyph drawn instead of the label (a compact icon button).
        #[serde(default)]
        icon: Option<String>,
    },
    /// Rows of text, one selectable line each; a click sends `ui.click` with the row index.
    ///
    /// With `numbered`, a line-number gutter is drawn — the script list of a lettering panel. The
    /// row at `selected` is highlighted (the current line, the open style).
    List {
        id: String,
        #[serde(default)]
        selected: Option<usize>,
        #[serde(default)]
        numbered: bool,
        rows: Vec<ListRow>,
    },
    /// A pane that scrolls on its own, with an optional pinned footer below it.
    ///
    /// `height` is the pane's height in points; without it the host shares the panel's free height
    /// among the height-less panes of one `column` by `weight` (1 by default). The footer is drawn
    /// under the pane and does not scroll with it — a lettering panel's counter and navigation stay
    /// put while the script above them scrolls.
    Scroll {
        id: String,
        #[serde(default)]
        height: Option<f64>,
        #[serde(default = "default_weight")]
        weight: u8,
        #[serde(default)]
        children: Vec<Widget>,
        #[serde(default)]
        footer: Vec<Widget>,
    },
    /// A bar inside the panel; `ui.progress` drives the status bar instead.
    Progress {
        fraction: Option<f64>,
    },
    /// A PNG or JPEG blob, decoded off the UI thread.
    Image {
        blob: String,
        #[serde(default)]
        max_height: Option<f64>,
    },
    Separator,
}

impl Widget {
    /// The widget's id, for the widgets that have one.
    pub fn id(&self) -> Option<&str> {
        match self {
            Widget::Text { id, .. }
            | Widget::Number { id, .. }
            | Widget::Toggle { id, .. }
            | Widget::Choice { id, .. }
            | Widget::Button { id, .. }
            | Widget::List { id, .. }
            | Widget::Scroll { id, .. } => Some(id),
            _ => None,
        }
    }

    fn children(&self) -> &[Widget] {
        match self {
            Widget::Column { children } | Widget::Row { children } | Widget::Section { children, .. } | Widget::Scroll { children, .. } => children,
            _ => &[],
        }
    }
}

/// The weight a `scroll` gets when it names none: one share of the column's free height.
fn default_weight() -> u8 {
    1
}

/// A widget id: `[A-Za-z0-9_.-]{1,64}`, unique within the view.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Whether a tree has a `scroll` pane anywhere, its pinned footers included.
///
/// A view with one is drawn with the panel's real height as its budget instead of inside the
/// panel body's own scroll area, so its panes can share that height and scroll independently.
pub fn has_scroll(root: &Widget) -> bool {
    matches!(root, Widget::Scroll { .. }) || root.children().iter().any(has_scroll)
}

/// Checks a whole tree: widget count, depth, ids, and the ranges the types cannot express. A bad
/// view is rejected whole: the reply carrying it is refused before any of it is applied, so the
/// view already on screen stays as it was and the panel stops with the reason.
pub fn check(root: &Widget) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut count = 0usize;
    walk(root, 1, &mut count, &mut seen)
}

fn walk(w: &Widget, depth: usize, count: &mut usize, seen: &mut BTreeSet<String>) -> Result<()> {
    *count += 1;
    if *count > MAX_WIDGETS {
        return Err(Error::Ops(format!("the view has more than {MAX_WIDGETS} widgets")));
    }
    if depth > MAX_DEPTH {
        return Err(Error::Ops(format!("the view nests deeper than {MAX_DEPTH}")));
    }
    if let Some(id) = w.id() {
        if !valid_id(id) {
            return Err(Error::Ops(format!("invalid widget id {id:?}: use 1-64 characters from A-Z a-z 0-9 . _ -")));
        }
        if !seen.insert(id.to_string()) {
            return Err(Error::Ops(format!("duplicate widget id {id:?}")));
        }
    }
    match w {
        Widget::Section { title, .. } => short_text("section title", Some(title))?,
        Widget::Label { text, color, font, .. } => {
            short_text("label text", Some(text))?;
            check_color("label", color.as_deref())?;
            check_font("label", font.as_deref())?;
        }
        Widget::Text { value, placeholder, max_length, .. } => {
            let cap = max_length.unwrap_or(MAX_TEXT_LENGTH);
            if cap > MAX_TEXT_LENGTH {
                return Err(Error::Ops(format!("`maxLength` {cap} exceeds {MAX_TEXT_LENGTH}")));
            }
            if value.chars().count() > cap {
                return Err(Error::Ops(format!("a `text` value is longer ({}) than its `maxLength` ({cap})", value.chars().count())));
            }
            short_text("placeholder", placeholder.as_deref())?;
        }
        Widget::Number { value, min, max, step, .. } => {
            if !(value.is_finite() && min.is_finite() && max.is_finite()) || min > max {
                return Err(Error::Ops("a `number` has a bad range".into()));
            }
            if step.is_some_and(|s| !s.is_finite() || s <= 0.0) {
                return Err(Error::Ops("a `number` has a non-positive `step`".into()));
            }
        }
        Widget::Toggle { label, .. } => short_text("toggle label", Some(label))?,
        Widget::Button { label, icon, .. } => {
            short_text("button label", Some(label))?;
            short_text("button icon", icon.as_deref())?;
        }
        Widget::List { selected, rows, .. } => {
            if rows.len() > MAX_LIST_ROWS {
                return Err(Error::Ops(format!("a `list` has more than {MAX_LIST_ROWS} rows")));
            }
            if selected.is_some_and(|i| i >= rows.len()) {
                return Err(Error::Ops("a `list` `selected` is past its last row".into()));
            }
            for row in rows {
                if row.text.chars().count() > MAX_TEXT_LENGTH {
                    return Err(Error::Ops(format!("a `list` row is longer than {MAX_TEXT_LENGTH} characters")));
                }
                check_color("list row", row.color.as_deref())?;
                check_font("list row", row.font.as_deref())?;
            }
        }
        Widget::Scroll { height, weight, .. } => {
            if height.is_some_and(|h| !h.is_finite() || !(0.0..=MAX_SCROLL_HEIGHT).contains(&h)) {
                return Err(Error::Ops(format!("a `scroll` `height` must be between 0 and {MAX_SCROLL_HEIGHT}")));
            }
            if !(1..=16).contains(weight) {
                return Err(Error::Ops("a `scroll` `weight` must be 1-16".into()));
            }
        }
        Widget::Choice { value, options, .. } => {
            if options.is_empty() || options.len() > MAX_OPTIONS {
                return Err(Error::Ops(format!("a `choice` must offer 1-{MAX_OPTIONS} options")));
            }
            for o in options {
                if o.value.is_empty() || o.value.len() > MAX_LABEL {
                    return Err(Error::Ops("a `choice` option needs a value of 1-256 bytes".into()));
                }
                short_text("choice label", Some(&o.label))?;
            }
            if !options.iter().any(|o| o.value == *value) {
                return Err(Error::Ops(format!("a `choice` value {value:?} is not one of its options")));
            }
        }
        Widget::Progress { fraction } => {
            if fraction.is_some_and(|f| !f.is_finite() || !(0.0..=1.0).contains(&f)) {
                return Err(Error::Ops("a `progress` fraction must be between 0 and 1".into()));
            }
        }
        Widget::Image { blob, max_height } => {
            if !crate::blobs::valid_id(blob) {
                return Err(Error::Ops(format!("`image` names {blob:?}, which is not a blob id")));
            }
            if max_height.is_some_and(|h| !h.is_finite() || h <= 0.0) {
                return Err(Error::Ops("an `image` `maxHeight` must be positive".into()));
            }
        }
        _ => {}
    }
    for child in w.children() {
        walk(child, depth + 1, count, seen)?;
    }
    // A `scroll`'s pinned footer is part of the tree too: it holds ids, and it can be deep.
    if let Widget::Scroll { footer, .. } = w {
        for child in footer {
            walk(child, depth + 1, count, seen)?;
        }
    }
    Ok(())
}

fn short_text(what: &str, text: Option<&str>) -> Result<()> {
    if text.is_some_and(|t| t.chars().count() > MAX_LABEL) {
        return Err(Error::Ops(format!("a {what} is longer than {MAX_LABEL} characters")));
    }
    Ok(())
}

/// `#rrggbb`, the one colour form a plug-in may name. Anything else is refused rather than guessed
/// at: a panel that sends `red` or `#f00` should hear that it did, not silently get the theme.
pub fn valid_color(c: &str) -> bool {
    c.strip_prefix('#').is_some_and(|hex| hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn check_color(what: &str, color: Option<&str>) -> Result<()> {
    match color {
        Some(c) if !valid_color(c) => Err(Error::Ops(format!("a {what} `color` must be #rrggbb, not {c:?}"))),
        _ => Ok(()),
    }
}

/// A font family name is a name, not a path: bounded in length, printable. The host looks it up in
/// its own database, so an unknown or misspelled family is not an error — it falls back.
fn check_font(what: &str, font: Option<&str>) -> Result<()> {
    let Some(font) = font else { return Ok(()) };
    if font.is_empty() {
        return Err(Error::Ops(format!("a {what} `font` must name a family, or be left out")));
    }
    if font.chars().count() > MAX_FONT_NAME {
        return Err(Error::Ops(format!("a {what} `font` is longer than {MAX_FONT_NAME} characters")));
    }
    if font.chars().any(char::is_control) {
        return Err(Error::Ops(format!("a {what} `font` contains a control character")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> Result<Widget> {
        serde_json::from_value(v).map_err(|e| Error::Ops(e.to_string()))
    }

    #[test]
    fn parses_the_spec_example() {
        // The `ui.view` example from `docs/plugins-v2.md`.
        let w = parse(json!({"type": "column", "children": [
            {"type": "choice", "id": "tool", "value": "txt2img",
             "options": [{"value": "txt2img", "label": "Text to Image"}, {"value": "inpaint", "label": "Inpaint"}]},
            {"type": "text", "id": "prompt", "value": "", "multiline": true, "placeholder": "Describe the image"},
            {"type": "section", "title": "Settings", "collapsed": true, "children": [
                {"type": "number", "id": "steps", "value": 4, "min": 1, "max": 50, "step": 1, "slider": true},
                {"type": "number", "id": "seed", "value": 0, "min": 0, "max": 4294967295.0, "step": 1}]},
            {"type": "button", "id": "generate", "label": "Generate", "primary": true},
            {"type": "label", "id": "status", "text": "ComfyUI: connected", "level": "muted"}]}))
        .unwrap();
        check(&w).unwrap();
        let Widget::Column { children } = &w else { return };
        assert_eq!(children.len(), 5);
        assert_eq!(children[2].id(), None, "a section has no id of its own");
        assert!(matches!(&children[3], Widget::Button { primary: true, .. }));
    }

    #[test]
    fn rejects_bad_trees() {
        // Not a widget type at all.
        assert!(parse(json!({"type": "canvas"})).is_err());
        let cases = [
            // Bad id.
            json!({"type": "button", "id": "no spaces", "label": "x"}),
            json!({"type": "button", "id": "", "label": "x"}),
            // Duplicate id.
            json!({"type": "column", "children": [
                {"type": "toggle", "id": "a", "value": true, "label": "x"},
                {"type": "toggle", "id": "a", "value": false, "label": "y"}]}),
            // Bad ranges.
            json!({"type": "number", "id": "n", "value": 1, "min": 5, "max": 1}),
            json!({"type": "number", "id": "n", "value": 1, "min": 0, "max": 2, "step": 0}),
            json!({"type": "progress", "fraction": 1.5}),
            // A choice whose value is not offered, or offers nothing.
            json!({"type": "choice", "id": "c", "value": "z", "options": [{"value": "a", "label": "A"}]}),
            json!({"type": "choice", "id": "c", "value": "a", "options": []}),
            // Not a blob id.
            json!({"type": "image", "blob": "17"}),
            json!({"type": "image", "blob": "b:1", "maxHeight": -1}),
            // Value longer than its own cap.
            json!({"type": "text", "id": "t", "value": "abcd", "maxLength": 2}),
        ];
        for case in cases {
            let w = parse(case.clone()).unwrap();
            assert!(check(&w).is_err(), "{case}");
        }
        // An indeterminate bar is valid, not a mistake.
        let w = parse(json!({"type": "progress", "fraction": null})).unwrap();
        check(&w).unwrap();
    }

    #[test]
    fn parses_the_lettering_widgets() {
        let w = parse(json!({"type": "column", "children": [
            {"type": "scroll", "id": "script", "weight": 2, "footer": [
                {"type": "row", "children": [
                    {"type": "number", "id": "lineNo", "value": 1, "min": 1, "max": 1, "step": 1},
                    {"type": "button", "id": "prev", "label": "", "icon": "◀"}]}]},
            {"type": "list", "id": "styles", "selected": 1, "numbered": true, "rows": [
                {"text": "Sound effect", "font": "Comic Sans MS", "color": "#ff8800"},
                {"text": "Narration", "font": "No Such Family"}]},
            {"type": "label", "text": "Font: Inter", "font": "Inter", "color": "#33ccff"}]}))
        .unwrap();
        check(&w).unwrap();
        let Widget::Column { children } = &w else { return };
        let Widget::Scroll { weight, footer, children: inner, .. } = &children[0] else { panic!("a scroll") };
        assert_eq!(*weight, 2);
        assert!(inner.is_empty());
        assert_eq!(footer.len(), 1);
        assert_eq!(children[0].id(), Some("script"));
        let Widget::List { numbered, selected, rows, .. } = &children[1] else { panic!("a list") };
        assert!(*numbered);
        assert_eq!(*selected, Some(1));
        assert_eq!(rows[0].color.as_deref(), Some("#ff8800"));
        // A family this machine does not have is valid, not a mistake: the host falls back.
        assert_eq!(rows[1].font.as_deref(), Some("No Such Family"));
        // A `scroll` without a weight takes one share of the column's free height.
        let w = parse(json!({"type": "scroll", "id": "s", "children": []})).unwrap();
        assert!(matches!(&w, Widget::Scroll { weight: 1, height: None, .. }));
    }

    #[test]
    fn rejects_bad_lettering_widgets() {
        let cases = [
            // Selected past the last row.
            json!({"type": "list", "id": "l", "selected": 1, "rows": [{"text": "a"}]}),
            // Colours are #rrggbb, not names or shorthand.
            json!({"type": "list", "id": "l", "rows": [{"text": "a", "color": "#f00"}]}),
            json!({"type": "list", "id": "l", "rows": [{"text": "a", "color": "#gggggg"}]}),
            json!({"type": "label", "text": "x", "color": "red"}),
            // Font names are short and printable; empty is a mistake, not "no font".
            json!({"type": "label", "text": "x", "font": ""}),
            json!({"type": "label", "text": "x", "font": "fold\nfamily"}),
            json!({"type": "list", "id": "l", "rows": [{"text": "a", "font": "\u{1}"}]}),
            // Scroll heights and weights are bounded.
            json!({"type": "scroll", "id": "s", "height": -1, "children": []}),
            json!({"type": "scroll", "id": "s", "height": 5000, "children": []}),
            json!({"type": "scroll", "id": "s", "weight": 0, "children": []}),
            json!({"type": "scroll", "id": "s", "weight": 17, "children": []}),
        ];
        for case in cases {
            let w = parse(case.clone()).unwrap();
            assert!(check(&w).is_err(), "{case}");
        }
        // An empty list (a folder with nothing in it) and an empty everything are fine.
        let w = parse(json!({"type": "list", "id": "l", "rows": []})).unwrap();
        check(&w).unwrap();

        // Too many rows, and a font name one character too long.
        let rows = vec![ListRow { text: "x".into(), color: None, font: None }; MAX_LIST_ROWS + 1];
        assert!(check(&Widget::List { id: "l".into(), selected: None, numbered: false, rows }).is_err());
        let rows = vec![ListRow { text: "x".into(), color: None, font: None }; MAX_LIST_ROWS];
        assert!(check(&Widget::List { id: "l".into(), selected: None, numbered: false, rows }).is_ok());
        let long = "f".repeat(MAX_FONT_NAME + 1);
        let label = Widget::Label { text: "x".into(), level: None, color: None, font: Some(long) };
        assert!(check(&label).is_err());
    }

    #[test]
    fn a_scroll_footer_is_part_of_the_tree() {
        // Its ids are checked like any other, and a duplicate there is refused.
        let w = parse(json!({"type": "scroll", "id": "s", "children": [], "footer": [
            {"type": "button", "id": "s", "label": "x"}]}))
        .unwrap();
        assert!(check(&w).is_err());
        // And it counts toward the depth limit: each footer nests one level deeper.
        let mut deep = json!({"type": "separator"});
        for i in 0..MAX_DEPTH {
            deep = json!({"type": "scroll", "id": format!("s{i}"), "footer": [deep]});
        }
        assert!(check(&parse(deep).unwrap()).is_err());
    }

    #[test]
    fn a_scroll_anywhere_switches_the_panel_to_pane_mode() {
        let plain = parse(json!({"type": "column", "children": [{"type": "label", "text": "x"}]})).unwrap();
        assert!(!has_scroll(&plain));
        let nested = parse(json!({"type": "row", "children": [
            {"type": "section", "title": "More", "children": [{"type": "scroll", "id": "s", "children": []}]}]}))
        .unwrap();
        assert!(has_scroll(&nested), "a pane below a row and a section still counts");
        let top = parse(json!({"type": "scroll", "id": "s", "children": []})).unwrap();
        assert!(has_scroll(&top));
    }

    #[test]
    fn reports_the_widget_features_it_draws() {
        // What a plug-in checks before using a widget: the names it is told are the ones in `Widget`.
        for feature in WIDGET_FEATURES {
            assert!(matches!(feature, "list" | "scroll" | "label.color" | "label.font" | "button.icon"), "{feature}");
        }
        assert!(WIDGET_FEATURES.contains(&"list"));
        assert!(WIDGET_FEATURES.contains(&"scroll"));
    }

    #[test]
    fn rejects_trees_that_are_too_big_or_too_deep() {
        let leaf = json!({"type": "separator"});
        let column = |n: usize| Widget::Column { children: (0..n).map(|_| parse(leaf.clone()).unwrap()).collect() };
        assert!(check(&column(MAX_WIDGETS - 1)).is_ok());
        assert!(check(&column(MAX_WIDGETS)).is_err());

        let mut deep = leaf;
        for _ in 0..MAX_DEPTH {
            deep = json!({"type": "row", "children": [deep]});
        }
        assert!(check(&parse(deep).unwrap()).is_err());
    }

    #[test]
    fn a_text_value_may_fill_its_cap() {
        let w = parse(json!({"type": "text", "id": "t", "value": "ab", "maxLength": 2})).unwrap();
        check(&w).unwrap();
        // The default cap applies when none is given.
        let long = "x".repeat(MAX_TEXT_LENGTH + 1);
        let w = parse(json!({"type": "text", "id": "t", "value": long})).unwrap();
        assert!(check(&w).is_err());
        assert!(check(&parse(json!({"type": "text", "id": "t", "value": "hi"})).unwrap()).is_ok());
    }
}
