/**
 * A run's log as WatchRun delivers it (ADR 0129, 0142): attempt sections and
 * byte chunks, with no line or UTF-8 boundary respected. Pure, so it is unit
 * tested without a server; `api.ts` feeds it.
 */

import type { RunLogLine } from "#lib/api.ts";

// CSI sequences and OSC strings; a sequence never spans lines in a build log.
// eslint-disable-next-line no-control-regex
const ANSI = /\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g;

/** One line of output as shown: escapes stripped, a carriage-return redraw reduced to its last state. */
export function cleanLine(raw: string): string {
  const plain = raw.replace(ANSI, "");
  return plain.slice(plain.lastIndexOf("\r") + 1);
}

export function toLine(text: string): RunLogLine {
  const clean = cleanLine(text);
  return {
    offsetSeconds: 0,
    text: clean,
    stream: /^error\b/.test(clean) ? "err" : "out",
  };
}

export class RunLog<A> {
  /** Bytes received so far: where a resumed stream starts. */
  offset = 0;
  attempts: { id: string; meta: A; lines: RunLogLine[]; partial: string }[] =
    [];
  private decoder = new TextDecoder();

  section(id: string, meta: A) {
    const last = this.attempts.at(-1);
    // A resumed stream repeats the section of the attempt it resumes inside.
    if (last?.id === id) last.meta = meta;
    else this.attempts.push({ id, meta, lines: [], partial: "" });
  }

  bytes(offset: number, data: Uint8Array) {
    // Chunks overlapping what was already received are cut, not duplicated.
    const skip = Math.max(0, this.offset - offset);
    if (skip >= data.length) return;
    this.offset = offset + data.length;
    const at = this.attempts.at(-1);
    if (!at) return;
    const text =
      at.partial + this.decoder.decode(data.subarray(skip), { stream: true });
    const parts = text.split("\n");
    at.partial = parts.pop()!;
    for (const p of parts) at.lines.push(toLine(p));
  }

  /** Completed lines of an attempt, then its unfinished last line if any. */
  lines(i: number): RunLogLine[] {
    const at = this.attempts[i];
    return at.partial ? [...at.lines, toLine(at.partial)] : [...at.lines];
  }
}
