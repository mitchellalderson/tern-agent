import 'models.dart';

/// A `write` or `destructive` call nobody approved in time, waiting for the
/// user (tooling spec §5.2). Approving runs it exactly once.
class OutboxItem {
  const OutboxItem({
    required this.id,
    required this.runId,
    required this.sessionId,
    required this.tool,
    required this.input,
    required this.summary,
    this.effect = 'write',
    this.taint,
    this.localOnly = false,
    required this.state,
    required this.createdAt,
    required this.expiresAt,
    this.note,
    this.edited = false,
    this.resultText,
    this.resultError = false,
  });

  factory OutboxItem.fromJson(Json j) {
    final result = j['result'] as Json?;
    return OutboxItem(
      id: j['id'] as int,
      runId: j['run_id'] as String,
      sessionId: j['session_id'] as String,
      tool: j['tool'] as String,
      input: j['input'],
      summary: j['summary'] as String,
      effect: j['effect'] as String? ?? 'write',
      taint: j['taint'] as String?,
      localOnly: j['local_only'] as bool? ?? false,
      state: j['state'] as String,
      createdAt: DateTime.fromMillisecondsSinceEpoch(j['created_at'] as int),
      expiresAt: DateTime.fromMillisecondsSinceEpoch(j['expires_at'] as int),
      note: j['note'] as String?,
      edited: j['edited'] as bool? ?? false,
      resultText: result?['content'] as String?,
      resultError: result?['is_error'] as bool? ?? false,
    );
  }

  final int id;
  final String runId;
  final String sessionId;
  final String tool;

  /// The arguments it runs with (the user's edit, if any).
  final Object? input;
  final String summary;

  /// `write` or `destructive`.
  final String effect;

  /// Why the run that queued it was tainted, if it was.
  final String? taint;

  /// Only a client on this computer may decide it.
  final bool localOnly;

  /// `pending`, `approved`, `done`, `failed`, `rejected` or `expired`.
  final String state;
  final DateTime createdAt;
  final DateTime expiresAt;
  final String? note;
  final bool edited;
  final String? resultText;
  final bool resultError;

  bool get pending => state == 'pending';

  /// `write · gmail_send …`, as approval cards show a call.
  String get headline => '$effect · $summary';

  String? get taintNote =>
      taint == null ? null : 'this run read untrusted content ($taint)';
}
