//! dullview: a folder of markdown files as a slide deck, rendered through templates and
//! reloaded as you edit.
//!
//! `dullview <dir>` presents a deck, `dullview --init <dir>` creates one, and
//! `dullview --import-potx <file> <dir>` turns a PowerPoint template into `_templates`.

mod deck;
mod import;
mod init;
mod template;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use rune::{
    AppSpec, Color, Cx, El, Family, Image, ImageCache, Key, KeyEvent, SurfaceFit, TextStyle, Tx,
    Waker, col, el, row, text,
};
use rune_kit::{Kit, Markdown, MarkdownStyle};

use deck::{Deck, Slide, Vars};
use template::{Element, Kind};

/// Slides are designed at this size and scaled to fit the window.
const DESIGN_WIDTH: f32 = 1280.0;
const RELOAD_EVERY: Duration = Duration::from_millis(250);

static ROOT: OnceLock<PathBuf> = OnceLock::new();

type Signature = Vec<(PathBuf, Option<SystemTime>, u64)>;

struct App {
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
}

fn init() -> App {
    let root = ROOT.get().cloned().unwrap_or_else(|| PathBuf::from("."));
    App {
        signature: deck::signature(&root),
        deck: deck::load(&root),
        root,
        current: 0,
        images: None,
        loaded: HashMap::new(),
        show_notes: false,
        overview: false,
        controls_until: None,
    }
}

impl App {
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
        self.loaded.clear();
        for rel in &self.deck.images {
            let Ok(bytes) = std::fs::read(self.root.join(rel)) else {
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
            self.loaded.insert(format!("./{key}"), image.clone());
            self.loaded.insert(key, image);
        }
    }

    fn go(&mut self, to: usize) {
        self.current = to.min(self.deck.slides.len().saturating_sub(1));
    }
}

fn setup(s: &mut App, waker: Waker) {
    s.images = Some(ImageCache::new(waker));
    s.load_images();
}

fn poll(s: &mut App, now: f64, tx: &mut Tx) -> bool {
    tx.wake_after(RELOAD_EVERY);
    let mut changed = false;
    if s.controls_until.is_some_and(|t| now >= t) {
        s.controls_until = None;
        changed = true;
    }
    let sig = deck::signature(&s.root);
    if sig != s.signature {
        s.signature = sig;
        s.reload();
        changed = true;
    }
    changed
}

/// How long the menu button stays after the mouse stops.
const CONTROLS_SECS: f64 = 2.5;

fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
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
    s: &App,
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
    let style = markdown_style(k, look, scale, s.deck.text_size, w);
    // Images fit the box: no wider than it, no taller than most of it.
    let image_box = (w, h * 0.75);
    let md = |src: String| {
        let images = s.loaded.clone();
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

fn slide_view(s: &App, slide: &Slide, k: &Kit, scale: f32, selectable: bool) -> El {
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
    if let Some(img) = look.get("background_image").and_then(|p| s.loaded.get(p)) {
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
                if let Some(img) = s.loaded.get(path) {
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
            s,
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
        page = page.child(area.child(text_box(s, md, &area_look, scale, (ew, eh), selectable)));
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
    let d = design(s.deck.slides.get(s.current));
    let stage = cx.layout_with("stage", |r| (r[2], r[3])).unwrap_or(d);
    let scale = (stage.0 / d.0).min(stage.1 / d.1).max(0.1);
    let body = match s.deck.slides.get(s.current) {
        Some(slide) => slide_view(s, slide, &k, scale, true),
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
                        .child(slide_view(s, slide, k, scale, false)),
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
        .child(col().w_full().padding(GAP).pb(56.0).child(grid))
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

/// The theme's fonts plus every font file in the deck, so templates can name them. They load
/// once at startup: a font added later needs a restart.
fn fonts(root: &Path) -> &'static [&'static [u8]] {
    let mut all: Vec<&'static [u8]> = rune_theme_opencode::FONTS.to_vec();
    for path in deck::font_files(root) {
        if let Ok(bytes) = std::fs::read(&path) {
            all.push(Box::leak(bytes.into_boxed_slice()));
        }
    }
    Box::leak(all.into_boxed_slice())
}

fn usage() -> ! {
    eprintln!(
        "usage: dullview <dir> [rune flags]\n       dullview --init <dir>\n       dullview --import-potx <file.potx> <dir> [--all-layouts]\n       dullview --templates <dir>"
    );
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next().unwrap_or_else(|| usage());
    if first == "-h" || first == "--help" {
        usage();
    }
    if first == "--init" {
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
    if first == "--import-potx" {
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
    if first == "--templates" {
        let dir = PathBuf::from(args.next().unwrap_or_else(|| usage()));
        print!("{}", deck::describe_templates(&dir));
        return;
    }
    let root = Path::new(&first);
    if !root.is_dir() {
        eprintln!(
            "dullview: {} is not a directory (create one with `dullview --init {}`)",
            root.display(),
            first
        );
        std::process::exit(1);
    }
    let _ = ROOT.set(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()));
    // A bare binary has no bundle icon; this gives the Dock and app switcher the logo.
    let _ = rune::set_app_icon(include_bytes!("../assets/icon.png").to_vec());
    let spec = AppSpec {
        fonts: fonts(root),
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
    rune::App::new(spec).args(args).run();
}
