// Editing and deleting agents and channels in the desktop app: the delete
// confirmations (which show what the daemon says it touches), renaming an
// agent's @name and a channel's #name, and the Archived list.
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/agent_sheet.dart';
import 'package:tern_desktop/ui/channel_view.dart';
import 'package:tern_desktop/ui/manage.dart';

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
  mainChannelId: 'dm:research',
);

const _ops = Agent(
  id: 'tern:ops',
  slug: 'ops',
  name: 'Ops',
  mainChannelId: 'dm:ops',
);

final _launch = Channel(
  id: 'c-launch',
  slug: 'launch',
  title: 'Launch',
  kind: 'topic',
  updatedAt: _t0,
);

class _Fake extends TernClient {
  _Fake() : super(const Endpoint.unix('/nonexistent'));

  var agentList = <Agent>[_tern, _research, _ops];
  var channelList = <Channel>[_launch];
  var archivedAgents = <Agent>[];
  var archivedChannels = <Channel>[];
  var agentBlockers = <String>[];
  var channelBlockers = <String>[];
  final deleted = <String>[];
  final updated = <(String, Map<String, Object?>)>[];
  final channelPatches = <({String id, String? slug, String? title})>[];

  @override
  Future<List<Agent>> agents({bool archived = false}) async =>
      archived ? [...agentList, ...archivedAgents] : agentList;

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'home',
      slug: 'home',
      title: 'Home',
      kind: 'main',
      agentId: 'tern',
      updatedAt: _t0,
    ),
    ...channelList,
    if (archived) ...archivedChannels,
  ];

  @override
  Future<List<Schedule>> schedules() async => const [];

  @override
  Future<AgentImpact> agentImpact(String id) async => AgentImpact(
    id: id,
    slug: 'research',
    name: 'Research',
    routines: const [Named(id: 'r1', label: 'morning brief')],
    ledChannels: const [Named(id: 'c-launch', label: 'launch')],
    conversations: 4,
    memories: 2,
    blockers: agentBlockers,
  );

  @override
  Future<void> deleteAgent(
    String id, {
    String? reassignTo,
    bool deleteRoutines = false,
  }) async {
    deleted.add('$id to=$reassignTo routines=$deleteRoutines');
    agentList = [
      for (final a in agentList)
        if (a.id != id) a,
    ];
  }

  @override
  Future<ChannelImpact> channelImpact(String id) async => ChannelImpact(
    id: id,
    slug: 'launch',
    title: 'Launch',
    posts: 12,
    conversations: 3,
    routines: const [Named(id: 'r1', label: 'brief')],
    blockers: channelBlockers,
  );

  @override
  Future<void> deleteChannel(String id) async {
    deleted.add(id);
    channelList = [
      for (final c in channelList)
        if (c.id != id) c,
    ];
  }

  @override
  Future<Agent> updateAgent(String id, Map<String, Object?> fields) async {
    updated.add((id, fields));
    return agentList.followedBy(archivedAgents).firstWhere((a) => a.id == id);
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
    channelPatches.add((id: id, slug: slug, title: title));
    if (archived == false) {
      channelList = [...channelList, ...archivedChannels];
      archivedChannels = [];
    }
    return _launch;
  }

  @override
  Future<List<AgentTemplate>> agentTemplates() async => const [];

  @override
  Future<Persona> persona(String id) async =>
      const Persona(persona: 'You are Research.', systemSections: '');

  @override
  Future<List<ProviderModels>> models({
    String? provider,
    bool refresh = false,
  }) async => const [];
}

Future<(TernStore, _Fake)> _store() async {
  final client = _Fake();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(
      pid: 1,
      version: '0.2.0',
      features: {'channels', 'agents'},
    )
    ..info = const Info(defaultModel: 'mock:m', tools: []);
  store.agents = await client.agents();
  store.channels = await client.channels();
  return (store, client);
}

/// A page with one button that opens `open`.
Future<void> _open(
  WidgetTester tester,
  Future<void> Function(BuildContext) open,
) async {
  tester.view.physicalSize = const Size(1200, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Builder(
        builder: (context) => Scaffold(
          body: TextButton(
            onPressed: () => open(context),
            child: const Text('open'),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('open'));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('deleting an agent says what it touches and hands routines on', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _open(tester, (c) => showDeleteAgent(c, store, _research));
    expect(find.text('Delete Research?'), findsOneWidget);
    expect(find.textContaining('4 conversations stay in History'), findsOne);
    expect(find.textContaining('2 private memories will be deleted'), findsOne);
    expect(find.text('morning brief'), findsOneWidget);
    expect(find.textContaining('#launch'), findsOneWidget);

    // Routines are handed to tern unless you say otherwise.
    await tester.tap(find.byKey(const Key('confirm-delete')));
    await tester.pumpAndSettle();
    expect(client.deleted, ['tern:research to=tern routines=false']);
    expect(store.agents.map((a) => a.id), ['tern', 'tern:ops']);
    expect(find.text('Delete Research?'), findsNothing);
  });

  testWidgets('an agent\'s routines can be deleted with it, or placed on '
      'another agent', (tester) async {
    final (store, client) = await _store();
    await _open(tester, (c) => showDeleteAgent(c, store, _research));
    await tester.tap(find.byKey(const Key('routines-to')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Ops').last);
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('confirm-delete')));
    await tester.pumpAndSettle();
    expect(client.deleted, ['tern:research to=tern:ops routines=false']);

    client.agentList = [_tern, _research, _ops];
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('routines-delete')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('confirm-delete')));
    await tester.pumpAndSettle();
    expect(client.deleted.last, 'tern:research to=null routines=true');
  });

  testWidgets('a blocked delete shows why and can\'t be confirmed', (
    tester,
  ) async {
    final (store, client) = await _store();
    client.agentBlockers = ['1 run(s) of @research are in progress'];
    await _open(tester, (c) => showDeleteAgent(c, store, _research));
    expect(find.textContaining('in progress'), findsOneWidget);
    final button = tester.widget<FilledButton>(
      find.byKey(const Key('confirm-delete')),
    );
    expect(button.onPressed, isNull);
    expect(client.deleted, isEmpty);
  });

  testWidgets('deleting a channel keeps its conversations and says so', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _open(tester, (c) => showDeleteChannel(c, store, _launch));
    expect(find.text('Delete #launch?'), findsOneWidget);
    expect(find.textContaining('12 posts will be deleted'), findsOne);
    expect(find.textContaining('3 conversations stay in History'), findsOne);
    expect(find.textContaining('their agent'), findsOneWidget);
    await tester.tap(find.byKey(const Key('confirm-delete')));
    await tester.pumpAndSettle();
    expect(client.deleted, ['c-launch']);
    expect(store.channels.any((c) => c.id == 'c-launch'), isFalse);
  });

  testWidgets('a blocked channel delete is disabled', (tester) async {
    final (store, client) = await _store();
    client.channelBlockers = ['1 run(s) are in progress in #launch'];
    await _open(tester, (c) => showDeleteChannel(c, store, _launch));
    expect(find.textContaining('in progress'), findsOneWidget);
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('confirm-delete')))
          .onPressed,
      isNull,
    );
  });

  testWidgets('the agent sheet renames the @name and offers Delete, except '
      'for the default agent', (tester) async {
    final (store, client) = await _store();
    await _open(tester, (c) => showAgentSheet(c, store, agent: _research));
    expect(find.byKey(const Key('delete-agent')), findsOneWidget);
    await tester.enterText(find.byKey(const Key('agent-slug')), 'scout');
    await tester.enterText(find.byKey(const Key('agent-name')), 'Scout');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    final fields = client.updated.single.$2;
    expect((fields['name'], fields['slug']), ('Scout', 'scout'));

    // Delete opens the confirmation over the sheet.
    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('delete-agent')));
    await tester.pumpAndSettle();
    expect(find.text('Delete Research?'), findsOneWidget);
    await tester.tap(find.text('Cancel').last);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Cancel').last);
    await tester.pumpAndSettle();

    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('delete-agent')), findsOneWidget);
  });

  testWidgets('the default agent has no Delete and no @name field', (
    tester,
  ) async {
    final (store, _) = await _store();
    await _open(tester, (c) => showAgentSheet(c, store, agent: _tern));
    expect(find.byKey(const Key('delete-agent')), findsNothing);
    expect(find.byKey(const Key('agent-slug')), findsNothing);
  });

  testWidgets('a channel is renamed with its #name and deleted from its '
      'settings', (tester) async {
    final (store, client) = await _store();
    await _open(tester, (c) => showChannelSettings(c, store, _launch));
    expect(find.byKey(const Key('delete-channel')), findsOneWidget);
    await tester.enterText(find.byKey(const Key('channel-slug')), 'launches');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();
    expect(client.channelPatches.single.slug, 'launches');

    await tester.tap(find.text('open'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('delete-channel')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('confirm-delete')));
    await tester.pumpAndSettle();
    expect(client.deleted, ['c-launch']);
    // The settings closed with the channel.
    expect(find.byKey(const Key('delete-channel')), findsNothing);
  });

  testWidgets('archived agents and channels can be restored', (tester) async {
    final (store, client) = await _store();
    const gone = Agent(
      id: 'tern:old',
      slug: 'old',
      name: 'Old',
      archived: true,
      mainChannelId: 'dm:old',
    );
    client.archivedAgents = [gone];
    client.archivedChannels = [
      Channel(
        id: 'c-old',
        slug: 'old-topic',
        title: 'Old topic',
        kind: 'topic',
        archived: true,
        updatedAt: _t0,
      ),
    ];
    await _open(tester, (c) => showArchived(c, store));
    expect(find.byKey(const Key('archived-agent-old')), findsOneWidget);
    expect(find.byKey(const Key('archived-channel-old-topic')), findsOneWidget);
    await tester.tap(
      find.descendant(
        of: find.byKey(const Key('archived-agent-old')),
        matching: find.text('Restore'),
      ),
    );
    await tester.pumpAndSettle();
    expect(client.updated.single.$1, 'tern:old');
    expect(client.updated.single.$2, {'archived': false});
    await tester.tap(
      find.descendant(
        of: find.byKey(const Key('archived-channel-old-topic')),
        matching: find.text('Restore'),
      ),
    );
    await tester.pumpAndSettle();
    expect(client.channelPatches.last.id, 'c-old');
  });
}
