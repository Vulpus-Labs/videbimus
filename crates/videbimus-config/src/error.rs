//! Located errors (ADR 0005 §5): every error names the file, line and
//! column, the element, the template, include and `each` chain that
//! produced the value, and the fix.

use std::fmt;

use crate::source::Location;

/// One step in how a value reached the place it was used.
#[derive(Clone, Debug)]
pub enum Via {
    /// Copied from a template.
    Template {
        /// The template's name.
        name: String,
        /// Where the template is defined.
        defined: Location,
        /// The `use="…"` that applied it.
        used: Location,
    },
    /// Read from an included file.
    Include {
        /// The `include` that read it.
        at: Location,
    },
    /// Expanded by an `each`.
    Each {
        /// The name bound to each item.
        binding: String,
        /// The item, as written.
        item: String,
        /// The `each`.
        at: Location,
    },
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Via::Template {
                name,
                defined,
                used,
            } => write!(
                f,
                "from template \"{name}\" (defined at {defined}), applied by `use` at {used}"
            ),
            Via::Include { at } => write!(f, "in a file included at {at}"),
            Via::Each { binding, item, at } => {
                write!(
                    f,
                    "in `each \"{binding}\"` at {at}, where ${binding} = {item}"
                )
            }
        }
    }
}

/// Where a node or value came from: its own location, and the chain of
/// templates, includes and `each` expansions that put it there, innermost
/// first.
#[derive(Clone, Debug)]
pub struct Origin {
    /// Where it is written.
    pub at: Location,
    /// How it got from there to where it is used, innermost first.
    pub via: Vec<Via>,
}

impl Origin {
    /// Written at `at`, used where it is written.
    #[must_use]
    pub fn new(at: Location) -> Self {
        Self {
            at,
            via: Vec::new(),
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.at, f)
    }
}

/// A config error.
///
/// Displays like a compiler error:
///
/// ```text
/// error: expected a time, found the string "8"
///   --> shots.kdl:12:24
///    |
/// 12 | shot "s02" from=(bar)4 to="8"
///    |                        ^^^^^^
///    = in: shot "s02", property `to`
///    = from template "face" (defined at shots.kdl:3:1), applied by `use` at shots.kdl:12:12
///    = help: give it a unit: (bar)8, (beat)8, (s)8 or (f)8
/// ```
#[derive(Clone, Debug)]
pub struct Error(Box<Inner>);

#[derive(Clone, Debug)]
struct Inner {
    message: String,
    origin: Option<Origin>,
    element: Option<String>,
    notes: Vec<String>,
    help: Option<String>,
}

impl Error {
    /// An error with no location yet.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(Box::new(Inner {
            message: message.into(),
            origin: None,
            element: None,
            notes: Vec::new(),
            help: None,
        }))
    }

    /// Placed at `origin`.
    #[must_use]
    pub fn at(mut self, origin: Origin) -> Self {
        self.0.origin = Some(origin);
        self
    }

    /// Names the element: ``shot "s02", property `to` ``.
    #[must_use]
    pub fn element(mut self, element: impl Into<String>) -> Self {
        self.0.element = Some(element.into());
        self
    }

    /// Names the element, unless one is named already.
    #[must_use]
    pub(crate) fn element_or(self, element: impl FnOnce() -> String) -> Self {
        if self.0.element.is_some() {
            self
        } else {
            self.element(element())
        }
    }

    /// Adds the fix.
    #[must_use]
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.0.help = Some(help.into());
        self
    }

    /// Adds the fix, if there is one.
    #[must_use]
    pub(crate) fn maybe_help(mut self, help: Option<String>) -> Self {
        if help.is_some() {
            self.0.help = help;
        }
        self
    }

    /// Adds a note: context such as which reference needed the value.
    #[must_use]
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.0.notes.push(note.into());
        self
    }

    /// What went wrong.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.0.message
    }

    /// Where, and through which templates and includes.
    #[must_use]
    pub fn origin(&self) -> Option<&Origin> {
        self.0.origin.as_ref()
    }

    /// The element: a node, and the property or argument.
    #[must_use]
    pub fn element_name(&self) -> Option<&str> {
        self.0.element.as_deref()
    }

    /// Notes.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.0.notes
    }

    /// The fix.
    #[must_use]
    pub fn help_text(&self) -> Option<&str> {
        self.0.help.as_deref()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "error: {}", self.0.message)?;
        if let Some(origin) = &self.0.origin {
            let (line, text, carets) = origin.at.snippet();
            let gutter = line.to_string().len();
            let pad = " ".repeat(gutter);
            writeln!(f, "{pad}--> {}", origin.at)?;
            writeln!(f, "{pad} |")?;
            writeln!(f, "{line} | {text}")?;
            writeln!(f, "{pad} | {carets}")?;
            if let Some(e) = &self.0.element {
                writeln!(f, "{pad} = in: {e}")?;
            }
            for via in &origin.via {
                writeln!(f, "{pad} = {via}")?;
            }
            for note in &self.0.notes {
                writeln!(f, "{pad} = note: {note}")?;
            }
            if let Some(help) = &self.0.help {
                writeln!(f, "{pad} = help: {help}")?;
            }
        } else {
            if let Some(e) = &self.0.element {
                writeln!(f, "  = in: {e}")?;
            }
            for note in &self.0.notes {
                writeln!(f, "  = note: {note}")?;
            }
            if let Some(help) = &self.0.help {
                writeln!(f, "  = help: {help}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

/// The closest of `candidates` to `word`, if any is close enough to be a
/// likely typo.
pub(crate) fn did_you_mean<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = (word.chars().count() / 3).max(1);
    candidates
        .into_iter()
        .map(|c| (edit_distance(word, c), c))
        .filter(|&(d, _)| d <= limit)
        .min_by_key(|&(d, _)| d)
        .map(|(_, c)| c)
}

/// Edit distance, counting a swap of neighbours as one edit (optimal
/// string alignment): `form` is one edit from `from`.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// "did you mean `x`? " if there's a near miss, then "known: a, b, c".
pub(crate) fn suggest<'a>(
    word: &str,
    what: &str,
    candidates: impl IntoIterator<Item = &'a str> + Clone,
) -> String {
    let close = did_you_mean(word, candidates.clone());
    let list: Vec<String> = candidates.into_iter().map(|c| format!("`{c}`")).collect();
    let known = if list.is_empty() {
        format!("there are no {what}")
    } else {
        format!("{what}: {}", list.join(", "))
    };
    match close {
        Some(c) => format!("did you mean `{c}`? ({known})"),
        None => known,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_misses() {
        assert_eq!(did_you_mean("lenght", ["length", "from"]), Some("length"));
        assert_eq!(did_you_mean("zzz", ["length", "from"]), None);
        assert_eq!(did_you_mean("form", ["to", "from"]), Some("from"));
        assert_eq!(
            suggest("lenght", "properties", ["length", "from"]),
            "did you mean `length`? (properties: `length`, `from`)"
        );
    }
}
