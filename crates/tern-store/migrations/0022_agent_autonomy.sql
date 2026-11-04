-- An agent's autonomy ceiling (docs/agent-profiles-design.md AP3): NULL = no
-- ceiling beyond the configured `autonomy`. A run's autonomy is the lower of
-- the two, frozen when it starts.
ALTER TABLE agents ADD COLUMN autonomy TEXT;
