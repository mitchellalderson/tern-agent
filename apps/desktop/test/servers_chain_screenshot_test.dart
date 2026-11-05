// Saved servers and the fallback-list editor with real fonts, to PNGs in
// build/screenshots/ (see ap3_screenshot_test.dart), light and dark:
//   flutter test test/servers_chain_screenshot_test.dart
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
import 'package:tern_desktop/ui/agent_sheet.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/providers.dart';
import 'package:tern_desktop/ui/servers.dart';

import 'model_chain_test.dart' show FakeChainClient, chainStore;
import 'servers_test.dart' show Env, linkFor, settle;

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

Widget _shell(GlobalKey key, bool dark, Widget home) {
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

  for (final dark in [false, true]) {
    final mode = dark ? 'dark' : 'light';

    testWidgets('servers $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final env = Env();
      addTearDown(env.dispose);
      await tester.runAsync(() async {
        await env.connections.connect(
          linkFor('home.example.net', name: 'Home server'),
        );
        await env.connections.connect(
          linkFor('work.example.net', name: 'Work box'),
        );
        await env.connections.useLocal();
      });
      env.world.down.add('home.example.net');
      final store = TernStore(TernClient(const Endpoint.unix('/nonexistent')))
        ..link = Link.online
        ..health = const Health(pid: 1, version: '0.2.0')
        ..info = const Info(
          defaultModel: 'anthropic:claude-sonnet-5-5',
          tools: [],
        );
      final key = GlobalKey();
      await tester.pumpWidget(
        _shell(key, dark, Home(store: store, connections: env.connections)),
      );
      await tester.pump(const Duration(milliseconds: 100));
      await tester.pump(const Duration(milliseconds: 100));
      await _shot(tester, key, 'servers_footer_$mode');

      await tester.tap(find.byKey(const Key('server-switcher')));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'servers_menu_$mode');
      await tester.tapAt(const Offset(900, 100));
      await tester.pumpAndSettle();

      // The manage dialog, with a server that doesn't answer.
      showServersDialog(tester.element(find.byType(Home)), env.connections);
      await tester.pumpAndSettle();
      await _shot(tester, key, 'servers_manage_$mode');
      await tester.tap(find.text('Done'));
      await tester.pumpAndSettle();

      // Adding: a tested address.
      showDialog<bool>(
        context: tester.element(find.byType(Home)),
        builder: (_) => ServerEditDialog(connections: env.connections),
      );
      await tester.pumpAndSettle();
      await tester.enterText(find.byKey(const Key('server-name-field')), 'Lab');
      await tester.enterText(
        find.byType(TextField).at(1),
        'lab.example.net:7420',
      );
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('test-connection')));
      await settle(tester);
      await _shot(tester, key, 'servers_add_tested_$mode');

      env.world.down.add('lab.example.net');
      await tester.enterText(
        find.byType(TextField).at(1),
        'http://lab.example.net',
      );
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('test-connection')));
      await settle(tester);
      await _shot(tester, key, 'servers_add_http_$mode');
      debugDefaultTargetPlatformOverride = null;
    });

    testWidgets('fallback editor $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 900) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final client = FakeChainClient(
        chain: const [
          'anthropic:claude-sonnet-5-5',
          'openrouter:kimi-k3',
          'gemini:flash',
        ],
      );
      final store = await chainStore(client);
      final key = GlobalKey();
      await tester.pumpWidget(
        _shell(key, dark, Scaffold(body: ProvidersDialog(store: store))),
      );
      await tester.pumpAndSettle();
      await _shot(tester, key, 'chain_models_summary_$mode');

      await tester.tap(find.byKey(const Key('edit-default-fallbacks')));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'chain_models_editor_$mode');

      await tester.tap(find.byKey(const Key('add-fallback')));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'chain_models_picker_$mode');
      await tester.tap(find.text('gpt-5', findRichText: true));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'chain_models_added_$mode');
      debugDefaultTargetPlatformOverride = null;
    });

    testWidgets('fallback editor in the agent sheet $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 1100) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final client = FakeChainClient();
      final store = await chainStore(client);
      final key = GlobalKey();
      const agent = Agent(
        id: 'tern:research',
        slug: 'research',
        name: 'Research',
        avatar: AvatarSpec(glyph: '★', color: 'blue'),
        model: ['anthropic:claude-sonnet-5-5', 'openrouter:kimi-k3'],
        mainChannelId: 'dm:research',
      );
      await tester.pumpWidget(
        _shell(
          key,
          dark,
          Scaffold(
            body: AgentSheet(store: store, editing: agent),
          ),
        ),
      );
      await tester.pumpAndSettle();
      final toggle = find.byKey(const Key('toggle-fallbacks'));
      await tester.ensureVisible(toggle);
      await tester.tap(toggle);
      await tester.pumpAndSettle();
      await _shot(tester, key, 'chain_sheet_$mode');
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
