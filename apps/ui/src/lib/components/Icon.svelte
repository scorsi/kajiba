<script lang="ts" module>
	/** Stroke icons on a 24×24 grid. Add a path here rather than pulling an icon set. */
	const PATHS = {
		folder: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
		file: 'M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z M14 3v5h5',
		symlink:
			'M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z M14 3v5h5 M9 17l5-5 M10 12h4v4',
		submodule: 'M4 7l8-4 8 4v10l-8 4-8-4z M4 7l8 4 8-4 M12 11v10',
		branch:
			'M6 3v12 M18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6z M6 21a3 3 0 1 0 0-6 3 3 0 0 0 0 6z M18 9a9 9 0 0 1-9 9',
		tag: 'M3 12V4a1 1 0 0 1 1-1h8l9 9-9 9z M7.5 7.5h.01',
		commit: 'M12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z M3 12h6 M15 12h6',
		copy: 'M9 9h11v11H9z M5 15H4V4h11v1',
		check: 'M5 12l5 5L20 7',
		archive: 'M3 4h18v4H3z M5 8v12h14V8 M10 12h4',
		sun: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z M12 2v2 M12 20v2 M4.9 4.9l1.4 1.4 M17.7 17.7l1.4 1.4 M2 12h2 M20 12h2 M4.9 19.1l1.4-1.4 M17.7 6.3l1.4-1.4',
		moon: 'M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z',
		monitor: 'M3 4h18v12H3z M8 20h8 M12 16v4',
		chevron: 'M6 9l6 6 6-6',
		key: 'M14.5 4a5.5 5.5 0 1 1-4.6 8.5L4 18.4V21h3v-2h2v-2h2l1.4-1.4A5.5 5.5 0 0 1 14.5 4z M16 8h.01',
		mirror: 'M4 12a8 8 0 0 1 14-5.3L20 9 M20 4v5h-5 M20 12a8 8 0 0 1-14 5.3L4 15 M4 20v-5h5',
		play: 'M7 4l13 8-13 8z',
		menu: 'M4 6h16 M4 12h16 M4 18h16',
		search: 'M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14z M20 20l-4-4',
		clock: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18z M12 7v5l3 2',
		user: 'M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z M5 21a7 7 0 0 1 14 0',
		logout: 'M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4 M16 17l5-5-5-5 M21 12H9',
		lock: 'M6 11h12v10H6z M8 11V7a4 4 0 0 1 8 0v4',
		x: 'M6 6l12 12 M18 6L6 18',
		chart: 'M4 20V11 M10 20V5 M16 20v-6 M21 20H3',
		server: 'M4 4h16v6H4z M4 14h16v6H4z M8 7h.01 M8 17h.01',
		alert: 'M12 3l10 18H2z M12 10v4 M12 17h.01',
		plus: 'M12 5v14 M5 12h14',
		trash: 'M4 7h16 M9 7V4h6v3 M6 7l1 13h10l1-13'
	} as const;

	export type IconName = keyof typeof PATHS;
</script>

<script lang="ts">
	let {
		name,
		size = 16,
		label,
		class: klass = ''
	}: { name: IconName; size?: number; label?: string; class?: string } = $props();
</script>

<svg
	width={size}
	height={size}
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width="1.75"
	stroke-linecap="round"
	stroke-linejoin="round"
	class="inline-block shrink-0 {klass}"
	role={label ? 'img' : undefined}
	aria-label={label}
	aria-hidden={label ? undefined : 'true'}
	focusable="false"
>
	<path d={PATHS[name]} />
</svg>
