// T6b in the desktop app, with real fonts, to PNGs in build/screenshots/
// (see screenshot_test.dart): Library → Tasks, Library → Activity ledger
// (intact and tampered), and Settings' security audit.
//   flutter test test/t6b_screenshot_test.dart
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
import 'package:tern_desktop/ui/security.dart';

import 'ledger_test.dart' show sampleLedger;
import 'routines_visible_test.dart' show FakeRoutinesClient, storeWith;
import 'security_test.dart' show sampleAudit;
import 'tasks_test.dart' show sampleTasks;

class _Client extends FakeRoutinesClient {
  var intact = true;

  @override
  Future<Tasks> tasks() async => sampleTasks;

  @override
  Future<List<LedgerEntry>> ledger({
    String? kind,
    String? channel,
    String? routine,
    String? connector,
    String? run,
    int? before,
    int? limit,
  }) async => sampleLedger;

  @override
  Future<LedgerCheck> verifyLedger() async => LedgerCheck(
    ok: intact,
    entries: 6,
    head: '9f2c' * 16,
    brokenAt: intact ? null : 4,
    reason: intact ? null : "its contents don't match its hash",
  );

  @override
  Future<SecurityAudit> securityAudit() async => sampleAudit;
}

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
    final flutterRoot = Platform.environment['FLUTTER_ROOT'];
    if (flutterRoot != null) {
      await _font('MaterialIcons', [
        '$flutterRoot/bin/cache/artifacts/material_fonts/MaterialIcons-Regular.otf',
      ]);
    }
  });

  for (final dark in [false, true]) {
    final mode = dark ? 'dark' : 'light';
    testWidgets('tasks, ledger and audit $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final c = dark ? TernColors.dark : TernColors.light;
      final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
      final key = GlobalKey();
      final client = _Client();
      final store = storeWith(client)
        ..health = const Health(
          pid: 1,
          version: '0.2.0',
          features: {
            'channels',
            'agents',
            'schedules',
            'tasks',
            'ledger',
            'security_audit',
          },
        );
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
            home: Home(store: store),
          ),
        ),
      );
      Future<void> settle() async {
        for (var i = 0; i < 5; i++) {
          await tester.pump(const Duration(milliseconds: 50));
        }
        expect(tester.takeException(), isNull);
      }

      store.openTasks();
      await settle();
      expect(find.text('Needs you'), findsOneWidget);
      await _shot(tester, key, 't6b_tasks_$mode');

      store.openLedger();
      await settle();
      expect(find.text('Intact · 6 entries'), findsOneWidget);
      await _shot(tester, key, 't6b_ledger_$mode');

      client.intact = false;
      store.openTasks();
      await settle();
      store.openLedger();
      await settle();
      expect(find.text('Changed outside tern at entry #4'), findsOneWidget);
      await _shot(tester, key, 't6b_ledger_tampered_$mode');

      showSecurityAudit(tester.element(find.byType(Home)), store);
      await settle();
      expect(find.text('Security audit'), findsOneWidget);
      await _shot(tester, key, 't6b_audit_$mode');
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
