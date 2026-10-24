//! Original-file access for the web UI: streaming downloads with range
//! support and resumable chunked uploads.
//!
//! The daemon's RPC can index any path, but raw bytes only leave the server,
//! and uploads only land, inside the configured file roots.

use std::path::{Path, PathBuf};

use axum::{
	body::Body,
	extract::{Query, Request, State},
	http::{header, HeaderValue, StatusCode},
	response::{IntoResponse, Response},
	Json,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::{plain_status, AppState};

/// Partial uploads are hidden files next to their destination, so a rename
/// finishes them without copying across volumes.
const PARTIAL_PREFIX: &str = ".sdupload-";

pub struct FileRoots(Vec<PathBuf>);

impl FileRoots {
	/// Roots are kept as given and canonicalized per request, so a root on a
	/// network mount that appears after startup still works.
	pub fn parse(raw: Option<&str>) -> Self {
		let roots = raw
			.unwrap_or_default()
			.split(',')
			.map(str::trim)
			.filter(|root| !root.is_empty())
			.map(PathBuf::from)
			.filter(|root| {
				let absolute = root.is_absolute();
				if !absolute {
					tracing::warn!("ignoring relative file root {}", root.display());
				}
				absolute
			})
			.collect();
		Self(roots)
	}

	pub fn is_empty(&self) -> bool {
		self.0.is_empty()
	}

	/// The roots as they currently resolve on disk; a root that is missing
	/// (an unmounted drive) is left out.
	pub async fn current(&self) -> Vec<PathBuf> {
		let mut roots = Vec::new();
		for root in &self.0 {
			if let Ok(root) = tokio::fs::canonicalize(root).await {
				roots.push(short_form(root));
			}
		}
		roots
	}

	/// Canonicalize `raw` and return it only if it lies inside a root.
	pub async fn resolve(&self, raw: &str) -> Result<PathBuf, Response> {
		let path = Path::new(raw);
		if !path.is_absolute() {
			return Err(plain_status(
				StatusCode::BAD_REQUEST,
				"path must be absolute",
			));
		}
		let Ok(path) = tokio::fs::canonicalize(path).await else {
			return Err(plain_status(StatusCode::NOT_FOUND, "no such file"));
		};
		let path = short_form(path);
		if self
			.current()
			.await
			.iter()
			.any(|root| path.starts_with(root))
		{
			return Ok(path);
		}
		Err(plain_status(
			StatusCode::FORBIDDEN,
			"path is outside the shared folders",
		))
	}
}

/// macOS shows the data volume both at `/` and under `/System/Volumes/Data`
/// (firmlinks), and Spacedrive reports the long form; canonicalizing keeps
/// whichever form it is given. Paths compare in the short form.
pub fn short_form(path: PathBuf) -> PathBuf {
	#[cfg(target_os = "macos")]
	if let Ok(rest) = path.strip_prefix("/System/Volumes/Data") {
		return Path::new("/").join(rest);
	}
	path
}

pub async fn get_roots(State(state): State<AppState>) -> Json<Vec<String>> {
	Json(
		state
			.files
			.current()
			.await
			.into_iter()
			.map(|root| root.to_string_lossy().into_owned())
			.collect(),
	)
}

/// RFC 3986 unreserved characters pass through; everything else is escaped.
/// Used for `filename*` and for building share URLs.
pub fn percent_encode(value: &str) -> String {
	let mut out = String::with_capacity(value.len());
	for byte in value.bytes() {
		if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
			out.push(byte as char);
		} else {
			out.push_str(&format!("%{byte:02X}"));
		}
	}
	out
}

/// Stream a file with range and conditional-request support. Files that a
/// browser would execute (HTML, SVG, XML) are always downloaded and
/// sandboxed, because they are served from the app's origin. Other types
/// skip the sandbox, which would stop Chrome's inline PDF viewer.
pub async fn serve_path(path: &Path, request: Request, download: bool) -> Response {
	let name = path
		.file_name()
		.map(|name| name.to_string_lossy().into_owned())
		.unwrap_or_default();
	let active = matches!(
		mime_guess::from_path(path).first_raw(),
		Some(
			"text/html"
				| "image/svg+xml"
				| "application/xhtml+xml"
				| "text/xml" | "application/xml"
		)
	);
	let mut response = match ServeFile::new(path).oneshot(request).await {
		Ok(response) => response.map(Body::new),
		Err(never) => match never {},
	};
	let headers = response.headers_mut();
	headers.insert(
		"x-content-type-options",
		HeaderValue::from_static("nosniff"),
	);
	if active {
		headers.insert(
			header::CONTENT_SECURITY_POLICY,
			HeaderValue::from_static("sandbox"),
		);
	}
	let disposition = if download || active {
		"attachment"
	} else {
		"inline"
	};
	if let Ok(value) = HeaderValue::from_str(&format!(
		"{disposition}; filename*=UTF-8''{}",
		percent_encode(&name)
	)) {
		headers.insert(header::CONTENT_DISPOSITION, value);
	}
	response
}

#[derive(Deserialize)]
pub struct FileQuery {
	path: String,
	download: Option<String>,
}

pub async fn get_file(
	State(state): State<AppState>,
	Query(query): Query<FileQuery>,
	request: Request,
) -> Response {
	let path = match state.files.resolve(&query.path).await {
		Ok(path) => path,
		Err(response) => return response,
	};
	if path.is_dir() {
		return plain_status(StatusCode::BAD_REQUEST, "folders cannot be downloaded");
	}
	serve_path(&path, request, query.download.is_some()).await
}

#[derive(Deserialize)]
pub struct UploadQuery {
	dir: String,
	name: String,
	/// Client-chosen id; it keeps two uploads of one name apart.
	id: uuid::Uuid,
	offset: u64,
	total: u64,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum UploadReply {
	/// More chunks are expected. On a 409 this is where the client resumes.
	Partial {
		received: u64,
	},
	Complete {
		path: String,
	},
}

pub fn valid_name(name: &str) -> bool {
	!name.is_empty()
		&& name.len() <= 255
		&& name != "."
		&& name != ".."
		&& !name.contains(['/', '\\', '\0'])
		&& !name.starts_with(PARTIAL_PREFIX)
}

/// Append one chunk to a partial upload, renaming it into place once
/// `total` bytes have arrived. A chunk whose offset disagrees with the bytes
/// on disk is refused with 409 and the current length, so a client whose
/// connection dropped mid-chunk resumes from what actually landed.
pub async fn put_upload(
	State(state): State<AppState>,
	Query(query): Query<UploadQuery>,
	body: Body,
) -> Response {
	if !valid_name(&query.name) {
		return plain_status(StatusCode::BAD_REQUEST, "invalid file name");
	}
	if query.offset > query.total {
		return plain_status(StatusCode::BAD_REQUEST, "offset is past the end");
	}
	let dir = match state.files.resolve(&query.dir).await {
		Ok(dir) => dir,
		Err(response) => return response,
	};
	if !dir.is_dir() {
		return plain_status(StatusCode::BAD_REQUEST, "upload target is not a folder");
	}

	let partial = dir.join(format!("{PARTIAL_PREFIX}{}", query.id.simple()));
	let opened = if query.offset == 0 {
		tokio::fs::File::create(&partial).await
	} else {
		let on_disk = tokio::fs::metadata(&partial)
			.await
			.map(|meta| meta.len())
			.unwrap_or(0);
		if on_disk != query.offset {
			return (
				StatusCode::CONFLICT,
				Json(UploadReply::Partial { received: on_disk }),
			)
				.into_response();
		}
		tokio::fs::OpenOptions::new()
			.append(true)
			.open(&partial)
			.await
	};
	let Ok(mut file) = opened else {
		return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "could not open upload");
	};

	let mut received = query.offset;
	let mut stream = body.into_data_stream();
	while let Some(chunk) = stream.next().await {
		let Ok(chunk) = chunk else {
			let _ = file.flush().await;
			return plain_status(StatusCode::BAD_REQUEST, "upload interrupted");
		};
		received += chunk.len() as u64;
		if received > query.total {
			drop(file);
			let _ = tokio::fs::remove_file(&partial).await;
			return plain_status(StatusCode::BAD_REQUEST, "more data than declared");
		}
		if file.write_all(&chunk).await.is_err() {
			return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "write failed");
		}
	}
	if file.flush().await.is_err() {
		return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "write failed");
	}
	drop(file);

	if received < query.total {
		return Json(UploadReply::Partial { received }).into_response();
	}
	let target = free_name(&dir, &query.name).await;
	if tokio::fs::rename(&partial, &target).await.is_err() {
		return plain_status(StatusCode::INTERNAL_SERVER_ERROR, "could not finish upload");
	}
	Json(UploadReply::Complete {
		path: target.to_string_lossy().into_owned(),
	})
	.into_response()
}

/// `name`, or the first of `name (1)`, `name (2)`, … that is not taken, so
/// an upload never replaces an existing file.
async fn free_name(dir: &Path, name: &str) -> PathBuf {
	let first = dir.join(name);
	if tokio::fs::symlink_metadata(&first).await.is_err() {
		return first;
	}
	let (stem, extension) = match name.rsplit_once('.') {
		Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
		_ => (name, None),
	};
	let mut n = 1u32;
	loop {
		let candidate = dir.join(match extension {
			Some(extension) => format!("{stem} ({n}).{extension}"),
			None => format!("{stem} ({n})"),
		});
		if tokio::fs::symlink_metadata(&candidate).await.is_err() {
			return candidate;
		}
		n += 1;
	}
}
