/**
 * Async data bound to reactive inputs. Call inside a component's script:
 *
 *   const repo = resource(() => loadRepo(page.params.owner, page.params.name));
 *
 * Reactive values read synchronously by `fn` are tracked; when they change the
 * request is re-run. Stale responses are dropped. `data` keeps the previous
 * value while reloading, so views can show it dimmed instead of a blank page.
 */
export interface Resource<T> {
  readonly data: T | undefined;
  readonly error: unknown;
  readonly loading: boolean;
  retry(): void;
}

export function resource<T>(fn: () => Promise<T>): Resource<T> {
  let data = $state<T | undefined>(undefined);
  let error = $state<unknown>(undefined);
  let loading = $state(true);
  let attempt = $state(0);
  let seq = 0;

  $effect(() => {
    void attempt;
    const current = ++seq;
    loading = true;
    error = undefined;
    fn().then(
      (value) => {
        if (current !== seq) return;
        data = value;
        loading = false;
      },
      (e) => {
        if (current !== seq) return;
        error = e;
        loading = false;
      },
    );
  });

  return {
    get data() {
      return data;
    },
    get error() {
      return error;
    },
    get loading() {
      return loading;
    },
    retry: () => attempt++,
  };
}
