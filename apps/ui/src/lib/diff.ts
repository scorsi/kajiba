/**
 * Line diff computed in the browser from two blobs (GetBlob at the commit and
 * at its parent). This gives a unified diff without a diff RPC.
 * LCS by dynamic programming, after trimming the common prefix and suffix;
 * refuses inputs whose middle part exceeds `limit` cells (returns undefined).
 */

export interface DiffLine {
  kind: "ctx" | "add" | "del";
  text: string;
  /** 1-based line numbers in the old / new text (absent on the other side). */
  old?: number;
  new?: number;
}

export interface Hunk {
  oldStart: number;
  newStart: number;
  lines: DiffLine[];
}

export function lineDiff(
  before: string,
  after: string,
  context = 3,
  limit = 4_000_000,
): Hunk[] | undefined {
  const a = split(before);
  const b = split(after);

  let pre = 0;
  while (pre < a.length && pre < b.length && a[pre] === b[pre]) pre++;
  let suf = 0;
  while (
    suf < a.length - pre &&
    suf < b.length - pre &&
    a[a.length - 1 - suf] === b[b.length - 1 - suf]
  )
    suf++;

  const am = a.slice(pre, a.length - suf);
  const bm = b.slice(pre, b.length - suf);
  if (am.length * bm.length > limit) return undefined;

  // LCS table on the middle part.
  const n = am.length;
  const m = bm.length;
  const t = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--)
    for (let j = m - 1; j >= 0; j--)
      t[i][j] =
        am[i] === bm[j]
          ? t[i + 1][j + 1] + 1
          : Math.max(t[i + 1][j], t[i][j + 1]);

  const all: DiffLine[] = [];
  for (let k = 0; k < pre; k++)
    all.push({ kind: "ctx", text: a[k], old: k + 1, new: k + 1 });
  let i = 0;
  let j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && am[i] === bm[j]) {
      all.push({
        kind: "ctx",
        text: am[i],
        old: pre + i + 1,
        new: pre + j + 1,
      });
      i++;
      j++;
    } else if (i < n && (j === m || t[i + 1][j] >= t[i][j + 1])) {
      all.push({ kind: "del", text: am[i], old: pre + i + 1 }); // deletions first, like git
      i++;
    } else {
      all.push({ kind: "add", text: bm[j], new: pre + j + 1 });
      j++;
    }
  }
  for (let k = 0; k < suf; k++) {
    all.push({
      kind: "ctx",
      text: a[a.length - suf + k],
      old: a.length - suf + k + 1,
      new: b.length - suf + k + 1,
    });
  }

  // Group changes into hunks with `context` lines around them.
  const hunks: Hunk[] = [];
  let cur: Hunk | undefined;
  let lastChange = -Infinity;
  all.forEach((line, idx) => {
    if (line.kind === "ctx") return;
    const from = Math.max(0, idx - context);
    if (!cur || from > lastChange + context) {
      if (cur)
        for (let k = lastChange + 1; k <= lastChange + context; k++)
          cur.lines.push(all[k]);
      const first = all[from];
      cur = {
        oldStart: first.old ?? first.new ?? 1,
        newStart: first.new ?? first.old ?? 1,
        lines: [],
      };
      hunks.push(cur);
      for (let k = from; k < idx; k++) cur.lines.push(all[k]);
    } else {
      for (let k = lastChange + 1; k < idx; k++) cur.lines.push(all[k]);
    }
    cur.lines.push(line);
    lastChange = idx;
  });
  if (cur) {
    for (
      let k = lastChange + 1;
      k < Math.min(all.length, lastChange + 1 + context);
      k++
    ) {
      cur.lines.push(all[k]);
    }
  }
  return hunks;
}

function split(s: string): string[] {
  if (s === "") return [];
  return (s.endsWith("\n") ? s.slice(0, -1) : s).split("\n");
}
