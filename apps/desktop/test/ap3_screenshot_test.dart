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
              File(
                'packages/tern_client/test/fixtures/bundle_preview_agents.json',
              ).readAsStringSync(),
            )
            as Json,
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
  store.transcripts[TernStore.streamKeyOf('dm:research')] =
      Transcript.fromStream([
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
}
