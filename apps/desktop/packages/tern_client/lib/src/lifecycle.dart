import 'models.dart';

/// `GET /v1/status`: one cheap summary of the daemon.
class DaemonStatus {
  const DaemonStatus({
    required this.version,
    required this.pid,
    required this.uptime,
    this.managedBy,
    this.runningRuns = 0,
    this.pendingApprovals = 0,
    this.activeSchedules = 0,
    this.gateways = const [],
  });

  factory DaemonStatus.fromJson(Json j) => DaemonStatus(
    version: j['version'] as String,
    pid: j['pid'] as int,
    uptime: Duration(seconds: j['uptime_secs'] as int? ?? 0),
    managedBy: j['managed_by'] as String?,
    runningRuns: j['running_runs'] as int? ?? 0,
    pendingApprovals: j['pending_approvals'] as int? ?? 0,
    activeSchedules: j['active_schedules'] as int? ?? 0,
    gateways: [
      for (final g in j['gateways'] as List? ?? const [])
        GatewayStatus.fromJson(g as Json),
    ],
  );

  final String version;
  final int pid;
  final Duration uptime;

  /// `launchd` or `systemd` when the service started it; null when started
  /// by hand or by the app.
  final String? managedBy;

  /// Top-level runs in progress.
  final int runningRuns;

  /// Approvals and secret requests waiting for an answer.
  final int pendingApprovals;
  final int activeSchedules;
  final List<GatewayStatus> gateways;
}

class GatewayStatus {
  const GatewayStatus({required this.name, required this.ok});

  factory GatewayStatus.fromJson(Json j) =>
      GatewayStatus(name: j['name'] as String, ok: j['ok'] as bool? ?? false);

  final String name;
  final bool ok;
}

/// A run that stopping tern pauses; it resumes on the next start.
class PausedRun {
  const PausedRun({
    required this.runId,
    required this.sessionId,
    required this.title,
  });

  factory PausedRun.fromJson(Json j) => PausedRun(
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    title: j['title'] as String? ?? 'a task',
  );

  final String runId;
  final String sessionId;

  /// The conversation's title, or the start of the message that began it.
  final String title;
}

/// `POST /v1/shutdown`'s answer.
class ShutdownReply {
  const ShutdownReply({required this.stopping, this.runs = const []});

  factory ShutdownReply.fromJson(Json j) => ShutdownReply(
    stopping: j['stopping'] as bool? ?? false,
    runs: [
      for (final r in j['runs'] as List? ?? const [])
        PausedRun.fromJson(r as Json),
    ],
  );

  /// False for a dry run.
  final bool stopping;
  final List<PausedRun> runs;
}
