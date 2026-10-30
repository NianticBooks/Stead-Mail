<script lang="ts">
  import { autofocus } from "../focus";
  import { modKey } from "../platform";
  import { app } from "../state.svelte";
  import Icon from "./Icon.svelte";

  // Each shortcut is a list of alternatives; each alternative is a sequence of keys.
  const rows: { keys: string[][]; what: string }[] = [
    { keys: [["j"], ["↓"]], what: "Next message" },
    { keys: [["k"], ["↑"]], what: "Previous message" },
    { keys: [["c"]], what: "Compose" },
    { keys: [["r"]], what: "Reply" },
    { keys: [["Shift", "R"]], what: "Reply to everyone" },
    { keys: [["u"]], what: "Mark read / unread" },
    { keys: [["s"]], what: "Star / unstar" },
    { keys: [["#"], ["Delete"]], what: "Move to Trash" },
    { keys: [["/"]], what: "Search" },
    { keys: [["g", "i"]], what: "Go to Inbox (press g, then i)" },
    { keys: [["g", "c"]], what: "Go to Calendar (press g, then c)" },
    { keys: [[modKey, "Enter"]], what: "Send (in compose) or save (in an event)" },
    { keys: [["t"]], what: "Calendar: jump to today" },
    { keys: [["n"]], what: "Calendar: new event" },
    { keys: [["["], ["←"]], what: "Calendar: previous period" },
    { keys: [["]"], ["→"]], what: "Calendar: next period" },
    { keys: [["m"], ["w"], ["d"], ["a"]], what: "Calendar: month, week, day, agenda" },
    { keys: [["s"]], what: "Calendar: sync with Google and iCloud now" },
    { keys: [["Esc"]], what: "Close dialog, clear search" },
    { keys: [["?"]], what: "This help" },
  ];
</script>

{#if app.helpOpen}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="overlay" role="dialog" aria-modal="true" aria-label="Keyboard shortcuts" tabindex="-1" use:autofocus onkeydown={(e) => e.key === "Escape" && (app.helpOpen = false)}>
    <div class="modal">
      <header><h2>Keyboard shortcuts</h2><button class="icon-btn" onclick={() => (app.helpOpen = false)} aria-label="Close"><Icon name="x" /></button></header>
      <dl>
        {#each rows as r}
          <dt>{#each r.keys as alt, i}{#if i > 0}<span class="muted or">or</span>{/if}{#each alt as k, j}{#if j > 0}<span class="muted">+</span>{/if}<kbd>{k}</kbd>{/each}{/each}</dt>
          <dd>{r.what}</dd>
        {/each}
      </dl>
    </div>
  </div>
{/if}

<style>
  .modal { width: min(420px, 100%); }
  header { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; border-bottom: 1px solid var(--line); }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 10px 18px; margin: 0; padding: 16px 20px 20px; align-items: center; }
  dt { white-space: nowrap; display: flex; align-items: center; gap: 4px; }
  .or { font-size: 12px; margin: 0 2px; }
  dd { margin: 0; color: var(--fg-1); }
</style>
