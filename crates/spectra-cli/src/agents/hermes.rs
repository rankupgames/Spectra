use std::{
    env, fs,
    path::{Path, PathBuf},
};

use super::{
    BoxError, configured_command_is_spectra, json::Ownership, resolved, spectra_executable,
    write_atomic,
};

const LABEL: &str = "Hermes Agent";

pub(super) fn path() -> Result<PathBuf, BoxError> {
    if let Some(path) = env::var_os("SPECTRA_HERMES_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    if let Some(home) = env::var_os("HERMES_HOME") {
        return Ok(PathBuf::from(home).join("config.yaml"));
    }
    let home = env::var_os("SPECTRA_HOME")
        .or_else(|| env::var_os("HOME"))
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or("unable to locate the user home directory")?;
    Ok(PathBuf::from(home).join(".hermes/config.yaml"))
}

pub(super) fn install(dry_run: bool) -> Result<String, BoxError> {
    let path = path()?;
    let executable = spectra_executable()?;
    let text = read(&path)?;
    let document = Document::parse(text)?;
    let ownership = document
        .spectra
        .as_ref()
        .map(|range| ownership(&document.lines[range.clone()], &executable))
        .transpose()?;
    if ownership == Some(Ownership::Foreign) {
        return Err(format!("Hermes already has a non-Spectra-owned MCP entry named 'spectra' in {}; refusing to overwrite it", path.display()).into());
    }
    if ownership == Some(Ownership::Current) {
        return Ok(format!(
            "{LABEL}: Spectra topology MCP is already configured."
        ));
    }
    let verb = if ownership.is_some() {
        "update"
    } else {
        "configure"
    };
    if dry_run {
        return Ok(format!(
            "{LABEL}: Would {verb} the Spectra topology MCP in {}.",
            path.display()
        ));
    }
    let updated = document.with_entry(&executable)?;
    write_atomic(&path, updated.as_bytes())?;
    Ok(format!(
        "{LABEL}: Spectra topology MCP {}. Restart the agent if it is running.",
        if ownership.is_some() {
            "updated"
        } else {
            "configured"
        }
    ))
}

pub(super) fn uninstall(dry_run: bool) -> Result<String, BoxError> {
    let path = path()?;
    let executable = spectra_executable()?;
    let document = Document::parse(read(&path)?)?;
    let Some(range) = document.spectra.clone() else {
        return Ok(format!("{LABEL}: Spectra is not configured."));
    };
    if ownership(&document.lines[range], &executable)? == Ownership::Foreign {
        return Err(
            "Hermes's 'spectra' MCP entry is not owned by Spectra; refusing to remove it".into(),
        );
    }
    if dry_run {
        return Ok(format!(
            "{LABEL}: Would remove Spectra's topology MCP configuration."
        ));
    }
    write_atomic(&path, document.without_entry().as_bytes())?;
    Ok(format!(
        "{LABEL}: Removed Spectra's topology MCP configuration."
    ))
}

pub(super) fn status() -> Result<String, BoxError> {
    let executable = spectra_executable()?;
    let document = Document::parse(read(&path()?)?)?;
    let mcp = match document.spectra.as_ref() {
        None => "missing",
        Some(range) => match ownership(&document.lines[range.clone()], &executable)? {
            Ownership::Current => "current",
            Ownership::Stale => "stale",
            Ownership::Foreign => "foreign conflict",
        },
    };
    Ok(format!("{LABEL}: MCP={mcp}, Ledger=not available"))
}

fn read(path: &Path) -> Result<String, BoxError> {
    if path.exists() {
        Ok(fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

fn ownership(lines: &[String], current: &Path) -> Result<Ownership, BoxError> {
    let mut command = None;
    let mut args = None;
    for line in lines.iter().skip(1) {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("command:") {
            command = Some(parse_yaml_string(value.trim())?);
        } else if let Some(value) = trimmed.strip_prefix("args:") {
            args = serde_json::from_str::<Vec<String>>(value.trim()).ok();
        }
    }
    let Some(command) = command else {
        return Ok(Ownership::Foreign);
    };
    let args_match = args.as_deref() == Some(&["serve".to_owned(), "--mcp".to_owned()]);
    let configured = PathBuf::from(command);
    if args_match && resolved(&configured) == current {
        Ok(Ownership::Current)
    } else if args_match && configured_command_is_spectra(&configured) {
        Ok(Ownership::Stale)
    } else {
        Ok(Ownership::Foreign)
    }
}

fn parse_yaml_string(value: &str) -> Result<String, BoxError> {
    if value.starts_with('"') {
        Ok(serde_json::from_str(value)?)
    } else if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        Ok(value[1..value.len() - 1].replace("''", "'"))
    } else {
        Ok(value.split(" #").next().unwrap_or(value).trim().to_owned())
    }
}

#[derive(Debug)]
struct Document {
    lines: Vec<String>,
    root: Option<std::ops::Range<usize>>,
    root_inline_empty: bool,
    spectra: Option<std::ops::Range<usize>>,
    newline: &'static str,
}

impl Document {
    fn parse(text: String) -> Result<Self, BoxError> {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let lines: Vec<String> = text.split_inclusive('\n').map(ToOwned::to_owned).collect();
        let mut root_start = None;
        let mut root_inline_empty = false;
        for (index, line) in lines.iter().enumerate() {
            let content = line.trim_end_matches(['\r', '\n']);
            if leading_spaces(content) == 0 && content.trim_start().starts_with("mcp_servers:") {
                let suffix = content.trim_start()["mcp_servers:".len()..].trim();
                if !suffix.is_empty() && suffix != "{}" {
                    return Err("Hermes 'mcp_servers' must be a YAML mapping".into());
                }
                root_start = Some(index);
                root_inline_empty = suffix == "{}";
                break;
            }
        }
        let Some(start) = root_start else {
            return Ok(Self {
                lines,
                root: None,
                root_inline_empty: false,
                spectra: None,
                newline,
            });
        };
        let end = (start + 1..lines.len())
            .find(|index| {
                let value = lines[*index].trim();
                !value.is_empty() && !value.starts_with('#') && leading_spaces(&lines[*index]) == 0
            })
            .unwrap_or(lines.len());
        let mut spectra = None;
        if !root_inline_empty {
            for index in start + 1..end {
                let value = lines[index].trim();
                if value == "spectra:" && leading_spaces(&lines[index]) > 0 {
                    let indent = leading_spaces(&lines[index]);
                    let child_end = (index + 1..end)
                        .find(|next| {
                            let value = lines[*next].trim();
                            !value.is_empty()
                                && !value.starts_with('#')
                                && leading_spaces(&lines[*next]) <= indent
                        })
                        .unwrap_or(end);
                    spectra = Some(index..child_end);
                    break;
                }
            }
        }
        Ok(Self {
            lines,
            root: Some(start..end),
            root_inline_empty,
            spectra,
            newline,
        })
    }

    fn with_entry(mut self, executable: &Path) -> Result<String, BoxError> {
        let entry = entry_lines(executable, self.newline)?;
        if let Some(range) = self.spectra {
            self.lines.splice(range, entry);
        } else if let Some(root) = self.root {
            if self.root_inline_empty {
                self.lines[root.start] = format!("mcp_servers:{}", self.newline);
                self.lines.splice(root.start + 1..root.start + 1, entry);
            } else {
                self.lines.splice(root.end..root.end, entry);
            }
        } else {
            if self.lines.last().is_some_and(|line| !line.ends_with('\n')) {
                self.lines.push(self.newline.to_owned());
            }
            if !self.lines.is_empty() && self.lines.iter().any(|line| !line.trim().is_empty()) {
                self.lines.push(self.newline.to_owned());
            }
            self.lines.push(format!("mcp_servers:{}", self.newline));
            self.lines.extend(entry);
        }
        Ok(self.lines.concat())
    }

    fn without_entry(mut self) -> String {
        let Some(range) = self.spectra else {
            return self.lines.concat();
        };
        let root = self.root.clone().expect("spectra entry requires root");
        self.lines.splice(range.clone(), std::iter::empty());
        let remaining_end = root.end - (range.end - range.start);
        let has_other_entry = self.lines[root.start + 1..remaining_end]
            .iter()
            .any(|line| {
                let value = line.trim();
                !value.is_empty() && !value.starts_with('#')
            });
        if !has_other_entry {
            self.lines[root.start] = format!("mcp_servers: {{}}{}", self.newline);
        }
        self.lines.concat()
    }
}

fn entry_lines(executable: &Path, newline: &str) -> Result<Vec<String>, BoxError> {
    let executable = executable
        .to_str()
        .ok_or("Spectra executable path is not valid UTF-8")?;
    let command = serde_json::to_string(executable)?;
    Ok(vec![
        format!("  spectra:{newline}"),
        format!("    command: {command}{newline}"),
        format!("    args: [\"serve\", \"--mcp\"]{newline}"),
    ])
}

fn leading_spaces(value: &str) -> usize {
    value.bytes().take_while(|byte| *byte == b' ').count()
}
