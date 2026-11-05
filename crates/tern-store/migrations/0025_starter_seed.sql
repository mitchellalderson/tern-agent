-- First-run starter agents (DECISIONS.md "Seeded starter agents"). The
-- "already seeded" flag lives in kv ('tern', 'starters_seeded'); an install
-- that existed before this version gets it set by `migrations::migrate`
-- (which knows whether the database was brand new), so an upgrade never
-- grows agents it did not ask for. Nothing to change in the schema.
SELECT 1;
