/** Compact pixel offsets; unmeasured wrapped rows start at one 20px line. */
export function linePositions(
  indices: Uint32Array,
  heights: Map<number, number>,
) {
  const p = new Float64Array(indices.length + 1);
  for (let i = 0; i < indices.length; i++)
    p[i + 1] = p[i] + (heights.get(indices[i]) ?? 20);
  return p;
}
// Stay below browser scroll-height limits even for millions of short lines.
const physicalHeight = (p: Float64Array) =>
  Math.min(8_000_000, p[p.length - 1]);
export const scrollScale = (p: Float64Array, viewport: number) =>
  Math.max(
    1,
    (p[p.length - 1] - viewport) / Math.max(1, physicalHeight(p) - viewport),
  );
export function lineTop(p: Float64Array, index: number, viewport: number) {
  return Math.min(
    Math.max(0, physicalHeight(p) - viewport),
    p[index] / scrollScale(p, viewport),
  );
}
export function lineWindow(
  p: Float64Array,
  top: number,
  viewport: number,
  overscan = 8,
) {
  const count = p.length - 1;
  const height = physicalHeight(p);
  top = Math.max(0, Math.min(top, Math.max(0, height - viewport)));
  const logical = top * scrollScale(p, viewport);
  const find = (pixel: number) => {
    let lo = 0,
      hi = count;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (p[mid + 1] <= pixel) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  };
  const start = Math.max(0, find(logical) - overscan);
  const end = Math.min(count, find(logical + viewport) + 1 + overscan);
  const before = Math.max(0, top - (logical - p[start]));
  return {
    start,
    end,
    before,
    after: Math.max(0, height - before - (p[end] - p[start])),
  };
}
