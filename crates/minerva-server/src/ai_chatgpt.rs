//! Native ChatGPT token sharing: credentials never cross the desktop bridge.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const ISSUER: &str = "https://auth.openai.com";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
#[derive(Default)]
pub(super) struct Runtime {
    flows: HashMap<String, Value>,
    requests: HashMap<String, tokio::sync::watch::Sender<bool>>,
    refresh: std::sync::Arc<tokio::sync::Mutex<()>>,
}
pub(super) fn cancellation(
    store: &Shared,
    id: &str,
) -> Result<Option<tokio::sync::watch::Receiver<bool>>, ApiError> {
    let mut s = store.lock().map_err(|_| internal())?;
    let (p, key) = provider(&s, id)?;
    if p.kind != "chatgpt" {
        return Ok(None);
    }
    secret(&s, &p, &key)?;
    Ok(Some(
        s.ai.oauth
            .requests
            .entry(id.to_string())
            .or_insert_with(|| tokio::sync::watch::channel(false).0)
            .subscribe(),
    ))
}
pub(super) fn cancel(s: &mut Store, id: &str) {
    if let Some(sender) = s.ai.oauth.requests.remove(id) {
        let _ = sender.send(true);
    }
}
pub(super) async fn cancelled(rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(rx) = rx {
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    } else {
        std::future::pending::<()>().await;
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Session {
    client_id: String,
    subject: String,
    access_token: String,
    refresh_token: String,
    expires: u64,
    scope: String,
}
fn error() -> ApiError {
    bad("ChatGPT sign-in failed. Please reconnect your account.")
}
fn random() -> String {
    let mut b = [0u8; 32];
    OsRng.fill_bytes(&mut b);
    B64.encode(b)
}
fn session(s: &Store, key: &[u8]) -> Result<Session, ApiError> {
    serde_json::from_str(&s.ai.decrypt(key)?).map_err(|_| error())
}
pub(super) fn secret(s: &Store, p: &Provider, key: &[u8]) -> Result<String, ApiError> {
    if p.kind == "chatgpt" {
        let session = session(s, key)?;
        if session.access_token.is_empty() {
            return Err(error());
        }
        Ok(session.access_token)
    } else {
        s.ai.decrypt(key)
    }
}
async fn json_response(response: reqwest::Response) -> Result<Value, ApiError> {
    if !response.status().is_success() {
        return Err(error());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| error())?;
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(error());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| error())
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() < 32768)
        .ok_or_else(error)
}
// Reject algorithm confusion, untrusted issuers, and tokens for another app/account.
fn verify_id(
    token: &str,
    keys: &Value,
    client: &str,
    nonce: Option<&str>,
    subject: Option<&str>,
) -> Result<String, ApiError> {
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(error());
    }
    let header: Value =
        serde_json::from_slice(&B64.decode(parts[0]).map_err(|_| error())?).map_err(|_| error())?;
    if header["alg"] != "RS256" || text(&header, "kid").is_err() {
        return Err(error());
    }
    let key = keys["keys"]
        .as_array()
        .ok_or_else(error)?
        .iter()
        .find(|k| {
            k["kid"] == header["kid"]
                && k["kty"] == "RSA"
                && (k["use"].is_null() || k["use"] == "sig")
                && (k["alg"].is_null() || k["alg"] == "RS256")
        })
        .ok_or_else(error)?;
    let n = B64.decode(text(key, "n")?).map_err(|_| error())?;
    let e = B64.decode(text(key, "e")?).map_err(|_| error())?;
    let signature = B64.decode(parts[2]).map_err(|_| error())?;
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &signature,
        )
        .map_err(|_| error())?;
    let claims: Value =
        serde_json::from_slice(&B64.decode(parts[1]).map_err(|_| error())?).map_err(|_| error())?;
    let audience = claims["aud"] == client
        || claims["aud"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == client));
    if claims["iss"] != ISSUER
        || !audience
        || claims["exp"].as_u64().unwrap_or(0) <= now()
        || claims["iat"].as_u64().is_none_or(|t| t > now() + 5)
        || nonce.is_some_and(|n| claims["nonce"] != n)
        || subject.is_some_and(|s| claims["sub"] != s)
    {
        return Err(error());
    }
    if claims["aud"].as_array().is_some_and(|a| a.len() > 1) && claims["azp"] != client {
        return Err(error());
    }
    Ok(text(&claims, "sub")?.to_string())
}
async fn tokens(
    client: &reqwest::Client,
    value: Value,
    client_id: &str,
    nonce: Option<&str>,
    old: Option<&Session>,
) -> Result<Session, ApiError> {
    if !text(&value, "token_type")?.eq_ignore_ascii_case("bearer") {
        return Err(error());
    }
    let scope = value["scope"]
        .as_str()
        .or_else(|| old.map(|s| s.scope.as_str()))
        .ok_or_else(error)?;
    if !scope
        .split_whitespace()
        .any(|s| s == "chatgpt.tokens.use.direct")
    {
        return Err(error());
    }
    let subject = if let Some(token) = value["id_token"].as_str() {
        let keys = json_response(
            client
                .get(format!("{ISSUER}/.well-known/jwks.json"))
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .map_err(|_| error())?,
        )
        .await?;
        let subject = verify_id(
            token,
            &keys,
            client_id,
            nonce,
            old.map(|s| s.subject.as_str()),
        )?;
        let claims: Value = serde_json::from_slice(
            &B64.decode(token.split('.').nth(1).ok_or_else(error)?)
                .map_err(|_| error())?,
        )
        .map_err(|_| error())?;
        if let Some(hash) = claims.get("at_hash") {
            let digest = Sha256::digest(text(&value, "access_token")?.as_bytes());
            if hash != &json!(B64.encode(&digest[..16])) {
                return Err(error());
            }
        }
        subject
    } else {
        old.map(|s| s.subject.clone()).ok_or_else(error)?
    };
    let expires = value["expires_in"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 86400)
        .ok_or_else(error)?;
    Ok(Session {
        client_id: client_id.into(),
        subject,
        access_token: text(&value, "access_token")?.into(),
        refresh_token: text(&value, "refresh_token")?.into(),
        expires: now() + expires,
        scope: scope.into(),
    })
}
pub(super) async fn fresh(store: &Shared, id: &str) -> Result<(), ApiError> {
    refresh_at(store, id, TOKEN).await
}
async fn refresh_at(store: &Shared, id: &str, token_endpoint: &str) -> Result<(), ApiError> {
    let mutex = {
        let s = store.lock().map_err(|_| internal())?;
        let (p, _) = provider(&s, id)?;
        if p.kind != "chatgpt" {
            return Ok(());
        }
        if !s.ai.desktop {
            return Err(forbidden());
        }
        s.ai.oauth.refresh.clone()
    };
    let _guard = mutex.lock().await;
    let (old, encrypted, client) = {
        let s = store.lock().map_err(|_| internal())?;
        let (_, encrypted) = provider(&s, id)?;
        (session(&s, &encrypted)?, encrypted, s.ai.client.clone())
    };
    if old.access_token.is_empty() || old.refresh_token.is_empty() {
        return Err(error());
    }
    if old.expires > now() + 60 {
        return Ok(());
    }
    let value = json_response(
        client
            .post(token_endpoint)
            .timeout(Duration::from_secs(30))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", old.client_id.as_str()),
                ("refresh_token", old.refresh_token.as_str()),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| error())?,
    )
    .await?;
    let new = tokens(&client, value, &old.client_id, None, Some(&old)).await?;
    let s = store.lock().map_err(|_| internal())?;
    let changed =
        s.db.execute(
            "UPDATE ai_providers SET secret=?2 WHERE id=?1 AND secret=?3 AND private=1",
            params![
                id,
                s.ai.encrypt(&serde_json::to_string(&new).map_err(|_| internal())?)?,
                encrypted
            ],
        )
        .map_err(|_| internal())?;
    if changed != 1 {
        return Err(error());
    }
    Ok(())
}
async fn models(client: &reqwest::Client, access: &str) -> Result<Value, ApiError> {
    let v = json_response(
        client
            .get(format!("{RESOURCE}/models"))
            .timeout(Duration::from_secs(30))
            .bearer_auth(access)
            .send()
            .await
            .map_err(|_| error())?,
    )
    .await?;
    let models:Vec<_>=v["models"].as_array().ok_or_else(error)?.iter().filter(|m|m["visibility"]=="list").filter_map(|m|Some(json!({"id":m["slug"].as_str()?,"name":m["display_name"].as_str().unwrap_or(m["slug"].as_str()?)}))).collect();
    if models.is_empty() {
        return Err(error());
    }
    Ok(json!(models))
}
pub(super) fn edit(s: &Store, p: &Provider, v: &Value) -> Result<Value, ApiError> {
    if !s.ai.desktop
        || v["kind"] != "chatgpt"
        || v["endpoint"] != RESOURCE
        || v["api_key"].as_str().is_some_and(|k| !k.is_empty())
    {
        return Err(forbidden());
    }
    let name = field(v, "name")?;
    let model = field(v, "model")?;
    s.db.execute(
        "UPDATE ai_providers SET name=?2,model=?3 WHERE id=?1",
        params![p.id, name, model],
    )
    .map_err(|_| internal())?;
    Ok(json!(provider(s, &p.id)?.0))
}
pub(super) async fn endpoint(
    store: &Shared,
    headers: &HeaderMap,
    method: &Method,
    parts: &[&str],
    v: &Value,
) -> Result<Value, ApiError> {
    {
        let s = store.lock().map_err(|_| internal())?;
        admin(&authenticate(&s.data, headers)?)?;
        if !s.ai.desktop {
            return Err(forbidden());
        }
    }
    match (method.as_str(), parts) {
        ("POST", ["chatgpt", "start"]) => start(store, v).await,
        ("GET", ["chatgpt", "flows", id]) => {
            let s = store.lock().map_err(|_| internal())?;
            s.ai.oauth.flows.get(*id).cloned().ok_or_else(missing)
        }
        ("DELETE", ["chatgpt", "flows", id]) => {
            store
                .lock()
                .map_err(|_| internal())?
                .ai
                .oauth
                .flows
                .remove(*id);
            Ok(json!({"ok":true}))
        }
        ("POST", ["chatgpt", id, "models"]) => {
            fresh(store, id).await?;
            let (client, token) = {
                let s = store.lock().map_err(|_| internal())?;
                let (p, k) = provider(&s, id)?;
                if p.kind != "chatgpt" {
                    return Err(forbidden());
                }
                (s.ai.client.clone(), secret(&s, &p, &k)?)
            };
            Ok(json!({"models":models(&client,&token).await?}))
        }
        ("POST", ["chatgpt", id, "signout"]) => {
            store.lock().map_err(|_| internal())?.ai.oauth.flows.clear();
            let (client, old) = {
                let mut s = store.lock().map_err(|_| internal())?;
                let (p, k) = provider(&s, id)?;
                if p.kind != "chatgpt" {
                    return Err(forbidden());
                }
                let old = session(&s, &k)?;
                cancel(&mut s, id);
                let mut cleared = old.clone();
                cleared.access_token.clear();
                cleared.refresh_token.clear();
                cleared.expires = 0;
                s.db.execute(
                    "UPDATE ai_providers SET secret=?2 WHERE id=?1",
                    params![
                        id,
                        s.ai.encrypt(&serde_json::to_string(&cleared).map_err(|_| internal())?)?
                    ],
                )
                .map_err(|_| internal())?;
                (s.ai.client.clone(), old)
            };
            let revoked = revoke(&client, &old).await;
            Ok(json!({"ok":true,"revoked":revoked}))
        }
        _ => Err(missing()),
    }
}
async fn revoke(client: &reqwest::Client, s: &Session) -> bool {
    if s.refresh_token.is_empty() {
        return true;
    }
    let result = async {
        let metadata = json_response(
            client
                .get(format!("{ISSUER}/.well-known/openid-configuration"))
                .timeout(Duration::from_secs(15))
                .send()
                .await
                .map_err(|_| error())?,
        )
        .await?;
        let endpoint = text(&metadata, "revocation_endpoint")?;
        let url = url::Url::parse(endpoint).map_err(|_| error())?;
        if url.origin().ascii_serialization() != ISSUER || metadata["issuer"] != ISSUER {
            return Err(error());
        }
        let r = client
            .post(url)
            .timeout(Duration::from_secs(15))
            .form(&[
                ("token", s.refresh_token.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", s.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|_| error())?;
        Ok::<bool, ApiError>(r.status().is_success())
    }
    .await;
    result.unwrap_or(false)
}
async fn start(store: &Shared, v: &Value) -> Result<Value, ApiError> {
    let name = field(v, "name")?;
    let existing = v["provider_id"].as_str().map(str::to_owned);
    let (host, old, client) = {
        let mut s = store.lock().map_err(|_| internal())?;
        s.ai.oauth
            .flows
            .retain(|_, v| v["created"].as_u64().unwrap_or(0) + 600 > now());
        if s.ai.oauth.flows.values().any(|v| v["status"] == "pending") {
            return Err(bad("A ChatGPT sign-in is already in progress"));
        }
        let count: i64 =
            s.db.query_row("SELECT COUNT(*) FROM ai_providers", [], |r| r.get(0))
                .map_err(|_| internal())?;
        if existing.is_none() && count >= 20 {
            return Err(bad("Maximum 20 provider profiles"));
        }
        let old = if let Some(id) = &existing {
            let (p, k) = provider(&s, id)?;
            if p.kind != "chatgpt" {
                return Err(forbidden());
            }
            Some(session(&s, &k)?)
        } else {
            None
        };
        s.db.execute(
            "INSERT OR IGNORE INTO ai_oauth_host VALUES(1,?1)",
            [format!("urn:uuid:{}", id())],
        )
        .map_err(|_| internal())?;
        let host: String =
            s.db.query_row("SELECT host FROM ai_oauth_host WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(|_| internal())?;
        (host, old, s.ai.client.clone())
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| error())?;
    let redirect = format!(
        "http://127.0.0.1:{}/auth/callback",
        listener.local_addr().map_err(|_| error())?.port()
    );
    let state = random();
    let nonce = random();
    let verifier = random();
    let flow = id();
    let mut url =
        url::Url::parse("https://auth.openai.com/api/accounts/authorize").map_err(|_| error())?;
    {
        let mut q = url.query_pairs_mut();
        q.extend_pairs([
            (
                "client_id",
                old.as_ref()
                    .map(|s| s.client_id.as_str())
                    .unwrap_or("dynamic_agent_client"),
            ),
            ("ext_agent_host_id", &host),
            ("response_type", "code"),
            ("redirect_uri", &redirect),
            ("scope", SCOPE),
            ("resource", RESOURCE),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            (
                "code_challenge",
                &B64.encode(Sha256::digest(verifier.as_bytes())),
            ),
        ]);
        if old.is_none() {
            q.append_pair("agent_name_hint", "Minerva");
        }
    }
    {
        let mut s = store.lock().map_err(|_| internal())?;
        if s.ai.oauth.flows.values().any(|v| v["status"] == "pending")
            || s.ai.oauth.flows.len() >= 20
        {
            return Err(bad("A ChatGPT sign-in is already in progress"));
        }
        s.ai.oauth
            .flows
            .insert(flow.clone(), json!({"status":"pending","created":now()}));
    }
    let task_store = store.clone();
    let task_flow = flow.clone();
    tokio::spawn(async move {
        let result=tokio::time::timeout(Duration::from_secs(300),async {
            loop{
                if !task_store.lock().map_err(|_|internal())?.ai.oauth.flows.contains_key(&task_flow) { return Err(error()); }
                let(mut socket,_)=match tokio::time::timeout(Duration::from_secs(1),listener.accept()).await {Ok(v)=>v.map_err(|_|error())?,Err(_)=>continue};
                let mut buffer=Vec::new();
                let read=tokio::time::timeout(Duration::from_secs(3),async {
                    while !buffer.ends_with(b"\r\n\r\n") {
                        let mut chunk=[0u8;1024];let n=socket.read(&mut chunk).await.map_err(|_|error())?;
                        if n==0 || buffer.len()+n>16384 {return Err(error());}buffer.extend_from_slice(&chunk[..n]);
                    } Ok::<(),ApiError>(())
                }).await;
                if !matches!(read,Ok(Ok(()))){continue;}let size=buffer.len();
                let request=String::from_utf8_lossy(&buffer[..size]);
                let target=request.lines().next().unwrap_or("").strip_prefix("GET ").and_then(|s|s.strip_suffix(" HTTP/1.1")).unwrap_or("");
                let callback=url::Url::parse(&format!("http://127.0.0.1{target}"));
                let query=callback.as_ref().ok().filter(|u|u.path()=="/auth/callback").map(|u|u.query_pairs().into_owned().collect::<HashMap<_,_>>()).unwrap_or_default();
                if query.get("state")!=Some(&state){let _=socket.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await;continue;}
                let _=socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\n\r\nReturn to Minerva to finish signing in.").await;
                let _=socket.shutdown().await;
                if query.contains_key("error"){return Err(error());}
                let code=query.get("code").ok_or_else(error)?;
                let issued=query.get("client_id").filter(|s|s.starts_with("oaiapp_")).ok_or_else(error)?;
                if old.as_ref().is_some_and(|o|o.client_id!=*issued){return Err(error());}
                if !task_store.lock().map_err(|_|internal())?.ai.oauth.flows.contains_key(&task_flow){return Err(error());}
                let value=json_response(client.post(TOKEN).timeout(Duration::from_secs(30)).form(&[("grant_type","authorization_code"),("client_id",issued),("code",code),("code_verifier",&verifier),("redirect_uri",&redirect),("resource",RESOURCE)]).send().await.map_err(|_|error())?).await?;
                let session=tokens(&client,value,issued,Some(&nonce),old.as_ref()).await?;
                let catalog=models(&client,&session.access_token).await?;
                let id=existing.clone().unwrap_or_else(super::id);
                let s=task_store.lock().map_err(|_|internal())?;
                if !s.ai.oauth.flows.contains_key(&task_flow){return Err(error());}
                let encrypted=s.ai.encrypt(&serde_json::to_string(&session).map_err(|_|internal())?)?;
                if existing.is_some(){
                    let count=s.db.execute("UPDATE ai_providers SET secret=?2 WHERE id=?1 AND kind='chatgpt' AND private=1",params![id,encrypted]).map_err(|_|internal())?;if count!=1{return Err(error());}
                }else{
                    s.db.execute("INSERT INTO ai_providers(id,name,kind,endpoint,model,secret,private,auth_mode) VALUES(?1,?2,'chatgpt',?3,?4,?5,1,'oauth')",params![id,name,RESOURCE,catalog[0]["id"].as_str().ok_or_else(error)?,encrypted]).map_err(|_|internal())?;
                }
                return Ok(json!({"status":"connected","provider_id":id,"models":catalog,"created":now()}));
            }
        }).await;
        if let Ok(mut s) = task_store.lock() {
            if let Some(v) = s.ai.oauth.flows.get_mut(&task_flow) {
                *v = match result {
                    Ok(Ok(v)) => v,
                    _ => json!({"status":"failed","created":now()}),
                };
            }
        }
    });
    Ok(json!({"flow_id":flow,"url":url.as_str()}))
}
pub(super) async fn completion_response(
    response: reqwest::Response,
) -> Result<(String, Vec<String>), ApiError> {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut bytes = Vec::new();
    let mut answer = String::new();
    let mut completed = false;
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.map_err(|_| error())?);
        if bytes.len() > 262144 {
            return Err(error());
        }
        // Retain incomplete UTF-8 until the next chunk.
        let valid = match std::str::from_utf8(&bytes) {
            Ok(s) => s.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => return Err(error()),
        };
        buffer.push_str(std::str::from_utf8(&bytes[..valid]).map_err(|_| error())?);
        bytes.drain(..valid);
        while let Some(end) = buffer.find('\n') {
            let line = buffer[..end].trim_end_matches('\r').to_string();
            buffer.drain(..=end);
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                continue;
            }
            let v: Value = serde_json::from_str(data).map_err(|_| error())?;
            match v["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => answer.push_str(v["delta"].as_str().unwrap_or("")),
                "response.completed" => completed = true,
                "error" | "response.failed" | "response.incomplete" => return Err(error()),
                _ => (),
            }
            if answer.len() > 64000 {
                return Err(bad("Provider returned empty or oversized output"));
            }
        }
        if buffer.len() > 262144 {
            return Err(error());
        }
        if completed {
            break;
        }
    }
    if !completed || answer.is_empty() {
        return Err(bad("Provider response was incomplete"));
    }
    Ok((answer, vec![]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{RsaKeyPair, RSA_PKCS1_SHA256},
    };
    fn signed(claims: Value) -> (String, Value) {
        // Public test-only key, never used by production authentication.
        let key =
            RsaKeyPair::from_pkcs8(include_bytes!("../tests/fixtures/oauth-test-key.der")).unwrap();
        let public = key.public();
        let jwks =
            serde_json::from_str(include_str!("../tests/fixtures/oauth-test-jwks.json")).unwrap();
        let input = format!(
            "{}.{}",
            B64.encode(br#"{"alg":"RS256","kid":"test"}"#),
            B64.encode(claims.to_string())
        );
        let mut signature = vec![0; public.modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut signature,
        )
        .unwrap();
        (format!("{input}.{}", B64.encode(signature)), jwks)
    }
    #[test]
    fn validates_signature_nonce_audience_issuer_expiry_and_account() {
        let claims = json!({"iss":ISSUER,"aud":"oaiapp_test","sub":"account","iat":now(),"exp":now()+3600,"nonce":"nonce"});
        let (token, keys) = signed(claims.clone());
        assert_eq!(
            verify_id(&token, &keys, "oaiapp_test", Some("nonce"), Some("account")).unwrap(),
            "account"
        );
        for (field, value) in [
            ("iss", json!("https://attacker.invalid")),
            ("aud", json!("other")),
            ("sub", json!("other")),
            ("nonce", json!("other")),
            ("exp", json!(now() - 1)),
            ("iat", json!(now() + 60)),
        ] {
            let mut invalid = claims.clone();
            invalid[field] = value;
            let (token, keys) = signed(invalid);
            assert!(
                verify_id(&token, &keys, "oaiapp_test", Some("nonce"), Some("account")).is_err(),
                "{field}"
            );
        }
        let mut corrupt = token.into_bytes();
        let n = corrupt.len();
        corrupt[n - 5] = if corrupt[n - 5] == b'A' { b'B' } else { b'A' };
        assert!(verify_id(
            std::str::from_utf8(&corrupt).unwrap(),
            &keys,
            "oaiapp_test",
            Some("nonce"),
            None
        )
        .is_err());
        assert!(verify_id("eyJhbGciOiJub25lIn0.e30.", &keys, "oaiapp_test", None, None).is_err());
    }
    #[tokio::test]
    async fn private_profiles_cannot_be_used_by_network_runtime() {
        let (store, _) = Store::open_local(":memory:").unwrap();
        let mut s = store.lock().unwrap();
        let u = s.data.users[0].user.clone();
        let profile=super::super::route(&mut s,&u,&Method::POST,&["providers"],json!({"name":"My model","kind":"compatible","endpoint":"http://127.0.0.1:11434/v1","model":"local-model","auth_mode":"none"})).unwrap();
        assert_eq!(profile["private"], true);
        assert_eq!(profile["auth_mode"], "none");
        let id = profile["id"].as_str().unwrap();
        let (p, key) = provider(&s, id).unwrap();
        let request = model_request(
            &s.ai.client,
            &p,
            &secret(&s, &p, &key).unwrap(),
            "system",
            vec![],
            false,
            false,
        )
        .build()
        .unwrap();
        assert!(!request.headers().contains_key("authorization"));
        s.ai.desktop = false;
        assert!(provider(&s, id).is_err());
        assert_eq!(
            super::super::route(&mut s, &u, &Method::GET, &["providers"], json!({})).unwrap(),
            json!([])
        );
        assert!(
            super::super::route(&mut s, &u, &Method::DELETE, &["providers", id], json!({}))
                .is_err()
        );
        assert!(super::super::route(&mut s,&u,&Method::POST,&["providers"],json!({"name":"My model","kind":"compatible","endpoint":"http://127.0.0.1:11434/v1","model":"local-model","auth_mode":"none"})).is_err());
    }
    #[tokio::test]
    async fn oauth_request_uses_subscription_payload_and_never_api_billing() {
        let (store, _) = Store::open_local(":memory:").unwrap();
        let s = store.lock().unwrap();
        let p = Provider {
            id: "test".into(),
            private: true,
            auth_mode: "oauth".into(),
            kind: "chatgpt".into(),
            name: "Personal".into(),
            endpoint: RESOURCE.into(),
            model: "discovered-model".into(),
            web_search: false,
            server_tools: vec![],
            max_tokens: 4096,
        };
        let request = model_request(
            &s.ai.client,
            &p,
            "subscription-token",
            "instructions",
            vec![json!({"role":"user","content":"hello"})],
            false,
            false,
        )
        .build()
        .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://api.openai.com/v1/responses"
        );
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert!(body["input"].is_array());
        for field in [
            "max_output_tokens",
            "max_tool_calls",
            "temperature",
            "previous_response_id",
        ] {
            assert!(body.get(field).is_none());
        }
    }
    #[tokio::test]
    async fn signout_cancels_active_requests_and_prevents_late_starts() {
        let (store, _) = Store::open_local(":memory:").unwrap();
        {
            let s = store.lock().unwrap();
            let session = Session {
                client_id: "oaiapp_test".into(),
                subject: "account".into(),
                access_token: "access".into(),
                refresh_token: "refresh".into(),
                expires: now() + 3600,
                scope: SCOPE.into(),
            };
            s.db.execute("INSERT INTO ai_providers(id,name,kind,endpoint,model,secret,private,auth_mode) VALUES('profile','Personal','chatgpt',?1,'model',?2,1,'oauth')",params![RESOURCE,s.ai.encrypt(&serde_json::to_string(&session).unwrap()).unwrap()]).unwrap();
        }
        let mut waiting = cancellation(&store, "profile").unwrap();
        {
            let mut s = store.lock().unwrap();
            cancel(&mut s, "profile");
            let (_, key) = provider(&s, "profile").unwrap();
            let mut cleared = session(&s, &key).unwrap();
            cleared.access_token.clear();
            cleared.refresh_token.clear();
            s.db.execute(
                "UPDATE ai_providers SET secret=?1",
                [s.ai
                    .encrypt(&serde_json::to_string(&cleared).unwrap())
                    .unwrap()],
            )
            .unwrap();
        }
        tokio::time::timeout(Duration::from_millis(100), cancelled(&mut waiting))
            .await
            .unwrap();
        assert!(cancellation(&store, "profile").is_err());
    }
    #[tokio::test]
    async fn completion_requires_completed_event_and_preserves_utf8() {
        let body = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"caffè\"}\n\ndata: {\"type\":\"response.completed\"}\n\n";
        let chunks: Vec<_> = body
            .as_bytes()
            .chunks(1)
            .map(|c| Ok::<_, std::io::Error>(c.to_vec()))
            .collect();
        let response = reqwest::Response::from(axum::http::Response::new(
            reqwest::Body::wrap_stream(futures_util::stream::iter(chunks)),
        ));
        assert_eq!(completion_response(response).await.unwrap().0, "caffè");
        for end in [
            "",
            "data: {\"type\":\"response.incomplete\"}\n\n",
            "data: {\"type\":\"error\"}\n\n",
        ] {
            let body = format!(
                "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}}\n\n{end}"
            );
            let response =
                reqwest::Response::from(axum::http::Response::new(reqwest::Body::from(body)));
            assert!(completion_response(response).await.is_err());
        }
    }
    #[tokio::test]
    async fn concurrent_refresh_rotates_once_and_keeps_credentials_encrypted() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let router=axum::Router::new().route("/token",axum::routing::post(move |axum::Form(form):axum::Form<HashMap<String,String>>|{let seen=seen.clone();async move{
            assert_eq!(form["client_id"],"oaiapp_test");assert_eq!(form["refresh_token"],"old-refresh");assert!(!form.contains_key("scope"));
            seen.fetch_add(1,Ordering::SeqCst);
            axum::Json(json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600,"token_type":"Bearer"}))
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let (store, _) = Store::open_local(":memory:").unwrap();
        {
            let s = store.lock().unwrap();
            let old = Session {
                client_id: "oaiapp_test".into(),
                subject: "account".into(),
                access_token: "old-access".into(),
                refresh_token: "old-refresh".into(),
                expires: now() - 1,
                scope: SCOPE.into(),
            };
            s.db.execute("INSERT INTO ai_providers(id,name,kind,endpoint,model,secret,private,auth_mode) VALUES('profile','Personal','chatgpt',?1,'model',?2,1,'oauth')",params![RESOURCE,s.ai.encrypt(&serde_json::to_string(&old).unwrap()).unwrap()]).unwrap();
        }
        let (a, b) = tokio::join!(
            refresh_at(&store, "profile", &endpoint),
            refresh_at(&store, "profile", &endpoint)
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let s = store.lock().unwrap();
        let (_, encrypted) = provider(&s, "profile").unwrap();
        let saved = session(&s, &encrypted).unwrap();
        assert_eq!(saved.refresh_token, "new-refresh");
        assert_eq!(saved.access_token, "new-access");
        assert!(!String::from_utf8_lossy(&encrypted).contains("new-refresh"));
        server.abort();
    }
    #[tokio::test]
    async fn cancelled_callback_does_not_register_and_invalid_state_is_not_consumed() {
        let (store, _) = Store::open_local(":memory:").unwrap();
        let flow = start(&store, &json!({"name":"Personal"})).await.unwrap();
        let url = url::Url::parse(flow["url"].as_str().unwrap()).unwrap();
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], "dynamic_agent_client");
        assert_eq!(query["code_challenge_method"], "S256");
        let redirect = &query["redirect_uri"];
        let response = reqwest::get(format!("{redirect}?state=wrong&error=access_denied"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let id = flow["flow_id"].as_str().unwrap();
        assert_eq!(
            store.lock().unwrap().ai.oauth.flows[id]["status"],
            "pending"
        );
        let response = reqwest::get(format!(
            "{redirect}?state={}&error=access_denied",
            query["state"]
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        for _ in 0..20 {
            if store.lock().unwrap().ai.oauth.flows[id]["status"] == "failed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let s = store.lock().unwrap();
        assert_eq!(s.ai.oauth.flows[id]["status"], "failed");
        assert_eq!(
            s.db.query_row("SELECT COUNT(*) FROM ai_providers", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
