// Routines (T1a, T1b): the list shows budget pauses, quiet runs and
// triggers, and the editor sends quiet hours, urgent, caps, the trigger,
// initiative and gate.
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
  final sentOptions = <RoutineOptions?>[];

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
    RoutineOptions? options,
  }) async {
    sentOptions.add(options);
    updates.add((id: id, limits: limits, enabled: enabled, resume: resume));
    return list.firstWhere((s) => s.id == id, orElse: () => list.first);
  }
}

class _HookClient extends _FakeClient {
  final created = <({String prompt, String auth})>[];
  var rotated = 0;
  var revoked = 0;
  var hook = Webhook(
    routineId: 'h',
    auth: 'hmac',
    path: '/v1/hooks/h',
    url: 'https://hooks.example.com/v1/hooks/h',
    createdAt: DateTime(2026, 11, 4),
  );

  @override
  Future<CreatedWebhook> createWebhook({
    required String prompt,
    String auth = 'hmac',
    String? deliverTo,
    ScheduleLimits? limits,
    String? channel,
    String? agent,
    RoutineOptions? options,
  }) async {
    created.add((prompt: prompt, auth: auth));
    return CreatedWebhook(
      schedule: Schedule(
        id: 'h',
        cron: '',
        prompt: prompt,
        sessionId: 's',
        enabled: true,
        trigger: 'webhook',
      ),
      webhook: Webhook(
        routineId: 'h',
        auth: auth,
        path: '/v1/hooks/h',
        url: hook.url,
        secret: 'tern_whk_first',
        createdAt: DateTime(2026, 11, 4),
      ),
    );
  }

  @override
  Future<List<Webhook>> webhooks() async => [hook];

  @override
  Future<Webhook> rotateWebhook(String id, {String? auth}) async {
    rotated++;
    return hook = Webhook(
      routineId: id,
      auth: hook.auth,
      path: hook.path,
      url: hook.url,
      secret: 'tern_whk_second',
      createdAt: hook.createdAt,
    );
  }

  @override
  Future<void> revokeWebhook(String id) async {
    revoked++;
    hook = Webhook(
      routineId: id,
      auth: hook.auth,
      path: hook.path,
      url: hook.url,
      active: false,
      createdAt: hook.createdAt,
    );
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
    tester.view.physicalSize = const Size(700, 1500) * 2;
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

  testWidgets('a watch routine shows its trigger and the editor sends it', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(700, 1600) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final client = _FakeClient();
    final store = TernStore(client);
    const watch = Schedule(
      id: 'w',
      cron: '',
      prompt: 'Summarize the PR',
      sessionId: 's',
      enabled: true,
      trigger: 'watch',
      watch: 'github.reviews',
      everySecs: 300,
      lastCheck: 'no new events',
      initiative: 'notify',
    );
    client.list = store.schedules = [watch];
    await tester.pumpWidget(_app(RoutinesSection(store: store)));
    await tester.pumpAndSettle();
    expect(find.textContaining('On github.reviews events'), findsOneWidget);
    expect(routineWhen(watch), 'On github.reviews events');
    expect(
      routineWhen(
        Schedule(
          id: 'o',
          cron: '',
          prompt: 'x',
          sessionId: 's',
          enabled: true,
          trigger: 'once',
          at: DateTime(2026, 11, 5, 9),
        ),
      ),
      startsWith('Once, '),
    );

    await tester.pumpWidget(_app(RoutineSheet(store: store, editing: watch)));
    await tester.pumpAndSettle();
    expect(find.text('github.reviews'), findsOneWidget);
    expect(find.text('5m'), findsOneWidget);
    expect(
      find.text('Notify: send now if it matters, else digest'),
      findsOneWidget,
    );
    // No gate for a watch: it already decides when to run.
    expect(find.byKey(const Key('routine-gate')), findsNothing);
    await tester.enterText(find.byKey(const Key('routine-every')), '10m');
    await tester.pump();
    await tester.tap(find.widgetWithText(FilledButton, 'Save'));
    await tester.pumpAndSettle();
    final sent = client.sentOptions.single!;
    expect(sent.toJson(), {
      'watch': 'github.reviews',
      'every': '10m',
      'initiative': 'notify',
      'gate': null,
      'gate_connector': null,
    });

    // Switching to "Once" needs a time before it can be saved.
    await tester.pumpWidget(const SizedBox());
    await tester.pumpWidget(_app(RoutineSheet(store: store, editing: watch)));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Once'));
    await tester.pump();
    final save = find.widgetWithText(FilledButton, 'Save');
    expect(tester.widget<FilledButton>(save).onPressed, isNull);
    await tester.enterText(
      find.byKey(const Key('routine-at')),
      '2030-01-02 09:00',
    );
    await tester.pump();
    expect(tester.widget<FilledButton>(save).onPressed, isNotNull);
  });

  testWidgets('a webhook routine shows its secret once, rotates and revokes', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(700, 1600) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final client = _HookClient();
    final store = TernStore(client);

    // New: "Webhook" needs no schedule, only the auth kind.
    await tester.pumpWidget(_app(RoutineSheet(store: store)));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField).first, 'Triage new issues');
    await tester.tap(find.text('Webhook'));
    await tester.pump();
    await tester.tap(find.text('Secret in header'));
    await tester.pump();
    // A webhook routine can have a gate that sees the event.
    expect(find.byKey(const Key('routine-gate')), findsNothing);
    await tester.tap(find.text('Quiet hours and limits'));
    await tester.pump();
    expect(find.byKey(const Key('routine-gate')), findsOneWidget);
    await tester.tap(find.widgetWithText(FilledButton, 'Create routine'));
    await tester.pumpAndSettle();
    expect(client.created.single, (
      prompt: 'Triage new issues',
      auth: 'bearer',
    ));
    expect(find.text('Webhook created'), findsOneWidget);
    expect(find.text('tern_whk_first'), findsOneWidget);
    expect(find.text('https://hooks.example.com/v1/hooks/h'), findsOneWidget);
    await tester.tap(find.text('Done'));
    await tester.pumpAndSettle();

    // Editing: the URL and its state, no trigger switch, rotate and revoke.
    final hook = Schedule(
      id: 'h',
      cron: '',
      prompt: 'Triage new issues',
      sessionId: 's',
      enabled: true,
      trigger: 'webhook',
      lastCheck: 'gate: ignored a delivery',
    );
    expect(routineWhen(hook), 'On webhook deliveries');
    await tester.pumpWidget(const SizedBox());
    await tester.pumpWidget(_app(RoutineSheet(store: store, editing: hook)));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('routine-trigger')), findsNothing);
    expect(find.byKey(const Key('routine-webhook-url')), findsOneWidget);
    await tester.tap(find.byKey(const Key('routine-webhook-rotate')));
    await tester.pumpAndSettle();
    expect(client.rotated, 1);
    expect(find.text('New webhook secret'), findsOneWidget);
    expect(find.text('tern_whk_second'), findsOneWidget);
    await tester.tap(find.text('Done'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('routine-webhook-revoke')));
    await tester.pumpAndSettle();
    expect(client.revoked, 1);
    expect(find.textContaining('Revoked: the URL answers 404'), findsOneWidget);
    expect(find.text('New secret…'), findsOneWidget);
  });
}
