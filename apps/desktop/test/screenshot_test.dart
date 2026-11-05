// Renders the main window with real fonts to PNGs in build/screenshots/,
// for checking the design without running the app:
//   flutter test test/screenshot_test.dart
// Uses the system font where it can find one; otherwise text is boxes.
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
import 'package:tern_desktop/ui/mcp_sheet.dart';

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

TernStore demoStore() {
  final now = DateTime.now();
  final store = TernStore(TernClient(const Endpoint.unix('/nonexistent')))
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.1.0',
      features: {'channels', 'agents'},
    )
    ..info = const Info(
      defaultModel: 'anthropic:claude-sonnet-5-5',
      tools: [
        (name: 'shell', description: 'Run a command'),
        (name: 'read_file', description: ''),
        (name: 'write_file', description: ''),
        (name: 'http_get', description: ''),
        (name: 'memory_save', description: ''),
        (name: 'schedule_create', description: ''),
        (name: 'mcp__github__search', description: ''),
      ],
    );
  store.sessions = [
    SessionItem(
      id: 's1',
      title: 'Research seed-stage fintechs',
      source: 'api',
      messages: 6,
      runs: 1,
      updatedAt: now.subtract(const Duration(minutes: 3)),
    ),
    SessionItem(
      id: 's2',
      title: 'Overnight sourcing',
      source: 'cron',
      messages: 4,
      runs: 3,
      updatedAt: now.subtract(const Duration(hours: 2)),
    ),
    SessionItem(
      id: 's3',
      title: 'Reply to Nora about renewal',
      source: 'telegram',
      messages: 8,
      runs: 2,
      updatedAt: now.subtract(const Duration(hours: 5)),
    ),
    SessionItem(
      id: 's4',
      title: 'Write a HubSpot connector',
      source: 'api',
      messages: 12,
      runs: 4,
      updatedAt: now.subtract(const Duration(days: 2)),
    ),
    SessionItem(
      id: 's5',
      title: 'Weekly expense report',
      source: 'api',
      messages: 3,
      runs: 1,
      updatedAt: now.subtract(const Duration(days: 9)),
    ),
  ];
  store.schedules = [
    Schedule(
      id: 'a',
      cron: '0 1 * * *',
      prompt: 'Overnight sourcing: find new seed-stage fintechs',
      sessionId: 's2',
      enabled: true,
      nextRunAt: DateTime(now.year, now.month, now.day + 1, 1),
      lastStatus: 'completed',
    ),
    Schedule(
      id: 'b',
      cron: '0 8 * * 1-5',
      prompt: 'Reply sweep',
      sessionId: 'x',
      enabled: true,
      nextRunAt: DateTime(now.year, now.month, now.day + 1, 8),
      lastStatus: 'failed',
    ),
    const Schedule(
      id: 'c',
      cron: '0 9 * * 1',
      prompt: 'Weekly expense report',
      sessionId: 'y',
      enabled: false,
    ),
  ];
  const demoAgents = [
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
      avatar: AvatarSpec(glyph: '★', color: 'teal'),
      mainChannelId: 'dm:research',
    ),
    Agent(
      id: 'tern:revops',
      slug: 'revops',
      name: 'RevOps',
      avatar: AvatarSpec(glyph: '■', color: 'amber'),
      mainChannelId: 'dm:revops',
    ),
    Agent(
      id: 'tern:growth',
      slug: 'growth',
      name: 'Growth',
      avatar: AvatarSpec(glyph: '▲', color: 'violet'),
      mainChannelId: 'dm:growth',
    ),
    Agent(
      id: 'tern:support',
      slug: 'support',
      name: 'Support',
      avatar: AvatarSpec(glyph: '♥', color: 'pink'),
      mainChannelId: 'dm:support',
    ),
  ];
  store.agents = demoAgents;
  Channel topic(String slug, String title, int last, List<String> members) =>
      Channel(
        id: slug,
        slug: slug,
        title: title,
        kind: 'topic',
        agentId: 'tern',
        mainSessionId: 'c-$slug',
        lastPostId: last,
        agents: ChannelAgents(
          members: [
            for (final (i, m) in members.indexed)
              ChannelMember(agentId: m, lead: i == 0, addedAt: now),
          ],
          agentWakes: true,
        ),
        updatedAt: now,
      );
  store.channels = [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      agentId: 'tern',
      mainSessionId: 's1',
      updatedAt: now,
    ),
    for (final a in demoAgents.skip(1))
      Channel(
        id: a.mainChannelId!,
        slug: 'dm-${a.slug}',
        title: a.name,
        kind: 'main',
        agentId: a.id,
        updatedAt: now,
      ),
    topic('monday-review', 'Monday review', 4, ['tern:revops', 'tern']),
    topic('activation', 'Activation', 6, ['tern:growth', 'tern:research']),
    topic('customers', 'Customers', 3, ['tern:support', 'tern:revops']),
    topic('launch', 'Launch', 2, ['tern:research', 'tern:growth', 'tern']),
  ];
  // Two channels have posts you haven't read, so the dots show.
  store.lastSeen
    ..['monday-review'] = 1
    ..['activation'] = 6
    ..['customers'] = 3
    ..['launch'] = 2;
  store.selected = 's1';
  store.draft = false;
  final t0 = now.subtract(const Duration(seconds: 50));
  final t = Transcript()
    ..items.add(
      UserItem(
        'Find seed-stage fintechs that raised in the last month and draft intros for the ones with a live signal.',
      ),
    );
  for (final e in <TernEvent>[
    const RunStarted(runId: 'r', sessionId: 's1'),
    const AssistantMessage(
      runId: 'r',
      text: "On it. I'll research the list, then draft.",
      replayed: false,
    ),
    const ToolStarted(
      runId: 'r',
      id: '1',
      name: 'http_get',
      summary: 'http_get crunchbase.com/search?stage=seed',
    ),
    const ToolFinished(
      runId: 'r',
      id: '1',
      name: 'http_get',
      isError: false,
      preview: '',
      output: '40 results',
      replayed: false,
    ),
    const ToolStarted(
      runId: 'r',
      id: '2',
      name: 'memory_save',
      summary: 'memory_save prefers short openers',
    ),
    const ToolFinished(
      runId: 'r',
      id: '2',
      name: 'memory_save',
      isError: false,
      preview: '',
      output: 'saved',
      replayed: false,
    ),
    const ToolStarted(
      runId: 'r',
      id: '3',
      name: 'write_file',
      summary: 'write_file drafts/intros.md',
    ),
    const ToolFinished(
      runId: 'r',
      id: '3',
      name: 'write_file',
      isError: false,
      preview: '',
      output: 'wrote 18 drafts',
      replayed: false,
    ),
    const ToolStarted(
      runId: 'r',
      id: 'sp0',
      name: 'agent_spawn',
      summary: 'agent_spawn pricing-research',
    ),
    const SubagentSpawned(
      runId: 'r',
      callId: 'sp0',
      childRunId: 'cr0',
      childSessionId: 'cs0',
      label: 'pricing-research',
      task: 'Find seed-stage pricing pages',
      model: 'anthropic:claude-haiku-5-5',
      status: 'running',
    ),
    const ToolStarted(
      runId: 'r',
      id: 'sp1',
      name: 'agent_spawn',
      summary: 'agent_spawn competitor-scan',
    ),
    const SubagentSpawned(
      runId: 'r',
      callId: 'sp1',
      childRunId: 'cr1',
      childSessionId: 'cs1',
      label: 'competitor-scan',
      task: 'Scan competitor launches',
      model: 'anthropic:claude-haiku-5-5',
      status: 'running',
    ),
    const ToolStarted(
      runId: 'r',
      id: 'sp2',
      name: 'agent_spawn',
      summary: 'agent_spawn docs-review',
    ),
    const SubagentSpawned(
      runId: 'r',
      callId: 'sp2',
      childRunId: 'cr2',
      childSessionId: 'cs2',
      label: 'docs-review',
      task: 'Review the intro drafts',
      model: 'anthropic:claude-haiku-5-5',
      status: 'running',
    ),
    const AssistantMessage(
      runId: 'r',
      text: 'Researched **40 accounts**, 22 with a live signal. Two raised last week, so those drafts lead with the round:\n\n- **Helix Pay**: seed, \$4M, hiring a first AE\n- **Ledgerly**: seed, \$6M, new CFO\n\nDrafts are in `drafts/intros.md`.',
      replayed: false,
    ),
    const ToolStarted(
      runId: 'r',
      id: '4',
      name: 'shell',
      summary: 'shell git push origin main',
    ),
  ]) {
    t.apply(e, now: t0);
  }
  (t.items.whereType<WorkItem>().first)
    ..runId = 'r'
    ..started = t0
    ..finished = t0.add(const Duration(seconds: 42));
  t.apply(
    const ApprovalRequested(
      Approval(
        id: 'a',
        runId: 'r',
        sessionId: 's1',
        tool: 'shell',
        input: {'command': 'git push origin main'},
        summary: 'shell git push origin main',
        effect: 'destructive',
        pattern: 'git push *',
      ),
    ),
  );
  final card = t.items.whereType<AgentsItem>().single;
  for (final (i, k) in card.children.indexed) {
    k.started = t0.subtract(Duration(seconds: 70 - i * 9));
  }
  t
    ..updateChild(
      'cr0',
      status: 'completed',
      result: '12 pricing pages; 9 list a free tier',
      finished: t0.subtract(const Duration(seconds: 8)),
    )
    ..apply(
      const SubagentMessage(
        runId: 'cr2',
        toRunId: 'r',
        kind: 'question',
        text: 'Include the two drafts marked WIP?',
      ),
    );
  store.transcripts['cs1'] = Transcript()
    ..apply(const RunStarted(runId: 'cr1', sessionId: 'cs1'))
    ..apply(
      const ToolStarted(
        runId: 'cr1',
        id: 'x',
        name: 'http_fetch',
        summary: 'http_fetch news.ycombinator.com',
      ),
    );
  store.transcripts['s1'] = t;
  store.transcripts['s3'] = Transcript()
    ..apply(
      const ApprovalRequested(
        Approval(
          id: 'b',
          runId: 'r2',
          sessionId: 's3',
          tool: 'shell',
          input: {},
          summary: 'x',
          pattern: 'x',
        ),
      ),
    );
  store.usage['s1'] = const Usage(
    inputTokens: 18400,
    outputTokens: 2100,
    cacheReadTokens: 12000,
  );
  return store;
}

/// Answers the add sheet's calls without a daemon.
class _McpClient extends TernClient {
  _McpClient() : super(const Endpoint.unix('/nonexistent'));

  @override
  Future<List<SecretStatus>> secrets() async => const [];

  @override
  Future<McpVerifyResult> verifyMcp(
    String name,
    McpServerConfig config, {
    Map<String, String> secrets = const {},
  }) async => const McpVerifyResult(
    error: McpVerifyError(
      kind: 'exited_early',
      message: 'failed to start: connection closed',
      stderrTail: [
        'GitHub MCP Server running on stdio',
        'Error: GITHUB_TOKEN is invalid (401 Bad credentials)',
      ],
    ),
  );
}

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
    testWidgets('main window ${dark ? 'dark' : 'light'}', (tester) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final c = dark ? TernColors.dark : TernColors.light;
      final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
      final key = GlobalKey();
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
            home: Home(store: demoStore()),
          ),
        ),
      );
      await tester.pump(const Duration(milliseconds: 100));
      expect(tester.takeException(), isNull);
      // The approval card leads with the call's effect.
      expect(
        find.byWidgetPredicate(
          (w) =>
              w is SelectableText &&
              w.data == 'destructive · shell git push origin main',
        ),
        findsOneWidget,
      );
      await tester.runAsync(() async {
        final boundary =
            key.currentContext!.findRenderObject()! as RenderRepaintBoundary;
        final image = await boundary.toImage(pixelRatio: 2);
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        final out = File(
          'build/screenshots/main_${dark ? 'dark' : 'light'}.png',
        );
        await out.create(recursive: true);
        await out.writeAsBytes(png!.buffer.asUint8List());
      });
      debugDefaultTargetPlatformOverride = null;
    });

    testWidgets('mcp ${dark ? 'dark' : 'light'}', (tester) async {
      tester.view.physicalSize = const Size(1280, 800) * 2;
      tester.view.devicePixelRatio = 2;
      addTearDown(tester.view.reset);
      debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
      final c = dark ? TernColors.dark : TernColors.light;
      final theme = ternTheme(c, dark ? Brightness.dark : Brightness.light);
      final key = GlobalKey();
      final now = DateTime.now();
      final store = demoStore()
        ..health = const Health(pid: 1, version: '0.1.0', features: {'mcp'})
        ..page = LibraryPage.mcp
        ..mcpServers = [
          McpStatus(
            name: 'github',
            config: const McpServerConfig(
              command: 'github-mcp-server',
              args: ['stdio'],
              envSecrets: ['GITHUB_TOKEN'],
            ),
            state: McpRunning(tools: 23, since: now),
          ),
          const McpStatus(
            name: 'linear',
            config: McpServerConfig(
              transport: McpTransport.http,
              url: 'https://mcp.linear.app/mcp',
              enabled: false,
            ),
            state: McpDisabled(),
          ),
          McpStatus(
            name: 'postgres',
            config: const McpServerConfig(command: 'pg-mcp'),
            state: McpUnavailable(
              error:
                  'failed to start: connection closed (`FATAL: password '
                  'authentication failed for user "app"`)',
              retryAt: now.add(const Duration(seconds: 12)),
              failures: 3,
            ),
          ),
          const McpStatus(
            name: 'filesystem',
            config: McpServerConfig(command: 'fs-mcp'),
            state: McpNotStarted(),
          ),
        ];
      Future<void> shot(String name) => tester.runAsync(() async {
        final boundary =
            key.currentContext!.findRenderObject()! as RenderRepaintBoundary;
        final image = await boundary.toImage(pixelRatio: 2);
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        final out = File(
          'build/screenshots/${name}_${dark ? 'dark' : 'light'}.png',
        );
        await out.create(recursive: true);
        await out.writeAsBytes(png!.buffer.asUint8List());
      });
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
      await shot('mcp');

      final sheetStore = TernStore(_McpClient())
        ..health = store.health
        ..mcpServers = store.mcpServers;
      showMcpSheet(
        tester.element(find.byType(Home)),
        sheetStore,
        drafts: const [
          ImportedServer(
            name: 'github',
            config: McpServerConfig(
              command: 'npx',
              args: ['-y', '@modelcontextprotocol/server-github'],
              env: {'LOG_LEVEL': 'warn'},
              envSecrets: ['GITHUB_TOKEN'],
            ),
            secrets: {'GITHUB_TOKEN': 'ghp_pasted'},
          ),
        ],
      );
      // Not pumpAndSettle: the retry countdown ticks every second.
      await tester.pump(const Duration(milliseconds: 400));
      await tester.tap(find.text('Test connection'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 400));
      expect(tester.takeException(), isNull);
      await shot('mcp_add');
      debugDefaultTargetPlatformOverride = null;
    });
  }
}
