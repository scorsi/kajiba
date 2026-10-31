<!-- `/<owner>/<name>/commits/<ref>` (ADR 0021) becomes the query form with History open (ADR 0134). -->
<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { listRefs } from "#lib/api.ts";
  import Skeleton from "#lib/components/Skeleton.svelte";
  import { repoQuery, splitRefPath } from "#lib/legacy-urls.ts";

  $effect(() => {
    const key = `${page.params.owner}/${page.params.name}`;
    const rest = page.params.ref ?? "";
    const open = (ref: string) =>
      goto(`${repoQuery(key, ref, "")}${ref ? "&" : "?"}history=1`, { replace: true });
    listRefs({ repo: key })
      .then(({ refs }) => open(splitRefPath(rest, refs.map((r) => r.name)).ref))
      .catch(() => open(""));
  });
</script>

<div class="p-6"><Skeleton lines={6} /></div>
