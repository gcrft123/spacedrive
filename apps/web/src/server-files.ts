import type {ShareLink, ShareLinkApi} from '@sd/interface';

/** Uploads go up in chunks this size: under Cloudflare's 100 MB request cap,
 * and small enough that a slow connection finishes each one well inside a
 * proxy's response timeout. */
const CHUNK = 8 * 1024 * 1024;
const RETRIES = 5;

let roots: string[] = [];

/** Fetch the folders sd-server serves files from. Called once before the UI
 * renders, so access checks are synchronous. */
export async function loadRoots() {
	try {
		const response = await fetch('/api/roots');
		if (response.ok) roots = (await response.json()) as string[];
	} catch {
		roots = [];
	}
}

/** macOS reports the data volume's paths under /System/Volumes/Data as well;
 * the server compares in the short form, so this does too. */
function shortForm(path: string) {
	return path.startsWith('/System/Volumes/Data/')
		? path.slice('/System/Volumes/Data'.length)
		: path;
}

export function allowsFileAccess(path: string) {
	const target = shortForm(path);
	return roots.some(
		(root) =>
			target === root || target.startsWith(`${root.replace(/\/$/, '')}/`)
	);
}

export function fileUrl(path: string, download = false) {
	const query = new URLSearchParams({path});
	if (download) query.set('download', '1');
	return `/api/file?${query}`;
}

export function downloadFiles(paths: string[]) {
	for (const path of paths) {
		const link = document.createElement('a');
		link.href = fileUrl(path, true);
		link.download = '';
		document.body.append(link);
		link.click();
		link.remove();
	}
}

async function failure(response: Response) {
	const text = (await response.text()).trim();
	return new Error(text || `${response.status} ${response.statusText}`);
}

type UploadReply =
	| {status: 'partial'; received: number}
	| {status: 'complete'; path: string};

async function uploadOne(
	directory: string,
	file: File,
	onSent: (bytes: number) => void
) {
	const id = crypto.randomUUID();
	let offset = 0;
	let attempts = 0;
	do {
		const end = Math.min(offset + CHUNK, file.size);
		const query = new URLSearchParams({
			dir: directory,
			name: file.name,
			id,
			offset: String(offset),
			total: String(file.size)
		});
		let response: Response;
		try {
			response = await fetch(`/api/upload?${query}`, {
				method: 'PUT',
				body: file.slice(offset, end)
			});
		} catch (error) {
			if (++attempts > RETRIES) throw error;
			await new Promise((resolve) =>
				setTimeout(resolve, 1000 * attempts)
			);
			// The server answers 409 with what actually landed, so retrying at
			// the same offset resumes rather than duplicating bytes.
			continue;
		}
		if (response.status === 409) {
			const reply = (await response.json()) as UploadReply;
			if (reply.status !== 'partial' || ++attempts > RETRIES)
				throw await failure(response);
			onSent(reply.received - offset);
			offset = reply.received;
			continue;
		}
		if (!response.ok) throw await failure(response);
		const reply = (await response.json()) as UploadReply;
		onSent(end - offset);
		attempts = 0;
		if (reply.status === 'complete') return;
		offset = reply.received;
	} while (offset <= file.size);
}

export async function uploadFiles(
	directory: string,
	files: File[],
	onProgress?: (sent: number, total: number) => void
) {
	const total = files.reduce((sum, file) => sum + file.size, 0);
	let sent = 0;
	for (const file of files) {
		await uploadOne(directory, file, (bytes) => {
			sent += bytes;
			onProgress?.(sent, total);
		});
	}
}

function absolute(link: ShareLink): ShareLink {
	return {...link, url: new URL(link.url, window.location.origin).toString()};
}

export const shareLinks: ShareLinkApi = {
	async list(path) {
		const response = await fetch(
			`/api/shares?${new URLSearchParams({path})}`
		);
		if (!response.ok) throw await failure(response);
		return ((await response.json()) as ShareLink[]).map(absolute);
	},
	async create(path, {expiresInHours, password}) {
		const response = await fetch('/api/shares', {
			method: 'POST',
			headers: {'Content-Type': 'application/json'},
			body: JSON.stringify({
				path,
				expires_in_hours: expiresInHours,
				password
			})
		});
		if (!response.ok) throw await failure(response);
		return absolute((await response.json()) as ShareLink);
	},
	async remove(token) {
		const response = await fetch(
			`/api/shares/${encodeURIComponent(token)}`,
			{method: 'DELETE'}
		);
		if (!response.ok && response.status !== 404)
			throw await failure(response);
	}
};
