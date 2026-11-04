// The configuration window against a fake client serving a real
// `/v1/config/settings` reply (fixtures/config_settings.json). Also renders
// it with real fonts to build/screenshots/config_{light,dark}.png:
//   flutter test test/config_dialog_test.dart
import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/config_editor.dart';

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

Future<void> _pump(
  WidgetTester tester,
  TernClient client, {
  bool dark = false,
  GlobalKey? key,
  List<String>? revealed,
  Future<void> Function()? onRestart,
}) async {
  tester.view.physicalSize = const Size(900, 760) * 2;
  tester.view.devicePixelRatio = 2;
  addTearDown(tester.view.reset);
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
          body: ConfigDialog(
            client: client,
            onRestart: onRestart,
            reveal: (p) async => revealed?.add(p),
          ),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

Finder _row(String key) =>
    find.byWidgetPredicate((w) => w is SettingRow && w.setting.key == key);

Future<void> _section(WidgetTester tester, String title) async {
  await tester.tap(find.text(title).first);
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

  testWidgets('sections, a flagged toggle, restart hint and reset', (
    tester,
  ) async {
    final client = _FakeClient();
    var restarted = false;
    final revealed = <String>[];
    await _pump(
      tester,
      client,
      revealed: revealed,
      onRestart: () async => restarted = true,
    );
    for (final t in ['General', 'Models', 'Shell & sandbox', 'API & devices']) {
      expect(find.text(t), findsOneWidget);
    }
    expect(find.text('Workspace'), findsOneWidget);

    await _section(tester, 'Shell & sandbox');
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

    await tester.tap(find.descendant(of: net, matching: find.byType(Switch)));
    await tester.pumpAndSettle();
    expect(client.calls, ['set sandbox.network false']);
    expect(
      find.descendant(of: net, matching: find.text('Restart to apply')),
      findsOneWidget,
    );
    expect(find.text('1 saved change applies when tern restarts.'), findsOne);

    await tester.tap(
      find.descendant(
        of: net,
        matching: find.byTooltip('Reset to default (true)'),
      ),
    );
    await tester.pumpAndSettle();
    expect(client.calls.last, 'reset sandbox.network');
    expect(find.text('Restart tern'), findsNothing);

    // A choice.
    await tester.tap(
      find.descendant(of: _row('sandbox.confine'), matching: find.text('off')),
    );
    await tester.pumpAndSettle();
    expect(client.calls.last, 'set sandbox.confine "off"');
    await tester.tap(find.text('Restart tern'));
    await tester.pumpAndSettle();
    expect(restarted, isTrue);
    expect(find.byType(ConfigDialog), findsNothing);

    expect(revealed, isEmpty);
  });

  testWidgets('numbers, lists and models are parsed; errors show inline', (
    tester,
  ) async {
    final client = _FakeClient();
    await _pump(tester, client);

    final iters = find.descendant(
      of: _row('max_iterations'),
      matching: find.byType(TextField),
    );
    await tester.enterText(iters, 'lots');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    expect(find.text('A whole number'), findsOneWidget);
    expect(client.calls, isEmpty);
    await tester.enterText(iters, '25');
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    expect(client.calls, ['set max_iterations 25']);
    expect(find.text('A whole number'), findsNothing);

    await _section(tester, 'Models');
    await tester.enterText(
      find.descendant(
        of: _row('default_model'),
        matching: find.byType(TextField),
      ),
      'anthropic:a, openrouter:b',
    );
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    expect(
      client.calls.last,
      'set default_model ["anthropic:a","openrouter:b"]',
    );

    await _section(tester, 'Sub-agents');
    await tester.enterText(
      find.descendant(
        of: _row('subagents.max_depth'),
        matching: find.byType(TextField),
      ),
      '9',
    );
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pumpAndSettle();
    expect(find.text('subagents.max_depth: must be at most 3'), findsOneWidget);
    // The fixture sets max_depth = 2, not the default.
    expect(
      find.descendant(
        of: _row('subagents.max_depth'),
        matching: find.text('Changed'),
      ),
      findsOneWidget,
    );
    expect(
      find.descendant(
        of: _row('subagents.models'),
        matching: find.text('Edit in config.toml'),
      ),
      findsOneWidget,
    );
  });

  testWidgets('search finds settings across sections', (tester) async {
    await _pump(tester, _FakeClient());
    await tester.enterText(find.byType(TextField).first, 'timeout');
    await tester.pumpAndSettle();
    expect(_row('sandbox.shell_timeout_secs'), findsOneWidget);
    expect(_row('subagents.ask_parent_timeout_secs'), findsOneWidget);
    expect(_row('secret_request_timeout_secs'), findsOneWidget);
    expect(_row('sandbox.network'), findsNothing);
    expect(find.text('Shell timeout (seconds)'), findsOneWidget);

    await tester.enterText(find.byType(TextField).first, 'zzzz');
    await tester.pumpAndSettle();
    expect(find.textContaining('No setting matches'), findsOneWidget);
  });

  testWidgets('a device without owner scope is told so', (tester) async {
    await _pump(tester, _FakeClient(failWith: 403));
    expect(find.textContaining('paired as owner'), findsOneWidget);
  });

  testWidgets('Open config.toml reveals the file', (tester) async {
    final revealed = <String>[];
    await _pump(tester, _FakeClient(), revealed: revealed);
    await tester.tap(find.text('Open config.toml'));
    expect(revealed, ['/Users/you/.tern/config.toml']);
  });

  for (final dark in [false, true]) {
    testWidgets('screenshot ${dark ? 'dark' : 'light'}', (tester) async {
      final client = _FakeClient();
      final key = GlobalKey();
      await _pump(tester, client, dark: dark, key: key, onRestart: () async {});
      await _section(tester, 'Shell & sandbox');
      await tester.tap(
        find.descendant(
          of: _row('sandbox.confine'),
          matching: find.text('required'),
        ),
      );
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      await tester.runAsync(() async {
        final boundary =
            key.currentContext!.findRenderObject()! as RenderRepaintBoundary;
        final image = await boundary.toImage(pixelRatio: 2);
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        final out = File(
          'build/screenshots/config_${dark ? 'dark' : 'light'}.png',
        );
        await out.create(recursive: true);
        await out.writeAsBytes(png!.buffer.asUint8List());
      });
    });
  }
}
