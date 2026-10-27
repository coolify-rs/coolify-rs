# Application UI for `build_pack = 'dockerimage'`

Source: `.upstream/snapshot` (Coolify main @ b5823c7). Paths are relative to the snapshot. Strings in
backticks or quotes are verbatim. `{…}` marks interpolation. `U` = application uuid, `P` = project uuid,
`E` = environment uuid, `D` = deployment uuid.

**Read this first.** This snapshot ships the redesigned UI: cards built from `application-settings-section`,
grouped sidebar menus teleported into the main left nav, resource actions docked in the fixed top bar. It does
**not** match the classic v4 tab UI. Domains no longer live on the General page: there is a separate Domains page
(`app/Livewire/Project/Application/Domains.php`, about 2.3k lines, not covered here). Spec it separately.

Related notes: `applications-api.md` (API create path), `deployment-job-dockerimage.md` (what a deploy does),
`applications-lifecycle.md` (stop/restart internals).

---

## 0. Shared plumbing

### 0.1 Toasts (`resources/views/layouts/base.blade.php:543-623`)
Livewire `dispatch('success'|'error'|'warning'|'info', …)`:
- one string argument → toast titled `Success` / `Error` / `Warning` / `Info`, with the string as description;
- two arguments → title = arg 1, description = arg 2.
`error` uses toast type `danger`. Descriptions render HTML, so `<br>` and `<a>` in messages are real markup.

`handleError($e, $this)` (`bootstrap/helpers/shared.php:663-697`): `TooManyRequestsException` → `Too many requests. Please try again in {n} seconds.`;
`UniqueConstraintViolationException` → `Duplicate entry found. Please use a different name.`; `ModelNotFoundException` → 404;
anything else → `dispatch('error', $e->getMessage())`. A caught `ValidationException` therefore becomes an `Error`
toast. Its message is Laravel's summary: the first message, plus ` (and {n} more error)` / ` (and {n} more errors)`.

### 0.2 Form components
- **`x-forms.input`**, `resources/views/components/forms/input.blade.php` with `app/View/Components/Forms/Input.php`:
  - Label row: `label` text, then a red `*` (`x-highlighted`) when `required`, then a `?` helper (`x-helper`, popover with HTML) when `helper` is set.
  - `wire:model="{id}"` is deferred: the value syncs on the next request.
  - A dirty field gets an inset 4px left bar, `#6b16ed` in light mode and `#fcd452` in dark mode.
  - The input is disabled while any request is loading (`wire:loading.attr="disabled"`).
  - `type="password"` adds an eye toggle and a copy button.
  - The validation error appears under the field as red text (`text-red-500`) inside `<label class="label">`.
  - `canGate` / `canResource` disables the field when the user is not authorized.
  - The HTML id is `{id}-{random}`.
- **`x-forms.listbox`**, `resources/views/components/forms/listbox.blade.php`:
  - A custom dropdown. Options are `{value, label, description?, disabled?}`.
  - The value is `@entangle(id)`, or `.live` with `live`.
  - With `onChange="method"`, picking an option sets the value and then calls `$wire.method()`.
  - The placeholder is `Select…`.
- **`x-unsaved-bar`**, `resources/views/components/unsaved-bar.blade.php`:
  - A floating pill at the bottom center. Text: `You have changes that haven't been saved yet.`
  - Buttons: `Reset` (reloads the window) and `Save changes` with a `<kbd>Enter</kbd>` hint (calls `action`).
  - It appears 300 ms after the component becomes dirty, scoped to `targets`, and hides while a request is saving.
  - Pressing Enter anywhere (outside a textarea, button, link, select or contenteditable) triggers the save.
- **`x-application.settings-section`**, `resources/views/components/application/settings-section.blade.php`:
  - A `<section class="application-settings-section" id=… data-settings-section-title=title>` with a `<header><h3>`.
  - With `helper`, the title is underlined and opens a popover; the helper label is `More information about {title}`.
  - Optional `actions` slot, then a body `div.application-settings-section-body` (`is-flush` when `flush`).
- **`x-application.settings-subsection`**, `…/settings-subsection.blade.php`:
  - A `<section>` with `mt-5 border-t pt-5`; no top border when `first`.
  - Header `h3` (`text-sm font-semibold`), plus an optional helper and `actions` slot.
- **`x-modal-confirmation`**, `resources/views/components/modal-confirmation.blade.php`:
  - A two- or three-step modal teleported to `<body>`. The header is `title` plus an × close button.
  - **Step 1** appears only when `checkboxes` are given. It shows the checkboxes and a red `{step1ButtonText}` button (default `Continue`).
  - **Step 2:**
    - A danger callout titled `Warning`. Its text is `warningMessage`, or by default `This operation is permanent and cannot be undone. Please think again before proceeding!`
    - `The following actions will be performed:`, followed by each action as a red `- {action}` item, and the label of each checked checkbox.
    - When `confirmWithText` is set: `Confirm actions`, `{confirmationLabel}`, a read-only copyable input with `confirmationText`, a `{shortConfirmationLabel}` input, and the button stays disabled until the texts match.
    - Footer buttons: `Cancel` (or `Back` when there are checkboxes) and a red `{step2ButtonText}`.
  - **Step 3** appears only when `confirmWithPassword` is set and the password step is not skipped:
    - Callout titled `Final Confirmation`: `Please enter your password to confirm this destructive action.`
    - Label `Your Password`, placeholder `Enter your password`.
    - Buttons: `Back` and the `{step3ButtonText}` button (default `Confirm`).
  - Submitting sets each checkbox property with `$wire.$set(id, checked)`, then calls `submitAction(..., password, selectedActions?)`.
  - When the password step is skipped and `step2ButtonText` is `Continue`, the step-2 button says `Confirm`.
  - Defaults: `title` `Are you sure?`, `confirmationText` `Confirm Deletion`, `confirmationLabel` `Please confirm the execution of the actions by entering the Name below`, `shortConfirmationLabel` `Name`.
- **`x-status-badge`**, `resources/views/components/status-badge.blade.php`:
  - A pill (`h-6 rounded-full border bg-neutral-100 px-2 text-xs`, dark `bg-white/[0.07]`).
  - A dot coloured by `type`: `neutral` = `bg-neutral-400` (dark `bg-neutral-500`), `success` = `bg-emerald-500`, `warning` = `bg-warning`, `error` = `bg-red-500`.
  - Text is `label` and `status` joined with a space.
  - `pulse` adds a ping animation. `dynamic` renders the slot instead.
- **`x-empty`**, `resources/views/components/empty.blade.php`:
  - A centered empty state with an icon box (`icon-name`), `title`, `description` and an optional `contents` slot.
  - `size` sets the minimum height: `sm` = `min-h-32`, `base` = `min-h-40`, `lg` = `min-h-96`.
- **`x-callout`**, `resources/views/components/callout.blade.php`: types `warning`, `danger`, `info`, with an icon, a title and the slot text.
- **`x-forms.button`**, `resources/views/components/forms/button.blade.php` and `app/View/Components/Forms/Button.php`:
  - Class `button`, plus `isHighlighted` or `isError` variants.
  - When it has `wire:click`, it shows a spinner and is disabled while loading.
  - `canGate` without permission disables it and shows the tooltip `You do not have permission to perform this action.`
- **`x-forms.copy-input`**, `resources/views/components/forms/copy-input.blade.php`: a label, a read-only input that selects its text on focus, and a copy button.

---

## 1. Creating a Docker image application ("New resource" flow)

### 1.1 Flow up to the image form
- Page `GET /project/{P}/environment/{E}/new` (route `project.resource.create`, `routes/web.php:386`, middleware `can.create.resources`) → `App\Livewire\Project\Resource\Create`.
  - Page title: `{project name limit 10} > New | Coolify` (`resources/views/livewire/project/resource/create.blade.php:2-4`).
- With no `type`, it renders `livewire:project.new.select`. The "Docker Image" card is `app/Livewire/Project/New/Select.php:209-214`:
  - id `docker-image`, name `Docker Image`
  - description `Deploy an application using a prebuilt image from any Docker registry, without a Git repository.`
  - docs `https://coolify.io/docs/applications`
- `setType('docker-image')` (`Select.php:392-449`):
  - If there is exactly one usable server and no build servers, it auto-selects that server.
  - Otherwise it goes to the `servers` step.
- `setServer()` (`:451-470`): if the server has exactly one destination (standalone or swarm), it auto-selects it.
- `whatToDoNext()` (`:500-514`) redirects to `project.resource.create` with query `type=docker-image&destination={destination uuid}&server_id={id}`.
- `Create::mount()` (`app/Livewire/Project/Resource/Create.php:18-134`):
  - Needs `createAnyResource`.
  - Redirects to `dashboard` when the project, environment or destination is missing.
  - For `docker-image` it only sets `$type`, and the view renders `<livewire:project.new.docker-image :type="$type" />` (`create.blade.php:19-20`).

### 1.2 Image form: `app/Livewire/Project/New/DockerImage.php` + `resources/views/livewire/project/new/docker-image.blade.php`

Layout (`docker-image.blade.php:1-32`):
- Wrapper `mt-8 w-full max-w-[920px] lg:mt-3`; it autofocuses the first `autofocus` input.
- `<form wire:submit="submit">` contains one `section.application-settings-section`.
- Header (`div.application-settings-section-header`):
  - `<h2>Docker image</h2>`
  - `<p>Deploy an existing image from Docker Hub or another OCI registry.</p>`
  - Right side: a highlighted submit button `Create application`.
- Body (`application-settings-section-body space-y-4`):

| Property (Livewire) | Label | Placeholder | Helper | Notes |
|---|---|---|---|---|
| `imageName` (string, default `''`) | `Image name` | `nginx, ghcr.io/user/app:v1.2.3, or nginx:stable@sha256:…` | `Paste a complete image reference, or enter a name and use one of the optional fields below.` | `required` (red `*`), `autofocus` |
| `imageTag` (string, default `''`) | `Tag` | `latest` | `Use a mutable tag such as latest or v1.2.3.` | |
| `imageSha256` (string, default `''`) | `SHA256 digest` | `59e02939b1bf39f16c93138a28727aec…` | `Use the 64-character digest without the sha256: prefix.` | |

- Tag and digest sit in a grid (`sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] sm:items-end`, `aria-label="Tag and SHA256 digest are mutually exclusive"`).
- Between them is a centered `OR` (`text-xs font-semibold text-neutral-400`, `sm:h-9`).

**Auto-split, `updatedImageName()` (`DockerImage.php:39-82`).** The field uses deferred `wire:model`, so this runs on the submit request, before `submit()`.
- It does nothing when `imageName` is empty, when `imageTag` or `imageSha256` is already filled, or when `imageName` contains neither `:` nor `@`.
- It parses with `App\Services\DockerImageParser` (`app/Services/DockerImageParser.php`). When the base name differs from the input:
  - digest → `imageSha256 = <hash>`, `imageTag = ''`;
  - else, when the tag is not `latest` or the input contained `:` → `imageTag = <tag>`, `imageSha256 = ''`;
  - `imageName = base name`.
- Parser errors are ignored.
- Parser rules:
  - `/^(.+)@sha256:([a-f0-9]{64})$/i` → digest.
  - Otherwise, the last `:` is a tag separator only if it comes after the last `/`. A 64-hex tag is flagged as a hash.
  - The first path segment is the registry when it contains `.` or `:`.

**`submit()` (`DockerImage.php:84-163`):**
1. `authorize('create', Application::class)`.
2. Validation. No custom messages or attributes, so the field names are Laravel's defaults `image name`, `image tag`, `image sha256`. Errors show inline under the fields because they are not caught.
   - `imageName`: `required|string|max:255|regex:DOCKER_IMAGE_NAME_PATTERN` (`app/Support/ValidationPatterns.php:145,271-286`).
     `/\A(?=.{1,255}\z)(?:(?:(?:[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?|\[[0-9a-f:.]+\])(?::[0-9]+)?\/)?[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*(?:\/[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*)*)(?:@sha256)?\z/`
   - `imageTag`: `nullable|string|max:128|regex:/\A[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}\z/` (`:153,291-306`).
   - `imageSha256`: `nullable|string|regex:/^[a-f0-9]{64}$/i`.
3. If both tag and digest are set, it adds the error `Provide either a tag or SHA256 digest, not both.` to **both** `imageTag` and `imageSha256`, and returns.
4. Reference string:
   - digest → `{imageName}@sha256:{hash}` (a leading `sha256:` is stripped, case-insensitive, after trim);
   - tag → `{imageName}:{tag}`;
   - neither → `{imageName}:latest`.
   The string is re-parsed with `DockerImageParser`.
5. Destination: `find_resource_destination_for_current_team(query.destination)`. When missing it throws `Exception('Destination not found.')`, which is uncaught and gives a Livewire error page.
6. Project and environment are looked up by route uuids under the current team (`firstOrFail`).
7. `docker_registry_image_name` = parser base name; on digest it gets the suffix `@sha256` unless it already ends with it. `docker_registry_image_tag` = `sha256-{hash}` on digest, else the tag.
8. INSERT `applications`:
   - `name`: `docker-image-{new_public_id()}` (24 lowercase random chars)
   - `repository_project_id`: `0`
   - `git_repository`: `coollabsio/coolify`
   - `git_branch`: `main`
   - `build_pack`: `dockerimage`
   - `ports_exposes`: `80`
   - the two image columns from step 7
   - `environment_id`, `destination_id`, `destination_type` (destination morph class)
   - `health_check_enabled`: `false`
9. UPDATE `name = 'docker-image-{app uuid}'` and `fqdn = generateUrl(server, random: app uuid)` (`bootstrap/helpers/shared.php:1477-1491`):
   - The wildcard is `server.settings.wildcard_domain`, else `sslip(server)`, which is `http://{ip}.sslip.io` (`127.0.0.1` for server 0 in dev; IPv6 written with `-` in place of `:`).
   - The result is `{scheme}://{uuid}.{host}{path}`.
10. Redirect (SPA `navigate` unless `instance_settings.is_wire_navigate_enabled = false`) to `project.application.configuration` = `/project/{P}/environment/{E}/application/{U}`.

There is no success toast. The UI path writes fewer columns than the API path (`applications-api.md`).

---

## 2. Application page frame

### 2.1 Routes (`routes/web.php:390-425`)
Prefix: `project/{project_uuid}/environment/{environment_uuid}/application/{application_uuid}`. Every row below is a `GET`.

| Path | Name | Component |
|---|---|---|
| `/` | `project.application.configuration` | `Configuration` → General |
| `/domains` | `project.application.domains` | Configuration → `Domains` + `Shared\Maintenance` |
| `/swarm` | `project.application.swarm` | Configuration → `Swarm` (only if the server is a swarm, else empty) |
| `/advanced` | `project.application.advanced` | Configuration → `Advanced` |
| `/environment-variables` | `project.application.environment-variables` | Configuration → `Shared\EnvironmentVariable\All` + `Shared\SecretManagerLinks` |
| `/persistent-storage` | `project.application.persistent-storage` | Configuration → `Service\Storage` |
| `/backups` | `project.application.backup.index` | `Application\Backup\Index` |
| `/backups/{backup_uuid}` (+ `/executions`, `/danger`) | `project.application.backup.show` / `.executions` / `.danger` | `Application\Backup\Show` (`/s3` and `/retention` redirect to show) |
| `/source` | `project.application.source` | Configuration → `Source` only if `git_based()`. **dockerimage: empty content** |
| `/servers` | `project.application.servers` | Configuration → `Shared\Destination` |
| `/scheduled-tasks` | `project.application.scheduled-tasks.show` | Configuration → `Shared\ScheduledTask\All` |
| `/webhooks` | `project.application.webhooks` | Configuration → `Shared\Webhooks` |
| `/preview-deployments` | `project.application.preview-deployments` | Configuration → `Previews` |
| `/healthcheck` | `project.application.healthcheck` | Configuration → `Shared\HealthChecks` (compose apps redirect to `/`) |
| `/rollback` | `project.application.rollback` | Configuration → `Rollback` |
| `/resource-limits` | `project.application.resource-limits` | Configuration → `Shared\ResourceLimits` |
| `/resource-operations` | `project.application.resource-operations` | Configuration → `Shared\ResourceOperations` |
| `/metrics` | `project.application.metrics` | Configuration → `Shared\Metrics` |
| `/analytics` | `project.application.analytics` | Configuration → `Analytics` (lazy when traffic analytics is enabled) |
| `/tags` | `project.application.tags` | Configuration → `Shared\Tags` |
| `/danger` | `project.application.danger` | Configuration → `Shared\Danger` |
| `/deployment` | `project.application.deployment.index` | `Deployment\Index` |
| `/deployment/{deployment_uuid}` | `project.application.deployment.show` | `Deployment\Show` |
| `/logs` | `project.application.logs` | `Shared\Logs` (runtime logs) |
| `/terminal` | `project.application.command` | `Shared\ExecuteContainerCommand` (middleware `can.access.terminal`) |
| `/files` | `project.application.files` | `Shared\FileBrowser` (middleware `can.access.terminal`) |
| `/tasks/{task_uuid}` | `project.application.scheduled-tasks` | redirect → `scheduled-tasks.show` |

`Configuration` (`app/Livewire/Project/Application/Configuration.php`):
- Resolves project → environment → application under `currentTeam()`, with `firstOrFail` (404).
- `currentRoute` is the route name, refreshed only on full page loads.
- Listens to `buildPackUpdated` and `refresh`, both `$refresh`.

View `resources/views/livewire/project/application/configuration.blade.php`:
- Title `{app name limit 10} > Configuration | Coolify`.
- `livewire:project.shared.configuration-checker` draws the "Configuration changes · Deploy again to apply" warning in `#configuration-warning-hud-slot`; it is out of scope here.
- Then `livewire:project.application.heading`.
- Then `section.application-settings-workspace mt-4 lg:mt-0` with the grid `xl:grid-cols-[210px_minmax(0,1fr)] gap-8`: the sidebar component, then the page content (a big `@if` on `currentRoute`).

### 2.2 Sidebar: `resources/views/components/application/configuration-sidebar.blade.php`
Rendering:
- It renders twice:
  - in-page `<aside class="application-settings-navigation in-main-nav">`, hidden by CSS (`resources/css/app.css:2172`); it carries `data-settings-search-items` for the ⌘K palette;
  - `@teleport('#main-nav-context-0')`, which replaces the main left sidebar menu with a `Dashboard` / `Back to the main menu` back link (`registerMainNavContext(0, 'Dashboard', 'Back to the main menu')`).
- Each item is an `<a class="menu-item [menu-item-active]">` with `wire:navigate` (except Terminal), an icon (`x-reicon`) and `span.menu-item-label`.
- Groups are separated by a `border-t` divider.

Item visibility (`:10-139`). **For dockerimage on a non-swarm server, with terminal access:**

| Group | Items shown (in order) |
|---|---|
| Settings | General, Domains, Environment Variables, Persistent Storage, Advanced, Healthcheck (Swarm only if `server.isSwarm()`) |
| Observe & troubleshoot | Runtime Logs, Deployment Logs, Terminal, Files (with a beta badge), Metrics, Analytics |
| Deploy | Servers (with the server status badge), Preview Deployments (shown because `build_pack === 'dockerimage'`). **Git Source hidden** (`git_based()` is false for dockerimage, `app/Models/Application.php:1452-1462`) |
| Automation | Scheduled Tasks, Webhooks, Backups |
| Operations | Resource Operations, Resource Limits, Rollback, Tags |
| Danger Zone | Danger Zone |

- Terminal and Files are hidden on swarm servers and without `canAccessTerminal`.
- Active rules: Backups is active on `project.application.backup*`, Deployment Logs on `project.application.deployment*`, Scheduled Tasks on `project.application.scheduled-tasks*`.
- Icons (`:147-172`):

| Item | Icon |
|---|---|
| General | `settings` |
| Domains | `globe` |
| Advanced | `grid` |
| Swarm | `destinations` |
| Environment Variables | `variables` |
| Persistent Storage | `storages` |
| Backups | `database` |
| Terminal | `browser-terminal` |
| Files | `folder` |
| Deployment Logs | `time-back` |
| Runtime Logs | `unordered-list` |
| Git Source | `sources` |
| Servers | `servers` |
| Scheduled Tasks | `calendar` |
| Webhooks | `webhook` |
| Preview Deployments | `eye` |
| Healthcheck | `feedback` |
| Rollback | `time-back` |
| Resource Limits | `cpu` |
| Resource Operations | `server-update` |
| Metrics | `graph` |
| Analytics | `analytics` |
| Tags | `tags` |
| Danger Zone | `danger` |

**In-page anchors.** Under the active item only, `div.nav-children` holds `button.menu-subitem` entries. A click calls `history.replaceState(#id)` and `window.scrollToSettingsSection(id)` (`resources/js/app.js:85`), which smooth-scrolls and plays a 500 ms `is-section-highlight` flash.

| Page | Anchors |
|---|---|
| General | `application-details-section` "Application details", `access-section` "Access", `build-pipeline-section` "Build & deploy", `container-section` "Container" (Container omitted for compose) |
| Advanced | `advanced-build-section` "Build & deploy", `advanced-container-section` "Container", `advanced-proxy-section` "Proxy" |
| Webhooks | `deploy-webhook-section` "Deploy webhook", `manual-git-webhooks-section` "Manual Git webhooks" |
| Preview Deployments | `preview-template-section` "URL template", **dockerimage only:** `manual-preview-section` "Manual preview", then `preview-deployments-section` "Deployments" |
| Healthcheck | `healthcheck-configuration-section` "Configuration", `healthcheck-timing-section` "Timing and retries" |
| Resource Operations | `clone-resource-section` "Clone resource", `move-resource-section` "Move resource" |

**Servers badge.** `livewire:project.application.server-status-badge` (`resources/views/livewire/project/application/server-status-badge.blade.php`) draws a red warning-triangle SVG:
- title `One or more servers are unreachable or misconfigured.` when `application.server_status === false`;
- title `Application is in degraded state across multiple servers.` when there are additional servers and the status contains `degraded`.
It refreshes on the team events `ServiceStatusChanged` and `ServiceChecked`.

### 2.3 Heading: `app/Livewire/Project/Application/Heading.php` + `resources/views/livewire/project/application/heading.blade.php`

**Where things render:**
- Below `xl` (`heading.blade.php:14-140`), one row in the page:
  - `h1` = app name (`text-[24px] font-semibold tracking-tight truncate`);
  - under it, `x-status-summary`, the compact `x-applications.links`, the "Deploying…" indicator when one is running, and the restart-limit badge;
  - on the right, the action split button.
- At `xl` and above:
  - the actions are `@teleport('#resource-action-hud-slot')` into the fixed top bar (`resources/views/layouts/app.blade.php:82`): the Deploying indicator, then `x-applications.links` (non-compact, an `app-tab` button with an external-link icon), then the split button;
  - the name and status are in the top-bar breadcrumb (`resources/views/components/top-breadcrumb.blade.php:259-277`): a resource switcher with `livewire:project.application.status` as meta, shown only at `xl`.
- `livewire:project.application.status` (`resources/views/livewire/project/application/status.blade.php`) is `x-status-summary` plus the restart-limit badge, with `wire:poll.10000ms="refreshStatus"` (calls `application.refresh()`).
- The heading root has `wire:poll.10000ms="checkStatus"`. When the server is functional this dispatches the job `GetContainersStatus(server)`; otherwise it shows the toast `Server is not functional.`

**Listeners:**
- `compose_loaded`, `configurationChanged`, `update_links` → `$refresh`.
- Team channel: `ServiceStatusChanged` → `checkStatus`, `ServiceChecked` → `$refresh`.
- `ApplicationStopFailed` → error toast titled `Failed to stop the application.` with `{message}` as description, only when the uuid matches.

**`x-status-summary`** (`resources/views/components/status-summary.blade.php`). Input is `application.status`, for example `running:healthy`, `exited:unhealthy`, `running:unknown:excluded`, or `running (healthy)`.
- Parsing: `container` = part before `:`, `health` = part after. Without `:`, the `x(y)` form is accepted. `excluded` anywhere means monitoring is off.
- `containerLabel` = `Str::headline(container)`, for example `Running`, `Exited`, `Restarting`.
- `healthLabel`:
  - `Monitoring disabled` when excluded;
  - `Healthy` / `Unhealthy` / `Starting`;
  - otherwise `Not configured`.
- `containerType`:
  - starts with `running` → success;
  - starts with `starting`, `restarting` or `degraded` → warning;
  - anything else → error.
- Summary label and type (first match wins):
  1. container type error → `{containerLabel}`, error;
  2. container starts with `degraded` → `Degraded`, warning;
  3. health `unhealthy` → `Degraded`, error;
  4. container type warning → `{containerLabel}`, warning;
  5. excluded → `{containerLabel} (monitoring disabled)`, warning;
  6. health `starting` → `{containerLabel} (healthcheck starting)`, warning;
  7. health missing or unknown → `{containerLabel} (no healthcheck)`, warning;
  8. otherwise → `{containerLabel}`, success.
- Rendering: `x-status-badge as=button` with a dot (`bg-success` / `bg-warning` / `bg-error`), the label and a chevron.
- Clicking opens a `listbox-panel` menu:
  - header `Application status`;
  - row `Container` with its dot and `{containerLabel}`;
  - row `Healthcheck` with its dot and `{healthLabel}`. The row links to `/healthcheck` with `wire:navigate`.
  - When the health label is `Not configured`, a `?` helper (label `About unconfigured healthchecks`) says: `No healthcheck is configured, so Coolify can only report the container state. Traffic can still be routed to the container, but Coolify cannot verify that the application inside it is ready to receive requests.`

**Restart-limit badge** (`resources/views/components/application/restart-limit-warning.blade.php`):
- Condition: `stoppedAfterRestartLimit()` = status starts with `exited` && `container_present === true` && `restart_limit_reached === true` (`Application.php:743-748`).
- Renders a warning `x-status-badge` with text `Restart limit reached` and title `Container has crashed and Coolify stopped it after {max(restart_count, restartLimitMaximum())} restart attempts.`

**Links menu** (`resources/views/components/applications/links.blade.php`):
- Button text `Links` with a chevron.
- Each `fqdn` (comma split) becomes `<a target=_blank>` with a `Production` badge (`deployment-badge deployment-badge-production`) and `getFqdnWithoutPort(fqdn)`.
- Each preview with an fqdn becomes a `PR #{n}` badge (`deployment-badge-preview`) and the URL.
- Each `ports_mappings_array` entry becomes:
  - server 0: `http://localhost:{hostPort}`, text `Port {mapping}`;
  - other servers: `http://{server ip}:{hostPort}`, text `{ip}:{hostPort}`, repeated for each additional server.
- With no links: `No links available`.

**Deploying indicator** (`resources/views/components/deploying-indicator.blade.php`):
- Shown when `runningDeploymentUrl` is set: the `deployment_url` of the newest queue row of this app with status `in_progress` or `queued` (`Heading.php:151-157`).
- `<a href wire:navigate title="View the running deployment log">` with a spinning SVG and the text `Deploying…`.

**Action split button** (`resources/views/components/split-action.blade.php`). Gated by `@can('deploy')`. It has a main button and a caret (`aria-label="More actions"`) that opens a menu of `listbox-option` rows. For dockerimage, `needsComposeFile()` is false.

| Status | Main button | Menu items |
|---|---|---|
| starts with `exited` | icon `play-circle`, `Deploy`, or `Retry deployment` when the restart limit is reached → `deploy` | `Deploy (without cache)` / `Retry deployment (without cache)` → `deploy(true)` (not on swarm); `Remove container` (icon `stop-circle text-error`) → opens the stop modal, only when `container_present !== false` |
| other, non-swarm | icon `refresh`, `Redeploy` → `deploy` | `Redeploy (without cache)` → `force_deploy_without_cache` when status starts with `running`, else `deploy(true)`; `Restart` (icon `restart`) → opens the restart modal; `Stop` (icon `stop-circle text-error`) → opens the stop modal |
| other, swarm | icon `refresh`, `Update Service` → `deploy` | `Stop` |

The modals sit in a hidden div (`heading.blade.php:115-138`) and are triggered with `#application-mobile-stop-trigger` / `#application-mobile-restart-trigger`. The desktop menu uses the same ids.

- **Stop / Remove container modal** (`canGate="deploy"`, `submitAction="stop"`, `confirmWithText=false`, `confirmWithPassword=false`, `step1ButtonText="Continue"`, `step2ButtonText="Confirm"`):
  - Title: `Confirm Container Removal?` when exited, else `Confirm Application Stopping?`
  - Checkbox `docker_cleanup` (Livewire `public bool $docker_cleanup = true`), label `Run Docker Cleanup (remove unused images and builder cache).` (`lang/en.json:43`). Step 1 shows it pre-checked.
  - Actions when exited: `The exited application container will be removed.` and `Anonymous volumes may become eligible for Docker cleanup.`
  - Actions otherwise: `This application will be stopped.` and `All non-persistent data of this application will be deleted.`
- **Restart modal** (`submitAction="restart"`, no checkboxes, so it opens on step 2; footer `Cancel` and `Confirm`):
  - Title: `Confirm Application Restart?`
  - Action: `This application will be restarted without rebuilding.`

**Methods:**
- `deploy(bool $force_rebuild = false)` (`Heading.php:170-221`), `authorize('deploy')`. Pre-checks, each an error toast:
  - swarm with empty `docker_registry_image_name` → title `Failed to deploy.`, description `To deploy to a Swarm cluster you must set a Docker image name first.`
  - `settings.is_build_server_enabled` with an empty image → title `Failed to deploy.`, description `To use a build server, you must first set a Docker image.<br>More information here: <a target="_blank" class="underline" href="https://coolify.io/docs/knowledge-base/server/build-server">documentation</a>`
  - additional servers with an empty image → title `Failed to deploy.`, description `Before deploying to multiple servers, you must first set a Docker image in the General tab.<br>More information here: <a target="_blank" class="underline" href="https://coolify.io/docs/knowledge-base/server/multiple-servers">documentation</a>`
  - Then `D = new_public_id()` and `queue_application_deployment(application, D, force_rebuild)` (`bootstrap/helpers/applications.php:19`).
  - Result `queue_full` → error toast titled `Deployment queue full` with the message `Deployment queue is full. Please wait for existing deployments to complete.`
  - Result `skipped` → error toast titled `Deployment skipped` with the message, for example `Deployment already queued for this commit.`
  - Otherwise redirect (navigate) to `project.application.deployment.show` for `D`. There is no success toast.
- `force_deploy_without_cache()` → `deploy(force_rebuild: true)`.
- `restart()` (`:246-283`):
  - The additional-servers check uses the title `Failed to deploy`, without the period.
  - Then `queue_application_deployment(..., restart_only: true)`.
  - `queue_full` → error toast `Deployment queue full`.
  - `skipped` → a **success** toast titled `Deployment skipped`.
  - Otherwise redirect to the deployment show page.
- `stop()` (`:229-244`): info toast `Gracefully stopping application.<br/>It could take a while depending on the application.`, then queues the job `StopApplication::dispatch(app, false, $docker_cleanup)` and writes the audit entry `ui.application.stopped`. There is no redirect.

---

## 3. General page (dockerimage): `app/Livewire/Project/Application/General.php` + `resources/views/livewire/project/application/general.blade.php`

### 3.1 Properties ↔ columns (`General.php:385-506`, `syncData`)

All properties map to `applications.*` columns unless marked `settings.` (`application_settings`).

| Property | Column | Property | Column |
|---|---|---|---|
| `name` | `name` | `description` | `description` |
| `fqdn` | `fqdn` | `gitRepository` | `git_repository` |
| `gitBranch` | `git_branch` | `gitCommitSha` | `git_commit_sha` |
| `installCommand` | `install_command` | `buildCommand` | `build_command` |
| `startCommand` | `start_command` | `buildPack` | `build_pack` |
| `staticImage` | `static_image` | `baseDirectory` | `base_directory` |
| `publishDirectory` | `publish_directory` | `portsExposes` | `ports_exposes` |
| `portsMappings` | `ports_mappings` | `customNetworkAliases` | `custom_network_aliases` |
| `dockerfile` | `dockerfile` | `dockerfileLocation` | `dockerfile_location` (default `/Dockerfile` only for the dockerfile pack) |
| `dockerfileTargetBuild` | `dockerfile_target_build` | `dockerRegistryImageName` | `docker_registry_image_name` |
| `dockerRegistryImageTag` | `docker_registry_image_tag` | `dockerComposeLocation` | `docker_compose_location` |
| `dockerComposeRaw` | `docker_compose_raw` | `dockerComposeCustomStartCommand` / `…BuildCommand` | `docker_compose_custom_start_command` / `…_build_command` |
| `customLabels` | `custom_labels` (base64; property holds the decoded text, `parseContainerLabels()`) | `customDockerRunOptions` | `custom_docker_run_options` |
| `preDeploymentCommand` | `pre_deployment_command` | `preDeploymentCommandContainer` | `pre_deployment_command_container` |
| `postDeploymentCommand` | `post_deployment_command` | `postDeploymentCommandContainer` | `post_deployment_command_container` |
| `customNginxConfiguration` | `custom_nginx_configuration` | `isHttpBasicAuthEnabled` | `is_http_basic_auth_enabled` |
| `httpBasicAuthUsername` | `http_basic_auth_username` | `httpBasicAuthPassword` | `http_basic_auth_password` (null for users who cannot update) |
| `httpBasicAuthHashAlgorithm` | `http_basic_auth_hash_algorithm` (`bcrypt` / `argon2id`; read from `usesArgon2idBasicAuth()`) | `httpBasicAuthBcryptCost` | `http_basic_auth_bcrypt_cost` (default 10) |
| `httpBasicAuthArgon2idMemoryCost` | `http_basic_auth_argon2id_memory_cost` (65536) | `httpBasicAuthArgon2idTimeCost` | `http_basic_auth_argon2id_time_cost` (4) |
| `watchPaths` | `watch_paths` | `redirect` | `redirect` |
| `isStatic` | `settings.is_static` | `isSpa` | `settings.is_spa` |
| `siteType` | UI only: `dynamic` / `static` / `spa` | `isBuildServerEnabled` | `settings.is_build_server_enabled` |
| `isPreserveRepositoryEnabled` | `settings.is_preserve_repository_enabled` | `isContainerLabelEscapeEnabled` | `settings.is_container_label_escape_enabled` |
| `isContainerLabelReadonlyEnabled` | `settings.is_container_label_readonly_enabled` | | |

Notes on `syncData(toModel: true)`:
- It runs `validate()` and a compose injection check.
- It throws `Argon2id needs a server that runs the Caddy proxy, version 2.11 or newer.` when argon2id is chosen and the server cannot do it.
- It assigns every attribute but **saves only `settings`** (with an audit entry `ui.application.settings_updated`). The caller saves the application.

### 3.2 Rules, messages, attributes (`General.php:161-304`)

Rules that matter for dockerimage. Every other rule still runs on the hidden properties, using their stored values.
- `name`: `required|string|min:3|max:255|regex:/^[\p{L}\p{M}\p{N}\s\-_.@\/&()#,:+]+$/u`
- `description`: `nullable|string|max:255|regex:/^[\p{L}\p{M}\p{N}\s\-_.,!?()\'\"+=*@\/&]+$/u`
- `fqdn`: `nullable` when unchanged from the model, else `applicationDomainRules()`.
- `gitRepository`: `required`. `gitBranch`: `required|string|ValidGitBranch`. Both are pre-filled by the create flow.
- `buildPack`: `required`. `staticImage`: `required|enum StaticImageTypes`. `baseDirectory`: `required` + directory regex.
- `portsExposes`: `nullable|string|regex:/^(\d+)(,\d+)*$/`. `portsMappings`: `ValidationPatterns::portMappingRules()`.
- `dockerRegistryImageName`: `nullable|string|max:255|regex:DOCKER_IMAGE_NAME_PATTERN`. `dockerRegistryImageTag`: `nullable|string|max:128|regex:DOCKER_IMAGE_TAG_PATTERN`.
- `customDockerRunOptions`: `nullable|string|max:2000|regex:SHELL_SAFE_COMMAND_PATTERN`. `preDeploymentCommand` and `postDeploymentCommand`: `nullable`.
- `customLabels`: `nullable`.
- `httpBasicAuthUsername`: `string|nullable`. `httpBasicAuthPassword`: `string|nullable|max:72`. `httpBasicAuthHashAlgorithm`: required enum.
- Cost ranges: bcrypt cost `4..14`, argon2id memory `8192..262144`, argon2id time `1..12`.
- `redirect`: `string|required`.
- Booleans: `boolean|required`.

Messages:
- `combinedMessages()`:
  - `name.regex`: `The name may only contain letters (including Unicode), numbers, spaces, and these characters: - _ . / @ & ( ) # , : +`
  - `name.min`: `The name must be at least :min characters.`
  - `name.max`: `The name may not be greater than :max characters.`
  - `description.regex`: `The description may only contain letters (including Unicode), numbers, spaces, and common punctuation: - _ . , ! ? ( ) ' " + = * / @ &`
  - `description.max`: `The description may not be greater than :max characters.`
- `name.required`: `The Name field is required.`
- `portsExposes.regex`: `Ports exposes must be a comma-separated list of port numbers (e.g. 3000,3001).`
- `portsMappings.regex`: `Port mappings must be a comma-separated list of port pairs or ranges with optional IP and protocol (e.g. 3000:3000, 8080:80/udp, 127.0.0.1:8080:80, [::1]::80).`
- `customDockerRunOptions.regex`: `The custom Docker run options contain invalid characters. Allowed: alphanumerics, && / || chaining, balanced quotes, globs (*, ?), !, and safe path/arg chars. Blocked: bare &, bare |, ;, $, backtick, (, ), <, >, \, newlines.`
- `redirect.required`: `The Redirect setting is required.` `redirect.string`: `The Redirect setting must be a string.`
- Boolean `.required` / `.boolean` messages follow the pattern `The {X} setting is required.` / `The {X} setting must be true or false.`
- Image fields have **no custom message**: the default is `The Docker registry image name field format is invalid.` (attribute names below).

Domain errors (`ValidationPatterns.php:584-656`), one per offending URL:
- `Invalid URL: {url}`
- `Invalid URL scheme: {scheme} for URL: {url}. Only http and https are supported.`
- `Invalid port for URL: {url}. The port must be between 1 and 65535.`
- `Invalid URL: {url}. The hostname must be a fully qualified domain name.`
- `The domains field must be a string.`

`validationAttributes` (`:266-304`):

| Property | Attribute name |
|---|---|
| `name` | `name` |
| `description` | `description` |
| `fqdn` | `FQDN` |
| `portsExposes` | `Ports exposes` |
| `portsMappings` | `Ports mappings` |
| `dockerRegistryImageName` | `Docker registry image name` |
| `dockerRegistryImageTag` | `Docker registry image tag` |
| `customLabels` | `Custom labels` |
| `customDockerRunOptions` | `Custom docker run commands` |
| `customNetworkAliases` | `Custom docker network aliases` |
| `redirect` | `Redirect` |
| `isContainerLabelEscapeEnabled` | `Is container label escape enabled` |
| `isContainerLabelReadonlyEnabled` | `Is container label readonly` |

Others follow the same style. `submit()` catches all exceptions, so validation failures appear as an `Error` toast (§0.1). Whether inline field errors also appear depends on Livewire's error bag; the toast is guaranteed.

### 3.3 `mount()` side effects (`:306-364`)
- `application.parse()` returns an empty collection for non-compose apps, so no toast.
- If `customLabels` is empty, the proxy is not `NONE` and `settings.is_container_label_readonly_enabled === true` (and the user can update), it generates labels with `generateLabelsApplication()` and saves `custom_labels` immediately.
- If status starts with `running` and `config_hash` is null, it dispatches `configurationChanged`.

### 3.4 Layout, section by section (dockerimage)

The root is an Alpine `x-data` with `canUpdate` (the `update` policy) and `shouldDisable()` (= `initLoadingCompose || !canUpdate`). Fields use `x-bind:disabled="!canUpdate"` or `shouldDisable()`.

The form is `<form wire:submit='submit' class="application-settings-form flex flex-col">`, followed by `x-unsaved-bar action="submit"` (`general.blade.php:10-11`). Its targets are:
```
name,description,buildPack,staticImage,baseDirectory,dockerComposeLocation,dockerComposeCustomBuildCommand,dockerComposeCustomStartCommand,watchPaths,dockerfileLocation,dockerfileTargetBuild,publishDirectory,installCommand,buildCommand,startCommand,customNginxConfiguration,dockerfile,dockerRegistryImageName,dockerRegistryImageTag,portsExposes,portsMappings,customNetworkAliases,customDockerRunOptions,httpBasicAuthUsername,httpBasicAuthPassword,preDeploymentCommand,preDeploymentCommandContainer,postDeploymentCommand,postDeploymentCommandContainer,isContainerLabelReadonlyEnabled,isContainerLabelEscapeEnabled,customLabels
```
There is **no visible Save button** on the main form. Saving goes through the unsaved bar or Enter.

Cards stack in `div.application-settings-grid flex flex-col gap-6`.

#### A. `application-details-section`, "Application details" (`:13-27`)
- Helper: `Name the application and choose the build strategy Coolify should use to deploy it.`
- Actions slot: `Reload compose`, compose only. **Hidden.**
- `grid gap-4`:
  - `name`, label `Name`, required.
  - `description`, label `Description`.

#### B. `access-section`, "Access" (`:29-175`)
- Helper: `Manage how this application is reached publicly, who can access it, and how it is reached from the Docker network.`
- **`#public-access-section`** (`border-b pb-5` for non-compose), `h3` `Public access`:
  - A clickable card (`rounded-lg border bg-neutral-50/60 px-4 py-3`) with a globe icon box. The whole card links to `/domains` (`wire:navigate`).
  - Primary line: the first trimmed domain of `fqdn`, or `No public domain configured`.
  - Secondary line:
    - `+{n} more domain` or `+{n} more domains` when extra domains exist;
    - `Make this application available from a URL` when there are 0 domains;
    - otherwise `Manage DNS checks and redirect settings`.
  - Right-side button (`a.button`, with an `arrow-right` icon): `Manage domains` when there is at least one domain, else `Add domain`. Its `aria-label` is `Manage application domains` or `Add an application domain`.
  - The domain count uses `countDomains($fqdn)`.
- **`#security-section`**, subsection "Authentication" (`first`, with `border-b py-5`):
  - If `settings.is_container_label_readonly_enabled == false`: `x-empty size=sm icon-name=admin`, title `Authentication is managed through labels`, description `Authentication is managed via manual proxy labels. Switch label management back to Coolify to configure it here.`, button `Go to Container labels` → `scrollToSettingsSection('container-labels-section')`.
  - Else, listbox `isHttpBasicAuthEnabled`:
    - label `Method`, `onChange="instantSave"`;
    - helper `HTTP Basic Authentication adds the required authentication labels to the proxy. Coolify currently supports a single username and password.`;
    - options `None` (false) and `HTTP Basic Authentication` (true).
  - When enabled, a 2-column grid:
    - `httpBasicAuthUsername`, label `Username`, required;
    - `httpBasicAuthPassword`, `type=password`, label `Password`, required.
  - Then `h3` `Password hashing` with a grid (3 columns when argon2id, else 2):
    - `httpBasicAuthHashAlgorithm`, label `Algorithm`, `onChange="instantSave"`.
      - Helper: `Algorithm that hashes the password for the proxy labels.<br><br>Traefik supports only bcrypt. Argon2id needs the Caddy proxy, version 2.11 or newer. With Argon2id, labels generated for Traefik use bcrypt.`
      - Option `bcrypt`, description `Works with every proxy`.
      - Option `Argon2id`, disabled unless `server.caddySupportsArgon2idBasicAuth()`. Description: `Memory-hard, more resistant to cracking` when supported; `Needs Caddy 2.11 or newer on this server` when the proxy is Caddy; otherwise `Only supported by the Caddy proxy`.
    - With argon2id:
      - `httpBasicAuthArgon2idMemoryCost`, label `Memory`, options `8 MiB`…`256 MiB` (values = MiB × 1024: 8, 16, 32, 64, 128, 256). Helper: `Memory that the proxy needs to check a password. More memory makes the hash harder to crack, but every login attempt that the proxy has not seen before uses this much memory on the server.`
      - `httpBasicAuthArgon2idTimeCost`, label `Iterations`, options 1–12. Helper: `Number of passes over the memory. More iterations make the hash harder to crack and a password check slower.`
    - With bcrypt: `httpBasicAuthBcryptCost`, label `Cost factor`, options 4–14. Helper: `Each step doubles the work to check or crack the password. Traefik checks the password on every request, so a higher cost makes every request slower.`
    - All of these use `onChange="instantSave"`.
- **`livewire:project.application.internal-access`** (`app/Livewire/Project/Application/InternalAccess.php` + view), `section#internal-access-section pt-5`, `h3` `Internal access`:
  - `wire:init="loadCurrentInternalHostname"` calls `getCurrentApplicationContainerStatus(server, app, 0)` and takes the first running container, else the first container. `Names` becomes the hostname.
  - Grid with 4 columns at `lg`:
    - `Internal hostname`: a copy-input with the hostname. When there is none, a read-only input showing `Loading…` until loaded, then `No deployed container found`.
    - `Docker network`: `destination.network`.
    - `Exposed ports`: `ports_exposes` split, trimmed and joined with `, `, or `None`.
    - `Network aliases`: `custom_network_aliases` joined with `, `, or `None`.
  - Footer (`border-t pt-4`):
    - text `Internal hostnames are only reachable by resources connected to this Docker network.`
    - button `Edit networking` → `$dispatch('open-networking-modal')`.
  - Listens to `applicationNetworkingUpdated` and refreshes the application.

#### C. `build-pipeline-section`, "Build & deploy" (`:177-473`)
- Helper: `How Coolify builds the application, where it pushes the image, and the commands it runs around each deployment.`
- **Hidden for dockerimage:**
  - build strategy listbox `buildPack` (Railpack, `Nixpacks (deprecated)`, Static, Dockerfile, Compose), condition `!dockerfile && build_pack !== 'dockerimage'`;
  - `staticImage` "Web server";
  - `siteType` "Site type", condition `could_set_build_commands()`, which is only nixpacks or railpack;
  - repository paths (Base directory, Dockerfile location, Publish directory);
  - watch paths;
  - install, build and start commands;
  - `dockerfileTargetBuild`;
  - `isBuildServerEnabled` "Builder selection";
  - custom Nginx configuration (`isStatic || static`);
  - compose blocks;
  - the Dockerfile textarea (only when `application.dockerfile` is set).
- Shown for dockerimage (`:211-212`): `<p class="text-sm text-neutral-500 dark:text-fg-dim">Nothing to build. This application deploys a prebuilt Docker image.</p>`
- **`#container-image-section`**, subsection titled `Image` (title `Image registry` for other packs), `grid gap-4 lg:grid-cols-2` (`:399-448`):
  - non-swarm:
    - `dockerRegistryImageName`, label `Image`, placeholder `nginx`, not marked required;
    - `dockerRegistryImageTag`, label `Tag`, placeholder `alpine`, helper `Enter a tag (e.g., 'latest', 'v1.2.3') or SHA256 hash (e.g., 'sha256-59e02939b1bf39f16c93138a28727aec520bb916da021180ae502c61626b3cf0')`.
  - swarm: the same, but `Image` is `required` (red `*`). The swarm registry notice is hidden for dockerimage.
- **`#deployment-lifecycle-section`**, subsection "Deployment commands", `grid sm:grid-cols-2`:
  - `preDeploymentCommand`, label `Pre-deployment`, placeholder `php artisan migrate`, helper `An optional script or command to execute in the existing container before the deployment begins.<br>It is always executed with 'sh -c', so you do not need add it manually.`
  - `postDeploymentCommand`, label `Post-deployment`, placeholder `php artisan migrate`, helper `An optional script or command to execute in the newly built container after the deployment completes.<br>It is always executed with 'sh -c', so you do not need add it manually.`
  - The `Container name` fields (`pre/postDeploymentCommandContainer`) are compose only. **Hidden.**

#### D. Networking modal, non-compose (`:475-590`)
- `x-modal-input`, title `Networking`, subtitle `Ports the container exposes, host port mappings and internal network aliases.`
- `wireOpen="networkingModalOpen"` entangles it with that property. The window event `open-networking-modal` opens it.
- Contains its own `<form wire:submit="submit">`.
- Detected PORT banners, only when `settings.is_container_label_readonly_enabled` and `detectPortFromEnvironment()` returns a port (`General.php:993-1015`):
  - `ports_exposes` empty → warning box, bold `PORT environment variable detected ({port})`, text `Your Ports Exposes field is empty. Consider setting it to <strong>{port}</strong> to ensure the proxy routes traffic correctly.`
  - port not in `ports_exposes` → warning box, bold `PORT mismatch detected`, text `Your PORT environment variable is set to <strong>{port}</strong>, but it's not in your Ports Exposes configuration. Ensure they match for proper proxy routing.`
  - match → blue info box, bold `PORT environment variable configured`, text `Your PORT environment variable ({port}) matches your Ports Exposes configuration.`
- When `portsExposes` is empty or `'0'` and `fqdn` is set: info callout titled `No ports exposed`, text `This application does not expose any ports and will not be reachable through the proxy or your domains. This behavior is normal for background workers, bots, or scheduled tasks. If your application needs to handle HTTP traffic, please specify the port(s) it listens on.`
- Grid `lg:grid-cols-[14rem_16rem_minmax(0,1fr)]`:
  - `portsExposes`, label `Ports exposes`, placeholder `3000,3001`. `{H}` below is `You can also set a different internal port for each domain on the <a class='underline dark:text-white' href='{domains url}'>Domains</a> page.`
    - If `settings.is_container_label_readonly_enabled === false`: read-only, helper `Readonly labels are disabled. You can set the ports manually in the labels section.<br><br>{H}`.
    - Else: editable, helper `A comma separated list of ports your application uses. The first port will be used as default healthcheck port if nothing defined in the Healthcheck menu. Be sure to set this correctly.<br><br>{H}`.
    - Below it: `You can also set an internal port per domain on <a>Domains</a>.`
  - `portsMappings` (non-swarm only), label `Port mappings`, placeholder `3000:3000`, helper `A comma separated list of ports you would like to map to the host system. Useful when you do not want to use domains.<br><br><span …>Format:</span> host:container<br><br><span …>Example:</span> 3000:3000,3002:3002<br><br>Rolling update is not supported if you have a port mapped to the host.`
  - `customNetworkAliases` (non-swarm only), label `Network aliases`, helper `A comma separated list of custom network aliases you would like to add for container in Docker network.<br><br><span …>Example:</span><br>api.internal,api.local`
- Footer: `Close` (closes the modal) and the highlighted submit `Save` (`canGate=update`).
- A successful `submit()` sets `networkingModalOpen = false`.

#### E. `container-section`, "Container", non-compose (`:592-638`)
- Helper: `Options and labels applied to the container when it starts.`
- `customDockerRunOptions`:
  - label `Custom Docker options`;
  - placeholder `--cap-add SYS_ADMIN --device=/dev/fuse --security-opt apparmor:unconfined --ulimit nofile=1024:1024 --tmpfs /run:rw,noexec,nosuid,size=65536k --hostname=myapp`;
  - helper `You can add custom docker run options that will be used when your container is started.<br>Note: Not all options are supported, as they could mess up Coolify's automation and could cause bad experience for users.<br><br>Check the <a class='underline dark:text-white' href='https://coolify.io/docs/knowledge-base/docker/custom-commands'>docs.</a>`
- **`#container-labels-section`**, subsection "Labels":
  - `isContainerLabelReadonlyEnabled`, label `Label management`, `onChange="instantSave"`. Helper: `When Coolify manages the labels, they are regenerated automatically and manual edits can be lost.<br><br>If you edit them yourself, be careful: a wrong label set can break the proxy configuration after a restart (you can always reset to the Coolify defaults).` Options: `Managed by Coolify (auto-generated)` (true) and `Managed manually (edit labels yourself)` (false).
  - `isContainerLabelEscapeEnabled`, label `Special characters`, `onChange="instantSave"`. Helper: `By default, $ (and other special characters) are escaped: writing $ saves it as $$.<br><br>Keep them unescaped if you want to use environment variables inside the labels.` Options: `Escape special characters ($ becomes $$)` (true) and `Keep unescaped (allow env variables)` (false).
  - `Active labels` label row, with a `Reset to defaults` modal-confirmation button on the right (only for users who can update):
    - title `Confirm Labels Reset to Coolify Defaults?`, `submitAction="resetDefaultLabels(true)"`;
    - actions `All your custom proxy labels will be lost.` and `Proxy labels (traefik, caddy, etc) will be reset to the coolify defaults.`;
    - `confirmationText="{fqdn}/"`, `confirmationLabel="Please confirm the execution of the actions by entering the Application URL below"`, `shortConfirmationLabel="Application URL"`;
    - no password step; `step2ButtonText="Permanently Reset Labels"`.
  - `customLabels` textarea, 15 rows, Monaco with language `ini`. Read-only and disabled when Coolify manages the labels; editable otherwise.

#### F. Domain conflict modal (`:643`, `resources/views/components/domain-conflict-modal.blade.php`)
- Shown when `showDomainConflictModal && count(domainConflicts) > 0`.
- Header `Domain already in use`.
- Danger callout titled `Domain conflict detected`: `The following domain(s) are already in use by other resources. Using the same domain for multiple resources can cause routing conflicts and unpredictable behavior.`
- One red item per conflict: `<strong>{domain}</strong> is used by {resource_name as link, or plain text for type instance} ({resource_type}[: {service_name}])`. Types are `application`, `service` and `instance` (`bootstrap/helpers/domains.php:118`).
- Warning callout titled `What will happen if you continue?`, with the list:
  - `Only one resource will be accessible at this domain`
  - `The routing behavior will be unpredictable`
  - `You may experience service disruptions`
  - `SSL certificates might not work correctly`
- Buttons: `Cancel` (sets `showDomainConflictModal=false`) and red `Proceed anyway` → `confirmDomainUsage()`, which sets `forceSaveDomains=true`, closes the modal and calls `submit()` again.

### 3.5 `submit($showToaster = true)` (`General.php:829-991`), in order
1. `authorize('update')`. Records the current managed DNS hostnames.
2. `resetErrorBag()`. `portsExposes` = spaces removed and trimmed, or null. `portsMappings` = spaces removed and trimmed.
3. `validate()` (all rules).
4. Snapshots the old `ports_exposes`, `custom_network_aliases`, `is_container_label_escape_enabled`, compose location and base dir.
5. If `fqdn` changed, `fqdn = normalizeApplicationDomains(fqdn)`. Normally it has not changed: General has no fqdn input, but the property is still bound.
6. `sslipDomainWarning(fqdn)`: true when any domain contains both `https` and `sslip`. Then it shows a warning toast with `__('warning.sslipdomain')` = `Your configuration is saved, but sslip domain with https is <span class='dark:text-red-500 text-red-500 font-bold'>NOT</span> recommended, because Let's Encrypt servers with this public domain are rate limited (SSL certificate validation will fail). <br><br>Use your own domain instead.` (`lang/en.json:49`), and **suppresses** the final success toast.
7. `syncData(toModel: true)` saves the settings.
8. If `redirect` is dirty, `setRedirect()` runs (redirect `www` without a www domain gives the error `You want to redirect to www, but you do not have a www domain set.<br><br>Please add www to your domain list ({dnsHint}).`; otherwise it saves, resets labels and shows the toast `Redirect updated.`). If `dockerfile` is dirty, it parses the healthcheck.
9. **`checkFqdns()`** (`:762-797`) runs on every save while `fqdn` is set:
   - With no additional servers, each domain goes through `validateDNSEntry`. That function always passes `sslip.io` hosts, and passes everything when `instance_settings.is_dns_validation_enabled` is off.
   - On failure, error toast titled `Validating DNS failed.` with the description `{guidance}<br><br>Check this <a target='_blank' class='underline dark:text-white' href='https://coolify.io/docs/knowledge-base/dns-configuration'>documentation</a> for further help.`, where `{guidance}` is `Required DNS record type {A|AAAA} pointing to {ip}` or `DNS validation failed. Check your DNS records.`
   - Then, unless `forceSaveDomains` is set, `checkDomainUsage(resource: app)`. Conflicts set `domainConflicts` and `showDomainConflictModal=true`, and **submit returns** before the application is saved. The settings were already saved in step 7.
   - Otherwise it writes `fqdn` back to the model and calls `resetDefaultLabels(false)`.
10. Base and publish directories lose their trailing `/`.
11. `application.save()`. If `customLabels` is empty, the proxy is not `NONE` and labels are **not** readonly, it generates and saves labels.
12. If ports or escape changed, `resetDefaultLabels()`. This only regenerates when labels are Coolify-managed, unless it is a manual reset.
13. **dockerimage only:** `validate(['dockerRegistryImageName' => required + pattern])`. This runs *after* the save, so an empty image name is saved and then reported as `The Docker registry image name field is required.` (toast).
14. Trims `customDockerRunOptions`. Writes `custom_labels = base64(customLabels)`, saves, refreshes, queues DNS release of removed hostnames, and runs `syncData()`.
15. If ports or aliases changed, dispatches `applicationNetworkingUpdated` to `InternalAccess`. Sets `networkingModalOpen=false`.
16. Success toast `Application settings updated!`, unless suppressed by the sslip warning.
17. On any exception: refresh, `syncData()`, `handleError` toast.
18. `finally`: dispatches `configurationChanged`, which refreshes the heading and the configuration checker.

**`instantSave()`** (`:515-562`). Used by every `onChange="instantSave"` listbox:
- `syncData(toModel)` saves the settings.
- The application row is saved **only** if basic auth on/off or the hash or cost columns changed. Any other attribute changes are discarded by the `refresh()` that follows.
- Toast `Settings saved.`
- If ports or escape changed, `resetDefaultLabels(false)`. If labels are readonly, `resetDefaultLabels(false)` again. If preserve-repository was turned off, file storages get `is_based_on_git=false`.
- Dispatches `configurationChanged`.

**`resetDefaultLabels($manualReset = false)`** (`:740-760`):
- Returns early unless labels are readonly (Coolify-managed) or this is a manual reset.
- `customLabels = implode("\n", generateLabelsApplication(app))`, saved base64 to `custom_labels`, refresh, sync, dispatch `configurationChanged`.
- No toast.

Other methods: `getWildcardDomain()` (toast `Wildcard domain generated.`) and `generateDomain()` (toast `Domain generated.`) still exist but are **not referenced** by `general.blade.php` for non-compose apps. Generating domains happens on the Domains page.

---

## 4. Deployments list: `app/Livewire/Project/Application/Deployment/Index.php` + `resources/views/livewire/project/application/deployment/index.blade.php`

Standalone page (`embedded=false`):
- Title `{app name limit 10} > Deployments | Coolify`.
- Shows the configuration checker and the heading (`wire:key="application-heading-deployment-index"`).
- Same workspace grid; the sidebar has `current-route="project.application.deployment.index"`, so "Deployment Logs" is active.

`mount()` (`Index.php:78-126`):
- Project missing → redirect to `dashboard`. Environment missing → 404. Application missing → `dashboard`.
- Query string `?pull_request_id=`:
  - a non-positive or non-integer value is cleared with the toast `Invalid Pull Request ID in URL. Filter cleared.`;
  - live edits that fail the same check give `Invalid Pull Request ID. Please enter a valid positive number.`
- `defaultTake = 10` (3 when embedded). `skip = 0`. Loads filter options and the first page.

Data (`Application::deployments()`, `app/Models/Application.php:1291-1406`):
- Rows from `application_deployment_queues` for the app.
- `pull_request_id` filter.
- Search is case-insensitive LIKE `%q%` on `deployment_uuid`, `commit`, `commit_message`, `server_name`, `status`. Extra matches:
  - aliases `success` → finished, `in progress` → in_progress, `cancelled` → cancelled-by-user;
  - a numeric value → that `pull_request_id`;
  - `pull request` / `pull requests` / `pr` → `pull_request_id > 0`;
  - `webhook(s)`, `rollback(s)`, `api`, `manual`.
- Filters: `status:{status}`, `source:{manual|pull-request|webhook|api|rollback}`, `server:{id}`. Values within one group are OR'ed; groups are AND'ed.
- Order: `created_at` then `id`, desc (`newest`) or asc (`oldest`). Then `skip` / `take`. Returns `count` too.

Filter options (`:293-368`), shown only for values present in this app's rows:
- Status: `Success` (finished), `Failed`, `In progress`, `Queued`, `Cancelled`.
- Source: `Manual`, `Pull requests`, `Webhooks`, `API`, `Rollbacks`.
- Server: `server_name`, or `Server #{id}`.
- Pull request: `Pull request #{n}`, taken from queue rows with PR > 0 and from previews. The first option is `All deployments`.

Toolbar (`x-table.toolbar`, `index.blade.php:51-181`):
- Search input (`x-table.search`): placeholder and aria-label `Search deployments`, `wire:model.live.debounce.300ms="search"`, a search icon that becomes a spinner while loading.
- `x-table.filter` button: text `Filter`, or `Pull request #{n}` when a PR filter is active, with a count pill. It is highlighted when active.
  - The panel has group headings `Status`, `Source`, `Server`, `Pull request`.
  - Each option is a checkbox row; a selected box is filled with the brand colour and shows a check.
  - Footer button `Reset filters` → `clearFilter`, disabled when nothing is active.
  - Toggling a status, source or server filter clears the PR filter, and picking a PR clears the other filters. Both reset to page 1.
- `x-table.sort` button `Sort` with options `Newest first` and `Oldest first`; the selected one shows a check.

Container: `x-application.settings-section` titled `Deployment history`, helper `Search, filter, and open a deployment to inspect its build logs.`, `flush`.

Table (CSS grid `.deployment-table-grid`, `resources/css/app.css:3602-3643`):
- Standalone columns `7.5rem | minmax(7rem,.8fr) | minmax(12rem,1.7fr) | minmax(8rem,.9fr) | 6.5rem | minmax(7rem,.8fr)`, min width 59rem below `lg` (horizontal scroll).
- Header: `Status`, `Source`, `Commit`, `Started`, `Duration`, `Server`.
- Per row:
  - **Status** = `x-status-badge`:

    | Status | Label | Type |
    |---|---|---|
    | `finished` | `Success` | success |
    | `in_progress` | `In progress` | warning |
    | `queued` | `Queued` | warning |
    | `failed` | `Failed` | error |
    | `cancelled-by-user` | `Cancelled` | neutral |
    | other | `Str::headline` | neutral |

  - **Source**, first match:
    - webhook with PR → `Webhook · PR #{n}`
    - webhook → `Webhook`
    - PR > 0 → `Pull request #{n}`
    - `rollback` → `Rollback`
    - `is_api` → `API`
    - otherwise `Manual`
  - **Commit**: when `commit` is set, a mono link with the first 7 chars to `gitCommitLink(commit)` (target `_blank`), then the first line of `commit_message`, truncated, with a title. Otherwise `-`.
  - **Started**: `Carbon::parse(created_at)->diffForHumans()`, for example `5 minutes ago`. Its title is `formatDateInServerTimezone(created_at, server)` = `Y-m-d H:i:s T` in `server.settings.server_timezone` (`bootstrap/helpers/timezone.php`).
  - **Duration**:
    - `queued` → `Waiting`;
    - `in_progress` → `calculateDuration(created_at, now)`;
    - otherwise `calculateDuration(created_at, finished_at)`, or `-`.
    - Format: `%dd %Hh %Im %Ss` when days > 0, `%Hh %Im %Ss` when hours > 0, else `%Im %Ss`, zero-padded (for example `01m 07s`).
  - **Server**: `server_name`, or the destination server name, or `-`.
  - The whole row links to `{index url}/{deployment_uuid}` with `wire:navigate`, using an invisible `a.deployment-row-link absolute inset-0`. The cells sit above it and only their own links take clicks.
  - The row has `data-table-row-active` when it matches `selectedDeploymentUuid`.
- While a page, filter or reset request is in flight, the table gets `opacity-50 pointer-events-none`. An overlay with `Filtering deployments...` covers filter changes.

Pagination (`resources/views/components/table-pagination.blade.php`):
- Footer text `{from}–{to} of {total}` (en dash).
- Page-size select (`resources/views/components/page-size-select.blade.php`): options 10, 25, 50, 100 and `Custom…` (number input 1–100, placeholder `1–100`).
  - A value over 100 gives the warning toast `Maximum is 100 rows per page.`
  - Shown as `{n} / page`. Saved to `localStorage['coolify.page-size.deployments']`.
  - It sets `defaultTake`, which is clamped to 1..100 and resets `skip`.
- Prev/next icon buttons (`Previous page` / `Next page`) call `previousPage` / `nextPage`. They are disabled on the first and last page.
- `goToPage(n)` clamps to `1..ceil(count/take)`. `skip = (page-1)*take`.
- The footer is hidden when `total <= 10`, because the page-size slot is present.

Polling: `wire:poll.5000ms="reloadDeployments"` **only while `skip == 0`** (first page). It also `$refresh`es on the team event `ServiceChecked`.

Empty state: `x-empty size=sm icon-name=layers`, title `No deployments found`. Description:
- `No deployments match the current search and filters.` when a search or filter is active;
- otherwise `Deploy the application to create its first deployment record.`

Embedded mode (inside the deployment show page, `embedded=true`):
- No title, heading or sidebar. `defaultTake=3`.
- Columns: `Source | Commit | Duration | Server | Status` (right-aligned).
- The status column shows the short absolute age (`diffForHumans(DIFF_ABSOLUTE, short)`, for example `5m`) and a coloured dot (`bg-emerald-500`, or `bg-warning` pulsing, `bg-red-500`, `bg-neutral-400`). Its title is `{statusLabel} · {server time}`.
- From 2xl up, only the Commit and Status columns show. The Commit cell gets a second line `{source} · {duration} · {server}`, and `fit()` sizes `defaultTake` to the available height.
- A plain left click on a row calls `$wire.$parent.selectDeployment(uuid)` without navigating, and shows a spinner in that row.
- No page-size select.

---

## 5. Deployment log page: `app/Livewire/Project/Application/Deployment/Show.php` + `resources/views/livewire/project/application/deployment/show.blade.php`

`mount()` (`Show.php:38-73`):
- Project missing → `dashboard`. Environment missing → 404. Application missing → `dashboard`.
- Deployment missing (by `deployment_uuid` + `application_id`) → redirect to the deployments index.
- `is_debug_enabled` = `false` for team members, else `settings.is_debug_enabled`.
- `isKeepAliveOn` = status is neither `finished` nor `failed`. Note that `cancelled-by-user` keeps polling.

Page:
- Title `{app name limit 10} > Deployment | Coolify`.
- Configuration checker and heading (`wire:key="application-heading-deployment-show"`).
- Sidebar with `current-route="project.application.deployment.show"`.
- Content column: at 2xl, `grid-cols-[30rem_minmax(0,1fr)]` = the embedded deployment history (§4), then the logs panel. Below 2xl they stack.
- `selectDeployment(uuid)` (`:78-101`):
  - swaps the shown deployment in place;
  - dispatches `deploymentSelected` to Index (row highlight);
  - calls `history.replaceState` to `/deployment/{uuid}`;
  - the logs panel has `wire:key="deployment-logs-{uuid}"`, so it remounts its Alpine state;
  - shows `opacity-60` while loading.

Logs panel (Alpine state `:19-285`):
- Panel height: `h-[calc(100dvh-8rem)] min-h-[32rem]`, `xl:h-[32rem]`, `2xl:h-[calc(100dvh-10rem)]`, sticky at 2xl.
- `#screen` gets class `fullscreen` when fullscreen; the property is entangled with Livewire `fullscreen`.
- Inner `div.logs-viewer` has `wire:poll.2000ms="polling"` **only when `isKeepAliveOn`**. It is `rounded-xl border shadow-sm` when not fullscreen. Background white, dark `bg-log`.

Toolbar (`.logs-viewer-toolbar`), left part (`.logs-viewer-primary`):
- `Follow Logs` icon button (down-arrow SVG), only while `isKeepAliveOn`. It toggles auto-scroll and gets `logs-viewer-btn-active` when on.
- `Fullscreen` / `Minimize` icon buttons.
- Status: a dot and a label.
  - Label: `In progress`, else `Str::headline(status)` (`Finished`, `Failed`, `Queued`, `Cancelled By User`).
  - Dot: `bg-emerald-500` for finished, `bg-red-500` for failed, `bg-warning motion-safe:animate-pulse` for queued and in_progress, `bg-neutral-400` otherwise.

Toolbar, right part (`.logs-viewer-end`):
1. **`livewire:project.application.deployment-navbar`** (`app/Livewire/Project/Application/DeploymentNavbar.php` + `resources/views/livewire/project/application/deployment-navbar.blade.php`). Hidden unless status is `queued` or `in_progress`.
   - When `queued`: button `Force start` → `force_start()`, `authorize('deploy')`, `force_start_deployment(queue)`. If the deployment was no longer queued: info toast `This deployment is no longer queued, so it was not started again.`
   - Red button `Cancel deployment` → `cancel()`. **No confirmation.**
     - `authorize('deploy')`.
     - Sets status `cancelled-by-user`.
     - `addLogEntry('Deployment cancelled by user.', 'stderr')`.
     - `CleanupCancelledDeployment::run(queue, teamId)`.
     - `finally`: `next_after_cancel(server, app)`.
     - There is no success toast. Errors go through `handleError`.
   - It listens for `deploymentFinished` and refreshes the queue row.
   - `show_debug()` and `copyLogsToClipboard()` exist but are not used by the current view.
2. Search box:
   - `input type=search`, placeholder and aria-label `Find in logs`, `x-model.debounce.300ms="searchQuery"`, search icon on the left.
   - A `<kbd>` shortcut hint `⌘F` (Mac) or `Ctrl+F` while empty; an × `Clear search` button while filled.
   - Ctrl/⌘+F focuses it, unless another input outside the panel has focus.
   - Filtering is client side over `[data-log-line]` by `data-log-content` (`{timestamp} {text}`):
     - lines that do not match get `hidden`;
     - matches are wrapped in `span.log-highlight`, skipped while the user has a text selection inside `#logs`;
     - the count shows as `{n} matches` in `.logs-viewer-meta`;
     - zero matches with a query shows `No matches found.`
   - The search is re-applied after every Livewire morph of `#logs`.
3. `View` dropdown (`x-table.dropdown`, button title `View Options`, sliders icon, chevron):
   - heading `Display`;
   - `Timestamps`, a client toggle `showTimestamps` that defaults to on and shows `✓`;
   - `Debug logs`, only for users who can update: `wire:click="toggleDebug"` flips `settings.is_debug_enabled` (with an audit entry) and refreshes the queue. It shows `✓` when on and a spinner while loading.
4. `Export` dropdown (button title `Copy or Download Logs`, download icon):
   - note `Copied and downloaded logs hide secrets.`
   - `Copy visible logs` → `$wire.exportLogs(showTimestamps, searchQuery)`, then the clipboard.
     - Toasts: success `Logs copied to clipboard.`; errors `Clipboard is not available. Please use HTTPS or localhost.`, `Failed to copy logs to clipboard.`, `Failed to prepare logs for export.`
   - `Download visible logs` → same content plus a trailing newline, file `deployment-{uuid}-{YYYY-MM-DD-HH-MM-SS}.txt` (UTC ISO with `T` and `:` replaced by `-`).
   - `Download full log (with debug)`, only for users who can update → `$wire.downloadAllLogs()`:
     - every line, with `[DEBUG] ` before hidden lines;
     - file `deployment-{uuid}-all-logs-{timestamp}.txt`, toast `All logs downloaded.`;
     - the label changes to a spinner and `Downloading...` while it runs.
   - `exportLogs` (`Show.php:151-166`):
     - each line becomes `{timestamp} {[CMD]: }{trimmed text}`, or the text alone without timestamps;
     - filtered by the case-insensitive search;
     - joined with `\n`;
     - passed through `sanitizeLogsForExport(lines, EnvironmentVariable::logRedactionSecretsFor(app, includePreview: PR != 0))`.

Log body (`#logsContainer` > `#logs`, `font-logs text-[11px] sm:text-xs leading-relaxed`, vertical scroll):
- Lines come from `decode_remote_command_output(queue)` (`bootstrap/helpers/remoteProcess.php:269-366`):
  - The input is `application_deployment_queues.logs`, a JSON array of `{command?, output, type: stdout|stderr, timestamp, hidden, batch, order}`. When decoding fails it retries with UTF-8 sanitizing, then gives `[]`.
  - Hidden entries are dropped unless debug is on. Debug is forced off for non-admins of the app's team.
  - Entries are sorted by `order`.
  - The timestamp is converted to the server timezone and formatted `Y-M-d H:i:s` (for example `2026-Oct-28 14:03:22`).
  - The first time a `command`+`batch` pair appears, a command line is added with `command:true`. Then `output` is split on newlines into one line each.
- Each line: `div[data-log-line].log-line.logs-viewer-line` (plus `mt-2` for command lines), then `span.logs-viewer-timestamp` (`x-show="showTimestamps"`), then `span.logs-viewer-line-text`. Text is `[CMD]: ` + the command for command lines, else the trimmed line.
- Line classes: `text-success dark:text-warning` when hidden (debug); `text-red-500` when stderr; `font-bold` when it is a command.
- Empty: `No logs yet.`

Auto-scroll:
- `alwaysScroll` starts true while `isKeepAliveOn`. A requestAnimationFrame loop scrolls to the bottom every 250 ms.
- Following turns off on: wheel up, touch dragging down, ArrowUp / PageUp / Home, or the Follow button (which also blocks auto-resume).
- Scrolling back to within 10 px of the bottom re-enables it (150 ms debounce), unless it was turned off manually.
- After each `#logs` morph it scrolls to the bottom when following.
- `polling()` (`Show.php:130-141`) refreshes the queue row. When the deployment stops being keep-alive, it dispatches `deploymentFinished` once. The panel then stops following 500 ms later, and the navbar refreshes.
