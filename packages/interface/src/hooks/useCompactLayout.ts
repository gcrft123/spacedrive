import {useSyncExternalStore} from 'react';

const QUERY = '(max-width: 767px)';

function subscribe(onChange: () => void) {
	const media = window.matchMedia(QUERY);
	media.addEventListener('change', onChange);
	return () => media.removeEventListener('change', onChange);
}

/**
 * Phone-width windows, where the sidebar and inspector open over the content
 * instead of beside it, and a tap opens what a double-click would.
 */
export function useCompactLayout() {
	return useSyncExternalStore(
		subscribe,
		() => window.matchMedia(QUERY).matches,
		() => false
	);
}
