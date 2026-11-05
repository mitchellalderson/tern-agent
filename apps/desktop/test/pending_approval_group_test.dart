// A tainted run's read-only requests, waiting together across its
// sub-agents and tools, become one card with bulk answers.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/items.dart';

const _repo = '/Users/me/proj';

Approval _read(
  String id,
  String tool,
  String path, {
  String run = 'root',
  String session = 's',
  String? parent,
  String? rootSession,
  bool grant = true,
  String effect = 'read',
  String? taint = 'spawned by a tainted run',
}) => Approval(
  id: id,
  runId: run,
  sessionId: session,
  parentRunId: parent,
  rootSessionId: rootSession,
  tool: tool,
  input: {'path': path},
  summary: '$tool $path',
  effect: effect,
  pattern: null,
  taint: taint,
  grant: grant
      ? const GrantOffer(
          key: 'readscope:$_repo',
          label: 'read tools in $_repo',
          runOnly: true,
        )
      : null,
);

List<Item> _screenshot() => [
  ApprovalItem(_read('a1', 'read_file', '$_repo/docs/release-checklist.md')),
  ApprovalItem(
    _read(
      'a2',
      'list_dir',
      '$_repo/crates/tern-store',
      run: 'c1',
      session: 's2',
      parent: 'root',
      rootSession: 's',
    ),
  ),
  ApprovalItem(
    _read(
      'a3',
      'list_dir',
      '$_repo/.github/workflows',
      run: 'c2',
      session: 's3',
      parent: 'root',
      rootSession: 's',
    ),
  ),
];

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
    test('sub-agents and read tools of one run tree become one group', () {
      final out = groupApprovals([UserItem('go'), ..._screenshot()]);
      expect(out.map((i) => i.runtimeType), [
        UserItem,
        PendingApprovalGroupItem,
      ]);
      final g = out.last as PendingApprovalGroupItem;
      expect(g.count, 3);
      expect(g.tools, ['read_file', 'list_dir']);
      expect(g.folder, _repo);
      expect(g.grantRepresentatives, hasLength(1));
    });

    test('a lone card, other trees, writes and answered cards stay apart', () {
      final one = ApprovalItem(_read('a1', 'read_file', '$_repo/a'));
      expect(groupApprovals([one]).single, same(one));
      final out = groupApprovals([
        ApprovalItem(_read('a1', 'read_file', '$_repo/a')),
        ApprovalItem(_read('a2', 'read_file', '$_repo/b', session: 'other')),
        ApprovalItem(_read('a3', 'write_file', '$_repo/c', effect: 'write')),
        ApprovalItem(_read('a4', 'read_file', '$_repo/d', taint: null)),
        ApprovalItem(_read('a5', 'read_file', '$_repo/e'))..outcome = 'denied',
      ]);
      expect(out.any((i) => i is PendingApprovalGroupItem), isFalse);
    });

    test('the group sits where the first card was', () {
      final out = groupApprovals([
        UserItem('a'),
        ApprovalItem(_read('a1', 'read_file', '$_repo/a')),
        AssistantItem('b'),
        ApprovalItem(_read('a2', 'list_dir', '$_repo/b')),
      ]);
      expect(out.map((i) => i.runtimeType), [
        UserItem,
        PendingApprovalGroupItem,
        AssistantItem,
      ]);
    });
  });

  group('the group card', () {
    testWidgets('says what is pending and answers all once', (tester) async {
      final items = _screenshot();
      final g = PendingApprovalGroupItem(items.cast<ApprovalItem>());
      final (_, client) = await _pump(tester, g);
      expect(
        find.text(
          '3 pending: read_file, list_dir in $_repo (spawned by tainted run)',
        ),
        findsOneWidget,
      );
      await tester.tap(find.text('Allow all 3'));
      await tester.pump();
      expect(client.answers, [
        ('a1', Decision.once),
        ('a2', Decision.once),
        ('a3', Decision.once),
      ]);
      expect(
        items.every((i) => (i as ApprovalItem).outcome == 'approved'),
        true,
      );
    });

    testWidgets('for this run answers once per scope and covers the rest', (
      tester,
    ) async {
      final items = _screenshot();
      final g = PendingApprovalGroupItem(items.cast<ApprovalItem>());
      final (_, client) = await _pump(tester, g);
      await tester.tap(find.text('Allow all for this run'));
      await tester.pump();
      expect(client.answers, [('a1', Decision.run)]);
      for (final i in items) {
        expect((i as ApprovalItem).outcome, 'allowed for this run');
      }
    });

    testWidgets('deny all denies each, and the list expands with input', (
      tester,
    ) async {
      final items = _screenshot();
      final g = PendingApprovalGroupItem(items.cast<ApprovalItem>());
      final (_, client) = await _pump(tester, g);
      await tester.tap(find.text('Show list'));
      await tester.pump();
      expect(
        find.textContaining('list_dir $_repo/.github/workflows'),
        findsOne,
      );
      await tester.tap(find.text('Show input').first);
      await tester.pump();
      expect(find.textContaining('"path"'), findsOneWidget);
      await tester.tap(find.text('Deny all'));
      await tester.pump();
      expect(client.answers.map((a) => a.$2), everyElement(Decision.deny));
      expect(client.answers, hasLength(3));
    });

    testWidgets('without a read scope there is no for-this-run button', (
      tester,
    ) async {
      final g = PendingApprovalGroupItem([
        ApprovalItem(_read('a1', 'read_file', '$_repo/a', grant: false)),
        ApprovalItem(_read('a2', 'list_dir', '$_repo/b', grant: false)),
      ]);
      await _pump(tester, g);
      expect(find.text('Allow all for this run'), findsNothing);
      expect(find.text('Allow all 2'), findsOneWidget);
    });
  });

  group('a single tainted read card', () {
    testWidgets('offers only for this run, and says why no always', (
      tester,
    ) async {
      final (_, client) = await _pump(
        tester,
        ApprovalItem(_read('a1', 'read_file', '$_repo/a')),
      );
      expect(find.text('Allow for this run'), findsOneWidget);
      expect(find.text('Allow for this conversation'), findsNothing);
      expect(find.text('Always allow this'), findsNothing);
      expect(find.byKey(const ValueKey('approval-no-always')), findsOneWidget);
      expect(find.text('Show input'), findsOneWidget);
      await tester.tap(find.text('Allow for this run'));
      await tester.pump();
      expect(client.answers, [('a1', Decision.run)]);
    });
  });
}
