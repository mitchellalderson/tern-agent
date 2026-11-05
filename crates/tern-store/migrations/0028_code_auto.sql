-- Code auto mode (docs/alpha-coding-agent.md): a per-agent opt-in, never set
-- by a starter, a bundle or a default. 0 = off. Turning it on is a ledger
-- entry (`code_auto.agent`) carrying who confirmed which disclaimer.
ALTER TABLE agents ADD COLUMN code_auto INTEGER NOT NULL DEFAULT 0;
