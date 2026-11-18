-- tern is local-only: messaging gateways (Telegram, ntfy, Slack) are gone.
-- Pairing tables, per-agent bots and gateway chat state go with them.
DROP TABLE IF EXISTS pairing_codes;
DROP TABLE IF EXISTS telegram_pairings;
ALTER TABLE agents DROP COLUMN telegram;
DELETE FROM kv WHERE namespace IN
  ('chat_agent', 'bot_agent', 'chat_channel', 'chat_channel_topic');

-- A delivery target is now local: `home`, `dm` (the owning agent's DM) or
-- `channel:<id>`. A routine, run, wait or held delivery that named a
-- gateway chat (`telegram:123`, `ntfy:topic`) falls back to Home.
UPDATE schedules SET deliver_to = 'home'
  WHERE deliver_to IS NOT NULL AND deliver_to NOT IN ('home', 'dm')
    AND deliver_to NOT LIKE 'channel:%';
UPDATE runs SET deliver_to = 'home'
  WHERE deliver_to IS NOT NULL AND deliver_to NOT IN ('home', 'dm')
    AND deliver_to NOT LIKE 'channel:%';
UPDATE waits SET deliver_to = 'home'
  WHERE deliver_to IS NOT NULL AND deliver_to NOT IN ('home', 'dm')
    AND deliver_to NOT LIKE 'channel:%';
UPDATE held_deliveries SET target = 'home'
  WHERE target NOT IN ('home', 'dm') AND target NOT LIKE 'channel:%';
