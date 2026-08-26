# Use SQLite for the repository store

tk uses SQLite for the **Repository Store**. The CLI needs atomic updates across
current **Ticket** and **Epic** state and the **Mutation Log**. SQLite provides
transactions, queryable local state, durability, and simple temporary-file
tests without a custom storage engine.
