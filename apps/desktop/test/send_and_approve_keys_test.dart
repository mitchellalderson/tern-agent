// A failed send stays visible as unsent with a Retry, and Enter alone does
// not approve a tool call.
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/items.dart';

class _Client extends TernClient {
  _Client() : super(const Endpoint.unix('/nonexistent'));

  var fail = true;
  final sent = <String>[];
  final answers = <(String, Decision)>[];

  @override
  Future<Started> send(
    String text, {
    String? session,
    String? model,
    String? workspace,
  }) async {
    if (fail) throw const TernException('no model is set up');
    sent.add(text);
    return Started(sessionId: session ?? 's1', runId: 'r1', model: 'm');
  }

  @override
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [];

  @override
  Future<void> answer(String approval, Decision decision) async =>
      answers.add((approval, decision));
}

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: SingleChildScrollView(child: child)),
);

void main() {
  test(
    'a failed send keeps the message, marked unsent; retry sends it',
    () async {
      final client = _Client();
      final store = TernStore(client)..selected = 's1';
      expect(await store.send('hello'), isTrue);
      final item = store.current!.items.whereType<UserItem>().single;
      expect(item.failed, isTrue);
      expect(store.error, 'no model is set up');

      client.fail = false;
      await store.retrySend(item);
      final items = store.current!.items.whereType<UserItem>().toList();
      expect(items, hasLength(1));
      expect(items.single.failed, isFalse);
      expect(client.sent, ['hello']);
    },
  );

  test(
    'a failed first message has nothing to retry: the text goes back',
    () async {
      final store = TernStore(_Client());
      expect(await store.send('hello'), isFalse);
    },
  );

  testWidgets('the unsent message says so and offers Retry', (tester) async {
    final store = TernStore(_Client())..selected = 's1';
    final item = UserItem('hello')..failed = true;
    await tester.pumpWidget(_app(ItemView(item: item, store: store)));
    expect(find.byKey(const Key('not-sent')), findsOneWidget);
    expect(find.text('Retry'), findsOneWidget);
  });

  testWidgets('Enter alone does not approve', (tester) async {
    final client = _Client();
    final store = TernStore(client);
    final item = ApprovalItem(
      const Approval(
        id: 'a1',
        runId: 'r',
        sessionId: 's',
        tool: 'shell',
        input: {'command': 'rm -rf build'},
        summary: 'rm -rf build',
        effect: 'destructive',
        pattern: null,
        grant: null,
        notice: null,
        count: 1,
      ),
    );
    await tester.pumpWidget(_app(ApprovalCard(item: item, store: store)));
    await tester.pump();

    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pump();
    expect(client.answers, isEmpty, reason: 'Enter alone must not approve');
    expect(
      FocusManager.instance.primaryFocus?.debugLabel ?? '',
      isNot(contains('FilledButton')),
    );
    expect(find.text('Allow once'), findsOneWidget);
  });
}
