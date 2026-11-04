// Sign-in cards and the Connections view against a fake client.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/auth_card.dart';
import 'package:tern_desktop/ui/connections.dart';

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final calls = <String>[];

  @override
  Future<void> answerAuthPrompt(String id, String answer) async =>
      calls.add('answer $id $answer');

  @override
  Future<ConnectionList> connections() async => _list;

  @override
  Future<Account> makeDefaultConnection(Account a) async {
    calls.add('default ${a.slug}');
    return a;
  }

  @override
  Future<void> removeConnection(Account a) async =>
      calls.add('remove ${a.slug}');
}

Account _acct(
  String slug, {
  String target = 'gmail',
  bool isDefault = false,
  String status = 'ok',
  String? error,
}) => Account.fromJson({
  'id': '$target/$slug',
  'kind': 'connector',
  'target': target,
  'slug': slug,
  'label': 'The $slug one',
  'account': '$slug@x.com',
  'auth': {'method': 'oauth'},
  'is_default': isDefault,
  'status': status,
  'last_error': error,
  'title': '$target · $slug',
});

final _list = ConnectionList(
  accounts: [
    _acct('work', isDefault: true),
    _acct('home', status: 'needs_reauth', error: 'token expired'),
  ],
  targets: const [
    ConnectionTarget(kind: 'connector', name: 'gmail', methods: ['oauth']),
    ConnectionTarget(kind: 'mcp', name: 'notion', methods: ['oauth', 'key']),
  ],
);

AuthPrompt _prompt(AuthCard card, {String id = 'a1', String? callId}) =>
    AuthPrompt(id: id, callId: callId, card: card);

const _choose = AuthCard(
  kind: 'choose',
  title: 'Connect Notion',
  options: [
    AuthOption(id: 'key', label: 'Use a key', detail: 'Paste a token'),
    AuthOption(
      id: 'oauth',
      label: 'Sign in',
      detail: 'Through your browser',
      recommended: true,
    ),
  ],
  notes: ['Tokens stay on this computer'],
);

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: child),
);

TernStore _store(_FakeClient c) => TernStore(c)
  ..health = const Health(pid: 1, version: '0.1.0', features: {'connections'});

void main() {
  testWidgets('a choose card marks the recommended option and answers', (
    tester,
  ) async {
    final client = _FakeClient();
    final store = _store(client)..authPrompts = [_prompt(_choose)];
    await tester.pumpWidget(_app(AuthCardOverlay(store: store)));
    expect(find.text('Connect Notion'), findsOneWidget);
    expect(find.text('Sign in (recommended)'), findsOneWidget);
    expect(find.text('Use a key'), findsOneWidget);
    expect(find.text('Paste a token'), findsOneWidget);
    expect(find.text('Tokens stay on this computer'), findsOneWidget);

    await tester.tap(find.text('Use a key'));
    await tester.pumpAndSettle();
    expect(client.calls, ['answer a1 key']);
    expect(store.authPrompts, isEmpty);
    expect(find.text('Connect Notion'), findsNothing);
  });

  testWidgets('a sign-in card offers a code only when it can; close ends it', (
    tester,
  ) async {
    final store = _store(_FakeClient())
      ..authPrompts = [
        _prompt(
          const AuthCard(
            kind: 'sign_in',
            title: 'Sign in to gmail · work',
            url: 'https://accounts.example/auth?x=1',
          ),
        ),
      ];
    await tester.pumpWidget(_app(AuthCardOverlay(store: store)));
    expect(find.text('Open sign-in page'), findsOneWidget);
    expect(find.text('https://accounts.example/auth?x=1'), findsOneWidget);
    expect(find.text('Use a code instead'), findsNothing);
    expect(find.textContaining('Waiting for you'), findsOneWidget);

    store.authPrompts = [
      _prompt(
        const AuthCard(
          kind: 'sign_in',
          title: 'Sign in to gmail · work',
          url: 'https://accounts.example/auth',
          deviceAvailable: true,
        ),
      ),
    ];
    store.onEventForTest(const ConnectionsChanged());
    await tester.pumpWidget(_app(AuthCardOverlay(store: store)));
    expect(find.text('Use a code instead'), findsOneWidget);

    store.onEventForTest(const AuthPromptClosed('a1'));
    await tester.pump();
    expect(find.text('Open sign-in page'), findsNothing);
  });

  testWidgets('a device card shows its code, and a new card replaces it', (
    tester,
  ) async {
    final store = _store(_FakeClient());
    store.onEventForTest(
      AuthPromptOpened(
        _prompt(
          const AuthCard(
            kind: 'sign_in',
            title: 'Sign in',
            url: 'https://x.test/a',
            deviceAvailable: true,
          ),
          callId: 't1',
        ),
      ),
    );
    store.onEventForTest(
      AuthPromptOpened(
        _prompt(
          const AuthCard(
            kind: 'sign_in',
            title: 'Sign in',
            userCode: 'ABCD-1234',
            verificationUri: 'https://x.test/device',
          ),
          id: 'a2',
          callId: 't1',
        ),
      ),
    );
    expect(store.authPrompts.map((p) => p.id), ['a2']);
    await tester.pumpWidget(_app(AuthCardOverlay(store: store)));
    expect(find.text('ABCD-1234'), findsOneWidget);
    expect(find.text('Open https://x.test/device'), findsOneWidget);
  });

  testWidgets('the agent call finishing closes its card', (tester) async {
    final store = _store(_FakeClient())
      ..authPrompts = [_prompt(_choose, callId: 't9')];
    store.onEventForTest(
      const ToolFinished(
        runId: 'r',
        id: 't9',
        name: 'auth_propose',
        isError: false,
        preview: '',
        output: '',
        replayed: false,
      ),
    );
    expect(store.authPrompts, isEmpty);
  });

  testWidgets('the view groups accounts by target with badges and actions', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(1000, 1000);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    final client = _FakeClient();
    final store = _store(client)..connectionList = _list;
    await tester.pumpWidget(_app(ConnectionsList(store: store)));
    expect(find.text('gmail'), findsOneWidget);
    expect(find.text('notion'), findsOneWidget);
    expect(find.text('Uses its default account'), findsOneWidget);
    expect(find.text('Add another account'), findsOneWidget);
    expect(find.text('Add account'), findsOneWidget);
    expect(find.text('default'), findsOneWidget);
    expect(find.text('needs reauth'), findsOneWidget);
    expect(find.text('token expired'), findsOneWidget);
    expect(find.text('work@x.com'), findsOneWidget);

    // Only the non-default account can be made the default.
    await tester.tap(find.text('Make default'));
    await tester.pumpAndSettle();
    expect(client.calls, ['default home']);

    await tester.tap(find.text('Remove').last);
    await tester.pumpAndSettle();
    expect(find.textContaining('tokens, keys, cache and grants'), findsOne);
    await tester.tap(find.text('Remove').last);
    await tester.pumpAndSettle();
    expect(client.calls.last, startsWith('remove '));
  });
}
