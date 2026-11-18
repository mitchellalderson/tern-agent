-- tern is local-only: nothing outside this machine can start a routine.
-- Webhook routines have no trigger left, so they go with their secrets.
-- (`routine_events` rows cascade from the routine.)
DELETE FROM schedules WHERE trigger_kind = 'webhook';
DROP TABLE IF EXISTS routine_webhooks;
