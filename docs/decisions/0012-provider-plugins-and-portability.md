# ADR 0012: Provider plugins and portability back to Coolify

Status: proposed (2026-11-06)

## Context

coolify-rs runs on Coolify's database, and [ADR 0010](0010-plugins.md) keeps that two-way: a database
coolify-rs ran on can go back to Coolify, which never reads the `coolify_rs_plugins` schema. Every
capability so far, including `resources` and `deploy` ([ADR 0011](0011-plugin-resource-and-deploy-capabilities.md)),
keeps it so, because plugins never write to Coolify's tables.

Some extensions can't work that way. A **provider plugin** supplies a kind of thing Coolify itself offers
a fixed set of: a build pack (Coolify knows `nixpacks`, `static`, `dockerfile`, `dockercompose`,
`railpack` and `dockerimage`), a database type, a DNS or cloud provider. Its resources must live in
Coolify's own tables, so Coolify's screens, API, deploy queue and notifications see them, and they carry a
value Coolify does not know, such as `applications.build_pack = 'acme-go'`. Coolify, given that row, would
fail to deploy it or show it wrongly.

This ADR sets the rules provider plugins will follow and adds the check that protects the way back. It
does not add a provider interface: that needs its own design and its own ADR when a first provider is
written.

## Decision

### 1. Ownership is recorded

A row of one of Coolify's tables that holds a value only a plugin understands is **plugin-owned**, and the
ownership is recorded in coolify-rs's own schema:

```sql
coolify_rs_plugins.owned_resources (
    table_name text,      -- the Coolify table, e.g. 'applications'
    row_id bigint,        -- the row's id
    plugin_id text,       -- 'publisher/name'
    note text,            -- what is foreign about it, e.g. 'build_pack acme-go'
    created_at timestamptz,
    PRIMARY KEY (table_name, row_id)
)
```

- The host, not the plugin, writes Coolify's row and the ownership row, in one transaction, so a foreign
  value never exists unrecorded through the plugin path.
- The table has no foreign key to the installed plugins. Uninstalling a provider plugin does not take its
  values out of Coolify's tables, so the record must outlive it. Uninstalling one that still owns rows is
  refused until they are handed back (section 4).
- Coolify's tables gain no column and no row of coolify-rs's own. The table is created with the rest of
  the plugin schema.

### 2. Parity becomes one-way for owned rows only

- Everything else stays two-way. A database with plugins but no owned rows goes back to Coolify as is.
- For owned rows, coolify-rs keeps answering Coolify's API from the database as it always does, so the
  foreign value appears where Coolify would show the column. Parity suites run without provider plugins,
  and with the canary plugin of ADR 0010, as before.
- A database with owned rows cannot be handed back until each is handed back. That is the one-way
  consequence, and the only one: coolify-rs still takes any Coolify database.

### 3. `coolify-rs check-portable`

A read-only command that answers "can Coolify take this database back?":

- **Owned rows**: every row of `coolify_rs_plugins.owned_resources`, with its plugin and note.
- **Values Coolify does not accept**: every row of Coolify's tables with a value outside Coolify's closed
  set, whether or not a plugin marked it, so a value written around the plugin path (by hand, by a bug, by
  a plugin that forgot) is caught too. To start, `applications.build_pack` against Coolify's
  `BuildPackTypes` enum (`app/Enums/BuildPackTypes.php`: `nixpacks`, `static`, `dockerfile`,
  `dockercompose`, `railpack`) plus `dockerimage`, which Coolify's Docker image endpoints write. Each
  provider ADR adds the columns it opens.
- **The plugin schema**: whether `coolify_rs_plugins` exists. Coolify ignores it, so it can stay;
  `DROP SCHEMA coolify_rs_plugins CASCADE` removes every trace of plugins.

It reads `DATABASE_URL` (or `DB_*`), never creates the plugin schema, prints a report for people or JSON
with `--json`, and exits **0** when portable, **1** when not, **2** when it could not check.

### 4. Handing a database back

1. Run `coolify-rs check-portable`.
2. For each owned row, either let the provider plugin convert it to a Coolify equivalent (a provider
   interface will require a `hand-back` function, e.g. turning `acme-go` into `dockerfile` with the
   Dockerfile it would have generated), change it to a Coolify value through the API or UI, or delete the
   resource. A conversion removes the ownership row in the same transaction.
3. Fix any unknown value the check lists without an owner the same way.
4. Run the check again until it exits 0.
5. Optionally drop `coolify_rs_plugins`. Then start Coolify on the database.

### 5. Review

Provider plugins write to Coolify's tables, so they sit in the highest review tier of ADR 0010: two
maintainers and a security review, plus a check that the plugin's hand-back produces values Coolify
accepts. Until a provider interface exists, no plugin can write to Coolify's tables at all.

## Consequences

- Hand-back is mechanical and checkable, before anyone points Coolify at the database.
- The check is useful today: it confirms that a database coolify-rs ran on, with or without plugins, is
  still Coolify's, and it catches foreign values from any source.
- Each future provider must name the columns it opens and extend the check's closed sets, and must ship a
  hand-back. That is extra work per provider, on purpose.
