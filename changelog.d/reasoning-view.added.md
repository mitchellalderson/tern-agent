- **Reasoning view**: when a model returns its reasoning (Anthropic thinking, OpenAI-compatible `reasoning_content` /
  `reasoning`), a chevron beside "Thinking" opens a quiet box that streams it live. The message keeps a collapsed
  "Thought for 12s" note. No chevron when there is none; redacted reasoning reads "Reasoning hidden". Turn it off with
  `show_reasoning = false`. Never sent to channels.
