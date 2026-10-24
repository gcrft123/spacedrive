import {UploadSimple} from '@phosphor-icons/react';
import {CircleButton, toast} from '@spacedrive/primitives';
import clsx from 'clsx';
import {useRef, useState, type DragEvent, type ReactNode} from 'react';
import {usePlatform} from '../../../contexts/PlatformContext';
import {useExplorer} from '../context';

/**
 * Upload from the browser into the folder being shown, on platforms that
 * accept uploads (the web server). The folder must be on the server itself:
 * a path on another device names nothing there.
 */
export function useUpload() {
	const platform = usePlatform();
	const {currentPath, mode, devices} = useExplorer();

	let directory: string | null = null;
	if (
		platform.uploadFiles &&
		mode.type === 'browse' &&
		currentPath &&
		'Physical' in currentPath
	) {
		const here = [...devices.values()].find((device) => device.is_current);
		const path = currentPath.Physical.path;
		if (
			(!here || here.slug === currentPath.Physical.device_slug) &&
			(platform.allowsFileAccess?.(path) ?? true)
		) {
			directory = path;
		}
	}

	const upload = async (files: File[]) => {
		if (!directory || !platform.uploadFiles || files.length === 0) return;
		const label =
			files.length === 1 ? `“${files[0].name}”` : `${files.length} files`;
		const id = toast.info(`Uploading ${label}`, {duration: Infinity});
		try {
			await platform.uploadFiles(directory, files, (sent, total) => {
				const percent =
					total > 0 ? Math.floor((sent / total) * 100) : 100;
				toast.info(`Uploading ${label} · ${percent}%`, {
					id,
					duration: Infinity
				});
			});
			toast.success(`Uploaded ${label}`, {id, duration: 4000});
		} catch (error) {
			toast.error(
				`Upload failed: ${error instanceof Error ? error.message : error}`,
				{
					id,
					duration: 8000
				}
			);
		}
	};

	return {directory, upload};
}

export function UploadButton() {
	const {directory, upload} = useUpload();
	const input = useRef<HTMLInputElement>(null);
	if (!directory) return null;
	return (
		<>
			<CircleButton
				icon={UploadSimple}
				onClick={() => input.current?.click()}
			/>
			<input
				ref={input}
				type="file"
				multiple
				hidden
				onChange={(event) => {
					const files = Array.from(event.target.files ?? []);
					event.target.value = '';
					void upload(files);
				}}
			/>
		</>
	);
}

/** Files dragged in from the operating system, not items dragged within the app. */
function carriesFiles(event: DragEvent) {
	return Array.from(event.dataTransfer.types).includes('Files');
}

export function UploadDropZone({children}: {children: ReactNode}) {
	const {directory, upload} = useUpload();
	const [depth, setDepth] = useState(0);
	if (!directory) return <>{children}</>;

	const folder = directory.split('/').filter(Boolean).pop() ?? directory;
	return (
		<div
			className="relative flex min-h-0 flex-1 flex-col"
			onDragEnter={(event) => {
				if (!carriesFiles(event)) return;
				event.preventDefault();
				setDepth((value) => value + 1);
			}}
			onDragOver={(event) => {
				if (!carriesFiles(event)) return;
				event.preventDefault();
				event.dataTransfer.dropEffect = 'copy';
			}}
			onDragLeave={(event) => {
				if (!carriesFiles(event)) return;
				setDepth((value) => Math.max(0, value - 1));
			}}
			onDrop={(event) => {
				if (!carriesFiles(event)) return;
				event.preventDefault();
				setDepth(0);
				void upload(Array.from(event.dataTransfer.files));
			}}
		>
			{children}
			<div
				className={clsx(
					'border-accent bg-accent/10 pointer-events-none absolute inset-2 flex items-center justify-center rounded-xl border-2 border-dashed transition-opacity',
					depth > 0 ? 'opacity-100' : 'opacity-0'
				)}
			>
				<div className="bg-app-box text-ink flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-medium shadow-lg">
					<UploadSimple size={16} weight="bold" />
					Drop to upload to {folder}
				</div>
			</div>
		</div>
	);
}
