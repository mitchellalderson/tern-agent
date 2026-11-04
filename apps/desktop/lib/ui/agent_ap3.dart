import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../theme.dart';

/// The autonomy ceiling: "No ceiling" or one of [AutonomyLevel]. It only
/// ever lowers what the configured autonomy gives (`min`), so it is a cap,
/// never a grant (DECISIONS: Agent profiles AP3).
class AutonomyPicker extends StatelessWidget {
  const AutonomyPicker({
    super.key,
    required this.value,
    required this.onChanged,
  });

  /// `readonly`, `supervised`, `autonomous`, `yolo`; null = no ceiling.
  final String? value;
  final ValueChanged<String?> onChanged;

  static const _none = 'none';

  @override
  Widget build(BuildContext context) {
    final small = Theme.of(context).textTheme.bodySmall;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          'A cap, not a permission. Its runs get the lower of this and your '
          'configured autonomy, so a ceiling can only hold it back; it never '
          'lets the agent do more than you allowed. Sub-agents it starts get '
          'the lower of its level and their own.',
          key: const Key('autonomy-explainer'),
          style: small,
        ),
        const SizedBox(height: 4),
        RadioGroup<String>(
          groupValue: AutonomyLevel.parse(value)?.wire ?? _none,
          onChanged: (v) => onChanged(v == null || v == _none ? null : v),
          child: Column(
            children: [
              RadioListTile<String>(
                key: const Key('autonomy-none'),
                value: _none,
                dense: true,
                contentPadding: EdgeInsets.zero,
                title: const Text('No ceiling', style: TextStyle(fontSize: 13)),
                subtitle: Text(
                  'Your configured autonomy applies as it is.',
                  style: small,
                ),
              ),
              for (final l in AutonomyLevel.values)
                RadioListTile<String>(
                  key: Key('autonomy-${l.wire}'),
                  value: l.wire,
                  dense: true,
                  contentPadding: EdgeInsets.zero,
                  title: Text(
                    'At most ${l.label.toLowerCase()}',
                    style: const TextStyle(fontSize: 13),
                  ),
                  subtitle: Text(l.line, style: small),
                ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A small "Capped at supervised" chip for an agent's header, when it has a
/// ceiling.
class AutonomyChip extends StatelessWidget {
  const AutonomyChip({super.key, required this.agent});

  final Agent agent;

  @override
  Widget build(BuildContext context) {
    final level = agent.autonomyLevel;
    if (level == null) return const SizedBox.shrink();
    final c = context.colors;
    return Tooltip(
      message:
          '${agent.name} never runs above ${level.label.toLowerCase()} '
          'autonomy, whatever your setting is. ${level.line}',
      child: Container(
        key: const Key('autonomy-chip'),
        margin: const EdgeInsets.only(right: 4),
        padding: const EdgeInsets.symmetric(horizontal: 7, vertical: 3),
        decoration: BoxDecoration(
          border: Border.all(color: c.hairline),
          borderRadius: BorderRadius.circular(6),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.vertical_align_top, size: 12, color: c.muted),
            const SizedBox(width: 4),
            Flexible(
              child: Text(
                'Capped: ${level.label.toLowerCase()}',
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 11.5, color: c.muted),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// An agent's own Telegram bot: the NAME of a secret holding the bot's
/// token (never the token). The daemon refuses a name another bot already
/// polls, and the refusal shows when the sheet is saved. Bots start with
/// the daemon, so a change applies after a restart.
class TelegramBotField extends StatelessWidget {
  const TelegramBotField({
    super.key,
    required this.agent,
    required this.agents,
    required this.secrets,
    required this.value,
    required this.onChanged,
    required this.onSetSecret,
    this.loadError,
  });

  final Agent agent;
  final List<Agent> agents;

  /// Null while loading.
  final List<SecretStatus>? secrets;
  final String? loadError;

  /// The chosen secret's name; null = no bot of its own.
  final String? value;
  final ValueChanged<String?> onChanged;
  final VoidCallback onSetSecret;

  /// Why `s` can't be this agent's bot, when it can't be.
  String? _why(SecretStatus s) {
    if (s.name == value) return null;
    if (s.usedBy.contains('telegram')) return "the main bot's token";
    for (final o in agents) {
      if (o.id != agent.id && o.telegramSecret == s.name) {
        return "@${o.slug}'s bot";
      }
    }
    if (!s.isSet) return 'not set';
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final small = Theme.of(context).textTheme.bodySmall;
    final list = secrets;
    // The saved name stays choosable even if it is no longer listed.
    final orphan =
        value != null && !(list?.any((s) => s.name == value) ?? false)
        ? value
        : null;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          'Give it a bot of its own, so you can talk to it in Telegram without '
          "@mentions. Pick the secret that holds the bot's token from "
          "@BotFather. This holds the secret's name, never the token, and "
          'each bot needs its own token.',
          style: small,
        ),
        const SizedBox(height: 8),
        if (list == null && loadError == null)
          const LinearProgressIndicator(minHeight: 2)
        else
          DropdownButtonFormField<String?>(
            key: const Key('agent-bot'),
            initialValue: value,
            isExpanded: true,
            isDense: true,
            decoration: InputDecoration(
              isDense: true,
              border: OutlineInputBorder(
                borderRadius: BorderRadius.circular(8),
              ),
            ),
            items: [
              const DropdownMenuItem<String?>(
                value: null,
                child: Text('No bot of its own'),
              ),
              if (orphan != null)
                DropdownMenuItem<String?>(
                  value: orphan,
                  child: Text(orphan, style: mono.copyWith(fontSize: 12.5)),
                ),
              for (final s in list ?? const <SecretStatus>[])
                DropdownMenuItem<String?>(
                  value: s.name,
                  enabled: _why(s) == null,
                  child: Row(
                    children: [
                      Flexible(
                        child: Text(
                          s.name,
                          overflow: TextOverflow.ellipsis,
                          style: mono.copyWith(
                            fontSize: 12.5,
                            color: _why(s) == null ? null : c.muted,
                          ),
                        ),
                      ),
                      if (_why(s) case final why?) ...[
                        const SizedBox(width: 8),
                        Text(
                          why,
                          style: TextStyle(fontSize: 11.5, color: c.muted),
                        ),
                      ],
                    ],
                  ),
                ),
            ],
            onChanged: onChanged,
          ),
        if (loadError case final e?)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              "Couldn't list your secrets: $e",
              style: TextStyle(fontSize: 12, color: c.error),
            ),
          ),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            key: const Key('agent-bot-set-secret'),
            onPressed: onSetSecret,
            style: TextButton.styleFrom(
              padding: EdgeInsets.zero,
              minimumSize: const Size(0, 30),
              visualDensity: VisualDensity.compact,
            ),
            child: const Text('Add the bot token as a secret…'),
          ),
        ),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(Icons.restart_alt, size: 14, color: c.muted),
            const SizedBox(width: 6),
            Expanded(
              child: Text(
                'Bots start with the daemon: a change here applies when the '
                'daemon restarts. Until then, its old bot (or none) stays.',
                key: const Key('agent-bot-restart-note'),
                style: small,
              ),
            ),
          ],
        ),
      ],
    );
  }
}
