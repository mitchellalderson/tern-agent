import 'models.dart';

/// A sub-agent: a run another run started with `agent_spawn`, in its own
/// session.
class ChildRun {
  const ChildRun({
    required this.runId,
    required this.sessionId,
    required this.parentRunId,
    required this.callId,
    required this.label,
    required this.task,
    this.model,
    required this.depth,
    required this.status,
    this.error,
    required this.startedAt,
    this.finishedAt,
    required this.usage,
    this.resultPreview,
    this.pendingQuestion,
    this.agentId,
  });

  factory ChildRun.fromJson(Json j) => ChildRun(
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    parentRunId: j['parent_run_id'] as String,
    callId: j['call_id'] as String? ?? '',
    label: j['label'] as String,
    task: j['task'] as String? ?? '',
    model: j['model'] as String?,
    depth: j['depth'] as int? ?? 1,
    status: j['status'] as String,
    error: j['error'] as String?,
    startedAt: _time(j['started_at'] as int),
    finishedAt: switch (j['finished_at']) {
      final int t => _time(t),
      _ => null,
    },
    usage: j['usage'] is Json
        ? Usage.fromJson(j['usage'] as Json)
        : const Usage(),
    resultPreview: j['result_preview'] as String?,
    pendingQuestion: j['pending_question'] as String?,
    agentId: j['agent_id'] as String?,
  );

  final String runId;
  final String sessionId;
  final String parentRunId;

  /// The `agent_spawn` tool call that started it.
  final String callId;
  final String label;
  final String task;
  final String? model;
  final int depth;

  /// `queued`, `running`, `question`, `completed`, `failed`, `interrupted`.
  final String status;
  final String? error;
  final DateTime startedAt;
  final DateTime? finishedAt;
  final Usage usage;
  final String? resultPreview;
  final String? pendingQuestion;

  /// The agent profile it runs as, when its parent delegated to one.
  final String? agentId;
}

DateTime _time(int t) =>
    DateTime.fromMillisecondsSinceEpoch(t > 100000000000 ? t : t * 1000);
