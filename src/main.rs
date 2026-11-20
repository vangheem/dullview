//! dullview: a folder of markdown files as a slide deck, rendered through templates and
//! reloaded as you edit.
//!
//! `dullview <dir>` presents a deck, `dullview --init <dir>` creates one, and
//! `dullview --import-potx <file> <dir>` turns a PowerPoint template into `_templates`.

mod appbundle;
mod deck;
mod import;
mod init;
mod library;
mod template;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use rune::{
    AppSpec, Color, Cx, El, Family, FileDialogOptions, Image, ImageCache, Key, KeyEvent, MenuItem,
    SurfaceFit, TextStyle, Tx, Waker, col, el, row, text,
};
use rune_kit::{Kit, Markdown, MarkdownStyle};

use deck::{Deck, Slide, Vars};
use library::Config;
use template::{Element, Kind};

/// Slides are designed at this size and scaled to fit the window.
const DESIGN_WIDTH: f32 = 1280.0;
const RELOAD_EVERY: Duration = Duration::from_millis(250);

static START: OnceLock<PathBuf> = OnceLock::new();

type Signature = Vec<(PathBuf, Option<SystemTime>, u64)>;

/// A recent presentation on the start screen, with what its first slide needs to draw.
struct Preview {
    path: PathBuf,
    deck: Option<Deck>,
    images: HashMap<String, Image>,
}

struct App {
    config: Config,
    /// The folder or archive that's open, as the user picked it.
    source: Option<PathBuf>,
    /// An open archive's modification time, to unpack it again when it changes.
    source_modified: Option<SystemTime>,
    /// The deck folder: `source`, or where its archive unpacked.
    root: PathBuf,
    deck: Deck,
    signature: Signature,
    current: usize,
    images: Option<ImageCache>,
    loaded: HashMap<String, Image>,
    show_notes: bool,
    /// Whether the slide overview is open.
    overview: bool,
    /// When the menu button hides again, on the runtime clock; it shows while the mouse moves.
    controls_until: Option<f64>,
    previews: Vec<Preview>,
    /// Why the last open failed, shown on the start screen.
    error: Option<String>,
    /// A window title to set on the next frame.
    title: Option<String>,
    logo: Option<Image>,
}

fn init() -> App {
    let mut s = App {
        config: Config::load(),
        source: None,
        source_modified: None,
        root: PathBuf::new(),
        deck: Deck::empty(),
        signature: Vec::new(),
        current: 0,
        images: None,
        loaded: HashMap::new(),
        show_notes: false,
        overview: false,
        controls_until: None,
        previews: Vec::new(),
        error: None,
        title: None,
        logo: None,
    };
    if let Some(path) = START.get() {
        s.open(path.clone());
    }
    s
}

impl App {
    fn assets(&self) -> Assets<'_> {
        Assets {
            images: &self.loaded,
            text_size: self.deck.text_size,
        }
    }

    /// Opens a presentation folder or archive, or says why it can't.
    fn open(&mut self, path: PathBuf) {
        match library::resolve(&path) {
            Ok(root) => {
                self.source_modified = library::is_archive(&path)
                    .then(|| library::modified(&path))
                    .flatten();
                self.root = root;
                self.deck = deck::load(&self.root);
                self.signature = deck::signature(&self.root);
                self.current = 0;
                self.overview = false;
                self.error = None;
                self.title = Some(format!("{} — dullview", library::display_name(&path)));
                self.remember(&path);
                self.source = Some(path);
                self.load_images();
            }
            Err(e) => {
                self.error = Some(e);
                self.config.forget(&path);
                self.save_config();
            }
        }
    }

    fn remember(&mut self, path: &Path) {
        self.config.remember(path);
        self.save_config();
    }

    /// Headless renders (snapshots, tests) leave the real config alone.
    fn save_config(&self) {
        if rune::tooling::headless_run().is_none() {
            self.config.save();
        }
    }

    /// Back to the start screen.
    fn close(&mut self) {
        self.source = None;
        self.source_modified = None;
        self.root = PathBuf::new();
        self.deck = Deck::empty();
        self.signature.clear();
        self.loaded.clear();
        self.overview = false;
        self.show_notes = false;
        self.title = Some("dullview".into());
        self.load_previews();
    }

    fn reload(&mut self) {
        let name = self.deck.slides.get(self.current).map(|s| s.name.clone());
        self.deck = deck::load(&self.root);
        self.current = name
            .and_then(|n| self.deck.slides.iter().position(|s| s.name == n))
            .unwrap_or(self.current)
            .min(self.deck.slides.len().saturating_sub(1));
        self.load_images();
    }

    fn load_images(&mut self) {
        let Some(cache) = &self.images else { return };
        self.loaded = load_images(cache, &self.root, &self.deck);
    }

    /// Loads the recent presentations that still exist, for the start screen.
    fn load_previews(&mut self) {
        let Some(cache) = &self.images else { return };
        self.previews = self
            .config
            .recent
            .iter()
            .filter(|r| r.path.exists())
            .take(MAX_PREVIEWS)
            .map(|r| {
                let root = library::resolve(&r.path).ok();
                let deck = root.as_deref().map(deck::load);
                let images = match (&root, &deck) {
                    (Some(root), Some(deck)) => load_images(cache, root, deck),
                    _ => HashMap::new(),
                };
                Preview {
                    path: r.path.clone(),
                    deck,
                    images,
                }
            })
            .collect();
    }

    fn go(&mut self, to: usize) {
        self.current = to.min(self.deck.slides.len().saturating_sub(1));
    }
}

const MAX_PREVIEWS: usize = 12;

fn load_images(cache: &ImageCache, root: &Path, deck: &Deck) -> HashMap<String, Image> {
    let mut out = HashMap::new();
    for rel in &deck.images {
        let Ok(bytes) = std::fs::read(root.join(rel)) else {
            continue;
        };
        let svg = rel
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        let image = if svg {
            cache.svg(bytes, 2048.0, 2.0)
        } else {
            cache.load(bytes)
        };
        let key = rel.to_string_lossy().replace('\\', "/");
        out.insert(format!("./{key}"), image.clone());
        out.insert(key, image);
    }
    out
}

fn setup(s: &mut App, waker: Waker) {
    let cache = ImageCache::new(waker);
    s.logo = Some(cache.load(include_bytes!("../assets/icon.png").to_vec()));
    s.images = Some(cache);
    s.load_images();
    if s.source.is_none() {
        s.load_previews();
    }
    // The first run on a Mac adds dullview.app to ~/Applications, once.
    if appbundle::supported()
        && rune::tooling::headless_run().is_none()
        && !s.config.app_installed
        && !appbundle::launched_from_app()
    {
        if !appbundle::installed() {
            let _ = appbundle::install();
        }
        s.config.app_installed = true;
        s.config.save();
    }
}

fn poll(s: &mut App, now: f64, tx: &mut Tx) -> bool {
    tx.wake_after(RELOAD_EVERY);
    let mut changed = false;
    if let Some(title) = s.title.take() {
        tx.set_title(title);
    }
    if s.controls_until.is_some_and(|t| now >= t) {
        s.controls_until = None;
        changed = true;
    }
    let Some(source) = s.source.clone() else {
        return changed;
    };
    // An archive that changed unpacks again, into a new folder.
    if s.source_modified.is_some() && library::modified(&source) != s.source_modified {
        s.source_modified = library::modified(&source);
        if let Ok(root) = library::resolve(&source) {
            s.root = root;
            s.signature.clear();
        }
    }
    let sig = deck::signature(&s.root);
    if sig != s.signature {
        s.signature = sig;
        s.reload();
        changed = true;
    }
    changed
}

fn open_folder(_: &mut App, tx: &mut Tx) {
    let options = FileDialogOptions {
        title: "Open Presentation Folder".into(),
        ..Default::default()
    };
    tx.open_folder(options, |s: &mut App, result, _| match result {
        Ok(Some(path)) => s.open(path),
        Ok(None) => {}
        Err(e) => s.error = Some(e.to_string()),
    });
}

fn open_archive(_: &mut App, tx: &mut Tx) {
    let options = FileDialogOptions {
        title: "Open Presentation Archive".into(),
        ..Default::default()
    }
    .filter("Presentation archive", library::ARCHIVE_EXTS);
    tx.open_file(options, |s: &mut App, result, _| match result {
        Ok(Some(path)) => s.open(path),
        Ok(None) => {}
        Err(e) => s.error = Some(e.to_string()),
    });
}

fn close_presentation(s: &mut App, _: &mut Tx) {
    s.close();
}

/// How long the menu button stays after the mouse stops.
const CONTROLS_SECS: f64 = 2.5;

fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if s.source.is_none() {
        return false;
    }
    let last = s.deck.slides.len().saturating_sub(1);
    match &ev.key {
        Key::Right | Key::Down | Key::PageDown | Key::Enter => s.go(s.current + 1),
        Key::Text(t) if t == " " && !ev.mods.cmd => s.go(s.current + 1),
        Key::Left | Key::Up | Key::PageUp | Key::Backspace => s.go(s.current.saturating_sub(1)),
        Key::Home => s.go(0),
        Key::End => s.go(last),
        Key::Command('f') => tx.toggle_full_screen(),
        Key::Text(t) if t.eq_ignore_ascii_case("n") && !ev.mods.cmd => s.show_notes = !s.show_notes,
        Key::Text(t) if t.eq_ignore_ascii_case("o") && !ev.mods.cmd => s.overview = !s.overview,
        Key::Escape if s.overview => s.overview = false,
        // Also the File menu's key equivalent; here for windows without the menu bar.
        Key::Command('w') if ev.mods.shift => s.close(),
        _ => return false,
    }
    true
}

/// How text in a flow or an area looks: the slide's style, with an area's own attributes on top.
struct Look<'a> {
    attrs: [&'a Vars; 2],
}

impl<'a> Look<'a> {
    fn get(&self, key: &str) -> Option<&'a str> {
        self.attrs
            .iter()
            .rev()
            .find_map(|a| a.get(key).map(String::as_str))
    }
    fn f32(&self, key: &str, default: f32) -> f32 {
        self.get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    fn color(&self, key: &str, default: Color) -> Color {
        self.get(key).and_then(rune::color::hex).unwrap_or(default)
    }
}

/// Family names live as long as the app; each distinct name leaks once.
fn family(name: &str) -> Family {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    let name = match names.iter().find(|n| **n == name) {
        Some(n) => *n,
        None => {
            let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
            names.push(leaked);
            leaked
        }
    };
    Family::Named(name)
}

fn markdown_style(k: &Kit, look: &Look, scale: f32, text_size: f32, width: f32) -> MarkdownStyle {
    let fg = look.color("color", hex("#e8e6e3"));
    let accent = look.color("accent", k.c.text_accent);
    let bg = look.color("background", hex("#16181d"));
    let z = scale * text_size * look.f32("scale", 1.0).clamp(0.05, 10.0);
    let mut body = TextStyle {
        size: 28.0 * z,
        line_height: 40.0 * z,
        ..k.ty.body
    };
    if let Some(f) = look.get("font") {
        body.family = family(f);
    }
    let mut heading_base = body;
    if let Some(f) = look.get("heading_font") {
        heading_base.family = family(f);
    }
    let heading = |size: f32| TextStyle {
        size: size * z,
        line_height: size * 1.2 * z,
        weight: 700.0,
        ..heading_base
    };
    MarkdownStyle::from_kit(k)
        .text(fg)
        .muted(fg.alpha(0.7))
        .link(accent)
        .link_underline(accent.alpha(0.4))
        .inline_code_bg(fg.alpha(0.1))
        .code_block(fg.alpha(0.06), fg.alpha(0.15))
        .code_block_padding(20.0 * z, 16.0 * z)
        .code_block_font(TextStyle {
            size: 22.0 * z,
            line_height: 32.0 * z,
            ..k.ty.mono
        })
        .quote(accent, 4.0 * z, 20.0 * z, fg.alpha(0.85))
        .rule(fg.alpha(0.25))
        .list(40.0 * z, 10.0 * z)
        .body_font(body)
        .heading_font(1, heading(60.0))
        .heading_font(2, heading(44.0))
        .heading_font(3, heading(34.0))
        .heading_font(4, heading(28.0))
        // Mermaid diagrams in the slide's colours, sized with its text, as wide as the box.
        .diagram(fg.alpha(0.06), fg.alpha(0.35), fg.alpha(0.6))
        .diagram_surface(bg, Color::TRANSPARENT, fg.alpha(0.04))
        .diagram_note(accent.alpha(0.15), accent.alpha(0.5))
        .diagram_font(TextStyle {
            size: 20.0 * z,
            line_height: 26.0 * z,
            ..body
        })
        .diagram_max_width(width)
        .diagram_min_scale(0.0)
}

fn hex(s: &str) -> Color {
    rune::color::hex(s).unwrap_or(Color::BLACK)
}

/// Markdown laid out in a `w`×`h` box, aligned as `look` says.
/// `selectable` lets the presenter select and copy the text; thumbnails leave it off so a
/// click on them goes to the slide.
fn text_box(
    a: &Assets,
    src: &str,
    look: &Look,
    scale: f32,
    (w, h): (f32, f32),
    selectable: bool,
) -> El {
    // Diagram labels and code colours come from the kit, so pick the light or dark one that
    // reads on this slide's background.
    let bg = look.color("background", hex("#16181d"));
    let dark = 0.2126 * bg.r + 0.7152 * bg.g + 0.0722 * bg.b < 0.18;
    let k = rune_theme_opencode::kit(dark);
    let style = markdown_style(k, look, scale, a.text_size, w);
    // Images fit the box: no wider than it, no taller than most of it.
    let image_box = (w, h * 0.75);
    let md = |src: String| {
        let images = a.images.clone();
        Markdown::new(src)
            .images(move |src| images.get(src).cloned())
            .image_frame(move |e, f| {
                let Some([iw, ih]) = f.image.size() else {
                    return e;
                };
                let (iw, ih) = (iw as f32, ih as f32);
                let fit = (image_box.0 / iw).min(image_box.1 / ih).min(scale);
                e.w(iw * fit).h(ih * fit).max_w(iw * fit)
            })
            .style(style.clone())
            .selectable(selectable)
            .build(k)
    };
    let align = look.get("align").unwrap_or("left");
    let content = match align {
        // Markdown lays its lines out from the left, so centred and right-aligned text
        // places each top-level block on its own.
        "center" | "right" => {
            let mut c = col().w(w).gap(16.0 * scale);
            c = if align == "center" {
                c.items_center()
            } else {
                c.items_end()
            };
            for (i, block) in deck::blocks(src).into_iter().enumerate() {
                c = c.child(col().max_w(w).child(md(block).key(format!("block-{i}"))));
            }
            c
        }
        _ => col().w(w).child(md(src.to_string())),
    };
    // Text that doesn't fit runs past its box, as in PowerPoint, rather than vanishing.
    let mut frame = col().size(w, h);
    frame = match look.get("valign").unwrap_or("top") {
        "center" | "middle" => frame.justify_center(),
        "bottom" => frame.justify_end(),
        _ => frame.justify_start(),
    };
    frame.child(content)
}

/// What slides draw with besides themselves: the deck's pictures and text size.
struct Assets<'a> {
    images: &'a HashMap<String, Image>,
    text_size: f32,
}

/// The slide's size at `scale`: 1280 points wide, as tall as its `aspect` (16:9 by default).
fn design(slide: Option<&Slide>) -> (f32, f32) {
    let aspect = slide
        .and_then(|s| s.style.get("aspect"))
        .and_then(|a| {
            let (w, h) = a.split_once([':', '/', 'x'])?;
            Some(w.trim().parse::<f32>().ok()? / h.trim().parse::<f32>().ok()?)
        })
        .filter(|a| a.is_finite() && *a > 0.2 && *a < 5.0)
        .unwrap_or(16.0 / 9.0);
    (DESIGN_WIDTH, DESIGN_WIDTH / aspect)
}

fn slide_view(a: &Assets, slide: &Slide, k: &Kit, scale: f32, selectable: bool) -> El {
    let (dw, dh) = design(Some(slide));
    let (w, h) = (dw * scale, dh * scale);
    let empty = Vars::new();
    let look = Look {
        attrs: [&slide.style, &empty],
    };
    let pad = look.f32("padding", 80.0) * scale;
    let mut page = col()
        .key("slide")
        .size(w, h)
        .fill(look.color("background", hex("#16181d")))
        .clip_rounded(0.0);
    if let Some(img) = look.get("background_image").and_then(|p| a.images.get(p)) {
        page = page.child(
            el().absolute(0.0, 0.0)
                .size(w, h)
                .pixels_fit(img.pixels(), SurfaceFit::Fill),
        );
    }
    let place = |e: &Element| {
        let [x, y, ew, eh] = e.rect;
        (x * w, y * h, ew * w, eh * h)
    };
    // Decoration first, then the flowing text, then placed text on top.
    for (i, e) in slide.elements.iter().enumerate() {
        let (x, y, ew, eh) = place(e);
        let radius = e
            .attrs
            .get("radius")
            .and_then(|r| r.parse::<f32>().ok())
            .unwrap_or(0.0);
        let radius = if e.attrs.get("shape").is_some_and(|s| s == "ellipse") {
            ew.min(eh) / 2.0
        } else {
            radius * scale
        };
        match &e.kind {
            Kind::Box => {
                let fill = e.attrs.get("fill").and_then(|c| rune::color::hex(c));
                if let Some(fill) = fill {
                    page = page.child(
                        el().key(format!("el-{i}"))
                            .absolute(x, y)
                            .size(ew, eh)
                            .radius(radius)
                            .fill(fill),
                    );
                }
            }
            Kind::Image(path) => {
                if let Some(img) = a.images.get(path) {
                    let fit = if e.attrs.get("fit").is_some_and(|f| f == "cover") {
                        SurfaceFit::Fill
                    } else {
                        SurfaceFit::Fit
                    };
                    page = page.child(
                        el().key(format!("el-{i}"))
                            .absolute(x, y)
                            .size(ew, eh)
                            .clip_rounded(radius)
                            .pixels_fit(img.pixels(), fit),
                    );
                }
            }
            Kind::Area(_) => {}
        }
    }
    if !slide.markdown.trim().is_empty() {
        page = page.child(el().key("flow").absolute(pad, pad).child(text_box(
            a,
            &slide.markdown,
            &look,
            scale,
            (w - 2.0 * pad, h - 2.0 * pad),
            selectable,
        )));
    }
    for (i, e) in slide.elements.iter().enumerate() {
        let Kind::Area(md) = &e.kind else { continue };
        let (x, y, ew, eh) = place(e);
        let area_look = Look {
            attrs: [&slide.style, &e.attrs],
        };
        let mut area = col().key(format!("el-{i}")).absolute(x, y).size(ew, eh);
        if let Some(fill) = e.attrs.get("fill").and_then(|c| rune::color::hex(c)) {
            area = area.fill(fill);
        }
        page = page.child(area.child(text_box(a, md, &area_look, scale, (ew, eh), selectable)));
    }
    if let Some(err) = &slide.error {
        page = page.child(
            text(format!("{}: {err}", slide.name))
                .font(k.ty.ui)
                .color(k.c.danger.icon)
                .absolute(8.0, 8.0),
        );
    }
    page
}

fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = rune_theme_opencode::kit(true).clone();
    let screen = if s.source.is_some() {
        presenting(s, cx, &k)
    } else {
        start_screen(s, cx, &k)
    };
    // Drop a presentation folder or archive anywhere to open it.
    screen
        .on_drop_files(|s: &mut App, files, _| {
            if let Some(path) = files.first() {
                s.open(path.clone());
            }
        })
        .accepts_files(|p| p.is_dir() || library::is_archive(p))
}

fn presenting(s: &App, cx: &mut Cx<App>, k: &Kit) -> El {
    let k = k.clone();
    let d = design(s.deck.slides.get(s.current));
    let stage = cx.layout_with("stage", |r| (r[2], r[3])).unwrap_or(d);
    let scale = (stage.0 / d.0).min(stage.1 / d.1).max(0.1);
    let body = match s.deck.slides.get(s.current) {
        Some(slide) => slide_view(&s.assets(), slide, &k, scale, true),
        None => col()
            .gap(8.0)
            .items_center()
            .child(text("No slides").font(k.ty.ui_medium).color(k.c.text_base))
            .child(
                text(format!("Add NNN-name.md files to {}", s.root.display()))
                    .font(k.ty.ui)
                    .color(k.c.text_muted),
            ),
    };
    let counter = el()
        .key("counter")
        .px(8.0)
        .py(3.0)
        .radius(6.0)
        .fill(Color::BLACK.alpha(0.55))
        .pin(None, None, Some(12.0), Some(10.0))
        .child(
            text(format!(
                "{} / {}",
                s.current + 1,
                s.deck.slides.len().max(1)
            ))
            .font(k.ty.ui)
            .color(Color::WHITE.alpha(0.9)),
        );
    let stage = col()
        .key("stage")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .clip_rounded(0.0)
        .center()
        .fill(Color::BLACK)
        // A right-click advances; a left click is left to selecting text.
        .on_secondary_press(|s: &mut App, _, _: &mut Tx| s.go(s.current + 1))
        .child(body)
        .child(counter);
    let mut screen = col()
        .key("screen")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .on_pointer_move_local(|s: &mut App, _, tx: &mut Tx| {
            s.controls_until = Some(tx.clock() + CONTROLS_SECS);
            tx.wake_after(Duration::from_secs_f64(CONTROLS_SECS + 0.05));
        })
        .child(stage);
    if s.overview {
        screen = screen.child(overview(
            s,
            &k,
            cx.layout_with("screen", |r| r[2]).unwrap_or(1280.0),
        ));
    }
    if s.controls_until.is_some() || s.overview {
        screen = screen.child(menu_button(&k, s.overview));
    }
    let mut root = col().w_full().h_full().child(screen);
    if s.show_notes {
        root = root.child(notes_panel(s, &k));
    }
    root
}

fn button(k: &Kit, key: &str, label: &str, primary: bool, action: fn(&mut App, &mut Tx)) -> El {
    el().key(key.to_string())
        .role("button")
        .px(16.0)
        .py(9.0)
        .radius(8.0)
        .fill(if primary {
            k.c.text_accent
        } else {
            Color::WHITE.alpha(0.08)
        })
        .border(1.0, Color::WHITE.alpha(if primary { 0.0 } else { 0.12 }))
        .on_press_tx(move |s: &mut App, tx: &mut Tx| action(s, tx))
        .child(
            text(label.to_string())
                .font(k.ty.ui_medium)
                .color(if primary {
                    hex("#0f1115")
                } else {
                    Color::WHITE
                }),
        )
}

/// Shown with no presentation open: open buttons, and recent presentations with a preview of
/// their first slide.
fn start_screen(s: &App, cx: &mut Cx<App>, k: &Kit) -> El {
    const CARD_W: f32 = 300.0;
    const GAP: f32 = 24.0;
    let width = cx.layout_with("start", |r| r[2]).unwrap_or(1280.0);
    let mut header = row().gap(16.0).items_center();
    if let Some(logo) = &s.logo {
        header = header.child(
            el().size(56.0, 56.0)
                .pixels_fit(logo.pixels(), SurfaceFit::Fit),
        );
    }
    header = header.child(
        col()
            .gap(2.0)
            .child(
                text("dullview")
                    .font(TextStyle {
                        size: 28.0,
                        line_height: 34.0,
                        weight: 700.0,
                        ..k.ty.body
                    })
                    .color(Color::WHITE),
            )
            .child(
                text("Markdown slide decks")
                    .font(k.ty.ui)
                    .color(Color::WHITE.alpha(0.55)),
            ),
    );
    let actions = row()
        .gap(10.0)
        .items_center()
        .child(button(k, "open-folder", "Open Folder…", true, open_folder))
        .child(button(
            k,
            "open-archive",
            "Open Archive…",
            false,
            open_archive,
        ))
        .child(
            text("or drop a presentation folder, .zip or .tar.gz here")
                .font(k.ty.ui)
                .color(Color::WHITE.alpha(0.45)),
        );
    let mut page = col()
        .gap(28.0)
        .w_full()
        .max_w(1100.0)
        .child(header)
        .child(actions);
    if let Some(err) = &s.error {
        page = page.child(
            text(err.clone())
                .font(k.ty.ui)
                .color(k.c.danger.icon)
                .wrap(),
        );
    }
    page = page.child(
        text(if s.previews.is_empty() {
            "No recent presentations yet"
        } else {
            "Recent"
        })
        .font(k.ty.ui_medium)
        .color(Color::WHITE.alpha(0.7)),
    );
    let cols = ((width.min(1100.0) + GAP) / (CARD_W + GAP))
        .floor()
        .max(1.0) as usize;
    let mut grid = col().gap(GAP);
    for (r, chunk) in s.previews.chunks(cols).enumerate() {
        let mut line = row().gap(GAP).items_start();
        for (c, p) in chunk.iter().enumerate() {
            let i = r * cols + c;
            let path = p.path.clone();
            let thumb = match p
                .deck
                .as_ref()
                .and_then(|d| d.slides.first().map(|sl| (d, sl)))
            {
                Some((deck, slide)) => {
                    let assets = Assets {
                        images: &p.images,
                        text_size: deck.text_size,
                    };
                    let scale = CARD_W / design(Some(slide)).0;
                    slide_view(&assets, slide, k, scale, false)
                }
                None => col()
                    .size(CARD_W, CARD_W * 9.0 / 16.0)
                    .center()
                    .fill(Color::WHITE.alpha(0.04))
                    .child(
                        text("No slides")
                            .font(k.ty.ui)
                            .color(Color::WHITE.alpha(0.4)),
                    ),
            };
            let card = col()
                .key(format!("recent-{i}"))
                .role("button")
                .label(format!("Open {}", library::display_name(&p.path)))
                .w(CARD_W)
                .gap(8.0)
                .on_press(move |s: &mut App| s.open(path.clone()))
                .child(
                    el().radius(6.0)
                        .clip_rounded(6.0)
                        .border(1.0, Color::WHITE.alpha(0.15))
                        .child(thumb),
                )
                .child(
                    text(library::display_name(&p.path))
                        .font(k.ty.ui_medium)
                        .color(Color::WHITE)
                        .truncate()
                        .max_w(CARD_W),
                )
                .child(
                    text(tilde(&p.path))
                        .font(k.ty.ui)
                        .color(Color::WHITE.alpha(0.45))
                        .truncate()
                        .max_w(CARD_W),
                );
            line = line.child(card);
        }
        grid = grid.child(line);
    }
    page = page.child(grid);
    col()
        .key("start")
        .w_full()
        .h_full()
        .fill(hex("#16181d"))
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(col().w_full().items_center().px(40.0).py(48.0).child(page))
}

/// `/Users/me/talks/q4` as `~/talks/q4`.
fn tilde(path: &Path) -> String {
    let full = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && full.starts_with(&home) => {
            format!("~{}", &full[home.len()..])
        }
        _ => full,
    }
}

fn menu_button(k: &Kit, open: bool) -> El {
    // Three bars, drawn rather than a glyph so they look the same in every font.
    let bar = || {
        el().size(14.0, 2.0)
            .radius(1.0)
            .fill(Color::WHITE.alpha(0.9))
    };
    col()
        .key("menu")
        .role("button")
        .label(if open {
            "Close slide overview"
        } else {
            "Slide overview"
        })
        .size(34.0, 28.0)
        .radius(6.0)
        .center()
        .gap(3.0)
        .fill(if open {
            k.c.text_accent
        } else {
            Color::BLACK.alpha(0.55)
        })
        .pin(Some(12.0), None, None, Some(10.0))
        .on_press(|s: &mut App| s.overview = !s.overview)
        .child(bar())
        .child(bar())
        .child(bar())
}

/// A scrollable grid of every slide; pressing one goes to it.
fn overview(s: &App, k: &Kit, width: f32) -> El {
    const THUMB_W: f32 = 260.0;
    const GAP: f32 = 20.0;
    let cols = (((width - 2.0 * GAP) + GAP) / (THUMB_W + GAP))
        .floor()
        .max(1.0) as usize;
    let mut grid = col().gap(GAP).items_center().w_full();
    for (r, chunk) in s.deck.slides.chunks(cols).enumerate() {
        let mut line = row().gap(GAP);
        for (c, slide) in chunk.iter().enumerate() {
            let i = r * cols + c;
            let scale = THUMB_W / design(Some(slide)).0;
            let current = i == s.current;
            let thumb = col()
                .key(format!("thumb-{i}"))
                .role("button")
                .label(format!("Slide {}", i + 1))
                .gap(6.0)
                .on_press(move |s: &mut App| {
                    s.go(i);
                    s.overview = false;
                })
                .child(
                    el().radius(4.0)
                        .clip_rounded(4.0)
                        .border(
                            if current { 3.0 } else { 1.0 },
                            if current {
                                k.c.text_accent
                            } else {
                                Color::WHITE.alpha(0.15)
                            },
                        )
                        .child(slide_view(&s.assets(), slide, k, scale, false)),
                )
                .child(
                    text(format!("{}  {}", i + 1, slide.name.trim_end_matches(".md")))
                        .font(k.ty.ui)
                        .color(if current {
                            Color::WHITE
                        } else {
                            Color::WHITE.alpha(0.7)
                        })
                        .truncate()
                        .max_w(THUMB_W),
                );
            line = line.child(thumb);
        }
        grid = grid.child(line);
    }
    col()
        .key("overview")
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .fill(Color::BLACK.alpha(0.94))
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(
            col()
                .w_full()
                .padding(GAP)
                .pb(56.0)
                .gap(GAP)
                .child(row().w_full().child(button(
                    k,
                    "close-presentation",
                    "← All presentations",
                    false,
                    close_presentation,
                )))
                .child(grid),
        )
}

fn notes_panel(s: &App, k: &Kit) -> El {
    let notes = s
        .deck
        .slides
        .get(s.current)
        .map_or("", |sl| sl.notes.as_str());
    let content = if notes.is_empty() {
        text("No notes for this slide")
            .font(k.ty.ui)
            .color(k.c.text_muted)
    } else {
        let body = rune::TextStyle {
            size: 20.0,
            line_height: 30.0,
            ..k.ty.body
        };
        Markdown::new(notes.to_string())
            .style(MarkdownStyle::from_kit(k).body_font(body))
            .build(k)
    };
    col()
        .key("notes")
        .w_full()
        .h(240.0)
        .px(24.0)
        .py(16.0)
        .fill(k.c.bg_base)
        .border(1.0, k.c.border_base)
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .child(content)
}

/// The theme's fonts plus every font file in the deck dullview starts with, so templates can
/// name them. They load once at startup: a font added later, or in a deck opened later, needs
/// a restart.
fn fonts(root: Option<&Path>) -> &'static [&'static [u8]] {
    let mut all: Vec<&'static [u8]> = rune_theme_opencode::FONTS.to_vec();
    for path in root.map(deck::font_files).unwrap_or_default() {
        if let Ok(bytes) = std::fs::read(&path) {
            all.push(Box::leak(bytes.into_boxed_slice()));
        }
    }
    Box::leak(all.into_boxed_slice())
}

fn usage() -> ! {
    eprintln!(
        "usage: dullview [<folder or archive>] [rune flags]\n       dullview --init <dir>\n       dullview --import-potx <file.potx> <dir> [--all-layouts]\n       dullview --templates <dir>\n       dullview install-app | uninstall-app"
    );
    std::process::exit(2);
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if appbundle::cli(&argv) {
        return;
    }
    let mut args = argv.into_iter().skip(1).peekable();
    match args.peek().map(String::as_str) {
        Some("-h" | "--help") => usage(),
        Some("--init") => {
            args.next();
            let dir = PathBuf::from(args.next().unwrap_or_else(|| usage()));
            match init::run(&dir) {
                Ok(()) => println!(
                    "Created a deck in {}\nRun: dullview {}",
                    dir.display(),
                    dir.display()
                ),
                Err(e) => {
                    eprintln!("dullview: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--import-potx") => {
            args.next();
            let rest: Vec<String> = args.collect();
            let all = rest.iter().any(|a| a == "--all-layouts");
            let mut paths = rest.iter().filter(|a| *a != "--all-layouts");
            let (Some(file), Some(dir)) = (paths.next(), paths.next()) else {
                usage()
            };
            match import::run(Path::new(file), Path::new(dir), all) {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("dullview: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Some("--templates") => {
            args.next();
            let dir = PathBuf::from(args.next().unwrap_or_else(|| usage()));
            print!("{}", deck::describe_templates(&dir));
            return;
        }
        // A deck path; anything starting with `-` is a flag for Rune, with no deck.
        Some(first) if !first.starts_with('-') => {
            let path = PathBuf::from(args.next().unwrap_or_default());
            if !path.exists() {
                eprintln!(
                    "dullview: {} doesn't exist (create a deck with `dullview --init {}`)",
                    path.display(),
                    path.display()
                );
                std::process::exit(1);
            }
            let _ = START.set(path.canonicalize().unwrap_or(path));
        }
        _ => {}
    }
    // A bare binary has no bundle icon; this gives the Dock and app switcher the logo.
    let _ = rune::set_app_icon(include_bytes!("../assets/icon.png").to_vec());
    let start_root = START.get().and_then(|p| library::resolve(p).ok());
    let spec = AppSpec {
        fonts: fonts(start_root.as_deref()),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        families: rune_theme_opencode::FAMILIES,
        min_size: Some((480.0, 270.0)),
        ..AppSpec::new(
            "dullview",
            (DESIGN_WIDTH, DESIGN_WIDTH * 9.0 / 16.0),
            init,
            view,
        )
    };
    let shortcut = |s: &str| s.parse::<rune::Shortcut>().ok();
    let item = |title: &str, keys: &str, action: fn(&mut App, &mut Tx)| {
        let item = MenuItem::new(title, action).menu("File");
        match shortcut(keys) {
            Some(k) => item.key_equivalent(k),
            None => item,
        }
    };
    rune::App::new(spec)
        .menu_item(item("Open Folder…", "mod+o", open_folder))
        .menu_item(item("Open Archive…", "mod+shift+o", open_archive))
        .menu_item(item(
            "Close Presentation",
            "mod+shift+w",
            close_presentation,
        ))
        .args(args)
        .run();
}
