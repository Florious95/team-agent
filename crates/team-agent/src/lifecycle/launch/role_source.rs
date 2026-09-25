//! ---
//! purpose: 把源席最新角色文件物化到 dynamic-role-files
//! contract:
//!   provides:
//!     - name: materialize_latest_role
//!       what: clone/add 用的角色落盘，以磁盘最新文件为准
//! boundary:
//!   - 不负责 fork 窗口注入
//!   - 不读 provider session 落盘
//! maturity: wired
//! ---
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::lifecycle::LifecycleError;
use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value};

fn set_yaml_map_value(value: &mut Value, key: &str, next: Value) -> Result<(), LifecycleError> {
    let Value::Map(pairs) = value else {
        return Err(LifecycleError::Compile(
            "agent entry is not a map".to_string(),
        ));
    };
    let mut found = false;
    for (_, existing) in pairs
        .iter_mut()
        .filter(|(existing_key, _)| existing_key.as_str() == key)
    {
        *existing = next.clone();
        found = true;
    }
    if !found {
        pairs.push((key.to_string(), next));
    }
    Ok(())
}

fn strip_label_quotes(mut label: &str) -> &str {
    loop {
        let bytes = label.as_bytes();
        if bytes.len() >= 2
            && matches!(bytes[0], b'\'' | b'"')
            && bytes[0] == bytes[bytes.len() - 1]
        {
            label = &label[1..label.len() - 1];
        } else {
            return label;
        }
    }
}

pub(crate) struct MaterializedRole {
    path: PathBuf,
    device: u64,
    inode: u64,
    keep: bool,
}

impl MaterializedRole {
    /// ---
    /// purpose: 取物化出来的角色文件路径
    /// returns: 落盘路径
    /// ---
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// ---
    /// purpose: 标记该文件由调用方接管，Drop 时不再删除
    /// ---
    pub(crate) fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for MaterializedRole {
    fn drop(&mut self) {
        if !self.keep
            && std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
                metadata.file_type().is_file()
                    && metadata.dev() == self.device
                    && metadata.ino() == self.inode
            })
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// ---
/// purpose: 读源席最新角色文件，改名后物化到托管目录
/// params:
///   state: 用于找源席的 dynamic_role_file，找不到时退到 team 目录下的同名 md
///   as_agent_id: 新席位名，写进 front matter 的 name
///   label: 非空时覆盖 front matter 的 role
///   agent_label: 非空时写入 front matter 的 label
/// returns: 物化结果，未调用 keep 时 Drop 会删掉该文件
/// errors: 源文件缺失、未声明 name 或声明与源席不符时返回 Compile；目标已存在返回 RequirementUnmet；建目录或写盘失败返回 StatePersist
/// ---
pub(crate) fn materialize_latest_role(
    run_workspace: &Path,
    team_dir: &Path,
    state: &serde_json::Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
    agent_label: Option<&str>,
) -> Result<MaterializedRole, LifecycleError> {
    let source_path = resolve_role_source(run_workspace, team_dir, state, source_agent_id)?;
    let (mut meta, body) = crate::compiler::read_front_matter(&source_path)
        .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    let declared = meta
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            LifecycleError::Compile(format!(
                "source role file does not declare name: {}",
                source_path.display()
            ))
        })?;
    if declared != source_agent_id.as_str() {
        return Err(LifecycleError::Compile(format!(
            "source role file declares name '{}' but source agent is '{}'",
            declared, source_agent_id
        )));
    }
    set_yaml_map_value(
        &mut meta,
        "name",
        Value::Str(as_agent_id.as_str().to_string()),
    )?;
    if let Some(label) = label
        .map(strip_label_quotes)
        .filter(|value| !value.is_empty())
    {
        set_yaml_map_value(&mut meta, "role", Value::Str(label.to_string()))?;
    }
    if let Some(agent_label) = agent_label
        .map(strip_label_quotes)
        .filter(|value| !value.is_empty())
    {
        set_yaml_map_value(&mut meta, "label", Value::Str(agent_label.to_string()))?;
    }
    if let Some(role) = meta.get("role").and_then(Value::as_str).map(str::to_string) {
        let role_without_quotes = strip_label_quotes(&role);
        if !role_without_quotes.is_empty() && role_without_quotes != role {
            set_yaml_map_value(
                &mut meta,
                "role",
                Value::Str(role_without_quotes.to_string()),
            )?;
        }
    }

    let managed_dir = run_workspace.join(".team").join("dynamic-role-files");
    std::fs::create_dir_all(&managed_dir)
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    let path = managed_dir.join(format!("{}.md", as_agent_id.as_str()));
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(LifecycleError::RequirementUnmet(format!(
                "managed role file already exists: {}",
                path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(LifecycleError::StatePersist(error.to_string())),
    }
    let rendered = format!("---\n{}---\n\n{}", yaml::dumps(&meta), body);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?
        .as_nanos();
    let temp = managed_dir.join(format!(
        ".{}.md.tmp-{}-{nonce}",
        as_agent_id.as_str(),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| LifecycleError::StatePersist(error.to_string()))?;
    if let Err(error) = file
        .write_all(rendered.as_bytes())
        .and_then(|_| file.sync_all())
    {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(LifecycleError::StatePersist(error.to_string()));
    }
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            drop(file);
            let _ = std::fs::remove_file(&temp);
            return Err(LifecycleError::StatePersist(error.to_string()));
        }
    };
    let (device, inode) = (metadata.dev(), metadata.ino());
    drop(file);
    if let Err(error) = std::fs::hard_link(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(LifecycleError::RequirementUnmet(format!(
                "managed role file already exists: {}",
                path.display()
            )));
        }
        return Err(LifecycleError::StatePersist(error.to_string()));
    }
    if let Err(error) = std::fs::remove_file(&temp) {
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
            metadata.file_type().is_file() && metadata.dev() == device && metadata.ino() == inode
        }) {
            let _ = std::fs::remove_file(&path);
        }
        return Err(LifecycleError::StatePersist(error.to_string()));
    }
    Ok(MaterializedRole {
        path,
        device,
        inode,
        keep: false,
    })
}

pub(crate) fn resolve_role_source(
    run_workspace: &Path,
    team_dir: &Path,
    state: &serde_json::Value,
    source_agent_id: &AgentId,
) -> Result<PathBuf, LifecycleError> {
    if let Some(raw) = state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .and_then(|agent| agent.get("dynamic_role_file"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(raw);
        let resolved = if path.is_absolute() {
            path
        } else {
            run_workspace.join(path)
        };
        if resolved.is_file() {
            return Ok(resolved);
        }
        return Err(LifecycleError::Compile(format!(
            "source dynamic role file not found: {}",
            resolved.display()
        )));
    }
    let path = team_dir
        .join("agents")
        .join(format!("{}.md", source_agent_id.as_str()));
    if path.is_file() {
        Ok(path)
    } else {
        Err(LifecycleError::Compile(format!(
            "source role file not found: {}",
            path.display()
        )))
    }
}
