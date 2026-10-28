<script lang="ts">
  import { app } from "../state.svelte";
  import Icon from "./Icon.svelte";
  import Avatar from "./Avatar.svelte";
  import { isQuietFolder } from "../mailboxes";

  let showQuiet = $state(false);

  const visible = $derived(app.folders.filter((f) => showQuiet || !isQuietFolder(f.mailbox) || f.mailbox.name === app.mailbox));
  const hiddenCount = $derived(app.folders.length - visible.length);
</script>

<aside class="rail" aria-label="Accounts and folders">
  <div class="top">
    <div class="brand"><Icon name="mail" size={18} /> Mailcore</div>
    <button class="btn primary compose" onclick={() => app.openCompose()} title="Compose (c)">
      <Icon name="compose" size={15} /> Compose
    </button>
  </div>

  <nav class="scroll">
    {#each app.accounts as a (a.id)}
      <button class="account" class:active={a.id === app.accountId} onclick={() => a.id !== app.accountId && app.selectAccount(a.id)}>
        <Avatar label={a.name} size={26} />
        <span class="who"><strong>{a.name}</strong><span class="muted">{a.email}</span></span>
      </button>

      {#if a.id === app.accountId}
        <ul class="folders">
          {#each visible as f (f.mailbox.id)}
            <li>
              <button
                class="folder"
                class:active={f.mailbox.name === app.mailbox}
                style:padding-left="{10 + f.depth * 14}px"
                onclick={() => app.selectMailbox(f.mailbox.name)}
              >
                <Icon name={f.icon === "star" ? "star" : f.icon} size={15} />
                <span class="label">{f.label}</span>
                {#if f.mailbox.unseen}<span class="badge">{f.mailbox.unseen > 9999 ? "9999+" : f.mailbox.unseen}</span>{/if}
              </button>
            </li>
          {/each}
          {#if app.folders.length === 0}
            <li class="muted hint">{app.syncing ? "Loading folders…" : "No folders yet. Press sync."}</li>
          {/if}
          {#if hiddenCount > 0 || showQuiet}
            <li>
              <button class="folder quiet" onclick={() => (showQuiet = !showQuiet)}>
                <Icon name="chevron" size={14} />
                <span class="label">{showQuiet ? "Hide Trash and Junk" : `Show ${hiddenCount} more`}</span>
              </button>
            </li>
          {/if}
        </ul>
      {/if}
    {/each}
  </nav>

  <div class="foot">
    <button class="foot-btn" class:alert={app.failedOutbox > 0} onclick={() => (app.outboxOpen = true)} title="Outgoing mail">
      <Icon name="send" size={15} /> Outbox
      {#if app.pendingOutbox > 0}<span class="badge" class:danger={app.failedOutbox > 0} class:warn={app.failedOutbox === 0}>{app.pendingOutbox}</span>{/if}
    </button>
    <button class="icon-btn" onclick={() => (app.helpOpen = true)} title="Keyboard shortcuts (?)" aria-label="Keyboard shortcuts"><Icon name="keyboard" /></button>
    <button class="icon-btn" onclick={() => (app.settingsOpen = true)} title="Settings" aria-label="Settings"><Icon name="sliders" /></button>
  </div>
</aside>

<style>
  .rail { display: flex; flex-direction: column; min-height: 0; background: var(--bg-1); border-right: 1px solid var(--line); }
  .top { padding: 14px 12px 10px; display: grid; gap: 12px; }
  .brand { display: flex; align-items: center; gap: 8px; font-weight: 700; font-size: 15px; padding: 0 4px; letter-spacing: 0.01em; }
  .compose { width: 100%; padding: 8px 12px; }
  .scroll { flex: 1; overflow-y: auto; padding: 4px 8px 12px; min-height: 0; }
  .account {
    display: flex; align-items: center; gap: 9px; width: 100%; text-align: left; padding: 8px; margin-top: 6px;
    background: transparent; border: 0; border-radius: var(--radius); cursor: pointer; color: var(--fg-1);
  }
  .account:hover { background: var(--bg-2); }
  .account.active { color: var(--fg-0); }
  .who { display: grid; min-width: 0; line-height: 1.25; }
  .who strong { font-weight: 600; font-size: 13px; }
  .who span { font-size: 11.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .folders { list-style: none; margin: 2px 0 6px; padding: 0; display: grid; gap: 1px; }
  .folder {
    display: flex; align-items: center; gap: 9px; width: 100%; text-align: left; padding: 6px 10px; border: 0;
    background: transparent; border-radius: var(--radius-sm); color: var(--fg-1); cursor: pointer;
  }
  .folder:hover { background: var(--bg-2); color: var(--fg-0); }
  .folder.active { background: var(--accent-bg); color: var(--fg-0); }
  .folder.active :global(svg) { color: var(--accent); }
  .folder.quiet { color: var(--fg-3); font-size: 12px; }
  .label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .hint { padding: 6px 12px; font-size: 12.5px; }
  .foot { display: flex; align-items: center; gap: 2px; padding: 8px; border-top: 1px solid var(--line); }
  .foot-btn {
    flex: 1; display: flex; align-items: center; gap: 8px; padding: 6px 8px; background: transparent; border: 0;
    border-radius: var(--radius-sm); color: var(--fg-1); cursor: pointer; text-align: left;
  }
  .foot-btn:hover { background: var(--bg-2); color: var(--fg-0); }
  .foot-btn.alert { color: var(--danger); }
</style>
