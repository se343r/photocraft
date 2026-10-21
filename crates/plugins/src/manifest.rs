//! The plug-in manifest (`pc_manifest`) and parameter schema.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{Error, Result};

/// Longest manifest the host reads.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Most parameters a plug-in may declare.
pub const MAX_PARAMS: usize = 64;
/// Largest neighbourhood (`overlap`) a plug-in may ask for, in pixels.
pub const MAX_OVERLAP: u32 = 256;
/// Narrowest a panel plug-in may ask to be, in points.
pub const MIN_PANEL_WIDTH: u32 = 160;
/// Widest a panel plug-in may ask to be, in points.
pub const MAX_PANEL_WIDTH: u32 = 640;
/// Most loopback hosts a panel plug-in may declare.
pub const MAX_NETWORK_HOSTS: usize = 16;

/// What a plug-in is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// ABI v1: a pixel transform, run on one layer in a fresh instance per band.
    Filter,
    /// ABI v2: a long-lived panel that returns operations for the host to carry out.
    Panel,
}

/// The panel a `kind:"panel"` plug-in declares.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    /// Dock-tab label; `name`'s rule (1-64 printable characters) applies.
    pub title: String,
    /// Narrowest the user may drag the panel, in points.
    #[serde(default = "default_panel_min_width")]
    pub min_width: u32,
}

fn default_panel_min_width() -> u32 {
    260
}

/// What a panel asks for. Everything is off unless declared, and a declared capability is still only
/// granted when the user allows it (the documented ABI v2 consent dialog).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// What the panel may see or change in the document: `[]`, `["read"]` or `["read","addLayers"]`.
    #[serde(default)]
    pub document: Vec<DocumentCap>,
    /// Loopback HTTP access. Parsed and validated here; the host refuses `http.request` until it
    /// implements the transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    /// A per-plug-in key/value store (≤ 1 MiB) kept beside the preferences.
    #[serde(default)]
    pub storage: bool,
}

/// One document capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentCap {
    /// `doc.info` and `doc.capture`: see the canvas and the layer stack.
    Read,
    /// `doc.commit`: add layers to the document (one undo step).
    AddLayers,
}

/// Declared loopback access.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    /// `host` or `host:port`; `"127.0.0.1:*"` asks for any port, `"localhost:8188"` for one.
    pub hosts: Vec<String>,
    /// Lets the user edit the list in Preferences (a ComfyUI moved to another port).
    #[serde(default)]
    pub user_can_edit: bool,
}

/// Which pixels a filter is given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Area {
    /// The layer's non-empty pixels (like Invert or Gaussian Blur).
    #[default]
    Content,
    /// The whole canvas, including transparent pixels (like Clouds).
    Canvas,
}

/// One declared parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ParamSpec {
    /// A real number in `min..=max` (a slider in the dialog).
    Number { min: f64, max: f64, default: f64 },
    /// A whole number in `min..=max`.
    Int {
        #[serde(default = "int_min")]
        min: i64,
        #[serde(default = "int_max")]
        max: i64,
        #[serde(default)]
        default: i64,
    },
    /// A checkbox.
    Bool {
        #[serde(default)]
        default: bool,
    },
    /// One of `options` (a dropdown).
    Choice {
        options: Vec<String>,
        #[serde(default)]
        default: Option<String>,
    },
}

fn int_min() -> i64 {
    -1_000_000
}
fn int_max() -> i64 {
    1_000_000
}

/// The manifest a plug-in returns from `pc_manifest` (UTF-8 JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Stable id, `[A-Za-z0-9._-]{1,64}` (reverse-DNS style is recommended: `org.example.invert`).
    pub id: String,
    /// Menu label.
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub kind: Kind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Parameters in declaration (dialog) order; a JSON object `{"name": spec, …}` in the manifest.
    #[serde(default, deserialize_with = "ordered_params", serialize_with = "params_as_object")]
    pub params: Vec<(String, ParamSpec)>,
    /// Pixels of context the filter needs around each output pixel (0 = point filter).
    #[serde(default)]
    pub overlap: u32,
    #[serde(default)]
    pub area: Area,
    /// A `panel` plug-in's panel declaration; absent (and refused) for filters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<Panel>,
    /// What a panel asks for; ignored for filters.
    #[serde(default)]
    pub capabilities: Capabilities,
}

impl Manifest {
    /// Parses and validates manifest JSON.
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Manifest(format!("manifest is {} bytes (limit {MAX_MANIFEST_BYTES})", bytes.len())));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Manifest("manifest is not UTF-8".into()))?;
        let man: Manifest = serde_json::from_str(text).map_err(|e| Error::Manifest(format!("manifest: {e}")))?;
        man.validate()?;
        Ok(man)
    }

    fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            return Err(Error::Manifest(format!("invalid id {:?}: use 1-64 characters from A-Z a-z 0-9 . _ -", self.id)));
        }
        let name = self.name.trim();
        let printable = name.chars().all(|c| !c.is_control() && !hidden_format(c)) && name.chars().any(visible);
        if !printable || name.chars().count() > 64 {
            return Err(Error::Manifest("`name` must be 1-64 printable characters".into()));
        }
        if self.version.len() > 32 || self.description.len() > 1024 || self.author.len() > 128 {
            return Err(Error::Manifest("`version`, `description` or `author` is too long".into()));
        }
        if self.overlap > MAX_OVERLAP {
            return Err(Error::Manifest(format!("`overlap` {} exceeds {MAX_OVERLAP}", self.overlap)));
        }
        match self.kind {
            Kind::Panel => {
                let Some(panel) = &self.panel else {
                    return Err(Error::Manifest("a `panel` plug-in must declare `panel`".into()));
                };
                let title = panel.title.trim();
                let printable = title.chars().all(|c| !c.is_control() && !hidden_format(c)) && title.chars().any(visible);
                if !printable || title.chars().count() > 64 {
                    return Err(Error::Manifest("`panel.title` must be 1-64 printable characters".into()));
                }
                if !(MIN_PANEL_WIDTH..=MAX_PANEL_WIDTH).contains(&panel.min_width) {
                    return Err(Error::Manifest(format!("`panel.minWidth` {} must be between {MIN_PANEL_WIDTH} and {MAX_PANEL_WIDTH}", panel.min_width)));
                }
            }
            // `overlap` and `area` describe pixels; a panel has none.
            Kind::Filter if self.panel.is_some() => return Err(Error::Manifest("`panel` is only for a `panel` plug-in".into())),
            Kind::Filter => {}
        }
        if let Some(net) = &self.capabilities.network {
            if net.hosts.is_empty() || net.hosts.len() > MAX_NETWORK_HOSTS {
                return Err(Error::Manifest(format!("`capabilities.network.hosts` must list 1-{MAX_NETWORK_HOSTS} hosts")));
            }
            for host in &net.hosts {
                if !valid_network_host(host) {
                    return Err(Error::Manifest(format!(
                        "`capabilities.network.hosts` entry {host:?}: only loopback (127.0.0.1, [::1], localhost) is allowed"
                    )));
                }
            }
        }
        for (k, spec) in &self.params {
            if !valid_key(k) {
                return Err(Error::Manifest(format!("invalid parameter name {k:?}")));
            }
            match spec {
                ParamSpec::Number { min, max, default } if !(min.is_finite() && max.is_finite() && default.is_finite()) || min > max => {
                    return Err(Error::Manifest(format!("parameter `{k}`: bad range")));
                }
                ParamSpec::Int { min, max, .. } if min > max => return Err(Error::Manifest(format!("parameter `{k}`: bad range"))),
                ParamSpec::Choice { options, default } => {
                    if options.is_empty() || options.len() > 64 || options.iter().any(|o| o.is_empty() || o.contains(['|', '"'])) {
                        return Err(Error::Manifest(format!("parameter `{k}`: choices must be 1-64 non-empty strings without | or \"")));
                    }
                    if default.as_ref().is_some_and(|d| !options.contains(d)) {
                        return Err(Error::Manifest(format!("parameter `{k}`: default is not one of the options")));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The parameter notation the command registry and the generated dialogs use
    /// (`{"amount":0..100=50,"mode":"a|b","mono":bool=false}`).
    pub fn params_notation(&self) -> String {
        let parts: Vec<String> = self
            .params
            .iter()
            .map(|(k, spec)| match spec {
                ParamSpec::Number { min, max, default } => format!("\"{k}\":{min}..{max}={default}"),
                ParamSpec::Int { default, .. } => format!("\"{k}\":int={default}"),
                ParamSpec::Bool { default } => format!("\"{k}\":bool={default}"),
                ParamSpec::Choice { options, default } => {
                    // The dialog's default is the first option, so list the default first.
                    let mut opts = options.clone();
                    if let Some(d) = default
                        && let Some(i) = opts.iter().position(|o| o == d)
                    {
                        let d = opts.remove(i);
                        opts.insert(0, d);
                    }
                    format!("\"{k}\":\"{}\"", opts.join("|"))
                }
            })
            .collect();
        format!("{{{}}}", parts.join(","))
    }

    /// Validates user `params` against the schema: missing keys take their defaults, numbers (defaults
    /// included) are clamped into range, unknown keys are dropped; a value of the wrong type is an error.
    pub fn resolve_params(&self, params: &Value) -> Result<Map<String, Value>> {
        let given = match params {
            Value::Null => &Map::new(),
            Value::Object(m) => m,
            _ => return Err(Error::Params("plug-in parameters must be a JSON object".into())),
        };
        let mut out = Map::new();
        for (k, spec) in &self.params {
            let v = given.get(k).filter(|v| !v.is_null());
            let wrong = |what: &str| Error::Params(format!("`{k}` must be {what}"));
            let resolved = match spec {
                ParamSpec::Number { min, max, default } => match v {
                    None => json!((*default).clamp(*min, *max)),
                    Some(v) => json!(v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| wrong("a number"))?.clamp(*min, *max)),
                },
                ParamSpec::Int { min, max, default } => match v {
                    None => json!((*default).clamp(*min, *max)),
                    Some(v) => {
                        let f = v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| wrong("a number"))?;
                        json!((f.round().clamp(*min as f64, *max as f64)) as i64)
                    }
                },
                ParamSpec::Bool { default } => match v {
                    None => json!(default),
                    Some(v) => json!(v.as_bool().ok_or_else(|| wrong("true or false"))?),
                },
                ParamSpec::Choice { options, default } => match v {
                    None => json!(default.clone().or_else(|| options.first().cloned()).unwrap_or_default()),
                    Some(v) => {
                        let s = v.as_str().ok_or_else(|| wrong("a string"))?;
                        if !options.iter().any(|o| o == s) {
                            return Err(Error::Params(format!("`{k}` must be one of {}", options.join(", "))));
                        }
                        json!(s)
                    }
                },
            };
            out.insert(k.clone(), resolved);
        }
        Ok(out)
    }
}

/// Reads the `params` object keeping its key order (serde_json's `Map` would sort it).
fn ordered_params<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<(String, ParamSpec)>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<(String, ParamSpec)>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object of parameter specs")
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some((k, v)) = map.next_entry::<String, ParamSpec>()? {
                if out.len() >= MAX_PARAMS {
                    return Err(serde::de::Error::custom(format!("too many parameters (limit {MAX_PARAMS})")));
                }
                out.push((k, v));
            }
            Ok(out)
        }
    }
    d.deserialize_any(V)
}

fn params_as_object<S: serde::Serializer>(p: &[(String, ParamSpec)], s: S) -> std::result::Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut m = s.serialize_map(Some(p.len()))?;
    for (k, v) in p {
        m.serialize_entry(k, v)?;
    }
    m.end()
}

/// A declared host must be loopback: `127.x.x.x`, `[::1]` or `localhost`, optionally with a port
/// (`:8188`) or any port (`:*`). A bare IPv6 address needs its brackets, as in a URL.
pub(crate) fn valid_network_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 64 {
        return false;
    }
    let (name, port) = if let Some(rest) = host.strip_prefix('[') {
        match rest.split_once(']') {
            Some((addr, tail)) => match tail.strip_prefix(':') {
                Some(port) => (addr, Some(port)),
                None if tail.is_empty() => (addr, None),
                None => return false,
            },
            None => return false,
        }
    } else {
        match host.split_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (host, None),
        }
    };
    let port_ok = match port {
        None | Some("*") => true,
        Some(p) => p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n)),
    };
    port_ok && (name == "localhost" || name == "::1" || loopback_v4(name))
}

/// `127.0.0.0/8`: the package's own loopback, never a public address that merely starts with `127`.
fn loopback_v4(name: &str) -> bool {
    let mut parts = name.split('.');
    let (Some("127"), Some(a), Some(b), Some(c), None) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    [a, b, c].iter().all(|o| !o.is_empty() && o.bytes().all(|b| b.is_ascii_digit()) && o.parse::<u8>().is_ok())
}

/// `[A-Za-z0-9._-]{1,64}`.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The Unicode format characters (general category Cf, Unicode 15.1). Most draw nothing, and the bidi
/// controls among them (U+061C, U+200E-200F, U+202A-202E, U+2066-2069) reorder the text around them.
const FORMAT: &[(char, char)] = &[
    ('\u{AD}', '\u{AD}'),
    ('\u{600}', '\u{605}'),
    ('\u{61C}', '\u{61C}'),
    ('\u{6DD}', '\u{6DD}'),
    ('\u{70F}', '\u{70F}'),
    ('\u{890}', '\u{891}'),
    ('\u{8E2}', '\u{8E2}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
    ('\u{110BD}', '\u{110BD}'),
    ('\u{110CD}', '\u{110CD}'),
    ('\u{13430}', '\u{1343F}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0001}', '\u{E0001}'),
    ('\u{E0020}', '\u{E007F}'),
];

/// Default-ignorable characters outside Cf (fillers, variation selectors): allowed in a name, but they
/// draw nothing on their own.
const BLANK: &[(char, char)] = &[
    ('\u{34F}', '\u{34F}'),
    ('\u{115F}', '\u{1160}'),
    ('\u{17B4}', '\u{17B5}'),
    ('\u{180B}', '\u{180D}'),
    ('\u{180F}', '\u{180F}'),
    ('\u{3164}', '\u{3164}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{FFA0}', '\u{FFA0}'),
    ('\u{E0100}', '\u{E01EF}'),
];

fn in_ranges(c: char, ranges: &[(char, char)]) -> bool {
    ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// A format character a menu label must not carry: it hides in the label or reorders it. The zero-width
/// non-joiner and joiner stay allowed, since Persian, Indic scripts and emoji sequences need them and they
/// cannot reorder text.
fn hidden_format(c: char) -> bool {
    !matches!(c, '\u{200C}' | '\u{200D}') && in_ranges(c, FORMAT)
}

/// A character that draws something on its own.
fn visible(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !in_ranges(c, FORMAT) && !in_ranges(c, BLANK)
}

fn valid_key(k: &str) -> bool {
    (1..=64).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && !k.starts_with('_') && k != "id" && k != "layer"
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{"id":"org.example.tone","name":"Tone","version":"1.2","kind":"filter",
        "params":{"amount":{"type":"number","min":0,"max":100,"default":50},
                  "mode":{"type":"choice","options":["soft","hard"],"default":"hard"},
                  "mono":{"type":"bool"},"seed":{"type":"int","min":0,"max":9}},
        "overlap":2,"area":"canvas"}"#;

    const PANEL: &str = r#"{"id":"com.example.panel","name":"Example Panel","version":"2.0","kind":"panel",
        "panel":{"title":"Example","minWidth":300},
        "capabilities":{"document":["read","addLayers"],"storage":true}}"#;

    #[test]
    fn parses_and_keeps_param_order() {
        let m = Manifest::parse(FULL.as_bytes()).unwrap();
        assert_eq!(m.id, "org.example.tone");
        assert_eq!(m.params.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["amount", "mode", "mono", "seed"]);
        assert_eq!(m.overlap, 2);
        assert_eq!(m.area, Area::Canvas);
        assert_eq!(m.params_notation(), r#"{"amount":0..100=50,"mode":"hard|soft","mono":bool=false,"seed":int=0}"#);
    }

    #[test]
    fn resolves_defaults_clamps_and_rejects_wrong_types() {
        let m = Manifest::parse(FULL.as_bytes()).unwrap();
        let p = m.resolve_params(&json!({"amount": 500, "seed": 3.6, "junk": 1})).unwrap();
        assert_eq!(Value::Object(p), json!({"amount": 100.0, "mode": "hard", "mono": false, "seed": 4}));
        assert!(m.resolve_params(&json!({"amount": "lots"})).is_err());
        assert!(m.resolve_params(&json!({"mode": "medium"})).is_err());
        assert!(m.resolve_params(&json!({"mono": 1})).is_err());
        assert!(m.resolve_params(&json!([1, 2])).is_err());
        assert!(m.resolve_params(&Value::Null).is_ok());
    }

    #[test]
    fn out_of_range_defaults_are_clamped() {
        // Issue #1012: a missing value fell back to a default outside the declared range unclamped.
        let m = Manifest::parse(
            br#"{"id":"org.example.tone","name":"Tone","kind":"filter",
                "params":{"amount":{"type":"number","min":0,"max":100,"default":1000},
                          "steps":{"type":"int","min":1,"max":16,"default":1000},
                          "low":{"type":"number","min":-1,"max":1,"default":-5},
                          "count":{"type":"int","min":3,"max":9}}}"#,
        )
        .unwrap();
        let resolved = json!({"amount": 100.0, "steps": 16, "low": -1.0, "count": 3});
        assert_eq!(Value::Object(m.resolve_params(&json!({})).unwrap()), resolved);
        assert_eq!(Value::Object(m.resolve_params(&Value::Null).unwrap()), resolved);
        assert_eq!(Value::Object(m.resolve_params(&json!({"amount": null, "steps": null})).unwrap()), resolved);
        let given = m.resolve_params(&json!({"amount": 500, "steps": 500, "low": 0.5, "count": 5})).unwrap();
        assert_eq!(Value::Object(given), json!({"amount": 100.0, "steps": 16, "low": 0.5, "count": 5}));
        // In-range defaults are unchanged.
        let full = Manifest::parse(FULL.as_bytes()).unwrap();
        assert_eq!(Value::Object(full.resolve_params(&json!({})).unwrap()), json!({"amount": 50.0, "mode": "hard", "mono": false, "seed": 0}));
    }

    #[test]
    fn rejects_bad_manifests() {
        for bad in [
            "",
            "[]",
            "not json",
            r#"{"id":"","name":"x","kind":"filter"}"#,
            r#"{"id":"a b","name":"x","kind":"filter"}"#,
            r#"{"id":"a","name":"","kind":"filter"}"#,
            // `kind:"panel"` is valid only with a `panel` declaration (see the panel tests).
            r#"{"id":"a","name":"x","kind":"panel"}"#,
            r#"{"id":"a","name":"x"}"#,
            r#"{"id":"a","name":"x","kind":"filter","overlap":100000}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":{"r":{"type":"number","min":5,"max":1,"default":1}}}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":{"c":{"type":"choice","options":[]}}}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":{"__x":{"type":"bool"}}}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":[1]}"#,
        ] {
            assert!(Manifest::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(Manifest::parse(&[0xff, 0xfe]).is_err());
        assert!(Manifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
    }

    #[test]
    fn parses_a_panel_manifest() {
        let m = Manifest::parse(PANEL.as_bytes()).unwrap();
        assert_eq!(m.kind, Kind::Panel);
        let panel = m.panel.as_ref().unwrap();
        assert_eq!(panel.title, "Example");
        assert_eq!(panel.min_width, 300);
        assert_eq!(m.capabilities.document, [DocumentCap::Read, DocumentCap::AddLayers]);
        assert!(m.capabilities.storage);
        assert!(m.capabilities.network.is_none());
        // A filter carries no panel declaration and asks for nothing.
        let f = Manifest::parse(FULL.as_bytes()).unwrap();
        assert_eq!(f.panel, None);
        assert_eq!(f.capabilities, Capabilities::default());
    }

    #[test]
    fn panel_declaration_is_required_bounded_and_printable() {
        assert_eq!(panel_min_width(r#"{"id":"a","name":"x","kind":"panel","panel":{"title":"T"}}"#), Some(260));
        for bad in [
            r#"{"id":"a","name":"x","kind":"panel"}"#,
            r#"{"id":"a","name":"x","kind":"panel","panel":{"title":"T","minWidth":80}}"#,
            r#"{"id":"a","name":"x","kind":"panel","panel":{"title":"T","minWidth":1000}}"#,
            r#"{"id":"a","name":"x","kind":"panel","panel":{"title":""}}"#,
            r#"{"id":"a","name":"x","kind":"panel","panel":{"title":"T\u{200B}"}}"#,
            // `panel` means nothing on a filter.
            r#"{"id":"a","name":"x","kind":"filter","panel":{"title":"T"}}"#,
        ] {
            assert!(Manifest::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    fn panel_min_width(manifest: &str) -> Option<u32> {
        Manifest::parse(manifest.as_bytes()).ok().and_then(|m| m.panel).map(|p| p.min_width)
    }

    fn with_network_hosts(hosts: &[String]) -> Result<Manifest> {
        let json = json!({"id": "com.example.panel", "name": "Panel", "kind": "panel",
            "panel": {"title": "Panel"}, "capabilities": {"network": {"hosts": hosts}}});
        Manifest::parse(json.to_string().as_bytes())
    }

    #[test]
    fn network_capability_is_loopback_only() {
        for good in ["127.0.0.1", "127.0.0.1:8188", "127.9.9.9:1", "localhost", "localhost:*", "[::1]", "[::1]:3000"] {
            assert!(with_network_hosts(&[good.to_string()]).is_ok(), "{good}");
        }
        for bad in
            ["", "example.com", "127.0.0.1.evil.com", "10.0.0.1", "localhost:0", "localhost:70000", "localhost:x", "::1", "[::1", "127.0.0.256", "127.0.0.1:"]
        {
            assert!(with_network_hosts(&[bad.to_string()]).is_err(), "{bad}");
        }
        // Empty and oversized lists are refused too.
        assert!(with_network_hosts(&[]).is_err());
        let many: Vec<String> = (0..=MAX_NETWORK_HOSTS).map(|_| "127.0.0.1".to_string()).collect();
        assert!(with_network_hosts(&many).is_err());
    }

    #[test]
    fn unknown_capabilities_are_ignored_not_fatal() {
        // Forward compatible: a manifest from a host that grew a capability still installs.
        let m = Manifest::parse(
            br#"{"id":"com.example.panel","name":"Panel","kind":"panel","panel":{"title":"Panel"},
                "capabilities":{"document":["read"],"storage":true,"telepathy":true}}"#,
        )
        .unwrap();
        assert_eq!(m.capabilities.document, [DocumentCap::Read]);
        assert!(m.capabilities.storage);
    }

    fn with_name(name: &str) -> Result<Manifest> {
        Manifest::parse(json!({"id": "org.example.tone", "name": name, "kind": "filter"}).to_string().as_bytes())
    }

    #[test]
    fn name_must_be_printable_unicode() {
        for bad in [
            "Tone\u{202E}evil",
            "\u{2067}Tone\u{2069}",
            "Tone\u{200F}",
            "\u{200B}\u{200B}\u{200B}",
            "Tone\u{00AD}",
            "\u{FEFF}Tone",
            "\u{200D}",
            "\u{3164}\u{FE0F}",
            " \u{3000} ",
            "Tone\u{7}",
        ] {
            assert!(with_name(bad).is_err(), "{bad:?}");
        }
        // Unicode names still install, with joiners where the script or emoji needs them.
        for good in ["Tone…", "Kontrast ±", "می\u{200C}خواهم", "\u{1F469}\u{200D}\u{1F4BB} Code", "\u{2764}\u{FE0F} Love", "色调"] {
            assert_eq!(with_name(good).map(|m| m.name).ok().as_deref(), Some(good));
        }
        // The documented limit counts characters, not bytes.
        assert!(with_name(&"é".repeat(64)).is_ok());
        assert!(with_name(&"é".repeat(65)).is_err());
    }
}
