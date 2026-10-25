//! Keep the index of the file roots current. Spacedrive learns about changes
//! from filesystem events, which a network mount does not reliably deliver,
//! so a folder looks unchanged until it is scanned again. The server asks for
//! that scan itself: of a folder as soon as an upload lands in it, and of
//! every root on a timer, for changes made some other way.

use std::{sync::Arc, time::Duration};

use serde_json::{json, Value};
use tokio::{
	io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
	net::TcpStream,
};
use tracing::warn;

use crate::files::FileRoots;

const INTERVAL: Duration = Duration::from_secs(30 * 60);

async fn call(socket_addr: &str, request: &Value) -> std::io::Result<Value> {
	let mut stream = TcpStream::connect(socket_addr).await?;
	stream.write_all(format!("{request}\n").as_bytes()).await?;
	let mut line = String::new();
	BufReader::new(stream).read_line(&mut line).await?;
	serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// Start an index job over `paths` in every library.
pub async fn scan(socket_addr: String, paths: Vec<String>, recursive: bool) {
	let listed = call(
		&socket_addr,
		&json!({"Query": {"method": "query:libraries.list", "library_id": null, "payload": {"include_stats": false}}}),
	)
	.await;
	let libraries = match listed {
		Ok(reply) => reply["JsonOk"]
			.as_array()
			.map(|list| {
				list.iter()
					.filter_map(|library| library["id"].as_str().map(String::from))
					.collect::<Vec<_>>()
			})
			.unwrap_or_default(),
		Err(error) => {
			warn!("could not list libraries to rescan {paths:?}: {error}");
			return;
		}
	};
	let scope = if recursive { "Recursive" } else { "Current" };
	for library in libraries {
		let request = json!({"Action": {
			"method": "action:indexing.start.input",
			"library_id": library,
			"payload": {"library_id": library, "paths": paths, "scope": scope, "include_hidden": false},
		}});
		match call(&socket_addr, &request).await {
			Ok(reply) if reply.get("JsonOk").is_some() => {}
			Ok(reply) => warn!("rescan of {paths:?} refused: {reply}"),
			Err(error) => warn!("rescan of {paths:?} failed: {error}"),
		}
	}
}

/// Rescan every root on a timer. The first scan comes one interval after
/// startup, since the daemon scans on its own when it starts.
pub fn every_interval(socket_addr: String, files: Arc<FileRoots>) {
	tokio::spawn(async move {
		let mut tick = tokio::time::interval(INTERVAL);
		tick.tick().await;
		loop {
			tick.tick().await;
			let roots: Vec<String> = files
				.current()
				.await
				.into_iter()
				.map(|root| root.to_string_lossy().into_owned())
				.collect();
			if !roots.is_empty() {
				scan(socket_addr.clone(), roots, true).await;
			}
		}
	});
}
