// The correctness crux of the editor binding.
//
// The engine reports diagnostic spans as **UTF-8 byte offsets** into the checked text. Monaco (and JS
// strings) address text in **UTF-16 code units**. A naive `text.slice(span.start, span.end)` is wrong
// for any non-ASCII input. We build a byte→UTF-16 index once per check (O(n)) and reuse it for every
// diagnostic, then let `model.getPositionAt` (which already speaks UTF-16 offsets) resolve line/column.

/** Bytes a Unicode scalar occupies in UTF-8 (1–4). */
function utf8Len(codePoint: number): number {
  if (codePoint < 0x80) return 1;
  if (codePoint < 0x800) return 2;
  if (codePoint < 0x10000) return 3;
  return 4;
}

/**
 * Map a UTF-8 byte offset into `text` to its UTF-16 code-unit offset.
 *
 * Build once per checked string, then call for each diagnostic endpoint. A byte offset on a character
 * boundary maps exactly; one that lands *inside* a multi-byte character (the engine shouldn't emit
 * these, but we never throw on bad input) snaps back to the enclosing character's start.
 */
export function makeByteToUtf16(text: string): (byteOffset: number) => number {
  // byteToU16[b] is defined only at the starting byte of each character (sparse), plus an end sentinel.
  const byteToU16: number[] = [];
  let bytePos = 0;
  let u16Pos = 0;
  for (const ch of text) {
    // for..of iterates by Unicode scalar (code point), collapsing surrogate pairs.
    byteToU16[bytePos] = u16Pos;
    const cp = ch.codePointAt(0) ?? 0;
    bytePos += utf8Len(cp);
    u16Pos += cp > 0xffff ? 2 : 1; // astral scalar = a surrogate pair = 2 UTF-16 units
  }
  byteToU16[bytePos] = u16Pos; // end sentinel: offset == total byte length → total UTF-16 length

  const totalBytes = bytePos;
  return (byteOffset: number): number => {
    let b = byteOffset;
    if (b < 0) b = 0;
    if (b > totalBytes) b = totalBytes;
    const exact = byteToU16[b];
    if (exact !== undefined) return exact;
    // Inside a multi-byte char: walk back to the nearest filled boundary.
    let k = b;
    while (k > 0 && byteToU16[k] === undefined) k--;
    return byteToU16[k] ?? 0;
  };
}

/** A diagnostic's UTF-16 [start, end) offsets — the form Monaco's `getPositionAt` consumes. */
export interface Utf16Span {
  startU16: number;
  endU16: number;
}

/** Convert a byte span to a UTF-16 span using a prebuilt mapper. */
export function byteSpanToUtf16(
  b2u: (byteOffset: number) => number,
  span: { start: number; end: number },
): Utf16Span {
  return { startU16: b2u(span.start), endU16: b2u(span.end) };
}
