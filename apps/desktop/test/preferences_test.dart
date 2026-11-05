// The Settings window against a fake client serving a real
// `/v1/config/settings` reply (fixtures/config_settings.json). Also renders
// it with real fonts to build/screenshots/settings_{light,dark}.png:
//   flutter test test/preferences_test.dart
import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/daemon.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/config_editor.dart';
import 'package:tern_desktop/ui/preferences.dart';
import 'package:tern_desktop/ui/providers.dart';
import 'package:tern_desktop/ui/secrets.dart';

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

/// Keeps the fixture's settings and changes them as the daemon would:
/// what it started with is the fixture, so restart settings changed from
/// it are pending.
class _FakeClient extends TernClient {
  _FakeClient({this.failWith}) : super(const Endpoint.unix('/nonexistent')) {
    final j = jsonDecode(
      File('test/fixtures/config_settings.json').readAsStringSync(),
    ) as Json;
    view = j;
    for (final sec in j['sections'] as List) {
      for (final s in (sec as Json)['settings'] as List) {
        final setting = s as Json;
        settings[setting['key'] as String] = setting;
        running[setting['key'] as String] = setting['value'];
      }
    }
  }

  final int? failWith;
  late final Json view;
  final settings = <String, Json>{};
  final running = <String, Object?>{};
  final calls = <String>[];

  @override
  Future<List<ProviderStatus>> providers() async => const [
    ProviderStatus(
      name: 'anthropic',
      kind: ProviderKind.anthropic,
      secret: 'ANTHROPIC_API_KEY',
      keySource: 'file',
      keyHint: 'sk-ant…a1B2',
      isDefault: true,
    ),
  ];

  @override
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async => const [];

  @override
  Future<List<SecretStatus>> secrets() async => const [
    SecretStatus(
      name: 'ANTHROPIC_API_KEY',
      inFile: true,
      inEnv: false,
      usedBy: ['provider anthropic'],
    ),
  ];

  @override
  Future<ConfigView> configSettings() async {
    if (failWith case final s?) throw TernException('nope', status: s);
    return ConfigView.fromJson(view);
  }

  Json _update(String key, Object? value, {required bool set}) {
    final s = settings[key]!;
    s['value'] = value;
    s['set'] = set;
    s['restart_pending'] =
        s['applies'] == 'restart' &&
        jsonEncode(value) != jsonEncode(running[key]);
    s['risky'] = switch (key) {
      'sandbox.network' => value == true,
      'sandbox.confine' => value == 'off',
      _ => false,
    };
    return s;
  }

  @override
  Future<ConfigSetting> setSetting(String key, Object? value) async {
    calls.add('set $key ${jsonEncode(value)}');
    if (key == 'subagents.max_depth' && value is int && value > 3) {
      throw const TernException(
        'subagents.max_depth: must be at most 3',
        status: 400,
      );
    }
    return ConfigSetting.fromJson(_update(key, value, set: true));
  }

  @override
  Future<ConfigSetting> resetSetting(String key) async {
    calls.add('reset $key');
    final s = settings[key]!;
    return ConfigSetting.fromJson(_update(key, s['default'], set: false));
  }
}

Future<TernStore> _pump(
  WidgetTester tester,
  TernClient client, {
  bool dark = false,
  GlobalKey? key,
  List<String>? revealed,
  Future<void> Function()? onRestart,
  PrefSection section = PrefSection.general,
}) async {
  tester.view.physicalSize = const Size(1000, 780) * 2;
  tester.view.devicePixelRatio = 2;
  addTearDown(tester.view.reset);
  final store = TernStore(client);
  addTearDown(store.dispose);
  final c = dark ? TernColors.dark : TernColors.light;
  final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
  await tester.pumpWidget(
    RepaintBoundary(
      key: key,
      child: MaterialApp(
        debugShowCheckedModeBanner: false,
        theme: theme.copyWith(
          textTheme: theme.textTheme.apply(fontFamily: '.AppleSystemUIFont'),
        ),
        home: Scaffold(
          body: PreferencesDialog(
            control: TernControl(
              bin: null,
              env: const {'HOME': '/nonexistent'},
            ),
            prefs: AppPrefs(File('/nonexistent/desktop.json')),
            store: store,
            onRestart: onRestart,
            section: section,
            reveal: (p) async => revealed?.add(p),
          ),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
  return store;
}

Finder _row(String key) =>
    find.byWidgetPredicate((w) => w is SettingRow && w.setting.key == key);

/// Scroll to [f] in the settings pane and tap it.
Future<void> _tap(WidgetTester tester, Finder f) async {
  await tester.ensureVisible(f);
  await tester.pumpAndSettle();
  await tester.tap(f);
  await tester.pumpAndSettle();
}

Future<void> _section(WidgetTester tester, PrefSection s) async {
  await tester.tap(find.byKey(Key('settings-nav-${s.name}')));
  await tester.pumpAndSettle();
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
    final flutterRoot = Platform.environment['FLUTTER_ROOT'];
    if (flutterRoot != null) {
      await _font('MaterialIcons', [
        '$flutterRoot/bin/cache/artifacts/material_fonts/MaterialIcons-Regular.otf',
      ]);
    }
  });

  testWidgets('one window, every section, General first', (tester) async {
    await _pump(tester, _FakeClient());
    // Folders appears only on a core with workspaces (workspaces_test.dart).
    for (final s in PrefSection.values.where((s) => s != PrefSection.folders)) {
      expect(find.byKey(Key('settings-nav-${s.name}')), findsOneWidget);
    }
    expect(find.byKey(const Key('settings-nav-folders')), findsNothing);
    expect(find.text('Start tern when I log in'), findsOneWidget);
    expect(_row('workspace'), findsOneWidget);
    // Settings that belong elsewhere are not here.
    expect(_row('sandbox.network'), findsNothing);
    expect(_row('secret_request_timeout_secs'), findsNothing);
  });

  testWidgets('permissions: a flagged toggle, restart hint and reset', (
    tester,
  ) async {
    final client = _FakeClient();
    var restarted = false;
    await _pump(tester, client, onRestart: () async => restarted = true);
    await _section(tester, PrefSection.permissions);
    expect(_row('autonomy'), findsOneWidget);
    expect(_row('taint.egress'), findsOneWidget);
    final net = _row('sandbox.network');
    expect(
      find.descendant(of: net, matching: find.textContaining('internet')),
      findsOneWidget,
    );
    expect(
      find.descendant(of: net, matching: find.byIcon(Icons.shield_outlined)),
      findsOneWidget,
      reason: 'network on is the risky value',
    );
    expect(find.text('Restart tern'), findsNothing);

    await _tap(tester, find.descendant(of: net, matching: find.byType(Switch)));
    expect(client.calls, ['set sandbox.network false']);
    expect(
      find.descendant(of: net, matching: find.text('Restart to apply')),
      findsOneWidget,
    );
    expect(find.text('1 saved change applies when tern restarts.'), findsOne);

    await _tap(
      tester,
      find.descendant(
        of: net,
        matching: find.byTooltip('Reset to default (true)'),
      ),
    );
    expect(client.calls.last, 'reset sandbox.network');
    expect(find.text('Restart tern'), findsNothing);

    await _tap(
      tester,
      find.descendant(of: _row('sandbox.confine'), matching: find.text('off')),
    );
    expect(client.calls.last, 'set sandbox.confine "off"');
    await _tap(tester, find.text('Restart tern'));
    expect(restarted, isTrue);
    expect(find.byType(PreferencesDialog), findsNothing);
  });

  testWidgets('limits: numbers are parsed and server errors show inline', (
    tester,
  ) async {
    final client = _FakeClient();
    await _pump(tester, client);
    await _section(tester, PrefSection.limits);
    expect(_row('subagents.max_result_chars'), findsOneWidget);
    expect(_row('schedules.usd_per_day'), findsOneWidget);
    // Tuning knobs live under Advanced.
    expect(_row('subagents.max_children'), findsNothing);

    Future<void> enter(String key, String text) async {
      await tester.enterText(
        find.descendant(of: _row(key), matching: find.byType(TextField)),
        text,
      );
      await tester.testTextInput.receiveAction(TextInputAction.done);
      await tester.pumpAndSettle();
    }

    await enter('max_iterations', 'lots');
    expect(find.text('A whole number'), findsOneWidget);
    expect(client.calls, isEmpty);
    await enter('max_iterations', '25');
    expect(client.calls, ['set max_iterations 25']);

    await enter('subagents.max_result_chars', '12000');
    expect(client.calls.last, 'set subagents.max_result_chars 12000');

    await enter('subagents.max_depth', '9');
    expect(find.text('subagents.max_depth: must be at most 3'), findsOneWidget);
    expect(
      find.descendant(
        of: _row('subagents.max_depth'),
        matching: find.text('Changed'),
      ),
      findsOneWidget,
    );
  });

  testWidgets('models embeds the providers editor, once, without a copy', (
    tester,
  ) async {
    final client = _FakeClient();
    await _pump(tester, client);
    await _section(tester, PrefSection.models);
    expect(find.byType(ProvidersDialog), findsOneWidget);
    expect(find.text('Connect a provider'), findsOneWidget);
    expect(find.text('anthropic'), findsWidgets);
    // The default model is edited by the providers editor's chain editor,
    // not by a second row.
    expect(_row('default_model'), findsNothing);
    expect(_row('subagents.default_model'), findsOneWidget);
    // The embedded editor has no window chrome of its own: the one Done is
    // Settings'.
    expect(find.text('Done'), findsOneWidget);
    await tester.enterText(
      find.descendant(
        of: _row('subagents.default_model'),
        matching: find.byType(TextField),
      ),
      'anthropic:a, openrouter:b',
    );
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    expect(
      client.calls.last,
      'set subagents.default_model ["anthropic:a","openrouter:b"]',
    );
  });

  testWidgets('secrets embeds the editor; values are never shown', (
    tester,
  ) async {
    await _pump(tester, _FakeClient(), section: PrefSection.secrets);
    expect(find.byType(SecretsDialog), findsOneWidget);
    expect(find.text('ANTHROPIC_API_KEY'), findsOneWidget);
    expect(find.textContaining("can't be shown again"), findsOneWidget);
    expect(find.text('Add secret'), findsOneWidget);
  });

  testWidgets('advanced lists tuning knobs and reveals the file', (
    tester,
  ) async {
    final revealed = <String>[];
    await _pump(tester, _FakeClient(), revealed: revealed);
    await _section(tester, PrefSection.advanced);
    expect(_row('context_window'), findsOneWidget);
    expect(_row('subagents.max_children'), findsOneWidget);
    expect(_row('api.trust_proxy'), findsOneWidget);
    expect(_row('workspace'), findsNothing);
    await tester.tap(find.text('Open config.toml'));
    expect(revealed, ['/Users/you/.tern/config.toml']);
  });

  testWidgets('about shows versions and where things are kept', (tester) async {
    final store = await _pump(tester, _FakeClient());
    store.health = const Health(pid: 1, version: '0.4.2');
    await _section(tester, PrefSection.about);
    expect(find.textContaining('tern desktop'), findsOneWidget);
    expect(find.text('/Users/you/.tern'), findsOneWidget);
    expect(find.text('/Users/you/.tern/logs'), findsOneWidget);
    expect(find.textContaining('no telemetry'), findsOneWidget);
    expect(find.byKey(const Key('about-uninstall')), findsOneWidget);
    expect(
      find.textContaining('cp -R "/Users/you/.tern" ~/tern-backup'),
      findsOneWidget,
    );
  });

  testWidgets('search finds settings and sections across the window', (
    tester,
  ) async {
    await _pump(tester, _FakeClient());
    final search = find.byKey(const Key('settings-search'));
    await tester.enterText(search, 'timeout');
    await tester.pumpAndSettle();
    expect(_row('sandbox.shell_timeout_secs'), findsOneWidget);
    expect(_row('subagents.ask_parent_timeout_secs'), findsOneWidget);
    expect(_row('secret_request_timeout_secs'), findsOneWidget);
    expect(_row('sandbox.network'), findsNothing);
    expect(find.text('Shell timeout (seconds)'), findsOneWidget);

    // A section with an editor of its own answers too, and opens.
    await tester.enterText(search, 'api key');
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('settings-result-secrets')), findsOneWidget);
    await tester.tap(find.byKey(const Key('settings-result-secrets')));
    await tester.pumpAndSettle();
    expect(find.byType(SecretsDialog), findsOneWidget);
    expect(tester.widget<TextField>(search).controller!.text, isEmpty);

    await tester.enterText(search, 'zzzz');
    await tester.pumpAndSettle();
    expect(find.textContaining('No setting matches'), findsOneWidget);
  });

  testWidgets('a device without owner scope is told so', (tester) async {
    await _pump(tester, _FakeClient(failWith: 403));
    expect(find.textContaining('paired as owner'), findsOneWidget);
  });

  for (final dark in [false, true]) {
    testWidgets('screenshot ${dark ? 'dark' : 'light'}', (tester) async {
      final client = _FakeClient();
      final key = GlobalKey();
      await _pump(
        tester,
        client,
        dark: dark,
        key: key,
        onRestart: () async {},
        section: PrefSection.permissions,
      );
      await _tap(
        tester,
        find.descendant(
          of: _row('sandbox.confine'),
          matching: find.text('required'),
        ),
      );
      expect(tester.takeException(), isNull);
      await tester.runAsync(() async {
        final boundary =
            key.currentContext!.findRenderObject()! as RenderRepaintBoundary;
        final image = await boundary.toImage(pixelRatio: 2);
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        final out = File(
          'build/screenshots/settings_${dark ? 'dark' : 'light'}.png',
        );
        await out.create(recursive: true);
        await out.writeAsBytes(png!.buffer.asUint8List());
      });
    });
  }
}
