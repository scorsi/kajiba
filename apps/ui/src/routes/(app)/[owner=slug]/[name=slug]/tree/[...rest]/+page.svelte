<!-- `/<owner>/<name>/tree/<ref>/<path>` (ADR 0021) becomes the query form (ADR 0134). -->
<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { listRefs } from "#lib/api.ts";
  import Skeleton from "#lib/components/Skeleton.svelte";
  import { repoQuery, splitRefPath } from "#lib/legacy-urls.ts";

  $effect(() => {
    const key = `${page.params.owner}/${page.params.name}`;
    const rest = page.params.rest ?? "";
    listRefs({ repo: key })
      .then(({ refs }) => {
        const { ref, path } = splitRefPath(rest, refs.map((r) => r.name));
        return goto(repoQuery(key, ref, path), { replace: true });
      })
      .catch(() => goto(repoQuery(key, "", ""), { replace: true }));
  });
</script>

<div class="p-6"><Skeleton lines={6} /></div>
