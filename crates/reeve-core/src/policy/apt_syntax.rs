//! apt's own syntax, read the way apt reads it: its booleans
//! (`StringToBool` in apt-pkg/contrib/strutl.cc), its command line
//! (apt-pkg/contrib/cmndline.cc), and the package arguments it expands
//! (apt-pkg/cacheset.cc for globs and regexes, apt-pkg/cachefilter-patterns.cc
//! for apt-patterns(7)).
//!
//! The policy needs three answers from these: is this run a simulation,
//! does it switch off the checks that keep essential packages, and can a
//! package argument take a package the system can't lose. Each is read
//! from apt's grammar rather than from a list of spellings, so a form that
//! isn't listed here still reads the way apt reads it.

use regex::Regex;

// ── booleans ──

/// apt's `StringToBool`: a number `strtol` (base 0) reads to the end and
/// that is 0 or 1 (`1`, `01`, `0x1`, `+1`, ` 1`, and an empty string as 0),
/// else yes/true/with/on/enable and no/false/without/off/disable in any
/// case. Anything else is neither.
pub(crate) fn string_to_bool(s: &str) -> Option<bool> {
    if let Some(n) = strtol_whole(s) {
        return match n {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
    }
    match s.to_ascii_lowercase().as_str() {
        "yes" | "true" | "with" | "on" | "enable" => Some(true),
        "no" | "false" | "without" | "off" | "disable" => Some(false),
        _ => None,
    }
}

/// `strtol(s, &end, 0)` when `end` is the end of `s`: leading C-locale
/// whitespace, a sign, then `0x` hex, `0` octal, or decimal digits.
fn strtol_whole(s: &str) -> Option<i64> {
    if s.is_empty() {
        return Some(0);
    }
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (radix, digits) = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => (16, hex),
        None if t.starts_with('0') => (8, t),
        None => (10, t),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let n = i64::from_str_radix(digits, radix).ok()?;
    Some(if negative { -n } else { n })
}

// ── the command line ──

/// An apt command line, read as apt's `CommandLine::Parse` reads it: short
/// clusters whose argument-taking letter takes the rest of the word or the
/// next one (`-c/tmp/x`, `-yc /tmp/x`, `-oK=V`), long names in any case
/// with `=` or the next word, `--no-foo` as `foo` off, `--` ending options,
/// and a boolean that takes its sense from the next word when that word is
/// one `StringToBool` reads (`-s 0` is not a simulation).
#[derive(Debug, Default)]
pub(crate) struct AptLine {
    /// Words that aren't options: the verb, then packages.
    pub positional: Vec<String>,
    /// `-o Name=Value` items, the name lower-cased.
    pub config: Vec<(String, String)>,
    /// Files named by `-c` / `--config-file`.
    pub config_files: Vec<String>,
    /// Boolean options by their long name (`simulate`, `yes`,
    /// `allow-remove-essential`, `force-yes`, …) with the sense apt gave them.
    pub switches: Vec<(String, bool)>,
}

#[derive(Clone, Debug)]
enum Kind {
    /// A boolean, by the name it's recorded under (`""` for one nobody asks about).
    Bool(String),
    /// Takes an argument the policy doesn't read (`-t`, `--solver`).
    Arg,
    /// `-q`: a level, which takes a number if one follows.
    Level,
    /// `-c FILE`.
    Config,
    /// `-o Name=Value`.
    Item,
}

fn long_kind(name: &str) -> Option<Kind> {
    const LONG: &[(&str, &str)] = &[
        ("simulate", "simulate"),
        ("just-print", "simulate"),
        ("dry-run", "simulate"),
        ("recon", "simulate"),
        ("no-act", "simulate"),
        ("yes", "yes"),
        ("assume-yes", "yes"),
        ("allow-remove-essential", "allow-remove-essential"),
        ("force-yes", "force-yes"),
    ];
    const ARG: &[&str] = &[
        "target-release",
        "default-release",
        "host-architecture",
        "build-profiles",
        "snapshot",
        "solver",
        "planner",
        "comment",
        "error-on",
        "with-source",
        "cli-version",
    ];
    if let Some((_, canon)) = LONG.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return Some(Kind::Bool((*canon).to_string()));
    }
    if ARG.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        return Some(Kind::Arg);
    }
    match name.to_ascii_lowercase().as_str() {
        "quiet" | "silent" => Some(Kind::Level),
        "config-file" => Some(Kind::Config),
        "option" => Some(Kind::Item),
        _ => None,
    }
}

fn short_kind(c: char) -> Kind {
    match c {
        'c' => Kind::Config,
        'o' => Kind::Item,
        't' | 'a' | 'P' | 'S' => Kind::Arg,
        'q' => Kind::Level,
        's' => Kind::Bool("simulate".into()),
        'y' => Kind::Bool("yes".into()),
        _ => Kind::Bool(String::new()),
    }
}

impl AptLine {
    pub(crate) fn parse(args: &[String]) -> Self {
        let mut line = AptLine::default();
        let mut i = 0;
        while i < args.len() {
            let w = args[i].as_str();
            i += 1;
            let Some(opt) = w.strip_prefix('-') else {
                line.positional.push(w.to_string());
                continue;
            };
            if opt == "-" {
                line.positional.extend(args[i..].iter().cloned());
                break;
            }
            if let Some(long) = opt.strip_prefix('-') {
                let (name, attached) = match long.split_once('=') {
                    Some((n, v)) => (n, Some(v)),
                    None => (long, None),
                };
                // The exact name, else the text after the first dash
                // (`--no-simulate`) as a long name or a single letter, with
                // the text before it as the sense.
                let (kind, prefix) = match long_kind(name) {
                    Some(k) => (k, None),
                    None => {
                        let retry = name.split_once('-').and_then(|(before, after)| {
                            let mut letters = after.chars();
                            let k = long_kind(after).or_else(|| {
                                match (letters.next(), letters.next()) {
                                    (Some(c), None) => Some(short_kind(c)),
                                    _ => None,
                                }
                            })?;
                            Some((k, Some(before)))
                        });
                        retry.unwrap_or((Kind::Bool(name.to_ascii_lowercase()), None))
                    }
                };
                line.take(kind, attached, attached.is_some(), prefix, args, &mut i);
                continue;
            }
            // A short cluster (`-` alone is nothing): a letter's argument is
            // what follows `=`, else the rest of the word, else the next word.
            let mut rest = opt;
            while let Some(c) = rest.chars().next() {
                rest = &rest[c.len_utf8()..];
                let (attached, certain) = match rest.strip_prefix('=') {
                    Some(v) => (Some(v), true),
                    None => ((!rest.is_empty()).then_some(rest), false),
                };
                if line.take(short_kind(c), attached, certain, None, args, &mut i) {
                    break;
                }
            }
        }
        line
    }

    /// One option, as apt's `HandleOpt` takes it. `attached` is the text
    /// after `=` (`certain`) or the rest of a short cluster; without it the
    /// next word serves when it doesn't start with `-`. `prefix` is the text
    /// before the dash of a `--no-foo` form. Returns whether the rest of the
    /// cluster was used up.
    fn take(
        &mut self,
        kind: Kind,
        attached: Option<&str>,
        certain: bool,
        prefix: Option<&str>,
        args: &[String],
        i: &mut usize,
    ) -> bool {
        let (arg, from_next) = match attached {
            Some(a) => (Some(a.to_string()), false),
            None => (args.get(*i).filter(|w| !w.starts_with('-')).cloned(), true),
        };
        match kind {
            Kind::Arg | Kind::Config | Kind::Item => {
                // Without an argument apt stops with an error.
                let Some(a) = arg else { return true };
                if from_next {
                    *i += 1;
                }
                match kind {
                    Kind::Config => self.config_files.push(a),
                    Kind::Item => {
                        if let Some((k, v)) = a.split_once('=') {
                            self.config.push((k.to_ascii_lowercase(), v.to_string()));
                        }
                    }
                    _ => {}
                }
                true
            }
            Kind::Level => {
                let numeric = arg
                    .as_deref()
                    .is_some_and(|a| !a.is_empty() && a.chars().all(|c| c.is_ascii_digit()));
                if numeric && from_next {
                    *i += 1;
                }
                // Not a number: the level goes up by one and the cluster goes on.
                numeric
            }
            Kind::Bool(name) => {
                let mut sense = None;
                let mut used_rest = false;
                if let Some(a) = &arg {
                    match string_to_bool(a) {
                        Some(s) => {
                            sense = Some(s);
                            if from_next {
                                *i += 1;
                            } else {
                                used_rest = true;
                            }
                        }
                        // `--foo=bogus`: apt stops with an error.
                        None if certain => sense = Some(true),
                        None => {}
                    }
                }
                if sense.is_none() {
                    sense = prefix.and_then(string_to_bool);
                }
                if !name.is_empty() {
                    self.switches.push((name, sense.unwrap_or(true)));
                }
                used_rest
            }
        }
    }

    /// The sense of a boolean option, last one wins; off when it isn't there.
    pub(crate) fn on(&self, name: &str) -> bool {
        self.switches
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .is_some_and(|(_, sense)| *sense)
    }

    /// Whether a `-o` item (lower-cased name) was set to something apt reads as true.
    pub(crate) fn config_on(&self, key: &str) -> bool {
        self.config
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .is_some_and(|(_, v)| string_to_bool(v) == Some(true))
    }

    /// The values of a `-o` item, every time it was set (`Dpkg::Options::`).
    pub(crate) fn config_values(&self, key: &str) -> Vec<String> {
        self.config
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

// ── package arguments ──

/// Shell-style glob, case-folded like apt's `fnmatch(…, FNM_CASEFOLD)`:
/// `*`, `?`, and `[set]` (ranges, `!`/`^` negation).
pub(crate) fn glob_match(pat: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|k| go(&p[1..], &t[k..])),
            Some('?') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some('[') => {
                let Some(close) = p.iter().skip(2).position(|c| *c == ']').map(|i| i + 2) else {
                    return t.first() == Some(&'[') && go(&p[1..], &t[1..]);
                };
                let Some(&c) = t.first() else {
                    return false;
                };
                let set = &p[1..close];
                let (negate, set) = match set.first() {
                    Some('!' | '^') => (true, &set[1..]),
                    _ => (false, set),
                };
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        hit |= set[i] <= c && c <= set[i + 2];
                        i += 3;
                    } else {
                        hit |= set[i] == c;
                        i += 1;
                    }
                }
                hit != negate && go(&p[close + 1..], &t[1..])
            }
            Some(&c) => t.first() == Some(&c) && go(&p[1..], &t[1..]),
        }
    }
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    go(&p, &t)
}

/// What a package argument can take from `names`, read as apt reads an
/// argument that isn't an exact package name (`CacheSetHelper::PackageFromString`):
/// an apt pattern when it starts with `~` or `?`; else a case-folded glob
/// when it has one of `.?*[]!`; else apt's unanchored, case-insensitive
/// regex when it has one of `.?+*|[^$`. A `:arch` suffix without pattern
/// characters is split off first, as apt splits it. The first name
/// selected, or the error apt itself would stop on.
pub(crate) fn selects<'a>(arg: &str, names: &[&'a str]) -> Result<Option<&'a str>, String> {
    if arg.starts_with(['~', '?']) {
        let pat = parse(arg)?;
        return Ok(names.iter().copied().find(|n| eval(&pat, n) != Sel::No));
    }
    const FNMATCH: &[char] = &['.', '?', '*', '[', ']', '!'];
    const REGEX: &[char] = &['.', '?', '+', '*', '|', '[', '^', '$'];
    if arg.contains(FNMATCH) {
        let glob = without_arch(arg, FNMATCH);
        if let Some(hit) = names.iter().copied().find(|n| glob_match(glob, n)) {
            return Ok(Some(hit));
        }
    }
    if arg.contains(REGEX) {
        let re = without_arch(arg, REGEX);
        if let Ok(r) = Regex::new(&format!("(?i){re}")) {
            return Ok(names.iter().copied().find(|n| r.is_match(n)));
        }
    }
    Ok(None)
}

/// apt takes the text after the last `:` as an architecture when it has
/// none of the pattern's special characters.
fn without_arch<'s>(pattern: &'s str, special: &[char]) -> &'s str {
    match pattern.rfind(':') {
        Some(at) if !pattern[at + 1..].contains(special) => &pattern[..at],
        _ => pattern,
    }
}

/// What a pattern says about one package: it selects it, it doesn't, or
/// Reeve can't tell (the term is about state or fields it doesn't have).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sel {
    Yes,
    No,
    Maybe,
}

/// An apt pattern, reduced to what matters for a name: logic, name tests,
/// terms that hold for every installed package, terms that hold for none,
/// and terms Reeve can't evaluate.
#[derive(Debug)]
pub(crate) enum Pat {
    And(Vec<Pat>),
    Or(Vec<Pat>),
    Not(Box<Pat>),
    /// `?name(REGEX)` / `~nREGEX`: unanchored, case-insensitive.
    Name(String),
    /// `?exact-name(NAME)`.
    Exact(String),
    /// `?installed`, `?true`: every package asked about is installed.
    Yes,
    /// `?false`, `?virtual`, `?config-files`: never an installed package.
    No,
    /// `?architecture`, `?version`, `?essential`, `?garbage`, dependencies, …
    Unknown,
}

pub(crate) fn eval(p: &Pat, name: &str) -> Sel {
    match p {
        Pat::And(ps) => {
            let r: Vec<Sel> = ps.iter().map(|p| eval(p, name)).collect();
            if r.contains(&Sel::No) {
                Sel::No
            } else if r.contains(&Sel::Maybe) {
                Sel::Maybe
            } else {
                Sel::Yes
            }
        }
        Pat::Or(ps) => {
            let r: Vec<Sel> = ps.iter().map(|p| eval(p, name)).collect();
            if r.contains(&Sel::Yes) {
                Sel::Yes
            } else if r.contains(&Sel::Maybe) {
                Sel::Maybe
            } else {
                Sel::No
            }
        }
        Pat::Not(p) => match eval(p, name) {
            Sel::Yes => Sel::No,
            Sel::No => Sel::Yes,
            Sel::Maybe => Sel::Maybe,
        },
        Pat::Name(re) => match Regex::new(&format!("(?i){re}")) {
            Ok(r) if r.is_match(name) => Sel::Yes,
            Ok(_) => Sel::No,
            Err(_) => Sel::Maybe,
        },
        Pat::Exact(n) => {
            if n == name {
                Sel::Yes
            } else {
                Sel::No
            }
        }
        Pat::Yes => Sel::Yes,
        Pat::No => Sel::No,
        Pat::Unknown => Sel::Maybe,
    }
}

/// Parse an apt pattern (apt-patterns(7)) the way apt's `PatternTreeParser`
/// does: `|` is or, juxtaposition is and, `!` is not, `(…)` groups;
/// `~x` short forms take a word that ends at space or `!?~|,()`; `?term(…)`
/// long forms take comma-separated words, quoted words, or patterns. What
/// apt would reject is an error.
pub(crate) fn parse(s: &str) -> Result<Pat, String> {
    let mut p = Parser {
        c: s.chars().collect(),
        i: 0,
    };
    p.space();
    let node = p.or()?;
    p.space();
    if p.i != p.c.len() {
        return Err(format!("expected the end of the pattern at {}", p.rest()));
    }
    Ok(node)
}

struct Parser {
    c: Vec<char>,
    i: usize,
}

enum Arg {
    Word(String),
    Pat(Pat),
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn rest(&self) -> String {
        self.c[self.i..].iter().collect()
    }

    fn space(&mut self) {
        while self.peek() == Some(' ') {
            self.i += 1;
        }
    }

    fn or(&mut self) -> Result<Pat, String> {
        let mut v = vec![self.and()?];
        loop {
            self.space();
            if self.peek() != Some('|') {
                break;
            }
            self.i += 1;
            self.space();
            v.push(self.and()?);
        }
        Ok(if v.len() == 1 {
            v.pop().expect("one")
        } else {
            Pat::Or(v)
        })
    }

    fn and(&mut self) -> Result<Pat, String> {
        let mut v = Vec::new();
        loop {
            self.space();
            match self.peek() {
                Some('!' | '~' | '?' | '(') => v.push(self.unary()?),
                _ => break,
            }
        }
        if v.is_empty() {
            return Err(format!("expected a pattern at {}", self.rest()));
        }
        Ok(if v.len() == 1 {
            v.pop().expect("one")
        } else {
            Pat::And(v)
        })
    }

    fn unary(&mut self) -> Result<Pat, String> {
        if self.peek() == Some('!') {
            self.i += 1;
            return Ok(Pat::Not(Box::new(self.primary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Pat, String> {
        match self.peek() {
            Some('(') => {
                self.i += 1;
                let node = self.or()?;
                self.space();
                if self.peek() != Some(')') {
                    return Err("expected ) to close a group".into());
                }
                self.i += 1;
                Ok(node)
            }
            Some('~') => self.short(),
            Some('?') => self.long(),
            _ => Err(format!("expected a pattern at {}", self.rest())),
        }
    }

    fn short(&mut self) -> Result<Pat, String> {
        // apt's table, in its order: the first entry that matches wins.
        const SHORT: &[(&str, &str, bool)] = &[
            ("r", "?architecture", true),
            ("A", "?archive", true),
            ("M", "?automatic", false),
            ("b", "?broken", false),
            ("c", "?config-files", false),
            ("DDepends:", "?depends", true),
            ("DPre-Depends:", "?pre-depends", true),
            ("DSuggests:", "?suggests", true),
            ("DRecommends:", "?recommends", true),
            ("DConflicts:", "?conflicts", true),
            ("DReplaces:", "?replaces", true),
            ("DObsoletes:", "?obsoletes", true),
            ("DBreaks:", "?breaks", true),
            ("DEnhances:", "?enhances", true),
            ("D", "?depends", true),
            ("RDepends:", "?reverse-depends", true),
            ("RPre-Depends:", "?reverse-pre-depends", true),
            ("RSuggests:", "?reverse-suggests", true),
            ("RRecommends:", "?reverse-recommends", true),
            ("RConflicts:", "?reverse-conflicts", true),
            ("RReplaces:", "?reverse-replaces", true),
            ("RObsoletes:", "?reverse-obsoletes", true),
            ("RBreaks:", "?reverse-breaks", true),
            ("REnhances:", "?reverse-enhances", true),
            ("R", "?reverse-depends", true),
            ("E", "?essential", false),
            ("F", "?false", false),
            ("g", "?garbage", false),
            ("i", "?installed", false),
            ("n", "?name", true),
            ("o", "?obsolete", false),
            ("O", "?origin", true),
            ("p", "?priority", true),
            ("s", "?section", true),
            ("e", "?source-package", true),
            ("T", "?true", false),
            ("U", "?upgradable", false),
            ("V", "?version", true),
            ("v", "?virtual", false),
        ];
        self.i += 1;
        let rest = self.rest();
        for (name, term, takes) in SHORT {
            if !rest.starts_with(name) {
                continue;
            }
            self.i += name.chars().count();
            let args = if *takes {
                vec![self.argument(true)?]
            } else {
                Vec::new()
            };
            return build(term, args);
        }
        Err(format!(
            "unknown short pattern ~{}",
            rest.chars().next().unwrap_or(' ')
        ))
    }

    fn long(&mut self) -> Result<Pat, String> {
        let start = self.i;
        self.i += 1;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '-') {
            self.i += 1;
        }
        let term: String = self.c[start..self.i].iter().collect();
        if term.len() <= 1 {
            return Err("a pattern needs a name after ?".into());
        }
        let mut args = Vec::new();
        if self.peek() == Some('(') {
            self.i += 1;
            self.space();
            if self.peek() == Some(')') {
                self.i += 1;
            } else {
                args.push(self.argument(false)?);
                self.space();
                while self.peek() == Some(',') {
                    self.i += 1;
                    self.space();
                    if self.peek() == Some(')') {
                        break;
                    }
                    args.push(self.argument(false)?);
                    self.space();
                }
                if self.peek() != Some(')') {
                    return Err(format!("expected ) to close {term}"));
                }
                self.i += 1;
            }
        }
        build(&term, args)
    }

    /// A quoted word, a word, or a pattern. A word can't start with
    /// `!?~|,()` or a space; a short form's word also ends at those, a long
    /// form's only at `|,()`.
    fn argument(&mut self, short: bool) -> Result<Arg, String> {
        if self.peek() == Some('"') {
            self.i += 1;
            let start = self.i;
            while let Some(c) = self.peek() {
                if c == '"' {
                    let w: String = self.c[start..self.i].iter().collect();
                    self.i += 1;
                    return Ok(Arg::Word(w));
                }
                self.i += 1;
            }
            return Err("an unclosed quote".into());
        }
        const START: &[char] = &['!', '?', '~', '|', ',', '(', ')', ' ', '\0'];
        const LONG: &[char] = &['|', ',', '(', ')', '\0'];
        if let Some(c) = self.peek() {
            if !START.contains(&c) {
                let stop = if short { START } else { LONG };
                let start = self.i;
                while matches!(self.peek(), Some(c) if !stop.contains(&c)) {
                    self.i += 1;
                }
                return Ok(Arg::Word(self.c[start..self.i].iter().collect()));
            }
        }
        Ok(Arg::Pat(self.or()?))
    }
}

/// A term with its arguments, checked for the count apt requires.
fn build(term: &str, args: Vec<Arg>) -> Result<Pat, String> {
    let n = args.len();
    let need = |count: usize| -> Result<(), String> {
        if n == count {
            Ok(())
        } else {
            Err(format!("{term} takes {count} argument(s), not {n}"))
        }
    };
    fn patterns(term: &str, args: Vec<Arg>) -> Result<Vec<Pat>, String> {
        args.into_iter()
            .map(|a| match a {
                Arg::Pat(p) => Ok(p),
                Arg::Word(w) => Err(format!("{term} takes a pattern, not the word {w}")),
            })
            .collect()
    }
    fn word(term: &str, args: Vec<Arg>) -> Result<String, String> {
        match args.into_iter().next() {
            Some(Arg::Word(w)) => Ok(w),
            _ => Err(format!("{term} takes a word")),
        }
    }
    match term {
        "?and" | "?narrow" => Ok(Pat::And(patterns(term, args)?)),
        "?or" => Ok(Pat::Or(patterns(term, args)?)),
        "?not" => {
            need(1)?;
            Ok(Pat::Not(Box::new(patterns(term, args)?.remove(0))))
        }
        "?any-version" | "?all-versions" => {
            need(1)?;
            Ok(patterns(term, args)?.remove(0))
        }
        "?name" => {
            need(1)?;
            Ok(Pat::Name(word(term, args)?))
        }
        "?exact-name" => {
            need(1)?;
            Ok(Pat::Exact(word(term, args)?))
        }
        "?installed" | "?true" => {
            need(0)?;
            Ok(Pat::Yes)
        }
        "?false" | "?virtual" | "?config-files" => {
            need(0)?;
            Ok(Pat::No)
        }
        "?architecture" | "?version" | "?section" | "?priority" | "?origin" | "?archive"
        | "?codename" | "?source-package" | "?source-version" => {
            need(1)?;
            word(term, args)?;
            Ok(Pat::Unknown)
        }
        "?essential" | "?automatic" | "?broken" | "?garbage" | "?obsolete" | "?upgradable"
        | "?phasing" | "?security" => {
            need(0)?;
            Ok(Pat::Unknown)
        }
        "?depends"
        | "?pre-depends"
        | "?suggests"
        | "?recommends"
        | "?conflicts"
        | "?replaces"
        | "?obsoletes"
        | "?breaks"
        | "?enhances"
        | "?reverse-depends"
        | "?reverse-pre-depends"
        | "?reverse-suggests"
        | "?reverse-recommends"
        | "?reverse-conflicts"
        | "?reverse-replaces"
        | "?reverse-obsoletes"
        | "?reverse-breaks"
        | "?reverse-enhances" => {
            need(1)?;
            Ok(Pat::Unknown)
        }
        _ => Err(format!("unknown pattern {term}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn booleans_read_like_apt() {
        for s in [
            "1", "01", "0x1", "0X01", "+1", " 1", "\t1", "yes", "TRUE", "With", "on", "enable",
        ] {
            assert_eq!(string_to_bool(s), Some(true), "{s:?}");
        }
        for s in [
            "0", "00", "-0", "0x0", "", "no", "False", "without", "off", "DISABLE",
        ] {
            assert_eq!(string_to_bool(s), Some(false), "{s:?}");
        }
        for s in [
            "2", "0x2", "-1", "1 ", "08", "0x", "1e0", "0ad", "y", "t", "+", " ",
        ] {
            assert_eq!(string_to_bool(s), None, "{s:?}");
        }
    }

    fn line(s: &str) -> AptLine {
        AptLine::parse(&s.split(' ').map(String::from).collect::<Vec<_>>())
    }

    #[test]
    fn the_command_line_reads_like_apt() {
        let l = line("-y -o X=1 install a -- -b");
        assert_eq!(l.positional, ["install", "a", "-b"]);
        assert_eq!(l.config, [("x".to_string(), "1".to_string())]);
        // -c with the file attached, after other letters, or as the next word.
        for s in [
            "-c/tmp/x remove sed",
            "-yc/tmp/x remove sed",
            "-yc /tmp/x remove sed",
            "--config-file=/tmp/x remove sed",
            "--Config-File /tmp/x remove sed",
            "-c=/tmp/x remove sed",
        ] {
            let l = line(s);
            assert_eq!(l.config_files, ["/tmp/x"], "{s}");
            assert_eq!(l.positional, ["remove", "sed"], "{s}");
        }
        // -o glued, clustered, or separate; the name is lower-cased.
        for s in [
            "-oDpkg::Options::=--force-all remove x",
            "-yoDpkg::Options::=--force-all remove x",
            "--option=DPKG::Options::=--force-all remove x",
            "-o Dpkg::Options::=--force-all remove x",
        ] {
            assert_eq!(
                line(s).config_values("dpkg::options::"),
                ["--force-all"],
                "{s}"
            );
        }
        // A boolean takes its sense from the next word when apt reads it as one.
        assert!(line("-s remove htop").on("simulate"));
        assert!(line("-ys remove htop").on("simulate"));
        assert!(!line("-s 0 remove htop").on("simulate"));
        assert_eq!(line("-s 0 remove htop").positional, ["remove", "htop"]);
        assert!(!line("--simulate no remove htop").on("simulate"));
        assert!(!line("--no-simulate remove htop").on("simulate"));
        assert!(!line("--dry-run=false remove htop").on("simulate"));
        assert!(line("-s remove 0ad").on("simulate"));
        assert_eq!(line("-s remove 0ad").positional, ["remove", "0ad"]);
        assert!(line("install -y htop").on("yes"));
        assert_eq!(line("install -y htop").positional, ["install", "htop"]);
        assert!(line("--allow-remove-essential remove sed").on("allow-remove-essential"));
        assert!(line("--Allow-Remove-Essential=0x1 remove sed").on("allow-remove-essential"));
        assert!(!line("--allow-remove-essential=false remove sed").on("allow-remove-essential"));
        assert!(!line("--no-allow-remove-essential remove sed").on("allow-remove-essential"));
        // -q takes a number, not a word; -t, -a, -P, -S take the next word.
        assert_eq!(line("-q 2 remove htop").positional, ["remove", "htop"]);
        assert_eq!(line("-qq remove htop").positional, ["remove", "htop"]);
        assert_eq!(line("-q remove htop").positional, ["remove", "htop"]);
        assert_eq!(
            line("-t bookworm-backports install x").positional,
            ["install", "x"]
        );
        assert_eq!(line("-a arm64 build-dep x").positional, ["build-dep", "x"]);
        // A `-o` item that is read as true, and the ones that aren't.
        assert!(
            line("-o APT::Get::allow-remove-essential=0x1 remove sed")
                .config_on("apt::get::allow-remove-essential")
        );
        assert!(
            line("-o APT::Get::allow-remove-essential=+1 remove sed")
                .config_on("apt::get::allow-remove-essential")
        );
        let spaced = AptLine::parse(
            &["-o", "APT::Get::allow-remove-essential= 1", "remove", "sed"].map(String::from),
        );
        assert!(spaced.config_on("apt::get::allow-remove-essential"));
        assert!(
            !line("-o APT::Get::allow-remove-essential=0 remove sed")
                .config_on("apt::get::allow-remove-essential")
        );
        assert!(
            !line("-o APT::Get::allow-remove-essential=2 remove sed")
                .config_on("apt::get::allow-remove-essential")
        );
        assert!(
            !line("-o APT::Get::allow-remove-essential= remove sed")
                .config_on("apt::get::allow-remove-essential")
        );
    }

    #[test]
    fn globs_fold_case() {
        assert!(glob_match("linux*", "linux-image-6.1.0-37-amd64"));
        assert!(glob_match("LINUX*", "linux-image-6.1.0-37-amd64"));
        assert!(glob_match("*6.1.0-37-AMD64", "linux-image-6.1.0-37-amd64"));
        assert!(glob_match(
            "linux-image-6.1.0-3?-amd64",
            "linux-image-6.1.0-37-amd64"
        ));
        assert!(glob_match(
            "l[a-m]nux-image-*",
            "linux-image-6.1.0-37-amd64"
        ));
        assert!(!glob_match("l[!i]nux*", "linux-image-6.1.0-37-amd64"));
        assert!(!glob_match(
            "linux-image-6.1.0-36-*",
            "linux-image-6.1.0-37-amd64"
        ));
    }

    #[test]
    fn patterns_parse_and_evaluate_like_apt() {
        let image = "linux-image-6.1.0-37-amd64";
        let names = [image, "libc6", "htop"];
        let takes = |p: &str| selects(p, &names).unwrap();
        // Name tests, in any case, anchored or not.
        for p in [
            "~nlinux",
            "~nLINUX",
            "?name(linux)",
            "?name(^linux-image-)",
            "~nl[i]nux-image",
            "?name(\"linux-image\")",
        ] {
            assert_eq!(takes(p), Some(image), "{p}");
        }
        for p in [
            "~nhtop",
            "?name(^htop$)",
            "?exact-name(htop)",
            "~nhtop~i",
            "?and(~nhtop,~i)",
            "htop*",
        ] {
            assert_eq!(takes(p), Some("htop"), "{p}");
        }
        for p in ["?not(~i)", "~F", "~v", "?config-files", "~nnothing"] {
            assert_eq!(takes(p), None, "{p}");
        }
        // State and architecture terms can take anything installed.
        for p in [
            "~i",
            "?installed",
            "~T",
            "~E",
            "~ramd64",
            "?architecture(amd64)",
            "~nlinux~i",
            "~nlinux ~i",
            "?and(~nlinux,~i)",
            "?and(~nlinux, ~i)",
            "?not(?name(^htop$))",
            "?installed !~nhtop",
            "~nlinux|~nhtop",
            "~i (~nlinux)",
            "?narrow(~i,~nlinux)",
            "?any-version(~nlinux)",
            "~nlinux~V6",
            "?garbage",
        ] {
            assert_eq!(takes(p), Some(image), "{p}");
        }
        assert_eq!(takes("?not(~nlinux)"), Some("libc6"));
        assert_eq!(takes("~nlib|~nhtop"), Some("libc6"));
        // What apt rejects is an error.
        for p in [
            "~",
            "~x",
            "?",
            "?name",
            "?name()",
            "?name(a,b)",
            "?nope(x)",
            "?name(linux",
            "~i|",
            "~i)",
            "~nlinux htop",
            "?name(linux):amd64",
            "?not(~i,~i)",
        ] {
            assert!(selects(p, &names).is_err(), "{p}");
        }
        // Globs and regexes, case-folded, with an arch suffix split off.
        for p in [
            "linux*",
            "LINUX*",
            "*6.1.0-37-AMD64",
            "linux*:amd64",
            "LINUX-IMAGE-6.1.0-37-amd64",
            "linux-image-6\\.1\\.0-37-amd64",
            "linux-image-6.1.0-37-amd64|htop",
            "^linux-image-.*:amd64",
            "6.1.0-37*",
            "linux-image-6.1.0-3?-amd64",
        ] {
            assert_eq!(takes(p), Some(image), "{p}");
        }
        for p in [
            "linux-image-6.1.0-36-amd64",
            "LINUX-IMAGE-6.1.0-36-AMD64",
            "python3.11",
            "libfoo.*",
            "vim*",
            "linux-image-6.1.0-36-*",
            "libstdc++6",
            "libc6",
        ] {
            assert_eq!(takes(p), None, "{p}");
        }
        assert_eq!(takes("lib*"), Some("libc6"));
        assert_eq!(takes("^lib"), Some("libc6"));
    }
}
