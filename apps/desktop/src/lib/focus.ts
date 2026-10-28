/** Svelte action: move focus into a dialog as soon as it appears. With a selector, focus that
 *  control (falling back to the dialog itself, which has tabindex="-1"). */
export function autofocus(node: HTMLElement, selector?: string) {
  queueMicrotask(() => {
    const target = (selector ? node.querySelector<HTMLElement>(selector) : null) ?? node;
    target.focus();
    if (target instanceof HTMLTextAreaElement) target.setSelectionRange(0, 0);
  });
}
