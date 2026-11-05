// The visual editor for fallback model lists (T6a): the editor widget on
// its own, and the default model's fallbacks in the Models dialog.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/model_chain_editor.dart';
import 'package:tern_desktop/ui/providers.dart';

const _lists = [
  ProviderModels(
    provider: 'anthropic',
    label: 'Anthropic',
    models: [
      ModelInfo(id: 'claude-sonnet-5-5', name: 'Claude Sonnet 5.5'),
      ModelInfo(id: 'claude-haiku-5-5', name: 'Claude Haiku 5.5'),
    ],
  ),
  ProviderModels(
    provider: 'openrouter',
    label: 'OpenRouter',
    models: [
      ModelInfo(id: 'kimi-k3'),
      ModelInfo(id: 'gpt-5'),
    ],
  ),
];

class _FakeClient extends TernClient {
  _FakeClient({this.chain = const ['anthropic:claude-sonnet-5-5']})
    : super(const Endpoint.unix('/nonexistent'));

  /// What `default_model` holds.
  List<String> chain;
  final saved = <List<String>>[];
  TernException? saveError;

  @override
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async => _lists;

  @override
  Future<List<ProviderStatus>> providers() async => [
    const ProviderStatus(
      name: 'anthropic',
      kind: ProviderKind.anthropic,
      secret: 'ANTHROPIC_API_KEY',
      keySource: 'file',
      keyHint: 'sk-ant…good',
      isDefault: true,
    ),
  ];

  @override
  Future<Info> info() async => Info(defaultModel: chain.first, tools: const []);

  @override
  Future<ConfigView> configSettings() async => ConfigView(
    path: '/x/config.toml',
    sections: [
      ConfigSection(
        id: 'models',
        title: 'Models',
        settings: [
          ConfigSetting(
            key: 'default_model',
            kind: SettingKind.model,
            value: chainToWire(chain),
          ),
        ],
      ),
    ],
  );

  @override
  Future<ConfigSetting> setDefaultModelChain(List<String> next) async {
    if (saveError case final e?) throw e;
    saved.add(next);
    chain = next;
    return ConfigSetting(
      key: 'default_model',
      kind: SettingKind.model,
      value: chainToWire(next),
    );
  }

  @override
  Future<List<Agent>> agents({bool archived = false}) async => const [];

  @override
  Future<List<Channel>> channels({bool archived = false}) async => const [];
}

Future<TernStore> _store(_FakeClient client, {bool owner = true}) async {
  final store = TernStore(client)
    ..link = Link.online
    ..health = Health(
      pid: 1,
      version: '0.2.0',
      features: const {'providers', 'agents'},
      authKind: 'device',
      scope: owner ? 'owner' : 'chat',
    )
    ..info = Info(defaultModel: client.chain.first, tools: const []);
  await store.loadModels();
  return store;
}

Widget _app(Widget child) => MaterialApp(
  theme: ternTheme(TernColors.light, Brightness.light),
  home: Scaffold(body: SingleChildScrollView(child: child)),
);

/// Holds the editor's list the way a dialog would.
class _Holder extends StatefulWidget {
  const _Holder({required this.store, required this.initial, this.onChange});

  final TernStore store;
  final List<String> initial;
  final void Function(List<String>)? onChange;

  @override
  State<_Holder> createState() => _HolderState();
}

class _HolderState extends State<_Holder> {
  late List<String> chain = widget.initial;

  @override
  Widget build(BuildContext context) => SizedBox(
    width: 520,
    child: ModelChainEditor(
      store: widget.store,
      chain: chain,
      onChanged: (v) {
        setState(() => chain = v);
        widget.onChange?.call(v);
      },
    ),
  );
}

void main() {
  late List<String> latest;

  Future<TernStore> editor(
    WidgetTester tester,
    List<String> chain, {
    _FakeClient? client,
  }) async {
    final store = await _store(client ?? _FakeClient(chain: chain));
    latest = chain;
    await tester.pumpWidget(
      _app(_Holder(store: store, initial: chain, onChange: (v) => latest = v)),
    );
    await tester.pumpAndSettle();
    return store;
  }

  group('the editor', () {
    testWidgets('labels the primary and what is tried next, with badges', (
      tester,
    ) async {
      await editor(tester, [
        'anthropic:claude-sonnet-5-5',
        'openrouter:kimi-k3',
        'anthropic:claude-haiku-5-5',
      ]);
      expect(find.text('PRIMARY'), findsOneWidget);
      expect(find.text('THEN TRY'), findsNWidgets(2));
      // The provider's name as a badge; the model id and its own name.
      expect(find.text('Anthropic'), findsNWidgets(2));
      expect(find.text('OpenRouter'), findsOneWidget);
      expect(find.textContaining('claude-sonnet-5-5'), findsOneWidget);
      expect(find.textContaining('Claude Sonnet 5.5'), findsOneWidget);
      // When fallbacks kick in, in a short note.
      expect(
        find.textContaining('If it fails (overloaded, rate-limited'),
        findsOneWidget,
      );
    });

    testWidgets('up and down buttons reorder; the ends are disabled', (
      tester,
    ) async {
      await editor(tester, ['a:1', 'b:2', 'c:3']);
      IconButton btn(String key) => tester.widget<IconButton>(
        find.descendant(
          of: find.byKey(Key(key)),
          matching: find.byType(IconButton),
        ),
      );
      expect(btn('chain-up-0').onPressed, isNull);
      expect(btn('chain-down-2').onPressed, isNull);

      await tester.tap(find.byKey(const Key('chain-down-0')));
      await tester.pumpAndSettle();
      expect(latest, ['b:2', 'a:1', 'c:3']);
      // The primary changed, so the labels follow the models, not the rows.
      expect(
        tester.widget<Text>(find.byKey(const Key('chain-label-0'))).data,
        'PRIMARY',
      );
      await tester.tap(find.byKey(const Key('chain-up-2')));
      await tester.pumpAndSettle();
      expect(latest, ['b:2', 'c:3', 'a:1']);
    });

    testWidgets('dragging the handle reorders', (tester) async {
      await editor(tester, ['a:1', 'b:2', 'c:3']);
      final handles = find.byIcon(Icons.drag_indicator);
      final g = await tester.startGesture(tester.getCenter(handles.first));
      await tester.pump(const Duration(milliseconds: 50));
      for (var i = 0; i < 12; i++) {
        await g.moveBy(const Offset(0, 24));
        await tester.pump(const Duration(milliseconds: 16));
      }
      await g.up();
      await tester.pumpAndSettle();
      expect(latest, ['b:2', 'c:3', 'a:1']);
    });

    testWidgets('remove drops a row; the last one can\'t go', (tester) async {
      await editor(tester, ['a:1', 'b:2']);
      await tester.tap(find.byKey(const Key('chain-remove-1')));
      await tester.pumpAndSettle();
      expect(latest, ['a:1']);
      expect(
        tester
            .widget<IconButton>(
              find.descendant(
                of: find.byKey(const Key('chain-remove-0')),
                matching: find.byType(IconButton),
              ),
            )
            .onPressed,
        isNull,
      );
    });

    testWidgets('add opens the searchable list; a repeat is refused', (
      tester,
    ) async {
      await editor(tester, ['anthropic:claude-sonnet-5-5']);
      await tester.tap(find.byKey(const Key('add-fallback')));
      await tester.pumpAndSettle();
      // Searching narrows the list.
      await tester.enterText(
        find.byKey(const ValueKey('model-filter')),
        'kimi',
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('kimi-k3', findRichText: true));
      await tester.pumpAndSettle();
      expect(latest, ['anthropic:claude-sonnet-5-5', 'openrouter:kimi-k3']);

      await tester.tap(find.byKey(const Key('add-fallback')));
      await tester.pumpAndSettle();
      await tester.enterText(
        find.byKey(const ValueKey('model-filter')),
        'kimi',
      );
      await tester.pumpAndSettle();
      await tester.tap(find.text('kimi-k3', findRichText: true).last);
      await tester.pumpAndSettle();
      expect(latest, hasLength(2));
      expect(find.text('That model is already in the list.'), findsOneWidget);
    });

    testWidgets('tapping a model replaces it in place', (tester) async {
      await editor(tester, [
        'anthropic:claude-sonnet-5-5',
        'openrouter:kimi-k3',
      ]);
      await tester.tap(find.byKey(const Key('chain-pick-1')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('gpt-5', findRichText: true));
      await tester.pumpAndSettle();
      expect(latest, ['anthropic:claude-sonnet-5-5', 'openrouter:gpt-5']);
    });

    testWidgets('a provider that is not connected is an error on its row', (
      tester,
    ) async {
      final store = await editor(tester, [
        'anthropic:claude-sonnet-5-5',
        'gemini:flash',
        'anthropic:claude-opus-9',
      ]);
      expect(
        find.text(
          'No connected provider is called "gemini". Connect it in Models, '
          'or pick a model from a provider you have.',
        ),
        findsOneWidget,
      );
      // Unlisted under a provider that is there: a warning, not an error.
      expect(
        find.textContaining("Anthropic doesn't list this model"),
        findsOneWidget,
      );
      expect(chainBlocked(store, latest), isTrue);
      expect(
        chainBlocked(store, [
          'anthropic:claude-sonnet-5-5',
          'anthropic:claude-opus-9',
        ]),
        isFalse,
      );
      // One already saved isn't held against the user.
      expect(
        chainBlocked(
          store,
          ['anthropic:claude-sonnet-5-5', 'gemini:flash'],
          saved: ['gemini:flash'],
        ),
        isFalse,
      );
      // A single model is the daemon's to check.
      expect(chainBlocked(store, ['gemini:flash']), isFalse);
    });
  });

  group('Models dialog', () {
    Future<_FakeClient> open(
      WidgetTester tester, {
      List<String> chain = const ['anthropic:claude-sonnet-5-5'],
      bool owner = true,
    }) async {
      tester.view.physicalSize = const Size(1200, 1400);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      final client = _FakeClient(chain: chain);
      final store = await _store(client, owner: owner);
      await tester.pumpWidget(_app(ProvidersDialog(store: store)));
      await tester.pumpAndSettle();
      return client;
    }

    testWidgets('shows the fallbacks the config holds', (tester) async {
      await open(
        tester,
        chain: ['anthropic:claude-sonnet-5-5', 'openrouter:kimi-k3'],
      );
      expect(
        find.text('Then, if it fails: openrouter:kimi-k3'),
        findsOneWidget,
      );
      expect(find.text('Edit fallbacks'), findsOneWidget);
    });

    testWidgets('adding a fallback writes default_model as a list', (
      tester,
    ) async {
      final client = await open(tester);
      expect(
        find.text('No fallbacks: if it fails, the message fails.'),
        findsOneWidget,
      );
      await tester.tap(find.byKey(const Key('edit-default-fallbacks')));
      await tester.pumpAndSettle();
      // Nothing to save until something changes.
      FilledButton save() => tester.widget<FilledButton>(
        find.byKey(const Key('save-default-fallbacks')),
      );
      expect(save().onPressed, isNull);

      await tester.tap(find.byKey(const Key('add-fallback')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('kimi-k3', findRichText: true));
      await tester.pumpAndSettle();
      expect(save().onPressed, isNotNull);
      await tester.tap(find.byKey(const Key('save-default-fallbacks')));
      await tester.pumpAndSettle();

      expect(client.saved, [
        ['anthropic:claude-sonnet-5-5', 'openrouter:kimi-k3'],
      ]);
      expect(
        find.text(
          'Default model: anthropic:claude-sonnet-5-5, then openrouter:kimi-k3.',
        ),
        findsOneWidget,
      );
      expect(
        find.text('Then, if it fails: openrouter:kimi-k3'),
        findsOneWidget,
      );
    });

    testWidgets('cancel throws the edits away', (tester) async {
      final client = await open(tester);
      await tester.tap(find.byKey(const Key('edit-default-fallbacks')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('add-fallback')));
      await tester.pumpAndSettle();
      await tester.tap(find.text('kimi-k3', findRichText: true));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('cancel-default-fallbacks')));
      await tester.pumpAndSettle();
      expect(client.saved, isEmpty);
      expect(find.byKey(const Key('chain-list')), findsNothing);
    });

    testWidgets('a bad entry can\'t be saved; a refusal is explained', (
      tester,
    ) async {
      final client = await open(
        tester,
        chain: ['anthropic:claude-sonnet-5-5', 'openrouter:kimi-k3'],
      );
      await tester.tap(find.byKey(const Key('edit-default-fallbacks')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('add-fallback')));
      await tester.pumpAndSettle();
      await tester.enterText(
        find.byKey(const ValueKey('model-filter')),
        'gemini:flash',
      );
      await tester.pumpAndSettle();
      await tester.tap(
        find.textContaining('Use "gemini:flash"', findRichText: true),
      );
      await tester.pumpAndSettle();
      expect(
        tester
            .widget<FilledButton>(
              find.byKey(const Key('save-default-fallbacks')),
            )
            .onPressed,
        isNull,
      );

      // Take it out and add a good one; this device may not write config.
      await tester.tap(find.byKey(const Key('chain-remove-2')));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('chain-down-0')));
      await tester.pumpAndSettle();
      client.saveError = const TernException('forbidden', status: 403);
      await tester.tap(find.byKey(const Key('save-default-fallbacks')));
      await tester.pumpAndSettle();
      expect(
        find.textContaining("isn't allowed to change models"),
        findsOneWidget,
      );
      // Still editing, nothing lost.
      expect(find.byKey(const Key('chain-list')), findsOneWidget);
      expect(client.saved, isEmpty);
    });

    testWidgets('a chat-scope device isn\'t offered the editor', (
      tester,
    ) async {
      await open(tester, owner: false);
      expect(find.byKey(const Key('edit-default-fallbacks')), findsNothing);
      expect(
        find.byKey(const Key('default-fallbacks-summary')),
        findsOneWidget,
      );
    });
  });
}
