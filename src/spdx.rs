//! A small SPDX license-*expression* evaluator.
//!
//! Real manifests do not hand you a single license id. Cargo gives you
//! `MIT OR Apache-2.0`, `(MIT OR Apache-2.0) AND Unicode-3.0`, the legacy
//! slash form `MIT/Apache-2.0`, and `Apache-2.0 WITH LLVM-exception`; npm
//! gives you `(MIT OR CC0-1.0)`. Substring-matching a denylist against
//! that text is wrong in both directions — `"GPL"` matches `LGPL-2.1` and
//! even `AGPL`, and a naive equality check on `MIT OR Apache-2.0` matches
//! neither `MIT` nor `Apache-2.0`. So the expression is actually parsed,
//! and allow/deny are evaluated over the boolean structure.

/// One parsed license expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A single license id, possibly with a `WITH <exception>` suffix.
    Id {
        id: String,
        exception: Option<String>,
    },
    /// `A OR B` — the user may pick either, so either satisfying the
    /// policy is enough.
    Or(Vec<Expr>),
    /// `A AND B` — the user is bound by both, so both must satisfy it.
    And(Vec<Expr>),
}

/// Canonical form used for comparing a license id against a policy entry.
///
/// Uppercases, drops a trailing `+`, and drops the `-only` / `-or-later`
/// suffixes SPDX added when it deprecated the bare GPL ids. That means a
/// denylist entry of `GPL-3.0` catches all four real spellings in the
/// wild — `GPL-3.0`, `GPL-3.0+`, `GPL-3.0-only`, `GPL-3.0-or-later` —
/// which is what anyone writing `deny = ["GPL-3.0"]` actually means.
/// It also means you cannot deny `GPL-3.0-only` while allowing
/// `GPL-3.0-or-later`; that distinction is deliberately not expressible.
pub fn normalize_id(id: &str) -> String {
    let mut s = id.trim().to_ascii_uppercase();
    if let Some(stripped) = s.strip_suffix('+') {
        s = stripped.to_string();
    }
    for suffix in ["-OR-LATER", "-ONLY"] {
        if let Some(stripped) = s.strip_suffix(suffix) {
            // Keep `LICENSEREF-...` and anything else that merely ends in
            // those letters out of trouble by requiring a non-empty stem.
            if !stripped.is_empty() {
                s = stripped.to_string();
            }
        }
    }
    s
}

fn tokenize(input: &str) -> Vec<String> {
    // Cargo's legacy separator is `/` meaning OR (`MIT/Apache-2.0`, still
    // published by plenty of crates). Normalise it before splitting.
    let spaced = input
        .replace('/', " OR ")
        .replace('(', " ( ")
        .replace(')', " ) ");
    spaced.split_whitespace().map(|t| t.to_string()).collect()
}

struct Parser {
    tokens: Vec<String>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|s| s.as_str())
    }

    fn next(&mut self) -> Option<String> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// `or_expr := and_expr (OR and_expr)*` — OR binds loosest, matching
    /// the SPDX spec's precedence (`A AND B OR C` is `(A AND B) OR C`).
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut parts = vec![self.parse_and()?];
        while matches!(self.peek(), Some(t) if t.eq_ignore_ascii_case("OR")) {
            self.next();
            parts.push(self.parse_and()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().expect("length checked")
        } else {
            Expr::Or(parts)
        })
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut parts = vec![self.parse_atom()?];
        while matches!(self.peek(), Some(t) if t.eq_ignore_ascii_case("AND")) {
            self.next();
            parts.push(self.parse_atom()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().expect("length checked")
        } else {
            Expr::And(parts)
        })
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        let token = self
            .next()
            .ok_or_else(|| "unexpected end of expression".to_string())?;
        if token == "(" {
            let inner = self.parse_or()?;
            match self.next() {
                Some(t) if t == ")" => Ok(inner),
                _ => Err("unbalanced parenthesis".to_string()),
            }
        } else if token == ")" {
            Err("unexpected ')'".to_string())
        } else {
            let mut exception = None;
            if matches!(self.peek(), Some(t) if t.eq_ignore_ascii_case("WITH")) {
                self.next();
                exception = self.next();
            }
            Ok(Expr::Id {
                id: token,
                exception,
            })
        }
    }
}

/// Parses an SPDX expression. Anything that is not a well-formed
/// expression (`"see LICENSE file"`, an empty string) is an error rather
/// than being silently treated as a license id — the caller reports those
/// as unresolved instead of pretending to have an answer.
pub fn parse(input: &str) -> Result<Expr, String> {
    let tokens = tokenize(input);
    if tokens.is_empty() {
        return Err("empty license expression".to_string());
    }
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.parse_or()?;
    if parser.pos != parser.tokens.len() {
        return Err(format!("trailing tokens in license expression: {input:?}"));
    }
    Ok(expr)
}

/// Collects every license id mentioned anywhere in the expression, in
/// source order. Used for reporting *which* id tripped a denylist.
pub fn ids(expr: &Expr) -> Vec<String> {
    let mut out = Vec::new();
    collect_ids(expr, &mut out);
    out
}

fn collect_ids(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Id { id, .. } => out.push(id.clone()),
        Expr::Or(parts) | Expr::And(parts) => {
            for p in parts {
                collect_ids(p, out);
            }
        }
    }
}

fn leaf_matches(id: &str, exception: Option<&str>, set: &[String]) -> bool {
    // `Apache-2.0 WITH LLVM-exception` matches a policy entry written out
    // in full, and also a plain `Apache-2.0` entry: the exception only
    // ever grants extra permission, so allowing the base license implies
    // allowing it with an exception, and denying the base license should
    // deny it with one too.
    let normalized = normalize_id(id);
    if let Some(exc) = exception {
        let full = normalize_id(&format!("{id} WITH {exc}"));
        if set.iter().any(|e| normalize_id(e) == full) {
            return true;
        }
    }
    set.iter().any(|e| normalize_id(e) == normalized)
}

/// True if the expression can be satisfied using only licenses in
/// `allow`. `OR` needs one branch satisfied; `AND` needs all of them.
///
/// This is the part a substring check gets wrong: given
/// `allow = ["MIT", "Apache-2.0"]`, the real crates.io expression
/// `(MIT OR Apache-2.0) AND Unicode-3.0` is **not** satisfied, because
/// the `AND Unicode-3.0` half binds you regardless of which side of the
/// `OR` you take.
pub fn satisfies_allowlist(expr: &Expr, allow: &[String]) -> bool {
    match expr {
        Expr::Id { id, exception } => leaf_matches(id, exception.as_deref(), allow),
        Expr::Or(parts) => parts.iter().any(|p| satisfies_allowlist(p, allow)),
        Expr::And(parts) => parts.iter().all(|p| satisfies_allowlist(p, allow)),
    }
}

/// True if the expression is unavoidably denied — i.e. every way of
/// satisfying it requires a license in `deny`.
///
/// This is the exact dual of the allowlist rule, and it matters:
/// `MIT OR GPL-3.0` under `deny = ["GPL-3.0"]` is **not** a violation,
/// because you can simply take the MIT branch. `MIT AND GPL-3.0` is.
pub fn violates_denylist(expr: &Expr, deny: &[String]) -> bool {
    match expr {
        Expr::Id { id, exception } => leaf_matches(id, exception.as_deref(), deny),
        Expr::Or(parts) => parts.iter().all(|p| violates_denylist(p, deny)),
        Expr::And(parts) => parts.iter().any(|p| violates_denylist(p, deny)),
    }
}

/// The first id in the expression that is itself on `deny`, for the
/// "denied by ..." note in the report.
pub fn offending_id(expr: &Expr, deny: &[String]) -> Option<String> {
    ids(expr)
        .into_iter()
        .find(|id| leaf_matches(id, None, deny))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_a_plain_id() {
        assert_eq!(
            parse("MIT").unwrap(),
            Expr::Id {
                id: "MIT".into(),
                exception: None
            }
        );
    }

    #[test]
    fn parses_the_legacy_cargo_slash_form_as_or() {
        // `libmount 0.1.15` on crates.io really is published as
        // "MIT/Apache-2.0" — the pre-SPDX Cargo spelling.
        let expr = parse("MIT/Apache-2.0").unwrap();
        assert!(satisfies_allowlist(&expr, &allow(&["Apache-2.0"])));
        assert!(satisfies_allowlist(&expr, &allow(&["MIT"])));
    }

    #[test]
    fn or_is_satisfied_by_either_branch() {
        let expr = parse("MIT OR Apache-2.0").unwrap();
        assert!(satisfies_allowlist(&expr, &allow(&["Apache-2.0"])));
        assert!(!satisfies_allowlist(&expr, &allow(&["BSD-3-Clause"])));
    }

    #[test]
    fn and_requires_every_branch() {
        // This is the real `unicode-ident 1.0.24` expression from this
        // monorepo's own dependency tree.
        let expr = parse("(MIT OR Apache-2.0) AND Unicode-3.0").unwrap();
        assert!(
            !satisfies_allowlist(&expr, &allow(&["MIT", "Apache-2.0"])),
            "the AND half still binds you, so a MIT+Apache allowlist is not enough"
        );
        assert!(satisfies_allowlist(
            &expr,
            &allow(&["MIT", "Apache-2.0", "Unicode-3.0"])
        ));
    }

    #[test]
    fn or_with_one_denied_branch_is_not_a_violation() {
        let expr = parse("MIT OR GPL-3.0").unwrap();
        assert!(
            !violates_denylist(&expr, &allow(&["GPL-3.0"])),
            "you can take the MIT branch, so this is dual-licensed and fine"
        );
    }

    #[test]
    fn and_with_one_denied_branch_is_a_violation() {
        let expr = parse("MIT AND GPL-3.0").unwrap();
        assert!(violates_denylist(&expr, &allow(&["GPL-3.0"])));
        assert_eq!(
            offending_id(&expr, &allow(&["GPL-3.0"])).as_deref(),
            Some("GPL-3.0")
        );
    }

    #[test]
    fn deny_gpl3_catches_only_and_or_later_and_plus_spellings() {
        for spelling in ["GPL-3.0", "GPL-3.0-only", "GPL-3.0-or-later", "GPL-3.0+"] {
            let expr = parse(spelling).unwrap();
            assert!(
                violates_denylist(&expr, &allow(&["GPL-3.0"])),
                "{spelling} should be caught by a GPL-3.0 denylist entry"
            );
        }
    }

    #[test]
    fn deny_gpl3_does_not_catch_lgpl_or_agpl() {
        // The substring-matching bug this evaluator exists to avoid:
        // "GPL-3.0" is a substring of neither of these as an *id*, but is
        // as raw text.
        for unrelated in ["LGPL-3.0-only", "AGPL-3.0", "LGPL-2.1"] {
            let expr = parse(unrelated).unwrap();
            assert!(
                !violates_denylist(&expr, &allow(&["GPL-3.0"])),
                "{unrelated} must not be caught by a GPL-3.0 denylist entry"
            );
        }
        assert!(violates_denylist(
            &parse("AGPL-3.0").unwrap(),
            &allow(&["AGPL-3.0"])
        ));
    }

    #[test]
    fn matching_is_case_insensitive_both_ways() {
        let expr = parse("mit or apache-2.0").unwrap();
        assert!(satisfies_allowlist(&expr, &allow(&["MIT"])));
        assert!(violates_denylist(
            &parse("GPL-3.0").unwrap(),
            &allow(&["gpl-3.0"])
        ));
    }

    #[test]
    fn with_exception_matches_the_base_license_and_the_full_form() {
        let expr = parse("Apache-2.0 WITH LLVM-exception").unwrap();
        assert!(satisfies_allowlist(&expr, &allow(&["Apache-2.0"])));
        assert!(satisfies_allowlist(
            &expr,
            &allow(&["Apache-2.0 WITH LLVM-exception"])
        ));
        assert!(!satisfies_allowlist(&expr, &allow(&["MIT"])));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // `A AND B OR C` must parse as `(A AND B) OR C`, so C alone is
        // enough to satisfy it.
        let expr = parse("MIT AND Unicode-3.0 OR BSD-3-Clause").unwrap();
        assert!(satisfies_allowlist(&expr, &allow(&["BSD-3-Clause"])));
        assert!(!satisfies_allowlist(&expr, &allow(&["MIT"])));
    }

    #[test]
    fn junk_is_an_error_not_a_silent_license_id() {
        assert!(parse("").is_err());
        assert!(parse("(MIT OR Apache-2.0").is_err());
        assert!(parse("MIT OR").is_err());
    }

    #[test]
    fn nonsense_words_parse_as_ids_but_match_nothing() {
        // `"BSD-like"` is a real value in one of this repo's package-lock
        // files. It parses as an id and simply matches no policy entry —
        // it does not crash, and it does not accidentally match `BSD`.
        let expr = parse("BSD-like").unwrap();
        assert!(!satisfies_allowlist(&expr, &allow(&["BSD-3-Clause"])));
        assert!(!violates_denylist(&expr, &allow(&["GPL-3.0"])));
    }
}
