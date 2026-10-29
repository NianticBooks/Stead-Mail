<script lang="ts">
  import { autofocus } from "../focus";
  import { cal } from "../calendar.svelte";
  import { CALENDAR_COLORS } from "../cal";
  import type { Calendar } from "../types";
  import Icon from "./Icon.svelte";

  const target = cal.calendarDialog;
  const existing: Calendar | null = target && target !== "new" ? target : null;

  let name = $state(existing?.name ?? "");
  let color = $state(existing?.color ?? CALENDAR_COLORS[cal.calendars.length % CALENDAR_COLORS.length]);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let confirmDelete = $state(false);

  const validColor = $derived(/^#[0-9a-fA-F]{6}$/.test(color));
  const localCount = $derived(cal.calendars.filter((c) => c.kind === "local").length);
  const canDelete = $derived(!!existing && existing.kind === "local" && localCount > 1);

  async function save() {
    if (!name.trim() || !validColor || busy) return;
    busy = true;
    error = await cal.saveCalendar(existing, name, color);
    busy = false;
  }

  async function remove() {
    if (!existing) return;
    busy = true;
    error = await cal.deleteCalendar(existing);
    busy = false;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      cal.calendarDialog = null;
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="overlay" role="dialog" aria-modal="true" aria-label={existing ? "Edit calendar" : "New calendar"} tabindex="-1" use:autofocus={"input[name=name]"} {onkeydown}>
  <form class="modal" onsubmit={(e) => { e.preventDefault(); void save(); }}>
    <header>
      <h2>{existing ? "Edit calendar" : "New calendar"}</h2>
      <span class="spacer"></span>
      <button type="button" class="icon-btn" onclick={() => (cal.calendarDialog = null)} aria-label="Close"><Icon name="x" /></button>
    </header>
    <div class="body">
      <label class="field">Name <input class="input" name="name" bind:value={name} maxlength="60" autocomplete="off" /></label>
      <div class="field">
        <span>Colour</span>
        <div class="swatches" role="radiogroup" aria-label="Colour">
          {#each CALENDAR_COLORS as c}
            <button type="button" class="swatch" class:on={color.toLowerCase() === c} style:background={c} onclick={() => (color = c)} role="radio" aria-checked={color.toLowerCase() === c} aria-label={c}></button>
          {/each}
          <input class="input hex" bind:value={color} spellcheck="false" aria-label="Hex colour" aria-invalid={!validColor} />
        </div>
      </div>
      {#if existing}
        <div class="tools">
          <button type="button" class="btn sm" onclick={() => cal.exportCalendar(existing)}><Icon name="download" size={14} /> Export .ics</button>
          <button type="button" class="btn sm" onclick={() => cal.importInto(existing)} disabled={existing.read_only}><Icon name="plus" size={14} /> Import .ics</button>
        </div>
      {/if}
      {#if error}<div class="problem" role="alert">{error}</div>{/if}
    </div>
    <footer>
      {#if existing}
        {#if confirmDelete}
          <span class="muted small">Delete it and all its events?</span>
          <button type="button" class="btn sm danger" onclick={remove} disabled={busy}>Yes, delete</button>
          <button type="button" class="btn sm ghost" onclick={() => (confirmDelete = false)}>Keep</button>
        {:else}
          <button type="button" class="btn sm danger" onclick={() => (confirmDelete = true)} disabled={!canDelete || busy} title={canDelete ? "" : "Keep at least one local calendar"}><Icon name="trash" size={14} /> Delete</button>
        {/if}
      {/if}
      <span class="spacer"></span>
      <button type="button" class="btn" onclick={() => (cal.calendarDialog = null)}>Cancel</button>
      <button type="submit" class="btn primary" disabled={!name.trim() || !validColor || busy}>Save</button>
    </footer>
  </form>
</div>

<style>
  .modal { width: min(420px, 100%); }
  header { display: flex; align-items: center; gap: 8px; padding: 12px 16px; border-bottom: 1px solid var(--line); }
  .spacer { flex: 1; }
  .body { padding: 14px 16px; display: grid; gap: 14px; }
  .swatches { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
  .swatch { width: 24px; height: 24px; border-radius: 50%; border: 2px solid transparent; cursor: pointer; padding: 0; }
  .swatch.on { border-color: var(--fg-0); box-shadow: 0 0 0 2px var(--bg-2) inset; }
  .hex { width: 100px; font-family: var(--font-mono); font-size: 12.5px; }
  .hex[aria-invalid="true"] { border-color: var(--danger); }
  .tools { display: flex; gap: 8px; }
  .problem { color: var(--danger); font-size: 12.5px; }
  .small { font-size: 12.5px; }
  footer { display: flex; gap: 8px; align-items: center; padding: 10px 16px; border-top: 1px solid var(--line); }
</style>
