//! Optional administrator-installed text-processing workers. No plugin code is embedded.
use super::*;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    id: String,
    name: String,
    pattern: String,
    #[serde(default = "text_input")]
    input: String,
    #[serde(default = "yes")]
    requires_ai: bool,
}
fn text_input() -> String {
    "text".into()
}
fn yes() -> bool {
    true
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    protocol: u32,
    enabled: bool,
    commands: Vec<Command>,
    #[serde(default)]
    media: bool,
}
#[derive(Clone)]
struct Plugin {
    manifest: Manifest,
    directory: PathBuf,
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}
fn root() -> PathBuf {
    PathBuf::from(std::env::var("MINERVA_PLUGINS").unwrap_or_else(|_| "plugins".into()))
        .join("server")
}
fn discover(root: &Path) -> (Vec<Plugin>, Vec<Value>) {
    let mut plugins = vec![];
    let mut diagnostics = vec![];
    let Ok(entries) = std::fs::read_dir(root) else {
        return (plugins, diagnostics);
    };
    for entry in entries.flatten().take(64) {
        let directory = entry.path();
        if !directory.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let load = || -> Option<Manifest> {
            let path = directory.join("manifest.json");
            if std::fs::metadata(&path).ok()?.len() > 32768 {
                return None;
            }
            let m: Manifest = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
            if m.id != name
                || !valid_id(&m.id)
                || m.protocol != 1
                || m.version.is_empty()
                || m.name.len() > 200
                || m.commands.len() > 20
                || m.commands.is_empty()
            {
                return None;
            }
            let mut ids = HashSet::new();
            if m.commands.iter().any(|c| {
                !["text", "youtube", "audio"].contains(&c.input.as_str())
                    || !valid_id(&c.id)
                    || !valid_id(&c.pattern)
                    || c.name.len() > 200
                    || !ids.insert(c.id.clone())
                    || !directory
                        .join("patterns")
                        .join(format!("{}.md", c.pattern))
                        .is_file()
            }) || !directory.join("worker.py").is_file()
            {
                return None;
            }
            Some(m)
        };
        match load() {
            Some(m) if m.enabled => plugins.push(Plugin{manifest:m,directory}),
            Some(_) => diagnostics.push(json!({"id":name,"status":"disabled"})),
            None => diagnostics.push(json!({"id":name,"status":"invalid","message":"Check protocol 1 manifest, unique commands, worker.py and pattern files"})),
        }
    }
    plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    (plugins, diagnostics)
}
fn python() -> String {
    std::env::var("MINERVA_PLUGIN_PYTHON").unwrap_or_else(|_| "python3".into())
}
pub(super) fn initialize(db: &Connection) -> Result<(), rusqlite::Error> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS plugin_jobs(id TEXT PRIMARY KEY,user_id TEXT NOT NULL,note_id TEXT NOT NULL,plugin_id TEXT NOT NULL,status TEXT NOT NULL,payload TEXT NOT NULL,created_at INTEGER NOT NULL);
        UPDATE plugin_jobs SET status='interrupted' WHERE status IN ('queued','running');")
}
async fn worker(p: &Plugin, request: Value) -> Result<Value, ApiError> {
    let seconds = if request["operation"] == "ingest" {
        180
    } else {
        10
    };
    let run = async {
        let mut child = tokio::process::Command::new(python())
            .arg("-I")
            .arg(p.directory.join("worker.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                bad("Python 3 is unavailable. Install it or configure MINERVA_PLUGIN_PYTHON.")
            })?;
        let mut stdin = child.stdin.take().ok_or_else(internal)?;
        let mut stdout = child.stdout.take().ok_or_else(internal)?.take(2097153);
        let input = serde_json::to_vec(&request).map_err(|_| internal())?;
        let write = async move {
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let read = async {
            let mut out = vec![];
            stdout.read_to_end(&mut out).await?;
            Ok::<_, std::io::Error>(out)
        };
        let (_, out) =
            tokio::try_join!(write, read).map_err(|_| bad("Plugin communication failed"))?;
        if out.len() > 2097152 {
            return Err(bad("Plugin output exceeded its size limit"));
        }
        let status = child.wait().await.map_err(|_| internal())?;
        if !status.success() {
            let error = serde_json::from_slice::<Value>(&out)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| {
                    "Plugin failed. Check dependencies or try another model.".into()
                });
            return Err(bad(&error.chars().take(500).collect::<String>()));
        }
        serde_json::from_slice(&out).map_err(|_| bad("Plugin returned invalid JSON"))
    };
    tokio::time::timeout(Duration::from_secs(seconds), run)
        .await
        .map_err(|_| bad("Plugin worker timed out"))?
}
fn note_for<'a>(s: &'a Store, u: &User, key: &str) -> Result<&'a Note, ApiError> {
    let n = s
        .data
        .notes
        .iter()
        .find(|n| n.id == key && !n.trashed)
        .ok_or_else(missing)?;
    note_access(&s.data, u, n, false)?;
    Ok(n)
}
fn job(s: &Store, u: &User, key: &str) -> Result<Value, ApiError> {
    let row: Option<(String,String,String)>=s.db.query_row("SELECT note_id,status,payload FROM plugin_jobs WHERE id=?1 AND user_id=?2 AND status!='deleted'",params![key,u.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|internal())?;
    let (note, status, payload) = row.ok_or_else(missing)?;
    note_for(s, u, &note)?;
    let mut v: Value = serde_json::from_str(&payload).map_err(|_| internal())?;
    v["id"] = json!(key);
    v["status"] = json!(status);
    if status == "interrupted" {
        v["error"] = json!("Server restarted during processing. Retry to start a new job.");
    }
    Ok(v)
}
pub(super) async fn endpoint(
    State(store): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    if method != Method::GET
        && headers
            .get("x-minerva-client")
            .and_then(|h| h.to_str().ok())
            != Some("1")
    {
        return Err(bad("X-Minerva-Client: 1 is required"));
    }
    if body.len() > 65536 {
        return Err(bad("Request is too large"));
    }
    let v: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|_| bad("Invalid JSON"))?
    };
    let parts: Vec<_> = uri
        .path()
        .trim_start_matches("/api/v1/server-plugins/")
        .split('/')
        .collect();
    {
        let s = store.lock().map_err(|_| internal())?;
        authenticate(&s.data, &headers)?;
    }
    if method == Method::GET && parts == ["capabilities"] {
        let (plugins, mut diagnostics) = discover(&root());
        let python_ok = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::process::Command::new(python())
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await
        .is_ok_and(|r| r.is_ok_and(|s| s.success()));
        let mut media = HashMap::new();
        for p in &plugins {
            if p.manifest.media && python_ok {
                match worker(p, json!({"operation":"diagnostics"})).await {
                    Ok(v) => {
                        media.insert(p.manifest.id.clone(), v);
                    }
                    Err(e) => {
                        diagnostics
                            .push(json!({"id":p.manifest.id,"status":"unavailable","message":e.1}));
                    }
                }
            }
        }
        let s = store.lock().map_err(|_| internal())?;
        let count: i64 =
            s.db.query_row("SELECT COUNT(*) FROM ai_providers", [], |r| r.get(0))
                .map_err(|_| internal())?;
        let asr = ai::transcription_available(&s);
        let mut commands = vec![];
        for p in &plugins {
            for c in &p.manifest.commands {
                let m = media.get(&p.manifest.id).cloned().unwrap_or(json!({}));
                let available = python_ok
                    && (!c.requires_ai || count > 0)
                    && match c.input.as_str() {
                        "youtube" => m["youtube"] == true,
                        "audio" => asr,
                        _ => true,
                    };
                commands.push(json!({"plugin_id":p.manifest.id,"id":c.id,"name":c.name,"pattern":c.pattern,"input":c.input,"available":available,"ai_available":count>0,"audio_fallback":asr && m["allow_audio_fallback"]==true,"language":m["preferred_language"],"reason":if !python_ok {"Install Python 3"} else if c.input=="youtube" && m["youtube"]!=true {"Install yt-dlp and validate the plugin configuration"} else if c.input=="audio" && !asr {"Configure a transcription provider and model"} else {"Configure an AI provider"}}));
            }
        }
        diagnostics.push(json!({"dependency":"transcription","available":asr,"remediation":"Set MINERVA_ASR_PROVIDER and MINERVA_ASR_MODEL"}));
        for m in media.values() {
            if let Some(ds) = m["diagnostics"].as_array() {
                diagnostics.extend(ds.clone());
            }
        }
        diagnostics.push(json!({"dependency":"python3","available":python_ok,"remediation":"Install Python 3 or set MINERVA_PLUGIN_PYTHON"}));
        diagnostics.push(json!({"dependency":"ai_provider","available":count>0,"remediation":"Add a profile in AI providers"}));
        return Ok(
            axum::Json(json!({"commands":commands,"diagnostics":diagnostics})).into_response(),
        );
    }
    if parts.len() == 2 && parts[0] == "jobs" {
        let mut s = store.lock().map_err(|_| internal())?;
        let u = authenticate(&s.data, &headers)?;
        let result = job(&s, &u, parts[1])?;
        if method == Method::GET {
            return Ok(axum::Json(result).into_response());
        }
        if method == Method::DELETE {
            if let Some(task) = s.plugin_tasks.remove(parts[1]) {
                task.abort();
            }
            s.db.execute(
                "UPDATE plugin_jobs SET status='deleted',payload='{}' WHERE id=?1",
                [parts[1]],
            )
            .map_err(|_| internal())?;
            return Ok(axum::Json(json!({"ok":true})).into_response());
        }
        if method == Method::POST && v["action"] == "cancel" {
            if let Some(task) = s.plugin_tasks.remove(parts[1]) {
                task.abort();
            }
            s.db.execute("UPDATE plugin_jobs SET status='cancelled' WHERE id=?1 AND status IN ('queued','running')",[parts[1]]).map_err(|_|internal())?;
            return Ok(axum::Json(job(&s, &u, parts[1])?).into_response());
        }
        return Err(missing());
    }
    if method != Method::POST || parts != ["jobs"] {
        return Err(missing());
    }
    let (plugins, _) = discover(&root());
    let p = plugins
        .into_iter()
        .find(|p| v["plugin_id"] == p.manifest.id)
        .ok_or_else(|| bad("Plugin is missing, disabled or invalid"))?;
    let command = p
        .manifest
        .commands
        .iter()
        .find(|c| v["command"] == c.id)
        .ok_or_else(|| bad("Unknown plugin command"))?
        .clone();
    let key = id();
    let (mut payload, user_id) = {
        let s = store.lock().map_err(|_| internal())?;
        let u = authenticate(&s.data, &headers)?;
        let n = note_for(&s, &u, &field(&v, "note_id")?)?;
        let mut sources = vec![];
        let selected = v["selection"].as_str().unwrap_or("");
        if command.input != "text" {
            if command.input == "youtube" {
                canonical_video(v["url"].as_str().unwrap_or(""))?;
            }
            if command.input == "audio" {
                let attachment = field(&v, "attachment_id")?;
                let belongs: bool =
                    s.db.query_row(
                        "SELECT EXISTS(SELECT 1 FROM attachments WHERE id=?1 AND note_id=?2)",
                        params![attachment, n.id],
                        |r| r.get(0),
                    )
                    .map_err(|_| internal())?;
                if !belongs {
                    return Err(missing());
                }
                if !ai::transcription_available(&s) {
                    return Err(bad("Transcription provider is not configured"));
                }
            }
        } else if let Some(block) = v["block_id"].as_str().filter(|s| !s.is_empty()) {
            let b = n
                .blocks
                .iter()
                .find(|b| b.id == block)
                .ok_or_else(missing)?;
            if !selected.is_empty() && !b.markdown.contains(selected) {
                return Err(bad("Selected text changed. Select it again."));
            }
            sources.push(
                json!({"id":b.id,"text":if selected.is_empty(){b.markdown.as_str()}else{selected}}),
            );
        } else {
            if !selected.is_empty() {
                return Err(bad("Selection requires a source block"));
            }
            sources = n
                .blocks
                .iter()
                .filter(|b| !b.markdown.trim().is_empty())
                .map(|b| json!({"id":b.id,"text":b.markdown}))
                .collect();
        }
        let size: usize = sources
            .iter()
            .map(|s| s["text"].as_str().unwrap_or("").len())
            .sum();
        if command.input == "text" && (size == 0 || size > 192000 || sources.len() > 1000) {
            return Err(bad(
                "Choose 1–192,000 bytes of text in at most 1,000 blocks.",
            ));
        }
        let active:i64=s.db.query_row("SELECT COUNT(*) FROM plugin_jobs WHERE status IN ('queued','running') AND user_id=?1",[&u.id],|r|r.get(0)).map_err(|_|internal())?;
        let total: i64 =
            s.db.query_row(
                "SELECT COUNT(*) FROM plugin_jobs WHERE status IN ('queued','running')",
                [],
                |r| r.get(0),
            )
            .map_err(|_| internal())?;
        let recent: i64 =
            s.db.query_row(
                "SELECT COUNT(*) FROM plugin_jobs WHERE user_id=?1 AND created_at>?2",
                params![u.id, now().saturating_sub(3600)],
                |r| r.get(0),
            )
            .map_err(|_| internal())?;
        if active > 0 || total >= 2 || recent >= 20 {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS,"Processing limit reached: one job per user, two per server, 20 per hour. Wait or cancel an active job.".into()));
        }
        let provider_id = v["provider_id"].as_str().unwrap_or("").to_string();
        if command.requires_ai || !provider_id.is_empty() {
            let _ = ai::completion(&s, &provider_id, "", "")?;
        }
        let mut payload = json!({"note_id":n.id,"note_revision":n.revision,"plugin_id":p.manifest.id,"plugin_version":p.manifest.version,"command":command.id,"pattern":command.pattern,"provider_id":provider_id,"sources":sources,"input":command.input,"url":v["url"],"language":v["language"],"attachment_id":v["attachment_id"],"allow_audio":v["allow_audio"]==true,"chunk_results":[]});
        if let Some(previous) = v["retry_of"].as_str() {
            let old = job(&s, &u, previous)?;
            if old["note_id"] != payload["note_id"]
                || old["plugin_id"] != payload["plugin_id"]
                || old["plugin_version"] != payload["plugin_version"]
                || old["command"] != payload["command"]
                || old["provider_id"] != payload["provider_id"]
            {
                return Err(bad(
                    "Retry must use the same note, plugin version, command and provider",
                ));
            }
            for key in [
                "sources",
                "source",
                "chunk_results",
                "url",
                "language",
                "attachment_id",
                "allow_audio",
            ] {
                if let Some(value) = old.get(key) {
                    payload[key] = value.clone();
                }
            }
        }
        s.db.execute(
            "DELETE FROM plugin_jobs WHERE created_at<?1 AND status NOT IN ('queued','running')",
            [now().saturating_sub(7 * 86400)],
        )
        .map_err(|_| internal())?;
        s.db.execute(
            "INSERT INTO plugin_jobs VALUES(?1,?2,?3,?4,'queued',?5,?6)",
            params![key, u.id, n.id, p.manifest.id, payload.to_string(), now()],
        )
        .map_err(|_| internal())?;
        (payload, u.id)
    };
    // Register the task before releasing its start gate, so cancellation cannot race registration.
    let (start, ready) = tokio::sync::oneshot::channel::<()>();
    let task_store = store.clone();
    let task_key = key.clone();
    let task = tokio::spawn(async move {
        if ready.await.is_err() {
            return;
        }
        let result = tokio::time::timeout(
            Duration::from_secs(1800),
            execute(&task_store, &task_key, &user_id, &p, &mut payload),
        )
        .await
        .unwrap_or_else(|_| Err(bad("Processing timed out. Retry with less text.")));
        if let Ok(mut s) = task_store.lock() {
            let mut saved = payload;
            let status = match result {
                Ok(result) => {
                    saved["result"] = result;
                    "completed"
                }
                Err(e) => {
                    saved["error"] = json!(e.1);
                    "failed"
                }
            };
            let _=s.db.execute("UPDATE plugin_jobs SET status=?1,payload=?2 WHERE id=?3 AND status IN ('queued','running')",params![status,saved.to_string(),task_key]);
            s.plugin_tasks.remove(&task_key);
        }
    });
    store
        .lock()
        .map_err(|_| internal())?
        .plugin_tasks
        .insert(key.clone(), task.abort_handle());
    let _ = start.send(());
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(json!({"id":key,"status":"queued"})),
    )
        .into_response())
}
fn canonical_video(raw: &str) -> Result<String, ApiError> {
    let u = url::Url::parse(raw).map_err(|_| bad("Enter an HTTPS YouTube video URL"))?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
    {
        return Err(bad(
            "Enter an HTTPS YouTube video URL without credentials or a port",
        ));
    }
    let parts: Vec<_> = u.path().trim_matches('/').split('/').collect();
    let video = match u.host_str().unwrap_or("") {
        "youtu.be" if parts.len() == 1 => parts[0].to_string(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" => {
            if u.path() == "/watch" {
                u.query_pairs()
                    .find(|(k, _)| k == "v")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default()
            } else if parts.len() == 2 && ["shorts", "embed", "live"].contains(&parts[0]) {
                parts[1].into()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    };
    if video.len() != 11
        || !video
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err(bad(
            "Enter a single YouTube video, not a playlist or another website",
        ));
    }
    Ok(format!("https://www.youtube.com/watch?v={video}"))
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, ApiError> {
        let dir = std::env::temp_dir().join(format!("minerva-media-{}", id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|_| bad("Cannot create a private media processing directory"))?;
        Ok(Self(dir))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn checkpoint(
    store: &Shared,
    key: &str,
    user_id: &str,
    p: &Plugin,
    payload: &Value,
    stage: &str,
) -> Result<(), ApiError> {
    let s = store.lock().map_err(|_| internal())?;
    let u = s
        .data
        .users
        .iter()
        .find(|a| a.user.id == user_id && !a.user.disabled)
        .ok_or_else(forbidden)?;
    let current = job(&s, &u.user, key)?;
    if !["queued", "running"].contains(&current["status"].as_str().unwrap_or("")) {
        return Err(bad("Job is no longer active"));
    }
    if !discover(&root())
        .0
        .iter()
        .any(|x| x.manifest.id == p.manifest.id && x.manifest.version == p.manifest.version)
    {
        return Err(bad("Plugin was disabled, removed or updated"));
    }
    let mut snapshot = payload.clone();
    snapshot["stage"] = json!(stage);
    s.db.execute(
        "UPDATE plugin_jobs SET status='running',payload=?1 WHERE id=?2",
        params![snapshot.to_string(), key],
    )
    .map_err(|_| internal())?;
    Ok(())
}
fn transcript_segments(v: &Value) -> Result<Vec<Value>, ApiError> {
    let segments = v["segments"].as_array().ok_or_else(|| {
        bad("Transcription model must return verbose_json with segment timestamps")
    })?;
    let mut result = vec![];
    let mut size = 0;
    let mut last = 0.0;
    for (i, segment) in segments.iter().enumerate() {
        let text = segment["text"]
            .as_str()
            .ok_or_else(|| bad("Invalid transcript text"))?
            .trim();
        let start = segment["start"]
            .as_f64()
            .ok_or_else(|| bad("Missing transcript timestamps"))?;
        let end = segment["end"]
            .as_f64()
            .ok_or_else(|| bad("Missing transcript timestamps"))?;
        if !start.is_finite() || !end.is_finite() || start < last || end < start || end > 14400.0 {
            return Err(bad(
                "Invalid transcript timing or audio longer than four hours",
            ));
        }
        last = start;
        size += text.len();
        if !text.is_empty() {
            result.push(json!({"id":format!("s{}",i+1),"text":text,"start":start,"end":end}));
        }
    }
    if result.is_empty() || result.len() > 6000 || size > 192000 {
        return Err(bad(
            "Transcript is empty or exceeds the 192 KB / 6,000 segment limit",
        ));
    }
    Ok(result)
}
fn chunks(sources: &[Value]) -> Result<Vec<Vec<Value>>, ApiError> {
    let mut result = vec![];
    let mut current = vec![];
    let mut size = 0;
    for source in sources {
        let mut remaining = source["text"]
            .as_str()
            .ok_or_else(|| bad("Invalid source text"))?;
        while !remaining.is_empty() {
            let mut end = remaining.len().min(8000);
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            let mut part = source.clone();
            part["text"] = json!(&remaining[..end]);
            if size + end > 12000 || current.len() >= 100 {
                result.push(std::mem::take(&mut current));
                size = 0;
            }
            current.push(part);
            size += end;
            remaining = &remaining[end..];
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    if result.len() > 24 {
        return Err(bad(
            "Source needs more than 24 model calls. Process a shorter source.",
        ));
    }
    Ok(result)
}
async fn execute(
    store: &Shared,
    key: &str,
    user_id: &str,
    p: &Plugin,
    payload: &mut Value,
) -> Result<Value, ApiError> {
    checkpoint(store, key, user_id, p, payload, "preparing")?;
    if payload["sources"].as_array().is_none_or(|a| a.is_empty()) {
        let mut audio = None;
        if payload["input"] == "youtube" {
            let url = canonical_video(payload["url"].as_str().unwrap_or(""))?;
            checkpoint(store, key, user_id, p, payload, "retrieving_captions")?;
            let acquired = worker(
                p,
                json!({"operation":"ingest","url":url,"language":payload["language"]}),
            )
            .await?;
            payload["source"] = acquired["source"].clone();
            if acquired["needs_audio"] == true {
                if payload["allow_audio"] != true {
                    return Err(bad(acquired["error"].as_str().unwrap_or("No captions available. Choose another language or permitted audio fallback.")));
                }
                {
                    let s = store.lock().map_err(|_| internal())?;
                    if !ai::transcription_available(&s) {
                        return Err(bad("No transcription provider configured"));
                    }
                }
                let scratch = Scratch::new()?;
                checkpoint(store, key, user_id, p, payload, "retrieving_audio")?;
                worker(p,json!({"operation":"ingest","url":url,"language":payload["language"],"audio":true,"authorized":true,"directory":scratch.0})).await?;
                let path = scratch.0.join("source.m4a");
                if std::fs::metadata(&path)
                    .map_err(|_| bad("Audio was not retrieved"))?
                    .len()
                    > 20 * 1024 * 1024
                {
                    return Err(bad("Audio exceeds 20 MiB"));
                }
                audio = Some(std::fs::read(path).map_err(|_| bad("Cannot read retrieved audio"))?);
                // Scratch is removed here, even on failures; audio is only retained in memory.
            } else {
                payload["sources"] = acquired["sources"].clone();
            }
        } else if payload["input"] == "audio" {
            let s = store.lock().map_err(|_| internal())?;
            let (name, bytes): (String, Vec<u8>) =
                s.db.query_row(
                    "SELECT filename,content FROM attachments WHERE id=?1 AND note_id=?2",
                    params![
                        payload["attachment_id"].as_str(),
                        payload["note_id"].as_str()
                    ],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|_| internal())?
                .ok_or_else(missing)?;
            payload["source"] = json!({"type":"audio","title":name,"attachment_id":payload["attachment_id"],"url":format!("/api/v1/attachments/{}",payload["attachment_id"].as_str().unwrap_or(""))});
            audio = Some(bytes);
        }
        if let Some(bytes) = audio {
            checkpoint(store, key, user_id, p, payload, "transcribing")?;
            let request = {
                let s = store.lock().map_err(|_| internal())?;
                ai::transcription(&s, bytes)?
            };
            let transcript = ai::read_transcription(request).await?;
            payload["sources"] = json!(transcript_segments(&transcript)?);
            payload["source"]["language"] = transcript["language"].clone();
        }
        checkpoint(store, key, user_id, p, payload, "transcript_ready")?;
    }
    let sources = payload["sources"]
        .as_array()
        .ok_or_else(|| bad("No transcript was returned"))?
        .clone();
    if payload["provider_id"].as_str().unwrap_or("").is_empty() {
        return Ok(json!({"items":[],"transcript_only":true}));
    }
    let batches = chunks(&sources)?;
    let completed = payload["chunk_results"].as_array().map_or(0, Vec::len);
    payload["total_chunks"] = json!(batches.len());
    for (index, batch) in batches.iter().enumerate().skip(completed) {
        payload["completed_chunks"] = json!(index);
        checkpoint(store, key, user_id, p, payload, "extracting_knowledge")?;
        let prepared = worker(
            p,
            json!({"operation":"prepare","pattern":payload["pattern"],"sources":batch}),
        )
        .await?;
        let system = prepared["system"]
            .as_str()
            .filter(|s| s.len() <= 16000)
            .ok_or_else(|| bad("Invalid plugin prompt"))?;
        let input = prepared["input"]
            .as_str()
            .filter(|s| s.len() <= 48000)
            .ok_or_else(|| bad("Invalid plugin input"))?;
        let completion = {
            let s = store.lock().map_err(|_| internal())?;
            ai::completion(
                &s,
                payload["provider_id"].as_str().unwrap_or(""),
                system,
                input,
            )?
        };
        let answer = completion.run(&store).await?;
        let result = worker(
            p,
            json!({"operation":"validate","answer":answer,"sources":batch}),
        )
        .await?;
        payload["chunk_results"]
            .as_array_mut()
            .ok_or_else(internal)?
            .push(result);
        checkpoint(store, key, user_id, p, payload, "extracting_knowledge")?;
    }
    let mut items = vec![];
    let mut seen = HashSet::new();
    for result in payload["chunk_results"].as_array().ok_or_else(internal)? {
        for item in result["items"]
            .as_array()
            .ok_or_else(|| bad("Invalid plugin result"))?
        {
            let identity = format!(
                "{}:{}",
                item["kind"],
                item["text"].as_str().unwrap_or("").to_lowercase()
            );
            if seen.insert(identity) {
                items.push(item.clone());
            }
        }
    }
    payload["completed_chunks"] = json!(batches.len());
    checkpoint(store, key, user_id, p, payload, "completed")?;
    Ok(json!({"items":items,"chunks":batches.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn media_urls_timestamps_and_unicode_chunking() {
        assert_eq!(
            canonical_video("https://youtu.be/abcdefghijk?t=1").unwrap(),
            "https://www.youtube.com/watch?v=abcdefghijk"
        );
        for raw in [
            "https://localhost/watch?v=abcdefghijk",
            "https://youtube.com.evil/watch?v=abcdefghijk",
            "http://youtu.be/abcdefghijk",
        ] {
            assert!(canonical_video(raw).is_err());
        }
        let text = "🌱 learning ".repeat(3000);
        let batches = chunks(&[json!({"id":"b1","text":text})]).unwrap();
        let joined = batches
            .iter()
            .flatten()
            .map(|s| s["text"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(joined, text);
        assert!(batches.len() > 1);
        assert!(
            transcript_segments(&json!({"segments":[{"text":"Hi","start":3,"end":2}]})).is_err()
        );
        let result =
            transcript_segments(&json!({"segments":[{"text":" Hi ","start":3,"end":5}]})).unwrap();
        assert_eq!(result[0]["start"], 3.0);
        assert_eq!(result[0]["text"], "Hi");
    }
    #[test]
    fn absent_invalid_disabled_and_valid_plugins() {
        let dir = std::env::temp_dir().join(format!("minerva-host-{}", id()));
        assert!(discover(&dir).0.is_empty());
        let p = dir.join("test");
        std::fs::create_dir_all(p.join("patterns")).unwrap();
        std::fs::write(p.join("manifest.json"), "oops").unwrap();
        assert_eq!(discover(&dir).1[0]["status"], "invalid");
        std::fs::write(p.join("worker.py"), "").unwrap();
        std::fs::write(p.join("patterns/summary.md"), "Summarize").unwrap();
        let mut m = json!({"id":"test","name":"Test","version":"1","protocol":1,"enabled":true,"commands":[{"id":"summary","name":"Summary","pattern":"summary"}]});
        std::fs::write(p.join("manifest.json"), m.to_string()).unwrap();
        assert_eq!(discover(&dir).0.len(), 1);
        m["enabled"] = json!(false);
        std::fs::write(p.join("manifest.json"), m.to_string()).unwrap();
        assert_eq!(discover(&dir).1[0]["status"], "disabled");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn restart_marks_running_jobs_interrupted() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        db.execute(
            "INSERT INTO plugin_jobs VALUES('j','u','n','p','running','{}',0)",
            [],
        )
        .unwrap();
        initialize(&db).unwrap();
        assert_eq!(
            db.query_row("SELECT status FROM plugin_jobs", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "interrupted"
        );
    }
}
