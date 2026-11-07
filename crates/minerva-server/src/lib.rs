mod administration;
mod ai;
mod anki;
mod attachments;
mod audio;
mod bootstrap;
mod calendar;
mod collaboration;
mod comments;
mod databases;
mod embeds;
mod export;
mod flashcards;
mod formulas;
mod fsrs;
mod graph;
mod i18n;
mod import;
mod journal;
mod live;
mod mail;
mod mcp;
mod oauth;
mod plugins;
mod previews;
mod push;
mod record_pages;
mod recordings;
mod reminders;
mod removal;
mod render;
mod replication;
mod search;
mod server_plugins;
mod sharing;
mod sites;
mod storage;
mod tokens;
mod twofactor;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use minerva_core::*;
use render::render;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
struct Account {
    user: User,
    password_hash: String,
    #[serde(default)]
    two_factor: twofactor::TwoFactor,
    /// Set when an administrator chooses the password: the person picks their
    /// own on their next sign-in.
    #[serde(default)]
    must_change_password: bool,
    /// The workspace the app opens first.
    #[serde(default)]
    default_workspace: Option<String>,
    /// The overview page's widgets, in order (see `web/dashboard.js`); `None`
    /// shows the default layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dashboard: Option<Value>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Session {
    user_id: String,
    expires: u64,
}
#[derive(Clone, Serialize, Deserialize)]
struct WorkspaceGrant {
    workspace_id: String,
    user_id: String,
    role: Role,
}
#[derive(Clone, Serialize, Deserialize, Default)]
struct Data {
    server_id: String,
    users: Vec<Account>,
    workspaces: Vec<Workspace>,
    folders: Vec<Folder>,
    grants: Vec<Grant>,
    /// Access to every folder of a workspace, including folders added later.
    #[serde(default)]
    workspace_grants: Vec<WorkspaceGrant>,
    notes: Vec<Arc<Note>>,
    /// Earlier versions of notes added by the current request, until `persist`
    /// saves them; saved versions stay in SQLite (see `storage.rs`).
    #[serde(skip)]
    history: HashMap<String, Vec<Arc<Note>>>,
    /// When each note's newest earlier version was taken (memory only).
    #[serde(skip)]
    versioned_at: HashMap<String, u64>,
    sessions: HashMap<String, Session>,
    #[serde(default)]
    /// Each note's live editing (Yjs) state, shared between copies of `Data` like
    /// `history`.
    /// Each note's collaborative document; shared between copies of `Data`
    /// until a request changes one.
    collaboration: Arc<HashMap<String, Arc<String>>>,
    #[serde(skip)]
    presence: HashMap<String, Vec<collaboration::Presence>>,
    /// Pending second-factor sign-ins, by challenge hash (memory only).
    #[serde(skip)]
    login_challenges: HashMap<String, twofactor::Challenge>,
    #[serde(default)]
    databases: Vec<Database>,
    #[serde(default)]
    mcp_credentials: Vec<mcp::Credential>,
    #[serde(default)]
    api_tokens: Vec<tokens::ApiToken>,
    /// Calendar feed addresses, by their secret's hash (see `calendar.rs`).
    #[serde(default)]
    calendar_feeds: Vec<calendar::Feed>,
    #[serde(default)]
    shares: Vec<sharing::Share>,
    #[serde(default)]
    publications: Vec<sharing::Publication>,
    #[serde(default)]
    sites: Vec<sites::Site>,
    /// Workspace ID → its Journal folder, Templates folder, and journal template note.
    #[serde(default)]
    journal_folders: HashMap<String, String>,
    #[serde(default)]
    template_folders: HashMap<String, String>,
    #[serde(default)]
    journal_templates: HashMap<String, String>,
    /// Replication: per-entity field clocks, the last issued clock, and applied
    /// push operations (see `replication.rs`).
    #[serde(default)]
    clocks: FieldClocks,
    #[serde(default)]
    hlc: String,
    #[serde(default)]
    sync_ops: HashMap<String, (u64, Value)>,
    /// Set while applying changes pulled from an upstream server, so they are not
    /// pushed back (memory only).
    #[serde(skip)]
    upstream: bool,
    /// A desktop app's embedded server, which may apply upstream changes.
    #[serde(default)]
    local: bool,
    /// Remote workspaces replicated into a desktop app, their sync status (memory
    /// only), and offline edits that lost to other changes.
    #[serde(default)]
    replicas: Vec<replication::Replica>,
    #[serde(skip)]
    sync_status: HashMap<String, Value>,
    #[serde(default)]
    sync_conflicts: Vec<Value>,
    /// Email waiting to be sent, `/remindme` reminders, and password-reset links
    /// by token hash (see `mail.rs`).
    #[serde(default)]
    mail_outbox: Vec<mail::Outgoing>,
    #[serde(default)]
    reminders: Vec<reminders::Reminder>,
    /// Push notifications: the server's VAPID key, people's subscriptions, and
    /// notifications waiting to go out (see `push.rs`).
    #[serde(default)]
    vapid: String,
    #[serde(default)]
    push_subscriptions: Vec<push::Subscription>,
    #[serde(default)]
    push_outbox: Vec<push::Message>,
    /// The language each person last used the app in, by user ID (see `i18n.rs`).
    #[serde(default)]
    languages: HashMap<String, String>,
    #[serde(default)]
    password_resets: HashMap<String, mail::Reset>,
    /// Inline tags of each note, worked out once per version of the note (memory
    /// only; shared by every copy of `Data`).
    #[serde(skip)]
    tag_cache: TagCache,
    /// Bumped when a note's comments change, so open pages hear about it
    /// (memory only; comments are kept in their own table, see `comments.rs`).
    #[serde(skip)]
    comment_activity: HashMap<String, u64>,
}
/// Replication's field clocks, by entity: shared between copies of `Data` and
/// copied only where a request changes them, since every request copies `Data`.
type FieldClocks = Arc<HashMap<String, Arc<std::collections::BTreeMap<String, String>>>>;
/// A note's version (held weakly) and its inline tags.
type CachedTags = (std::sync::Weak<Note>, Vec<String>);
#[derive(Clone, Default)]
struct TagCache(Arc<Mutex<HashMap<String, CachedTags>>>);
pub struct Store {
    db: Connection,
    data: Data,
    /// `data`'s row in the `settings` table as last saved (see `storage.rs`).
    saved: String,
    attempts: Limiter,
    ai: ai::Runtime,
    plugin_tasks: HashMap<String, tokio::task::AbortHandle>,
    /// IDs of notes that changed, for open pages (see `live.rs`).
    events: tokio::sync::broadcast::Sender<String>,
}
pub type Shared = Arc<Mutex<Store>>;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn id() -> String {
    Uuid::new_v4().to_string()
}
fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
fn password_hash(password: &str) -> Result<String, ApiError> {
    if password.len() < 12 || password.len() > 256 {
        return Err(bad("Passwords must contain 12–256 characters"));
    }
    Argon2::default()
        .hash_password(
            password.as_bytes(),
            &SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(|_| internal())?,
        )
        .map(|v| v.to_string())
        .map_err(|_| internal())
}
impl Store {
    pub fn open(
        path: &str,
        email: &str,
        password: &str,
    ) -> Result<Shared, Box<dyn std::error::Error>> {
        Self::open_with(path, || initial(email, password, false))
    }
    /// The embedded server of a desktop app: one owner who never signs in with a
    /// password, and a new session for this process each time it opens. Nothing
    /// listens on the network; the app passes requests to `app` in-process.
    pub fn open_local(path: &str) -> Result<(Shared, String), Box<dyn std::error::Error>> {
        let unusable = format!("{}{}", id(), id());
        let store = Self::open_with(path, || initial("local@minerva.localhost", &unusable, true))?;
        let token = format!("{}{}", id(), id());
        {
            let mut s = store.lock().map_err(|_| "Store lock poisoned")?;
            if !s.data.local {
                return Err(format!("{path} belongs to a server, not a desktop app").into());
            }
            s.ai.desktop = true;
            s.db.execute("UPDATE ai_providers SET private=1", [])?;
            let mut d = s.data.clone();
            let user_id = d.users[0].user.id.clone();
            d.sessions.clear();
            d.sessions.insert(
                hash_token(&token),
                Session {
                    user_id,
                    expires: now() + 86400 * 365 * 10,
                },
            );
            persist(&mut s, d).map_err(|e| e.1)?;
        }
        Ok((store, token))
    }
    fn open_with(
        path: &str,
        initial: impl FnOnce() -> Result<Data, ApiError>,
    ) -> Result<Shared, Box<dyn std::error::Error>> {
        let db = Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
        storage::initialize(&db)?;
        comments::initialize(&db)?;
        attachments::initialize(&db)?;
        replication::initialize(&db)?;
        let ai = ai::initialize(&db, path)?;
        server_plugins::initialize(&db)?;
        flashcards::initialize(&db)?;
        graph::initialize(&db)?;
        tokens::initialize(&db)?;
        oauth::initialize(&db)?;
        let (data, saved) = storage::open(&db, initial, |data| {
            ensure_personal(data);
            push::ensure_keys(data);
            // Databases from before replication get a change-log row and field clocks
            // for every entity, so a replica's first pull receives everything.
            if replication::is_empty(&db)? {
                let changes = replication::track(&Data::default(), data).map_err(|e| e.1)?;
                replication::log(&db, &changes, false)?;
            }
            Ok(())
        })?;
        Ok(Arc::new(Mutex::new(Self {
            db,
            data,
            saved,
            attempts: Limiter::default(),
            ai,
            plugin_tasks: HashMap::new(),
            events: live::channel(),
        })))
    }
}
/// A dashboard layout: up to 40 widgets of known types and sizes, with small
/// option objects; `null` restores the default layout.
fn valid_dashboard(v: &Value) -> Result<Option<Value>, ApiError> {
    if v.is_null() {
        return Ok(None);
    }
    let widgets = v.as_array().ok_or_else(|| bad("Widgets must be a list"))?;
    if widgets.len() > 40 || v.to_string().len() > 32 * 1024 {
        return Err(bad("A dashboard holds up to 40 widgets"));
    }
    const TYPES: [&str; 8] = [
        "welcome",
        "folders",
        "recent",
        "mentions",
        "favorites",
        "journal",
        "flashcards",
        "chart",
    ];
    for w in widgets {
        let ok = w["id"]
            .as_str()
            .is_some_and(|i| !i.is_empty() && i.len() <= 40)
            && w["type"].as_str().is_some_and(|t| TYPES.contains(&t))
            && w["size"]
                .as_str()
                .is_some_and(|s| matches!(s, "s" | "m" | "l" | "full"))
            && (w.get("options").is_none() || w["options"].is_object());
        if !ok {
            return Err(bad("Invalid widget"));
        }
    }
    Ok(Some(v.clone()))
}
/// The name of every account's personal workspace.
const PERSONAL: &str = "My Workspace";
/// Creates an account's personal workspace, with a first folder and the guide
/// in `lang`, and makes it the workspace the app opens. The workspace keeps
/// its English name, which the app shows in the reader's language.
fn create_personal(d: &mut Data, user_id: &str, lang: &str) -> String {
    let w = Workspace {
        id: id(),
        name: PERSONAL.into(),
        owner_id: user_id.into(),
        personal: true,
    };
    let folder = Folder {
        id: id(),
        workspace_id: w.id.clone(),
        name: i18n::tr(lang, "General"),
        parent_id: None,
    };
    d.notes.push(Arc::new(getting_started(&folder.id, lang)));
    d.folders.push(folder);
    if let Some(a) = d.users.iter_mut().find(|a| a.user.id == user_id) {
        a.default_workspace = Some(w.id.clone());
    }
    let wid = w.id.clone();
    d.workspaces.push(w);
    wid
}
/// The manual every personal workspace starts with, in `lang`: one block per
/// paragraph of `getting-started.md` (or its translation, such as
/// `getting-started.it.md`; their code, tables and lists have no blank lines).
fn getting_started(folder_id: &str, lang: &str) -> Note {
    let guide = match lang {
        "it" => include_str!("getting-started.it.md"),
        _ => include_str!("getting-started.md"),
    };
    Note {
        id: id(),
        folder_id: folder_id.into(),
        title: i18n::tr(lang, "Getting Started"),
        icon: "🧭".into(),
        blocks: guide
            .split("\n\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .enumerate()
            .map(|(i, s)| Block::new(s, i))
            .collect(),
        revision: 1,
        updated_at: now(),
        favorite: true,
        trashed: false,
        archived: false,
        cover: "gradient:3".into(),
        tags: vec!["guide".into()],
        journal: None,
        metadata: Default::default(),
        print_template: String::new(),
        record: None,
        folded: None,
    }
}
/// Every account has one personal workspace. Older data adopts an unshared
/// workspace the account owns that is evidently personal; otherwise one is
/// created.
fn ensure_personal(d: &mut Data) {
    for a in &mut d.users {
        if !d.local && a.user.admin {
            a.default_workspace = None;
            a.user.can_create_workspace = false;
        } else {
            a.user.can_create_workspace = true;
        }
    }
    let users: Vec<String> = d
        .users
        .iter()
        .filter(|a| d.local || !a.user.admin)
        .map(|a| a.user.id.clone())
        .collect();
    for uid in users {
        if d.workspaces.iter().any(|w| w.owner_id == uid && w.personal) {
            continue;
        }
        let adopt = d
            .workspaces
            .iter()
            .filter(|w| w.owner_id == uid && replication::replica(d, &w.id).is_none())
            .filter(|w| {
                matches!(
                    w.name.as_str(),
                    "My workspace" | "My Workspace" | "Personal"
                )
            })
            .find(|w| !removal::shared(d, w))
            .map(|w| w.id.clone());
        match adopt {
            Some(wid) => {
                let w = d.workspaces.iter_mut().find(|w| w.id == wid).unwrap();
                w.personal = true;
                w.name = PERSONAL.into();
                if let Some(a) = d.users.iter_mut().find(|a| a.user.id == uid) {
                    a.default_workspace.get_or_insert(wid);
                }
            }
            None => {
                let had_default = d
                    .users
                    .iter()
                    .find(|a| a.user.id == uid)
                    .and_then(|a| a.default_workspace.clone());
                create_personal(d, &uid, &i18n::default_language());
                // Someone who already works elsewhere keeps opening there.
                if let Some(default) = had_default {
                    if let Some(a) = d.users.iter_mut().find(|a| a.user.id == uid) {
                        a.default_workspace = Some(default);
                    }
                }
            }
        }
    }
}
/// A new server's data: its first account, a workspace, and a welcome note.
fn initial(email: &str, password: &str, local: bool) -> Result<Data, ApiError> {
    let user = User {
        token_workspace: None,
        id: id(),
        name: if local { "You" } else { "Administrator" }.into(),
        email: email.to_lowercase(),
        admin: true,
        can_create_workspace: local,
        disabled: false,
    };
    // A server starts with a team workspace to share (the administrator's own
    // "My Workspace" is added when the server opens); the desktop app's own
    // server has only the personal one.
    let workspace = Workspace {
        id: id(),
        name: if local { PERSONAL } else { "Team" }.into(),
        owner_id: user.id.clone(),
        personal: local,
    };
    let folder = Folder {
        id: id(),
        workspace_id: workspace.id.clone(),
        name: "Getting started".into(),
        parent_id: None,
    };
    let text = ["## A little space for big ideas", "Welcome to **Minerva**, your team's home for notes, knowledge, and everything in between.", "> Your ideas belong to you. All of your notes are stored on your own Rust server.", "## Make yourself at home", "- [ ] Create a folder for your next project", "- [ ] Write your first note", "- [ ] Invite someone to your workspace", "## Think in blocks", "Click any block to edit its Markdown. Press **Enter** to close the line, render it, and start a new block.", "Use the **+** button to add a heading, task, quote, or code block. Your changes sync live with everyone viewing this note.", "---", "Small thoughts. Meaningful connections. A place to begin."];
    let note = Note {
        id: id(),
        folder_id: folder.id.clone(),
        title: "Welcome to Minerva".into(),
        icon: "✳".into(),
        blocks: text
            .iter()
            .enumerate()
            .map(|(i, s)| Block::new(s, i))
            .collect(),
        revision: 1,
        updated_at: now(),
        favorite: true,
        trashed: false,
        archived: false,
        cover: String::new(),
        tags: vec![],
        journal: None,
        metadata: Default::default(),
        print_template: String::new(),
        record: None,
        folded: None,
    };
    Ok(Data {
        server_id: id(),
        users: vec![Account {
            user,
            password_hash: password_hash(password)?,
            two_factor: Default::default(),
            must_change_password: false,
            default_workspace: None,
            dashboard: None,
        }],
        workspaces: if local { vec![workspace] } else { vec![] },
        folders: if local { vec![folder] } else { vec![] },
        notes: if local { vec![Arc::new(note)] } else { vec![] },
        local,
        ..Default::default()
    })
}
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, axum::Json(json!({"error":self.1}))).into_response()
    }
}
fn bad(s: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, s.into())
}
/// A message with values: English with `{name}` filled in from `values`. The
/// app finds it by its template in the catalog and shows it translated.
fn badv(template: &str, values: &[(&str, &str)]) -> ApiError {
    bad(&i18n::fill(template, values))
}
fn forbidden() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "You do not have permission for this action".into(),
    )
}
fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Not found".into())
}
fn internal() -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Storage operation failed".into(),
    )
}
fn field(v: &Value, key: &str) -> Result<String, ApiError> {
    let s = v[key]
        .as_str()
        .ok_or_else(|| badv("Missing {key}", &[("key", key)]))?
        .trim();
    if s.is_empty() || s.len() > 200 {
        return Err(bad("Names must contain 1–200 characters"));
    }
    Ok(s.into())
}
fn role(d: &Data, u: &User, folder_id: &str) -> Option<Role> {
    if u.admin && !d.local {
        return None;
    }
    let f = d.folders.iter().find(|f| f.id == folder_id)?;
    if !tokens::within(u, &f.workspace_id) {
        return None;
    }
    if let Some(r) = replication::replica(d, &f.workspace_id) {
        return replication::replica_role(d, r, folder_id);
    }
    // Administrators have no access to other people's workspaces: like
    // everyone, they see what they own and what is shared with them.
    if d.workspaces
        .iter()
        .any(|w| w.id == f.workspace_id && w.owner_id == u.id)
    {
        return Some(Role::Editor);
    }
    // The higher of workspace access and a folder grant.
    let workspace = d
        .workspace_grants
        .iter()
        .find(|g| g.workspace_id == f.workspace_id && g.user_id == u.id)
        .map(|g| g.role.clone());
    // A folder grant also covers the folder's subfolders.
    let folder = ancestors(d, folder_id)
        .filter_map(|fid| {
            d.grants
                .iter()
                .find(|g| g.folder_id == fid && g.user_id == u.id)
                .map(|g| g.role.clone())
        })
        .max_by_key(|r| *r == Role::Editor);
    match (workspace, folder) {
        (Some(Role::Editor), _) | (_, Some(Role::Editor)) => Some(Role::Editor),
        (w, f) => w.or(f),
    }
}
/// The folder and its parents, innermost first. Depth is bounded so damaged data
/// cannot loop.
fn ancestors<'a>(d: &'a Data, folder_id: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    std::iter::successors(Some(folder_id), move |id| {
        d.folders
            .iter()
            .find(|f| f.id == *id)
            .and_then(|f| f.parent_id.as_deref())
    })
    .take(32)
}
/// Checks a new parent for a folder: same workspace, and not the folder itself or
/// one of its descendants.
fn valid_parent(
    d: &Data,
    workspace_id: &str,
    parent: &str,
    moving: Option<&str>,
) -> Result<String, ApiError> {
    let p = d
        .folders
        .iter()
        .find(|f| f.id == parent)
        .ok_or_else(missing)?;
    if p.workspace_id != workspace_id {
        return Err(bad("Choose a folder in the same workspace"));
    }
    if let Some(moving) = moving {
        if ancestors(d, parent).any(|id| id == moving) {
            return Err(bad("A folder cannot move into itself"));
        }
    }
    if ancestors(d, parent).count() >= 8 {
        return Err(bad("Folders can be nested up to 8 levels"));
    }
    Ok(parent.to_string())
}
/// Normalizes a metadata key: `Client Name` becomes `client_name`.
pub(crate) fn metadata_key(raw: &str) -> String {
    let mut key = String::new();
    for c in raw.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else if !key.is_empty() && !key.ends_with('_') {
            key.push('_');
        }
    }
    key.trim_end_matches('_').chars().take(40).collect()
}
/// Up to 50 `{key: value}` pairs; keys are normalized, values are plain text.
fn normalize_metadata(v: &Value) -> Result<std::collections::BTreeMap<String, String>, ApiError> {
    let Some(fields) = v.as_object() else {
        return if v.is_null() {
            Ok(Default::default())
        } else {
            Err(bad("Metadata must be an object"))
        };
    };
    if fields.len() > 50 {
        return Err(bad("A note can have up to 50 metadata fields"));
    }
    let mut out = std::collections::BTreeMap::new();
    for (raw, value) in fields {
        let key = metadata_key(raw);
        if key.is_empty() || !key.starts_with(|c: char| c.is_ascii_lowercase()) {
            return Err(badv(
                "“{raw}” is not a valid field name; start with a letter",
                &[("raw", raw)],
            ));
        }
        let value = match value {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => continue,
            _ => return Err(bad("Metadata values must be text")),
        };
        if value.chars().count() > 4000 {
            return Err(badv("“{key}” is too long", &[("key", &key)]));
        }
        if !value.is_empty() {
            out.insert(key, value);
        }
    }
    Ok(out)
}
fn access(d: &Data, u: &User, f: &str, write: bool) -> Result<(), ApiError> {
    match role(d, u, f) {
        Some(Role::Editor) => Ok(()),
        Some(Role::Viewer) if !write => Ok(()),
        _ => Err(forbidden()),
    }
}
/// Effective access to one note: its folder role, raised by a direct member share.
fn note_role(d: &Data, u: &User, n: &Note) -> Option<Role> {
    if u.admin && !d.local {
        return None;
    }
    if !tokens::note_within(d, u, n) {
        return None;
    }
    let folder = role(d, u, &n.folder_id);
    match (folder, sharing::member_role(d, &u.id, n)) {
        (_, Some(Role::Editor)) => Some(Role::Editor),
        (Some(r), _) => Some(r),
        (None, shared) => shared,
    }
}
fn note_access(d: &Data, u: &User, n: &Note, write: bool) -> Result<(), ApiError> {
    match note_role(d, u, n) {
        Some(Role::Editor) => Ok(()),
        Some(Role::Viewer) if !write => Ok(()),
        _ => Err(forbidden()),
    }
}
/// Covers are a preset gradient, an uploaded attachment, or an HTTPS image URL.
fn valid_cover(cover: &str) -> bool {
    cover.is_empty()
        || cover
            .strip_prefix("gradient:")
            .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8"))
        || render::attachment(cover).is_some_and(|(_, absolute)| !absolute)
        || (cover.len() <= 500
            && url::Url::parse(cover)
                .is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some())
            && !cover.contains(['"', '<', '>', ' ', '\'']))
}
/// Up to 20 unique normalized tags.
fn normalize_tags(tags: &[String]) -> Result<Vec<String>, ApiError> {
    if tags.len() > 20 {
        return Err(bad("A note can have up to 20 tags"));
    }
    let mut normalized: Vec<String> = vec![];
    for tag in tags {
        let tag = render::normalize_tag(tag)
            .ok_or_else(|| bad("Tags use letters, digits, -, _ and /, up to 40 characters"))?;
        if !normalized.contains(&tag) {
            normalized.push(tag);
        }
    }
    Ok(normalized)
}
/// Keeps `note` as an earlier version of itself; the newest
/// `storage::KEPT_VERSIONS` are kept.
fn keep_version(d: &mut Data, note: Arc<Note>) {
    d.versioned_at.insert(note.id.clone(), note.updated_at);
    d.history.entry(note.id.clone()).or_default().push(note);
}
/// `inline_tags`, from the cache while the note has not changed. A cached entry
/// holds a weak reference, so its note's address cannot be reused while cached.
fn cached_inline_tags(d: &Data, n: &Arc<Note>) -> Vec<String> {
    let mut cache = d.tag_cache.0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((note, tags)) = cache.get(&n.id) {
        if std::ptr::eq(note.as_ptr(), Arc::as_ptr(n)) {
            return tags.clone();
        }
    }
    if cache.len() > 2 * d.notes.len() + 64 {
        cache.retain(|_, (note, _)| note.strong_count() > 0);
    }
    let tags = inline_tags(n);
    cache.insert(n.id.clone(), (Arc::downgrade(n), tags.clone()));
    tags
}
/// A note's details for lists: everything but its blocks.
fn note_summary(n: &Note) -> Value {
    let mut v = json!({
        "id": n.id,
        "folder_id": n.folder_id,
        "title": n.title,
        "icon": n.icon,
        "revision": n.revision,
        "updated_at": n.updated_at,
        "favorite": n.favorite,
        "trashed": n.trashed,
        "archived": n.archived,
        "cover": n.cover,
        "tags": n.tags,
        "journal": n.journal,
    });
    if !n.metadata.is_empty() {
        v["metadata"] = json!(n.metadata);
    }
    if !n.print_template.is_empty() {
        v["print_template"] = json!(n.print_template);
    }
    if let Some(record) = &n.record {
        v["record"] = json!(record);
    }
    v
}
fn inline_tags(n: &Note) -> Vec<String> {
    let mut tags: Vec<String> = vec![];
    for b in &n.blocks {
        for tag in render::inline_tags(&b.markdown) {
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }
    tags
}
/// Commits a modified snapshot of the data to SQLite when it changed, with its
/// change-log rows.
fn persist(s: &mut Store, mut d: Data) -> Result<(), ApiError> {
    let upstream = std::mem::take(&mut d.upstream);
    let changes = replication::track(&s.data, &mut d)?;
    let tx = s.db.unchecked_transaction().map_err(|_| internal())?;
    let (changed, saved) = storage::write(&tx, &s.data, &mut d, &s.saved)?;
    if changed {
        // Attachments of permanently deleted notes go in the same transaction.
        let kept: HashSet<&str> = d.notes.iter().map(|n| n.id.as_str()).collect();
        let removed: Vec<&str> = s
            .data
            .notes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| !kept.contains(id))
            .collect();
        for id in removed {
            search::forget_files_of(&tx, id).map_err(|_| internal())?;
            tx.execute("DELETE FROM attachments WHERE note_id=?1", [id])
                .map_err(|_| internal())?;
        }
        replication::log(&tx, &changes, upstream).map_err(|_| internal())?;
        // A deleted account's private data goes with it.
        let users: HashSet<&str> = d.users.iter().map(|a| a.user.id.as_str()).collect();
        for gone in s
            .data
            .users
            .iter()
            .filter(|a| !users.contains(a.user.id.as_str()))
        {
            for table in [
                "ai_chats",
                "flashcard_decks",
                "flashcard_media",
                "flashcard_progress",
                "flashcard_reviews",
                "flashcard_settings",
                "graph_positions",
                "plugin_jobs",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE user_id=?1"),
                    [&gone.user.id],
                )
                .map_err(|_| internal())?;
            }
        }
        // A deleted workspace's shared flashcard decks go with it.
        let workspaces: HashSet<&str> = d.workspaces.iter().map(|w| w.id.as_str()).collect();
        for gone in s
            .data
            .workspaces
            .iter()
            .filter(|w| !workspaces.contains(w.id.as_str()))
        {
            let owner = format!("workspace:{}", gone.id);
            for sql in [
                "DELETE FROM flashcard_progress WHERE deck_id IN (SELECT id FROM flashcard_decks WHERE user_id=?1)",
                "DELETE FROM flashcard_media WHERE user_id=?1",
                "DELETE FROM flashcard_decks WHERE user_id=?1",
            ] {
                tx.execute(sql, [&owner]).map_err(|_| internal())?;
            }
            tx.execute(
                "DELETE FROM graph_positions WHERE workspace_id=?1",
                [&gone.id],
            )
            .map_err(|_| internal())?;
        }
        tx.commit().map_err(|_| internal())?;
        s.saved = saved;
    }
    d.history.clear();
    let changed = live::changed_notes(&s.data, &d);
    s.data = d;
    for id in changed {
        // No page listening is fine.
        let _ = s.events.send(id);
    }
    Ok(())
}
fn admin(u: &User) -> Result<(), ApiError> {
    if u.admin {
        Ok(())
    } else {
        Err(forbidden())
    }
}
fn owns(d: &Data, u: &User, w: &str) -> bool {
    if !tokens::within(u, w) {
        return false;
    }
    if u.admin && !d.local {
        return false;
    }
    if let Some(r) = replication::replica(d, w) {
        return d.upstream || (r.owner && !r.revoked);
    }
    d.workspaces.iter().any(|v| v.id == w && v.owner_id == u.id)
}
/// The owner, and members who can edit the whole workspace: they add folders
/// and set up its Journal and Templates.
fn edits_workspace(d: &Data, u: &User, w: &str) -> bool {
    if !tokens::within(u, w) {
        return false;
    }
    if u.admin && !d.local {
        return false;
    }
    owns(d, u, w)
        || (replication::replica(d, w).is_none()
            && d.workspace_grants
                .iter()
                .any(|g| g.workspace_id == w && g.user_id == u.id && g.role == Role::Editor))
}
fn authenticate(d: &Data, headers: &HeaderMap) -> Result<User, ApiError> {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookie = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';')
                .find_map(|p| p.trim().strip_prefix("minerva_session="))
        });
    if let Some(token) = bearer.filter(|t| t.starts_with(tokens::PREFIX)) {
        return tokens::user(d, token).ok_or(ApiError(
            StatusCode::UNAUTHORIZED,
            "This API token is not valid".into(),
        ));
    }
    let session = bearer
        .or(cookie)
        .and_then(|t| d.sessions.get(&hash_token(t)))
        .filter(|s| s.expires > now());
    session
        .and_then(|s| {
            d.users
                .iter()
                .find(|a| a.user.id == s.user_id && !a.user.disabled)
        })
        .map(|a| a.user.clone())
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "Please sign in".into()))
}
/// The session token presented as a bearer token or session cookie.
fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("cookie")
                .and_then(|h| h.to_str().ok())
                .and_then(|h| {
                    h.split(';')
                        .find_map(|p| p.trim().strip_prefix("minerva_session="))
                })
        })
}
/// Creates a seven-day session and the cookie that carries it.
fn start_session(d: &mut Data, user: User, secure: &str) -> (Value, Option<String>) {
    let token = format!("{}{}", id(), id());
    d.sessions.retain(|_, s| s.expires > now());
    d.sessions.insert(
        hash_token(&token),
        Session {
            user_id: user.id.clone(),
            expires: now() + 86400 * 7,
        },
    );
    (
        json!({"user":user,"token":token}),
        Some(format!(
            "minerva_session={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800{secure}"
        )),
    )
}
fn note_view(n: &Note) -> Value {
    let mut v = serde_json::to_value(n).unwrap();
    v["inline_tags"] = json!(inline_tags(n));
    v["rendered"] = json!(n
        .blocks
        .iter()
        .map(|b| render(&b.markdown))
        .collect::<Vec<_>>());
    v
}

/// Only the reduced editor may be embedded, and only by explicitly trusted origins.
/// Other sites allowed to show Minerva's pages in a frame
/// (`MINERVA_EMBED_ORIGINS`), space-separated for `frame-ancestors`.
pub(crate) fn embed_origins() -> String {
    std::env::var("MINERVA_EMBED_ORIGINS")
        .unwrap_or_default()
        .split(',')
        .filter_map(|raw| {
            let url = url::Url::parse(raw.trim()).ok()?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return None;
            }
            Some(url.origin().ascii_serialization())
        })
        .collect::<Vec<_>>()
        .join(" ")
}
async fn editor_pane() -> Response {
    let origins = embed_origins();
    // Video frames: YouTube, or the desktop app's player page on 127.0.0.1.
    let policy = format!("default-src 'self'; script-src 'self' https://www.youtube.com https://s.ytimg.com; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: http: https:; media-src 'self' blob: http: https:; connect-src 'self'; frame-src https://www.youtube.com https://www.youtube-nocookie.com http://127.0.0.1:* 'self'; frame-ancestors 'self' {origins}; base-uri 'none'; form-action 'self'");
    (
        [("content-security-policy", policy)],
        axum::response::Html(include_str!("../../../web/index.html")),
    )
        .into_response()
}

/// The service worker, named after this build's web app: a new release gives
/// a new worker, which replaces the app kept on devices.
fn service_worker() -> &'static str {
    static WORKER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    WORKER.get_or_init(|| {
        let mut hash: u64 = 0xcbf29ce484222325;
        for part in [
            include_str!("../../../web/dist/app.js"),
            include_str!("../../../web/style.css"),
            include_str!("../../../web/index.html"),
            include_str!("../../../web/capture.html"),
            include_str!("../../../web/dist/capture.js"),
        ] {
            for b in part.bytes() {
                hash = (hash ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
        include_str!("../../../web/sw.js").replace("__VERSION__", &format!("{hash:016x}"))
    })
}

pub fn app(store: Shared) -> Router {
    mail::spawn(store.clone());
    search::spawn(store.clone());
    push::spawn(store.clone());
    Router::new()
        .route("/", get(|| async { axum::response::Html(include_str!("../../../web/index.html")) }))
        .route("/pane", get(editor_pane))
        .route("/app.js", get(|| async { ([("content-type","text/javascript; charset=utf-8")], include_str!("../../../web/dist/app.js")) }))
        .route("/style.css", get(|| async { ([("content-type","text/css; charset=utf-8")], include_str!("../../../web/style.css")) }))
        .route("/share", get(|| async { ([("x-robots-tag","noindex"),("referrer-policy","no-referrer")], axum::response::Html(include_str!("../../../web/index.html"))) }))
        // Emoji names and search tags for the pickers, loaded when one opens.
        .route("/emoji.json", get(|| async { ([("content-type","application/json"),("cache-control","public, max-age=86400")], include_str!("../../../web/dist/emoji.json")) }))
        .route("/mermaid.js", get(|| async { ([("content-type","text/javascript; charset=utf-8"),("cache-control","public, max-age=86400")], include_str!("../../../web/dist/mermaid.js")) }))
        .route("/export/{note_id}", get(export::export))
        .route("/export.js", get(|| async { ([("content-type","text/javascript; charset=utf-8")], export::EXPORT_JS) }))
        .route("/favicon.ico", get(|| async { ([("content-type","image/x-icon"),("cache-control","public, max-age=86400")], include_bytes!("../../../web/icons/favicon.ico").as_slice()) }))
        .route("/favicon.png", get(|| async { ([("content-type","image/png"),("cache-control","public, max-age=86400")], include_bytes!("../../../web/icons/favicon-32.png").as_slice()) }))
        // The installable app: its description, icons, worker and quick capture.
        .route("/manifest.webmanifest", get(|| async { ([("content-type","application/manifest+json"),("cache-control","no-cache")], include_str!("../../../web/manifest.webmanifest")) }))
        .route("/sw.js", get(|| async { ([("content-type","text/javascript; charset=utf-8"),("cache-control","no-cache")], service_worker()) }))
        .route("/capture", get(|| async { axum::response::Html(include_str!("../../../web/capture.html")) }))
        // Shares are taken by the app's service worker; without one, the quick note page.
        .route("/capture/share", axum::routing::post(|| async { axum::response::Redirect::to("/capture") }))
        .route("/capture.js", get(|| async { ([("content-type","text/javascript; charset=utf-8")], include_str!("../../../web/dist/capture.js")) }))
        .route("/icons/icon-192.png", get(|| async { ([("content-type","image/png"),("cache-control","public, max-age=86400")], include_bytes!("../../../web/icons/icon-192.png").as_slice()) }))
        .route("/icons/icon-512.png", get(|| async { ([("content-type","image/png"),("cache-control","public, max-age=86400")], include_bytes!("../../../web/icons/icon-512.png").as_slice()) }))
        .route("/icons/maskable-512.png", get(|| async { ([("content-type","image/png"),("cache-control","public, max-age=86400")], include_bytes!("../../../web/icons/maskable-512.png").as_slice()) }))
        .route("/apple-touch-icon.png", get(|| async { ([("content-type","image/png"),("cache-control","public, max-age=86400")], include_bytes!("../../../web/icons/apple-touch-icon.png").as_slice()) }))
        .route("/plugins/{*path}", get(plugins::serve))
        .route("/calendar/{file}", get(calendar::feed))
        .route("/public.css", get(|| async { ([("content-type","text/css; charset=utf-8")], sharing::PUBLIC_CSS) }))
        .route("/shared", get(sharing::public))
        .route("/shared/", get(sharing::public))
        .route("/shared/{*path}", get(sharing::public))
        .route("/p", get(sharing::moved))
        .route("/p/", get(sharing::moved))
        .route("/p/{*path}", get(sharing::moved))
        .route("/preview/{note_id}", get(sharing::preview))
        .route("/api/v1/shared/link-preview", get(previews::guest_endpoint))
        .route("/api/v1/shared/live", get(live::guest_endpoint))
        .route("/api/v1/shared/{*path}", any(sharing::shared).layer(DefaultBodyLimit::max(attachments::FILE_LIMIT + 1024)))
        .route("/api/v1/bootstrap/notes", get(bootstrap::endpoint))
        .route("/api/v1/live", get(live::endpoint))
        .route("/api/v1/live/wait", get(live::wait))
        .route("/api/v1/link-preview", get(previews::endpoint))
        .route("/api/v1/import/notion", axum::routing::post(import::notion_endpoint))
        .route("/api/v1/import", axum::routing::post(import::endpoint).layer(DefaultBodyLimit::max(import::IMPORT_LIMIT)))
        .route("/api/v1/admin/overview", get(administration::overview))
        .route("/api/v1/admin/storage", any(attachments::storage))
        .route("/api/v1/admin/storage/cleanup", any(attachments::storage))
        .route("/api/v1/notes/{note_id}/attachments", any(attachments::endpoint).layer(DefaultBodyLimit::max(attachments::FILE_LIMIT + 1024)))
        .route("/api/v1/notes/{note_id}/attachments/{attachment_id}", any(attachments::endpoint).layer(DefaultBodyLimit::max(attachments::FILE_LIMIT + 1024)))
        .route("/api/v1/attachments/{attachment_id}", get(attachments::endpoint))
        .route("/api/v1/attachments/{attachment_id}/preview", get(attachments::endpoint))
        // Anki packages can be large; their media is not kept.
        .route("/api/v1/flashcards/import", any(flashcards::import).layer(DefaultBodyLimit::max(100 * 1024 * 1024)))
        // A card's picture or sound (up to 20 MB).
        .route("/api/v1/flashcards/decks/{deck}/media", axum::routing::post(flashcards::upload_media).layer(DefaultBodyLimit::max(21 * 1024 * 1024)))
        .route("/api/v1/flashcards/{*path}", any(flashcards::endpoint))
        // Questions may carry images and documents (see ai.rs `uploads`).
        .route("/api/v1/ai/{*path}", any(ai::endpoint).layer(DefaultBodyLimit::max(30 * 1024 * 1024)))
        .route("/api/v1/server-plugins/{*path}", any(server_plugins::endpoint))
        .route("/api/v1/{*path}", any(api))
        // attach_file carries files of up to 10 MB, base64-encoded.
        // OAuth for MCP clients (off unless MINERVA_MCP_OAUTH=1).
        .route("/.well-known/oauth-protected-resource/api/workspaces/{workspace_id}/mcp", get(oauth::resource_metadata))
        .route("/.well-known/oauth-authorization-server", get(oauth::server_metadata))
        .route("/oauth/register", axum::routing::post(oauth::register))
        .route("/oauth/authorize", get(oauth::authorize).post(oauth::decide))
        .route("/oauth/token", axum::routing::post(oauth::token))
        .route("/api/workspaces/{workspace_id}/mcp", any(mcp::endpoint).layer(DefaultBodyLimit::max(15 * 1024 * 1024)))
        // Public sites (`/blog/...`); paths the server uses are reserved and win.
        .route("/{site}", get(sharing::public))
        .route("/{site}/", get(sharing::public))
        .route("/{site}/{*path}", get(sharing::public))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(axum::middleware::map_response(|mut r: Response| async move {
            let h = r.headers_mut();
            h.insert("x-content-type-options", "nosniff".parse().unwrap());
            h.insert("referrer-policy", "same-origin".parse().unwrap());
            h.entry("content-security-policy").or_insert("default-src 'self'; script-src 'self' https://www.youtube.com https://s.ytimg.com; style-src 'self' 'unsafe-inline'; img-src 'self' data: http: https:; media-src 'self' blob: http: https:; connect-src 'self'; frame-src 'self' https: http:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
            h.entry("cache-control").or_insert("no-store".parse().unwrap()); r
        }))
        .layer(axum::middleware::from_fn_with_state(store.clone(), administration::restrict))
        // Tokens: their last use, and read-only ones kept to reading.
        .layer(axum::middleware::from_fn_with_state(store.clone(), tokens::guard))
        .with_state(store)
}
async fn api(
    State(store): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    tokio::task::spawn_blocking(move || {
        let mut s = store.lock().map_err(|_| internal())?;
        let mut d = s.data.clone();
        let write = method != Method::GET;
        if write
            && headers
                .get("x-minerva-client")
                .and_then(|h| h.to_str().ok())
                != Some("1")
        {
            return Err(bad("X-Minerva-Client: 1 is required"));
        }
        let mut value: Value = if body.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&body).map_err(|_| bad("Invalid JSON"))?
        };
        let path = uri.path().trim_start_matches("/api/v1/");
        if method == Method::GET && path == "bootstrap" {
            value = json!({"omit_notes": url::form_urlencoded::parse(
                uri.query().unwrap_or("").as_bytes()
            ).any(|(k, v)| k == "notes" && v == "0")});
        }
        let outcome = if path.starts_with("sync/") || path.starts_with("local/") {
            authenticate(&d, &headers).and_then(|u| {
                let query = uri.query().unwrap_or("");
                replication::endpoint(&s.db, &mut d, &u, method.as_str(), path, query, value)
                    .map(|v| (v, None))
            })
        } else {
            let store = &mut *s;
            dispatch(
                &store.db,
                &mut d,
                &mut store.attempts,
                method.as_str(),
                path,
                &headers,
                value,
            )
        };
        // Failed second-factor attempts must still count, even though the
        // request's other changes are discarded.
        s.data.login_challenges = d.login_challenges.clone();
        let (result, cookie) = outcome?;
        // POSTs that only read (searching, rendering, embeds) change nothing,
        // so they skip saving; the editor renders a block at every change.
        let reads_only = matches!(path, "search" | "render" | "embeds");
        if write && !reads_only {
            let actor = authenticate(&d, &headers).ok();
            mail::assignments(&s.data, &mut d, actor.as_ref());
            persist(&mut s, d)?;
        }
        let mut response = axum::Json(result).into_response();
        if let Some(cookie) = cookie {
            response
                .headers_mut()
                .insert("set-cookie", cookie.parse().map_err(|_| internal())?);
        }
        Ok(response)
    })
    .await
    .map_err(|_| internal())?
}
/// Sign-in throttling per account, so one attacker cannot lock everyone out, plus
/// a generous server-wide ceiling against credential stuffing.
#[derive(Default)]
struct Limiter {
    global: Vec<u64>,
    accounts: HashMap<String, Vec<u64>>,
}
impl Limiter {
    const PER_ACCOUNT: usize = 10;
    const SERVER: usize = 300;
    fn check(&mut self, key: &str) -> Result<(), ApiError> {
        let at = now();
        self.global.retain(|t| *t + 60 > at);
        self.accounts.retain(|_, times| {
            times.retain(|t| *t + 60 > at);
            !times.is_empty()
        });
        let account = self.accounts.get(key).map_or(0, Vec::len);
        if account >= Self::PER_ACCOUNT || self.global.len() >= Self::SERVER {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many sign-in attempts. Try again in a minute.".into(),
            ));
        }
        self.global.push(at);
        self.accounts.entry(key.to_string()).or_default().push(at);
        Ok(())
    }
}
/// A real Argon2 hash to verify against when the account does not exist, so
/// response time does not reveal which email addresses have accounts.
fn dummy_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| password_hash("not-a-real-account-password").unwrap_or_default())
}
fn dispatch(
    db: &Connection,
    d: &mut Data,
    attempts: &mut Limiter,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    v: Value,
) -> Result<(Value, Option<String>), ApiError> {
    let secure = if std::env::var("MINERVA_SECURE_COOKIE").as_deref() == Ok("1") {
        "; Secure"
    } else {
        ""
    };
    if path == "health" && method == "GET" {
        return Ok((
            json!({"status":"ok","api_version":1,"server_id":d.server_id,"sync_protocol":1,"mode":if d.local {"local"} else {"server"},"mail":mail::enabled()}),
            None,
        ));
    }
    if path == "login" && method == "POST" {
        let email = field(&v, "email")?.to_lowercase();
        attempts.check(&format!("login:{email}"))?;
        let password = v["password"]
            .as_str()
            .filter(|s| s.len() <= 256)
            .ok_or_else(|| bad("Invalid password"))?;
        let Some(a) = d
            .users
            .iter()
            .find(|a| a.user.email == email && !a.user.disabled)
        else {
            if let Ok(hash) = PasswordHash::new(dummy_hash()) {
                let _ = Argon2::default().verify_password(password.as_bytes(), &hash);
            }
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "Invalid email or password".into(),
            ));
        };
        if Argon2::default()
            .verify_password(
                password.as_bytes(),
                &PasswordHash::new(&a.password_hash).map_err(|_| internal())?,
            )
            .is_err()
        {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "Invalid email or password".into(),
            ));
        }
        let user = a.user.clone();
        // With 2FA, a correct password only earns a challenge for the second step.
        if let Some(challenge) = twofactor::challenge(d, &user.id) {
            return Ok((challenge, None));
        }
        return Ok(start_session(d, user, secure));
    }
    if path == "login/2fa" && method == "POST" {
        let challenge = v["challenge"].as_str().unwrap_or("");
        attempts.check(&format!("2fa:{}", hash_token(challenge)))?;
        let user = twofactor::complete(d, &v)?;
        return Ok(start_session(d, user, secure));
    }
    if path.starts_with("password-reset") && method == "POST" {
        attempts.check(&format!("reset:{}", v["email"].as_str().unwrap_or("token")))?;
        if let Some(result) = mail::reset_route(d, path, &v)? {
            return Ok((result, None));
        }
    }
    let u = authenticate(d, headers)?;
    let parts: Vec<&str> = path.split('/').collect();
    if tokens::used(headers) && tokens::reserved(&parts) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "API tokens cannot manage the account; sign in instead".into(),
        ));
    }
    if let ("GET", ["workspaces", wid, "graph"]) = (method, parts.as_slice()) {
        return Ok((graph::with_positions(db, d, &u, wid)?, None));
    }
    if let ("PUT", ["workspaces", wid, "graph", "positions"]) = (method, parts.as_slice()) {
        return Ok((graph::save_positions(db, d, &u, wid, &v)?, None));
    }
    if let ["push", rest @ ..] = parts.as_slice() {
        return Ok((push::route(d, &u, method, rest, &v)?, None));
    }
    if let ["calendar-feed", rest @ ..] = parts.as_slice() {
        let workspace = match rest {
            [] => None,
            [w] => Some(*w),
            _ => return Err(missing()),
        };
        return Ok((
            calendar::route(d, &u, method, workspace, &v, headers)?,
            None,
        ));
    }
    if let ("POST", ["embeds"]) = (method, parts.as_slice()) {
        return Ok((embeds::route(d, &u, &v)?, None));
    }
    if parts.first() == Some(&"api-tokens") {
        return Ok((tokens::route(db, d, &u, method, &parts, &v)?, None));
    }
    if let Some(result) = administration::route(d, &u, method, &parts, &v)? {
        return Ok((result, None));
    }

    if method == "GET" {
        if let Some(result) = mail::people_route(db, d, &u, &parts)? {
            return Ok((result, None));
        }
    }
    if parts.first() == Some(&"reminders") {
        return Ok((reminders::route(d, &u, method, &parts, &v)?, None));
    }
    if parts.first() == Some(&"databases") {
        return Ok((databases::route(d, &u, method, &parts, v)?, None));
    }
    if let Some(result) = comments::route(db, d, &u, method, &parts, &v)? {
        return Ok((result, None));
    }
    if method == "DELETE"
        && matches!(parts.as_slice(), [_, _])
        && matches!(parts[0], "notes" | "folders" | "workspaces")
    {
        return Ok((removal::route(d, &u, &parts)?, None));
    }
    if parts.first() == Some(&"sites") {
        return Ok((sites::route(d, &u, method, &parts, &v)?, None));
    }
    if parts.first() == Some(&"mcp-credentials") {
        return Ok((mcp::credentials(db, d, &u, method, &parts, v)?, None));
    }
    if parts.first() == Some(&"account") {
        let current = session_token(headers).map(String::from);
        if let Some(result) = twofactor::route(d, &u, method, &parts, &v, current)? {
            return Ok((result, None));
        }
    }
    if matches!(parts.first(), Some(&"journal" | &"templates")) || parts.get(2) == Some(&"template")
    {
        if let Some(result) = journal::route(d, &u, method, &parts, &v)? {
            return Ok((result, None));
        }
    }
    if parts.first() == Some(&"sharing") || matches!(parts.get(2), Some(&"shares" | &"publication"))
    {
        if let Some(result) = sharing::route(d, &u, method, &parts, &v)? {
            return Ok((result, None));
        }
    }
    let result = match (method, parts.as_slice()) {
        ("POST", ["notes", nid, "sync"]) => {
            let n = d.notes.iter().find(|n| n.id == *nid).ok_or_else(missing)?;
            let actor = collaboration::Actor {
                id: u.id.clone(),
                name: u.name.clone(),
                role: note_role(d, &u, n),
                guest: false,
            };
            collaboration::sync(d, &actor, nid, v)?
        }
        ("POST", ["logout"]) => {
            let token = session_token(headers);
            if let Some(t) = token {
                d.sessions.remove(&hash_token(t));
            }
            return Ok((
                json!({"ok":true}),
                Some(format!(
                    "minerva_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}"
                )),
            ));
        }
        ("GET", ["bootstrap"]) => {
            let folders: Vec<_> = d
                .folders
                .iter()
                .filter(|f| role(d, &u, &f.id).is_some())
                .collect();
            let workspaces: Vec<_> = d
                .workspaces
                .iter()
                .filter(|w| tokens::within(&u, &w.id))
                .filter(|w| {
                    owns(d, &u, &w.id)
                        || (!u.admin
                            && d.workspace_grants
                                .iter()
                                .any(|g| g.workspace_id == w.id && g.user_id == u.id))
                        || folders.iter().any(|f| f.workspace_id == w.id)
                })
                .collect();
            let readable: HashSet<&str> = folders.iter().map(|f| f.id.as_str()).collect();
            let notes: Vec<_> = d
                .notes
                .iter()
                .filter(|n| v["omit_notes"] != true && readable.contains(n.folder_id.as_str()))
                .map(|n| {
                    let mut v = note_summary(n);
                    v["sharing"] = sharing::summary(d, &n.id);
                    v["inline_tags"] = json!(cached_inline_tags(d, n));
                    v
                })
                .collect();
            let visible = |map: &HashMap<String, String>| -> HashMap<String, String> {
                map.iter()
                    .filter(|(ws, _)| workspaces.iter().any(|w| &&w.id == ws))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            };
            let roles: HashMap<_, _> = folders
                .iter()
                .map(|f| (&f.id, role(d, &u, &f.id)))
                .collect();
            let workspaces_view: Vec<Value> = workspaces
                .iter()
                .map(|w| {
                    let mut v = json!(w);
                    v["can_delete"] = json!(removal::can_delete_workspace(d, &u, w));
                    v["can_edit"] = json!(edits_workspace(d, &u, &w.id));
                    v["shared"] = json!(removal::shared(d, w));
                    v
                })
                .collect();
            let account = d.users.iter().find(|a| a.user.id == u.id);
            json!({"notes_paged":v["omit_notes"] == true,"server_id":d.server_id,"user":u,"must_change_password":account.is_some_and(|a| a.must_change_password),"language":d.languages.get(&u.id),"owns_workspace":d.workspaces.iter().any(|w| owns(d,&u,&w.id)),"default_workspace":account.and_then(|a| a.default_workspace.clone()).filter(|w| tokens::within(&u, w)),"workspaces":workspaces_view,"folders":folders,"notes":notes,"shared_notes":sharing::shared_with(d,&u),"journal_folders":visible(&d.journal_folders),"template_folders":visible(&d.template_folders),"journal_templates":visible(&d.journal_templates),"sites":sites::listed(d,&workspaces),"roles":roles,"databases":d.databases.iter().filter(|db|role(d,&u,&db.folder_id).is_some()).map(|db|json!({"id":db.id,"folder_id":db.folder_id,"name":db.name,"updated_at":db.updated_at,"record_count":db.rows.iter().filter(|r|!r.archived).count()})).collect::<Vec<_>>()})
        }
        ("POST", ["search"]) => {
            let query = v["query"].as_str().unwrap_or("").trim().to_lowercase();
            if query.len() > 200 {
                return Err(bad("Search query is too long"));
            }
            // `#tag` searches tags (note tags and inline tags) by prefix; anything
            // else searches titles, text and tags, best matches first.
            let workspace = v["workspace_id"].as_str();
            // Archived notes stay out, unless the archive is what is searched.
            let archive = v["archived"].as_bool().unwrap_or(false);
            // Filters: a folder with its subfolders, a tag, a date of last change.
            let folder = v["folder_id"].as_str().filter(|f| !f.is_empty());
            let within: Option<HashSet<&str>> = folder.map(|root| {
                d.folders
                    .iter()
                    .filter(|f| {
                        let mut at = Some(*f);
                        for _ in 0..32 {
                            match at {
                                Some(x) if x.id == root => return true,
                                Some(x) => {
                                    at = x
                                        .parent_id
                                        .as_deref()
                                        .and_then(|p| d.folders.iter().find(|f| f.id == p))
                                }
                                None => return false,
                            }
                        }
                        false
                    })
                    .map(|f| f.id.as_str())
                    .collect()
            });
            let in_folder = |id: &str| within.as_ref().is_none_or(|w| w.contains(id));
            let tag_filter = v["tag"]
                .as_str()
                .map(|t| t.trim_start_matches('#').to_lowercase())
                .filter(|t| !t.is_empty());
            let after = v["updated_after"].as_u64().unwrap_or(0);
            // The notes with the tag (or one under it, `tag/…`), worked out once.
            let with_tag: Option<HashSet<&str>> = tag_filter.as_deref().map(|tag| {
                let nested = format!("{tag}/");
                d.notes
                    .iter()
                    .filter(|n| {
                        n.tags
                            .iter()
                            .chain(cached_inline_tags(d, n).iter())
                            .any(|t| t == tag || t.starts_with(&nested))
                    })
                    .map(|n| n.id.as_str())
                    .collect()
            });
            let in_workspace = |folder_id: &str| {
                workspace.is_none_or(|w| {
                    d.folders
                        .iter()
                        .any(|f| f.id == folder_id && f.workspace_id == w)
                })
            };
            let keep = |n: &Note| {
                !n.trashed
                    && n.archived == archive
                    && note_role(d, &u, n).is_some()
                    && in_workspace(&n.folder_id)
                    && in_folder(&n.folder_id)
                    && n.updated_at >= after
                    && with_tag.as_ref().is_none_or(|w| w.contains(n.id.as_str()))
            };
            let filtered = folder.is_some() || tag_filter.is_some() || after > 0;
            let found = match query.strip_prefix('#') {
                Some(tag) => d
                    .notes
                    .iter()
                    .filter(|n| {
                        keep(n)
                            && n.tags
                                .iter()
                                .chain(cached_inline_tags(d, n).iter())
                                .any(|t| t.starts_with(tag))
                    })
                    .take(50)
                    .map(|n| (&**n, String::new()))
                    .collect(),
                // Filters alone list the notes they keep, latest first.
                None if query.is_empty() && filtered => {
                    let mut kept: Vec<&Note> =
                        d.notes.iter().map(|n| &**n).filter(|n| keep(n)).collect();
                    kept.sort_by_key(|n| std::cmp::Reverse(n.updated_at));
                    kept.into_iter()
                        .take(50)
                        .map(|n| (n, String::new()))
                        .collect()
                }
                None => search::matches(db, d, &query, keep, 50)?,
            };
            let mut matches: Vec<Value> = found.into_iter().map(|(n, snippet)| json!({"kind": "note", "id": n.id, "title": n.title, "icon": n.icon, "folder_id": n.folder_id, "updated_at": n.updated_at, "snippet": snippet})).collect();
            // Records and files' text, when asked for: after the notes.
            let include: Vec<&str> = v["include"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let plain = !query.is_empty() && !query.starts_with('#') && !archive;
            if plain && include.contains(&"records") && tag_filter.is_none() {
                let found = search::records(
                    db,
                    d,
                    &query,
                    |base, r| {
                        role(d, &u, &base.folder_id).is_some()
                            && in_workspace(&base.folder_id)
                            && in_folder(&base.folder_id)
                            && r.updated_at >= after
                    },
                    20,
                )?;
                matches.extend(found.into_iter().map(|(base, r, snippet)| {
                    json!({"kind": "record", "id": r.id, "database_id": base.id, "database_name": base.name, "title": databases::record_title(base, r), "folder_id": base.folder_id, "updated_at": r.updated_at, "snippet": snippet})
                }));
            }
            if plain && include.contains(&"files") {
                let found = search::files(db, d, &query, keep, 20)?;
                matches.extend(found.into_iter().map(|(n, attachment, name, snippet)| {
                    json!({"kind": "file", "id": attachment, "note_id": n.id, "note_title": n.title, "title": name, "icon": n.icon, "folder_id": n.folder_id, "updated_at": n.updated_at, "snippet": snippet})
                }));
            }
            json!(matches)
        }
        ("POST", ["workspaces"]) => {
            if u.admin && !d.local {
                return Err(forbidden());
            }
            let w = Workspace {
                id: id(),
                name: field(&v, "name")?,
                owner_id: u.id.clone(),
                personal: false,
            };
            d.folders.push(Folder {
                id: id(),
                workspace_id: w.id.clone(),
                name: "General".into(),
                parent_id: None,
            });
            d.workspaces.push(w.clone());
            json!(w)
        }
        // Renames a workspace you own; personal workspaces keep their name.
        ("PATCH", ["workspaces", wid]) => {
            if !owns(d, &u, wid) {
                return Err(forbidden());
            }
            let name = field(&v, "name")?;
            let w = d
                .workspaces
                .iter_mut()
                .find(|w| w.id == *wid)
                .ok_or_else(missing)?;
            if w.personal {
                return Err(bad("A personal workspace keeps its name"));
            }
            w.name = name;
            json!(w)
        }
        ("POST", ["folders"]) => {
            let wid = field(&v, "workspace_id")?;
            if !d.workspaces.iter().any(|w| w.id == wid) {
                return Err(missing());
            }
            if !edits_workspace(d, &u, &wid) {
                return Err(forbidden());
            }
            let parent_id = match v["parent_id"].as_str().filter(|p| !p.is_empty()) {
                Some(parent) => Some(valid_parent(d, &wid, parent, None)?),
                None => None,
            };
            let f = Folder {
                id: id(),
                workspace_id: wid,
                name: field(&v, "name")?,
                parent_id,
            };
            d.folders.push(f.clone());
            json!(f)
        }
        ("PATCH", ["folders", fid]) => {
            let f = d
                .folders
                .iter()
                .find(|f| f.id == *fid)
                .ok_or_else(missing)?;
            if !edits_workspace(d, &u, &f.workspace_id) {
                return Err(forbidden());
            }
            let name = v.get("name").map(|_| field(&v, "name")).transpose()?;
            // `parent_id`: a folder to move into, or null for the top level.
            let parent = match v.get("parent_id") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(p) => Some(Some(valid_parent(
                    d,
                    &f.workspace_id,
                    p.as_str().ok_or_else(|| bad("Invalid parent folder"))?,
                    Some(fid),
                )?)),
            };
            let f = d.folders.iter_mut().find(|f| f.id == *fid).unwrap();
            if let Some(name) = name {
                f.name = name;
            }
            if let Some(parent) = parent {
                f.parent_id = parent;
            }
            json!(f)
        }
        ("PATCH", ["notes", nid]) => {
            // Moves a note to another folder of the same workspace. Folder editors
            // only, on both ends; a share recipient cannot move a note away.
            let n = d.notes.iter().find(|n| n.id == *nid).ok_or_else(missing)?;
            access(d, &u, &n.folder_id, true)?;
            if n.record.is_some() {
                return Err(bad("A record's page stays with its database"));
            }
            let target = field(&v, "folder_id")?;
            access(d, &u, &target, true)?;
            let workspace = |id: &str| {
                d.folders
                    .iter()
                    .find(|f| f.id == id)
                    .map(|f| f.workspace_id.clone())
            };
            if workspace(&n.folder_id) != workspace(&target) {
                return Err(bad("Notes can only move within their workspace"));
            }
            let n = Arc::make_mut(d.notes.iter_mut().find(|n| n.id == *nid).unwrap());
            n.folder_id = target;
            n.updated_at = now();
            let mut view = note_view(n);
            view["role"] = json!(note_role(
                d,
                &u,
                d.notes.iter().find(|n| n.id == *nid).unwrap()
            ));
            view
        }
        ("POST", ["notes"]) => {
            let fid = field(&v, "folder_id")?;
            access(d, &u, &fid, true)?;
            let blocks = match v["markdown"].as_str() {
                Some(m) if m.len() > 200_000 => {
                    return Err(bad("At most 200,000 bytes of Markdown"))
                }
                Some(m) => mcp::markdown_blocks(m)?,
                None => vec![Block::new("", 0)],
            };
            let n = Note {
                id: id(),
                folder_id: fid,
                title: field(&v, "title")?,
                icon: "▤".into(),
                blocks,
                revision: 1,
                updated_at: now(),
                favorite: false,
                trashed: false,
                archived: false,
                cover: String::new(),
                tags: vec![],
                journal: None,
                metadata: Default::default(),
                print_template: String::new(),
                record: None,
                folded: None,
            };
            d.notes.push(Arc::new(n.clone()));
            note_view(&n)
        }
        // Folding kept in the note, for everyone (`shared`), or on each device.
        ("PUT", ["notes", nid, "folding"]) => {
            let n = d.notes.iter().find(|n| n.id == *nid).ok_or_else(missing)?;
            note_access(d, &u, n, true)?;
            let folded = match v["shared"].as_bool() {
                Some(true) => {
                    let list: Vec<String> = v["folded"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();
                    // Only blocks of the note, once each.
                    let mut kept: Vec<String> = vec![];
                    for id in list {
                        if n.blocks.iter().any(|b| b.id == id) && !kept.contains(&id) {
                            kept.push(id);
                        }
                    }
                    Some(kept)
                }
                Some(false) => None,
                None => return Err(bad("Say whether folding is shared")),
            };
            let mut changed = Note::clone(n);
            if changed.folded != folded {
                changed.folded = folded;
                changed.updated_at = now();
                *d.notes.iter_mut().find(|n| n.id == *nid).unwrap() = Arc::new(changed.clone());
            }
            json!({"folded": changed.folded})
        }
        // One block, changed where it is shown (an embed): refused when it changed
        // since the caller read it (`base`), so no one's edit is lost.
        ("PUT", ["notes", nid, "blocks", bid]) => {
            let markdown = v["markdown"]
                .as_str()
                .ok_or_else(|| bad("markdown is required"))?
                .to_string();
            if markdown.len() > 100_000 {
                return Err(bad("Block is too large"));
            }
            let base = v["base"].as_str();
            let note = mcp::edit_blocks(d, &u, nid, |blocks| {
                let b = blocks
                    .iter_mut()
                    .find(|b| b.id == *bid)
                    .ok_or_else(|| bad("No such block"))?;
                if base.is_some_and(|base| base != b.markdown) {
                    return Err(ApiError(
                        StatusCode::CONFLICT,
                        "This block changed meanwhile. Open the note to see the latest text."
                            .into(),
                    ));
                }
                b.markdown = markdown;
                Ok(())
            })?;
            note_view(&note)
        }
        ("POST", ["notes", nid, "append"]) => {
            let after = v["after_block_id"].as_str();
            let markdown = v["markdown"]
                .as_str()
                .ok_or_else(|| bad("markdown is required"))?;
            let (note, block_ids) = mcp::append_markdown(d, &u, nid, markdown, after)?;
            let mut view = note_view(&note);
            view["added_block_ids"] = json!(block_ids);
            view
        }
        ("GET", ["notes", nid]) => {
            let n = d.notes.iter().find(|n| n.id == *nid).ok_or_else(missing)?;
            note_access(d, &u, n, false)?;
            let mut view = note_view(n);
            view["role"] = json!(note_role(d, &u, n));
            view["sharing"] = sharing::summary(d, &n.id);
            let target = format!("#note/{}", n.id);
            view["backlinks"] = json!(d
                .notes
                .iter()
                .filter(|o| o.id != n.id && !o.trashed && note_role(d, &u, o).is_some())
                .filter(|o| o.blocks.iter().any(|b| b.markdown.contains(&target)))
                .map(|o| json!({"id":o.id,"title":o.title,"icon":o.icon}))
                .collect::<Vec<_>>());
            view
        }
        ("PUT", ["notes", nid]) => {
            let n = d.notes.iter().find(|n| n.id == *nid).ok_or_else(missing)?;
            note_access(d, &u, n, true)?;
            let mut save: SaveNote = serde_json::from_value(v).map_err(|_| bad("Invalid note"))?;
            // A record's page keeps its record's name.
            if let Some(name) = record_pages::saved_title(d, n) {
                save.title = name;
            }
            if save.revision != n.revision {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "This note changed on another client. Export your draft before reloading."
                        .into(),
                ));
            }
            if save.title.trim().is_empty()
                || save.title.len() > 200
                || save.icon.len() > 24
                || save.blocks.len() > 2000
            {
                return Err(bad("Invalid title, icon, or too many blocks"));
            }
            let mut ids = HashSet::new();
            for b in &save.blocks {
                if b.markdown.len() > 100_000
                    || b.id.len() > 80
                    || b.id.is_empty()
                    || !ids.insert(&b.id)
                    || !b.x.is_finite()
                    || !b.y.is_finite()
                    || b.x.abs() > 100_000.0
                    || b.y.abs() > 100_000.0
                {
                    return Err(bad("Invalid block"));
                }
            }
            if !valid_cover(&save.cover) {
                return Err(bad("Invalid cover"));
            }
            let tags = normalize_tags(&save.tags)?;
            keep_version(d, n.clone());
            let n = Arc::make_mut(d.notes.iter_mut().find(|n| n.id == *nid).unwrap());
            n.title = save.title;
            n.icon = save.icon;
            n.blocks = save.blocks;
            n.favorite = save.favorite;
            n.trashed = save.trashed;
            if let Some(archived) = save.archived {
                n.archived = archived;
            }
            n.cover = save.cover;
            n.tags = tags;
            n.revision += 1;
            n.updated_at = now();
            let saved = n.clone();
            collaboration::replace_note(d, &saved)?;
            let mut view = note_view(&saved);
            view["role"] = json!(note_role(d, &u, &saved));
            view
        }
        ("PUT", ["notes", nid, "metadata"]) => {
            // Metadata is saved on its own, so editing a note's text never
            // overwrites it (and the reverse).
            let n = d.notes.iter().find(|n| n.id == *nid).ok_or_else(missing)?;
            note_access(d, &u, n, true)?;
            let metadata = normalize_metadata(&v["metadata"])?;
            let template = v["print_template"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string();
            if !template.is_empty() && plugins::get(plugins::Kind::PdfPrints, &template).is_none() {
                return Err(bad("That PDF template is not installed"));
            }
            let n = Arc::make_mut(d.notes.iter_mut().find(|n| n.id == *nid).unwrap());
            n.metadata = metadata;
            n.print_template = template;
            json!({"metadata": n.metadata, "print_template": n.print_template})
        }
        ("GET", ["notes", nid, "history"]) => {
            let n = d.notes.iter().find(|n| n.id == *nid).ok_or_else(missing)?;
            // Past revisions may hold text removed before the note was shared, so
            // they stay with folder members rather than individual share recipients.
            access(d, &u, &n.folder_id, false)?;
            json!(storage::versions(db, nid)?)
        }
        ("POST", ["render"]) => {
            // One block, or a list of blocks (an AI reply) in one request.
            if let Some(blocks) = v["blocks"].as_array() {
                if blocks.len() > 500 {
                    return Err(bad("Too many blocks"));
                }
                let html = blocks
                    .iter()
                    .map(|b| match b.as_str() {
                        Some(md) if md.len() <= 100_000 => Ok(render(md)),
                        _ => Err(bad("Invalid block")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                return Ok((json!({ "html": html }), None));
            }
            let md = v["markdown"]
                .as_str()
                .ok_or_else(|| bad("Missing markdown"))?;
            if md.len() > 100_000 {
                return Err(bad("Block is too large"));
            }
            json!({"html":render(md)})
        }
        ("GET", ["admin"]) => {
            admin(&u)?;
            let users: Vec<Value> = d
                .users
                .iter()
                .map(|a| {
                    let mut user = json!(a.user);
                    user["two_factor"] = json!(a.two_factor.enabled());
                    user
                })
                .collect();
            json!({"users":users,"grants":[],"workspace_grants":[]})
        }
        ("POST", ["users"]) => {
            admin(&u)?;
            let email = field(&v, "email")?.to_lowercase();
            if !email.contains('@') {
                return Err(bad("Invalid email"));
            }
            if d.users.iter().any(|a| a.user.email == email) {
                return Err(bad("Email already exists"));
            }
            // With email, the password may be left out: the invitation lets the
            // person choose their own.
            let password = match v["password"].as_str().filter(|p| !p.is_empty()) {
                Some(p) => p.to_string(),
                None if mail::enabled() => format!("{}{}", id(), id()),
                None => String::new(),
            };
            let hash = password_hash(&password)?;
            let user = User {
                token_workspace: None,
                id: id(),
                name: field(&v, "name")?,
                email,
                admin: false,
                can_create_workspace: true,
                disabled: false,
            };
            d.users.push(Account {
                user: user.clone(),
                password_hash: hash,
                two_factor: Default::default(),
                must_change_password: true,
                default_workspace: None,
                dashboard: None,
            });
            // In the server's language, or else the administrator's.
            let lang = if std::env::var("MINERVA_LANGUAGE").is_ok() {
                i18n::default_language()
            } else {
                i18n::for_request(headers)
            };
            create_personal(d, &user.id, &lang);
            let invited = mail::invite(d, &u, &user, &lang);
            let mut out = json!(user);
            out["invited"] = json!(invited);
            out
        }
        ("PATCH", ["users", uid]) => {
            admin(&u)?;
            if *uid == u.id {
                return Err(bad("You cannot modify your own administrator account here"));
            }
            let a = d
                .users
                .iter_mut()
                .find(|a| a.user.id == *uid)
                .ok_or_else(missing)?;
            if v.get("can_create_workspace").is_some() {
                a.user.can_create_workspace = !a.user.admin;
            }
            if v.get("name").is_some() {
                a.user.name = field(&v, "name")?;
            }
            if let Some(b) = v["admin"].as_bool() {
                a.user.admin = b;
                a.user.can_create_workspace = !b;
            }
            if v.get("email").is_some() {
                let email = field(&v, "email")?.to_lowercase();
                if !email.contains('@') {
                    return Err(bad("Invalid email"));
                }
                if email != a.user.email && d.users.iter().any(|o| o.user.email == email) {
                    return Err(bad("Email already exists"));
                }
                let a = d.users.iter_mut().find(|a| a.user.id == *uid).unwrap();
                a.user.email = email;
            }
            let a = d.users.iter_mut().find(|a| a.user.id == *uid).unwrap();
            if let Some(b) = v["disabled"].as_bool() {
                a.user.disabled = b;
            }
            if let Some(p) = v["password"].as_str() {
                a.password_hash = password_hash(p)?;
                a.must_change_password = true;
            }
            if v["two_factor"].as_bool() == Some(false) {
                twofactor::reset(a);
            }
            let user = json!(a.user);
            d.sessions.retain(|_, s| s.user_id != *uid);
            // Half-finished second-factor sign-ins do not survive account changes.
            twofactor::forget(d, uid);
            user
        }
        // Deletes an account. Workspaces it owns are kept and pass to the
        // administrator; its private data (AI chats, flashcards, plugin jobs) is
        // removed when the change is saved.
        ("DELETE", ["users", uid]) => {
            admin(&u)?;
            if *uid == u.id {
                return Err(bad("You cannot delete your own account"));
            }
            if !d.users.iter().any(|a| a.user.id == *uid) {
                return Err(missing());
            }
            // Personal workspaces go with the account: nobody else may open them.
            // Workspaces shared with others pass to the administrator, so their
            // members keep them.
            let owned: Vec<(String, bool)> = d
                .workspaces
                .iter()
                .filter(|w| w.owner_id == *uid)
                .map(|w| (w.id.clone(), removal::shared(d, w)))
                .collect();
            let (mut transferred, mut removed) = (0, 0);
            for (wid, shared) in owned {
                if shared {
                    let successor = v["successor_id"].as_str().filter(|next| *next != *uid && d.users.iter().any(|a| a.user.id==*next && !a.user.admin && !a.user.disabled)).ok_or_else(||bad("Choose a non-administrator successor for shared workspaces before deleting this account"))?.to_string();
                    if let Some(w) = d.workspaces.iter_mut().find(|w| w.id == wid) {
                        w.owner_id = successor.clone();
                        w.personal = false;
                    }
                    d.workspace_grants
                        .retain(|g| g.workspace_id != wid || g.user_id != successor);
                    transferred += 1;
                } else {
                    removal::remove_workspace(d, &wid);
                    removed += 1;
                }
            }
            d.users.retain(|a| a.user.id != *uid);
            d.sessions.retain(|_, s| s.user_id != *uid);
            d.grants.retain(|g| g.user_id != *uid);
            d.workspace_grants.retain(|g| g.user_id != *uid);
            d.shares.retain(|s| s.user_id.as_deref() != Some(*uid));
            mcp::forget_user(d, uid);
            tokens::forget_user(d, uid);
            push::forget_user(d, uid);
            calendar::forget_user(d, uid);
            d.languages.remove(*uid);
            twofactor::forget(d, uid);
            json!({"ok":true,"workspaces_transferred":transferred,"workspaces_deleted":removed})
        }
        // Your overview page's widgets.
        ("GET", ["account", "dashboard"]) => {
            let a = d
                .users
                .iter()
                .find(|a| a.user.id == u.id)
                .ok_or_else(missing)?;
            json!({ "widgets": a.dashboard })
        }
        ("PUT", ["account", "dashboard"]) => {
            let widgets = valid_dashboard(&v["widgets"])?;
            let a = d
                .users
                .iter_mut()
                .find(|a| a.user.id == u.id)
                .ok_or_else(missing)?;
            a.dashboard = widgets;
            json!({ "widgets": a.dashboard })
        }
        // The language the app shows, for emails and new things.
        ("PUT", ["account", "language"]) => {
            let language = v["language"].as_str().unwrap_or("");
            if !i18n::known(language) {
                return Err(bad("Unknown language"));
            }
            d.languages.insert(u.id.clone(), language.to_string());
            json!({ "language": language })
        }
        // Changes your own password; other sessions end, this one stays.
        ("POST", ["account", "password"]) => {
            attempts.check(&format!("password:{}", u.id))?;
            let a = d
                .users
                .iter_mut()
                .find(|a| a.user.id == u.id)
                .ok_or_else(missing)?;
            let current = v["current_password"].as_str().unwrap_or("");
            let valid = current.len() <= 256
                && PasswordHash::new(&a.password_hash).is_ok_and(|h| {
                    Argon2::default()
                        .verify_password(current.as_bytes(), &h)
                        .is_ok()
                });
            if !valid {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    "Your current password is not correct".into(),
                ));
            }
            a.password_hash = password_hash(v["new_password"].as_str().unwrap_or(""))?;
            a.must_change_password = false;
            let created: Option<String> = None;
            let keep = session_token(headers).map(hash_token);
            d.sessions
                .retain(|key, s| s.user_id != u.id || keep.as_ref() == Some(key));
            twofactor::forget(d, &u.id);
            json!({"ok":true,"workspace_id":created})
        }
        ("PUT", ["workspace-access"]) => {
            // Owners share their own workspaces; administrators too, but
            // only the workspaces they own.
            let wid = field(&v, "workspace_id")?;
            let uid = field(&v, "user_id")?;
            // Refused before anything else, so nothing is revealed.
            if !edits_workspace(d, &u, &wid) {
                return Err(forbidden());
            }
            if d.workspaces.iter().any(|w| w.id == wid && w.personal) {
                return Err(bad("A personal workspace cannot be shared"));
            }
            if !d
                .users
                .iter()
                .any(|a| a.user.id == uid && !a.user.admin && !a.user.disabled)
            {
                return Err(missing());
            }
            if d.workspaces
                .iter()
                .any(|w| w.id == wid && w.owner_id == uid)
            {
                return Err(bad("The owner always has edit access"));
            }
            let role = match v["role"].as_str() {
                Some("viewer") => Some(Role::Viewer),
                Some("editor") => Some(Role::Editor),
                Some("none") => None,
                _ => return Err(bad("Invalid role")),
            };
            let had = d.workspace_grants.iter().any(|g| {
                g.workspace_id == wid && g.user_id == uid && Some(&g.role) == role.as_ref()
            });
            if let (false, Some(r)) = (had, &role) {
                let name = d
                    .workspaces
                    .iter()
                    .find(|w| w.id == wid)
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                mail::access(
                    d,
                    &u,
                    &uid,
                    &format!("the workspace “{name}”"),
                    *r == Role::Editor,
                );
            }
            d.workspace_grants
                .retain(|g| g.workspace_id != wid || g.user_id != uid);
            if let Some(role) = role {
                d.workspace_grants.push(WorkspaceGrant {
                    workspace_id: wid,
                    user_id: uid,
                    role,
                });
            }
            json!({"ok":true})
        }
        ("PUT", ["permissions"]) => {
            // Owners share their own workspaces; administrators too, but
            // only the workspaces they own.
            let fid = field(&v, "folder_id")?;
            let uid = field(&v, "user_id")?;
            let workspace = d
                .folders
                .iter()
                .find(|f| f.id == fid)
                .map(|f| f.workspace_id.clone())
                .unwrap_or_default();
            if !edits_workspace(d, &u, &workspace) {
                return Err(forbidden());
            }
            if d.workspaces.iter().any(|w| w.id == workspace && w.personal) {
                return Err(bad("A personal workspace cannot be shared"));
            }
            if !d
                .users
                .iter()
                .any(|a| a.user.id == uid && !a.user.admin && !a.user.disabled)
            {
                return Err(missing());
            }
            let role = match v["role"].as_str() {
                Some("viewer") => Some(Role::Viewer),
                Some("editor") => Some(Role::Editor),
                Some("none") => None,
                _ => return Err(bad("Invalid role")),
            };
            let had = d
                .grants
                .iter()
                .any(|g| g.folder_id == fid && g.user_id == uid && Some(&g.role) == role.as_ref());
            if let (false, Some(r)) = (had, &role) {
                let folder = d
                    .folders
                    .iter()
                    .find(|f| f.id == fid)
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                let space = d
                    .workspaces
                    .iter()
                    .find(|w| w.id == workspace)
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                mail::access(
                    d,
                    &u,
                    &uid,
                    &format!("the folder “{folder}” in “{space}”"),
                    *r == Role::Editor,
                );
            }
            d.grants.retain(|g| g.folder_id != fid || g.user_id != uid);
            if let Some(role) = role {
                d.grants.push(Grant {
                    folder_id: fid,
                    user_id: uid,
                    role,
                });
            }
            json!({"ok":true})
        }
        _ => return Err(missing()),
    };
    Ok((result, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Fails when `Note` gains a field that `note_summary` leaves out.
    #[test]
    fn note_summary_is_the_note_without_blocks() {
        let plain = getting_started("folder", "en");
        let mut detailed = plain.clone();
        detailed.metadata.insert("author".into(), "Ada".into());
        detailed.print_template = "01-standard".into();
        detailed.journal = Some("2026-11-01".into());
        for n in [plain, detailed] {
            let mut full = serde_json::to_value(&n).unwrap();
            full.as_object_mut().unwrap().remove("blocks");
            assert_eq!(note_summary(&n), full);
        }
    }
}
