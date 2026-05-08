# Vendored SQLite Amalgamation

Source: https://www.sqlite.org/2026/sqlite-amalgamation-3530000.zip
Version: SQLite 3.53.0 (2026-04-09)

Files:
- `sqlite3.c`
- `sqlite3.h`
- `sqlite3ext.h`

Compiled into the `tk` executable through `build.zig`. SQLite is in the
public domain; see `https://www.sqlite.org/copyright.html`.

To upgrade, replace these three files with a newer amalgamation and update
this note. Verify build flags in `build.zig` against the amalgamation's
`SQLITE_*` defaults.
