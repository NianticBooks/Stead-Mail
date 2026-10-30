<script lang="ts">
  import { autofocus } from "../focus";
  import { app } from "../state.svelte";
  import { api } from "../api";
  import Icon from "./Icon.svelte";
  import Avatar from "./Avatar.svelte";
  import AiSettings from "./AiSettings.svelte";
  import type { CalendarSummary, OAuthIdentity, Provider } from "../types";

  type Tab = "accounts" | "ai" | "google" | "general";
  let tab = $state<Tab>("accounts");

  // ---- add account
  let adding = $state(false);
  let provider = $state<Provider>("icloud");
  let name = $state("");
  let email = $state("");
  let password = $state("");
  let imapHost = $state("");
  let imapPort = $state("993");
  let smtpHost = $state("");
  let smtpPort = $state("587");
  let busy = $state(false);
  let formError = $state<string | null>(null);
  let formWarning = $state<string | null>(null);
  let confirmRemove = $state<string | null>(null);

  const hints: Record<Provider, { title: string; steps: string }> = {
    icloud: { title: "iCloud Mail", steps: "Use an app-specific password, not your Apple ID password: appleid.apple.com → Sign-In and Security → App-Specific Passwords." },
    gmail: { title: "Gmail", steps: "Turn on 2-Step Verification, then create an app password at myaccount.google.com/apppasswords and paste the 16 characters here." },
    custom: { title: "Other (IMAP)", steps: "Enter your provider’s IMAP and SMTP servers. Port 993 uses TLS directly; SMTP on 587 uses STARTTLS and 465 uses TLS." },
  };

  const canSubmit = $derived(email.includes("@") && password.length > 0 && (provider !== "custom" || (imapHost.trim() !== "" && smtpHost.trim() !== "")));

  function resetForm() {
    name = ""; email = ""; password = ""; imapHost = ""; imapPort = "993"; smtpHost = ""; smtpPort = "587";
    formError = null; formWarning = null;
  }

  async function submit(e: Event) {
    e.preventDefault();
    if (!canSubmit || busy) return;
    busy = true;
    formError = null;
    formWarning = null;
    try {
      const label = name.trim() || (provider === "custom" ? email.split("@")[1] : hints[provider].title.split(" ")[0]);
      const r = await app.addAccount({
        name: label, email: email.trim(), provider, password,
        ...(provider === "custom" ? { imap_host: imapHost.trim(), imap_port: Number(imapPort) || 993, smtp_host: smtpHost.trim(), smtp_port: Number(smtpPort) || 587 } : {}),
      });
      if (r.warning) app.toast("info", r.warning);
      resetForm();
      adding = false;
      if (app.accounts.length === 1) app.settingsOpen = false;
    } catch (err) {
      formError = typeof err === "string" ? err : String(err);
    } finally {
      busy = false;
    }
  }

  async function remove(id: string) {
    try {
      await app.removeAccount(id);
      confirmRemove = null;
    } catch (e) {
      app.toast("error", typeof e === "string" ? e : String(e));
    }
  }

  // ---- Google Calendar
  let gConfigured = $state<boolean | null>(null);
  let gClientId = $state("");
  let gSecret = $state("");
  let gIdentities = $state<OAuthIdentity[]>([]);
  let gBusy = $state(false);
  let gError = $state<string | null>(null);
  let gCalendars = $state<Record<string, CalendarSummary[]>>({});

  async function loadGoogle() {
    try {
      gConfigured = (await api.googleConfigured()).configured;
      gIdentities = await api.googleIdentities();
    } catch (e) {
      gError = String(e);
    }
  }

  $effect(() => {
    if (app.settingsOpen && tab === "google" && gConfigured === null) void loadGoogle();
  });

  async function saveClient(e: Event) {
    e.preventDefault();
    gBusy = true;
    gError = null;
    try {
      await api.googleConfigure(gClientId.trim(), gSecret.trim() || null);
      gSecret = "";
      gConfigured = true;
    } catch (err) {
      gError = String(err);
    } finally {
      gBusy = false;
    }
  }

  async function signIn() {
    gBusy = true;
    gError = null;
    try {
      await api.googleLogin();
      gIdentities = await api.googleIdentities();
    } catch (err) {
      gError = typeof err === "string" ? err : String(err);
    } finally {
      gBusy = false;
    }
  }

  async function showCalendars(email: string) {
    try {
      gCalendars[email] = await api.googleCalendars(email);
    } catch (err) {
      gError = String(err);
    }
  }

  async function signOut(email: string) {
    try {
      await api.googleLogout(email);
      gIdentities = await api.googleIdentities();
      delete gCalendars[email];
    } catch (err) {
      gError = String(err);
    }
  }

  function close() {
    if (app.accounts.length === 0) return; // first run: an account is required to continue
    app.settingsOpen = false;
    adding = false;
  }

  $effect(() => {
    // First run opens straight onto the add-account form.
    if (app.settingsOpen && app.accounts.length === 0) adding = true;
  });
</script>

{#if app.settingsOpen}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="overlay" role="dialog" aria-modal="true" aria-label="Settings" tabindex="-1" use:autofocus onkeydown={(e) => e.key === "Escape" && close()}>
    <div class="modal settings">
      <nav>
        <h2>Settings</h2>
        {#each [["accounts", "Accounts"], ["ai", "AI"], ["google", "Google Calendar"], ["general", "General"]] as [id, label]}
          <button class="tab" class:active={tab === id} onclick={() => (tab = id as Tab)}>{label}</button>
        {/each}
        <span class="grow"></span>
        {#if app.accounts.length > 0}<button class="btn" onclick={close}>Done</button>{/if}
      </nav>

      <div class="pane">
        {#if tab === "accounts"}
          {#if app.accounts.length === 0 && !app.booted}
            <p class="muted">Loading…</p>
          {/if}
          {#if app.accounts.length === 0}
            <div class="welcome"><Icon name="mail" size={30} /><h1>Welcome to Stead</h1><p class="muted">Add your first account to get started. Mail stays on this computer.</p></div>
          {/if}

          {#each app.accounts as a (a.id)}
            <div class="acct">
              <Avatar label={a.name} size={34} />
              <div class="info"><strong>{a.name}</strong><span class="muted">{a.email} · {a.imap.host}</span></div>
              {#if confirmRemove === a.id}
                <span class="muted">Remove and forget its password?</span>
                <button class="btn danger sm" onclick={() => remove(a.id)}>Remove</button>
                <button class="btn sm" onclick={() => (confirmRemove = null)}>Cancel</button>
              {:else}
                <button class="btn sm" onclick={() => (confirmRemove = a.id)}>Remove</button>
              {/if}
            </div>
          {/each}

          {#if !adding}
            <button class="btn" onclick={() => (adding = true)}><Icon name="plus" size={15} /> Add account</button>
          {:else}
            <form onsubmit={submit} class="form">
              <h3>Add account</h3>
              <div class="seg" role="tablist">
                {#each ["icloud", "gmail", "custom"] as p}
                  <button type="button" role="tab" aria-selected={provider === p} class:active={provider === p} onclick={() => (provider = p as Provider)}>{hints[p as Provider].title}</button>
                {/each}
              </div>
              <p class="hint muted">{hints[provider].steps}</p>
              <label class="field">Email address<input class="input" type="email" bind:value={email} placeholder="you@example.com" autocomplete="off" required /></label>
              <label class="field">{provider === "custom" ? "Password" : "App password"}<input class="input" type="password" bind:value={password} autocomplete="off" required /></label>
              <label class="field"><span>Display name <span class="muted">(optional)</span></span><input class="input" bind:value={name} placeholder={provider === "custom" ? "Work" : hints[provider].title.split(" ")[0]} /></label>
              {#if provider === "custom"}
                <div class="grid2">
                  <label class="field">IMAP server<input class="input" bind:value={imapHost} placeholder="imap.example.com" /></label>
                  <label class="field">Port<input class="input" bind:value={imapPort} inputmode="numeric" /></label>
                  <label class="field">SMTP server<input class="input" bind:value={smtpHost} placeholder="smtp.example.com" /></label>
                  <label class="field">Port<input class="input" bind:value={smtpPort} inputmode="numeric" /></label>
                </div>
              {/if}
              {#if formError}<div class="error" role="alert"><Icon name="alert" size={15} /> {formError}</div>{/if}
              {#if formWarning}<div class="warn">{formWarning}</div>{/if}
              <div class="btnrow">
                <button class="btn primary" type="submit" disabled={!canSubmit || busy}><Icon name={busy ? "loader" : "check"} size={15} /> {busy ? "Checking connection…" : "Add and check"}</button>
                {#if app.accounts.length > 0}<button class="btn ghost" type="button" onclick={() => { adding = false; resetForm(); }}>Cancel</button>{/if}
              </div>
              <p class="muted tiny">Your password is stored in the system keyring, never in a file.</p>
            </form>
          {/if}
        {:else if tab === "ai"}
          <AiSettings />
        {:else if tab === "google"}
          <h3>Google Calendar</h3>
          <p class="muted">Connect Google Calendar to see and accept invitations. Only calendar permissions are requested; mail never goes through Google’s API.</p>
          {#if gError}<div class="error" role="alert"><Icon name="alert" size={15} /> {gError}</div>{/if}
          {#if gConfigured === null}
            <p class="muted">Loading…</p>
          {:else if !gConfigured}
            <form onsubmit={saveClient} class="form">
              <p class="hint muted">Create a <strong>Desktop app</strong> OAuth client in Google Cloud Console (APIs &amp; Services → Credentials), enable the Google Calendar API, and add your own account as a test user.</p>
              <label class="field">Client ID<input class="input" bind:value={gClientId} placeholder="…apps.googleusercontent.com" required spellcheck="false" /></label>
              <label class="field">Client secret<input class="input" type="password" bind:value={gSecret} autocomplete="off" /></label>
              <div class="btnrow"><button class="btn primary" type="submit" disabled={gBusy || !gClientId.trim()}>Save client</button></div>
            </form>
          {:else}
            {#each gIdentities as g (g.id)}
              <div class="acct">
                <Avatar label={g.email} size={34} />
                <div class="info"><strong>{g.email}</strong><span class="muted">Signed in · calendar access</span></div>
                <button class="btn sm" onclick={() => showCalendars(g.email)}>Calendars</button>
                <button class="btn sm" onclick={() => signOut(g.email)}>Sign out</button>
              </div>
              {#if gCalendars[g.email]}
                <ul class="cals">{#each gCalendars[g.email] as c (c.id)}<li>{c.summary}{#if c.primary} <span class="badge">primary</span>{/if} <span class="muted">{c.access_role}</span></li>{/each}</ul>
              {/if}
            {/each}
            <button class="btn" onclick={signIn} disabled={gBusy}>
              <Icon name={gBusy ? "loader" : "plus"} size={15} /> {gBusy ? "Finish signing in in your browser…" : gIdentities.length ? "Add another Google account" : "Sign in with Google"}
            </button>
          {/if}
        {:else}
          <h3>General</h3>
          <label class="toggle">
            <input type="checkbox" checked={app.prefs.markReadOnOpen} onchange={(e) => app.setPref("markReadOnOpen", e.currentTarget.checked)} />
            <span>Mark messages as read when I open them<br /><span class="muted">Also marks them read on the mail server.</span></span>
          </label>
          <div class="privacy">
            <Icon name="shield" size={16} />
            <span>Remote images are always blocked until you load them for a single message. Scripts and forms in mail never run.</span>
          </div>
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .settings { width: min(820px, 100%); height: min(620px, calc(100vh - 48px)); flex-direction: row; }
  nav { width: 190px; flex: none; display: flex; flex-direction: column; gap: 2px; padding: 16px 10px; background: var(--bg-1); border-right: 1px solid var(--line); }
  nav h2 { padding: 0 10px 12px; font-size: 15px; }
  .tab { text-align: left; padding: 8px 10px; background: transparent; border: 0; border-radius: var(--radius-sm); color: var(--fg-1); cursor: pointer; }
  .tab:hover { background: var(--bg-2); }
  .tab.active { background: var(--accent-bg); color: var(--fg-0); }
  .grow { flex: 1; }
  .pane { flex: 1; overflow-y: auto; padding: 22px 26px; display: grid; gap: 14px; align-content: start; min-width: 0; }
  .welcome { display: grid; gap: 4px; justify-items: start; padding-bottom: 6px; }
  .welcome h1 { font-size: 20px; margin-top: 8px; }
  .acct { display: flex; align-items: center; gap: 12px; padding: 10px 12px; background: var(--bg-1); border: 1px solid var(--line); border-radius: var(--radius); }
  .info { flex: 1; min-width: 0; display: grid; line-height: 1.3; }
  .info span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 12.5px; }
  .form { display: grid; gap: 12px; padding: 16px; background: var(--bg-1); border: 1px solid var(--line); border-radius: var(--radius); }
  .seg { display: flex; gap: 4px; padding: 3px; background: var(--bg-0); border: 1px solid var(--line); border-radius: var(--radius); }
  .seg button { flex: 1; padding: 6px 8px; background: transparent; border: 0; border-radius: var(--radius-sm); color: var(--fg-1); cursor: pointer; }
  .seg button.active { background: var(--bg-3); color: var(--fg-0); }
  .hint { margin: 0; font-size: 12.5px; line-height: 1.5; }
  .tiny { font-size: 12px; margin: 0; }
  .grid2 { display: grid; grid-template-columns: 1fr 90px; gap: 10px; }
  .btnrow { display: flex; gap: 8px; }
  .error { display: flex; gap: 8px; align-items: flex-start; padding: 8px 10px; border-radius: var(--radius-sm); color: var(--danger); background: color-mix(in srgb, var(--danger) 10%, transparent); border: 1px solid color-mix(in srgb, var(--danger) 35%, transparent); overflow-wrap: anywhere; }
  .warn { padding: 8px 10px; border-radius: var(--radius-sm); color: var(--warn); background: color-mix(in srgb, var(--warn) 10%, transparent); }
  .cals { margin: -6px 0 0; padding: 0 0 0 58px; list-style: none; display: grid; gap: 4px; color: var(--fg-1); }
  .toggle { display: flex; gap: 12px; align-items: flex-start; cursor: pointer; line-height: 1.4; }
  .toggle input { margin-top: 3px; }
  .privacy { display: flex; gap: 10px; align-items: flex-start; padding: 12px; background: var(--bg-1); border: 1px solid var(--line); border-radius: var(--radius); color: var(--fg-1); }
</style>
