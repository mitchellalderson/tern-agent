// Provider setup and the model picker against a fake client
// (docs/provider-setup-spec.md §7.2). Also renders
// build/screenshots/{welcome,models,model_picker}_{light,dark}.png.
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/model_picker.dart';
import 'package:tern_desktop/ui/providers.dart';

const _models = [
  ModelInfo(
    id: 'claude-opus-5-5',
    name: 'Claude Opus 5.5',
    contextWindow: 200000,
  ),
  ModelInfo(
    id: 'claude-sonnet-5-5',
    name: 'Claude Sonnet 5.5',
    contextWindow: 200000,
  ),
  ModelInfo(
    id: 'claude-haiku-5-5',
    name: 'Claude Haiku 5.5',
    contextWindow: 200000,
  ),
];

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final providerList = <ProviderStatus>[];
  final verified = <ProviderDraft>[];
  final saved = <({ProviderDraft draft, String? defaultModel})>[];
  final sessionModels = <String>[];
  final defaults = <String>[];
  var setupNeeded = true;

  @override
  Future<Info> info() async => Info(
    defaultModel: defaults.lastOrNull,
    tools: const [],
    setupNeeded: setupNeeded,
  );

  @override
  Future<List<ProviderStatus>> providers() async => providerList;

  @override
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async => [
    const ProviderModels(provider: 'anthropic', models: _models),
    const ProviderModels(
      provider: 'ollama',
      message: "Couldn't reach localhost: connection refused. Is the server running?",
      models: [],
    ),
  ];

  @override
  Future<Verified> verifyProvider(
    ProviderDraft draft, {
    String? existing,
  }) async {
    verified.add(draft);
    final ok = draft.useEnvKey || draft.key != 'sk-bad';
    return Verified(
      ok: ok,
      name:
          draft.name ??
          (draft.kind == ProviderKind.anthropic ? 'anthropic' : 'local'),
      message: ok
          ? null
          : 'anthropic rejected that key. Check you copied all of it.',
      warnings: const [],
      keyHint: draft.key == null ? null : 'sk-ant…good',
      models: ok ? _models : const [],
    );
  }

  @override
  Future<ProviderStatus> saveProvider(
    ProviderDraft draft, {
    String? existing,
    String? defaultModel,
  }) async {
    saved.add((draft: draft, defaultModel: defaultModel));
    if (defaultModel != null) defaults.add(defaultModel);
    setupNeeded = false;
    final s = ProviderStatus(
      name: draft.name ?? 'anthropic',
      kind: draft.kind ?? ProviderKind.anthropic,
      secret: 'ANTHROPIC_API_KEY',
      keySource: 'file',
      keyHint: 'sk-ant…good',
      isDefault: defaultModel != null,
    );
    providerList.add(s);
    return s;
  }

  @override
  Future<Resolved> checkModel(String model) async => Resolved(
    model: model.contains(':') ? model : 'anthropic:$model',
    warning: model.contains('mystery') ? 'not in the list' : null,
  );

  @override
  Future<void> setModel(String session, String model) async =>
      sessionModels.add(model);

  @override
  Future<Resolved> setDefaultModel(String model) async {
    defaults.add(model);
    return Resolved(model: model);
  }
}

TernStore _store(_FakeClient client) => TernStore(client)
  ..link = Link.online
  ..health = const Health(pid: 1, version: '0.1.0', features: {'providers'})
  ..info = Info(tools: const [], setupNeeded: client.setupNeeded);

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
    const lists = [
      ProviderModels(provider: 'anthropic', models: _models),
      ProviderModels(provider: 'local', message: 'down', models: []),
    ];

    test('current first, then every provider', () {
      final rows = modelRows(lists, '', current: 'anthropic:claude-haiku-5-5');
      expect(rows.first, isA<ModelHeader>());
      expect((rows[1] as ModelChoice).current, isTrue);
      expect(rows.whereType<ModelChoice>().length, 4);
      expect((rows.last as ModelHeader).note, "Couldn't list models: down");
    });

    test('filters by subsequence and offers the typed text', () {
      final rows = modelRows(lists, 'opus');
      expect(rows.whereType<ModelChoice>().map((r) => r.full), [
        'anthropic:claude-opus-5-5',
      ]);
      expect((rows.last as TypedChoice).text, 'opus');
      expect(
        modelRows(lists, 'Sonnet 5').whereType<ModelChoice>().single.id,
        'claude-sonnet-5-5',
        reason: 'display names match too',
      );
      expect(modelRows(lists, 'zzz').single, isA<TypedChoice>());
    });
  });

  testWidgets('first run: Anthropic, a rejected key, then a good one', (
    tester,
  ) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
    tester.view.physicalSize = const Size(1280, 860) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final client = _FakeClient();
    final store = _store(client);
    await tester.pumpWidget(_app(Home(store: store)));
    await tester.pump();
    expect(find.text('Connect a model to get started'), findsOneWidget);
    expect(find.text('Set up a model…'), findsOneWidget);

    await tester.tap(find.text('Anthropic'));
    await tester.pumpAndSettle();
    expect(find.text('Paste your Anthropic API key'), findsOneWidget);
    expect(find.text('Get an API key ↗'), findsOneWidget);
    final field = find.byKey(const ValueKey('api-key'));
    expect(tester.widget<TextField>(field).obscureText, isTrue);

    await tester.tap(find.text('Check key'));
    await tester.pump();
    expect(find.text('Paste your API key first.'), findsOneWidget);

    await tester.enterText(field, 'sk-bad');
    await tester.tap(find.text('Check key'));
    await tester.pumpAndSettle();
    expect(find.textContaining('rejected that key'), findsOneWidget);

    // A paste (a jump of many characters) is checked on its own.
    await tester.enterText(field, '');
    await tester.enterText(field, 'sk-ant-api03-good');
    await tester.pump(const Duration(milliseconds: 500));
    await tester.pumpAndSettle();
    expect(client.verified.last.key, 'sk-ant-api03-good');
    expect(find.textContaining('Key sk-ant…good works'), findsOneWidget);
    final sonnet = find.textContaining('claude-sonnet-5-5', findRichText: true);
    expect(sonnet, findsOneWidget);

    await tester.tap(sonnet);
    await tester.pumpAndSettle();
    expect(client.saved.single.defaultModel, 'anthropic:claude-sonnet-5-5');
    expect(client.saved.single.draft.key, 'sk-ant-api03-good');
    expect(store.setupNeeded, isFalse);
    expect(store.info?.defaultModel, 'anthropic:claude-sonnet-5-5');
    expect(find.text('Connect a model to get started'), findsNothing);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('an OpenAI-compatible server with no key and a typed model', (
    tester,
  ) async {
    final client = _FakeClient();
    final store = _store(client);
    ProviderStatus? done;
    await tester.pumpWidget(
      _app(
        Scaffold(
          body: SingleChildScrollView(
            child: ProviderSetup(store: store, onDone: (p) => done = p),
          ),
        ),
      ),
    );
    await tester.tap(find.text('OpenAI-compatible'));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const ValueKey('base-url')),
      'http://localhost:11434/v1',
    );
    await tester.enterText(
      find.byKey(const ValueKey('provider-name')),
      'ollama',
    );
    await tester.pump();
    await tester.tap(find.text('Next'));
    await tester.pumpAndSettle();
    expect(find.textContaining("Leave it blank"), findsOneWidget);
    await tester.tap(find.text('Check key'));
    await tester.pumpAndSettle();
    final draft = client.verified.single;
    expect(draft.key, isNull);
    expect(draft.baseUrl, 'http://localhost:11434/v1');
    expect(draft.name, 'ollama');

    await tester.enterText(
      find.byKey(const ValueKey('model-filter')),
      'llama3:8b',
    );
    await tester.pump();
    await tester.tap(
      find.textContaining('Use "llama3:8b"', findRichText: true),
    );
    await tester.pumpAndSettle();
    expect(client.saved.single.defaultModel, 'llama3:8b');
    expect(done?.name, 'ollama');
  });

  testWidgets('the model chip picks a model for a new conversation', (
    tester,
  ) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
    tester.view.physicalSize = const Size(1280, 800) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final client = _FakeClient()..setupNeeded = false;
    final store = _store(client)
      ..info = const Info(defaultModel: 'anthropic:claude-opus-5-5', tools: []);
    await tester.pumpWidget(_app(Home(store: store)));
    await tester.pump();
    await tester.tap(
      find.widgetWithText(TextButton, 'anthropic:claude-opus-5-5'),
    );
    await tester.pumpAndSettle();
    expect(
      find.text(
        "Couldn't list models: Couldn't reach localhost: "
        'connection refused. Is the server running?',
      ),
      findsOneWidget,
    );

    await tester.enterText(find.byKey(const ValueKey('model-filter')), 'haiku');
    await tester.pump();
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pumpAndSettle();
    // No conversation yet: remembered for the first message.
    expect(store.draftModel, 'anthropic:claude-haiku-5-5');
    expect(
      find.widgetWithText(TextButton, 'anthropic:claude-haiku-5-5'),
      findsOneWidget,
    );
    expect(client.defaults, isEmpty);

    // A typed model the provider doesn't list, also made the default.
    // Pressed directly: under the test clock the title bar's double-click
    // recognizer still holds the first tap, so a second tap would zoom.
    tester
        .widget<TextButton>(
          find.widgetWithText(TextButton, 'anthropic:claude-haiku-5-5'),
        )
        .onPressed!();
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const ValueKey('model-filter')),
      'mystery',
    );
    await tester.tap(find.byType(Checkbox));
    await tester.pump();
    await tester.tap(find.textContaining('Use "mystery"', findRichText: true));
    await tester.pumpAndSettle();
    expect(client.defaults, ['anthropic:mystery']);
    expect(store.modelWarning, 'not in the list');
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('an older daemon gets an explanation, not a 404', (tester) async {
    final client = _FakeClient();
    final store = _store(client)
      ..health = const Health(pid: 1, version: '0.1.0');
    expect(store.daemonTooOld, isTrue);
    await tester.pumpWidget(
      _app(Scaffold(body: ProvidersDialog(store: store))),
    );
    await tester.pump();
    expect(find.textContaining('older than this app'), findsOneWidget);
    await store.loadModels();
    expect(store.modelLists, isEmpty);
  });

  for (final dark in [false, true]) {
    final mode = dark ? 'dark' : 'light';
    testWidgets('screenshots $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final key = GlobalKey();

      final client = _FakeClient();
      final store = _store(client);
      await tester.pumpWidget(
        _app(
          Home(store: store),
          dark: dark,
          key: key,
        ),
      );
      await tester.pump();
      await tester.tap(find.text('Anthropic'));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'welcome_$mode');

      client
        ..setupNeeded = false
        ..providerList.addAll([
          const ProviderStatus(
            name: 'anthropic',
            kind: ProviderKind.anthropic,
            secret: 'ANTHROPIC_API_KEY',
            keySource: 'file',
            keyHint: 'sk-ant…a1B2',
            isDefault: true,
          ),
          const ProviderStatus(
            name: 'ollama',
            kind: ProviderKind.openaiCompat,
            baseUrl: 'http://localhost:11434/v1',
            keySource: 'none',
            error: ListError('unreachable', 'connection refused'),
            message:
                "Couldn't reach localhost: connection refused. "
                'Is the server running?',
            isDefault: false,
          ),
        ]);
      store.info = const Info(
        defaultModel: 'anthropic:claude-sonnet-5-5',
        tools: [],
      );
      await tester.pumpWidget(
        _app(
          Home(store: store),
          dark: dark,
          key: key,
        ),
      );
      await tester.pump();
      showProvidersDialog(tester.element(find.byType(Home)), store);
      await tester.pumpAndSettle();
      await _shot(tester, key, 'models_$mode');
      Navigator.of(tester.element(find.byType(ProvidersDialog))).pop();
      await tester.pumpAndSettle();

      // Pressed directly, as in the chip test (the title bar's double-click
      // recognizer holds earlier taps under the test clock).
      tester
          .widget<TextButton>(
            find.widgetWithText(TextButton, 'anthropic:claude-sonnet-5-5'),
          )
          .onPressed!();
      await tester.pumpAndSettle();
      expect(find.byType(ModelPickerDialog), findsOneWidget);
      await _shot(tester, key, 'model_picker_$mode');
      expect(tester.takeException(), isNull);
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
