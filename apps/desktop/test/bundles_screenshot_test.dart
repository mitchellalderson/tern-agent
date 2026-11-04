// Library → Bundles and the install dialog for an update that adds a
// lookalike host, with real fonts, to PNGs in build/screenshots/ (see
// screenshot_test.dart).
//   flutter test test/bundles_screenshot_test.dart
import 'dart:async';
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
import 'package:tern_desktop/ui/bundles.dart';

import 'bundles_test.dart' show FakeBundlesClient;

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
    testWidgets('bundles $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 900) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final c = dark ? TernColors.dark : TernColors.light;
      final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
      final key = GlobalKey();
      final store = TernStore(
        FakeBundlesClient(),
      )..health = const Health(pid: 1, version: '0.1.0', features: {'bundles'});
      await tester.runAsync(store.loadBundles);
      await tester.pumpWidget(
        RepaintBoundary(
          key: key,
          child: MaterialApp(
            debugShowCheckedModeBanner: false,
            theme: theme.copyWith(
              textTheme: theme.textTheme.apply(
                fontFamily: '.AppleSystemUIFont',
              ),
            ),
            home: Scaffold(
              body: BundlesView(
                store: store,
                panelOpen: false,
                onTogglePanel: () {},
              ),
            ),
          ),
        ),
      );
      await tester.pump(const Duration(milliseconds: 100));
      await _shot(tester, key, 'bundles_page_$mode');

      final context = tester.element(find.byType(BundlesView));
      unawaited(
        showInstallBundle(
          context,
          store,
          source: 'https://example.com/notion-recipe.tar.zst',
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(tester.takeException(), isNull);
      await _shot(tester, key, 'bundles_install_$mode');
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
