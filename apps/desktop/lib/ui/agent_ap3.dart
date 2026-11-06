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
    this.codeAuto = false,
    this.codeEnabled = true,
  });

  /// False for a read-only agent: it can't opt in to code auto mode.
  final bool codeEnabled;

  /// `readonly`, `supervised`, `code`, `autonomous`, `yolo`; null = no
  /// ceiling. `code` is code auto mode: choosing it is the per-agent opt-in
  /// (the sheet asks to confirm on save), and it is not a plain cap.
  final String? value;
  final ValueChanged<String?> onChanged;

  /// Whether code auto mode is (or will be) on for this agent.
  final bool codeAuto;

  static const _none = 'none';

  /// The scale's plain-word label and one line per level, lowest first. The
  /// wire name shows beside it so it maps to config and the CLI.
  static const _copy = <AutonomyLevel, (String, String)>{
    AutonomyLevel.readonly: (
      'Read only',
      'Looks, never changes anything.',
    ),
    AutonomyLevel.supervised: (
      'Ask first',
      'Asks before it changes anything. tern\'s default.',
    ),
    AutonomyLevel.code: (
      'Code',
      'Edits files and runs build and test commands in its folder without '
          'asking.',
    ),
    AutonomyLevel.autonomous: (
      'Mostly on its own',
      'Writes without asking unless it read untrusted content. Destructive '
          'actions still ask.',
    ),
    AutonomyLevel.yolo: (
      'Full',
      'Nothing asks. Forbidden tools stay forbidden.',
    ),
  };

  @override
  Widget build(BuildContext context) {
    final small = Theme.of(context).textTheme.bodySmall;
    final muted = context.colors.muted;
    Widget title(String label, String? wire) => Row(
      children: [
        Flexible(child: Text(label, style: const TextStyle(fontSize: 13))),
        if (wire != null) ...[
          const SizedBox(width: 8),
          Text(wire, style: mono.copyWith(fontSize: 11, color: muted)),
        ],
      ],
    );
    Widget end(String label) => Text(
      label,
      style: small?.copyWith(color: muted, fontWeight: FontWeight.w600),
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: Text(
                'Low asks you before anything; high acts without asking. '
                'This caps your global setting and never raises it.',
                key: const Key('autonomy-explainer'),
                style: small,
              ),
            ),
            Tooltip(
              message:
                  'Sub-agents it starts get the lower of this level and '
                  'their own.',
              child: Padding(
                padding: const EdgeInsets.only(left: 8, top: 1),
                child: Icon(Icons.info_outline, size: 14, color: muted),
              ),
            ),
          ],
        ),
        const SizedBox(height: 8),
        RadioGroup<String>(
          groupValue: AutonomyLevel.parse(value)?.wire ?? _none,
          onChanged: (v) => onChanged(v == null || v == _none ? null : v),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              RadioListTile<String>(
                key: const Key('autonomy-none'),
                value: _none,
                dense: true,
                contentPadding: EdgeInsets.zero,
                title: title('Use my global setting (default)', null),
                subtitle: Text(
                  'No cap for this agent.',
                  style: small,
                ),
              ),
              const SizedBox(height: 16),
              end('Low'),
              for (final l in AutonomyLevel.values)
                RadioListTile<String>(
                  key: Key('autonomy-${l.wire}'),
                  value: l.wire,
                  enabled: l != AutonomyLevel.code || codeEnabled,
                  dense: true,
                  contentPadding: EdgeInsets.zero,
                  title: title(_copy[l]!.$1, l.wire),
                  subtitle: l == AutonomyLevel.code && !codeEnabled
                      ? Text(
                          'A read-only agent can\'t use code auto mode.',
                          key: const Key('autonomy-code-disabled'),
                          style: small,
                        )
                      : l == AutonomyLevel.code
                      ? Text(
                          '${_copy[l]!.$2} Not fully sandboxed: it can '
                          'change or delete anything in its folder. You '
                          'confirm before it turns on, and a snapshot is '
                          'saved before its first change.'
                          '${codeAuto && value == 'code' ? ' Turned on when you save.' : ''}',
                          key: const Key('autonomy-code-copy'),
                          style: small,
                        )
                      : Text(_copy[l]!.$2, style: small),
                ),
              end('High'),
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
      child: Padding(
        key: const Key('autonomy-chip'),
        padding: const EdgeInsets.only(right: 8),
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
