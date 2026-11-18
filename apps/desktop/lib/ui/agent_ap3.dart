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
      'Asks before any change.',
    ),
    AutonomyLevel.code: (
      'Code',
      'Edits files and runs builds and tests without asking, unless it read '
          'untrusted content.',
    ),
    AutonomyLevel.autonomous: (
      'Mostly on its own',
      'The default. Writes without asking, unless it read untrusted content. '
          'Destructive actions still ask.',
    ),
    AutonomyLevel.yolo: (
      'Full',
      'Nothing asks.',
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
                'Low asks before anything; high doesn\'t. Caps your global '
                'setting, never raises it.',
                key: const Key('autonomy-explainer'),
                style: small,
              ),
            ),
            Tooltip(
              message:
                  'Sub-agents get the lower of this level and their own. '
                  'Untrusted content is anything from the web, MCP servers '
                  'or connectors. Full still can\'t use forbidden tools.',
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
                          '${_copy[l]!.$2} Not fully sandboxed. You confirm '
                          'before it turns on; a snapshot is saved first.'
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
