-- Taint (docs/personal-agent-tooling-spec.md §5.3, T3a).
-- Why the run started tainted (untrusted input, a tainted parent). Set when
-- the run is created and never changed, so a resumed run starts the same.
ALTER TABLE runs ADD COLUMN input_taint TEXT;
-- Why the run is tainted now (NULL: clean). Sticky: set once, from
-- input_taint or the first untrusted step, and never cleared. For views; the
-- agent loop rebuilds its own bit from input_taint and the journal.
ALTER TABLE runs ADD COLUMN taint TEXT;
