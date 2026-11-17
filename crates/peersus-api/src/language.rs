//! Languages: the tag a person's language is known by, and what a language file holds.
//!
//! The application's own words are written in English, in its code; English needs no file and is always there. Every other
//! language is a file in the gettext PO format (`msgid` / `msgstr`, the format Poedit and Weblate edit), keyed by the English
//! text itself. The same format serves the application and an extension: an extension's package may hold its own language
//! files for the words its manifest declares, and an extension that learns the person's language (see `docs/api/`) can read
//! files of its own with [`Catalog::parse`] for the words it writes at run time.
//!
//! A language file is untrusted: it may come from a download or from a person. [`Catalog::parse`] reads it within fixed bounds
//! and refuses what it does not understand, never panics, and [`Catalog::problems`] says where a translation does not fit its
//! English text (a missing or extra `{placeholder}`, the wrong number of plural forms, text that is too long). A plural rule is
//! the gettext expression of the file's header, read by a small parser of that grammar alone (integers, `n`, arithmetic,
//! comparisons, `!`, `&&`, `||`, `?:`), bounded in size and depth, and checked over a range of counts before it is accepted:
//! nothing in a file is ever run.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{borrow::Cow, collections::HashMap, fmt, str::FromStr};

/// The longest language tag, in bytes: room for a language, a script and a region (`sr-latn-rs`, `zh-hant-tw`).
pub const MAX_TAG: usize = 15;
/// The largest language file read, in bytes: several times the application's own (some 60 KiB), and small enough to pass
/// whole between a window and the application.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// The most messages one file may hold.
pub const MAX_MESSAGES: usize = 20_000;
/// The longest translation of one message, in characters.
pub const MAX_TRANSLATION_CHARS: usize = 4096;
/// The longest name a language gives itself, in characters.
pub const MAX_NAME_CHARS: usize = 40;
/// The most plural forms a language may have.
pub const MAX_FORMS: usize = 6;

/// A language, by its tag: a primary language of 2 or 3 letters, then optional subtags of 1 to 8 letters or digits, joined by
/// `-` (`ro`, `pt-br`, `sr-latn`). Kept lowercase, so two spellings of one tag are one language. Small and `Copy`: the tag is
/// held inline, so a language can be handed around as freely as a number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Language {
    bytes: [u8; MAX_TAG],
    len: u8,
}

/// A language tag that is not one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a language tag")]
pub struct BadTag;

impl Language {
    /// The language the application is written in: always there, needing no file.
    pub const ENGLISH: Self = {
        let mut bytes = [0; MAX_TAG];
        bytes[0] = b'e';
        bytes[1] = b'n';
        Self { bytes, len: 2 }
    };

    /// A tag as a person or a file writes it: `-` or `_` between subtags, any case.
    pub fn parse(text: &str) -> Result<Self, BadTag> {
        if text.is_empty() || text.len() > MAX_TAG {
            return Err(BadTag);
        }
        let mut bytes = [0; MAX_TAG];
        for (index, subtag) in text.split(['-', '_']).enumerate() {
            let fits = (1..=8).contains(&subtag.len())
                && if index == 0 {
                    subtag.len() <= 3
                        && subtag.len() >= 2
                        && subtag.bytes().all(|byte| byte.is_ascii_alphabetic())
                } else {
                    subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
                };
            if !fits {
                return Err(BadTag);
            }
        }
        for (slot, byte) in bytes.iter_mut().zip(text.bytes()) {
            *slot = if byte == b'_' {
                b'-'
            } else {
                byte.to_ascii_lowercase()
            };
        }
        Ok(Self {
            bytes,
            len: text.len() as u8,
        })
    }

    /// The language of a POSIX locale name as the environment gives it (`ro_RO.UTF-8`, `pt_BR@euro`), or of a BCP 47 tag as a
    /// platform gives it (`ro-RO`). `None` for the locales that name no language (`C`, `POSIX`) and for what is not a locale.
    pub fn from_locale(locale: &str) -> Option<Self> {
        let name = locale.split(['.', '@']).next().unwrap_or_default();
        if name.eq_ignore_ascii_case("c") || name.eq_ignore_ascii_case("posix") {
            return None;
        }
        Self::parse(name).ok()
    }

    /// The tag, lowercase, subtags joined by `-`.
    pub fn tag(&self) -> &str {
        // Only ASCII bytes are ever stored, so this cannot fail; an empty tag is the impossible case's answer.
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }

    /// The primary language alone: `pt` of `pt-br`.
    pub fn primary(&self) -> Self {
        let tag = self.tag();
        let end = tag.find('-').unwrap_or(tag.len());
        Self::parse(&tag[..end]).unwrap_or(*self)
    }

    pub fn is_english(&self) -> bool {
        self.primary() == Self::ENGLISH
    }
}

impl Default for Language {
    fn default() -> Self {
        Self::ENGLISH
    }
}

impl fmt::Display for Language {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.tag())
    }
}

impl fmt::Debug for Language {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Language({})", self.tag())
    }
}

impl FromStr for Language {
    type Err = BadTag;
    fn from_str(text: &str) -> Result<Self, BadTag> {
        Self::parse(text)
    }
}

impl Serialize for Language {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.tag())
    }
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = Cow::<str>::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// The `{name}` placeholders of a text, in order, each once: what a translation must keep.
pub fn placeholders(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('}') else { break };
        let name = &tail[..end];
        if !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && !found.contains(&name)
        {
            found.push(name);
        }
        rest = &tail[end + 1..];
    }
    found
}

/// How a language picks the form of a word for a count: the gettext `Plural-Forms` header, `nplurals=N; plural=EXPR;`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluralRule {
    forms: usize,
    expression: Vec<Node>,
}

/// One node of a plural expression, its operands by index into the expression (always earlier nodes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Node {
    Count,
    Number(u64),
    Not(usize),
    Binary(Operator, usize, usize),
    Choose(usize, usize, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operator {
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

/// The most nodes a plural expression may have, and how deeply it may nest.
const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 32;
/// The counts a rule is tried on before it is accepted: every form it gives must exist.
const TRIED_COUNTS: std::ops::RangeInclusive<u64> = 0..=1100;

impl PluralRule {
    /// English and every language with one form for one and another for the rest.
    pub fn one_other() -> Self {
        Self::parse("nplurals=2; plural=(n != 1);").unwrap_or(Self {
            forms: 1,
            expression: vec![Node::Number(0)],
        })
    }

    /// A `Plural-Forms` header value. Refused unless it parses, names 1 to [`MAX_FORMS`] forms, and gives an existing form
    /// for every count tried.
    pub fn parse(header: &str) -> Result<Self, &'static str> {
        let mut forms = None;
        let mut expression = None;
        for part in header
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            let (key, value) = part
                .split_once('=')
                .ok_or("a part of the plural rule has no value")?;
            match key.trim() {
                "nplurals" => {
                    let count: usize = value
                        .trim()
                        .parse()
                        .map_err(|_| "nplurals is not a number")?;
                    if !(1..=MAX_FORMS).contains(&count) {
                        return Err("nplurals is out of range");
                    }
                    forms = Some(count);
                }
                "plural" => expression = Some(Parser::read(value)?),
                _ => return Err("the plural rule has an unknown part"),
            }
        }
        let rule = Self {
            forms: forms.ok_or("the plural rule has no nplurals")?,
            expression: expression.ok_or("the plural rule has no expression")?,
        };
        for count in TRIED_COUNTS.chain([1_000_000, u64::from(u32::MAX)]) {
            match rule.evaluate(count) {
                Some(form) if form < rule.forms as u64 => {}
                _ => return Err("the plural rule gives a form that does not exist"),
            }
        }
        Ok(rule)
    }

    /// How many forms a word has in this language.
    pub fn forms(&self) -> usize {
        self.forms
    }

    /// Which form `count` takes, always one that exists.
    pub fn form(&self, count: u64) -> usize {
        let last = self.forms.saturating_sub(1);
        self.evaluate(count)
            .map_or(last, |form| usize::try_from(form).unwrap_or(last).min(last))
    }

    /// The expression's value for `count`; `None` where it divides by zero.
    fn evaluate(&self, count: u64) -> Option<u64> {
        let mut values = Vec::with_capacity(self.expression.len());
        for node in &self.expression {
            let value = match *node {
                Node::Count => count,
                Node::Number(number) => number,
                Node::Not(operand) => u64::from(values[operand] == 0),
                Node::Choose(condition, then, otherwise) => {
                    if values[condition] != 0 {
                        values[then]
                    } else {
                        values[otherwise]
                    }
                }
                Node::Binary(operator, left, right) => {
                    let (left, right): (u64, u64) = (values[left], values[right]);
                    match operator {
                        Operator::Or => u64::from(left != 0 || right != 0),
                        Operator::And => u64::from(left != 0 && right != 0),
                        Operator::Equal => u64::from(left == right),
                        Operator::NotEqual => u64::from(left != right),
                        Operator::Less => u64::from(left < right),
                        Operator::LessOrEqual => u64::from(left <= right),
                        Operator::Greater => u64::from(left > right),
                        Operator::GreaterOrEqual => u64::from(left >= right),
                        Operator::Add => left.wrapping_add(right),
                        Operator::Subtract => left.wrapping_sub(right),
                        Operator::Multiply => left.wrapping_mul(right),
                        Operator::Divide => left.checked_div(right)?,
                        Operator::Remainder => left.checked_rem(right)?,
                    }
                }
            };
            values.push(value);
        }
        values.last().copied()
    }
}

/// A recursive-descent reader of the plural grammar, by C precedence: `?:`, `||`, `&&`, `== !=`, `< <= > >=`, `+ -`,
/// `* / %`, `!`, then `n`, a number or a bracketed expression. Every node it makes refers only to nodes made before it, so
/// evaluating them in order needs no recursion.
struct Parser<'a> {
    text: &'a [u8],
    at: usize,
    nodes: Vec<Node>,
    depth: usize,
}

type Step = Result<usize, &'static str>;

impl<'a> Parser<'a> {
    fn read(text: &'a str) -> Result<Vec<Node>, &'static str> {
        let mut parser = Self {
            text: text.as_bytes(),
            at: 0,
            nodes: Vec::new(),
            depth: 0,
        };
        parser.choose()?;
        parser.blank();
        if parser.at != parser.text.len() {
            return Err("the plural expression has text it does not understand");
        }
        Ok(parser.nodes)
    }

    fn push(&mut self, node: Node) -> Step {
        if self.nodes.len() >= MAX_NODES {
            return Err("the plural expression is too long");
        }
        self.nodes.push(node);
        Ok(self.nodes.len() - 1)
    }

    fn blank(&mut self) {
        while self.text.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    /// Takes `token` if it comes next (and is not the start of a longer one: `<` is not `<=`).
    fn take(&mut self, token: &str) -> bool {
        self.blank();
        let rest = &self.text[self.at..];
        if !rest.starts_with(token.as_bytes()) {
            return false;
        }
        let next = rest.get(token.len()).copied();
        let longer = match token {
            "<" | ">" | "!" | "=" => next == Some(b'='),
            "|" => next == Some(b'|'),
            "&" => next == Some(b'&'),
            _ => false,
        };
        if longer {
            return false;
        }
        self.at += token.len();
        true
    }

    fn nested(&mut self, step: impl FnOnce(&mut Self) -> Step) -> Step {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("the plural expression nests too deeply");
        }
        let result = step(self);
        self.depth -= 1;
        result
    }

    fn choose(&mut self) -> Step {
        self.nested(|parser| {
            let condition = parser.or()?;
            if !parser.take("?") {
                return Ok(condition);
            }
            let then = parser.choose()?;
            if !parser.take(":") {
                return Err("the plural expression has `?` without `:`");
            }
            let otherwise = parser.choose()?;
            parser.push(Node::Choose(condition, then, otherwise))
        })
    }

    fn binary(&mut self, operators: &[(&str, Operator)], next: fn(&mut Self) -> Step) -> Step {
        let mut left = next(self)?;
        'more: loop {
            for (token, operator) in operators {
                if self.take(token) {
                    let right = next(self)?;
                    left = self.push(Node::Binary(*operator, left, right))?;
                    continue 'more;
                }
            }
            return Ok(left);
        }
    }

    fn or(&mut self) -> Step {
        self.binary(&[("||", Operator::Or)], Self::and)
    }

    fn and(&mut self) -> Step {
        self.binary(&[("&&", Operator::And)], Self::equality)
    }

    fn equality(&mut self) -> Step {
        self.binary(
            &[("==", Operator::Equal), ("!=", Operator::NotEqual)],
            Self::comparison,
        )
    }

    fn comparison(&mut self) -> Step {
        self.binary(
            &[
                ("<=", Operator::LessOrEqual),
                (">=", Operator::GreaterOrEqual),
                ("<", Operator::Less),
                (">", Operator::Greater),
            ],
            Self::sum,
        )
    }

    fn sum(&mut self) -> Step {
        self.binary(
            &[("+", Operator::Add), ("-", Operator::Subtract)],
            Self::product,
        )
    }

    fn product(&mut self) -> Step {
        self.binary(
            &[
                ("*", Operator::Multiply),
                ("/", Operator::Divide),
                ("%", Operator::Remainder),
            ],
            Self::unary,
        )
    }

    fn unary(&mut self) -> Step {
        self.nested(|parser| {
            if parser.take("!") {
                let operand = parser.unary()?;
                return parser.push(Node::Not(operand));
            }
            parser.atom()
        })
    }

    fn atom(&mut self) -> Step {
        if self.take("(") {
            let inner = self.choose()?;
            if !self.take(")") {
                return Err("the plural expression has an unclosed bracket");
            }
            return Ok(inner);
        }
        if self.take("n") {
            return self.push(Node::Count);
        }
        self.blank();
        let start = self.at;
        while self.text.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        let digits = std::str::from_utf8(&self.text[start..self.at]).unwrap_or_default();
        let number = digits.parse().map_err(
            |_| "the plural expression has something that is not n, a number or a bracket",
        )?;
        self.push(Node::Number(number))
    }
}

/// What one message of a file translates into.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Translation {
    One(String),
    Forms(Vec<String>),
}

/// One message as a file holds it: its English text (and English plural), and what it translates into.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    english: String,
    english_plural: Option<String>,
    translation: Translation,
}

/// A language file, read: the language it is, the name it gives itself, its plural rule and its translations.
///
/// A template (`messages.pot`) is read the same way: no language, every translation empty. A message with an empty
/// translation, or marked `fuzzy`, is not translated: whoever asks gets the English text back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    language: Option<Language>,
    name: Option<String>,
    machine: bool,
    plural: PluralRule,
    entries: HashMap<String, Entry>,
}

/// Why a file could not be read, and on which line (from 1).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {what}")]
pub struct ParseError {
    pub line: usize,
    pub what: &'static str,
}

/// Where a translation does not fit the English text it translates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub english: String,
    pub what: &'static str,
}

/// The key of a message: its context and its English text, joined as gettext joins them.
fn key(context: Option<&str>, english: &str) -> String {
    match context {
        Some(context) => format!("{context}\u{4}{english}"),
        None => english.to_owned(),
    }
}

/// The parts of one entry while it is being read.
#[derive(Default)]
struct Pending {
    context: Option<String>,
    english: Option<String>,
    english_plural: Option<String>,
    single: Option<String>,
    forms: Vec<Option<String>>,
    fuzzy: bool,
    /// Which string a continuation line adds to.
    last: Field,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Field {
    #[default]
    None,
    Context,
    English,
    EnglishPlural,
    Single,
    Form(usize),
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.context.is_none()
            && self.english.is_none()
            && self.single.is_none()
            && self.forms.is_empty()
    }

    /// The string `field` names, if this message has a place for it.
    fn slot(&mut self, field: Field) -> Option<&mut Option<String>> {
        match field {
            Field::None => None,
            Field::Context => Some(&mut self.context),
            Field::English => Some(&mut self.english),
            Field::EnglishPlural => Some(&mut self.english_plural),
            Field::Single => Some(&mut self.single),
            Field::Form(index) => self.forms.get_mut(index),
        }
    }
}

/// No language and no messages: what a file with nothing in it reads as.
impl Default for Catalog {
    fn default() -> Self {
        Self {
            language: None,
            name: None,
            machine: false,
            plural: PluralRule::one_other(),
            entries: HashMap::new(),
        }
    }
}

impl Catalog {
    /// Reads a language file or a template. Refuses a file larger than [`MAX_FILE_BYTES`], with more than [`MAX_MESSAGES`]
    /// messages, with a message twice, or with anything the PO format does not allow; a language file must name its language
    /// (`Language:`) and may name itself (`X-Language-Name:`), its plural rule (`Plural-Forms:`, else one/other) and say that a
    /// machine translated it (`X-Translation: ai`).
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        if text.len() > MAX_FILE_BYTES {
            return Err(ParseError {
                line: 0,
                what: "the file is too large",
            });
        }
        let mut catalog = Self::default();
        let mut pending = Pending::default();
        let mut started = 0;
        let mut line_number = 0;
        for line in text.strip_prefix('\u{feff}').unwrap_or(text).lines() {
            line_number += 1;
            let error = |what| ParseError {
                line: line_number,
                what,
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(comment) = line.strip_prefix('#') {
                // `#,` flags; `#~` an obsolete message, kept by the tools and never used.
                if let Some(flags) = comment.strip_prefix(',') {
                    if !pending.is_empty() && pending.last != Field::None {
                        catalog.finish(std::mem::take(&mut pending), started)?;
                    }
                    pending.fuzzy |= flags.split(',').any(|flag| flag.trim() == "fuzzy");
                }
                continue;
            }
            if line.starts_with('"') {
                let piece = unquote(line).ok_or_else(|| error("a string is not well formed"))?;
                let last = pending.last;
                let Some(Some(text)) = pending.slot(last) else {
                    return Err(error("a string continues nothing"));
                };
                text.push_str(&piece);
                continue;
            }
            let (keyword, rest) = line
                .split_once(|c: char| c.is_ascii_whitespace())
                .ok_or_else(|| error("a line has a keyword and no string"))?;
            let value = unquote(rest.trim()).ok_or_else(|| error("a string is not well formed"))?;
            let field = match keyword {
                "msgctxt" => Field::Context,
                "msgid" => Field::English,
                "msgid_plural" => Field::EnglishPlural,
                "msgstr" => Field::Single,
                _ => {
                    let index = keyword
                        .strip_prefix("msgstr[")
                        .and_then(|rest| rest.strip_suffix(']'))
                        .and_then(|index| index.parse::<usize>().ok())
                        .ok_or_else(|| error("a line has a keyword the format does not have"))?;
                    if index >= MAX_FORMS || index != pending.forms.len() {
                        return Err(error("plural forms are out of order or too many"));
                    }
                    Field::Form(index)
                }
            };
            // A context or an English text after a translation starts the next message.
            let starts_next = matches!(field, Field::Context | Field::English)
                && (pending.single.is_some() || !pending.forms.is_empty());
            if starts_next {
                catalog.finish(std::mem::take(&mut pending), started)?;
            }
            if pending.is_empty() && pending.last == Field::None {
                started = line_number;
            }
            if let Field::Form(_) = field {
                pending.forms.push(None);
            }
            let Some(slot) = pending.slot(field) else {
                return Err(error("a line has a keyword the format does not have"));
            };
            if slot.is_some() {
                return Err(error("a message has the same keyword twice"));
            }
            *slot = Some(value.into_owned());
            pending.last = field;
        }
        if !pending.is_empty() {
            catalog.finish(pending, started)?;
        }
        Ok(catalog)
    }

    /// Takes in one whole message, or the header (the message whose English text is empty).
    fn finish(&mut self, pending: Pending, line: usize) -> Result<(), ParseError> {
        let error = |what| ParseError { line, what };
        let english = pending
            .english
            .ok_or_else(|| error("a message has no msgid"))?;
        let translation = match (
            pending.english_plural.is_some(),
            pending.single,
            pending.forms.is_empty(),
        ) {
            (false, Some(single), true) => Translation::One(single),
            (true, None, false) => Translation::Forms(
                pending
                    .forms
                    .into_iter()
                    .map(Option::unwrap_or_default)
                    .collect(),
            ),
            _ => return Err(error("a message's translation does not match its msgid")),
        };
        if english.is_empty() && pending.context.is_none() {
            let Translation::One(header) = translation else {
                return Err(error("the header has plural forms"));
            };
            return self.header(&header).map_err(error);
        }
        if self.entries.len() >= MAX_MESSAGES {
            return Err(error("the file has too many messages"));
        }
        let translation = if pending.fuzzy {
            match translation {
                Translation::One(_) => Translation::One(String::new()),
                Translation::Forms(forms) => Translation::Forms(vec![String::new(); forms.len()]),
            }
        } else {
            translation
        };
        let entry = Entry {
            english: english.clone(),
            english_plural: pending.english_plural,
            translation,
        };
        if self
            .entries
            .insert(key(pending.context.as_deref(), &english), entry)
            .is_some()
        {
            return Err(error("a message is in the file twice"));
        }
        Ok(())
    }

    fn header(&mut self, header: &str) -> Result<(), &'static str> {
        for line in header.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name.trim() {
                "Language" if !value.is_empty() => {
                    self.language = Some(
                        Language::parse(value)
                            .map_err(|_| "the header's Language is not a language tag")?,
                    );
                }
                "X-Language-Name" if !value.is_empty() => {
                    if !crate::name::text(value, MAX_NAME_CHARS) {
                        return Err("the header's X-Language-Name is not a short name");
                    }
                    self.name = Some(value.to_owned());
                }
                "Plural-Forms" => self.plural = PluralRule::parse(value)?,
                "X-Translation" => {
                    self.machine = match value {
                        "ai" => true,
                        "person" => false,
                        _ => return Err("the header's X-Translation is neither ai nor person"),
                    };
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The language the file is in; `None` for a template.
    pub fn language(&self) -> Option<Language> {
        self.language
    }

    /// The name the language gives itself, in itself (`Română`).
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether the file says a machine translated it (`X-Translation: ai`), so that a list of languages can say so.
    pub fn machine_translated(&self) -> bool {
        self.machine
    }

    /// The translation of `english` (in `context`, if it has one), if the file translates it.
    pub fn get(&self, context: Option<&str>, english: &str) -> Option<&str> {
        match &self.entries.get(&key(context, english))?.translation {
            Translation::One(text) if !text.is_empty() => Some(text),
            _ => None,
        }
    }

    /// The form of `english` (its singular) for `count`, if the file translates it.
    pub fn plural(&self, context: Option<&str>, english: &str, count: u64) -> Option<&str> {
        match &self.entries.get(&key(context, english))?.translation {
            Translation::Forms(forms) => forms
                .get(self.plural.form(count))
                .map(String::as_str)
                .filter(|text| !text.is_empty()),
            Translation::One(_) => None,
        }
    }

    /// Whether the file has `english` as a message (translated or not).
    pub fn contains(&self, context: Option<&str>, english: &str) -> bool {
        self.entries.contains_key(&key(context, english))
    }

    /// How many messages the file has, and how many of them it translates.
    pub fn counts(&self) -> (usize, usize) {
        let translated = self
            .entries
            .values()
            .filter(|entry| match &entry.translation {
                Translation::One(text) => !text.is_empty(),
                Translation::Forms(forms) => forms.iter().all(|form| !form.is_empty()),
            })
            .count();
        (self.entries.len(), translated)
    }

    /// Where the translations do not fit their English text, against `template` (the messages that exist) when there is
    /// one: a message the template does not have, a missing or added placeholder, a plural with the wrong number of forms,
    /// a translation that is too long or holds a control character the English does not.
    pub fn problems(&self, template: Option<&Catalog>) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut problem = |english: &str, what| {
            problems.push(Problem {
                english: english.to_owned(),
                what,
            })
        };
        for (key, entry) in &self.entries {
            if let Some(template) = template {
                match template.entries.get(key) {
                    None => {
                        problem(&entry.english, "the template has no such message");
                        continue;
                    }
                    Some(known)
                        if known.english_plural.is_some() != entry.english_plural.is_some() =>
                    {
                        problem(&entry.english, "plural in one file and not in the other");
                        continue;
                    }
                    Some(_) => {}
                }
            }
            let mut wanted = placeholders(&entry.english);
            if let Some(plural) = &entry.english_plural {
                for name in placeholders(plural) {
                    if !wanted.contains(&name) {
                        wanted.push(name);
                    }
                }
            }
            wanted.sort_unstable();
            let texts: Vec<&str> = match &entry.translation {
                Translation::One(text) => vec![text],
                Translation::Forms(forms) => {
                    if forms.len() != self.plural.forms() {
                        problem(
                            &entry.english,
                            "the number of plural forms is not the language's",
                        );
                    }
                    forms.iter().map(String::as_str).collect()
                }
            };
            for text in texts.into_iter().filter(|text| !text.is_empty()) {
                let mut found = placeholders(text);
                found.sort_unstable();
                let fits = match entry.translation {
                    // A plural form may leave out what its English has (`one file` for `{count} files`), never add.
                    Translation::Forms(_) => found.iter().all(|name| wanted.contains(name)),
                    Translation::One(_) => found == wanted,
                };
                if !fits {
                    problem(
                        &entry.english,
                        "the placeholders differ from the English text",
                    );
                }
                if text.chars().count() > MAX_TRANSLATION_CHARS {
                    problem(&entry.english, "the translation is too long");
                }
                let controls =
                    |text: &str| text.chars().filter(|c| c.is_control()).collect::<Vec<_>>();
                if controls(text)
                    .iter()
                    .any(|c| !controls(&entry.english).contains(c))
                {
                    problem(
                        &entry.english,
                        "the translation has a control character the English text does not",
                    );
                }
            }
        }
        problems.sort_by(|left, right| left.english.cmp(&right.english));
        problems
    }

    /// The file with only the translations that fit (see [`Self::problems`]): a downloaded or imported file is used for
    /// what it gets right, and English shows where it does not.
    pub fn without_problems(mut self, template: Option<&Catalog>) -> (Self, usize) {
        let problems = self.problems(template);
        let mut dropped = 0;
        for problem in &problems {
            let before = self.entries.len();
            self.entries
                .retain(|_, entry| entry.english != problem.english);
            dropped += before - self.entries.len();
        }
        (self, dropped)
    }
}

/// A PO string: `"…"` with C escapes (`\n`, `\t`, `\r`, `\"`, `\\`). `None` for anything else.
fn unquote(text: &str) -> Option<Cow<'_, str>> {
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    if !inner.contains('\\') {
        return (!inner.contains('"')).then_some(Cow::Borrowed(inner));
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(match chars.next()? {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '"' => '"',
                '\\' => '\\',
                _ => return None,
            }),
            '"' => return None,
            c => out.push(c),
        }
    }
    Some(Cow::Owned(out))
}

/// The PO string of `text`, quoted and escaped: what a tool writing a language file needs.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_one_language_however_it_is_written() {
        let ro = Language::parse("ro").unwrap();
        assert_eq!(ro.tag(), "ro");
        assert_eq!(Language::parse("pt_BR").unwrap().tag(), "pt-br");
        assert_eq!(
            Language::parse("PT-br").unwrap(),
            Language::parse("pt-BR").unwrap()
        );
        assert_eq!(Language::parse("pt-br").unwrap().primary().tag(), "pt");
        assert_eq!(Language::parse("sr-Latn-RS").unwrap().tag(), "sr-latn-rs");
        assert!(Language::ENGLISH.is_english() && Language::parse("en-GB").unwrap().is_english());
        assert!(!ro.is_english());
        for bad in [
            "",
            "r",
            "1o",
            "ro-",
            "-ro",
            "ro--ro",
            "ro ro",
            "ro.utf8",
            "românește",
            "ro-abcdefghi",
            "a-b-c-d-e-f-g-h",
        ] {
            assert_eq!(Language::parse(bad), Err(BadTag), "{bad:?}");
        }
    }

    #[test]
    fn a_locale_of_the_environment_names_its_language_or_none() {
        assert_eq!(Language::from_locale("ro_RO.UTF-8").unwrap().tag(), "ro-ro");
        assert_eq!(Language::from_locale("pt_BR@euro").unwrap().tag(), "pt-br");
        assert_eq!(Language::from_locale("de").unwrap().tag(), "de");
        for none in ["C", "C.UTF-8", "POSIX", "", "??"] {
            assert_eq!(Language::from_locale(none), None, "{none:?}");
        }
    }

    #[test]
    fn a_tag_is_saved_as_its_text_and_read_back_checked() {
        let ro = Language::parse("ro").unwrap();
        assert_eq!(serde_json::to_string(&ro).unwrap(), "\"ro\"");
        assert_eq!(serde_json::from_str::<Language>("\"ro\"").unwrap(), ro);
        assert!(serde_json::from_str::<Language>("\"not a tag\"").is_err());
    }

    #[test]
    fn placeholders_are_named_once_in_order() {
        assert_eq!(placeholders("{a} and {b}, {a} {} {x y} {"), vec!["a", "b"]);
    }

    #[test]
    fn plural_rules_of_real_languages_pick_their_forms() {
        let romanian = PluralRule::parse(
            "nplurals=3; plural=(n==1 ? 0 : (n==0 || (n%100 > 0 && n%100 < 20)) ? 1 : 2);",
        )
        .unwrap();
        let picks: Vec<usize> = [1, 0, 2, 19, 20, 101, 119, 120]
            .into_iter()
            .map(|n| romanian.form(n))
            .collect();
        assert_eq!(picks, [0, 1, 1, 1, 2, 1, 1, 2]);
        let polish = PluralRule::parse(
            "nplurals=3; plural=(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);",
        )
        .unwrap();
        let picks: Vec<usize> = [1, 2, 4, 5, 12, 22, 25]
            .into_iter()
            .map(|n| polish.form(n))
            .collect();
        assert_eq!(picks, [0, 1, 1, 2, 2, 1, 2]);
        let english = PluralRule::one_other();
        assert_eq!(
            (
                english.forms(),
                english.form(1),
                english.form(0),
                english.form(7)
            ),
            (2, 0, 1, 1)
        );
        let one = PluralRule::parse("nplurals=1; plural=0;").unwrap();
        assert_eq!(one.form(5), 0);
        assert_eq!(
            PluralRule::parse("nplurals=2; plural=!(n>1);")
                .unwrap()
                .form(2),
            0
        );
    }

    #[test]
    fn a_plural_rule_that_is_wrong_or_hostile_is_refused_not_run() {
        for bad in [
            "",
            "nplurals=2;",
            "plural=n;",
            "nplurals=0; plural=0;",
            "nplurals=7; plural=0;",
            "nplurals=2; plural=n;",     // gives forms that do not exist
            "nplurals=2; plural=(n/0);", // divides by zero
            "nplurals=2; plural=(n%(n-n));",
            "nplurals=2; plural=(n != 1;", // unclosed
            "nplurals=2; plural=n ? 1;",   // no `:`
            "nplurals=2; plural=system(1);",
            "nplurals=2; plural=n = 1;",
            "nplurals=2; plural=(n != 1); extra=1;",
            "nplurals=2; plural=18446744073709551616;", // too large a number
        ] {
            assert!(PluralRule::parse(bad).is_err(), "{bad:?}");
        }
        let deep = format!(
            "nplurals=2; plural={}0{};",
            "(".repeat(100),
            ")".repeat(100)
        );
        assert!(PluralRule::parse(&deep).is_err());
        let long = format!("nplurals=2; plural=0{};", "+0".repeat(200));
        assert!(PluralRule::parse(&long).is_err());
        let not = format!("nplurals=2; plural={}0;", "!".repeat(100));
        assert!(PluralRule::parse(&not).is_err());
    }

    #[test]
    fn a_file_says_a_machine_translated_it_only_in_its_header_and_only_in_two_words() {
        let with = |value: &str| {
            Catalog::parse(&format!(
                "msgid \"\"\nmsgstr \"Language: ro\\nX-Translation: {value}\\n\"\n"
            ))
        };
        assert!(with("ai").unwrap().machine_translated());
        assert!(!with("person").unwrap().machine_translated());
        assert!(with("maybe").is_err());
        assert!(!Catalog::parse(ROMANIAN).unwrap().machine_translated());
    }

    const ROMANIAN: &str = r#"
# A comment.
msgid ""
msgstr ""
"Language: ro\n"
"X-Language-Name: Română\n"
"Plural-Forms: nplurals=3; plural=(n==1 ? 0 : (n==0 || (n%100 > 0 && n%100 < 20)) ? 1 : 2);\n"

msgid "Send"
msgstr "Trimite"

msgid "Sent to {name}"
msgstr ""
"Trimis către "
"{name}"

msgid "Untranslated"
msgstr ""

#, fuzzy
msgid "Guessed"
msgstr "Ghicit"

msgctxt "ago"
msgid "{count} minute"
msgid_plural "{count} minutes"
msgstr[0] "{count} minut"
msgstr[1] "{count} minute"
msgstr[2] "{count} de minute"

msgid "Say \"hi\"\tnow"
msgstr "Spune \"salut\"\tacum"
"#;

    #[test]
    fn a_language_file_is_read_with_its_header_plurals_and_escapes() {
        let catalog = Catalog::parse(ROMANIAN).unwrap();
        assert_eq!(catalog.language(), Some(Language::parse("ro").unwrap()));
        assert_eq!(catalog.name(), Some("Română"));
        assert_eq!(catalog.plural.forms(), 3);
        assert_eq!(catalog.get(None, "Send"), Some("Trimite"));
        assert_eq!(
            catalog.get(None, "Sent to {name}"),
            Some("Trimis către {name}")
        );
        assert_eq!(catalog.get(None, "Untranslated"), None);
        assert_eq!(
            catalog.get(None, "Guessed"),
            None,
            "a fuzzy translation is not used"
        );
        assert_eq!(catalog.get(None, "Missing"), None);
        assert_eq!(
            catalog.get(None, "Say \"hi\"\tnow"),
            Some("Spune \"salut\"\tacum")
        );
        assert_eq!(
            catalog.plural(Some("ago"), "{count} minute", 1),
            Some("{count} minut")
        );
        assert_eq!(
            catalog.plural(Some("ago"), "{count} minute", 5),
            Some("{count} minute")
        );
        assert_eq!(
            catalog.plural(Some("ago"), "{count} minute", 25),
            Some("{count} de minute")
        );
        assert_eq!(
            catalog.plural(None, "{count} minute", 1),
            None,
            "the context is part of the message"
        );
        assert_eq!(catalog.counts(), (6, 4));
        assert!(
            catalog.problems(None).is_empty(),
            "{:?}",
            catalog.problems(None)
        );
    }

    #[test]
    fn a_file_the_format_does_not_allow_is_refused_with_its_line() {
        for (bad, line) in [
            ("msgid \"a\"\nmsgstr \"b\"\nmsgid \"a\"\nmsgstr \"c\"\n", 3),
            ("msgstr \"b\"\n", 1),
            ("msgid \"a\"\nmsgstr \"b\" junk\n", 2),
            ("msgid \"a\"\nmsgstr \"b\\q\"\n", 2),
            ("msgid \"a\"\nmsgstr[1] \"b\"\n", 2),
            ("\"orphan\"\n", 1),
            ("msgid \"a\"\nmsgfoo \"b\"\n", 2),
            ("msgid \"\"\nmsgstr \"Language: not a tag\\n\"\n", 1),
            (
                "msgid \"\"\nmsgstr \"Plural-Forms: nplurals=2; plural=n;\\n\"\n",
                1,
            ),
            ("msgid \"a\"\nmsgid_plural \"as\"\nmsgstr \"b\"\n", 1),
        ] {
            let error = Catalog::parse(bad).unwrap_err();
            assert_eq!(error.line, line, "{bad:?}: {error}");
        }
        assert!(Catalog::parse(&"x".repeat(MAX_FILE_BYTES + 1)).is_err());
    }

    #[test]
    fn translations_that_do_not_fit_their_english_are_named_and_can_be_left_out() {
        let template = Catalog::parse(
            "msgid \"Send {count}\"\nmsgstr \"\"\n\nmsgid \"Hi\"\nmsgstr \"\"\n\nmsgid \"{count} file\"\nmsgid_plural \"{count} files\"\nmsgstr[0] \"\"\nmsgstr[1] \"\"\n",
        )
        .unwrap();
        let file = Catalog::parse(concat!(
            "msgid \"\"\nmsgstr \"Language: xx\\n\"\n\n",
            "msgid \"Send {count}\"\nmsgstr \"Trimite {număr}\"\n\n",
            "msgid \"Hi\"\nmsgstr \"Salut\\n\"\n\n",
            "msgid \"Gone\"\nmsgstr \"Dus\"\n\n",
            "msgid \"{count} file\"\nmsgid_plural \"{count} files\"\nmsgstr[0] \"un fișier\"\nmsgstr[1] \"{count} fișiere\"\n",
        ))
        .unwrap();
        let problems = file.problems(Some(&template));
        let what: Vec<(&str, &str)> = problems
            .iter()
            .map(|problem| (problem.english.as_str(), problem.what))
            .collect();
        assert_eq!(
            what,
            [
                ("Gone", "the template has no such message"),
                (
                    "Hi",
                    "the translation has a control character the English text does not"
                ),
                (
                    "Send {count}",
                    "the placeholders differ from the English text"
                ),
            ]
        );
        let (kept, dropped) = file.without_problems(Some(&template));
        assert_eq!(dropped, 3);
        assert_eq!(kept.plural(None, "{count} file", 1), Some("un fișier"));
        assert_eq!(kept.get(None, "Hi"), None);
    }

    #[test]
    fn quoting_and_reading_back_give_the_same_text() {
        for text in ["plain", "a \"b\" c", "line\nnext\ttab \\ end", ""] {
            let quoted = quote(text);
            assert_eq!(unquote(&quoted).unwrap(), text);
        }
    }
}
