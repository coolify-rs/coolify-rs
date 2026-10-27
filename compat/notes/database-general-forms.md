# Database "General" forms: upstream spec (Redis, KeyDB, Dragonfly, MongoDB, MySQL, MariaDB, ClickHouse)

Source: `.upstream/snapshot`
- Views: `resources/views/livewire/project/database/<t>/general.blade.php`
- Components: `app/Livewire/Project/Database/<T>/General.php`
- Connection block: `app/Livewire/Project/Database/<T>/StatusInfo.php` (all use trait
  `app/Traits/HasDatabaseStatusInfo.php`, view `resources/views/livewire/project/database/status-info.blade.php`,
  which renders component `resources/views/components/database-status-info.blade.php`)

Baseline = PostgreSQL (`postgresql/general.blade.php` + `Postgresql/General.php`), already ported.
Everything below is "same as PostgreSQL" unless listed. Strings are verbatim; multi-line callout bodies
are written on one line (Blade source wraps them; rendered HTML whitespace collapses to one space).

---

## 0. Shared skeleton (identical in all 8 types, including PostgreSQL)

These parts are byte-identical across all types except for the noted per-type tokens:

- `<x-unsaved-bar action="submit" />` at top of form.
- Section `database-details-section`, title `Database details`, description
  `Manage the identity, container image, and credentials of this <Label> database.`
  (`<Label>` = `PostgreSQL` / `Redis` / `KeyDB` / `Dragonfly` / `MongoDB` / `MySQL` / `MariaDB` / `ClickHouse`).
  Actions slot: `x-modal-input title="Resource details" buttonTitle="Details"` with `livewire:project.shared.resource-details`.
  - `Name` (`id="name"`), `Description` (`id="description"`) in 2-col grid; `Image` (`id="image"`, `required`) spanning 2 cols.
    All `canGate="update" :canResource="$database"`.
  - Image helper: per type (see each type; Dragonfly has **no** helper).
- Section `access-section`, title `Access`, description
  `How applications and clients connect to this database, and whether it is reachable from the internet.`
  - Actions slot: when `$isPublic`, `x-process-dialog` with title `Proxy logs`, `livewire:project.shared.get-logs`
    (container `<uuid>-proxy`, `:collapsible="false" lazy`), button `View logs`.
  - Subsection `connection-section`, title `Connection`, helper `Use these URLs to connect to the database.`, `first`
    → `livewire:project.database.<t>.status-info :database="$database"`.
  - Subsection `public-access-section`, title `Public access`, helper `Expose this database through the managed TCP proxy.`,
    `class="relative"`, with `<x-table.loading target="instantSave" text="Updating public access..." />`:
    - `x-forms.listbox id="isPublic" label="Access" live onChange="instantSave"` (wrapped in
      `wire:key="public-access-{{ $publicPort ?: 'unset' }}"`), disabled when user cannot `update`; options:
      `Private` (false) / `Public through TCP proxy` (true) — when `blank($publicPort)` label is
      `Public through TCP proxy (set public port first)` and option disabled.
    - `publicPort`: `type="number"`, label `Public port`, placeholder = default port, `disabled="{{ $isPublic }}"`.
    - `publicPortTimeout`: `type="number"`, placeholder `3600`, label `Proxy timeout`, helper
      `Timeout in seconds. The default is 3600.`, `disabled="{{ $isPublic }}"`.
    - `portsMappings`: placeholder `3000:<port>`, label `Port mappings`, helper
      `Comma-separated host-to-container mappings, for example 3000:<port>.`
- Section `container-section`, title `Container`; description is
  `Docker options, configuration, and log forwarding for the database container.` (types with a Configuration subsection)
  or `Docker options and log forwarding for the database container.` (Dragonfly, ClickHouse).
  - `customDockerRunOptions`: label `Custom Docker options`, placeholder `--cap-add SYS_ADMIN --device=/dev/fuse`, helper
    `Add supported docker run options used when the container starts. Unsupported options can interfere with Coolify automation.`
  - Optional subsection `configuration-section` (title `Configuration`) — per type.
  - Subsection `log-delivery-section`, title `Log delivery`, helper `Forward container logs to the drain configured on the server.`:
    `x-forms.listbox id="isLogDrainEnabled" label="Log drain" live onChange="instantSaveAdvanced"`, disabled when user cannot
    `update`; options `Do not forward logs` (false) / `Forward logs to the server drain` (true).
- "Hidden for member" replacement field everywhere: `<x-forms.input label="…" disabled value="Hidden (only admins can view)" />`
  (no id, no helper).

### Global differences from PostgreSQL that apply to ALL seven types
1. Root div class is `application-settings-form` (PostgreSQL: `application-settings-form flex flex-col gap-6`, because PG has a
   second section outside the `<form>`).
2. **No** `initialization-section` subsection and **no** `initialization-scripts-section` (init scripts are PostgreSQL-only).
   No `new_filename`/`new_content`/`initScripts` props, no `save_init_script`/`delete_init_script`/`save_new_init_script`.
3. The `isPublic` listbox has **no** `:onChangeArgs="[]"`, and `instantSave()` takes **no** `?bool $isPublic` parameter
   (PG: `instantSave(?bool $isPublic = null)` which first assigns it). Otherwise `instantSave` is identical to PG
   (same two guard errors `Public port is required.` / `Database must be started to be publicly accessible.`, same success
   messages `Database is now publicly accessible.` / `Database is no longer publicly accessible.`, dispatch `databaseUpdated`,
   catch: re-authorize, flip `isPublic`, `syncData(true)`, `handleError`).
4. `instantSaveAdvanced()` identical to PG in all types (error `Log drain is not enabled on the server. Please enable it first.`
   and resets `isLogDrainEnabled=false`; on success two toasts `Database updated.` then
   `You need to restart the service for the changes to take effect.`).
5. `mount()` identical to PG (authorize `view`, `syncData()`, set `server = destination.server`; if missing dispatch error
   `Database destination server is not configured.` and return; after try:
   `isPasswordHiddenForMember = ! (user can 'update' database)`). Exceptions: ClickHouse also sets `anonymousDataVolume`.
   (PG/Mongo/MySQL/MariaDB catch `Exception`; Redis/KeyDB/Dragonfly/ClickHouse catch `\Throwable` — no behavioural difference
   for a port.) Note the early `return` on missing server skips setting `isPasswordHiddenForMember` (stays `false`) in every type.
6. Default-port tokens: Redis/KeyDB/Dragonfly `6379`, MongoDB `27017`, MySQL/MariaDB `3306`, ClickHouse `8123` (PG `5432`).

Common rules (same in every type, same as PG): `name` → `ValidationPatterns::nameRules()`
(`required|string|min:3|max:255|regex:NAME_PATTERN`), `description` → `descriptionRules()`
(`nullable|string|max:255|regex:DESCRIPTION_PATTERN`), `portsMappings` → `portMappingRules()`
(`nullable|string|regex:PORT_MAPPINGS_PATTERN`), `isPublic` `nullable|boolean`, `publicPort` `nullable|integer|min:1|max:65535`,
`publicPortTimeout` `nullable|integer|min:1`, `isLogDrainEnabled` `nullable|boolean`.
`ValidationPatterns::databaseIdentifierRules(enforcePattern: X)` = `['required','string','min:1','max:63']` + `'regex:DB_IDENTIFIER_PATTERN'` only if X.
`ValidationPatterns::databasePasswordRules(enforcePattern: X)` = `['required','string','min:1','max:128']` + `'regex:DB_PASSWORD_PATTERN'` only if X.
In every type, X = "the Livewire value differs from the current model attribute" (same conditional-regex pattern as PG).

Common messages (all types): `array_merge(ValidationPatterns::combinedMessages(), ValidationPatterns::portMappingMessages(), [...])`.
Per-field generated messages:
- `databaseIdentifierMessages(field, Label)`: `field.regex` = `The {Label} may only contain letters, digits, and underscores, and must start with a letter or underscore.`;
  `field.min` = `The {Label} must be at least :min character.`; `field.max` = `The {Label} may not be greater than :max characters.`
- `databasePasswordMessages(field, Label)`: `field.regex` = `The {Label} may not contain shell-unsafe characters (backtick, $, ;, |, &, <, >, \, quotes, spaces, or control characters).`;
  `field.min` / `field.max` as above.
- Fixed strings used below: `'name.required' => 'The Name field is required.'`, `'image.required' => 'The Docker Image field is required.'`,
  `'image.string' => 'The Docker Image must be a string.'`, `'publicPort.integer' => 'The Public Port must be an integer.'`,
  `'publicPort.min' => 'The Public Port must be at least 1.'`, `'publicPort.max' => 'The Public Port must not exceed 65535.'`,
  `'publicPortTimeout.integer' => 'The Public Port Timeout must be an integer.'`, `'publicPortTimeout.min' => 'The Public Port Timeout must be at least 1.'`

### Common `submit()` skeleton (PG) and which types deviate
PG: authorize `update` → strip all spaces from `portsMappings` if truthy (`str()->replace(' ', '')->trim()`) → if
`str(publicPort)->isEmpty()` set `publicPort = null` → `syncData(true)` (validate + write columns + `save()`) → toasts
`success: Database updated.` + dispatch `databaseUpdated`; catch → `handleError`; **finally**: if `config_hash` is null
call `$database->isConfigurationChanged(true)` else dispatch `configurationChanged`.
`syncData(true)` always writes `public_port = publicPort ?: null`, `public_port_timeout = publicPortTimeout ?: null`.

| Type | authorize ability in submit | publicPort empty→null | extra normalisation | finally config_hash block | other |
|---|---|---|---|---|---|
| Redis | `manageEnvironment` | **no** | — | **no** (finally dispatches `refreshEnvs` to `EnvironmentVariable\All`) | writes env vars after save |
| KeyDB | `manageEnvironment` | yes | — | yes | password only before first start |
| Dragonfly | `update` | yes | — | yes | password only before first start |
| MongoDB | `update` | yes | `mongoConf` empty → `null` | yes | — |
| MySQL | `update` | yes | — | yes | — |
| MariaDB | `update` | yes | — | yes | — |
| ClickHouse | `update` | yes | — | yes | user+password only before first start |

### Listeners
- PG: `save_init_script`, `delete_init_script`.
- Redis: `protected $listeners = ['envsUpdated' => 'refresh']`.
- KeyDB, Dragonfly, ClickHouse: `use ListensToTeamChannel`; `getListeners()` = `teamChannelListeners(['DatabaseProxyStopped' => 'databaseProxyStopped'])`.
  `databaseProxyStopped()`: `$database->refresh()`, reassign `isPublic`, `publicPort`, `publicPortTimeout` from model, dispatch `databaseUpdated`.
- MongoDB, MySQL, MariaDB: none.
- All have `refresh()` = `$database->refresh(); syncData();`.

---

## 1. Redis (`Redis/General.php`, `redis/general.blade.php`)

### Section structure
1. `database-details-section` — description `Manage the identity, container image, and credentials of this Redis database.`
   - Image helper: `Use a published Redis image from Docker Hub.`
   - Subsection `credentials-section`, title `Credentials`, helper `Keep these values aligned with the credentials configured inside Redis.`
   - (no initialization subsection)
2. `access-section` (status-info `livewire:project.database.redis.status-info`; port tokens `6379`).
3. `container-section` — description `Docker options, configuration, and log forwarding for the database container.`
   - Subsection `configuration-section`, title `Configuration`, helper `Override only the Redis directives you need. All other defaults remain active.`
   - Subsection `log-delivery-section`.

### Callout (credentials) — ALWAYS shown (unlike PG)
`<x-callout type="warning" title="…">`:
- title: started_at ? `Keep credentials synchronized` : `Verify the initial credentials`
- body: started_at ? `Changing values here does not update Redis. Update Redis first, then synchronize the values here so automations continue working.`
  : `These values can only be changed here before the first start.`
- Grid below always has class `mt-4 grid gap-4 lg:grid-cols-2` (not conditional).

### Fields
| Livewire prop | Storage | Label | Type | Placeholder | Helper | required / readonly / disabled |
|---|---|---|---|---|---|---|
| `redisUsername` (`string`) | env var `REDIS_USERNAME` (read via model accessor `redis_username`, which returns the env value or `'default'` if blank) | `Username` | text | — | started_at ? `You can only change this in the database.` : `Shared REDIS_USERNAME values make this field read-only.` | Rendered **only if** `version_compare($redisVersion, '6.0', '>=')`. `:required="!$database->started_at"`; `:disabled="!$database->started_at && $this->isSharedVariable('REDIS_USERNAME')"`. Not readonly when started. canGate update. |
| `redisPassword` (`?string`) | env var `REDIS_PASSWORD` (accessor `redis_password` reads env value or null) | `Password` | `password` | — | started_at ? `You can only change this in the database.` : `Shared REDIS_PASSWORD values make this field read-only.` | Three-way branch: (a) `$isPasswordHiddenForMember` → `label="Password" disabled value="Hidden (only admins can view)"`; (b) `elseif ($isPasswordLocked)` → `label="Password" disabled value="Locked (shown once)" helper="REDIS_PASSWORD is locked. Its value is never shown again."` (no id); (c) else real input with `:required="!$database->started_at"`, `:disabled="!$database->started_at && $this->isSharedVariable('REDIS_PASSWORD')"`. Not readonly when started. |
| `redisConf` (`?string`) | column `redis_conf` | `Custom Redis configuration` | `x-forms.textarea`, `rows="10"` | see below | `Coolify automatically applies requirepass using the password above. If you override requirepass here, keep both values identical.` | If `$isPasswordHiddenForMember` → `x-forms.input label="Custom Redis configuration" disabled value="Hidden (only admins can view)"`. |
| `customDockerRunOptions`, `isPublic`, `publicPort`, `publicPortTimeout`, `portsMappings`, `isLogDrainEnabled`, `name`, `description`, `image` | columns as PG | — | — | — | — | as shared skeleton |

`redisConf` placeholder is a multi-line attribute, verbatim bytes (note the 4 leading spaces on lines 2 and 3, they come from Blade indentation and are part of the attribute value):
```
# maxmemory 256mb
    # maxmemory-policy allkeys-lru
    # timeout 300
```
i.e. `"# maxmemory 256mb\n    # maxmemory-policy allkeys-lru\n    # timeout 300"`.

`isSharedVariable($name)` = `runtime_environment_variables()->where('key', $name)->where('is_shared', true)->exists()`.

### Extra public props / mount-computed flags
- `public string $redisVersion` = `$database->getRedisVersion()` (set in `syncData()` read branch) = the part after the first `:` in
  `image` (`explode(':', image)[1] ?? '0.0'`). Compared with `version_compare(..., '6.0', '>=')`. (So `redis:7.2` → `7.2`; `redis` → `0.0`; `redis:alpine` → `alpine`, which compares < 6.0.)
- `public bool $isPasswordLocked` = `$database->isCredentialLocked()` (read branch) = `(bool) runtimeEnvironmentVariable('REDIS_PASSWORD')?->isLockedForLogs()`
  (`isLockedForLogs`: true if `is_shown_once`, or if the value references locked shared variables).
- `isPasswordHiddenForMember` as shared.
- Read-branch `syncData()` order: name, description, image, portsMappings, isPublic, publicPort, publicPortTimeout, isLogDrainEnabled,
  customDockerRunOptions, redisVersion, redisUsername (= `redis_username`), `$canSeeCredentials = user can update`,
  `redisConf = canSee ? redis_conf : null`, `isPasswordLocked = isCredentialLocked()`,
  `redisPassword = canSee && !isPasswordLocked ? redis_password : null`.

### Validation — `rules()` (in order)
```php
'name' => ValidationPatterns::nameRules(),
'description' => ValidationPatterns::descriptionRules(),
'redisConf' => 'nullable',
'image' => 'required',
'portsMappings' => ValidationPatterns::portMappingRules(),
'isPublic' => 'nullable|boolean',
'publicPort' => 'nullable|integer|min:1|max:65535',
'publicPortTimeout' => 'nullable|integer|min:1',
'isLogDrainEnabled' => 'nullable|boolean',
'customDockerRunOptions' => 'nullable',
'redisUsername' => ValidationPatterns::databaseIdentifierRules(
    enforcePattern: $this->redisUsername !== $this->database->redis_username,
),
'redisPassword' => $this->database->isCredentialLocked()
    ? ['nullable']
    : ValidationPatterns::databasePasswordRules(
        enforcePattern: $this->redisPassword !== $this->database->redis_password,
    ),
```
Notes: `redisUsername` is validated (required) even when the field is not rendered (Redis < 6); the accessor default `'default'`
satisfies it. `redisPassword` is required unless the credential is locked (then only `nullable`).

### `messages()`
```php
array_merge(combinedMessages(), portMappingMessages(), [
    'name.required' => 'The Name field is required.',
    'image.required' => 'The Docker Image field is required.',
    'publicPort.integer' => 'The Public Port must be an integer.',
    'publicPort.min' => 'The Public Port must be at least 1.',
    'publicPort.max' => 'The Public Port must not exceed 65535.',
    'publicPortTimeout.integer' => 'The Public Port Timeout must be an integer.',
    'publicPortTimeout.min' => 'The Public Port Timeout must be at least 1.',
    ...databaseIdentifierMessages('redisUsername', 'Redis Username'),
    ...databasePasswordMessages('redisPassword', 'Redis Password'),
])
```

### `$validationAttributes`
```php
'name' => 'Name', 'description' => 'Description', 'redisConf' => 'Redis Configuration', 'image' => 'Image',
'portsMappings' => 'Port Mapping', 'isPublic' => 'Is Public', 'publicPort' => 'Public Port',
'publicPortTimeout' => 'Public Port Timeout', 'customDockerRunOptions' => 'Custom Docker Options',
'redisUsername' => 'Redis Username', 'redisPassword' => 'Redis Password',
```
(note `Custom Docker Options`, PG says `Custom Docker Run Options`)

### `syncData(true)` writes (in order)
validate → `name`, `description`, `redis_conf`, `image`, `ports_mappings`, `is_public`, `public_port = publicPort ?: null`,
`public_port_timeout = publicPortTimeout ?: null`, `is_log_drain_enabled`, `custom_docker_run_options` → `save()`.
No credential columns are written here.

### `submit()`
1. `authorize('manageEnvironment', $database)` (not `update`).
2. If `portsMappings` truthy: strip spaces + trim.
3. **No** publicPort empty→null step (but `syncData` still writes `publicPort ?: null`).
4. `syncData(true)`.
5. If `version_compare($redisVersion, '6.0', '>=')`: `runtime_environment_variables()->updateOrCreate(['key' => 'REDIS_USERNAME'], ['value' => $redisUsername, 'resourceable_id' => $database->id])`.
6. If `! $database->isCredentialLocked()` (re-evaluated at submit time): `updateOrCreate(['key' => 'REDIS_PASSWORD'], ['value' => $redisPassword, 'resourceable_id' => $database->id])`.
   (Comment: "The form never loads a locked password, so saving its empty value would overwrite it.")
   Env vars are written regardless of `started_at` (the UI only marks them "You can only change this in the database." after start but does not make them readonly).
7. Toast `success: Database updated.`; dispatch `databaseUpdated`.
8. catch → `handleError`. **finally**: `dispatch('refreshEnvs')->to(App\Livewire\Project\Shared\EnvironmentVariable\All::class)`.
   **No** `config_hash` / `isConfigurationChanged` / `configurationChanged` handling at all.

`instantSave` / `instantSaveAdvanced` call `syncData(true)` (columns only; do NOT write env vars).

---

## 2. KeyDB (`Keydb/General.php`, `keydb/general.blade.php`)

### Section structure
1. `database-details-section` — description `Manage the identity, container image, and credentials of this KeyDB database.`
   - Image helper: `Use a published eqalpha/keydb image from Docker Hub.`
   - Subsection `credentials-section`, title `Credentials`, helper `Keep this value aligned with the password configured inside KeyDB.` (singular "this value"/"password")
2. `access-section` (status-info `livewire:project.database.keydb.status-info`; port tokens `6379`).
3. `container-section` — description `Docker options, configuration, and log forwarding for the database container.`
   - `configuration-section`, title `Configuration`, helper `Override only the KeyDB directives you need. All other defaults remain active.`
   - `log-delivery-section`.

### Credentials subsection layout (only a password; no username, no database)
- If `started_at`:
  - no callout;
  - hidden-for-member → `label="Password" disabled value="Hidden (only admins can view)"`;
  - else `x-forms.input label="Password" id="keydbPassword" type="password" required readonly helper="You can only change this in the database."` (canGate update). Not wrapped in a grid/div.
- Else (not started):
  - Callout `type="warning" title="Verify the initial credentials"`, body
    `You can only change this password here before the first start. Later changes must be made inside the database.`
  - then `<div class="mt-4">` containing: hidden-for-member → disabled `Password` placeholder field; else
    `label="Password" id="keydbPassword" type="password" required` (no helper, not readonly).

### Configuration subsection
- hidden-for-member → `x-forms.input label="Custom KeyDB configuration" disabled value="Hidden (only admins can view)"`.
- else `x-forms.textarea label="Custom KeyDB configuration" rows="10" id="keydbConf"`, no placeholder, helper (raw HTML, verbatim):
  `<a target='_blank' class='underline' href='https://raw.githubusercontent.com/Snapchat/KeyDB/unstable/keydb.conf'>View the default KeyDB configuration</a>`

### Fields → storage
| Prop | Column | Notes |
|---|---|---|
| `keydbPassword` (`string`) | column `keydb_password` (encrypted cast) | NOT an env var in the form. Written only when `! $database->started_at`. Model `saving` hook: if existing row and `keydb_password` dirty → `legacy_password_quoting = false`. |
| `keydbConf` (`?string`) | `keydb_conf` | |
| others | as PG | |

No `isPasswordLocked`, no `isSharedVariable`, no REDIS_PASSWORD writes in the General form (even though the model has
`isCredentialLocked()` based on env var `REDIS_PASSWORD` — that only affects the status-info block, see §8).

### mount / read-branch syncData
Order: name, description, `$canSeeCredentials`, `keydbConf = canSee ? keydb_conf : null`, `keydbPassword = canSee ? keydb_password : ''`,
image, portsMappings, isPublic, publicPort, publicPortTimeout, customDockerRunOptions, isLogDrainEnabled.

### `rules()` (in order)
```php
'name' => ValidationPatterns::nameRules(),
'description' => ValidationPatterns::descriptionRules(),
'keydbConf' => 'nullable|string',
'keydbPassword' => ValidationPatterns::databasePasswordRules(
    enforcePattern: $this->keydbPassword !== $this->database->keydb_password,
),
'image' => 'required|string',
'portsMappings' => ValidationPatterns::portMappingRules(),
'isPublic' => 'nullable|boolean',
'publicPort' => 'nullable|integer|min:1|max:65535',
'publicPortTimeout' => 'nullable|integer|min:1',
'customDockerRunOptions' => 'nullable|string',
'isLogDrainEnabled' => 'nullable|boolean',
```
Note: password is always `required` (also after start, where it is readonly and not saved).

### `messages()`
```php
array_merge(combinedMessages(), portMappingMessages(), [
    ...databasePasswordMessages('keydbPassword', 'KeyDB Password'),
    'image.required' => 'The Docker Image field is required.',
    'image.string' => 'The Docker Image must be a string.',
    'publicPort.integer' => 'The Public Port must be an integer.',
    'publicPort.min' => 'The Public Port must be at least 1.',
    'publicPort.max' => 'The Public Port must not exceed 65535.',
    'publicPortTimeout.integer' => 'The Public Port Timeout must be an integer.',
    'publicPortTimeout.min' => 'The Public Port Timeout must be at least 1.',
])
```
No `name.required` override. **No `$validationAttributes`** (Laravel default attribute names apply, e.g. "keydb password").

### `syncData(true)` writes (in order)
validate → `name`, `description`, `keydb_conf`, [`keydb_password` only if `! started_at`], `image`, `ports_mappings`, `is_public`,
`public_port ?: null`, `public_port_timeout ?: null`, `custom_docker_run_options`, `is_log_drain_enabled` → `save()`.
(Comment: "After the first start the password is read-only, so autofilled values must not be saved.")

### `submit()`
`authorize('manageEnvironment')` → strip spaces in portsMappings → publicPort empty→null → `syncData(true)` → `Database updated.` +
`databaseUpdated` → finally config_hash block (same as PG).

---

## 3. Dragonfly (`Dragonfly/General.php`, `dragonfly/general.blade.php`)

Identical to KeyDB except:

- Section description `Manage the identity, container image, and credentials of this Dragonfly database.`
- **Image input has no helper** (`<x-forms.input label="Image" id="image" required canGate="update" :canResource="$database" />`).
- Credentials subsection helper: `Keep these values aligned with the credentials configured inside Dragonfly.` (plural, unlike KeyDB)
- Password prop `dragonflyPassword` → column `dragonfly_password` (encrypted; same `legacy_password_quoting = false` saving hook).
  Same started/not-started layout and the same callout title/body as KeyDB:
  `Verify the initial credentials` / `You can only change this password here before the first start. Later changes must be made inside the database.`
  Started: `label="Password" id="dragonflyPassword" type="password" required readonly helper="You can only change this in the database."`.
- Status-info `livewire:project.database.dragonfly.status-info`; port tokens `6379`.
- `container-section` description `Docker options and log forwarding for the database container.` and **no `configuration-section`**
  (no conf prop).
- `submit()` authorizes `update` (KeyDB: `manageEnvironment`).
- read-branch syncData order: name, description, canSee, `dragonflyPassword = canSee ? dragonfly_password : ''`, image, portsMappings,
  isPublic, publicPort, publicPortTimeout, customDockerRunOptions, isLogDrainEnabled.

`rules()`:
```php
'name' => ValidationPatterns::nameRules(),
'description' => ValidationPatterns::descriptionRules(),
'dragonflyPassword' => ValidationPatterns::databasePasswordRules(
    enforcePattern: $this->dragonflyPassword !== $this->database->dragonfly_password,
),
'image' => 'required|string',
'portsMappings' => ValidationPatterns::portMappingRules(),
'isPublic' => 'nullable|boolean',
'publicPort' => 'nullable|integer|min:1|max:65535',
'publicPortTimeout' => 'nullable|integer|min:1',
'customDockerRunOptions' => 'nullable|string',
'isLogDrainEnabled' => 'nullable|boolean',
```
`messages()`: same as KeyDB with `...databasePasswordMessages('dragonflyPassword', 'Dragonfly Password')` first. No `$validationAttributes`.

`syncData(true)`: name, description, [`dragonfly_password` only if `! started_at`], image, ports_mappings, is_public, public_port ?: null,
public_port_timeout ?: null, custom_docker_run_options, is_log_drain_enabled → save.

---

## 4. MongoDB (`Mongodb/General.php`, `mongodb/general.blade.php`)

### Section structure
1. `database-details-section` — `Manage the identity, container image, and credentials of this MongoDB database.`
   - Image helper `Use a published MongoDB image from Docker Hub.`
   - `credentials-section`, helper `Keep these values aligned with the credentials configured inside MongoDB.`
   - no initialization subsection.
2. `access-section` (status-info `livewire:project.database.mongodb.status-info`; port tokens `27017`).
3. `container-section` — `Docker options, configuration, and log forwarding for the database container.`
   - `configuration-section`, helper `Override the MongoDB configuration used by this container.`
   - `log-delivery-section`.

### Callout — same condition as PG (only when `started_at`)
title `Keep credentials synchronized`, body
`Changing values here does not update MongoDB. Update MongoDB first, then synchronize the values here so backups and other automations continue working.`
Grid class `{{ started_at ? 'mt-4 ' : '' }}grid gap-4 lg:grid-cols-2` (same as PG).

### Fields
| Prop | Column | Label | Type | Placeholder | Helper | Conditions |
|---|---|---|---|---|---|---|
| `mongoInitdbRootUsername` (`string`) | `mongo_initdb_root_username` | started_at ? `Initial username` : `Username` | text | `If empty: root` | — | `:required="!$database->started_at"`; not readonly |
| `mongoInitdbRootPassword` (`string`) | `mongo_initdb_root_password` | started_at ? `Initial password` : `Password` | `password` | — | — | `required` always; hidden-for-member → `label="Password" disabled value="Hidden (only admins can view)"` (label is always `Password` in hidden variant) |
| `mongoInitdbDatabase` (`string`) | `mongo_initdb_database` | started_at ? `Initial database` : `Database` | text | `If empty, it will match the username.` | started_at ? `You can only change this in the database.` : null | `:readonly="(bool) started_at"`, `:required="!started_at"` |
| `mongoConf` (`?string`) | `mongo_conf` | `Custom MongoDB configuration` | textarea rows=10 | — | — | hidden-for-member → disabled input `Custom MongoDB configuration` / `Hidden (only admins can view)` |

### read-branch syncData
name, description, canSee, `mongoConf = canSee ? mongo_conf : null`, `mongoInitdbRootUsername`, `mongoInitdbRootPassword = canSee ? … : ''`,
`mongoInitdbDatabase`, image, portsMappings, isPublic, publicPort, publicPortTimeout, isLogDrainEnabled, customDockerRunOptions.

### `rules()` (in order)
```php
'name' => ValidationPatterns::nameRules(),
'description' => ValidationPatterns::descriptionRules(),
'mongoConf' => 'nullable',
'mongoInitdbRootUsername' => ValidationPatterns::databaseIdentifierRules(
    enforcePattern: $this->mongoInitdbRootUsername !== $this->database->mongo_initdb_root_username,
),
'mongoInitdbRootPassword' => ValidationPatterns::databasePasswordRules(
    enforcePattern: $this->mongoInitdbRootPassword !== $this->database->mongo_initdb_root_password,
),
'mongoInitdbDatabase' => ValidationPatterns::databaseIdentifierRules(
    enforcePattern: $this->mongoInitdbDatabase !== $this->database->mongo_initdb_database,
),
'image' => 'required',
'portsMappings' => ValidationPatterns::portMappingRules(),
'isPublic' => 'nullable|boolean',
'publicPort' => 'nullable|integer|min:1|max:65535',
'publicPortTimeout' => 'nullable|integer|min:1',
'isLogDrainEnabled' => 'nullable|boolean',
'customDockerRunOptions' => 'nullable',
```
(Username/database rules are always `required` server-side even though the inputs drop `required` after start.)

### `messages()`
```php
array_merge(combinedMessages(), portMappingMessages(), [
    'name.required' => 'The Name field is required.',
    ...databaseIdentifierMessages('mongoInitdbRootUsername', 'Root Username'),
    ...databasePasswordMessages('mongoInitdbRootPassword', 'Root Password'),
    ...databaseIdentifierMessages('mongoInitdbDatabase', 'MongoDB Database'),
    'image.required' => 'The Docker Image field is required.',
    'publicPort.integer' => 'The Public Port must be an integer.',
    'publicPort.min' => 'The Public Port must be at least 1.',
    'publicPort.max' => 'The Public Port must not exceed 65535.',
    'publicPortTimeout.integer' => 'The Public Port Timeout must be an integer.',
    'publicPortTimeout.min' => 'The Public Port Timeout must be at least 1.',
])
```

### `$validationAttributes`
```php
'name' => 'Name', 'description' => 'Description', 'mongoConf' => 'Mongo Configuration',
'mongoInitdbRootUsername' => 'Root Username', 'mongoInitdbRootPassword' => 'Root Password',
'mongoInitdbDatabase' => 'Database', 'image' => 'Image', 'portsMappings' => 'Port Mapping',
'isPublic' => 'Is Public', 'publicPort' => 'Public Port', 'publicPortTimeout' => 'Public Port Timeout',
'customDockerRunOptions' => 'Custom Docker Run Options',
```

### `syncData(true)` (in order)
validate → name, description, `mongo_conf`, `mongo_initdb_root_username`, `mongo_initdb_root_password`, `mongo_initdb_database`
(all unconditionally, even after start — like PG), image, ports_mappings, is_public, public_port ?: null, public_port_timeout ?: null,
is_log_drain_enabled, custom_docker_run_options → save.

### `submit()`
Same as PG, plus after the publicPort step: `if (str($this->mongoConf)->isEmpty()) $this->mongoConf = null;`. Authorize `update`.
Finally config_hash block.

---

## 5. MySQL (`Mysql/General.php`, `mysql/general.blade.php`)

### Section structure
1. `database-details-section` — `Manage the identity, container image, and credentials of this MySQL database.`
   - Image helper `Use a published MySQL image from Docker Hub.`
   - `credentials-section`, helper `Keep these values aligned with the credentials configured inside MySQL.`
2. `access-section` (status-info `livewire:project.database.mysql.status-info`; port tokens `3306`).
3. `container-section` — `Docker options, configuration, and log forwarding for the database container.`
   - `configuration-section`, helper `Override the MySQL configuration used by this container.`
   - `log-delivery-section`.

### Callout — only when `started_at` (as PG)
title `Keep credentials synchronized`, body
`Changing values here does not update MySQL. Update MySQL first, then synchronize the values here so backups and other automations continue working.`
Grid class `{{ started_at ? 'mt-4 ' : '' }}grid gap-4 lg:grid-cols-2`.

### Fields (grid order)
| Prop | Column | Label | Type | Placeholder | Helper | Conditions |
|---|---|---|---|---|---|---|
| `mysqlRootPassword` (`string`) | `mysql_root_password` | `Root password` | `password` | — | — | `:required="(bool) $database->started_at"` (required only AFTER start — verbatim upstream); hidden-for-member → `label="Root password" disabled value="Hidden (only admins can view)"` |
| `mysqlUser` (`string`) | `mysql_user` | `Normal user` | text | — | — | `required` always; no placeholder |
| `mysqlPassword` (`string`) | `mysql_password` | `Normal user password` | `password` | — | — | `required`; hidden-for-member → `label="Normal user password" disabled value="Hidden (only admins can view)"` |
| `mysqlDatabase` (`string`) | `mysql_database` | `Initial database` | text | `If empty, it will match the normal user.` | started_at ? `You can only change this in the database.` : null | `:readonly="(bool) started_at"`; no required attr |
| `mysqlConf` (`?string`) | `mysql_conf` | `Custom MySQL configuration` | textarea rows=10 | — | — | hidden-for-member → disabled input |

### read-branch syncData
name, description, canSee, `mysqlRootPassword = canSee ? … : ''`, mysqlUser, `mysqlPassword = canSee ? … : ''`, mysqlDatabase,
`mysqlConf = canSee ? … : null`, image, portsMappings, isPublic, publicPort, publicPortTimeout, isLogDrainEnabled, customDockerRunOptions.

### `rules()` (in order)
```php
'name' => ValidationPatterns::nameRules(),
'description' => ValidationPatterns::descriptionRules(),
'mysqlRootPassword' => ValidationPatterns::databasePasswordRules(
    enforcePattern: $this->mysqlRootPassword !== $this->database->mysql_root_password,
),
'mysqlUser' => ValidationPatterns::databaseIdentifierRules(
    enforcePattern: $this->mysqlUser !== $this->database->mysql_user,
),
'mysqlPassword' => ValidationPatterns::databasePasswordRules(
    enforcePattern: $this->mysqlPassword !== $this->database->mysql_password,
),
'mysqlDatabase' => ValidationPatterns::databaseIdentifierRules(
    enforcePattern: $this->mysqlDatabase !== $this->database->mysql_database,
),
'mysqlConf' => 'nullable',
'image' => 'required',
'portsMappings' => ValidationPatterns::portMappingRules(),
'isPublic' => 'nullable|boolean',
'publicPort' => 'nullable|integer|min:1|max:65535',
'publicPortTimeout' => 'nullable|integer|min:1',
'isLogDrainEnabled' => 'nullable|boolean',
'customDockerRunOptions' => 'nullable',
```
(Root password is server-side `required` always, regardless of the view's `required` attr.)

### `messages()`
```php
array_merge(combinedMessages(), portMappingMessages(), [
    'name.required' => 'The Name field is required.',
    ...databasePasswordMessages('mysqlRootPassword', 'Root Password'),
    ...databaseIdentifierMessages('mysqlUser', 'MySQL User'),
    ...databasePasswordMessages('mysqlPassword', 'MySQL Password'),
    ...databaseIdentifierMessages('mysqlDatabase', 'MySQL Database'),
    'image.required' => 'The Docker Image field is required.',
    'publicPort.integer' => 'The Public Port must be an integer.',
    'publicPort.min' => 'The Public Port must be at least 1.',
    'publicPort.max' => 'The Public Port must not exceed 65535.',
    'publicPortTimeout.integer' => 'The Public Port Timeout must be an integer.',
    'publicPortTimeout.min' => 'The Public Port Timeout must be at least 1.',
])
```

### `$validationAttributes`
```php
'name' => 'Name', 'description' => 'Description', 'mysqlRootPassword' => 'Root Password', 'mysqlUser' => 'User',
'mysqlPassword' => 'Password', 'mysqlDatabase' => 'Database', 'mysqlConf' => 'MySQL Configuration', 'image' => 'Image',
'portsMappings' => 'Port Mapping', 'isPublic' => 'Is Public', 'publicPort' => 'Public Port',
'publicPortTimeout' => 'Public Port Timeout', 'customDockerRunOptions' => 'Custom Docker Run Options',
```

### `syncData(true)` (in order)
validate → name, description, mysql_root_password, mysql_user, mysql_password, mysql_database, mysql_conf (all unconditional),
image, ports_mappings, is_public, public_port ?: null, public_port_timeout ?: null, is_log_drain_enabled, custom_docker_run_options → save.

### `submit()`
Exactly PG's (authorize `update`, ports strip, publicPort empty→null, syncData, toasts, finally config_hash block).

---

## 6. MariaDB (`Mariadb/General.php`, `mariadb/general.blade.php`)

Identical to MySQL with these substitutions (verified by diff):

- Props `mariadbRootPassword`, `mariadbUser`, `mariadbPassword`, `mariadbDatabase`, `mariadbConf` → columns `mariadb_root_password`,
  `mariadb_user`, `mariadb_password`, `mariadb_database`, `mariadb_conf`.
- Strings: section description `…credentials of this MariaDB database.`; Image helper `Use a published MariaDB image from Docker Hub.`;
  credentials helper `Keep these values aligned with the credentials configured inside MariaDB.`; callout body
  `Changing values here does not update MariaDB. Update MariaDB first, then synchronize the values here so backups and other automations continue working.`;
  configuration helper `Override the MariaDB configuration used by this container.`; textarea / hidden label `Custom MariaDB configuration`.
- Labels of credential fields are the same as MySQL (`Root password`, `Normal user`, `Normal user password`, `Initial database`,
  placeholder `If empty, it will match the normal user.`). Root password also `:required="(bool) $database->started_at"`.
- Status-info `livewire:project.database.mariadb.status-info`; port tokens `3306`.
- messages: `...databasePasswordMessages('mariadbRootPassword', 'Root Password')`, `...databaseIdentifierMessages('mariadbUser', 'MariaDB User')`,
  `...databasePasswordMessages('mariadbPassword', 'MariaDB Password')`, `...databaseIdentifierMessages('mariadbDatabase', 'MariaDB Database')`
  (rest identical, same order).
- validationAttributes: `mariadbRootPassword => 'Root Password'`, `mariadbUser => 'User'`, `mariadbPassword => 'Password'`,
  `mariadbDatabase => 'Database'`, `mariadbConf => 'MariaDB Configuration'`, and **`customDockerRunOptions => 'Custom Docker Options'`**
  (MySQL: `Custom Docker Run Options`).
- rules / syncData / submit: same as MySQL with renamed props/columns.

---

## 7. ClickHouse (`Clickhouse/General.php`, `clickhouse/general.blade.php`)

### Extra top-of-form danger callout (before the first section, inside the form, after the unsaved bar)
Shown when `$anonymousDataVolume` is truthy. `<x-callout type="danger" title="Current data is not in the data volume">`:
- `<p>`: `Because of a bug in older Coolify versions, a file mount removed the data volume. This database keeps its current data in the unnamed Docker volume <code>{{ $anonymousDataVolume }}</code>. Starting or restarting it is blocked, because it would use the data volume and the current data would seem lost.`
- `<p class="mt-2">`: `An admin can click "Keep current data" to use this volume as the data volume. The previous data volume is not used anymore, but stays on the server. It can contain data from before the file mount was added.`
- `@can('update', $database)`: `<div class="mt-3">` with `x-modal-confirmation title="Keep current data?" buttonTitle="Keep current data" submitAction="keepCurrentDataVolume" :confirmWithText="false" :confirmWithPassword="false" step2ButtonText="Keep current data"`, actions:
  1. `'Use the unnamed volume ' . $anonymousDataVolume . ' as the data volume of this database.'`
  2. `Starts and restarts are allowed again and keep the current data.`
  3. `The previous data volume stays on the server and is not used anymore.`

`#[Locked] public ?string $anonymousDataVolume` — set in `mount()` (inside try, after the server check) to `$database->anonymousDataVolume()`.
`keepCurrentDataVolume()`: authorize `update`; `$volumeName = $database->anonymousDataVolume()`; if null → `anonymousDataVolume = null`,
dispatch `info: This database already uses its data volume.` and return; else `$database->keepAnonymousDataVolume($volumeName)`,
`anonymousDataVolume = null`, dispatch `success: The current data volume is now the data volume of this database. You can restart it safely.`;
catch `\Throwable` → `handleError` (not returned).

### Section structure
1. `database-details-section` — `Manage the identity, container image, and credentials of this ClickHouse database.`
   - Image helper `Use a published clickhouse/clickhouse-server image from Docker Hub.`
   - `credentials-section`, helper `Keep these values aligned with the credentials configured inside ClickHouse.`
2. `access-section` (status-info `livewire:project.database.clickhouse.status-info`; port tokens `8123`).
3. `container-section` — `Docker options and log forwarding for the database container.`; **no configuration subsection**; `log-delivery-section`.

### Credentials callout — inverted vs PG: shown only when NOT started
`@if (!$database->started_at)`: `type="warning" title="Verify the initial credentials"`, body
`You can only change these credentials here before the first start. Later changes must be made inside the database.`
Grid class `{{ !$database->started_at ? 'mt-4 ' : '' }}grid gap-4 lg:grid-cols-2`.

### Fields
| Prop | Column | Label | Type | Placeholder | Helper | Conditions |
|---|---|---|---|---|---|---|
| `clickhouseAdminUser` (`string`) | `clickhouse_admin_user` | started_at ? `Initial username` : `Username` | text | `If empty: clickhouse` | — (none) | `:readonly="(bool) started_at"`, `required` always |
| `clickhouseAdminPassword` (`string`) | `clickhouse_admin_password` | started_at ? `Initial password` : `Password` | `password` | — | started_at ? `You can only change this in the database.` : null | `required`, `:readonly="(bool) started_at"`; hidden-for-member → `label="Password" disabled value="Hidden (only admins can view)"` |

### read-branch syncData
name, description, `clickhouseAdminUser`, canSee, `clickhouseAdminPassword = canSee ? … : ''`, image, portsMappings, isPublic, publicPort,
publicPortTimeout, customDockerRunOptions, isLogDrainEnabled.

### `rules()` (in order)
```php
'name' => ValidationPatterns::nameRules(),
'description' => ValidationPatterns::descriptionRules(),
'clickhouseAdminUser' => ValidationPatterns::databaseIdentifierRules(
    enforcePattern: $this->clickhouseAdminUser !== $this->database->clickhouse_admin_user,
),
'clickhouseAdminPassword' => ValidationPatterns::databasePasswordRules(
    enforcePattern: $this->clickhouseAdminPassword !== $this->database->clickhouse_admin_password,
),
'image' => 'required|string',
'portsMappings' => ValidationPatterns::portMappingRules(),
'isPublic' => 'nullable|boolean',
'publicPort' => 'nullable|integer|min:1|max:65535',
'publicPortTimeout' => 'nullable|integer|min:1',
'customDockerRunOptions' => 'nullable|string',
'isLogDrainEnabled' => 'nullable|boolean',
```

### `messages()`
```php
array_merge(combinedMessages(), portMappingMessages(), [
    ...databaseIdentifierMessages('clickhouseAdminUser', 'Admin User'),
    ...databasePasswordMessages('clickhouseAdminPassword', 'Admin Password'),
    'image.required' => 'The Docker Image field is required.',
    'image.string' => 'The Docker Image must be a string.',
    'publicPort.integer' => 'The Public Port must be an integer.',
    'publicPort.min' => 'The Public Port must be at least 1.',
    'publicPort.max' => 'The Public Port must not exceed 65535.',
    'publicPortTimeout.integer' => 'The Public Port Timeout must be an integer.',
    'publicPortTimeout.min' => 'The Public Port Timeout must be at least 1.',
])
```
No `name.required` override. **No `$validationAttributes`.**

### `syncData(true)` (in order)
validate → name, description, [`clickhouse_admin_user` and `clickhouse_admin_password` only if `! started_at`], image, ports_mappings,
is_public, public_port ?: null, public_port_timeout ?: null, custom_docker_run_options, is_log_drain_enabled → save.
(Comment: "After the first start the credentials are read-only, so autofilled values must not be saved.")

### `submit()`
Same as PG (authorize `update`, strip ports, publicPort empty→null, syncData, toasts, finally config_hash block).

---

## 8. Connection / status-info block

All types render the shared `x-database-status-info` component; only the per-type `StatusInfo` hooks differ.
PostgreSQL baseline: label `Postgres`, `supportsSsl()` true, `sslModeOptions` = allow/prefer/require/verify-ca/verify-full,
`sslModeHelper` `Choose the SSL verification mode for PostgreSQL connections`, `showPublicUrlPlaceholder` false,
`afterRefresh` loads `sslMode = ssl_mode`, `applyExtraSslAttributes` saves `ssl_mode`, `updatedSslMode()` → `instantSaveSSL()`.

Shared component behaviour (for reference, unchanged across types):
- URL labels are `"{$label} URL (internal)"` and `"{$label} URL (public)"`; helper on both
  `If you change the user/password/port, this could be different. This is with the default values.`
- If hidden-for-member: both URL fields as disabled `Hidden (only admins can view)`.
- Else if `isCredentialLocked` (only when the model has `isCredentialLocked()` and it returns true): **only** the internal field,
  disabled, `value="Locked (shown once)"`, helper `The password variable is locked, so the URL that contains it is not shown.`
  (no public field at all).
- Else: internal URL (`type="password" readonly copyable wire:model="dbUrl"`); public URL same if `dbUrlPublic` set;
  else if `showPublicUrlPlaceholder`: public field `readonly value="Starting the database will generate this."` (not password type, not copyable);
  else no public field.
- SSL block (when `supportsSsl`): heading `SSL configuration`, text `Encryption settings can only be changed while the database is stopped.`;
  `SSL` listbox Enabled/Disabled; `SSL mode` listbox only if `sslModeOptions` non-null.
- Trait `refresh()` nulls `dbUrl`/`dbUrlPublic` when hidden-for-member OR credential locked.

| Type | label (`databaseLabel()`) | supportsSsl | sslModeOptions (value ⇒ label) | sslModeHelper | showPublicUrlPlaceholder | credential lock applies? |
|---|---|---|---|---|---|---|
| PostgreSQL | `Postgres` | yes | allow⇒`allow (insecure)`, prefer⇒`prefer (secure)`, require⇒`require (secure)`, verify-ca⇒`verify-ca (secure)`, verify-full⇒`verify-full (secure)` | `Choose the SSL verification mode for PostgreSQL connections` | no | no (no method) |
| Redis | `Redis` | yes | **none** (no SSL mode listbox; no afterRefresh/applyExtraSslAttributes/updatedSslMode) | — | no | **yes** (`REDIS_PASSWORD` env var locked) |
| KeyDB | `KeyDB` | yes | none | — | **yes** | **yes** (`serverPasswordEnvironmentVariable()` = env `REDIS_PASSWORD`, returns null when `legacy_password_quoting` and not secret-manager) |
| Dragonfly | `Dragonfly` | yes | none | — | **yes** | **yes** (same as KeyDB, env `REDIS_PASSWORD`) |
| MongoDB | `Mongo` | yes | allow⇒`allow (insecure)`, prefer⇒`prefer (secure)`, require⇒`require (secure)`, verify-full⇒`verify-full (secure)` (**no verify-ca**) | `Choose the SSL verification mode for MongoDB connections` | no | no |
| MySQL | `MySQL` | yes | PREFERRED⇒`Prefer (secure)`, REQUIRED⇒`Require (secure)`, VERIFY_CA⇒`Verify CA (secure)`, VERIFY_IDENTITY⇒`Verify Full (secure)` (no insecure option) | `Choose the SSL verification mode for MySQL connections` | no | no |
| MariaDB | `MariaDB` | yes | none | — | no | no |
| ClickHouse | `Clickhouse` (lower-case h) | **no** (no SSL block at all; enableSsl/cert not loaded) | — | — | **yes** | no |

MongoDB and MySQL have the same `afterRefresh` / `applyExtraSslAttributes` / `updatedSslMode` hooks as PG (persist `ssl_mode`).
The `title` keys in sslModeOptions are not rendered by the component (only `label`).

Note the asymmetry: KeyDB and Dragonfly General forms ignore the credential lock (always show/edit the `keydb_password` /
`dragonfly_password` column), but their status-info hides the URL with "Locked (shown once)" when env `REDIS_PASSWORD` is locked.
Redis is the only General form that also reflects the lock (`isPasswordLocked`).

Connection URL shapes (model accessors, for reference): Redis internal `{redis|rediss}://[<urlenc username>:]<urlenc password>@<uuid>:{6379|6380}/0`
(username part only when redis version >= 6.0; `?cacert=/etc/ssl/certs/coolify-ca.crt` appended when SSL and `ssl_mode === 'verify-ca'`),
external uses server IP and `public_port`. KeyDB/Dragonfly: `{redis|rediss}://:<urlenc password>@<uuid>:{6379|6380}/0` (no username), same cacert suffix rule;
password = `connectionPassword()` (env `REDIS_PASSWORD` value if present and non-empty, else the column).
