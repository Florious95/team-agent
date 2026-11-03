//! Test-only oracle for framework-emitted recovery guidance.
//!
//! Contract (unified start, end state): every command the framework tells a
//! human or agent to run must (1) name a catalog verb that is not a
//! compatibility verb and not the retired `agent add`, and (2) be admitted by
//! the strict control-argument grammar, including the confirmation flags the
//! handler requires. Guidance never re-adopts a role file with `--force`.
use super::arguments;
use super::spec::{command_spec, COMMAND_SPECS};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Retired lifecycle verbs. They stay banned from guidance after excision removes
/// them from the catalog, so the ratchet keeps working in the post-excision sweep.
const RETIRED_VERBS: &[&str] = &[
    "quick-start", "add-agent", "start-agent", "stop-agent", "reset-agent", "remove-agent",
    "clone-agent", "fork-agent", "shutdown",
];

/// Retired verbs plus anything the human catalog marks as a compatibility bridge.
pub(crate) fn compatibility_verbs() -> Vec<&'static str> {
    let mut verbs = RETIRED_VERBS.to_vec();
    verbs.extend(
        COMMAND_SPECS
            .iter()
            .filter(|spec| spec.summary.starts_with("Compatibility"))
            .map(|spec| spec.name)
            .filter(|name| !RETIRED_VERBS.contains(name)),
    );
    verbs
}

const STOP_WORDS: &[&str] = &[
    "then", "or", "and", "followed", "before", "after", "to", "for", "if", "when", "first",
    "in", "with", "only", "from", "is", "are", "until", "so", "once",
];
const PLACEHOLDERS: &[&str] = &[
    "AGENT", "NEW_AGENT", "NEW_ID", "ID", "TEAMDIR", "WORKSPACE", "PATH", "FILE", "TEAM",
];

fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    for ch in text.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

fn normalize(words: Vec<String>) -> Vec<String> {
    let mut argv = Vec::new();
    for word in words {
        if STOP_WORDS.contains(&word.as_str()) || word.starts_with('(') {
            break;
        }
        // `.`/`..` are paths; only a punctuated word ends the sentence.
        let path_dots = word.chars().all(|c| c == '.');
        let ends_sentence = !path_dots
            && word.ends_with(['.', ',', ')', ':'])
            && !word.starts_with("--");
        let word = if path_dots {
            word
        } else {
            word.trim_end_matches(['.', ',', ')', ':']).to_string()
        };
        let word = if (word.starts_with('<') && word.ends_with('>'))
            || (word.starts_with('{') && word.ends_with('}'))
            || PLACEHOLDERS.contains(&word.as_str())
        {
            "x".to_string()
        } else {
            word
        };
        if !word.is_empty() {
            argv.push(word);
        }
        if ends_sentence {
            break;
        }
    }
    argv
}

/// Every `team-agent …` command embedded in free text, as argv after `team-agent`.
pub(crate) fn embedded_commands(text: &str) -> Vec<Vec<String>> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"team-agent\s+([^`\n;]+)").expect("command pattern"))
        .captures_iter(text)
        .map(|capture| normalize(split_words(&capture[1])))
        .collect()
}

/// Backtick-quoted commands such as `restart --force ID` that omit `team-agent`.
fn quoted_commands(text: &str) -> Vec<Vec<String>> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"`([^`]+)`").expect("quoted pattern"))
        .captures_iter(text)
        .filter_map(|capture| {
            let words = split_words(&capture[1]);
            let first = words.first()?;
            (first != "team-agent" && command_spec(first).is_some()).then(|| normalize(words))
        })
        .collect()
}

/// Admission of one argv (without the leading `team-agent`).
pub(crate) fn admit(argv: &[String]) -> Result<(), String> {
    let render = || format!("team-agent {}", argv.join(" "));
    let (verb, rest) = argv.split_first().ok_or_else(|| "empty command".to_string())?;
    if matches!(verb.as_str(), "--help" | "-h" | "--version") {
        return Ok(());
    }
    if compatibility_verbs().contains(&verb.as_str()) {
        return Err(format!("{}: compatibility verb {verb}", render()));
    }
    command_spec(verb).ok_or_else(|| format!("{}: unknown verb {verb}", render()))?;
    let (schema, args): (&str, &[String]) = match verb.as_str() {
        "start" => ("intent-start", rest),
        "stop" => ("intent-stop", rest),
        "restart" => ("intent-restart", rest),
        "team" | "agent" => {
            let (sub, args) = rest
                .split_first()
                .ok_or_else(|| format!("{}: missing {verb} operation", render()))?;
            if matches!(sub.as_str(), "--help" | "-h") {
                return Ok(());
            }
            let schema = match (verb.as_str(), sub.as_str()) {
                (_, "start") => "intent-start",
                (_, "stop") => "intent-stop",
                (_, "restart") => "intent-restart",
                ("team", "status") => "status",
                ("agent", "add") => {
                    return Err(format!("{}: retired `agent add`; use start", render()))
                }
                ("agent", "remove") => "remove-agent",
                ("agent", "reset") => "reset-agent",
                ("agent", "clone") => "clone-agent",
                ("agent", "fork") => "fork-agent",
                _ => return Err(format!("{}: unknown {verb} operation {sub}", render())),
            };
            (schema, args)
        }
        other => (other, rest),
    };
    let checked = match arguments::parse(schema, args) {
        Ok(checked) => checked,
        // Route/leader-prompt/provider launch verbs own their payload parsers.
        Err(error) if error.to_string().contains("no argument schema for") => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", render())),
    };
    let has = |flag: &str| {
        checked
            .tokens
            .iter()
            .any(|token| token == flag || token.starts_with(&format!("{flag}=")))
    };
    for (needs_schema, flag) in [
        ("reset-agent", "--discard-session"),
        ("remove-agent", "--confirm"),
        ("clone-agent", "--as"),
        ("fork-agent", "--as"),
    ] {
        if schema == needs_schema && !has(flag) {
            return Err(format!("{}: missing required {flag}", render()));
        }
    }
    Ok(())
}

/// Retired verbs recommended by a guidance cue (even without `team-agent`), the
/// retired `agent add`, `--force` re-adoption, and the inadmissible `restart --force`.
pub(crate) fn compatibility_cues(text: &str) -> Vec<String> {
    static CUE: OnceLock<Regex> = OnceLock::new();
    let cue = CUE.get_or_init(|| {
        let verbs = compatibility_verbs()
            .iter()
            .map(|verb| regex::escape(verb))
            .collect::<Vec<_>>()
            .join("|");
        Regex::new(&format!(
            r"(?i)(?:team-agent|\buse|\brun|\brerun|\bre-run|with:)\s+`?(?:team-agent\s+)?({verbs})\b|team-agent\s+agent\s+add\b|re-adopt it with --force|\brestart --force\b"
        ))
        .expect("cue pattern")
    });
    cue.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

/// All violations in one guidance text; empty means admitted.
pub(crate) fn violations(text: &str) -> Vec<String> {
    let mut found = compatibility_cues(text)
        .into_iter()
        .map(|cue| format!("compatibility guidance `{cue}`"))
        .collect::<Vec<_>>();
    found.extend(
        embedded_commands(text)
            .iter()
            .chain(quoted_commands(text).iter())
            .filter_map(|argv| admit(argv).err()),
    );
    found
}

#[track_caller]
pub(crate) fn assert_admitted(text: &str) {
    let found = violations(text);
    assert!(found.is_empty(), "inadmissible recovery guidance {found:?} in: {text}");
}

/// Verbs (with `team`/`agent` operation) of the admitted commands in `text`.
pub(crate) fn admitted_verbs(text: &str) -> Vec<String> {
    embedded_commands(text)
        .into_iter()
        .filter(|argv| admit(argv).is_ok())
        .map(|argv| match argv.first().map(String::as_str) {
            Some("team" | "agent") => argv.iter().take(2).cloned().collect::<Vec<_>>().join(" "),
            _ => argv.first().cloned().unwrap_or_default(),
        })
        .collect()
}

/// Production source of the crate with comments and `#[cfg(test)]` items removed.
fn production_lines(path: &Path, text: &str) -> Vec<(usize, String)> {
    if text.starts_with("#![cfg(test)]") {
        return Vec::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = Vec::new();
    let mut index = 0;
    let exempt_fn = path.ends_with("cli/emit.rs").then_some("fn command_help(");
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        let gated = trimmed.starts_with("#[cfg(test)]");
        let exempt = exempt_fn.is_some_and(|name| trimmed.contains(name));
        if gated || exempt {
            let mut item = index + usize::from(gated);
            while item < lines.len() && lines[item].trim_start().starts_with("#[") {
                item += 1;
            }
            let mut depth = 0i64;
            let mut opened = false;
            while item < lines.len() {
                let current = lines[item];
                depth += current.matches('{').count() as i64 - current.matches('}').count() as i64;
                opened |= current.contains('{');
                item += 1;
                if (opened && depth <= 0) || (!opened && current.trim_end().ends_with(';')) {
                    break;
                }
            }
            index = item;
            continue;
        }
        if !trimmed.starts_with("//") {
            kept.push((index + 1, line.to_string()));
        }
        index += 1;
    }
    kept
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.is_dir() {
            if !matches!(name, "tests" | "test_support" | "testdata") {
                rust_files(&path, out);
            }
        } else if name.ends_with(".rs")
            && !name.ends_with("tests.rs")
            && !name.ends_with("_test.rs")
            && !name.ends_with("_red.rs")
            && name != "guidance_oracle.rs"
        {
            out.push(path);
        }
    }
}

/// Compatibility or inadmissible guidance anywhere in production source. The command catalog and
/// the compatibility commands' own help pages describe those verbs and are exempt.
pub(crate) fn production_guidance_violations() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    let mut found = Vec::new();
    for path in files {
        let relative = path.strip_prefix(&root).unwrap_or(&path).to_path_buf();
        if relative == Path::new("cli/spec.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read source");
        // Source lines are templates, so only the cue ratchet applies here; rendered
        // guidance is parsed by the behavior tests through `violations`.
        for (line, content) in production_lines(&relative, &text) {
            for cue in compatibility_cues(&content) {
                found.push(format!("{}:{line}: `{cue}`", relative.display()));
            }
        }
    }
    found
}
