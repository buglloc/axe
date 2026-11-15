pub(super) const DEFAULT_ACCEPT_ENV: &str = "LANG,LC_*";
pub(super) const MAX_VARIABLES: usize = 64;
pub(super) const MAX_ENVIRONMENT_BYTES: usize = 32 * 1024;

pub(super) fn accepts(patterns: &[String], name: &str, value: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.bytes().any(|byte| byte == 0 || byte == b'=')
        && value.len() <= 4096
        && !value.contains('\0')
        && patterns
            .iter()
            .any(|pattern| matches_pattern(pattern.as_bytes(), name.as_bytes()))
}

fn matches_pattern(pattern: &[u8], name: &[u8]) -> bool {
    let (mut input, mut position) = (0, 0);
    let mut star = None;
    let mut consumed = 0;
    while input < name.len() {
        if pattern.get(position) == Some(&b'*') {
            star = Some(position);
            position += 1;
            consumed = input;
        } else if pattern
            .get(position)
            .is_some_and(|byte| *byte == b'?' || *byte == name[input])
        {
            input += 1;
            position += 1;
        } else if let Some(retry) = star {
            consumed += 1;
            input = consumed;
            position = retry + 1;
        } else {
            return false;
        }
    }
    pattern[position..].iter().all(|byte| *byte == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_openssh_wildcards_without_shell_globbing() {
        for (pattern, name, accepted) in [
            ("LANG", "LANG", true),
            ("LANG", "LANGUAGE", false),
            ("LC_*", "LC_", true),
            ("LC_*", "LC_ALL", true),
            ("*TOKEN*", "MY_TOKEN_FILE", true),
            ("TEST_?L*WED", "TEST_ALLOWED", true),
            ("TEST_?L*WED", "TEST_AWED", false),
            ("A**?*Z", "ABZ", true),
            ("A*B?C", "ABXXC", false),
            ("*A*B", "AABA", false),
            ("*A*B", "ABAAB", true),
            ("?", "A", true),
            ("?", "AB", false),
            ("*?", "*", true),
            ("a*", "ABC", false),
            ("FOO[AB]", "FOOA", false),
            ("FOO[AB]", "FOO[AB]", true),
            ("!LC_*", "LC_ALL", false),
            ("", "LANG", false),
        ] {
            assert_eq!(
                accepts(&[pattern.to_owned()], name, "value"),
                accepted,
                "{pattern:?} vs {name:?}"
            );
        }
    }

    #[test]
    fn catch_all_accepts_every_valid_environment_name_without_a_denylist() {
        let patterns = [String::from("*")];
        for name in [
            "HOME",
            "PATH",
            "TERM",
            "SSH_AUTH_SOCK",
            "AXE_STORE_MODE",
            "__AXE_STATE",
            "LD_PRELOAD",
            "DYLD_LIBRARY_PATH",
            "ENV.WITH.DOTS",
            "A-B",
            "1",
            "*",
        ] {
            assert!(accepts(&patterns, name, "value"), "{name}");
        }
        for name in ["", "A=B", "A\0B"] {
            assert!(!accepts(&patterns, name, "value"), "{name:?}");
        }
        assert!(!accepts(&patterns, &"A".repeat(129), "value"));
        assert!(!accepts(&patterns, "A", &"x".repeat(4097)));
        assert!(!accepts(&patterns, "A", "x\0y"));
    }
}
