import {Copy, LinkSimple, LockSimple, Trash} from '@phosphor-icons/react';
import {
	Button,
	Dialog,
	dialogManager,
	PasswordInput,
	Select,
	SelectOption,
	toast,
	useDialog,
	type UseDialogProps
} from '@spacedrive/primitives';
import {useMutation, useQuery, useQueryClient} from '@tanstack/react-query';
import {useState} from 'react';
import {useForm} from 'react-hook-form';
import {
	usePlatform,
	type ShareLink,
	type ShareLinkApi
} from '../../contexts/PlatformContext';

interface ShareLinkDialogProps extends UseDialogProps {
	paths: string[];
	api: ShareLinkApi;
}

const EXPIRY = [
	{value: '24', label: '1 day'},
	{value: '168', label: '7 days'},
	{value: '720', label: '30 days'},
	{value: 'never', label: 'Never'}
];

/**
 * Share by link: anyone with the address can open the file or folder,
 * with no account. Opens on platforms that serve links (the web server);
 * elsewhere `share` falls back to the native share sheet.
 */
export function useShare() {
	const platform = usePlatform();
	const api = platform.shareLinks;
	return {
		canShare: (paths: string[]) =>
			api
				? paths.length > 0 &&
					paths.every(
						(path) => platform.allowsFileAccess?.(path) ?? true
					)
				: !!platform.shareFiles,
		share: async (paths: string[]) => {
			if (api) {
				await dialogManager.create((props: UseDialogProps) => (
					<ShareLinkDialog {...props} paths={paths} api={api} />
				));
			} else if (platform.shareFiles) {
				await platform.shareFiles(paths);
			}
		}
	};
}

function nameOf(path: string) {
	return path.split('/').filter(Boolean).pop() ?? path;
}

function expiryText(link: ShareLink) {
	if (link.expires_at === null) return 'Never expires';
	const days = Math.round((link.expires_at * 1000 - Date.now()) / 86_400_000);
	if (days < 1) return 'Expires today';
	if (days === 1) return 'Expires tomorrow';
	return `Expires in ${days} days`;
}

async function copy(url: string) {
	try {
		await navigator.clipboard.writeText(url);
		toast.success('Link copied');
	} catch {
		toast.error("Couldn't copy the link");
	}
}

function ShareLinkDialog(props: ShareLinkDialogProps) {
	const dialog = useDialog(props);
	const form = useForm();
	const queryClient = useQueryClient();
	const [expiry, setExpiry] = useState('168');
	const [password, setPassword] = useState('');
	const queryKey = ['shareLinks', ...props.paths];
	const close = () => dialogManager.setState(props.id, {open: false});

	const links = useQuery({
		queryKey,
		queryFn: async () =>
			(
				await Promise.all(
					props.paths.map((path) => props.api.list(path))
				)
			).flat()
	});

	const create = useMutation({
		mutationFn: () =>
			Promise.all(
				props.paths.map((path) =>
					props.api.create(path, {
						expiresInHours:
							expiry === 'never' ? null : Number(expiry),
						password: password || null
					})
				)
			),
		onSuccess: async (created) => {
			setPassword('');
			await queryClient.invalidateQueries({queryKey});
			if (created.length === 1) await copy(created[0].url);
		},
		onError: (error) =>
			toast.error(
				`Couldn't create the link: ${error instanceof Error ? error.message : error}`
			)
	});

	const remove = useMutation({
		mutationFn: (token: string) => props.api.remove(token),
		onSuccess: () => queryClient.invalidateQueries({queryKey}),
		onError: (error) =>
			toast.error(
				`Couldn't remove the link: ${error instanceof Error ? error.message : error}`
			)
	});

	const title =
		props.paths.length === 1
			? `Share “${nameOf(props.paths[0])}”`
			: `Share ${props.paths.length} items`;

	return (
		<Dialog
			dialog={dialog}
			form={form}
			title={title}
			icon={<LinkSimple size={20} weight="bold" />}
			ctaLabel={props.paths.length === 1 ? 'Create link' : 'Create links'}
			loading={create.isPending}
			submitDisabled={create.isPending}
			onSubmit={form.handleSubmit(() => create.mutateAsync())}
			onCancelled={close}
			cancelLabel="Done"
			formClassName="!min-w-[480px] !max-w-[480px]"
		>
			<div className="space-y-4 py-1">
				<div className="grid grid-cols-2 gap-3">
					<label className="text-ink-dull space-y-1.5 text-xs">
						<span>Expires</span>
						<Select value={expiry} onChange={setExpiry} size="sm">
							{EXPIRY.map((option) => (
								<SelectOption
									key={option.value}
									value={option.value}
								>
									{option.label}
								</SelectOption>
							))}
						</Select>
					</label>
					<label className="text-ink-dull space-y-1.5 text-xs">
						<span>Password</span>
						<PasswordInput
							size="sm"
							value={password}
							onChange={(event) =>
								setPassword(event.target.value)
							}
							placeholder="Optional"
							autoComplete="new-password"
						/>
					</label>
				</div>

				{(links.data?.length ?? 0) > 0 && (
					<div className="space-y-1.5">
						<div className="text-ink-dull text-xs">
							Active links
						</div>
						{links.data!.map((link) => (
							<div
								key={link.token}
								className="bg-app-box text-ink flex items-center gap-2 rounded-md px-3 py-2 text-sm"
							>
								<div className="min-w-0 flex-1">
									<div className="truncate">
										{props.paths.length > 1 && (
											<span className="mr-1.5">
												{link.name}
											</span>
										)}
										<span className="text-ink-dull">
											{link.url.replace(
												/^https?:\/\//,
												''
											)}
										</span>
									</div>
									<div className="text-ink-faint flex items-center gap-1 text-[11px]">
										{link.has_password && (
											<LockSimple
												size={11}
												weight="bold"
											/>
										)}
										{expiryText(link)}
									</div>
								</div>
								<Button
									type="button"
									variant="outline"
									size="sm"
									onClick={() => copy(link.url)}
								>
									<Copy size={14} />
								</Button>
								<Button
									type="button"
									variant="outline"
									size="sm"
									disabled={remove.isPending}
									onClick={() => remove.mutate(link.token)}
								>
									<Trash size={14} />
								</Button>
							</div>
						))}
					</div>
				)}
			</div>
		</Dialog>
	);
}
