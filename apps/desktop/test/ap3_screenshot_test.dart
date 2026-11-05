// AP3 screens with real fonts, to PNGs in build/screenshots/ (see
// screenshot_test.dart): the agent sheet's autonomy ceiling and bot field,
// the channel dialog's "Let agents decide", and the bundle plan's agents.
//   flutter test test/ap3_screenshot_test.dart
import 'dart:convert';
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
import 'package:tern_desktop/ui/agent_sheet.dart';
import 'package:tern_desktop/ui/bundles.dart';
import 'package:tern_desktop/ui/channel_view.dart';
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

const _research = Agent(
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
  autonomy: 'supervised',
);
const _ops = Agent(
  id: 'tern:ops',
  slug: 'ops',
  name: 'Ops',
  avatar: AvatarSpec(glyph: '■', color: 'orange'),
  mainChannelId: 'dm:ops',
  telegramSecret: 'OPS_BOT',
);
const _tern = Agent(
  id: 'tern',
  slug: 'tern',
  name: 'tern',
  memory: 'shared',
  mainChannelId: 'home',
  isDefault: true,
);

/// 25 starters in categories, like the core's catalog.
final _starters = <AgentTemplate>[
  const AgentTemplate(
    id: 'blank',
    name: '',
    category: 'General',
    description: 'Start blank.',
    persona: 'You are {name}.',
  ),
  for (final (id, name, category, description, ceiling) in const [
    (
      'research',
      'Research',
      'Research',
      'Looks things up on the web and in your files.',
      'readonly',
    ),
    (
      'analyst',
      'Analyst',
      'Research',
      'Reads reports and spreadsheets, explains what changed.',
      'readonly',
    ),
    (
      'librarian',
      'Librarian',
      'Research',
      'Files, tags and finds your notes and PDFs.',
      'supervised',
    ),
    (
      'reviewer',
      'Code reviewer',
      'Coding',
      'Reads a diff and reports real bugs, ranked.',
      'readonly',
    ),
    (
      'coder',
      'Coder',
      'Coding',
      'Writes and fixes code in your repo, with tests.',
      'supervised',
    ),
    (
      'devops',
      'DevOps',
      'Coding',
      'Watches deploys and logs, proposes the fix.',
      'supervised',
    ),
    (
      'docs',
      'Docs writer',
      'Coding',
      'Keeps READMEs and changelogs current.',
      'supervised',
    ),
    (
      'inbox',
      'Inbox',
      'Everyday',
      'Sweeps your mail every morning.',
      'supervised',
    ),
    (
      'calendar',
      'Scheduler',
      'Everyday',
      'Finds times and drafts the invite.',
      'supervised',
    ),
    (
      'travel',
      'Travel',
      'Everyday',
      'Plans trips and watches prices.',
      'readonly',
    ),
    (
      'budget',
      'Bookkeeper',
      'Everyday',
      'Categorizes spending and flags odd charges.',
      'readonly',
    ),
    (
      'marketing',
      'Marketing',
      'Business',
      'Drafts posts in your voice.',
      'supervised',
    ),
    (
      'sales',
      'Sales prep',
      'Business',
      'Briefs you on a company before the call.',
      'readonly',
    ),
    (
      'support',
      'Support',
      'Business',
      'Triages tickets and drafts replies.',
      'supervised',
    ),
    (
      'recruiter',
      'Recruiter',
      'Business',
      'Screens applications against your bar.',
      'readonly',
    ),
    (
      'ops',
      'Ops',
      'Business',
      'Runs your checklists and chases what is late.',
      'autonomous',
    ),
    (
      'writer',
      'Writer',
      'Writing',
      'Drafts and edits long-form in your voice.',
      'supervised',
    ),
    (
      'editor',
      'Editor',
      'Writing',
      'Tightens prose and checks facts.',
      'readonly',
    ),
    (
      'translator',
      'Translator',
      'Writing',
      'Translates and keeps terminology consistent.',
      'readonly',
    ),
    (
      'news',
      'News brief',
      'Monitoring',
      'A morning digest of what you follow.',
      'readonly',
    ),
    (
      'price',
      'Price watch',
      'Monitoring',
      'Tells you when a price or listing changes.',
      'readonly',
    ),
    (
      'uptime',
      'Uptime',
      'Monitoring',
      'Checks your sites and pages you if one is down.',
      'readonly',
    ),
    (
      'tutor',
      'Tutor',
      'Learning',
      'Teaches a topic with questions, not lectures.',
      'readonly',
    ),
    (
      'coach',
      'Coach',
      'Learning',
      'Keeps you to the plan you set.',
      'readonly',
    ),
    (
      'home',
      'Household',
      'Everyday',
      'Keeps lists, chores and reminders.',
      'supervised',
    ),
  ])
    AgentTemplate(
      id: id,
      name: name,
      category: category,
      description: description,
      persona:
          'You are $name, the user\'s $name agent.\n\n- Your job: '
          '$description\n- Lead with the answer, then the evidence.',
      autonomy: ceiling,
      avatar: const AvatarSpec(glyph: '★', color: 'blue'),
    ),
];

class _Client extends TernClient {
  _Client() : super(const Endpoint.unix('/nonexistent'));

  @override
  Future<List<SecretStatus>> secrets() async => const [
    SecretStatus(
      name: 'ANTHROPIC_API_KEY',
      inFile: true,
      inEnv: false,
      usedBy: ['provider anthropic'],
    ),
    SecretStatus(
      name: 'MAIN_BOT',
      inFile: true,
      inEnv: false,
      usedBy: ['telegram'],
    ),
    SecretStatus(name: 'OPS_BOT', inFile: true, inEnv: false, usedBy: []),
    SecretStatus(name: 'RESEARCH_BOT', inFile: true, inEnv: false, usedBy: []),
  ];

  @override
  Future<List<AgentTemplate>> agentTemplates() async => _starters;

  @override
  Future<Persona> persona(String id) async => const Persona(
    persona: "You are Research, the user's research agent.",
    systemSections: '## Untrusted content',
  );

  @override
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async => const [];

  @override
  Future<BundlePreview> previewBundle(String source, {String? sha256}) async =>
      BundlePreview.fromJson(
        jsonDecode(
          File('packages/tern_client/test/fixtures/bundle_preview_agents.json')
              .readAsStringSync(),
        ) as Json,
      );
}

Channel _launch() => Channel(
  id: 'launch',
  slug: 'launch',
  title: 'Launch',
  kind: 'topic',
  instructions: 'Amounts in EUR.',
  mainSessionId: 'launch-main',
  agentId: 'tern:research',
  agents: ChannelAgents(
    members: [
      ChannelMember(agentId: 'tern:research', lead: true, addedAt: _now),
      ChannelMember(agentId: 'tern:ops', addedAt: _now),
    ],
    letAgentsDecide: true,
  ),
  updatedAt: _now,
);

TernStore _store() {
  final store = TernStore(_Client())
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.2.0',
      features: {'channels', 'agents', 'providers', 'bundles'},
    )
    ..info = const Info(
      defaultModel: 'anthropic:claude-sonnet-5-5',
      tools: [
        (name: 'read_file', description: 'Read a file.'),
        (name: 'shell', description: 'Run a command.'),
      ],
    )
    ..agents = const [_tern, _research, _ops]
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
      Channel(
        id: 'dm:research',
        slug: 'dm-research',
        title: 'Research',
        kind: 'main',
        agentId: 'tern:research',
        mainSessionId: 'dm:research-main',
        updatedAt: _now,
      ),
      _launch(),
    ];
  store
    ..channel = 'dm:research'
    ..streamChannel = 'dm:research'
    ..selected = 'dm:research-main'
    ..draft = false;
  store.transcripts[TernStore.streamKeyOf(
    'dm:research',
  )] = Transcript.fromStream([
    StreamMessage(
      sessionId: 'dm:research-main',
      messageId: 1,
      message: SessionMessage(
        role: 'user',
        runId: 'r1',
        content: [TextBlock('Delete the old exports.')],
        summary: false,
      ),
      createdAt: _now,
    ),
    StreamMessage(
      sessionId: 'dm:research-main',
      messageId: 2,
      message: SessionMessage(
        role: 'assistant',
        runId: 'r1',
        content: [
          TextBlock(
            'That needs approval, and I am capped at supervised, so I '
            'will ask first.',
          ),
        ],
        summary: false,
      ),
      createdAt: _now,
    ),
  ]);
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
    testWidgets('AP3 screens $mode', (tester) async {
      tester.view.physicalSize = const Size(1280, 900) * 2;
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
      expect(find.byKey(const Key('autonomy-chip')), findsOneWidget);
      await _shot(tester, key, 'ap3_header_$mode');

      // The sheet: the ceiling, then the bot.
      await tester.tap(find.byTooltip('Edit Research'));
      await tester.pumpAndSettle();
      final scroll = find
          .descendant(
            of: find.descendant(
              of: find.byType(AgentSheet),
              matching: find.byType(SingleChildScrollView),
            ),
            matching: find.byType(Scrollable),
          )
          .first;
      await tester.tap(find.text('Advanced'));
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(
        find.byKey(const Key('autonomy-explainer')),
        200,
        scrollable: scroll,
      );
      await tester.pumpAndSettle();
      await _shot(tester, key, 'ap3_autonomy_$mode');
      await tester.scrollUntilVisible(
        find.byKey(const Key('agent-bot-restart-note')),
        200,
        scrollable: scroll,
      );
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('agent-bot')));
      await tester.pumpAndSettle();
      await _shot(tester, key, 'ap3_bot_$mode');
      await tester.tap(find.text('RESEARCH_BOT').last);
      await tester.pumpAndSettle();
      await _shot(tester, key, 'ap3_bot_chosen_$mode');
      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();

      // The channel dialog.
      await tester.pump();
      store
        ..channel = 'launch'
        ..streamChannel = 'launch'
        ..selected = null
        ..draft = false;
      store.notifyListeners();
      await tester.pumpAndSettle();
      showChannelSettings(
        tester.element(find.byType(Home)),
        store,
        store.channels.last,
      );
      await tester.pumpAndSettle();
      await _shot(tester, key, 'ap3_channel_$mode');
      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();

      // The bundle plan.
      showInstallBundle(
        tester.element(find.byType(Home)),
        store,
        source: 'https://example.com/research-pack.tar.zst',
      );
      await tester.pumpAndSettle();
      await _shot(tester, key, 'ap3_bundle_$mode');
      expect(tester.takeException(), isNull);
      debugDefaultTargetPlatformOverride = null;
    });
  }

  // The create sheet at a roomy and a small window: blank, the starter
  // select open, typing to filter, a starter picked, Advanced opened.
  for (final dark in [false, true]) {
    for (final (w, h) in [(1280, 900), (1100, 680)]) {
      final mode = dark ? 'dark' : 'light';
      testWidgets('create sheet $mode ${w}x$h', (tester) async {
        tester.view.physicalSize = Size(w * 2.0, h * 2.0);
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
        // Home's own header overflows below ~1200px; that is not the sheet.
        tester.takeException();
        final name = '${mode}_${w}x$h';
        await tester.tap(find.byTooltip('New agent'));
        await tester.pumpAndSettle();
        await _shot(tester, key, 'sheet_create_blank_$name');
        await tester.tap(find.byKey(const Key('starter-select')));
        await tester.pumpAndSettle();
        await _shot(tester, key, 'sheet_starters_$name');
        await tester.enterText(find.byKey(const Key('starter-search')), 'mail');
        await tester.pumpAndSettle();
        await _shot(tester, key, 'sheet_starters_filtered_$name');
        await tester.enterText(find.byKey(const Key('starter-search')), '');
        await tester.pumpAndSettle();
        await tester.tap(find.byKey(const Key('starter-research')));
        await tester.pumpAndSettle();
        await _shot(tester, key, 'sheet_create_picked_$name');
        await tester.ensureVisible(find.text('Advanced'));
        await tester.tap(find.text('Advanced'));
        await tester.pumpAndSettle();
        await _shot(tester, key, 'sheet_create_advanced_$name');
        expect(tester.takeException(), isNull);
        debugDefaultTargetPlatformOverride = null;
      });
    }
  }
}
