# Parity audit: agents slice (buzz-app → buzz-gpui)

Scope: buzz-app `src/features/{agents,agents2,host,sessions,pairing,workflows}`, `src/bundled/{agents,agents2,agent-activity,sessions,terminal,workflows,bestie,builderlab,pairing}`, `src/agent-plugins`, `src/plugins`, `src/buzz-mcp`, and the native agent, terminal, pairing and OAuth modules in `src-tauri/src`. The audit covers the main checkout only; `.claude/worktrees/` was ignored. Remote agent hosts (buzzd, Add host, per-host sections, host badges) are intended additions and are not listed as gaps.

## Path conventions

- **buzz-app paths** are relative to `/home/ben/projects/buzz-app`. Short forms:
  - `BA/` = `src/bundled/agents/`
  - `FA/` = `src/features/agents/`
  - `ST/` = `src-tauri/src/`
- **Port paths** are relative to `/home/ben/projects/buzz-gpui/crates`. Short forms:
  - `ui/agents.rs` = `app/src/ui/agents.rs`
  - `ui/agents/*.rs` = `app/src/ui/agents/*.rs`
  - `host/` = `host/src/`
- **Size:** S = hours, M = one to three days, L = a week or more.

## Work in progress elsewhere

Another agent is adding these. They are marked **IN PROGRESS** wherever they appear:

- agent restart on save;
- provider "Test connection";
- Databricks workspace and filter fields;
- Databricks "Disconnect";
- Escape closing dropdowns;
- clearing an agent's effort.

---

## 1. Agents page frame and inventory (`BA/AgentsPage.tsx`, `AgentControlPanel.tsx`, `UnifiedInventory.tsx`, `InventoryView.tsx`, `inventory-*.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Body when no desktop control or runtime unavailable: "Open the desktop app to import and run agents…", plus the teams and old-library slots | BA/AgentsPage.tsx:358-365, 466-473 | DIFFERENT: the port always has an embedded host. When the host is offline it shows only the connection alert (ui/agents.rs:487-537) and no library fallback | Show the old library and teams when no host can be reached | S |
| Legacy `ManagedAgents` body for older hosts (header "Individual agents" + Use-here explainer + "No agents yet. Create an agent or import one from old Buzz below.") | BA/AgentsPage.tsx:483-534 | DIFFERENT: not needed while the GUI and host protocol ship together | Nothing, or keep for mixed versions | S |
| Old-library slot: "Connect to a community to browse the old library." plus "Retry connection" | BA/AgentsPage.tsx:241-252 | MISSING | See §13 | S |
| Profile companion: clicking a tile opens the agent's profile PanelCard ("View profile for {name}") | BA/AgentsPage.tsx:163-197, 253-267; BA/AgentCard.tsx:394-405 | MISSING: a tile click always opens Manage (ui/agents.rs:686-709) | Companion dock with the profile panel | M |
| Routed edit target: a page route `{pubkey}` opens Edit for that agent, with opened, not-found and unavailable outcomes | BA/edit-route.ts:4-13; BA/index.tsx:23-27; BA/AgentControlPanel.tsx:146-178 | MISSING | Navigation route and deep link to Edit | M |
| Display names resolved through the relay identity-names service, with disambiguation | BA/AgentControlPanel.tsx:116-131 | MISSING: shows the saved `agent.name` only (ui/agents.rs:724) | Use community profile names | S |
| "Waiting for the desktop app…" while a control operation is busy | BA/AgentControlPanel.tsx:323 | DIFFERENT: pending state is tracked per card (ui/agents.rs:66-69) | Optional | S |
| Controller `state.error` shown as alert or status | BA/AgentControlPanel.tsx:310-314 | PARTIAL: only connection errors appear (ui/agents.rs:487-537). A failed snapshot read is swallowed (app/src/hosts.rs:399-416) | Surface snapshot and refresh errors | S |
| Inventory groups: "Individual agents" (local in this community, tiles); "Local agents in other communities"; "Available to import"; "Relay-only agents" (rows); community subsections; "Community unknown" | BA/InventoryView.tsx:169-243; BA/inventory-decisions.ts:4-7 | DIFFERENT: one flat tile grid per host with every agent, whatever its community (ui/agents.rs:558-588) | Group by community within each host and add the row-list groups | L |
| Row layout (`.agent-inventory-row`) with chevron details toggle, and a one-column layout at ≤640px | BA/AgentControls.css:16-91; BA/InventoryIdentityCard.tsx:350-385 | MISSING | Row component | M |
| "Archived (N)" accordion and "All your agents are archived in this community." | BA/InventoryView.tsx:267-300 | MISSING | Needs identity archive (§14) | M |
| "Profiles without identities" section | BA/InventoryView.tsx:302-316 | MISSING | Needs relay library (§13) | M |
| Teams slot on the Agents page (channel-templates) | BA/AgentsPage.tsx:345-350; BA/InventoryView.tsx:270 | MISSING | See §12 | M |
| Community inventory status lines: "Checking community inventory…"; "Agent names and pictures could not be checked for {community}. Refresh to retry."; "Community inventory could not be checked…"; "Showing known community associations. Community checks are not current." | BA/UnifiedInventory.tsx:384-404; BA/use-community-inventory.ts | MISSING | Per-community `agent-inventory` reads | M |
| Inventory warnings: "{warning} Retry local discovery by reopening the app." | BA/UnifiedInventory.tsx:342-346 | MISSING | Host warnings field | S |
| "Refresh agents" also refreshes the relay library, archives and community reads | BA/UnifiedInventory.tsx:356-371 | PARTIAL: refreshes only the host snapshot and harnesses (ui/agents.rs:231-237) | Extend once those sources exist | S |
| Background fetch of public profiles (names, pictures) for agent keys | BA/UnifiedInventory.tsx:205-214 | PARTIAL: uses the saved `agent.picture` only (ui/agents.rs:678-680) | Profile lookups | S |
| Focus and scroll to a newly imported card; focus returns to "Actions for…" after archive | BA/InventoryView.tsx:150-167; BA/AgentCard.tsx:107-112 | MISSING | Focus handling | S |

## 2. Agent card (`BA/AgentCard.tsx`, `FA/AgentAvatar.tsx`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Presence dot driven by relay presence (online, away, offline, unknown), re-read every 5s up to six times after start or stop | BA/AgentCard.tsx:113-141, 186 | DIFFERENT: derived from process status (ui/agents.rs:272-280, 677) | Use relay presence for the agent key | M |
| Thinking badge on the squircle avatar | BA/AgentCard.tsx:176-184 | MISSING | See §17-H | M |
| Model line: `launchModel`; "Custom model" when set through the environment; `harness.model`; "Default model". The harness is not shown | BA/AgentCard.tsx:153-160, 200-212 | DIFFERENT: no "Custom model" case, and a harness logo is drawn beside the model (ui/agents.rs:290-297, 745-757) | Add "Custom model"; decide on the logo | S |
| Tile has no process status or buttons; process control lives in Manage | BA/AgentCard.tsx:386-420 | DIFFERENT: the tile shows a status line and Start, Stop and Restart buttons (ui/agents.rs:758-768, 789-842) | Decide whether to keep the extra controls | S |
| "Review agent status" button when just imported, failed, profile pending, or there is a notice | BA/AgentCard.tsx:409-418; BA/AgentsPage.tsx:500-506 | MISSING | Button that opens Manage | S |
| "Archived" pill | BA/AgentCard.tsx (archived label) | MISSING | Needs archive | S |
| Menu item "View profile" | BA/AgentCard.tsx:240-252 | MISSING | Needs the companion (§1) | S |
| Menu items "Archive agent" / "Unarchive agent" (icons; disabled while pending) | BA/AgentCard.tsx:270-287 | MISSING | See §14 | S |
| Menu item "Duplicate", which opens Add agent as "Duplicate {name}" with name "{name} copy" | BA/AgentCard.tsx:322-340; BA/AgentControlPanel.tsx:105-110; BA/AgentCreateDialog.tsx:186-193, 338 | MISSING | Seed the create editor from an agent | S |
| Menu item "Share" (direct share or export) | BA/AgentCard.tsx:342-350 | MISSING | See §10 | S |
| Several local setups: "Edit/Duplicate/Delete {name}" choice rows with relay URL and npub | BA/AgentCard.tsx:296-365 | MISSING | Multi-setup menu | S |
| Disabled Edit with a note: "Import this identity to edit in Foundation." / "No linked identity to edit." | BA/AgentCard.tsx:368-380 | MISSING | — | S |
| Identity popover "{name} identity details" with npubs | BA/AgentCard.tsx:448-474 | MISSING | — | S |
| "No linked identity" | BA/AgentCard.tsx:476-480 | MISSING | — | S |
| Feedback slot: "Archiving…" / "Unarchiving…" / "{Archive\|Unarchive} failed: {error}" with Retry | BA/InventoryIdentityCard.tsx:200-235 | MISSING | — | S |
| Port-only menu items "Start on launch" (check) and "Retry profile publication" | n/a (buzz-app has no start-on-launch UI; profile retry is in Manage and Edit) | DIFFERENT (ui/agents.rs:952-968) | Decide whether to keep | S |
| Picture shown through `useAvatarPreview` (relay-hosted media preview) | BA/AgentCard.tsx:142-152 | PARTIAL: the URL goes straight to Avatar (ui/agents.rs:678-680) | Use the authenticated media loader | S |

## 3. Manage dialog and process control (`BA/ManagedAgentActions.tsx`, `BA/InventoryIdentityCard.tsx`, `FA/control.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Imported, not started. Start it when you are ready." / "…Choose Use here to set up this identity in a community." | BA/ManagedAgentActions.tsx:50-58 | MISSING: `configured` is never read (host/api.rs:177) | Read `configured` | S |
| "Checking agent status…" and outcome notices: "The agent didn't {start}. {reason} Try again." / "We couldn't confirm whether the agent started. Refresh status before trying again." | BA/ManagedAgentActions.tsx:88, 140-231 | PARTIAL: shows the raw RPC error (ui/agents.rs:145-147, 1057-1062) | Refresh after a failure and classify | S |
| Launch-block reasons: "Choose Use here before starting this imported identity.", "Refresh status before starting.", "Waiting for the current operation.", "The bundled agent runtime is unavailable.", "Waiting to start; unlock Keychain if prompted.", "Waiting for the process transition." | FA/control.ts:379-397 | PARTIAL: only not-connected, runtime-unavailable and Stopping (ui/agents.rs:1018-1026). The tile's Start ignores runtime availability (ui/agents.rs:806-815) | Shared block function for tile and dialog | S |
| Stop as recovery: allowed after an error or with an enabled pending launch | FA/control.ts:406-422 | PARTIAL: `stoppable()` covers Running, Starting and Waiting only (ui/agents.rs:282-287) | Match the rules | S |
| One operation at a time: "Another agent operation is in progress." / "Refresh local agents before trying again." | FA/control.ts:510-531 | DIFFERENT: pending is tracked per agent (ui/agents.rs:130-153) | Decide whether to serialize | S |
| Standard failure copy: "…Could not confirm the operation. Check current status and saved settings before retrying… Your edits are retained." | FA/control.ts:538-548 | DIFFERENT: raw error | Wrapper copy | S |
| Restart lives in Edit → Runtime, not in Manage | BA/ManagedAgentActions.tsx:100-135 | DIFFERENT: Manage has Restart / "Restart to apply" (ui/agents.rs:1106-1118) | Optional | S |
| Identity actions in Manage: installation select ("Choose an installation" / Installed / Development Buzz), Import, Use here, Clone (with tooltip), Remove | BA/InventoryIdentityCard.tsx:100-347 | MISSING | See §9 and §14 | L |
| "Identity & sources" details: communities, installations, npub | BA/InventoryIdentityCard.tsx:350-385 | MISSING | — | S |
| Host diagnostics shown in Manage | BA/AgentEditor.tsx:302-309 (buzz-app shows them only in Edit → Technical details) | DIFFERENT (ui/agents.rs:1063-1068) | Optional | S |
| Credential wait: dialogs can close and Stop stays available while a Keychain prompt is pending | FA/control.ts:313-319, 549-564 | PARTIAL: only the "Waiting" label exists (ui/agents.rs:249-258) | Pending-launch state | S |

## 4. Add agent dialog (`BA/AgentCreateDialog.tsx`, `AgentSettingsFields.tsx`, `AgentHarnessEditor.tsx`, `AgentEnvironmentEditor.tsx`, `ProviderApiKeyField.tsx`, `agent-edit.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Destination is the current community; there is no picker | BA/AgentCreateDialog.tsx:116-135; BA/AgentControlPanel.tsx:300-312 | DIFFERENT: the port adds a Location block with Host (intended) and a Community select (ui/agents/editor.rs:680-755) | The Community select is a deviation; keep or drop | S |
| Sidebar "Import" button opens "Import agent snapshot" | BA/AgentCreateDialog.tsx:368-379 | DIFFERENT: the sidebar button "Import from Buzz" opens old-install import (ui/agents/editor.rs:930-941) | Add snapshot import (§10) | M |
| Catalog "AGENTS" list in the sidebar, preview pane, "Add agent" / "Added to My Agents", "Adding agent…", "Withdrawn agent" | BA/AgentCreateDialog.tsx:380-465 | MISSING: the "AGENTS" label is reused for import candidates (ui/agents/import.rs:206-245) | See §11 | M |
| Seeded drafts from catalog and clone (runtime, model, provider, picture, session policy) and the clone notice | BA/AgentCreateDialog.tsx:67-104, 340-360 | MISSING | — | S |
| Default harness: the agent-defaults harness if available, else `buzz-agent` | BA/AgentCreateDialog.tsx:40-63 | DIFFERENT: defaults harness if installed, else the first installed, else Custom (ui/agents/form.rs:311-333) | Align the fallback | S |
| Default provider for Buzz Agent: the first provider (`databricks_v2`) unless defaults supply one | BA/AgentCreateDialog.tsx:56-62 | DIFFERENT: defaults to Inherit / "Not set" (ui/agents/form.rs:290) | Align | S |
| Harness help: "Pi needs its CLI, Node.js and buzz-pi-acp…"; "{preset} needs its ACP launcher. Set it up under Settings → Agents → Harnesses."; link "Open Harnesses in Settings"; "Opening Settings discards unsaved edits." | BA/AgentHarnessEditor.tsx:103-131 | PARTIAL: generic "{label} needs {missing} before it can run. {hint}" and no link (ui/agents/form.rs:554-568) | Exact copy and a Settings link | S |
| Harness option "{label} (current executable)" for a saved preset command; label "Custom executable / current value" | BA/AgentHarnessEditor.tsx:60-77 | PARTIAL: the label is "Custom executable" (ui/agents/form.rs:537-545) | — | S |
| A harness change resets args to the option's `defaultArgs`, the provider to its first provider, and the model to "" | BA/AgentHarnessEditor.tsx:78-101 | PARTIAL: resets the provider and effort only; args and model are kept (ui/agents/form.rs:408-415) | Native `defaultArgs` and resets | S |
| Preset alert: "This agent has model or provider settings that Buzz cannot apply to {label} yet…", "Current model/provider", "Use {label} defaults" button; validation "Use {preset} defaults before saving this agent." | BA/AgentSettingsFields.tsx:280-310; BA/agent-edit.ts:81-82 | DIFFERENT: model and provider are dropped silently on save (ui/agents/editor.rs:404-410) | Show the alert and button | S |
| `PresetSetupHint`: backticked commands as inline code; setup URL | FA/PresetSetupHint.tsx:1-8 | PARTIAL: plain text with literal backticks, and `setup_url` is never shown (ui/agents/form.rs:484-487) | Inline-code spans and a link | S |
| Windows notices: Git Bash needed; "Databricks sign-in is not supported on Windows yet." | BA/AgentSettingsFields.tsx:224-236 | MISSING (relevant only if the port ships on Windows) | — | S |
| Hidden `GOOSE_PROVIDER` override notice | BA/AgentSettingsFields.tsx:237-243 | MISSING | — | S |
| API key field: eye toggle ("Show API key" / "Hide API key"); "Will remove on save"; helper "…Keys exported in your shell profile are not used. Saved keys are stored in this device's local agent settings files." plus Goose and Pi variants | BA/ProviderApiKeyField.tsx:32-70; BA/AgentSettingsFields.tsx:245-275 | PARTIAL: masked field with no reveal; different helper (ui/agents/form.rs:699-715) | Reveal toggle and copy | S |
| A typed key belongs to the provider it was typed for; changing the provider drops it | BA/AgentSettingsFields.tsx:171-187 | DIFFERENT: the key resolves at save time, so a typed key moves to the new provider's variable (ui/agents/form.rs:376-386) | Drop the typed key when the provider changes | S |
| Per-agent "Effort" select | not in buzz-app's agent form (effort exists only in defaults and snapshots: src/app/AgentDefaultsCard.tsx:720) | DIFFERENT: the port adds an Effort select (ui/agents/form.rs:853-901) | Keep as an extension or remove | S |
| Clearing an agent's effort | — | IN PROGRESS (ui/agents/editor.rs:416-420 only sends a set value) | — | S |
| Escape closes open selects and the model dropdown | Base UI Select / Combobox; BA/AgentModelPicker.tsx:388, 418 | IN PROGRESS | — | S |
| Owner-only build note: "This build allows instructions only from the owner and verified same-owner agents." | ui.md §5a.7 (BA/AgentSettingsFields.tsx) | MISSING | — | S |
| Footer helper variants for Pi and presets ("Configure {preset}'s model and sign-in in the harness itself…") | BA/AgentSettingsFields.tsx:404-412 | PARTIAL: one variant (ui/agents/editor.rs:817-821) | — | S |
| Create pipeline: create, start, then "Finishing…" profile publication. Failures go to "Checking…" (refresh and classify) with: "We couldn't confirm whether the agent was created…"; "…was saved, but couldn't start… select Start agent"; "…started, but profile setup didn't finish… Select Finish profile"; "Finish profile" button | BA/AgentCreateDialog.tsx:180-312, 474-583 | PARTIAL: create then start, with raw errors. The profile is published by the host in the background (host/lib.rs:286-298, 333). No Finish profile or Checking phases (ui/agents/editor.rs:483-574) | Phase machine and copy | M |
| Status lines: "{name} was saved. Finishing its profile…", "{name} was saved. Start it to finish setup.", "Connect to a community and use a rebuilt desktop app to create an agent.", runtime-unavailable alert | BA/AgentCreateDialog.tsx:525-545 | PARTIAL | — | S |
| Extra footer "Retry status" when control is in error | BA/AgentCreateDialog.tsx:584-590 | MISSING | — | S |
| Backdrop and Escape close only when not dirty and not busy | BA/AgentCreateDialog.tsx:325-330 | DIFFERENT: closes whenever not busy and drops edits (ui/agents/editor.rs:964-968) | Dirty guard | S |

## 5. Model picker (`BA/AgentModelPicker.tsx`, `FA/models.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Buzz Agent / Databricks catalog: "Browse models" signs in through the browser ("Loading models… sign in through your browser if asked.") | BA/AgentModelPicker.tsx:186-215; ST/agent_models.rs:500-630 | MISSING: the host refuses ("…needs a sign-in this host cannot run yet. Enter a model ID.", host/lib.rs:375-381); no browse for Buzz Agent (ui/agents/form.rs:725). The Databricks fields and Disconnect are IN PROGRESS, but browser sign-in is not on that list | Host-side PKCE OAuth and catalog | L |
| Test connection button and result ("Sending a short test message…", "Connected. The model replied.", "Connected using {model}.", failure icon) | BA/AgentModelPicker.tsx:140-160, 332-353 | IN PROGRESS | — | M |
| Advanced → Model accordion: "Model ID (custom or blank)" field | BA/AgentModelPicker.tsx:575-600 | MISSING: a single model field (ui/agents/form.rs:717-781) | — | S |
| "Databricks workspace (HTTPS origin)" and "Model filter (optional)" with inherited placeholders; "Editing either field saves both displayed values." | BA/AgentModelPicker.tsx:612-646 | IN PROGRESS | — | M |
| "Disconnect" and "Credentials are shared within Foundation for this workspace… Disconnect removes this app's cache…" | BA/AgentModelPicker.tsx:647-668 | IN PROGRESS | — | S |
| "Refresh models" (Pi, Databricks) | BA/AgentModelPicker.tsx:602-610, 649-654 | PARTIAL: only "Retry models" inside the menu after a failure (ui/agents/form.rs:798-804) | — | S |
| "Cancel model lookup" / "Cancel sign-in", then "Cancelled. Retry when ready."; one lookup at a time | BA/AgentModelPicker.tsx:509-521; FA/models.ts:30-120 | MISSING: a newer lookup replaces the older one, with no cancel or timeout (ui/agents/form.rs:955-990) | Cancel, 185s timeout | S |
| Empty-catalog statuses: Goose "No models found for this Goose provider…"; Pi "No signed-in Pi providers found…"; Databricks "No models found…"; Pi "No Pi models for this provider…" | BA/AgentModelPicker.tsx:230-240, 281, 543-545 | PARTIAL: only "Type a model ID to use a custom model." (ui/agents/form.rs:820-825) | — | S |
| Warnings: model not in Goose's list; not in Pi's catalog; "Choose a model for this provider before starting, or clear Provider…"; `GOOSE_MODEL` / `BUZZ_AGENT_MODEL` override wording; Goose info paragraph | BA/AgentModelPicker.tsx:525-572 | PARTIAL: one generic override note (ui/agents/form.rs:759-764) | — | S |
| Goose list cap note "Showing up to N models. Type to search all {M}." | BA/AgentModelPicker.tsx:488-493 | PARTIAL: capped at 200 with no note (ui/agents/form.rs:818) | — | S |
| Pi catalog loads as soon as Pi is selected; models are filtered to the chosen provider | BA/AgentModelPicker.tsx:150-200 | PARTIAL: loads when the Provider menu opens, with no provider filter (ui/agents/form.rs:145-152, 806-819) | — | S |
| Combobox: Enter commits the highlighted item; typed text marks the dialog dirty; items show "{id}" and " · Custom ID" | BA/AgentModelPicker.tsx:355-480 | PARTIAL: dropdown menu plus text field, no Custom ID item (ui/agents/form.rs:826-846) | — | S |

## 6. Edit agent dialog (`BA/AgentEditor.tsx`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Avatar editor (upload and change picture, squircle) with picture validation | BA/AgentEditor.tsx:127-131, 185-200 | MISSING: static avatar (ui/agents/editor.rs:1055-1060) | Avatar upload into `AgentEdit.picture` | M |
| Runtime accordion buttons: Start / "Retry start", Stop, Restart / "Restart to apply" | BA/AgentEditor.tsx:243-270 | MISSING: the Runtime block has no buttons (ui/agents/editor.rs:1080-1100) | Add buttons | S |
| Runtime mode line variants: "Imported · close the editor and choose Use here before starting", "Enabled intent saved · execution unavailable", "Enabled · starts with buzz-app", "Enabled · manual-start only", "Start on launch enabled", "Manual start · a later sent mention can start this agent" | BA/AgentEditor.tsx:229-242 | PARTIAL: two variants. "A later sent mention can start" is promised but mention wake is missing (ui/agents/editor.rs:1075-1079) | — | S |
| Revision note: "Running revision none", "Saved changes are not running yet. Stop ends current work…" | BA/AgentEditor.tsx:271-277 | PARTIAL (ui/agents/editor.rs:1091-1099) | — | S |
| Runtime and Technical details as collapsed accordions | BA/AgentEditor.tsx:216-312 | DIFFERENT: always-open sections | — | S |
| "Public key (npub)", npub-encoded and select-all | BA/AgentEditor.tsx:287-294 | DIFFERENT: shows hex (ui/agents/editor.rs:1114) | npub | S |
| Save restarts running agents whose effective settings changed; toast "Saved." / "Saved. Restarted N agents." / restart-failure notice kept in the dialog | BA/AgentEditor.tsx:160-171; FA/control.ts:162-169; ST/agents.rs:970-1035 | IN PROGRESS | — | M |
| After save, profile publication with "Settings saved. Publishing profile update…" / "Settings saved; profile publication is unconfirmed…" | BA/AgentEditor.tsx:148-160 | PARTIAL: publishes silently and closes (ui/agents/editor.rs:600-610) | Notices | S |
| "Retry profile publication" button and "Profile published. Running work was not restarted." | BA/AgentEditor.tsx:320-345 | PARTIAL: only in the card menu (ui/agents.rs:960-968) | — | S |
| `agent.error` and `state.error` alerts; "Retry status" | BA/AgentEditor.tsx:308-318, 347-351 | MISSING | — | S |
| Stale copy: "The host has a newer saved revision. Your edits are still here; copy anything you need, then discard to load the latest settings." | BA/AgentEditor.tsx:352-358 | DIFFERENT: different wording (ui/agents/editor.rs:1129-1135) | Copy | S |
| Close blocked while dirty or uploading | BA/AgentEditor.tsx:98-101 | DIFFERENT: closes when not busy, dropping edits (ui/agents/editor.rs:964-968) | Dirty guard | S |

## 7. Delete dialog (`BA/AgentDeleteDialog.tsx`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Retry status" button inside the dialog when control is in error; fallback "Could not confirm deletion." | BA/AgentDeleteDialog.tsx:30-36, 70-75 | MISSING | — | S |

## 8. Import from old Buzz (`BA/AgentImport.tsx` vs `ui/agents/import.rs`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Entry point: an "Import from another installation" accordion on the Agents page, or a modal opened from an "Available to import" row | BA/AgentImport.tsx:281-287; BA/AgentControlPanel.tsx:375-431 | DIFFERENT: "Import from Buzz" in the Add dialog sidebar with a detail pane (ui/agents/editor.rs:930-941; ui/agents/import.rs:206-245) | Keep, or add the page entry | S |
| Intro copy (copies key and settings, leaves the agent stopped, explains repair, says to stop the old agent and turn off its auto-start) | BA/AgentImport.tsx:288-295 | DIFFERENT: shorter (ui/agents/import.rs:353-359) | Add the "disable automatic startup there" advice | S |
| Browsing without a destination ("browsing does not need a connection") | BA/AgentImport.tsx:299-304 | DIFFERENT: needs a community and a connected host (ui/agents/import.rs:89-100) | Preview without a destination | M |
| "Community: {destination}" line | BA/AgentImport.tsx:296-298 | MISSING | — | S |
| "Import options" section, open by default when there is no destination | BA/AgentImport.tsx:400-459 | MISSING: the Source select is always visible (ui/agents/import.rs:286-333) | — | S |
| Destination community text field | BA/AgentImport.tsx:432-443 | MISSING | — | S |
| "Load agents" button | BA/AgentImport.tsx:444-449 | MISSING: loads automatically | — | S |
| Source path and warnings, deduplicated | BA/AgentImport.tsx:451-458 | PARTIAL: not deduplicated; shown only with no selection (ui/agents/import.rs:441-449) | — | S |
| Loading copy "Loading agents from old Buzz…" | BA/AgentImport.tsx:314 | DIFFERENT: "Loading agents from Buzz…" (ui/agents/import.rs:383) | — | S |
| Load error "Could not load agents from old Buzz. Check Import options and try again." | BA/AgentImport.tsx:75-81, 315-325 | DIFFERENT: raw host error (ui/agents/import.rs:119-125) | — | S |
| Refresh agent status before each preview | BA/AgentImport.tsx:69-72 | MISSING | — | S |
| Candidates whose key is already managed are hidden | BA/AgentImport.tsx:94-105 | DIFFERENT: listed with a check and "Already on this host" (ui/agents/import.rs:230-232, 263-264) | — | S |
| Empty-state variants ("No agents left to import or repair…", "No agents in this local library.", single-agent unavailable) | BA/AgentImport.tsx:326-334 | PARTIAL: one variant (ui/agents/import.rs:432-433) | — | S |
| Per-candidate status "Not imported" / "Team instructions not imported" | BA/AgentImport.tsx:346-348 | DIFFERENT: summary count line (ui/agents/import.rs:435-439) | — | S |
| Pubkey under a collapsed "Identity" disclosure | BA/AgentImport.tsx:354-359 | DIFFERENT: always shown (ui/agents/import.rs:406) | — | S |
| Per-row Import button ("Import {name}") | BA/AgentImport.tsx:383-396 | DIFFERENT: select, then the footer "Import agent" (ui/agents/import.rs:258-281) | — | S |
| "Repair team import" for `needsTeamImport` agents, with its status copy; page accordion "Repair team import from another installation" | BA/AgentImport.tsx:101-118, 308-313, 336-338, 392-395; BA/AgentControlPanel.tsx:375-394 | MISSING: `needs_team_import` exists but is unused (host/api.rs:176) | Repair mode in host and UI | M |
| "Clone to this community" (create form seeded from the record, new key); "Could not read clone settings…" | BA/AgentImport.tsx:361-382 | MISSING | `CloneSettings` request | M |
| After import, close and hand the imported agent to the page (focus the card) | BA/AgentImport.tsx:120 | DIFFERENT: stays open and reloads, "{name} was imported…" (ui/agents/import.rs:179-189) | — | S |
| Failed commit: "Import didn't finish. Reload the source before trying again." | BA/AgentImport.tsx:121-130 | DIFFERENT: raw error and auto-reload (ui/agents/import.rs:190-197) | — | S |
| "Import is unavailable in this app session." | BA/AgentImport.tsx:223-225, 305-307 | MISSING | Only if commit can be missing | S |
| Single-agent mode: "Import {name}?" dialog with Source/Destination, explanation, "Importing…" | BA/AgentImport.tsx:135-280 | MISSING | — | M |
| Single-agent: "Use destination" (personal space); "Choose another source"; "Reload source"; "Loading agent…" | BA/AgentImport.tsx:149-160, 177-255 | MISSING | — | S |

## 9. Use here and Clone (`BA/LocalInventoryAction.tsx`, `BA/AgentControlPanel.tsx`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Set up agent here" dialog: "Set up the imported agent in this community. It will not start yet.", "Destination: …", "Use here" (resolves the community, finishes the import) | BA/AgentControlPanel.tsx:432-481; BA/LocalInventoryAction.tsx:40-80; ST/agents.rs:1354-1365 | MISSING | Host `UseHere` request and dialog | M |
| "Review agent to clone" dialog with "Review clone", which opens Add agent "Clone agent" with the clone notice | BA/AgentControlPanel.tsx:443-449; BA/LocalInventoryAction.tsx:52-53, 81-93; ST/agents.rs:1366-1386 | MISSING | Host clone-settings request | M |
| Hints and errors: "Connect to the destination community to use this identity there.", "Use here is unavailable.", "Cloning this agent is unavailable in this app version.", "Could not complete this action. Refresh and try again.", "This app cannot set up this agent yet. Import it first…" | BA/LocalInventoryAction.tsx:55-101; BA/AgentControlPanel.tsx:472-476 | MISSING | — | S |

## 10. Agent snapshots: export, import, chat preview, direct share (`BA/AgentSnapshots.tsx`, `BA/SnapshotPreview.tsx`, `BA/DirectShare.tsx`, `FA/snapshot*.ts`, `FA/SnapshotShareDialog.tsx`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Export {name}" dialog (Cancel / Export; cannot close while exporting) | BA/AgentSnapshots.tsx:216-243 | MISSING | Dialog and menu entry | M |
| Memories select: Agent only / + core memory / + all memories | BA/AgentSnapshots.tsx:245-265 | MISSING | Memory read | M |
| File format select: PNG / JSON | BA/AgentSnapshots.tsx:266-279 | MISSING | PNG tEXt encoder | M |
| Disclosure about portable config and secrets; "Review portable configuration" JSON preview; artwork source | BA/AgentSnapshots.tsx:280-295 | MISSING | — | S |
| Required "I reviewed the portable configuration…" checkbox, reset when the agent changes | BA/AgentSnapshots.tsx:98-105, 296-307 | MISSING | — | S |
| Memory plaintext warning and confirmation checkbox | BA/AgentSnapshots.tsx:308-336 | MISSING | — | S |
| Eligibility rules (buzz-agent only, owner-only, no env or backend) and credential-looking text blocked | FA/snapshot.ts:160-164, 392-452 | MISSING | — | S |
| PNG embeds the avatar as 512px artwork, else a placeholder; downloads `{name}.agent.{png,json}`; "Agent configuration changed. Review it again before exporting." | BA/AgentSnapshots.tsx:24-33, 163-207 | MISSING | Image resize, save dialog | M |
| "Import agent snapshot" dialog with "Choose file" (.json, .png, legacy) | BA/AgentSnapshots.tsx:556-662 | MISSING | File picker and parser | M |
| Legacy-file error (.persona.*, .zip); size limits; parse errors | FA/snapshot.ts:7-25, 187-218, 759-845 | MISSING | — | S |
| Preview: name, instructions, "No system prompt included.", fresh-keypair note, runtime/response/parallelism/session summary, "Full embedded manifest" | BA/AgentSnapshots.tsx:663-742 | MISSING | — | S |
| Memory restore option ("Restore memory under the new agent identity"), unavailable fallback, "No memory included — config only." | BA/AgentSnapshots.tsx:680-714; ST/agents.rs:1595-1612 | MISSING | Host memory write | M |
| "Import is blocked: …" list; runtime mapped to an installed harness ("{runtime} is unavailable. Install it in Settings first.") | BA/AgentSnapshots.tsx:715-720; FA/snapshot.ts:502-603 | MISSING | — | S |
| Upload embedded artwork; "Creating agent…"; duplicate-identity warning on failure | BA/AgentSnapshots.tsx:507-551, 745 | MISSING | — | S |
| "Agent imported" result with "Publish profile"; partial memory error and "Retry memory restore" | BA/AgentSnapshots.tsx:438-488, 536-539, 567-636 | MISSING | — | S |
| Chat attachment card for snapshot files with "Add agent" / "Add team" | src/features/messages/SnapshotAttachment.tsx:11-50; FA/snapshot-preview.ts:15-29 | MISSING | — | M |
| Link context menu "Preview snapshot"; "Preview snapshot" dialog (loading, error + Retry, Cancel); authenticated read with SHA-256 check; agent vs team detection | src/features/conversation/MessageLink.tsx:276-289; BA/SnapshotPreview.tsx:116-197; FA/snapshot-preview.ts:66-159 | MISSING | — | M |
| Pasting a copied snapshot link attaches the file | FA/snapshot-clipboard.ts:17-67 | MISSING | — | M |
| "Share {name}" dialog: review block, recipient picker, Send / "Sending…" / "Retry send", toast "Sent a copy of {name}" | FA/SnapshotShareDialog.tsx:192-383; BA/DirectShare.tsx:69-218; FA/snapshot-send.ts:24-103 | MISSING | — | M |
| Pending sends survive restart ("Retry the earlier message to confirm its delivery before editing.") | FA/SnapshotShareDialog.tsx:121-162, 385-389; FA/snapshot-recovery.ts | MISSING | — | M |
| "What's included" select and plaintext warning; "Share memories?" confirmation | FA/SnapshotShareDialog.tsx:390-427, 457-478 | MISSING | — | S |
| "Copy link" (upload, copy URL + HTML; "Copying…" / "Copied") | FA/SnapshotShareDialog.tsx:428-439; FA/snapshot-link.ts | MISSING | Rich clipboard | M |
| Export switch inside Share; "Share to catalog" switch; error copy | FA/SnapshotShareDialog.tsx:145-307, 348-350; BA/DirectShare.tsx:164-227 | MISSING | — | S |

## 11. Community catalog (`BA/CommunityCatalog.tsx`, `FA/catalog*.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Catalog reader (kinds 30175/30178, paged, newest version per entry, 5,000-event cap) | FA/catalog.ts:70-304 | MISSING | Relay-crate reader | L |
| "Share to catalog" switch with agent and team copy | BA/CommunityCatalog.tsx:46-49, 79-163 | MISSING | — | M |
| Share notices (queued, published, no longer discoverable, stalled + Retry / Dismiss); "Failed to update catalog sharing." | BA/CommunityCatalog.tsx:61-72, 164-236 | MISSING | — | M |
| Publish and unshare rules; agent and team validation (limits, invisible characters, unsupported runtime, allowlist downgraded) | FA/catalog.ts:387-446; FA/catalog-protocol.ts:152-300 | MISSING | — | M |
| "Community Catalog" browser dialog (Agents/Teams sidebar, loading/error/"Nothing shared yet") | BA/CommunityCatalog.tsx:495-643 | MISSING | — | M |
| Agent detail (avatar, "Added by", metadata grid, instructions); team detail (members, expandable rows) | BA/CommunityCatalog.tsx:652-711, 811-892 | MISSING | — | S |
| "Add agent" pre-fills the create form; "Added to My Agents" remembered per community; "Add team" / "Added to my teams" | BA/CommunityCatalog.tsx:338-427, 555-590; FA/catalog.ts:334-357 | MISSING | — | M |
| Non-stock ACP transport blocked ("…uses the X transport, which this app can't run yet…") | FA/catalog-protocol.ts:111-123; BA/CommunityCatalog.tsx:713-722 | MISSING | — | S |
| Team "Share {team}" dialog | BA/CommunityCatalog.tsx:302-336 | MISSING | Needs teams | S |

## 12. Teams (`BA/Team*Dialog.tsx`, `FA/team-*.ts`; native `ST/agents.rs:1676-1823`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Team snapshot format (size limits, `buzz_team_snapshot` chunk, checksum errors) | FA/team-encoding.ts:1-93 | MISSING (no teams model) | Channel-templates/teams model first | L |
| "Import team snapshot" dialog: members, "Restore member memories", "Respond-to allowlist" Clear/Keep, "Creating team…", "Team imported" | BA/TeamImportDialog.tsx:40-157 | MISSING | — | M |
| Resumable team import (receipt, profile publish, team record) | FA/team-bundles.ts:27-101; FA/team-import.ts:9-120 | MISSING | — | L |
| "Export {team}" dialog (memories, confirmation, JSON/PNG, `.team.{fmt}`) | BA/TeamExportDialog.tsx:11-156 | MISSING | — | M |
| "Deploy team to channel" dialog and behaviour (apply instructions, restart if stale, publish profile, add to channel, start; per-member failures) | BA/TeamDeployDialog.tsx:16-122; FA/team-deployment.ts:24-98 | MISSING | — | M |
| Team direct share, catalog switch, export | BA/DirectShare.tsx:232-314 | MISSING | — | S |
| Native team instructions, capture, export, preview | ST/agents.rs:1676-1823 | MISSING | Host requests | M |

## 13. Old agent library and discovery (`BA/AgentLibrary.tsx`, `FA/library.ts`, `relay-library.ts`, `inventory.ts`, `known.ts`, `choices.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Other agents (N)" disclosure, identity rows by name, "No visible identities in your Buzz library." | BA/AgentLibrary.tsx:41-123 | MISSING | — | M |
| "Profiles without identities" list | BA/AgentLibrary.tsx:124-144 | MISSING | — | S |
| Library states ("Reading agent inventory…", cleared, connect, error) and footer disclaimers | BA/AgentLibrary.tsx:76-96, 145-155; FA/library.ts:76-84 | MISSING | — | S |
| Relay inventory read (kinds 30175/30177, owner only, avatar inheritance) | FA/relay-library.ts:1-129; FA/library.ts:24-42 | MISSING | — | M |
| Merge of local and relay sources with partial-failure messages | FA/inventory.ts:1-88 | MISSING | — | S |
| Per-community inventory request and profile lookup across memberships | BA/use-community-inventory.ts:1-151; BA/community-profiles.ts:1-44 | MISSING | — | M |
| Known-agent keys (profile flag plus library) | FA/known.ts:1-16; FA/use-known.ts | PARTIAL: `is_agent` only (relay/src/profiles.rs:78) | Add library keys | S |
| Agent choices (managed + legacy, archived excluded, templates) | FA/choices.ts:1-252; FA/use-choices.ts | PARTIAL: no legacy library, no archive filter | — | M |
| Public agent metadata (type and capabilities from 30177/10100) | FA/public-metadata.ts:1-95 | PARTIAL: only `is_agent` (relay/src/profiles.rs:56-78) | — | S |

## 14. Identity archive and relay-only removal (`FA/AgentArchiveDialog.tsx`, `BA/inventory-archive.ts`, `BA/RelayAgentRemove.tsx`, `FA/relay-removal.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Archive {name}?" dialog (hides from search, mentions, Add member; "This agent is running. Archiving won't stop it.") | FA/AgentArchiveDialog.tsx:1-40 | MISSING | Identity-archive relay support | M |
| Archive/Unarchive request per identity; toast "{name} archived in {community}" with Undo; Undo notices retired when the view or community changes | BA/inventory-archive.ts:11-91; BA/UnifiedInventory.tsx:150-190 | MISSING | — | M |
| Relay-only "Remove" button and "Remove {name}?" dialog with three bullets | BA/RelayAgentRemove.tsx:39-85 | MISSING | — | S |
| Removal: archive, delete 30177 (NIP-09), best-effort channel removal; "Record deletion is not confirmed. Retry." | FA/relay-removal.ts:14-213 | MISSING | — | M |

## 15. Agent-requested updates (`BA/AgentUpdateReview.tsx`, `FA/management-request.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Listen for `agent_management_request` (create/update), queue capped at 200 | FA/management-request.ts:1-105; BA/AgentUpdateReview.tsx:74-88, 323-332 | MISSING | Relay subscription | M |
| Membership check; toast "Could not verify request membership" + Retry | BA/AgentUpdateReview.tsx:89-137, 189-201, 334-344 | MISSING | — | S |
| Toasts "Agent creation needs attention… Create the agent yourself", "Could not load personal agents", "Agent update needs attention" | BA/AgentUpdateReview.tsx:203-253 | MISSING | — | S |
| Editor pre-filled with the requested changes: "Requested by an agent. Review every field before saving."; writes paused until membership is confirmed | BA/AgentUpdateReview.tsx:254-285, 346-376 | MISSING | Reuse the editor with an initial draft | M |

## 16. Mention wake (`FA/mention-wake.ts`, `FA/AgentWakeNotice.tsx`, `FA/control.ts:840-914`, `ST/agents.rs:559-586, 1103-1126`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Sending a message that p-tags your own stopped agent starts it with a replay floor; a waiting or starting agent gets the mention attached; Stop cancels the wake | FA/mention-wake.ts:1-88; FA/control.ts:840-914; ST/agents.rs:559-586, 1103-1126, 1150-1240 | MISSING, though the port's copy promises it (ui/agents/editor.rs:1078) | Outbox hook, host start-with-replay and attach-mention | M |
| "Agent could not start" toast ("Message sent, but X could not start…" plus variants; "Dismiss agent notice") | FA/AgentWakeNotice.tsx:1-19; FA/control.ts:848-912 | MISSING | — | S |

## 17. Agents2, attention, activity, thinking badge

### 17-A. Agents2 page (`src/bundled/agents2/*`, `src/features/agents2/*`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Agents2" primary page (`buzz.agents2`) | src/bundled/agents2/index.tsx:1-14; src/bundled/index.ts:175-178 | MISSING | Page, nav, and Agents2 service with native key custody | L |
| Header: "Agents2" or the agent's name; back "All agents"; "New agent" | src/bundled/agents2/Agents2Page.tsx:169-199 | MISSING | — | S |
| Unavailable, error ("Agents could not load") and empty ("No agents yet", two variants) states | Agents2Page.tsx:88-103, 143-161 | MISSING | — | S |
| Tile grid (10rem): avatar, name, type summary or "Type unavailable"; dimmed when the type is missing | Agents2Page.tsx:52-60, 263-290 | MISSING | — | M |
| Listbox keyboard model (arrows by column, Home/End, Enter opens Build, Escape deselects); click selects, double-click opens | Agents2Page.tsx:202-276 | MISSING | — | M |
| Side panel (22rem, animated, reduced motion respected) | Agents2Page.module.css:2-27, 156-181 | MISSING | — | M |
| Peek: avatar, "type · summary", Close, "Open", type Peek slot, "Enable the plugin…" note, "Wakes on" summary | Agents2Page.tsx:318-370; src/features/agents2/AttentionPanel.tsx:73-101 | MISSING | — | M |
| New agent form: Name, Type radio cards, "Create and set up", error copy | Agents2Page.tsx:372-458; src/features/agents2/service.ts:339-363 | MISSING | — | M |
| Build view: identity rail (avatar, inline rename, Type, Key + copy "Copied"), "Delete agent", tab host (type tabs then "Attention") | Agents2Page.tsx:460-574; src/bundled/agents2/AgentTabs.tsx; src/features/agents2/tabs.tsx | MISSING | — | M |
| Delete dialog "Delete {name}?" and background cleanup (leave channels, archive, forget; retried on reconnect) | src/features/agents2/AgentManage.tsx:13-75; service.ts:493-597 | MISSING | — | M |
| Agents2 tabs inside the profile panel and "Delete agent" there | src/bundled/profiles/ProfilePanel.tsx:181-190, 425-492 | MISSING | — | M |

### 17-B. Attention editor (`src/features/agents2/AttentionPanel.tsx`, `attention.ts`, `store.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Intro and the always-on "Mentions and replies" row | AttentionPanel.tsx:152-164 | MISSING | — | S |
| Interest groups: title from slug, instructions, menu Edit/Remove (Remove guarded while used), inline edit, empty copy, "Add watch" / "Add timer" | AttentionPanel.tsx:230-384, 557-583 | MISSING | — | M |
| Watch rows: icon, title, subtitle, on/off switch, "Edit JSON", Remove | AttentionPanel.tsx:416-515 | MISSING | — | M |
| Timer rows: next run, ran X of N, until, "Ended", "Done…", "Run again" | AttentionPanel.tsx:386-414, 477-499; attention.ts:388-423 | MISSING | — | M |
| "Without an interest" and "Skipped" sections | AttentionPanel.tsx:180-212, 517-555; store.ts:43-103 | MISSING | — | M |
| JSON editor with full spec validation | AttentionPanel.tsx:586-633; attention.ts:68-243 | MISSING | — | M |
| New interest, watch (Wake on, In, channels, text, replies, advanced filter grammar) and timer (Prompt, Every, Stop after, Until) forms | AttentionPanel.tsx:636-977; attention.ts:245-339 | MISSING | — | M |
| Limits (100 interests / 100 watches+timers) and persistence `buzz.agents2.v1` | store.ts:38-173 | MISSING | — | S |

### 17-C. Agents2 runtime (`src/features/agents2/service.ts`, `native.ts`; `ST/app_agents.rs`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Agent-type registry (title, description, defaults, summary, Peek, tabs, run, timeout ≤30 min) | service.ts:135-154, 308-327 | MISSING | Extension mechanism | L |
| Native custody: `app_agent_list` | ST/app_agents.rs:149-156 | MISSING | — | S |
| Native custody: `app_agent_create` (generate key, owner attests, save atomically) | ST/app_agents.rs:157-185 | MISSING | — | M |
| Native custody: `app_agent_rename` | ST/app_agents.rs:186-195 | MISSING | — | S |
| Native custody: `app_agent_delete` / `app_agent_forget` | ST/app_agents.rs:196-222 | MISSING | — | S |
| Native custody: `app_agent_publish` (bounded agent-signed event) | ST/app_agents.rs:223-235 | MISSING | — | S |
| Native custody: `app_agent_publish_profile` | ST/app_agents.rs:236-267 | MISSING | — | S |
| Native custody: `app_agent_query` (agent-authenticated relay read) | ST/app_agents.rs:268-313 | MISSING | — | S |
| Native custody: `app_agent_upload` (agent-signed media upload) | ST/app_agents.rs:314-361 | MISSING | — | S |
| Native custody: `app_agent_remember` (memory write) | ST/app_agents.rs:362-400 | MISSING | — | S |
| Scoped to the selected community and owner; reload after changes | service.ts:442-488, 603-655 | MISSING | — | M |
| Wake on mentions, DMs and replies; event watches; timers; never hears itself | attention.ts:341-386; service.ts:680-800 | MISSING | — | M |
| Run queue (one per agent, queue 32, 60/min, deadline), same-owner loop guard, profile sync retried | service.ts:179-188, 490-597, 722-880 | MISSING | — | M |

### 17-D/E/F. Agent Activity panel, transcript and data layer (`src/bundled/agent-activity/*`, `FA/activity*.ts`, `FA/observer.ts`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Observer telemetry (owner-encrypted kind 24200, decrypted, leased) | FA/activity.ts:80-139, 451-533; FA/observer.ts:1-58 | MISSING (no 24200 in the port) | Prerequisite for everything below | L |
| "Agent Activity" panel and `buzz:agent-activity?…` targets; plugin lease | src/bundled/agent-activity/index.tsx:1-33; FA/activity-target.ts:1-53 | MISSING | — | M |
| Panel states: connect, host cannot decode, "Retry live feed", empty "No captured activity yet…" | src/bundled/agent-activity/ActivityPanel.tsx:37-42, 389-410 | MISSING | — | S |
| Agent select and conversation select (channel, thread with first prompt line) | ActivityPanel.tsx:64-177, 413-464 | MISSING | — | M |
| Status line ("N observed working turn(s)." etc.), Transcript/Raw tabs, notices, diagnostics | ActivityPanel.tsx:187-285, 465-483 | MISSING | — | S |
| Saved-history paging ("Show latest saved activity", "Load older activity") | ActivityPanel.tsx:204-221, 277-283, 355-369; FA/activity.ts:393-450 | MISSING | Archive host | M |
| Raw view accordion with event JSON and limits | ActivityPanel.tsx:286-341; FA/activity-records.ts:16-51 | MISSING | — | M |
| Transcript: turn header (config, status, facts), "The start of this turn isn't loaded.", error and "Working…" rows | src/bundled/agent-activity/ActivityTranscript.tsx:56-169; FA/activity-transcript.ts:180-195 | MISSING | — | M |
| Transcript items: system prompt sections, prompts (steer), thoughts, Markdown agent reply, tool calls (kind icon, permission badge, diffs, output, elision), plan checklist, status items, chunk merging | ActivityTranscript.tsx:174-379; FA/activity-transcript.ts:92-514 | MISSING (markdown renderer reusable: app/src/ui/markdown.rs) | — | M |
| Turn state machine (working ≤30s, ended, unknown); bounded live window; access filtering; working-channel set | FA/activity.ts:15-321, 380-389, 503-604 | MISSING | — | M |
| Typing as working evidence (kind 9/20002 from agents, 8s) | FA/activity.ts:619-698 | PARTIAL: relay/src/typing.rs tracks 20002 pulses but does not link them to agents | Join with known agents | S |

### 17-G/H. Current-activity indicators and thinking badge

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Live activity labels ("Working", "Waiting for permission", "Reading {file}", "Running N tools"…) | FA/current-activity.ts:6-139 | MISSING | — | S |
| Shared `AgentActivityRow` (avatar, name, duration, scope, "View activity") | FA/AgentActivityRow.tsx:9-83 | MISSING | — | S |
| Composer indicator separating "X is working" from "X is typing", with avatar stack | src/features/messages/TypingIndicator.tsx:30-228 | PARTIAL: everyone is "is typing", channels only (app/src/ui/typing.rs:59) | — | M |
| Working-agents hover popover from the composer indicator (scopes, open thread, View activity, keyboard) | TypingIndicator.tsx:110-165, 230-340 | MISSING | — | M |
| Sidebar channel "working" badge | src/bundled/channels/ChannelSidebarItem.tsx:83-119, 240-252 | MISSING | — | M |
| Channel activity popover "Working now" | src/bundled/channels/ChannelActivityPopover.tsx:40-46, 197-330 | MISSING | — | M |
| Profile "Latest activity" preview | src/bundled/profiles/ProfileActivity.tsx:11-92 | MISSING | — | S |
| Thinking badge on squircle agent avatars (message rows, profile, Agents card), morph animation, a11y ", thinking" / ", available" | FA/AgentAvatar.tsx:20-101; src/shared/design-system/ui/agent-thinking/ThinkingBadge.tsx:7-157 | MISSING (presence dot only: ui/src/components/avatar.rs:46, 101) | — | M |

## 18. Sessions / Me (`src/features/sessions/*` = FS, `src/bundled/sessions/*` = BS)

The port's header has a "Me" tab with no handler (app/src/ui/workspace.rs:385). The port has no `Page::Me` (app/src/ui/sidebar.rs:13-18).

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Legacy "Sessions (now in Me)" page redirects to Me ("Opening Me…") | BS/index.tsx:6-42 | MISSING | Route alias | S |
| Me workspace: "Session history" aside with sections nav, session column | BS/SessionsWorkspace.tsx:40-99; BS/SessionsPage.tsx:73-255 | MISSING | Me page | L |
| Connection states: "Connecting to your community…", "Connect to a community to work with your agents.", error | BS/SessionsPage.tsx:100-120 | MISSING | — | S |
| "Loading Me placement…", "Loading your conversation…", "This conversation is in Messages." | BS/SessionsPage.tsx:210-220, 450-462 | MISSING | — | S |
| Section list: "Conversations" / "Sessions" default section, "New conversation" / "New session", "Sections couldn't load." | BS/SessionSections.tsx:140-200 | MISSING | — | M |
| Section menu: "Session settings…", "Delete section" (+ error) | BS/SessionSections.tsx:215-250 | MISSING | — | S |
| "New session in {section}" button | BS/SessionSections.tsx:280-290 | MISSING | — | S |
| Session row with actions menu (button, right-click, ContextMenu key) | BS/SessionSections.tsx:295-300, 600-650 | MISSING | — | S |
| Session menu: "Share…", "Rename", Move to / Section submenu (+ "New group…" / "New section…"), Copy submenu (name, ID, link) and copy notice, "Session settings…" | BS/SessionSections.tsx:300-400, 474 | MISSING | — | M |
| "No sessions in this section." | BS/SessionSections.tsx:406 | MISSING | — | S |
| Move pending: "Saving section…", "Retry move", "Dismiss" | BS/SessionSections.tsx:445-470 | MISSING | — | S |
| "Create new section" dialog with "Section name" | BS/SessionSections.tsx:530-570 | MISSING | — | S |
| Session heading (channel name, Archived badge) and New session view titles | FS/SessionPresentation.tsx:10-95 | MISSING | — | S |
| Session actions menu: "Rename", "Session settings…" | BS/SessionsPage.tsx:383-416 | MISSING | — | S |
| Tab pane toggle "Open tab pane" / "Close tab pane" | BS/SessionsPage.tsx:417-425 | MISSING | — | M |
| Session agent control in the header | FS/SessionAgentControl.tsx:7-58 | MISSING | — | S |
| AgentChoice picker: "Search agents", arrow/Enter keys, presence and admission labels, "No matching agents.", "Loading agents…", "No agents available in this community.", "Change agent: {name}" | FS/AgentChoice.tsx:26-388 | MISSING | — | M |
| New-session composer "Message your agents" / "Message this session"; draft kept when sending is unavailable; actions menu; start errors | FS/NewSessionComposer.tsx:31-477 | MISSING | — | L |
| Pending session start survives restart ("Your session start couldn't be saved on this device…") | FS/pending-start.ts; FS/NewSessionComposer.tsx:90-140 | MISSING | — | M |
| Recipient rules: "Session participants are still loading…", "There are multiple agents in this session. @mention who should respond." | FS/recipients.ts:30-42 | MISSING | — | S |
| Personal ("Me") vs shared classification and metadata | FS/personal.ts; FS/metadata.ts | MISSING | — | S |
| "Rename session" dialog (1–120 chars, permission notice) | FS/RenameSession.tsx:19-151 | MISSING | — | S |
| "Session settings" (section defaults): "Start from a template" / "Custom settings", "Full Canvas", stale-draft notice, "Discard draft and reload saved settings", "Retry settings", "Loading settings…" | FS/WorkspaceSettings.tsx:29-293 | MISSING | — | M |
| Workspace fields: Project folders (≤20), "Work in a new worktree", Source repository + Browse, Worktree location, Base branch, Canvas | FS/WorkspaceFields.tsx:11-167 | MISSING | — | M |
| Draft session settings dialog (Canvas before start) | FS/DraftSessionSettings.tsx:16-120 | MISSING | — | S |
| Workspace validation and section-default errors (24 KiB Canvas, invalid metadata, section removed…) | FS/workspace.ts:20-310 | MISSING | — | S |
| Open a session at a selected message ("Selected message", loading, "could not be opened") | FS/SessionMessageTarget.tsx:36-214 | MISSING | — | S |
| Share dialog "Share session" / "Share conversation": "Copy link", "Share to" (+ "Create new channel…"), Private channel, people search, "Who has session access" (Everyone / Selected people), Private vs public with "Make public and share", progress steps, errors | BS/SessionShare.tsx:57-1143; FS/share.ts:30-308 | MISSING | — | L |
| Share-attempt recovery ("This share is already in progress in another view.") | FS/share-attempt.ts; BS/SessionShare.tsx:278-283 | MISSING | — | S |
| Session link targets | BS/SessionsPage.tsx:568-578 | MISSING | — | S |

## 19. Workflows (`src/bundled/workflows/*` = BW, `src/features/workflows/*` = FW)

The port renders workflow-authored messages only (app/src/notifications.rs:180-190; app/src/ui/message.rs:54).

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Workflows" primary page and nav entry | BW/index.tsx:3-13 | MISSING | Page::Workflows | M |
| Relay workflow protocol, queries, HTTP trigger, types, capability probe | FW/protocol.ts; FW/queries.ts; FW/http.ts; FW/types.ts; FW/host.ts; FW/capability.ts | MISSING | Relay-crate support | L |
| Page states: "Connecting to your community…", "Connect to a community to browse workflows.", "Workflow operations are unavailable from this host." | BW/WorkflowsPage.tsx:40-50, 95-105 | MISSING | — | S |
| Channel filter ("All workflows", Community channels), "Reading channels…", errors, partial-list note, "Refresh workflows" | BW/WorkflowsPage.tsx:155-210 | MISSING | — | S |
| Landing cards with trigger presentation (message, reaction + emoji, diff, schedule summary, webhook) | BW/WorkflowLanding.tsx:440-620; BW/workflowCardPresentation.ts | MISSING | — | M |
| Card enable switch with "Turn on this workflow?" and frequency warnings ("This workflow may run often") | BW/WorkflowLanding.tsx:520-535, 640-655; BW/workflowActivationWarning.ts | MISSING | — | M |
| Card menu: "Edit workflow" / "View workflow", "Run now", Delete; read-only reasons | BW/WorkflowLanding.tsx:440-600 | MISSING | — | S |
| Delete confirmation, "Deleting…", "Deletion unconfirmed" | BW/WorkflowLanding.tsx:600-640 | MISSING | — | S |
| Landing status ("Reading workflows…", discovery paused, "Workflows loaded.") and "New workflow" | BW/WorkflowLanding.tsx:785-830 | MISSING | — | S |
| Editor dialog: "Create/View/Edit workflow", "Close editor", actions menu, Form/YAML mode switch and switch-blocked message | BW/WorkflowEditor.tsx:110-240 | MISSING | — | L |
| Inline workflow name edit (Enter/Escape) | BW/WorkflowEditor.tsx:280-335 | MISSING | — | S |
| "Enabled in configuration" switch and "Save this workflow enabled?" | BW/WorkflowEditor.tsx:360-370, 470-482 | MISSING | — | S |
| YAML editor ("Original text is kept until you edit… Schedules use UTC.") | BW/WorkflowEditor.tsx:400-410; BW/workflowYamlDocument.ts | MISSING | — | M |
| "Workflow settings & activity" accordion with Description | BW/WorkflowEditor.tsx:450-460 | MISSING | — | S |
| Form flow: trigger node, step nodes, "Add step" / "Add after Step N", inspector, "Remove step" | BW/WorkflowForm.tsx:140-480 | MISSING | — | L |
| Triggers: Message posted, Reaction added (Emoji), Diff posted, Schedule, Webhook | BW/WorkflowForm.tsx:50-60, 240-320 | MISSING | — | M |
| Actions: Send Message (text, destination, reply in thread), Delay, Call Webhook | BW/workflowFormTypes.ts:79-81; BW/WorkflowForm.tsx:520-650 | MISSING | — | M |
| Conditions: Basic/Advanced, operators, Add/Remove, evalexpr, "Replace with basic filters?" | BW/WorkflowConditions.tsx:20-210; BW/workflowConditionExpression.ts | MISSING | — | M |
| Schedule fields: Repeats, Repeat on, Day of month, Existing interval, warnings | BW/WorkflowScheduleFields.tsx:15-258; BW/workflowSchedule.ts | MISSING | — | M |
| Cron expression input and parser | BW/CronExpressionInput.tsx; BW/cronExpression.ts | MISSING | — | M |
| Webhook action fields: URL, HTTP method, Headers, Request body | BW/WorkflowWebhookFields.tsx:20-134 | MISSING | — | S |
| Webhook secret: "Webhook ready" dialog with reveal and copy; "Continue without this secret?" | BW/WorkflowWebhookSecretDialog.tsx; BW/WorkflowWebhookSecrets.tsx | MISSING | — | M |
| Form/YAML parse errors ("… — use the YAML editor"); header uniqueness | BW/workflowFormTypes.ts:270-525; BW/WorkflowEditor.tsx:110-120 | MISSING | — | S |
| Channel workflows view: "Saved configurations", "Refresh configurations", "New workflow", Configured enabled/disabled/Unreadable, "Read runs" / "Hide runs", "Run now", "Configuration details" | BW/WorkflowChannel.tsx:316-560 | MISSING | — | M |
| Confirmations "Leave this draft?", "Run this workflow now?", "Delete this workflow?" | BW/WorkflowChannel.tsx:555-590; BW/ConfirmAction.tsx | MISSING | — | S |
| Runs: "Runs", "Refresh runs", states, "Run details and trace", Run ID, "Newest runs" / "Older runs" | BW/WorkflowRuns.tsx:50-124 | MISSING | — | M |
| "Recent activity" operations: pending/succeeded/rejected/unknown copy, "Delivery details", "Check saved configuration", dismiss confirmation | BW/WorkflowOperations.tsx:10-171 | MISSING | — | M |
| Deletion notices ("Deleting workflow…", "Couldn't delete…", "Checking deletion…") | BW/WorkflowDeletionNotices.tsx:20-164 | MISSING | — | S |
| Channel picker combobox "Choose a channel" | BW/WorkflowChannelPicker.tsx:40-68 | MISSING | — | S |
| Duration formatting | BW/workflowDuration.ts | MISSING | — | S |

## 20. Terminal (`src/bundled/terminal/*` = BT, `ST/terminal/*`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Per-channel "Terminal" companion panel | BT/index.tsx:66-72; BT/TerminalPanel.tsx | MISSING (no companion dock in the port) | Dock and panel | L |
| Toolbar launcher "Toggle channel terminal" with shortcut in its title | BT/index.tsx:39-64 | MISSING | — | S |
| Shortcut Mod+J "Toggle channel terminal" in shortcut settings | BT/index.tsx:14-35 | MISSING | — | S |
| Header "BuzzTerm" with channel and thread labels; "Restart terminal", "End session", "Hide terminal (Cmd/Ctrl+J)" | BT/TerminalPanel.tsx:85-160 | MISSING | — | S |
| Escape stays inside the terminal | BT/TerminalPanel.tsx:195 | MISSING | — | S |
| Sessions per channel/thread kept while hidden; 20-session cap; input-full error | BT/sessions.ts:100-200 | MISSING | — | M |
| Terminal renderer, theme from tokens, JetBrains Mono, resize | BT/renderer.ts; BT/appearance.ts | MISSING | Terminal emulator widget | L |
| Welcome splash: "buzz" hex wordmark, pointer-reactive, reduced motion | BT/splash.ts; BT/banner.ts | MISSING | — | M |
| Native PTY: login shell with TERM and BUZZ_CHANNEL_ID / BUZZ_CHANNEL / BUZZ_NPUB / BUZZ_RELAY_URL / BUZZ_TERM_SESSION / BUZZ_THREAD_ID; spawn, read, write, resize, close, close-owner; context validation | ST/terminal/mod.rs:200-324; ST/terminal/unix.rs:20-45; ST/terminal/context.rs | MISSING | — | L |

## 21. Mobile pairing (`src/bundled/pairing/*`, `src/features/pairing/client.ts`, `ST/pairing/*`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Settings card "Pair mobile" (Account group) | src/bundled/pairing/index.tsx:3-14 | MISSING (no pairing in app/src/ui/settings) | — | S |
| Account gate: "Use existing Buzz account", mismatch error, dev-broker note, desktop-only note | src/bundled/pairing/PairingSettings.tsx:90-206 | MISSING | — | S |
| Community address input ("Enter your community's https:// or wss:// address.") | PairingSettings.tsx:105-115, 220-230 | MISSING | — | S |
| Steps "Scan QR code", "Enter code on your phone" / "Confirm mobile code", "Pair your mobile app" / "Paired" | PairingSettings.tsx:145-170, 365-400 | MISSING | — | M |
| QR display (SVG with reveal) | PairingSettings.tsx:255-262; ST/pairing/qr.rs; ST/pairing/qr-reveal.css | MISSING | — | M |
| Six-digit code entry or comparison | PairingSettings.tsx:263-300 | MISSING | — | S |
| End states "Check your phone" + "Start a new pairing", "Phone paired" + "Pair another phone"; expired, uncertain, error, cancelled | PairingSettings.tsx:330-356; ST/pairing/mod.rs:20-40 | MISSING | — | S |
| Client polling and cancel errors | src/features/pairing/client.ts:100-177 | MISSING | — | S |
| Native pairing session over relay: start, status, confirm, deny, cancel, account | ST/pairing/mod.rs:160-577; ST/pairing/relay.rs; ST/pairing/identity.rs | MISSING | — | L |

## 22. Bestie (`src/bundled/bestie`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| "Bestie" primary page with placeholder "Meet your Bestie" / "Your companion's home in Buzz. Agent chat isn't connected yet." | src/bundled/bestie/index.tsx:9-53 | MISSING | Page and nav icon | S |

## 23. Builderlab (`src/bundled/builderlab/**` = BB)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Settings → Integrations → "Builderlab" card | BB/index.tsx:36-60 | MISSING | — | S |
| Login: "Sign in with Builderlab", "Finish sign-in in your browser…", "Cancel sign-in", "Signed in as {email}.", "Sign out", unavailable reason | BB/login/Login.tsx:9-75 | MISSING | — | M |
| OAuth PKCE through the loopback callback, code exchange, `/v1/auth/me` check | BB/oauth/browser.ts; BB/oauth/session.ts | MISSING | Needs §24 | M |
| "Remote agents" list ("No remote agents yet.", Refresh / Retry) | BB/agents/RemoteAgents.tsx:278-465 | MISSING: buzzd hosts are self-hosted processes, not the Beekeeper service | — | M |
| Create remote agent (name, optional instructions, community or Personal space), enrollment (attest, kind-30177 registration), "Finish setup", "Retry community setup" | BB/agents/RemoteAgents.tsx:285-330; BB/agents/enrollment.ts; BB/agents/registration.ts | MISSING | — | L |
| Edit instructions on Active agents | BB/agents/RemoteAgents.tsx:382-430 | MISSING | — | S |
| "Delete {name}?" (kind-5 deletion, Beekeeper delete, tombstones) | BB/agents/RemoteAgents.tsx:465-486; BB/agents/enrollment.ts | MISSING | — | M |

## 24. OAuth loopback callback (`ST/oauth_callback/*`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `oauth_callback_begin` / `wait` / `cancel`: 127.0.0.1 ephemeral listener, injected `redirect_uri`, 256-bit single-use state, opens browser, timeout | ST/oauth_callback/mod.rs:177-215; ST/oauth_callback/README.md | MISSING | — | M |
| Callback HTML page "Sign-in callback" with strict headers | ST/oauth_callback/mod.rs:330-349 | MISSING | — | S |

## 25. Claude Code agent plugin (`src/agent-plugins/claude-code/*` = CC)

The Claude Code harness preset (claude-agent-acp) exists in the port (host/harness.rs:40-47). This plugin is a separate Agents2 type.

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Agents2 type "Claude Code" (one continuing session per thread; summary = model) | CC/index.ts:39-52 | MISSING | Agents2 first | L |
| Claude tab: status ("Checking Claude Code…", "not installed", "needs you to sign in", "is ready (account)"), "Install Claude Code", "Sign in", "Cancel", "Check again", setup output, sign-in code + "Send" | CC/tabs.ts:100-225; CC/setup.ts | MISSING | — | M |
| "Running sessions" list (Working/Idle; empty copy) | CC/tabs.ts:228-270 | MISSING | — | S |
| Settings tab: Model (default/Opus/Sonnet/Haiku), Workspace, Sessions (per thread/channel), Answers (Only you/Anyone) + warning | CC/tabs.ts:20-31, 300-350 | MISSING | — | S |
| Runtime: warmed spare session, live-session cap, memory read, base prompt, saved sessions | CC/runtime.ts; CC/sessions.ts; CC/prompt.ts; CC/base_prompt.md; CC/client.ts | MISSING | — | L |

## 26. Plugin system (`src/plugins/*`, `src/features/host/service.ts`; native `ST/host_process.rs`, `host_command.rs`, `host_request.rs`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Plugin manager: bundled and external plugins, enable/disable, activation with timeout, starting/active/failed status | src/plugins/manager.ts; src/plugins/runtime.ts; src/plugins/timeout.ts; src/plugins/status.ts | MISSING | — | L |
| Persisted plugin configuration (`buzzodz.plugins.v1`) and module loader | src/plugins/storage.ts; src/plugins/modules.ts | MISSING | — | M |
| Settings → Plugins UI, plugin import from folder or Git, "External plugins are paused for this launch", recovery screen | src/app/Settings.tsx:219-320 (uses src/plugins) | MISSING | — | M |
| Contribution resolution (match order) and owned-contribution error boundary | src/plugins/contributions.ts; src/plugins/OwnedContribution.tsx | MISSING | — | M |
| Manifest host capabilities (commands, networkOrigins, processes) and the public author API | src/plugins/api.ts:6-42; src/plugins/author.ts; src/plugins/types.ts | MISSING | — | M |
| Host service for plugins (request, run command, spawn) | src/features/host/service.ts | MISSING | — | M |
| `plugin_host_process_spawn` / `write` / `kill` (streamed stdout/stderr, killed on reload or exit, identity stripped) | ST/host_process.rs:1-431 | MISSING | — | M |
| `plugin_host_run_command` (bounded stdout, effective PATH) | ST/host_command.rs:178-300 | MISSING | — | S |
| `plugin_host_request` (HTTP to declared origins) | ST/host_request.rs:39-367 | MISSING | — | S |
| Trusted plugin security service for agents (`agent_security`: register, default and per-agent policies) | FA/security.ts:1-115; ST/agents.rs:1650-1675 | MISSING | — | M |

## 27. buzz-mcp (`src/buzz-mcp/*`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| Standalone stdio MCP server (`BUZZ_RELAY_URL`, `BUZZ_PRIVATE_KEY`, `BUZZ_AUTH_TAG`, `BUZZ_CHANNEL`, `BUZZ_MCP_ROOT`); `dist-buzz-mcp` build | src/buzz-mcp/main.ts:1-53; src/buzz-mcp/node.ts; vite.buzz-mcp.config.ts | MISSING | — | M |
| Tool `send` (text/path, channel, reply, files, mentions, final) | src/buzz-mcp/tools.ts:28-45 | MISSING | — | S |
| Tool `react` | src/buzz-mcp/tools.ts:46-51 | MISSING | — | S |
| Tool `read` (thread, channel latest, search q) | src/buzz-mcp/tools.ts:52-55 | MISSING | — | S |
| Tool `channels` | src/buzz-mcp/tools.ts:56 | MISSING | — | S |
| Tool `members` | src/buzz-mcp/tools.ts:57 | MISSING | — | S |
| Tool `users` | src/buzz-mcp/tools.ts:58 | MISSING | — | S |
| Tool `dm` | src/buzz-mcp/tools.ts:59 | MISSING | — | S |
| Tool `mem_get` | src/buzz-mcp/tools.ts:60 | MISSING | — | S |
| Tool `mem_set` | src/buzz-mcp/tools.ts:61-63 | MISSING | — | S |
| Tool `canvas` | src/buzz-mcp/tools.ts:64-68 | MISSING | — | S |
| Client and RPC layer (`callTool`, error text) used by the Claude Code plugin | src/buzz-mcp/client.ts; src/buzz-mcp/rpc.ts | MISSING | — | S |

## 28. Native agent backend (`ST/agents.rs`, `ST/agents/*`, `ST/agent_models.rs`, `ST/goose_models.rs`, `ST/pi_models.rs` vs `host/`)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `agent_control_save` / `agent_control_save_defaults` restart running agents whose effective settings changed (`restarted`, `restartFailures`) | ST/agents.rs:970-1070 | IN PROGRESS (host Save does not restart today: host/lib.rs:226-233) | — | M |
| "Restart required" from the controller's `restartDiff` | FA/control.ts:15-25, 79-80; ST/agents.rs:1068-1070 | DIFFERENT: based on a revision mismatch (ui/agents.rs:299-304) | Expose `restart_diff` | S |
| Start pipeline: credential request, "waiting" (Keychain), one pending start per agent ("Agent start already in progress; use Stop to cancel"), Stop cancels a waiting start, start tickets | ST/agents.rs:1150-1350 | DIFFERENT: calls `controller.action(Start)` directly on the host thread (host/lib.rs:206-218) | Async start with cancellable waits | M |
| Pi preflight `verify` before start | ST/pi_models.rs:37-150; ST/agents.rs:1240-1290 | MISSING | — | S |
| Databricks OAuth credential wait at start; Disconnect cancels waiting and queued starts ("Start cancelled by Disconnect; reconnect and retry Start") | ST/agents.rs:816-853 | IN PROGRESS (Disconnect) | — | M |
| Refuse start while old Buzz is running ("Stop old Buzz before starting agents here…") | ST/agents.rs:587-593, 1613-1648 | MISSING | — | S |
| Mention attach and replay floor (`agent_control_attach_mention`, `action` with `replay_floor`) | ST/agents.rs:559-586, 1103-1126 | MISSING | See §16 | M |
| Harness log: `agent_control_log_challenge` + `agent_control_read_log` (owner-signed challenge) and the profile log viewer | ST/agents.rs:598-662, 937-961; src/bundled/profiles/ProfileHarnessLog.tsx | MISSING: diagnostics lines only | Host log request and viewer | M |
| `agent_control_use_here` | ST/agents.rs:1354-1365 | MISSING | See §9 | M |
| `agent_control_local_clone_settings` / `agent_control_clone_settings` | ST/agents.rs:1366-1386 | MISSING | See §9 | S |
| `agent_control_snapshot_memory_write` (restore memories from a snapshot) | ST/agents.rs:1595-1612; ST/agents/snapshot_memory.rs | MISSING | — | M |
| Agent-signed owner-addressed memory event publication | ST/agents/profile_http.rs:93-126 | MISSING | — | S |
| Team instructions, capture, export, preview | ST/agents.rs:1676-1823 | MISSING | See §12 | M |
| Databricks catalog `agent_models_run` connect/refresh (PKCE through `buzz_agent`, cache dir, build-default host/filter) | ST/agent_models.rs:20-31, 245-630 | MISSING (the host refuses: host/lib.rs:375-381). The Databricks fields are IN PROGRESS | Host OAuth and catalog | L |
| `agent_models_run` disconnect | ST/agent_models.rs:48-53; ST/agents.rs:816-853 | IN PROGRESS | — | S |
| `agent_models_run` test (one tiny completion; Goose and Pi inference checks) | ST/agent_models.rs:52-53; ST/goose_models.rs:25-60; ST/pi_models.rs:154-275 | IN PROGRESS | — | M |
| `agent_models_begin` / `agent_models_cancel` ticket admission (one lookup at a time, cancellable) | ST/agent_models.rs:245-260 | MISSING | — | S |
| `claude_auth_status` (`claude auth status`) | ST/agents.rs:148-200 | MISSING | — | S |
| Snapshot fields: `harnessOptions` with `defaultArgs`, providers and `configurationPolicy` (authentication, model-with-provider) supplied natively | ST/agents.rs:306-470 | DIFFERENT: providers and efforts hardcoded in host/harness.rs:84-150; no defaultArgs or policy | Move to host-supplied options | M |
| Snapshot flags: `createAvailable`, `importAvailable`, `localInventoryActions`, `avatarEditingAvailable`, `parked`, `inventoryWarnings`, `databricksDefaults` | ST/agents.rs:526-545; FA/control.ts:120-160 | MISSING (host/api.rs:136-146 has seq, agents, runtime fields, default workspace) | Add as features land | S |

## 29. Harness setup, managed tools, detection (`ST/harness_setup.rs`, `ST/managed_npm.rs`, `ST/agents.rs` harness options)

| Feature | buzz-app reference | Port status | What's needed | Size |
|---|---|---|---|---|
| `pi_install`: app-owned Node + npm install of Pi and buzz-pi-acp, install/update ("Install" / "Update Pi") | ST/harness_setup.rs:244-323; ST/managed_npm.rs:439-500 | MISSING | Managed tools under the host's state dir | L |
| `claude_install`: managed Claude Code adapter; rejects the native stub | ST/harness_setup.rs:325-369; ST/managed_npm.rs | MISSING | — | M |
| Pinned Node artifacts with checksum, scrubbed npm env, prefix-link refusal, release shims, pruning | ST/managed_npm.rs:20-438 | MISSING | — | M |
| One install at a time ("A Harness installation is already in progress."); installer process group killed on Quit; install log; failure copy | ST/harness_setup.rs:19-200; FA/control.ts:587-615 | MISSING | — | S |
| After a Pi install, start agents that were waiting for Pi | ST/harness_setup.rs:210-243; ST/agents.rs:796-815 | MISSING | — | S |
| Detection prefers managed tools over user tools; statuses "Ready" / "CLI needed" / "Adapter needed" / "Unknown"; Pi update available | ST/agents.rs:258-305 | PARTIAL: PATH-only detection, statuses "Ready" / "Needs a, b" (host/harness.rs:150-189; ui/agents.rs:1175-1226) | Status vocabulary and managed paths | M |

---

## DONE count

These features match buzz-app closely enough and are not listed above.

| Area | DONE | Items counted |
|---|---|---|
| Page frame and inventory | 10 | Header title; "Add agent" button; page menu "Refresh agents"; "Reading local agent status…"; "statuses may be out of date"; empty-state copy; sort by name then pubkey; tile grid column math; runtime-unavailable alert; live status updates (push events replace polling) |
| Agent card | 8 | Card chrome and min height; 80px squircle portrait with initial fallback; name truncation; "Restart required" badge; "Actions for {name}" trigger; "Manage agent"; "Edit"; "Delete" (danger) |
| Manage dialog | 9 | Title; process label with "Last known:"; "Starts with this app."; error text; profile-pending line + "Retry profile"; Start / "Retry start" (prominent); Stop; relay line; process-status label table |
| Add agent dialog and fields | 35 | Title; description; 860px width; sidebar "Create agent"; Name; Agent instructions (6 rows); "AI configuration" legend; "(install first)" disabled harnesses; Custom executable + Executable; provider inherit labels; Custom provider; "LLM Provider" label; Pi "(API key needed)"; "Loading signed-in providers…"; model placeholders; "Type a model ID…"; "Loading models…"; "Retry models"; Conversation context select + description; Environment accordion (Workspace, Arguments); `BUZZ_ACP_AGENTS=10` pre-listed; env row placeholders and states; Remove/Undo; Variable name + Add variable + both errors; footer helper; five validation messages; "Creating agent…" / "{name} was created. Starting it…"; footer Cancel/Close + Create/Creating…/Start agent; request-id reuse on retry; preset note; saved-key placeholders; Pi model pick sets provider and model; write-only saved env keys; Goose catalog; Pi catalog |
| Edit agent dialog | 7 | Title, width and close X; identity block; "Save changes" disabled until dirty; "Discard changes" when stale; Cancel; Relay detail; Host diagnostics |
| Delete dialog | 1 | Title, body, Cancel, destructive "Delete agent", inline error |
| Import from old Buzz | 2 | Installed/Development source select with reload; stripped-team-instructions note |
| Misc agent features | 1 | Harness preset metadata (Hermes, Claude Code) |
| Agent control (frontend contract) | 5 | Start on app launch; retry profile publication; create (prepare + owner attestation + commit); delete, import preview/commit and save defaults requests; restart action |
| Native backend | 9 | Restore start-on-launch agents at host start; shut agents down on exit; publish pending profiles at start; creation-profile publication with current check; login-shell PATH discovery; owner attestation signed by the GUI identity; import preview/commit in the host; save defaults; isolated model lookups (cleared env, process-group kill) |
| Harness list | 1 | Harness rows with Ready status and "Check again" |
| **Total** | **88** | |

## Counts by status

| Status | Rows |
|---|---|
| MISSING | 288 |
| PARTIAL | 33 |
| DIFFERENT | 38 |
| IN PROGRESS | 10 |
| DONE (counted only) | 88 |

The 10 IN PROGRESS rows cover the six in-progress items: restart on save appears twice (UI and native); Databricks Disconnect appears three times (UI, model run, start cancellation); Test connection appears twice. Two Databricks rows (browser sign-in and catalog connect/refresh) stay MISSING: that work is not on the in-progress list.
