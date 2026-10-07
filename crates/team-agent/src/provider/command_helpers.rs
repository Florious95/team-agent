//! Pure append operations. Callers own ordering, filtering, escaping and env-unset policy.
use std::collections::BTreeMap;

pub(crate) fn append_pair(argv: &mut Vec<String>, flag: &str, value: impl Into<String>) {
    argv.push(flag.to_string());
    argv.push(value.into());
}

pub(crate) fn append_opt_pair(argv: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        append_pair(argv, flag, value);
    }
}

pub(crate) fn append_env_pair(env: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        env.insert(key.to_string(), value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_preserves_order_and_raw_values() {
        let mut argv = vec!["cli".to_string()];
        append_opt_pair(&mut argv, "--absent", None);
        append_opt_pair(&mut argv, "--empty", Some(""));
        append_pair(&mut argv, "--raw", " \"'\\\n ".to_string());
        append_pair(&mut argv, "--repeat", "first");
        append_pair(&mut argv, "--repeat", "last");
        assert_eq!(argv, ["cli", "--empty", "", "--raw", " \"'\\\n ", "--repeat", "first", "--repeat", "last"]);
    }

    #[test]
    fn env_append_preserves_absence_empty_values_and_last_write() {
        let mut env = BTreeMap::from([("Z".to_string(), "old".to_string())]);
        append_env_pair(&mut env, "Z", None);
        assert_eq!(env["Z"], "old");
        append_env_pair(&mut env, "A", Some(""));
        append_env_pair(&mut env, "Z", Some(" \"'\\\n "));
        assert_eq!(env.into_iter().collect::<Vec<_>>(), vec![("A".to_string(), "".to_string()), ("Z".to_string(), " \"'\\\n ".to_string())]);
    }
}
