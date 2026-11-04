// Routines (T1a): the list shows budget pauses and quiet runs, and the
// editor sends quiet hours, urgent and caps.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/routines.dart';

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final updates =
      <({String id, ScheduleLimits? limits, bool? enabled, bool resume})>[];
  var list = <Schedule>[];

  @override
  Future<List<Schedule>> schedules() async => list;

  @override
  Future<CronPreview> previewCron(String cron) async =>
      CronPreview(cron: cron, next: [DateTime(2026, 11, 4, 8)]);

  @override
  Future<Schedule> updateSchedule(
    String id, {
    String? cron,
    String? prompt,
    bool? enabled,
    String? deliverTo,
    ScheduleLimits? limits,
    bool resume = false,
    String? channel,
    String? agent,
  }) async {
    updates.add((id: id, limits: limits, enabled: enabled, resume: resume));
    return list.firstWhere((s) => s.id == id, orElse: () => list.first);
  }
}

final _tomorrow = () {
  final n = DateTime.now();
  return DateTime(n.year, n.month, n.day + 1);
}();

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: child),
);

void main() {
  testWidgets('the list shows budget pauses and quiet runs', (tester) async {
    final client = _FakeClient();
    final store = TernStore(client);
    client.list = store.schedules = [
      Schedule(
        id: 'a',
        cron: '0 * * * *',
        prompt: 'Check the feed',
        sessionId: 's1',
        enabled: true,
        paused: 'budget',
        pausedUntil: _tomorrow,
        pausedReason: 'runs_per_day',
      ),
      const Schedule(
        id: 'b',
        cron: '0 8 * * *',
        prompt: 'Inbox sweep',
        sessionId: 's2',
        enabled: true,
        lastStatus: 'completed',
        lastQuiet: true,
      ),
    ];
    await tester.pumpWidget(
      _app(SizedBox(width: 320, child: RoutinesSection(store: store))),
    );
    expect(find.text('Paused: budget · resumes tomorrow'), findsOneWidget);
    expect(find.textContaining('Last run quiet'), findsOneWidget);

    // "Resume" on a budget pause lifts it.
    await tester.tap(find.byTooltip('Routine actions').first);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Resume'));
    await tester.pumpAndSettle();
    expect(client.updates.single.id, 'a');
    expect(client.updates.single.enabled, isTrue);
    expect(client.updates.single.resume, isTrue);
  });

  testWidgets('the editor sends quiet hours, urgent and caps', (tester) async {
    tester.view.physicalSize = const Size(700, 1100) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final client = _FakeClient();
    const editing = Schedule(
      id: 'a',
      cron: '0 8 * * *',
      prompt: 'Inbox sweep',
      sessionId: 's',
      enabled: true,
      limits: ScheduleLimits(quietHours: '23:00-06:00', runsPerDay: 4),
    );
    client.list = [editing];
    final store = TernStore(client);
    await tester.pumpWidget(_app(RoutineSheet(store: store, editing: editing)));
    await tester.pumpAndSettle();
    // Non-default limits open the section, filled in.
    expect(find.text('QUIET HOURS'), findsOneWidget);
    expect(find.text('23:00-06:00'), findsOneWidget);
    expect(find.text('4'), findsOneWidget);

    await tester.enterText(
      find.widgetWithText(TextField, '23:00-06:00'),
      'late',
    );
    await tester.pump();
    expect(find.text('Use HH:MM-HH:MM, e.g. 22:00-07:30'), findsOneWidget);
    final save = find.widgetWithText(FilledButton, 'Save');
    expect(tester.widget<FilledButton>(save).onPressed, isNull);

    await tester.tap(find.text('Off'));
    await tester.tap(find.text('Urgent: send even during quiet hours'));
    await tester.enterText(find.widgetWithText(TextField, '4'), '');
    final fields = find.widgetWithText(TextField, 'default');
    await tester.enterText(fields.at(1), '0.25');
    await tester.enterText(fields.at(2), '50000');
    await tester.pump();
    await tester.tap(save);
    await tester.pumpAndSettle();

    final sent = client.updates.single.limits!;
    expect(sent.toJson(), {
      'quiet_hours': 'none',
      'urgent': true,
      'runs_per_day': null,
      'usd_per_day': 0.25,
      'tokens_per_run': 50000,
    });
  });
}
