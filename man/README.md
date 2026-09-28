# Editing the manual

Run `make manpage` to regenerate `man/tk.1`, then review and commit its diff
with your source changes. Do not edit the generated file directly.

The single `tk(1)` page covers all public commands and nested subcommands:

- Clap definitions own syntax, concise descriptions, arguments, options,
  and parser defaults. These also supply `tk --help` and command help.
- `commands/<command-path>.roff` holds extended guidance beside a command's
  generated reference. For example, `commands/sync-log.roff` belongs to
  `tk sync log`. Add a fragment only when a command needs more explanation.
- `sections/description.roff` introduces tk. `sections/reference.roff` holds
  exit codes, environment variables, shared examples, and external references.

Keep behavioral defaults that clap does not express in the command guidance.
When renaming or removing a command, update its fragment too; generation
rejects fragments that no longer match a public command. Use command names
for cross-references within this page, not links to separate `tk-<command>(1)`
pages.

Preview and check the result:

```sh
make manpage
man -l man/tk.1
make check-manpage
```

`make check-manpage` generates in memory and fails if the checked-in file
differs. It does not rewrite files. CI runs this check. Keep `Cargo.lock`
changes and any resulting manual diff together when updating the generator.

Without make, use `cargo run --locked --example manpage` to regenerate and
add `-- --check` to check. The generator lives in
`crates/tk/examples/manpage.rs`; its dependencies are for development only.
Release builds embed the reviewed `man/tk.1`, and `tk manpage` prints or
installs those bytes.
