// Home: one stream over many sessions (docs/channels-design.md §2.6, §5.2).
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/state/transcript.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/channel_items.dart';
import 'package:tern_desktop/ui/home.dart';
import 'package:tern_desktop/ui/thread_pane.dart';

final _t0 = DateTime(2026, 11, 3, 9);

StreamMessage _msg(
  String session,
  int id,
  String role,
  String text, {
  bool queued = false,
  List<ContentBlock>? content,
}) => StreamMessage(
  sessionId: session,
  messageId: id,
  message: SessionMessage(
    role: role,
    runId: 'r-$session',
    content: content ?? [TextBlock(text)],
    summary: false,
  ),
  queued: queued,
  createdAt: _t0.add(Duration(minutes: id)),
);

/// Newest first, as the daemon sends it.
final _page = StreamPage(
  items: [
    StreamSideChat(
      sessionId: 'side',
      title: '> Northwind is 310 EUR\n\nWhy so much?',
      branchedFrom: 'message:1',
      createdAt: _t0.add(const Duration(minutes: 30)),
    ),
    _msg('s2', 21, 'assistant', 'Launch plan drafted.'),
    _msg('s2', 20, 'user', 'and the venue', queued: true),
    _msg('s2', 19, 'user', 'Plan the launch'),
    StreamCut(
      sessionId: 's2',
      prevSessionId: 's1',
      reason: 'new_topic',
      carry: '[Continuing from your previous session]\nSummary: invoices.',
      createdAt: _t0.add(const Duration(minutes: 18)),
    ),
    StreamPost(
      Post(
        id: 3,
        channelId: 'home',
        sessionId: 'cron-1',
        authorKind: 'routine',
        authorId: 'routine:brief',
        author: 'Morning brief',
        kind: 'answer',
        text: 'Two meetings today.',
        createdAt: _t0.add(const Duration(minutes: 10)),
      ),
    ),
    _msg(
      's1',
      3,
      'user',
      '',
      content: const [
        ToolResultBlock(toolUseId: 'c1', content: '2 files', isError: false),
      ],
    ),
    _msg(
      's1',
      2,
      'assistant',
      '',
      content: const [
        ToolUseBlock(id: 'c1', name: 'list_dir', input: {'path': '.'}),
      ],
    ),
    _msg('s1', 1, 'user', 'Northwind is 310 EUR'),
  ],
  next: '1.1.1',
);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final sends = <({String text, bool newTopic})>[];
  final threadSends = <({String text, String? channel, String? from})>[];
  var head = 's2';

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      mainSessionId: head,
      updatedAt: _t0,
    ),
  ];

  @override
  Future<StreamPage> stream(
    String channel, {
    String? before,
    int limit = 200,
  }) async => before == null
      ? _page
      : StreamPage(items: [_msg('s0', 0, 'user', 'The very first thing')]);

  @override
  Future<Usage> usage(String session) async => const Usage();

  @override
  Future<List<SessionItem>> sessions({int limit = 50, String? channel}) async =>
      [
        SessionItem(
          id: 's2',
          title: 'Plan the launch',
          source: 'api',
          messages: 3,
          runs: 1,
          updatedAt: _t0,
          prevSessionId: 's1',
          segmentReason: 'new_topic',
        ),
        SessionItem(
          id: 's1',
          title: 'Northwind is 310 EUR',
          source: 'api',
          messages: 3,
          runs: 1,
          updatedAt: _t0,
          segmentReason: 'migrated',
        ),
        SessionItem(
          id: 'side',
          title: 'Why so much?',
          source: 'api',
          messages: 1,
          runs: 1,
          updatedAt: _t0,
          branchedFrom: 'message:1',
        ),
      ];

  @override
  Future<SessionDetail> session(String id) async => SessionDetail(
    id: id,
    title: 'Northwind is 310 EUR',
    messages: const [
      SessionMessage(
        role: 'user',
        content: [TextBlock('Northwind is 310 EUR')],
        summary: false,
      ),
    ],
  );

  @override
  Future<Sent> sendToChannel(
    String channel,
    String text, {
    int? replyTo,
    bool newTopic = false,
  }) async {
    sends.add((text: text, newTopic: newTopic));
    return Sent(
      runId: 'r-new',
      sessionId: newTopic ? 's3' : head,
      newSegment: newTopic,
      queued: text.startsWith('also'),
    );
  }

  @override
  Future<Started> send(
    String text, {
    String? session,
    String? model,
    String? channel,
    String? branchedFrom,
  }) async {
    threadSends.add((text: text, channel: channel, from: branchedFrom));
    return const Started(sessionId: 'side2', runId: 'r-side', model: 'm');
  }
}

Future<(TernStore, _FakeClient)> _home() async {
  final client = _FakeClient();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0.2.0', features: {'channels'})
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  await store.openHome();
  return (store, client);
}

Future<void> _pump(WidgetTester tester, TernStore store) async {
  tester.view.physicalSize = const Size(1500, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  // The window's native menus exist on macOS only.
  debugDefaultTargetPlatformOverride = TargetPlatform.macOS;
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Home(store: store),
    ),
  );
  await tester.pump();
}

void main() {
  test('the stream becomes one transcript with dividers, posts and cards', () {
    final t = Transcript.fromStream(_page.items.reversed);
    expect(t.items.map((i) => i.runtimeType), [
      UserItem,
      WorkItem,
      PostItem,
      CutItem,
      UserItem,
      UserItem,
      AssistantItem,
      SideChatItem,
    ]);
    // A tool call and its result pair up across messages.
    final work = t.items.whereType<WorkItem>().single;
    expect(work.steps.single.status, StepStatus.ok);
    expect((t.items.first as UserItem).messageId, 1);
    expect(t.items.whereType<UserItem>().map((u) => u.queued), [
      false,
      false,
      true,
    ]);
  });

  test('a channel digest reads as a note, not as the user', () {
    final t = Transcript.fromSession(
      const SessionDetail(
        id: 's',
        messages: [
          SessionMessage(
            role: 'user',
            content: [
              TextBlock('[Posts in #finances since you last looked: 2. …]'),
            ],
            summary: false,
          ),
        ],
      ),
    );
    final pill = t.items.single as PillItem;
    expect(
      (pill.kind, pill.text),
      (PillKind.posts, 'Caught up on posts in #finances'),
    );
  });

  test('Home opens on the current segment and maps its sessions', () async {
    final (store, _) = await _home();
    expect(store.homeOpen, isTrue);
    expect(store.selected, 's2');
    expect(
      store.current!.items.whereType<CutItem>().single.cut.reason,
      'new_topic',
    );
    // Events of an earlier segment land in Home's one transcript.
    expect(store.transcriptOf('s1'), same(store.current));
    expect(store.homeNext, '1.1.1');
    await store.loadOlderHome();
    expect(
      (store.current!.items.first as UserItem).text,
      'The very first thing',
    );
    expect(store.homeNext, isNull);
  });

  test('sending: queued, and a new topic', () async {
    final (store, client) = await _home();
    await store.send('also the caterer');
    final queued = store.current!.items.whereType<UserItem>().last;
    expect((queued.text, queued.queued), ('also the caterer', true));

    store.toggleNewTopic();
    expect(store.newTopic, isTrue);
    client.head = 's3';
    await store.send('Something else');
    expect(client.sends.last, (text: 'Something else', newTopic: true));
    expect(store.newTopic, isFalse);
    expect(store.selected, 's3');
  });

  testWidgets('Home shows the stream, History and a side chat pane', (
    tester,
  ) async {
    final (store, client) = await _home();
    await _pump(tester, store);

    expect(find.text('Home'), findsWidgets);
    expect(
      find.text('New session · new topic · summary carried over'),
      findsOneWidget,
    );
    expect(find.text('Morning brief'), findsOneWidget);
    expect(find.text('Two meetings today.'), findsOneWidget);
    expect(find.text('Side chat · Why so much?'), findsOneWidget);
    expect(
      find.text('Sent while tern was working · read at its next step'),
      findsOneWidget,
    );
    expect(find.text('Load earlier'), findsOneWidget);

    // The divider expands to what was carried over.
    await tester.tap(find.byType(CutDivider));
    await tester.pump();
    expect(find.textContaining('Summary: invoices.'), findsOneWidget);

    // New topic, from the composer.
    await tester.tap(find.byTooltip('New topic (⌘N)').last);
    await tester.pump();
    expect(
      find.text('New topic: a fresh session, with a summary of this one'),
      findsOneWidget,
    );

    // History lists the stream's sessions and side chats.
    await tester.tap(find.text('History'));
    // The title bar also listens for double clicks.
    await tester.pump(const Duration(milliseconds: 500));
    await tester.pumpAndSettle();
    expect(find.text('Current session · 0m ago'), findsNothing);
    expect(find.textContaining('Earlier conversation'), findsOneWidget);
    expect(find.textContaining('Side chat ·'), findsWidgets);
    await tester.tap(find.text('Northwind is 310 EUR').last);
    await tester.pumpAndSettle();
    expect(store.thread, 's1');
    expect(find.byType(ThreadPane), findsOneWidget);

    // Branching a message opens a new side chat beside the stream.
    store.branchFrom('Launch plan drafted.', messageId: 21);
    await tester.pump();
    expect(find.text('New side chat'), findsOneWidget);
    await tester.enterText(
      find.descendant(
        of: find.byType(ThreadPane),
        matching: find.byType(TextField),
      ),
      'Which venue?',
    );
    await tester.pump();
    await tester.tap(
      find.descendant(
        of: find.byType(ThreadPane),
        matching: find.byTooltip('Send (⏎)'),
      ),
    );
    await tester.pump();
    expect(client.threadSends.single, (
      text: '> Launch plan drafted.\n\nWhich venue?',
      channel: 'home',
      from: 'message:21',
    ));
    expect(store.thread, 'side2');
    debugDefaultTargetPlatformOverride = null;
  });
}
