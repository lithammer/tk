# Create tickets locally by default

tk creates new **Tickets** and **Epics** as local objects by default, even when a **Primary Backend** is configured. Because tk is agent-first, agents need a low-friction place to capture temporary context, deferred decisions, and follow-ups without polluting upstream issue trackers; **Promotion** is the explicit curation step that creates backend-backed objects.
