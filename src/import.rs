//! `dullview --import-potx <file> <dir>`: turns a PowerPoint template's slide layouts into
//! dullview templates in `<dir>/_templates`.
//!
//! Each layout becomes `_templates/<layout-name>.md`. Its placeholders become placed areas
//! (`{{title}}`, `{{subtitle}}`, `{{body}}`, `{{body2}}`, …), its pictures image elements
//! (copied to `_templates/media`), its filled shapes boxes, and the theme's colours and fonts
//! the template's front matter. `.pptx` files work too. What dullview can't draw (EMF/WMF
//! pictures, charts, gradients, lines, OLE objects) is skipped and counted in the report.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::path::Path;

use roxmltree::{Document, Node};

use crate::deck::{DEFAULT_TEMPLATE, TEMPLATES_DIR};
use crate::template::join;

const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
/// Base sizes dullview's markdown uses at 1280 points wide: body text and `#` headings.
const BODY_PT: f32 = 28.0;
const H1_PT: f32 = 60.0;
const DESIGN_WIDTH: f32 = 1280.0;
/// Stands in for a slide number field until the text is escaped.
const SLIDE_NUMBER: &str = "\u{0}slide\u{0}";
const MEDIA_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp"];

struct Package {
    files: HashMap<String, Vec<u8>>,
}

impl Package {
    fn open(path: &Path) -> Result<Package, String> {
        let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| format!("{}: not a PowerPoint file ({e})", path.display()))?;
        let mut files = HashMap::new();
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
            if f.is_dir() {
                continue;
            }
            let mut bytes = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            files.insert(f.name().trim_start_matches('/').to_string(), bytes);
        }
        Ok(Package { files })
    }

    fn text(&self, part: &str) -> Option<String> {
        self.files
            .get(part)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// A part's relationships: id to (type suffix, absolute part name).
    fn rels(&self, part: &str) -> HashMap<String, (String, String)> {
        let (dir, name) = part.rsplit_once('/').unwrap_or(("", part));
        let rels_part = if dir.is_empty() {
            format!("_rels/{name}.rels")
        } else {
            format!("{dir}/_rels/{name}.rels")
        };
        let mut out = HashMap::new();
        let Some(xml) = self.text(&rels_part) else {
            return out;
        };
        let Ok(doc) = Document::parse(&xml) else {
            return out;
        };
        for r in doc.descendants().filter(|n| n.has_tag_name("Relationship")) {
            if r.attribute("TargetMode") == Some("External") {
                continue;
            }
            let (Some(id), Some(target), Some(kind)) = (
                r.attribute("Id"),
                r.attribute("Target"),
                r.attribute("Type"),
            ) else {
                continue;
            };
            let kind = kind.rsplit('/').next().unwrap_or("").to_string();
            out.insert(id.to_string(), (kind, join(dir, target)));
        }
        out
    }
}

fn a<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name((NS_A, name)))
}
fn p<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.has_tag_name((NS_P, name)))
}
fn r_attr(n: Node, name: &str) -> Option<String> {
    n.attribute((NS_R, name)).map(str::to_string)
}
fn num(n: Node, name: &str) -> Option<f64> {
    n.attribute(name)?.parse().ok()
}

#[derive(Default, Clone)]
struct Theme {
    colors: HashMap<String, [f64; 3]>,
    major: Option<String>,
    minor: Option<String>,
    /// The theme part, for resolving pictures in its background fills.
    part: String,
}

/// Turns `bg1`/`tx1`/… into theme colour names.
#[derive(Clone)]
struct ClrMap(HashMap<String, String>);

impl ClrMap {
    fn from(node: Option<Node>) -> ClrMap {
        let mut m: HashMap<String, String> = [
            ("bg1", "lt1"),
            ("tx1", "dk1"),
            ("bg2", "lt2"),
            ("tx2", "dk2"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        if let Some(n) = node {
            for attr in n.attributes() {
                m.insert(attr.name().to_string(), attr.value().to_string());
            }
        }
        ClrMap(m)
    }
}

struct Ctx<'t> {
    theme: &'t Theme,
    map: &'t ClrMap,
}

impl Ctx<'_> {
    /// The colour of a colour element (`a:srgbClr`, `a:schemeClr`, …), with its modifiers.
    /// `ph` stands in for `phClr`.
    fn color(&self, c: Node, ph: Option<[f64; 3]>) -> Option<[f64; 3]> {
        let val = c.attribute("val").unwrap_or("");
        let base = match c.tag_name().name() {
            "srgbClr" => parse_hex(val)?,
            "sysClr" => parse_hex(c.attribute("lastClr").unwrap_or(match val {
                "window" => "FFFFFF",
                _ => "000000",
            }))?,
            "schemeClr" if val == "phClr" => ph?,
            "schemeClr" => {
                let name = self.map.0.get(val).map(String::as_str).unwrap_or(val);
                *self.theme.colors.get(name)?
            }
            "prstClr" => match val {
                "white" => [1.0; 3],
                _ => [0.0; 3],
            },
            _ => return None,
        };
        Some(modify(base, c))
    }

    /// The colour of a fill-bearing element's `a:solidFill`, or the first stop of its `a:gradFill`.
    fn fill(&self, n: Node, ph: Option<[f64; 3]>) -> Option<[f64; 3]> {
        let f = a(n, "solidFill").or_else(|| a(n, "gradFill"))?;
        self.fill_color(f, ph)
    }

    /// The colour of an `a:solidFill`, or of an `a:gradFill`'s first stop.
    fn fill_color(&self, f: Node, ph: Option<[f64; 3]>) -> Option<[f64; 3]> {
        let holder = match f.tag_name().name() {
            "solidFill" => f,
            "gradFill" => f.descendants().find(|c| c.has_tag_name((NS_A, "gs")))?,
            _ => return None,
        };
        holder
            .children()
            .filter(|c| c.is_element())
            .find_map(|c| self.color(c, ph))
    }
}

fn parse_hex(s: &str) -> Option<[f64; 3]> {
    if s.len() != 6 {
        return None;
    }
    let c = |i: usize| {
        u8::from_str_radix(&s[i..i + 2], 16)
            .ok()
            .map(|v| v as f64 / 255.0)
    };
    Some([c(0)?, c(2)?, c(4)?])
}

fn to_hex(c: [f64; 3]) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

/// DrawingML's lumMod/lumOff (in HSL), tint and shade.
fn modify(mut c: [f64; 3], node: Node) -> [f64; 3] {
    for m in node.children().filter(|n| n.is_element()) {
        let v = num(m, "val").unwrap_or(100000.0) / 100000.0;
        match m.tag_name().name() {
            "lumMod" | "lumOff" => {
                let (h, s, mut l) = rgb_to_hsl(c);
                if m.tag_name().name() == "lumMod" {
                    l *= v
                } else {
                    l += v
                }
                c = hsl_to_rgb(h, s, l.clamp(0.0, 1.0));
            }
            "tint" => c = c.map(|x| x + (1.0 - x) * (1.0 - v)),
            "shade" => c = c.map(|x| x * v),
            _ => {}
        }
    }
    c
}

fn rgb_to_hsl([r, g, b]: [f64; 3]) -> (f64, f64, f64) {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [f64; 3] {
    if s == 0.0 {
        return [l; 3];
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |mut t: f64| {
        if t < 0.0 {
            t += 1.0
        }
        if t > 1.0 {
            t -= 1.0
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

fn load_theme(pkg: &Package, part: &str) -> Theme {
    let mut t = Theme {
        part: part.to_string(),
        ..Theme::default()
    };
    let Some(xml) = pkg.text(part) else { return t };
    let Ok(doc) = Document::parse(&xml) else {
        return t;
    };
    if let Some(scheme) = doc
        .descendants()
        .find(|n| n.has_tag_name((NS_A, "clrScheme")))
    {
        let ctx_theme = Theme::default();
        let map = ClrMap::from(None);
        let ctx = Ctx {
            theme: &ctx_theme,
            map: &map,
        };
        for entry in scheme.children().filter(|n| n.is_element()) {
            if let Some(c) = entry
                .children()
                .filter(|n| n.is_element())
                .find_map(|c| ctx.color(c, None))
            {
                t.colors.insert(entry.tag_name().name().to_string(), c);
            }
        }
    }
    let font = |which: &str| {
        let f = doc.descendants().find(|n| n.has_tag_name((NS_A, which)))?;
        let face = a(f, "latin")?.attribute("typeface")?;
        (!face.is_empty()).then(|| face.to_string())
    };
    t.major = font("majorFont");
    t.minor = font("minorFont");
    t
}

/// A rectangle in EMUs.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Maps a group's child coordinates to the slide's.
#[derive(Clone, Copy)]
struct Transform {
    off: (f64, f64),
    scale: (f64, f64),
    ch_off: (f64, f64),
}

impl Transform {
    const IDENTITY: Transform = Transform {
        off: (0.0, 0.0),
        scale: (1.0, 1.0),
        ch_off: (0.0, 0.0),
    };
    fn apply(&self, r: Rect) -> Rect {
        Rect {
            x: self.off.0 + (r.x - self.ch_off.0) * self.scale.0,
            y: self.off.1 + (r.y - self.ch_off.1) * self.scale.1,
            w: r.w * self.scale.0,
            h: r.h * self.scale.1,
        }
    }
}

fn xfrm_rect(xfrm: Node) -> Option<Rect> {
    let off = a(xfrm, "off")?;
    let ext = a(xfrm, "ext")?;
    Some(Rect {
        x: num(off, "x")?,
        y: num(off, "y")?,
        w: num(ext, "cx")?,
        h: num(ext, "cy")?,
    })
}

fn sp_rect(sp: Node) -> Option<Rect> {
    let sp_pr = p(sp, "spPr").or_else(|| p(sp, "grpSpPr"))?;
    xfrm_rect(a(sp_pr, "xfrm")?)
}

/// What a placeholder is and where its template looks for inherited styling.
#[derive(Clone, Debug, PartialEq)]
struct PhKey {
    kind: String,
    idx: Option<String>,
}

fn ph_of(sp: Node) -> Option<PhKey> {
    let nv = sp
        .children()
        .find(|c| c.tag_name().name().starts_with("nv"))?;
    let ph = p(p(nv, "nvPr")?, "ph")?;
    Some(PhKey {
        kind: ph.attribute("type").unwrap_or("body").to_string(),
        idx: ph.attribute("idx").map(str::to_string),
    })
}

fn is_hidden(sp: Node) -> bool {
    sp.children()
        .find(|c| c.tag_name().name().starts_with("nv"))
        .and_then(|nv| nv.children().find(|c| c.tag_name().name() == "cNvPr"))
        .is_some_and(|c| c.attribute("hidden") == Some("1"))
}

/// The text a shape shows, one line per paragraph.
fn shape_text(sp: Node) -> String {
    let Some(body) = p(sp, "txBody") else {
        return String::new();
    };
    body.children()
        .filter(|n| n.has_tag_name((NS_A, "p")))
        .map(|para| {
            let mut line = String::new();
            for run in para.children() {
                if run.has_tag_name((NS_A, "fld")) {
                    // Slide numbers become the placeholder; dates and other fields are dropped.
                    if run.attribute("type") == Some("slidenum") {
                        line.push_str(SLIDE_NUMBER);
                    }
                } else if run.has_tag_name((NS_A, "r")) {
                    line.extend(
                        run.descendants()
                            .filter(|n| n.has_tag_name((NS_A, "t")))
                            .filter_map(|t| t.text()),
                    );
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Text styling that placeholders inherit: layout, then master placeholder, then master text styles.
#[derive(Default, Clone, Debug)]
struct TextLook {
    size_pt: Option<f64>,
    color: Option<[f64; 3]>,
    align: Option<String>,
    anchor: Option<String>,
    font: Option<String>,
}

impl TextLook {
    fn or(self, other: TextLook) -> TextLook {
        TextLook {
            size_pt: self.size_pt.or(other.size_pt),
            color: self.color.or(other.color),
            align: self.align.or(other.align),
            anchor: self.anchor.or(other.anchor),
            font: self.font.or(other.font),
        }
    }
}

/// The level-1 look a list style (`a:lstStyle`, `p:titleStyle`, …) gives, and a shape body's anchor.
fn level1_look(ctx: &Ctx, list: Option<Node>, body_pr: Option<Node>) -> TextLook {
    let mut look = TextLook::default();
    if let Some(l1) = list.and_then(|l| a(l, "lvl1pPr")) {
        look.align = l1.attribute("algn").map(str::to_string);
        if let Some(rpr) = a(l1, "defRPr") {
            look.size_pt = num(rpr, "sz").map(|s| s / 100.0);
            look.color = ctx.fill(rpr, None);
            look.font = a(rpr, "latin")
                .and_then(|l| l.attribute("typeface"))
                .map(str::to_string);
        }
    }
    look.anchor = body_pr
        .and_then(|b| b.attribute("anchor"))
        .map(str::to_string);
    look
}

/// The first run's look in a shape that has its own text.
fn run_look(ctx: &Ctx, sp: Node) -> TextLook {
    let mut look = TextLook::default();
    let Some(body) = p(sp, "txBody") else {
        return look;
    };
    look.anchor = a(body, "bodyPr")
        .and_then(|b| b.attribute("anchor"))
        .map(str::to_string);
    if let Some(ppr) = body.descendants().find(|n| n.has_tag_name((NS_A, "pPr"))) {
        look.align = ppr.attribute("algn").map(str::to_string);
    }
    if let Some(rpr) = body
        .descendants()
        .find(|n| n.has_tag_name((NS_A, "rPr")) || n.has_tag_name((NS_A, "endParaRPr")))
    {
        look.size_pt = num(rpr, "sz").map(|s| s / 100.0);
        look.color = ctx.fill(rpr, None);
        look.font = a(rpr, "latin")
            .and_then(|l| l.attribute("typeface"))
            .map(str::to_string);
    }
    look.or(level1_look(ctx, a(body, "lstStyle"), None))
}

struct Master {
    part: String,
    xml: String,
    theme: Theme,
    map: ClrMap,
}

/// One imported layout, before it's written out.
struct Out {
    name: String,
    front: BTreeMap<String, String>,
    comments: Vec<String>,
    lines: Vec<String>,
    has_body: bool,
    /// The master's plain title-and-content layout: the best `default.md`.
    is_content: bool,
}

#[derive(Default)]
struct Report {
    templates: Vec<String>,
    media: usize,
    skipped: BTreeMap<String, usize>,
}

impl Report {
    fn skip(&mut self, what: &str) {
        *self.skipped.entry(what.to_string()).or_default() += 1;
    }
}

struct Importer<'p> {
    pkg: &'p Package,
    dir: std::path::PathBuf,
    slide: (f64, f64),
    report: Report,
    copied: HashMap<String, String>,
}

impl Importer<'_> {
    /// Copies a picture part into `_templates/media` and returns its path relative to the
    /// template, or `None` for formats dullview can't draw.
    fn media(&mut self, part: &str) -> Option<String> {
        if let Some(done) = self.copied.get(part) {
            return Some(done.clone());
        }
        let name = part.rsplit('/').next()?.to_string();
        let ext = name.rsplit('.').next()?.to_ascii_lowercase();
        if !MEDIA_EXTS.contains(&ext.as_str()) {
            self.report.skip(&format!("{ext} pictures"));
            return None;
        }
        let bytes = self.pkg.files.get(part)?;
        let media_dir = self.dir.join(TEMPLATES_DIR).join("media");
        fs::create_dir_all(&media_dir).ok()?;
        fs::write(media_dir.join(&name), bytes).ok()?;
        self.report.media += 1;
        let rel = format!("media/{name}");
        self.copied.insert(part.to_string(), rel.clone());
        Some(rel)
    }

    /// The picture a `a:blip` shows, preferring its SVG version.
    fn blip_media(
        &mut self,
        blip: Node,
        rels: &HashMap<String, (String, String)>,
    ) -> Option<String> {
        let svg = blip
            .descendants()
            .find(|n| n.tag_name().name() == "svgBlip")
            .and_then(|n| r_attr(n, "embed"));
        for id in svg.into_iter().chain(r_attr(blip, "embed")) {
            if let Some((_, part)) = rels.get(&id)
                && let Some(m) = self.media(part)
            {
                return Some(m);
            }
        }
        None
    }

    fn frac(&self, r: Rect) -> [f64; 4] {
        [
            r.x / self.slide.0,
            r.y / self.slide.1,
            r.w / self.slide.0,
            r.h / self.slide.1,
        ]
    }

    fn rect_attrs(&self, r: Rect) -> String {
        let [x, y, w, h] = self.frac(r);
        format!("x={} y={} w={} h={}", fmt(x), fmt(y), fmt(w), fmt(h))
    }

    /// Design points per PowerPoint point.
    fn pt_scale(&self) -> f64 {
        DESIGN_WIDTH as f64 / (self.slide.0 / 12700.0)
    }

    /// The `background`/`background_image` a `p:bg` gives.
    fn background(
        &mut self,
        bg: Node,
        ctx: &Ctx,
        rels: &HashMap<String, (String, String)>,
        out: &mut Out,
    ) {
        if let Some(pr) = p(bg, "bgPr") {
            if let Some(blip) = a(pr, "blipFill").and_then(|b| a(b, "blip")) {
                if let Some(m) = self.blip_media(blip, rels) {
                    out.front.insert("background_image".into(), m);
                }
            } else if let Some(c) = ctx.fill(pr, None) {
                out.front.insert("background".into(), to_hex(c));
            }
        } else if let Some(r) = p(bg, "bgRef") {
            let ph = r
                .children()
                .filter(|c| c.is_element())
                .find_map(|c| ctx.color(c, None));
            let idx = num(r, "idx").unwrap_or(0.0) as usize;
            let theme_xml = self.pkg.text(&ctx.theme.part).unwrap_or_default();
            let style = Document::parse(&theme_xml).ok().and_then(|doc| {
                let list = if idx >= 1001 {
                    "bgFillStyleLst"
                } else {
                    "fillStyleLst"
                };
                let i = if idx >= 1001 {
                    idx - 1001
                } else {
                    idx.saturating_sub(1)
                };
                let l = doc.descendants().find(|n| n.has_tag_name((NS_A, list)))?;
                let fill = l.children().filter(|c| c.is_element()).nth(i)?;
                let embed = fill
                    .descendants()
                    .find(|n| n.has_tag_name((NS_A, "blip")))
                    .and_then(|b| r_attr(b, "embed"));
                Some((ctx.fill_color(fill, ph), embed))
            });
            match style {
                Some((_, Some(embed))) => {
                    let rels = self.pkg.rels(&ctx.theme.part);
                    if let Some((_, part)) = rels.get(&embed)
                        && let Some(m) = self.media(part)
                    {
                        out.front.insert("background_image".into(), m);
                    }
                }
                Some((Some(c), None)) => {
                    out.front.insert("background".into(), to_hex(c));
                }
                _ => {
                    if let Some(c) = ph {
                        out.front.insert("background".into(), to_hex(c));
                    }
                }
            }
        }
    }

    /// Adds a layout's (or master's) shapes to `out`.
    #[allow(clippy::too_many_arguments)]
    fn shapes(
        &mut self,
        tree: Node,
        t: Transform,
        ctx: &Ctx,
        rels: &HashMap<String, (String, String)>,
        placeholders: bool,
        inherit: &dyn Fn(&PhKey) -> (Option<Rect>, TextLook),
        counts: &mut HashMap<String, usize>,
        out: &mut Out,
    ) {
        let mut placed: Vec<(Rect, Node, PhKey)> = Vec::new();
        for sp in tree.children().filter(|n| n.is_element()) {
            if is_hidden(sp) {
                continue;
            }
            match sp.tag_name().name() {
                "grpSp" => {
                    let Some(xfrm) = p(sp, "grpSpPr").and_then(|g| a(g, "xfrm")) else {
                        continue;
                    };
                    let (Some(r), Some(ch_off), Some(ch_ext)) =
                        (xfrm_rect(xfrm), a(xfrm, "chOff"), a(xfrm, "chExt"))
                    else {
                        continue;
                    };
                    let (cw, ch) = (
                        num(ch_ext, "cx").unwrap_or(r.w),
                        num(ch_ext, "cy").unwrap_or(r.h),
                    );
                    let outer = t.apply(r);
                    let inner = Transform {
                        off: (outer.x, outer.y),
                        scale: (
                            if cw > 0.0 { outer.w / cw } else { 1.0 },
                            if ch > 0.0 { outer.h / ch } else { 1.0 },
                        ),
                        ch_off: (
                            num(ch_off, "x").unwrap_or(0.0),
                            num(ch_off, "y").unwrap_or(0.0),
                        ),
                    };
                    self.shapes(sp, inner, ctx, rels, placeholders, inherit, counts, out);
                }
                "pic" => {
                    let Some(r) = sp_rect(sp).map(|r| t.apply(r)) else {
                        continue;
                    };
                    let Some(blip) = p(sp, "blipFill").and_then(|b| a(b, "blip")) else {
                        continue;
                    };
                    if let Some(m) = self.blip_media(blip, rels) {
                        out.lines
                            .push(format!("::: image {} src={m}", self.rect_attrs(r)));
                    }
                }
                "sp" => {
                    if let Some(ph) = ph_of(sp) {
                        if placeholders
                            && let Some(r) = sp_rect(sp).map(|r| t.apply(r)).or(inherit(&ph).0)
                        {
                            placed.push((r, sp, ph));
                        }
                        continue;
                    }
                    let Some(r) = sp_rect(sp).map(|r| t.apply(r)) else {
                        continue;
                    };
                    let text = shape_text(sp);
                    let sp_pr = p(sp, "spPr");
                    let fill = sp_pr.and_then(|s| ctx.fill(s, None));
                    let geom = sp_pr
                        .and_then(|s| a(s, "prstGeom"))
                        .and_then(|g| g.attribute("prst"))
                        .unwrap_or("rect");
                    let shape = match geom {
                        "ellipse" => " shape=ellipse",
                        "roundRect" => " radius=12",
                        _ => "",
                    };
                    if let Some(c) = fill {
                        if matches!(geom, "rect" | "roundRect" | "ellipse") {
                            out.lines.push(format!(
                                "::: box {} fill={}{shape}",
                                self.rect_attrs(r),
                                to_hex(c)
                            ));
                        } else {
                            self.report.skip(&format!("{geom} shapes"));
                        }
                    }
                    if !text.is_empty() {
                        let look = run_look(ctx, sp);
                        let md = text
                            .lines()
                            .map(|l| escape(l).replace(SLIDE_NUMBER, "{{slide}}"))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        self.area(r, &look, BODY_PT, &md, out);
                    }
                }
                "graphicFrame" => self.report.skip("charts, tables and objects"),
                "cxnSp" => self.report.skip("lines"),
                _ => {}
            }
        }
        // Number placeholders in reading order (top to bottom, then left to right), so
        // {{body}} is the left column and {{body2}} the right.
        let row = self.slide.1 / 20.0;
        placed.sort_by(|(a, ..), (b, ..)| {
            ((a.y / row).round(), a.x)
                .partial_cmp(&((b.y / row).round(), b.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (r, sp, ph) in placed {
            self.placeholder(sp, &ph, r, ctx, inherit, counts, out);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn placeholder(
        &mut self,
        sp: Node,
        ph: &PhKey,
        r: Rect,
        ctx: &Ctx,
        inherit: &dyn Fn(&PhKey) -> (Option<Rect>, TextLook),
        counts: &mut HashMap<String, usize>,
        out: &mut Out,
    ) {
        let (_, inherited_look) = inherit(ph);
        let body = p(sp, "txBody");
        let own = level1_look(
            ctx,
            body.and_then(|b| a(b, "lstStyle")),
            body.and_then(|b| a(b, "bodyPr")),
        );
        let look = own.or(inherited_look);
        let (var, md, base): (&'static str, &str, f32) = match ph.kind.as_str() {
            "title" | "ctrTitle" => ("title", "# {{%}}", H1_PT),
            "subTitle" => ("subtitle", "{{%}}", BODY_PT),
            "dt" => return,
            "ftr" => ("footer", "{{%}}", BODY_PT),
            "sldNum" => ("slide", "{{%}}", BODY_PT),
            "pic" => ("picture", "{{%}}", BODY_PT),
            _ => ("body", "{{%}}", BODY_PT),
        };
        let prompt = shape_text(sp);
        let custom = sp
            .descendants()
            .find(|n| n.has_tag_name((NS_P, "ph")))
            .is_some_and(|n| n.attribute("hasCustomPrompt") == Some("1"));
        // A custom prompt ("Speaker Name") names the placeholder: {{speaker_name}}.
        let named = (custom && matches!(var, "body" | "subtitle" | "picture"))
            .then(|| var_name(&prompt))
            .flatten()
            .map(|n| synonym(&n).map_or(n, str::to_string))
            .filter(|n| !counts.contains_key(n.as_str()) && !RESERVED.contains(&n.as_str()));
        let name = match named {
            Some(n) => {
                counts.insert(n.clone(), 1);
                n
            }
            None => {
                let n = counts.entry(var.to_string()).or_default();
                *n += 1;
                match (var, *n) {
                    ("slide", _) => "slide".to_string(),
                    (v, 1) => v.to_string(),
                    (v, n) => format!("{v}{n}"),
                }
            }
        };
        if name == "body" {
            out.has_body = true;
        }
        if !prompt.is_empty() && !prompt.starts_with("Click to edit") && name != "slide" {
            out.comments.push(format!(
                "{{{{{name}}}}}: {}",
                prompt.lines().next().unwrap_or("")
            ));
        }
        self.area(r, &look, base, &md.replace('%', &name), out);
    }

    fn area(&self, r: Rect, look: &TextLook, base: f32, md: &str, out: &mut Out) {
        let mut attrs = self.rect_attrs(r);
        if let Some(size) = look.size_pt {
            let scale = size * self.pt_scale() / base as f64;
            let _ = write!(attrs, " scale={}", fmt(scale));
        }
        if let Some(c) = look.color {
            let _ = write!(attrs, " color={}", to_hex(c));
        }
        match look.align.as_deref() {
            Some("ctr") => attrs.push_str(" align=center"),
            Some("r") => attrs.push_str(" align=right"),
            _ => {}
        }
        match look.anchor.as_deref() {
            Some("ctr") => attrs.push_str(" valign=center"),
            Some("b") => attrs.push_str(" valign=bottom"),
            _ => {}
        }
        if let Some(f) = look.font.as_deref().and_then(|f| font_name(f, &out.front)) {
            let key = if base == H1_PT {
                "heading_font"
            } else {
                "font"
            };
            let _ = write!(attrs, " {key}=\"{f}\"");
        }
        out.lines.push(format!("::: area {attrs}\n{md}\n:::"));
    }
}

/// A theme font reference (`+mj-lt`, `+mn-lt`) or a face name, as a family dullview can ask for.
fn font_name(face: &str, front: &BTreeMap<String, String>) -> Option<String> {
    match face {
        "+mj-lt" => front.get("heading_font").cloned(),
        "+mn-lt" => front.get("font").cloned(),
        f if f.starts_with('+') => None,
        f => Some(clean_face(f)),
    }
}

/// "Arial Bold" is Arial at a bold weight; dullview already bolds headings.
fn clean_face(face: &str) -> String {
    face.strip_suffix(" Bold").unwrap_or(face).to_string()
}

/// Placeholder names dullview fills itself.
const RESERVED: &[&str] = &["body", "slide", "total", "template", "title"];

/// Prompts that mean a key slides already use.
fn synonym(name: &str) -> Option<&'static str> {
    match name {
        "subtitle" | "sub_title" | "subheadline" | "sub_headline" | "subhead" | "tagline" => {
            Some("subtitle")
        }
        "speaker_name" | "presenter" | "presenter_name" | "author" | "author_name" => {
            Some("author")
        }
        _ => None,
    }
}

/// A short prompt as a placeholder name: "Speaker Name" → `speaker_name`.
fn var_name(prompt: &str) -> Option<String> {
    let first = prompt.lines().next()?.trim();
    let words: Vec<String> = first
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    (!words.is_empty() && words.len() <= 4).then(|| words.join("_"))
}

fn escape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for ch in line.chars() {
        if matches!(
            ch,
            '*' | '_' | '`' | '#' | '[' | ']' | '<' | '>' | '\\' | '{' | '}'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "layout".into() } else { out }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Text styles a master gives each kind of placeholder, and its placeholders' positions.
fn master_inherit<'a>(
    master: &'a Document<'a>,
    ctx: &Ctx,
) -> impl Fn(&PhKey) -> (Option<Rect>, TextLook) + 'a {
    let root = master.root_element();
    let styles = p(root, "txStyles");
    let style_look = |name: &str| level1_look(ctx, styles.and_then(|s| p(s, name)), None);
    let title = style_look("titleStyle");
    let body = style_look("bodyStyle");
    let other = style_look("otherStyle");
    let mut phs: Vec<(PhKey, Option<Rect>, TextLook)> = Vec::new();
    for sp in root.descendants().filter(|n| n.has_tag_name((NS_P, "sp"))) {
        if let Some(ph) = ph_of(sp) {
            let b = p(sp, "txBody");
            let look = level1_look(
                ctx,
                b.and_then(|b| a(b, "lstStyle")),
                b.and_then(|b| a(b, "bodyPr")),
            );
            phs.push((ph, sp_rect(sp), look));
        }
    }
    move |key: &PhKey| {
        fn family(k: &str) -> &str {
            match k {
                "title" | "ctrTitle" => "title",
                "dt" | "ftr" | "sldNum" => k,
                _ => "body",
            }
        }
        let found = phs
            .iter()
            .find(|(p, _, _)| {
                p.idx.is_some() && p.idx == key.idx && family(&p.kind) == family(&key.kind)
            })
            .or_else(|| {
                phs.iter()
                    .find(|(p, _, _)| family(&p.kind) == family(&key.kind))
            });
        let base = match family(&key.kind) {
            "title" => title.clone(),
            "body" => body.clone(),
            _ => other.clone(),
        };
        match found {
            Some((_, r, look)) => (*r, look.clone().or(base)),
            None => (None, base),
        }
    }
}

/// A layout without a subtitle placeholder shows `{{subtitle}}` under the title, since most
/// slides set one.
fn fit_subtitle(out: &mut Out, counts: &HashMap<String, usize>) {
    if counts.contains_key("subtitle") {
        return;
    }
    if let Some(line) = out
        .lines
        .iter_mut()
        .find(|l| l.ends_with("\n# {{title}}\n:::"))
    {
        *line = line.replace("\n# {{title}}\n:::", "\n# {{title}}\n\n{{subtitle}}\n:::");
    }
}

/// The master's "Title and Content" layout: by name, else by layout type, else the first
/// with a title and a content placeholder.
fn content_layout(pkg: &Package, masters: &[Master]) -> Option<String> {
    // (part, name, type, has a title and a content placeholder)
    let mut layouts: Vec<(String, String, Option<String>, bool)> = Vec::new();
    for master in masters {
        let rels = pkg.rels(&master.part);
        let Ok(doc) = Document::parse(&master.xml) else {
            continue;
        };
        for id in doc
            .descendants()
            .filter(|n| n.has_tag_name((NS_P, "sldLayoutId")))
            .filter_map(|n| r_attr(n, "id"))
        {
            let Some((_, part)) = rels.get(&id) else {
                continue;
            };
            let Some(xml) = pkg.text(part) else { continue };
            let Ok(layout) = Document::parse(&xml) else {
                continue;
            };
            let root = layout.root_element();
            let name = p(root, "cSld")
                .and_then(|c| c.attribute("name"))
                .map(slug)
                .unwrap_or_default();
            // PowerPoint's content placeholder has no type (or `obj`): "Click to add text".
            let phs: Vec<Option<&str>> = root
                .descendants()
                .filter(|n| n.has_tag_name((NS_P, "ph")))
                .map(|ph| ph.attribute("type"))
                .collect();
            let titled = phs.contains(&Some("title"));
            let content = phs.iter().any(|t| matches!(t, None | Some("obj")));
            layouts.push((
                part.clone(),
                name,
                root.attribute("type").map(str::to_string),
                titled && content,
            ));
        }
    }
    layouts
        .iter()
        .find(|(_, name, ..)| name.starts_with("title-and-content"))
        .or_else(|| {
            layouts
                .iter()
                .find(|(_, _, kind, _)| kind.as_deref() == Some("obj"))
        })
        .or_else(|| layouts.iter().find(|(.., simple)| *simple))
        .map(|(part, ..)| part.clone())
}

/// The layout parts the file's own slides use, in slide order.
fn used_layouts(
    pkg: &Package,
    pres_doc: &Document,
    pres_rels: &HashMap<String, (String, String)>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in pres_doc
        .descendants()
        .filter(|n| n.has_tag_name((NS_P, "sldId")))
        .filter_map(|n| r_attr(n, "id"))
    {
        let Some((_, slide)) = pres_rels.get(&id) else {
            continue;
        };
        for (kind, part) in pkg.rels(slide).into_values() {
            if kind == "slideLayout" && !out.contains(&part) {
                out.push(part);
            }
        }
    }
    out
}

/// Imports the layouts the file's slides use (every layout when `all`, or when it has no
/// slides) into `dir/_templates`.
pub fn run(file: &Path, dir: &Path, all: bool) -> Result<String, String> {
    let pkg = Package::open(file)?;
    let pres = "ppt/presentation.xml";
    let pres_xml = pkg
        .text(pres)
        .ok_or("no ppt/presentation.xml: not a PowerPoint file")?;
    let pres_doc = Document::parse(&pres_xml).map_err(|e| e.to_string())?;
    let size = pres_doc
        .descendants()
        .find(|n| n.has_tag_name((NS_P, "sldSz")))
        .and_then(|n| Some((num(n, "cx")?, num(n, "cy")?)))
        .unwrap_or((12192000.0, 6858000.0));
    let pres_rels = pkg.rels(pres);
    let mut masters: Vec<Master> = Vec::new();
    for id in pres_doc
        .descendants()
        .filter(|n| n.has_tag_name((NS_P, "sldMasterId")))
        .filter_map(|n| r_attr(n, "id"))
    {
        let Some((_, part)) = pres_rels.get(&id) else {
            continue;
        };
        let Some(xml) = pkg.text(part) else { continue };
        let rels = pkg.rels(part);
        let theme_part = rels
            .values()
            .find(|(k, _)| k == "theme")
            .map(|(_, p)| p.clone())
            .unwrap_or_default();
        let theme = load_theme(&pkg, &theme_part);
        let map = {
            let doc = Document::parse(&xml).map_err(|e| e.to_string())?;
            ClrMap::from(p(doc.root_element(), "clrMap"))
        };
        masters.push(Master {
            part: part.clone(),
            xml,
            theme,
            map,
        });
    }
    if masters.is_empty() {
        return Err("no slide masters found".into());
    }
    let mut wanted = if all {
        Vec::new()
    } else {
        used_layouts(&pkg, &pres_doc, &pres_rels)
    };
    // Slides without a `template:` need a plain title-and-content layout, used or not.
    let content = content_layout(&pkg, &masters);
    if !wanted.is_empty()
        && let Some(c) = &content
        && !wanted.contains(c)
    {
        wanted.push(c.clone());
    }

    fs::create_dir_all(dir.join(TEMPLATES_DIR)).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut imp = Importer {
        pkg: &pkg,
        dir: dir.to_path_buf(),
        slide: size,
        report: Report::default(),
        copied: HashMap::new(),
    };
    let g = gcd(size.0 as u64, size.1 as u64).max(1);
    let aspect = format!("{}:{}", size.0 as u64 / g, size.1 as u64 / g);
    let mut outs: Vec<Out> = Vec::new();
    let mut used: HashMap<String, usize> = HashMap::new();

    for master in &masters {
        let mdoc = Document::parse(&master.xml).map_err(|e| e.to_string())?;
        let mroot = mdoc.root_element();
        let mrels = pkg.rels(&master.part);
        let master_ctx = Ctx {
            theme: &master.theme,
            map: &master.map,
        };
        let inherit = master_inherit(&mdoc, &master_ctx);
        let layout_ids: Vec<String> = mroot
            .descendants()
            .filter(|n| n.has_tag_name((NS_P, "sldLayoutId")))
            .filter_map(|n| r_attr(n, "id"))
            .collect();
        for id in layout_ids {
            let Some((_, part)) = mrels.get(&id) else {
                continue;
            };
            if !wanted.is_empty() && !wanted.contains(part) {
                continue;
            }
            let Some(xml) = pkg.text(part) else { continue };
            let Ok(doc) = Document::parse(&xml) else {
                continue;
            };
            let root = doc.root_element();
            let rels = pkg.rels(part);
            let map = match p(root, "clrMapOvr").and_then(|o| a(o, "overrideClrMapping")) {
                Some(o) => ClrMap::from(Some(o)),
                None => master.map.clone(),
            };
            let ctx = Ctx {
                theme: &master.theme,
                map: &map,
            };
            let Some(csld) = p(root, "cSld") else {
                continue;
            };
            let name = csld.attribute("name").unwrap_or("Layout").to_string();
            // PowerPoint names copies of a layout "1_Closing Slide", "2_…".
            let plain = match name.split_once('_') {
                Some((n, rest)) if !rest.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
                    rest
                }
                _ => name.as_str(),
            };
            let mut base = slug(plain);
            let n = used.entry(base.clone()).or_default();
            *n += 1;
            if *n > 1 {
                base = format!("{base}-{n}");
            }
            let mut out = Out {
                name: base,
                front: BTreeMap::new(),
                comments: Vec::new(),
                lines: Vec::new(),
                has_body: false,
                is_content: content.as_deref() == Some(part.as_str()),
            };
            out.comments.push(format!(
                "Imported from {} (layout \"{name}\")",
                file.file_name()
                    .map(|f| f.to_string_lossy())
                    .unwrap_or_default()
            ));
            out.front.insert("aspect".into(), aspect.clone());
            out.front.insert("padding".into(), "64".into());
            if let Some(c) = ctx
                .theme
                .colors
                .get(map.0.get("tx1").map(String::as_str).unwrap_or("dk1"))
            {
                out.front.insert("color".into(), to_hex(*c));
            }
            if let Some(c) = ctx.theme.colors.get("accent1") {
                out.front.insert("accent".into(), to_hex(*c));
            }
            if let Some(f) = &ctx.theme.minor {
                out.front.insert("font".into(), clean_face(f));
            }
            if let Some(f) = &ctx.theme.major {
                out.front.insert("heading_font".into(), clean_face(f));
            }
            // Background: the layout's own, else the master's.
            match p(csld, "bg") {
                Some(bg) => imp.background(bg, &ctx, &rels, &mut out),
                None => {
                    if let Some(bg) = p(mroot, "cSld").and_then(|c| p(c, "bg")) {
                        imp.background(bg, &master_ctx, &mrels, &mut out);
                    }
                }
            }
            let mut counts = HashMap::new();
            // The master's own pictures and shapes sit under the layout's, unless it hides them.
            if root.attribute("showMasterSp") != Some("0")
                && let Some(tree) = p(mroot, "cSld").and_then(|c| p(c, "spTree"))
            {
                let mut master_counts = HashMap::new();
                imp.shapes(
                    tree,
                    Transform::IDENTITY,
                    &master_ctx,
                    &mrels,
                    false,
                    &inherit,
                    &mut master_counts,
                    &mut out,
                );
            }
            if let Some(tree) = p(csld, "spTree") {
                imp.shapes(
                    tree,
                    Transform::IDENTITY,
                    &ctx,
                    &rels,
                    true,
                    &inherit,
                    &mut counts,
                    &mut out,
                );
            }
            fit_subtitle(&mut out, &counts);
            outs.push(out);
        }
    }

    for out in &outs {
        let mut s = String::from("---\n");
        for c in &out.comments {
            let _ = writeln!(s, "# {c}");
        }
        if !out.has_body {
            s.push_str(
                "# This layout has no body area: slide text after the front matter isn't shown.\n",
            );
        }
        for (k, v) in &out.front {
            let _ = writeln!(s, "{k}: \"{v}\"");
        }
        s.push_str("---\n");
        for line in &out.lines {
            s.push_str(line);
            s.push('\n');
        }

        let path = dir.join(TEMPLATES_DIR).join(format!("{}.md", out.name));
        fs::write(&path, s).map_err(|e| format!("{}: {e}", path.display()))?;
        imp.report.templates.push(out.name.clone());
    }

    let default_path = dir
        .join(TEMPLATES_DIR)
        .join(format!("{DEFAULT_TEMPLATE}.md"));
    let mut made_default = None;
    let kept_default = default_path.exists();
    if !kept_default {
        let pick = outs
            .iter()
            .find(|o| o.is_content)
            .or_else(|| {
                outs.iter()
                    .find(|o| o.has_body && o.lines.iter().any(|l| l.contains("{{title}}")))
            })
            .or_else(|| outs.first());
        if let Some(o) = pick {
            let src = dir.join(TEMPLATES_DIR).join(format!("{}.md", o.name));
            fs::copy(&src, &default_path).map_err(|e| e.to_string())?;
            made_default = Some(o.name.clone());
        }
    }

    let r = &imp.report;
    let mut msg = format!(
        "Imported {} templates into {}",
        r.templates.len(),
        dir.join(TEMPLATES_DIR).display()
    );
    for t in &r.templates {
        let _ = write!(msg, "\n  {t}");
    }
    if let Some(d) = made_default {
        let _ = write!(msg, "\ndefault.md is a copy of {d}");
    } else if kept_default {
        msg.push_str("\nKept your default.md: slides without a `template:` still use it");
    }
    if r.media > 0 {
        let _ = write!(msg, "\nCopied {} pictures to _templates/media", r.media);
    }
    if !r.skipped.is_empty() {
        msg.push_str("\nSkipped (dullview can't draw these):");
        for (what, n) in &r.skipped {
            let _ = write!(msg, "\n  {n} {what}");
        }
    }
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import(fixture: &str) -> (tempdir::Dir, String) {
        let dir = tempdir::Dir::new(fixture);
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let report = run(&src, &dir.0, true).expect("imports");
        (dir, report)
    }

    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new(name: &str) -> Dir {
                let p = std::env::temp_dir()
                    .join(format!("dullview-test-{}-{name}", std::process::id()));
                let _ = std::fs::remove_dir_all(&p);
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn imports_python_pptx_default_layouts() {
        let (dir, report) = import("python-pptx-default.pptx");
        let t = dir.0.join("_templates");
        let title = fs::read_to_string(t.join("title-slide.md")).unwrap();
        assert!(title.contains("# {{title}}"), "{title}");
        assert!(title.contains("{{subtitle}}"), "{title}");
        assert!(title.contains("aspect: \"4:3\""), "{title}");
        let content = fs::read_to_string(t.join("title-and-content.md")).unwrap();
        assert!(content.contains("{{body}}"), "{content}");
        assert!(t.join("default.md").exists(), "{report}");
        // The imported templates load as a deck.
        fs::write(
            dir.0.join("001-a.md"),
            "---\ntemplate: title-slide\ntitle: Hi\nsubtitle: There\n---\n",
        )
        .unwrap();
        let deck = crate::deck::load(&dir.0);
        let slide = &deck.slides[0];
        assert_eq!(slide.error, None);
        assert!(
            slide
                .elements
                .iter()
                .any(|e| matches!(&e.kind, crate::template::Kind::Area(md) if md.contains("# Hi")))
        );
    }

    #[test]
    fn imports_layout_and_master_pictures() {
        let (dir, report) = import("bug60715.pptx");
        let t = dir.0.join("_templates");
        assert!(
            t.join("media").read_dir().unwrap().next().is_some(),
            "{report}"
        );
        let any_picture = fs::read_dir(&t)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .any(|e| {
                let s = fs::read_to_string(e.path()).unwrap();
                s.contains("background_image:") || s.contains("::: image")
            });
        assert!(any_picture, "{report}");
    }

    #[test]
    fn imports_only_the_layouts_slides_use() {
        let dir = tempdir::Dir::new("used");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bug60715.pptx");
        let report = run(&src, &dir.0, false).expect("imports");
        let mut names: Vec<String> = fs::read_dir(dir.0.join("_templates"))
            .unwrap()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_suffix(".md")
                    .map(str::to_string)
            })
            .collect();
        names.sort();
        // The one layout its slide uses, the content layout, and default.md copied from it.
        assert_eq!(
            names,
            ["default", "rubrik-och-innehåll", "rubrikbild"],
            "{report}"
        );
    }

    #[test]
    fn colour_modifiers() {
        let xml = r#"<a:schemeClr xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" val="accent1"><a:lumMod val="50000"/></a:schemeClr>"#;
        let doc = Document::parse(xml).unwrap();
        let mut theme = Theme::default();
        theme.colors.insert("accent1".into(), [1.0, 0.0, 0.0]);
        let map = ClrMap::from(None);
        let ctx = Ctx {
            theme: &theme,
            map: &map,
        };
        assert_eq!(
            to_hex(ctx.color(doc.root_element(), None).unwrap()),
            "#800000"
        );
    }
}
