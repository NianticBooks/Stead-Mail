export interface Piece {
  text: string;
  href?: string;
}

const URL_RE = /https?:\/\/[^\s<>"'`]+/gi;
const TRAILING = /[.,;:!?)\]}>'"]+$/;

/** Split plain text into text and link pieces. Rendering these as elements (never as HTML) means
 *  message text can not inject markup. Trailing punctuation is not part of a link. */
export function linkify(text: string): Piece[] {
  const out: Piece[] = [];
  let last = 0;
  for (const m of text.matchAll(URL_RE)) {
    let url = m[0];
    const tail = url.match(TRAILING);
    // Keep a closing parenthesis that balances one inside the URL (Wikipedia-style links).
    if (tail) {
      let strip = tail[0];
      while (strip.startsWith(")") && (url.match(/\(/g) ?? []).length >= (url.slice(0, url.length - strip.length + 1).match(/\)/g) ?? []).length) {
        strip = strip.slice(1);
      }
      url = url.slice(0, url.length - strip.length);
    }
    const start = m.index ?? 0;
    if (start > last) out.push({ text: text.slice(last, start) });
    out.push({ text: url, href: url });
    last = start + url.length;
  }
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}
