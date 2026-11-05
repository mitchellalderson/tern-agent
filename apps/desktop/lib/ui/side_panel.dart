import 'package:flutter/material.dart';

import '../state/store.dart';
import '../theme.dart';
import 'inspector.dart';
import 'memory_section.dart';
import 'recent_work.dart';
import 'routines.dart';
import 'tools.dart';
import 'widgets.dart';

/// The agent's routines, usage, tools and memory (agent profiles §7.2). What
/// it is doing right now is already in the conversation, so it isn't repeated
/// here; "All runs…" opens the journal.
class SidePanel extends StatelessWidget {
  const SidePanel({super.key, required this.store, required this.onClose});

  final TernStore store;
  final VoidCallback onClose;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final usage = store.selected == null ? null : store.usage[store.selected];
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
                if (store.selected case final session?
                    when (store.sessions.any(
                      (s) => s.id == session && s.runs > 0,
                    )))
                  Align(
                    alignment: Alignment.centerLeft,
                    child: TextButton(
                      onPressed: () =>
                          showInspector(context, store, session: session),
                      style: TextButton.styleFrom(
                        foregroundColor: c.muted,
                        padding: const EdgeInsets.symmetric(horizontal: 6),
                      ),
                      child: const Text('All runs…'),
                    ),
                  ),
                if (store.hasAgents && store.streamAgent != null) ...[
                  const SizedBox(height: 22),
                  RecentWorkSection(store: store),
                ],
                const SizedBox(height: 22),
                RoutinesSection(store: store),
                const SizedBox(height: 16),
                Text('Usage', style: labels),
                const SizedBox(height: 8),
                if (usage == null || usage.total == 0)
                  Text(
                    'No tokens yet',
                    style: Theme.of(context).textTheme.bodySmall,
                  )
                else ...[
                  Text(
                    [
                      '${formatTokens(usage.inputTokens)} in',
                      '${formatTokens(usage.outputTokens)} out',
                      if (usage.cacheReadTokens > 0)
                        '${formatTokens(usage.cacheReadTokens)} cached',
                    ].join(' · '),
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  if ((usage.contextTokens, usage.contextWindow) case (
                    final used?,
                    final window?,
                  ))
                    Text(
                      'Context ${formatTokens(used)} / ${formatTokens(window)}',
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                ],
                const SizedBox(height: 22),
                ToolsSection(store: store),
                if (store.hasAgents) ...[
                  const SizedBox(height: 22),
                  MemorySection(store: store),
                ],
              ],
            ),
          ),
        ],
      ),
    );
  }
}
