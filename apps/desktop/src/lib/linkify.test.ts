import { describe, expect, it } from "vitest";
import { linkify } from "./linkify";

describe("linkify", () => {
  it("finds links and leaves the rest as text", () => {
    expect(linkify("see https://example.com/a?b=1 now")).toEqual([
      { text: "see " },
      { text: "https://example.com/a?b=1", href: "https://example.com/a?b=1" },
      { text: " now" },
    ]);
  });
  it("does not swallow trailing punctuation", () => {
    const p = linkify("Go to https://example.com/x, then (https://example.com/y).");
    expect(p.filter((x) => x.href).map((x) => x.href)).toEqual(["https://example.com/x", "https://example.com/y"]);
  });
  it("keeps a balanced closing parenthesis inside the link", () => {
    expect(linkify("https://en.wikipedia.org/wiki/Mail_(software)").find((x) => x.href)!.href).toBe("https://en.wikipedia.org/wiki/Mail_(software)");
  });
  it("never produces markup and ignores other schemes", () => {
    const p = linkify("javascript:alert(1) <script>x</script> ftp://x.test");
    expect(p).toEqual([{ text: "javascript:alert(1) <script>x</script> ftp://x.test" }]);
  });
  it("handles empty input and text with no links", () => {
    expect(linkify("")).toEqual([]);
    expect(linkify("plain")).toEqual([{ text: "plain" }]);
  });
});
