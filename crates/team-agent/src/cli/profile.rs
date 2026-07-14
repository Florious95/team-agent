//! `profile {init,doctor,show}` CLI verb.

use super::*;

const SECRET_BOUNDARY: &str = "# Team Agent Profile Secret Boundary\n";

pub fn cmd_profile(args: &ProfileArgs) -> Result<CmdResult, CliError> {
    let value = match args.command.as_str() {
        "init" => init_profile(args)?,
        "doctor" => doctor_profile(args)?,
        "show" => show_profile(args)?,
        other => {
            return Err(CliError::Usage(format!("invalid profile command: {other}")));
        }
    };
    Ok(CmdResult::from_json(value, args.json))
}

fn init_profile(args: &ProfileArgs) -> Result<Value, CliError> {
    let auth_mode = args.auth_mode.as_deref().unwrap_or("subscription");
    validate_auth_mode(auth_mode)?;
    let dir = profile_dir(args);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.env", args.name));
    let template_path = dir.join(format!("{}.example.env", args.name));
    let body = profile_template(&args.name, auth_mode);
    let created_profile = write_new_file(&path, &body)?;
    if created_profile {
        set_owner_only_permissions(&path)?;
    }
    let created_template = write_new_file(&template_path, &body)?;
    write_boundary_files(&dir)?;

    let mut obj = Map::new();
    obj.insert("ok".to_string(), Value::Bool(true));
    obj.insert("profile".to_string(), Value::String(args.name.clone()));
    obj.insert(
        "auth_mode".to_string(),
        Value::String(auth_mode.to_string()),
    );
    obj.insert("path".to_string(), path_value(&path));
    obj.insert("template_path".to_string(), path_value(&template_path));
    obj.insert("created_profile".to_string(), Value::Bool(created_profile));
    obj.insert(
        "created_template".to_string(),
        Value::Bool(created_template),
    );
    obj.insert("secret_written".to_string(), Value::Bool(created_profile));
    obj.insert(
        "safe_inspection_command".to_string(),
        Value::String(safe_inspection_command(args)),
    );
    obj.insert(
        "raw_file_read_allowed_for_agents".to_string(),
        Value::Bool(false),
    );
    obj.insert(
        "instruction".to_string(),
        Value::String("Inspect credentials with team-agent profile show; do not read raw profile files from agent context.".to_string()),
    );
    Ok(Value::Object(obj))
}

fn doctor_profile(args: &ProfileArgs) -> Result<Value, CliError> {
    let dir = profile_dir(args);
    let path = dir.join(format!("{}.env", args.name));
    let template_path = dir.join(format!("{}.example.env", args.name));
    if !path.exists() {
        return Ok(missing_profile_value(args, &path, Some(&template_path)));
    }
    let values = read_profile_env(&path)?;
    let auth_mode =
        profile_value(&values, "AUTH_MODE").unwrap_or_else(|| "subscription".to_string());
    let secret_keys = secret_keys_present(&values);
    let keys = sorted_keys(&values);
    let mut obj = Map::new();
    obj.insert("ok".to_string(), Value::Bool(true));
    obj.insert("path".to_string(), path_value(&path));
    obj.insert("profile".to_string(), Value::String(args.name.clone()));
    obj.insert(
        "credential_present".to_string(),
        Value::Bool(credential_present(&values)),
    );
    obj.insert("auth_mode".to_string(), Value::String(auth_mode));
    obj.insert("keys_present".to_string(), string_array(keys));
    obj.insert(
        "raw_file_read_allowed_for_agents".to_string(),
        Value::Bool(false),
    );
    obj.insert(
        "redaction_engine".to_string(),
        Value::String("key_name".to_string()),
    );
    obj.insert("safe_for_agent_context".to_string(), Value::Bool(true));
    obj.insert(
        "safe_inspection_command".to_string(),
        Value::String(safe_inspection_command(args)),
    );
    obj.insert("secret_keys_present".to_string(), string_array(secret_keys));
    obj.insert("secret_values_printed".to_string(), Value::Bool(false));
    obj.insert("suggestion".to_string(), Value::Null);
    obj.insert("template_path".to_string(), path_value(&template_path));
    Ok(Value::Object(obj))
}

fn show_profile(args: &ProfileArgs) -> Result<Value, CliError> {
    let path = profile_dir(args).join(format!("{}.env", args.name));
    if !path.exists() {
        return Ok(missing_profile_value(args, &path, None));
    }
    let values = read_profile_env(&path)?;
    let auth_mode =
        profile_value(&values, "AUTH_MODE").unwrap_or_else(|| "subscription".to_string());
    let secret_keys = secret_keys_present(&values);
    let missing_common = missing_common_keys(&values, &auth_mode);
    let mut obj = Map::new();
    obj.insert("ok".to_string(), Value::Bool(true));
    obj.insert("profile".to_string(), Value::String(args.name.clone()));
    obj.insert(
        "credential_present".to_string(),
        Value::Bool(credential_present(&values)),
    );
    obj.insert("auth_mode".to_string(), Value::String(auth_mode));
    obj.insert("values".to_string(), redacted_values(&values));
    obj.insert(
        "keys_present".to_string(),
        string_array(sorted_keys(&values)),
    );
    obj.insert("secret_keys_present".to_string(), string_array(secret_keys));
    obj.insert("missing_common".to_string(), string_array(missing_common));
    obj.insert("safe_for_agent_context".to_string(), Value::Bool(true));
    obj.insert("secret_values_printed".to_string(), Value::Bool(false));
    obj.insert(
        "raw_file_read_allowed_for_agents".to_string(),
        Value::Bool(false),
    );
    obj.insert(
        "instruction".to_string(),
        Value::String("Values are redacted for safe inspection by agents.".to_string()),
    );
    Ok(Value::Object(obj))
}

fn profile_dir(args: &ProfileArgs) -> PathBuf {
    let _ = &args.team;
    args.workspace
        .join(".team")
        .join("current")
        .join("profiles")
}

fn validate_auth_mode(auth_mode: &str) -> Result<(), CliError> {
    match auth_mode {
        "compatible_api" | "official_api" | "subscription" => Ok(()),
        other => Err(CliError::Usage(format!("invalid --auth-mode: {other}"))),
    }
}

fn profile_template(name: &str, auth_mode: &str) -> String {
    match auth_mode {
        "compatible_api" | "official_api" => {
            format!("AUTH_MODE={auth_mode}\nPROFILE_NAME={name}\nAPI_KEY=\nMODEL=\n")
        }
        _ => format!("AUTH_MODE=subscription\nPROFILE_NAME={name}\n"),
    }
}

fn write_new_file(path: &Path, body: &str) -> Result<bool, CliError> {
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(path, body)?;
    Ok(true)
}

fn write_boundary_files(dir: &Path) -> Result<(), CliError> {
    for name in ["AGENTS.md", "CLAUDE.md"] {
        write_new_file(&dir.join(name), SECRET_BOUNDARY)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_permissions(path: &Path) -> Result<(), CliError> {
    let _ = path;
    Ok(())
}

fn read_profile_env(path: &Path) -> Result<Map<String, Value>, CliError> {
    let raw = std::fs::read_to_string(path)?;
    let mut values = Map::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        values.insert(key.to_string(), Value::String(value.to_string()));
    }
    Ok(values)
}

fn redacted_values(values: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for key in sorted_keys(values) {
        let value = values.get(&key).and_then(Value::as_str).unwrap_or("");
        let mut item = Map::new();
        item.insert("present".to_string(), Value::Bool(!value.is_empty()));
        if is_secret_key(&key) {
            item.insert("redacted".to_string(), Value::Bool(true));
        } else {
            item.insert("value".to_string(), Value::String(value.to_string()));
        }
        out.insert(key, Value::Object(item));
    }
    Value::Object(out)
}

fn profile_value(values: &Map<String, Value>, key: &str) -> Option<String> {
    values
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn missing_profile_value(args: &ProfileArgs, path: &Path, template_path: Option<&Path>) -> Value {
    let mut obj = Map::new();
    obj.insert("ok".to_string(), Value::Bool(false));
    obj.insert("profile".to_string(), Value::String(args.name.clone()));
    obj.insert("path".to_string(), path_value(path));
    if let Some(template) = template_path {
        obj.insert("template_path".to_string(), path_value(template));
    }
    obj.insert(
        "suggestion".to_string(),
        Value::String(format!(
            "Run team-agent profile init {} --auth-mode subscription.",
            args.name
        )),
    );
    obj.insert(
        "safe_inspection_command".to_string(),
        Value::String(safe_inspection_command(args)),
    );
    Value::Object(obj)
}

fn safe_inspection_command(args: &ProfileArgs) -> String {
    format!(
        "team-agent profile show {} --workspace {}",
        args.name,
        args.workspace.display()
    )
}

fn path_value(path: &Path) -> Value {
    Value::String(path.to_string_lossy().to_string())
}

fn sorted_keys(values: &Map<String, Value>) -> Vec<String> {
    let mut keys = values.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

fn string_array(values: Vec<String>) -> Value {
    Value::Array(values.into_iter().map(Value::String).collect())
}

fn secret_keys_present(values: &Map<String, Value>) -> Vec<String> {
    sorted_keys(values)
        .into_iter()
        .filter(|key| is_secret_key(key))
        .collect()
}

fn credential_present(values: &Map<String, Value>) -> bool {
    values
        .iter()
        .any(|(key, value)| is_secret_key(key) && value.as_str().is_some_and(|raw| !raw.is_empty()))
}

fn missing_common_keys(values: &Map<String, Value>, auth_mode: &str) -> Vec<String> {
    let required = match auth_mode {
        "compatible_api" | "official_api" => ["API_KEY", "MODEL"].as_slice(),
        _ => ["AUTH_MODE", "PROFILE_NAME"].as_slice(),
    };
    required
        .iter()
        .filter(|key| {
            values
                .get(**key)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        })
        .map(|key| (*key).to_string())
        .collect()
}

fn is_secret_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || upper.ends_with("_KEY")
}
