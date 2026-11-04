import 'models.dart';

/// One finding of `tern security audit` (tooling spec §8.4).
class AuditFinding {
  const AuditFinding({
    required this.severity,
    required this.area,
    required this.title,
    this.fix,
    this.subject,
  });

  factory AuditFinding.fromJson(Json j) => AuditFinding(
    severity: j['severity'] as String,
    area: j['area'] as String,
    title: j['title'] as String,
    fix: j['fix'] as String?,
    subject: j['subject'] as String?,
  );

  /// `high`, `medium`, `low` or `info`.
  final String severity;

  /// `listeners`, `devices`, `autonomy`, `routines`, `connectors`, …
  final String area;
  final String title;
  final String? fix;
  final String? subject;
}

class SecurityAudit {
  const SecurityAudit({
    this.findings = const [],
    this.high = 0,
    this.medium = 0,
    this.low = 0,
  });

  factory SecurityAudit.fromJson(Json j) => SecurityAudit(
    findings: [
      for (final f in j['findings'] as List) AuditFinding.fromJson(f as Json),
    ],
    high: j['high'] as int,
    medium: j['medium'] as int,
    low: j['low'] as int,
  );

  /// Most severe first.
  final List<AuditFinding> findings;
  final int high;
  final int medium;
  final int low;

  /// `2 high, 1 medium`, or `nothing to fix`.
  String get summary {
    final parts = [
      if (high > 0) '$high high',
      if (medium > 0) '$medium medium',
      if (low > 0) '$low low',
    ];
    return parts.isEmpty ? 'nothing to fix' : parts.join(', ');
  }
}
