# Use Zig 0.16 for the first implementation

tk will start in Zig 0.16 because the project is an opportunity to explore Zig while building a small static command-line tool with explicit filesystem and subprocess behavior. Rust was considered for stronger algebraic domain modeling and mature CLI snapshot testing, and Go was considered as the pragmatic CLI default, but Zig is acceptable because exploration is part of the project goal.

We accept that Zig may require more custom test tooling than Rust or Go. The implementation should still keep normal engineering boundaries between domain logic, command handling, storage, and subprocess execution because that improves testability and maintainability, not because of a hypothetical rewrite. We will not contort the design to keep another language as a fallback.
