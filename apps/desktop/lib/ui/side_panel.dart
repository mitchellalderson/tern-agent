import 'package:flutter/material.dart';

import '../state/store.dart';
import '../state/transcript.dart';
import '../theme.dart';
import 'agents.dart';
import 'inspector.dart';
import 'routines.dart';
import 'tools.dart';
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
                    InkWell(
                      borderRadius: BorderRadius.circular(6),
                      // Into the run's journal: full input, output, timing.
                      onTap: switch ((store.selected, work?.runId)) {
                        (final String session, final String run) =>
                          () => showInspector(
                            context,
                            store,
                            session: session,
                            run: run,
                          ),
                        _ => null,
                      },
                      child: Padding(
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
                    ),
                ],
                if ((t?.items.whereType<AgentsItem>().lastOrNull?.children ??
                        const <AgentChild>[])
                    case final kids when kids.isNotEmpty) ...[
                  const SizedBox(height: 10),
                  Text(
                    'Agents',
                    style: TextStyle(fontSize: 12.5, color: c.muted),
                  ),
                  const SizedBox(height: 4),
                  for (final k in kids)
                    InkWell(
                      borderRadius: BorderRadius.circular(6),
                      onTap: switch ((store.selected, k.runId)) {
                        (final String session, final String run) =>
                          () => showInspector(
                            context,
                            store,
                            session: session,
                            run: run,
                          ),
                        _ => null,
                      },
                      child: Padding(
                        padding: const EdgeInsets.symmetric(vertical: 3),
                        child: Row(
                          children: [
                            SizedBox(width: 16, child: AgentGlyph(k.status)),
                            const SizedBox(width: 6),
                            Text(k.label, style: mono.copyWith(fontSize: 12)),
                            const SizedBox(width: 8),
                            Expanded(
                              child: Text(
                                agentActivity(k, store),
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: TextStyle(fontSize: 12, color: c.muted),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                ],
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
                const SizedBox(height: 22),
                RoutinesSection(store: store),
                const SizedBox(height: 16),
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
                ToolsSection(store: store),
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
