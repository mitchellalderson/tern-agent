// Home's stream and a topic channel with real fonts, to PNGs in
// build/screenshots/ (see screenshot_test.dart):
//   flutter test test/channels_screenshot_test.dart
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
import 'package:tern_desktop/ui/home.dart';

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

final _now = DateTime.now();

Channel _ch(String id, String slug, String title, {int? last}) => Channel(
  id: id,
  slug: slug,
  title: title,
  kind: id == 'home' ? 'main' : 'topic',
  mainSessionId: id == 'home' ? 'h2' : 'fin-main',
  lastPostId: last,
  instructions: id == 'fin' ? 'Amounts in EUR. Never move money.' : null,
  updatedAt: _now,
);

StreamMessage _m(String s, int id, String role, String text, {int ago = 0}) =>
    StreamMessage(
      sessionId: s,
      messageId: id,
      message: SessionMessage(
        role: role,
        runId: 'r-$s',
        content: [TextBlock(text)],
        summary: false,
      ),
      createdAt: _now.subtract(Duration(minutes: ago)),
    );

Post _post(
  int id,
  String text, {
  String authorKind = 'agent',
  String author = 'tern',
  String session = 'fin-main',
  int? replyTo,
  int replies = 0,
  int ago = 0,
}) => Post(
  id: id,
  channelId: 'fin',
  sessionId: session,
  authorKind: authorKind,
  authorId: author,
  author: author,
  kind: authorKind == 'user' ? 'message' : 'answer',
  text: text,
  replyTo: replyTo,
  replies: replies,
  createdAt: _now.subtract(Duration(minutes: ago)),
);

TernStore _store() {
  final store = TernStore(TernClient(const Endpoint.unix('/nonexistent')))
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0.2.0', features: {'channels'})
    ..info = const Info(defaultModel: 'anthropic:claude-sonnet-5-5', tools: [])
    ..channels = [
      _ch('home', 'home', 'Home'),
      _ch('fin', 'finances', 'Finances', last: 9),
      _ch('launch', 'launch', 'Launch', last: 4),
    ];
  store.lastSeen['fin'] = 9;
  final home = Transcript.fromStream([
    _m('h1', 1, 'user', 'Which invoices are still open?', ago: 900),
    _m(
      'h1',
      2,
      'assistant',
      'Two: **Acme** (\$1,240) and **Northwind** (\$310).',
      ago: 899,
    ),
    _m('h1', 3, 'user', "Let's hold the Northwind one.", ago: 880),
    _m('h1', 4, 'assistant', "Holding it. I'll remind you Friday.", ago: 879),
    StreamPost(
      Post(
        id: 3,
        channelId: 'home',
        sessionId: 'cron-1',
        authorKind: 'routine',
        authorId: 'routine:brief',
        author: 'Morning brief',
        kind: 'answer',
        text:
            'Two meetings today: design review at 10:00, Nora at 15:30. '
            'One invoice needs a decision.',
        createdAt: _now.subtract(const Duration(minutes: 140)),
      ),
    ),
    StreamCut(
      sessionId: 'h2',
      prevSessionId: 'h1',
      reason: 'day',
      carry: '[Continuing from your previous session]',
      createdAt: _now.subtract(const Duration(minutes: 30)),
    ),
    _m('h2', 10, 'user', 'And the other invoice?', ago: 29),
    _m(
      'h2',
      11,
      'assistant',
      "Acme's \$1,240 is still open. Northwind is on hold until Friday, as "
          'you asked yesterday.',
      ago: 28,
    ),
    StreamSideChat(
      sessionId: 'side',
      title: '> Two: Acme and Northwind\n\nWhy is Acme so high?',
      branchedFrom: 'message:2',
      createdAt: _now.subtract(const Duration(minutes: 20)),
    ),
    _m('h2', 12, 'user', 'Pay Acme from the business account.', ago: 2),
  ]);
  home.items.whereType<UserItem>().last.queued = true;
  home.items.add(
    ApprovalItem(
      const Approval(
        id: 'a1',
        runId: 'r9',
        sessionId: 'fin-main',
        tool: 'http_fetch',
        input: {'url': 'https://bank.example/transfer'},
        summary: 'http_fetch POST https://bank.example/transfer',
        effect: 'destructive',
        pattern: null,
      ),
    ),
  );
  store.transcripts[TernStore.homeKey] = home;
  store.posts['fin'] = [
    _post(
      5,
      'How much did we spend on cards this month?',
      authorKind: 'user',
      author: 'you',
      ago: 300,
    ),
    _post(
      6,
      'About **€1,240** across 38 charges; card fees are €61 of that.',
      replyTo: 5,
      replies: 1,
      ago: 298,
    ),
    _post(
      7,
      'Why so much?',
      authorKind: 'user',
      author: 'you',
      session: 'fin-side',
      replyTo: 6,
      ago: 200,
    ),
    _post(
      8,
      '2 invoices need a decision: Acme €1,240, Northwind €310.',
      authorKind: 'routine',
      author: 'reply-sweep',
      session: 'cron-2',
      ago: 60,
    ),
    _post(9, 'Card fees comparison done; Mercury is cheapest for us.', ago: 5),
  ];
  return store;
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
    testWidgets('home and a channel ${dark ? 'dark' : 'light'}', (
      tester,
    ) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final c = dark ? TernColors.dark : TernColors.light;
      final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
      final key = GlobalKey();
      final store = _store()
        ..channel = TernStore.homeId
        ..selected = 'h2'
        ..draft = false
        ..homeNext = 'more';
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
      await tester.pump(const Duration(milliseconds: 100));
      expect(tester.takeException(), isNull);
      await _shot(tester, key, 'home_${dark ? 'dark' : 'light'}');

      // #finances, with a side thread open beside it, and the approval its
      // run is waiting on answered in Home.
      store
        ..channel = 'fin'
        ..selected = 'fin-main'
        ..thread = 'fin-side'
        ..transcripts['fin-side'] = (Transcript()
          ..items.addAll([
            UserItem('Why so much?'),
            AssistantItem(
              'Mostly card fees on foreign transactions: 14 of the 38 '
              'charges were in USD.',
            ),
          ]));
      store.onEventForTest(
        TernEvent.fromJson({
          'type': 'run_started',
          'run_id': 'r9',
          'session_id': 'fin-main',
          'channel_id': 'fin',
        }),
      );
      store.notifyListeners();
      await tester.pump(const Duration(milliseconds: 100));
      expect(tester.takeException(), isNull);
      await _shot(tester, key, 'channel_${dark ? 'dark' : 'light'}');
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
