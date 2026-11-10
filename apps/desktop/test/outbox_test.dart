// Outbox cards in Home (tooling spec §5.2) against a fake client.
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/outbox.dart';

OutboxItem item({
  String state = 'pending',
  String? taint,
  String? file,
  bool stale = false,
}) => OutboxItem.fromJson(
  jsonDecode(
    jsonEncode({
      'id': 12,
      'run_id': 'r',
      'session_id': 's',
      'tool': 'gmail_send',
      'input': {'to': 'bob@example.com'},
      'summary': 'gmail_send to bob@example.com',
      'effect': 'write',
      'taint': ?taint,
      'file': ?file,
      'stale': stale,
      'state': state,
      'created_at': DateTime.now().millisecondsSinceEpoch,
      'expires_at': DateTime.now()
          .add(const Duration(hours: 71, minutes: 30))
          .millisecondsSinceEpoch,
    }),
  ) as Json,
);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final decided = <String>[];
  List<OutboxItem> items = [item()];

  @override
  Future<List<OutboxItem>> outbox({bool all = false}) async => items;

  @override
  Future<OutboxItem> approveOutbox(int id, {Object? input}) async {
    decided.add('approve $id ${jsonEncode(input)}');
    items = [];
    return item(state: 'approved');
  }

  @override
  Future<OutboxItem> rejectOutbox(int id, {String? note}) async {
    decided.add('reject $id $note');
    items = [];
    return item(state: 'rejected');
  }
}

Future<(_FakeClient, TernStore)> pump(WidgetTester tester, OutboxItem o) async {
  final client = _FakeClient()..items = [o];
  final store = TernStore(client)..outbox = [o];
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: ListenableBuilder(
          listenable: store,
          builder: (_, _) => OutboxStrip(store: store),
        ),
      ),
    ),
  );
  return (client, store);
}

void main() {
  test('decodes items and the change event', () {
    final o = item(taint: 'result of http_fetch');
    expect(o.pending, isTrue);
    expect(o.headline, 'write · gmail_send to bob@example.com');
    expect(
      o.taintNote,
      'this run read untrusted content (result of http_fetch)',
    );
    final e = TernEvent.fromJson(
      jsonDecode('{"outbox_changed":{"id":12,"state":"done"}}') as Json,
    );
    expect(e, isA<OutboxChanged>());
    expect((e as OutboxChanged).state, 'done');
  });

  testWidgets('a pending item shows why it waits and can be approved', (
    tester,
  ) async {
    final (client, store) = await pump(tester, item(taint: 'result of inbox'));
    expect(find.text('Waiting for you · outbox #12'), findsOneWidget);
    expect(find.text('write · gmail_send to bob@example.com'), findsOneWidget);
    expect(
      find.text('Careful: this run read untrusted content (result of inbox)'),
      findsOneWidget,
    );
    expect(store.pendingApprovals, 1);
    await tester.tap(find.text('Approve'));
    await tester.pumpAndSettle();
    expect(client.decided, ['approve 12 null']);
    expect(find.byType(OutboxCard), findsNothing, reason: 'gone once decided');
  });

  testWidgets('reject sends the note', (tester) async {
    final (client, _) = await pump(tester, item());
    await tester.tap(find.text('Reject'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byType(TextField), 'not to Bob');
    await tester.tap(find.widgetWithText(FilledButton, 'Reject'));
    await tester.pumpAndSettle();
    expect(client.decided, ['reject 12 not to Bob']);
  });

  testWidgets('edit approves with the changed arguments', (tester) async {
    final (client, _) = await pump(tester, item());
    await tester.tap(find.text('Edit…'));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byType(TextField),
      '{"to": "carol@example.com"}',
    );
    await tester.tap(find.text('Approve with these'));
    await tester.pumpAndSettle();
    expect(client.decided, ['approve 12 {"to":"carol@example.com"}']);
  });

  testWidgets('a stale item says why and can only be discarded', (
    tester,
  ) async {
    for (final o in [
      item(file: '/w/vs-trigger-outline.md', stale: true),
      item(file: '/w/vs-trigger-outline.md', state: 'stale'),
    ]) {
      final (client, _) = await pump(tester, o);
      expect(o.stale, isTrue);
      expect(find.text('Out of date · outbox #12'), findsOneWidget);
      expect(
        find.text('vs-trigger-outline.md changed since this was queued.'),
        findsOneWidget,
      );
      expect(find.text('Approve'), findsNothing);
      expect(find.text('Edit…'), findsNothing);
      expect(find.text('Reject'), findsNothing);
      await tester.tap(find.widgetWithText(FilledButton, 'Discard'));
      await tester.pumpAndSettle();
      expect(client.decided, ['reject 12 null']);
      expect(find.byType(OutboxCard), findsNothing);
    }
  });
}
