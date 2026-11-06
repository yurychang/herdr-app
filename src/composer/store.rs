//! Each client has its own locked SQLite journal. No shared.json last-writer-wins drafts.
use anyhow::{Result, anyhow};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Mutex};

struct Store {
    id: String,
    db: Connection,
    _lock: std::fs::File,
}
static STORE: Mutex<Option<Store>> = Mutex::new(None);
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn root() -> PathBuf {
    crate::mods::app_dir().join("composer")
}
fn with<T>(f: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
    let mut slot = STORE.lock().unwrap();
    if slot.is_none() {
        *slot = Some(Store::open()?);
    }
    f(slot.as_mut().unwrap())
}
impl Store {
    fn open() -> Result<Self> {
        Self::open_at(root())
    }
    fn open_at(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(root.join("clients"))?;
        let id_path = root.join("client-id");
        let primary = match std::fs::read_to_string(&id_path) {
            Ok(id) => {
                uuid::Uuid::parse_str(id.trim())?;
                id.trim().to_owned()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = uuid::Uuid::new_v4().to_string();
                crate::persist::write_atomic(&id_path, &id)?;
                id
            }
            Err(e) => return Err(e.into()),
        };
        let mut id = primary;
        let lock = loop {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(root.join("clients").join(format!("{id}.lock")))?;
            if file.try_lock().is_ok() {
                break file;
            }
            id = uuid::Uuid::new_v4().to_string();
        };
        let db = Connection::open(root.join("clients").join(format!("{id}.sqlite")))?;
        db.busy_timeout(std::time::Duration::from_secs(3))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS drafts(key TEXT PRIMARY KEY,body TEXT NOT NULL,revision INTEGER NOT NULL,updated INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS submissions(id TEXT PRIMARY KEY,body TEXT NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS history(id TEXT PRIMARY KEY,project TEXT NOT NULL,agent TEXT NOT NULL,body TEXT NOT NULL,created INTEGER NOT NULL);")?;
        Ok(Self {
            id,
            db,
            _lock: lock,
        })
    }
}
fn rows(db: &Connection, sql: &str) -> Result<Vec<Value>> {
    let mut statement = db.prepare(sql)?;
    let records = statement.query_map([], |row| row.get::<_, String>(0))?;
    records.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
}
pub fn attachment_folder() -> Result<PathBuf> {
    with(|s| Ok(root().join("images").join(&s.id)))
}
pub fn load() -> Result<Value> {
    with(|s| {
        prune_history(&s.db)?;
        Ok(
            json!({"client":s.id,"drafts":rows(&s.db,"SELECT body FROM drafts ORDER BY updated DESC")?,
        "pending":rows(&s.db,"SELECT body FROM submissions WHERE state NOT IN ('written','rejected','resolved') ORDER BY created")?}),
        )
    })
}
pub fn save_draft(draft: Value) -> Result<Value> {
    with(|s| save_to(s, &draft))
}
fn save_to(s: &mut Store, draft: &Value) -> Result<Value> {
    let key = draft["key"]
        .as_str()
        .ok_or_else(|| anyhow!("draft key missing"))?;
    let revision = draft["revision"]
        .as_i64()
        .ok_or_else(|| anyhow!("draft revision missing"))?;
    s.db.execute("INSERT INTO drafts VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET body=excluded.body,revision=excluded.revision,updated=excluded.updated WHERE excluded.revision>=drafts.revision",
        params![key,serde_json::to_string(&draft)?,revision,now()])?;
    Ok(Value::Null)
}
pub fn begin_submission(args: &Value) -> Result<()> {
    let id = args["request"]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("id missing"))?;
    with(|s| {
        let mut body = args.clone();
        body["receipt"] = json!({"state":"pending"});
        s.db.execute(
            "INSERT INTO submissions VALUES(?1,?2,'pending',?3)",
            params![id, serde_json::to_string(&body)?, now()],
        )?;
        Ok(())
    })
}
pub fn finish_submission(args: &Value, receipt: &Value) -> Result<()> {
    let mut body = args.clone();
    body["receipt"] = receipt.clone();
    let id = args["request"]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("id missing"))?;
    with(|s| {
        let tx = s.db.transaction()?;
        tx.execute(
            "UPDATE submissions SET body=?2,state=?3 WHERE id=?1",
            params![
                id,
                serde_json::to_string(&body)?,
                receipt["state"].as_str().unwrap_or("unknown")
            ],
        )?;
        if receipt["state"] == "written" {
            tx.execute(
                "INSERT OR IGNORE INTO history VALUES(?1,?2,?3,?4,?5)",
                params![
                    id,
                    args["project"].as_str().unwrap_or(""),
                    args["agent"].as_str().unwrap_or(""),
                    serde_json::to_string(&body)?,
                    now()
                ],
            )?;
            tx.execute(
                "DELETE FROM drafts WHERE key=?1 AND revision=?2",
                params![
                    args["draft"]["key"].as_str().unwrap_or(""),
                    args["draft"]["revision"].as_i64().unwrap_or(-1)
                ],
            )?;
            tx.execute(
                "DELETE FROM history WHERE created<?1",
                params![now() - 30 * 24 * 60 * 60 * 1000],
            )?;
            tx.execute("DELETE FROM history WHERE project=?1 AND agent=?2 AND id NOT IN (SELECT id FROM history WHERE project=?1 AND agent=?2 ORDER BY created DESC LIMIT 500)",params![args["project"].as_str().unwrap_or(""),args["agent"].as_str().unwrap_or("")])?;
        }
        tx.commit()?;
        Ok(())
    })
}
pub fn update_receipt(receipt: &Value) -> Result<()> {
    let id = receipt["request"]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("receipt id missing"))?;
    let previous: Option<Value> = with(|s| {
        use rusqlite::OptionalExtension;
        let body: Option<String> =
            s.db.query_row("SELECT body FROM submissions WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(body.map(|b| serde_json::from_str(&b)).transpose()?)
    })?;
    if let Some(args) = previous {
        if receipt["resolved"] == true {
            with(|s| {
                s.db.execute("UPDATE submissions SET state='resolved' WHERE id=?1", [id])?;
                Ok(())
            })
        } else {
            finish_submission(&args, receipt)
        }
    } else {
        Ok(())
    }
}
pub fn history(args: &Value) -> Result<Value> {
    with(|s| {
        prune_history(&s.db)?;
        let query = format!("%{}%", args["query"].as_str().unwrap_or(""));
        let mut stmt = s.db.prepare(
            "SELECT body FROM history WHERE project=?1 AND agent=?2 AND body LIKE ?3 ORDER BY created DESC LIMIT 100",
        )?;
        let records = stmt.query_map(
            params![
                args["project"].as_str().unwrap_or(""),
                args["agent"].as_str().unwrap_or(""),
                query
            ],
            |r| r.get::<_, String>(0),
        )?;
        Ok(json!(
            records
                .map(|r| Ok(serde_json::from_str::<Value>(&r?)?))
                .collect::<Result<Vec<_>>>()?
        ))
    })
}
fn prune_history(db: &Connection) -> Result<()> {
    db.execute("DELETE FROM history WHERE created<?1 OR id IN (SELECT id FROM (SELECT id,ROW_NUMBER() OVER(PARTITION BY project,agent ORDER BY created DESC,id DESC) AS n FROM history) WHERE n>500)", [now() - 30 * 24 * 60 * 60 * 1000])?;
    Ok(())
}
pub fn recovery() -> Result<Value> {
    let own = load()?;
    let mut result = own["drafts"].as_array().cloned().unwrap_or_default();
    let own_id = own["client"].as_str().unwrap_or("");
    for entry in std::fs::read_dir(root().join("clients"))? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sqlite")
            || path.file_stem().and_then(|s| s.to_str()) == Some(own_id)
        {
            continue;
        }
        let recovered =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(anyhow::Error::from)
                .and_then(|db| rows(&db, "SELECT body FROM drafts ORDER BY updated DESC"));
        let drafts = match recovered {
            Ok(drafts) => drafts,
            Err(error) => {
                result.push(json!({"recoveryError":error.to_string(),"source":path}));
                continue;
            }
        };
        for mut draft in drafts {
            draft["importedFrom"] = json!(path.file_stem().map(|s| s.to_string_lossy()));
            result.push(draft);
        }
    }
    Ok(json!(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("my-herdr-store-test-{}", uuid::Uuid::new_v4()))
    }
    #[test]
    fn drafts_survive_reopen_and_stale_async_saves_cannot_overwrite_newer_edits() {
        let root = scratch();
        let mut s = Store::open_at(root.clone()).unwrap();
        let draft = json!({"key":"target","revision":2,"text":"中文\n  🙂é","references":[{"path":"D:/中文 空白.png"}],"selection":{"start":1,"end":4}});
        save_to(&mut s, &draft).unwrap();
        save_to(&mut s, &json!({"key":"target","revision":1,"text":"stale"})).unwrap();
        drop(s);
        let s = Store::open_at(root.clone()).unwrap();
        assert_eq!(rows(&s.db, "SELECT body FROM drafts").unwrap(), vec![draft]);
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn concurrent_clients_never_write_the_same_database() {
        let root = scratch();
        let mut a = Store::open_at(root.clone()).unwrap();
        let b = Store::open_at(root.clone()).unwrap();
        assert_ne!(a.id, b.id);
        save_to(
            &mut a,
            &json!({"key":"a","revision":1,"text":"only client a"}),
        )
        .unwrap();
        assert!(rows(&b.db, "SELECT body FROM drafts").unwrap().is_empty());
        drop(a);
        drop(b);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn storage_errors_preserve_the_previous_durable_draft() {
        let root = scratch();
        let mut s = Store::open_at(root.clone()).unwrap();
        save_to(&mut s, &json!({"key":"a","revision":1,"text":"saved"})).unwrap();
        s.db.execute_batch("PRAGMA query_only=ON").unwrap();
        assert!(save_to(&mut s, &json!({"key":"a","revision":2,"text":"unsaved"})).is_err());
        assert_eq!(
            rows(&s.db, "SELECT body FROM drafts").unwrap()[0]["text"],
            "saved"
        );
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn history_retention_never_prunes_drafts_or_unknown_submissions() {
        let root = scratch();
        let mut s = Store::open_at(root.clone()).unwrap();
        save_to(&mut s, &json!({"key":"unsent","revision":1,"text":"keep"})).unwrap();
        s.db.execute(
            "INSERT INTO submissions VALUES('unknown','{}','unknown',0)",
            [],
        )
        .unwrap();
        let tx = s.db.transaction().unwrap();
        for n in 0..502 {
            tx.execute(
                "INSERT INTO history VALUES(?1,'project','codex','{}',?2)",
                params![format!("id-{n}"), now() + n],
            )
            .unwrap();
        }
        tx.execute(
            "INSERT INTO history VALUES('old','other','claude','{}',0)",
            [],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO history VALUES('other','other','claude','{}',?1)",
            [now()],
        )
        .unwrap();
        tx.commit().unwrap();
        prune_history(&s.db).unwrap();
        let count: i64 =
            s.db.query_row("SELECT COUNT(*) FROM history", [], |r| r.get(0))
                .unwrap();
        assert_eq!(count, 501);
        assert_eq!(rows(&s.db, "SELECT body FROM drafts").unwrap().len(), 1);
        assert_eq!(
            rows(&s.db, "SELECT body FROM submissions").unwrap().len(),
            1
        );
        drop(s);
        std::fs::remove_dir_all(root).unwrap();
    }
}
