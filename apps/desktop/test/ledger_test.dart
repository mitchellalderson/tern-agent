// The activity ledger page (tooling spec §8.1) against a fake client.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/ledger.dart';
import 'package:tern_desktop/ui/underline_tabs.dart';

LedgerEntry entry(
  int seq,
  String kind,
  String summary, {
  String actor = 'owner',
  String? session,
  String? routine,
  String? connector,
}) => LedgerEntry.fromJson({
  'seq': seq,
  'at': DateTime(2026, 11, 4, 9, seq).millisecondsSinceEpoch,
  'kind': kind,
  'actor': actor,
  'subject': 'x',
  'summary': summary,
  'session_id': ?session,
  'routine_id': ?routine,
  'connector': ?connector,
  'hash': 'ab' * 32,
});

final sampleLedger = [
  entry(
    6,
    'effect',
    'write · gmail_send to bob@example.com as gmail · work',
    actor: 'agent:tern',
    session: 's1',
    connector: 'gmail',
  ),
  entry(
    5,
    'outbox.approve',
    'approved outbox #3: gmail_send to bob@example.com',
    session: 's1',
  ),
  entry(
    4,
    'autonomy.routine',
    'routine inbox: the configured profile → yolo',
    routine: 'inbox',
  ),
  entry(3, 'grant.add', 'always allow shell git *'),
  entry(
    2,
    'connection.add',
    'added gmail · work (me@work.com)',
    connector: 'gmail',
  ),
  entry(1, 'autonomy.config', 'autonomy is supervised'),
];

class FakeLedgerClient extends TernClient {
  FakeLedgerClient({this.ok = true})
    : super(const Endpoint.unix('/nonexistent'));

  final bool ok;
  final asked = <String?>[];

  @override
  Future<List<LedgerEntry>> ledger({
    String? kind,
    String? channel,
    String? routine,
    String? connector,
    String? run,
    int? before,
    int? limit,
  }) async {
    asked.add(kind);
    return [
      for (final e in sampleLedger)
        if (kind == null || e.family == kind) e,
    ];
  }

  @override
  Future<LedgerCheck> verifyLedger() async => LedgerCheck(
    ok: ok,
    entries: 6,
    head: 'ab' * 32,
    brokenAt: ok ? null : 4,
    reason: ok ? null : "its contents don't match its hash",
  );
}

Future<TernStore> pumpLedger(
  WidgetTester tester,
  FakeLedgerClient client,
) async {
  tester.view.physicalSize = const Size(1000, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  final store = TernStore(client);
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: LedgerView(store: store, panelOpen: false, onTogglePanel: () {}),
      ),
    ),
  );
  await tester.pumpAndSettle();
  return store;
}

void main() {
  test('decodes entries and the check', () {
    final e = sampleLedger.first;
    expect(e.family, 'effect');
    expect(ledgerActor(e.actor), 'tern');
    expect(ledgerActor('owner'), 'you');
    final c = LedgerCheck.fromJson({
      'ok': false,
      'entries': 3,
      'head': 'h',
      'broken': {'seq': 2, 'reason': 'it was edited'},
    });
    expect((c.ok, c.brokenAt, c.reason), (false, 2, 'it was edited'));
  });

  testWidgets('lists entries with an intact chain and filters by kind', (
    tester,
  ) async {
    final client = FakeLedgerClient();
    await pumpLedger(tester, client);
    expect(find.text('Intact · 6 entries'), findsOneWidget);
    expect(find.textContaining('always allow shell git *'), findsOneWidget);
    expect(find.textContaining('by you'), findsWidgets);

    await tester.tap(find.widgetWithText(UnderlineTab, 'Grants'));
    await tester.pumpAndSettle();
    expect(client.asked.last, 'grant');
    expect(find.textContaining('always allow shell git *'), findsOneWidget);
    expect(find.textContaining('added gmail'), findsNothing);
  });

  testWidgets('says where the chain was changed', (tester) async {
    await pumpLedger(tester, FakeLedgerClient(ok: false));
    expect(find.text('Changed outside tern at entry #4'), findsOneWidget);
    expect(find.textContaining("don't match its hash"), findsOneWidget);
  });

  testWidgets('an entry opens its conversation or its routine', (tester) async {
    final store = await pumpLedger(tester, FakeLedgerClient());
    await tester.tap(find.textContaining('routine inbox'));
    await tester.pump();
    expect(store.page, LibraryPage.routines);
  });
}
