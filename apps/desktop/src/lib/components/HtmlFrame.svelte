<script lang="ts">
  // Shows already-sanitised HTML mail inside a sandboxed, script-free frame with its own CSP.
  // `allow-same-origin` (without `allow-scripts`) is only there so the parent can intercept link
  // clicks and measure the content; scripts can not run in the frame in any case.
  let { html, allowImages, onlink }: { html: string; allowImages: boolean; onlink: (url: string) => void } = $props();

  let frame = $state<HTMLIFrameElement | null>(null);
  let height = $state(240);

  const csp = $derived(`default-src 'none'; img-src data:${allowImages ? " https:" : ""}; style-src 'unsafe-inline'`);
  const doc = $derived(
    `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}">` +
      `<style>html{background:#fff}body{margin:0;padding:18px 22px;font:14px/1.55 system-ui,sans-serif;color:#1b1f27;word-wrap:break-word;overflow-wrap:anywhere}` +
      `img{max-width:100%;height:auto}table{max-width:100%}a{color:#2563eb}blockquote{margin:8px 0;padding-left:12px;border-left:3px solid #d0d5dd;color:#475467}</style>` +
      `</head><body>${html}</body></html>`,
  );

  function resize() {
    const d = frame?.contentDocument;
    if (d?.documentElement) height = Math.max(120, d.documentElement.scrollHeight);
  }

  function onload() {
    const d = frame?.contentDocument;
    if (!d) return;
    d.addEventListener("click", (e) => {
      const a = (e.target as Element | null)?.closest?.("a");
      if (a) {
        e.preventDefault();
        const href = a.getAttribute("href");
        if (href) onlink(href);
      }
    });
    resize();
    if (d.body) new ResizeObserver(resize).observe(d.body);
  }
</script>

<iframe
  bind:this={frame}
  title="Message content"
  sandbox="allow-same-origin"
  srcdoc={doc}
  {onload}
  style:height="{height}px"
></iframe>

<style>
  iframe {
    display: block;
    width: 100%;
    border: 0;
    border-radius: 8px;
    background: #fff;
    color-scheme: light;
  }
</style>
