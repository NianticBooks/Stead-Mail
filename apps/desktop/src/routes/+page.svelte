<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "$lib/state.svelte";
  import { isUnread, isStarred } from "$lib/format";
  import Sidebar from "$lib/components/Sidebar.svelte";
  import MessageList from "$lib/components/MessageList.svelte";
  import Reader from "$lib/components/Reader.svelte";
  import Compose from "$lib/components/Compose.svelte";
  import Settings from "$lib/components/Settings.svelte";
  import OutboxPanel from "$lib/components/OutboxPanel.svelte";
  import Shortcuts from "$lib/components/Shortcuts.svelte";
  import Toasts from "$lib/components/Toasts.svelte";
  import CalendarView from "$lib/components/CalendarView.svelte";
  import EventDialog from "$lib/components/EventDialog.svelte";
  import CalendarDialog from "$lib/components/CalendarDialog.svelte";
  import ConnectDialog from "$lib/components/ConnectDialog.svelte";
  import SyncLogDialog from "$lib/components/SyncLogDialog.svelte";
  import { cal } from "$lib/calendar.svelte";

  let list = $state<MessageList | null>(null);
  let chord = $state<string | null>(null);
  let chordTimer: ReturnType<typeof setTimeout> | undefined;

  onMount(() => {
    void app.init().catch((e) => app.toast("error", String(e)));
  });

  const overlayOpen = $derived(app.compose !== null || app.settingsOpen || app.outboxOpen || app.helpOpen || cal.overlayOpen);

  // Entering the calendar loads what it shows; leaving costs nothing.
  $effect(() => {
    if (app.view === "calendar") void cal.open();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    // Escape closes the topmost dialog wherever focus is. (Compose handles its own, with a
    // discard confirmation, and stops the event before it gets here.)
    if (e.key === "Escape") {
      if (app.helpOpen) return void (app.helpOpen = false);
      if (app.outboxOpen) return void (app.outboxOpen = false);
      if (app.settingsOpen && app.accounts.length > 0) return void (app.settingsOpen = false);
    }
    const t = e.target as HTMLElement | null;
    const typing = !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable);
    if (typing || overlayOpen) return;

    if (chord === "g") {
      chord = null;
      clearTimeout(chordTimer);
      if (e.key === "i") {
        e.preventDefault();
        app.view = "mail";
        void app.selectMailbox("INBOX");
      } else if (e.key === "c") {
        e.preventDefault();
        app.view = "calendar";
      }
      return;
    }

    if (app.view === "calendar") {
      switch (e.key) {
        case "t": e.preventDefault(); cal.today(); break;
        case "n": e.preventDefault(); cal.openNew(); break;
        case "[": case "ArrowLeft": e.preventDefault(); cal.step(-1); break;
        case "]": case "ArrowRight": e.preventDefault(); cal.step(1); break;
        case "m": cal.setView("month"); break;
        case "w": cal.setView("week"); break;
        case "d": cal.setView("day"); break;
        case "a": cal.setView("agenda"); break;
        case "s": if (cal.hasSynced) { e.preventDefault(); void cal.sync(); } break;
        case "?": app.helpOpen = true; break;
        case "g":
          chord = "g";
          chordTimer = setTimeout(() => (chord = null), 1200);
          break;
      }
      return;
    }

    switch (e.key) {
      case "j":
      case "ArrowDown":
        e.preventDefault();
        void app.step(1);
        break;
      case "k":
      case "ArrowUp":
        e.preventDefault();
        void app.step(-1);
        break;
      case "c":
        e.preventDefault();
        app.openCompose();
        break;
      case "r":
        if (app.detail) {
          e.preventDefault();
          app.reply();
        }
        break;
      case "u":
        if (app.detail) void app.setSeen(app.detail.uid, isUnread(app.detail));
        break;
      case "s":
        if (app.detail) void app.setStarred(app.detail.uid, !isStarred(app.detail));
        break;
      case "/":
        e.preventDefault();
        list?.focusSearch();
        break;
      case "?":
        app.helpOpen = true;
        break;
      case "g":
        chord = "g";
        chordTimer = setTimeout(() => (chord = null), 1200);
        break;
      case "Escape":
        if (app.hits !== null) app.clearSearch();
        break;
    }
  }
</script>

<svelte:window {onkeydown} />
<svelte:head><title>{app.currentMailbox?.unseen ? `(${app.currentMailbox.unseen}) ` : ""}Mailcore</title></svelte:head>

<div class="shell" class:cal={app.view === "calendar"} inert={overlayOpen}>
  <Sidebar />
  {#if app.view === "mail"}
    <MessageList bind:this={list} />
    <Reader />
  {:else}
    <CalendarView />
  {/if}
</div>

{#if cal.dialog}<EventDialog />{/if}
{#if cal.calendarDialog}<CalendarDialog />{/if}
{#if cal.connectOpen}<ConnectDialog />{/if}
{#if cal.logOpen}<SyncLogDialog />{/if}

<Compose />
<Settings />
<OutboxPanel />
<Shortcuts />
<Toasts />

<style>
  .shell.cal { grid-template-columns: 212px minmax(0, 1fr); }
  .shell { display: grid; grid-template-columns: 212px minmax(270px, 340px) minmax(0, 1fr); height: 100vh; }
</style>
