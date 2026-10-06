-- ADR-0017: input_hash becomes a hash of the body alone ("b2:<sha256>").
-- A migration cannot read raw files, so the data upgrade is code, run by the
-- pipeline and gated by this marker: 1 = old hashes may exist, 2 = upgraded.
-- A new catalog has no summaries, so it starts at 2 only when the step has run.
INSERT OR IGNORE INTO settings(key, value, updated_at)
VALUES ('summary.input_hash_version', '1', strftime('%Y-%m-%dT%H:%M:%SZ', 'now'));
