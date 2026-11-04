import 'models.dart';

/// One item of the Tasks view (tooling spec §8.2): something running,
/// queued, scheduled, waiting on you, or failed recently.
class TaskItem {
  const TaskItem({
    required this.section,
    required this.kind,
    required this.id,
    required this.title,
    this.detail,
    this.at,
    this.runId,
    this.parentRunId,
    this.sessionId,
    this.channelId,
    this.agentId,
    this.routineId,
    this.tainted = false,
  });

  factory TaskItem.fromJson(Json j) => TaskItem(
    section: j['section'] as String,
    kind: j['kind'] as String,
    id: j['id'] as String,
    title: j['title'] as String,
    detail: j['detail'] as String?,
    at: switch (j['at']) {
      final int ms => DateTime.fromMillisecondsSinceEpoch(ms),
      _ => null,
    },
    runId: j['run_id'] as String?,
    parentRunId: j['parent_run_id'] as String?,
    sessionId: j['session_id'] as String?,
    channelId: j['channel_id'] as String?,
    agentId: j['agent_id'] as String?,
    routineId: j['routine_id'] as String?,
    tainted: j['tainted'] as bool? ?? false,
  );

  /// `needs_you`, `running`, `queued`, `scheduled` or `failed`.
  final String section;

  /// `approval`, `outbox`, `secret_request`, `sign_in`, `run`, `events`,
  /// `delivery`, `routine` or `routine_paused`.
  final String kind;
  final String id;
  final String title;
  final String? detail;

  /// When it started, is due, or failed.
  final DateTime? at;
  final String? runId;
  final String? parentRunId;

  /// Where it lives.
  final String? sessionId;
  final String? channelId;
  final String? agentId;
  final String? routineId;
  final bool tainted;
}

class Tasks {
  const Tasks({this.items = const [], this.counts = const {}});

  factory Tasks.fromJson(Json j) => Tasks(
    items: [for (final t in j['items'] as List) TaskItem.fromJson(t as Json)],
    counts: {
      for (final MapEntry(:key, :value) in (j['counts'] as Json).entries)
        key: value as int,
    },
  );

  final List<TaskItem> items;
  final Map<String, int> counts;

  static const sections = [
    'needs_you',
    'running',
    'queued',
    'scheduled',
    'failed',
  ];

  List<TaskItem> section(String s) =>
      items.where((t) => t.section == s).toList();
}
