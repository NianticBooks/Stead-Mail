<script lang="ts">
  import { app } from "../state.svelte";
  import { autofocus } from "../focus";
  import { modKey } from "../platform";
  import Icon from "./Icon.svelte";
  import AiDraft from "./AiDraft.svelte";
  import { formatSize } from "../format";

  let confirmDiscard = $state(false);
  let bodyBox = $state<HTMLTextAreaElement | null>(null);

  // If focus fell to the page (a focused control was disabled or removed), bring it back into the
  // message so typing and the dialog's keys keep working.
  $effect(() => {
    void app.compose?.ai.busy;
    void app.compose?.ai.preview;
    queueMicrotask(() => {
      if (app.compose && document.activeElement === document.body) bodyBox?.focus();
    });
  });
  function close() {
    const c = app.compose;
    if (!c || c.sending) return;
    if (c.dirty && !confirmDiscard) {
      confirmDiscard = true;
      return;
    }
    app.compose = null;
  }

  async function keep() {
    if (await app.saveDraft()) {
      app.toast("success", "Draft saved");
      app.compose = null;
    }
  }

  function discard() {
    const id = app.compose?.draftId;
    app.compose = null;
    if (id) void app.deleteDraft(id);
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void app.send();
    }
  }

  const sizeNote = $derived(app.compose ? app.compose.attachments.length : 0);
</script>

<!-- Keys are handled on the window, not on the dialog: a focused button that becomes disabled (while an AI
     draft is being written) drops focus to the page, and Escape must still work then. -->
<svelte:window onkeydown={(e) => app.compose && onkeydown(e)} />

{#if app.compose}
  {@const c = app.compose}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="overlay" role="dialog" aria-modal="true" aria-label="New message" tabindex="-1" use:autofocus={c.to ? "textarea" : 'input[aria-label="To"]'}>
    <div class="modal compose">
      <header>
        <h2>{c.draftId ? "Draft" : c.inReplyTo ? "Reply" : "New message"}</h2>
        <button class="icon-btn" onclick={close} aria-label="Close"><Icon name="x" /></button>
      </header>

      <div class="fields">
        {#if app.accounts.length > 1}
          <label class="row"><span>From</span>
            <select class="input" bind:value={c.accountId}>
              {#each app.accounts as a (a.id)}<option value={a.id}>{a.name} &lt;{a.email}&gt;</option>{/each}
            </select>
          </label>
        {/if}
        <label class="row"><span>To</span>
          <input class="input" aria-label="To" bind:value={c.to} oninput={() => (c.dirty = true)} placeholder="name@example.com, …" spellcheck="false" autocomplete="off" />
          {#if !c.showCc}<button class="btn ghost sm" type="button" onclick={() => (c.showCc = true)}>Cc / Bcc</button>{/if}
        </label>
        {#if c.showCc}
          <label class="row"><span>Cc</span><input class="input" bind:value={c.cc} oninput={() => (c.dirty = true)} spellcheck="false" autocomplete="off" /></label>
          <label class="row"><span>Bcc</span><input class="input" bind:value={c.bcc} oninput={() => (c.dirty = true)} spellcheck="false" autocomplete="off" /></label>
        {/if}
        <label class="row"><span>Subject</span><input class="input" bind:value={c.subject} oninput={() => (c.dirty = true)} /></label>
      </div>

      <textarea bind:this={bodyBox} class="body" bind:value={c.body} oninput={() => (c.dirty = true)} placeholder="Write your message…" aria-label="Message body"></textarea>

      <AiDraft />

      {#if c.attachments.length}
        <div class="atts">
          {#each c.attachments as a, i (a.path)}
            <span class="att"><Icon name="clip" size={13} /> {a.name}
              <button class="icon-btn tiny" onclick={() => { c.attachments.splice(i, 1); }} aria-label="Remove {a.name}"><Icon name="x" size={12} /></button>
            </span>
          {/each}
        </div>
      {/if}

      {#if c.error}<div class="error" role="alert"><Icon name="alert" size={15} /> {c.error}</div>{/if}

      {#if confirmDiscard}
        <div class="confirm" role="alertdialog">
          <span>Keep this message?</span>
          <button class="btn primary sm" onclick={keep}>Save draft</button>
          <button class="btn danger sm" onclick={discard}>Discard</button>
          <button class="btn sm" onclick={() => (confirmDiscard = false)}>Keep editing</button>
        </div>
      {/if}

      <footer>
        <button class="btn primary" onclick={() => app.send()} disabled={c.sending} title="Send ({modKey}+Enter)">
          <Icon name={c.sending ? "loader" : "send"} size={15} /> {c.sending ? "Sending…" : "Send"}
        </button>
        <button class="btn" onclick={() => app.attachFiles()} disabled={c.sending}><Icon name="clip" size={15} /> Attach{sizeNote ? ` (${sizeNote})` : ""}</button>
        <span class="hint muted">{modKey}+Enter to send</span>
        <span class="spacer"></span>
        <button class="btn ghost" onclick={close} disabled={c.sending}>Discard</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .compose { width: min(760px, 100%); height: min(640px, calc(100vh - 48px)); }
  header { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; border-bottom: 1px solid var(--line); }
  .fields { display: grid; padding: 8px 16px 0; }
  .row { display: flex; align-items: center; gap: 10px; border-bottom: 1px solid var(--line); padding: 4px 0; transition: border-color 0.12s; }
  .row:focus-within { border-bottom-color: var(--accent); box-shadow: 0 1px 0 var(--accent); }
  .row span { width: 56px; color: var(--fg-2); font-size: 12.5px; flex: none; }
  .row :global(.input) { border: 0; background: transparent; padding: 6px 0; box-shadow: none; }
  .row select.input {
    appearance: none; background-color: var(--bg-1); border: 1px solid var(--line-strong); border-radius: var(--radius-sm);
    padding: 5px 30px 5px 10px; color: var(--fg-0);
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='%237c8696' stroke-width='2.5' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='m6 9 6 6 6-6'/%3E%3C/svg%3E");
    background-repeat: no-repeat; background-position: right 10px center;
  }
  .row select.input:focus-visible { border-color: var(--accent); }
  .body {
    flex: 1; min-height: 120px; resize: none; padding: 14px 16px; background: transparent; border: 0; outline: none;
    color: var(--fg-0); line-height: 1.55; font-family: var(--font-ui);
  }
  .atts { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 16px 8px; }
  .att { display: inline-flex; align-items: center; gap: 6px; padding: 3px 4px 3px 10px; background: var(--bg-3); border: 1px solid var(--line-strong); border-radius: 999px; font-size: 12.5px; }
  .tiny { width: 20px; height: 20px; }
  .error, .confirm { display: flex; align-items: center; gap: 8px; margin: 0 16px 8px; padding: 8px 10px; border-radius: var(--radius-sm); }
  .error { color: var(--danger); background: color-mix(in srgb, var(--danger) 10%, transparent); border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent); }
  .confirm { background: var(--bg-3); border: 1px solid var(--line-strong); }
  .confirm span { flex: 1; }
  footer { display: flex; align-items: center; gap: 8px; padding: 12px 16px; border-top: 1px solid var(--line); }
  .spacer { flex: 1; }
  .hint { font-size: 12px; }
</style>
