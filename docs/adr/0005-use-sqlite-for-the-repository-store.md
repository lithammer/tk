# Use SQLite for the repository store

tk will use SQLite for the **Repository Store**. The CLI needs atomic updates across current **Ticket** and **Epic** state plus the **Mutation Log**, and SQLite provides transactions, queryable local state, durability, and straightforward temp-file testing without inventing a custom storage engine.
