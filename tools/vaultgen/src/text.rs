//! Note text: prose from a fixed word list, with the markdown an Obsidian vault really has
//! (wikilinks with aliases and headings, markdown links, embeds, tags, maths, frontmatter,
//! callouts, tasks, tables, code).

use crate::{NoteRef, Rng};

const WORDS: &str = "the of and to in is that for it as with was on be by this are from at or an \
have not which but all were when we there can more if their will each about how up out them then \
she many some so these would other into has her two like him see time could no make than first \
been its who now people my made over did down only way find use may water long little very after \
words called just where most know get through back much before go good new write our used me man \
too any day same right look think also around another came come work three word must because does \
part even place well such here take why things help put years different away again off went old \
number great tell men say small every found still between name should home big give air line set \
own under read last never us left end along while might next sound below saw something thought \
both few those always looked show large often together asked house world going want school \
important until form food keep children feet land side without boy once animals life enough took \
sometimes four head above kind began almost live page got earth need far hand high year mother \
light parts country father let night following picture being study second eyes soon times story \
boys since white days ever paper hard near sentence better best across during today others sure \
means knew why true river measure garden energy method signal theory system pattern library \
review draft meeting budget lecture chapter figure result sample graph matrix vector proof lemma \
recipe flour butter onion garlic journey station harbour mountain forest valley question answer";

pub struct Words(Vec<&'static str>);

impl Default for Words {
    fn default() -> Self {
        Self::new()
    }
}

impl Words {
    pub fn new() -> Words {
        Words(WORDS.split_whitespace().collect())
    }
    pub fn word(&self, r: &mut Rng) -> &'static str {
        self.0[r.below(self.0.len())]
    }
    pub fn phrase(&self, r: &mut Rng, lo: usize, hi: usize) -> String {
        let n = r.range(lo, hi);
        (0..n).map(|_| self.word(r)).collect::<Vec<_>>().join(" ")
    }
    pub fn sentence(&self, r: &mut Rng) -> String {
        let mut s = self.phrase(r, 5, 18);
        if let Some(c) = s.get_mut(0..1) {
            c.make_ascii_uppercase();
        }
        s.push(*r.pick(&['.', '.', '.', '?', '!']));
        s
    }
    pub fn paragraph(&self, r: &mut Rng, sentences: usize) -> String {
        (0..sentences)
            .map(|_| self.sentence(r))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

pub struct Ctx<'a> {
    pub notes: &'a [NoteRef],
    pub images: &'a [String],
    pub pdfs: &'a [String],
}

const TAGS: &[&str] = &[
    "#todo",
    "#idea",
    "#reading",
    "#project/active",
    "#project/done",
    "#maths",
    "#meeting",
    "#teaching/lecture",
    "#recipe",
    "#travel",
    "#review",
    "#draft",
];

const INLINE_MATHS: &[&str] = &[
    r"$a^2 + b^2 = c^2$",
    r"$\frac{1}{n}\sum_{i=1}^{n} x_i$",
    r"$e^{i\pi} + 1 = 0$",
    r"$\nabla \cdot \mathbf{E} = \rho / \varepsilon_0$",
    r"$\lim_{x \to 0} \frac{\sin x}{x} = 1$",
    r"$O(n \log n)$",
];

const BLOCK_MATHS: &[&str] = &[
    "$$\n\\int_{-\\infty}^{\\infty} e^{-x^2}\\,dx = \\sqrt{\\pi}\n$$",
    "$$\n\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}^{-1} = \\frac{1}{ad-bc}\\begin{pmatrix} d & -b \\\\ -c & a \\end{pmatrix}\n$$",
    "$$\n\\hat{f}(\\xi) = \\int f(x)\\, e^{-2\\pi i x \\xi}\\, dx\n$$",
    "$$\n\\sum_{k=0}^{n} \\binom{n}{k} = 2^n\n$$",
];

fn link(r: &mut Rng, w: &crate::text::Words, ctx: &Ctx) -> String {
    let n = &ctx.notes[r.below(ctx.notes.len())];
    match r.below(10) {
        0 => format!("[[{}|{}]]", n.name, w.phrase(r, 1, 3)),
        1 => format!("[[{}#{}]]", n.name, title_case(w.word(r))),
        2 => {
            let target = if n.folder.is_empty() {
                format!("{}.md", n.name)
            } else {
                format!("{}/{}.md", n.folder, n.name)
            };
            format!("[{}]({})", w.phrase(r, 1, 3), target.replace(' ', "%20"))
        }
        _ => format!("[[{}]]", n.name),
    }
}

/// A note of roughly `target` bytes.
pub fn note(r: &mut Rng, w: &Words, ctx: &Ctx, target: usize) -> String {
    let mut s = String::new();
    if r.chance(0.3) {
        s.push_str(&format!(
            "---\ntags: [{}, {}]\naliases: [{}]\ncreated: 20{:02}-{:02}-{:02}\n---\n",
            w.word(r),
            w.word(r),
            title_case(&w.phrase(r, 1, 3)),
            18 + r.below(8),
            1 + r.below(12),
            1 + r.below(28)
        ));
    }
    s.push_str(&format!("# {}\n\n", title_case(&w.phrase(r, 2, 6))));
    while s.len() < target {
        match r.below(20) {
            0 => s.push_str(&format!("## {}\n\n", title_case(&w.phrase(r, 1, 4)))),
            1 => {
                for _ in 0..r.range(2, 5) {
                    let done = if r.chance(0.4) { "x" } else { " " };
                    s.push_str(&format!(
                        "- [{done}] {} {}\n",
                        w.phrase(r, 2, 7),
                        link(r, w, ctx)
                    ));
                }
                s.push('\n');
            }
            2 => {
                for _ in 0..r.range(2, 6) {
                    s.push_str(&format!("- {}\n", w.phrase(r, 3, 10)));
                }
                s.push('\n');
            }
            3 if !ctx.images.is_empty() => {
                s.push_str(&format!("![[{}]]\n\n", r.pick(ctx.images)));
            }
            4 => s.push_str(&format!("{}\n\n", r.pick_str(BLOCK_MATHS))),
            5 => s.push_str(&format!(
                "> [!{}] {}\n> {}\n\n",
                r.pick(&["note", "tip", "warning", "quote"]),
                title_case(&w.phrase(r, 1, 3)),
                w.sentence(r)
            )),
            6 => s.push_str(&format!(
                "```{}\nfn {}() {{\n    let x = {};\n}}\n```\n\n",
                r.pick(&["rust", "python", "js", ""]),
                w.word(r),
                r.below(1000)
            )),
            7 => {
                s.push_str("| item | value |\n|---|---|\n");
                for _ in 0..r.range(2, 5) {
                    s.push_str(&format!("| {} | {} |\n", w.word(r), r.below(100)));
                }
                s.push('\n');
            }
            8 if !ctx.pdfs.is_empty() && r.chance(0.2) => {
                s.push_str(&format!("See [[{}]].\n\n", r.pick(ctx.pdfs)));
            }
            _ => {
                let k = r.range(2, 6);
                let mut p = w.paragraph(r, k);
                if r.chance(0.6) {
                    p.push(' ');
                    p.push_str(&link(r, w, ctx));
                }
                if r.chance(0.3) {
                    p.push(' ');
                    p.push_str(r.pick_str(TAGS));
                }
                if r.chance(0.1) {
                    p.push(' ');
                    p.push_str(r.pick_str(INLINE_MATHS));
                }
                s.push_str(&p);
                s.push_str("\n\n");
            }
        }
    }
    s
}

/// A maths-heavy note: a formula in most paragraphs.
pub fn maths_note(r: &mut Rng, w: &Words) -> String {
    let mut s = String::from("# Maths\n\n");
    for i in 0..120 {
        s.push_str(&w.paragraph(r, 2));
        s.push(' ');
        s.push_str(r.pick_str(INLINE_MATHS));
        s.push_str(" and ");
        s.push_str(r.pick_str(INLINE_MATHS));
        s.push_str("\n\n");
        if i % 3 == 0 {
            s.push_str(r.pick_str(BLOCK_MATHS));
            s.push_str("\n\n");
        }
    }
    s
}
