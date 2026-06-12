import { describe, expect, it } from "vitest";

import { byteSpanToUtf16, makeByteToUtf16 } from "./spanmap";

// The mapper's contract: a UTF-8 byte offset → the UTF-16 code-unit offset of the same position.
// We derive expectations from TextEncoder (UTF-8 truth) so the test can't drift from reality.
function byteOffsetOfCharIndex(text: string, charCount: number): number {
  let bytes = 0;
  let seen = 0;
  for (const ch of text) {
    if (seen === charCount) break;
    bytes += new TextEncoder().encode(ch).length;
    seen++;
  }
  return bytes;
}

describe("makeByteToUtf16", () => {
  it("maps ASCII identically (1 byte = 1 UTF-16 unit)", () => {
    const text = "hello world";
    const b2u = makeByteToUtf16(text);
    expect(b2u(0)).toBe(0);
    expect(b2u(6)).toBe(6);
    expect(b2u(text.length)).toBe(text.length);
  });

  it("handles 2-byte (é), 3-byte (€), and 4-byte (😀) characters", () => {
    // "a" (1B,1u) "é" (2B,1u) "€" (3B,1u) "😀" (4B,2u) "b" (1B,1u)
    const text = "aé€😀b";
    const b2u = makeByteToUtf16(text);
    expect(b2u(0)).toBe(0); // before "a"
    expect(b2u(1)).toBe(1); // before "é"
    expect(b2u(3)).toBe(2); // before "€"
    expect(b2u(6)).toBe(3); // before "😀"
    expect(b2u(10)).toBe(5); // before "b" (😀 added 2 UTF-16 units)
    expect(b2u(11)).toBe(6); // end
  });

  it("maps an astral character span to a 2-unit UTF-16 range", () => {
    const text = "x😀y";
    const b2u = makeByteToUtf16(text);
    // "😀" occupies bytes [1,5) and UTF-16 units [1,3).
    const span = byteSpanToUtf16(b2u, { start: 1, end: 5 });
    expect(span).toEqual({ startU16: 1, endU16: 3 });
    expect(text.slice(span.startU16, span.endU16)).toBe("😀");
  });

  it("handles combining marks (each is its own scalar)", () => {
    // "e" + combining acute (U+0301): 1 + 2 bytes, 1 + 1 UTF-16 units.
    const text = "éz";
    const b2u = makeByteToUtf16(text);
    expect(b2u(0)).toBe(0);
    expect(b2u(1)).toBe(1); // before the combining mark
    expect(b2u(3)).toBe(2); // before "z"
  });

  it("snaps a mid-character byte offset back to the character start", () => {
    const text = "a😀"; // 😀 = bytes [1,5)
    const b2u = makeByteToUtf16(text);
    expect(b2u(2)).toBe(1); // byte 2 is inside 😀 → snaps to its start (UTF-16 offset 1)
    expect(b2u(3)).toBe(1);
  });

  it("clamps out-of-range offsets", () => {
    const text = "hi";
    const b2u = makeByteToUtf16(text);
    expect(b2u(-5)).toBe(0);
    expect(b2u(999)).toBe(2);
  });

  it("agrees with TextEncoder across a mixed string at every character boundary", () => {
    const text = "Olá, 世界! café 😀 naïve";
    const b2u = makeByteToUtf16(text);
    let charIdx = 0;
    let u16 = 0;
    for (const ch of text) {
      const byte = byteOffsetOfCharIndex(text, charIdx);
      expect(b2u(byte)).toBe(u16);
      u16 += ch.codePointAt(0)! > 0xffff ? 2 : 1;
      charIdx++;
    }
  });
});
