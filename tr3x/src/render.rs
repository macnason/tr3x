//! Markdown to ANSI renderer for the terminal chat view.
//!
//! Output is printed into normal scrollback, so it has to read well on any
//! theme: only the 16 ANSI palette colours (plus default fg) and text
//! attributes are used, never a background, 256-colour or 24-bit colour.
//! Every returned row is self-contained: it starts clean and, if it set
//! anything, ends with `ESC[0m` (and closes any OSC 8 hyperlink).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

use pulldown_cmark::{
    Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd, TextMergeStream,
};
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Rendered markdown: one element per terminal row.
pub struct Rendered {
    /// ANSI styled rows, no trailing `\n`, visible width <= requested width.
    pub lines: Vec<String>,
}

/// Render `text` as GitHub-flavoured markdown for a terminal `width` columns wide.
#[cfg(test)]
pub fn markdown(text: &str, width: usize) -> Rendered {
    markdown_resolved(text, width, str::to_string)
}

pub fn markdown_with_links(text: &str, width: usize, links: &crate::link::FileLinks) -> Rendered {
    markdown_resolved(text, width, |url| links.resolve(url))
}

fn markdown_resolved(text: &str, width: usize, resolve: impl Fn(&str) -> String) -> Rendered {
    let width = width.max(MIN_WIDTH);
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let events: Vec<Event> = TextMergeStream::new(Parser::new_ext(text, opts))
        .map(|event| match event {
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => Event::Start(Tag::Link {
                link_type,
                dest_url: resolve(&dest_url).into(),
                title,
                id,
            }),
            other => other,
        })
        .collect();
    let blocks = Builder::new(events).run();
    let cx = Ctx {
        base: Style::default(),
        depth: 0,
    };
    Rendered {
        lines: tidy(render_seq(&blocks, width, &cx, true)),
    }
}

/// Display width of `line`, ignoring ANSI CSI / OSC sequences.
pub fn visible_width(line: &str) -> usize {
    UnicodeWidthStr::width(strip_ansi(line).as_str())
}

/// `line` without ANSI CSI / OSC (and other escape) sequences.
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.peek() {
            Some('[') => {
                it.next();
                for n in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&n) {
                        break;
                    }
                }
            }
            // OSC, DCS, APC, PM, SOS: terminated by BEL or ST (ESC \).
            Some(']' | 'P' | '_' | '^' | 'X') => {
                it.next();
                while let Some(n) = it.next() {
                    if n == '\x07' {
                        break;
                    }
                    if n == '\x1b' {
                        if it.peek() == Some(&'\\') {
                            it.next();
                        }
                        break;
                    }
                }
            }
            Some(_) => {
                it.next();
            }
            None => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Style + emission
// ---------------------------------------------------------------------------

const MIN_WIDTH: usize = 20;
const RESET: &str = "\x1b[0m";
const OSC_CLOSE: &str = "\x1b]8;;\x1b\\";
const BLUE: u8 = 34;
const CYAN: u8 = 36;
const GREEN: u8 = 32;

#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Style {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    fg: Option<u8>,
    link: Option<Rc<str>>,
}

impl Style {
    fn dim() -> Style {
        Style {
            dim: true,
            ..Style::default()
        }
    }

    /// `self` layered over `base`: attributes add up, own colour / link win.
    fn over(&self, base: &Style) -> Style {
        Style {
            bold: self.bold || base.bold,
            dim: self.dim || base.dim,
            italic: self.italic || base.italic,
            underline: self.underline || base.underline,
            strike: self.strike || base.strike,
            fg: self.fg.or(base.fg),
            link: self.link.clone().or_else(|| base.link.clone()),
        }
    }

    fn same_sgr(&self, o: &Style) -> bool {
        self.bold == o.bold
            && self.dim == o.dim
            && self.italic == o.italic
            && self.underline == o.underline
            && self.strike == o.strike
            && self.fg == o.fg
    }

    fn sgr(&self) -> Option<String> {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".into());
        }
        if self.dim {
            codes.push("2".into());
        }
        if self.italic {
            codes.push("3".into());
        }
        if self.underline {
            codes.push("4".into());
        }
        if self.strike {
            codes.push("9".into());
        }
        if let Some(fg) = self.fg {
            codes.push(fg.to_string());
        }
        if codes.is_empty() {
            None
        } else {
            Some(format!("\x1b[{}m", codes.join(";")))
        }
    }
}

type Frag = (String, Style);
type Line = Vec<Frag>;

fn push_frag(line: &mut Line, text: &str, style: &Style) {
    if text.is_empty() {
        return;
    }
    if let Some((t, s)) = line.last_mut()
        && s == style
    {
        t.push_str(text);
        return;
    }
    line.push((text.to_string(), style.clone()));
}

fn frag_width(line: &[Frag]) -> usize {
    line.iter()
        .map(|(t, _)| UnicodeWidthStr::width(t.as_str()))
        .sum()
}

fn dim(s: &str) -> String {
    format!("\x1b[2m{s}{RESET}")
}

fn clean_url(url: &str) -> String {
    url.chars()
        .filter(|c| !c.is_control() && *c != ' ')
        .collect()
}

/// Serialise one row of styled fragments into a self-contained string.
fn emit(frags: &[Frag]) -> String {
    let mut out = String::new();
    let mut cur = Style::default();
    let mut sgr_on = false;
    let mut link: Option<&str> = None;
    for (text, st) in frags {
        if text.is_empty() {
            continue;
        }
        let new_link = st.link.as_deref();
        if new_link != link && link.is_some() {
            out.push_str(OSC_CLOSE);
        }
        if !st.same_sgr(&cur) {
            if sgr_on {
                out.push_str(RESET);
                sgr_on = false;
            }
            if let Some(code) = st.sgr() {
                out.push_str(&code);
                sgr_on = true;
            }
            cur = st.clone();
        }
        if new_link != link {
            if let Some(u) = new_link {
                out.push_str("\x1b]8;;");
                out.push_str(u);
                out.push_str("\x1b\\");
            }
            link = new_link;
        }
        out.push_str(text);
    }
    if link.is_some() {
        out.push_str(OSC_CLOSE);
    }
    if sgr_on {
        out.push_str(RESET);
    }
    out
}

// ---------------------------------------------------------------------------
// Inline model + word wrapping
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Piece {
    Text(String, Style),
    Soft,
    Hard,
}

enum Atom {
    Word(Line, usize),
    Space(Style),
    Hard,
}

fn is_cjk(g: &str) -> bool {
    g.chars().next().is_some_and(|c| {
        matches!(c as u32,
            0x2E80..=0xA4CF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
    })
}

fn is_space(g: &str) -> bool {
    g.chars().all(|c| c.is_whitespace() && c != '\u{a0}')
}

fn atoms(pieces: &[Piece], base: &Style) -> Vec<Atom> {
    fn flush(out: &mut Vec<Atom>, word: &mut Line, ww: &mut usize) {
        if !word.is_empty() {
            out.push(Atom::Word(std::mem::take(word), *ww));
            *ww = 0;
        }
    }
    let mut out: Vec<Atom> = Vec::new();
    let mut word: Line = Vec::new();
    let mut ww = 0usize;
    for p in pieces {
        match p {
            Piece::Soft => {
                flush(&mut out, &mut word, &mut ww);
                if !matches!(out.last(), Some(Atom::Space(_))) {
                    out.push(Atom::Space(base.clone()));
                }
            }
            Piece::Hard => {
                flush(&mut out, &mut word, &mut ww);
                out.push(Atom::Hard);
            }
            Piece::Text(t, st) => {
                let st = st.over(base);
                for g in t.graphemes(true) {
                    if is_space(g) {
                        flush(&mut out, &mut word, &mut ww);
                        if !matches!(out.last(), Some(Atom::Space(_))) {
                            out.push(Atom::Space(st.clone()));
                        }
                    } else if is_cjk(g) {
                        // No spaces in CJK text: every glyph is a break opportunity.
                        flush(&mut out, &mut word, &mut ww);
                        out.push(Atom::Word(vec![(g.to_string(), st.clone())], g.width()));
                    } else {
                        push_frag(&mut word, g, &st);
                        ww += g.width();
                    }
                }
                flush(&mut out, &mut word, &mut ww);
            }
        }
    }
    flush(&mut out, &mut word, &mut ww);
    out
}

/// Greedy word wrap. Long words are hard-broken on grapheme boundaries.
fn layout(atoms: &[Atom], width: usize) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut cur: Line = Vec::new();
    let mut cw = 0usize;
    let mut pending: Option<Style> = None;
    for a in atoms {
        match a {
            Atom::Space(s) => {
                if cw > 0 {
                    pending = Some(s.clone());
                }
            }
            Atom::Hard => {
                lines.push(std::mem::take(&mut cur));
                cw = 0;
                pending = None;
            }
            Atom::Word(frags, ww) => {
                let sp = usize::from(pending.is_some());
                if cw > 0 && cw + sp + ww > width {
                    lines.push(std::mem::take(&mut cur));
                    cw = 0;
                    pending = None;
                }
                if cw > 0 {
                    if let Some(s) = pending.take() {
                        push_frag(&mut cur, " ", &s);
                        cw += 1;
                    }
                } else {
                    pending = None;
                }
                if cw + ww <= width {
                    for (t, s) in frags {
                        push_frag(&mut cur, t, s);
                    }
                    cw += ww;
                } else {
                    for (t, s) in frags {
                        for g in t.graphemes(true) {
                            let gw = g.width();
                            if cw > 0 && cw + gw > width {
                                lines.push(std::mem::take(&mut cur));
                                cw = 0;
                            }
                            push_frag(&mut cur, g, s);
                            cw += gw;
                        }
                    }
                }
            }
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn wrap(pieces: &[Piece], width: usize, base: &Style) -> Vec<String> {
    layout(&atoms(pieces, base), width)
        .iter()
        .map(|l| emit(l))
        .collect()
}

// ---------------------------------------------------------------------------
// Block model + parser
// ---------------------------------------------------------------------------

enum Block {
    Para(Vec<Piece>, bool),
    Heading(u8, Vec<Piece>),
    List {
        start: Option<u64>,
        items: Vec<Item>,
        loose: bool,
    },
    Quote(Vec<Block>),
    Code {
        lang: Option<String>,
        text: String,
    },
    Table {
        aligns: Vec<Alignment>,
        head: Vec<Vec<Piece>>,
        rows: Vec<Vec<Vec<Piece>>>,
    },
    Rule,
    Html(String),
    Footnote(String, Vec<Block>),
    /// Transient: first child of a task list item.
    TaskMarker(bool),
}

struct Item {
    task: Option<bool>,
    blocks: Vec<Block>,
}

fn clean_text(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .map(|c| if matches!(c, '\n' | '\t') { ' ' } else { c })
        .collect()
}

/// Split `s` into (slice, is_bare_url) runs.
fn split_urls(s: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut last = 0;
    while let Some(p) = s[i..].find("http") {
        let at = i + p;
        i = at + 4;
        let rest = &s[at..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            continue;
        };
        if s[..at]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        let mut end = rest
            .char_indices()
            .find(|(_, c)| c.is_whitespace() || matches!(c, '<' | '>'))
            .map_or(rest.len(), |(k, _)| k);
        loop {
            let url = &rest[..end];
            let Some(c) = url.chars().next_back() else {
                break;
            };
            let strip = match c {
                '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' => true,
                ')' => url.matches(')').count() > url.matches('(').count(),
                ']' => url.matches(']').count() > url.matches('[').count(),
                '}' => url.matches('}').count() > url.matches('{').count(),
                _ => false,
            };
            if !strip {
                break;
            }
            end -= c.len_utf8();
        }
        if end <= scheme {
            continue;
        }
        if at > last {
            out.push((&s[last..at], false));
        }
        out.push((&s[at..at + end], true));
        last = at + end;
        i = last;
    }
    if last < s.len() {
        out.push((&s[last..], false));
    }
    out
}

#[derive(Default)]
struct Inline {
    pieces: Vec<Piece>,
    cur: Style,
    saved: Vec<Style>,
    image_marks: Vec<usize>,
}

fn link_style(base: &Style, url: &str) -> Style {
    let url = clean_url(url);
    let mut st = base.clone();
    if !url.is_empty() {
        st.underline = true;
        st.fg = Some(BLUE);
        st.link = Some(Rc::from(url.as_str()));
    }
    st
}

impl Inline {
    fn text(&mut self, s: &str) {
        if self.cur.link.is_some() {
            self.pieces
                .push(Piece::Text(clean_text(s), self.cur.clone()));
            return;
        }
        for (part, is_url) in split_urls(s) {
            let st = if is_url {
                link_style(&self.cur, part)
            } else {
                self.cur.clone()
            };
            self.pieces.push(Piece::Text(clean_text(part), st));
        }
    }

    fn code(&mut self, s: &str) {
        let mut st = self.cur.clone();
        st.bold = true;
        if st.link.is_none() {
            st.fg = Some(CYAN);
        }
        self.pieces.push(Piece::Text(clean_text(s), st));
    }

    fn html(&mut self, s: &str) {
        let lower = s.trim().to_ascii_lowercase();
        if matches!(lower.as_str(), "<br>" | "<br/>" | "<br />") {
            self.pieces.push(Piece::Hard);
        } else {
            let mut st = self.cur.clone();
            st.dim = true;
            self.pieces.push(Piece::Text(clean_text(s), st));
        }
    }

    fn footnote_ref(&mut self, label: &str) {
        let mut st = self.cur.clone();
        st.dim = true;
        self.pieces
            .push(Piece::Text(format!("[{}]", clean_text(label)), st));
    }

    fn start(&mut self, tag: &Tag) -> bool {
        let prev = self.cur.clone();
        match tag {
            Tag::Emphasis => self.cur.italic = true,
            Tag::Strong => self.cur.bold = true,
            Tag::Strikethrough => self.cur.strike = true,
            Tag::Superscript | Tag::Subscript => {}
            Tag::Link { dest_url, .. } => self.cur = link_style(&prev, dest_url),
            Tag::Image { dest_url, .. } => {
                self.pieces.push(Piece::Text("🖼 ".into(), prev.clone()));
                self.image_marks.push(self.pieces.len());
                self.cur = link_style(&prev, dest_url);
            }
            _ => return false,
        }
        self.saved.push(prev);
        true
    }

    fn end(&mut self, end: &TagEnd) {
        if matches!(end, TagEnd::Image)
            && let Some(mark) = self.image_marks.pop()
            && self.pieces.len() == mark
        {
            self.pieces
                .push(Piece::Text("image".into(), self.cur.clone()));
        }
        if let Some(prev) = self.saved.pop() {
            self.cur = prev;
        }
    }

    fn take(&mut self) -> Vec<Piece> {
        let mut p = std::mem::take(&mut self.pieces);
        while matches!(p.last(), Some(Piece::Soft | Piece::Hard)) {
            p.pop();
        }
        p
    }
}

fn is_inline_end(e: &TagEnd) -> bool {
    matches!(
        e,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

struct Builder<'a> {
    ev: std::vec::IntoIter<Event<'a>>,
    inl: Inline,
    heading: u8,
}

impl<'a> Builder<'a> {
    fn new(events: Vec<Event<'a>>) -> Self {
        Builder {
            ev: events.into_iter(),
            inl: Inline::default(),
            heading: 1,
        }
    }

    fn run(mut self) -> Vec<Block> {
        self.blocks()
    }

    fn flush(&mut self, out: &mut Vec<Block>) {
        let p = self.inl.take();
        if p.iter().any(|x| match x {
            Piece::Text(t, _) => !t.trim().is_empty(),
            _ => false,
        }) {
            out.push(Block::Para(p, false));
        }
    }

    /// Handle an inline-level event. Returns false when it isn't one.
    fn inline_event(&mut self, ev: &Event<'a>) -> bool {
        match ev {
            Event::Text(s) => self.inl.text(s),
            Event::Code(s) | Event::InlineMath(s) | Event::DisplayMath(s) => self.inl.code(s),
            Event::Html(s) | Event::InlineHtml(s) => self.inl.html(s),
            Event::FootnoteReference(l) => self.inl.footnote_ref(l),
            Event::SoftBreak => self.inl.pieces.push(Piece::Soft),
            Event::HardBreak => self.inl.pieces.push(Piece::Hard),
            Event::Start(t) => return self.inl.start(t),
            Event::End(e) if is_inline_end(e) => self.inl.end(e),
            _ => return false,
        }
        true
    }

    fn blocks(&mut self) -> Vec<Block> {
        let mut out = Vec::new();
        while let Some(ev) = self.ev.next() {
            if self.inline_event(&ev) {
                continue;
            }
            match ev {
                Event::Start(tag) => {
                    self.flush(&mut out);
                    match tag {
                        Tag::Paragraph => {}
                        Tag::Heading { level, .. } => {
                            self.heading = match level {
                                HeadingLevel::H1 => 1,
                                HeadingLevel::H2 => 2,
                                HeadingLevel::H3 => 3,
                                HeadingLevel::H4 => 4,
                                HeadingLevel::H5 => 5,
                                HeadingLevel::H6 => 6,
                            };
                        }
                        Tag::BlockQuote(_) => {
                            let kids = self.blocks();
                            out.push(Block::Quote(kids));
                        }
                        Tag::CodeBlock(kind) => {
                            let lang = match kind {
                                CodeBlockKind::Fenced(info) => info
                                    .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
                                    .next()
                                    .filter(|s| !s.is_empty())
                                    .map(clean_text),
                                CodeBlockKind::Indented => None,
                            };
                            let mut text = String::new();
                            for ev in self.ev.by_ref() {
                                match ev {
                                    Event::Text(s) => text.push_str(&s),
                                    Event::End(TagEnd::CodeBlock) => break,
                                    _ => {}
                                }
                            }
                            while text.ends_with('\n') {
                                text.pop();
                            }
                            out.push(Block::Code { lang, text });
                        }
                        Tag::HtmlBlock => {
                            let mut text = String::new();
                            for ev in self.ev.by_ref() {
                                match ev {
                                    Event::Html(s) | Event::Text(s) => text.push_str(&s),
                                    Event::End(TagEnd::HtmlBlock) => break,
                                    _ => {}
                                }
                            }
                            out.push(Block::Html(text));
                        }
                        Tag::List(start) => out.push(self.list(start)),
                        Tag::FootnoteDefinition(label) => {
                            let kids = self.blocks();
                            out.push(Block::Footnote(clean_text(&label), kids));
                        }
                        Tag::Table(aligns) => out.push(self.table(aligns)),
                        // Unknown containers: keep their content.
                        _ => {
                            let kids = self.blocks();
                            out.extend(kids);
                        }
                    }
                }
                Event::End(TagEnd::Paragraph) => {
                    let p = self.inl.take();
                    out.push(Block::Para(p, true));
                }
                Event::End(TagEnd::Heading(_)) => {
                    let p = self.inl.take();
                    out.push(Block::Heading(self.heading, p));
                }
                Event::End(_) => break,
                Event::Rule => {
                    self.flush(&mut out);
                    out.push(Block::Rule);
                }
                Event::TaskListMarker(c) => {
                    self.flush(&mut out);
                    out.push(Block::TaskMarker(c));
                }
                _ => {}
            }
        }
        self.flush(&mut out);
        out
    }

    fn list(&mut self, start: Option<u64>) -> Block {
        let mut items = Vec::new();
        let mut loose = false;
        while let Some(ev) = self.ev.next() {
            match ev {
                Event::Start(Tag::Item) => {
                    let mut blocks = self.blocks();
                    let task = match blocks.first() {
                        Some(Block::TaskMarker(c)) => {
                            let c = *c;
                            blocks.remove(0);
                            Some(c)
                        }
                        _ => None,
                    };
                    if blocks.iter().any(|b| matches!(b, Block::Para(_, true))) {
                        loose = true;
                    }
                    items.push(Item { task, blocks });
                }
                Event::End(TagEnd::List(_)) => break,
                _ => {}
            }
        }
        Block::List {
            start,
            items,
            loose,
        }
    }

    fn table(&mut self, aligns: Vec<Alignment>) -> Block {
        let mut head = Vec::new();
        let mut rows = Vec::new();
        let mut row: Vec<Vec<Piece>> = Vec::new();
        while let Some(ev) = self.ev.next() {
            if self.inline_event(&ev) {
                continue;
            }
            match ev {
                Event::Start(Tag::TableHead | Tag::TableRow) => row.clear(),
                Event::End(TagEnd::TableCell) => {
                    let c = self.inl.take();
                    row.push(c);
                }
                Event::End(TagEnd::TableHead) => head = std::mem::take(&mut row),
                Event::End(TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        Block::Table { aligns, head, rows }
    }
}

// ---------------------------------------------------------------------------
// Block rendering
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Ctx {
    base: Style,
    depth: usize,
}

/// Render blocks, separated by one blank line when `gap`, none otherwise.
fn render_seq(blocks: &[Block], w: usize, cx: &Ctx, gap: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in blocks {
        let lines = render_block(b, w, cx);
        if lines.is_empty() {
            continue;
        }
        if gap && !out.is_empty() {
            out.push(String::new());
        }
        out.extend(lines);
    }
    out
}

/// Prefix the first row with `first`, the rest with `rest`; blank rows get no
/// trailing whitespace.
fn prefix_lines(lines: Vec<String>, first: &str, rest: &str) -> Vec<String> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| {
            let p = if i == 0 { first } else { rest };
            if l.is_empty() {
                p.trim_end().to_string()
            } else {
                format!("{p}{l}")
            }
        })
        .collect()
}

fn render_block(b: &Block, w: usize, cx: &Ctx) -> Vec<String> {
    match b {
        Block::Para(p, _) => wrap(p, w, &cx.base),
        Block::Heading(level, p) => {
            let st = match level {
                1 => Style {
                    bold: true,
                    underline: true,
                    ..Style::default()
                },
                2 => Style {
                    bold: true,
                    ..Style::default()
                },
                _ => Style {
                    bold: true,
                    italic: true,
                    ..Style::default()
                },
            };
            wrap(p, w, &st.over(&cx.base))
        }
        Block::List {
            start,
            items,
            loose,
        } => {
            let bullet = ["•", "◦", "▪"][cx.depth.min(2)];
            let num_w = start.map_or(0, |s| {
                s.saturating_add((items.len() as u64).saturating_sub(1))
                    .to_string()
                    .len()
            });
            let inner = Ctx {
                base: cx.base.clone(),
                depth: cx.depth + 1,
            };
            let mut out: Vec<String> = Vec::new();
            for (i, item) in items.iter().enumerate() {
                let (marker, mw) = match (item.task, start) {
                    (Some(true), _) => (format!("\x1b[{GREEN}m☑{RESET}"), 1),
                    (Some(false), _) => ("☐".to_string(), 1),
                    (None, Some(s)) => (
                        format!("{:>num_w$}.", s.saturating_add(i as u64)),
                        num_w + 1,
                    ),
                    (None, None) => (bullet.to_string(), 1),
                };
                let lines = render_item(&marker, mw + 1, &item.blocks, w, &inner, *loose);
                if lines.is_empty() {
                    continue;
                }
                if *loose && !out.is_empty() {
                    out.push(String::new());
                }
                out.extend(lines);
            }
            out
        }
        Block::Quote(kids) => {
            if w < 10 {
                return render_seq(kids, w, cx, true);
            }
            let mut inner = cx.clone();
            inner.base.dim = true;
            let lines = render_seq(kids, w - 2, &inner, true);
            let bar = dim("│");
            prefix_lines(lines, &format!("{bar} "), &format!("{bar} "))
        }
        Block::Code { lang, text } => render_code(lang.as_deref(), text, w),
        Block::Table { aligns, head, rows } => render_table(aligns, head, rows, w, cx),
        Block::Rule => vec![dim(&"─".repeat(w.min(60)))],
        Block::Html(text) => {
            let mut pieces: Vec<Piece> = Vec::new();
            for line in text.lines() {
                let line = clean_text(line);
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if !pieces.is_empty() {
                    pieces.push(Piece::Hard);
                }
                pieces.push(Piece::Text(line.to_string(), Style::dim()));
            }
            wrap(&pieces, w, &cx.base)
        }
        Block::Footnote(label, kids) => {
            let marker = dim(&format!("[{label}]"));
            let mw = label.width() + 3;
            render_item(&marker, mw, kids, w, cx, true)
        }
        Block::TaskMarker(_) => Vec::new(),
    }
}

/// A marker (already styled) followed by hang-indented content. `mw` is the
/// full prefix width including the space after the marker.
fn render_item(
    marker: &str,
    mw: usize,
    blocks: &[Block],
    w: usize,
    cx: &Ctx,
    loose: bool,
) -> Vec<String> {
    if w < mw + 8 {
        return render_seq(blocks, w, cx, loose);
    }
    let inner = render_seq(blocks, w - mw, cx, loose);
    if inner.is_empty() {
        return vec![marker.to_string()];
    }
    prefix_lines(inner, &format!("{marker} "), &" ".repeat(mw))
}

// ---------------------------------------------------------------------------
// Code blocks + syntax highlighting
// ---------------------------------------------------------------------------

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn find_syntax(lang: &str) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let l = lang.to_ascii_lowercase();
    let canon = match l.as_str() {
        "ts" | "tsx" | "js" | "jsx" | "typescript" | "javascript" | "mjs" | "cjs" => "JavaScript",
        "sh" | "bash" | "zsh" | "shell" | "console" | "shell-session" | "shellscript" => {
            "Bourne Again Shell (bash)"
        }
        "py" | "python3" => "Python",
        "rs" => "Rust",
        "json" | "jsonc" | "json5" => "JSON",
        "diff" | "patch" => "Diff",
        "yml" => "YAML",
        "md" => "Markdown",
        "c++" | "cc" | "hpp" => "C++",
        "golang" => "Go",
        "rb" => "Ruby",
        other => other,
    };
    let found = ss
        .find_syntax_by_name(canon)
        .or_else(|| ss.find_syntax_by_token(canon))
        .or_else(|| ss.find_syntax_by_token(&l));
    match (found, canon) {
        (None, "JSON") => ss.find_syntax_by_name("JavaScript"),
        (f, _) => f,
    }
}

#[derive(Clone, Copy, Default)]
struct Patch {
    fg: Option<u8>,
    bold: bool,
    dim: bool,
    italic: bool,
}

const fn fg(c: u8) -> Patch {
    Patch {
        fg: Some(c),
        bold: false,
        dim: false,
        italic: false,
    }
}
const PLAIN: Patch = Patch {
    fg: None,
    bold: false,
    dim: false,
    italic: false,
};
const BOLD: Patch = Patch {
    fg: None,
    bold: true,
    dim: false,
    italic: false,
};
const ITALIC: Patch = Patch {
    fg: None,
    bold: false,
    dim: false,
    italic: true,
};

/// Scope prefix -> look. First match wins, so specific entries go first.
/// `punctuation` is deliberately absent: it defers to the enclosing scope
/// (quotes stay green inside a string, comment markers stay dim).
const RULES: &[(&str, Patch)] = &[
    (
        "comment",
        Patch {
            fg: None,
            bold: false,
            dim: true,
            italic: true,
        },
    ),
    ("string", fg(32)),
    ("constant", fg(35)),
    ("variable.language", fg(35)),
    ("variable.parameter", PLAIN),
    ("variable.function", fg(36)),
    ("keyword.operator", PLAIN),
    ("keyword", fg(34)),
    ("storage", fg(34)),
    ("entity.name.function", fg(36)),
    ("entity.name.macro", fg(36)),
    ("entity.name.tag", fg(34)),
    ("entity.name.section", BOLD),
    ("entity.name", fg(33)),
    ("entity.other.attribute-name", fg(33)),
    ("entity.other.inherited-class", fg(33)),
    ("support.function", fg(36)),
    ("support.macro", fg(36)),
    ("support.type", fg(33)),
    ("support.class", fg(33)),
    ("support.constant", fg(35)),
    ("markup.inserted", fg(32)),
    ("markup.deleted", fg(31)),
    ("markup.heading", BOLD),
    ("markup.bold", BOLD),
    ("markup.italic", ITALIC),
    ("markup.raw", fg(32)),
    ("markup.underline.link", fg(34)),
    ("meta.diff.header", BOLD),
    ("meta.diff.range", fg(36)),
    ("invalid", fg(31)),
];

fn rule_for(scope: &str) -> Option<Patch> {
    RULES.iter().find_map(|(p, patch)| {
        let hit =
            scope == *p || (scope.starts_with(p) && scope.as_bytes().get(p.len()) == Some(&b'.'));
        hit.then_some(*patch)
    })
}

fn style_for(stack: &ScopeStack, cache: &mut HashMap<Scope, Option<Patch>>) -> Style {
    for sc in stack.as_slice().iter().rev() {
        let rule = *cache
            .entry(*sc)
            .or_insert_with(|| rule_for(&sc.build_string()));
        if let Some(p) = rule {
            return Style {
                fg: p.fg,
                bold: p.bold,
                dim: p.dim,
                italic: p.italic,
                ..Style::default()
            };
        }
    }
    Style::default()
}

fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let n = 4 - col % 4;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else if !c.is_control() {
            out.push(c);
            col += c.width().unwrap_or(0);
        }
    }
    out
}

fn trim_frags(line: &mut Line) {
    while let Some((t, _)) = line.last_mut() {
        let trimmed = t.trim_end().len();
        t.truncate(trimmed);
        if t.is_empty() {
            line.pop();
        } else {
            break;
        }
    }
}

fn highlight(lines: &[String], syntax: &SyntaxReference) -> Vec<Line> {
    let ss = syntaxes();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut cache: HashMap<Scope, Option<Patch>> = HashMap::new();
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let nl = format!("{line}\n");
        let mut frags: Line = Vec::new();
        match state.parse_line(&nl, ss) {
            Ok(ops) => {
                let mut last = 0usize;
                for (pos, op) in ops {
                    if pos > last {
                        let st = style_for(&stack, &mut cache);
                        push_frag(&mut frags, &nl[last..pos], &st);
                    }
                    let _ = stack.apply(&op);
                    last = pos;
                }
                if last < nl.len() {
                    let st = style_for(&stack, &mut cache);
                    push_frag(&mut frags, &nl[last..], &st);
                }
            }
            Err(_) => push_frag(&mut frags, &nl, &Style::default()),
        }
        if let Some((t, _)) = frags.last_mut() {
            let n = t.trim_end_matches('\n').len();
            t.truncate(n);
        }
        out.push(frags);
    }
    out
}

fn truncate_frags(frags: Line, max: usize) -> Line {
    if frag_width(&frags) <= max {
        return frags;
    }
    let keep = max.saturating_sub(1);
    let mut out: Line = Vec::new();
    let mut cw = 0usize;
    'outer: for (t, s) in &frags {
        for g in t.graphemes(true) {
            let gw = g.width();
            if cw + gw > keep {
                break 'outer;
            }
            push_frag(&mut out, g, s);
            cw += gw;
        }
    }
    push_frag(&mut out, "…", &Style::dim());
    out
}

fn render_code(lang: Option<&str>, text: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(l) = lang {
        let l: String = l.chars().take(w).collect();
        out.push(dim(&l));
    }
    let lines: Vec<String> = text.split('\n').map(expand_tabs).collect();
    let syntax = lang
        .and_then(find_syntax)
        .filter(|_| lines.len() <= 2000 && lines.iter().all(|l| l.len() <= 2000));
    let styled: Vec<Line> = match syntax {
        Some(s) => highlight(&lines, s),
        None => lines
            .iter()
            .map(|l| vec![(l.clone(), Style::default())])
            .collect(),
    };
    let gutter = format!("{} ", dim("│"));
    let bare = dim("│");
    for mut frags in styled {
        trim_frags(&mut frags);
        let frags = truncate_frags(frags, w.saturating_sub(2));
        if frags.is_empty() {
            out.push(bare.clone());
        } else {
            out.push(format!("{gutter}{}", emit(&frags)));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn render_table(
    aligns: &[Alignment],
    head: &[Vec<Piece>],
    rows: &[Vec<Vec<Piece>>],
    w: usize,
    cx: &Ctx,
) -> Vec<String> {
    let n = aligns
        .len()
        .max(head.len())
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if n == 0 {
        return Vec::new();
    }
    let head_base = Style {
        bold: true,
        ..Style::default()
    }
    .over(&cx.base);

    let overhead = 3 * n + 1;
    if w < overhead + n {
        return table_fallback(head, rows, w, cx, &head_base);
    }

    let empty: Vec<Piece> = Vec::new();
    let mut grid: Vec<Vec<Vec<Atom>>> = Vec::new();
    for (ri, row) in std::iter::once(head)
        .chain(rows.iter().map(Vec::as_slice))
        .enumerate()
    {
        let base = if ri == 0 { &head_base } else { &cx.base };
        grid.push(
            (0..n)
                .map(|c| atoms(row.get(c).unwrap_or(&empty), base))
                .collect(),
        );
    }

    let natural: Vec<usize> = (0..n)
        .map(|c| {
            grid.iter()
                .flat_map(|row| layout(&row[c], usize::MAX / 8))
                .map(|l| frag_width(&l))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();

    let avail = w - overhead;
    let widths = shrink(&natural, avail);

    let bar = dim("│");
    let border = |l: &str, m: &str, r: &str| {
        let segs: Vec<String> = widths.iter().map(|cw| "─".repeat(cw + 2)).collect();
        dim(&format!("{l}{}{r}", segs.join(m)))
    };

    let mut out = vec![border("┌", "┬", "┐")];
    for (ri, row) in grid.iter().enumerate() {
        let cells: Vec<Vec<Line>> = row
            .iter()
            .zip(&widths)
            .map(|(a, cw)| layout(a, *cw))
            .collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
        for r in 0..height {
            let mut s = bar.clone();
            for (c, cw) in widths.iter().enumerate() {
                let (text, tw) = match cells[c].get(r) {
                    Some(l) => (emit(l), frag_width(l)),
                    None => (String::new(), 0),
                };
                let pad = cw.saturating_sub(tw);
                let (left, right) = match aligns.get(c) {
                    Some(Alignment::Right) => (pad, 0),
                    Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };
                s.push(' ');
                s.push_str(&" ".repeat(left));
                s.push_str(&text);
                s.push_str(&" ".repeat(right));
                s.push(' ');
                s.push_str(&bar);
            }
            out.push(s);
        }
        if ri == 0 {
            out.push(border("├", "┼", "┤"));
        }
    }
    out.push(border("└", "┴", "┘"));
    out
}

/// Cap the widest columns at a common level so the total fits `avail`.
fn shrink(natural: &[usize], avail: usize) -> Vec<usize> {
    let total: usize = natural.iter().sum();
    if total <= avail {
        return natural.to_vec();
    }
    let sum_at = |l: usize| natural.iter().map(|w| (*w).min(l)).sum::<usize>();
    let (mut lo, mut hi) = (1usize, natural.iter().copied().max().unwrap_or(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if sum_at(mid) <= avail {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut widths: Vec<usize> = natural.iter().map(|w| (*w).min(lo)).collect();
    let mut left = avail.saturating_sub(widths.iter().sum());
    for (i, nat) in natural.iter().enumerate() {
        if left == 0 {
            break;
        }
        if *nat > lo {
            widths[i] += 1;
            left -= 1;
        }
    }
    widths
}

/// Too narrow for a grid: one wrapped line per row, cells split by "│".
fn table_fallback(
    head: &[Vec<Piece>],
    rows: &[Vec<Vec<Piece>>],
    w: usize,
    cx: &Ctx,
    head_base: &Style,
) -> Vec<String> {
    let mut out = Vec::new();
    for (ri, row) in std::iter::once(head)
        .chain(rows.iter().map(Vec::as_slice))
        .enumerate()
    {
        let mut pieces: Vec<Piece> = Vec::new();
        for (ci, cell) in row.iter().enumerate() {
            if ci > 0 {
                pieces.push(Piece::Text(" │ ".into(), Style::dim()));
            }
            pieces.extend(cell.iter().cloned());
        }
        let base = if ri == 0 { head_base } else { &cx.base };
        out.extend(wrap(&pieces, w, base));
    }
    out
}

// ---------------------------------------------------------------------------
// Final tidy: no leading / trailing / doubled blank rows
// ---------------------------------------------------------------------------

fn tidy(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for l in lines {
        let l = l.trim_end().to_string();
        if l.is_empty() && out.last().is_none_or(String::is_empty) {
            continue;
        }
        out.push(l);
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: &str = r#"# Title with `code` and a very long heading that needs to wrap around the terminal width

Intro paragraph with **bold text that spans quite a few words and will wrap**, *italic*, ~~struck~~,
`inline_code`, and a [link to the docs](https://example.com/docs/with/a/long/path) plus a bare
URL https://example.com/bare/url/here. CJK 日本語のテキストはスペースなしで折り返されます。Emoji 🎉🚀👨‍👩‍👧 end.
Averyveryverylongunbreakablewordthatgoeswayoverthewidthofthetermina1andmustbehardbroken ok.

## Lists

- first item with enough text that it has to wrap onto another line of output
  - nested one
    - nested two with **bold**
- [ ] todo
- [x] done

1. one
2. two with a long wrapped line that keeps going until it must hang under the text, ok
10. ten

> quoted **bold** text that is long enough to wrap around the available quote width easily
>
> second quoted paragraph

### Code

```rust
fn main() {
    // comment
    let s = "hello";
    println!("{} {}", s, 42);
}
```

```diff
@@ -1,2 +1,2 @@
-old
+new
```

    indented code

| Name | Qty | Notes |
|:-----|----:|:-----:|
| apple | 3 | **fresh** and `crisp` |
| banana split | 12 | [link](https://example.com) |

---

<div>raw html</div>

![alt text](https://example.com/i.png)

#### Tail
Final line.
"#;

    fn lines(s: &str, w: usize) -> Vec<String> {
        markdown(s, w).lines
    }

    fn plain(ls: &[String]) -> Vec<String> {
        ls.iter().map(|l| strip_ansi(l)).collect()
    }

    /// All SGR parameter groups in order of appearance.
    fn sgr_groups(line: &str) -> Vec<Vec<u32>> {
        let mut out = Vec::new();
        let b: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '\x1b' && b.get(i + 1) == Some(&'[') {
                let mut j = i + 2;
                let mut params = String::new();
                while j < b.len() && !('\x40'..='\x7e').contains(&b[j]) {
                    params.push(b[j]);
                    j += 1;
                }
                if b.get(j) == Some(&'m') {
                    out.push(
                        params
                            .split(';')
                            .map(|p| p.parse().unwrap_or(0))
                            .collect::<Vec<u32>>(),
                    );
                }
                i = j;
            }
            i += 1;
        }
        out
    }

    fn has_color(line: &str, c: u32) -> bool {
        sgr_groups(line).iter().any(|g| g.contains(&c))
    }

    /// Row starts clean and leaves no SGR / hyperlink open.
    fn assert_self_contained(line: &str) {
        assert!(!line.contains('\n'), "newline in {line:?}");
        let mut sgr_open = false;
        let mut link_open = false;
        let b: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '\x1b' {
                match b.get(i + 1) {
                    Some('[') => {
                        let mut j = i + 2;
                        let mut params = String::new();
                        while !('\x40'..='\x7e').contains(&b[j]) {
                            params.push(b[j]);
                            j += 1;
                        }
                        assert_eq!(b[j], 'm', "non-SGR CSI in {line:?}");
                        let all_zero = params.split(';').all(|p| p.is_empty() || p == "0");
                        sgr_open = !all_zero;
                        i = j;
                    }
                    Some(']') => {
                        let rest: String = b[i..].iter().collect();
                        assert!(rest.starts_with("\x1b]8;;"), "unexpected OSC in {line:?}");
                        let end = rest.find("\x1b\\").expect("unterminated OSC 8");
                        link_open = end > "\x1b]8;;".len();
                        i += rest[..end + 2].chars().count() - 1;
                    }
                    _ => panic!("stray escape in {line:?}"),
                }
            }
            i += 1;
        }
        assert!(!sgr_open, "SGR left open: {line:?}");
        assert!(!link_open, "link left open: {line:?}");
    }

    #[test]
    fn strip_and_width() {
        let s = "\x1b[1mhi\x1b[0m \x1b]8;;https://x\x1b\\日本\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi(s), "hi 日本");
        assert_eq!(visible_width(s), 7);
        assert_eq!(visible_width("plain"), 5);
    }

    #[test]
    fn wrapping_keeps_every_line_within_width() {
        for w in [10, 20, 33, 40, 80, 120] {
            let eff = w.max(20);
            for l in lines(BIG, w) {
                assert!(
                    visible_width(&l) <= eff,
                    "w={w} {l:?} is {}",
                    visible_width(&l)
                );
            }
        }
    }

    #[test]
    fn wrapped_styles_are_reopened() {
        let ls = lines(
            "**aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm**",
            20,
        );
        assert!(ls.len() >= 2);
        for l in &ls {
            assert!(l.starts_with("\x1b[1m"), "{l:?}");
            assert!(l.ends_with(RESET));
        }
    }

    #[test]
    fn no_style_leaks_and_rows_are_clean() {
        for w in [20, 40, 80] {
            for l in lines(BIG, w) {
                assert_self_contained(&l);
                assert_eq!(l, l.trim_end(), "trailing space: {l:?}");
            }
        }
    }

    #[test]
    fn blank_lines_are_tidy() {
        let ls = lines("\n\n# a\n\n\n\npara\n\n---\n\n", 40);
        assert!(!ls.first().unwrap().is_empty());
        assert!(!ls.last().unwrap().is_empty());
        assert!(ls.windows(2).all(|p| !(p[0].is_empty() && p[1].is_empty())));
        assert_eq!(plain(&ls)[1], "");
        assert!(markdown("", 40).lines.is_empty());
    }

    #[test]
    fn only_palette_colours_and_no_background() {
        for w in [20, 80] {
            for l in lines(BIG, w) {
                for g in sgr_groups(&l) {
                    for p in g {
                        let ok = matches!(p, 0 | 1 | 2 | 3 | 4 | 7 | 9 | 22 | 23 | 24 | 29 | 39 | 30..=37 | 90..=97);
                        assert!(ok, "disallowed SGR {p} in {l:?}");
                    }
                }
                assert!(!l.contains("38;5") && !l.contains("38;2") && !l.contains("48;"));
            }
        }
    }

    #[test]
    fn table_has_box_chars_and_aligned_columns() {
        let ls = lines("| a | bb |\n|:-|-:|\n| 1 | **22** |\n| three | 4 |\n", 80);
        let p = plain(&ls);
        assert!(p[0].starts_with('┌') && p[0].ends_with('┐'), "{p:?}");
        assert!(p[2].starts_with('├') && p[2].ends_with('┤'));
        assert!(p.last().unwrap().starts_with('└'));
        let width = visible_width(&p[0]);
        assert!(p.iter().all(|l| visible_width(l) == width), "{p:?}");
        let cols = |l: &str, c: char| -> Vec<usize> {
            l.chars()
                .enumerate()
                .filter(|(_, x)| *x == c)
                .map(|(i, _)| i)
                .collect()
        };
        let header = cols(&p[1], '│');
        assert_eq!(cols(&p[3], '│'), header);
        assert_eq!(cols(&p[4], '│'), header);
        // right-aligned column: value flush against the right border
        assert!(p[3].ends_with("22 │"), "{p:?}");
        assert!(ls[1].contains("\x1b[1m"), "header bold");
        assert!(ls[3].contains("\x1b[1m22"), "bold inside cell");
        assert!(ls[0].starts_with("\x1b[2m"), "dim borders");
    }

    #[test]
    fn narrow_table_wraps_cells() {
        let src = "| id | description |\n|---|---|\n| 1 | a rather long description that cannot possibly fit on one row |\n| 2 | short |\n";
        let ls = lines(src, 30);
        let p = plain(&ls);
        assert!(p.iter().all(|l| visible_width(l) <= 30), "{p:?}");
        let width = visible_width(&p[0]);
        assert!(p.iter().all(|l| visible_width(l) == width));
        assert!(p.len() > 7, "expected multi-row cell: {p:?}");
        assert!(p.iter().any(|l| l.starts_with("│ 1 ")));
        for l in &ls {
            assert_self_contained(l);
        }
    }

    #[test]
    fn tiny_table_falls_back() {
        let src = "|a|b|c|d|e|f|g|h|\n|-|-|-|-|-|-|-|-|\n|1|2|3|4|5|6|7|8|\n";
        for l in lines(src, 20) {
            assert!(visible_width(&l) <= 20);
        }
    }

    #[test]
    fn rust_code_block_is_highlighted_with_gutter() {
        let ls = lines(
            "```rust\nfn main() {\n    let s = \"hi\"; // note\n}\n```",
            60,
        );
        let p = plain(&ls);
        assert_eq!(p[0], "rust");
        assert!(ls[0].starts_with("\x1b[2m"));
        assert!(
            p[1..].iter().all(|l| l.starts_with("│ ") || l == "│"),
            "{p:?}"
        );
        assert!(p[1].contains("fn main"));
        let all = ls.join("\n");
        assert!(has_color(&all, 34), "keyword blue");
        assert!(has_color(&all, 32), "string green");
        assert!(
            ls[2].contains("\x1b[2;3m") || ls[2].contains("\x1b[2m"),
            "comment dim"
        );
        for l in &ls {
            assert_self_contained(l);
        }
    }

    #[test]
    fn code_is_truncated_not_wrapped_and_tabs_expand() {
        let long = "x".repeat(100);
        let ls = lines(&format!("```\n{long}\n\tindented\n```"), 40);
        let p = plain(&ls);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(visible_width(&p[0]), 40);
        assert!(p[0].ends_with('…'));
        assert!(ls[0].ends_with("\x1b[2m…\x1b[0m"));
        assert_eq!(p[1], "│     indented");
    }

    #[test]
    fn unknown_language_is_plain_but_labelled() {
        let ls = lines("```nonsense\nfoo bar\n```", 40);
        assert_eq!(plain(&ls), vec!["nonsense", "│ foo bar"]);
    }

    #[test]
    fn lists_nest_and_hang_indent() {
        let src = "- a\n  - b\n    - c\n- d with a very long line that surely needs to wrap around twice at narrow widths\n";
        let p = plain(&lines(src, 24));
        assert_eq!(p[0], "• a");
        assert_eq!(p[1], "  ◦ b");
        assert_eq!(p[2], "    ▪ c");
        assert!(p[3].starts_with("• d with"));
        assert!(p[4].starts_with("  ") && !p[4].starts_with("   "), "{p:?}");

        let src = "9. nine\n10. ten that is long enough to wrap onto the next line right here\n";
        let p = plain(&lines(src, 24));
        assert_eq!(p[0], " 9. nine");
        assert!(p[1].starts_with("10. ten"));
        assert!(p[2].starts_with("    "), "{p:?}");
    }

    #[test]
    fn task_items() {
        let ls = lines("- [ ] open\n- [x] done\n", 40);
        let p = plain(&ls);
        assert_eq!(p, vec!["☐ open", "☑ done"]);
        assert!(ls[1].starts_with("\x1b[32m☑"));
        assert!(!ls[1].contains("\x1b[9m"));
    }

    #[test]
    fn unresolved_file_links_stay_plain_and_web_links_stay_clickable() {
        let links = crate::link::FileLinks::default();
        let result = markdown_with_links(
            "[report](docs/a.md:12) [web](https://example.com)",
            80,
            &links,
        );
        let joined = result.lines.join("\n");
        assert!(joined.contains("report"));
        assert!(!joined.contains(";;docs/a.md"));
        assert!(joined.contains(";;https://example.com"));
    }

    #[test]
    fn links_open_and_close_on_each_line() {
        let src = "see [a very long link text that will certainly wrap over several rows](https://example.com/x) now";
        let ls = lines(src, 24);
        assert!(ls.len() >= 3);
        let mut with_link = 0;
        for l in &ls {
            let opens = l.matches("\x1b]8;;https://example.com/x\x1b\\").count();
            let closes = l.matches("\x1b]8;;\x1b\\").count();
            assert_eq!(opens, closes, "{l:?}");
            with_link += opens;
            assert_self_contained(l);
        }
        assert!(with_link >= 3);
        let one = &lines("[hi](https://e.com)", 40)[0];
        assert_eq!(
            one,
            "\x1b[4;34m\x1b]8;;https://e.com\x1b\\hi\x1b]8;;\x1b\\\x1b[0m"
        );
    }

    #[test]
    fn bare_and_auto_links() {
        let ls = lines(
            "go to https://example.com/a_b_c, or <https://auto.dev/x>.",
            80,
        );
        assert_eq!(ls.len(), 1);
        assert!(ls[0].contains("\x1b]8;;https://example.com/a_b_c\x1b\\"));
        assert!(ls[0].contains("\x1b]8;;https://auto.dev/x\x1b\\"));
        assert_eq!(
            strip_ansi(&ls[0]),
            "go to https://example.com/a_b_c, or https://auto.dev/x."
        );
    }

    #[test]
    fn inline_styles_and_headings() {
        let l = &lines("**b** *i* ~~s~~ `c`", 40)[0];
        assert!(l.contains("\x1b[1mb"));
        assert!(l.contains("\x1b[3mi"));
        assert!(l.contains("\x1b[9ms"));
        assert!(l.contains("\x1b[1;36mc"));
        assert_eq!(strip_ansi(l), "b i s c");

        assert_eq!(lines("# H", 40)[0], "\x1b[1;4mH\x1b[0m");
        assert_eq!(lines("## H", 40)[0], "\x1b[1mH\x1b[0m");
        assert_eq!(lines("### H", 40)[0], "\x1b[1;3mH\x1b[0m");
        assert_eq!(lines("###### H", 40)[0], "\x1b[1;3mH\x1b[0m");
    }

    #[test]
    fn quote_rule_html_image() {
        let ls = lines("> a\n>\n> b", 40);
        assert_eq!(plain(&ls), vec!["│ a", "│", "│ b"]);
        assert!(ls[0].starts_with("\x1b[2m│\x1b[0m \x1b[2ma"));

        let r = lines("---", 100);
        assert_eq!(plain(&r)[0].chars().count(), 60);
        assert_eq!(lines("---", 30)[0], dim(&"─".repeat(30)));

        let h = lines("<div>raw</div>", 40);
        assert_eq!(plain(&h), vec!["<div>raw</div>"]);
        assert!(h[0].contains("\x1b[2m"));
        let h = lines("a <b>x</b>", 40);
        assert_eq!(plain(&h), vec!["a <b>x</b>"]);

        let im = lines("![alt text](https://e.com/i.png)", 40);
        assert_eq!(plain(&im), vec!["🖼 alt text"]);
        assert!(im[0].contains("\x1b]8;;https://e.com/i.png\x1b\\"));
    }

    #[test]
    fn escapes_in_input_are_neutralised() {
        let ls = lines("hi \x1b[31mred\x1b[0m\n\n```\n\x1b[2Jcode\n```", 40);
        for l in &ls {
            assert_self_contained(l);
        }
        assert_eq!(plain(&ls), vec!["hi [31mred[0m", "", "│ [2Jcode"]);
    }

    #[test]
    fn cjk_and_emoji_wrap_on_width() {
        for l in lines(
            "日本語のテキストはスペースなしで折り返されます🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉",
            21,
        ) {
            assert!(visible_width(&l) <= 21, "{l:?}");
        }
    }

    #[test]
    fn long_word_is_hard_broken() {
        let ls = lines(&"a".repeat(55), 20);
        assert_eq!(
            plain(&ls).iter().map(|l| l.len()).collect::<Vec<_>>(),
            vec![20, 20, 15]
        );
    }

    #[test]
    fn footnotes_and_loose_lists() {
        let ls = lines("text[^1]\n\n[^1]: the note\n", 40);
        let p = plain(&ls);
        assert_eq!(p[0], "text[1]");
        assert!(p.last().unwrap().contains("the note"));
        let p = plain(&lines("- a\n\n- b\n", 40));
        assert_eq!(p, vec!["• a", "", "• b"]);
        let p = plain(&lines("- a\n- b\n", 40));
        assert_eq!(p, vec!["• a", "• b"]);
    }

    #[test]
    #[ignore]
    fn print_sample() {
        let doc = format!(
            "{BIG}\n\n```ts\nconst x: number = 1; // js highlighting\nexport function f(a) {{ return `t${{a}}`; }}\n```\n\n```bash\nls -la | grep \"foo\" && echo $HOME\n```\n\n```python\ndef f(x):\n    return [i * 2 for i in range(x)]  # doubles\n```\n\n```json\n{{\"a\": [1, true, null], \"b\": \"s\"}}\n```\n\nA table with wide content:\n\n| Command | What it does | Example |\n|---|---|---|\n| `cargo test` | Runs every test in the workspace and prints a summary of what passed and failed | `cargo test -p tr3x` |\n| `cargo clippy` | Lints | `cargo clippy --all-targets` |\n"
        );
        for w in [80] {
            println!("--- width {w} ---");
            for l in markdown(&doc, w).lines {
                println!("{l}");
            }
        }
    }
}
