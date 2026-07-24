//! A small **revset** (revision-set) query language — design doc §5.8, and the
//! `RV` revset engine of the §4 architecture diagram.
//!
//! Revsets (Mercurial lineage, jj dialect, §5.8, §10) name *sets* of commits by
//! expression rather than one at a time. This module implements the set-algebra
//! core of that language over commits and refs:
//!
//! * literal ref names (`main`), bare;
//! * `id:<hex>` literals for a specific commit;
//! * the nullary functions `all()`, `heads()`, `draft()`, `public()`;
//! * the set operators `&` (intersection), `|` (union) and `~` (complement),
//!   with parentheses.
//!
//! `~` binds tightest, then `&`, then `|`, so `draft() & ~public()` parses as
//! `draft() & (~public())`. Evaluation is total and deterministic: results are
//! [`BTreeSet`]s, so ordering is canonical.
//!
//! Parsing is a hand-rolled tokenizer plus recursive-descent parser — no
//! parser-generator dependency.

use std::collections::BTreeSet;

use omoplata_identity::{CommitId, Phase};

use crate::error::WorkError;

/// A parsed revset expression (an AST over commit sets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevExpr {
    /// A literal ref name, resolved to its target commit (`main`).
    Ref(String),
    /// A specific commit named by id (`id:<hex>`).
    Id(String),
    /// Every commit in the universe (`all()`).
    All,
    /// Every ref target (`heads()`).
    Heads,
    /// Set intersection (`a & b`).
    And(Box<RevExpr>, Box<RevExpr>),
    /// Set union (`a | b`).
    Or(Box<RevExpr>, Box<RevExpr>),
    /// Set complement against the universe (`~a`).
    Not(Box<RevExpr>),
    /// Commits whose phase is [`Draft`](Phase::Draft) (`draft()`).
    Draft,
    /// Commits whose phase is [`Public`](Phase::Public) (`public()`).
    Public,
    /// Commits landed in a named queue (`landed(release-1.2)`; bare `landed()`
    /// means the implicit `trunk` queue). See ADR-0009: `landed(release-1.2) &
    /// ~landed(trunk)` is the "needs backporting to trunk" query.
    Landed(String),
}

/// The context a revset is evaluated against.
///
/// Supplies the three things evaluation needs: the universe of commits, ref
/// resolution, and a phase lookup. Implementors keep these total so evaluation
/// never panics.
pub trait RevsetContext {
    /// Every commit in scope — the universe `all()` and the complement `~`
    /// range over.
    fn universe(&self) -> BTreeSet<CommitId>;

    /// Resolve a ref name to its target commit, or `None` if there is no such
    /// ref.
    fn resolve(&self, name: &str) -> Option<CommitId>;

    /// Every ref target — the result of `heads()`.
    fn heads(&self) -> BTreeSet<CommitId>;

    /// The phase of a commit, or `None` if unknown.
    fn phase(&self, commit: &CommitId) -> Option<Phase>;

    /// The commits landed in queue `queue` (ADR-0009) — the targets of its
    /// per-queue public refs.
    ///
    /// Ref-namespace disambiguation: a non-`trunk` queue's landings live at
    /// `public/<queue>/<change>`; `trunk` keeps the legacy `public/<change>`
    /// shape, where `<change>` may itself contain `/` (e.g. `ws/hotfix`). A
    /// `public/…` ref therefore belongs to a non-trunk queue **iff** its first
    /// segment after `public/` names a *registered* queue, and to `trunk`
    /// otherwise — which is why implementations need the registered-queue
    /// name set.
    fn landed(&self, queue: &str) -> BTreeSet<CommitId>;
}

/// A simple in-memory [`RevsetContext`] built from a ref map plus optional phase
/// information.
///
/// The universe is the union of ref targets, phase-annotated commits, and any
/// commits added with [`with_commits`](MapContext::with_commits).
#[derive(Debug, Clone, Default)]
pub struct MapContext {
    refs: std::collections::BTreeMap<String, CommitId>,
    phases: std::collections::BTreeMap<CommitId, Phase>,
    extra: BTreeSet<CommitId>,
    queues: BTreeSet<String>,
}

impl MapContext {
    /// Build a context from a `name -> CommitId` ref map.
    #[must_use]
    pub fn new(refs: std::collections::BTreeMap<String, CommitId>) -> Self {
        Self {
            refs,
            phases: std::collections::BTreeMap::new(),
            extra: BTreeSet::new(),
            queues: BTreeSet::new(),
        }
    }

    /// Record the registered queue names (ADR-0009), used by
    /// [`landed`](RevsetContext::landed) to disambiguate `public/…` refs
    /// between `trunk`'s legacy shape and per-queue namespaces.
    #[must_use]
    pub fn with_queues(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.queues.extend(names);
        self
    }

    /// Record the phase of a commit (adding it to the universe).
    #[must_use]
    pub fn with_phase(mut self, commit: CommitId, phase: Phase) -> Self {
        self.phases.insert(commit, phase);
        self
    }

    /// Add commits to the universe that are not necessarily ref targets.
    #[must_use]
    pub fn with_commits(mut self, commits: impl IntoIterator<Item = CommitId>) -> Self {
        self.extra.extend(commits);
        self
    }
}

impl RevsetContext for MapContext {
    fn universe(&self) -> BTreeSet<CommitId> {
        let mut set: BTreeSet<CommitId> = self.refs.values().cloned().collect();
        set.extend(self.phases.keys().cloned());
        set.extend(self.extra.iter().cloned());
        set
    }

    fn resolve(&self, name: &str) -> Option<CommitId> {
        self.refs.get(name).cloned()
    }

    fn heads(&self) -> BTreeSet<CommitId> {
        self.refs.values().cloned().collect()
    }

    fn phase(&self, commit: &CommitId) -> Option<Phase> {
        self.phases.get(commit).copied()
    }

    fn landed(&self, queue: &str) -> BTreeSet<CommitId> {
        let mut out = BTreeSet::new();
        for (name, commit) in &self.refs {
            let Some(rest) = name.strip_prefix("public/") else {
                continue;
            };
            let owner = match rest.split_once('/') {
                // First segment names a registered queue ⇒ that queue's ref;
                // otherwise the whole rest is a trunk change id (which may
                // itself contain '/', e.g. `ws/hotfix`).
                Some((first, _)) if self.queues.contains(first) => first,
                _ => "trunk",
            };
            if owner == queue {
                out.insert(commit.clone());
            }
        }
        out
    }
}

/// Evaluate a parsed expression against `ctx`, yielding the matching commit set.
///
/// Evaluation is total and deterministic (the result is a [`BTreeSet`]).
///
/// # Errors
///
/// [`WorkError::UnknownRef`] if a literal ref name does not resolve. An
/// `id:<hex>` literal that names no commit in the universe simply matches
/// nothing (it is not an error).
pub fn eval(expr: &RevExpr, ctx: &dyn RevsetContext) -> Result<BTreeSet<CommitId>, WorkError> {
    match expr {
        RevExpr::Ref(name) => {
            let commit = ctx
                .resolve(name)
                .ok_or_else(|| WorkError::UnknownRef(name.clone()))?;
            Ok([commit].into_iter().collect())
        }
        RevExpr::Id(hex) => {
            let commit = CommitId::new(hex.clone());
            Ok(if ctx.universe().contains(&commit) {
                [commit].into_iter().collect()
            } else {
                BTreeSet::new()
            })
        }
        RevExpr::All => Ok(ctx.universe()),
        RevExpr::Heads => Ok(ctx.heads()),
        RevExpr::And(a, b) => {
            let l = eval(a, ctx)?;
            let r = eval(b, ctx)?;
            Ok(l.intersection(&r).cloned().collect())
        }
        RevExpr::Or(a, b) => {
            let mut l = eval(a, ctx)?;
            l.extend(eval(b, ctx)?);
            Ok(l)
        }
        RevExpr::Not(a) => {
            let inner = eval(a, ctx)?;
            Ok(ctx.universe().difference(&inner).cloned().collect())
        }
        RevExpr::Draft => Ok(phase_filter(ctx, Phase::Draft)),
        RevExpr::Public => Ok(phase_filter(ctx, Phase::Public)),
        RevExpr::Landed(queue) => Ok(ctx.landed(queue)),
    }
}

/// Every commit in the universe whose phase equals `want`.
fn phase_filter(ctx: &dyn RevsetContext, want: Phase) -> BTreeSet<CommitId> {
    ctx.universe()
        .into_iter()
        .filter(|c| ctx.phase(c) == Some(want))
        .collect()
}

/// Parse and evaluate `input` against `ctx` in one step.
///
/// # Errors
///
/// [`WorkError::Parse`] on a malformed expression, or the errors of [`eval`].
pub fn query(input: &str, ctx: &dyn RevsetContext) -> Result<BTreeSet<CommitId>, WorkError> {
    eval(&parse(input)?, ctx)
}

// ── Tokenizer ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    And,
    Or,
    Not,
    LParen,
    RParen,
    /// An identifier: a ref name, a function name, or an `id:<hex>` literal.
    Word(String),
}

/// Whether `c` may appear inside a bare word (ref name, function name, or the
/// `id:<hex>` literal, which keeps its colons).
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':')
}

fn tokenize(input: &str) -> Result<Vec<Token>, WorkError> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '&' => {
                chars.next();
                tokens.push(Token::And);
            }
            '|' => {
                chars.next();
                tokens.push(Token::Or);
            }
            '~' => {
                chars.next();
                tokens.push(Token::Not);
            }
            '(' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            c if is_word_char(c) => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if is_word_char(c) {
                        word.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Word(word));
            }
            other => {
                return Err(WorkError::Parse(format!("unexpected character '{other}'")));
            }
        }
    }
    Ok(tokens)
}

// ── Recursive-descent parser ─────────────────────────────────────────────────

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// `or := and ('|' and)*`
    fn parse_or(&mut self) -> Result<RevExpr, WorkError> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.pos += 1;
            let right = self.parse_and()?;
            left = RevExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `and := unary ('&' unary)*`
    fn parse_and(&mut self) -> Result<RevExpr, WorkError> {
        let mut left = self.parse_unary()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.pos += 1;
            let right = self.parse_unary()?;
            left = RevExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `unary := '~' unary | primary`
    fn parse_unary(&mut self) -> Result<RevExpr, WorkError> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.pos += 1;
            Ok(RevExpr::Not(Box::new(self.parse_unary()?)))
        } else {
            self.parse_primary()
        }
    }

    /// `primary := '(' or ')' | word ['(' ')'] `
    fn parse_primary(&mut self) -> Result<RevExpr, WorkError> {
        match self.next() {
            Some(Token::LParen) => {
                let inner = self.parse_or()?;
                match self.next() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(WorkError::Parse("expected ')'".to_owned())),
                }
            }
            Some(Token::Word(word)) => {
                if matches!(self.peek(), Some(Token::LParen)) {
                    // A function call: consume '(' [word] ')'.
                    self.pos += 1;
                    let arg = if let Some(Token::Word(a)) = self.peek() {
                        let a = a.clone();
                        self.pos += 1;
                        Some(a)
                    } else {
                        None
                    };
                    match self.next() {
                        Some(Token::RParen) => {}
                        _ => {
                            return Err(WorkError::Parse(format!("expected ')' after {word}(")));
                        }
                    }
                    match (word.as_str(), arg) {
                        ("all", None) => Ok(RevExpr::All),
                        ("heads", None) => Ok(RevExpr::Heads),
                        ("draft", None) => Ok(RevExpr::Draft),
                        ("public", None) => Ok(RevExpr::Public),
                        // Bare landed() means the implicit trunk queue.
                        ("landed", arg) => {
                            Ok(RevExpr::Landed(arg.unwrap_or_else(|| "trunk".to_owned())))
                        }
                        (other, Some(arg)) => Err(WorkError::Parse(format!(
                            "function '{other}' takes no argument (got '{arg}')"
                        ))),
                        (other, None) => {
                            Err(WorkError::Parse(format!("unknown function '{other}'")))
                        }
                    }
                } else if let Some(hex) = word.strip_prefix("id:") {
                    if hex.is_empty() {
                        Err(WorkError::Parse("empty id: literal".to_owned()))
                    } else {
                        Ok(RevExpr::Id(hex.to_owned()))
                    }
                } else {
                    Ok(RevExpr::Ref(word))
                }
            }
            Some(other) => Err(WorkError::Parse(format!("unexpected token {other:?}"))),
            None => Err(WorkError::Parse("unexpected end of input".to_owned())),
        }
    }
}

/// Parse a revset expression string into a [`RevExpr`].
///
/// # Errors
///
/// [`WorkError::Parse`] if the input cannot be tokenized or does not form a
/// valid expression (including trailing tokens).
pub fn parse(input: &str) -> Result<RevExpr, WorkError> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Err(WorkError::Parse("empty expression".to_owned()));
    }
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.parse_or()?;
    if parser.pos != parser.tokens.len() {
        return Err(WorkError::Parse(
            "trailing tokens after expression".to_owned(),
        ));
    }
    Ok(expr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn commit(s: &str) -> CommitId {
        CommitId::new(s)
    }

    fn ctx() -> MapContext {
        let mut refs = BTreeMap::new();
        refs.insert("a".to_owned(), commit("aaa"));
        refs.insert("b".to_owned(), commit("bbb"));
        MapContext::new(refs)
    }

    #[test]
    fn parse_precedence_not_binds_tightest() {
        // ~a & b  ==  (~a) & b
        let e = parse("~a & b").unwrap();
        assert_eq!(
            e,
            RevExpr::And(
                Box::new(RevExpr::Not(Box::new(RevExpr::Ref("a".into())))),
                Box::new(RevExpr::Ref("b".into())),
            )
        );
    }

    #[test]
    fn eval_union() {
        let set = query("a | b", &ctx()).unwrap();
        assert_eq!(set, [commit("aaa"), commit("bbb")].into_iter().collect());
    }

    #[test]
    fn eval_intersection_disjoint_is_empty() {
        let set = query("a & b", &ctx()).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn eval_intersection_same_is_singleton() {
        let set = query("a & a", &ctx()).unwrap();
        assert_eq!(set, [commit("aaa")].into_iter().collect());
    }

    #[test]
    fn eval_not() {
        // ~a over the universe {aaa, bbb} is {bbb}.
        let set = query("~a", &ctx()).unwrap();
        assert_eq!(set, [commit("bbb")].into_iter().collect());
    }

    #[test]
    fn eval_heads_and_all() {
        let heads = query("heads()", &ctx()).unwrap();
        let all = query("all()", &ctx()).unwrap();
        let expect: BTreeSet<_> = [commit("aaa"), commit("bbb")].into_iter().collect();
        assert_eq!(heads, expect);
        assert_eq!(all, expect);
    }

    #[test]
    fn eval_draft_and_not_public() {
        let ctx = MapContext::new(BTreeMap::new())
            .with_phase(commit("d1"), Phase::Draft)
            .with_phase(commit("p1"), Phase::Public);
        let set = query("draft() & ~public()", &ctx).unwrap();
        assert_eq!(set, [commit("d1")].into_iter().collect());
    }

    #[test]
    fn eval_id_literal() {
        let ctx = ctx();
        let set = query("id:aaa", &ctx).unwrap();
        assert_eq!(set, [commit("aaa")].into_iter().collect());
        // An id not present matches nothing (not an error).
        assert!(query("id:zzz", &ctx).unwrap().is_empty());
    }

    #[test]
    fn eval_id_literal_with_colons() {
        let ctx = MapContext::new(BTreeMap::new()).with_commits([commit("sha256:dead")]);
        let set = query("id:sha256:dead", &ctx).unwrap();
        assert_eq!(set, [commit("sha256:dead")].into_iter().collect());
    }

    #[test]
    fn parse_errors_on_garbage() {
        assert!(matches!(parse("@#$"), Err(WorkError::Parse(_))));
        assert!(matches!(parse("a &"), Err(WorkError::Parse(_))));
        assert!(matches!(parse("(a"), Err(WorkError::Parse(_))));
        assert!(matches!(parse("a b"), Err(WorkError::Parse(_))));
        assert!(matches!(parse(""), Err(WorkError::Parse(_))));
    }

    #[test]
    fn eval_unknown_ref_errors() {
        assert!(matches!(
            query("nope", &ctx()),
            Err(WorkError::UnknownRef(_))
        ));
    }

    #[test]
    fn parens_override_precedence() {
        let set = query("(a | b) & b", &ctx()).unwrap();
        assert_eq!(set, [commit("bbb")].into_iter().collect());
    }
}

#[cfg(test)]
mod landed_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ctx() -> MapContext {
        let mut refs = BTreeMap::new();
        // trunk landings (legacy shape; change ids contain '/').
        refs.insert("public/ws/a".to_owned(), CommitId::new("c-a"));
        refs.insert("public/ws/b".to_owned(), CommitId::new("c-b"));
        // release-line landings.
        refs.insert(
            "public/release-1.2/ws/a".to_owned(),
            CommitId::new("c-a-12"),
        );
        // an unrelated draft ref.
        refs.insert("ws/a".to_owned(), CommitId::new("c-a"));
        MapContext::new(refs).with_queues(["release-1.2".to_owned()])
    }

    #[test]
    fn landed_splits_trunk_from_named_queue() {
        let ctx = ctx();
        let trunk = ctx.landed("trunk");
        assert!(trunk.contains(&CommitId::new("c-a")) && trunk.contains(&CommitId::new("c-b")));
        assert!(!trunk.contains(&CommitId::new("c-a-12")));

        let rel = ctx.landed("release-1.2");
        assert_eq!(rel.len(), 1);
        assert!(rel.contains(&CommitId::new("c-a-12")));
    }

    #[test]
    fn trunk_change_whose_first_segment_is_not_a_queue_stays_trunk() {
        // `ws/a` starts with `ws`, which is NOT a registered queue, so
        // `public/ws/a` is a trunk landing even though it contains '/'.
        let ctx = ctx();
        assert!(ctx.landed("ws").is_empty());
    }

    #[test]
    fn needs_backport_query_evaluates() {
        let ctx = ctx();
        // Landed in trunk but not yet in the release line.
        let out = query("landed(trunk) & ~landed(release-1.2)", &ctx).unwrap();
        assert!(out.contains(&CommitId::new("c-a")) && out.contains(&CommitId::new("c-b")));
        assert!(!out.contains(&CommitId::new("c-a-12")));
    }

    #[test]
    fn bare_landed_means_trunk_and_args_are_rejected_elsewhere() {
        let ctx = ctx();
        assert_eq!(
            query("landed()", &ctx).unwrap(),
            query("landed(trunk)", &ctx).unwrap()
        );
        assert!(matches!(parse("draft(x)"), Err(WorkError::Parse(_))));
    }
}
