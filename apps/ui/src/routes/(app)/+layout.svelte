<!-- Signed-in pages: the shell (the forge as a foldable band) around every page. -->
<script lang="ts">
	import Shell from '#lib/components/Shell.svelte';

	let { children } = $props();
</script>

<Shell>{@render children()}</Shell>
