//! Reading a presentation directory: slides (`NNN-name.md` at the top level, in filename
//! order), templates (`_templates/<name>.md`) and front matter.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub type Vars = BTreeMap<String, String>;

pub const TEMPLATES_DIR: &str = "_templates";
pub const VARIABLES_FILE: &str = "variables.yaml";
pub const DEFAULT_TEMPLATE: &str = "default";
/// Front matter keys that style a slide rather than fill a placeholder. Templates set them;
/// a slide's own front matter overrides them.
pub const LAYOUT_KEYS: &[&str] = &[
    "background",
    "background_image",
    "color",
    "accent",
    "align",
    "valign",
    "padding",
    "scale",
    "font",
    "heading_font",
    "aspect",
];
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"];

/// Used when a deck has no `_templates/default.md`.
const BUILTIN_DEFAULT: &str = "{{body}}\n";

#[derive(Clone, Debug, PartialEq)]
pub struct Slide {
    /// The file name, e.g. `001-intro.md`.
    pub name: String,
    /// The template's flowing markdown with every placeholder filled in.
    pub markdown: String,
    /// The template's placed areas, images and boxes.
    pub elements: Vec<crate::template::Element>,
    /// Everything after the body's first `---` line: for the presenter, never drawn on the slide.
    pub notes: String,
    /// Layout and colours: the template's front matter, overridden by the slide's.
    pub style: Vars,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Deck {
    pub slides: Vec<Slide>,
    /// Image files in the deck, relative to its root (`assets/logo.png`).
    pub images: Vec<PathBuf>,
    /// Multiplies every slide's text size (`text_size` in `variables.yaml`).
    pub text_size: f32,
}

impl Deck {
    /// No presentation open.
    pub fn empty() -> Deck {
        Deck {
            slides: Vec::new(),
            images: Vec::new(),
            text_size: 1.0,
        }
    }
}

/// Splits `---` fenced `key: value` front matter from the rest of `src`.
pub fn front_matter(src: &str) -> (Vars, &str) {
    let mut vars = Vars::new();
    let rest = src.strip_prefix('\u{feff}').unwrap_or(src);
    let Some(after) = rest
        .strip_prefix("---")
        .filter(|r| r.starts_with('\n') || r.starts_with("\r\n"))
    else {
        return (vars, rest);
    };
    let mut offset = rest.len() - after.len();
    for line in after.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim();
        if trimmed == "---" {
            return (vars, &rest[offset..]);
        }
        parse_line(trimmed, &mut vars);
    }
    // No closing fence: treat the whole file as content.
    (Vars::new(), rest)
}

/// A `key: value` line; blank lines and `#` comments are skipped.
fn parse_line(line: &str, vars: &mut Vars) {
    if line.is_empty() || line.starts_with('#') {
        return;
    }
    if let Some((k, v)) = line.split_once(':') {
        vars.insert(k.trim().to_string(), unquote(v.trim()).to_string());
    }
}

/// The deck's `variables.yaml`: flat `key: value` pairs every slide can use as placeholders,
/// plus settings such as `text_size`.
pub fn variables(root: &Path) -> Vars {
    let mut vars = Vars::new();
    if let Ok(src) = fs::read_to_string(root.join(VARIABLES_FILE)) {
        for line in src.lines() {
            parse_line(line.trim(), &mut vars);
        }
    }
    vars
}

/// `text_size` as a multiplier: `110` or `110%` is 1.1. Defaults to 1.
pub fn text_size(vars: &Vars) -> f32 {
    vars.get("text_size")
        .and_then(|v| v.trim().trim_end_matches('%').trim().parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .map_or(1.0, |v| (v / 100.0).clamp(0.25, 4.0))
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|v| v.strip_suffix(q)) {
            return inner;
        }
    }
    v
}

/// Replaces `{{ name }}` with `vars[name]`. A line whose placeholders all come out empty is
/// dropped, so an unset `## {{subtitle}}` doesn't leave an empty heading behind.
pub fn fill(template: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(template.len());
    for line in template.split_inclusive('\n') {
        let mut filled = String::with_capacity(line.len());
        let (mut any, mut all_empty) = (false, true);
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            let Some(len) = rest[start + 2..].find("}}") else {
                break;
            };
            filled.push_str(&rest[..start]);
            let key = rest[start + 2..start + 2 + len].trim();
            let value = vars.get(key).map(String::as_str).unwrap_or("");
            any = true;
            all_empty &= value.trim().is_empty();
            filled.push_str(value);
            rest = &rest[start + 2 + len + 2..];
        }
        filled.push_str(rest);
        if !(any && all_empty) {
            out.push_str(&filled);
        }
    }
    out
}

/// Splits markdown into its top-level blocks at blank lines outside code fences.
pub fn blocks(src: &str) -> Vec<String> {
    let (mut out, mut cur, mut fence) = (Vec::new(), String::new(), false);
    for line in src.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fence = !fence;
        }
        if !fence && line.trim().is_empty() {
            if !cur.trim().is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            continue;
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Each template in the deck with the placeholders it uses, for `dullview --templates`.
pub fn describe_templates(root: &Path) -> String {
    let dir = root.join(TEMPLATES_DIR);
    let mut names: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "md"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        return format!("No templates in {}\n", dir.display());
    }
    let mut rows = Vec::new();
    for path in names {
        let name = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let src = fs::read_to_string(&path).unwrap_or_default();
        let (_, body) = front_matter(&src);
        let mut vars: Vec<String> = Vec::new();
        let mut rest = body;
        while let Some(start) = rest.find("{{") {
            let Some(len) = rest[start + 2..].find("}}") else {
                break;
            };
            let key = rest[start + 2..start + 2 + len].trim().to_string();
            if !matches!(key.as_str(), "slide" | "total") && !vars.contains(&key) {
                vars.push(key);
            }
            rest = &rest[start + 2 + len + 2..];
        }
        let vars = if vars.is_empty() {
            "(no placeholders)".to_string()
        } else {
            vars.join(", ")
        };
        rows.push((name, vars));
    }
    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(n, v)| format!("{n:width$}  {v}\n"))
        .collect()
}

/// `.ttf`, `.otf` and `.ttc` files anywhere in the deck.
pub fn font_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, 0, &mut |p| {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if matches!(ext.as_deref(), Some("ttf" | "otf" | "ttc")) {
            out.push(p.to_path_buf());
        }
    });
    out.sort();
    out
}

fn is_slide(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "md")
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| !n.starts_with('.') && !n.eq_ignore_ascii_case("README.md"))
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Every file whose change should reload the deck, with its modification time and size.
pub fn signature(root: &Path) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    let mut out = Vec::new();
    walk(root, 0, &mut |p| {
        if is_slide(p) || is_image(p) || p.file_name().is_some_and(|n| n == VARIABLES_FILE) {
            let meta = fs::metadata(p).ok();
            out.push((
                p.to_path_buf(),
                meta.as_ref().and_then(|m| m.modified().ok()),
                meta.map_or(0, |m| m.len()),
            ));
        }
    });
    out.sort();
    out
}

fn walk(dir: &Path, depth: usize, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if depth < 3 {
                walk(&path, depth + 1, f);
            }
        } else {
            f(&path);
        }
    }
}

pub fn load(root: &Path) -> Deck {
    let mut files: Vec<PathBuf> = fs::read_dir(root)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_slide(p))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let total = files.len();
    let deck_vars = variables(root);
    let slides = files
        .iter()
        .enumerate()
        .map(|(i, path)| load_slide(root, path, i + 1, total, &deck_vars))
        .collect();
    let mut images = Vec::new();
    walk(root, 0, &mut |p| {
        if is_image(p)
            && let Ok(rel) = p.strip_prefix(root)
        {
            images.push(rel.to_path_buf());
        }
    });
    images.sort();
    Deck {
        slides,
        images,
        text_size: text_size(&deck_vars),
    }
}

fn load_slide(root: &Path, path: &Path, number: usize, total: usize, deck_vars: &Vars) -> Slide {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return Slide {
                name,
                markdown: String::new(),
                elements: Vec::new(),
                notes: String::new(),
                style: Vars::new(),
                error: Some(e.to_string()),
            };
        }
    };
    let (slide_vars, body) = front_matter(&src);
    // The slide's own front matter wins over the deck's variables.
    let mut vars = deck_vars.clone();
    vars.extend(slide_vars);
    let (body, notes) = split_notes(body);
    let template_name = vars
        .get("template")
        .cloned()
        .unwrap_or_else(|| DEFAULT_TEMPLATE.into());
    let template_path = root.join(TEMPLATES_DIR).join(format!("{template_name}.md"));
    let (template_src, error) = match fs::read_to_string(&template_path) {
        Ok(t) => (t, None),
        Err(_) if template_name == DEFAULT_TEMPLATE => (BUILTIN_DEFAULT.to_string(), None),
        Err(_) => (
            BUILTIN_DEFAULT.to_string(),
            Some(format!("template not found: _templates/{template_name}.md")),
        ),
    };
    let (mut style, template_body) = front_matter(&template_src);
    let (body, sections) = split_sections(body);
    vars.extend(sections);
    vars.insert("body".into(), body.trim().to_string());
    vars.insert("slide".into(), number.to_string());
    vars.insert("total".into(), total.to_string());
    // Parse the template before filling it, so slide text can't open or close its areas.
    let mut layout = crate::template::parse(template_body, TEMPLATES_DIR);
    layout.flow = fill(&layout.flow, &vars);
    for e in &mut layout.elements {
        if let crate::template::Kind::Area(md) = &mut e.kind {
            *md = fill(md, &vars);
        }
    }
    // A template's background image is relative to the template; a slide's to the deck.
    if let Some(bg) = style.get_mut("background_image") {
        *bg = crate::template::join(TEMPLATES_DIR, bg);
    }
    // Slides can override the template's layout keys directly.
    for key in LAYOUT_KEYS {
        if let Some(v) = vars.get(*key) {
            style.insert((*key).to_string(), v.clone());
        }
    }
    Slide {
        name,
        markdown: layout.flow,
        elements: layout.elements,
        notes,
        style,
        error,
    }
}

/// Pulls `::: name` … `:::` sections out of a slide body: each fills the template's
/// `{{name}}` placeholder with markdown (a second column, say). The rest is the body.
pub fn split_sections(body: &str) -> (String, Vars) {
    let (mut rest, mut sections) = (String::new(), Vars::new());
    let mut open: Option<(String, String)> = None;
    let mut fence = false;
    for line in body.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if !fence && let Some(name) = t.strip_prefix(":::") {
            let name = name.trim();
            match (&mut open, name.is_empty()) {
                (Some(_), true) => {
                    let (k, v) = open.take().unwrap_or_default();
                    sections.insert(k, v.trim().to_string());
                    continue;
                }
                (None, false) if !name.contains(char::is_whitespace) => {
                    open = Some((name.to_string(), String::new()));
                    continue;
                }
                _ => {}
            }
        }
        match &mut open {
            Some((_, v)) => v.push_str(line),
            None => rest.push_str(line),
        }
    }
    if let Some((k, v)) = open {
        sections.insert(k, v.trim().to_string());
    }
    (rest, sections)
}

/// Splits a slide body at its first `---` line outside a code fence: the slide's content above,
/// the author's notes below.
pub fn split_notes(body: &str) -> (&str, String) {
    let mut fence = false;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if !fence && t == "---" {
            return (
                &body[..offset],
                body[offset + line.len()..].trim().to_string(),
            );
        }
        offset += line.len();
    }
    (body, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_and_fill() {
        let (vars, body) =
            front_matter("---\ntemplate: title\ntitle: \"Hello: world\"\n---\nSome *body*\n");
        assert_eq!(vars["template"], "title");
        assert_eq!(vars["title"], "Hello: world");
        assert_eq!(body, "Some *body*\n");

        let mut v = vars.clone();
        v.insert("body".into(), "Some *body*".into());
        let out = fill("# {{ title }}\n## {{subtitle}}\n\n{{body}}\n", &v);
        assert_eq!(out, "# Hello: world\n\nSome *body*\n");
    }

    #[test]
    fn notes_after_rule() {
        let (body, notes) = split_notes("Shown\n```\n---\n```\n---\nSay this out loud\n");
        assert_eq!(body, "Shown\n```\n---\n```\n");
        assert_eq!(notes, "Say this out loud");
        assert_eq!(split_notes("No notes\n"), ("No notes\n", String::new()));
    }

    #[test]
    fn sections_fill_placeholders() {
        let (body, sections) = split_sections("Left\n\n::: body2\nRight\n- item\n:::\nMore left\n");
        assert_eq!(body, "Left\n\nMore left\n");
        assert_eq!(sections["body2"], "Right\n- item");
    }

    #[test]
    fn text_size_percent() {
        let vars = |v: &str| Vars::from([("text_size".to_string(), v.to_string())]);
        assert_eq!(text_size(&Vars::new()), 1.0);
        assert_eq!(text_size(&vars("110")), 1.1);
        assert_eq!(text_size(&vars("90%")), 0.9);
        assert_eq!(text_size(&vars("big")), 1.0);
    }

    #[test]
    fn no_front_matter() {
        let (vars, body) = front_matter("# Just markdown\n");
        assert!(vars.is_empty());
        assert_eq!(body, "# Just markdown\n");
    }
}
