//! `vaultgen`: writes a seeded, realistic Obsidian vault for the benchmarks (DESIGN §18).
//!
//!   vaultgen --out DIR [--notes 10000] [--attachments 20000] [--pdfs 300]
//!            [--large-pdf 100MB/500p | --large-pdf none] [--big-folder 5000] [--seed 1]
//!
//! The same arguments always give the same bytes (its own PRNG, no dependency whose output could
//! change with a version bump). Besides the random notes, `Bench/` holds the notes the benchmarks
//! open by name; see `bench_notes`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

mod pdf;
mod png;
mod text;

use text::Words;

/// SplitMix64: tiny, fast and stable forever.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9e37_79b9_7f4a_7c15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + self.below(hi - lo + 1)
    }
    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    pub fn pick_str(&mut self, xs: &[&'static str]) -> &'static str {
        xs[self.below(xs.len())]
    }
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub out: PathBuf,
    pub notes: usize,
    pub attachments: usize,
    pub pdfs: usize,
    /// (bytes, pages)
    pub large_pdf: Option<(usize, usize)>,
    pub big_folder: usize,
    pub seed: u64,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            out: PathBuf::from("vault"),
            notes: 10_000,
            attachments: 20_000,
            pdfs: 300,
            large_pdf: Some((100 << 20, 500)),
            big_folder: 5_000,
            seed: 1,
        }
    }
}

fn parse_size(s: &str) -> Option<(usize, usize)> {
    if s == "none" {
        return None;
    }
    let (mb, pages) = s.split_once('/')?;
    let mb: usize = mb.trim_end_matches("MB").parse().ok()?;
    let pages: usize = pages.trim_end_matches('p').parse().ok()?;
    Some((mb << 20, pages))
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    let num = |v: Option<&String>, f: &str| -> Result<usize, String> {
        v.and_then(|v| v.parse().ok())
            .ok_or(format!("{f} takes a number"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => o.out = it.next().ok_or("--out takes a directory")?.into(),
            "--notes" => o.notes = num(it.next(), a)?,
            "--attachments" => o.attachments = num(it.next(), a)?,
            "--pdfs" => o.pdfs = num(it.next(), a)?,
            "--big-folder" => o.big_folder = num(it.next(), a)?,
            "--seed" => o.seed = num(it.next(), a)? as u64,
            "--large-pdf" => {
                let v = it
                    .next()
                    .ok_or("--large-pdf takes e.g. 100MB/500p or none")?;
                o.large_pdf = if v == "none" {
                    None
                } else {
                    Some(parse_size(v).ok_or("--large-pdf takes e.g. 100MB/500p or none")?)
                };
            }
            "-h" | "--help" => {
                println!("vaultgen --out DIR [--notes N] [--attachments N] [--pdfs N] [--large-pdf 100MB/500p|none] [--big-folder N] [--seed N]");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument: {a}")),
        }
    }
    Ok(o)
}

fn main() {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("vaultgen: {e}");
            std::process::exit(2);
        }
    };
    let t = std::time::Instant::now();
    match generate(&o) {
        Ok(s) => eprintln!(
            "vaultgen: {} notes, {} images, {} PDFs, {:.0} MB in {:.1} s → {}",
            s.notes,
            s.images,
            s.pdfs,
            s.bytes as f64 / 1048576.0,
            t.elapsed().as_secs_f64(),
            o.out.display()
        ),
        Err(e) => {
            eprintln!("vaultgen: {e}");
            std::process::exit(1);
        }
    }
}

#[derive(Default, Debug)]
pub struct Stats {
    pub notes: usize,
    pub images: usize,
    pub pdfs: usize,
    pub bytes: u64,
}

struct Out<'a> {
    root: &'a Path,
    stats: Stats,
}

impl Out<'_> {
    fn write(&mut self, rel: &str, bytes: &[u8]) -> io::Result<()> {
        let p = self.root.join(rel);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d)?;
        }
        let mut f = io::BufWriter::new(fs::File::create(&p)?);
        f.write_all(bytes)?;
        f.flush()?;
        self.stats.bytes += bytes.len() as u64;
        Ok(())
    }
}

/// A note's place: its folder (may be empty) and unique name (without `.md`).
#[derive(Clone)]
pub struct NoteRef {
    pub folder: String,
    pub name: String,
}

impl NoteRef {
    fn path(&self) -> String {
        if self.folder.is_empty() {
            format!("{}.md", self.name)
        } else {
            format!("{}/{}.md", self.folder, self.name)
        }
    }
}

/// The folder tree: a few top-level areas, nested up to 6 deep.
fn folders(r: &mut Rng, w: &Words) -> Vec<String> {
    let mut out = vec![String::new()];
    let tops = [
        "Projects",
        "Areas",
        "Resources",
        "Archive",
        "Journal",
        "Reading",
        "Work",
        "Teaching",
        "Research",
        "Recipes",
        "Travel",
        "Ideas",
    ];
    for top in tops {
        out.push(top.to_string());
        let mut frontier = vec![top.to_string()];
        for depth in 1..6 {
            let mut next = vec![];
            for f in &frontier {
                let kids = if depth == 1 {
                    r.range(2, 5)
                } else {
                    r.range(0, 3)
                };
                for k in 0..kids {
                    let name = format!("{} {}", text::title_case(w.word(r)), k + 1);
                    let p = format!("{f}/{name}");
                    out.push(p.clone());
                    next.push(p);
                }
            }
            frontier = next;
        }
    }
    out
}

pub fn generate(o: &Opts) -> io::Result<Stats> {
    fs::create_dir_all(&o.out)?;
    let mut out = Out {
        root: &o.out,
        stats: Stats::default(),
    };
    let mut r = Rng::new(o.seed);
    let w = Words::new();
    let dirs = folders(&mut r, &w);

    // Attachments first, so notes can embed them. Obsidian's "attachments in a folder" layout,
    // spread over subfolders like a vault that has grown for years.
    let mut images = Vec::with_capacity(o.attachments);
    for i in 0..o.attachments {
        let year = 2018 + i % 8;
        let name = format!(
            "attachments/{year}/Pasted image {year}{:04}{:06}.png",
            101 + (i / 8) % 1200,
            i
        );
        let (wd, ht) = if r.chance(0.04) {
            (r.range(1600, 2400), r.range(1000, 1600))
        } else {
            (r.range(64, 900), r.range(48, 700))
        };
        let noise = if r.chance(0.3) {
            r.range(4, 40) as u8
        } else {
            0
        };
        out.write(&name, &png::image(&mut r, wd, ht, noise))?;
        images.push(name.rsplit('/').next().unwrap().to_string());
        out.stats.images += 1;
    }
    let mut pdfs = Vec::with_capacity(o.pdfs);
    for i in 0..o.pdfs {
        let name = format!(
            "{} {}.pdf",
            text::title_case(&w.phrase(&mut r, 2, 4)),
            i + 1
        );
        let pages = r.range(1, 30);
        let bytes = r.range(20 << 10, 600 << 10);
        out.write(
            &format!("attachments/papers/{name}"),
            &pdf::make(&mut r, &w, pages, bytes),
        )?;
        pdfs.push(name);
        out.stats.pdfs += 1;
    }

    // Note names: unique words + a number, in random folders; plus the big folder.
    let n_big = o.big_folder.min(o.notes);
    let mut notes = Vec::with_capacity(o.notes);
    for i in 0..o.notes - n_big {
        let folder = if r.chance(0.1) {
            String::new()
        } else {
            r.pick(&dirs).clone()
        };
        let name = format!("{} {}", text::title_case(&w.phrase(&mut r, 1, 4)), i + 1);
        notes.push(NoteRef { folder, name });
    }
    for i in 0..n_big {
        notes.push(NoteRef {
            folder: "Big folder".into(),
            name: format!("Item {:05} {}", i + 1, w.word(&mut r)),
        });
    }

    let ctx = text::Ctx {
        notes: &notes,
        images: &images,
        pdfs: &pdfs,
    };
    for n in &notes {
        let len = r.range(80, 1500);
        let body = text::note(&mut r, &w, &ctx, len);
        out.write(&n.path(), body.as_bytes())?;
        out.stats.notes += 1;
    }
    bench_notes(&mut r, &w, &ctx, &mut out)?;
    if let Some((bytes, pages)) = o.large_pdf {
        out.write("Bench/Large.pdf", &pdf::make(&mut r, &w, pages, bytes))?;
        out.stats.pdfs += 1;
    }
    Ok(out.stats)
}

/// Fixed notes the benchmarks open by name (DESIGN §18).
fn bench_notes(r: &mut Rng, w: &Words, ctx: &text::Ctx, out: &mut Out) -> io::Result<()> {
    // Embeds need attachments; the 5 MB PDF is generated here so it exists whatever --pdfs says.
    out.write("Bench/Five MB.pdf", &pdf::make(r, w, 40, 5 << 20))?;
    out.stats.pdfs += 1;
    let mut large = String::from("# Large note\n\n");
    while large.len() < 1 << 20 {
        large.push_str(&text::note(
            r,
            w,
            &text::Ctx {
                images: &[],
                pdfs: &[],
                ..*ctx
            },
            2000,
        ));
        large.push('\n');
    }
    out.write("Bench/Large note.md", large.as_bytes())?;
    out.write("Bench/Maths.md", text::maths_note(r, w).as_bytes())?;
    let embeds = |n_img: usize, n_pdf: usize, r: &mut Rng| {
        let mut s = String::new();
        for i in 0..n_img.max(n_pdf) {
            s.push_str(&w.paragraph(r, 2));
            s.push_str("\n\n");
            if i < n_img && !ctx.images.is_empty() {
                s.push_str(&format!(
                    "![[{}]]\n\n",
                    ctx.images[i * 7 % ctx.images.len()]
                ));
            }
            if i < n_pdf {
                s.push_str("![[Five MB.pdf]]\n\n");
            }
        }
        s
    };
    out.write(
        "Bench/Gallery.md",
        format!("# Gallery\n\n{}", embeds(50, 0, r)).as_bytes(),
    )?;
    out.write(
        "Bench/Mixed media.md",
        format!("# Mixed media\n\n{}", embeds(30, 3, r)).as_bytes(),
    )?;
    out.write(
        "Bench/Small note.md",
        b"# Small note\n\nA short note to type into.\n",
    )?;
    out.stats.notes += 5;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn small(out: &Path, seed: u64) -> Opts {
        Opts {
            out: out.into(),
            notes: 300,
            attachments: 60,
            pdfs: 5,
            large_pdf: Some((2 << 20, 20)),
            big_folder: 50,
            seed,
        }
    }

    /// Hash of every file's path and bytes, in path order.
    fn digest(root: &Path) -> String {
        let mut files = vec![];
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p)
                } else {
                    files.push(p)
                }
            }
        }
        files.sort();
        let mut h = Sha256::new();
        for f in files {
            h.update(f.strip_prefix(root).unwrap().to_string_lossy().as_bytes());
            h.update(fs::read(&f).unwrap());
        }
        format!("{:x}", h.finalize())
    }

    #[test]
    fn same_seed_same_bytes() {
        let (a, b, c) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        generate(&small(a.path(), 7)).unwrap();
        generate(&small(b.path(), 7)).unwrap();
        generate(&small(c.path(), 8)).unwrap();
        assert_eq!(digest(a.path()), digest(b.path()));
        assert_ne!(digest(a.path()), digest(c.path()));
    }

    #[test]
    fn images_decode_and_pdfs_are_well_formed() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Rng::new(3);
        for (w, h, noise) in [(1, 1, 0), (64, 48, 0), (300, 200, 20), (17, 900, 4)] {
            let bytes = png::image(&mut r, w, h, noise);
            let dec = ::png::Decoder::new(std::io::Cursor::new(&bytes));
            let mut rd = dec.read_info().unwrap();
            let mut buf = vec![0; rd.output_buffer_size().unwrap()];
            let info = rd.next_frame(&mut buf).unwrap();
            assert_eq!((info.width as usize, info.height as usize), (w, h));
        }
        let pdf = pdf::make(&mut r, &Words::new(), 3, 100_000);
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
        assert!((90_000..120_000).contains(&pdf.len()), "{}", pdf.len());
        // The xref offsets point at the objects.
        let s = String::from_utf8_lossy(&pdf);
        let xref = s.rfind("xref\n").unwrap();
        for (i, line) in s[xref..].lines().skip(3).take(3 + 3 * 3).enumerate() {
            let off: usize = line[..10].parse().unwrap();
            assert!(
                s[off..].starts_with(&format!("{} 0 obj", i + 1)),
                "object {}",
                i + 1
            );
        }
        drop(d);
    }

    #[test]
    fn the_core_importer_takes_it_whole() {
        use jess_core::import::{self, FolderSource, PlanOptions};
        let d = tempfile::tempdir().unwrap();
        let o = small(d.path(), 1);
        let st = generate(&o).unwrap();
        struct None_(jess_core::state::MetaState);
        impl import::Existing for None_ {
            fn state(&self) -> &jess_core::state::MetaState {
                &self.0
            }
            fn text(&mut self, _: jess_core::Id) -> Option<Vec<u8>> {
                None
            }
        }
        let plan = import::plan(
            &mut FolderSource {
                root: d.path().into(),
            },
            &mut None_(Default::default()),
            &PlanOptions::default(),
        )
        .unwrap();
        let r = &plan.report;
        assert_eq!(r.notes, st.notes);
        assert_eq!(r.images, st.images);
        assert_eq!(r.pdfs, st.pdfs);
        assert!(
            r.unresolved.is_empty(),
            "{:?}",
            &r.unresolved[..r.unresolved.len().min(5)]
        );
        assert!(
            r.collisions.is_empty() && r.skipped.is_empty(),
            "{:?} {:?}",
            r.collisions,
            r.skipped
        );
    }
}
