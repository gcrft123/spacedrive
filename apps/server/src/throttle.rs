//! Per-client lockout after repeated wrong passwords, for the server login
//! and for password-protected share links.

use std::{
	collections::HashMap,
	net::{IpAddr, SocketAddr},
	sync::Mutex,
	time::{Duration, Instant},
};

use axum::http::HeaderMap;

const LIMIT: u32 = 10;
const WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct Throttle {
	failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl Throttle {
	pub fn blocked(&self, ip: IpAddr) -> bool {
		let failures = self.failures.lock().expect("throttle lock poisoned");
		failures
			.get(&ip)
			.is_some_and(|(count, since)| *count >= LIMIT && since.elapsed() < WINDOW)
	}

	pub fn fail(&self, ip: IpAddr) {
		let mut failures = self.failures.lock().expect("throttle lock poisoned");
		if failures.len() > 10_000 {
			failures.retain(|_, (_, since)| since.elapsed() < WINDOW);
		}
		let entry = failures.entry(ip).or_insert((0, Instant::now()));
		if entry.1.elapsed() >= WINDOW {
			*entry = (0, Instant::now());
		}
		entry.0 += 1;
	}

	pub fn clear(&self, ip: IpAddr) {
		self.failures
			.lock()
			.expect("throttle lock poisoned")
			.remove(&ip);
	}
}

/// Behind a tunnel or proxy on this machine every connection comes from
/// loopback, so the proxy's client header names the real client. The header
/// is trusted only from loopback; a remote client could otherwise forge it
/// to dodge the lockout.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
	if peer.ip().is_loopback() {
		let forwarded = headers
			.get("cf-connecting-ip")
			.and_then(|value| value.to_str().ok())
			.and_then(|value| value.trim().parse().ok());
		if let Some(ip) = forwarded {
			return ip;
		}
	}
	peer.ip()
}
