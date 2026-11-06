//! The personal integration owns the agent SDK helper. Core remains unaware of it.
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::process::Stdio;
use std::time::Duration;

pub fn discover(record: &Value, cwd: &str) -> Result<Value> {
    let executable = std::env::var_os("MYHERDR_COMPLETIONS")
        .ok_or_else(|| anyhow!("Skills helper is not configured"))?;
    let executable = std::path::PathBuf::from(executable);
    if !executable.is_absolute() || !executable.is_file() {
        bail!("Skills helper must be an existing absolute path")
    }
    let argv: Value = serde_json::from_str(
        record["labels"]["myherdr.command"]
            .as_str()
            .ok_or_else(|| {
                anyhow!("Restart this agent through the App to enable skills discovery")
            })?,
    )?;
    let mut command = crate::spawn::command(executable);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Match the managed CLI's configuration, not the isolated Herdr config home.
    if std::env::var("MYHERDR_DEV_AGENT_ENV").as_deref() == Ok("1") {
        for key in [
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
        ] {
            match std::env::var_os(format!("MYHERDR_AGENT_{key}")) {
                Some(v) if !v.is_empty() => {
                    command.env(key, v);
                }
                _ => {
                    command.env_remove(key);
                }
            }
        }
    }
    let mut child = command.spawn()?;
    let result = (|| {
        let input = json!({"agent":record["labels"]["myherdr.agent"],"argv":argv,"cwd":cwd});
        child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Missing helper input"))?
            .write_all(&serde_json::to_vec(&input)?)?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Missing helper output"))?
            .take(4 * 1024 * 1024 + 1);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let result = output.read_to_end(&mut data).map(|_| data);
            let _ = tx.send(result);
        });
        let data = rx
            .recv_timeout(Duration::from_secs(16))
            .map_err(|_| anyhow!("Skills discovery timed out"))??;
        if data.len() > 4 * 1024 * 1024 {
            bail!("Skills metadata exceeds 4 MiB")
        }
        let value: Value = serde_json::from_slice(&data)?;
        if let Some(error) = value["error"].as_str() {
            bail!("{error}")
        }
        if !value["items"].is_array() {
            bail!("Unsupported skills metadata")
        }
        Ok(value)
    })();
    // This is our metadata-only helper, never an agent running in a user's pane.
    let _ = child.kill();
    let _ = child.wait();
    result
}
