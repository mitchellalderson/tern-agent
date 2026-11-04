// `tern security audit` in the desktop app (tooling spec §8.4).
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/security.dart';

final sampleAudit = SecurityAudit.fromJson({
  'findings': [
    {
      'severity': 'high',
      'area': 'autonomy',
      'title': 'autonomy = "yolo": nothing asks',
      'fix': 'Set autonomy = "supervised" unless you mean it.',
    },
    {
      'severity': 'medium',
      'area': 'connectors',
      'title': 'connector weather reads WEATHER_API_KEY itself with ctx.secret',
      'subject': 'weather',
    },
    {
      'severity': 'low',
      'area': 'devices',
      'title': 'device phone (owner scope): everything',
    },
    {
      'severity': 'info',
      'area': 'listeners',
      'title': 'API: the local socket only (no TCP listener)',
    },
  ],
  'high': 1,
  'medium': 1,
  'low': 1,
});

class FakeAuditClient extends TernClient {
  FakeAuditClient() : super(const Endpoint.unix('/nonexistent'));

  @override
  Future<SecurityAudit> securityAudit() async => sampleAudit;
}

void main() {
  test('decodes the audit and sums it up', () {
    expect(sampleAudit.summary, '1 high, 1 medium, 1 low');
    expect(sampleAudit.findings[1].subject, 'weather');
    expect(const SecurityAudit().summary, 'nothing to fix');
  });

  testWidgets('lists findings, info hidden until asked', (tester) async {
    final store = TernStore(FakeAuditClient());
    await tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: Scaffold(body: SecurityAuditDialog(store: store)),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('To review: 1 high, 1 medium, 1 low.'), findsOneWidget);
    expect(find.text('autonomy = "yolo": nothing asks'), findsOneWidget);
    expect(find.textContaining('local socket only'), findsNothing);
    await tester.tap(find.text('Show 1 more (for information)'));
    await tester.pumpAndSettle();
    expect(find.textContaining('local socket only'), findsOneWidget);
  });
}
