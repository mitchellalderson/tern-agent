// Scoped approval choices on the card, and identical approved requests
// grouped in the transcript.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/items.dart';

const _summary =
    'connector_test draft maricopa_assessor.city_stats (http hosts: '
    'gis.mcassessor.maricopa.gov/arcgis/rest/services/Parcels/MapServer/*; '
    'secrets: none; rate: 30/min; tools: city_stats (read))';

Approval _ask(
  String id, {
  GrantOffer? grant = const GrantOffer(
    key: 'draft maricopa_assessor.city_stats#1',
    label: 'draft maricopa_assessor.city_stats',
  ),
  int count = 1,
  String summary = _summary,
  String tool = 'connector_test',
}) => Approval(
  id: id,
  runId: 'r',
  sessionId: 's',
  tool: tool,
  input: const {'name': 'maricopa_assessor', 'tool': 'city_stats'},
  summary: summary,
  effect: 'read',
  pattern: null,
  grant: grant,
  count: count,
);

ApprovalItem _answered(String id, [String outcome = 'approved']) =>
    ApprovalItem(_ask(id))..outcome = outcome;

WorkItem _work({String name = 'connector_test', bool running = false}) {
  final w = WorkItem(runId: 'r');
  w.steps.add(
    ToolStep(id: 't', name: name, summary: 'x')
      ..status = running ? StepStatus.running : StepStatus.ok,
  );
  return w;
}

class _Client extends TernClient {
  _Client() : super(const Endpoint.unix('/nonexistent'));

  final answers = <(String, Decision)>[];

  @override
  Future<void> answer(String approval, Decision decision) async =>
      answers.add((approval, decision));
}

Future<(TernStore, _Client)> _pump(WidgetTester tester, Item item) async {
  final client = _Client();
  final store = TernStore(client);
  tester.view.physicalSize = const Size(900, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: SingleChildScrollView(
          child: ItemView(item: item, store: store),
        ),
      ),
    ),
  );
  await tester.pump();
  return (store, client);
}

void main() {
  group('grouping', () {
    test('eight identical approvals with their tool cards become one row', () {
      final items = <Item>[
        UserItem('go'),
        for (var i = 0; i < 8; i++) ...[_answered('a$i'), _work()],
        AssistantItem('done'),
      ];
      final out = groupApprovals(items);
      expect(out.map((i) => i.runtimeType), [
        UserItem,
        ApprovalGroupItem,
        AssistantItem,
      ]);
      final g = out[1] as ApprovalGroupItem;
      expect(g.count, 8);
      expect(g.members, hasLength(16));
      expect(g.label, 'connector_test · draft maricopa_assessor.city_stats');
    });

    test('a shared card counts for each request it answered', () {
      final shared = ApprovalItem(_ask('a1', count: 3))..outcome = 'approved';
      final out = groupApprovals([shared, _work(), _answered('a2'), _work()]);
      expect((out.single as ApprovalGroupItem).count, 4);
    });

    test('denials, different things and a lone approval stay as they are', () {
      final other = ApprovalItem(
        _ask('b', summary: '${_summary}x', tool: 'connector_test'),
      )..outcome = 'approved';
      final items = <Item>[
        _answered('a1'),
        _work(),
        _answered('a2', 'denied'),
        _answered('a3'),
        _work(),
        other,
        _work(),
      ];
      // Nothing repeats back to back: no group at all.
      expect(groupApprovals(items), hasLength(items.length));
      // A different call in the middle splits the run in two groups.
      final out = groupApprovals([
        _answered('a1'),
        _work(),
        _answered('a2'),
        _work(),
        other,
        _work(),
        _answered('a3'),
        _work(),
        _answered('a4'),
        _work(),
      ]);
      expect(out.map((i) => i.runtimeType), [
        ApprovalGroupItem,
        ApprovalItem,
        WorkItem,
        ApprovalGroupItem,
      ]);
    });

    test('a call still running is not folded into the group', () {
      final out = groupApprovals([
        _answered('a1'),
        _work(),
        _answered('a2'),
        _work(),
        _answered('a3'),
        _work(running: true),
      ]);
      expect(out.map((i) => i.runtimeType), [
        ApprovalGroupItem,
        ApprovalItem,
        WorkItem,
      ]);
      expect((out.first as ApprovalGroupItem).count, 2);
    });

    test('answered approval notes from a channel group the same way', () {
      Post note(int id, String text) => Post(
        id: id,
        channelId: 'home',
        authorKind: 'system',
        authorId: 'system',
        author: 'tern',
        kind: 'needs_you',
        text: text,
        approvalId: 'a$id',
        resolvedAt: DateTime(2026, 11, 3),
        createdAt: DateTime(2026, 11, 3),
      );
      final what = 'connector_test draft x.y (http hosts: h)';
      final out = groupApprovals([
        PostItem(note(1, 'Approved: $what')),
        _work(),
        PostItem(note(2, 'Approved for this run: $what')),
        _work(),
        PostItem(note(3, 'Denied: $what')),
      ]);
      expect(out.map((i) => i.runtimeType), [ApprovalGroupItem, PostItem]);
      expect(
        (out.first as ApprovalGroupItem).label,
        'connector_test · draft x.y',
      );
    });

    test('a card that gains requests updates in place and is not re-added', () {
      final t = Transcript()..apply(ApprovalRequested(_ask('a1')));
      expect(t.apply(ApprovalRequested(_ask('a1', count: 2))), isTrue);
      expect(t.apply(ApprovalRequested(_ask('a1', count: 2))), isFalse);
      expect((t.items.single as ApprovalItem).approval.count, 2);
    });
  });

  group('the card', () {
    testWidgets('offers run, conversation and always for exactly this', (
      tester,
    ) async {
      final item = ApprovalItem(_ask('a1', count: 3));
      final (_, client) = await _pump(tester, item);
      expect(
        find.textContaining('wants to run connector_test ×3'),
        findsOneWidget,
      );
      expect(find.text('Allow once'), findsOneWidget);
      expect(find.text('Allow for this run'), findsOneWidget);
      expect(find.text('Allow for this conversation'), findsOneWidget);
      expect(find.text('Always allow this'), findsOneWidget);
      await tester.tap(find.text('Allow for this run'));
      await tester.pump();
      expect(client.answers, [('a1', Decision.run)]);
      expect(item.outcome, 'allowed for this run');
    });

    testWidgets('conversation and always send their own answers', (
      tester,
    ) async {
      for (final (label, want, outcome) in [
        (
          'Allow for this conversation',
          Decision.session,
          'allowed for this conversation',
        ),
        ('Always allow this', Decision.always, 'always allowed'),
      ]) {
        final item = ApprovalItem(_ask('a1'));
        final (_, client) = await _pump(tester, item);
        await tester.tap(find.text(label));
        await tester.pump();
        expect(client.answers, [('a1', want)]);
        expect(item.outcome, outcome);
      }
    });

    testWidgets('without a grant (tainted, destructive) only once or deny', (
      tester,
    ) async {
      await _pump(tester, ApprovalItem(_ask('a1', grant: null)));
      expect(find.text('Allow once'), findsOneWidget);
      expect(find.text('Deny'), findsOneWidget);
      expect(find.text('Allow for this run'), findsNothing);
      expect(find.text('Allow for this conversation'), findsNothing);
      expect(find.text('Always allow this'), findsNothing);
    });
  });

  testWidgets('a call covered by a grant says so, not "Approved"', (
    tester,
  ) async {
    await _pump(tester, _answered('a1', 'approved_by_grant'));
    expect(find.textContaining('Covered by a grant: connector_test'), findsOne);
    expect(find.textContaining('Approved:'), findsNothing);
  });

  test('a covered call groups with the approvals of the same thing', () {
    final out = groupApprovals([
      _answered('a1'),
      _work(),
      _answered('a2', 'approved_by_grant'),
      _work(),
    ]);
    expect((out.single as ApprovalGroupItem).count, 2);
  });

  group('the grouped row', () {
    testWidgets('says how many were approved and expands to the items', (
      tester,
    ) async {
      final g = groupApprovals([
        for (var i = 0; i < 8; i++) ...[_answered('a$i'), _work()],
      ]).single;
      await _pump(tester, g);
      expect(
        find.text(
          'connector_test · draft maricopa_assessor.city_stats ×8 approved',
        ),
        findsOneWidget,
      );
      expect(
        find.textContaining('Approved: connector_test draft'),
        findsNothing,
      );
      await tester.tap(find.byIcon(Icons.expand_more));
      await tester.pump();
      expect(
        find.textContaining(
          'Approved: connector_test draft',
          findRichText: true,
        ),
        findsNWidgets(8),
      );
    });
  });
}
