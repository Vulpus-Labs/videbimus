//! Arguments with run-time substitutions: `$in.<label>`, `$out`,
//! `$chunk.*`, `$cores`, `$script.<name>` and `$source.<name>`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use videbimus_core::Interval;

/// A part of the chunk's frame range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChunkField {
    /// The first frame.
    Start,
    /// One past the last frame, as config's `to`.
    End,
    /// The last frame (inclusive: Blender's `--end`).
    Last,
    /// The number of frames.
    Count,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Lit(String),
    In(String),
    Out,
    Chunk(ChunkField),
    Cores,
    Script(String),
    Source(String),
}

/// A string with substitutions, parsed and checked at load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Template(pub(crate) Vec<Piece>);

/// What the template may refer to, for checking at load.
pub(crate) struct Names<'a> {
    pub inputs: &'a [String],
    pub scripts: &'a [String],
    pub sources: &'a [String],
    pub frames: bool,
}

/// A problem with a template: what, and how to fix it.
#[derive(Debug)]
pub(crate) struct Problem {
    pub message: String,
    pub help: String,
}

/// The values substituted when a chunk runs.
pub(crate) struct Vars<'a> {
    pub inputs: &'a BTreeMap<String, PathBuf>,
    pub out: PathBuf,
    pub chunk: Option<Interval>,
    pub cores: u32,
    pub scripts: &'a BTreeMap<String, PathBuf>,
    pub sources: &'a BTreeMap<String, PathBuf>,
}

fn is_name(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn choices(kind: &str, what: &str, names: &[String], got: &str) -> String {
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    match videbimus_runtime::registry::did_you_mean(got, &refs) {
        Some(k) => format!("did you mean `${kind}.{k}`?"),
        None if names.is_empty() => format!("this command declares no {what}s"),
        None => format!(
            "its {what}s are {}",
            names
                .iter()
                .map(|n| format!("`${kind}.{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

impl Template {
    /// Parses `s`. `$$` is a literal `$`; a `$` followed by a name that
    /// isn't one of the substitutions (`$HOME`, `$1`) is kept as written,
    /// for the shell or program that reads it.
    pub(crate) fn parse(s: &str, names: &Names<'_>) -> Result<Self, Problem> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut rest = s;
        while let Some(at) = rest.find('$') {
            lit.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            if let Some(r) = after.strip_prefix('$') {
                lit.push('$');
                rest = r;
                continue;
            }
            let head_len = after.find(|c| !is_name(c)).unwrap_or(after.len());
            let head = &after[..head_len];
            let tail = &after[head_len..];
            // `.field`, for the heads that take one
            let field = || -> (&str, &str) {
                let Some(f) = tail.strip_prefix('.') else {
                    return ("", tail);
                };
                let n = f.find(|c| !is_name(c)).unwrap_or(f.len());
                let name = f[..n].trim_end_matches('-');
                (name, &f[name.len()..])
            };
            let (piece, next) = match head {
                "out" => (Piece::Out, tail),
                "cores" => (Piece::Cores, tail),
                "in" | "script" | "source" | "chunk" => {
                    let (name, next) = field();
                    (Self::fielded(head, name, names)?, next)
                }
                _ => {
                    lit.push('$');
                    rest = after;
                    continue;
                }
            };
            if !lit.is_empty() {
                pieces.push(Piece::Lit(std::mem::take(&mut lit)));
            }
            pieces.push(piece);
            rest = next;
        }
        lit.push_str(rest);
        if !lit.is_empty() {
            pieces.push(Piece::Lit(lit));
        }
        Ok(Self(pieces))
    }

    fn fielded(head: &str, name: &str, names: &Names<'_>) -> Result<Piece, Problem> {
        let problem = |message: String, help: String| Problem { message, help };
        if head == "chunk" {
            if !names.frames {
                return Err(problem(
                    format!(
                        "`$chunk.{name}` needs a frame output, and this command's output is single"
                    ),
                    "declare frames: output frames=\"####.png\" from=(f)1 to=(f)100".into(),
                ));
            }
            return match name {
                "start" => Ok(Piece::Chunk(ChunkField::Start)),
                "end" => Ok(Piece::Chunk(ChunkField::End)),
                "last" => Ok(Piece::Chunk(ChunkField::Last)),
                "count" => Ok(Piece::Chunk(ChunkField::Count)),
                _ => Err(problem(
                    format!("`$chunk.{name}` isn't a part of the chunk"),
                    "the chunk's frames are $chunk.start, $chunk.last (inclusive), $chunk.end (one past the last) and $chunk.count".into(),
                )),
            };
        }
        let (list, what) = match head {
            "in" => (names.inputs, "input"),
            "script" => (names.scripts, "script"),
            _ => (names.sources, "source"),
        };
        if name.is_empty() {
            return Err(problem(
                format!("`${head}` needs the {what}'s name: `${head}.<name>`"),
                choices(head, what, list, ""),
            ));
        }
        if !list.iter().any(|n| n == name) {
            let help = choices(head, what, list, name);
            let help = if head == "in" {
                format!("{help}; an input is declared with input \"{name}\" \"<asset>\"")
            } else {
                help
            };
            return Err(problem(format!("there is no {what} `{name}`"), help));
        }
        Ok(match head {
            "in" => Piece::In(name.to_owned()),
            "script" => Piece::Script(name.to_owned()),
            _ => Piece::Source(name.to_owned()),
        })
    }

    /// The input labels it refers to.
    pub(crate) fn inputs(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|p| match p {
            Piece::In(l) => Some(l.as_str()),
            _ => None,
        })
    }

    /// The literal text, if it has no substitutions.
    pub(crate) fn literal(&self) -> Option<String> {
        self.0
            .iter()
            .map(|p| match p {
                Piece::Lit(s) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Option<String>>()
    }

    /// The string with `vars` substituted.
    pub(crate) fn fill(&self, vars: &Vars<'_>) -> String {
        let mut out = String::new();
        for p in &self.0 {
            match p {
                Piece::Lit(s) => out.push_str(s),
                Piece::In(l) => out.push_str(&vars.inputs[l].to_string_lossy()),
                Piece::Out => out.push_str(&vars.out.to_string_lossy()),
                Piece::Cores => out.push_str(&vars.cores.to_string()),
                Piece::Script(n) => out.push_str(&vars.scripts[n].to_string_lossy()),
                Piece::Source(n) => out.push_str(&vars.sources[n].to_string_lossy()),
                Piece::Chunk(f) => {
                    let iv = vars.chunk.expect("checked at load: a frame output");
                    let v = match f {
                        ChunkField::Start => iv.start.0,
                        ChunkField::End => iv.end.0,
                        ChunkField::Last => iv.end.0 - 1,
                        ChunkField::Count => iv.len(),
                    };
                    out.push_str(&v.to_string());
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use videbimus_core::Frame;

    fn names(frames: bool) -> (Vec<String>, Vec<String>) {
        let _ = frames;
        (vec!["shot".into(), "song-mix".into()], vec!["scene".into()])
    }

    fn parse(s: &str, frames: bool) -> Result<Template, Problem> {
        let (inputs, scripts) = names(frames);
        Template::parse(
            s,
            &Names {
                inputs: &inputs,
                scripts: &scripts,
                sources: &[],
                frames,
            },
        )
    }

    #[test]
    fn substitutes() {
        let inputs = BTreeMap::from([
            ("shot".to_owned(), PathBuf::from("/s/shot")),
            ("song-mix".to_owned(), PathBuf::from("/s/mix")),
        ]);
        let scripts = BTreeMap::from([("scene".to_owned(), PathBuf::from("/c/hw.py"))]);
        let vars = Vars {
            inputs: &inputs,
            out: PathBuf::from("/o"),
            chunk: Some(Interval::new(Frame(10), Frame(20))),
            cores: 4,
            scripts: &scripts,
            sources: &BTreeMap::new(),
        };
        let fill = |s: &str| parse(s, true).unwrap().fill(&vars);
        assert_eq!(fill("$in.shot/panel.mp4"), "/s/shot/panel.mp4");
        assert_eq!(fill("$in.song-mix"), "/s/mix");
        assert_eq!(fill("--out=$out/####"), "--out=/o/####");
        assert_eq!(
            fill("$chunk.start-$chunk.last ($chunk.count) <$chunk.end"),
            "10-19 (10) <20"
        );
        assert_eq!(fill("-t$cores"), "-t4");
        assert_eq!(fill("$script.scene"), "/c/hw.py");
        assert_eq!(fill("$$out costs $$5"), "$out costs $5");
        assert_eq!(
            fill("echo $HOME $1 $(seq $0) $"),
            "echo $HOME $1 $(seq $0) $"
        );
        assert_eq!(fill("$outer"), "$outer");
        assert_eq!(
            parse("plain", true).unwrap().literal().as_deref(),
            Some("plain")
        );
        assert_eq!(parse("$out", true).unwrap().literal(), None);
    }

    #[test]
    fn problems() {
        let e = parse("$in.shoot", true).unwrap_err();
        assert_eq!(e.message, "there is no input `shoot`");
        assert!(e.help.contains("did you mean `$in.shot`?"), "{}", e.help);
        let e = parse("$chunk.first", true).unwrap_err();
        assert!(e.message.contains("isn't a part of the chunk"));
        let e = parse("$chunk.start", false).unwrap_err();
        assert!(e.message.contains("needs a frame output"));
        let e = parse("$in", true).unwrap_err();
        assert!(e.message.contains("needs the input's name"));
        let e = parse("$script.x", true).unwrap_err();
        assert!(e.help.contains("`$script.scene`"), "{}", e.help);
    }
}
