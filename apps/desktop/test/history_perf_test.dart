// Cost of a long Home history: store load, first build, a live rebuild, and
// how many item widgets exist. `flutter test test/history_perf_test.dart`
// prints the numbers (docs/history-windowing.md has before and after).
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/items.dart';

final _t0 = DateTime(2026, 11, 3, 9);

/// [n] messages, newest first: per turn a user line, a long assistant
/// answer with a code block, a tool call and its result.
List<StreamItem> longHistory(int n) {
  StreamMessage msg(int id, String role, List<ContentBlock> content) =>
      StreamMessage(
        sessionId: 's1',
        messageId: id,
        message: SessionMessage(
          role: role,
          runId: 'r1',
          content: content,
          summary: false,
        ),
        createdAt: _t0.add(Duration(seconds: id)),
      );
  final out = <StreamItem>[];
  for (var id = 1; id <= n; id++) {
    out.add(switch (id % 4) {
      1 => msg(id, 'user', [TextBlock('Question number $id about invoices')]),
      2 => msg(id, 'assistant', [
        TextBlock(
          'Answer $id.\n\n${'The invoice total is **310 EUR**. ' * 20}\n\n'
          '```\nline one\nline two\n```\n\n- first\n- second',
        ),
      ]),
      3 => msg(id, 'assistant', [
        ToolUseBlock(id: 'c$id', name: 'list_dir', input: {'path': '.'}),
      ]),
      _ => msg(id, 'user', [
        ToolResultBlock(
          toolUseId: 'c${id - 1}',
          content: List.generate(80, (i) => 'file_$i.txt').join('\n'),
          isError: false,
        ),
      ]),
    });
  }
  return out.reversed.toList();
}

class PerfClient extends TernClient {
  PerfClient(this.items) : super(const Endpoint.unix('/nonexistent'));

  /// Newest first, as the daemon sends them.
  final List<StreamItem> items;
  var streamCalls = 0;
  var lastLimit = 0;

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      mainSessionId: 's1',
      updatedAt: _t0,
    ),
  ];

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async {
    streamCalls++;
    lastLimit = limit;
    final start = before == null ? 0 : int.parse(before);
    final page = items.skip(start).take(limit).toList();
    final end = start + page.length;
    return StreamPage(items: page, next: end < items.length ? '$end' : null);
  }

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [];
}

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
      for (var i = 0; i < 20; i++) {
        // ignore: invalid_use_of_protected_member
        store.notifyListeners();
        await tester.pump();
      }
      final perRebuild = sw.elapsedMicroseconds / 20 / 1000;
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
        'ItemViews=$built rebuild=${perRebuild.toStringAsFixed(1)}ms '
        'resync=${resync}ms (limit ${client.lastLimit}) '
        'rss+${((rss1 - rss0) / 1048576).round()}MB',
      );
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
