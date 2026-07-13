use std::{
    env, fs,
    path::{Path, PathBuf},
};

use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstRootNode},
};

use super::{
    BoxError,
    json::{Ownership, opencode_entry, opencode_ownership},
    spectra_executable, write_atomic,
};

const LABEL: &str = "OpenCode";

pub(super) fn path() -> Result<PathBuf, BoxError> {
    if let Some(path) = env::var_os("SPECTRA_OPENCODE_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("SPECTRA_HOME")
                .or_else(|| env::var_os("HOME"))
                .or_else(|| env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .ok_or("unable to locate the user configuration directory")?;
    Ok(base.join("opencode/opencode.json"))
}

pub(super) fn install(dry_run: bool) -> Result<String, BoxError> {
    let path = path()?;
    let executable = spectra_executable()?;
    let (root, root_object) = read(&path)?;
    let mcp = match root_object.object_value("mcp") {
        Some(object) => object,
        None if root_object.get("mcp").is_some() => {
            return Err(format!("{} field 'mcp' must be an object", path.display()).into());
        }
        None => root_object
            .object_value_or_create("mcp")
            .expect("new object"),
    };
    let ownership = mcp
        .get("spectra")
        .map(|property| {
            property
                .to_serde_value()
                .ok_or_else(|| "OpenCode 'spectra' MCP entry has no value".into())
                .and_then(|entry| opencode_ownership(&entry, &executable))
        })
        .transpose()?;
    if ownership == Some(Ownership::Foreign) {
        return Err(format!("OpenCode already has a non-Spectra-owned MCP entry named 'spectra' in {}; refusing to overwrite it", path.display()).into());
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
    let value = input_value(opencode_entry(&executable)?)?;
    match mcp.get("spectra") {
        Some(property) => property.set_value(value),
        None => {
            mcp.append("spectra", value);
        }
    }
    write_atomic(&path, root.to_string().as_bytes())?;
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
    let (root, root_object) = read(&path)?;
    let Some(mcp) = root_object.object_value("mcp") else {
        return Ok(format!("{LABEL}: Spectra is not configured."));
    };
    let Some(property) = mcp.get("spectra") else {
        return Ok(format!("{LABEL}: Spectra is not configured."));
    };
    let entry = property
        .to_serde_value()
        .ok_or("OpenCode 'spectra' MCP entry has no value")?;
    if opencode_ownership(&entry, &executable)? == Ownership::Foreign {
        return Err(
            "OpenCode's 'spectra' MCP entry is not owned by Spectra; refusing to remove it".into(),
        );
    }
    if dry_run {
        return Ok(format!(
            "{LABEL}: Would remove Spectra's topology MCP configuration."
        ));
    }
    property.remove();
    write_atomic(&path, root.to_string().as_bytes())?;
    Ok(format!(
        "{LABEL}: Removed Spectra's topology MCP configuration."
    ))
}

pub(super) fn status() -> Result<String, BoxError> {
    let path = path()?;
    let executable = spectra_executable()?;
    let (_, root_object) = read(&path)?;
    let mcp_object = match root_object.object_value("mcp") {
        Some(object) => Some(object),
        None if root_object.get("mcp").is_some() => {
            return Err(format!("{} field 'mcp' must be an object", path.display()).into());
        }
        None => None,
    };
    let mcp = match mcp_object.and_then(|object| object.get("spectra")) {
        None => "missing",
        Some(property) => {
            let entry = property
                .to_serde_value()
                .ok_or("OpenCode 'spectra' MCP entry has no value")?;
            match opencode_ownership(&entry, &executable)? {
                Ownership::Current => "current",
                Ownership::Stale => "stale",
                Ownership::Foreign => "foreign conflict",
            }
        }
    };
    Ok(format!("{LABEL}: MCP={mcp}, Ledger=not available"))
}

fn read(path: &Path) -> Result<(CstRootNode, jsonc_parser::cst::CstObject), BoxError> {
    let text = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
    let root = CstRootNode::parse(&text, &ParseOptions::default())
        .map_err(|error| format!("{} is not valid JSONC: {error}", path.display()))?;
    let object = root
        .object_value_or_create()
        .ok_or_else(|| format!("{} must contain a JSON object", path.display()))?;
    Ok((root, object))
}

fn input_value(value: serde_json::Value) -> Result<CstInputValue, BoxError> {
    Ok(match value {
        serde_json::Value::Null => CstInputValue::Null,
        serde_json::Value::Bool(value) => CstInputValue::Bool(value),
        serde_json::Value::Number(value) => CstInputValue::Number(value.to_string()),
        serde_json::Value::String(value) => CstInputValue::String(value),
        serde_json::Value::Array(values) => CstInputValue::Array(
            values
                .into_iter()
                .map(input_value)
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(values) => CstInputValue::Object(
            values
                .into_iter()
                .map(|(key, value)| Ok((key, input_value(value)?)))
                .collect::<Result<_, BoxError>>()?,
        ),
    })
}
