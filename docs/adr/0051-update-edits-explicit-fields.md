# Update edits explicit fields

`tk update` edits explicitly named fields on a Ticket or Epic. Callers can
change title and body independently, alongside Priority and Epic Membership
where those apply. An omitted field keeps its current Repository Store value.

The combined-message form can erase a body when a caller supplies only a
title (tk-223). Naming the fields makes the write intent explicit without
asking paragraph structure to stand for omission or deletion. Separate verbs
such as `rename` would express one edit clearly, but field flags also let
callers combine edits in one atomic update. Creation keeps its title-and-body
message format.

`--title` (`-t`) replaces the title. `--body` (`-b`) supplies a replacement
body inline; `--body-file` reads it from a file, or from stdin when the path
is `-`. The two body sources conflict. A title edit can accompany either body
source and other field edits in the same invocation. `--body-file` has no
short flag; the removed `-F` remains unused.

`--body` always takes literal text, including a leading `@`. File input uses
`--body-file` instead of an `@path` prefix so callers can pass arbitrary body
text without escaping a source marker. The extra flag keeps the input rules
explicit.

Body replacement preserves the supplied UTF-8 text in the Repository Store
and queued Mutation snapshot, including whitespace and line endings, and
rejects NUL. Titles trim only outer ASCII spaces and tabs and must contain
at least one character that is not Unicode whitespace (`char::is_whitespace`).
They reject NUL and these line breaks: LF (U+000A), VT (U+000B), FF (U+000C),
CR (U+000D), NEL (U+0085), LINE SEPARATOR (U+2028), and PARAGRAPH SEPARATOR
(U+2029). Other title text is preserved.

Supplying an empty body clears it, including an empty file or stdin stream.
There is no separate clear flag: an explicit body source replaces the field,
while omitting both body sources preserves it. An accidentally empty source
can therefore clear a body; callers must check the content they supply.

All requested field writes and required Mutations commit in one Repository
Store transaction. Backend delivery keeps the existing full title/body
snapshots and separate relationship Mutations. A title-only edit can therefore
overwrite a body changed independently on the Backend when sync applies the
snapshot. The local transaction promises neither field isolation nor atomic
delivery on the Backend.

`tk update` removes `-m` / `--message` and `-F` / `--file`. Old invocations
fail with ordinary unknown-option errors; neither option gains a new meaning.
Migration guidance belongs only in release notes, which must explain that an
old combined-message file needs its title and body separated. Command help
and shipped examples describe the new interface without migration guidance.
`tk add` keeps its combined-message input.

Append remains a separate design in tk-222, which takes these field-editing
rules as its starting point.
