// Long histories stay cheap: only a window is built, older history opens on
// "Show earlier" without moving what is on screen, new arrivals wait behind
// a pill while the reader is scrolled up, and long messages and runs of
// tool calls fold.
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/collapse.dart';
import 'package:tern_desktop/ui/feed.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/items.dart';

import 'support/history_fixture.dart';

StreamMessage _m(int id, String role, List<ContentBlock> content) =>
    StreamMessage(
      sessionId: 's1',
      messageId: id,
      message: SessionMessage(
        role: role,
        runId: 'r1',
        content: content,
        summary: false,
      ),
      createdAt: historyT0.add(Duration(seconds: id)),
    );

Future<TernStore> _open(
  WidgetTester tester,
  List<StreamItem> newestFirst,
) async {
  final store = TernStore(PerfClient(newestFirst))
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0.2.0', features: {'channels'})
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  await store.openHome();
  tester.view.physicalSize = const Size(1500, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Home(store: store),
    ),
  );
  await tester.pump();
  return store;
}

ScrollPosition _pos(WidgetTester tester) => tester
    .state<ScrollableState>(
      find
          .descendant(
            of: find.byType(FeedList),
            matching: find.byType(Scrollable),
          )
          .first,
    )
    .position;

/// Scroll to the oldest built entry, until [finder] shows.
Future<void> _scrollTo(WidgetTester tester, Finder finder) async {
  for (var i = 0; i < 40 && finder.evaluate().isEmpty; i++) {
    final p = _pos(tester);
    p.jumpTo(p.maxScrollExtent);
    await tester.pump();
  }
}

/// A widget test on macOS, where the window chrome is its own.
void _test(String name, Future<void> Function(WidgetTester tester) body) =>
    testWidgets(name, (tester) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      try {
        await body(tester);
      } finally {
        debugDefaultTargetPlatformOverride = null;
      }
    });

void main() {
  _test('a long history builds a window, and Show earlier widens it', (
    tester,
  ) async {
    final store = await _open(tester, longHistory(5000));
    // The first page of 200 messages is 150 items; the window is 100.
    expect(store.current!.items.length, 150);
    expect(find.byType(ItemView).evaluate().length, lessThan(20));

    final more = find.textContaining('Show earlier (50+ more)');
    await _scrollTo(tester, more);
    expect(more, findsOneWidget);

    // Widening keeps what is on screen where it is.
    final before = _pos(tester).pixels;
    await tester.tap(more);
    await tester.pump();
    expect(_pos(tester).pixels, before);
    expect(find.text('Show earlier'), findsNothing);
    await _scrollTo(tester, find.text('Show earlier'));

    // The window now covers what is loaded: the next press pages the daemon.
    await tester.tap(find.text('Show earlier'));
    await tester.pump();
    await tester.pump();
    expect(store.current!.items.length, 300);
    expect(store.homeNext, isNotNull);
    expect(find.byType(ItemView).evaluate().length, lessThan(30));
  });

  _test('Show earlier is a labelled button', (tester) async {
    final handle = tester.ensureSemantics();
    await _open(tester, longHistory(5000));
    final more = find.bySemanticsLabel('Show earlier messages, 50 more');
    await _scrollTo(tester, more);
    expect(more, findsOneWidget);
    expect(tester.getSemantics(more).flagsCollection.isButton, isTrue);
    handle.dispose();
  });

  _test('scrolled up, new messages wait behind a pill', (tester) async {
    final store = await _open(tester, longHistory(400));
    final p = _pos(tester);
    p.jumpTo(600);
    await tester.pump();
    expect(find.text('Jump to latest'), findsOneWidget);

    store.current!.items.add(AssistantItem('A late arrival'));
    // ignore: invalid_use_of_protected_member
    store.notifyListeners();
    await tester.pump();
    expect(_pos(tester).pixels, 600, reason: 'what they read does not move');
    expect(find.text('New messages'), findsOneWidget);
    expect(find.textContaining('A late arrival'), findsNothing);

    await tester.tap(find.text('New messages'));
    await tester.pumpAndSettle();
    expect(find.textContaining('A late arrival'), findsOneWidget);
    expect(find.text('New messages'), findsNothing);
    expect(_pos(tester).pixels, 0);
  });

  _test('at the bottom, a new message just appears', (tester) async {
    final store = await _open(tester, longHistory(40));
    store.current!.items.add(AssistantItem('Right here'));
    // ignore: invalid_use_of_protected_member
    store.notifyListeners();
    await tester.pump();
    expect(find.textContaining('Right here'), findsOneWidget);
    expect(find.text('New messages'), findsNothing);
  });

  _test('a long message folds to a few lines and opens', (tester) async {
    final text = List.generate(
      60,
      (i) => 'Line $i of a long answer',
    ).join('\n\n');
    final store = await _open(tester, [
      _m(2, 'assistant', [TextBlock(text)]),
      _m(1, 'user', [TextBlock('Tell me everything')]),
    ]);
    expect(store.current!.items.length, 2);
    expect(find.textContaining('Line 0 of'), findsOneWidget);
    expect(find.textContaining('Line 59 of'), findsNothing);
    final show = find.textContaining('Show more (');
    expect(show, findsOneWidget);

    await tester.tap(show);
    await tester.pump();
    expect(find.text('Show less'), findsOneWidget);
    expect(find.textContaining('Show more'), findsNothing);

    await tester.tap(find.text('Show less'));
    await tester.pump();
    expect(find.textContaining('Line 59 of'), findsNothing);
  });

  _test('Show more keeps the text where it is', (tester) async {
    final long = List.generate(
      60,
      (i) => 'Line $i of a long answer',
    ).join('\n\n');
    await _open(tester, [
      for (var i = 20; i > 2; i--) _m(i + 10, 'user', [TextBlock('Newer $i')]),
      _m(12, 'assistant', [TextBlock(long)]),
      for (var i = 10; i > 0; i--) _m(i, 'user', [TextBlock('Older $i')]),
    ]);
    final p = _pos(tester);
    // Scroll until the long message's start is on screen.
    final head = find.textContaining('Line 0 of');
    for (var o = 0.0; head.evaluate().isEmpty && o < p.maxScrollExtent;) {
      o += 200;
      p.jumpTo(o.clamp(0, p.maxScrollExtent));
      await tester.pump();
    }
    expect(head, findsOneWidget);
    final top = tester.getTopLeft(head).dy;

    await tester.tap(find.textContaining('Show more ('));
    await tester.pump();
    await tester.pump();
    expect(tester.getTopLeft(head).dy, closeTo(top, 0.5));
    expect(find.text('Show less'), findsOneWidget);

    await tester.tap(find.text('Show less'));
    await tester.pump();
    await tester.pump();
    expect(tester.getTopLeft(head).dy, closeTo(top, 0.5));
  });

  _test('a run of tool calls is one line until opened', (tester) async {
    final calls = [
      for (var i = 0; i < 10; i++)
        ToolUseBlock(
          id: 'c$i',
          name: i < 8 ? 'read_file' : 'list_dir',
          input: {'path': 'f$i'},
        ),
    ];
    final results = [
      for (var i = 0; i < 10; i++)
        ToolResultBlock(toolUseId: 'c$i', content: 'ok $i', isError: false),
    ];
    final store = await _open(tester, [
      _m(4, 'assistant', [const TextBlock('All done.')]),
      _m(3, 'user', results),
      _m(2, 'assistant', calls),
      _m(1, 'user', [const TextBlock('Read them all')]),
    ]);
    expect(store.current!.items.whereType<WorkItem>(), hasLength(1));
    expect(find.text('read_file ×8, list_dir ×2'), findsOneWidget);
    expect(find.text('Show all 10'), findsOneWidget);
    expect(find.text('→'), findsNothing, reason: 'no step rows');

    await tester.tap(find.text('Show all 10'));
    await tester.pump();
    expect(find.text('Hide'), findsOneWidget);
    expect(find.text('→'), findsNWidgets(10));
  });

  group('collapsedPreview', () {
    test('short text is shown whole', () {
      expect(collapsedPreview('a\nb\nc'), isNull);
      expect(collapsedPreview('x' * collapseChars), isNull);
    });

    test('long text keeps its first lines and closes an open fence', () {
      final text = [
        'intro',
        '```',
        for (var i = 0; i < 40; i++) 'code $i',
        '```',
      ].join('\n');
      final p = collapsedPreview(text)!;
      expect(p.text, startsWith('intro\n```\ncode 0'));
      expect(p.text, endsWith('\n```'));
      expect(p.hiddenLines, greaterThan(20));
    });

    test('one enormous line is cut', () {
      final p = collapsedPreview('y' * 50000)!;
      expect(p.text.length, lessThan(collapseChars));
    });
  });

  group('FeedWindow', () {
    test('slices the newest entries, ending where it was held', () {
      final w = FeedWindow(initial: 3);
      final all = List.generate(10, (i) => i);
      expect(w.slice(all), [7, 8, 9]);
      expect((w.hiddenEarlier(10), w.hiddenNewer(10)), (7, 0));
      w.hold(10);
      expect(w.slice([...all, 10, 11]), [7, 8, 9]);
      expect(w.hiddenNewer(12), 2);
      w.showEarlier(2);
      expect(w.slice(all), [5, 6, 7, 8, 9]);
      w
        ..release()
        ..reset();
      expect(w.slice(all), [7, 8, 9]);
    });
  });
}
