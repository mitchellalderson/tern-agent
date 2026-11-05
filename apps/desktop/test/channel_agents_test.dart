// Agents in topic channels (docs/agent-profiles-design.md §5, AP2): member
// avatars with the lead, "+ Agent", `@` autocomplete, notes and mentions in
// the timeline, and the lead and wake settings.
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/channel_view.dart';
import 'package:tern_desktop/ui/conversation.dart';
import 'package:tern_desktop/ui/home.dart';

final _t0 = DateTime(2026, 11, 4, 9);

const _tern = Agent(
  id: 'tern',
  slug: 'tern',
  name: 'tern',
  memory: 'shared',
  mainChannelId: 'home',
  isDefault: true,
);
const _research = Agent(
  id: 'tern:research',
  slug: 'research',
  name: 'Research',
  avatar: AvatarSpec(glyph: '★', color: 'teal'),
  mainChannelId: 'dm:research',
);
const _ops = Agent(
  id: 'tern:ops',
  slug: 'ops',
  name: 'Ops',
  avatar: AvatarSpec(glyph: '■', color: 'orange'),
  mainChannelId: 'dm:ops',
);

class _FakeClient extends TernClient {
  _FakeClient() : super(const Endpoint.unix('/nonexistent'));

  final added = <String>[];
  final sent = <String>[];
  final patches = <({String? lead, bool? agentWakes})>[];
  var lead = 'tern:research';
  var wakes = false;
  var decide = false;
  final decidePatches = <bool>[];

  ChannelAgents get _agents => ChannelAgents(
    letAgentsDecide: decide,
    members: [
      ChannelMember(agentId: lead, lead: true, addedAt: _t0),
      if (lead != 'tern') ChannelMember(agentId: 'tern', addedAt: _t0),
      for (final a in added) ChannelMember(agentId: a, addedAt: _t0),
    ],
    agentWakes: wakes,
  );

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      agentId: 'tern',
      mainSessionId: 'h1',
      updatedAt: _t0,
    ),
    for (final a in [_research, _ops])
      Channel(
        id: a.mainChannelId!,
        slug: 'dm-${a.slug}',
        title: a.name,
        kind: 'main',
        agentId: a.id,
        updatedAt: _t0,
      ),
    Channel(
      id: 'launch',
      slug: 'launch',
      title: 'Launch',
      kind: 'topic',
      agentId: lead == 'tern' ? null : lead,
      mainSessionId: 'main',
      lastPostId: 4,
      agents: _agents,
      updatedAt: _t0,
    ),
  ];

  @override
  Future<List<Agent>> agents({bool archived = false}) async => [
    _tern,
    _research,
    _ops,
  ];

  @override
  Future<List<Post>> posts(
    String channel, {
    int? before,
    int limit = 50,
  }) async => channel != 'launch'
      ? []
      : [
          // What "Let agents decide" leaves after routing a post.
          Post(
            id: 5,
            channelId: 'launch',
            authorKind: 'system',
            authorId: 'system',
            author: 'tern',
            kind: 'note',
            text: 'Sent to @research, @ops',
            createdAt: _t0.add(const Duration(minutes: 5)),
          ),
          Post(
            id: 4,
            channelId: 'launch',
            authorKind: 'system',
            authorId: 'system',
            author: 'tern',
            kind: 'note',
            text:
                '@ops not woken: #launch\'s hourly wake budget (6) is used up',
            createdAt: _t0.add(const Duration(minutes: 4)),
          ),
          Post(
            id: 3,
            channelId: 'launch',
            sessionId: 'main',
            runId: 'r3',
            authorKind: 'agent',
            authorId: 'tern:research',
            author: 'Research',
            kind: 'answer',
            text: 'Three venues fit. @ops can you book the first?',
            replyTo: 1,
            mentions: const ['tern:ops'],
            wakeDepth: 1,
            createdAt: _t0.add(const Duration(minutes: 3)),
          ),
          Post(
            id: 2,
            channelId: 'launch',
            authorKind: 'system',
            authorId: 'system',
            author: 'tern',
            kind: 'note',
            text: 'Research joined',
            createdAt: _t0.add(const Duration(minutes: 2)),
          ),
          Post(
            id: 1,
            channelId: 'launch',
            sessionId: 'main',
            authorKind: 'user',
            authorId: 'owner',
            author: 'you',
            kind: 'message',
            text: 'Find a venue for the offsite',
            createdAt: _t0.add(const Duration(minutes: 1)),
          ),
        ];

  @override
  Future<ChannelAgents> addChannelAgent(String channel, String agent) async {
    added.add(agent);
    return _agents;
  }

  @override
  Future<Channel> updateChannel(
    String id, {
    String? slug,
    String? title,
    String? instructions,
    bool clearInstructions = false,
    bool? archived,
    String? lead,
    bool? agentWakes,
    int? maxHops,
    int? wakesPerHour,
    bool? letAgentsDecide,
  }) async {
    patches.add((lead: lead, agentWakes: agentWakes));
    if (letAgentsDecide != null) {
      decidePatches.add(letAgentsDecide);
      decide = letAgentsDecide;
    }
    if (lead != null) this.lead = lead;
    if (agentWakes != null) wakes = agentWakes;
    return (await channels()).last;
  }

  @override
  Future<Sent> sendToChannel(
    String channel,
    String text, {
    int? replyTo,
    bool newTopic = false,
  }) async {
    sent.add(text);
    return const Sent(runId: 'r9', sessionId: 'main');
  }

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
  Future<List<Schedule>> schedules() async => [];
}

Future<(TernStore, _FakeClient)> _store() async {
  final client = _FakeClient();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.2.0',
      features: {'channels', 'agents'},
    )
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  store.channels = await client.channels();
  store.agents = await client.agents();
  await store.openChannelView('launch');
  return (store, client);
}

Future<void> _pump(WidgetTester tester, TernStore store) async {
  tester.view.physicalSize = const Size(1400, 900);
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

Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

void main() {
  test('mentions are marked as links for known agents only', () {
    expect(
      markMentions('Ask @ops, not me@example.com or @nobody', ['ops']),
      'Ask [@ops](mention:ops), not me@example.com or @nobody',
    );
    expect(markMentions('@Ops first', ['ops']), '[@Ops](mention:ops) first');
    expect(markMentions('no one', const []), 'no one');
  });

  test('the store names authors and lists who @ can name', () async {
    final (store, _) = await _store();
    expect(store.authorName('tern:research', 'x'), 'Research');
    expect(store.authorName('tern:research/scout-1', 'x'), 'Research/scout-1');
    expect(store.agentOfAuthor('tern:ops')?.name, 'Ops');
    final ch = store.openChannel!;
    expect(ch.lead, 'tern:research');
    expect(store.membersOf(ch).map((a) => a.name), ['Research', 'tern']);
    expect(store.mentionable(ch).map((m) => (m.agent.slug, m.member)), [
      ('research', true),
      ('tern', true),
      ('ops', false),
    ]);
  });

  testWidgets('the header shows the members, lead first, and adds an agent', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _pump(tester, store);
    expect(find.byType(MemberAvatars), findsOneWidget);
    expect(
      find.byTooltip('Research · lead: answers posts that mention no one'),
      findsOneWidget,
    );
    expect(
      find.byTooltip('tern · answers when @tern is mentioned'),
      findsOneWidget,
    );
    // The notes are quiet lines; the mention is a link.
    expect(find.byType(NoteLine), findsNWidgets(3));
    expect(find.text('Research joined'), findsOneWidget);
    // The routing note of "Let agents decide" is one of them.
    expect(
      find.descendant(
        of: find.byType(NoteLine),
        matching: find.textContaining('Sent to', findRichText: true),
      ),
      findsOneWidget,
    );

    await tester.tap(find.text('+ Agent'));
    await tester.pump(const Duration(milliseconds: 500));
    await _settle(tester);
    // Only agents not in the channel are offered.
    expect(find.text('@ops'), findsOneWidget);
    expect(find.text('@research'), findsNothing);
    await tester.tap(find.text('Ops').last);
    await _settle(tester);
    expect(client.added, ['tern:ops']);
    expect(store.openChannel!.memberIds, ['tern:research', 'tern', 'tern:ops']);
    expect(find.byTooltip('Every agent is in #launch'), findsOneWidget);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('@ suggests agents, members first, and completes', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _pump(tester, store);
    final field = find.descendant(
      of: find.byType(Composer),
      matching: find.byType(TextField),
    );
    await tester.enterText(field, 'Can @');
    await tester.pump();
    expect(find.byType(MentionSuggestions), findsOneWidget);
    expect(find.text('will join'), findsOneWidget, reason: 'Ops is not in it');
    await tester.enterText(field, 'Can @o');
    await tester.pump();
    expect(find.text('Ops'), findsWidgets);
    expect(
      find.descendant(
        of: find.byType(MentionSuggestions),
        matching: find.text('Research'),
      ),
      findsNothing,
    );
    // ⏎ completes rather than sending.
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pump();
    expect(tester.widget<TextField>(field).controller!.text, 'Can @ops ');
    expect(client.sent, isEmpty);
    expect(find.byType(MentionSuggestions), findsNothing);
    await tester.enterText(field, 'Can @ops book it?');
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await _settle(tester);
    expect(client.sent, ['Can @ops book it?']);
    // No @ in an email address.
    await tester.enterText(field, 'mail me@');
    await tester.pump();
    expect(find.byType(MentionSuggestions), findsNothing);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('the channel settings change the lead and agent wakes', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _pump(tester, store);
    await tester.tap(find.byTooltip('Channel settings'));
    await tester.pump(const Duration(milliseconds: 500));
    await _settle(tester);
    expect(find.byType(ChannelDialog), findsOneWidget);
    expect(find.text('Lead'), findsOneWidget);
    expect(
      find.textContaining("Agents' @mentions are shown but start nothing"),
      findsOneWidget,
    );
    await tester.tap(find.text('Agents can wake each other'));
    await tester.pump();
    expect(
      find.textContaining(
        'at most 2 in a row from your message, and 6 an hour',
      ),
      findsOneWidget,
    );
    await tester.tap(find.byType(DropdownButtonFormField<String>));
    await _settle(tester);
    await tester.tap(find.text('tern').last);
    await _settle(tester);
    await tester.tap(find.text('Save'));
    await _settle(tester);
    expect(client.patches.last, (lead: 'tern', agentWakes: true));
    expect(store.openChannel!.lead, 'tern');
    expect(store.openChannel!.agents!.agentWakes, isTrue);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('"Let agents decide" is off until switched on, with a line '
      'saying what it does', (tester) async {
    final (store, client) = await _store();
    await _pump(tester, store);
    await tester.tap(find.byTooltip('Channel settings'));
    await tester.pump(const Duration(milliseconds: 500));
    await _settle(tester);
    final toggle = find.byKey(const Key('let-agents-decide'));
    expect(toggle, findsOneWidget);
    expect(tester.widget<SwitchListTile>(toggle).value, isFalse);
    expect(
      find.textContaining('always goes to the lead'),
      findsOneWidget,
      reason: 'off says where unmentioned posts go',
    );
    await tester.tap(toggle);
    await tester.pump();
    expect(
      find.textContaining('picked by one cheap model call'),
      findsOneWidget,
    );
    expect(find.textContaining('the lead answers'), findsOneWidget);
    await tester.tap(find.text('Save'));
    await _settle(tester);
    expect(client.decidePatches, [true]);
    expect(store.openChannel!.agents!.letAgentsDecide, isTrue);

    // Open again: it shows on, and saving without a change sends nothing.
    await tester.tap(find.byTooltip('Channel settings'));
    await tester.pump(const Duration(milliseconds: 500));
    await _settle(tester);
    expect(tester.widget<SwitchListTile>(toggle).value, isTrue);
    await tester.tap(find.text('Save'));
    await _settle(tester);
    expect(client.decidePatches, [true]);
    debugDefaultTargetPlatformOverride = null;
  });
}
