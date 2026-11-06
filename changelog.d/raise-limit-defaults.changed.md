- **Higher token and context limits.** Each model's own window and output limit are used when known, instead of a flat
  128k / 8192. Fallbacks: 200k context, 32k output; local servers keep 32k context. New `max_tokens` setting. Compaction
  starts at 80% (was 75%); summaries get twice the room.
