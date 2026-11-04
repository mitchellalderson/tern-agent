// Topic channels in the desktop app (docs/channels-design.md §5.2).
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/channel_view.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/routines.dart';
import 'package:tern_desktop/ui/thread_pane.dart';

final _t0 = DateTime(2026, 11, 3, 9);

Post _post(
  int id,
  String text, {
  String kind = 'answer',
  String authorKind = 'agent',
  String author = 'tern',
  String session = 'main',
  int? replyTo,
  int replies = 0,
}) => Post(
  id: id,
  channelId: 'fin',
  sessionId: session,
  runId: 'r$id',
  authorKind: authorKind,
  authorId: authorKind == 'user' ? 'owner' : author,
  author: author,
  kind: kind,
  text: text,
  replyTo: replyTo,
  replies: replies,
  createdAt: _t0.add(Duration(minutes: id)),
);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final sent = <({String channel, String text, int? replyTo})>[];
  final created = <String>[];
  final scheduleChannels = <String?>[];
  var lastPost = 6;

  Channel _ch(String id, String slug, String title, {int? last}) => Channel(
    id: id,
    slug: slug,
    title: title,
    kind: id == 'home' ? 'main' : 'topic',
    mainSessionId: id == 'home' ? 'h1' : 'main',
    lastPostId: last,
    instructions: id == 'fin' ? 'Amounts in EUR.' : null,
    updatedAt: _t0,
  );

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    _ch('home', 'home', 'Home'),
    _ch('fin', 'finances', 'Finances', last: lastPost),
    _ch('launch', 'launch', 'Launch', last: 2),
    for (final c in created) _ch(c, c, c),
  ];

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => channel != 'fin'
      ? []
      : [
          // Newest first; 5 and 6 are a side thread off post 2.
          _post(6, 'Mostly card fees.', session: 'side', replyTo: 5),
          _post(
            5,
            'Why so much?',
            kind: 'message',
            authorKind: 'user',
            author: 'you',
            session: 'side',
            replyTo: 2,
          ),
          _post(
            3,
            '2 invoices need a decision.',
            authorKind: 'routine',
            author: 'reply-sweep',
            session: 'cron-1',
          ),
          _post(2, 'About 1,240 EUR.', replyTo: 1, replies: 1),
          _post(
            1,
            'How much did we spend?',
            kind: 'message',
            authorKind: 'user',
            author: 'you',
          ),
        ];

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async => const StreamPage(items: []);

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<SessionDetail> session(String id) async =>
      SessionDetail(id: id, messages: const []);

  @override
  Future<Sent> sendToChannel(
    String channel,
    String text, {
    int? replyTo,
    bool newTopic = false,
  }) async {
    sent.add((channel: channel, text: text, replyTo: replyTo));
    return Sent(
      post: _post(
        10 + sent.length,
        text,
        kind: 'message',
        authorKind: 'user',
        author: 'you',
        session: replyTo == null ? 'main' : 'side2',
        replyTo: replyTo,
      ),
      runId: 'run${sent.length}',
      sessionId: replyTo == null ? 'main' : 'side2',
    );
  }

  @override
  Future<Channel> createChannel({
    required String slug,
    String? title,
    String? instructions,
  }) async {
    created.add(slug);
    return _ch(slug, slug, title ?? slug);
  }

  @override
  Future<CronPreview> previewCron(String cron) async =>
      CronPreview(cron: cron, next: [_t0]);

  @override
  Future<Schedule> createSchedule({
    required String cron,
    required String prompt,
    String? deliverTo,
    ScheduleLimits? limits,
    String? channel,
  }) async {
    scheduleChannels.add(channel);
    return Schedule(
      id: 's',
      cron: cron,
      prompt: prompt,
      sessionId: 'x',
      enabled: true,
    );
  }

  @override
  Future<List<Schedule>> schedules() async => [];
}

Future<(TernStore, _FakeClient)> _store() async {
  final client = _FakeClient();
  final store = TernStore(client, lastSeen: {'launch': 2})
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0.2.0', features: {'channels'})
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  store.channels = await client.channels();
  await store.openHome();
  return (store, client);
}

Future<void> _pump(WidgetTester tester, TernStore store) async {
  tester.view.physicalSize = const Size(1500, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Home(store: store),
    ),
  );
  await tester.pump();
}

/// Let dialogs open and futures finish; the activity line animates for as
/// long as a run is on, so `pumpAndSettle` would never return.
Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  test('read state: unread until opened, saved through the callback', () async {
    var saves = 0;
    final client = _FakeClient();
    final store = TernStore(
      client,
      lastSeen: {'launch': 2},
      onReadChanged: () => saves++,
    )..health = const Health(pid: 1, version: '0', features: {'channels'});
    store.channels = await client.channels();
    final fin = store.topics.first;
    expect(store.topics.map((c) => c.slug), ['finances', 'launch']);
    expect(store.unread(fin), isTrue);
    expect(store.unread(store.topics.last), isFalse);
    await store.openChannelView('fin');
    expect(store.lastSeen['fin'], 6);
    expect(saves, 1);
    // The timeline keeps side-thread replies out.
    final shown = store.posts['fin']!
        .where((p) => ChannelView.inTimeline(p, 'main'))
        .map((p) => p.id);
    expect(shown, [1, 2, 3]);
  });

  test('approvals from a channel are answered in Home', () async {
    final (store, _) = await _store();
    await store.openChannelView('fin');
    store.onEventForTest(const RunStarted(runId: 'r9', sessionId: 'main'));
    // The daemon adds channel_id; hand-built events can't, so use JSON.
    store.onEventForTest(
      TernEvent.fromJson({
        'type': 'tool_started',
        'run_id': 'r9',
        'session_id': 'main',
        'channel_id': 'fin',
        'id': 't1',
        'name': 'shell',
        'summary': 'shell ls',
      }),
    );
    store.onEventForTest(
      const ApprovalRequested(
        Approval(
          id: 'a1',
          runId: 'r9',
          sessionId: 'main',
          tool: 'shell',
          input: {},
          summary: 'shell rm x',
          pattern: null,
        ),
      ),
    );
    final home = store.transcripts[TernStore.homeKey]!;
    expect(home.items.whereType<ApprovalItem>().single.approval.id, 'a1');
    expect(store.transcripts['main']!.items.whereType<ApprovalItem>(), isEmpty);
    expect(store.waitingFrom('fin'), 1);
    expect(store.pendingApprovals, 1);
    // The run ending closes it there too.
    store.onEventForTest(const RunFinished(runId: 'r9', status: 'completed'));
    expect(home.items.whereType<ApprovalItem>().single.outcome, 'expired');
    expect(store.waitingFrom('fin'), 0);
  });

  testWidgets('sidebar, timeline, posting, threads and a new channel', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _pump(tester, store);

    // CHANNELS in the sidebar, with an unread dot on #finances only.
    expect(find.text('CHANNELS'), findsOneWidget);
    expect(find.text('Finances'), findsOneWidget);
    expect(find.byTooltip('New posts'), findsOneWidget);

    await tester.tap(find.text('Finances'));
    await tester.pumpAndSettle();
    expect(store.channel, 'fin');
    expect(find.text('How much did we spend?'), findsOneWidget);
    expect(find.text('About 1,240 EUR.'), findsOneWidget);
    expect(find.text('reply-sweep'), findsOneWidget);
    expect(
      find.text('Mostly card fees.'),
      findsNothing,
      reason: 'in its thread',
    );
    expect(find.text('1 reply'), findsOneWidget);
    expect(find.byTooltip('New posts'), findsNothing);

    // Posting from the composer.
    await tester.enterText(
      find.widgetWithText(TextField, 'Message #finances'),
      'And Northwind?',
    );
    await tester.pump();
    await tester.tap(find.byTooltip('Send (⏎)').first);
    await _settle(tester);
    expect(client.sent.single, (
      channel: 'fin',
      text: 'And Northwind?',
      replyTo: null,
    ));
    expect(find.text('And Northwind?'), findsOneWidget);

    // A thread's replies open beside the timeline.
    await tester.tap(find.text('1 reply'));
    await _settle(tester);
    expect(store.thread, 'side');
    expect(find.byType(ThreadPane), findsOneWidget);

    // Reply in thread on the routine's post.
    await tester.tap(
      find.descendant(
        of: find.ancestor(
          of: find.text('reply-sweep'),
          matching: find.byType(TimelinePost),
        ),
        matching: find.text('Reply in thread'),
      ),
    );
    await _settle(tester);
    expect(find.text('Reply to reply-sweep'), findsOneWidget);
    await tester.enterText(
      find.descendant(
        of: find.byType(ThreadPane),
        matching: find.byType(TextField),
      ),
      'Hold Northwind',
    );
    await tester.pump();
    await tester.tap(
      find.descendant(
        of: find.byType(ThreadPane),
        matching: find.byTooltip('Send (⏎)'),
      ),
    );
    await _settle(tester);
    expect(client.sent.last.replyTo, 3);
    expect(store.thread, 'side2');

    // A new channel.
    await tester.tap(find.byTooltip('New channel'));
    await _settle(tester);
    await tester.enterText(
      find
          .descendant(
            of: find.byType(ChannelDialog),
            matching: find.byType(TextField),
          )
          .first,
      'Q4 Launch!',
    );
    await tester.pump();
    expect(find.text('#q4-launch'), findsOneWidget);
    await tester.tap(find.text('Create'));
    await _settle(tester);
    expect(client.created, ['q4-launch']);
    expect(store.channel, 'q4-launch');
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('a routine chooses the channel it posts to', (tester) async {
    tester.view.physicalSize = const Size(700, 1200) * 2;
    tester.view.devicePixelRatio = 2;
    addTearDown(tester.view.reset);
    final (store, client) = await _store();
    await store.openChannelView('fin');
    await tester.pumpWidget(
      MaterialApp(
        theme: ternTheme(TernColors.light, Brightness.light),
        home: Scaffold(body: RoutineSheet(store: store)),
      ),
    );
    await tester.pumpAndSettle();
    // Made from #finances: posts there unless changed.
    expect(find.text('Post to #finances'), findsOneWidget);
    await tester.enterText(find.byType(TextField).first, 'Sweep invoices');
    await tester.pumpAndSettle();
    await tester.tap(find.text('Post to #finances'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Post to #launch').last);
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Create routine'));
    await tester.pumpAndSettle();
    expect(client.scheduleChannels, ['launch']);
  });

  test('slugs are made from titles', () {
    expect(slugFor('Q4 Launch!'), 'q4-launch');
    expect(slugFor('  --Finances--  '), 'finances');
    expect(slugFor('x' * 40).length, 32);
  });
}
