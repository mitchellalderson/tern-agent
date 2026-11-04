import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'widgets.dart';

/// RECENT WORK in an agent's side panel: work it did for other agents
/// (delegated batches) and its routine runs, with status, who asked and
/// when. Each opens where it ran.
class RecentWorkSection extends StatefulWidget {
  const RecentWorkSection({super.key, required this.store});

  final TernStore store;

  @override
  State<RecentWorkSection> createState() => _RecentWorkSectionState();
}

class _RecentWorkSectionState extends State<RecentWorkSection> {
  String? _loadedFor;
  List<AgentWork>? _work;

  Future<void> _load(String agent, String key) async {
    _loadedFor = key;
    try {
      final w = await widget.store.client.agentWork(agent, limit: 8);
      if (mounted && _loadedFor == key) setState(() => _work = w);
    } on TernException {
      if (mounted && _loadedFor == key) setState(() => _work = const []);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final store = widget.store;
    final agent = store.streamAgent;
    if (agent == null) return const SizedBox.shrink();
    final key = '${agent.id}:${store.workVersion}';
    if (key != _loadedFor) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (mounted && _loadedFor != key) _load(agent.id, key);
      });
    }
    final work = _work ?? const <AgentWork>[];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text('RECENT WORK', style: Theme.of(context).textTheme.labelSmall),
        const SizedBox(height: 6),
        if (work.isEmpty)
          Text(
            'Nothing yet: work other agents hand ${agent.name}, and its '
            'routines, show here.',
            style: Theme.of(context).textTheme.bodySmall,
          )
        else
          for (final w in work)
            InkWell(
              borderRadius: BorderRadius.circular(6),
              onTap: () => store.openThread(w.sessionId),
              child: Padding(
                padding: const EdgeInsets.symmetric(vertical: 5),
                child: Row(
                  children: [
                    SizedBox(
                      width: 16,
                      child: switch (w.status) {
                        'running' => SizedBox.square(
                          dimension: 10,
                          child: CircularProgressIndicator(
                            strokeWidth: 1.5,
                            color: c.muted,
                          ),
                        ),
                        'failed' => Icon(Icons.close, size: 14, color: c.error),
                        _ => Icon(Icons.check, size: 14, color: c.ok),
                      },
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            w.title.isEmpty ? '(no brief)' : w.title,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(fontSize: 12.5),
                          ),
                          Text(
                            [
                              if (w.kind == 'routine')
                                'routine'
                              else
                                'for ${w.caller ?? 'another agent'}',
                              if (w.tasks > 1) '${w.tasks} tasks',
                              relativeTime(w.startedAt),
                            ].join(' · '),
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(fontSize: 11.5, color: c.muted),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
      ],
    );
  }
}
