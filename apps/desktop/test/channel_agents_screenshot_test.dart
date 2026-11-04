// Agents in a topic channel with real fonts, to PNGs in build/screenshots/
// (see screenshot_test.dart): #launch with its members and lead, posts
// that mention agents, notes, a live member line and the `@` suggestions;
// then the channel settings with the lead and agent wakes.
//   flutter test test/channel_agents_screenshot_test.dart
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
import 'package:tern_desktop/ui/channel_view.dart';
import 'package:tern_desktop/ui/conversation.dart';
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

const _agents = [
  Agent(
    id: 'tern',
    slug: 'tern',
    name: 'tern',
    memory: 'shared',
    mainChannelId: 'home',
    isDefault: true,
  ),
  Agent(
    id: 'tern:research',
    slug: 'research',
    name: 'Research',
    avatar: AvatarSpec(glyph: '★', color: 'blue'),
    mainChannelId: 'dm:research',
  ),
  Agent(
    id: 'tern:ops',
    slug: 'ops',
    name: 'Ops',
    avatar: AvatarSpec(glyph: '■', color: 'orange'),
    mainChannelId: 'dm:ops',
  ),
  Agent(
    id: 'tern:marketing',
    slug: 'marketing',
    name: 'Marketing',
    avatar: AvatarSpec(glyph: '♥', color: 'pink'),
    mainChannelId: 'dm:marketing',
  ),
];

Post _post(
  int id,
  String text, {
  String authorKind = 'agent',
  String authorId = 'tern:research',
  String author = 'Research',
  String kind = 'answer',
  List<String> mentions = const [],
  int ago = 0,
}) => Post(
  id: id,
  channelId: 'launch',
  sessionId: authorKind == 'system' ? null : 'main',
  authorKind: authorKind,
  authorId: authorId,
  author: author,
  kind: kind,
  text: text,
  mentions: mentions,
  createdAt: _now.subtract(Duration(minutes: ago)),
);

TernStore _store() {
  final store = TernStore(TernClient(const Endpoint.unix('/nonexistent')))
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.2.0',
      features: {'channels', 'agents'},
    )
    ..info = const Info(defaultModel: 'anthropic:claude-sonnet-5-5', tools: [])
    ..agents = _agents
    ..channels = [
      Channel(
        id: 'home',
        slug: 'home',
        title: 'Home',
        kind: 'main',
        agentId: 'tern',
        mainSessionId: 'h1',
        updatedAt: _now,
      ),
      for (final a in _agents.skip(1))
        Channel(
          id: a.mainChannelId!,
          slug: 'dm-${a.slug}',
          title: a.name,
          kind: 'main',
          agentId: a.id,
          updatedAt: _now,
        ),
      Channel(
        id: 'launch',
        slug: 'launch',
        title: 'Launch',
        kind: 'topic',
        agentId: 'tern:research',
        mainSessionId: 'main',
        lastPostId: 9,
        instructions: 'Offsite in March, 40 people, budget EUR 12k.',
        agents: ChannelAgents(
          members: [
            ChannelMember(agentId: 'tern:research', lead: true, addedAt: _now),
            ChannelMember(agentId: 'tern', addedAt: _now),
            ChannelMember(agentId: 'tern:ops', addedAt: _now),
          ],
          agentWakes: true,
        ),
        updatedAt: _now,
      ),
    ];
  store.lastSeen['launch'] = 9;
  store.posts['launch'] = [
    _post(
      1,
      'Find a venue for the March offsite, 40 people.',
      authorKind: 'user',
      authorId: 'owner',
      author: 'you',
      kind: 'message',
      ago: 50,
    ),
    _post(
      2,
      'Three venues fit the budget: **Kloster Eberbach** (EUR 9.8k), '
      '**Gut Sonnenhof** (EUR 11.2k) and **Schloss Hohenstein** (EUR 12.4k, '
      'over by 400). @ops can you hold the first for 12–14 March?',
      mentions: ['tern:ops'],
      ago: 48,
    ),
    _post(
      3,
      'Ops joined',
      authorKind: 'system',
      authorId: 'system',
      author: 'tern',
      kind: 'note',
      ago: 47,
    ),
    _post(
      4,
      'Held until Friday; the deposit is EUR 1,500. @research it\'s yours to '
      'confirm.',
      authorId: 'tern:ops',
      author: 'Ops',
      mentions: ['tern:research'],
      ago: 45,
    ),
    _post(
      5,
      '@research not woken: agents have already woken each other 2 times in '
      'a row here, the most #launch allows',
      authorKind: 'system',
      authorId: 'system',
      author: 'tern',
      kind: 'note',
      ago: 45,
    ),
    _post(
      6,
      '@tern draft the invite email, and @marketing a teaser post.',
      authorKind: 'user',
      authorId: 'owner',
      author: 'you',
      kind: 'message',
      mentions: ['tern', 'tern:marketing'],
      ago: 3,
    ),
    _post(
      7,
      'Marketing joined',
      authorKind: 'system',
      authorId: 'system',
      author: 'tern',
      kind: 'note',
      ago: 3,
    ),
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
    final mode = dark ? 'dark' : 'light';
    testWidgets('agents in a channel $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final c = dark ? TernColors.dark : TernColors.light;
      final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
      final key = GlobalKey();
      final store = _store()
        ..channel = 'launch'
        ..selected = 'main';
      // Two members at work: tern, and Marketing's helper.
      for (final (run, agent) in [('r1', 'tern'), ('r2', 'tern:marketing')]) {
        store.onEventForTest(
          TernEvent.fromJson({
            'type': 'run_started',
            'run_id': run,
            'session_id': 's-$run',
            'channel_id': 'launch',
            'agent_id': agent,
          }),
        );
      }
      store.onEventForTest(
        TernEvent.fromJson({
          'type': 'tool_started',
          'run_id': 'r2',
          'session_id': 's-r2',
          'channel_id': 'launch',
          'id': 't1',
          'name': 'web_search',
          'summary': 'web_search offsite teaser ideas',
        }),
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
      await tester.pump(const Duration(milliseconds: 100));
      final field = find.descendant(
        of: find.byType(Composer),
        matching: find.byType(TextField),
      );
      await tester.enterText(field, 'Thanks. @');
      await tester.pump(const Duration(milliseconds: 100));
      expect(find.byType(MentionSuggestions), findsOneWidget);
      expect(find.byType(MemberAvatars), findsOneWidget);
      expect(tester.takeException(), isNull);
      await _shot(tester, key, 'channel_agents_$mode');

      await tester.enterText(field, '');
      showChannelSettings(
        tester.element(find.byType(ChannelView)),
        store,
        store.openChannel!,
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      await tester.pump(const Duration(milliseconds: 400));
      expect(find.byType(ChannelDialog), findsOneWidget);
      expect(tester.takeException(), isNull);
      await _shot(tester, key, 'channel_settings_$mode');
      Navigator.of(tester.element(find.byType(ChannelDialog))).pop();
      await tester.pump(const Duration(milliseconds: 400));
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
