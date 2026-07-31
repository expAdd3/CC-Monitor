use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_LOG_BYTES: u64 = 512 * 1024;
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn initialize(directory: &Path) -> std::io::Result<()> {
    fs::create_dir_all(directory)?;
    let path = directory.join("cc-monitor.log");
    let _ = LOG_PATH.set(path);
    event("app_started");
    Ok(())
}

pub fn event(code: &'static str) {
    let Some(path) = LOG_PATH.get() else {
        return;
    };
    let _ = append_event(path, code);
}

fn append_event(path: &Path, code: &str) -> std::io::Result<()> {
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES) {
        let rotated = path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        fs::rename(path, rotated)?;
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let safe_code: String = code
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .take(64)
        .collect();
    writeln!(
        OpenOptions::new().create(true).append(true).open(path)?,
        "{timestamp} {safe_code}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_events_strip_untrusted_text_and_rotate() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cc-monitor.log");
        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();

        append_event(&path, "reindex-failed\nnext").unwrap();

        assert!(path.with_extension("log.1").exists());
        let value = fs::read_to_string(path).unwrap();
        let (_, code) = value.trim_end().split_once(' ').unwrap();
        assert_eq!(code, "reindexfailednext");
    }
}
