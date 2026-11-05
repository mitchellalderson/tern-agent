// Cost of a long Home history: store load, first build, a live rebuild, and
// how many item widgets exist. `flutter test test/history_perf_test.dart`
// prints the numbers.
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/items.dart';

import 'support/history_fixture.dart';

void main() {
  for (final n in [5000, 20000]) {
    testWidgets('Home with $n messages', (tester) async {
      final client = PerfClient(longHistory(n));
      final store = TernStore(client)
        ..link = Link.online
        ..health = const Health(
          pid: 1,
          version: '0.2.0',
          features: {'channels'},
        )
        ..info = const Info(defaultModel: 'mock:m', tools: []);
      final rss0 = ProcessInfo.currentRss;
      final sw = Stopwatch()..start();
      await store.openHome();
      final load = sw.elapsedMilliseconds;
      // A user who keeps pressing "Load earlier" until everything is in.
      while (store.homeNext != null) {
        await store.loadOlderHome();
      }
      final loadAll = sw.elapsedMilliseconds;
      tester.view.physicalSize = const Size(1500, 1000);
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      sw
        ..reset()
        ..start();
      await tester.pumpWidget(
        MaterialApp(
          theme: ternTheme(TernColors.light, Brightness.light),
          home: Home(store: store),
        ),
      );
      await tester.pump();
      final first = sw.elapsedMilliseconds;
      final built = find.byType(ItemView).evaluate().length;
      // A live rebuild: what every streamed token costs.
      sw
        ..reset()
        ..start();
      final times = <double>[];
      for (var i = 0; i < 40; i++) {
        sw
          ..reset()
          ..start();
        // ignore: invalid_use_of_protected_member
        store.notifyListeners();
        await tester.pump();
        times.add(sw.elapsedMicroseconds / 1000);
      }
      times.sort();
      final perRebuild = times[times.length ~/ 2];
      // What every rebuild used to do first: group the whole history.
      final all = store.current!.items;
      sw
        ..reset()
        ..start();
      for (var i = 0; i < 10; i++) {
        groupApprovals(all);
      }
      final groupAll = sw.elapsedMicroseconds / 10 / 1000;
      sw
        ..reset()
        ..start();
      for (var i = 0; i < 10; i++) {
        groupApprovals(all.length > 100 ? all.sublist(all.length - 100) : all);
      }
      final groupWindow = sw.elapsedMicroseconds / 10 / 1000;
      // A resync (reconnect, new segment) re-reads what is loaded.
      sw
        ..reset()
        ..start();
      await store.openHome();
      await tester.pump();
      final resync = sw.elapsedMilliseconds;
      final rss1 = ProcessInfo.currentRss;
      // ignore: avoid_print
      print(
        'PERF n=$n items=${store.current?.items.length} '
        'openHome=${load}ms pageAll=${loadAll}ms firstBuild=${first}ms '
        'ItemViews=$built rebuild(median)=${perRebuild.toStringAsFixed(1)}ms '
        'groupAll=${groupAll.toStringAsFixed(2)}ms '
        'groupWindow=${groupWindow.toStringAsFixed(2)}ms '
        'resync=${resync}ms (limit ${client.lastLimit}) '
        'rss+${((rss1 - rss0) / 1048576).round()}MB',
      );
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
