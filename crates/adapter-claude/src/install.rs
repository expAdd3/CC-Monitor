use serde_json::{json, Value};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;
use uuid::Uuid;

use crate::hook::HOOK_EVENTS;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookInstallation {
    pub installation_id: Uuid,
    pub staged_hook: PathBuf,
    pub database: PathBuf,
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("settings.json is malformed: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("settings root must be a JSON object")]
    Root,
}

pub fn stage_hook(source: &Path, app_data_dir: &Path) -> Result<PathBuf, InstallError> {
    let bin = app_data_dir.join("bin");
    fs::create_dir_all(&bin)?;
    let target = bin.join("cc-monitor-hook");
    atomic_copy(source, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    }
    Ok(target)
}

pub fn install(
    settings_path: &Path,
    installation: &HookInstallation,
) -> Result<PathBuf, InstallError> {
    let mut settings = read_settings(settings_path)?;
    let command = owned_command(installation);
    let legacy_hook = legacy_hook_path(settings_path);
    remove_recognized(&mut settings, Some(installation), legacy_hook.as_deref());
    let hooks = settings
        .as_object_mut()
        .ok_or(InstallError::Root)?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(InstallError::Root)?;
    for event in HOOK_EVENTS {
        let groups = hooks
            .entry((*event).to_owned())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or(InstallError::Root)?;
        let mut group = json!({"hooks":[{"type":"command","command":command}]});
        if *event == "PreToolUse" {
            group["matcher"] = json!("AskUserQuestion");
        }
        groups.push(group);
    }
    write_settings(settings_path, &settings)
}

pub fn uninstall(
    settings_path: &Path,
    installation: &HookInstallation,
) -> Result<Option<PathBuf>, InstallError> {
    if !settings_path.exists() {
        return Ok(None);
    }
    let mut settings = read_settings(settings_path)?;
    let changed = remove_recognized(&mut settings, Some(installation), None);
    if !changed {
        return Ok(None);
    }
    write_settings(settings_path, &settings).map(Some)
}

fn read_settings(path: &Path) -> Result<Value, InstallError> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let bytes = fs::read(path)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() {
        return Err(InstallError::Root);
    }
    Ok(value)
}

fn write_settings(path: &Path, settings: &Value) -> Result<PathBuf, InstallError> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("settings has no parent"))?;
    fs::create_dir_all(parent)?;
    let backup = path.with_extension("json.cc-monitor-backup");
    #[cfg(unix)]
    let original_mode = if path.exists() {
        Some(unix_mode(path)?)
    } else {
        None
    };
    if path.exists() && !backup.exists() {
        fs::copy(path, &backup)?;
        #[cfg(unix)]
        set_unix_mode(&backup, original_mode.expect("existing file has a mode"))?;
    }
    let temporary = parent.join(format!(".settings.json.{}.tmp", Uuid::now_v7()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        #[cfg(unix)]
        set_unix_mode(&temporary, original_mode.unwrap_or(0o600))?;
        serde_json::to_writer_pretty(&mut file, settings)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok::<_, InstallError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(backup)
}

#[cfg(unix)]
fn unix_mode(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(path)?.permissions().mode() & 0o7777)
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn atomic_copy(source: &Path, target: &Path) -> Result<(), InstallError> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::other("target has no parent"))?;
    let temporary = parent.join(format!(".cc-monitor-hook.{}.tmp", Uuid::now_v7()));
    let result = (|| {
        fs::copy(source, &temporary)?;
        fs::rename(&temporary, target)?;
        Ok::<_, io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}

fn shell_quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn owned_command(installation: &HookInstallation) -> String {
    format!(
        "{} --database {} --installation-id {} || true",
        shell_quote(&installation.staged_hook),
        shell_quote(&installation.database),
        installation.installation_id
    )
}

fn remove_recognized(
    settings: &mut Value,
    owner: Option<&HookInstallation>,
    legacy_hook: Option<&Path>,
) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut changed = false;
    for groups in hooks.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let before = entries.len();
            entries.retain(|entry| {
                !entry
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| {
                        is_owned(command, owner)
                            || legacy_hook.is_some_and(|path| is_legacy(command, path))
                    })
            });
            changed |= entries.len() != before;
        }
        groups.retain(|group| {
            group
                .get("hooks")
                .and_then(Value::as_array)
                .is_none_or(|entries| !entries.is_empty())
        });
    }
    changed
}

fn is_owned(command: &str, owner: Option<&HookInstallation>) -> bool {
    owner.is_some_and(|installation| command == owned_command(installation))
}

fn legacy_hook_path(settings_path: &Path) -> Option<PathBuf> {
    let claude_dir = settings_path.parent()?;
    let home = claude_dir.parent()?;
    Some(home.join(".cc-monitor/cc_hook.py"))
}

fn is_legacy(command: &str, expected_script: &Path) -> bool {
    let Ok(tokens) = shell_words::split(command) else {
        return false;
    };
    if tokens.len() != 2 && tokens.len() != 4 {
        return false;
    }
    let executable = Path::new(&tokens[0])
        .file_name()
        .and_then(|name| name.to_str());
    let valid_executable = matches!(executable, Some("python3"));
    let valid_script = Path::new(&tokens[1]) == expected_script;
    let valid_suffix = tokens.len() == 2 || (tokens[2] == "||" && tokens[3] == "true");
    valid_executable && valid_script && valid_suffix
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn installation(root: &Path) -> HookInstallation {
        HookInstallation {
            installation_id: Uuid::now_v7(),
            staged_hook: root.join("bin/cc-monitor-hook"),
            database: root.join("state.db"),
        }
    }

    #[test]
    fn install_is_idempotent_and_preserves_unrelated_hooks() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join(".claude/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(&settings, r#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"other"}]}]}}"#).unwrap();
        let install = installation(dir.path());
        install_hook_twice(&settings, &install);
        let value: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(
            value["hooks"]["PreToolUse"][0]["matcher"],
            "AskUserQuestion"
        );
        assert!(settings.with_extension("json.cc-monitor-backup").exists());
    }

    fn install_hook_twice(settings: &Path, install_spec: &HookInstallation) {
        install(settings, install_spec).unwrap();
        install(settings, install_spec).unwrap();
    }

    #[test]
    fn uninstall_requires_exact_owner_and_preserves_unrelated() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let install_spec = installation(dir.path());
        install(&settings, &install_spec).unwrap();
        let other = HookInstallation {
            installation_id: Uuid::now_v7(),
            ..install_spec.clone()
        };
        assert!(uninstall(&settings, &other).unwrap().is_none());
        assert!(uninstall(&settings, &install_spec).unwrap().is_some());
        let value: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
        assert!(value["hooks"]["Stop"].as_array().unwrap().is_empty());
    }

    #[test]
    fn install_replaces_only_recognized_legacy_commands() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join(".claude/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        let legacy = dir.path().join(".cc-monitor/cc_hook.py");
        fs::write(
            &settings,
            format!(
                r#"{{"hooks":{{"Stop":[
                  {{"hooks":[{{"type":"command","command":"python3 '{}' || true"}}]}},
                  {{"hooks":[{{"type":"command","command":"python3 '/tmp/cc_hook.py'"}}]}}
                ]}}}}"#,
                legacy.display()
            ),
        )
        .unwrap();
        install(&settings, &installation(dir.path())).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
        let commands: Vec<_> = value["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|group| group["hooks"][0]["command"].as_str().unwrap())
            .collect();
        assert_eq!(commands.len(), 2);
        assert!(commands.contains(&"python3 '/tmp/cc_hook.py'"));
        assert!(commands
            .iter()
            .any(|command| command.contains("--installation-id")));
    }

    #[test]
    fn legacy_recognition_rejects_backup_compound_and_other_paths() {
        let dir = tempdir().unwrap();
        let script = dir.path().join(".cc-monitor/cc_hook.py");
        let exact = format!("python3 '{}' || true", script.display());
        assert!(is_legacy(&exact, &script));
        assert!(is_legacy(
            &format!("/usr/bin/python3 '{}'", script.display()),
            &script
        ));
        for command in [
            format!("python3 '{}.backup' || true", script.display()),
            format!("echo prefix && python3 '{}'", script.display()),
            format!("python3 '{}' --extra", script.display()),
            "python3 '/tmp/cc_hook.py' || true".to_owned(),
            format!("python3 '{}' || false", script.display()),
            format!("python '{}'", script.display()),
        ] {
            assert!(!is_legacy(&command, &script), "{command}");
        }
    }

    #[test]
    fn removal_preserves_unrelated_command_in_same_group() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let install_spec = installation(dir.path());
        let owned = owned_command(&install_spec);
        fs::write(
            &settings,
            format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[
                  {{"type":"command","command":{owned:?}}},
                  {{"type":"command","command":"other"}}
                ]}}]}}}}"#
            ),
        )
        .unwrap();
        uninstall(&settings, &install_spec).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"].as_array().unwrap().len(),
            1
        );
        assert_eq!(value["hooks"]["Stop"][0]["hooks"][0]["command"], "other");
    }

    #[test]
    fn malformed_settings_are_never_overwritten() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(&settings, "{bad").unwrap();
        let original = fs::read(&settings).unwrap();
        assert!(matches!(
            install(&settings, &installation(dir.path())),
            Err(InstallError::Malformed(_))
        ));
        assert_eq!(fs::read(settings).unwrap(), original);
    }

    #[test]
    fn stages_executable_atomically() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, b"binary").unwrap();
        let target = stage_hook(&source, &dir.path().join("app")).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(target).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn settings_permissions_are_explicit_and_preserved() {
        use std::os::unix::fs::PermissionsExt;

        for mode in [0o600, 0o640] {
            let dir = tempdir().unwrap();
            let settings = dir.path().join(".claude/settings.json");
            fs::create_dir_all(settings.parent().unwrap()).unwrap();
            fs::write(&settings, "{}").unwrap();
            fs::set_permissions(&settings, fs::Permissions::from_mode(mode)).unwrap();

            install(&settings, &installation(dir.path())).unwrap();
            let backup = settings.with_extension("json.cc-monitor-backup");
            assert_eq!(unix_mode(&settings).unwrap(), mode);
            assert_eq!(unix_mode(&backup).unwrap(), mode);
            assert_eq!(unix_mode(&backup).unwrap() & !mode, 0);
        }

        let dir = tempdir().unwrap();
        let settings = dir.path().join(".claude/settings.json");
        install(&settings, &installation(dir.path())).unwrap();
        assert_eq!(unix_mode(&settings).unwrap(), 0o600);
    }
}
