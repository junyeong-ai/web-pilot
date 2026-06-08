//! Shared `*`-glob matching for URL selectors — `frame url <pattern>` and
//! `tab find --url <pattern>`. One definition so the two selectors behave
//! identically and neither can regress to the old star-stripping substring,
//! which broke a middle `*` and let an empty or all-`*` pattern match everything.
//! The browser-mode `frame url` mirror is `urlGlobMatch` in `browser.js` (JS
//! can't share this); a unit test in each guards the parity.

/// Whether `pattern` is empty or only wildcards/whitespace. Such a pattern would
/// match every URL, so a selector that must pick exactly one rejects it up front
/// rather than silently switching to the first listed frame/tab.
pub fn is_blank(pattern: &str) -> bool {
    pattern.replace('*', "").trim().is_empty()
}

/// Match `url` against a `*`-glob `pattern`: every non-`*` run of the pattern
/// must appear in `url` in order, with `*` standing for any (possibly empty) run
/// between them. There is no start/end anchoring — it is a *contains* match, the
/// least surprising reading of "find the one whose URL has this in it": `/auth/`
/// matches any URL holding it, and `accounts.*/o/oauth2` spans a wildcard.
/// Callers reject a [`is_blank`] pattern first, so the segment iterator is never
/// empty (an empty iterator would vacuously match everything).
pub fn matches(pattern: &str, url: &str) -> bool {
    let mut cursor = 0;
    for segment in pattern.split('*').filter(|s| !s.is_empty()) {
        match url[cursor..].find(segment) {
            Some(rel) => cursor += rel + segment.len(),
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_substring_and_wildcards() {
        // Plain substring (the common case).
        assert!(matches("/auth/", "https://x.com/auth/login"));
        assert!(!matches("/auth/", "https://x.com/login"));
        // Leading/trailing `*` are no-ops for a contains match.
        assert!(matches("*auth*", "https://x.com/auth/login"));
        // A middle `*` spans arbitrary characters in order — the case the old
        // `replace('*', "")` broke (it searched for "authlogin").
        assert!(matches("auth*login", "https://x.com/auth/x/login"));
        assert!(!matches("login*auth", "https://x.com/auth/login"));
    }

    #[test]
    fn blank_detects_empty_and_all_wildcard() {
        for blank in ["", "  ", "*", "**", " * "] {
            assert!(is_blank(blank), "{blank:?} is blank");
        }
        for real in ["/auth/", "*auth*", "a"] {
            assert!(!is_blank(real), "{real:?} is not blank");
        }
    }
}
