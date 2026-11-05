// Routines made in conversation stay findable: a routine Research runs for
// #news shows in Research's panel, in #news (header and panel) and on the
// Library's Routines page; the agent's answer carries a card that opens it;
// and every list refreshes live on `schedules_changed`.
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/routines.dart';

class FakeRoutinesClient extends TernClient {
  FakeRoutinesClient() : super(const Endpoint.unix('/nonexistent'));

  var list = <Schedule>[];
  var reads = 0;

  @override
  Future<List<Schedule>> schedules() async {
    reads++;
    return list;
  }

  @override
  Future<CronPreview> previewCron(String cron) async =>
      CronPreview(cron: cron, next: [DateTime(2026, 11, 5, 9)]);
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
];

final routine = Schedule(
  id: 'sch_ai_news',
  cron: '0 9 * * *',
  prompt: "Look up this week's AI news and summarise it.",
  sessionId: 'routine-session',
  enabled: true,
  nextRunAt: DateTime(_now.year, _now.month, _now.day + 1, 9),
  channelId: 'news',
  agentId: 'tern:research',
  initiative: 'interrupt',
);

const answer =
    'Done. Research will look up AI news every morning at 9:00 and post it '
    'here. [Open the routine](tern://routine/sch_ai_news)';

TernStore storeWith(FakeRoutinesClient client) {
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.2.0',
      features: {'channels', 'agents', 'schedules'},
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
      Channel(
        id: 'dm:research',
        slug: 'dm-research',
        title: 'Research',
        kind: 'main',
        agentId: 'tern:research',
        updatedAt: _now,
      ),
      Channel(
        id: 'news',
        slug: 'news',
        title: 'News',
        kind: 'topic',
        agentId: 'tern',
        mainSessionId: 'main',
        lastPostId: 2,
        updatedAt: _now,
      ),
    ];
  store.lastSeen['news'] = 2;
  store.posts['news'] = [
    Post(
      id: 1,
      channelId: 'news',
      sessionId: 'main',
      authorKind: 'user',
      authorId: 'owner',
      author: 'you',
      kind: 'message',
      text: 'Every morning, have Research look up AI news',
      createdAt: _now.subtract(const Duration(minutes: 2)),
    ),
    Post(
      id: 2,
      channelId: 'news',
      sessionId: 'main',
      authorKind: 'agent',
      authorId: 'tern',
      author: 'tern',
      kind: 'answer',
      text: answer,
      createdAt: _now.subtract(const Duration(minutes: 1)),
    ),
  ];
  return store;
}

Future<void> pumpHome(WidgetTester tester, TernStore store) async {
  tester.view.physicalSize = const Size(1400, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Home(store: store),
    ),
  );
  await tester.pump(const Duration(milliseconds: 100));
}

void main() {
  test('a schedule tool result becomes a routine card, live and replayed', () {
    const out =
        'created schedule sch_ai_news (cron 0 9 * * *); runs as Research; '
        'next run Thu; results are posted to #news\n'
        'Link for the user (include it in your answer): '
        '[Open the routine](tern://routine/sch_ai_news)';
    final live = Transcript()
      ..apply(
        TernEvent.fromJson({
          'type': 'tool_finished',
          'run_id': 'r',
          'id': 't1',
          'name': 'schedule_create',
          'is_error': false,
          'output': out,
        }),
      );
    final card = live.items.whereType<RoutineItem>().single;
    expect((card.scheduleId, card.created), ('sch_ai_news', true));
    expect(live.items.whereType<PillItem>(), isEmpty);

    final replayed = Transcript.fromSession(
      const SessionDetail(
        id: 's',
        messages: [
          SessionMessage(
            role: 'assistant',
            summary: false,
            content: [
              ToolUseBlock(
                id: 't1',
                name: 'schedule_create',
                input: {'cron': '0 9 * * *'},
              ),
            ],
          ),
          SessionMessage(
            role: 'user',
            summary: false,
            content: [
              ToolResultBlock(toolUseId: 't1', content: out, isError: false),
            ],
          ),
        ],
      ),
    );
    expect(
      replayed.items.whereType<RoutineItem>().single.scheduleId,
      'sch_ai_news',
    );
    expect(routineLinksIn(answer), ['sch_ai_news']);
    expect(routineIdOf('tern://routine/sch_ai_news'), 'sch_ai_news');
    expect(routineIdOf('https://example.com'), isNull);
  });

  testWidgets('a routine made in #news shows there, in Research and in the '
      'Library, live', (tester) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
    final client = FakeRoutinesClient();
    final store = storeWith(client)
      ..channel = 'news'
      ..selected = 'main';
    await pumpHome(tester, store);
    // Not there yet: the agent hasn't made it.
    expect(
      find.text('No routines post here yet. Ask for one, or add one.'),
      findsOneWidget,
    );
    expect(
      find.byKey(const ValueKey('routine-card-sch_ai_news')),
      findsOneWidget,
    );
    expect(find.text('This routine was deleted.'), findsOneWidget);

    // The daemon says the routines changed: every list re-reads.
    client.list = [routine];
    store.onEventForTest(
      TernEvent.fromJson({
        'schedules_changed': {'id': 'sch_ai_news'},
      }),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(client.reads, 1);

    // #news: the panel's list, the header's count and the answer's card.
    expect(find.text('Routines · #news'), findsOneWidget);
    expect(
      find.text("Look up this week's AI news and summarise it."),
      findsOneWidget,
    );
    expect(
      find.descendant(
        of: find.byKey(const ValueKey('channel-routines')),
        matching: find.text('1'),
      ),
      findsOneWidget,
    );
    expect(find.text('Runs as Research · posts to #news'), findsOneWidget);

    // The card opens it in the editor.
    await tester.tap(find.widgetWithText(TextButton, 'Open').last);
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.byType(RoutineSheet), findsOneWidget);
    Navigator.of(tester.element(find.byType(RoutineSheet))).pop();
    await tester.pump(const Duration(milliseconds: 400));

    // The header lists the channel's routines.
    await tester.tap(find.byKey(const ValueKey('channel-routines')));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 400));
    expect(find.text('Routines in #news'), findsOneWidget);
    await tester.tap(find.text('Close'));
    await tester.pump(const Duration(milliseconds: 400));

    // Research's DM: its panel lists it as Research's.
    store.channel = 'dm:research';
    store.notifyListeners();
    await tester.pump();
    expect(find.text('Routines · Research'), findsOneWidget);
    expect(
      find.text("Look up this week's AI news and summarise it."),
      findsOneWidget,
    );

    // The Library's Routines page lists every routine, with where it posts.
    store.openRoutines();
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(find.byType(RoutinesPage), findsOneWidget);
    expect(
      find.descendant(
        of: find.byType(RoutinesPage),
        matching: find.textContaining('→ #news'),
      ),
      findsOneWidget,
    );

    // Deleted elsewhere: gone from every list on the next event.
    client.list = [];
    store.onEventForTest(
      TernEvent.fromJson({
        'schedules_changed': {'id': 'sch_ai_news'},
      }),
    );
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(
      find.descendant(
        of: find.byType(RoutinesPage),
        matching: find.textContaining('→ #news'),
      ),
      findsNothing,
    );
    expect(tester.takeException(), isNull);
    debugDefaultTargetPlatformOverride = null;
  });
}
