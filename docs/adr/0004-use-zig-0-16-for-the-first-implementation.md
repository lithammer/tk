# Use Zig 0.16 for the first implementation

The first tk implementation used Zig 0.16. The project offered a chance to
explore Zig while building a small static command-line tool with explicit
filesystem and subprocess behavior. Rust offered stronger algebraic domain
modeling and mature CLI snapshot testing, while Go was a practical default for
CLI tools. Zig was a sound choice because exploration was one of the project's
goals.

The project accepted that Zig might require more custom test tooling than Rust
or Go. The implementation still separated domain logic, command handling,
storage, and subprocess execution because those boundaries aid testing and
maintenance. It did not shape the design around a possible rewrite.
