-- Freeze per-run inputs so replay after a crash builds identical requests.
ALTER TABLE runs ADD COLUMN model TEXT;
ALTER TABLE runs ADD COLUMN system_prompt TEXT;
ALTER TABLE runs ADD COLUMN input_message_id INTEGER;

-- Which journal step appended a message (NULL for the run's input message).
ALTER TABLE messages ADD COLUMN step_idx INTEGER;
CREATE INDEX messages_run_step ON messages(run_id, step_idx);
