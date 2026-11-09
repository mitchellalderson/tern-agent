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

/// An active `wait_until` (docs/wait-until-design.md): a one-shot check
/// tied to a conversation, polled by the daemon with no model call.
class Wait {
  const Wait({
    required this.id,
    required this.note,
    required this.agentId,
    required this.sessionId,
    this.channelId,
    this.target = '',
    this.until,
    this.expiresAt,
    this.lastCheck,
  });

  factory Wait.fromJson(Json j) => Wait(
    id: j['id'] as String,
    note: j['note'] as String,
    agentId: j['agent_id'] as String,
    sessionId: j['session_id'] as String,
    channelId: j['channel_id'] as String?,
    target: j['target'] as String? ?? '',
    until: j['until'] as String?,
    expiresAt: switch (j['expires_at']) {
      final int ms => DateTime.fromMillisecondsSinceEpoch(ms),
      _ => null,
    },
    lastCheck: j['last_check'] as String?,
  );

  final String id;
  final String note;
  final String agentId;
  final String sessionId;
  final String? channelId;

  /// What it checks: `github.run_status`, `example.com`, `time`.
  final String target;

  /// `status == completed`; null for a time wait.
  final String? until;

  /// When it gives up.
  final DateTime? expiresAt;
  final String? lastCheck;
}
