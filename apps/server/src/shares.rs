//! Share links: a token that grants anyone holding it read access to one
//! file or folder inside the file roots, optionally expiring and optionally
//! behind a password.
//!
//! Links live in `shares.json` in the data directory. Public pages under
//! `/s/:token` bypass the server login and are rendered here as plain HTML,
//! since the people opening them have no Spacedrive account.

use std::{
	collections::HashMap,
	net::SocketAddr,
	path::{Path as FsPath, PathBuf},
	time::{SystemTime, UNIX_EPOCH},
};

use argon2::{
	password_hash::{
		rand_core::OsRng as HashRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
	},
	Argon2,
};
use axum::{
	extract::{ConnectInfo, Path, Query, Request, State},
	http::{header, HeaderMap, HeaderValue, StatusCode},
	response::{IntoResponse, Response},
	Form, Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::{
	files::{percent_encode, serve_path, short_form},
	plain_status, throttle, AppState,
};

const TEN_YEARS_HOURS: u32 = 24 * 365 * 10;

#[derive(Clone, Serialize, Deserialize)]
struct Share {
	token: String,
	/// Canonical path inside a file root.
	path: PathBuf,
	created_at: i64,
	expires_at: Option<i64>,
	/// Argon2 PHC string.
	password_hash: Option<String>,
}

impl Share {
	fn expired(&self, now: i64) -> bool {
		self.expires_at.is_some_and(|at| at <= now)
	}

	fn name(&self) -> String {
		self.path
			.file_name()
			.map(|name| name.to_string_lossy().into_owned())
			.unwrap_or_else(|| self.path.to_string_lossy().into_owned())
	}
}

pub struct ShareStore {
	file: PathBuf,
	/// Keys the unlock cookie, so a cookie proves the password was entered
	/// without storing the password or a session table.
	key: [u8; 32],
	shares: RwLock<HashMap<String, Share>>,
}

fn now() -> i64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.map(|elapsed| elapsed.as_secs() as i64)
		.unwrap_or_default()
}

fn write_private(path: &FsPath, bytes: &[u8]) -> std::io::Result<()> {
	use std::io::Write;
	let mut options = std::fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.mode(0o600);
	}
	let mut file = options.open(path)?;
	file.write_all(bytes)?;
	file.sync_all()
}

impl ShareStore {
	pub fn load(data_dir: &FsPath) -> std::io::Result<Self> {
		let file = data_dir.join("shares.json");
		let list: Vec<Share> = match std::fs::read(&file) {
			Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
			Err(error) => return Err(error),
		};
		let key_file = data_dir.join("share-key");
		let key = match std::fs::read(&key_file) {
			Ok(bytes) if bytes.len() == 32 => bytes.try_into().expect("length checked"),
			_ => {
				let mut key = [0u8; 32];
				rand::rngs::OsRng.fill_bytes(&mut key);
				write_private(&key_file, &key)?;
				key
			}
		};
		Ok(Self {
			file,
			key,
			shares: RwLock::new(list.into_iter().map(|s| (s.token.clone(), s)).collect()),
		})
	}

	/// Persist under the caller's write lock, via a temp file and rename so a
	/// crash never leaves a half-written list.
	async fn save(&self, shares: &HashMap<String, Share>) -> std::io::Result<()> {
		let mut list: Vec<&Share> = shares.values().collect();
		list.sort_by_key(|share| share.created_at);
		let bytes = serde_json::to_vec_pretty(&list).map_err(std::io::Error::other)?;
		let temp = self.file.with_extension("json.tmp");
		let target = self.file.clone();
		tokio::task::spawn_blocking(move || {
			write_private(&temp, &bytes)?;
			std::fs::rename(&temp, &target)
		})
		.await
		.map_err(std::io::Error::other)?
	}

	async fn get(&self, token: &str) -> Lookup {
		match self.shares.read().await.get(token) {
			None => Lookup::Missing,
			Some(share) if share.expired(now()) => Lookup::Expired,
			Some(share) => Lookup::Found(share.clone()),
		}
	}

	fn unlock_cookie(&self, share: &Share) -> Option<String> {
		let hash = share.password_hash.as_deref()?;
		let mac = blake3::keyed_hash(&self.key, format!("{}\n{hash}", share.token).as_bytes());
		Some(mac.to_hex().to_string())
	}

	fn unlocked(&self, share: &Share, headers: &HeaderMap) -> bool {
		let Some(expected) = self.unlock_cookie(share) else {
			return true;
		};
		let name = cookie_name(&share.token);
		headers
			.get_all(header::COOKIE)
			.iter()
			.filter_map(|value| value.to_str().ok())
			.flat_map(|value| value.split(';'))
			.filter_map(|pair| pair.trim().split_once('='))
			.any(|(key, value)| {
				key == name
					&& blake3::Hash::from_hex(value).ok() == blake3::Hash::from_hex(&expected).ok()
			})
	}
}

enum Lookup {
	Found(Share),
	Expired,
	Missing,
}

fn cookie_name(token: &str) -> String {
	format!("sd_share_{token}")
}

// Owner API, behind the server login.

#[derive(Serialize)]
pub struct ShareView {
	token: String,
	url: String,
	path: String,
	name: String,
	is_dir: bool,
	created_at: i64,
	expires_at: Option<i64>,
	has_password: bool,
}

impl From<&Share> for ShareView {
	fn from(share: &Share) -> Self {
		Self {
			token: share.token.clone(),
			url: format!("/s/{}", share.token),
			path: share.path.to_string_lossy().into_owned(),
			name: share.name(),
			is_dir: share.path.is_dir(),
			created_at: share.created_at,
			expires_at: share.expires_at,
			has_password: share.password_hash.is_some(),
		}
	}
}

#[derive(Deserialize)]
pub struct ListQuery {
	path: Option<String>,
}

pub async fn list(State(state): State<AppState>, Query(query): Query<ListQuery>) -> Response {
	let wanted = match &query.path {
		Some(raw) => match state.files.resolve(raw).await {
			Ok(path) => Some(path),
			Err(response) => return response,
		},
		None => None,
	};
	let now = now();
	let shares = state.shares.shares.read().await;
	let mut views: Vec<(i64, ShareView)> = shares
		.values()
		.filter(|share| !share.expired(now))
		.filter(|share| wanted.as_ref().is_none_or(|path| &share.path == path))
		.map(|share| (share.created_at, ShareView::from(share)))
		.collect();
	views.sort_by_key(|(created_at, _)| std::cmp::Reverse(*created_at));
	Json(views.into_iter().map(|(_, view)| view).collect::<Vec<_>>()).into_response()
}

#[derive(Deserialize)]
pub struct CreateShare {
	path: String,
	expires_in_hours: Option<u32>,
	password: Option<String>,
}

pub async fn create(State(state): State<AppState>, Json(input): Json<CreateShare>) -> Response {
	let path = match state.files.resolve(&input.path).await {
		Ok(path) => path,
		Err(response) => return response,
	};
	let password_hash = match input.password.filter(|password| !password.is_empty()) {
		None => None,
		Some(password) => {
			let hashed = tokio::task::spawn_blocking(move || {
				let salt = SaltString::generate(&mut HashRng);
				Argon2::default()
					.hash_password(password.as_bytes(), &salt)
					.map(|hash| hash.to_string())
			})
			.await;
			match hashed {
				Ok(Ok(hash)) => Some(hash),
				_ => {
					return plain_status(
						StatusCode::INTERNAL_SERVER_ERROR,
						"could not hash password",
					)
				}
			}
		}
	};
	let mut token = [0u8; 18];
	rand::rngs::OsRng.fill_bytes(&mut token);
	let created_at = now();
	let share = Share {
		token: URL_SAFE_NO_PAD.encode(token),
		path,
		created_at,
		expires_at: input
			.expires_in_hours
			.map(|hours| created_at + i64::from(hours.min(TEN_YEARS_HOURS)) * 3600),
		password_hash,
	};

	let mut shares = state.shares.shares.write().await;
	shares.retain(|_, held| !held.expired(created_at));
	shares.insert(share.token.clone(), share.clone());
	if let Err(error) = state.shares.save(&shares).await {
		tracing::error!("could not save shares: {error}");
		shares.remove(&share.token);
		return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "could not save the link");
	}
	(StatusCode::CREATED, Json(ShareView::from(&share))).into_response()
}

pub async fn remove(State(state): State<AppState>, Path(token): Path<String>) -> Response {
	let mut shares = state.shares.shares.write().await;
	let Some(removed) = shares.remove(&token) else {
		return plain_status(StatusCode::NOT_FOUND, "no such link");
	};
	if let Err(error) = state.shares.save(&shares).await {
		tracing::error!("could not save shares: {error}");
		shares.insert(token, removed);
		return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "could not save");
	}
	StatusCode::NO_CONTENT.into_response()
}

// Public pages, outside the server login.

pub async fn public_root(
	State(state): State<AppState>,
	Path(token): Path<String>,
	headers: HeaderMap,
) -> Response {
	let share = match state.shares.get(&token).await {
		Lookup::Found(share) => share,
		Lookup::Expired => {
			return message_page(
				StatusCode::GONE,
				"This link has expired",
				"Ask the person who sent it for a new one.",
			)
		}
		Lookup::Missing => {
			return message_page(
				StatusCode::NOT_FOUND,
				"Link not found",
				"It may have been removed, or the address is incomplete.",
			)
		}
	};
	if !state.shares.unlocked(&share, &headers) {
		return password_page(&share, None);
	}
	if share.path.is_dir() {
		folder_page(&share, &share.path, &[]).await
	} else {
		file_page(&share).await
	}
}

#[derive(Deserialize)]
pub struct PathQuery {
	download: Option<String>,
}

pub async fn public_path(
	State(state): State<AppState>,
	Path((token, rest)): Path<(String, String)>,
	Query(query): Query<PathQuery>,
	request: Request,
) -> Response {
	let share = match state.shares.get(&token).await {
		Lookup::Found(share) => share,
		Lookup::Expired => {
			return message_page(
				StatusCode::GONE,
				"This link has expired",
				"Ask the person who sent it for a new one.",
			)
		}
		Lookup::Missing => {
			return message_page(
				StatusCode::NOT_FOUND,
				"Link not found",
				"It may have been removed, or the address is incomplete.",
			)
		}
	};
	if !state.shares.unlocked(&share, request.headers()) {
		return redirect(&format!("/s/{token}"));
	}
	let download = query.download.is_some();

	if !share.path.is_dir() {
		return serve_path(&share.path, request, download).await;
	}
	let Some(segments) = safe_segments(&rest) else {
		return message_page(
			StatusCode::NOT_FOUND,
			"Not found",
			"That item isn't part of this share.",
		);
	};
	let mut target = share.path.clone();
	target.extend(&segments);
	let Ok(target) = tokio::fs::canonicalize(&target).await else {
		return message_page(
			StatusCode::NOT_FOUND,
			"Not found",
			"That item isn't part of this share.",
		);
	};
	let target = short_form(target);
	if !target.starts_with(&share.path) {
		return message_page(
			StatusCode::NOT_FOUND,
			"Not found",
			"That item isn't part of this share.",
		);
	}
	if target.is_dir() {
		folder_page(&share, &target, &segments).await
	} else {
		serve_path(&target, request, download).await
	}
}

/// Hidden names (dotfiles, partial uploads) and parent references are not
/// reachable through a share.
fn safe_segments(rest: &str) -> Option<Vec<String>> {
	rest.split('/')
		.filter(|segment| !segment.is_empty())
		.map(|segment| {
			(!segment.starts_with('.') && !segment.contains('\\')).then(|| segment.to_string())
		})
		.collect()
}

#[derive(Deserialize)]
pub struct UnlockForm {
	password: String,
}

pub async fn unlock(
	State(state): State<AppState>,
	Path(token): Path<String>,
	ConnectInfo(peer): ConnectInfo<SocketAddr>,
	headers: HeaderMap,
	Form(form): Form<UnlockForm>,
) -> Response {
	let share = match state.shares.get(&token).await {
		Lookup::Found(share) => share,
		Lookup::Expired => {
			return message_page(
				StatusCode::GONE,
				"This link has expired",
				"Ask the person who sent it for a new one.",
			)
		}
		Lookup::Missing => {
			return message_page(
				StatusCode::NOT_FOUND,
				"Link not found",
				"It may have been removed, or the address is incomplete.",
			)
		}
	};
	let (Some(hash), Some(cookie)) = (
		share.password_hash.clone(),
		state.shares.unlock_cookie(&share),
	) else {
		return redirect(&format!("/s/{token}"));
	};
	let ip = throttle::client_ip(peer, &headers);
	if state.throttle.blocked(ip) {
		return password_page(
			&share,
			Some("Too many wrong passwords. Try again in 15 minutes."),
		);
	}
	let correct = tokio::task::spawn_blocking(move || {
		PasswordHash::new(&hash)
			.map(|parsed| {
				Argon2::default()
					.verify_password(form.password.as_bytes(), &parsed)
					.is_ok()
			})
			.unwrap_or(false)
	})
	.await
	.unwrap_or(false);
	if !correct {
		state.throttle.fail(ip);
		return password_page(&share, Some("That password isn't right."));
	}
	state.throttle.clear(ip);
	let secure = headers
		.get("x-forwarded-proto")
		.is_some_and(|proto| proto.as_bytes() == b"https");
	let set_cookie = format!(
		"{}={cookie}; Path=/s/{token}; HttpOnly; SameSite=Lax; Max-Age=2592000{}",
		cookie_name(&token),
		if secure { "; Secure" } else { "" },
	);
	let mut response = redirect(&format!("/s/{token}"));
	if let Ok(value) = HeaderValue::from_str(&set_cookie) {
		response.headers_mut().insert(header::SET_COOKIE, value);
	}
	response
}

// Rendering.

fn escape(text: &str) -> String {
	let mut out = String::with_capacity(text.len());
	for c in text.chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			'\'' => out.push_str("&#39;"),
			c => out.push(c),
		}
	}
	out
}

fn human_size(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
	let mut size = bytes as f64;
	let mut unit = 0;
	while size >= 1000.0 && unit < UNITS.len() - 1 {
		size /= 1000.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes} B")
	} else {
		format!("{size:.1} {}", UNITS[unit])
	}
}

fn expiry_note(share: &Share) -> String {
	let Some(at) = share.expires_at else {
		return String::new();
	};
	let left = at - now();
	let text = match left {
		s if s < 3600 => "Expires in less than an hour".to_string(),
		s if s < 7200 => "Expires in 1 hour".to_string(),
		s if s < 86_400 => format!("Expires in {} hours", s / 3600),
		s => format!("Expires in {} days", (s + 86_399) / 86_400),
	};
	format!(r#"<p class="note">{text}</p>"#)
}

const STYLE: &str = r#"
:root{color-scheme:dark;--bg:#111114;--card:#1b1b20;--line:#2a2a31;--text:#ececf1;--muted:#8d8d99;--accent:#2599ff}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:15px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",system-ui,sans-serif;-webkit-font-smoothing:antialiased}
main{max-width:760px;margin:0 auto;padding:28px 16px 48px}
.brand{display:flex;align-items:center;gap:8px;color:var(--muted);font-size:13px;margin-bottom:22px}
.brand b{color:var(--text);font-weight:600}
.card{background:var(--card);border:1px solid var(--line);border-radius:14px;overflow:hidden}
.head{padding:18px 18px 14px}
h1{font-size:19px;margin:0 0 4px;word-break:break-word}
.meta,.note{color:var(--muted);font-size:13px;margin:0}
.note{margin-top:6px}
.preview{background:#0b0b0d;border-top:1px solid var(--line);border-bottom:1px solid var(--line);display:flex;justify-content:center}
.preview img,.preview video{display:block;max-width:100%;max-height:70vh}
.preview audio{width:100%;margin:18px}
.actions{padding:14px 18px 18px}
.button{display:inline-flex;align-items:center;justify-content:center;min-height:44px;padding:0 20px;border-radius:10px;background:var(--accent);color:#fff;font-weight:600;text-decoration:none;border:0;font-size:15px;cursor:pointer}
.crumbs{padding:14px 18px;border-bottom:1px solid var(--line);font-size:14px;color:var(--muted);word-break:break-word}
.crumbs a{color:var(--text);text-decoration:none}
ul{list-style:none;margin:0;padding:0}
li{display:flex;align-items:center;border-bottom:1px solid var(--line)}
li:last-child{border-bottom:0}
li a.name{flex:1;min-width:0;display:flex;align-items:center;gap:12px;padding:12px 18px;color:var(--text);text-decoration:none}
li a.name span{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
li .size{color:var(--muted);font-size:13px;white-space:nowrap}
li a.get{padding:12px 18px;color:var(--accent);text-decoration:none;font-size:14px}
.icon{flex:none;width:28px;height:28px;border-radius:7px;background:#26262e;display:grid;place-items:center;font-size:14px}
.empty{padding:28px 18px;color:var(--muted)}
form{padding:4px 18px 18px;display:flex;flex-direction:column;gap:12px}
input{min-height:44px;border-radius:10px;border:1px solid var(--line);background:#121216;color:var(--text);padding:0 14px;font-size:16px}
.error{color:#ff6b6b;font-size:14px;margin:0}
"#;

fn html(status: StatusCode, title: &str, body: &str) -> Response {
	let page = format!(
		r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="robots" content="noindex"><title>{title}</title><style>{STYLE}</style></head><body><main><div class="brand"><b>Spacedrive</b> shared with you</div>{body}</main></body></html>"#,
		title = escape(title),
	);
	(
		status,
		[
			(header::CONTENT_TYPE, "text/html; charset=utf-8"),
			(header::CACHE_CONTROL, "no-store"),
			(header::REFERRER_POLICY, "no-referrer"),
			(header::X_FRAME_OPTIONS, "DENY"),
			(
				header::CONTENT_SECURITY_POLICY,
				"default-src 'self'; style-src 'unsafe-inline'; form-action 'self'",
			),
		],
		page,
	)
		.into_response()
}

fn redirect(location: &str) -> Response {
	(
		StatusCode::SEE_OTHER,
		[(header::LOCATION, location.to_string())],
	)
		.into_response()
}

fn message_page(status: StatusCode, title: &str, detail: &str) -> Response {
	html(
		status,
		title,
		&format!(
			r#"<div class="card"><div class="head"><h1>{}</h1><p class="meta">{}</p></div></div>"#,
			escape(title),
			escape(detail)
		),
	)
}

fn password_page(share: &Share, error: Option<&str>) -> Response {
	let error = error
		.map(|text| format!(r#"<p class="error">{}</p>"#, escape(text)))
		.unwrap_or_default();
	html(
		if error.is_empty() {
			StatusCode::OK
		} else {
			StatusCode::UNAUTHORIZED
		},
		&share.name(),
		&format!(
			r#"<div class="card"><div class="head"><h1>{name}</h1><p class="meta">This link is password protected.</p></div><form method="post" action="/s/{token}"><input type="password" name="password" placeholder="Password" autocomplete="current-password" autofocus required>{error}<button class="button" type="submit">Open</button></form></div>"#,
			name = escape(&share.name()),
			token = share.token,
		),
	)
}

fn icon_for(path: &FsPath, is_dir: bool) -> &'static str {
	if is_dir {
		return "📁";
	}
	match mime_guess::from_path(path).first_raw().unwrap_or("") {
		mime if mime.starts_with("image/") => "🖼",
		mime if mime.starts_with("video/") => "🎬",
		mime if mime.starts_with("audio/") => "🎵",
		"application/pdf" => "📄",
		"application/zip" | "application/x-tar" | "application/gzip" => "🗜",
		_ => "📄",
	}
}

async fn file_page(share: &Share) -> Response {
	let name = share.name();
	let size = tokio::fs::metadata(&share.path)
		.await
		.map(|meta| human_size(meta.len()))
		.unwrap_or_default();
	let src = format!("/s/{}/{}", share.token, percent_encode(&name));
	let mime = mime_guess::from_path(&share.path).first_raw().unwrap_or("");
	let preview = match mime {
		m if m.starts_with("image/") && m != "image/svg+xml" => {
			format!(r#"<div class="preview"><img src="{src}" alt=""></div>"#)
		}
		m if m.starts_with("video/") => {
			format!(
				r#"<div class="preview"><video src="{src}" controls playsinline preload="metadata"></video></div>"#
			)
		}
		m if m.starts_with("audio/") => {
			format!(
				r#"<div class="preview"><audio src="{src}" controls preload="metadata"></audio></div>"#
			)
		}
		_ => String::new(),
	};
	html(
		StatusCode::OK,
		&name,
		&format!(
			r#"<div class="card"><div class="head"><h1>{name}</h1><p class="meta">{size}</p>{expiry}</div>{preview}<div class="actions"><a class="button" href="{src}?download=1">Download</a></div></div>"#,
			name = escape(&name),
			expiry = expiry_note(share),
		),
	)
}

async fn folder_page(share: &Share, dir: &FsPath, segments: &[String]) -> Response {
	let Ok(mut reader) = tokio::fs::read_dir(dir).await else {
		return message_page(
			StatusCode::NOT_FOUND,
			"Not found",
			"This folder can't be read right now.",
		);
	};
	let mut entries = Vec::new();
	while let Ok(Some(entry)) = reader.next_entry().await {
		let name = entry.file_name().to_string_lossy().into_owned();
		if name.starts_with('.') {
			continue;
		}
		let Ok(meta) = tokio::fs::metadata(entry.path()).await else {
			continue;
		};
		entries.push((meta.is_dir(), name, meta.len()));
	}
	entries.sort_by(|a, b| {
		b.0.cmp(&a.0)
			.then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
	});

	let base = std::iter::once(format!("/s/{}", share.token))
		.chain(segments.iter().map(|segment| percent_encode(segment)))
		.collect::<Vec<_>>()
		.join("/");

	let mut crumbs = format!(
		r#"<a href="/s/{}">{}</a>"#,
		share.token,
		escape(&share.name())
	);
	let mut href = format!("/s/{}", share.token);
	for segment in segments {
		href = format!("{href}/{}", percent_encode(segment));
		crumbs.push_str(&format!(r#" / <a href="{href}">{}</a>"#, escape(segment)));
	}

	let rows = if entries.is_empty() {
		r#"<div class="empty">This folder is empty.</div>"#.to_string()
	} else {
		let items: String = entries
			.iter()
			.map(|(is_dir, name, size)| {
				let link = format!("{base}/{}", percent_encode(name));
				let icon = icon_for(FsPath::new(name), *is_dir);
				if *is_dir {
					format!(
						r#"<li><a class="name" href="{link}"><i class="icon">{icon}</i><span>{}</span></a></li>"#,
						escape(name)
					)
				} else {
					format!(
						r#"<li><a class="name" href="{link}"><i class="icon">{icon}</i><span>{}</span></a><span class="size">{}</span><a class="get" href="{link}?download=1">Download</a></li>"#,
						escape(name),
						human_size(*size)
					)
				}
			})
			.collect();
		format!("<ul>{items}</ul>")
	};

	html(
		StatusCode::OK,
		&share.name(),
		&format!(
			r#"<div class="card"><div class="crumbs">{crumbs}{expiry}</div>{rows}</div>"#,
			expiry = expiry_note(share),
		),
	)
}
