//! A small glob matcher for `ingest.local.deny`: `*` and `?` stay inside one path
//! segment, `**` crosses segments. Matching is on `/`-separated canonical paths.

pub(crate) fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = normalize(pattern).chars().collect();
    let t: Vec<char> = normalize(text).chars().collect();
    go(&p, &t)
}

fn normalize(s: &str) -> String {
    let s = s.replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn go(p: &[char], t: &[char]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some('*') if p.get(1) == Some(&'*') => {
            let rest = &p[2..];
            // `**/` also matches zero segments.
            let rest = rest.strip_prefix(&['/']).unwrap_or(rest);
            (0..=t.len()).any(|i| go(rest, &t[i..]))
        }
        Some('*') => {
            let rest = &p[1..];
            let mut i = 0;
            loop {
                if go(rest, &t[i..]) {
                    return true;
                }
                if i >= t.len() || t[i] == '/' {
                    return false;
                }
                i += 1;
            }
        }
        Some('?') => t.first().is_some_and(|c| *c != '/') && go(&p[1..], &t[1..]),
        Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn globs() {
        assert!(matches("/home/u/secret/**", "/home/u/secret/a/b.txt"));
        assert!(matches("/home/u/secret/**", "/home/u/secret/a.txt"));
        assert!(!matches("/home/u/secret/*", "/home/u/secret/a/b.txt"));
        assert!(matches("**/*.pem", "/x/y/key.pem"));
        assert!(matches("**/id_?sa", "/home/u/.ssh/id_rsa"));
        assert!(!matches("**/*.pem", "/x/y/key.pem.txt"));
        assert!(matches("/a/b.txt", "/a/b.txt"));
    }
}
