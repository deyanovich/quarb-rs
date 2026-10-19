//! What an adapter may say about a target it refuses. A target
//! often carries credentials (`neo4j://ada:pw@db/app`), and an
//! error that quotes it would print them: in a terminal, a log, a
//! gate transcript. The password is replaced before the target is
//! quoted anywhere.

/// `target` with the password of its userinfo replaced by `***`:
/// `scheme://user:***@host/…`. A target without a password is
/// returned as it is.
pub fn redact(target: &str) -> String {
    let (head, rest) = match target.split_once("://") {
        Some((scheme, rest)) => (format!("{scheme}://"), rest),
        None => (String::new(), target),
    };
    // The userinfo ends at the last `@` before the first `/`, `?`
    // or `#` of the rest (a password may hold `@`).
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let Some(at) = rest[..authority_end].rfind('@') else {
        return target.to_string();
    };
    let userinfo = &rest[..at];
    let Some((user, _)) = userinfo.split_once(':') else {
        return target.to_string();
    };
    format!("{head}{user}:***@{}", &rest[at + 1..])
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn passwords_are_replaced_and_nothing_else() {
        assert_eq!(
            redact("neo4j://ada:pw@db:7474/app?key=name"),
            "neo4j://ada:***@db:7474/app?key=name"
        );
        assert_eq!(
            redact("arango://ada:p@ss@host/db"),
            "arango://ada:***@host/db"
        );
        assert_eq!(redact("mongodb://ada@host/db"), "mongodb://ada@host/db");
        assert_eq!(redact("sqlite.db"), "sqlite.db");
        assert_eq!(
            redact("ldap://host/dc=x?user=a:b@c"),
            "ldap://host/dc=x?user=a:b@c"
        );
        assert_eq!(redact("user:pw@host:1433/db"), "user:***@host:1433/db");
    }
}
