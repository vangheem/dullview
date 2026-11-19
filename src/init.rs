//! `dullview --init <dir>`: a starter deck.

use std::fs;
use std::path::Path;

const FILES: &[(&str, &str)] = &[
    (
        "variables.yaml",
        r##"# Deck-wide settings, and placeholders every slide can use ({{author}}).
# A slide's own front matter overrides these.

# Text size in percent: 90 shrinks all text, 110 grows it.
text_size: 100
author: Your Name
"##,
    ),
    (
        "_templates/default.md",
        r##"---
# Layout: background, color, accent (hex), align (left|center|right),
# valign (top|center|bottom), padding (points), scale (text size multiplier).
background: "#16181d"
color: "#e8e6e3"
accent: "#7aa2f7"
valign: top
padding: 80
scale: 1
---
# {{title}}

{{body}}
"##,
    ),
    (
        "_templates/title.md",
        r##"---
background: "#0f1115"
color: "#f2f2f2"
accent: "#7aa2f7"
align: center
valign: center
padding: 96
scale: 1.2
---
# {{title}}

### {{subtitle}}

{{body}}

*{{author}}*
"##,
    ),
    (
        "_templates/section.md",
        r##"---
background: "#7aa2f7"
color: "#0f1115"
accent: "#0f1115"
valign: center
padding: 96
scale: 1.4
---
# {{title}}

{{body}}
"##,
    ),
    (
        "001-intro.md",
        r##"---
template: title
title: My Presentation
subtitle: Made with dullview
---
"##,
    ),
    (
        "002-agenda.md",
        r##"---
title: Agenda
---
- Every `.md` file here is one slide, in filename order
- Front matter picks a `template` and fills its `{{placeholders}}`
- The markdown below the front matter becomes `{{body}}`
- Save a file and the slide updates

---
Author notes go below a `---` line. They never show on the slide; press N to read them.
"##,
    ),
    (
        "003-section.md",
        r##"---
template: section
title: Templates
---
They live in `_templates/`.
"##,
    ),
    (
        "004-code.md",
        r##"---
title: Code and tables
---
```rust
fn main() {
    println!("hello, slides");
}
```

| Key            | Action         |
| -------------- | -------------- |
| → / Space      | Next slide     |
| ←              | Previous slide |
| ⌘F             | Full screen    |
"##,
    ),
];

pub fn run(dir: &Path) -> Result<(), String> {
    if dir.exists() {
        let has_slides = fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .any(|e| e.path().extension().is_some_and(|x| x == "md"));
        if has_slides {
            return Err(format!("{} already has slides", dir.display()));
        }
    }
    for (rel, content) in FILES {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&path, content).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    fs::create_dir_all(dir.join("assets")).map_err(|e| e.to_string())?;
    Ok(())
}
