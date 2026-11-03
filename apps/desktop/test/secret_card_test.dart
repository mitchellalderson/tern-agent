// The secret request card against a fake client, plus how the store and
// transcript close it.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/items.dart';

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final posted = <Map<String, String>>[];
  int? failWith;

  @override
  Future<void> answerSecretRequest(
    String id,
    Map<String, String> values,
  ) async {
    if (failWith case final status?) {
      throw TernException('nope', status: status);
    }
    posted.add(values);
  }
}

SecretRequest request({
  bool inFile = false,
  bool inEnv = false,
  List<String> usedBy = const ['connector weather (draft)'],
  String? agentLabel,
}) => SecretRequest(
  id: 'q1',
  runId: 'r',
  sessionId: 's',
  callId: 'toolu_1',
  secrets: [
    SecretAsk(
      name: 'WEATHER_API_KEY',
      inFile: inFile,
      inEnv: inEnv,
      usedBy: usedBy,
    ),
  ],
  reason: 'to call the weather API',
  agentLabel: agentLabel,
);

Future<(_FakeClient, SecretRequestItem)> pump(
  WidgetTester tester,
  SecretRequest r,
) async {
  final client = _FakeClient();
  final store = TernStore(client);
  final item = SecretRequestItem(r);
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: ListenableBuilder(
          listenable: store,
          builder: (_, _) => SecretRequestCard(item: item, store: store),
        ),
      ),
    ),
  );
  return (client, item);
}

void main() {
  testWidgets('not set: shows the agent quoted, uses, and obscured field', (
    tester,
  ) async {
    await pump(tester, request());
    expect(find.text('The agent is asking for a secret'), findsOneWidget);
    expect(
      find.text('The agent says: “to call the weather API”'),
      findsOneWidget,
    );
    expect(find.text('WEATHER_API_KEY'), findsOneWidget);
    expect(find.text('not set'), findsOneWidget);
    expect(find.text('Used by connector weather (draft)'), findsOneWidget);
    final field = tester.widget<TextField>(find.byType(TextField));
    expect(field.obscureText, isTrue);
    expect(field.autocorrect, isFalse);
    expect(field.enableSuggestions, isFalse);

    await tester.tap(find.text('Show'));
    await tester.pump();
    expect(tester.widget<TextField>(find.byType(TextField)).obscureText, false);
  });

  testWidgets('nothing uses it, and a sub-agent label', (tester) async {
    await pump(tester, request(usedBy: const [], agentLabel: 'scout'));
    expect(find.text('Nothing uses this secret yet.'), findsOneWidget);
    expect(find.text('Sub-agent scout is asking for a secret'), findsOneWidget);
  });

  testWidgets('env wins', (tester) async {
    await pump(tester, request(inEnv: true));
    expect(
      find.text('set in the environment, which wins over a saved value'),
      findsOneWidget,
    );
  });

  testWidgets('replace: a saved value is replaced', (tester) async {
    await pump(tester, request(inFile: true));
    expect(find.text('saved — this replaces it'), findsOneWidget);
  });

  testWidgets('a 403 shows the card read-only', (tester) async {
    final (client, item) = await pump(tester, request());
    client.failWith = 403;
    await tester.enterText(find.byType(TextField), 'abc');
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect(item.open, isTrue);
    expect(find.text('Enter this on an owner device'), findsOneWidget);
    expect(find.byType(TextField), findsNothing);
    expect(find.text('Save'), findsNothing);
  });

  testWidgets('Save posts the values and collapses without them', (
    tester,
  ) async {
    final (client, item) = await pump(tester, request());
    await tester.enterText(find.byType(TextField), 'sk-secret-123');
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect(client.posted, [
      {'WEATHER_API_KEY': 'sk-secret-123'},
    ]);
    expect(item.outcome, 'saved');
    expect(find.text('WEATHER_API_KEY saved ✓'), findsOneWidget);
    expect(find.byType(TextField), findsNothing);
    expect(find.textContaining('sk-secret'), findsNothing);
  });

  testWidgets('Save with nothing entered posts only non-empty values', (
    tester,
  ) async {
    final (client, _) = await pump(tester, request());
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect(client.posted, [<String, String>{}]);
  });

  testWidgets('Skip posts an empty map', (tester) async {
    final (client, item) = await pump(tester, request());
    await tester.enterText(find.byType(TextField), 'typed');
    await tester.tap(find.text('Skip'));
    await tester.pumpAndSettle();
    expect(client.posted, [<String, String>{}]);
    expect(item.outcome, 'skipped');
  });

  group('transcript', () {
    test('shown once; tool_finished closes it with the result', () {
      final t = Transcript()..apply(SecretRequested(request()));
      expect(t.apply(SecretRequested(request())), isFalse);
      expect(t.items.whereType<SecretRequestItem>(), hasLength(1));
      // Another call finishing doesn't close it.
      t.closeSecretRequests('r', 'other', '{}');
      expect((t.items.single as SecretRequestItem).open, isTrue);
      t.apply(
        const ToolFinished(
          runId: 'r',
          id: 'toolu_1',
          name: 'secret_request',
          isError: false,
          preview: '',
          output: '{"WEATHER_API_KEY":"set"}',
          replayed: false,
        ),
      );
      final item = t.items.whereType<SecretRequestItem>().single;
      expect(item.open, isFalse);
      expect(item.result, {'WEATHER_API_KEY': 'set'});
    });

    test('a run ending closes its open requests', () {
      final t = Transcript()..apply(SecretRequested(request()));
      t.apply(const RunFinished(runId: 'r', status: 'interrupted'));
      expect((t.items.first as SecretRequestItem).outcome, 'expired');
    });
  });

  test('the store routes a sub-agent request to its root conversation', () {
    final store = TernStore(_FakeClient());
    final r = SecretRequest(
      id: 'q2',
      runId: 'child',
      sessionId: 'cs',
      callId: 'c1',
      secrets: const [],
      reason: 'x',
      agentLabel: 'scout',
      rootSessionId: 'root',
    );
    store.onEventForTest(SecretRequested(r));
    expect(store.transcripts['root']!.items.single, isA<SecretRequestItem>());
    expect(store.pendingSecrets, 1);
  });
}
