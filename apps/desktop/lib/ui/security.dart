import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import '../state/store.dart';
import '../theme.dart';

Future<void> showSecurityAudit(BuildContext context, TernStore store) =>
    showDialog<void>(
      context: context,
      builder: (_) => SecurityAuditDialog(store: store),
    );

Color severityColor(TernColors c, String severity) => switch (severity) {
  'high' => c.error,
  'medium' => c.warn,
  'low' => c.text,
  _ => c.muted,
};

/// `tern security audit` (tooling spec §8.4), run on the core: how exposed
/// it is and what could act without asking, most severe first.
class SecurityAuditDialog extends StatefulWidget {
  const SecurityAuditDialog({super.key, required this.store});

  final TernStore store;

  @override
  State<SecurityAuditDialog> createState() => _SecurityAuditDialogState();
}

class _SecurityAuditDialogState extends State<SecurityAuditDialog> {
  SecurityAudit? _audit;
  String? _error;
  var _showInfo = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final a = await widget.store.client.securityAudit();
      if (mounted) setState(() => _audit = a);
    } on TernException catch (e) {
      if (mounted) setState(() => _error = e.message);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final audit = _audit;
    final shown = [
      for (final f in audit?.findings ?? const <AuditFinding>[])
        if (_showInfo || f.severity != 'info') f,
    ];
    final hidden = (audit?.findings.length ?? 0) - shown.length;
    return Dialog(
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(14)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 620, maxHeight: 640),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 20, 22, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              const Text(
                'Security audit',
                style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
              ),
              const SizedBox(height: 4),
              Text(
                audit == null
                    ? 'Checking listeners, devices, autonomy, routines, '
                          'connectors and connections…'
                    : audit.summary == 'nothing to fix'
                    ? 'Nothing to fix.'
                    : 'To review: ${audit.summary}.',
                style: TextStyle(fontSize: 12.5, color: c.muted),
              ),
              const SizedBox(height: 12),
              if (_error case final e?)
                Text(e, style: TextStyle(color: c.error, fontSize: 12.5)),
              if (audit == null && _error == null)
                const Padding(
                  padding: EdgeInsets.all(24),
                  child: Center(child: CircularProgressIndicator()),
                ),
              if (audit != null)
                Flexible(
                  child: ListView(
                    shrinkWrap: true,
                    children: [
                      for (final f in shown) AuditFindingRow(finding: f),
                      if (hidden > 0)
                        Align(
                          alignment: Alignment.centerLeft,
                          child: TextButton(
                            onPressed: () => setState(() => _showInfo = true),
                            child: Text('Show $hidden more (for information)'),
                          ),
                        ),
                    ],
                  ),
                ),
              const SizedBox(height: 8),
              Row(
                children: [
                  Text(
                    'Also: tern security audit',
                    style: TextStyle(fontSize: 11.5, color: c.muted),
                  ),
                  const Spacer(),
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Done'),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class AuditFindingRow extends StatelessWidget {
  const AuditFindingRow({super.key, required this.finding});

  final AuditFinding finding;

  @override
  Widget build(BuildContext context) {
    final c = context.colors;
    final f = finding;
    final color = severityColor(c, f.severity);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 7),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Container(
            width: 58,
            padding: const EdgeInsets.symmetric(vertical: 2),
            decoration: BoxDecoration(
              color: color.withValues(alpha: 0.1),
              borderRadius: BorderRadius.circular(5),
            ),
            child: Text(
              f.severity,
              textAlign: TextAlign.center,
              style: TextStyle(
                fontSize: 11,
                fontWeight: FontWeight.w600,
                color: color,
              ),
            ),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(f.title, style: const TextStyle(fontSize: 13)),
                const SizedBox(height: 2),
                Text(
                  [f.area, ?f.fix].join(' · '),
                  style: TextStyle(fontSize: 11.5, color: c.muted),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
