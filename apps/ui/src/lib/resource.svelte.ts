/**
 * Async data bound to reactive inputs. Call inside a component's script:
 *
 *   const repo = resource(() => loadRepo(page.params.owner, page.params.name));
 *
 * Reactive values read synchronously by `fn` are tracked; when they change the
 * request is re-run. Stale responses are dropped. `data` keeps the previous
 * value while reloading, so views can show it dimmed instead of a blank page.
 * `fn` receives a signal aborted on reload or component cleanup.
 * `refreshWhile` schedules another load after one second while its predicate holds.
 */
export interface Resource<T> {
  readonly data: T | undefined;
  readonly error: unknown;
  readonly loading: boolean;
  retry(): void;
}

export function resource<T>(
  fn: (signal: AbortSignal) => Promise<T>,
  refreshWhile?: (data: T) => boolean,
): Resource<T> {
  let data = $state<T | undefined>(undefined);
  let error = $state<unknown>(undefined);
  let loading = $state(true);
  let attempt = $state(0);
  let seq = 0;

  $effect(() => {
    void attempt;
    const current = ++seq;
    const stop = new AbortController();
    let refresh: ReturnType<typeof setTimeout> | undefined;
    loading = true;
    error = undefined;
    fn(stop.signal).then(
      (value) => {
        if (current !== seq) return;
        data = value;
        loading = false;
        if (refreshWhile?.(value)) refresh = setTimeout(() => attempt++, 1000);
      },
      (e) => {
        if (current !== seq) return;
        error = e;
        loading = false;
      },
    );
    return () => {
      seq++;
      stop.abort();
      clearTimeout(refresh);
    };
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
