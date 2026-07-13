use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use atomic_write_file::AtomicWriteFile;

const MARKER_BEGIN: &str = "# >>> spectra autosync hook >>>";
const MARKER_END: &str = "# <<< spectra autosync hook <<<";
const HOOKS: [&str; 3] = ["post-commit", "post-merge", "post-checkout"];

type BoxError = Box<dyn std::error::Error>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HookStatus {
    Missing,
    Partial,
    Current,
}

impl HookStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Partial => "partial",
            Self::Current => "current",
        }
    }
}

pub(crate) struct HookReport {
    pub(crate) hooks_dir: PathBuf,
    pub(crate) hooks: Vec<&'static str>,
}

pub(crate) fn install(project: &Path) -> Result<HookReport, BoxError> {
    let hooks_dir = hooks_dir(project)?;
    fs::create_dir_all(&hooks_dir)?;
    let block = marker_block();
    for hook in HOOKS {
        let path = hooks_dir.join(hook);
        let original = read_optional(&path)?;
        let base = strip_marker_block(&original)
            .trim_end_matches(char::is_whitespace)
            .to_owned();
        let contents = if base.is_empty() {
            format!("#!/bin/sh\n{block}\n")
        } else {
            format!("{base}\n\n{block}\n")
        };
        write_executable(&path, contents.as_bytes())?;
    }
    Ok(HookReport {
        hooks_dir,
        hooks: HOOKS.to_vec(),
    })
}

pub(crate) fn remove(project: &Path) -> Result<HookReport, BoxError> {
    let hooks_dir = hooks_dir(project)?;
    let mut removed = Vec::new();
    for hook in HOOKS {
        let path = hooks_dir.join(hook);
        if !path.exists() {
            continue;
        }
        let original = fs::read_to_string(&path)?;
        if !original.contains(MARKER_BEGIN) {
            continue;
        }
        let stripped = strip_marker_block(&original);
        if is_effectively_empty(&stripped) && !path.is_symlink() {
            fs::remove_file(&path)?;
        } else {
            let contents = format!("{}\n", stripped.trim_end_matches(char::is_whitespace));
            write_executable(&path, contents.as_bytes())?;
        }
        removed.push(hook);
    }
    Ok(HookReport {
        hooks_dir,
        hooks: removed,
    })
}

pub(crate) fn status(project: &Path) -> Result<(HookStatus, PathBuf), BoxError> {
    let hooks_dir = hooks_dir(project)?;
    let installed = HOOKS
        .iter()
        .filter(|hook| {
            fs::read_to_string(hooks_dir.join(hook))
                .is_ok_and(|contents| contents.contains(MARKER_BEGIN))
        })
        .count();
    let status = match installed {
        0 => HookStatus::Missing,
        count if count == HOOKS.len() => HookStatus::Current,
        _ => HookStatus::Partial,
    };
    Ok((status, hooks_dir))
}

fn hooks_dir(project: &Path) -> Result<PathBuf, BoxError> {
    let project = project.canonicalize()?;
    if !project.is_dir() {
        return Err(format!("{} is not a directory", project.display()).into());
    }
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(&project)
        .output()
        .map_err(|error| format!("unable to run git: {error}"))?;
    if !output.status.success() {
        return Err(format!("{} is not a Git working tree", project.display()).into());
    }
    let value = String::from_utf8(output.stdout)?;
    let value = value.trim();
    if value.is_empty() {
        return Err("git returned an empty hooks path".into());
    }
    let path = PathBuf::from(value);
    Ok(if path.is_absolute() {
        path
    } else {
        project.join(path)
    })
}

fn marker_block() -> String {
    [
        MARKER_BEGIN,
        "# Keeps the Spectra topology index fresh when live watching is unavailable.",
        "# Managed by Spectra; remove with `spectra autosync remove`.",
        "if command -v spectra >/dev/null 2>&1; then",
        "  spectra_root=\"$(git rev-parse --show-toplevel 2>/dev/null)\" || exit 0",
        "  ( spectra sync --quiet \"$spectra_root\" >/dev/null 2>&1 & ) >/dev/null 2>&1",
        "fi",
        MARKER_END,
    ]
    .join("\n")
}

fn strip_marker_block(contents: &str) -> String {
    let mut kept = Vec::new();
    let mut inside = false;
    for line in contents.lines() {
        match line.trim() {
            MARKER_BEGIN => inside = true,
            MARKER_END => inside = false,
            _ if !inside => kept.push(line),
            _ => {}
        }
    }
    kept.join("\n")
}

fn is_effectively_empty(contents: &str) -> bool {
    contents
        .lines()
        .all(|line| line.trim().is_empty() || line.trim_start().starts_with("#!"))
}

fn read_optional(path: &Path) -> Result<String, BoxError> {
    if path.exists() {
        Ok(fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

fn write_executable(path: &Path, contents: &[u8]) -> Result<(), BoxError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let destination = if path.is_symlink() {
        path.canonicalize()?
    } else {
        path.to_path_buf()
    };
    let mut file = AtomicWriteFile::open(&destination)?;
    file.write_all(contents)?;
    file.commit()?;
    set_executable(&destination)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), BoxError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), BoxError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn install_is_idempotent_and_remove_preserves_user_hook() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        let initialized = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(initialized.success());
        let hooks = root.join(".git/hooks");
        let checkout = hooks.join("post-checkout");
        fs::write(&checkout, "#!/bin/sh\necho user-hook\n").unwrap();

        let first = install(&root).unwrap();
        assert_eq!(first.hooks, HOOKS);
        install(&root).unwrap();
        let contents = fs::read_to_string(&checkout).unwrap();
        assert!(contents.contains("echo user-hook"));
        assert_eq!(contents.matches(MARKER_BEGIN).count(), 1);
        assert_eq!(status(&root).unwrap().0, HookStatus::Current);

        let removed = remove(&root).unwrap();
        assert_eq!(removed.hooks, HOOKS);
        assert_eq!(
            fs::read_to_string(&checkout).unwrap(),
            "#!/bin/sh\necho user-hook\n"
        );
        assert!(!hooks.join("post-commit").exists());
        assert!(!hooks.join("post-merge").exists());
        assert_eq!(status(&root).unwrap().0, HookStatus::Missing);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marker_removal_does_not_touch_surrounding_lines() {
        let original = format!("#!/bin/sh\nbefore\n{}\nafter\n", marker_block());
        assert_eq!(strip_marker_block(&original), "#!/bin/sh\nbefore\nafter");
    }

    #[test]
    fn install_honors_a_repository_hooks_path() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args(["config", "core.hooksPath", ".custom-hooks"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );

        let report = install(&root).unwrap();
        assert_eq!(
            report.hooks_dir,
            root.canonicalize().unwrap().join(".custom-hooks")
        );
        assert!(report.hooks_dir.join("post-checkout").exists());
        remove(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn temp_root() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "spectra-git-hooks-{}-{id}-{sequence}",
            std::process::id()
        ))
    }
}
