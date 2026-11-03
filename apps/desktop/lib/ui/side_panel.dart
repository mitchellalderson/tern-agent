import 'package:flutter/material.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'widgets.dart';

/// What the agent is doing (spec §2.3). Routines, connectors and memory
/// join it once the engine exposes them.
class SidePanel extends StatelessWidget {
  const SidePanel({super.key, required this.store, required this.onClose});

  final TernStore store;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final t = store.current;
    final work = t?.items.whereType<WorkItem>().lastOrNull;
    final usage = store.selected == null ? null : store.usage[store.selected];
    final tools = store.info?.tools ?? const [];
    final labels = Theme.of(context).textTheme.labelSmall;
    return Container(
      color: c.sidebar,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          WindowDragArea(
            child: Container(
              height: titleBarInset + 12,
              padding: const EdgeInsets.fromLTRB(18, 0, 8, 0),
              child: Row(
                children: [
                  const Expanded(
                    child: Text(
                      'Activity',
                      style: TextStyle(
                        fontSize: 14,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                  ),
                  QuietIconButton(
                    icon: Icons.close,
                    tooltip: 'Close',
                    onPressed: onClose,
                  ),
                ],
              ),
            ),
          ),
          const Hairline(),
          Expanded(
            child: ListView(
              padding: const EdgeInsets.fromLTRB(18, 16, 18, 18),
              children: [
                Text('NOW', style: labels),
                const SizedBox(height: 8),
                if (t?.activeRun == null && work == null)
                  Text('Idle', style: Theme.of(context).textTheme.bodySmall)
                else ...[
                  Text(
                    t?.activeRun != null ? 'Running' : 'Last run',
                    style: TextStyle(fontSize: 12.5, color: c.muted),
                  ),
                  const SizedBox(height: 6),
                  for (final s in work?.steps ?? const <ToolStep>[])
                    Padding(
                      padding: const EdgeInsets.symmetric(vertical: 3),
                      child: Row(
                        children: [
                          Icon(
                            switch (s.status) {
                              StepStatus.ok => Icons.check,
                              StepStatus.error => Icons.close,
                              StepStatus.running => Icons.more_horiz,
                            },
                            size: 14,
                            color: switch (s.status) {
                              StepStatus.ok => c.ok,
                              StepStatus.error => c.error,
                              StepStatus.running => c.muted,
                            },
                          ),
                          const SizedBox(width: 8),
                          Expanded(
                            child: Text(
                              s.summary,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: const TextStyle(fontSize: 12.5),
                            ),
                          ),
                        ],
                      ),
                    ),
                ],
                const SizedBox(height: 22),
                Text('USAGE', style: labels),
                const SizedBox(height: 8),
                if (usage == null || usage.total == 0)
                  Text(
                    'No tokens yet',
                    style: Theme.of(context).textTheme.bodySmall,
                  )
                else ...[
                  _Stat('Input', formatTokens(usage.inputTokens)),
                  _Stat('Output', formatTokens(usage.outputTokens)),
                  if (usage.cacheReadTokens > 0)
                    _Stat('Cache read', formatTokens(usage.cacheReadTokens)),
                ],
                const SizedBox(height: 22),
                Text('TOOLS · ${tools.length}', style: labels),
                const SizedBox(height: 8),
                Wrap(
                  spacing: 6,
                  runSpacing: 6,
                  children: [
                    for (final tool in tools)
                      Tooltip(
                        message: tool.description,
                        child: Container(
                          padding: const EdgeInsets.symmetric(
                            horizontal: 8,
                            vertical: 3,
                          ),
                          decoration: BoxDecoration(
                            color: c.background,
                            border: Border.all(color: c.hairline),
                            borderRadius: BorderRadius.circular(6),
                          ),
                          child: Text(
                            tool.name,
                            style: mono.copyWith(fontSize: 11, color: c.muted),
                          ),
                        ),
                      ),
                  ],
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _Stat extends StatelessWidget {
  const _Stat(this.label, this.value);

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.symmetric(vertical: 2),
    child: Row(
      children: [
        Expanded(
          child: Text(
            label,
            style: TextStyle(fontSize: 12.5, color: context.colors.muted),
          ),
        ),
        Text(value, style: const TextStyle(fontSize: 12.5)),
      ],
    ),
  );
}
