## Code Review Rules
Focus on consequential, repository-specific issues. Skip nits unless they hide a real bug.
Don't infer that a value mutates in place just because SwiftUI uses `.task(id:)` / `.onChange`;
check whether the screen actually changes it.

### Left sidebar
Archived/committed chats show last change time (`last_message_at`), not `archived_recency`.
Don't suggest switching archived rows to archive age.

### Presets / settings
Comet is unshipped. Don't require serde aliases or settings-file back-compat for renamed
presets unless explicitly asked.

### Concurrency
CommandCache holding one mutex across `probe()` is a deliberate same-cwd coalesce.
Don't require per-key in-flight futures speculatively.
