// Adding many agents to a channel at once: the searchable checklist, the
// add dialog (members checked and disabled, "Add 3 agents") and choosing
// members up-front when creating a channel.
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tern_client/tern_client.dart';
import 'package:tern_desktop/state/store.dart';
import 'package:tern_desktop/theme.dart';
import 'package:tern_desktop/ui/agent_picker.dart';
import 'package:tern_desktop/ui/channel_view.dart';

final _t0 = DateTime(2026, 11, 4, 9);

Agent _agent(String slug, {bool isDefault = false}) => Agent(
  id: isDefault ? slug : 'tern:$slug',
  slug: slug,
  name: slug[0].toUpperCase() + slug.substring(1),
  memory: 'shared',
  isDefault: isDefault,
);

class _Fake extends TernClient {
  _Fake() : super(const Endpoint.unix('/nonexistent'));

  final members = <String>['tern'];
  final bulk = <List<String>>[];
  final created = <String>[];

  /// Agents the core refuses.
  final refuse = <String>{};

  ChannelAgents get _agents => ChannelAgents(
    members: [
      for (final m in members)
        ChannelMember(agentId: m, lead: m == 'tern', addedAt: _t0),
    ],
  );

  @override
  Future<List<Channel>> channels({bool archived = false}) async => [
    Channel(
      id: 'launch',
      slug: 'launch',
      title: 'Launch',
      kind: 'topic',
      agents: _agents,
      updatedAt: _t0,
    ),
  ];

  @override
  Future<BulkAdd> addChannelAgents(String channel, List<String> agents) async {
    bulk.add(agents);
    final added = <String>[];
    final already = <String>[];
    final failed = <({String agent, String error})>[];
    for (final a in agents) {
      if (refuse.contains(a)) {
        failed.add((agent: a, error: 'archived'));
      } else if (members.contains(a)) {
        already.add(a);
      } else {
        members.add(a);
        added.add(a);
      }
    }
    return BulkAdd(
      agents: _agents,
      added: added,
      already: already,
      failed: failed,
    );
  }

  @override
  Future<Channel> createChannel({
    required String slug,
    String? title,
    String? instructions,
    String? workspace,
  }) async {
    created.add(slug);
    return Channel(
      id: slug,
      slug: slug,
      title: title ?? slug,
      kind: 'topic',
      agents: ChannelAgents(
        members: [ChannelMember(agentId: 'tern', lead: true, addedAt: _t0)],
      ),
      updatedAt: _t0,
    );
  }
}

Future<(TernStore, _Fake)> _store() async {
  final client = _Fake();
  final store = TernStore(client)
    ..link = Link.online
    ..health = const Health(pid: 1, version: '0.2.0', features: {'agents'});
  store.agents = [
    _agent('tern', isDefault: true),
    _agent('research'),
    _agent('ops'),
    _agent('infra'),
    _agent('design'),
  ];
  store.channels = await client.channels();
  return (store, client);
}

Future<void> _host(WidgetTester tester, Widget child) async {
  tester.view.physicalSize = const Size(1000, 1000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    MaterialApp(
      theme: ternTheme(TernColors.light, Brightness.light),
      home: Scaffold(
        body: Center(child: SingleChildScrollView(child: child)),
      ),
    ),
  );
  await tester.pump();
}

Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 100));
  }
}

bool? _checked(WidgetTester tester, String id) =>
    tester.widget<CheckboxListTile>(find.byKey(Key('agent-$id'))).value;

void main() {
  testWidgets('selecting, select all, clear and search', (tester) async {
    final (store, _) = await _store();
    var selected = <String>{};
    await _host(
      tester,
      StatefulBuilder(
        builder: (_, set) => AgentPicker(
          agents: store.agents,
          selected: selected,
          locked: const {'tern'},
          onChanged: (s) => set(() => selected = s),
        ),
      ),
    );
    expect(find.text('0 selected'), findsOneWidget);
    await tester.tap(find.byKey(const Key('agent-tern:ops')));
    await tester.pump();
    expect(selected, {'tern:ops'});
    expect(find.text('1 selected'), findsOneWidget);

    await tester.tap(find.byKey(const Key('agents-select-all')));
    await tester.pump();
    // tern is locked: never counted.
    expect(selected, {
      'tern:research',
      'tern:ops',
      'tern:infra',
      'tern:design',
    });
    expect(_checked(tester, 'tern'), isTrue);

    await tester.tap(find.byKey(const Key('agents-clear')));
    await tester.pump();
    expect(selected, isEmpty);

    // Select all applies to what the search shows.
    await tester.enterText(find.byKey(const Key('agent-search')), 'in');
    await tester.pump();
    expect(find.byKey(const Key('agent-tern:infra')), findsOneWidget);
    expect(find.byKey(const Key('agent-tern:ops')), findsNothing);
    await tester.tap(find.byKey(const Key('agents-select-all')));
    await tester.pump();
    expect(selected, {'tern:infra'});
    await tester.enterText(find.byKey(const Key('agent-search')), 'zzz');
    await tester.pump();
    expect(find.text('No agent matches'), findsOneWidget);
  });

  testWidgets('space toggles the focused row', (tester) async {
    final (store, _) = await _store();
    var selected = <String>{};
    await _host(
      tester,
      StatefulBuilder(
        builder: (_, set) => AgentPicker(
          autofocus: true,
          agents: store.agents,
          selected: selected,
          onChanged: (s) => set(() => selected = s),
        ),
      ),
    );
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowDown);
    await tester.pump();
    await tester.sendKeyEvent(LogicalKeyboardKey.space);
    await tester.pump();
    expect(selected, {'tern'});
  });

  testWidgets('members are checked and disabled; confirm adds the rest', (
    tester,
  ) async {
    final (store, client) = await _store();
    await _host(
      tester,
      AddAgentsDialog(store: store, channel: store.channels.first),
    );
    expect(_checked(tester, 'tern'), isTrue);
    expect(find.text('In channel'), findsOneWidget);
    // The disabled row ignores taps; the button waits for a choice.
    await tester.tap(find.byKey(const Key('agent-tern')));
    await tester.pump();
    expect(find.text('0 selected'), findsOneWidget);
    expect(
      tester
          .widget<FilledButton>(find.byKey(const Key('add-agents')))
          .onPressed,
      isNull,
    );

    await tester.tap(find.byKey(const Key('agents-select-all')));
    await tester.pump();
    expect(find.text('Add 4 agents'), findsOneWidget);
    await tester.tap(find.byKey(const Key('agent-tern:design')));
    await tester.pump();
    expect(find.text('Add 3 agents'), findsOneWidget);
    await tester.tap(find.byKey(const Key('add-agents')));
    await _settle(tester);

    // One request for all of them.
    expect(client.bulk, hasLength(1));
    expect(client.bulk.single.toSet(), {
      'tern:research',
      'tern:ops',
      'tern:infra',
    });
    expect(store.channels.first.memberIds, hasLength(4));
    expect(find.byType(AddAgentsDialog), findsNothing);
  });

  testWidgets('one that can not join is reported and the rest are in', (
    tester,
  ) async {
    final (store, client) = await _store();
    client.refuse.add('tern:ops');
    await _host(
      tester,
      AddAgentsDialog(store: store, channel: store.channels.first),
    );
    await tester.tap(find.byKey(const Key('agent-tern:ops')));
    await tester.pump();
    await tester.tap(find.byKey(const Key('agent-tern:infra')));
    await tester.pump();
    await tester.tap(find.text('Add 2 agents'));
    await _settle(tester);
    expect(find.textContaining("Couldn't add Ops (archived)"), findsOneWidget);
    // Infra joined: now locked; Ops is still chosen, so it can be retried.
    expect(_checked(tester, 'tern:infra'), isTrue);
    expect(find.text('In channel'), findsNWidgets(2));
    expect(find.text('Add 1 agent'), findsOneWidget);
  });

  testWidgets('a new channel can take its members up-front', (tester) async {
    final (store, client) = await _store();
    await _host(tester, ChannelDialog(store: store));
    // tern is always in.
    expect(find.text('Always in'), findsOneWidget);
    await tester.enterText(find.byType(TextField).first, 'Launch Plan');
    await tester.tap(find.byKey(const Key('agent-tern:research')));
    await tester.pump();
    await tester.tap(find.byKey(const Key('agent-tern:infra')));
    await tester.pump();
    await tester.tap(find.text('Create'));
    await _settle(tester);
    expect(client.created, ['launch-plan']);
    expect(client.bulk.single.toSet(), {'tern:research', 'tern:infra'});
  });
}
