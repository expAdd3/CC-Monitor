use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
    #[error("hook_managed_path_unsafe")]
    UnsafeManagedPath,
    #[error("settings_path_unsafe")]
    UnsafeSettingsPath,
    #[error("settings_write_conflict")]
    SettingsWriteConflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SettingsSnapshot {
    Missing,
    Present {
        identity: String,
        content_sha256: [u8; 32],
    },
}

struct SettingsDocument {
    value: Value,
    snapshot: SettingsSnapshot,
}

pub fn stage_hook(source: &Path, app_data_dir: &Path) -> Result<PathBuf, InstallError> {
    let target = ensure_managed_hook_path(app_data_dir)?;
    atomic_copy(source, &target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    }
    Ok(target)
}

/// Returns the one application-owned Hook target after rejecting symlinked or
/// non-directory parents and symlinked/non-file targets.
///
/// This is the shared lexical/canonical boundary for lifecycle operations. It
/// intentionally does not create the `bin` directory.
pub fn managed_hook_path(app_data_dir: &Path) -> Result<PathBuf, InstallError> {
    let app_metadata = fs::symlink_metadata(app_data_dir)?;
    if app_metadata.file_type().is_symlink() || !app_metadata.is_dir() {
        return Err(InstallError::UnsafeManagedPath);
    }
    let canonical_app = fs::canonicalize(app_data_dir)?;
    let bin = app_data_dir.join("bin");
    match fs::symlink_metadata(&bin) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(InstallError::UnsafeManagedPath);
            }
            let canonical_bin = fs::canonicalize(&bin)?;
            if canonical_bin.parent() != Some(canonical_app.as_path()) {
                return Err(InstallError::UnsafeManagedPath);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let target = bin.join("cc-monitor-hook");
    match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(InstallError::UnsafeManagedPath);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(target)
}

fn ensure_managed_hook_path(app_data_dir: &Path) -> Result<PathBuf, InstallError> {
    let target = managed_hook_path(app_data_dir)?;
    let bin = target.parent().ok_or(InstallError::UnsafeManagedPath)?;
    match fs::create_dir(bin) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    // Revalidate after creation so a pre-existing link/non-directory, or a
    // replacement observed between the two operations, is rejected.
    managed_hook_path(app_data_dir)
}

pub fn install(
    settings_path: &Path,
    installation: &HookInstallation,
) -> Result<PathBuf, InstallError> {
    let SettingsDocument {
        value: mut settings,
        snapshot,
    } = read_settings(settings_path)?;
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
    write_settings(settings_path, &settings, &snapshot)
}

pub fn uninstall(
    settings_path: &Path,
    installation: &HookInstallation,
) -> Result<Option<PathBuf>, InstallError> {
    if !settings_target_exists(settings_path, true)? {
        return Ok(None);
    }
    let SettingsDocument {
        value: mut settings,
        snapshot,
    } = read_settings(settings_path)?;
    let changed = remove_recognized(&mut settings, Some(installation), None);
    if !changed {
        return Ok(None);
    }
    write_settings(settings_path, &settings, &snapshot).map(Some)
}

/// Verifies the exact settings entries owned by one CC Monitor installation.
///
/// Unrelated hooks are deliberately ignored. The owned command must occur
/// exactly once for every supported event, and nowhere else.
pub fn installation_matches(
    settings_path: &Path,
    installation: &HookInstallation,
) -> Result<bool, InstallError> {
    if !settings_target_exists(settings_path, true)? {
        return Ok(false);
    }
    let settings = read_settings(settings_path)?.value;
    let Some(hooks) = settings.get("hooks").and_then(Value::as_object) else {
        return Ok(false);
    };
    let command = owned_command(installation);
    let mut total_matches = 0;
    for (event, groups) in hooks {
        let supported = HOOK_EVENTS.contains(&event.as_str());
        let Some(groups) = groups.as_array() else {
            if supported {
                return Ok(false);
            }
            continue;
        };
        let mut event_matches = 0;
        for group in groups {
            let matches = group
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|entry| {
                    entry.get("type").and_then(Value::as_str) == Some("command")
                        && entry.get("command").and_then(Value::as_str) == Some(&command)
                })
                .count();
            if matches > 0 && supported {
                if event == "PreToolUse" {
                    if group.get("matcher").and_then(Value::as_str) != Some("AskUserQuestion") {
                        return Ok(false);
                    }
                } else if group
                    .as_object()
                    .is_some_and(|value| value.contains_key("matcher"))
                {
                    return Ok(false);
                }
            }
            event_matches += matches;
        }
        total_matches += event_matches;
        if supported && event_matches != 1 {
            return Ok(false);
        }
        if !supported && event_matches != 0 {
            return Ok(false);
        }
    }
    Ok(total_matches == HOOK_EVENTS.len())
}

fn read_settings(path: &Path) -> Result<SettingsDocument, InstallError> {
    if !settings_target_exists(path, true)? {
        return Ok(SettingsDocument {
            value: json!({}),
            snapshot: SettingsSnapshot::Missing,
        });
    }
    let mut file = open_regular_settings(path)?;
    let identity = settings_file_identity(&file.metadata()?);
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut file, &mut bytes)?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() {
        return Err(InstallError::Root);
    }
    Ok(SettingsDocument {
        value,
        snapshot: SettingsSnapshot::Present {
            identity,
            content_sha256: Sha256::digest(&bytes).into(),
        },
    })
}

fn write_settings(
    path: &Path,
    settings: &Value,
    snapshot: &SettingsSnapshot,
) -> Result<PathBuf, InstallError> {
    prepare_settings_parent(path)?;
    let parent = settings_parent(path)?;
    verify_settings_snapshot(path, snapshot)?;
    let backup = path.with_extension("json.cc-monitor-backup");
    #[cfg(unix)]
    let original_mode = if settings_target_exists(path, false)? {
        use std::os::unix::fs::PermissionsExt;
        Some(
            open_regular_settings(path)?
                .metadata()?
                .permissions()
                .mode()
                & 0o7777,
        )
    } else {
        None
    };
    if settings_target_exists(path, false)? && !settings_target_exists(&backup, false)? {
        let mut source = open_regular_settings(path)?;
        let mut destination = create_private_file(&backup)?;
        io::copy(&mut source, &mut destination)?;
        destination.sync_all()?;
        #[cfg(unix)]
        set_unix_mode(&backup, original_mode.expect("existing file has a mode"))?;
    }
    let temporary = parent.join(format!(".settings.json.{}.tmp", Uuid::now_v7()));
    let result = (|| {
        let mut file = create_private_file(&temporary)?;
        #[cfg(unix)]
        set_unix_mode(&temporary, original_mode.unwrap_or(0o600))?;
        serde_json::to_writer_pretty(&mut file, settings)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        // A concurrent replacement of either the immediate parent or target
        // is rejected before the atomic rename. The rename itself replaces a
        // final-component symlink rather than following it.
        validate_settings_parent(parent, false)?;
        settings_target_exists(path, false)?;
        verify_settings_snapshot(path, snapshot)?;
        fs::rename(&temporary, path)?;
        Ok::<_, InstallError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(backup)
}

fn verify_settings_snapshot(path: &Path, expected: &SettingsSnapshot) -> Result<(), InstallError> {
    let current = if settings_target_exists(path, false)? {
        let mut file = open_regular_settings(path)?;
        let identity = settings_file_identity(&file.metadata()?);
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut file, &mut bytes)?;
        SettingsSnapshot::Present {
            identity,
            content_sha256: Sha256::digest(&bytes).into(),
        }
    } else {
        SettingsSnapshot::Missing
    };
    if &current != expected {
        return Err(InstallError::SettingsWriteConflict);
    }
    Ok(())
}

#[cfg(unix)]
fn settings_file_identity(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}", metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn settings_file_identity(metadata: &fs::Metadata) -> String {
    portable_metadata_identity(metadata)
}

fn settings_parent(path: &Path) -> Result<&Path, InstallError> {
    path.parent().ok_or(InstallError::UnsafeSettingsPath)
}

/// Validates the exact Claude settings target without following the final
/// component. The immediate parent is the trust boundary: it must be a real
/// directory, and the target (when present) must be a regular non-symlink
/// file. Missing targets remain valid for first-time installation.
fn settings_target_exists(path: &Path, allow_missing_parent: bool) -> Result<bool, InstallError> {
    validate_settings_parent(settings_parent(path)?, allow_missing_parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(InstallError::UnsafeSettingsPath);
            }
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn validate_settings_parent(parent: &Path, allow_missing: bool) -> Result<(), InstallError> {
    match fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(InstallError::UnsafeSettingsPath);
            }
            Ok(())
        }
        Err(error) if allow_missing && error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(InstallError::UnsafeSettingsPath)
        }
        Err(error) => Err(error.into()),
    }
}

fn prepare_settings_parent(path: &Path) -> Result<(), InstallError> {
    let parent = settings_parent(path)?;
    match fs::symlink_metadata(parent) {
        Ok(_) => validate_settings_parent(parent, false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let ancestor = parent.parent().ok_or(InstallError::UnsafeSettingsPath)?;
            validate_settings_parent(ancestor, false)?;
            fs::create_dir(parent)?;
            validate_settings_parent(parent, false)
        }
        Err(error) => Err(error.into()),
    }
}

fn open_regular_settings(path: &Path) -> Result<fs::File, InstallError> {
    settings_target_exists(path, false)?;
    let file = open_read_nofollow(path)?;
    if !file.metadata()?.is_file() {
        return Err(InstallError::UnsafeSettingsPath);
    }
    Ok(file)
}

fn create_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(not(unix))]
fn portable_metadata_identity(metadata: &fs::Metadata) -> String {
    use std::time::UNIX_EPOCH;

    fn time_key(value: io::Result<std::time::SystemTime>) -> String {
        value.map_or_else(
            |_| "unknown".to_owned(),
            |time| match time.duration_since(UNIX_EPOCH) {
                Ok(duration) => format!("+{}", duration.as_nanos()),
                Err(error) => format!("-{}", error.duration().as_nanos()),
            },
        )
    }

    format!(
        "len={};modified={};created={};readonly={}",
        metadata.len(),
        time_key(metadata.modified()),
        time_key(metadata.created()),
        metadata.permissions().readonly()
    )
}

#[cfg(unix)]
fn open_read_nofollow(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_read_nofollow(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().read(true).open(path)
}

#[cfg(all(unix, test))]
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
    let mut changed = false;
    let remove_hooks_object = {
        let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
            return false;
        };
        hooks.retain(|_, groups| {
            let Some(groups) = groups.as_array_mut() else {
                return true;
            };
            let mut event_changed = false;
            groups.retain_mut(|group| {
                let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                    return true;
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
                let group_changed = entries.len() != before;
                event_changed |= group_changed;
                !group_changed || !entries.is_empty()
            });
            changed |= event_changed;
            !event_changed || !groups.is_empty()
        });
        changed && hooks.is_empty()
    };
    if remove_hooks_object {
        settings
            .as_object_mut()
            .expect("settings root is an object")
            .remove("hooks");
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
        assert!(installation_matches(&settings, &install).unwrap());
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
        assert!(value.get("hooks").is_none());
    }

    #[test]
    fn uninstall_prunes_only_empty_structure_created_by_owned_hooks() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let install_spec = installation(dir.path());
        fs::write(
            &settings,
            r#"{"hooks":{"CustomEvent":[],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"other"}]}]}}"#,
        )
        .unwrap();

        install(&settings, &install_spec).unwrap();
        assert!(uninstall(&settings, &install_spec).unwrap().is_some());

        let value: Value = serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
        assert_eq!(value["hooks"]["CustomEvent"], json!([]));
        assert_eq!(
            value["hooks"]["UserPromptSubmit"],
            json!([{"hooks":[{"type":"command","command":"other"}]}])
        );
        assert_eq!(value["hooks"].as_object().unwrap().len(), 2);
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
    fn stale_settings_snapshot_never_overwrites_a_concurrent_update() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        fs::write(&settings, r#"{"theme":"dark"}"#).unwrap();
        let document = read_settings(&settings).unwrap();
        fs::write(&settings, r#"{"theme":"light","newUserSetting":true}"#).unwrap();

        let error = write_settings(&settings, &document.value, &document.snapshot).unwrap_err();

        assert!(matches!(error, InstallError::SettingsWriteConflict));
        assert_eq!(
            fs::read_to_string(settings).unwrap(),
            r#"{"theme":"light","newUserSetting":true}"#
        );
    }

    #[test]
    fn missing_settings_snapshot_never_replaces_a_concurrently_created_file() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join(".claude/settings.json");
        let document = read_settings(&settings).unwrap();
        fs::create_dir(settings.parent().unwrap()).unwrap();
        fs::write(&settings, r#"{"createdBy":"claude"}"#).unwrap();

        let error = write_settings(&settings, &document.value, &document.snapshot).unwrap_err();

        assert!(matches!(error, InstallError::SettingsWriteConflict));
        assert_eq!(
            fs::read_to_string(settings).unwrap(),
            r#"{"createdBy":"claude"}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn settings_operations_reject_symlink_targets_without_touching_the_destination() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = dir.path().join("outside.json");
        let settings = dir.path().join("settings.json");
        fs::write(&outside, r#"{"outside":true}"#).unwrap();
        symlink(&outside, &settings).unwrap();

        for result in [
            install(&settings, &installation(dir.path())).map(|_| ()),
            uninstall(&settings, &installation(dir.path())).map(|_| ()),
            installation_matches(&settings, &installation(dir.path())).map(|_| ()),
        ] {
            assert!(matches!(result, Err(InstallError::UnsafeSettingsPath)));
        }
        assert_eq!(fs::read_to_string(outside).unwrap(), r#"{"outside":true}"#);
    }

    #[cfg(unix)]
    #[test]
    fn settings_operations_reject_a_symlinked_immediate_parent() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let outside = dir.path().join("outside");
        let claude = dir.path().join(".claude");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &claude).unwrap();
        let settings = claude.join("settings.json");

        assert!(matches!(
            install(&settings, &installation(dir.path())),
            Err(InstallError::UnsafeSettingsPath)
        ));
        assert!(!outside.join("settings.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn settings_backup_symlink_is_rejected_without_overwriting_either_file() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let backup = settings.with_extension("json.cc-monitor-backup");
        let outside = dir.path().join("outside.json");
        fs::write(&settings, "{}").unwrap();
        fs::write(&outside, "do not replace").unwrap();
        symlink(&outside, &backup).unwrap();

        assert!(matches!(
            install(&settings, &installation(dir.path())),
            Err(InstallError::UnsafeSettingsPath)
        ));
        assert_eq!(fs::read_to_string(&settings).unwrap(), "{}");
        assert_eq!(fs::read_to_string(outside).unwrap(), "do not replace");
    }

    #[test]
    fn installation_match_requires_every_exact_owned_entry() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let install_spec = installation(dir.path());
        install(&settings, &install_spec).unwrap();
        assert!(installation_matches(&settings, &install_spec).unwrap());

        let mut value: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        value["hooks"]["Stop"].as_array_mut().unwrap().clear();
        fs::write(&settings, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(!installation_matches(&settings, &install_spec).unwrap());
    }

    #[test]
    fn installation_match_rejects_structural_and_cross_event_spoofs() {
        let dir = tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        let install_spec = installation(dir.path());
        install(&settings, &install_spec).unwrap();
        let healthy: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        let command = owned_command(&install_spec);

        let mut non_array = healthy.clone();
        non_array["hooks"]["Stop"] = json!({});
        non_array["hooks"]["Bogus"] = json!([{"hooks":[{"type":"command","command":command}]}]);
        fs::write(&settings, serde_json::to_vec(&non_array).unwrap()).unwrap();
        assert!(!installation_matches(&settings, &install_spec).unwrap());

        let mut wrong_matcher = healthy.clone();
        wrong_matcher["hooks"]["PreToolUse"][0]["matcher"] = json!("OtherTool");
        fs::write(&settings, serde_json::to_vec(&wrong_matcher).unwrap()).unwrap();
        assert!(!installation_matches(&settings, &install_spec).unwrap());

        let mut missing_matcher = healthy.clone();
        missing_matcher["hooks"]["PreToolUse"][0]
            .as_object_mut()
            .unwrap()
            .remove("matcher");
        fs::write(&settings, serde_json::to_vec(&missing_matcher).unwrap()).unwrap();
        assert!(!installation_matches(&settings, &install_spec).unwrap());

        let mut duplicate = healthy.clone();
        let duplicate_entry = duplicate["hooks"]["Stop"][0]["hooks"][0].clone();
        duplicate["hooks"]["Stop"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(duplicate_entry);
        fs::write(&settings, serde_json::to_vec(&duplicate).unwrap()).unwrap();
        assert!(!installation_matches(&settings, &install_spec).unwrap());

        let mut unexpected_matcher = healthy.clone();
        unexpected_matcher["hooks"]["Stop"][0]["matcher"] = json!("AskUserQuestion");
        fs::write(&settings, serde_json::to_vec(&unexpected_matcher).unwrap()).unwrap();
        assert!(!installation_matches(&settings, &install_spec).unwrap());

        let mut unknown_event = healthy;
        unknown_event["hooks"]["Bogus"] = json!([{"hooks":[{"type":"command","command":command}]}]);
        fs::write(&settings, serde_json::to_vec(&unknown_event).unwrap()).unwrap();
        assert!(!installation_matches(&settings, &install_spec).unwrap());
    }

    #[test]
    fn stages_executable_atomically() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("source");
        let app = dir.path().join("app");
        fs::write(&source, b"binary").unwrap();
        fs::create_dir(&app).unwrap();
        let target = stage_hook(&source, &app).unwrap();
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
    fn staging_rejects_a_symlinked_managed_bin_without_writing_outside() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let app_data = dir.path().join("app-data");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&app_data).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, app_data.join("bin")).unwrap();
        let source = dir.path().join("source-hook");
        fs::write(&source, b"hook").unwrap();

        let error = stage_hook(&source, &app_data).unwrap_err();

        assert!(matches!(error, InstallError::UnsafeManagedPath));
        assert!(!outside.join("cc-monitor-hook").exists());
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

    #[cfg(unix)]
    #[test]
    fn newly_created_private_files_never_grant_group_or_other_access() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let private = dir.path().join("private");
        let file = create_private_file(&private).unwrap();

        assert_eq!(
            file.metadata().unwrap().permissions().mode() & 0o077,
            0,
            "private backup/temp creation must be safe before content is written"
        );
    }
}
