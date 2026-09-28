# Ingest guide

Single-item ingest (`sb ingest <url-or-path> --context "<why>"`) is planned. Until
it is available, tell the user that documents are added by the scheduled sync
(Slack and Google Meet) and cannot be added one by one yet.

When it becomes available, the rules are:

- Only ingest what the user asks for. Never ingest links automatically.
- Ask at most one question: which project or case the document relates to. Pass
  the answer as `--context`; it becomes the entry's `background` section.
- Empty `decisions` / `action_items` are normal for documents. Do not keep asking
  the user questions to fill them.
