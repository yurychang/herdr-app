//! Native services for the personal composer mod; no screen scraping or CLI draft manipulation.
use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;
#[cfg(test)]
mod editor_tests;
mod files;
mod store;

static CORE: Mutex<Option<Arc<crate::hc::Core>>> = Mutex::new(None);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static SEARCH: AtomicU64 = AtomicU64::new(0);

pub fn managed_shell() -> String {
    if cfg!(windows) {
        // Pin the native 64-bit shell before Core refreshes the child PATH.
        if let Some(path) = ["ProgramW6432", "ProgramFiles"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(|root| std::path::PathBuf::from(root).join("PowerShell/7/pwsh.exe"))
            .chain(std::env::var_os("SystemRoot").map(|root| {
                std::path::PathBuf::from(root)
                    .join("System32/WindowsPowerShell/v1.0/powershell.exe")
            }))
            .find(|path| path.is_file())
        {
            return path.to_string_lossy().into_owned();
        }
    }
    crate::window::shell()
}

pub fn launch_line(command: &[String]) -> String {
    let line = crate::programs::powershell_line(command);
    if std::env::var("MYHERDR_DEV_AGENT_ENV").as_deref() != Ok("1") {
        return line;
    }
    // Isolate Herdr data, but preserve the user's CLI/runtime configuration. Route hc explicitly.
    format!(
        r#"$env:HC_SOCKET=$env:HC_TERMINAL_SOCKET; foreach ($n in @('XDG_CONFIG_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','XDG_DATA_HOME')) {{ $v=[Environment]::GetEnvironmentVariable('MYHERDR_AGENT_'+$n); [Environment]::SetEnvironmentVariable($n,$v) }}; {line}"#
    )
}

pub fn set_core(core: Arc<crate::hc::Core>) {
    *CORE.lock().unwrap() = Some(core);
    EPOCH.fetch_add(1, Ordering::SeqCst);
}
fn core() -> Result<Arc<crate::hc::Core>> {
    CORE.lock()
        .unwrap()
        .clone()
        .ok_or_else(|| anyhow!("Core is not connected"))
}
pub fn info() -> Value {
    let available = core().is_ok_and(|c| {
        c.hello["features"]
            .as_array()
            .is_some_and(|f| f.iter().any(|f| f == "durable_submit_v1"))
    });
    json!({"available":available,"epoch":EPOCH.load(Ordering::SeqCst)})
}
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| anyhow!("missing {k}"))
}

fn managed(core: &crate::hc::Core, terminal: &str) -> Result<Value> {
    if terminal.contains('/') {
        bail!("Only managed local agents are supported; use the native terminal")
    }
    let record = core.call("terminal.get", json!({"terminal":terminal}))?;
    let agent = record["labels"]["myherdr.agent"].as_str().unwrap_or("");
    if !matches!(agent, "codex" | "claude")
        || record["labels"]["input.session"].as_str().is_none()
        || record["labels"]["myherdr.argv"].as_str()
            != Some(serde_json::to_string(&record["argv"])?.as_str())
    {
        bail!(
            "This terminal is not a managed agent invocation; launch Codex or Claude from the composer"
        )
    }
    Ok(record)
}

pub fn request(
    command: String,
    args: Value,
) -> impl std::future::Future<Output = Result<gpui_shell::HostValue, gpui_shell::HostError>> + Send
{
    let generation = if command == "search" || command == "search.cancel" {
        SEARCH.fetch_add(1, Ordering::SeqCst) + 1
    } else {
        0
    };
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(handle(&command, args, generation));
    });
    async move {
        let result = rx
            .recv()
            .await
            .map_err(|_| gpui_shell::HostError::new("composer task lost"))?
            .map_err(|e| gpui_shell::HostError::new(format!("{e:#}")))?;
        Ok(crate::mods::to_host(&result))
    }
}

fn handle(command: &str, args: Value, generation: u64) -> Result<Value> {
    match command {
        "load" if core().is_err() => Ok(json!({"offline":true,"drafts":[],"pending":[]})),
        "load" => store::load(),
        "draft.save" => store::save_draft(args),
        "history" => store::history(&args),
        "recovery" => store::recovery(),
        "search" => files::search(&args, || SEARCH.load(Ordering::SeqCst) != generation),
        "search.cancel" => Ok(Value::Null),
        "file" => files::metadata(std::path::Path::new(string(&args, "path")?), false),
        "clipboard.save" => files::save_clipboard(&args),
        "newId" => Ok(json!(uuid::Uuid::new_v4().to_string())),
        "target" => {
            let core = core()?;
            let terminal = string(&args, "terminal")?;
            let record = managed(&core, terminal)?;
            let target = core.call("terminal.input_target", json!({"terminal":terminal}))?;
            let cwd = record["foreground"]["cwd"]
                .as_str()
                .or(record["cwd"].as_str())
                .unwrap_or("");
            let project = files::project(cwd);
            Ok(
                json!({"target":target,"agent":record["labels"]["myherdr.agent"],"cwd":cwd,"project":project,"epoch":EPOCH.load(Ordering::SeqCst)}),
            )
        }
        "submit" => {
            let core = core()?;
            if args["epoch"].as_u64() != Some(EPOCH.load(Ordering::SeqCst)) {
                bail!("Connection changed; confirm native input again")
            }
            let request = &args["request"];
            let record = managed(&core, string(&request["target"], "terminal")?)?;
            if record["labels"]["input.session"] != request["target"]["session"] {
                bail!("Managed session changed")
            }
            // Commit a recoverable local intent before any network/PTY operation.
            store::begin_submission(&args)?;
            match core.call_timeout("terminal.submit", request.clone(), Duration::from_secs(25)) {
                Ok(receipt) => {
                    store::finish_submission(&args, &receipt)?;
                    Ok(receipt)
                }
                Err(original) => {
                    // A timeout is not a rejection; query the same id, never resubmit.
                    match core.call("terminal.submission", json!({"id":request["id"]})) {
                        Ok(receipt) => {
                            store::finish_submission(&args, &receipt)?;
                            Ok(receipt)
                        }
                        Err(_) => {
                            let receipt = json!({"request":request,"state":"unknown","stage":"connection","reason":format!("{original:#}"),"resolved":false});
                            store::finish_submission(&args, &receipt)?;
                            Ok(receipt)
                        }
                    }
                }
            }
        }
        "receipt" => {
            let receipt = core()?.call("terminal.submission", json!({"id":string(&args,"id")?}))?;
            store::update_receipt(&receipt)?;
            Ok(receipt)
        }
        "resolve" => {
            if args["reviewed"] != true {
                bail!("Review the native terminal before resolving")
            }
            let core = core()?;
            let id = string(&args, "id")?;
            // not_found remains unknown locally until the person explicitly resolves it.
            let receipt = match core.call(
                "terminal.submission.resolve",
                json!({"id":id,"reviewed":true}),
            ) {
                Ok(r) => r,
                Err(e) if crate::hc::error_kind(&e) == Some("not_found") => {
                    json!({"request":{"id":id},"state":"unknown","resolved":true})
                }
                Err(e) => return Err(e),
            };
            store::update_receipt(&receipt)?;
            Ok(receipt)
        }
        "session.new" => {
            if args["reviewed"] != true {
                bail!("Review the native conversation before rebinding")
            }
            let core = core()?;
            let terminal = string(&args, "terminal")?;
            managed(&core, terminal)?;
            core.call(
                "terminal.label",
                json!({"terminal":terminal,"labels":{"input.session":uuid::Uuid::new_v4().to_string()}}),
            )?;
            Ok(Value::Null)
        }
        "stop" => {
            let core = core()?;
            let terminal = string(&args["target"], "terminal")?;
            managed(&core, terminal)?;
            let live = core.call("terminal.input_target", json!({"terminal":terminal}))?;
            if live != args["target"] {
                bail!("Target changed; inspect terminal before stopping")
            }
            core.call_timeout(
                "terminal.input_stop",
                json!({"target":live}),
                Duration::from_secs(10),
            )
        }
        _ => bail!("Unknown composer operation {command}"),
    }
}

pub fn clipboard(
    bytes: Vec<u8>,
) -> impl std::future::Future<Output = Result<gpui_shell::HostValue, gpui_shell::HostError>> + Send
{
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(files::clipboard(bytes));
    });
    async move {
        Ok(crate::mods::to_host(
            &rx.recv()
                .await
                .map_err(|_| gpui_shell::HostError::new("clipboard task lost"))?
                .map_err(|e| gpui_shell::HostError::new(e.to_string()))?,
        ))
    }
}
