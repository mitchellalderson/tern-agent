// Fleet status (docs/fleet-status-notes.md): one derivation for the sidebar,
// Home and the agent page header, the summary strip, and the spend bar.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/fleet.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/fleet_status.dart';
import 'package:tern_desktop/ui/sidebar.dart';

final _now = DateTime(2026, 11, 5, 12);

Agent _agent({
  String id = 'tern:a',
  int needsYou = 0,
  int activeRuns = 0,
  bool archived = false,
  DateTime? lastActivity,
  bool isDefault = false,
}) => Agent(
  id: id,
  slug: id.split(':').last,
  name: id.split(':').last,
  mainChannelId: isDefault ? 'home' : 'dm:$id',
  needsYou: needsYou,
  activeRuns: activeRuns,
  archived: archived,
  lastActivity: lastActivity,
  isDefault: isDefault,
);

Schedule _sched({
  String agent = 'tern:a',
  String? paused,
  double? spent,
  double? cap,
}) => Schedule(
  id: 's-$agent-${spent ?? 0}',
  cron: '0 8 * * *',
  prompt: 'p',
  sessionId: 'x',
  enabled: true,
  agentId: agent,
  paused: paused,
  spentUsd: spent,
  usdCap: cap,
);

AgentStatus _status(
  Agent a, {
  bool online = true,
  int waiting = 0,
  bool busy = false,
  List<Schedule> schedules = const [],
}) => agentStatus(
  a,
  linkOnline: online,
  now: _now,
  waiting: waiting,
  busy: busy,
  schedules: schedules,
);

class _Store extends TernStore {
  _Store() : super(TernClient(const Endpoint.unix('/nonexistent')));
  final opened = <String>[];

  @override
  Future<void> openAgent(Agent a) async => opened.add(a.id);

  @override
  Future<void> openHome({bool newTopic = false}) async => opened.add('home');
}

Future<void> _pump(WidgetTester tester, Widget child, {bool still = false}) =>
    tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: MediaQuery(
          data: MediaQueryData(disableAnimations: still),
          child: Scaffold(body: SizedBox(width: 260, child: child)),
        ),
      ),
    );

void main() {
  group('agentStatus', () {
    test('each state', () {
      expect(_status(_agent()), AgentStatus.idle);
      expect(_status(_agent(), busy: true), AgentStatus.working);
      expect(_status(_agent(activeRuns: 1)), AgentStatus.working);
      expect(_status(_agent(), waiting: 2), AgentStatus.needsYou);
      expect(_status(_agent(needsYou: 1)), AgentStatus.needsYou);
      expect(_status(_agent(), online: false), AgentStatus.offline);
      expect(_status(_agent(archived: true)), AgentStatus.archived);
      expect(
        _status(_agent(), schedules: [_sched(paused: 'budget')]),
        AgentStatus.overBudget,
      );
      expect(
        _status(
          _agent(lastActivity: _now.subtract(const Duration(minutes: 5))),
        ),
        AgentStatus.doneRecently,
      );
    });

    test('done recently fades after 15 minutes', () {
      final old = _agent(
        lastActivity: _now.subtract(
          doneRecentlyFor + const Duration(seconds: 1),
        ),
      );
      expect(_status(old), AgentStatus.idle);
      final edge = _agent(lastActivity: _now.subtract(doneRecentlyFor));
      expect(_status(edge), AgentStatus.doneRecently);
    });

    test('a budget pause that is not a budget pause does not count', () {
      expect(
        _status(_agent(), schedules: [_sched(paused: null)]),
        AgentStatus.idle,
      );
    });

    test('precedence: offline, needs you, working, over budget, archived', () {
      final all = _agent(
        needsYou: 1,
        activeRuns: 1,
        archived: true,
        lastActivity: _now,
      );
      final budget = [_sched(paused: 'budget')];
      expect(
        _status(all, online: false, waiting: 1, busy: true, schedules: budget),
        AgentStatus.offline,
      );
      expect(
        _status(all, waiting: 1, busy: true, schedules: budget),
        AgentStatus.needsYou,
      );
      expect(
        _status(
          _agent(archived: true, lastActivity: _now),
          busy: true,
          schedules: budget,
        ),
        AgentStatus.working,
      );
      expect(
        _status(_agent(archived: true, lastActivity: _now), schedules: budget),
        AgentStatus.overBudget,
      );
      expect(
        _status(_agent(archived: true, lastActivity: _now)),
        AgentStatus.archived,
      );
      expect(
        _status(_agent(lastActivity: _now)),
        AgentStatus.doneRecently,
        reason: 'recent beats idle',
      );
    });
  });

  group('FleetSummary', () {
    test('counts and wording', () {
      final a = _agent(id: 'tern:a');
      final b = _agent(id: 'tern:b');
      final c = _agent(id: 'tern:c');
      final d = _agent(id: 'tern:d');
      final s = FleetSummary([
        (a, AgentStatus.working),
        (b, AgentStatus.working),
        (c, AgentStatus.needsYou),
        (d, AgentStatus.idle),
      ]);
      expect(s.text, '1 needs you, 2 working');
      expect(s.firstNeedingYou, c);
      const none = FleetSummary([]);
      expect(none.text, isEmpty);
      expect(none.firstNeedingYou, isNull);
      expect(
        FleetSummary([(a, AgentStatus.needsYou), (b, AgentStatus.needsYou)])
            .text,
        '2 need you',
      );
      expect(FleetSummary([(a, AgentStatus.overBudget)]).text, '1 over budget');
    });
  });

  group('spendToday', () {
    test('sums routines that have both numbers', () {
      final s = spendToday([
        _sched(agent: 'a', spent: 0.25, cap: 0.5),
        _sched(agent: 'b', spent: 1.0, cap: 2.0),
        _sched(agent: 'c', spent: null, cap: 1.0),
        _sched(agent: 'd'),
      ]);
      expect(s, isNotNull);
      expect(s!.spent, 1.25);
      expect(s.cap, 2.5);
    });

    test('null without a cap or a price', () {
      expect(spendToday([]), isNull);
      expect(spendToday([_sched(spent: 0.1)]), isNull);
      expect(spendToday([_sched(cap: 1)]), isNull);
    });
  });

  group('FleetStrip', () {
    testWidgets('says who works and who needs you, and a tap opens them', (
      tester,
    ) async {
      final store = _Store()
        ..link = Link.online
        ..agents = [
          _agent(id: 'tern', isDefault: true),
          _agent(id: 'tern:a', activeRuns: 1),
          _agent(id: 'tern:b', needsYou: 2),
          _agent(id: 'tern:c', activeRuns: 1),
        ];
      await _pump(tester, FleetStrip(store: store));
      expect(find.text('1 needs you, 2 working'), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('fleet-strip')));
      await tester.pump();
      expect(store.opened, ['tern:b']);
    });

    testWidgets('is not tappable when nothing needs you', (tester) async {
      final store = _Store()
        ..link = Link.online
        ..agents = [_agent(activeRuns: 1)];
      await _pump(tester, FleetStrip(store: store));
      expect(find.text('1 working'), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('fleet-strip')));
      await tester.pump();
      expect(store.opened, isEmpty);
    });

    testWidgets('is hidden when quiet or offline', (tester) async {
      final store = _Store()
        ..link = Link.online
        ..agents = [_agent()];
      await _pump(tester, FleetStrip(store: store));
      expect(find.byKey(const ValueKey('fleet-strip')), findsNothing);
      store
        ..agents = [_agent(activeRuns: 1)]
        ..link = Link.offline;
      await _pump(tester, FleetStrip(store: store));
      expect(find.byKey(const ValueKey('fleet-strip')), findsNothing);
    });
  });

  group('StatusMark', () {
    testWidgets('pulses while working, holds still with reduced motion', (
      tester,
    ) async {
      Future<double> opacity() async {
        await tester.pump(const Duration(milliseconds: 300));
        return tester
            .widget<Opacity>(
              find.descendant(
                of: find.byType(StatusMark),
                matching: find.byType(Opacity),
              ),
            )
            .opacity;
      }

      await _pump(tester, const StatusMark(status: AgentStatus.working));
      final a = await opacity();
      final b = await opacity();
      expect(a, isNot(b), reason: 'animating');
      await tester.pumpWidget(const SizedBox());

      await _pump(
        tester,
        const StatusMark(status: AgentStatus.working),
        still: true,
      );
      expect(await opacity(), 1);
      expect(tester.hasRunningAnimations, isFalse);
    });

    testWidgets('carries its label for screen readers', (tester) async {
      await _pump(tester, const StatusMark(status: AgentStatus.needsYou));
      expect(find.bySemanticsLabel('needs you'), findsOneWidget);
    });
  });

  group('SpendBar', () {
    testWidgets('shows today against the cap, and hides without data', (
      tester,
    ) async {
      final store = _Store()..schedules = [_sched(spent: 0.31, cap: 0.5)];
      await _pump(tester, SpendBar(store: store));
      expect(find.text('Routines today \$0.31 of \$0.50 cap'), findsOneWidget);
      expect(
        tester
            .widget<LinearProgressIndicator>(
              find.byType(LinearProgressIndicator),
            )
            .value,
        closeTo(0.62, 1e-9),
      );

      store.schedules = [_sched(cap: 0.5)];
      await _pump(tester, SpendBar(store: store));
      expect(find.byKey(const ValueKey('spend-bar')), findsNothing);
    });

    testWidgets('the bar caps at full when over', (tester) async {
      final store = _Store()..schedules = [_sched(spent: 0.9, cap: 0.5)];
      await _pump(tester, SpendBar(store: store));
      expect(
        tester
            .widget<LinearProgressIndicator>(
              find.byType(LinearProgressIndicator),
            )
            .value,
        1,
      );
    });
  });

  testWidgets('the sidebar marks each agent and carries the strip and bar', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1000, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final store = _Store()
      ..link = Link.online
      ..health = const Health(
        pid: 1,
        version: '0.2.0',
        features: {'channels', 'agents'},
      )
      ..channels = [
        Channel(
          id: 'home',
          slug: 'home',
          title: 'Home',
          kind: 'main',
          updatedAt: _now,
        ),
      ]
      ..agents = [
        _agent(id: 'tern', isDefault: true),
        _agent(id: 'tern:a', activeRuns: 1),
        _agent(id: 'tern:b', needsYou: 1),
      ]
      ..schedules = [_sched(spent: 1, cap: 4)];
    await tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: Scaffold(
          body: SizedBox(width: 260, child: Sidebar(store: store)),
        ),
      ),
    );
    expect(find.byKey(const ValueKey('fleet-strip')), findsOneWidget);
    expect(find.text('1 needs you, 1 working'), findsOneWidget);
    StatusMark markOf(AgentStatus st) => tester.widget<StatusMark>(
      find.byWidgetPredicate((w) => w is StatusMark && w.status == st),
    );
    expect(markOf(AgentStatus.working), isNotNull);
    expect(find.byKey(const ValueKey('spend-bar')), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
  });
}
