//! Pure domain helpers — no SQLite, filesystem, Git, or subprocess access.
//!
//! Slice 0 ships this directory empty; domain vocabulary types (Display ID,
//! Priority, Item Status, Ticket Kind, …) land as their first consumer slice
//! arrives. The directory exists now so downstream slices can grow it without
//! introducing a new top-level module.
