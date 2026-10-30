<script lang="ts">
  import { autofocus } from "../focus";
  import { cal } from "../calendar.svelte";
  import type { RemoteCalendar } from "../types";
  import Icon from "./Icon.svelte";

  let error = $state<string | null>(null);
  let busyKey = $state<string | null>(null);

  const key = (c: RemoteCalendar) => `${c.source}|${c.account_id}|${c.remote_id}`;

  // One group per account, in the order the server listed them.
  const groups = $derived.by(() => {
    const out: { title: string; items: RemoteCalendar[] }[] = [];
    for (const c of cal.remote?.calendars ?? []) {
      const apple = /@(icloud|me|mac)\.com$/i.test(c.account);
      const title = `${c.source === "google" ? "Google" : apple ? "iCloud" : "CalDAV"} · ${c.account}`;
      let g = out.find((x) => x.title === title);
      if (!g) out.push((g = { title, items: [] }));
      g.items.push(c);
    }
    return out;
  });

  async function link(c: RemoteCalendar) {
    busyKey = key(c);
    error = await cal.link(c.source, c.account_id, c.remote_id);
    busyKey = null;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cal.connectOpen = false;
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="overlay" role="dialog" aria-modal="true" aria-label="Connect calendars" tabindex="-1" use:autofocus={"button.close"} {onkeydown}>
  <div class="modal">
    <header>
      <h2>Connect calendars</h2>
      <span class="spacer"></span>
      <button class="icon-btn close" onclick={() => (cal.connectOpen = false)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      <p class="muted small">
        A connected calendar is mirrored here and kept in sync both ways. If the same event changed in both places, the server’s version is kept and the difference is written to the sync log.
      </p>
      {#if cal.remoteLoading && !cal.remote}
        <p class="muted"><Icon name="loader" size={14} /> Asking your accounts…</p>
      {/if}
      {#each groups as g (g.title)}
        <section>
          <h3>{g.title}</h3>
          {#each g.items as c (key(c))}
            <div class="row">
              <span class="dot" style:background={c.color?.slice(0, 7) ?? "var(--fg-2)"}></span>
              <span class="name">{c.name}{#if c.read_only}<span class="muted small"> · read-only</span>{/if}</span>
              {#if c.linked_to}
                <span class="muted small"><Icon name="check" size={12} /> Connected</span>
              {:else}
                <button class="btn sm" onclick={() => link(c)} disabled={busyKey !== null}>
                  {#if busyKey === key(c)}<Icon name="loader" size={13} />{:else}<Icon name="link" size={13} />{/if} Connect
                </button>
              {/if}
            </div>
          {/each}
        </section>
      {/each}
      {#each cal.remote?.errors ?? [] as e}<div class="problem" role="alert">{e}</div>{/each}
      {#if cal.remote && groups.length === 0 && cal.remote.errors.length === 0}
        <p class="muted">No Google or iCloud accounts yet. Add one in Settings, or with <code>stead google login</code>.</p>
      {/if}
      {#if error}<div class="problem" role="alert">{error}</div>{/if}
    </div>
    <footer>
      <button class="btn sm ghost" onclick={() => cal.loadRemote()} disabled={cal.remoteLoading}><Icon name="refresh" size={13} /> Refresh list</button>
      <span class="spacer"></span>
      <button class="btn primary" onclick={() => (cal.connectOpen = false)}>Done</button>
    </footer>
  </div>
</div>

<style>
  .modal { width: min(520px, 100%); max-height: min(80vh, 640px); display: flex; flex-direction: column; }
  header { display: flex; align-items: center; gap: 8px; padding: 12px 16px; border-bottom: 1px solid var(--line); }
  .spacer { flex: 1; }
  .body { padding: 14px 16px; display: grid; gap: 14px; overflow-y: auto; }
  h3 { margin: 0 0 6px; font-size: 12px; color: var(--fg-2); text-transform: uppercase; letter-spacing: 0.05em; font-weight: 600; }
  .row { display: flex; align-items: center; gap: 10px; padding: 6px 0; }
  .dot { width: 10px; height: 10px; border-radius: 50%; flex: none; }
  .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .small { font-size: 12.5px; }
  .problem { color: var(--danger); font-size: 12.5px; }
  footer { display: flex; gap: 8px; align-items: center; padding: 10px 16px; border-top: 1px solid var(--line); }
  p { margin: 0; }
</style>
