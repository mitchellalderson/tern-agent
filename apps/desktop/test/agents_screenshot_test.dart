// Agent profiles with real fonts, to PNGs in build/screenshots/ (see
// screenshot_test.dart): the sidebar's AGENTS, Research's DM with an
// approval mirrored from Home, and the create sheet.
//   flutter test test/agents_screenshot_test.dart
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

Channel _ch(String id, String slug, String title, String kind, String? agent) =>
    Channel(
      id: id,
      slug: slug,
      title: title,
      kind: kind,
      agentId: agent,
      mainSessionId: '$id-main',
      updatedAt: _now,
    );

/// The sheet's calls: templates and a persona, nothing else.
class _Client extends TernClient {
  _Client() : super(const Endpoint.unix('/nonexistent'));

  @override
  Future<List<AgentTemplate>> agentTemplates() async => const [
    AgentTemplate(
      id: 'research',
      name: 'Research',
      description: 'Looks things up on the web and in your files.',
      persona:
          'You are Research, the user\'s research agent.\n\n- Your job: find '
          'things out. Search the web and the user\'s files, read the '
          'sources, and report what you found.\n- Lead with the answer, then '
          'the evidence, with a link or path for every claim.',
      tools: ToolScope(
        builtins: Pick.readOnly(),
        connectors: Pick.names([]),
        mcp: Pick.names([]),
      ),
      avatar: AvatarSpec(glyph: '★', color: 'blue'),
    ),
    AgentTemplate(
      id: 'inbox',
      name: 'Inbox',
      description: 'Sweeps your mail every morning.',
      persona: 'You are Inbox.',
      routine: (cron: '0 8 * * 1-5', prompt: 'Check my inbox.'),
    ),
    AgentTemplate(
      id: 'marketing',
      name: 'Marketing',
      description: 'Drafts posts in your voice.',
      persona: 'You are Marketing.',
    ),
    AgentTemplate(
      id: 'blank',
      name: '',
      description: 'Start blank.',
      persona: 'You are {name}.',
    ),
  ];

  @override
  Future<List<Memory>> memories({String? agent, String? query}) async => [
    Memory(
      id: 1,
      content: 'Acme moved its billing to Stripe in October.',
      createdAt: _now,
      agentId: agent,
    ),
    Memory(
      id: 2,
      content: 'Prefers primary sources over press coverage.',
      createdAt: _now,
      agentId: agent,
    ),
  ];
}

TernStore _store() {
  const research = Agent(
    id: 'tern:research',
    slug: 'research',
    name: 'Research',
    avatar: AvatarSpec(glyph: '★', color: 'blue'),
    tools: ToolScope(
      builtins: Pick.readOnly(),
      connectors: Pick.names([]),
      mcp: Pick.names([]),
    ),
    mainChannelId: 'dm:research',
  );
  final store = TernStore(_Client())
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.2.0',
      features: {'channels', 'agents'},
    )
    ..info = const Info(
      defaultModel: 'anthropic:claude-sonnet-5-5',
      tools: [
        (name: 'read_file', description: 'Read a file.'),
        (name: 'list_dir', description: 'List a directory.'),
        (name: 'http_fetch', description: 'Fetch a URL.'),
        (name: 'write_file', description: 'Write a file.'),
        (name: 'shell', description: 'Run a command.'),
      ],
    )
    ..agents = const [
      Agent(
        id: 'tern',
        slug: 'tern',
        name: 'tern',
        memory: 'shared',
        mainChannelId: 'home',
        isDefault: true,
      ),
      Agent(
        id: 'tern:inbox',
        slug: 'inbox',
        name: 'Inbox',
        avatar: AvatarSpec(glyph: '■', color: 'amber'),
        mainChannelId: 'dm:inbox',
      ),
      Agent(
        id: 'tern:marketing',
        slug: 'marketing',
        name: 'Marketing',
        avatar: AvatarSpec(glyph: '♥', color: 'pink'),
        mainChannelId: 'dm:marketing',
      ),
      research,
    ]
    ..channels = [
      _ch('home', 'home', 'Home', 'main', 'tern'),
      _ch('dm:research', 'dm-research', 'Research', 'main', 'tern:research'),
      _ch('dm:inbox', 'dm-inbox', 'Inbox', 'main', 'tern:inbox'),
      _ch(
        'dm:marketing',
        'dm-marketing',
        'Marketing',
        'main',
        'tern:marketing',
      ),
      _ch('fin', 'finances', 'Finances', 'topic', null),
    ]
    ..schedules = [
      Schedule(
        id: 's1',
        cron: '0 9 * * 1',
        prompt: 'Weekly digest of what changed at Acme and its competitors',
        sessionId: 'cron-1',
        enabled: true,
        agentId: 'tern:research',
        channelId: 'dm:research',
      ),
    ];
  store
    ..channel = 'dm:research'
    ..streamChannel = 'dm:research'
    ..selected = 'dm:research-main'
    ..draft = false;
  final dm = Transcript.fromStream([
    _m(
      'dm:research-main',
      1,
      'user',
      'What changed at Acme this month?',
      ago: 40,
    ),
    _m(
      'dm:research-main',
      2,
      'assistant',
      'Three things, from their changelog and two filings:\n\n'
          '1. **Billing moved to Stripe** (Oct 3).\n'
          '2. **A new EU data region** in Frankfurt (Oct 17).\n'
          '3. **Price change**: Team plan up from \$12 to \$15 a seat.',
      ago: 39,
    ),
    _m('dm:research-main', 3, 'user', 'Save that to my notes.', ago: 3),
  ]);
  store.transcripts[TernStore.streamKeyOf('dm:research')] = dm;
  final approval = ApprovalItem(
    const Approval(
      id: 'a1',
      runId: 'r9',
      sessionId: 'dm:research-main',
      tool: 'write_file',
      input: {'path': 'notes/acme.md'},
      summary: 'write_file notes/acme.md',
      pattern: null,
      channelId: 'dm:research',
      agentId: 'tern:research',
    ),
  );
  store.transcripts[TernStore.homeKey] = Transcript()..items.add(approval);
  dm.items.add(approval);
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
    testWidgets('an agent DM and the sheet ${dark ? 'dark' : 'light'}', (
      tester,
    ) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final c = dark ? TernColors.dark : TernColors.light;
      final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
      final key = GlobalKey();
      final store = _store();
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
      await tester.pump(const Duration(milliseconds: 100));
      expect(tester.takeException(), isNull);
      expect(find.text('Message Research'), findsOneWidget);
      await _shot(tester, key, 'agent_dm_${dark ? 'dark' : 'light'}');

      await tester.tap(find.byTooltip('New agent'));
      await tester.pumpAndSettle();
      await tester.tap(
        find.descendant(
          of: find.byType(Dialog),
          matching: find.widgetWithText(ChoiceChip, 'Research'),
        ),
      );
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      await _shot(tester, key, 'agent_sheet_${dark ? 'dark' : 'light'}');
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
