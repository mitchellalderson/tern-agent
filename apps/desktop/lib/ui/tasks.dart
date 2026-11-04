import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';
import 'ledger.dart' show LibraryHeader;
import 'widgets.dart';

const _headings = {
  'needs_you': 'Needs you',
  'running': 'Running',
  'queued': 'Queued',
  'scheduled': 'Scheduled',
  'failed': 'Failed recently',
};

IconData taskIcon(TaskItem t) => switch (t.kind) {
  'approval' => Icons.how_to_reg_outlined,
  'outbox' => Icons.outbox_outlined,
  'secret_request' => Icons.key_outlined,
  'sign_in' => Icons.login,
  'events' => Icons.inbox_outlined,
  'delivery' => Icons.schedule_send_outlined,
  'routine' => Icons.schedule,
  'routine_paused' => Icons.pause_circle_outline,
  _ when t.section == 'failed' => Icons.error_outline,
  _ when t.section == 'queued' => Icons.hourglass_empty,
  _ => Icons.play_circle_outline,
};

/// `in 3h`, `5m ago`.
String taskWhen(DateTime at, DateTime now) {
  final d = at.difference(now);
  final future = !d.isNegative;
  final mins = d.abs().inMinutes;
  if (mins == 0) return future ? 'now' : 'just now';
  final s = mins < 60
      ? '${mins}m'
      : mins < 48 * 60
      ? '${mins ~/ 60}h'
      : '${mins ~/ (24 * 60)}d';
  return future ? 'in $s' : '$s ago';
}

/// Library → Tasks (tooling spec §8.2): everything running, queued,
/// scheduled, waiting on you, or failed recently, across agents and
/// channels. Each row opens where the item lives.
class TasksView extends StatefulWidget {
  const TasksView({
    super.key,
    required this.store,
    required this.panelOpen,
    required this.onTogglePanel,
    this.refreshEvery = const Duration(seconds: 4),
    this.now,
  });

  final TernStore store;
  final bool panelOpen;
  final VoidCallback onTogglePanel;
  final Duration refreshEvery;

  /// For tests and screenshots.
  final DateTime Function()? now;

  @override
  State<TasksView> createState() => _TasksViewState();
}

class _TasksViewState extends State<TasksView> {
  Tasks? _tasks;
  String? _error;
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _load();
    _timer = Timer.periodic(widget.refreshEvery, (_) => _load());
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final t = await widget.store.client.tasks();
      if (mounted) {
        setState(() {
          _tasks = t;
          _error = null;
        });
      }
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  /// Requests live in their run's conversation; runs open as a thread;
  /// routine items open the Routines page.
  void _open(TaskItem t) {
    final s = widget.store;
    switch (t) {
      case TaskItem(section: 'needs_you', :final channelId?):
        s.openConversation(channelId);
      case TaskItem(kind: 'routine' || 'routine_paused' || 'events'):
        s.openRoutines();
      case TaskItem(:final sessionId?):
        s.openThread(sessionId);
      case TaskItem(routineId: _?):
        s.openRoutines();
      default:
        break;
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final tasks = _tasks;
    final now = widget.now?.call() ?? DateTime.now();
    return Column(
      children: [
        LibraryHeader(
          title: 'Tasks',
          panelOpen: widget.panelOpen,
          onTogglePanel: widget.onTogglePanel,
          actions: [
            QuietIconButton(
              icon: Icons.refresh,
              tooltip: 'Refresh',
              onPressed: _load,
            ),
          ],
        ),
        const Hairline(),
        Expanded(
          child: ListView(
            padding: const EdgeInsets.fromLTRB(24, 12, 24, 24),
            children: [
              if (_error case final e?)
                Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
              if (tasks == null && _error == null)
                const Padding(
                  padding: EdgeInsets.all(24),
                  child: Center(child: CircularProgressIndicator()),
                )
              else if (tasks != null && tasks.items.isEmpty)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 24),
                  child: Text(
                    'Nothing running, queued, scheduled or waiting on you.',
                    style: TextStyle(color: c.muted),
                  ),
                ),
              if (tasks != null)
                for (final section in Tasks.sections)
                  ..._section(context, tasks.section(section), section, now),
            ],
          ),
        ),
      ],
    );
  }

  List<Widget> _section(
    BuildContext context,
    List<TaskItem> items,
    String section,
    DateTime now,
  ) {
    if (items.isEmpty) return const [];
    final c = context.colors;
    final listed = {for (final t in items) ?t.runId};
    final top = items.where(
      (t) => t.parentRunId == null || !listed.contains(t.parentRunId),
    );
    return [
      Padding(
        padding: const EdgeInsets.only(top: 14, bottom: 4),
        child: Row(
          children: [
            Text(
              _headings[section] ?? section,
              style: TextStyle(
                fontSize: 12,
                fontWeight: FontWeight.w600,
                color: section == 'needs_you' ? c.accent : c.muted,
              ),
            ),
            const SizedBox(width: 6),
            Text(
              '${items.length}',
              style: TextStyle(fontSize: 12, color: c.muted),
            ),
          ],
        ),
      ),
      for (final t in top) ...[
        TaskRow(task: t, now: now, onOpen: () => _open(t)),
        for (final kid in items.where(
          (k) => k.parentRunId != null && k.parentRunId == t.runId,
        ))
          Padding(
            padding: const EdgeInsets.only(left: 22),
            child: TaskRow(task: kid, now: now, onOpen: () => _open(kid)),
          ),
      ],
    ];
  }
}

class TaskRow extends StatelessWidget {
  const TaskRow({
    super.key,
    required this.task,
    required this.now,
    this.onOpen,
  });

  final TaskItem task;
  final DateTime now;
  final VoidCallback? onOpen;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final t = task;
    final color = switch (t.section) {
      'needs_you' => c.accent,
      'failed' => c.error,
      _ => c.muted,
    };
    final meta = [
      ?t.detail?.split('\n').first,
      if (t.at case final at?) taskWhen(at, now),
      ?t.agentId,
    ];
    return InkWell(
      borderRadius: BorderRadius.circular(8),
      onTap: onOpen,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 7, horizontal: 4),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Icon(taskIcon(t), size: 16, color: color),
            const SizedBox(width: 10),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Row(
                    children: [
                      Flexible(
                        child: Text(
                          t.title,
                          style: const TextStyle(fontSize: 13),
                          overflow: TextOverflow.ellipsis,
                        ),
                      ),
                      if (t.tainted) ...[
                        const SizedBox(width: 6),
                        const TaintMark(),
                      ],
                    ],
                  ),
                  if (meta.isNotEmpty) ...[
                    const SizedBox(height: 2),
                    Text(
                      meta.join(' · '),
                      style: TextStyle(fontSize: 11.5, color: c.muted),
                      maxLines: 2,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ],
                ],
              ),
            ),
            Icon(Icons.chevron_right, size: 16, color: c.muted),
          ],
        ),
      ),
    );
  }
}
