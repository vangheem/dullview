//! A filled-in template's body: free-flowing markdown plus placed elements.
//!
//! ```text
//! ::: area x=0.06 y=0.08 w=0.88 h=0.15 valign=bottom scale=1.4
//! # {{title}}
//! :::
//! ::: image x=0.85 y=0.9 w=0.1 h=0.06 src=media/logo.png
//! ::: box x=0 y=0.95 w=1 h=0.05 fill=#00206b
//! ```
//!
//! Positions and sizes are fractions of the slide. `area` holds markdown up to the closing
//! `:::`; `image` and `box` are single lines. Everything outside them flows in the padded
//! content column as before.

use crate::deck::Vars;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// Markdown placed in a box.
    Area(String),
    /// A picture fitted into a box (path relative to the deck root).
    Image(String),
    /// A filled rectangle.
    Box,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub kind: Kind,
    /// Left, top, width and height as fractions of the slide.
    pub rect: [f32; 4],
    /// The rest of the attributes: `align`, `valign`, `scale`, `color`, `fill`, `font`, `radius`, …
    pub attrs: Vars,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub flow: String,
    pub elements: Vec<Element>,
}

/// `key=value key="quoted value"` pairs.
pub fn attrs(src: &str) -> Vars {
    let mut out = Vars::new();
    let mut rest = src.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_string();
        rest = &rest[eq + 1..];
        let value;
        if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            value = r[..end].to_string();
            rest = r.get(end + 1..).unwrap_or("");
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            value = rest[..end].to_string();
            rest = &rest[end..];
        }
        out.insert(key, value);
        rest = rest.trim_start();
    }
    out
}

fn rect(a: &Vars) -> [f32; 4] {
    let n = |k: &str, d: f32| a.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    [n("x", 0.0), n("y", 0.0), n("w", 1.0), n("h", 1.0)]
}

/// Splits a filled template body into flowing markdown and placed elements. `base` turns an
/// image `src` (relative to the template file) into a path relative to the deck root.
pub fn parse(src: &str, base: &str) -> Layout {
    let mut layout = Layout::default();
    let mut open: Option<(Vars, String)> = None;
    let mut fence = false;
    for line in src.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if !fence && let Some(directive) = t.strip_prefix(":::") {
            let directive = directive.trim();
            if directive.is_empty() {
                if let Some((a, md)) = open.take() {
                    layout.elements.push(Element {
                        kind: Kind::Area(md),
                        rect: rect(&a),
                        attrs: a,
                    });
                }
                continue;
            }
            let (kind, rest) = directive
                .split_once(char::is_whitespace)
                .unwrap_or((directive, ""));
            let a = attrs(rest);
            match kind {
                "area" => open = Some((a, String::new())),
                "image" => {
                    let path = a.get("src").map(|s| join(base, s)).unwrap_or_default();
                    layout.elements.push(Element {
                        kind: Kind::Image(path),
                        rect: rect(&a),
                        attrs: a,
                    });
                }
                "box" => layout.elements.push(Element {
                    kind: Kind::Box,
                    rect: rect(&a),
                    attrs: a,
                }),
                _ => layout.flow.push_str(line),
            }
            continue;
        }
        match &mut open {
            Some((_, md)) => md.push_str(line),
            None => layout.flow.push_str(line),
        }
    }
    if let Some((a, md)) = open {
        layout.elements.push(Element {
            kind: Kind::Area(md),
            rect: rect(&a),
            attrs: a,
        });
    }
    layout
}

/// `base/rel`, normalised, for paths written relative to a template.
pub fn join(base: &str, rel: &str) -> String {
    if rel.starts_with('/') || base.is_empty() {
        return rel.trim_start_matches("./").to_string();
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_images_and_flow() {
        let src = "Flow text\n::: area x=0.1 y=0.2 w=0.5 h=0.3 font=\"Open Sans\"\n# Title\n:::\n::: image x=0 y=0 w=1 h=1 src=media/bg.png\n::: box x=0 y=0.9 w=1 h=0.1 fill=#ff0000\n";
        let l = parse(src, "_templates");
        assert_eq!(l.flow, "Flow text\n");
        assert_eq!(l.elements.len(), 3);
        assert_eq!(l.elements[0].kind, Kind::Area("# Title\n".into()));
        assert_eq!(l.elements[0].rect, [0.1, 0.2, 0.5, 0.3]);
        assert_eq!(l.elements[0].attrs["font"], "Open Sans");
        assert_eq!(
            l.elements[1].kind,
            Kind::Image("_templates/media/bg.png".into())
        );
        assert_eq!(l.elements[2].attrs["fill"], "#ff0000");
    }

    #[test]
    fn join_paths() {
        assert_eq!(join("_templates", "../assets/a.png"), "assets/a.png");
        assert_eq!(
            join("_templates", "./media/a.png"),
            "_templates/media/a.png"
        );
    }
}
