import type { Platform } from "@sd/interface";
import {
	allowsFileAccess,
	downloadFiles,
	fileRoots,
	fileUrl,
	shareLinks,
	uploadFiles,
} from "./server-files";

/**
 * Web platform implementation for Spacedrive server
 *
 * This provides a minimal platform abstraction for the web client.
 * Unlike Tauri, web platform cannot access native file system or daemon state directly.
 */
export const platform: Platform = {
	platform: "web",

	openLink(url: string) {
		window.open(url, "_blank", "noopener,noreferrer");
	},

	// sd-server serves the UI, RPC, and sidecars from a single origin, so the
	// sidecar base URL is wherever this page was loaded from. Without this the
	// ServerContext never gets a serverUrl and thumbnails silently never load.
	async getDaemonStatus() {
		return {
			is_running: true,
			socket_path: "",
			server_url: window.location.origin,
			started_by_us: false,
		};
	},

	confirm(message: string, callback: (result: boolean) => void) {
		callback(window.confirm(message));
	},

	// Originals stream from sd-server, which serves only paths inside its
	// SD_FILE_ROOTS. Previews, "Open", downloads and uploads all go through it.
	convertFileSrc: (path) => fileUrl(path),

	async openPathDefault(path) {
		window.open(fileUrl(path), "_blank", "noopener,noreferrer");
		return { status: "success" };
	},

	get fileRoots() {
		return fileRoots();
	},
	allowsFileAccess,
	downloadFiles,
	uploadFiles,
	shareLinks,

	// Web-specific implementations (no native capabilities)
	// File pickers, daemon control, etc. are not available on web
};
