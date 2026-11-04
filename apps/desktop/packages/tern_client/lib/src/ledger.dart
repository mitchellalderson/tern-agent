import 'models.dart';

/// One entry of the activity ledger (tooling spec §8.1): an approval, a
/// grant, a write that ran, a connection added, an autonomy change, ….
/// Entries are hash-chained; [LedgerCheck] says whether the chain holds.
class LedgerEntry {
  const LedgerEntry({
    required this.seq,
    required this.at,
    required this.kind,
    required this.actor,
    required this.subject,
    required this.summary,
    this.detail,
    this.runId,
    this.sessionId,
    this.channelId,
    this.routineId,
    this.connector,
    this.hash = '',
  });

  factory LedgerEntry.fromJson(Json j) => LedgerEntry(
    seq: j['seq'] as int,
    at: DateTime.fromMillisecondsSinceEpoch(j['at'] as int),
    kind: j['kind'] as String,
    actor: j['actor'] as String,
    subject: j['subject'] as String,
    summary: j['summary'] as String,
    detail: j['detail'],
    runId: j['run_id'] as String?,
    sessionId: j['session_id'] as String?,
    channelId: j['channel_id'] as String?,
    routineId: j['routine_id'] as String?,
    connector: j['connector'] as String?,
    hash: j['hash'] as String? ?? '',
  );

  final int seq;
  final DateTime at;

  /// `approval`, `effect`, `grant.add`, `outbox.approve`, …
  final String kind;

  /// `owner` (you), `tern` (the host itself) or `agent:<id>`.
  final String actor;
  final String subject;
  final String summary;
  final Object? detail;
  final String? runId;
  final String? sessionId;
  final String? channelId;
  final String? routineId;
  final String? connector;
  final String hash;

  /// `grant` for `grant.add`.
  String get family => kind.split('.').first;
}

/// The hash chain, recomputed by the core.
class LedgerCheck {
  const LedgerCheck({
    required this.ok,
    required this.entries,
    required this.head,
    this.brokenAt,
    this.reason,
  });

  factory LedgerCheck.fromJson(Json j) {
    final broken = j['broken'] as Json?;
    return LedgerCheck(
      ok: j['ok'] as bool,
      entries: j['entries'] as int,
      head: j['head'] as String,
      brokenAt: broken?['seq'] as int?,
      reason: broken?['reason'] as String?,
    );
  }

  final bool ok;
  final int entries;
  final String head;

  /// The first entry that doesn't hold, and why.
  final int? brokenAt;
  final String? reason;
}
