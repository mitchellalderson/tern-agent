// "Connect a provider": presets, Test connection, the model picker's badges
// and manual entry, and the friendlier run-failed row (docs/provider-setup-spec.md
// §12). Also renders build/screenshots/presets_*_{light,dark}.png.
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/items.dart';
import 'package:tern_desktop/ui/model_picker.dart';
import 'package:tern_desktop/ui/providers.dart';

import 'providers_test.dart' show testPresets;

const _orModels = [
  ModelInfo(id: 'minimax/minimax-m3.1-flash', name: 'MiniMax M3.1 Flash'),
  ModelInfo(id: 'anthropic/claude-sonnet-4.5', name: 'Claude Sonnet 4.5'),
  ModelInfo(id: 'openai/gpt-4o-mini', name: 'GPT-4o mini'),
];

class _Fake extends TernClient {
  _Fake() : super(const Endpoint.unix('/nonexistent'));

  final verified = <ProviderDraft>[];
  final saved = <({ProviderDraft draft, String? defaultModel})>[];
  final providerList = <ProviderStatus>[];

  /// What the next verification answers; null means success.
  String? failWith;
  var verifyModels = _orModels;

  @override
  Future<Info> info() async => const Info(tools: [], setupNeeded: true);

  @override
  Future<List<ProviderStatus>> providers() async => providerList;

  @override
  Future<List<ProviderPreset>> providerPresets() async => testPresets;

  @override
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async => [
    const ProviderModels(
      provider: 'openrouter',
      label: 'OpenRouter',
      models: _orModels,
    ),
  ];

  @override
  Future<Verified> verifyProvider(
    ProviderDraft draft, {
    String? existing,
  }) async {
    verified.add(draft);
    final preset = testPresets.where((p) => p.id == draft.preset).firstOrNull;
    final bad = failWith;
    return Verified(
      ok: bad == null,
      name: draft.name ?? preset?.name ?? 'custom',
      message: bad,
      warnings: const [],
      keyHint: draft.key == null ? null : 'sk-or…test',
      models: bad == null ? verifyModels : const [],
    );
  }

  @override
  Future<ProviderStatus> saveProvider(
    ProviderDraft draft, {
    String? existing,
    String? defaultModel,
  }) async {
    saved.add((draft: draft, defaultModel: defaultModel));
    final preset = testPresets.where((p) => p.id == draft.preset).firstOrNull;
    return ProviderStatus(
      name: draft.name ?? preset?.name ?? 'custom',
      kind: draft.kind ?? ProviderKind.openaiCompat,
      keySource: 'file',
      isDefault: defaultModel != null,
    );
  }
}

TernStore _store(_Fake client) => TernStore(client)
  ..link = Link.online
  ..health = const Health(pid: 1, version: '0.1.0', features: {'providers'})
  ..info = const Info(tools: [], setupNeeded: true);

Widget _app(Widget home, {bool dark = false, GlobalKey? key}) {
  final c = dark ? TernColors.dark : TernColors.light;
  final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
  return RepaintBoundary(
    key: key,
    child: MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: theme.copyWith(
        textTheme: theme.textTheme.apply(fontFamily: '.AppleSystemUIFont'),
      ),
      home: home,
    ),
  );
}

Future<void> _shot(WidgetTester tester, GlobalKey key, String name) =>
    tester.runAsync(() async {
      final boundary =
          key.currentContext!.findRenderObject()! as RenderRepaintBoundary;
      final image = await boundary.toImage(pixelRatio: 2);
      final png = await image.toByteData(format: ui.ImageByteFormat.png);
      final out = File('build/screenshots/$name.png');
      await out.create(recursive: true);
      await out.writeAsBytes(png!.buffer.asUint8List());
    });

Future<void> _font(String family, List<String> paths) async {
  for (final p in paths) {
    final f = File(p);
    if (f.existsSync()) {
      final loader = FontLoader(family)
        ..addFont(Future.value(ByteData.sublistView(f.readAsBytesSync())));
      await loader.load();
      return;
    }
  }
}

Future<void> _pumpSetup(
  WidgetTester tester,
  _Fake client,
  TernStore store, {
  ValueChanged<ProviderStatus>? onDone,
  bool dark = false,
  GlobalKey? key,
}) async {
  await tester.pumpWidget(
    _app(
      Scaffold(
        body: Center(
          child: SizedBox(
            width: 560,
            child: Padding(
              padding: const EdgeInsets.all(20),
              child: SingleChildScrollView(
                child: ProviderSetup(store: store, onDone: onDone ?? (_) {}),
              ),
            ),
          ),
        ),
      ),
      dark: dark,
      key: key,
    ),
  );
  await tester.pump();
}

void main() {
  setUpAll(() async {
    await _font('.AppleSystemUIFont', [
      '/System/Library/Fonts/SFNS.ttf',
      '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf',
    ]);
    await _font('Menlo', [
      '/System/Library/Fonts/Menlo.ttc',
      '/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf',
    ]);
    if (Platform.environment['FLUTTER_ROOT'] case final root?) {
      await _font('MaterialIcons', [
        '$root/bin/cache/artifacts/material_fonts/MaterialIcons-Regular.otf',
      ]);
    }
  });

  group('modelRows', () {
    test('rows carry the provider label for a badge', () {
      final rows = modelRows(const [
        ProviderModels(
          provider: 'openrouter',
          label: 'OpenRouter',
          models: _orModels,
        ),
      ], '');
      expect(
        rows.whereType<ModelChoice>().every(
          (r) => r.providerLabel == 'OpenRouter',
        ),
        isTrue,
      );
    });

    test('suggestions stand in, labelled, when the list is empty', () {
      const lists = [
        ProviderModels(
          provider: 'openrouter',
          label: 'OpenRouter',
          models: [],
          suggested: ['openai/gpt-4o-mini', 'google/gemini-2.5-flash'],
        ),
      ];
      final rows = modelRows(lists, '');
      expect((rows.first as ModelHeader).note, contains('Suggestions'));
      final choices = rows.whereType<ModelChoice>().toList();
      expect(choices.map((c) => c.full), [
        'openrouter:openai/gpt-4o-mini',
        'openrouter:google/gemini-2.5-flash',
      ]);
      expect(choices.every((c) => c.suggested), isTrue);
      // Suggestions are filtered like models are.
      expect(
        modelRows(lists, 'gemini').whereType<ModelChoice>().single.id,
        'google/gemini-2.5-flash',
      );
      // Real models replace them.
      final real = modelRows(const [
        ProviderModels(
          provider: 'p',
          models: [ModelInfo(id: 'm')],
          suggested: ['s'],
        ),
      ], '');
      expect(real.whereType<ModelChoice>().map((c) => c.id), ['m']);
    });
  });

  testWidgets('OpenRouter: pick it, paste a key, test, pick a model', (
    tester,
  ) async {
    tester.view.physicalSize = const Size(900, 1000) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final client = _Fake();
    final store = _store(client);
    ProviderStatus? done;
    await _pumpSetup(tester, client, store, onDone: (p) => done = p);

    // Choices people recognise, no `provider:model` anywhere.
    for (final label in [
      'Anthropic',
      'OpenAI',
      'OpenRouter',
      'Google Gemini',
      'Groq',
      'Together AI',
      'Ollama (on this computer)',
      'Other (OpenAI-compatible URL)',
    ]) {
      expect(find.text(label), findsOneWidget, reason: label);
    }
    await tester.tap(find.text('OpenRouter'));
    await tester.pumpAndSettle();
    expect(find.text('Paste your OpenRouter API key'), findsOneWidget);
    expect(find.textContaining('openrouter.ai/keys'), findsOneWidget);
    expect(find.byKey(const ValueKey('preset-url')), findsOneWidget);
    expect(find.text('https://openrouter.ai/api/v1'), findsNothing);
    expect(
      tester.widget<Text>(find.byKey(const ValueKey('preset-url'))).data,
      'Talks to https://openrouter.ai/api/v1',
    );
    expect(find.text('Get an API key ↗'), findsOneWidget);

    await tester.enterText(find.byKey(const ValueKey('api-key')), 'sk-or-test');
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    // The draft names the preset; the daemon fills in the address and name.
    final draft = client.verified.single;
    expect(draft.preset, 'openrouter');
    expect(draft.baseUrl, isNull);
    expect(draft.name, isNull);
    expect(draft.key, 'sk-or-test');
    expect(find.textContaining('Connected to OpenRouter'), findsOneWidget);
    // Models carry the provider as a badge.
    expect(find.text('OpenRouter'), findsWidgets);
    final model = find.textContaining(
      'minimax/minimax-m3.1-flash',
      findRichText: true,
    );
    expect(model, findsOneWidget);

    await tester.tap(model);
    await tester.pumpAndSettle();
    expect(
      client.saved.single.defaultModel,
      'openrouter:minimax/minimax-m3.1-flash',
    );
    expect(done?.name, 'openrouter');
  });

  testWidgets('a failed test shows plain words and stays on the key', (
    tester,
  ) async {
    final client = _Fake()
      ..failWith =
          "openrouter.ai didn't recognise that address (HTTP 404). Check the "
          'URL; it usually ends in /v1.';
    final store = _store(client);
    await _pumpSetup(tester, client, store);
    await tester.tap(find.text('OpenRouter'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const ValueKey('api-key')), 'sk-or-test');
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('setup-error')), findsOneWidget);
    expect(
      find.textContaining("didn't recognise that address"),
      findsOneWidget,
    );
    expect(find.textContaining('{"error"'), findsNothing);
    expect(find.text('Test connection'), findsOneWidget);
    expect(client.saved, isEmpty);
  });

  testWidgets('a local preset needs no key', (tester) async {
    final client = _Fake();
    final store = _store(client);
    await _pumpSetup(tester, client, store);
    await tester.tap(find.text('Ollama (on this computer)'));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('api-key')), findsNothing);
    expect(find.text('Ollama (on this computer) needs no key'), findsOneWidget);
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    final draft = client.verified.single;
    expect(draft.preset, 'ollama');
    expect(draft.key, isNull);
    expect(find.textContaining('Connected to'), findsOneWidget);
  });

  testWidgets('"Enter a model id" saves it under the provider', (tester) async {
    final client = _Fake();
    final store = _store(client);
    await _pumpSetup(tester, client, store);
    await tester.tap(find.text('OpenRouter'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const ValueKey('api-key')), 'sk-or-test');
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const ValueKey('enter-model-id')));
    await tester.pumpAndSettle();
    expect(find.byKey(const ValueKey('use-typed-model')), findsOneWidget);
    await tester.enterText(
      find.byKey(const ValueKey('model-filter')),
      'vendor/some-new-model',
    );
    await tester.pump();
    await tester.tap(find.byKey(const ValueKey('use-typed-model')));
    await tester.pumpAndSettle();
    // The provider is added for the user.
    expect(
      client.saved.single.defaultModel,
      'openrouter:vendor/some-new-model',
    );
  });

  testWidgets('suggestions show when the provider lists no models', (
    tester,
  ) async {
    final client = _Fake()..verifyModels = const [];
    final store = _store(client);
    await _pumpSetup(tester, client, store);
    await tester.tap(find.text('OpenRouter'));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const ValueKey('api-key')), 'sk-or-test');
    await tester.tap(find.text('Test connection'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Suggestions below'), findsOneWidget);
    await tester.tap(
      find.textContaining('openai/gpt-4o-mini', findRichText: true),
    );
    await tester.pumpAndSettle();
    expect(client.saved.single.defaultModel, 'openrouter:openai/gpt-4o-mini');
  });

  group('the run-failed row', () {
    const raw = 'HTTP 404: {"error":{"message":"Not Found","code":404}}';

    TernStore store(_Fake client) => _store(client)
      ..info = const Info(
        defaultModel: 'openrouter:minimax/minimax-m3.1-flash',
        tools: [],
      )
      ..modelLists = const [
        ProviderModels(
          provider: 'openrouter',
          label: 'OpenRouter',
          models: _orModels,
        ),
      ];

    testWidgets('says what went wrong and offers Fix in Models', (
      tester,
    ) async {
      final client = _Fake();
      final s = store(client);
      await tester.pumpWidget(
        _app(
          Scaffold(
            body: Padding(
              padding: const EdgeInsets.all(24),
              child: ItemView(
                item: RunEndItem(runId: 'r1', status: 'failed', error: raw),
                store: s,
              ),
            ),
          ),
        ),
      );
      expect(
        find.text(
          "OpenRouter couldn't find that endpoint or model. Check the model "
          'name.',
        ),
        findsOneWidget,
      );
      expect(find.textContaining('HTTP 404'), findsNothing);
      await tester.tap(find.byKey(const ValueKey('show-error-details')));
      await tester.pump();
      expect(
        tester
            .widget<SelectableText>(find.byKey(const ValueKey('raw-error')))
            .data,
        raw,
      );

      await tester.tap(find.byKey(const ValueKey('fix-in-models')));
      await tester.pumpAndSettle();
      expect(find.byType(ProvidersDialog), findsOneWidget);
    });

    testWidgets('a rate limit has no Fix button; unknown text is shown as is', (
      tester,
    ) async {
      final client = _Fake();
      final s = store(client);
      Future<void> show(String error) => tester.pumpWidget(
        _app(
          Scaffold(
            body: ItemView(
              item: RunEndItem(runId: 'r1', status: 'failed', error: error),
              store: s,
            ),
          ),
        ),
      );
      await show('HTTP 429: slow down');
      expect(find.textContaining('limiting requests'), findsOneWidget);
      expect(find.byKey(const ValueKey('fix-in-models')), findsNothing);
      await show('tool exploded');
      expect(find.text('Run failed: tool exploded'), findsOneWidget);
      expect(find.byKey(const ValueKey('show-error-details')), findsNothing);
    });
  });

  for (final dark in [false, true]) {
    final mode = dark ? 'dark' : 'light';
    testWidgets('screenshots $mode', (tester) async {
      tester.view.physicalSize = const Size(640, 760) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final key = GlobalKey();
      final client = _Fake();
      final s = _store(client);

      await _pumpSetup(tester, client, s, dark: dark, key: key);
      await tester.pumpAndSettle();
      await _shot(tester, key, 'presets_choices_$mode');

      await tester.tap(find.text('OpenRouter'));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'presets_key_$mode');

      client.failWith =
          "openrouter.ai didn't recognise that address (HTTP 404). Check the "
          'URL; it usually ends in /v1.';
      await tester.enterText(
        find.byKey(const ValueKey('api-key')),
        'sk-or-v1-abcdef',
      );
      await tester.tap(find.text('Test connection'));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'presets_error_$mode');

      client.failWith = null;
      await tester.tap(find.text('Test connection'));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'presets_models_$mode');

      await tester.tap(find.byKey(const ValueKey('enter-model-id')));
      await tester.pumpAndSettle();
      await tester.enterText(
        find.byKey(const ValueKey('model-filter')),
        'vendor/model-name',
      );
      await tester.pump();
      await _shot(tester, key, 'presets_manual_$mode');

      // The failed-run row.
      tester.view.physicalSize = const Size(900, 220) * 2;
      final s2 = _store(client)
        ..info = const Info(
          defaultModel: 'openrouter:minimax/minimax-m3.1-flash',
          tools: [],
        )
        ..modelLists = const [
          ProviderModels(
            provider: 'openrouter',
            label: 'OpenRouter',
            models: _orModels,
          ),
        ];
      await tester.pumpWidget(
        _app(
          Scaffold(
            body: Padding(
              padding: const EdgeInsets.all(24),
              child: ItemView(
                item: RunEndItem(
                  runId: 'r1',
                  status: 'failed',
                  error:
                      'HTTP 404: {"error":{"message":"Not Found","code":404}}',
                ),
                store: s2,
              ),
            ),
          ),
          dark: dark,
          key: key,
        ),
      );
      await tester.tap(find.byKey(const ValueKey('show-error-details')));
      await tester.pump();
      await _shot(tester, key, 'presets_run_failed_$mode');
      expect(tester.takeException(), isNull);
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
